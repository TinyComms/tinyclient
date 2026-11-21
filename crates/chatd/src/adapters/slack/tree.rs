//! A workspace as a Space: its sidebar as the tree, and the unread state Slack keeps for it.

use super::{fnv, seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{BrowsedPage, Command, Failure};
use crate::store::{ConversationRow, SpaceRow};
use protocol::{BrowsedChannel, CallSupport, Conversation, ConversationKind, Group, GroupKind, MediaFile, MediaKind, Relation};
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

/// Channel ids Slack filed in its stars section. Those rows are starred on Slack.
fn starred_channels(sections: &[Section]) -> Vec<String> {
    let mut ids = Vec::new();
    for section in sections.iter().filter(|section| section.kind == "stars") {
        for id in &section.channels {
            if !id.is_empty() && !ids.iter().any(|held| held == id) {
                ids.push(id.clone());
            }
        }
    }
    ids
}

/// The same, from section kinds, so a proof can name a stars section without a workspace.
pub(super) fn starred_from(kinds: &[(&str, &[&str])]) -> Vec<String> {
    let sections: Vec<Section> = kinds
        .iter()
        .enumerate()
        .map(|(at, (kind, channels))| Section {
            id: format!("S{at}"),
            name: String::new(),
            kind: (*kind).to_owned(),
            channels: channels.iter().map(|id| (*id).to_owned()).collect(),
        })
        .collect();
    starred_channels(&sections)
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
    pub(super) fn row(&self, channel: &Value) -> Option<ConversationRow> {
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
        // Slack Connect: shared with another organization.
        if channel["is_ext_shared"] == true {
            row.mark = Some(protocol::Mark::External);
        }
        // Unverified: `is_read_only` is the documented flag; a channel only some may post in
        // (`posting_restricted_to`) is not told apart from one I may post in.
        row.read_only = ["is_read_only", "is_archived", "is_frozen"].iter().any(|flag| channel[*flag] == true);
        if channel["is_archived"] == true {
            row.read_only_reason = "This channel is archived.".to_owned();
        }
        row.rights.pin = !row.read_only;
        row.rights.rename = !row.read_only;
        row.rights.edit_topic = !row.read_only;
        row.rights.add_members = !row.read_only;
        row.rights.remove_members = !row.read_only;
        row.rights.manage_members = !row.read_only;
        Some(row)
    }

    /// Paged public channel browser (`conversations.list` with `types=public_channel`), never
    /// fetched whole. Includes archived channels as read-only so their history can be read.
    /// Filters the returned page by `query` when non-empty.
    pub(super) fn browse_channels(&self, query: &str, cursor: Option<&str>, limit: u32) -> Result<BrowsedPage, Fault> {
        let count = limit.clamp(1, 100).to_string();
        let mut params = vec![
            ("types", "public_channel"),
            ("exclude_archived", "false"),
            ("limit", count.as_str()),
        ];
        if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
            params.push(("cursor", cursor));
        }
        let page = self.api("conversations.list", &params)?;
        let needle = query.trim().to_lowercase();
        let mut channels = Vec::new();
        for ch in page["channels"].as_array().into_iter().flatten() {
            let Some(id) = ch["id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let raw_name = ch["name"].as_str().or(ch["name_normalized"].as_str()).unwrap_or("");
            if raw_name.is_empty() {
                continue;
            }
            let name = format!("#{raw_name}");
            let topic = ch["purpose"]["value"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| ch["topic"]["value"].as_str().filter(|s| !s.is_empty()))
                .unwrap_or("")
                .to_owned();
            if !needle.is_empty() && !name.to_lowercase().contains(&needle) && !topic.to_lowercase().contains(&needle) {
                continue;
            }
            channels.push(BrowsedChannel {
                key: self.ctx.key(id),
                name,
                topic,
                members: ch["num_members"].as_u64().unwrap_or(0) as u32,
                joined: ch["is_member"] == true,
                private: ch["is_private"] == true,
                archived: ch["is_archived"] == true,
            });
        }
        let next = page["response_metadata"]["next_cursor"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        Ok(BrowsedPage { channels, cursor: next })
    }

    /// Previews an unjoined public channel (or archived channel): records it as `joined: false`,
    /// `read_only: true`, outside the sidebar (`space: None`, `group: None`), and fetches one page of history.
    pub(super) fn preview_channel(&self, channel_id: &str) -> Result<(), Fault> {
        let info = self.api("conversations.info", &[("channel", channel_id), ("include_num_members", "true")])?;
        let ch = &info["channel"];
        let mut row = self.row(ch).ok_or_else(|| Fault::Slack("channel_not_found".to_owned()))?;
        let is_member = ch["is_member"] == true;
        let is_archived = ch["is_archived"] == true;
        if !is_member {
            row.joined = false;
            row.read_only = true;
            row.read_only_reason = if is_archived {
                "This channel is archived.".to_owned()
            } else {
                "Join this channel to post messages.".to_owned()
            };
            row.space = None;
            row.group = None;
            row.listed = false;
        }
        self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
        if is_archived {
            self.ctx
                .filing(channel_id, &crate::store::Filing { archived: true, ..Default::default() })
                .map_err(|err| Fault::Net(err.to_string()))?;
        }
        if let Ok(history) = self.api("conversations.history", &[("channel", channel_id), ("limit", "30")]) {
            let entries = history["messages"].as_array().map_or(&[][..], Vec::as_slice);
            self.authors(entries);
            let messages: Vec<protocol::Message> = entries.iter().filter_map(|e| self.message(channel_id, e)).collect();
            let _ = self.ctx.history(channel_id, &messages);
        }
        Ok(())
    }

    /// Renames a channel (`conversations.rename`) and updates the stored row.
    pub(super) fn rename_channel(&self, conversation_id: &str, title: &str) -> Result<(), Fault> {
        if thread_of(conversation_id).is_some() {
            return Err(Fault::Slack("not_a_channel".to_owned()));
        }
        let name = title.trim().trim_start_matches('#').trim();
        let renamed = self.api("conversations.rename", &[("channel", conversation_id), ("name", name)])?;
        let final_name = renamed["channel"]["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(name);
        if let Some(known) = self.ctx.known(conversation_id).map_err(|err| Fault::Net(err.to_string()))? {
            let mut row = self.again(&known);
            row.title = format!("#{final_name}");
            self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
            self.ctx.set_note(&format!("c:{conversation_id}"), &row.title);
        }
        Ok(())
    }

    /// Archives (`conversations.archive`) or unarchives (`conversations.unarchive`) a channel.
    pub(super) fn archive_channel(&self, conversation_id: &str, archive: bool) -> Result<(), Fault> {
        if thread_of(conversation_id).is_some() {
            return Err(Fault::Slack("not_a_channel".to_owned()));
        }
        let method = if archive { "conversations.archive" } else { "conversations.unarchive" };
        self.api(method, &[("channel", conversation_id)])?;
        self.ctx
            .filing(conversation_id, &crate::store::Filing { archived: archive, ..Default::default() })
            .map_err(|err| Fault::Net(err.to_string()))?;
        if let Some(known) = self.ctx.known(conversation_id).map_err(|err| Fault::Net(err.to_string()))? {
            let mut row = self.again(&known);
            row.read_only = archive;
            row.read_only_reason = if archive { "This channel is archived.".to_owned() } else { String::new() };
            row.rights.pin = !archive;
            row.rights.rename = !archive;
            row.rights.edit_topic = !archive;
            row.rights.add_members = !archive;
            row.rights.remove_members = !archive;
            row.rights.manage_members = !archive;
            self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
        }
        Ok(())
    }

    /// Updates a channel's name, topic, and/or description (purpose).
    pub(super) fn update_conversation(
        &self,
        conversation_id: &str,
        title: Option<&str>,
        topic: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), Fault> {
        if let Some(title) = title {
            self.rename_channel(conversation_id, title)?;
        }
        if let Some(topic) = topic {
            self.set_topic(conversation_id, topic)?;
        }
        if let Some(description) = description {
            self.set_purpose(conversation_id, description)?;
        }
        Ok(())
    }

    /// Sets a channel's topic (`conversations.setTopic`) and updates the stored row.
    pub(super) fn set_topic(&self, conversation_id: &str, topic: &str) -> Result<(), Fault> {
        if thread_of(conversation_id).is_some() {
            return Err(Fault::Slack("not_a_channel".to_owned()));
        }
        let updated = self.api("conversations.setTopic", &[("channel", conversation_id), ("topic", topic)])?;
        let final_topic = updated["topic"].as_str().unwrap_or(topic);
        if let Some(known) = self.ctx.known(conversation_id).map_err(|err| Fault::Net(err.to_string()))? {
            let mut row = self.again(&known);
            row.topic = final_topic.to_owned();
            self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
        }
        self.ctx.info_stale(conversation_id);
        Ok(())
    }

    /// Sets a channel's description (`conversations.setPurpose`) and invalidates cached info.
    pub(super) fn set_purpose(&self, conversation_id: &str, purpose: &str) -> Result<(), Fault> {
        if thread_of(conversation_id).is_some() {
            return Err(Fault::Slack("not_a_channel".to_owned()));
        }
        self.api("conversations.setPurpose", &[("channel", conversation_id), ("purpose", purpose)])?;
        self.ctx.info_stale(conversation_id);
        Ok(())
    }

    /// Joins a public channel (`conversations.join`) and places it in the workspace sidebar tree.
    pub(super) fn join_channel(&self, channel_id: &str) -> Result<(), Fault> {
        let joined = self.api("conversations.join", &[("channel", channel_id)])?;
        let ch = &joined["channel"];
        if let Some(mut row) = self.row(ch) {
            row.joined = true;
            self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
        } else {
            let info = self.api("conversations.info", &[("channel", channel_id)])?;
            if let Some(mut row) = self.row(&info["channel"]) {
                row.joined = true;
                self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
            }
        }
        Ok(())
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
            read_only_reason: known.read_only_reason.clone(),
            private: known.private,
            joined: known.joined,
            bot: known.bot,
            special: known.special,
            mark: known.mark,
            rights: known.rights,
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
        self.ctx.set_space_can_create_channels(&self.ctx.account.account, true)?;
        Ok(())
    }

    /// Creates a public or private Slack channel (`conversations.create`) and records it in the sidebar.
    /// If the workspace restricts channel creation (`restricted_action`), turns off `can_create_channels`
    /// on the Space so the shell hides the entry.
    pub(super) fn create_channel(&self, name: &str, private: bool, group: Option<&str>) -> Result<String, Fault> {
        let clean = name.trim().trim_start_matches('#').trim();
        if clean.is_empty() {
            return Err(Fault::Slack("invalid_name_required".to_owned()));
        }
        let is_private = if private { "true" } else { "false" };
        let created = match self.api("conversations.create", &[("name", clean), ("is_private", is_private)]) {
            Ok(created) => created,
            Err(Fault::Slack(ref err)) if err == "restricted_action" => {
                let _ = self.ctx.set_space_can_create_channels(&self.ctx.account.account, false);
                return Err(Fault::Slack("restricted_action".to_owned()));
            }
            Err(fault) => return Err(fault),
        };
        let ch = &created["channel"];
        let Some(id) = ch["id"].as_str().filter(|s| !s.is_empty()) else {
            return Err(Fault::Net("conversations.create: no channel".to_owned()));
        };
        let mut row = self.row(ch).unwrap_or_else(|| {
            let mut r = self.placed(id, ConversationKind::Channel, &format!("#{clean}"));
            r.group = self.ctx.note("group:channel");
            r.private = private;
            r.rights.pin = true;
            r.rights.rename = true;
            r.rights.edit_topic = true;
            r.rights.add_members = true;
            r.rights.remove_members = true;
            r.rights.manage_members = true;
            r
        });
        row.joined = true;
        row.private = ch["is_private"].as_bool().unwrap_or(private);
        if let Some(section_id) = group.filter(|g| !g.is_empty() && *g != DIRECTS) {
            row.group = Some(section_id.to_owned());
        }
        self.ctx.conversation(&row).map_err(|err| Fault::Net(err.to_string()))?;
        self.ctx.set_note(&format!("c:{id}"), &row.title);
        Ok(id.to_owned())
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
        // The mutes and the sidebar's "unreads only" are in the same prefs, read once here.
        let prefs = self.prefs()?;
        let unread_only = matches!(prefs["sidebar_behavior"].as_str(), Some("hide_read_channels" | "hide_read_channels_unless_starred"));
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
            .map(|(position, section)| Group {
                unread_only,
                ..Group::new(&section.id, &section.name, position as i32)
            })
            .collect();
        if used(DIRECTS) {
            groups.push(Group {
                kind: GroupKind::Directs,
                ..Group::new(DIRECTS, "Direct messages", sections.len() as i32)
            });
        }
        let have: HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        let starred: Vec<String> = starred_channels(&sections).into_iter().filter(|id| have.contains(id.as_str())).collect();
        self.ctx.tree(&self.ctx.account.account, &groups, &rows)?;
        drop(rows);
        for id in starred {
            self.ctx.starred(&id, true)?;
        }

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
        self.tell_partners();
        self.ctx.set_note("tree", "1");
        self.apply_mutes(&prefs)?;
        self.apply_counts(&counts)
    }

    /// At a connection: what was read or arrived while there was no socket.
    pub(super) fn resync(&self) -> Result<(), Failure> {
        let counts = self.api("client.counts", &[])?;
        self.mutes()?;
        // The rows are the cache's and now current: opening the Space soon asks nothing more.
        self.state().tree = Some(Instant::now());
        self.apply_counts(&counts)
    }

    /// The Space opened again, its sidebar long read: only the unread state, one
    /// `client.counts`. The rows and mutes are kept current by the socket.
    pub(super) fn recount(&self) -> Result<(), Failure> {
        let counts = self.api("client.counts", &[])?;
        self.state().tree = Some(Instant::now());
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

    /// The user's prefs: the mutes, and how the sidebar shows read channels. Null when Slack
    /// would not say: they are a refinement, and the sidebar is not held up for them.
    fn prefs(&self) -> Result<Value, Failure> {
        match self.api("users.prefs.get", &[]) {
            Ok(mut prefs) => Ok(prefs["prefs"].take()),
            Err(fault) => match Failure::from(fault) {
                failure @ Failure::SignedOut(_) => Err(failure),
                Failure::Dropped(_) => Ok(Value::Null),
            },
        }
    }

    /// The channels the user muted in Slack. Unverified for this kind of session:
    /// `users.prefs.get` and its `muted_channels` list are the documented user-token forms.
    ///
    /// Unverified: `sidebar_behavior` ("hide_read_channels") as the pref behind Slack's "show
    /// unreads only"; no per-section setting was found in any client, so it applies to every
    /// channel section, and is read when the sidebar is.
    fn mutes(&self) -> Result<(), Failure> {
        let prefs = self.prefs()?;
        self.apply_mutes(&prefs)
    }

    fn apply_mutes(&self, prefs: &Value) -> Result<(), Failure> {
        if prefs.is_null() {
            return Ok(());
        }
        if !prefs["muted_channels"].is_null() {
            self.pref_changed("muted_channels", &prefs["muted_channels"])?;
        }
        if !prefs["all_notifications_prefs"].is_null() {
            self.pref_changed("all_notifications_prefs", &prefs["all_notifications_prefs"])?;
        }
        if !prefs["highlight_words"].is_null() {
            self.pref_changed("highlight_words", &prefs["highlight_words"])?;
        }
        if !prefs["threads_everything"].is_null() {
            self.pref_changed("threads_everything", &prefs["threads_everything"])?;
        }
        Ok(())
    }

    /// Handles `users.prefs.get` fields and live `pref_change` socket events for mutes,
    /// `all_notifications_prefs` (per-channel levels & `suppress_at_channel`), `highlight_words`,
    /// and `threads_everything`.
    pub(super) fn pref_changed(&self, pref: &str, value: &Value) -> Result<(), Failure> {
        match pref {
            "muted_channels" => {
                let list = value.as_str().unwrap_or("");
                let now: HashSet<String> = list.split(',').filter(|id| !id.is_empty()).map(str::to_owned).collect();
                self.update_muted(now)?;
            }
            "all_notifications_prefs" => {
                let Some(parsed) = parse_notification_prefs(value) else {
                    return Ok(());
                };
                let affected: Vec<String> = {
                    let mut state = self.state();
                    let mut ids: HashSet<String> = state.channel_notify.keys().cloned().collect();
                    ids.extend(parsed.channel_notify.keys().cloned());
                    state.global_notify = parsed.global;
                    state.channel_notify = parsed.channel_notify;
                    state.suppress_at_channel = parsed.suppress_at_channel;
                    ids.into_iter().collect()
                };
                for id in &affected {
                    self.ctx.info_stale(id);
                }
                if !parsed.muted.is_empty() || !self.state().mutes_read {
                    self.update_muted(parsed.muted)?;
                }
            }
            "highlight_words" => {
                let raw = value.as_str().unwrap_or("");
                let words = parse_highlight_words(raw);
                self.state().highlight_words = words;
                self.ctx.set_note("highlight_words", raw);
            }
            "threads_everything" => {
                let on = value.as_bool().unwrap_or(false);
                self.state().thread_replies_notify = on;
            }
            _ => {}
        }
        Ok(())
    }

    fn update_muted(&self, now: HashSet<String>) -> Result<(), Failure> {
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
            self.ctx.info_stale(id);
        }
        for id in &now {
            self.ctx.muted(id, true)?;
            self.ctx.info_stale(id);
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
            self.forget_partner(id);
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

    /// `channel_purpose`, `group_purpose`: the channel description changed; refresh details on next open.
    pub(super) fn purpose_changed(&self, channel: &str, _purpose: &str) -> Result<(), Failure> {
        self.ctx.info_stale(channel);
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

/// Parsed `all_notifications_prefs` document from Slack `users.prefs.get` or `pref_change`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct NotificationPrefs {
    pub global: protocol::Notify,
    pub channel_notify: HashMap<String, protocol::Notify>,
    pub muted: HashSet<String>,
    pub suppress_at_channel: HashSet<String>,
}

fn parse_notify_level(raw: &str) -> Option<protocol::Notify> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "all" | "everything" => Some(protocol::Notify::All),
        "mentions_dms" | "mentions" | "highlights" | "nothing_except_mentions" => Some(protocol::Notify::Mentions),
        "nothing" | "none" => Some(protocol::Notify::Nothing),
        _ => None,
    }
}

/// Parses Slack's `all_notifications_prefs` (either a JSON-encoded string or a JSON object).
pub(super) fn parse_notification_prefs(value: &Value) -> Option<NotificationPrefs> {
    let owned = value.as_str().and_then(|text| serde_json::from_str::<Value>(text).ok());
    let doc = owned.as_ref().unwrap_or(value);
    if !doc.is_object() {
        return None;
    }
    let mut out = NotificationPrefs::default();
    if let Some(desktop) = doc["global"]["desktop"].as_str().and_then(parse_notify_level) {
        out.global = desktop;
    }
    if let Some(channels) = doc["channels"].as_object() {
        for (id, prefs) in channels {
            let is_muted = prefs["muted"] == true;
            if is_muted {
                out.muted.insert(id.clone());
                out.channel_notify.insert(id.clone(), protocol::Notify::Nothing);
            } else if let Some(level) = prefs["desktop"].as_str().and_then(parse_notify_level) {
                out.channel_notify.insert(id.clone(), level);
            } else if prefs["mentions"] == true {
                out.channel_notify.insert(id.clone(), protocol::Notify::Mentions);
            }
            if prefs["suppress_at_channel"] == true {
                out.suppress_at_channel.insert(id.clone());
            }
        }
    }
    Some(out)
}

/// Splits Slack's comma-separated `highlight_words` pref into trimmed, lowercased keywords.
pub(super) fn parse_highlight_words(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Returns true if `text` contains any keyword in `keywords` on ASCII/Unicode word boundaries,
/// case-insensitively.
pub(super) fn matches_keyword(text: &str, keywords: &[String]) -> bool {
    if text.is_empty() || keywords.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    for kw in keywords {
        if kw.is_empty() {
            continue;
        }
        let mut start = 0;
        while let Some(pos) = lower[start..].find(kw.as_str()) {
            let at = start + pos;
            let end = at + kw.len();
            let before_ok = lower[..at].chars().next_back().is_none_or(|c| !is_word(c));
            let after_ok = lower[end..].chars().next().is_none_or(|c| !is_word(c));
            if before_ok && after_ok {
                return true;
            }
            start = at + kw.len().max(1);
            if !lower.is_char_boundary(start) {
                break;
            }
        }
    }
    false
}
