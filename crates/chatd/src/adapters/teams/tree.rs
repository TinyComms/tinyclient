//! Teams' lists: the chats, which belong to no Space (meeting chats, app chats and the user's
//! own Notes among them), each team as a Space with its channels as the tree, and the unread
//! state Teams keeps for all of it. Everything comes from one call, and the Activity stream is
//! read once at connect for the mentions that came while tinyclient was closed.

use super::{chain_id, escaped, id_of, millis, now_ms, seconds, thread_of, Auth, Body, Fault, Teams, AGGREGATOR, SPACES, TREE_FRESH};
use crate::adapters::{ActivityPage, BrowsedPage, Command};
use crate::store::{ConversationRow, Filing, PreviewLine, SpaceRow};
use protocol::{ActivityItem, ActivityItemKind, BrowsedChannel, CallSupport, ConversationKind, Mark, Relation, Special};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The Space an earlier build kept the chats in. Gone: chats belong to no Space.
const OLD_CHATS: &str = "chats";
/// The user's own notes, Teams' "(You)" chat.
const NOTES: &str = "48:notes";
/// Teams' Activity stream: mentions, replies and reactions, as messages of a conversation.
pub(super) const ACTIVITY: &str = "48:notifications";
/// The Activity stream's page read at connect, and maximum pages walked when all entries on a
/// page are newer than the last run.
const ACTIVITY_PAGE: u32 = 50;
const ACTIVITY_MAX_PAGES: usize = 5;
/// The longest a reply chain's title is: the start of its post.
const CHAIN_TITLE: usize = 40;
/// Unread chats whose newest page is fetched after connecting, paced, so their rows have a
/// preview. Everything else is fetched when it is opened.
const WARMED: usize = 5;
/// People looked up in one listing, for the names of chats.
const NAMED: usize = 200;
/// A row recorded outside a listing sorts last until the next one.
const LAST: i32 = i32::MAX;

fn taken(listed: &mut Value, name: &str) -> Vec<Value> {
    match listed.get_mut(name).map(Value::take) {
        Some(Value::Array(found)) => found,
        _ => Vec::new(),
    }
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().unwrap_or("")
}

pub(super) fn is_channel(id: &str) -> bool {
    id.contains("@thread.tacv2") || id.contains("@thread.skype")
}

/// Parses Teams' pinned chat ids in their Teams order from user-level properties
/// (`pinnedChats`, `userDetails.pinnedChats`, `userProperties.pinnedChats`) and per-chat
/// `isPinned` / `properties.pinned` flags.
pub(super) fn pinned_chat_order(listed: &Value, chats: &[Value]) -> Vec<String> {
    let mut order = Vec::new();
    let mut push = |id: &str| {
        if !id.is_empty() && !order.iter().any(|existing: &String| existing == id) {
            order.push(id.to_owned());
        }
    };
    let sources = [
        &listed["pinnedChats"],
        &listed["userDetails"]["pinnedChats"],
        &listed["userProperties"]["pinnedChats"],
    ];
    for source in sources {
        match source {
            Value::Array(items) => {
                for item in items {
                    if let Some(id) = item.as_str() {
                        push(id);
                    }
                }
            }
            Value::String(raw) => {
                if let Ok(items) = serde_json::from_str::<Vec<String>>(raw) {
                    for id in items {
                        push(&id);
                    }
                }
            }
            _ => {}
        }
    }
    for chat in chats {
        let pinned = chat["isPinned"] == true
            || chat["properties"]["pinned"] == true
            || chat["properties"]["pinned"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"));
        if pinned {
            push(text(chat, "id"));
        }
    }
    order
}

/// What Teams says of how far a channel or chat was read.
struct Counted {
    unread: bool,
    last_read: Option<String>,
    /// Unix seconds of the newest message.
    updated: i64,
}

/// A channel's or chat's read state: its newest message against the read marker where both are
/// told, else Teams' own flag. Unverified which of them a real account carries for a chat.
fn counted(entry: &Value, me: &str) -> Counted {
    let last = &entry["lastMessage"];
    let newest = id_of(&last["id"]);
    let read = {
        let from_obj = id_of(&entry["consumptionHorizon"]["originalArrivalTime"]);
        if millis(&from_obj) > 0 {
            from_obj
        } else {
            entry["properties"]["consumptionhorizon"]
                .as_str()
                .and_then(|h| h.split(';').next())
                .unwrap_or("")
                .to_owned()
        }
    };
    let from = text(last, "from");
    let mine = entry["isLastMessageFromMe"] == true || (!me.is_empty() && from.rsplit('/').next() == Some(me));
    let unread = if mine || millis(&newest) == 0 {
        false
    } else if millis(&read) > 0 {
        millis(&newest) > millis(&read)
    } else {
        entry["isRead"] == false || entry["isMessageRead"] == false
    };
    Counted {
        unread,
        last_read: Some(read).filter(|read| millis(read) > 0),
        updated: match millis(&newest) / 1000 {
            0 => seconds(text(last, "composeTime")),
            updated => updated,
        },
    }
}

impl Teams {
    /// The shell opened a Space, or push has just connected.
    pub(super) fn open_space(&self) -> Result<(), Fault> {
        let (again, listed) = {
            let mut state = self.state();
            (std::mem::take(&mut state.resync), state.listed)
        };
        match listed {
            Some(at) if !again && at.elapsed() < TREE_FRESH => Ok(()),
            listed => self.list(listed.is_none()),
        }
    }

    /// Every team with its channels, and every chat, from the one call Teams' own client makes
    /// for its sidebar. Rows go to the store team by team and nothing is kept.
    fn list(&self, first: bool) -> Result<(), Fault> {
        let url = format!("{}/api/csa/api/v1/teams/users/me?isPrefetch=false&enableMembershipSummary=true", self.api);
        let mut listed = self.call("GET", &url, Auth::Both(AGGREGATOR), Body::None, true)?.body;
        let (teams, chats) = (taken(&mut listed, "teams"), taken(&mut listed, "chats"));
        let pinned_order = pinned_chat_order(&listed, &chats);
        drop(listed);
        let me = self.me();
        // The chats first: written with no Space, they leave the one an earlier build kept them
        // in before that Space goes.
        self.chats(chats, &pinned_order, &me, first)?;
        let teams: Vec<&Value> = teams.iter().filter(|team| team["isDeleted"] != true && !text(team, "id").is_empty()).collect();
        let spaces: Vec<SpaceRow> = teams
            .iter()
            .enumerate()
            .map(|(at, team)| SpaceRow {
                id: text(team, "id").to_owned(),
                name: text(team, "displayName").to_owned(),
                icon: None,
                position: at as i32,
            })
            .filter(|space| space.id != OLD_CHATS)
            .collect();
        self.state().spaces = spaces.clone();
        self.ctx.spaces(&spaces)?;

        for team in teams {
            let id = text(team, "id");
            let can_create_channels = team["isArchived"] != true
                && team["memberSettings"]["allowCreateUpdateChannels"] != false;
            self.ctx.set_space_can_create_channels(id, can_create_channels)?;
            // General first, then the order Teams lists them in. A channel the user hid is kept
            // apart (`opted_out`), behind the tree's "Show hidden". Unverified: that
            // `isFavorite: false` is what hiding a channel writes.
            let hidden = |channel: &Value| channel["isGeneral"] != true && channel["isFavorite"] == false && channel["isPinned"] != true;
            let mut channels: Vec<&Value> = team["channels"].as_array().into_iter().flatten().filter(|channel| channel["isDeleted"] != true).collect();
            channels.sort_by_key(|channel| channel["isGeneral"] != true);
            let rows: Vec<ConversationRow> = channels
                .iter()
                .enumerate()
                .map(|(position, channel)| {
                    let read_only = team["isArchived"] == true || channel["isArchived"] == true;
                    let is_general = channel["isGeneral"] == true;
                    let private = channel["channelType"].as_u64().is_some_and(|kind| kind != 0);
                    let mut row = ConversationRow {
                        space: Some(id.to_owned()),
                        position: position as i32,
                        call: CallSupport::Handoff,
                        private,
                        read_only,
                        opted_out: hidden(channel),
                        can_pin: !read_only,
                        ..ConversationRow::new(text(channel, "id"), ConversationKind::Channel, text(channel, "displayName"))
                    };
                    row.rights.pin = !read_only;
                    if !read_only {
                        if !is_general {
                            row.rights.rename = true;
                            row.rights.edit_topic = true;
                        }
                        if private || is_general {
                            row.rights.add_members = true;
                            row.rights.remove_members = true;
                            row.rights.manage_members = true;
                        }
                    }
                    row
                })
                .collect();
            self.ctx.tree(id, &[], &rows)?;
            // The group behind the team, which a link into the official app has to name.
            if let Some(group) = team["teamSiteInformation"]["groupId"].as_str() {
                self.ctx.set_note(&format!("group:{id}"), group);
            }
            for channel in channels {
                let chan_id = text(channel, "id");
                let level = super::info::parse_notify(channel, true)
                    .or_else(|| super::info::parse_notify(&channel["properties"], true))
                    .unwrap_or(protocol::Notify::All);
                self.state().notify.insert(chan_id.to_owned(), level);
                self.ctx.set_note(&format!("notify:{chan_id}"), super::info::notify_word(level));
                self.count(chan_id, &counted(channel, &me), level == protocol::Notify::Nothing)?;
            }
        }
        self.state().listed = Some(Instant::now());
        if first {
            if let Err(fault) = self.activity_feed() {
                tracing::warn!("chatd: Teams: activity: {}", super::reason(fault));
            }
            self.load_blocklist();
        }
        Ok(())
    }

    /// Records Teams' read state for a row. Teams does not say how many messages are unread or
    /// name us, so "some" is one; what push counted since the row was last read is kept.
    fn count(&self, id: &str, counted: &Counted, muted: bool) -> Result<(), Fault> {
        let (mut unread, mut mentions) = (u32::from(counted.unread), 0);
        if let Some(before) = self.ctx.known(id)?.filter(|before| counted.unread && before.last_read == counted.last_read) {
            unread = unread.max(before.unread);
            mentions = before.mentions;
        }
        self.ctx.counts(id, unread, mentions, counted.last_read.as_deref(), counted.updated)?;
        self.ctx.muted(id, muted)?;
        Ok(())
    }

    fn chats(&self, chats: Vec<Value>, pinned_order: &[String], me: &str, first: bool) -> Result<(), Fault> {
        let others = |chat: &Value| -> Vec<String> {
            let members = chat["members"].as_array().into_iter().flatten();
            members.filter_map(|member| member["mri"].as_str()).filter(|mri| *mri != me).map(str::to_owned).collect()
        };
        // System streams are not conversations, but for the user's own notes. The Activity
        // stream is read for its mentions (`activity_feed`); the call log is not read.
        let shown = |chat: &&Value| {
            let id = text(chat, "id");
            chat["isConversationDeleted"] != true && !id.is_empty() && (!id.starts_with("48:") || id == NOTES)
        };
        let mut chats: Vec<(&Value, Counted)> = chats.iter().filter(shown).map(|chat| (chat, counted(chat, me))).collect();
        // Teams' own order: the chat that spoke last is first.
        chats.sort_by_key(|(_, counted)| std::cmp::Reverse(counted.updated));
        // The people a chat is named after: its partner (a person or an app), or the first few
        // of a group with no title. Asked for together, once, and only those not already known.
        let mut wanted: Vec<String> = Vec::new();
        for (chat, _) in &chats {
            if chat["isOneOnOne"] == true || text(chat, "title").is_empty() {
                wanted.extend(others(chat).into_iter().take(3));
            }
        }
        wanted.truncate(NAMED);
        self.lookup(&wanted);

        let (mut partners, mut warm) = (HashMap::new(), Vec::new());
        for (position, (chat, counted)) in chats.iter().enumerate() {
            let (id, people) = (text(chat, "id"), others(chat));
            // A one-to-one with a person, or with an app (`28:`), which Teams may not flag as
            // one-to-one (unverified).
            let alone = people.len() == 1 && (chat["isOneOnOne"] == true || people[0].starts_with("28:"));
            let partner = people.first().filter(|_| alone);
            let mut row = self.chat_row(id, &title_of(chat), &people, partner.map(String::as_str));
            row.position = position as i32;
            row.meeting = is_meeting(chat);
            let pending_req = is_pending_request(chat);
            let chat_external = is_external_chat(chat, &people, &self.session.tenant)
                || people.iter().any(|p| self.cached(p).is_some_and(|k| k.external) || super::people::is_consumer_mri(p));
            if chat_external {
                row.mark = Some(Mark::External);
            } else if partner.is_some_and(|p| self.cached(p).is_some_and(|k| k.guest)) {
                row.mark = Some(Mark::Guest);
            }
            if pending_req {
                row.special = Special::Request;
                row.mark.get_or_insert(Mark::External);
            }
            let pin = pinned_order.iter().position(|pinned_id| pinned_id == id).map(|ix| ix as i32);
            row.filing = Some(Filing { hidden: chat["hidden"] == true, pin, ..Filing::default() });
            if row.title.is_empty() {
                // Never the user's own name: the last sender is used only when it was not me.
                let last = &chat["lastMessage"];
                let from = text(last, "from");
                let other = !from.is_empty() && from.rsplit('/').next() != Some(me);
                row.title = match (other, last["imDisplayName"].as_str()) {
                    (true, Some(name)) if !name.is_empty() => name.to_owned(),
                    _ if people.len() > 1 => format!("Group chat ({})", people.len() + 1),
                    _ => "Chat".to_owned(),
                };
            }
            if !pending_req {
                if let Some(partner) = partner.filter(|partner| partner.starts_with("8:")) {
                    partners.insert(partner.clone(), id.to_owned());
                }
            }
            if counted.unread && warm.len() < WARMED {
                warm.push(id.to_owned());
            }
            self.ctx.conversation(&row)?;
            self.count(id, counted, chat["members"].as_array().into_iter().flatten().any(|member| member["mri"] == me && member["isMuted"] == true))?;
            if let Some(line) = preview_of(chat, me) {
                self.ctx.preview(id, &line)?;
            }
        }
        // The list the user owns on Teams: the people they have a chat with, plus any blocked people.
        // Apps are not people.
        let blocked: HashSet<String> = self.state().blocked.clone();
        let mut relations: Vec<(String, Relation)> = partners
            .keys()
            .filter(|person| !blocked.contains(*person))
            .map(|person| (person.clone(), Relation::Contact))
            .collect();
        relations.extend(blocked.into_iter().map(|person| (person, Relation::Blocked)));
        self.ctx.relations(&relations)?;
        {
            let mut state = self.state();
            state.partners = partners;
            state.pinned_chats = pinned_order.to_vec();
        }
        if first {
            for id in warm {
                let _ = self.ctx.commands.send(Command::Warm(id));
            }
        }
        Ok(())
    }

    /// A chat's row: named after its partner, its own title, or the first people in it. The
    /// user's Notes are `Saved`; a chat with an app is `bot` and named after the app.
    fn chat_row(&self, id: &str, title: &str, people: &[String], partner: Option<&str>) -> ConversationRow {
        let name = |person: &String| self.name_of(person);
        let me = self.me();
        let (kind, title) = match partner {
            _ if id == NOTES => (ConversationKind::Direct, "Notes".to_owned()),
            Some(partner) => (ConversationKind::Direct, self.name_of(partner).unwrap_or_default()),
            None if !title.is_empty() => (ConversationKind::Group, title.to_owned()),
            None => {
                // The first two, then how many more: "Ana, Ben, +3".
                let mut names: Vec<String> = people.iter().filter(|person| **person != me).take(2).filter_map(name).collect();
                if people.len() > names.len() && !names.is_empty() {
                    names.push(format!("+{}", people.len() - names.len()));
                }
                (ConversationKind::Group, names.join(", "))
            }
        };
        let bot = partner.is_some_and(|partner| partner.starts_with("28:"));
        let notes = id == NOTES;
        let avatar = match notes {
            true => self.picture(&me, &title),
            false => partner.and_then(|partner| self.picture(partner, &title)),
        };
        let mut row = ConversationRow {
            avatar: avatar.map(|file| file.id),
            peer: partner.filter(|_| !bot).map(str::to_owned),
            call: if bot || notes { CallSupport::None } else { CallSupport::Handoff },
            bot,
            special: if notes { Special::Saved } else { Special::None },
            can_pin: !notes,
            ..ConversationRow::new(id, kind, title)
        };
        row.rights.pin = !notes;
        if kind == ConversationKind::Group {
            row.rights.rename = true;
            row.rights.add_members = true;
            row.rights.remove_members = true;
            row.rights.manage_members = true;
        }
        row
    }

    /// The Activity stream at connect: each mention of me newer than its conversation's read
    /// marker counts, so mentions that came while tinyclient was closed are not lost. Pages past
    /// 50 when every entry on a full page is newer than the last run (capped at [`ACTIVITY_MAX_PAGES`]).
    fn activity_feed(&self) -> Result<(), Fault> {
        let last_seen_ms = self.ctx.note("activity_cursor").as_deref().map(millis).unwrap_or(0);
        let mut newest_seen_ms = last_seen_ms;
        let mut found: HashMap<String, Vec<String>> = HashMap::new();
        let mut window = "startTime=1".to_owned();

        for _ in 0..ACTIVITY_MAX_PAGES {
            let (items, older) = self.page(ACTIVITY, ACTIVITY_PAGE, &window)?;
            if items.is_empty() {
                break;
            }
            let mut all_newer = true;
            for item in &items {
                let item_ms = millis(&id_of(&item["id"]));
                if item_ms > newest_seen_ms {
                    newest_seen_ms = item_ms;
                }
                if last_seen_ms > 0 && item_ms > 0 && item_ms <= last_seen_ms {
                    all_newer = false;
                }
                let activity = match &item["properties"]["activity"] {
                    Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
                    other => other.clone(),
                };
                let kind = text(&activity, "activityType").to_ascii_lowercase();
                if !kind.contains("mention") {
                    continue;
                }
                let (thread, message) = (text(&activity, "sourceThreadId"), id_of(&activity["sourceMessageId"]));
                if thread.is_empty() || millis(&message) == 0 {
                    continue;
                }
                // A mention in a reply chain is the chain's.
                let chain = id_of(&activity["sourceReplyChainId"]);
                let place = if millis(&chain) > 0 && chain != message { chain_id(thread, &chain) } else { thread.to_owned() };
                let ids = found.entry(place).or_default();
                if !ids.contains(&message) {
                    ids.push(message);
                }
            }
            if items.len() < ACTIVITY_PAGE as usize || older.is_empty() || !all_newer {
                break;
            }
            window = format!("startTime=1&syncState={older}");
        }
        if newest_seen_ms > last_seen_ms {
            self.ctx.set_note("activity_cursor", &newest_seen_ms.to_string());
        }
        for (place, messages) in found {
            let Some(row) = self.ctx.known(&place)?.or(self.ctx.known(thread_of(&place))?) else {
                continue;
            };
            let read = row.last_read.as_deref().map(millis).unwrap_or(0);
            let unseen = messages.iter().filter(|message| millis(message) > read).count() as u32;
            if unseen > row.mentions {
                let id = row.key.id.clone();
                let newest = messages.iter().map(|message| millis(message) / 1000).max().unwrap_or(0);
                self.ctx.counts(&id, row.unread.max(unseen), unseen, row.last_read.as_deref(), newest.max(row.updated))?;
            }
        }
        Ok(())
    }

    /// Reads one paged slice of Teams' `48:notifications` Activity stream, optionally filtered by
    /// [`ActivityItemKind`]. Each entry maps to an [`ActivityItem`] with target conversation,
    /// message id, thread id, actor, emoji, snippet, timestamp, and read state.
    pub(super) fn activity_feed_page(
        &self,
        kind: Option<ActivityItemKind>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ActivityPage, Fault> {
        let count = limit.clamp(1, ACTIVITY_PAGE);
        let window = match cursor.filter(|c| !c.is_empty()) {
            Some(sync_state) => format!("startTime=1&syncState={sync_state}"),
            None => "startTime=1".to_owned(),
        };
        let (raws, older) = self.page(ACTIVITY, count, &window)?;
        let (horizon_ms, read_ids) = {
            let mut state = self.state();
            if state.activity_read_horizon == 0 {
                if let Some(saved) = self.ctx.note("activity_read_horizon").as_deref().map(millis) {
                    state.activity_read_horizon = saved;
                }
            }
            (state.activity_read_horizon, state.activity_read_ids.clone())
        };
        let mut items = Vec::with_capacity(raws.len());
        let mut targets = Vec::new();
        let mut newest = 0i64;
        for raw in &raws {
            let Some(item) = self.parse_activity_item(raw, horizon_ms, &read_ids) else {
                continue;
            };
            newest = newest.max(millis(&item.id));
            targets.push((item.id.clone(), (thread_of(&item.conversation.id).to_owned(), item.message_id.clone())));
            if kind.is_some_and(|want| item.kind != want) {
                continue;
            }
            items.push(item);
        }
        {
            let mut state = self.state();
            if newest > state.activity_newest_ms {
                state.activity_newest_ms = newest;
            }
            if state.activity_targets.len() + targets.len() > 2_000 {
                state.activity_targets.clear();
            }
            for (id, target) in targets {
                state.activity_targets.insert(id, target);
            }
        }
        Ok(ActivityPage {
            items,
            cursor: (!older.is_empty()).then_some(older),
        })
    }

    /// Parses one raw message from Teams' `48:notifications` stream into an [`ActivityItem`].
    pub(super) fn parse_activity_item(
        &self,
        raw: &Value,
        horizon_ms: i64,
        read_ids: &HashSet<String>,
    ) -> Option<ActivityItem> {
        let activity = match &raw["properties"]["activity"] {
            Value::String(text) => serde_json::from_str::<Value>(text).unwrap_or(Value::Null),
            other => other.clone(),
        };
        let id = id_of(&raw["id"]);
        if id.is_empty() {
            return None;
        }
        let raw_type = text(&activity, "activityType");
        let (kind, emoji) = classify_activity(raw_type, &activity);
        let thread = text(&activity, "sourceThreadId");
        let fallback_conv = raw["conversationid"]
            .as_str()
            .filter(|c| !c.is_empty() && *c != ACTIVITY)
            .unwrap_or("");
        let target_thread = if !thread.is_empty() { thread } else { fallback_conv };
        if target_thread.is_empty() {
            return None;
        }
        let message_id = id_of(&activity["sourceMessageId"]);
        let chain = id_of(&activity["sourceReplyChainId"]);
        let is_chain = millis(&chain) > 0 && !message_id.is_empty() && chain != message_id;
        let thread_id = is_chain.then(|| chain_id(target_thread, &chain));
        let place = thread_id.as_deref().unwrap_or(target_thread);

        let from_raw = raw["from"].as_str().unwrap_or("");
        let actor_id = [
            activity["sourceUserId"].as_str(),
            activity["activityActorId"].as_str(),
            Some(from_raw.rsplit('/').next().unwrap_or(from_raw)),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_owned();
        let actor = [
            activity["sourceUserImDisplayName"].as_str(),
            activity["activityActorName"].as_str(),
            raw["imdisplayname"].as_str(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| (!actor_id.is_empty()).then(|| self.name_of(&actor_id)).flatten())
        .unwrap_or_default();

        let snippet = [
            activity["messagePreview"].as_str(),
            activity["activityDetails"].as_str(),
            activity["summary"].as_str(),
            activity["activitySubType"].as_str(),
            raw["content"].as_str(),
        ]
        .into_iter()
        .flatten()
        .map(|s| super::html::parse(s, &super::people::Nobody, "").text)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "))
        .find(|s| !s.is_empty())
        .unwrap_or_default();

        let item_ms = millis(&id);
        let time = [
            activity["activityTimestamp"].as_str().map(seconds).filter(|&t| t > 0),
            (item_ms > 0).then_some(item_ms / 1000),
            raw["composetime"].as_str().map(seconds).filter(|&t| t > 0),
        ]
        .into_iter()
        .flatten()
        .next()
        .unwrap_or(0);

        let prop_read = raw["properties"]["isRead"] == true
            || raw["properties"]["isRead"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"))
            || activity["isRead"] == true;
        let conv_already_read = !message_id.is_empty()
            && self
                .ctx
                .known(target_thread)
                .ok()
                .flatten()
                .and_then(|row| row.last_read)
                .is_some_and(|last_read| millis(&last_read) >= millis(&message_id) && millis(&message_id) > 0);
        let unread = !(prop_read
            || read_ids.contains(&id)
            || (horizon_ms > 0 && item_ms > 0 && item_ms <= horizon_ms)
            || conv_already_read);

        Some(ActivityItem {
            id,
            kind,
            conversation: self.ctx.key(place),
            message_id,
            thread_id,
            actor,
            actor_id,
            emoji,
            text: snippet,
            time,
            unread,
        })
    }

    /// Marks one activity feed entry (`Some(id)`) or the entire feed (`None`) read and syncs
    /// the read state to Teams' `48:notifications` stream.
    pub(super) fn activity_mark_read(&self, item_id: Option<&str>) -> Result<(), Fault> {
        let ts = now_ms();
        match item_id.filter(|id| !id.is_empty()) {
            Some(id) => {
                let path = format!(
                    "/v1/users/ME/conversations/{}/messages/{}/properties?name=isRead",
                    escaped(ACTIVITY),
                    escaped(id)
                );
                self.chat("PUT", &path, Body::Json(&json!({"isRead": "true"})), false)?;
                self.state().activity_read_ids.insert(id.to_owned());
            }
            None => {
                let marker = {
                    let state = self.state();
                    if state.activity_newest_ms > 0 { state.activity_newest_ms } else { ts }
                };
                let path = format!(
                    "/v1/users/ME/conversations/{}/properties?name=consumptionhorizon",
                    escaped(ACTIVITY)
                );
                let body = json!({"consumptionhorizon": format!("{marker};{ts};{marker}")});
                self.chat("PUT", &path, Body::Json(&body), false)?;
                self.state().activity_read_horizon = marker;
                self.ctx.set_note("activity_read_horizon", &marker.to_string());
            }
        }
        Ok(())
    }

    /// Marks any tracked activity items matching `(thread, message_id)` read locally and on Teams
    /// when their underlying conversation message is marked read.
    pub(super) fn mark_activity_for_conversation(&self, thread: &str, message_id: &str) {
        let read_ms = millis(message_id);
        let matching: Vec<String> = {
            let state = self.state();
            state
                .activity_targets
                .iter()
                .filter(|(act_id, (t, mid))| {
                    t == thread
                        && !state.activity_read_ids.contains(*act_id)
                        && (mid == message_id || (read_ms > 0 && millis(mid) > 0 && millis(mid) <= read_ms))
                })
                .map(|(act_id, _)| act_id.clone())
                .collect()
        };
        for act_id in matching {
            let _ = self.activity_mark_read(Some(&act_id));
        }
    }

    /// Handles a live `NewMessage` / `MessageUpdate` pushed for `48:notifications` instead of
    /// dropping it: updates mention/reply badges and raises notifications respecting muted state.
    pub(super) fn heard_activity(&self, raw: &Value) -> Result<(), Fault> {
        let (horizon_ms, read_ids) = {
            let state = self.state();
            (state.activity_read_horizon, state.activity_read_ids.clone())
        };
        let Some(item) = self.parse_activity_item(raw, horizon_ms, &read_ids) else {
            return Ok(());
        };
        let item_ms = millis(&item.id);
        {
            let mut state = self.state();
            if item_ms > state.activity_newest_ms {
                state.activity_newest_ms = item_ms;
            }
            state.activity_targets.insert(
                item.id.clone(),
                (thread_of(&item.conversation.id).to_owned(), item.message_id.clone()),
            );
        }
        if item_ms > 0 {
            self.ctx.set_note("activity_cursor", &item_ms.to_string());
        }
        if !item.unread {
            return Ok(());
        }
        let target_thread = thread_of(&item.conversation.id);
        if let Some(row) = self.ctx.known(&item.conversation.id)?.or(self.ctx.known(target_thread)?) {
            match item.kind {
                ActivityItemKind::Mention => {
                    let read = row.last_read.as_deref().map(millis).unwrap_or(0);
                    if millis(&item.message_id) == 0 || millis(&item.message_id) > read {
                        let next_mentions = row.mentions.saturating_add(1);
                        let next_unread = row.unread.max(next_mentions);
                        self.ctx.counts(
                            &row.key.id,
                            next_unread,
                            next_mentions,
                            row.last_read.as_deref(),
                            item.time.max(row.updated),
                        )?;
                    }
                }
                ActivityItemKind::ThreadReply | ActivityItemKind::MissedCall => {
                    if !row.muted {
                        self.ctx.mark_unread(&row.key.id)?;
                    }
                }
                ActivityItemKind::Reaction => {
                    // Reactions can be noisy; only notify when the conversation is set to All
                    // notifications (not muted or mentions-only).
                    let level = self
                        .state()
                        .notify
                        .get(target_thread)
                        .copied()
                        .unwrap_or(if row.muted { protocol::Notify::Nothing } else { protocol::Notify::All });
                    if level == protocol::Notify::All && !item.emoji.is_empty() {
                        self.ctx.hub.notify(crate::notify::Event::Reaction {
                            conversation: row.key.clone(),
                            emoji: item.emoji.clone(),
                            reactor: (!item.actor.is_empty()).then(|| item.actor.clone()),
                            snippet: item.text.clone(),
                            outgoing_target: true,
                        });
                    }
                }
                ActivityItemKind::App | ActivityItemKind::Invite | ActivityItemKind::Other => {}
            }
        }
        Ok(())
    }

    /// True once the conversation a message belongs to has a row. A chat never seen (someone
    /// has just started it) is described by Teams and recorded; a channel that is not in the
    /// tree is one the user hid or left, and stays out.
    pub(super) fn known(&self, conversation: &str) -> Result<bool, Fault> {
        let thread = thread_of(conversation);
        if self.ctx.known(thread)?.is_some() {
            return Ok(true);
        }
        if is_channel(thread) || thread.starts_with("48:") || !thread.starts_with("19:") {
            return Ok(false);
        }
        let me = self.me();
        let described = self.chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true);
        let described = match described {
            Ok(described) => described.body,
            Err(Fault::Refused { .. }) => Value::Null,
            Err(fault) => return Err(fault),
        };
        let members = described["members"].as_array().into_iter().flatten();
        let people: Vec<String> = members.filter_map(|member| member["id"].as_str()).filter(|id| *id != me).map(str::to_owned).collect();
        self.lookup(&people[..people.len().min(3)]);
        let properties = &described["properties"];
        let one = people.len() == 1 && (properties["uniquerosterthread"] == "true" || thread.ends_with("@unq.gbl.spaces") || people[0].starts_with("28:"));
        let partner = people.first().filter(|_| one).map(String::as_str);
        let mut row = self.chat_row(thread, text(properties, "topic"), &people, partner);
        row.meeting = thread.starts_with("19:meeting_");
        row.position = LAST;
        let pending_req = is_pending_request(&described);
        let chat_external = is_external_chat(&described, &people, &self.session.tenant)
            || people.iter().any(|p| self.cached(p).is_some_and(|k| k.external) || super::people::is_consumer_mri(p));
        if chat_external {
            row.mark = Some(Mark::External);
        } else if partner.is_some_and(|p| self.cached(p).is_some_and(|k| k.guest)) {
            row.mark = Some(Mark::Guest);
        }
        if pending_req {
            row.special = Special::Request;
            row.mark.get_or_insert(Mark::External);
        }
        if row.title.is_empty() {
            row.title = if people.len() > 1 { format!("Group chat ({})", people.len() + 1) } else { "Chat".to_owned() };
        }
        self.ctx.conversation(&row)?;
        if !pending_req {
            if let Some(partner) = partner.filter(|partner| partner.starts_with("8:")) {
                self.state().partners.insert(partner.to_owned(), thread.to_owned());
                self.ctx.relation(partner, Relation::Contact)?;
            }
        }
        Ok(true)
    }

    /// A reply chain's own conversation, so its replies have somewhere to be read. It names
    /// the channel it hangs off and the post that started it, so it stays through a listing
    /// and goes when its channel goes.
    /// It is titled by its post: the subject, else the post's first words; "Replies" until
    /// the post is known, and retitled once it is.
    pub(super) fn chain_known(&self, channel: &str, root: &str) -> Result<(), Fault> {
        let id = chain_id(channel, root);
        let title = self.ctx.stored_message(channel, root)?.map(|post| chain_title(&post.text)).filter(|title| !title.is_empty());
        if let Some(held) = self.ctx.known(&id)? {
            if let Some(title) = title.filter(|title| *title != held.title) {
                let mut row = ConversationRow::new(id, ConversationKind::Thread, title);
                row.parent = Some(channel.to_owned());
                row.root = Some(root.to_owned());
                (row.private, row.read_only) = (held.private, held.read_only);
                self.ctx.conversation(&row)?;
            }
            return Ok(());
        }
        let parent = self.ctx.known(channel)?;
        let mut row = ConversationRow::new(id, ConversationKind::Thread, title.unwrap_or_else(|| "Replies".to_owned()));
        row.parent = Some(channel.to_owned());
        row.root = Some(root.to_owned());
        if let Some(parent) = parent {
            row.private = parent.private;
            row.read_only = parent.read_only;
        }
        self.ctx.conversation(&row)?;
        Ok(())
    }

    /// Where the official app opens this conversation to call in it: a call to the person of a
    /// one-to-one chat, the meeting of a meeting chat, else the chat or channel itself, where
    /// its call button is. Microsoft's documented deep links; none was seen in a client's code.
    pub(super) fn call_link(&self, conversation: &str) -> String {
        let thread = thread_of(conversation);
        let row = self.ctx.known(thread).ok().flatten();
        let chat = || format!("msteams:/l/chat/{}/conversations", escaped(thread));
        match row {
            _ if thread.starts_with("19:meeting_") => format!("msteams:/l/meetup-join/{}/0", escaped(thread)),
            Some(row) if row.kind == ConversationKind::Channel => {
                let group = row.space.as_ref().and_then(|team| self.ctx.note(&format!("group:{team}"))).unwrap_or_default();
                format!(
                    "msteams:/l/channel/{}/{}?groupId={}&tenantId={}",
                    escaped(thread),
                    escaped(&row.title),
                    escaped(&group),
                    escaped(&self.session.tenant)
                )
            }
            Some(row) if row.kind == ConversationKind::Direct => {
                // The call link names people by sign-in name, which is known once looked up.
                let mail = row.peer.as_deref().and_then(|peer| self.cached(peer)).map(|known| known.mail).filter(|mail| !mail.is_empty());
                mail.map_or_else(chat, |mail| format!("msteams:/l/call/0/0?users={}", escaped(&mail)))
            }
            _ => chat(),
        }
    }

    /// Leaves a Teams team (`DELETE /v1/threads/{space_id}/members/{me}`) and drops it and its
    /// channels from the Spaces list.
    pub(super) fn leave_team(&self, space_id: &str) -> Result<(), Fault> {
        let path = format!("/v1/threads/{}/members/{}", escaped(space_id), escaped(&self.me()));
        self.chat("DELETE", &path, Body::None, false).map_err(|fault| match fault {
            Fault::Refused { status: 403 | 400, .. } => Fault::Refused {
                status: 403,
                message: "You cannot leave this team (you may be its last owner — manage ownership in Teams first).".to_owned(),
            },
            other => other,
        })?;
        let remaining = {
            let mut state = self.state();
            state.spaces.retain(|space| space.id != space_id);
            state.spaces.clone()
        };
        self.ctx.spaces(&remaining)?;
        Ok(())
    }

    /// Browses channels of a team (`space_id` non-empty, including hidden channels so they can
    /// be shown) or public teams in the organization (`space_id` empty), filtered by `query` and
    /// paged with a numeric offset cursor.
    pub(super) fn browse_channels(
        &self,
        space_id: &str,
        query: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<BrowsedPage, Fault> {
        let needle = query.trim().to_ascii_lowercase();
        let offset = cursor.and_then(|c| c.parse::<usize>().ok()).unwrap_or(0);
        let count = limit.clamp(1, 100) as usize;

        let mut all = Vec::new();
        if space_id.trim().is_empty() {
            let joined_spaces: HashSet<String> = self.state().spaces.iter().map(|s| s.id.clone()).collect();
            let url = format!("{}/beta/teams/public", self.place()?.middle);
            let answer = self.call("GET", &url, Auth::Both(SPACES), Body::None, true)?.body;
            let entries = [
                answer["teams"].as_array(),
                answer["value"].as_array(),
                answer.as_array(),
            ]
            .into_iter()
            .flatten()
            .next()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
            for team in entries {
                if team["isDeleted"] == true {
                    continue;
                }
                let id = text(team, "id");
                let name = text(team, "displayName").trim();
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let topic = team["description"].as_str().unwrap_or("").trim().to_owned();
                if !needle.is_empty()
                    && !name.to_ascii_lowercase().contains(&needle)
                    && !topic.to_ascii_lowercase().contains(&needle)
                {
                    continue;
                }
                let members = team["memberCount"]
                    .as_u64()
                    .or_else(|| team["membershipSummary"]["totalMemberCount"].as_u64())
                    .unwrap_or(0) as u32;
                all.push(BrowsedChannel {
                    key: self.ctx.key(id),
                    name: name.to_owned(),
                    topic,
                    members,
                    joined: joined_spaces.contains(id) || team["isMember"] == true,
                    private: false,
                    archived: team["isArchived"] == true,
                });
            }
        } else {
            let url = format!("{}/api/csa/api/v1/teams/{}/channels", self.api, escaped(space_id));
            let answer = self.call("GET", &url, Auth::Both(AGGREGATOR), Body::None, true)?.body;
            let entries = [
                answer["channels"].as_array(),
                answer["value"].as_array(),
                answer.as_array(),
            ]
            .into_iter()
            .flatten()
            .next()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
            for ch in entries {
                if ch["isDeleted"] == true {
                    continue;
                }
                let id = text(ch, "id");
                let name = text(ch, "displayName").trim();
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let topic = ch["description"].as_str().unwrap_or("").trim().to_owned();
                if !needle.is_empty()
                    && !name.to_ascii_lowercase().contains(&needle)
                    && !topic.to_ascii_lowercase().contains(&needle)
                {
                    continue;
                }
                let is_general = ch["isGeneral"] == true;
                let shown_in_sidebar = ch["isMember"] != false
                    && (is_general || ch["isFavorite"] != false || ch["isPinned"] == true);
                all.push(BrowsedChannel {
                    key: self.ctx.key(id),
                    name: name.to_owned(),
                    topic,
                    members: ch["memberCount"].as_u64().unwrap_or(0) as u32,
                    joined: shown_in_sidebar,
                    private: ch["channelType"].as_u64().is_some_and(|k| k != 0),
                    archived: ch["isArchived"] == true,
                });
            }
        }

        let total = all.len();
        let page: Vec<BrowsedChannel> = all.into_iter().skip(offset).take(count).collect();
        let next = (offset + page.len() < total).then(|| (offset + page.len()).to_string());
        Ok(BrowsedPage { channels: page, cursor: next })
    }

    /// Previews a hidden or unjoined Teams channel: stores it as read-only (`joined: false`) and
    /// fetches one page of history so the user can inspect it before showing or joining.
    pub(super) fn preview_channel(&self, conversation_id: &str) -> Result<(), Fault> {
        let thread = thread_of(conversation_id);
        let existing = self.ctx.known(thread)?;
        let described = self
            .chat("GET", &format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread)), Body::None, true)
            .map(|a| a.body)
            .unwrap_or(Value::Null);
        let title = described["properties"]["topic"]
            .as_str()
            .or_else(|| described["properties"]["displayName"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| existing.as_ref().map(|r| r.title.clone()))
            .unwrap_or_else(|| "Channel".to_owned());
        let mut row = ConversationRow {
            space: existing.as_ref().and_then(|r| r.space.clone()),
            position: existing.as_ref().map_or(LAST, |r| r.position),
            call: CallSupport::Handoff,
            private: existing.as_ref().is_some_and(|r| r.private),
            read_only: true,
            read_only_reason: "Show or join this channel to post messages.".to_owned(),
            opted_out: existing.as_ref().is_some_and(|r| r.opted_out),
            joined: false,
            ..ConversationRow::new(thread, ConversationKind::Channel, title)
        };
        if let Some(prev) = existing {
            row.topic = prev.topic;
        }
        self.ctx.conversation(&row)?;
        if let Ok((raws, _)) = self.page(thread, 30, "startTime=1") {
            let messages: Vec<protocol::Message> = raws.iter().filter_map(|raw| self.read_message(thread, raw)).collect();
            if !messages.is_empty() {
                self.ctx.history(thread, &messages)?;
            }
        }
        Ok(())
    }

    /// Shows a hidden channel of a team we are in (`isFavorite = true`), or joins an unjoined
    /// channel / public team (`PUT /v1/threads/{id}/members/{me}`).
    pub(super) fn join_channel_or_team(&self, conversation_id: &str) -> Result<(), Fault> {
        let thread = thread_of(conversation_id);
        let existing = self.ctx.known(thread)?;
        let is_hidden_channel = existing
            .as_ref()
            .is_some_and(|r| r.space.is_some() && (r.opted_out || !r.joined));
        if is_hidden_channel {
            let path = format!("/v1/users/ME/conversations/{}/properties?name=isFavorite", escaped(thread));
            self.chat("PUT", &path, Body::Json(&json!({"isFavorite": "true"})), false)?;
            if let Some(known) = existing {
                let row = ConversationRow {
                    space: known.space,
                    group: known.group,
                    position: known.position,
                    avatar: known.avatar.map(|a| a.id),
                    peer: known.peer,
                    call: known.call,
                    call_video: known.call_video,
                    limit: known.limit,
                    topic: known.topic,
                    parent: known.parent,
                    root: known.root,
                    read_only: false,
                    read_only_reason: String::new(),
                    private: known.private,
                    encryption: known.encryption,
                    bot: known.bot,
                    meeting: known.meeting,
                    special: known.special,
                    mark: known.mark,
                    opted_out: false,
                    rights: known.rights,
                    can_pin: true,
                    moderate: known.moderate,
                    joined: true,
                    ..ConversationRow::new(&known.key.id, known.kind, &known.title)
                };
                self.ctx.conversation(&row)?;
            }
            return Ok(());
        }
        let path = format!("/v1/threads/{}/members/{}", escaped(thread), escaped(&self.me()));
        self.chat("PUT", &path, Body::Json(&json!({"role": "User"})), false).map_err(|fault| match fault {
            Fault::Refused { status: 403, .. } => Fault::Refused {
                status: 403,
                message: "Your organization's policy or team settings do not allow joining this team or channel.".to_owned(),
            },
            other => other,
        })?;
        // Refresh teams/channels so a newly joined public team and its channels appear immediately.
        self.list(false)?;
        Ok(())
    }

    /// Creates a standard or private channel inside a team where members are allowed to.
    pub(super) fn create_channel(&self, space_id: &str, name: &str, private: bool) -> Result<String, Fault> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(Fault::Refused {
                status: 400,
                message: "Channel name cannot be empty.".to_owned(),
            });
        }
        let url = format!("{}/beta/teams/{}/channels", self.place()?.middle, escaped(space_id));
        let body = json!({
            "displayName": trimmed,
            "membershipType": if private { "private" } else { "standard" },
        });
        let answer = self
            .call("POST", &url, Auth::Both(SPACES), Body::Json(&body), false)
            .map_err(|fault| match fault {
                Fault::Refused { status: 403, .. } => Fault::Refused {
                    status: 403,
                    message: "Your organization's policy or team settings do not allow creating channels here.".to_owned(),
                },
                other => other,
            })?;
        let id = [
            answer.body["id"].as_str(),
            answer.body["channelId"].as_str(),
            answer.body["threadId"].as_str(),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .or_else(|| answer.location.rsplit('/').next().filter(|s| !s.is_empty()))
        .ok_or_else(|| Fault::Net("Teams created the channel without returning its id.".to_owned()))?
        .to_owned();
        let display = answer.body["displayName"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(trimmed);
        let mut row = ConversationRow {
            space: Some(space_id.to_owned()),
            position: LAST,
            call: CallSupport::Handoff,
            private,
            can_pin: true,
            ..ConversationRow::new(&id, ConversationKind::Channel, display)
        };
        row.rights.pin = true;
        row.rights.rename = true;
        row.rights.edit_topic = true;
        if private {
            row.rights.add_members = true;
            row.rights.remove_members = true;
            row.rights.manage_members = true;
        }
        self.ctx.conversation(&row)?;
        Ok(id)
    }

    /// Previews a Teams join code (`GET {middle}/beta/teams/joinCode/{code}`).
    pub(super) fn invite_preview(&self, raw: &str) -> Result<protocol::InvitePreview, Fault> {
        let code = parse_teams_join_code(raw).map_err(|message| Fault::Refused { status: 400, message })?;
        let url = format!("{}/beta/teams/joinCode/{}", self.place()?.middle, escaped(&code));
        let body = self
            .call("GET", &url, Auth::Both(SPACES), Body::None, true)
            .map_err(|fault| match fault {
                Fault::Refused { status: 404, .. } => Fault::Refused {
                    status: 400,
                    message: "That Teams join code was not found or has expired.".to_owned(),
                },
                Fault::Refused { status: 403, .. } => Fault::Refused {
                    status: 403,
                    message: "Your organization's policy does not allow joining this team by code.".to_owned(),
                },
                other => other,
            })?
            .body;
        let team = if body["team"].is_object() { &body["team"] } else { &body };
        let space_id = team["id"]
            .as_str()
            .or_else(|| team["teamId"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let name = team["displayName"]
            .as_str()
            .or_else(|| team["name"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&code)
            .to_owned();
        let description = team["description"].as_str().unwrap_or("").trim().to_owned();
        let members = team["memberCount"]
            .as_u64()
            .or_else(|| team["membershipSummary"]["totalMemberCount"].as_u64())
            .map(|n| n as u32);
        Ok(protocol::InvitePreview {
            code,
            name,
            description,
            icon: None,
            members,
            online: None,
            space_id: space_id.clone(),
            channel_id: space_id,
        })
    }

    /// Joins a Teams team by join code (`POST {middle}/beta/teams/joinByCode`), never retrying
    /// automatically, then refreshes the Spaces list.
    pub(super) fn invite_join(&self, raw: &str) -> Result<(Option<String>, Option<String>), Fault> {
        let code = parse_teams_join_code(raw).map_err(|message| Fault::Refused { status: 400, message })?;
        let url = format!("{}/beta/teams/joinByCode", self.place()?.middle);
        let body = self
            .call("POST", &url, Auth::Both(SPACES), Body::Json(&json!({"joinCode": code})), false)
            .map_err(|fault| match fault {
                Fault::Refused { status: 404, .. } => Fault::Refused {
                    status: 400,
                    message: "That Teams join code was not found or has expired.".to_owned(),
                },
                Fault::Refused { status: 403, .. } => Fault::Refused {
                    status: 403,
                    message: "Your organization's policy does not allow joining this team by code.".to_owned(),
                },
                other => other,
            })?
            .body;
        let team = if body["team"].is_object() { &body["team"] } else { &body };
        let space_id = team["id"]
            .as_str()
            .or_else(|| team["teamId"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let channel_id = team["generalChannelId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| space_id.clone());
        self.list(false)?;
        Ok((space_id, channel_id))
    }
}

/// Extracts a Teams join code from either a bare code (e.g. `ab12cd3`) or a Teams URL containing
/// a `code=` / `joinCode=` query parameter.
pub(super) fn parse_teams_join_code(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a Teams join code.".to_owned());
    }
    let candidate = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        let query = trimmed.split_once('?').map(|(_, q)| q).unwrap_or("");
        query
            .split('&')
            .find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                if k.eq_ignore_ascii_case("code") || k.eq_ignore_ascii_case("joincode") {
                    Some(v.split('#').next().unwrap_or(v).trim())
                } else {
                    None
                }
            })
            .unwrap_or("")
    } else {
        trimmed
    };
    if candidate.len() < 4
        || candidate.len() > 64
        || !candidate.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Enter a valid Teams join code (letters and digits).".to_owned());
    }
    Ok(candidate.to_owned())
}

/// A chat's own title: its topic, or a meeting's subject.
fn title_of(chat: &Value) -> String {
    let named = [&chat["title"], &chat["meetingInformation"]["subject"]];
    named.into_iter().filter_map(Value::as_str).map(str::trim).find(|title| !title.is_empty()).unwrap_or("").to_owned()
}

/// A meeting's chat: by its id, or by what the list says it is (unverified field).
fn is_meeting(chat: &Value) -> bool {
    text(chat, "id").starts_with("19:meeting_") || chat["chatType"] == "meeting" || chat["meetingInformation"].is_object()
}

/// True when a Teams chat object or thread description indicates a pending external/federated
/// chat request awaiting Accept or Block.
pub(super) fn is_pending_request(chat: &Value) -> bool {
    let flag = |v: &Value| *v == Value::Bool(true) || v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true"));
    let word = |v: &Value| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("pending"));
    let props = &chat["properties"];
    flag(&chat["isPending"])
        || flag(&chat["isExternalChatPending"])
        || flag(&props["isPendingExternal"])
        || flag(&props["isExternalChatPending"])
        || word(&chat["chatState"])
        || word(&chat["acceptanceStatus"])
        || word(&props["externalChatState"])
        || word(&props["chatState"])
}

/// True when a Teams chat or thread description involves someone outside our tenant.
pub(super) fn is_external_chat(chat: &Value, people: &[String], home_tenant: &str) -> bool {
    let flag = |v: &Value| *v == Value::Bool(true) || v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true"));
    let props = &chat["properties"];
    if flag(&chat["isExternal"])
        || flag(&chat["isFederated"])
        || flag(&props["isExternal"])
        || flag(&props["isFederated"])
        || flag(&props["isExternalThread"])
    {
        return true;
    }
    if people.iter().any(|p| super::people::is_consumer_mri(p)) {
        return true;
    }
    chat["members"].as_array().into_iter().flatten().any(|m| {
        let tid = m["tenantId"].as_str().unwrap_or("");
        !tid.is_empty() && !home_tenant.is_empty() && !tid.eq_ignore_ascii_case(home_tenant)
    })
}

/// The chat list's `lastMessage` as the row's preview line, its markup read for words alone.
fn preview_of(chat: &Value, me: &str) -> Option<PreviewLine> {
    let last = &chat["lastMessage"];
    let id = id_of(&last["id"]);
    let content = text(last, "content");
    if millis(&id) == 0 || content.is_empty() || !text(last, "messageType").starts_with("RichText") && text(last, "messageType") != "Text" {
        return None;
    }
    let words = super::html::parse(content, &super::people::Nobody, me).text;
    let words = words.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        return None;
    }
    let from = text(last, "from");
    Some(PreviewLine {
        id: id.clone(),
        text: words,
        sender: text(last, "imDisplayName").to_owned(),
        mine: chat["isLastMessageFromMe"] == true || (!me.is_empty() && from.rsplit('/').next() == Some(me)),
        sent: millis(&id) / 1000,
        expires_at: None,
    })
}

/// Classifies a Teams `properties.activity.activityType` into [`ActivityItemKind`] and optional emoji.
pub(super) fn classify_activity(raw_type: &str, activity: &Value) -> (ActivityItemKind, String) {
    let lower = raw_type.trim().to_ascii_lowercase();
    if lower.contains("mention") {
        return (ActivityItemKind::Mention, String::new());
    }
    if lower.contains("reply") || lower.contains("thread") {
        return (ActivityItemKind::ThreadReply, String::new());
    }
    if lower.contains("missed") || lower.contains("voicemail") || lower == "call" || lower.contains("callmissed") {
        return (ActivityItemKind::MissedCall, String::new());
    }
    if lower.contains("reaction") || matches!(lower.as_str(), "like" | "heart" | "laugh" | "surprised" | "sad" | "angry") {
        let key = [
            activity["reactionType"].as_str(),
            activity["activitySubType"].as_str(),
            activity["emoji"].as_str(),
            (!lower.contains("reaction")).then_some(raw_type),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("");
        let emoji = if key.is_empty() {
            String::new()
        } else {
            super::emoji::emoji_of(key)
        };
        return (ActivityItemKind::Reaction, emoji);
    }
    if lower.contains("team") || lower.contains("member") || lower.contains("invite") || lower.contains("added") {
        return (ActivityItemKind::Invite, String::new());
    }
    if lower.contains("app") || lower.contains("bot") || lower.contains("workflow") {
        return (ActivityItemKind::App, String::new());
    }
    (ActivityItemKind::Other, String::new())
}

/// A reply chain's title from its post's text: the subject line, else its first words.
fn chain_title(post: &str) -> String {
    let first = post.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    match first.chars().count() > CHAIN_TITLE {
        true => format!("{}…", first.chars().take(CHAIN_TITLE).collect::<String>().trim_end()),
        false => first.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_chain_is_titled_by_its_post_and_a_meeting_by_its_subject() {
        assert_eq!(chain_title("Release train\nWho drives this week?"), "Release train");
        assert_eq!(chain_title("A post with no subject that goes on for quite a while"), "A post with no subject that goes on for…");
        let meeting = json!({"id": "19:meeting_abc@thread.v2", "title": null, "meetingInformation": {"subject": "Weekly sync"}});
        assert!(is_meeting(&meeting));
        assert_eq!(title_of(&meeting), "Weekly sync");
        assert!(!is_meeting(&json!({"id": "19:grp@thread.v2"})));
    }

    #[test]
    fn unread_is_the_newest_message_against_the_read_marker() {
        let me = "8:orgid:me";
        let channel = |newest: u64, read: u64, from: &str| {
            json!({
                "consumptionHorizon": {"originalArrivalTime": read, "timeStamp": read + 5, "clientMessageId": "1"},
                "lastMessage": {"id": newest.to_string(), "from": from, "composeTime": "2026-10-09T08:56:40.0000000Z"},
                "isMessageRead": true,
            })
        };
        let behind = counted(&channel(1_760_000_300_300, 1_760_000_100_100, "8:orgid:ana"), me);
        assert!(behind.unread);
        assert_eq!((behind.last_read.as_deref(), behind.updated), (Some("1760000100100"), 1_760_000_300));
        assert!(!counted(&channel(1_760_000_300_300, 1_760_000_300_300, "8:orgid:ana"), me).unread);
        // What I said last is never unread, however old the marker.
        assert!(!counted(&channel(1_760_000_300_300, 1_760_000_100_100, "8:orgid:me"), me).unread);
        let link = "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/8:orgid:me";
        assert!(!counted(&channel(1_760_000_300_300, 1_760_000_100_100, link), me).unread);
        // A chat with no marker goes by Teams' own flag.
        let flagged = json!({"isRead": false, "lastMessage": {"id": "1760000300300", "from": "8:orgid:ana"}});
        assert!(counted(&flagged, me).unread && counted(&flagged, me).last_read.is_none());
        let read = json!({"isRead": true, "isLastMessageFromMe": false, "lastMessage": {"id": "1760000300300", "from": "8:orgid:ana"}});
        assert!(!counted(&read, me).unread);
        assert!(!counted(&json!({"isRead": false}), me).unread, "a chat with no message has nothing unread");
    }

    #[test]
    fn channels_are_told_from_chats_by_their_ids() {
        assert!(is_channel("19:abc@thread.tacv2") && is_channel("19:abc@thread.skype"));
        assert!(!is_channel("19:a_b@unq.gbl.spaces") && !is_channel("19:meeting_x@thread.v2"));
    }

    #[test]
    fn pinned_chats_are_read_in_teams_order_from_list_and_chat_flags() {
        let listed = json!({
            "userDetails": {
                "pinnedChats": "[\"19:grp2@thread.v2\",\"19:grp1@thread.v2\"]"
            },
            "chats": [
                {"id": "19:grp1@thread.v2", "isPinned": true},
                {"id": "19:grp2@thread.v2", "isPinned": true},
                {"id": "19:grp3@thread.v2", "properties": {"pinned": "true"}}
            ]
        });
        let chats = listed["chats"].as_array().unwrap();
        let pins = pinned_chat_order(&listed, chats);
        assert_eq!(pins, vec!["19:grp2@thread.v2", "19:grp1@thread.v2", "19:grp3@thread.v2"]);
    }

    #[test]
    fn classify_activity_maps_all_teams_notification_kinds() {
        assert_eq!(classify_activity("mention", &json!({})), (ActivityItemKind::Mention, String::new()));
        assert_eq!(classify_activity("channelMention", &json!({})), (ActivityItemKind::Mention, String::new()));
        assert_eq!(classify_activity("reply", &json!({})), (ActivityItemKind::ThreadReply, String::new()));
        assert_eq!(
            classify_activity("reaction", &json!({"reactionType": "heart"})),
            (ActivityItemKind::Reaction, "❤️".to_owned())
        );
        assert_eq!(classify_activity("like", &json!({})), (ActivityItemKind::Reaction, "👍".to_owned()));
        assert_eq!(classify_activity("missedCall", &json!({})), (ActivityItemKind::MissedCall, String::new()));
        assert_eq!(classify_activity("addedToTeam", &json!({})), (ActivityItemKind::Invite, String::new()));
        assert_eq!(classify_activity("appNotification", &json!({})), (ActivityItemKind::App, String::new()));
    }

    #[test]
    fn parse_teams_join_code_accepts_bare_code_and_url_and_rejects_garbage() {
        assert_eq!(parse_teams_join_code("TEAM42").unwrap(), "TEAM42");
        assert_eq!(
            parse_teams_join_code("https://teams.microsoft.com/l/team/join?code=ab12cd3").unwrap(),
            "ab12cd3"
        );
        assert_eq!(
            parse_teams_join_code("https://teams.microsoft.com/?joinCode=xy_99-z#frag").unwrap(),
            "xy_99-z"
        );
        assert!(parse_teams_join_code("").is_err());
        assert!(parse_teams_join_code("ab").is_err());
        assert!(parse_teams_join_code("bad code!").is_err());
        assert!(parse_teams_join_code("https://teams.microsoft.com/l/team/nocode").is_err());
    }
}
