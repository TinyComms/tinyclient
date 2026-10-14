//! A workspace as a Space: its sidebar as the tree, and the unread state Slack keeps for it.

use super::{fnv, seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{Command, Failure};
use crate::store::{ConversationRow, SpaceRow};
use protocol::{CallSupport, Conversation, ConversationKind, Group, GroupKind, MediaFile, MediaKind, Relation};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Conversations that need me whose history is fetched after connecting, paced, so their rows
/// have a preview: the newest few, once a run. Everything else is fetched when it is opened.
const WARMED: usize = 5;
/// The one group every direct and group conversation of the workspace sits under.
const DIRECTS: &str = "directs";

/// One conversation's unread state as `client.counts` gives it.
#[derive(Clone)]
pub(super) struct Counted {
    unread: u32,
    mentions: u32,
    last_read: Option<String>,
    latest: i64,
}
/// A row recorded outside a tree load sorts last in its section until the next load.
const LAST: i32 = i32::MAX;

/// One section of the sidebar, in the user's order.
struct Section {
    id: String,
    name: String,
    /// Slack's type: `standard` for the user's own, or the built-in it is.
    kind: String,
    channels: Vec<String>,
}

fn section(id: &str, name: &str) -> Section {
    Section {
        id: id.to_owned(),
        name: name.to_owned(),
        kind: id.to_owned(),
        channels: Vec::new(),
    }
}

impl Slack {
    /// A conversation's row in this workspace's Space. Every Slack conversation can hold a
    /// huddle, which the official app carries.
    pub(super) fn placed(&self, id: &str, kind: ConversationKind, title: &str) -> ConversationRow {
        ConversationRow {
            space: Some(self.ctx.account.account.clone()),
            position: LAST,
            call: CallSupport::Handoff,
            ..ConversationRow::new(id, kind, title)
        }
    }

    /// The row for one of Slack's conversation objects. Looks up the partner of a direct one.
    fn row(&self, channel: &Value) -> Option<ConversationRow> {
        let id = channel["id"].as_str()?;
        if channel["is_im"] == true {
            return Some(self.direct_row(id, channel["user"].as_str()?));
        }
        let name = channel["name"].as_str().or(channel["name_normalized"].as_str())?;
        let said = |what: &str| channel[what]["value"].as_str().filter(|text| !text.is_empty());
        if channel["is_mpim"] == true {
            // Group DMs are named `mpdm-ana--ben--cy-1`; everyone but me is the title.
            let people = name.strip_prefix("mpdm-").unwrap_or(name);
            let people = people.rsplit_once('-').map_or(people, |(people, _)| people);
            let me = self.state().handle.clone();
            let others: Vec<&str> = people.split("--").filter(|handle| *handle != me).collect();
            let mut row = self.placed(id, ConversationKind::Group, &others.join(", "));
            row.group = self.ctx.note("group:directs");
            row.private = true;
            return Some(row);
        }
        let mut row = self.placed(id, ConversationKind::Channel, &format!("#{name}"));
        row.group = self.ctx.note("group:channel");
        // The topic is what Slack shows in the header; the purpose stands in when there is none.
        row.topic = said("topic").or(said("purpose")).unwrap_or("").to_owned();
        row.private = channel["is_private"] == true;
        // Unverified: `is_read_only` is the documented flag; a channel only some may post in
        // (`posting_restricted_to`) is not told apart from one I may post in.
        row.read_only = ["is_read_only", "is_archived", "is_frozen"].iter().any(|flag| channel[*flag] == true);
        Some(row)
    }

    /// A stored row as the row to write again, with nothing lost.
    fn again(&self, known: &Conversation) -> ConversationRow {
        ConversationRow {
            space: known.space.clone(),
            group: known.group.clone(),
            position: known.position,
            avatar: known.avatar.as_ref().map(|avatar| avatar.id.clone()),
            peer: known.peer.clone(),
            call: known.call,
            topic: known.topic.clone(),
            parent: known.parent.clone(),
            root: known.root.clone(),
            read_only: known.read_only,
            private: known.private,
            ..ConversationRow::new(&known.key.id, known.kind, &known.title)
        }
    }

    /// Records a conversation the first time something arrives for it. False for one Slack
    /// will not describe to this workspace (a channel left since, or another workspace's):
    /// nothing is recorded for it, and Slack is not asked about it again this session.
    pub(super) fn known(&self, channel: &str) -> Result<bool, Failure> {
        if self.ctx.known(channel)?.is_some() {
            return Ok(true);
        }
        if self.state().unknown.contains(channel) {
            return Ok(false);
        }
        let info = match self.api("conversations.info", &[("channel", channel)]) {
            Ok(info) => info,
            Err(Fault::Slack(_)) => {
                self.unknown(channel);
                return Ok(false);
            }
            Err(fault) => return Err(fault.into()),
        };
        match self.row(&info["channel"]) {
            Some(row) => self.record(&row, info["channel"]["user"].as_str()).map(|()| true),
            None => {
                self.unknown(channel);
                Ok(false)
            }
        }
    }

    fn record(&self, row: &ConversationRow, partner: Option<&str>) -> Result<(), Failure> {
        match partner.filter(|_| row.kind == ConversationKind::Direct) {
            Some(partner) => self.direct_opened(&row.id, partner)?,
            None => self.ctx.conversation(row)?,
        }
        Ok(())
    }

    /// The thread's own conversation, so its replies have somewhere to be read. It names the
    /// channel it hangs off and the message that started it, so it stays through a sidebar
    /// load and goes when its channel goes. It is listed in no Space: the tree is the sidebar.
    pub(super) fn thread_known(&self, channel: &str, root: &str) -> Result<(), Failure> {
        let id = thread_id(channel, root);
        if self.ctx.known(&id)?.is_some() {
            return Ok(());
        }
        let parent = self.ctx.known(channel)?;
        let title = match &parent {
            Some(parent) if parent.kind.is_direct() => format!("Thread with {}", parent.title),
            Some(parent) => format!("Thread in {}", parent.title),
            None => "Thread".to_owned(),
        };
        let mut row = ConversationRow::new(id, ConversationKind::Thread, title);
        row.parent = Some(channel.to_owned());
        row.root = Some(root.to_owned());
        if let Some(parent) = parent {
            row.private = parent.private;
            row.read_only = parent.read_only;
        }
        self.ctx.conversation(&row)?;
        Ok(())
    }

    /// The workspace as the account's one Space, with its icon.
    pub(super) fn space(&self, name: &str) -> Result<(), Failure> {
        // Looked up once: `-` when the workspace has only Slack's default picture.
        let icon = self.ctx.note("icon").or_else(|| {
            let info = self.api("team.info", &[]).ok()?;
            let icon = &info["team"]["icon"];
            let urls = [&icon["image_88"], &icon["image_68"], &icon["image_132"]];
            let url = urls.into_iter().filter_map(Value::as_str).find(|url| self.hosted(url));
            let url = url.filter(|_| icon["image_default"] != true).unwrap_or("-");
            self.ctx.set_note("icon", url);
            Some(url.to_owned())
        });
        let icon = icon.filter(|url| url != "-").map(|url| {
            let file = MediaFile::new(format!("icon:{:x}", fnv(&url)), MediaKind::Icon);
            (file, url)
        });
        if let Some(registered) = &icon {
            self.ctx.media(std::slice::from_ref(registered))?;
        }
        self.ctx.spaces(&[SpaceRow {
            id: self.ctx.account.account.clone(),
            name: name.to_owned(),
            icon: icon.map(|(file, _)| file.id),
            position: 0,
        }])?;
        Ok(())
    }

    /// The sidebar's sections in the user's order. Unverified against a real workspace:
    /// `users.channelSections.list` is the web client's own call, and its sections are taken to
    /// be a linked list through `next_channel_section_id` with the first page of each one's
    /// channels inline. When it is refused the sidebar is Slack's default, "Channels".
    ///
    /// Slack's own sections for direct messages and apps are left out: every direct
    /// conversation sits under the one group that is theirs (`GroupKind::Directs`).
    fn sections(&self) -> Vec<Section> {
        let defaults = || vec![section("channels", "Channels")];
        let Ok(listed) = self.api("users.channelSections.list", &[]) else {
            return defaults();
        };
        let Some(all) = listed["channel_sections"].as_array().filter(|all| !all.is_empty()) else {
            return defaults();
        };
        let id_of = |section: &Value| section["channel_section_id"].as_str().unwrap_or("").to_owned();
        let followers: HashSet<&str> = all.iter().filter_map(|section| section["next_channel_section_id"].as_str()).collect();
        let by_id: HashMap<String, &Value> = all.iter().map(|section| (id_of(section), section)).collect();
        let mut ordered: Vec<&Value> = Vec::with_capacity(all.len());
        let mut next = all.iter().find(|section| !followers.contains(id_of(section).as_str()));
        while let Some(section) = next.filter(|_| ordered.len() < all.len()) {
            ordered.push(section);
            next = section["next_channel_section_id"].as_str().and_then(|id| by_id.get(id).copied());
        }
        // Anything the chain missed keeps the order Slack listed it in.
        for section in all {
            if !ordered.iter().any(|placed| id_of(placed) == id_of(section)) {
                ordered.push(section);
            }
        }
        let mut sections: Vec<Section> = ordered
            .into_iter()
            .map(|listed| {
                let kind = listed["type"].as_str().unwrap_or("standard");
                let name = match (listed["name"].as_str().filter(|name| !name.is_empty()), kind) {
                    (Some(name), _) => name,
                    (None, "channels") => "Channels",
                    (None, "direct_messages") => "Direct messages",
                    (None, "recent_apps") => "Apps",
                    (None, "stars") => "Starred",
                    (None, "slack_connect") => "External connections",
                    (None, _) => "More",
                };
                let channels = listed["channel_ids_page"]["channel_ids"].as_array();
                Section {
                    id: id_of(listed),
                    name: name.to_owned(),
                    kind: kind.to_owned(),
                    channels: channels.into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
                }
            })
            .collect();
        sections.retain(|section| !matches!(section.kind.as_str(), "direct_messages" | "recent_apps"));
        // The built-in holds whatever no other section claims, so it must exist.
        if !sections.iter().any(|section| section.kind == "channels") {
            sections.push(section("channels", "Channels"));
        }
        sections
    }

    /// The whole sidebar: every channel the user is in under the user's sections, then the open
    /// direct conversations under their one group. Read when the Space is opened, or at the
    /// first connection of a workspace never listed here; a connection otherwise reads only
    /// `client.counts`. Rows go to the store as they are built and are then let go.
    pub(super) fn load_tree(&self) -> Result<(), Failure> {
        let counts = self.api("client.counts", &[])?;
        let listed = |kind: &str| counts[kind].as_array().into_iter().flatten().filter_map(|c| c["id"].as_str());
        // Slack lists every direct conversation ever had; the sidebar shows the open ones, which
        // is what `client.counts` reports (unverified: taken from what the web client draws).
        let open: HashSet<&str> = listed("ims").chain(listed("mpims")).collect();
        let sections = self.sections();
        let default = |kind: &str| sections.iter().find(|section| section.kind == kind).map(|section| section.id.clone());
        self.ctx.set_note("group:channel", &default("channels").unwrap_or_default());
        self.ctx.set_note("group:directs", DIRECTS);

        let (mut channels, mut cursor) = (Vec::<Value>::new(), String::new());
        loop {
            let mut page = self.api(
                "users.conversations",
                &[
                    ("types", "public_channel,private_channel,mpim,im"),
                    ("exclude_archived", "true"),
                    ("limit", "200"),
                    ("cursor", &cursor),
                ],
            )?;
            cursor = page["response_metadata"]["next_cursor"].as_str().unwrap_or("").to_owned();
            if let Some(Value::Array(found)) = page.get_mut("channels").map(Value::take) {
                let direct = |channel: &Value| channel["is_im"] == true || channel["is_mpim"] == true;
                let shown = |channel: &Value| {
                    !direct(channel) || open.is_empty() || channel["id"].as_str().is_some_and(|id| open.contains(id))
                };
                channels.extend(found.into_iter().filter(shown));
            }
            if cursor.is_empty() {
                break;
            }
        }
        let partner = |channel: &Value| channel["user"].as_str().filter(|_| channel["is_im"] == true).map(str::to_owned);
        let partners: Vec<String> = channels.iter().filter_map(partner).collect();
        // Before any row: this registers each partner's picture and records them as people.
        self.users(&partners);

        let mut rows: Vec<(usize, ConversationRow)> = Vec::with_capacity(channels.len());
        let mut direct = HashMap::new();
        for channel in &channels {
            let Some(mut row) = self.row(channel) else {
                continue;
            };
            if let Some(person) = partner(channel) {
                direct.insert(person, row.id.clone());
            }
            if row.kind.is_direct() {
                // After every section.
                rows.push((sections.len(), row));
                continue;
            }
            // A section the user filed it under wins over the built-in for its kind.
            let filed = sections.iter().position(|section| section.channels.contains(&row.id));
            let at = filed.or_else(|| sections.iter().position(|section| Some(&section.id) == row.group.as_ref()));
            let at = at.unwrap_or(0);
            row.group = Some(sections[at].id.clone());
            rows.push((at, row));
        }
        drop(channels);
        // Slack sorts a section by name unless told otherwise; the position runs through the
        // whole Space, sections included.
        rows.sort_by_cached_key(|(at, row)| (*at, row.title.to_lowercase()));
        let rows: Vec<ConversationRow> = rows
            .into_iter()
            .enumerate()
            .map(|(position, (_, row))| ConversationRow {
                position: position as i32,
                ..row
            })
            .collect();
        let used = |id: &str| rows.iter().any(|row| row.group.as_deref() == Some(id));
        let mut groups: Vec<Group> = sections
            .iter()
            .enumerate()
            .filter(|(_, section)| used(&section.id))
            .map(|(position, section)| Group::new(&section.id, &section.name, position as i32))
            .collect();
        if used(DIRECTS) {
            groups.push(Group {
                kind: GroupKind::Directs,
                ..Group::new(DIRECTS, "Direct messages", sections.len() as i32)
            });
        }
        self.ctx.tree(&self.ctx.account.account, &groups, &rows)?;
        drop(rows);

        // The list the user owns on Slack: the people they have a direct conversation with.
        let relations: Vec<(String, Relation)> = direct.keys().map(|person| (person.clone(), Relation::Contact)).collect();
        self.ctx.relations(&relations)?;
        {
            let mut state = self.state();
            state.partners = direct;
            state.partners_read = true;
            state.tree = Some(Instant::now());
        }
        self.keep_partners();
        self.ctx.set_note("tree", "1");
        self.mutes()?;
        self.apply_counts(&counts)
    }

    /// At a connection: what was read or arrived while there was no socket.
    pub(super) fn resync(&self) -> Result<(), Failure> {
        let counts = self.api("client.counts", &[])?;
        self.mutes()?;
        self.apply_counts(&counts)
    }

    pub(super) fn count(&self, id: &str, counted: &Counted) -> Result<(), Failure> {
        self.ctx.counts(id, counted.unread, counted.mentions, counted.last_read.as_deref(), counted.latest)?;
        Ok(())
    }

    /// Slack's own unread state for every conversation: whether anything is unread, how many
    /// mention me (for a direct conversation, how many messages), and the read marker.
    ///
    /// The first time in a run, the few that need me most are queued to be warmed. One of them
    /// with no row yet (joined or opened since the sidebar was read) gets its row then, one
    /// lookup at a time between the warming gaps, never all at once here.
    fn apply_counts(&self, counts: &Value) -> Result<(), Failure> {
        let mut needing = Vec::new();
        for kind in ["channels", "mpims", "ims"] {
            for entry in counts[kind].as_array().into_iter().flatten() {
                let Some(id) = entry["id"].as_str() else {
                    continue;
                };
                let mentions = entry["mention_count"].as_u64().unwrap_or(0) as u32;
                let counted = Counted {
                    unread: mentions.max(u32::from(entry["has_unreads"] == true)),
                    mentions,
                    last_read: entry["last_read"].as_str().map(str::to_owned),
                    latest: seconds(entry["latest"].as_str().unwrap_or("")),
                };
                self.count(id, &counted)?;
                if mentions > 0 || (counted.unread > 0 && kind != "channels") {
                    needing.push((id, counted));
                }
            }
        }
        // CONTRACT: `client.counts.threads` says only that some thread is unread, not which, so
        // thread rows learn their unread state when they are opened (`conversations.replies`).
        if std::mem::replace(&mut self.state().warmed, true) {
            return Ok(());
        }
        needing.sort_by_key(|(_, counted)| std::cmp::Reverse(counted.latest));
        for (id, counted) in needing.into_iter().take(WARMED) {
            self.state().warm.insert(id.to_owned(), counted);
            let _ = self.ctx.commands.send(Command::Warm(format!("{}{id}", super::WARM)));
        }
        Ok(())
    }

    /// The channels the user muted in Slack. Unverified for this kind of session:
    /// `users.prefs.get` and its `muted_channels` list are the documented user-token forms.
    fn mutes(&self) -> Result<(), Failure> {
        let prefs = match self.api("users.prefs.get", &[]) {
            Ok(prefs) => prefs,
            Err(fault) => {
                let failure = Failure::from(fault);
                return match failure {
                    Failure::SignedOut(_) => Err(failure),
                    // Mutes are a refinement; the sidebar is not held up for them.
                    Failure::Dropped(_) => Ok(()),
                };
            }
        };
        let prefs = &prefs["prefs"];
        self.muted("muted_channels", &prefs["muted_channels"])?;
        if prefs["muted_channels"].is_null() {
            self.muted("all_notifications_prefs", &prefs["all_notifications_prefs"])?;
        }
        Ok(())
    }

    /// One of the two prefs that carry mutes, from `users.prefs.get` or a `pref_change` event:
    /// `muted_channels` is a comma-separated list, `all_notifications_prefs` a JSON document
    /// with a `muted` flag per channel.
    pub(super) fn muted(&self, pref: &str, value: &Value) -> Result<(), Failure> {
        let now: HashSet<String> = match pref {
            "muted_channels" => {
                let list = value.as_str().unwrap_or("");
                list.split(',').filter(|id| !id.is_empty()).map(str::to_owned).collect()
            }
            "all_notifications_prefs" => {
                let parsed = value.as_str().and_then(|text| serde_json::from_str::<Value>(text).ok());
                let Some(parsed) = parsed.as_ref().or(Some(value)).filter(|parsed| parsed["channels"].is_object()) else {
                    return Ok(());
                };
                let channels = parsed["channels"].as_object().into_iter().flatten();
                channels.filter(|(_, prefs)| prefs["muted"] == true).map(|(id, _)| id.clone()).collect()
            }
            _ => return Ok(()),
        };
        // What was muted when chatd last ran is in the store's rows, so the comparison is with
        // the list as it was then.
        let before: HashSet<String> = {
            let mut state = self.state();
            if !state.mutes_read {
                let stored = self.ctx.note("muted").unwrap_or_default();
                state.muted = stored.split(',').filter(|id| !id.is_empty()).map(str::to_owned).collect();
                state.mutes_read = true;
            }
            std::mem::replace(&mut state.muted, now.clone())
        };
        for id in before.difference(&now) {
            self.ctx.muted(id, false)?;
        }
        for id in &now {
            self.ctx.muted(id, true)?;
        }
        let mut list: Vec<&str> = now.iter().map(String::as_str).collect();
        list.sort_unstable();
        self.ctx.set_note("muted", &list.join(","));
        Ok(())
    }

    /// `channel_marked`, `im_marked` and the like: read on another device.
    pub(super) fn marked(&self, event: &Value) -> Result<(), Failure> {
        let Some(channel) = event["channel"].as_str() else {
            return Ok(());
        };
        let number = |names: &[&str]| names.iter().find_map(|name| event[*name].as_u64()).unwrap_or(0) as u32;
        let mentions = number(&["mention_count_display", "mention_count", "dm_count"]);
        let unread = number(&["unread_count_display", "unread_count"]).max(mentions);
        self.ctx.counts(channel, unread, mentions, event["ts"].as_str(), 0)?;
        Ok(())
    }

    /// `thread_marked`: a thread read, or marked unread, on another device. Unverified shape.
    pub(super) fn thread_marked(&self, event: &Value) -> Result<(), Failure> {
        let subscription = &event["subscription"];
        let (Some(channel), Some(root)) = (subscription["channel"].as_str(), subscription["thread_ts"].as_str()) else {
            return Ok(());
        };
        let unread = subscription["unread_count"].as_u64().unwrap_or(0) as u32;
        self.ctx.counts(&thread_id(channel, root), unread, 0, subscription["last_read"].as_str(), 0)?;
        self.thread_unread(channel, root, unread)
    }

    /// `channel_joined`, `group_joined`, `im_open`, `mpim_open`: a row to add. The event carries
    /// the channel whole or only its id, depending on the socket's mode.
    pub(super) fn joined(&self, event: &Value) -> Result<(), Failure> {
        let channel = &event["channel"];
        let Some(id) = channel.as_str().or(channel["id"].as_str()) else {
            return Ok(());
        };
        if let (Some(person), true) = (event["user"].as_str(), event["type"] == "im_open") {
            self.direct_opened(id, person)?;
            return Ok(());
        }
        match self.row(channel) {
            Some(row) => self.record(&row, channel["user"].as_str()),
            None => {
                // Joined now, whatever Slack said of it before.
                self.state().unknown.remove(id);
                self.known(id).map(drop)
            }
        }
    }

    /// `channel_left`, `channel_deleted`, `channel_archive` and their private twins.
    pub(super) fn left(&self, event: &Value) -> Result<(), Failure> {
        let channel = &event["channel"];
        if let Some(id) = channel.as_str().or(channel["id"].as_str()) {
            self.ctx.conversation_gone(id)?;
        }
        Ok(())
    }

    /// `channel_rename`, `group_rename`: the same row under its new name.
    pub(super) fn renamed(&self, event: &Value) -> Result<(), Failure> {
        let channel = &event["channel"];
        let (Some(id), Some(name)) = (channel["id"].as_str(), channel["name"].as_str()) else {
            return Ok(());
        };
        let Some(known) = self.ctx.known(id)? else {
            return Ok(());
        };
        let mut row = self.again(&known);
        row.title = format!("#{name}");
        self.ctx.conversation(&row)?;
        self.ctx.set_note(&format!("c:{id}"), &row.title);
        Ok(())
    }

    /// `channel_topic`, `group_topic`: the line in the header changed.
    pub(super) fn topic_changed(&self, channel: &str, topic: &str) -> Result<(), Failure> {
        let Some(known) = self.ctx.known(channel)?.filter(|known| known.topic != topic) else {
            return Ok(());
        };
        let mut row = self.again(&known);
        row.topic = topic.to_owned();
        self.ctx.conversation(&row)?;
        Ok(())
    }

    /// Where the official app opens this conversation's huddle. Of the two forms seen,
    /// `https://app.slack.com/huddle/TEAM/CHANNEL` is used: it is what Slack's own "copy huddle
    /// link" produces, and the system hands it to the Slack app when that is installed. The
    /// `slack://` form is not used. Unverified that it starts the huddle rather than only
    /// opening the conversation.
    pub(super) fn huddle_link(&self, conversation: &str) -> String {
        let channel = thread_of(conversation).map_or(conversation, |(channel, _)| channel);
        format!("https://app.slack.com/huddle/{}/{channel}", self.session.team_id)
    }
}
