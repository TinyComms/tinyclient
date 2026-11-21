//! Teams' lists: the chats, which belong to no Space (meeting chats, app chats and the user's
//! own Notes among them), each team as a Space with its channels as the tree, and the unread
//! state Teams keeps for all of it. Everything comes from one call, and the Activity stream is
//! read once at connect for the mentions that came while tinyclient was closed.

use super::{chain_id, escaped, id_of, millis, seconds, thread_of, Auth, Body, Fault, Teams, AGGREGATOR, TREE_FRESH};
use crate::adapters::Command;
use crate::store::{ConversationRow, Filing, PreviewLine, SpaceRow};
use protocol::{CallSupport, ConversationKind, Relation, Special};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

/// The Space an earlier build kept the chats in. Gone: chats belong to no Space.
const OLD_CHATS: &str = "chats";
/// The user's own notes, Teams' "(You)" chat.
const NOTES: &str = "48:notes";
/// Teams' Activity stream: mentions, replies and reactions, as messages of a conversation.
const ACTIVITY: &str = "48:notifications";
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

/// Parses Teams' pinned channel ids in their Teams order from user-level properties
/// (`pinnedChannels`, `userDetails.pinnedChannels`, `userProperties.pinnedChannels`) and
/// per-channel `isPinned` / `properties.pinned` flags.
pub(super) fn pinned_channel_order(listed: &Value, teams: &[Value], shared_channels: &[Value]) -> Vec<String> {
    let mut order = Vec::new();
    let mut push = |id: &str| {
        if !id.is_empty() && !order.iter().any(|existing: &String| existing == id) {
            order.push(id.to_owned());
        }
    };
    let sources = [
        &listed["pinnedChannels"],
        &listed["userDetails"]["pinnedChannels"],
        &listed["userProperties"]["pinnedChannels"],
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
    let check_channel = |ch: &Value, push: &mut dyn FnMut(&str)| {
        let pinned = ch["isPinned"] == true
            || ch["properties"]["pinned"] == true
            || ch["properties"]["pinned"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"));
        if pinned {
            push(text(ch, "id"));
        }
    };
    for team in teams {
        for ch in team["channels"].as_array().into_iter().flatten() {
            check_channel(ch, &mut push);
        }
    }
    for ch in shared_channels {
        check_channel(ch, &mut push);
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

/// Classifies a Teams channel by `channelType` (0 = standard, 1 = private, 2 = shared)
/// and returns `(private, shared, mark)` where cross-tenant shared channels get `Mark::External`.
pub(super) fn channel_flags(channel: &Value, team_tenant: &str, own_tenant: &str) -> (bool, bool, Option<protocol::Mark>) {
    let kind = channel["channelType"].as_u64().unwrap_or(0);
    let shared = kind == 2
        || channel["isShared"] == true
        || channel["membershipType"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("shared"));
    let private = !shared
        && (kind == 1
            || channel["membershipType"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("private")));
    let host_tenant = [&channel["tenantId"], &channel["hostTenantId"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .unwrap_or(team_tenant);
    let mark = (shared
        && !host_tenant.is_empty()
        && !own_tenant.is_empty()
        && !host_tenant.eq_ignore_ascii_case(own_tenant))
    .then_some(protocol::Mark::External);
    (private, shared, mark)
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
        let (teams, chats, shared_channels) = (
            taken(&mut listed, "teams"),
            taken(&mut listed, "chats"),
            taken(&mut listed, "sharedChannels"),
        );
        let pinned_order = pinned_chat_order(&listed, &chats);
        let pinned_channels = pinned_channel_order(&listed, &teams, &shared_channels);
        drop(listed);
        self.state().pinned_channels = pinned_channels.clone();
        let me = self.me();
        let own_tenant = self.session.tenant.clone();
        // The chats first: written with no Space, they leave the one an earlier build kept them
        // in before that Space goes.
        self.chats(chats, &pinned_order, &me, first)?;
        let teams: Vec<&Value> = teams.iter().filter(|team| team["isDeleted"] != true && !text(team, "id").is_empty()).collect();
        let mut seen_channel_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for team in &teams {
            for ch in team["channels"].as_array().into_iter().flatten() {
                let cid = text(ch, "id");
                if !cid.is_empty() {
                    seen_channel_ids.insert(cid.to_owned());
                }
            }
        }
        // Group standalone shared channels (from host teams the user is not a member of) by host team.
        let mut extra_teams: Vec<(String, String, Vec<&Value>)> = Vec::new();
        let mut extra_for_existing: HashMap<String, Vec<&Value>> = HashMap::new();
        for ch in &shared_channels {
            if ch["isDeleted"] == true {
                continue;
            }
            let cid = text(ch, "id");
            if cid.is_empty() || seen_channel_ids.contains(cid) {
                continue;
            }
            seen_channel_ids.insert(cid.to_owned());
            let host_id = [&ch["parentTeamId"], &ch["hostTeamId"]]
                .into_iter()
                .filter_map(Value::as_str)
                .find(|s| !s.is_empty())
                .unwrap_or("shared")
                .to_owned();
            if teams.iter().any(|t| text(t, "id") == host_id) {
                extra_for_existing.entry(host_id).or_default().push(ch);
            } else if let Some((_, _, list)) = extra_teams.iter_mut().find(|(id, _, _)| *id == host_id) {
                list.push(ch);
            } else {
                let host_name = [&ch["parentTeamName"], &ch["hostTeamName"], &ch["teamDisplayName"]]
                    .into_iter()
                    .filter_map(Value::as_str)
                    .find(|s| !s.is_empty())
                    .unwrap_or("Shared channels")
                    .to_owned();
                extra_teams.push((host_id, host_name, vec![ch]));
            }
        }

        let mut spaces: Vec<SpaceRow> = teams
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
        for (offset, (host_id, host_name, _)) in extra_teams.iter().enumerate() {
            if host_id != OLD_CHATS {
                spaces.push(SpaceRow {
                    id: host_id.clone(),
                    name: host_name.clone(),
                    icon: None,
                    position: (teams.len() + offset) as i32,
                });
            }
        }
        self.ctx.spaces(&spaces)?;

        for team in teams {
            let id = text(team, "id");
            let team_tenant = text(team, "tenantId");
            // General first, then the order Teams lists them in. A channel the user hid is kept
            // apart (`opted_out`), behind the tree's "Show hidden". Unverified: that
            // `isFavorite: false` is what hiding a channel writes.
            let hidden = |channel: &Value| channel["isGeneral"] != true && channel["isFavorite"] == false && channel["isPinned"] != true;
            let mut channels: Vec<&Value> = team["channels"].as_array().into_iter().flatten().filter(|channel| channel["isDeleted"] != true).collect();
            if let Some(extra) = extra_for_existing.remove(id) {
                channels.extend(extra);
            }
            channels.sort_by_key(|channel| channel["isGeneral"] != true);
            let rows: Vec<ConversationRow> = channels
                .iter()
                .enumerate()
                .map(|(position, channel)| {
                    let chan_id = text(channel, "id");
                    let (private, shared, mark) = channel_flags(channel, team_tenant, &own_tenant);
                    let pin = pinned_channels.iter().position(|p| p == chan_id).map(|ix| ix as i32);
                    ConversationRow {
                        space: Some(id.to_owned()),
                        position: position as i32,
                        call: CallSupport::Handoff,
                        private,
                        shared,
                        mark,
                        read_only: team["isArchived"] == true || channel["isArchived"] == true,
                        opted_out: hidden(channel),
                        filing: Some(Filing { pin, ..Filing::default() }),
                        ..ConversationRow::new(chan_id, ConversationKind::Channel, text(channel, "displayName"))
                    }
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
        for (host_id, _, channels) in extra_teams {
            let rows: Vec<ConversationRow> = channels
                .iter()
                .enumerate()
                .map(|(position, channel)| {
                    let (private, _shared, mark) = channel_flags(channel, "", &own_tenant);
                    ConversationRow {
                        space: Some(host_id.clone()),
                        position: position as i32,
                        call: CallSupport::Handoff,
                        private,
                        shared: true,
                        mark,
                        read_only: channel["isArchived"] == true,
                        opted_out: channel["isFavorite"] == false && channel["isPinned"] != true,
                        ..ConversationRow::new(text(channel, "id"), ConversationKind::Channel, text(channel, "displayName"))
                    }
                })
                .collect();
            self.ctx.tree(&host_id, &[], &rows)?;
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
            if let Some(partner) = partner.filter(|partner| partner.starts_with("8:")) {
                partners.insert(partner.clone(), id.to_owned());
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
        // The list the user owns on Teams: the people they have a chat with. Apps are not people.
        let relations: Vec<(String, Relation)> = partners.keys().map(|person| (person.clone(), Relation::Contact)).collect();
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
        ConversationRow {
            avatar: avatar.map(|file| file.id),
            peer: partner.filter(|_| !bot).map(str::to_owned),
            call: if bot || notes { CallSupport::None } else { CallSupport::Handoff },
            bot,
            special: if notes { Special::Saved } else { Special::None },
            ..ConversationRow::new(id, kind, title)
        }
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

    /// Parses one `48:notifications` message into a [`protocol::ActivityItem`].
    fn parse_activity_item(
        &self,
        item: &Value,
        read_horizon: i64,
        read_ids: &std::collections::HashSet<String>,
    ) -> Option<protocol::ActivityItem> {
        let id = id_of(&item["id"]);
        if id.is_empty() {
            return None;
        }
        let activity = match &item["properties"]["activity"] {
            Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
            other => other.clone(),
        };
        let thread = text(&activity, "sourceThreadId");
        if thread.is_empty() {
            return None;
        }
        let raw_kind = text(&activity, "activityType").to_ascii_lowercase();
        let kind = if raw_kind.contains("mention") {
            protocol::ActivityItemKind::Mention
        } else if raw_kind.contains("reply") || raw_kind.contains("thread") {
            protocol::ActivityItemKind::ThreadReply
        } else if raw_kind.contains("react") || raw_kind.contains("like") || raw_kind.contains("emotion") {
            protocol::ActivityItemKind::Reaction
        } else if raw_kind.contains("add") || raw_kind.contains("team") || raw_kind.contains("member") || raw_kind.contains("invite") {
            protocol::ActivityItemKind::Invite
        } else if raw_kind.contains("app") || raw_kind.contains("bot") || raw_kind.contains("card") {
            protocol::ActivityItemKind::App
        } else {
            protocol::ActivityItemKind::Other
        };
        let message_id = {
            let src = id_of(&activity["sourceMessageId"]);
            if src.is_empty() { id.clone() } else { src }
        };
        let chain = id_of(&activity["sourceReplyChainId"]);
        let thread_id = (millis(&chain) > 0 && chain != message_id).then(|| chain_id(thread, &chain));
        let actor_id = {
            let src = text(&activity, "sourceUserId");
            if !src.is_empty() {
                src.to_owned()
            } else {
                text(item, "from").rsplit('/').next().unwrap_or("").to_owned()
            }
        };
        let actor = {
            let from_act = text(&activity, "sourceUserImDisplayName");
            if !from_act.is_empty() {
                from_act.to_owned()
            } else {
                let from_msg = text(item, "imdisplayname");
                if !from_msg.is_empty() {
                    from_msg.to_owned()
                } else {
                    self.name_of(&actor_id).unwrap_or_default()
                }
            }
        };
        let emoji = if kind == protocol::ActivityItemKind::Reaction {
            let key = [&activity["reactionKey"], &activity["emoji"], &activity["activitySubtype"]]
                .into_iter()
                .filter_map(Value::as_str)
                .find(|s| !s.is_empty())
                .unwrap_or("");
            if key.is_empty() { String::new() } else { super::emoji::emoji_of(key) }
        } else {
            String::new()
        };
        let snippet = [&activity["messagePreview"], &activity["activityTitle"]]
            .into_iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                let content = text(item, "content");
                if content.is_empty() {
                    None
                } else {
                    let parsed = super::html::parse(content, &super::people::Nobody, &self.me()).text;
                    let cleaned = parsed.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
                    (!cleaned.is_empty()).then_some(cleaned)
                }
            })
            .unwrap_or_else(|| {
                if raw_kind.contains("call") || raw_kind.contains("voicemail") {
                    "Missed call".to_owned()
                } else if kind == protocol::ActivityItemKind::Invite {
                    "Added you to a team".to_owned()
                } else {
                    String::new()
                }
            });
        let item_ms = millis(&id);
        let time = match item_ms / 1000 {
            0 => seconds(text(item, "composetime")),
            ts => ts,
        };
        let prop_read = item["properties"]["isRead"] == true
            || item["properties"]["isRead"].as_str().is_some_and(|v| v.eq_ignore_ascii_case("true"))
            || activity["isRead"] == true
            || activity["read"] == true;
        let horizon_read = read_horizon > 0 && item_ms > 0 && item_ms <= read_horizon;
        let unread = !(prop_read || horizon_read || read_ids.contains(&id));
        Some(protocol::ActivityItem {
            id,
            kind,
            conversation: self.ctx.key(thread),
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

    /// Fetches a page of the Teams `48:notifications` activity feed for `Command::ActivityFeed`.
    pub(super) fn fetch_activity_feed(
        &self,
        kind: Option<protocol::ActivityItemKind>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<crate::adapters::ActivityPage, Fault> {
        let page_size = limit.clamp(1, ACTIVITY_PAGE);
        let window = match cursor.filter(|c| !c.is_empty()) {
            Some(sync) => format!("startTime=1&syncState={sync}"),
            None => "startTime=1".to_owned(),
        };
        let (raw_items, older) = self.page(ACTIVITY, page_size, &window)?;
        let read_horizon = self.ctx.note("activity_read_horizon").as_deref().map(millis).unwrap_or(0);
        let read_ids = self.state().activity_read.clone();
        let items = raw_items
            .iter()
            .filter_map(|raw| self.parse_activity_item(raw, read_horizon, &read_ids))
            .filter(|item| kind.is_none_or(|wanted| item.kind == wanted))
            .collect();
        Ok(crate::adapters::ActivityPage {
            items,
            cursor: (!older.is_empty()).then_some(older),
        })
    }

    /// Marks one activity item (`Some(id)`) or all items (`None`) read on `48:notifications`.
    pub(super) fn mark_activity_read(&self, item_id: Option<&str>) -> Result<(), Fault> {
        if let Some(id) = item_id.filter(|id| !id.is_empty()) {
            let path = format!(
                "/v1/users/ME/conversations/{}/messages/{}/properties?name=isRead",
                escaped(ACTIVITY),
                escaped(id)
            );
            self.chat("PUT", &path, Body::Json(&serde_json::json!({"isRead": "true"})), false)?;
            self.state().activity_read.insert(id.to_owned());
        } else {
            let ts = super::now_ms();
            let path = format!(
                "/v1/users/ME/conversations/{}/properties?name=consumptionhorizon",
                escaped(ACTIVITY)
            );
            let horizon = serde_json::json!({"consumptionhorizon": format!("{ts};{ts};{ts}")});
            self.chat("PUT", &path, Body::Json(&horizon), false)?;
            self.ctx.set_note("activity_read_horizon", &ts.to_string());
        }
        Ok(())
    }

    /// Handles a live push event arriving on `48:notifications`.
    pub(super) fn handle_activity_push(&self, raw: &Value) -> Result<(), Fault> {
        let item_ms = millis(&id_of(&raw["id"]));
        if item_ms > 0 {
            let prev = self.ctx.note("activity_cursor").as_deref().map(millis).unwrap_or(0);
            if item_ms > prev {
                self.ctx.set_note("activity_cursor", &item_ms.to_string());
            }
        }
        let read_horizon = self.ctx.note("activity_read_horizon").as_deref().map(millis).unwrap_or(0);
        let read_ids = self.state().activity_read.clone();
        let Some(item) = self.parse_activity_item(raw, read_horizon, &read_ids) else {
            return Ok(());
        };
        if item.kind == protocol::ActivityItemKind::Mention {
            let place = item.thread_id.as_deref().unwrap_or(&item.conversation.id);
            if let Some(row) = self.ctx.known(place)?.or(self.ctx.known(&item.conversation.id)?) {
                let msg_ms = millis(&item.message_id);
                let read = row.last_read.as_deref().map(millis).unwrap_or(0);
                if msg_ms > read {
                    let next_mentions = row.mentions.saturating_add(1);
                    self.ctx.counts(
                        &row.key.id,
                        row.unread.max(next_mentions),
                        next_mentions,
                        row.last_read.as_deref(),
                        item.time.max(row.updated),
                    )?;
                }
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
        if row.title.is_empty() {
            row.title = if people.len() > 1 { format!("Group chat ({})", people.len() + 1) } else { "Chat".to_owned() };
        }
        self.ctx.conversation(&row)?;
        if let Some(partner) = partner.filter(|partner| partner.starts_with("8:")) {
            self.state().partners.insert(partner.to_owned(), thread.to_owned());
            self.ctx.relation(partner, Relation::Contact)?;
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
            row.shared = parent.shared;
            row.mark = parent.mark;
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
}
