//! A chat's or channel's info pane, its mute, and whether the other side has seen my last
//! message. Each is one request to the chat service, made because the user opened something.
//!
//! Unverified on a real tenant, most likely wrong first:
//! - Mute is the chat service's `alerts` property, as Skype's web client writes it; the Teams
//!   client may keep its own notification setting elsewhere (no client read sets it).
//! - "Seen" is read from `GET /v1/threads/{id}/consumptionhorizons`, the members' read markers.
//! - A thread's `createdat` and `creator` properties, read off the thread description.

use super::links;
use super::{escaped, id_of, millis, now_ms, seconds, thread_of, Body, Fault, Teams};
use crate::adapters::{ConversationAbout, SpaceAbout};
use protocol::{ConversationKind, Message, Notify};
use serde_json::{json, Value};

/// The user's personal Saved messages stream on the Teams chat service.
pub(super) const SAVED_STREAM: &str = "48:saved";

/// One entry in a thread's `properties.pinnedMessages` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinnedEntry {
    pub message_id: String,
    pub pinned_by: Option<String>,
    pub pinned_at: Option<i64>,
}

/// Parses Teams thread `properties.pinnedMessages` or `properties.pinnedMessageIds`
/// (array or JSON-encoded string of objects `{messageId, pinnedBy, pinnedTime}` or ids).
pub(super) fn parse_pinned_messages(props: &Value) -> Vec<PinnedEntry> {
    let raw_list = |val: &Value| -> Vec<Value> {
        match val {
            Value::Array(items) => items.clone(),
            Value::String(s) => serde_json::from_str::<Vec<Value>>(s).unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    let mut entries = raw_list(&props["pinnedMessages"]);
    if entries.is_empty() {
        entries = raw_list(&props["pinnedMessageIds"]);
    }
    let mut out = Vec::new();
    for item in entries {
        let entry = match &item {
            Value::String(s) if !s.trim().is_empty() => PinnedEntry {
                message_id: s.trim().to_owned(),
                pinned_by: None,
                pinned_at: None,
            },
            Value::Number(n) => PinnedEntry {
                message_id: n.to_string(),
                pinned_by: None,
                pinned_at: None,
            },
            Value::Object(_) => {
                let message_id = [&item["messageId"], &item["id"], &item["message_id"]]
                    .into_iter()
                    .map(id_of)
                    .find(|id| !id.is_empty())
                    .unwrap_or_default();
                if message_id.is_empty() {
                    continue;
                }
                let pinned_by = [
                    &item["pinnedByDisplayName"],
                    &item["pinnedByName"],
                    &item["pinnedBy"],
                    &item["pinnedByMri"],
                ]
                .into_iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|s| !s.is_empty())
                .map(str::to_owned);
                let pinned_at = [&item["pinnedTime"], &item["pinnedAt"], &item["timestamp"]]
                    .into_iter()
                    .find_map(|v| match v {
                        Value::Number(n) => n.as_i64().map(|t| if t > 10_000_000_000 { t / 1000 } else { t }),
                        Value::String(s) => {
                            let ms = millis(s);
                            if ms > 10_000_000_000 {
                                Some(ms / 1000)
                            } else if ms > 0 {
                                Some(ms)
                            } else {
                                let sec = seconds(s);
                                (sec > 0).then_some(sec)
                            }
                        }
                        _ => None,
                    })
                    .filter(|&t| t > 0);
                PinnedEntry { message_id, pinned_by, pinned_at }
            }
            _ => continue,
        };
        if !out.iter().any(|existing: &PinnedEntry| existing.message_id == entry.message_id) {
            out.push(entry);
        }
    }
    out
}

/// Serializes pinned entries back into Teams' `properties.pinnedMessages` JSON string format.
fn encode_pinned_messages(entries: &[PinnedEntry]) -> String {
    let arr: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut obj = json!({"messageId": entry.message_id});
            if let Some(ref by) = entry.pinned_by {
                obj["pinnedBy"] = Value::String(by.clone());
            }
            if let Some(at) = entry.pinned_at {
                obj["pinnedTime"] = json!(at.saturating_mul(1000));
            }
            obj
        })
        .collect();
    Value::Array(arr).to_string()
}

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
        let pinned_entries = parse_pinned_messages(properties);
        let pinned_count = Some(pinned_entries.len() as u32);
        self.state().pinned_messages.insert(thread.to_owned(), pinned_entries);
        Ok(ConversationAbout {
            // A chat's topic is its title, already on the row: only a description is more.
            description: text("description"),
            members,
            created: (created > 0).then_some(created),
            creator,
            notify,
            pinned: pinned_count,
            external: if shared { "Shared channel".to_owned() } else { String::new() },
            link: self.share_link(thread, row.as_ref(), shared),
            ..ConversationAbout::default()
        })
    }

    /// The https link the info pane copies. A channel and a chat use Microsoft's documented
    /// forms. A meeting chat uses the join link. A reply chain links its post.
    fn share_link(&self, thread: &str, row: Option<&protocol::Conversation>, shared: bool) -> String {
        if thread.starts_with("19:meeting_") {
            return links::meeting_share(thread);
        }
        match row {
            Some(row) if row.kind == ConversationKind::Channel => {
                let group = row.space.as_ref().and_then(|team| self.ctx.note(&format!("group:{team}"))).unwrap_or_default();
                let mut link = links::channel_share(thread, &row.title, &group, &self.session.tenant);
                if shared {
                    link.push_str("&ngc=true&allowXTenantAccess=true");
                } else if row.private {
                    link.push_str("&ngc=true");
                }
                link
            }
            Some(row) if row.kind == ConversationKind::Thread => match (row.parent.as_deref(), row.root.as_deref()) {
                (Some(parent), Some(root)) => links::message_share(parent, root, &self.session.tenant, None),
                _ => String::new(),
            },
            _ => links::chat_share(thread),
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

    /// Resolves a pinned entry's `pinned_by` field (turning an `8:` MRI into a display name).
    fn pinned_by_name(&self, raw: Option<&str>) -> Option<String> {
        let who = raw?.trim();
        if who.is_empty() {
            return None;
        }
        if who.starts_with("8:") || who.starts_with("28:") {
            self.name_of(who).or_else(|| Some(who.to_owned()))
        } else {
            Some(who.to_owned())
        }
    }

    /// Looks up a single message from local store first, then falls back to fetching it from the
    /// Teams chat service (`GET /v1/users/ME/conversations/{thread}/messages/{id}`).
    fn fetch_message(&self, conversation: &str, message_id: &str) -> Result<Option<Message>, Fault> {
        if let Some(stored) = self.ctx.stored_message(conversation, message_id)? {
            return Ok(Some(stored));
        }
        let thread = thread_of(conversation);
        if thread != conversation {
            if let Some(stored) = self.ctx.stored_message(thread, message_id)? {
                return Ok(Some(stored));
            }
        }
        let path = format!(
            "/v1/users/ME/conversations/{}/messages/{}?view=msnp24Equivalent",
            escaped(thread),
            escaped(message_id)
        );
        match self.chat("GET", &path, Body::None, true) {
            Ok(answer) => Ok(self.read_message(thread, &answer.body)),
            Err(Fault::Refused { status: 404, .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Reads the pinned messages of a chat or channel from the thread's `pinnedMessages` property,
    /// marks each message `pinned` in the store, and returns them newest first (up to `PINNED_LIMIT`).
    pub(super) fn pinned_messages(&self, conversation: &str) -> Result<Vec<Message>, Fault> {
        let thread = thread_of(conversation);
        if thread.starts_with("48:") {
            return Ok(Vec::new());
        }
        let described = self.described(thread)?;
        let entries = parse_pinned_messages(&described["properties"]);
        self.apply_pinned_update(thread, entries.clone())?;

        let mris: Vec<String> = entries
            .iter()
            .filter_map(|e| e.pinned_by.as_deref())
            .filter(|who| who.starts_with("8:") || who.starts_with("28:"))
            .map(str::to_owned)
            .collect();
        if !mris.is_empty() {
            self.lookup(&mris);
        }

        let mut out = Vec::new();
        for entry in entries.iter().take(protocol::PINNED_LIMIT) {
            if let Ok(Some(mut msg)) = self.fetch_message(conversation, &entry.message_id) {
                msg.pinned = true;
                msg.pinned_by = self.pinned_by_name(entry.pinned_by.as_deref());
                msg.pinned_at = entry.pinned_at;
                out.push(msg);
            }
        }
        out.sort_by(|a, b| {
            let ta = a.pinned_at.unwrap_or(a.sent);
            let tb = b.pinned_at.unwrap_or(b.sent);
            tb.cmp(&ta).then_with(|| b.sent.cmp(&a.sent))
        });
        Ok(out)
    }

    /// Pins or unpins a message in a Teams chat or channel by updating the thread's
    /// `properties.pinnedMessages` list.
    pub(super) fn pin_message(&self, conversation: &str, message_id: &str, pinned: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        if thread.starts_with("48:") {
            return Err(Fault::Refused {
                status: 400,
                message: "Messages cannot be pinned in this conversation.".to_owned(),
            });
        }
        let mut entries = match self.described(thread) {
            Ok(described) => parse_pinned_messages(&described["properties"]),
            Err(_) => self.state().pinned_messages.get(thread).cloned().unwrap_or_default(),
        };
        let me = self.me();
        let now_secs = now_ms() / 1000;
        if pinned {
            if !entries.iter().any(|e| e.message_id == message_id) {
                entries.push(PinnedEntry {
                    message_id: message_id.to_owned(),
                    pinned_by: (!me.is_empty()).then_some(me),
                    pinned_at: Some(now_secs),
                });
            }
        } else {
            entries.retain(|e| e.message_id != message_id);
        }
        let path = format!("/v1/threads/{}/properties?name=pinnedMessages", escaped(thread));
        let body = json!({"pinnedMessages": encode_pinned_messages(&entries)});
        self.chat("PUT", &path, Body::Json(&body), false)?;
        self.apply_pinned_update(thread, entries)?;
        self.ctx.pinned(conversation, message_id, pinned)?;
        if thread != conversation {
            self.ctx.pinned(thread, message_id, pinned)?;
        }
        self.ctx.info_stale(thread);
        Ok(())
    }

    /// Applies an updated list of pinned entries for `thread` (from `described()`, `pin_message()`,
    /// or a live `ConversationUpdate`/`ThreadUpdate` push) to the local cache and store.
    pub(super) fn apply_pinned_update(&self, thread: &str, entries: Vec<PinnedEntry>) -> Result<(), Fault> {
        let prev = self.state().pinned_messages.insert(thread.to_owned(), entries.clone()).unwrap_or_default();
        for old in &prev {
            if !entries.iter().any(|cur| cur.message_id == old.message_id) {
                let _ = self.ctx.pinned(thread, &old.message_id, false);
            }
        }
        for cur in &entries {
            let _ = self.ctx.pinned(thread, &cur.message_id, true);
        }
        Ok(())
    }

    /// Saves or unsaves a message in the user's personal Teams Saved messages collection (`48:saved`).
    pub(super) fn save_message(&self, conversation: &str, message_id: &str, starred: bool) -> Result<(), Fault> {
        let thread = thread_of(conversation);
        let path = format!("/v1/users/ME/conversations/{}/messages/{}", escaped(SAVED_STREAM), escaped(message_id));
        if starred {
            let body = json!({
                "sourceConversationId": thread,
                "sourceMessageId": message_id,
                "savedTime": now_ms(),
            });
            self.chat("PUT", &path, Body::Json(&body), false)?;
            self.state().saved_ids.insert(message_id.to_owned());
        } else {
            self.chat("DELETE", &path, Body::None, false)?;
            self.state().saved_ids.remove(message_id);
        }
        for conv in [conversation, thread] {
            if let Ok(Some(mut stored)) = self.ctx.stored_message(conv, message_id) {
                stored.starred = starred;
                self.ctx.history(conv, &[stored])?;
            }
        }
        Ok(())
    }

    /// Lists saved messages from Teams' personal `48:saved` stream, newest first.
    pub(super) fn saved_messages(&self, conversation: Option<&str>, limit: u32) -> Result<Vec<Message>, Fault> {
        let count = limit.clamp(1, protocol::PAGE_LIMIT);
        let (items, _) = self.page(SAVED_STREAM, count, "startTime=1")?;
        let filter_thread = conversation.map(thread_of);
        let mut out = Vec::new();
        let mut saved_ids = Vec::new();
        for item in &items {
            let props = &item["properties"];
            let src_conv = [
                props["sourceConversationId"].as_str(),
                props["sourceThreadId"].as_str(),
                item["sourceConversationId"].as_str(),
                item["conversationid"].as_str(),
            ]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty() && *s != SAVED_STREAM)
            .unwrap_or("");
            let src_id = [
                &props["sourceMessageId"],
                &item["sourceMessageId"],
                &item["id"],
            ]
            .into_iter()
            .map(id_of)
            .find(|id| !id.is_empty())
            .unwrap_or_default();
            if src_id.is_empty() {
                continue;
            }
            saved_ids.push(src_id.clone());
            if let Some(want) = filter_thread {
                if !src_conv.is_empty() && thread_of(src_conv) != want {
                    continue;
                }
            }
            let conv_target = if src_conv.is_empty() { SAVED_STREAM } else { src_conv };
            let resolved = if !item["content"].as_str().unwrap_or("").is_empty() {
                let mut msg = self.read_message(conv_target, item);
                if let Some(m) = msg.as_mut() {
                    m.id = src_id.clone();
                }
                msg
            } else if !src_conv.is_empty() {
                self.fetch_message(src_conv, &src_id).ok().flatten()
            } else {
                None
            };
            if let Some(mut msg) = resolved {
                msg.starred = true;
                out.push(msg);
            }
        }
        {
            let mut state = self.state();
            for id in saved_ids {
                state.saved_ids.insert(id);
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.sent));
        out.truncate(count as usize);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pinned_messages_handles_json_string_array_and_plain_ids() {
        let props = json!({
            "pinnedMessages": json!([
                {"messageId": "1760000100100", "pinnedBy": "8:orgid:ana", "pinnedTime": 1_760_000_200_000i64},
                {"id": "1760000100200", "pinnedByDisplayName": "Ben Okafor", "pinnedAt": "2025-10-09T09:05:00.0000000Z"},
                {"messageId": "1760000100100", "pinnedBy": "8:orgid:ana"}
            ]).to_string()
        });
        let parsed = parse_pinned_messages(&props);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].message_id, "1760000100100");
        assert_eq!(parsed[0].pinned_by.as_deref(), Some("8:orgid:ana"));
        assert_eq!(parsed[0].pinned_at, Some(1_760_000_200));
        assert_eq!(parsed[1].message_id, "1760000100200");
        assert_eq!(parsed[1].pinned_by.as_deref(), Some("Ben Okafor"));
        assert!(parsed[1].pinned_at.is_some());

        let ids_only = json!({"pinnedMessageIds": ["1760000100100", 1760000100300u64]});
        let parsed2 = parse_pinned_messages(&ids_only);
        assert_eq!(parsed2.len(), 2);
        assert_eq!(parsed2[0].message_id, "1760000100100");
        assert_eq!(parsed2[1].message_id, "1760000100300");

        let roundtrip = json!({"pinnedMessages": encode_pinned_messages(&parsed)});
        assert_eq!(parse_pinned_messages(&roundtrip), parsed);
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
}
