//! A chat's or channel's info pane, its mute, and whether the other side has seen my last
//! message. Each is one request to the chat service, made because the user opened something.
//!
//! Unverified on a real tenant, most likely wrong first:
//! - Mute is the chat service's `alerts` property, as Skype's web client writes it; the Teams
//!   client may keep its own notification setting elsewhere (no client read sets it).
//! - "Seen" is read from `GET /v1/threads/{id}/consumptionhorizons`, the members' read markers.
//! - A thread's `createdat` and `creator` properties, read off the thread description.

use super::links;
use super::{escaped, id_of, millis, thread_of, Audience, Auth, Body, Fault, Teams};
use crate::adapters::{ConversationAbout, SpaceAbout};
use protocol::{ConversationKind, ConversationTab, MeetingEvent, MeetingResponse, Notify};
use serde_json::{json, Value};
use std::collections::HashMap;

/// "Seen" is told in one-to-one and small chats only, never in channels, as Teams does.
const SMALL: usize = 20;

pub(super) fn notify_word(notify: Notify) -> &'static str {
    match notify {
        Notify::All => "all",
        Notify::Mentions => "mentions",
        Notify::Nothing => "nothing",
    }
}

pub(super) fn parse_notify_word(word: &str) -> Option<Notify> {
    match word {
        "all" => Some(Notify::All),
        "mentions" => Some(Notify::Mentions),
        "nothing" => Some(Notify::Nothing),
        _ => None,
    }
}

/// Reads Teams per-conversation notification level from `channelNotificationSettings`,
/// `alerts` + `mentions`, `isMuted`, or `isFollowed`.
pub(super) fn parse_notify(source: &Value, is_channel: bool) -> Option<Notify> {
    let settings = match &source["channelNotificationSettings"] {
        Value::String(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    if settings.is_object() {
        if let Some(level) = settings["level"].as_str().or_else(|| settings["mode"].as_str()) {
            match level.to_ascii_lowercase().as_str() {
                "all" | "allposts" | "bannerandfeed" => return Some(Notify::All),
                "mentions" | "mentionsonly" => return Some(Notify::Mentions),
                "off" | "none" | "nothing" => return Some(Notify::Nothing),
                _ => {}
            }
        }
        let all_posts = &settings["allPosts"];
        let new_posts = if all_posts.is_null() { &settings["newPosts"] } else { all_posts };
        let mentions = &settings["mentions"];
        let is_on = |v: &Value| {
            *v == Value::Bool(true)
                || v.as_str().is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "bannerandfeed" | "feed" | "on" | "true" | "all"))
        };
        let is_off = |v: &Value| {
            *v == Value::Bool(false)
                || v.as_str().is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "off" | "none" | "false"))
        };
        if is_on(new_posts) {
            return Some(Notify::All);
        }
        if is_off(new_posts) {
            return Some(if is_off(mentions) { Notify::Nothing } else { Notify::Mentions });
        }
    }

    let alerts = &source["alerts"];
    let mentions = &source["mentions"];
    let mentions_on = *mentions == Value::Bool(true)
        || mentions.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("on"));
    match alerts {
        Value::Bool(true) => return Some(Notify::All),
        Value::Bool(false) => return Some(if mentions_on { Notify::Mentions } else { Notify::Nothing }),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "true" | "all" => return Some(Notify::All),
            "mentions" | "mentionsonly" => return Some(Notify::Mentions),
            "false" | "off" | "none" => return Some(if mentions_on { Notify::Mentions } else { Notify::Nothing }),
            _ => {}
        },
        _ => {}
    }

    if source["isMuted"] == true {
        return Some(Notify::Nothing);
    }
    if is_channel && source["isFollowed"] == true {
        return Some(Notify::All);
    }
    None
}

impl Teams {
    fn described(&self, thread: &str) -> Result<Value, Fault> {
        Ok(self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true)?.body)
    }

    pub(super) fn conversation_info(&self, conversation: &str) -> Result<ConversationAbout, Fault> {
        let thread = thread_of(conversation);
        let row = self.ctx.known(thread)?;
        let channel = super::tree::is_channel(thread);
        let remembered = self
            .state()
            .notify
            .get(thread)
            .copied()
            .or_else(|| self.ctx.note(&format!("notify:{thread}")).as_deref().and_then(parse_notify_word));
        let default_notify = if row.as_ref().is_some_and(|row| row.muted) {
            Notify::Nothing
        } else if let Some(level) = remembered {
            level
        } else {
            Notify::All
        };
        if thread.starts_with("48:") {
            return Ok(ConversationAbout { notify: default_notify, ..ConversationAbout::default() });
        }
        let described = self.described(thread)?;
        let properties = &described["properties"];
        let notify = parse_notify(properties, channel).unwrap_or(default_notify);
        self.state().notify.insert(thread.to_owned(), notify);
        self.ctx.set_note(&format!("notify:{thread}"), notify_word(notify));
        let members = described["members"].as_array().map(|members| members.len() as u32);
        let text = |name: &str| properties[name].as_str().unwrap_or("").trim().to_owned();
        let creator = text("creator");
        let creator = match creator.is_empty() {
            true => String::new(),
            false => {
                self.lookup(std::slice::from_ref(&creator));
                self.name_of(&creator).unwrap_or_default()
            }
        };
        let created = millis(&id_of(&properties["createdat"])) / 1000;
        let shared = properties["sharedChannel"] == true || properties["threadType"] == "sharedChannel";
        let link = self.share_link(thread, row.as_ref(), shared);
        let tabs = self.fetch_tabs(thread, channel, &described, &link);
        let meeting = self.meeting_for(thread, &described);
        Ok(ConversationAbout {
            // A chat's topic is its title, already on the row: only a description is more.
            description: text("description"),
            members,
            created: (created > 0).then_some(created),
            creator,
            notify,
            external: if shared { "Shared channel".to_owned() } else { String::new() },
            link,
            tabs,
            meeting,
            ..ConversationAbout::default()
        })
    }

    /// Today's meetings, soonest first. Graph's calendar view first; the middle tier when Graph
    /// refuses; the meeting chats already listed when neither answers. An empty Graph day is
    /// empty. A dead sign-in is still an error.
    pub(super) fn meetings(&self) -> Result<Vec<MeetingEvent>, Fault> {
        let now = super::now_ms() / 1000;
        let start = now - 3_600;
        let end = now + 26 * 3_600;
        match self.fetch_calendar(start, end) {
            Ok(mut events) => {
                events.sort_by_key(|event| event.start);
                {
                    let mut state = self.state();
                    for event in events.iter().rev().cloned() {
                        remember(&mut state.meetings, event);
                    }
                }
                Ok(today_sorted(events, now))
            }
            Err(Fault::Auth(why)) => Err(Fault::Auth(why)),
            Err(_) => Ok(today_sorted(self.state().meetings.values().cloned(), now)),
        }
    }

    /// Accept, decline, or tentatively accept. Propose-a-time is a tentative answer with a new
    /// start. Organizer and an empty id are not posted. `None` only updates what we remembered.
    pub(super) fn meeting_rsvp(&self, id: &str, response: MeetingResponse, propose: Option<i64>) -> Result<(), Fault> {
        if id.is_empty() || response == MeetingResponse::Organizer {
            return Err(Fault::Refused { status: 404, message: "no such meeting".to_owned() });
        }
        if response == MeetingResponse::None {
            self.note_response(id, response);
            return Ok(());
        }
        let known = {
            let state = self.state();
            state.meetings.get(id).cloned().or_else(|| state.meetings.values().find(|event| event.id == id || event.conversation == id).cloned())
        };
        let action = match response {
            MeetingResponse::Accepted => "accept",
            MeetingResponse::Tentative => "tentativelyAccept",
            MeetingResponse::Declined => "decline",
            MeetingResponse::None | MeetingResponse::Organizer => unreachable!(),
        };
        let duration = known.as_ref().map(|event| event.end - event.start).filter(|span| *span > 0).unwrap_or(30 * 60);
        let body = match (response, propose) {
            (MeetingResponse::Tentative, Some(start)) => json!({
                "sendResponse": true,
                "proposedNewTime": {
                    "start": {"dateTime": super::iso_utc(start), "timeZone": "UTC"},
                    "end": {"dateTime": super::iso_utc(start + duration), "timeZone": "UTC"},
                }
            }),
            _ => json!({"sendResponse": true}),
        };
        let url = format!("{}/v1.0/me/events/{}/{action}", self.graph.trim_end_matches('/'), escaped(id));
        self.call("POST", &url, Auth::Bearer(Audience::Graph), Body::Json(&body), false)?;
        self.note_response(id, response);
        Ok(())
    }

    fn note_response(&self, id: &str, response: MeetingResponse) {
        let mut state = self.state();
        if let Some(event) = state.meetings.get_mut(id) {
            event.response = response;
            return;
        }
        if let Some(event) = state.meetings.values_mut().find(|event| event.id == id || event.conversation == id) {
            event.response = response;
        }
    }

    /// The meeting this thread is, from the calendar remembered this run, else from the thread
    /// description. Missing calendar data leaves the rest of the info pane alone.
    fn meeting_for(&self, thread: &str, described: &Value) -> Option<MeetingEvent> {
        let remembered = {
            let state = self.state();
            state
                .meetings
                .values()
                .find(|event| event.start > 0 && (event.conversation == thread || event.id == thread))
                .cloned()
        };
        if remembered.is_some() {
            return remembered;
        }
        meeting_from_thread(self.cloud.endpoints().teams, thread, described)
    }

    /// Graph `calendarView`, then the middle tier's `calendarEvents`. An empty `value` array is
    /// a real empty answer and does not fall through.
    fn fetch_calendar(&self, start: i64, end: i64) -> Result<Vec<MeetingEvent>, Fault> {
        let host = self.cloud.endpoints().teams;
        let start_q = escaped(&super::iso_utc(start));
        let end_q = escaped(&super::iso_utc(end));
        let graph = format!("{}/v1.0/me/calendarView?startDateTime={start_q}&endDateTime={end_q}", self.graph.trim_end_matches('/'));
        match self.call("GET", &graph, Auth::Bearer(Audience::Graph), Body::None, false) {
            Ok(answer) => return Ok(parse_calendar_value(host, &answer.body)),
            Err(Fault::Auth(why)) => return Err(Fault::Auth(why)),
            Err(_) => {}
        }
        let place = self.place()?;
        let middle = format!("{}/beta/me/calendarEvents?startTime={start_q}&endTime={end_q}", place.middle.trim_end_matches('/'));
        match self.call("GET", &middle, Auth::Both(Audience::Spaces), Body::None, false) {
            Ok(answer) => Ok(parse_calendar_value(host, &answer.body)),
            Err(fault) => Err(fault),
        }
    }

    /// Reads channel or chat tabs from thread properties or Teams' middle-tier tabs endpoint.
    /// Never fails `ConversationInfo` if the tabs endpoint is unavailable.
    fn fetch_tabs(&self, thread: &str, is_channel: bool, described: &Value, channel_link: &str) -> Vec<ConversationTab> {
        let mut raw_tabs: Vec<Value> = match &described["properties"]["tabs"] {
            Value::Array(list) => list.clone(),
            Value::String(s) if !s.trim().is_empty() => serde_json::from_str::<Vec<Value>>(s).unwrap_or_default(),
            _ => Vec::new(),
        };
        if raw_tabs.is_empty() {
            if let Ok(place) = self.place() {
                let kind_seg = if is_channel { "channels" } else { "chats" };
                let url = format!("{}/beta/{kind_seg}/{}/tabs", place.middle, escaped(thread));
                if let Ok(answer) = self.call("GET", &url, Auth::Both(Audience::Spaces), Body::None, false) {
                    if let Some(list) = answer.body["value"]
                        .as_array()
                        .or_else(|| answer.body["tabs"].as_array())
                        .or_else(|| answer.body.as_array())
                    {
                        raw_tabs = list.clone();
                    }
                }
            }
        }
        parse_tabs(self.cloud.endpoints().teams, thread, channel_link, &raw_tabs)
    }

    /// The https link the info pane copies. A channel and a chat use Microsoft's documented
    /// forms. A meeting chat uses the join link. A reply chain links its post.
    fn share_link(&self, thread: &str, row: Option<&protocol::Conversation>, shared: bool) -> String {
        let host = self.cloud.endpoints().teams;
        if thread.starts_with("19:meeting_") {
            return links::meeting_share_on(host, thread);
        }
        match row {
            Some(row) if row.kind == ConversationKind::Channel => {
                let group = row.space.as_ref().and_then(|team| self.ctx.note(&format!("group:{team}"))).unwrap_or_default();
                let mut link = links::channel_share_on(host, thread, &row.title, &group, &self.session.tenant);
                if shared {
                    link.push_str("&ngc=true&allowXTenantAccess=true");
                } else if row.private {
                    link.push_str("&ngc=true");
                }
                link
            }
            Some(row) if row.kind == ConversationKind::Thread => match (row.parent.as_deref(), row.root.as_deref()) {
                (Some(parent), Some(root)) => links::message_share_on(host, parent, root, &self.session.tenant, None),
                _ => String::new(),
            },
            _ => links::chat_share_on(host, thread),
        }
    }

    /// A team's info, from its own thread. The team's id is that thread (General).
    pub(super) fn space_info(&self, space: &str) -> Result<SpaceAbout, Fault> {
        let described = self.described(space)?;
        let properties = &described["properties"];
        let text = |name: &str| properties[name].as_str().unwrap_or("").trim().to_owned();
        let me = self.me();
        let role = described["members"].as_array().into_iter().flatten().find(|member| member["id"].as_str() == Some(me.as_str()));
        let role = role.and_then(|member| member["role"].as_str()).and_then(super::people::role_label);
        let members = described["members"].as_array().map(|members| members.len() as u32);
        Ok(SpaceAbout {
            description: text("description"),
            members,
            roles: role.map(str::to_owned).into_iter().collect(),
            ..SpaceAbout::default()
        })
    }

    /// Updates Teams per-conversation notification level (`All`, `Mentions`, `Nothing`).
    pub(super) fn set_mute(&self, conversation: &str, notify: Notify) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let channel = super::tree::is_channel(thread);
        let path = format!("/v1/users/ME/conversations/{}/properties?name=alerts", escaped(thread));
        let alerts_body = match notify {
            Notify::All => json!({"alerts": "true"}),
            Notify::Mentions => json!({"alerts": "false", "mentions": "true"}),
            Notify::Nothing => json!({"alerts": "false"}),
        };
        self.chat("PUT", &path, Body::Json(&alerts_body), false)?;
        if channel {
            let settings_path = format!(
                "/v1/users/ME/conversations/{}/properties?name=channelNotificationSettings",
                escaped(thread)
            );
            let settings_body = match notify {
                Notify::All => json!({"channelNotificationSettings": {"allPosts": "bannerAndFeed", "mentions": "bannerAndFeed"}}),
                Notify::Mentions => json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "bannerAndFeed"}}),
                Notify::Nothing => json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "off"}}),
            };
            let _ = self.chat("PUT", &settings_path, Body::Json(&settings_body), false);
        }
        self.state().notify.insert(thread.to_owned(), notify);
        self.ctx.set_note(&format!("notify:{thread}"), notify_word(notify));
        self.ctx.muted(thread, notify == Notify::Nothing)?;
        Ok(())
    }

    /// Leaves a group chat by removing myself from its member list, then drops the row.
    pub(super) fn leave(&self, conversation: &str) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let path = format!("/v1/threads/{}/members/{}", escaped(thread), escaped(&self.me()));
        self.chat("DELETE", &path, Body::None, false)?;
        self.ctx.conversation_gone(conversation)?;
        Ok(())
    }

    /// Pins or unpins a Teams chat and updates local `Filing::pin`.
    pub(super) fn file_chat(&self, conversation: &str, pinned: Option<bool>) -> Result<(), Fault> {
        let Some(pin) = pinned else {
            return Ok(());
        };
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Only Teams chats can be pinned here.".to_owned(),
            });
        }
        if pin {
            let state = self.state();
            if !state.pinned_chats.iter().any(|id| id == thread) && state.pinned_chats.len() >= 15 {
                return Err(Fault::Refused {
                    status: 400,
                    message: "Teams allows at most 15 pinned chats.".to_owned(),
                });
            }
        }
        let path = format!("/v1/users/ME/conversations/{}/properties?name=pinned", escaped(thread));
        self.chat("PUT", &path, Body::Json(&json!({"pinned": if pin { "true" } else { "false" }})), false)?;
        let pin_pos = {
            let mut state = self.state();
            if pin {
                if let Some(pos) = state.pinned_chats.iter().position(|id| id == thread) {
                    Some(pos as i32)
                } else {
                    state.pinned_chats.push(thread.to_owned());
                    Some((state.pinned_chats.len() - 1) as i32)
                }
            } else {
                state.pinned_chats.retain(|id| id != thread);
                None
            }
        };
        let existing = self.ctx.known(thread)?;
        let filing = crate::store::Filing {
            hidden: existing.as_ref().is_some_and(|row| row.hidden),
            pin: pin_pos,
            ..crate::store::Filing::default()
        };
        self.ctx.filing(thread, &filing)?;
        Ok(())
    }

    /// Hides a Teams chat (`properties?name=hidden`) until a new message arrives.
    pub(super) fn hide_chat(&self, conversation: &str) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Only Teams chats can be hidden here.".to_owned(),
            });
        }
        let path = format!("/v1/users/ME/conversations/{}/properties?name=hidden", escaped(thread));
        self.chat("PUT", &path, Body::Json(&json!({"hidden": "true"})), false)?;
        let existing = self.ctx.known(thread)?;
        let filing = crate::store::Filing {
            hidden: true,
            pin: existing.as_ref().and_then(|row| row.pin),
            ..crate::store::Filing::default()
        };
        self.ctx.filing(thread, &filing)?;
        Ok(())
    }

    /// Deletes a Teams chat for the current user only and removes it from the local store.
    pub(super) fn delete_chat(&self, conversation: &str, delete_chat: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if super::tree::is_channel(thread) || thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Teams channels cannot be deleted here.".to_owned(),
            });
        }
        if !delete_chat {
            return Err(Fault::Refused {
                status: 400,
                message: "Teams only supports deleting the entire chat for yourself.".to_owned(),
            });
        }
        let path = format!("/v1/users/ME/conversations/{}", escaped(thread));
        self.chat("DELETE", &path, Body::None, false)?;
        self.state().pinned_chats.retain(|id| id != thread);
        self.ctx.conversation_gone(thread)?;
        Ok(())
    }

    /// After a chat's newest page: when my message is the newest, whether everyone else has
    /// read up to it, from their read markers. Not for channels, nor for large chats.
    pub(super) fn seen_by(&self, conversation: &str) -> Result<(), Fault> {
        let Some(row) = self.ctx.known(conversation)? else {
            return Ok(());
        };
        if row.kind == ConversationKind::Channel || row.kind == ConversationKind::Thread || conversation.starts_with("48:") || !row.preview_mine {
            return Ok(());
        }
        let path = format!("/v1/threads/{}/consumptionhorizons", escaped(conversation));
        let answer = match self.chat("GET", &path, Body::None, true) {
            Ok(answer) => answer.body,
            // A tenant or a chat that does not say: nothing is claimed.
            Err(Fault::Refused { .. }) => return Ok(()),
            Err(fault) => return Err(fault),
        };
        let me = self.me();
        let horizons: Vec<i64> = answer["consumptionhorizons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|member| member["id"].as_str().is_some_and(|id| id != me))
            .filter_map(|member| member["consumptionhorizon"].as_str()?.split(';').next().map(millis))
            .collect();
        if horizons.is_empty() || horizons.len() > SMALL {
            return Ok(());
        }
        // Everyone has read up to the least of their markers.
        if let Some(least) = horizons.into_iter().min().filter(|least| *least > 0) {
            self.ctx.seen(conversation, &least.to_string())?;
        }
        Ok(())
    }
}

/// A meeting chat the list already has, when it carries a start time. A subject with no start
/// (the usual chat-list shape) is not stored.
pub(super) fn meeting_from_chat(teams_host: &str, chat: &Value) -> Option<MeetingEvent> {
    let id = chat["id"].as_str().unwrap_or("").trim();
    let info = &chat["meetingInformation"];
    let meeting = id.starts_with("19:meeting_") || chat["chatType"] == "meeting" || info.is_object();
    if !meeting || id.is_empty() {
        return None;
    }
    let mut event = meeting_from_calendar_item(teams_host, chat)?;
    if event.conversation.is_empty() {
        event.conversation = id.to_owned();
    }
    // The list's id is the thread. Keeping it there lets a later calendar row replace this one.
    event.id = id.to_owned();
    Some(event)
}

/// Keeps one row per meeting. A later calendar row replaces the chat-list row for the same
/// thread. A chat-list row does not replace a calendar row.
pub(super) fn remember(meetings: &mut HashMap<String, MeetingEvent>, event: MeetingEvent) {
    if event.start <= 0 || event.id.is_empty() {
        return;
    }
    let incoming_calendar = calendar_row(&event);
    let conversation = event.conversation.clone();
    let matches = |key: &String, known: &MeetingEvent| {
        key == &event.id || (!conversation.is_empty() && (known.conversation == conversation || key == &conversation))
    };
    if !incoming_calendar && meetings.iter().any(|(key, known)| matches(key, known) && calendar_row(known)) {
        return;
    }
    let stale: Vec<String> = meetings.iter().filter(|(key, known)| matches(key, known) && *key != &event.id).map(|(key, _)| (*key).clone()).collect();
    for key in stale {
        meetings.remove(&key);
    }
    meetings.insert(event.id.clone(), event);
}

fn calendar_row(event: &MeetingEvent) -> bool {
    event.conversation.is_empty() || event.id != event.conversation
}

fn meeting_from_thread(teams_host: &str, thread: &str, described: &Value) -> Option<MeetingEvent> {
    let candidates = [&described["properties"]["meetingInformation"], &described["meetingInformation"], described];
    for item in candidates {
        let Some(mut event) = meeting_from_calendar_item(teams_host, item) else {
            continue;
        };
        if event.start <= 0 {
            continue;
        }
        if event.conversation.is_empty() && thread.starts_with("19:meeting_") {
            event.conversation = thread.to_owned();
        }
        if event.id.is_empty() {
            event.id = if event.conversation.is_empty() { thread.to_owned() } else { event.conversation.clone() };
        }
        return Some(event);
    }
    None
}

fn parse_calendar_value(teams_host: &str, body: &Value) -> Vec<MeetingEvent> {
    let list = body["value"]
        .as_array()
        .or_else(|| body["events"].as_array())
        .or_else(|| body["calendarEvents"].as_array())
        .or_else(|| body.as_array());
    list.map(|list| list.iter().filter_map(|item| meeting_from_calendar_item(teams_host, item)).collect()).unwrap_or_default()
}

fn meeting_from_calendar_item(teams_host: &str, item: &Value) -> Option<MeetingEvent> {
    if !item.is_object() {
        return None;
    }
    let start = event_instant(item, true);
    if start <= 0 {
        return None;
    }
    let mut end = event_instant(item, false);
    if end < start {
        end = start;
    }
    let mut title = first_text(&[
        &item["subject"],
        &item["title"],
        &item["meetingInformation"]["subject"],
        &item["meetingInformation"]["title"],
    ]);
    if title.is_empty() {
        title = "Meeting".to_owned();
    }
    let mut urls = Vec::new();
    for value in [
        &item["joinUrl"],
        &item["joinWebUrl"],
        &item["onlineMeeting"]["joinUrl"],
        &item["meetingJoinUrl"],
        &item["onlineMeetingUrl"],
        &item["meetingInformation"]["joinUrl"],
        &item["webLink"],
    ] {
        if let Some(url) = value.as_str().map(str::trim).filter(|url| !url.is_empty()) {
            urls.push(url.to_owned());
        }
    }
    let mut conversation = first_text(&[
        &item["threadId"],
        &item["conversationId"],
        &item["chatId"],
        &item["meetingInformation"]["threadId"],
    ]);
    if conversation.is_empty() {
        if let Some(thread) = urls.iter().find_map(|url| {
            let thread = thread_in_join(url);
            (!thread.is_empty()).then_some(thread)
        }) {
            conversation = thread;
        }
    }
    let raw_id = item["id"].as_str().unwrap_or("").trim();
    if conversation.is_empty() && raw_id.starts_with("19:meeting_") {
        conversation = raw_id.to_owned();
    }
    let mut id = first_text(&[&item["id"], &item["eventId"], &item["iCalUId"]]);
    if id.is_empty() {
        id = conversation.clone();
    }
    if id.is_empty() {
        return None;
    }
    let join_url = join_of(teams_host, &urls, &conversation);
    Some(MeetingEvent {
        id,
        title,
        start,
        end,
        organizer: organizer_of(item),
        join_url,
        response: response_of(item),
        conversation,
    })
}

fn event_instant(item: &Value, start: bool) -> i64 {
    let fields: &[&Value] = if start {
        &[
            &item["start"],
            &item["startTime"],
            &item["scheduledStartTime"],
            &item["startDateTime"],
            &item["meetingInformation"]["startTime"],
            &item["meetingInformation"]["start"],
        ]
    } else {
        &[
            &item["end"],
            &item["endTime"],
            &item["scheduledEndTime"],
            &item["endDateTime"],
            &item["meetingInformation"]["endTime"],
            &item["meetingInformation"]["end"],
        ]
    };
    fields.iter().map(|value| instant_of(value)).find(|instant| *instant > 0).unwrap_or(0)
}

fn instant_of(value: &Value) -> i64 {
    match value {
        Value::Number(number) => epoch_seconds(number.as_i64().unwrap_or(0)),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return 0;
            }
            if let Ok(number) = text.parse::<i64>() {
                return epoch_seconds(number);
            }
            super::seconds(text)
        }
        Value::Object(_) => {
            let date_time = value["dateTime"].as_str().unwrap_or("").trim();
            if date_time.is_empty() {
                0
            } else if let Ok(number) = date_time.parse::<i64>() {
                epoch_seconds(number)
            } else {
                super::seconds(date_time)
            }
        }
        _ => 0,
    }
}

fn epoch_seconds(number: i64) -> i64 {
    if number > 10_000_000_000 { number / 1000 } else { number.max(0) }
}

fn organizer_of(item: &Value) -> String {
    first_text(&[
        &item["organizer"]["emailAddress"]["name"],
        &item["organizer"]["displayName"],
        &item["organizerName"],
        &item["organizer"],
        &item["meetingInformation"]["organizerName"],
    ])
}

fn response_of(item: &Value) -> MeetingResponse {
    if item["isOrganizer"] == true {
        return MeetingResponse::Organizer;
    }
    let word = first_text(&[
        &item["responseStatus"]["response"],
        &item["response"],
        &item["myResponseType"],
        &item["meetingInformation"]["response"],
    ])
    .to_ascii_lowercase();
    match word.as_str() {
        "accepted" | "accept" => MeetingResponse::Accepted,
        "tentativelyaccepted" | "tentative" => MeetingResponse::Tentative,
        "declined" | "decline" => MeetingResponse::Declined,
        "organizer" => MeetingResponse::Organizer,
        _ => MeetingResponse::None,
    }
}

fn join_of(teams_host: &str, urls: &[String], conversation: &str) -> String {
    let preferred = urls.iter().find(|url| {
        let lower = url.to_ascii_lowercase();
        valid_http_url(url).is_some() && (lower.contains("meetup-join") || lower.contains("teams.microsoft"))
    });
    if let Some(url) = preferred.and_then(|url| valid_http_url(url)) {
        return url;
    }
    if let Some(url) = urls.iter().find_map(|url| valid_http_url(url)) {
        return url;
    }
    if conversation.starts_with("19:meeting_") {
        return links::meeting_share_on(teams_host, conversation);
    }
    String::new()
}

fn thread_in_join(url: &str) -> String {
    if let Some(found) = links::parse_share(url) {
        if found.kind == links::ShareKind::Meeting && !found.conversation.is_empty() {
            return found.conversation;
        }
    }
    let marker = "/l/meetup-join/";
    let lower = url.to_ascii_lowercase();
    let Some(at) = lower.find(marker) else {
        return String::new();
    };
    let rest = &url[at + marker.len()..];
    let id = percent_decode(rest.split(['/', '?', '#']).next().unwrap_or(""));
    if id.starts_with("19:") { id } else { String::new() }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if let Some(hex) = bytes.get(at + 1..at + 3).and_then(|hex| std::str::from_utf8(hex).ok()) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    at += 3;
                    continue;
                }
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8(out).unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

fn first_text(values: &[&Value]) -> String {
    values.iter().find_map(|value| value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)).unwrap_or_default()
}

fn today_sorted(events: impl IntoIterator<Item = MeetingEvent>, now: i64) -> Vec<MeetingEvent> {
    let mut events: Vec<MeetingEvent> = events.into_iter().filter(|event| event.start > 0 && same_local_day(event.start, now)).collect();
    events.sort_by_key(|event| event.start);
    let mut seen = Vec::new();
    events.retain(|event| {
        if event.conversation.is_empty() {
            return true;
        }
        if seen.iter().any(|id: &String| id == &event.conversation) {
            return false;
        }
        seen.push(event.conversation.clone());
        true
    });
    events.truncate(protocol::MEETINGS_LIMIT);
    events
}

fn same_local_day(a: i64, b: i64) -> bool {
    match (local_parts(a), local_parts(b)) {
        (Some((year_a, day_a)), Some((year_b, day_b))) => year_a == year_b && day_a == day_b,
        _ => false,
    }
}

fn local_parts(unix: i64) -> Option<(i32, i32)> {
    // SAFETY: the platform's localtime fills the struct handed to it.
    unsafe {
        let mut local: libc::tm = std::mem::zeroed();
        let unix = unix as libc::time_t;
        #[cfg(unix)]
        libc::localtime_r(&unix, &mut local);
        #[cfg(windows)]
        if libc::localtime_s(&mut local, &unix) != 0 {
            return None;
        }
        Some((local.tm_year, local.tm_yday))
    }
}

fn valid_http_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    (trimmed.starts_with("https://") || trimmed.starts_with("http://")).then(|| trimmed.to_owned())
}

fn classify_tab(app_id: &str, app_name: &str, display_name: &str) -> (&'static str, String) {
    let id = app_id.to_ascii_lowercase();
    let app = app_name.trim();
    let name = display_name.trim().to_ascii_lowercase();
    if id.contains("files.sharepoint") || id == "files" || name == "files" {
        return ("files", if app.is_empty() { "Files".to_owned() } else { app.to_owned() });
    }
    if id.contains(".wiki")
        || id.contains("onenote")
        || id == "0d820ecd-def2-4297-adad-78056cde7c78"
        || name == "notes"
        || name == "wiki"
    {
        return ("notes", if app.is_empty() { "Notes".to_owned() } else { app.to_owned() });
    }
    if id.contains(".planner") || id == "com.microsoft.teamspace.tab.planner" || app.eq_ignore_ascii_case("planner") {
        return ("planner", if app.is_empty() { "Planner".to_owned() } else { app.to_owned() });
    }
    if id.contains(".tab.web") || id == "com.microsoft.teamspace.tab.web" || app.eq_ignore_ascii_case("website") {
        return ("website", if app.is_empty() { "Website".to_owned() } else { app.to_owned() });
    }
    ("app", app.to_owned())
}

fn parse_tabs(teams_host: &str, thread: &str, channel_link: &str, raw_tabs: &[Value]) -> Vec<ConversationTab> {
    let mut tabs = Vec::new();
    for (idx, item) in raw_tabs.iter().take(protocol::TABS_LIMIT).enumerate() {
        let id = item["id"]
            .as_str()
            .or_else(|| item["tabId"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("tab-{idx}"));
        let name = item["displayName"]
            .as_str()
            .or_else(|| item["name"].as_str())
            .or_else(|| item["title"].as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let app_id = item["teamsAppId"]
            .as_str()
            .or_else(|| item["teamsApp"]["id"].as_str())
            .or_else(|| item["appDefinitionId"].as_str())
            .unwrap_or("")
            .trim();
        let app_name = item["teamsApp"]["displayName"]
            .as_str()
            .or_else(|| item["appName"].as_str())
            .unwrap_or("");
        let (kind, app) = classify_tab(app_id, app_name, &name);
        let config = &item["configuration"];
        let content_url = config["contentUrl"]
            .as_str()
            .or_else(|| item["contentUrl"].as_str())
            .and_then(valid_http_url)
            .unwrap_or_default();
        let mut web_url = item["webUrl"]
            .as_str()
            .or_else(|| config["websiteUrl"].as_str())
            .or_else(|| item["websiteUrl"].as_str())
            .and_then(valid_http_url)
            .unwrap_or_default();
        if web_url.is_empty() {
            if !content_url.is_empty() && kind == "website" {
                web_url = content_url.clone();
            } else if !app_id.is_empty() {
                let entity = config["entityId"].as_str().filter(|s| !s.is_empty()).unwrap_or(&id);
                let ctx = format!("{{\"channelId\":\"{}\"}}", thread);
                web_url = format!(
                    "{}/l/entity/{}/{}?context={}",
                    teams_host.trim_end_matches('/'),
                    escaped(app_id),
                    escaped(entity),
                    escaped(&ctx),
                );
            } else if let Some(fallback) = valid_http_url(channel_link) {
                web_url = fallback;
            }
        }
        tabs.push(ConversationTab {
            id,
            name,
            app,
            kind: kind.to_owned(),
            web_url,
            content_url,
        });
    }
    tabs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tabs_classifies_known_apps_validates_urls_and_builds_fallback_links() {
        let raw = vec![
            json!({
                "id": "t1",
                "displayName": "Files",
                "teamsAppId": "com.microsoft.teamspace.tab.files.sharepoint",
                "webUrl": "https://contoso.sharepoint.com/sites/Eng/Shared%20Documents"
            }),
            json!({
                "id": "t2",
                "displayName": "Q4 Board",
                "teamsAppId": "com.microsoft.teamspace.tab.planner",
                "teamsApp": {"displayName": "Planner"},
                "webUrl": "javascript:alert(1)"
            }),
            json!({
                "id": "t3",
                "displayName": "Docs",
                "teamsAppId": "com.microsoft.teamspace.tab.web",
                "configuration": {"websiteUrl": "https://docs.example.com"}
            }),
        ];
        let tabs = parse_tabs("https://gov.teams.microsoft.us", "19:chan@thread.tacv2", "https://gov.teams.microsoft.us/l/channel/x", &raw);
        assert_eq!(tabs.len(), 3);
        assert_eq!((tabs[0].kind.as_str(), tabs[0].name.as_str()), ("files", "Files"));
        assert_eq!((tabs[1].kind.as_str(), tabs[1].app.as_str()), ("planner", "Planner"));
        assert!(tabs[1].web_url.starts_with("https://gov.teams.microsoft.us/l/entity/"), "javascript URL must be rejected and replaced with cloud handoff: {}", tabs[1].web_url);
        assert_eq!((tabs[2].kind.as_str(), tabs[2].web_url.as_str()), ("website", "https://docs.example.com"));
    }

    #[test]
    fn parse_notify_reads_channel_settings_and_alerts_properties() {
        assert_eq!(
            parse_notify(&json!({"channelNotificationSettings": {"allPosts": "bannerAndFeed", "mentions": "bannerAndFeed"}}), true),
            Some(Notify::All)
        );
        assert_eq!(
            parse_notify(&json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "bannerAndFeed"}}), true),
            Some(Notify::Mentions)
        );
        assert_eq!(
            parse_notify(&json!({"channelNotificationSettings": {"allPosts": "off", "mentions": "off"}}), true),
            Some(Notify::Nothing)
        );
        assert_eq!(
            parse_notify(&json!({"alerts": "false", "mentions": "true"}), false),
            Some(Notify::Mentions)
        );
        assert_eq!(parse_notify(&json!({"alerts": "false"}), false), Some(Notify::Nothing));
        assert_eq!(parse_notify(&json!({"alerts": "true"}), false), Some(Notify::All));
        assert_eq!(parse_notify(&json!({"isFollowed": true}), true), Some(Notify::All));
        assert_eq!(parse_notify(&json!({"isMuted": true}), true), Some(Notify::Nothing));
    }

    fn local_noon() -> i64 {
        let now = super::super::now_ms() / 1000;
        let (hour, minute) = local_clock(now);
        now - hour * 3600 - minute * 60 - now.rem_euclid(60) + 12 * 3600
    }

    fn local_clock(unix: i64) -> (i64, i64) {
        unsafe {
            let mut local: libc::tm = std::mem::zeroed();
            let unix = unix as libc::time_t;
            #[cfg(unix)]
            libc::localtime_r(&unix, &mut local);
            #[cfg(windows)]
            if libc::localtime_s(&mut local, &unix) != 0 {
                return (0, 0);
            }
            (local.tm_hour as i64, local.tm_min as i64)
        }
    }

    #[test]
    fn a_calendar_item_reads_graph_aliases_and_rejects_a_script_join() {
        let host = "https://teams.microsoft.com";
        let thread = "19:meeting_abc@thread.v2";
        let item = json!({
            "id": "evt/1=a",
            "subject": "Standup",
            "start": {"dateTime": "2026-11-21T18:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-21T18:30:00.0000000", "timeZone": "UTC"},
            "organizer": {"emailAddress": {"name": "Ada Lovelace"}},
            "onlineMeeting": {"joinUrl": "javascript:alert(1)"},
            "responseStatus": {"response": "tentativelyAccepted"},
            "threadId": thread
        });
        let event = meeting_from_calendar_item(host, &item).unwrap();
        assert_eq!(event.title, "Standup");
        assert_eq!(event.organizer, "Ada Lovelace");
        assert_eq!(event.response, MeetingResponse::Tentative);
        assert_eq!(event.conversation, thread);
        assert!(event.start > 0 && event.end > event.start);
        assert_eq!(event.join_url, links::meeting_share_on(host, thread), "a script join falls back to the meeting link");
        assert!(!event.join_url.contains("javascript"));

        let preferred = json!({
            "id": "evt-2",
            "title": "Review",
            "start": "2026-11-21T19:00:00Z",
            "end": 1_763_752_800_i64,
            "organizerName": "Grace",
            "joinUrl": "https://teams.microsoft.com/l/meetup-join/19%3Ameeting_xyz%40thread.v2/0",
            "webLink": "https://outlook.office.com/calendar/other",
            "isOrganizer": true
        });
        let event = meeting_from_calendar_item(host, &preferred).unwrap();
        assert_eq!(event.response, MeetingResponse::Organizer);
        assert_eq!(event.organizer, "Grace");
        assert!(event.join_url.contains("meetup-join"));
        assert_eq!(event.conversation, "19:meeting_xyz@thread.v2");
        assert!(meeting_from_calendar_item(host, &json!({"subject": "No time"})).is_none());
    }

    #[test]
    fn a_subject_only_meeting_chat_is_not_stored_and_calendar_wins() {
        let host = "https://teams.microsoft.com";
        let thread = "19:meeting_abc@thread.v2";
        let chat = json!({"id": thread, "title": null, "meetingInformation": {"subject": "Weekly sync"}});
        assert!(meeting_from_chat(host, &chat).is_none());

        let noon = local_noon();
        let dated = json!({
            "id": thread,
            "chatType": "meeting",
            "meetingInformation": {
                "subject": "Weekly sync",
                "startTime": super::super::iso_utc(noon + 3600),
                "endTime": super::super::iso_utc(noon + 5400)
            }
        });
        let from_chat = meeting_from_chat(host, &dated).unwrap();
        assert_eq!(from_chat.id, thread);
        assert_eq!(from_chat.conversation, thread);
        assert!(from_chat.join_url.starts_with("https://"));

        let calendar = MeetingEvent {
            id: "graph-id".into(),
            title: "Weekly sync".into(),
            start: noon + 3600,
            end: noon + 5400,
            organizer: "Ada".into(),
            join_url: links::meeting_share_on(host, thread),
            response: MeetingResponse::Accepted,
            conversation: thread.into(),
        };
        let mut remembered = HashMap::new();
        remember(&mut remembered, from_chat);
        remember(&mut remembered, calendar.clone());
        assert_eq!(remembered.len(), 1);
        assert_eq!(remembered["graph-id"].organizer, "Ada");
        remember(&mut remembered, MeetingEvent { id: thread.into(), conversation: thread.into(), title: "Weekly sync".into(), start: noon + 3600, end: noon + 5400, ..MeetingEvent::default() });
        assert!(remembered.contains_key("graph-id"));
        assert!(!remembered.contains_key(thread));

        let yesterday = MeetingEvent { id: "old".into(), title: "Yesterday".into(), start: noon - 20 * 3600, end: noon - 19 * 3600, ..MeetingEvent::default() };
        let later = MeetingEvent { id: "later".into(), title: "Design review".into(), start: noon + 3 * 3600, end: noon + 4 * 3600, conversation: "19:meeting_later@thread.v2".into(), ..MeetingEvent::default() };
        let sooner = MeetingEvent { id: "sooner".into(), title: "Standup".into(), start: noon + 3600, end: noon + 5400, conversation: thread.into(), ..MeetingEvent::default() };
        let today = today_sorted([later.clone(), sooner.clone(), yesterday, calendar], noon);
        assert_eq!(today.iter().map(|event| event.title.as_str()).collect::<Vec<_>>(), ["Standup", "Design review"]);
        assert!(today[0].start < today[1].start);
    }
}
