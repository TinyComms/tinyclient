//! A workspace as a Space: its sidebar as the tree, and the unread state Slack keeps for it.

use super::{emoji, fnv, seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{BrowsedPage, Command, Failure};
use crate::store::{ConversationRow, SpaceRow};
use protocol::{BrowsedChannel, Conversation, ConversationKind, Group, GroupKind, MediaFile, MediaKind, Relation};
use serde_json::{json, Value};
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
    emoji: String,
    sort: String,
    channels: Vec<String>,
}

fn section_emoji(raw: Option<&str>) -> String {
    let clean = raw.unwrap_or("").trim().trim_matches(':');
    if clean.is_empty() {
        return String::new();
    }
    emoji::standard(clean).unwrap_or_else(|| format!(":{clean}:"))
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
            emoji: String::new(),
            sort: String::new(),
            channels: channels.iter().map(|id| (*id).to_owned()).collect(),
        })
        .collect();
    starred_channels(&sections)
}

/// The workspace id a channel names, when it is one of `known`. Otherwise `primary`, so an
/// org-wide channel is filed once.
pub(super) fn workspace_of(channel: &Value, known: &[&str], primary: &str) -> String {
    let owns = |id: &str| known.iter().any(|held| *held == id);
    for key in ["context_team_id", "team"] {
        if let Some(id) = channel[key].as_str().filter(|id| !id.is_empty() && owns(id)) {
            return id.to_owned();
        }
    }
    for key in ["shared_team_ids", "internal_team_ids"] {
        if let Some(id) = channel[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|id| !id.is_empty() && owns(id))
        {
            return id.to_owned();
        }
    }
    primary.to_owned()
}

fn section(id: &str, name: &str) -> Section {
    Section {
        id: id.to_owned(),
        name: name.to_owned(),
        kind: id.to_owned(),
        emoji: String::new(),
        sort: String::new(),
        channels: Vec::new(),
    }
}

impl Slack {
    /// The primary Space id for this account: for Enterprise Grid, the first workspace Space;
    /// for a standalone workspace, the workspace account id itself.
    pub(super) fn primary_space(&self) -> String {
        if self.is_enterprise() {
            if let Some(first) = self.session.workspaces.first().filter(|w| !w.id.is_empty()) {
                return first.id.clone();
            }
        }
        self.ctx.account.account.clone()
    }

    /// The Spaces belonging to this account: all Grid workspaces for an org, or the single
    /// workspace for a standalone account.
    pub(super) fn grid_spaces(&self) -> Vec<(String, String)> {
        if self.is_enterprise() && !self.session.workspaces.is_empty() {
            return self
                .session
                .workspaces
                .iter()
                .filter(|w| !w.id.is_empty())
                .map(|w| {
                    let label = if w.name.is_empty() { w.id.clone() } else { w.name.clone() };
                    (w.id.clone(), label)
                })
                .collect();
        }
        let name = if !self.session.enterprise_name.is_empty() {
            self.session.enterprise_name.clone()
        } else {
            self.ctx.account.account.clone()
        };
        vec![(self.ctx.account.account.clone(), name)]
    }

    /// Which workspace Space a channel belongs to. On Enterprise Grid, uses Slack's
    /// `context_team_id` / `shared_team_ids` / `internal_team_ids` / `team` when it names one of
    /// our org's workspaces, falling back to [`Self::primary_space`] so org-wide channels appear once.
    pub(super) fn workspace_for_channel(&self, channel: &Value) -> String {
        let primary = self.primary_space();
        if !self.is_enterprise() {
            return primary;
        }
        let known: Vec<&str> = self.session.workspaces.iter().map(|workspace| workspace.id.as_str()).collect();
        workspace_of(channel, &known, &primary)
    }

    /// A conversation's row in a specific workspace Space. Every Slack conversation can hold a
    /// huddle, which the official app carries.
    pub(super) fn placed_in(&self, space_id: &str, id: &str, kind: ConversationKind, title: &str) -> ConversationRow {
        ConversationRow {
            space: Some(space_id.to_owned()),
            position: LAST,
            call: super::call_support(),
            ..ConversationRow::new(id, kind, title)
        }
    }

    /// A conversation's row in this account's primary Space.
    pub(super) fn placed(&self, id: &str, kind: ConversationKind, title: &str) -> ConversationRow {
        self.placed_in(&self.primary_space(), id, kind, title)
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
        let space_id = self.workspace_for_channel(channel);
        let mut row = self.placed_in(&space_id, id, ConversationKind::Channel, &format!("#{name}"));
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

    /// The workspace as the account's one Space, with its icon. An Enterprise Grid org lists
    /// each of its workspaces as a Space under the one account.
    pub(super) fn space(&self, name: &str) -> Result<(), Failure> {
        if self.is_enterprise() {
            let listed = self.grid_spaces();
            let rows: Vec<SpaceRow> = listed
                .iter()
                .enumerate()
                .map(|(position, (id, label))| SpaceRow {
                    id: id.clone(),
                    name: label.clone(),
                    icon: None,
                    position: position as i32,
                })
                .collect();
            self.ctx.spaces(&rows)?;
            for (id, _) in &listed {
                self.ctx.set_space_can_create_channels(id, true)?;
            }
            return Ok(());
        }
        // Looked up once: `-` when the workspace has only Slack's default picture.
        let icon = self.ctx.note("icon").or_else(|| {
            let info = self.api("team.info", &[]).ok()?;
            if let Some(domain) = info["team"]["domain"].as_str().filter(|d| !d.is_empty()) {
                self.ctx.set_note("team:domain", domain);
            }
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

    fn disable_sidebar_sections(&self) {
        let mut caps = super::capability_ceiling();
        caps.sidebar_sections = false;
        self.ctx.capabilities(caps);
    }

    fn check_paid_section_error(&self, err: &Fault) {
        if let Fault::Slack(code) = err {
            if matches!(code.as_str(), "paid_only" | "paid_teams_only" | "enterprise_is_restricted" | "not_allowed") {
                self.disable_sidebar_sections();
            }
        }
    }

    /// Creates a custom sidebar section (`users.channelSections.create`, web-client call).
    /// On paid-plan refusals (`paid_only` / `paid_teams_only`), turns off `Capabilities::sidebar_sections`
    /// so the shell hides custom section controls on free workspaces.
    pub(super) fn section_create(&self, name: &str, emoji_raw: &str) -> Result<Group, Fault> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Fault::Slack("invalid_name_required".to_owned()));
        }
        let mut params = vec![("name", name)];
        let clean_emoji = emoji_raw.trim().trim_matches(':');
        if !clean_emoji.is_empty() {
            params.push(("emoji", clean_emoji));
        }
        let resp = match self.api("users.channelSections.create", &params) {
            Ok(resp) => resp,
            Err(err) => {
                self.check_paid_section_error(&err);
                return Err(err);
            }
        };
        let id = resp["channel_section_id"]
            .as_str()
            .or_else(|| resp["channel_section"]["channel_section_id"].as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Fault::Net("users.channelSections.create: no section id".to_owned()))?;
        let group = Group {
            emoji: section_emoji((!clean_emoji.is_empty()).then_some(clean_emoji)),
            ..Group::new(id, name, 0)
        };
        self.ctx
            .group(&self.ctx.account.account, &group)
            .map_err(|err| Fault::Net(err.to_string()))?;
        Ok(group)
    }

    /// Sections already stored for this workspace. The conversation page size does not drop groups.
    fn known_sections(&self) -> Result<Vec<String>, Fault> {
        let space = self.ctx.key(&self.ctx.account.account);
        let tree = self
            .ctx
            .hub
            .store()
            .tree(&space, 0, 1)
            .map_err(|err| Fault::Net(err.to_string()))?
            .ok_or_else(|| Fault::Slack("section_not_found".to_owned()))?;
        Ok(tree.groups.into_iter().map(|group| group.id).collect())
    }

    /// Refuses before the API unless every id is already in this workspace's section list.
    fn require_sections(&self, ids: &[&str]) -> Result<(), Fault> {
        let known = self.known_sections()?;
        if ids.iter().all(|id| known.iter().any(|have| have == id)) {
            Ok(())
        } else {
            Err(Fault::Slack("section_not_found".to_owned()))
        }
    }

    /// Renames or updates a custom sidebar section (`users.channelSections.update`).
    pub(super) fn section_rename(&self, section_id: &str, name: &str, emoji_raw: Option<&str>) -> Result<(), Fault> {
        let section_id = section_id.trim();
        let name = name.trim();
        if section_id.is_empty() || name.is_empty() {
            return Err(Fault::Slack("invalid_arguments".to_owned()));
        }
        self.require_sections(&[section_id])?;
        let mut params = vec![("channel_section_id", section_id), ("name", name)];
        let clean_emoji = emoji_raw.map(|e| e.trim().trim_matches(':'));
        if let Some(e) = clean_emoji {
            params.push(("emoji", e));
        }
        if let Err(err) = self.api("users.channelSections.update", &params) {
            self.check_paid_section_error(&err);
            return Err(err);
        }
        // The store guard has to end before `group` takes it again.
        let updated = self
            .ctx
            .hub
            .store()
            .tree(&self.ctx.key(&self.ctx.account.account), 0, 1)
            .ok()
            .flatten()
            .and_then(|t| t.groups.into_iter().find(|g| g.id == section_id));
        if let Some(mut group) = updated {
            group.name = name.to_owned();
            if let Some(e) = clean_emoji {
                group.emoji = section_emoji((!e.is_empty()).then_some(e));
            }
            self.ctx
                .group(&self.ctx.account.account, &group)
                .map_err(|err| Fault::Net(err.to_string()))?;
        }
        Ok(())
    }

    /// Deletes a custom sidebar section (`users.channelSections.delete`).
    pub(super) fn section_delete(&self, section_id: &str) -> Result<(), Fault> {
        let section_id = section_id.trim();
        if section_id.is_empty() {
            return Err(Fault::Slack("invalid_arguments".to_owned()));
        }
        self.require_sections(&[section_id])?;
        if let Err(err) = self.api("users.channelSections.delete", &[("channel_section_id", section_id)]) {
            self.check_paid_section_error(&err);
            return Err(err);
        }
        self.ctx
            .group_gone(&self.ctx.account.account, section_id)
            .map_err(|err| Fault::Net(err.to_string()))?;
        Ok(())
    }

    /// Reorders sidebar sections (`users.channelSections.order`).
    pub(super) fn section_reorder(&self, section_ids: &[String]) -> Result<(), Fault> {
        if section_ids.is_empty() {
            return Err(Fault::Slack("invalid_arguments".to_owned()));
        }
        let ids: Vec<&str> = section_ids.iter().map(String::as_str).collect();
        self.require_sections(&ids)?;
        let ids_json = json!(section_ids).to_string();
        if let Err(err) = self.api("users.channelSections.order", &[("channel_section_ids", ids_json.as_str())]) {
            self.check_paid_section_error(&err);
            return Err(err);
        }
        self.ctx
            .reorder_groups(&self.ctx.account.account, section_ids)
            .map_err(|err| Fault::Net(err.to_string()))?;
        Ok(())
    }

    /// Moves a channel, DM, or app between sidebar sections (`users.channelSections.channels.bulkUpdate`).
    pub(super) fn section_move_channel(
        &self,
        conversation_id: &str,
        from_section: Option<&str>,
        to_section: &str,
    ) -> Result<(), Fault> {
        let cid = super::thread_of(conversation_id).map_or(conversation_id, |(c, _)| c);
        let to = to_section.trim();
        if cid.is_empty() || to.is_empty() {
            return Err(Fault::Slack("invalid_arguments".to_owned()));
        }
        let from = from_section.map(str::trim).filter(|section| !section.is_empty() && *section != to);
        let mut ids = vec![to];
        if let Some(from) = from {
            ids.push(from);
        }
        self.require_sections(&ids)?;
        let insert = json!([{"channel_section_id": to, "channel_ids": [cid]}]).to_string();
        let remove = from_section
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != to && *s != DIRECTS)
            .map(|from| json!([{"channel_section_id": from, "channel_ids": [cid]}]).to_string());
        let mut params = vec![("insert", insert.as_str())];
        if let Some(ref rem) = remove {
            params.push(("remove", rem.as_str()));
        }
        if let Err(err) = self.api("users.channelSections.channels.bulkUpdate", &params) {
            self.check_paid_section_error(&err);
            return Err(err);
        }
        self.ctx
            .move_conversation_group(&self.ctx.account.account, cid, to)
            .map_err(|err| Fault::Net(err.to_string()))?;
        Ok(())
    }

    /// Live socket event `channel_sections_upserted`: adds/updates custom sidebar sections.
    pub(super) fn sections_upserted(&self, event: &Value) -> Result<(), Failure> {
        let items = event["channel_sections"]
            .as_array()
            .or_else(|| event["sections"].as_array())
            .map_or(&[][..], Vec::as_slice);
        for (ix, item) in items.iter().enumerate() {
            let Some(id) = item["channel_section_id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let kind = item["type"].as_str().unwrap_or("standard");
            if matches!(kind, "direct_messages" | "recent_apps") {
                continue;
            }
            let name = item["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("Section");
            let group = Group {
                emoji: section_emoji(item["emoji"].as_str()),
                sort: item["sort"].as_str().or_else(|| item["channel_sort"].as_str()).unwrap_or("").to_ascii_lowercase(),
                ..Group::new(id, name, ix as i32)
            };
            self.ctx.group(&self.ctx.account.account, &group)?;
        }
        Ok(())
    }

    /// Live socket event `channel_sections_removed` / `channel_sections_deleted`.
    pub(super) fn sections_removed(&self, event: &Value) -> Result<(), Failure> {
        let ids: Vec<&str> = event["channel_section_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(event["channel_section_id"].as_str())
            .filter(|s| !s.is_empty())
            .collect();
        for id in ids {
            self.ctx.group_gone(&self.ctx.account.account, id)?;
        }
        Ok(())
    }

    /// Live socket events `channel_sections_channels_upserted` and `channel_sections_channels_removed`.
    pub(super) fn sections_channels_changed(&self, event: &Value, added: bool) -> Result<(), Failure> {
        let fallback_chan = self.ctx.note("group:channel").unwrap_or_default();
        let entries = event["channel_sections"]
            .as_array()
            .or_else(|| event["sections"].as_array())
            .map_or(&[][..], Vec::as_slice);
        for entry in entries {
            let sec_id = entry["channel_section_id"].as_str().unwrap_or("");
            for cid in entry["channel_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if cid.is_empty() {
                    continue;
                }
                if added && !sec_id.is_empty() {
                    self.ctx.move_conversation_group(&self.ctx.account.account, cid, sec_id)?;
                } else if !added {
                    let is_direct = self.ctx.known(cid)?.is_some_and(|r| r.kind.is_direct());
                    let target = if is_direct { DIRECTS } else { fallback_chan.as_str() };
                    self.ctx.move_conversation_group(&self.ctx.account.account, cid, target)?;
                }
            }
        }
        if let Some(sec_id) = event["channel_section_id"].as_str().filter(|s| !s.is_empty()) {
            for cid in event["channel_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if added {
                    self.ctx.move_conversation_group(&self.ctx.account.account, cid, sec_id)?;
                } else {
                    let is_direct = self.ctx.known(cid)?.is_some_and(|r| r.kind.is_direct());
                    let target = if is_direct { DIRECTS } else { fallback_chan.as_str() };
                    self.ctx.move_conversation_group(&self.ctx.account.account, cid, target)?;
                }
            }
        }
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
                let _ = self.ctx.set_space_can_create_channels(&self.primary_space(), false);
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
                let page = &listed["channel_ids_page"];
                let mut channels: Vec<String> = page["channel_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let mut cursor = page["next_cursor"]
                    .as_str()
                    .or_else(|| listed["response_metadata"]["next_cursor"].as_str())
                    .unwrap_or("")
                    .to_owned();
                let sec_id = id_of(listed);
                let mut extra_pages = 0usize;
                while !cursor.is_empty() && !sec_id.is_empty() && extra_pages < 10 {
                    extra_pages += 1;
                    let Ok(more) = self.api(
                        "users.channelSections.channels.list",
                        &[("channel_section_id", sec_id.as_str()), ("cursor", cursor.as_str())],
                    ) else {
                        break;
                    };
                    for cid in more["channel_ids"]
                        .as_array()
                        .or_else(|| more["channel_ids_page"]["channel_ids"].as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        if !channels.iter().any(|existing| existing == cid) {
                            channels.push(cid.to_owned());
                        }
                    }
                    cursor = more["response_metadata"]["next_cursor"]
                        .as_str()
                        .or_else(|| more["channel_ids_page"]["next_cursor"].as_str())
                        .or_else(|| more["next_cursor"].as_str())
                        .unwrap_or("")
                        .to_owned();
                }
                let emoji_str = section_emoji(listed["emoji"].as_str());
                let sort = listed["sort"]
                    .as_str()
                    .or_else(|| listed["channel_sort"].as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                Section {
                    id: sec_id,
                    name: name.to_owned(),
                    kind: kind.to_owned(),
                    emoji: emoji_str,
                    sort,
                    channels,
                }
            })
            .collect();
        let recent_apps: Vec<String> = sections
            .iter()
            .filter(|section| section.kind == "recent_apps")
            .flat_map(|section| section.channels.iter().cloned())
            .collect();
        self.ctx.set_note("recent_apps", &recent_apps.join(","));
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
        let recent_app_channels: HashSet<String> = self
            .ctx
            .note("recent_apps")
            .unwrap_or_default()
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect();
        // The mutes and the sidebar's "unreads only" are in the same prefs, read once here.
        let prefs = self.prefs()?;
        let unread_only = matches!(prefs["sidebar_behavior"].as_str(), Some("hide_read_channels" | "hide_read_channels_unless_starred"));
        let default = |kind: &str| sections.iter().find(|section| section.kind == kind).map(|section| section.id.clone());
        self.ctx.set_note("group:channel", &default("channels").unwrap_or_default());
        self.ctx.set_note("group:directs", DIRECTS);

        let (mut channels, mut cursor, mut seen) = (Vec::<Value>::new(), String::new(), HashSet::new());
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
                    !direct(channel)
                        || open.is_empty()
                        || channel["id"]
                            .as_str()
                            .is_some_and(|id| open.contains(id) || recent_app_channels.contains(id))
                };
                channels.extend(found.into_iter().filter(shown).filter(|channel| match channel["id"].as_str() {
                    Some(id) => seen.insert(id.to_owned()),
                    None => true,
                }));
            }
            if cursor.is_empty() {
                break;
            }
        }
        let partner = |channel: &Value| channel["user"].as_str().filter(|_| channel["is_im"] == true).map(str::to_owned);
        let partners: Vec<String> = channels.iter().filter_map(partner).collect();
        // Before any row: this registers each partner's picture and records them as people.
        self.users(&partners);

        let mut rows: Vec<(usize, i64, ConversationRow)> = Vec::with_capacity(channels.len());
        let mut direct = HashMap::new();
        for channel in &channels {
            let Some(mut row) = self.row(channel) else {
                continue;
            };
            // `updated` lives on Slack's channel, not on the stored row.
            let updated = match channel["updated"].as_str() {
                Some(ts) => seconds(ts),
                None => channel["updated"].as_i64().unwrap_or(0),
            };
            if let Some(person) = partner(channel) {
                direct.insert(person, row.id.clone());
            }
            if row.kind.is_direct() {
                // Keep a DM or app inside a custom (`standard`) section when the user filed it there;
                // otherwise place it under the workspace's Direct messages group.
                if let Some(at) = sections
                    .iter()
                    .position(|section| section.kind == "standard" && section.channels.contains(&row.id))
                {
                    row.group = Some(sections[at].id.clone());
                    rows.push((at, updated, row));
                } else {
                    // After every section.
                    rows.push((sections.len(), updated, row));
                }
                continue;
            }
            // A section the user filed it under wins over the built-in for its kind.
            let filed = sections.iter().position(|section| section.channels.contains(&row.id));
            let at = filed.or_else(|| sections.iter().position(|section| Some(&section.id) == row.group.as_ref()));
            let at = at.unwrap_or(0);
            row.group = Some(sections[at].id.clone());
            rows.push((at, updated, row));
        }
        drop(channels);
        // Honour per-section sort ("recent" puts newest first; default is alphabetical).
        rows.sort_by(|(at_a, updated_a, row_a), (at_b, updated_b, row_b)| {
            at_a.cmp(at_b).then_with(|| {
                let recent = sections.get(*at_a).is_some_and(|s| s.sort == "recent");
                if recent {
                    updated_b.cmp(updated_a).then_with(|| row_a.title.to_lowercase().cmp(&row_b.title.to_lowercase()))
                } else {
                    row_a.title.to_lowercase().cmp(&row_b.title.to_lowercase())
                }
            })
        });
        let rows: Vec<ConversationRow> = rows
            .into_iter()
            .enumerate()
            .map(|(position, (_, _, row))| ConversationRow {
                position: position as i32,
                ..row
            })
            .collect();
        let have: HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        let starred: Vec<String> = starred_channels(&sections).into_iter().filter(|id| have.contains(id.as_str())).collect();
        self.publish_trees(&sections, &rows, unread_only)?;
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

    /// One Space's groups and the rows filed under it. A Grid org writes one tree per workspace,
    /// so a channel Slack shares across workspaces is stored once.
    fn publish_trees(&self, sections: &[Section], rows: &[ConversationRow], unread_only: bool) -> Result<(), Failure> {
        let spaces: Vec<String> = if self.is_enterprise() {
            self.grid_spaces().into_iter().map(|(id, _)| id).collect()
        } else {
            vec![self.ctx.account.account.clone()]
        };
        for space in spaces {
            let mut mine: Vec<ConversationRow> = rows.iter().filter(|row| row.space.as_deref() == Some(space.as_str())).cloned().collect();
            mine.sort_by_key(|row| row.position);
            for (position, row) in mine.iter_mut().enumerate() {
                row.position = position as i32;
            }
            let used = |id: &str| mine.iter().any(|row| row.group.as_deref() == Some(id));
            let mut groups: Vec<Group> = sections
                .iter()
                .enumerate()
                .filter(|(_, section)| used(&section.id))
                .map(|(position, section)| Group {
                    unread_only,
                    emoji: section.emoji.clone(),
                    sort: section.sort.clone(),
                    ..Group::new(&section.id, &section.name, position as i32)
                })
                .collect();
            if used(DIRECTS) {
                groups.push(Group {
                    kind: GroupKind::Directs,
                    ..Group::new(DIRECTS, "Direct messages", sections.len() as i32)
                });
            }
            self.ctx.tree(&space, &groups, &mine)?;
        }
        Ok(())
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
        let channel = subscription["channel"].as_str().or_else(|| event["channel"].as_str());
        let root = subscription["thread_ts"].as_str().or_else(|| event["thread_ts"].as_str());
        let (Some(channel), Some(root)) = (channel, root) else {
            return Ok(());
        };
        let unread = subscription["unread_count"]
            .as_u64()
            .or_else(|| event["unread_count"].as_u64())
            .unwrap_or(0) as u32;
        let last_read = subscription["last_read"].as_str().or_else(|| event["last_read"].as_str());
        self.ctx.counts(&thread_id(channel, root), unread, 0, last_read, 0)?;
        self.thread_unread(channel, root, unread)
    }

    /// `thread_subscribed` / `thread_unsubscribed`: updates the followed thread set and store filing live.
    pub(super) fn thread_subscription_changed(&self, event: &Value, followed: bool) -> Result<(), Failure> {
        let subscription = &event["subscription"];
        let channel = subscription["channel"].as_str().or_else(|| event["channel"].as_str());
        let root = subscription["thread_ts"].as_str().or_else(|| event["thread_ts"].as_str());
        let (Some(channel), Some(root)) = (channel, root) else {
            return Ok(());
        };
        let id = thread_id(channel, root);
        if followed {
            if self.ctx.known(channel)?.is_none() {
                return Ok(());
            }
            self.thread_known(channel, root)?;
            self.state().followed_threads.insert(id.clone());
        } else {
            self.state().followed_threads.remove(&id);
        }
        self.ctx.filing(&id, &crate::store::Filing { followed, ..crate::store::Filing::default() })?;
        if !followed {
            self.ctx.counts(&id, 0, 0, None, 0)?;
            self.thread_unread(channel, root, 0)?;
        }
        Ok(())
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
            // Before the row goes, so a huddle running there is emptied from it on the way.
            self.room_forgotten(id);
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
