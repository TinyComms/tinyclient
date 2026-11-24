//! What the panes, the picker and the lists ask of Slack beyond reading and sending: a
//! conversation's info, its notifications, its pins and files, the workspace's emoji, the
//! threads the user follows, and the newest message of rows on screen. Each runs because the
//! user opened or clicked something, or (previews) because the rows are on screen, paced.

use super::{emoji, fnv, seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{AccountFilesPage, ActivityPage, CatalogPage, ConversationAbout, Failure, SharedPage, SpaceAbout};
use crate::store::Filing;
use protocol::{
    ActivityItem, ActivityItemKind, AppHome, AppLink, AppProfile, AppSlashCommand, AppView,
    Bookmark, BookmarkKind, CatalogItem, ConversationKind, MediaFile, MediaKind, Message, Notify,
    SharedItem, SharedKind, SlashCommand, UnreadGroup, UserGroup,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The followed threads are asked for once in this long, however often the list is opened.
const THREADS_FRESH: Duration = Duration::from_secs(600);
/// Followed threads asked for in one call; the web client asks for fewer at a time.
const THREADS: &str = "20";
/// Rows on screen have their newest message fetched at most this often, plus up to half again
/// so the calls are not in step.
pub(super) const PREVIEW_EVERY: Duration = Duration::from_secs(1);
/// Rows waiting for a preview. More than a screen's worth is the screen having moved on.
const PREVIEWS_WAITING: usize = 64;
/// The most a page of `files.list` holds.
const FILES_PAGE: u32 = 100;
/// Other organisations named on one Slack Connect channel. More than this is not looked up.
const CONNECTED_TEAMS: usize = 4;

/// A direct message with someone at another company starts as an invitation Slack sends.
pub(super) const EXTERNAL_EMAIL: &str =
    "A direct message with someone at another company starts as an invitation. Send it in Slack: Home, Directories, External. If Directories is not there, open External connections.";

/// Workspace role from `users.info`, in the same order `space_info` shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceRole {
    PrimaryOwner,
    Owner,
    Admin,
    SingleGuest,
    MultiGuest,
    Member,
}

impl WorkspaceRole {
    fn from_user(user: &Value) -> Self {
        if user["is_primary_owner"] == true {
            Self::PrimaryOwner
        } else if user["is_owner"] == true {
            Self::Owner
        } else if user["is_admin"] == true {
            Self::Admin
        } else if user["is_ultra_restricted"] == true {
            Self::SingleGuest
        } else if user["is_restricted"] == true {
            Self::MultiGuest
        } else {
            Self::Member
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::PrimaryOwner => "Primary owner",
            Self::Owner => "Owner",
            Self::Admin => "Workspace admin",
            Self::SingleGuest => "Single-channel guest",
            Self::MultiGuest => "Multi-channel guest",
            Self::Member => "Member",
        }
    }

    fn is_workspace_admin(self) -> bool {
        matches!(self, Self::PrimaryOwner | Self::Owner | Self::Admin)
    }
}

/// PNG, GIF, JPEG, or WEBP, from the file bytes.
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn channel_of(conversation: &str) -> &str {
    thread_of(conversation).map_or(conversation, |(channel, _)| channel)
}

/// The note a team's name is kept under, so a second pane does not ask `team.info` again.
pub(super) fn team_note(id: &str) -> String {
    format!("team:{id}")
}

/// Team ids of the other organisations on a Slack Connect channel. `connected_team_ids` when
/// the channel is joined, otherwise `pending_connected_team_ids`. `shared_team_ids` includes
/// our own workspace and is not used. Ours is dropped. At most [`CONNECTED_TEAMS`].
pub(super) fn other_teams(channel: &Value, ours: &str) -> Vec<String> {
    let listed = |name: &str| -> Vec<String> {
        channel[name]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).filter(|id| !id.is_empty() && *id != ours).map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let mut teams = listed("connected_team_ids");
    if teams.is_empty() {
        teams = listed("pending_connected_team_ids");
    }
    teams.sort_unstable();
    teams.dedup();
    teams.truncate(CONNECTED_TEAMS);
    teams
}

/// A Slack Connect invitation that has not been accepted yet.
pub(super) fn invite_pending(channel: &Value) -> bool {
    if channel["is_pending_ext_shared"] == true {
        return true;
    }
    channel["pending_connected_team_ids"].as_array().is_some_and(|ids| ids.iter().any(|id| id.as_str().is_some_and(|id| !id.is_empty())))
}

/// Where the invitation is accepted. Slack documents that screen: Home, Directories, External,
/// or External connections in the sidebar. An admin may still have to approve.
pub(super) fn invite_handoff(organisation: &str) -> String {
    let who = if organisation.is_empty() { "another organisation" } else { organisation };
    format!("Invitation from {who}. Accept or decline it in Slack: Home, Directories, External. If Directories is not there, open External connections.")
}

/// A workspace domain Slack would put in `team.domain`: no slash, space or colon.
fn domain_ok(domain: &str) -> bool {
    !domain.is_empty() && !domain.contains(['/', ' ', ':'])
}

/// The lab's slack-connect-org driver. A build without this stays up and the driver fails.
/// The same helpers name a channel and write the hand-off.
pub(crate) fn prove_connect_names() -> Result<(), String> {
    let ours = "T01ALPHA";
    let joined = json!({
        "is_ext_shared": true,
        "connected_team_ids": ["T01ALPHA", "T0NORTH"],
        "shared_team_ids": ["T01ALPHA", "T0SOUTH"],
        "is_pending_ext_shared": false,
        "pending_connected_team_ids": [],
    });
    if other_teams(&joined, ours) != ["T0NORTH"] {
        return Err("a joined Slack Connect channel names the other organisation, not our own".into());
    }
    if invite_pending(&joined) {
        return Err("a joined channel is not a pending invite".into());
    }
    let pending = json!({
        "is_pending_ext_shared": true,
        "pending_connected_team_ids": ["T0NORTH"],
        "connected_team_ids": [],
        "shared_team_ids": [ours],
    });
    if !invite_pending(&pending) {
        return Err("a pending Slack Connect invite stays pending".into());
    }
    if other_teams(&pending, ours) != ["T0NORTH"] {
        return Err("a pending invite names the other organisation".into());
    }
    let handoff = invite_handoff("Northwind");
    if !handoff.contains("Northwind") || !handoff.contains("Directories") || !handoff.contains("External connections") {
        return Err("the hand-off names the organisation and Slack's External connections screen".into());
    }
    let own = json!({"connected_team_ids": [ours], "shared_team_ids": [ours, "T0SOUTH"]});
    if !other_teams(&own, ours).is_empty() {
        return Err("our own team is not named, and shared_team_ids is not the other organisation".into());
    }
    if !EXTERNAL_EMAIL.contains("Directories") || !EXTERNAL_EMAIL.contains("External connections") {
        return Err("an email invitation says where Slack accepts it".into());
    }
    Ok(())
}

impl Slack {
    /// A conversation's info pane: one `conversations.info`, with the member count. A direct
    /// conversation is two people and is not asked about.
    pub(super) fn conversation_info(&self, conversation: &str) -> Result<ConversationAbout, Fault> {
        let channel = channel_of(conversation);
        let notify = {
            let state = self.state();
            if state.muted.contains(channel) {
                Notify::Nothing
            } else if let Some(&level) = state.channel_notify.get(channel) {
                level
            } else {
                state.global_notify
            }
        };
        let known = self.ctx.known(channel).map_err(|err| Fault::Net(err.to_string()))?;
        if known.as_ref().is_some_and(|known| known.kind == ConversationKind::Direct) {
            return Ok(ConversationAbout { members: Some(2), notify, ..ConversationAbout::default() });
        }
        let info = self.api("conversations.info", &[("channel", channel), ("include_num_members", "true")])?;
        let channel = &info["channel"];
        let said = |what: &str| channel[what]["value"].as_str().filter(|text| !text.is_empty());
        // The creator only by a name already known: their profile is not asked for this.
        let creator = channel["creator"].as_str().map(|id| self.cached(id).map_or_else(|| id.to_owned(), |user| user.name));
        let names = self.connect_names(channel);
        let mut description = said("purpose").or(said("topic")).unwrap_or("").to_owned();
        if invite_pending(channel) {
            let handoff = invite_handoff(names.first().map(String::as_str).unwrap_or(""));
            if description.is_empty() {
                description = handoff;
            } else {
                description.push_str("\n\n");
                description.push_str(&handoff);
            }
        }
        let mut bookmarks = self.bookmarks(channel_of(conversation));
        // Surface the channel canvas from `conversations.info` (`properties.canvas`) when not
        // already present in bookmarks.list.
        let canvas = &channel["properties"]["canvas"];
        if let Some(file_id) = canvas["file_id"].as_str().filter(|id| !id.is_empty() && canvas["is_empty"] != true) {
            let already = bookmarks.iter().any(|b| {
                b.kind == BookmarkKind::Canvas && (b.id == file_id || b.link.contains(file_id))
            });
            if !already {
                let link = format!("{}docs/{}/{file_id}", self.session.url, self.session.team_id);
                bookmarks.insert(
                    0,
                    Bookmark {
                        id: file_id.to_owned(),
                        title: "Channel canvas".to_owned(),
                        link,
                        emoji: String::new(),
                        kind: BookmarkKind::Canvas,
                    },
                );
            }
        }
        let pinned = self
            .api("pins.list", &[("channel", channel_of(conversation))])
            .ok()
            .and_then(|v| v["items"].as_array().map(|items| items.len() as u32));
        Ok(ConversationAbout {
            description,
            members: channel["num_members"].as_u64().map(|count| count as u32),
            created: channel["created"].as_i64().filter(|created| *created > 0),
            creator: creator.unwrap_or_default(),
            notify,
            external: names.join(", "),
            bookmarks,
            pinned,
            ..ConversationAbout::default()
        })
    }

    /// The other organisations on a Slack Connect channel, looked up once each and kept.
    fn connect_names(&self, channel: &Value) -> Vec<String> {
        let mut names = Vec::new();
        for team in other_teams(channel, &self.session.team_id) {
            if let Some(name) = self.organisation(&team) {
                names.push(name);
            }
        }
        names
    }

    /// One team's name, from the note or from the documented `team.info`. An empty name or a
    /// failed lookup is skipped and not cached, so a later open can try again. Our own team is
    /// never asked: [`other_teams`] has already dropped it.
    fn organisation(&self, team: &str) -> Option<String> {
        let key = team_note(team);
        if let Some(name) = self.ctx.note(&key) {
            return (!name.is_empty()).then_some(name);
        }
        let info = self.api("team.info", &[("team", team)]).ok()?;
        let named = &info["team"];
        let name = named["name"].as_str().unwrap_or("").trim();
        if name.is_empty() {
            return None;
        }
        self.ctx.set_note(&key, name);
        if let Some(domain) = named["domain"].as_str().map(str::trim).filter(|domain| domain_ok(domain)) {
            self.ctx.set_note(&format!("domain:{domain}"), name);
        }
        Some(name.to_owned())
    }

    /// The other workspace's name on a forwarded message, only when that domain was already
    /// looked up. A forward is not worth a lookup. An extra dot is an enterprise host, and
    /// Slack's own hosts are not a workspace.
    pub(super) fn workspace_named(&self, url: &str) -> Option<String> {
        let host = super::https_host(url)?;
        let domain = host.strip_suffix(".slack.com")?;
        if domain.is_empty() || domain.contains('.') || matches!(domain, "app" | "files" | "status") {
            return None;
        }
        let name = self.ctx.note(&format!("domain:{domain}"))?;
        (!name.is_empty()).then_some(name)
    }

    /// Sets a conversation's notifications as the web client's channel menu does. Unverified
    /// against a real workspace: `users.prefs.setNotifications` and its fields are the
    /// community's reading of the web client; the mentions-only form most of all.
    pub(super) fn set_mute(&self, conversation: &str, notify: Notify) -> Result<(), Fault> {
        let channel = channel_of(conversation);
        match notify {
            Notify::All | Notify::Nothing => {
                let value = if notify == Notify::Nothing { "true" } else { "false" };
                let asked = [("name", "muted"), ("value", value), ("channel_id", channel), ("global", "false"), ("sync", "false")];
                self.api("users.prefs.setNotifications", &asked)?;
            }
            Notify::Mentions => {
                let prefs = json!([
                    {"name": "muted", "value": "false", "sync": true},
                    {"name": "desktop", "value": "mentions_dms", "sync": true},
                    {"name": "mobile", "value": "mentions_dms", "sync": true},
                ])
                .to_string();
                let asked = [("channel_ids", channel), ("global", "false"), ("sync", "true"), ("prefs", prefs.as_str())];
                self.api("users.prefs.setNotifications", &asked)?;
            }
        }
        let muted = notify == Notify::Nothing;
        let list = {
            let mut state = self.state();
            state.channel_notify.insert(channel.to_owned(), notify);
            if muted {
                state.muted.insert(channel.to_owned());
            } else {
                state.muted.remove(channel);
            }
            let mut list: Vec<String> = state.muted.iter().cloned().collect();
            list.sort_unstable();
            list.join(",")
        };
        self.ctx.set_note("muted", &list);
        self.ctx.muted(channel, muted).map_err(|err| Fault::Net(err.to_string()))
    }

    /// The pinned messages and files, as `pins.list` gives them, preserving who pinned each item and when.
    pub(super) fn pinned(&self, conversation: &str) -> Result<Vec<Message>, Fault> {
        let channel = channel_of(conversation);
        let pins = self.api("pins.list", &[("channel", channel)])?;
        let items = pins["items"].as_array().map_or(&[][..], Vec::as_slice);
        let mut user_ids: Vec<String> = Vec::new();
        for item in items {
            for id in [
                item["created_by"].as_str(),
                item["message"]["user"].as_str(),
                item["file"]["user"].as_str(),
            ]
            .into_iter()
            .flatten()
            {
                if !id.is_empty() {
                    user_ids.push(id.to_owned());
                }
            }
        }
        self.users(&user_ids);
        let mut pinned = Vec::new();
        for item in items {
            let built = match item["type"].as_str() {
                Some("message") => self.message(channel, &item["message"]),
                Some("file") => {
                    let file = &item["file"];
                    let shares = &file["shares"];
                    let shared = ["public", "private"].iter().find_map(|access| shares[*access][channel][0]["ts"].as_str());
                    let created = file["created"].as_i64().or_else(|| item["created"].as_i64()).unwrap_or(0);
                    let ts = shared.map(str::to_owned).unwrap_or_else(|| format!("{created}.000000"));
                    let user = file["user"].as_str().or_else(|| item["created_by"].as_str()).unwrap_or("");
                    let event = json!({"type": "message", "ts": ts, "user": user, "files": [file]});
                    self.message(channel, &event)
                }
                _ => None,
            };
            let Some(mut message) = built else {
                continue;
            };
            message.pinned = true;
            message.pinned_by = item["created_by"].as_str().filter(|id| !id.is_empty()).map(|id| self.user(id).name);
            message.pinned_at = item["created"].as_i64().filter(|ts| *ts > 0);
            pinned.push(message);
        }
        pinned.sort_by_key(|message| std::cmp::Reverse(message.pinned_at.unwrap_or(message.sent)));
        pinned.truncate(protocol::PINNED_LIMIT);
        Ok(pinned)
    }

    /// What was shared in a conversation, from Slack's own list of its files, because the user
    /// asked. Media are its pictures; files everything else. `before` is the timestamp of the
    /// item listed last. Links have no such list and are answered `None`: unsupported.
    ///
    /// Unverified: whether `types=images` also lists videos (Slack documents no `videos` type),
    /// and how far `ts_to` is honoured to the second.
    pub(super) fn search_shared(&self, conversation: &str, kind: SharedKind, before: Option<&str>, limit: u32) -> Option<Result<SharedPage, Fault>> {
        let types = match kind {
            SharedKind::Media => "images",
            SharedKind::Files => "all",
            SharedKind::Music | SharedKind::Voice | SharedKind::Gifs => return None,
            SharedKind::Links => {
                if before.is_some() {
                    return Some(Ok(SharedPage::default()));
                }
                let bookmarks = self.bookmarks(channel_of(conversation));
                let items: Vec<SharedItem> = bookmarks
                    .into_iter()
                    .filter(|b| !b.link.is_empty())
                    .take(limit.clamp(1, FILES_PAGE) as usize)
                    .map(|b| SharedItem {
                        message_id: format!("bookmark:{}", b.id),
                        sent: 0,
                        sender: String::new(),
                        attachment: None,
                        url: b.link,
                        title: b.title,
                    })
                    .collect();
                let total = Some(items.len() as u32);
                return Some(Ok(SharedPage { items, more: false, total }));
            }
        };
        Some(self.files(channel_of(conversation), kind, types, before, limit))
    }

    fn files(&self, channel: &str, kind: SharedKind, types: &str, before: Option<&str>, limit: u32) -> Result<SharedPage, Fault> {
        let count = limit.clamp(1, FILES_PAGE).to_string();
        let ts_to = before.map(|before| (seconds(before) - 1).to_string()).unwrap_or_default();
        let listed = self.api("files.list", &[("channel", channel), ("types", types), ("count", &count), ("ts_to", &ts_to)])?;
        let files = listed["files"].as_array().map_or(&[][..], Vec::as_slice);
        let mut items = Vec::new();
        for file in files {
            // The message it was shared in here, where Slack says; else when it was made.
            let shares = &file["shares"];
            let shared = ["public", "private"].iter().find_map(|access| shares[*access][channel][0]["ts"].as_str());
            let ts = shared.map(str::to_owned).unwrap_or_else(|| format!("{}.000000", file["created"].as_i64().unwrap_or(0)));
            let permalink = file["permalink"]
                .as_str()
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .unwrap_or("")
                .to_owned();
            if kind == SharedKind::Files && self.is_external(file) {
                let url = file["url_private"]
                    .as_str()
                    .map(str::trim)
                    .filter(|u| u.starts_with("https://"))
                    .unwrap_or(&permalink)
                    .to_owned();
                if !url.is_empty() {
                    let sender = file["user"].as_str().filter(|u| !u.is_empty()).map(|u| self.user(u).name).unwrap_or_default();
                    let title = file["title"]
                        .as_str()
                        .or_else(|| file["name"].as_str())
                        .unwrap_or("External file")
                        .to_owned();
                    items.push(SharedItem {
                        message_id: file["id"].as_str().unwrap_or(&ts).to_owned(),
                        sent: file["created"].as_i64().unwrap_or_else(|| seconds(&ts)),
                        sender,
                        attachment: None,
                        url,
                        title,
                    });
                }
                continue;
            }
            let event = json!({"type": "message", "ts": ts, "user": file["user"], "files": [file]});
            let Some(message) = self.message(channel, &event) else {
                continue;
            };
            let pictured = |kind: MediaKind| matches!(kind, MediaKind::Image | MediaKind::Video | MediaKind::Gif);
            for attachment in message.attachments.iter().filter(|attachment| pictured(attachment.file.kind) == (kind == SharedKind::Media)) {
                items.push(SharedItem {
                    message_id: message.id.clone(),
                    sent: message.sent,
                    sender: message.sender.name.clone(),
                    attachment: Some(attachment.clone()),
                    url: permalink.clone(),
                    title: attachment.name.clone(),
                });
            }
        }
        let paging = &listed["paging"];
        let more = paging["page"].as_u64().unwrap_or(1) < paging["pages"].as_u64().unwrap_or(1);
        // The total counts files of every kind asked for; for "all" that includes pictures.
        let total = paging["total"].as_u64().filter(|_| before.is_none() && kind == SharedKind::Media).map(|total| total as u32);
        Ok(SharedPage { items, more, total })
    }

    /// Paged workspace-wide file browser (`files.list` without a channel). Never walks the whole
    /// workspace; returns one page plus `cursor` for the next page.
    pub(super) fn account_files(
        &self,
        query: &str,
        kind: Option<&str>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<AccountFilesPage, Fault> {
        let count = limit.clamp(1, FILES_PAGE).to_string();
        let page_num: u32 = cursor.and_then(|c| c.parse().ok()).unwrap_or(1).max(1);
        let page_str = page_num.to_string();
        let types = match kind.map(str::trim).unwrap_or("") {
            "" | "all" => "all",
            "snippets" | "code" => "snippets",
            "images" | "media" => "images",
            "pdfs" | "pdf" => "pdfs",
            "gdocs" | "external" => "gdocs",
            other => other,
        };
        let listed = self.api(
            "files.list",
            &[("types", types), ("count", count.as_str()), ("page", page_str.as_str())],
        )?;
        let files = listed["files"].as_array().map_or(&[][..], Vec::as_slice);
        let needle = query.trim().to_ascii_lowercase();
        let mut items = Vec::with_capacity(files.len());
        let mut media = Vec::new();
        let mut variants = Vec::new();
        for file in files {
            let Some(id) = file["id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let title = file["title"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| file["name"].as_str().filter(|s| !s.is_empty()))
                .unwrap_or("untitled")
                .to_owned();
            let name = file["name"].as_str().unwrap_or(&title);
            if !needle.is_empty()
                && !title.to_ascii_lowercase().contains(&needle)
                && !name.to_ascii_lowercase().contains(&needle)
            {
                continue;
            }
            let sent = file["created"].as_i64().unwrap_or(0);
            let sender = file["user"]
                .as_str()
                .filter(|u| !u.is_empty())
                .map(|u| self.user(u).name)
                .unwrap_or_default();
            let permalink = file["permalink"]
                .as_str()
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .unwrap_or("")
                .to_owned();
            if self.is_external(file) {
                let url = file["url_private"]
                    .as_str()
                    .map(str::trim)
                    .filter(|u| u.starts_with("https://"))
                    .unwrap_or(&permalink)
                    .to_owned();
                items.push(SharedItem {
                    message_id: id.to_owned(),
                    sent,
                    sender,
                    attachment: None,
                    url,
                    title,
                });
                continue;
            }
            let attachment = self.file(file, &mut media, &mut variants);
            items.push(SharedItem {
                message_id: id.to_owned(),
                sent,
                sender,
                attachment,
                url: permalink,
                title,
            });
        }
        let mut sized = variants.iter().filter(|(_, sizes)| !sizes.is_empty());
        if let Err(err) = self.ctx.media(&media).and_then(|()| sized.try_for_each(|(id, sizes)| self.ctx.media_variants(id, sizes))) {
            tracing::warn!("chatd: Slack: {err}");
        }
        let paging = &listed["paging"];
        let page = paging["page"].as_u64().unwrap_or(u64::from(page_num)) as u32;
        let pages = paging["pages"].as_u64().unwrap_or(1) as u32;
        let more = page < pages;
        let next_cursor = more.then(|| (page + 1).to_string());
        Ok(AccountFilesPage { items, cursor: next_cursor, more })
    }

    /// Deletes a Slack file (`files.delete`, own files).
    pub(super) fn delete_file(&self, file_id: &str) -> Result<(), Fault> {
        if file_id.trim().is_empty() {
            return Err(Fault::Slack("file_not_found".to_owned()));
        }
        self.api("files.delete", &[("file", file_id.trim())])?;
        Ok(())
    }

    /// The workspace's custom emoji, from the list kept a day (`list_emoji`), a page at a time,
    /// those whose name holds `query`. Aliases of standard emoji are the shell's own list.
    pub(super) fn emoji_catalog(&self, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Fault> {
        self.list_emoji();
        let names = self.ctx.note("emoji:names").unwrap_or_default();
        let query = query.trim().trim_matches(':').to_lowercase();
        let from: usize = cursor.and_then(|cursor| cursor.parse().ok()).unwrap_or(0);
        let mut matching = names.split(',').filter(|name| !name.is_empty() && (query.is_empty() || name.to_lowercase().contains(&query)));
        let (mut items, mut files) = (Vec::new(), Vec::new());
        let mut walked = from;
        for name in matching.by_ref().skip(from) {
            if items.len() >= protocol::CATALOG_LIMIT {
                break;
            }
            walked += 1;
            let Some(url) = self.ctx.note(&format!("e:{name}")).filter(|url| !url.is_empty() && !url.starts_with('=')) else {
                continue;
            };
            let file = MediaFile::new(format!("emoji:{name}:{:x}", fnv(&url)), MediaKind::Emoji);
            files.push((file.clone(), url));
            items.push(CatalogItem {
                id: file.id.clone(),
                name: format!(":{name}:"),
                file: Some(file),
                pack: String::new(),
                animated: false,
                set: String::new(),
                locked: false,
            });
        }
        self.ctx.media(&files).map_err(|err| Fault::Net(err.to_string()))?;
        let more = matching.next().is_some();
        Ok(CatalogPage { items, cursor: more.then(|| walked.to_string()) })
    }

    /// Trending or searched GIFs for the composer's GIF picker (`CatalogKind::Gifs`).
    /// Preview thumbnails are registered with chatd's media store; `Command::Download` never
    /// sends Slack credentials to third-party hosts (GIPHY/Tenor).
    pub(super) fn gif_catalog(&self, query: &str, cursor: Option<&str>) -> Result<CatalogPage, Fault> {
        let q = query.trim();
        let next_in = cursor.unwrap_or("");
        let resp = if q.is_empty() {
            self.api("gifs.trending", &[("limit", "24"), ("cursor", next_in)])?
        } else {
            self.api("gifs.search", &[("q", q), ("limit", "24"), ("cursor", next_in)])?
        };
        let entries = resp["results"]
            .as_array()
            .or_else(|| resp["gifs"].as_array())
            .or_else(|| resp["data"].as_array())
            .map_or(&[][..], Vec::as_slice);
        let mut items = Vec::with_capacity(entries.len());
        let mut files = Vec::new();
        let gif_url_allowed = |u: &str| {
            self.hosted(u)
                || super::https_host(u)
                    .is_some_and(|host| super::within(host, "giphy.com") || super::within(host, "tenor.com"))
        };
        let preview_url_allowed = |u: &str| {
            self.hosted(u)
                || super::https_host(u)
                    .is_some_and(|host| super::within(host, "giphy.com") || super::within(host, "media.tenor.com"))
        };
        for entry in entries {
            let gif_url = entry["url"]
                .as_str()
                .or_else(|| entry["media_url"].as_str())
                .or_else(|| entry["images"]["original"]["url"].as_str())
                .map(str::trim)
                .filter(|u| gif_url_allowed(u));
            let Some(gif_url) = gif_url else {
                continue;
            };
            let preview_url = entry["preview_url"]
                .as_str()
                .or_else(|| entry["thumb_url"].as_str())
                .or_else(|| entry["images"]["fixed_height_small"]["url"].as_str())
                .or_else(|| entry["images"]["fixed_width_small"]["url"].as_str())
                .map(str::trim)
                .filter(|u| preview_url_allowed(u))
                .or_else(|| preview_url_allowed(gif_url).then_some(gif_url));
            let width = entry["width"]
                .as_u64()
                .or_else(|| entry["images"]["fixed_height_small"]["width"].as_str().and_then(|s| s.parse().ok()))
                .or_else(|| entry["images"]["original"]["width"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(240)
                .min(u64::from(u32::MAX)) as u32;
            let height = entry["height"]
                .as_u64()
                .or_else(|| entry["images"]["fixed_height_small"]["height"].as_str().and_then(|s| s.parse().ok()))
                .or_else(|| entry["images"]["original"]["height"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(180)
                .min(u64::from(u32::MAX)) as u32;
            let name = entry["title"]
                .as_str()
                .or_else(|| entry["name"].as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            let pack = entry["pack"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| if q.is_empty() { "Trending".to_owned() } else { String::new() });
            let file = preview_url.map(|url| {
                let file = MediaFile {
                    width,
                    height,
                    ..MediaFile::new(format!("gif:{:x}", fnv(url)), MediaKind::Image)
                };
                files.push((file.clone(), url.to_owned()));
                file
            });
            items.push(CatalogItem {
                id: gif_url.to_owned(),
                name,
                file,
                pack,
                animated: true,
                set: String::new(),
                locked: false,
            });
        }
        if !files.is_empty() {
            self.ctx.media(&files).map_err(|err| Fault::Net(err.to_string()))?;
        }
        let next_cursor = resp["response_metadata"]["next_cursor"]
            .as_str()
            .or_else(|| resp["next_cursor"].as_str())
            .or_else(|| resp["next"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Ok(CatalogPage { items, cursor: next_cursor })
    }

    /// The threads the user follows, asked once when the Threads list opens (and not again
    /// for ten minutes): each recorded as a followed thread with its newest replies.
    ///
    /// Unverified against a real workspace: `subscriptions.thread.getView` is the web client's
    /// own call, and its form (`fetch_threads_state`, `priority_mode`) and answer (`threads`
    /// of `root_msg` and `latest_replies`) are the community's capture. Unread replies are
    /// counted from the root's `last_read`, which the capture showed and Slack does not document.
    pub(super) fn open_threads(&self) -> Result<(), Failure> {
        {
            let mut state = self.state();
            if state.threads_at.is_some_and(|at| at.elapsed() < THREADS_FRESH) {
                return Ok(());
            }
            state.threads_at = Some(Instant::now());
        }
        let view = self.api("subscriptions.thread.getView", &[("limit", THREADS), ("fetch_threads_state", "true"), ("priority_mode", "all")])?;
        for thread in view["threads"].as_array().into_iter().flatten() {
            let root = &thread["root_msg"];
            let (Some(channel), Some(ts)) = (root["channel"].as_str(), root["ts"].as_str()) else {
                continue;
            };
            // Only a conversation already in the sidebar: nothing is asked to place one.
            if self.ctx.known(channel)?.is_none() {
                continue;
            }
            let replies = thread["latest_replies"].as_array().map_or(&[][..], Vec::as_slice);
            let mut entries = vec![root.clone()];
            entries.extend(replies.iter().cloned());
            self.authors(&entries);
            self.thread_known(channel, ts)?;
            let id = thread_id(channel, ts);
            let messages: Vec<Message> = entries.iter().filter_map(|entry| self.message(channel, entry)).collect();
            self.ctx.history(&id, &messages)?;
            self.state().followed_threads.insert(id.clone());
            self.ctx.filing(&id, &Filing { followed: true, ..Filing::default() })?;
            let read = root["last_read"].as_str().unwrap_or(ts);
            let unread = replies.iter().filter_map(|reply| reply["ts"].as_str()).filter(|reply| *reply > read).count() as u32;
            let latest = replies.iter().filter_map(|reply| reply["ts"].as_str()).max().unwrap_or(ts);
            self.ctx.counts(&id, unread, 0, root["last_read"].as_str(), seconds(latest))?;
        }
        Ok(())
    }

    /// Rows on screen with no preview: each one's newest message, one at a time on a thread of
    /// the adapter's own, at most one a second. A row no longer on screen when its turn comes,
    /// or that has a preview by then (a message arrived), is not asked about.
    pub(super) fn previews(self: &Arc<Self>, ids: Vec<String>) {
        let start = {
            let mut state = self.state();
            for id in ids {
                if thread_of(&id).is_none() && !state.previews.contains(&id) && state.previews.len() < PREVIEWS_WAITING {
                    state.previews.push_back(id);
                }
            }
            !std::mem::replace(&mut state.previewing, true)
        };
        if !start {
            return;
        }
        let slack = self.clone();
        std::thread::spawn(move || slack.preview_queue());
    }

    fn preview_queue(&self) {
        loop {
            let Some(id) = ({
                let mut state = self.state();
                let next = state.previews.pop_front();
                if next.is_none() {
                    state.previewing = false;
                }
                next
            }) else {
                return;
            };
            let on_screen = self.ctx.watching().rows.iter().any(|row| row.account() == self.ctx.account && row.id == id);
            let wanted = on_screen && self.ctx.known(&id).ok().flatten().is_some_and(|row| row.preview.is_empty());
            if !wanted {
                continue;
            }
            let wait = {
                let state = self.state();
                let gap = PREVIEW_EVERY.mul_f64(1. + super::jitter() / 2.);
                state.previewed.map_or(Duration::ZERO, |at| gap.saturating_sub(at.elapsed()))
            };
            if self.ctx.pause(wait) {
                return;
            }
            // The gap runs from one answer to the next request. A stamp taken before the
            // call lets a slow answer and a fast follow-up arrive less than a second apart.
            self.report(self.newest_of(&id));
            self.state().previewed = Some(Instant::now());
        }
    }

    fn newest_of(&self, channel: &str) -> Result<(), Failure> {
        let history = self.api("conversations.history", &[("channel", channel), ("limit", "1")])?;
        let entries = history["messages"].as_array().map_or(&[][..], Vec::as_slice);
        self.authors(entries);
        let messages: Vec<Message> = entries.iter().filter_map(|entry| self.message(channel, entry)).collect();
        // The newest message may be a huddle's, running now: the row then says so, unopened.
        self.rooms_read(channel, entries);
        self.ctx.history(channel, &messages)?;
        Ok(())
    }

    /// Reads one page of Slack's activity feed (`activity.feed`, web client call).
    /// Maps mentions, thread replies, reactions, app notifications, and channel invites
    /// into [`ActivityItem`] rows without rebuilding from message history.
    pub(super) fn activity_feed(
        &self,
        kind: Option<ActivityItemKind>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ActivityPage, Fault> {
        let count = limit.clamp(1, 50).to_string();
        let mut params = vec![("limit", count.as_str())];
        if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
            params.push(("cursor", cursor));
        }
        let types_filter = kind.and_then(|k| match k {
            ActivityItemKind::Mention => Some("at_mention,broadcast,keyword,usergroup_mention"),
            ActivityItemKind::ThreadReply => Some("thread_v2,thread"),
            ActivityItemKind::Reaction => Some("reaction"),
            ActivityItemKind::MissedCall => None,
            ActivityItemKind::App => Some("app_mention,bot_dm,workflow"),
            ActivityItemKind::Invite => Some("channel_invite"),
            ActivityItemKind::Other => None,
        });
        if let Some(types) = types_filter {
            params.push(("types", types));
        }
        let feed = self.api("activity.feed", &params)?;
        let raw_items = feed["items"].as_array().map_or(&[][..], Vec::as_slice);
        let mut items = Vec::with_capacity(raw_items.len());
        for entry in raw_items {
            let Some(mapped) = self.map_activity_item(entry) else {
                continue;
            };
            if kind.is_some_and(|want| mapped.kind != want) {
                continue;
            }
            items.push(mapped);
        }
        let next = feed["response_metadata"]["next_cursor"]
            .as_str()
            .or_else(|| feed["next_cursor"].as_str())
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        Ok(ActivityPage { items, cursor: next })
    }

    fn map_activity_item(&self, entry: &Value) -> Option<ActivityItem> {
        let inner = if entry["item"].is_object() { &entry["item"] } else { entry };
        let raw_type = inner["type"]
            .as_str()
            .or_else(|| entry["type"].as_str())
            .unwrap_or("");
        let kind = match raw_type {
            "at_mention" | "mention" | "broadcast" | "keyword" | "at_channel" | "at_here" | "at_everyone" | "usergroup_mention" => {
                ActivityItemKind::Mention
            }
            "thread_v2" | "thread" | "thread_reply" => ActivityItemKind::ThreadReply,
            "reaction" | "reaction_added" => ActivityItemKind::Reaction,
            "app_mention" | "app_home" | "bot_dm" | "workflow" | "app" => ActivityItemKind::App,
            "channel_invite" | "invite" => ActivityItemKind::Invite,
            _ => ActivityItemKind::Other,
        };
        let msg = &inner["message"];
        let react = &inner["reaction"];
        let channel = msg["channel"]
            .as_str()
            .or_else(|| react["channel"].as_str())
            .or_else(|| inner["channel"].as_str())
            .or_else(|| entry["channel"].as_str())
            .filter(|c| !c.is_empty())?;
        let message_id = msg["ts"]
            .as_str()
            .or_else(|| react["item_ts"].as_str())
            .or_else(|| react["ts"].as_str())
            .or_else(|| inner["message_ts"].as_str())
            .or_else(|| inner["ts"].as_str())
            .or_else(|| entry["ts"].as_str())
            .unwrap_or("")
            .to_owned();
        let thread_ts = msg["thread_ts"]
            .as_str()
            .or_else(|| inner["thread_ts"].as_str())
            .or_else(|| entry["thread_ts"].as_str())
            .filter(|t| !t.is_empty() && *t != message_id.as_str());
        let thread_id = thread_ts.map(|root| thread_id(channel, root));
        let actor_id = react["user"]
            .as_str()
            .or_else(|| msg["author_user_id"].as_str())
            .or_else(|| msg["user"].as_str())
            .or_else(|| inner["user"].as_str())
            .or_else(|| entry["user"].as_str())
            .unwrap_or("")
            .to_owned();
        let actor = if !actor_id.is_empty() {
            self.user(&actor_id).name
        } else {
            entry["actor"]
                .as_str()
                .or_else(|| inner["actor"].as_str())
                .unwrap_or("")
                .to_owned()
        };
        let raw_emoji = react["name"]
            .as_str()
            .or_else(|| inner["reaction"].as_str())
            .or_else(|| entry["reaction"].as_str())
            .unwrap_or("");
        let emoji_str = if raw_emoji.is_empty() {
            String::new()
        } else {
            emoji::standard(raw_emoji).unwrap_or_else(|| format!(":{raw_emoji}:"))
        };
        let text = msg["text"]
            .as_str()
            .or_else(|| inner["text"].as_str())
            .or_else(|| entry["text"].as_str())
            .unwrap_or("")
            .to_owned();
        let feed_ts = entry["feed_ts"]
            .as_str()
            .or_else(|| entry["id"].as_str())
            .or_else(|| (!message_id.is_empty()).then_some(message_id.as_str()))
            .unwrap_or("");
        let time = entry["time"]
            .as_i64()
            .filter(|t| *t > 0)
            .unwrap_or_else(|| seconds(feed_ts));
        let id = entry["id"]
            .as_str()
            .or_else(|| entry["feed_ts"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{channel}:{message_id}:{raw_type}"));
        let unread = entry["is_unread"] == true || entry["unread"] == true;
        Some(ActivityItem {
            id,
            kind,
            conversation: self.ctx.key(channel),
            message_id,
            thread_id,
            actor,
            actor_id,
            emoji: emoji_str,
            text,
            time,
            unread,
        })
    }

    /// Marks one activity feed item or the whole feed read (`activity.mark`).
    pub(super) fn activity_mark_read(&self, item_id: Option<&str>) -> Result<(), Fault> {
        match item_id.filter(|id| !id.is_empty()) {
            Some(id) => {
                self.api("activity.mark", &[("item_id", id)])?;
            }
            None => {
                self.api("activity.mark", &[("all", "true")])?;
            }
        }
        Ok(())
    }

    fn map_bookmark(raw: &Value) -> Option<Bookmark> {
        let id = raw["id"].as_str().filter(|s| !s.is_empty())?.to_owned();
        let title = raw["title"].as_str().unwrap_or("").to_owned();
        let link = raw["link"].as_str().unwrap_or("").to_owned();
        let raw_emoji = raw["emoji"].as_str().unwrap_or("").trim().trim_matches(':');
        let emoji_str = if raw_emoji.is_empty() {
            String::new()
        } else {
            emoji::standard(raw_emoji).unwrap_or_else(|| format!(":{raw_emoji}:"))
        };
        let kind = match raw["type"].as_str().unwrap_or("link") {
            "canvas" | "quip" => BookmarkKind::Canvas,
            "folder" => BookmarkKind::Folder,
            _ => BookmarkKind::Link,
        };
        Some(Bookmark { id, title, link, emoji: emoji_str, kind })
    }

    /// Lists a channel's bookmarks (`bookmarks.list`). Returns empty if unsupported or absent.
    pub(super) fn bookmarks(&self, channel: &str) -> Vec<Bookmark> {
        let Ok(listed) = self.api("bookmarks.list", &[("channel_id", channel)]) else {
            return Vec::new();
        };
        listed["bookmarks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Self::map_bookmark)
            .collect()
    }

    /// Adds a link bookmark to a channel (`bookmarks.add`).
    pub(super) fn bookmark_add(&self, conversation: &str, title: &str, link: &str, emoji: &str) -> Result<Bookmark, Fault> {
        let channel = channel_of(conversation);
        let mut params = vec![
            ("channel_id", channel),
            ("title", title),
            ("type", "link"),
            ("link", link),
        ];
        if !emoji.is_empty() {
            params.push(("emoji", emoji));
        }
        let added = self.api("bookmarks.add", &params)?;
        Self::map_bookmark(&added["bookmark"]).ok_or_else(|| Fault::Slack("invalid_bookmark".to_owned()))
    }

    /// Edits an existing channel bookmark (`bookmarks.edit`).
    pub(super) fn bookmark_edit(
        &self,
        conversation: &str,
        bookmark_id: &str,
        title: Option<&str>,
        link: Option<&str>,
        emoji: Option<&str>,
    ) -> Result<Bookmark, Fault> {
        let channel = channel_of(conversation);
        let mut params = vec![("channel_id", channel), ("bookmark_id", bookmark_id)];
        if let Some(t) = title {
            params.push(("title", t));
        }
        if let Some(l) = link {
            params.push(("link", l));
        }
        if let Some(e) = emoji {
            params.push(("emoji", e));
        }
        let edited = self.api("bookmarks.edit", &params)?;
        Self::map_bookmark(&edited["bookmark"]).ok_or_else(|| Fault::Slack("invalid_bookmark".to_owned()))
    }

    /// Removes a channel bookmark (`bookmarks.remove`).
    pub(super) fn bookmark_remove(&self, conversation: &str, bookmark_id: &str) -> Result<(), Fault> {
        let channel = channel_of(conversation);
        self.api("bookmarks.remove", &[("channel_id", channel), ("bookmark_id", bookmark_id)])?;
        Ok(())
    }

    /// Cached `users.info` role, fetched once for this connection.
    fn workspace_role(&self) -> Result<WorkspaceRole, Fault> {
        if let Some(role) = self.state().role {
            return Ok(role);
        }
        let Some(me) = self.me.get().filter(|id| !id.is_empty()) else {
            return Err(Fault::Slack("no_permission".to_owned()));
        };
        let resp = self.api("users.info", &[("user", me.as_str())])?;
        let role = WorkspaceRole::from_user(&resp["user"]);
        self.state().role = Some(role);
        Ok(role)
    }

    /// Owner or workspace admin. Guests and members are refused here, with no write call.
    fn require_workspace_admin(&self) -> Result<(), Fault> {
        if self.workspace_role()?.is_workspace_admin() {
            Ok(())
        } else {
            Err(Fault::Slack("no_permission".to_owned()))
        }
    }

    /// Workspace info for `Request::SpaceInfo`: calls `team.info` for name/domain/email_domain
    /// and reads our own user record for workspace role (`Primary owner`, `Owner`, `Workspace admin`,
    /// `Guest`, or `Member`).
    pub(super) fn space_info(&self, _space_id: &str) -> Result<SpaceAbout, Fault> {
        let info = self.api("team.info", &[])?;
        let team = &info["team"];
        let domain = team["domain"]
            .as_str()
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .or_else(|| self.ctx.note("team:domain"))
            .unwrap_or_default();
        if !domain.is_empty() {
            self.ctx.set_note("team:domain", &domain);
        }
        let url = if !domain.is_empty() && domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            format!("https://{domain}.slack.com")
        } else {
            team["url"]
                .as_str()
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .map(|u| u.trim_end_matches('/').to_owned())
                .unwrap_or_default()
        };
        let email_domain = team["email_domain"].as_str().unwrap_or("").trim();
        let description = if email_domain.is_empty() {
            String::new()
        } else {
            format!("Email domain: {email_domain}")
        };
        let mut nickname = String::new();
        let mut roles = Vec::new();
        if let Some(me) = self.me.get().filter(|id| !id.is_empty()) {
            if let Ok(resp) = self.api("users.info", &[("user", me.as_str())]) {
                let u = &resp["user"];
                nickname = u["profile"]["display_name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| u["profile"]["real_name"].as_str().filter(|s| !s.is_empty()))
                    .or_else(|| u["name"].as_str())
                    .unwrap_or("")
                    .to_owned();
                let role = WorkspaceRole::from_user(u);
                self.state().role = Some(role);
                roles.push(role.label().to_owned());
            }
        }
        Ok(SpaceAbout {
            description,
            members: None,
            online: None,
            nickname,
            roles,
            notify: Notify::All,
            domain,
            url,
            ..SpaceAbout::default()
        })
    }

    /// Invites someone by email to this workspace (`users.admin.invite`, private web-client call).
    pub(super) fn invite_member(&self, email: &str, channels: &[String], real_name: &str) -> Result<(), Fault> {
        let email = email.trim();
        if email.is_empty() || !email.contains('@') {
            return Err(Fault::Slack("invalid_email".to_owned()));
        }
        self.require_workspace_admin()?;
        let channels_csv: String = channels
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let mut params = vec![("email", email), ("resend", "true")];
        if !channels_csv.is_empty() {
            params.push(("channels", channels_csv.as_str()));
        }
        let real_name = real_name.trim();
        if !real_name.is_empty() {
            params.push(("real_name", real_name));
        }
        self.api("users.admin.invite", &params)?;
        Ok(())
    }

    /// Uploads a custom emoji (`emoji.add`, multipart upload). Validates the local path through
    /// `crate::blobs::upload::allowed` and rejects non-image or oversized (> 1 MiB) files.
    pub(super) fn add_custom_emoji(&self, name: &str, path: &str) -> Result<(), Fault> {
        let clean = name.trim().trim_matches(':').to_ascii_lowercase();
        if clean.is_empty()
            || clean.len() > 100
            || !clean
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err(Fault::Slack("error_name_bad_characters".to_owned()));
        }
        let (real, _) = crate::blobs::upload::allowed(path.trim()).map_err(|why| Fault::Net(format!("emoji: {why}")))?;
        let ext = real
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "png" | "gif" | "jpg" | "jpeg" | "webp") {
            return Err(Fault::Slack("error_invalid_image".to_owned()));
        }
        let meta = std::fs::metadata(&real).map_err(|err| Fault::Net(format!("emoji: {err}")))?;
        const MAX_EMOJI_BYTES: u64 = 1024 * 1024;
        if meta.len() == 0 || meta.len() > MAX_EMOJI_BYTES {
            return Err(Fault::Slack("error_too_big".to_owned()));
        }
        let bytes = std::fs::read(&real).map_err(|err| Fault::Net(format!("emoji: {err}")))?;
        let mime = image_mime(&bytes).ok_or_else(|| Fault::Slack("error_invalid_image".to_owned()))?;
        self.require_workspace_admin()?;
        let filename = real
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("emoji.png");
        self.api_multipart(
            "emoji.add",
            &[("mode", "data"), ("name", clean.as_str())],
            "image",
            filename,
            mime,
            &bytes,
        )?;
        // Force next emoji catalog fetch to pick up the new custom emoji.
        self.ctx.set_note("emoji", "0");
        Ok(())
    }

    fn map_user_group(raw: &Value) -> Option<UserGroup> {
        let id = raw["id"].as_str().filter(|s| !s.is_empty())?.to_owned();
        let disabled = raw["date_delete"].as_i64().unwrap_or(0) > 0
            || raw["deleted_by"].as_str().is_some_and(|by| !by.is_empty());
        if disabled {
            return None;
        }
        let name = raw["name"].as_str().unwrap_or("").to_owned();
        let handle = raw["handle"].as_str().unwrap_or("").to_owned();
        let description = raw["description"].as_str().unwrap_or("").to_owned();
        let members: Vec<String> = raw["users"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|u| u.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
            .collect();
        let member_count = raw["user_count"]
            .as_u64()
            .map(|n| n as u32)
            .unwrap_or(members.len() as u32);
        Some(UserGroup { id, name, handle, description, members, member_count })
    }

    /// Lists active workspace user groups (`usergroups.list` with `include_users=true`).
    pub(super) fn user_groups(&self) -> Result<Vec<UserGroup>, Fault> {
        let listed = self.api("usergroups.list", &[("include_users", "true"), ("include_count", "true")])?;
        let me = self.me.get().map_or("", String::as_str);
        let mut out = Vec::new();
        let mut mine = std::collections::HashSet::new();
        for raw in listed["usergroups"].as_array().into_iter().flatten() {
            let Some(group) = Self::map_user_group(raw) else {
                continue;
            };
            if !group.handle.is_empty() {
                self.ctx.set_note(&format!("g:{}", group.id), &group.handle);
            }
            if !me.is_empty() && group.members.iter().any(|u| u == me) {
                mine.insert(group.id.clone());
            }
            out.push(group);
        }
        self.state().my_groups = mine;
        Ok(out)
    }

    /// Creates a user group (`usergroups.create`).
    pub(super) fn user_group_create(&self, name: &str, handle: &str, description: &str) -> Result<UserGroup, Fault> {
        let name = name.trim();
        let handle = handle.trim().trim_start_matches('@');
        if name.is_empty() || handle.is_empty() {
            return Err(Fault::Slack("invalid_arguments".to_owned()));
        }
        self.require_workspace_admin()?;
        let mut params = vec![("name", name), ("handle", handle)];
        let description = description.trim();
        if !description.is_empty() {
            params.push(("description", description));
        }
        let resp = self.api("usergroups.create", &params)?;
        let group = Self::map_user_group(&resp["usergroup"])
            .ok_or_else(|| Fault::Slack("invalid_arguments".to_owned()))?;
        if !group.handle.is_empty() {
            self.ctx.set_note(&format!("g:{}", group.id), &group.handle);
        }
        Ok(group)
    }

    /// Updates a user group's name, handle, or description (`usergroups.update`).
    pub(super) fn user_group_update(
        &self,
        group_id: &str,
        name: Option<&str>,
        handle: Option<&str>,
        description: Option<&str>,
    ) -> Result<UserGroup, Fault> {
        let group_id = group_id.trim();
        if group_id.is_empty() {
            return Err(Fault::Slack("no_such_subteam".to_owned()));
        }
        self.require_workspace_admin()?;
        let mut params = vec![("usergroup", group_id)];
        if let Some(n) = name.map(str::trim).filter(|s| !s.is_empty()) {
            params.push(("name", n));
        }
        if let Some(h) = handle.map(|s| s.trim().trim_start_matches('@')).filter(|s| !s.is_empty()) {
            params.push(("handle", h));
        }
        if let Some(d) = description {
            params.push(("description", d.trim()));
        }
        let resp = self.api("usergroups.update", &params)?;
        let group = Self::map_user_group(&resp["usergroup"])
            .ok_or_else(|| Fault::Slack("no_such_subteam".to_owned()))?;
        if !group.handle.is_empty() {
            self.ctx.set_note(&format!("g:{}", group.id), &group.handle);
        }
        Ok(group)
    }

    /// Replaces the members of a user group (`usergroups.users.update`).
    pub(super) fn user_group_members(&self, group_id: &str, users: &[String]) -> Result<UserGroup, Fault> {
        let group_id = group_id.trim();
        if group_id.is_empty() {
            return Err(Fault::Slack("no_such_subteam".to_owned()));
        }
        self.require_workspace_admin()?;
        let csv: String = users
            .iter()
            .map(|u| u.trim())
            .filter(|u| !u.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let resp = self.api("usergroups.users.update", &[("usergroup", group_id), ("users", csv.as_str())])?;
        let group = Self::map_user_group(&resp["usergroup"])
            .ok_or_else(|| Fault::Slack("no_such_subteam".to_owned()))?;
        if let Some(me) = self.me.get().filter(|id| !id.is_empty()) {
            let mut state = self.state();
            if group.members.iter().any(|u| u == me) {
                state.my_groups.insert(group.id.clone());
            } else {
                state.my_groups.remove(&group.id);
            }
        }
        Ok(group)
    }

    /// Fetches a Slack app's About metadata and (optionally) its published Home tab.
    /// Called only when the user opens an app's Home or About tab — never at connect.
    ///
    /// Unverified against a real workspace: `apps.profile.get` and `views.get` (`type=home`)
    /// are web-client calls; falls back to `bots.info` for About details and `apps.home.get`
    /// for Home blocks when unavailable.
    pub(super) fn app_view(&self, conversation_id: &str, include_home: bool) -> Result<AppView, Fault> {
        let channel = channel_of(conversation_id);
        let stored = self.ctx.known(channel).map_err(|err| Fault::Net(err.to_string()))?;
        let peer_id = stored
            .as_ref()
            .and_then(|row| row.peer.clone())
            .filter(|id| !id.is_empty());
        let mut name = stored.as_ref().map(|r| r.title.clone()).unwrap_or_default();
        let (mut app_id, mut bot_id) = peer_id
            .as_deref()
            .and_then(|uid| self.ctx.note(&format!("app:{uid}")))
            .map(|raw| {
                let mut parts = raw.splitn(2, '|');
                (
                    parts.next().unwrap_or("").to_owned(),
                    parts.next().unwrap_or("").to_owned(),
                )
            })
            .unwrap_or_default();

        if app_id.is_empty() && bot_id.is_empty() {
            if let Some(uid) = peer_id.as_deref() {
                if let Ok(info) = self.api("users.info", &[("user", uid)]) {
                    let u = &info["user"];
                    let prof = &u["profile"];
                    app_id = prof["api_app_id"].as_str().unwrap_or("").to_owned();
                    bot_id = prof["bot_id"]
                        .as_str()
                        .or_else(|| u["bot_id"].as_str())
                        .unwrap_or("")
                        .to_owned();
                    if name.is_empty() {
                        name = prof["real_name"]
                            .as_str()
                            .or_else(|| u["real_name"].as_str())
                            .or_else(|| u["name"].as_str())
                            .unwrap_or("")
                            .to_owned();
                    }
                    if !app_id.is_empty() || !bot_id.is_empty() {
                        self.ctx.set_note(&format!("app:{uid}"), &format!("{app_id}|{bot_id}"));
                    }
                }
            }
        }

        let mut description = String::new();
        let mut commands = Vec::new();
        let mut links = Vec::new();
        let mut installed_by = String::new();
        let mut home_enabled = false;

        let valid_url = |s: &str| -> Option<String> {
            let trimmed = s.trim();
            trimmed.starts_with("https://").then(|| trimmed.to_owned())
        };

        // 1. Try `apps.profile.get` when we know `app_id` (or `bot_id`).
        let profile_resp = if !app_id.is_empty() {
            self.api("apps.profile.get", &[("app", app_id.as_str())]).ok()
        } else if !bot_id.is_empty() {
            self.api("apps.profile.get", &[("bot", bot_id.as_str())]).ok()
        } else {
            None
        };
        if let Some(resp) = &profile_resp {
            let app = if resp["app"].is_object() { &resp["app"] } else { resp };
            if app_id.is_empty() {
                if let Some(id) = app["id"].as_str().or_else(|| app["app_id"].as_str()).filter(|s| !s.is_empty()) {
                    app_id = id.to_owned();
                }
            }
            if let Some(n) = app["name"].as_str().filter(|s| !s.is_empty()) {
                name = n.to_owned();
            }
            description = app["long_description"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| app["short_description"].as_str().filter(|s| !s.is_empty()))
                .or_else(|| app["description"].as_str().filter(|s| !s.is_empty()))
                .unwrap_or("")
                .trim()
                .to_owned();
            home_enabled = app["app_home_enabled"] == true
                || app["is_app_home_enabled"] == true
                || app["home_tab_enabled"] == true;
            if let Some(installer) = app["installed_by"].as_str().filter(|s| !s.is_empty()) {
                installed_by = if installer.starts_with('U') || installer.starts_with('W') {
                    self.user(installer).name
                } else {
                    installer.to_owned()
                };
            }
            for cmd in app["slash_commands"]
                .as_array()
                .or_else(|| app["commands"].as_array())
                .into_iter()
                .flatten()
                .take(32)
            {
                let Some(command) = cmd["command"].as_str().filter(|s| !s.is_empty()) else {
                    continue;
                };
                commands.push(AppSlashCommand {
                    command: command.to_owned(),
                    description: cmd["description"].as_str().unwrap_or("").trim().to_owned(),
                    usage: cmd["usage_hint"]
                        .as_str()
                        .or_else(|| cmd["usage"].as_str())
                        .unwrap_or("")
                        .trim()
                        .to_owned(),
                });
            }
            for (field, label) in [
                ("app_homepage_url", "Homepage"),
                ("help_url", "Support"),
                ("privacy_policy_url", "Privacy policy"),
            ] {
                if let Some(url) = app[field].as_str().and_then(valid_url) {
                    links.push(AppLink { label: label.to_owned(), url });
                }
            }
            for raw_link in app["links"].as_array().into_iter().flatten().take(12) {
                if let Some(url) = raw_link["url"].as_str().and_then(valid_url) {
                    let label = raw_link["label"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Link")
                        .to_owned();
                    links.push(AppLink { label, url });
                }
            }
        } else if !bot_id.is_empty() {
            // Fallback to documented `bots.info`.
            if let Ok(resp) = self.api("bots.info", &[("bot", bot_id.as_str())]) {
                let bot = &resp["bot"];
                if let Some(n) = bot["name"].as_str().filter(|s| !s.is_empty()) {
                    name = n.to_owned();
                }
                if app_id.is_empty() {
                    if let Some(id) = bot["app_id"].as_str().filter(|s| !s.is_empty()) {
                        app_id = id.to_owned();
                    }
                }
            }
        }

        let team = &self.ctx.account.account;
        let domain = self.ctx.note("team:domain").unwrap_or_default();
        let directory_url = if !domain.is_empty() && domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            format!("https://{domain}.slack.com/apps")
        } else {
            format!("https://app.slack.com/apps/{team}")
        };
        let open_url = format!("https://app.slack.com/client/{team}/{channel}");

        // 2. Fetch published Home tab blocks when requested.
        let home = if include_home {
            let view_resp = if !app_id.is_empty() {
                self.api("views.get", &[("type", "home"), ("app_id", app_id.as_str())])
                    .or_else(|_| self.api("apps.home.get", &[("app_id", app_id.as_str())]))
                    .ok()
            } else if !bot_id.is_empty() {
                self.api("views.get", &[("type", "home"), ("bot_id", bot_id.as_str())]).ok()
            } else {
                None
            };
            match view_resp {
                Some(resp) => {
                    let view = if resp["view"].is_object() { &resp["view"] } else { &resp };
                    let view_id = view["id"].as_str().unwrap_or("").to_owned();
                    let blocks = view["blocks"].as_array().map_or(&[][..], Vec::as_slice);
                    if !blocks.is_empty() {
                        home_enabled = true;
                        let (text, spans, cards, controls) = self.parse_app_home_blocks(blocks);
                        Some(AppHome {
                            app_id: app_id.clone(),
                            view_id,
                            text,
                            spans,
                            cards,
                            controls,
                            empty_reason: String::new(),
                        })
                    } else {
                        Some(AppHome {
                            app_id: app_id.clone(),
                            view_id,
                            empty_reason: "This app has not published a Home view.".to_owned(),
                            ..AppHome::default()
                        })
                    }
                }
                None => Some(AppHome {
                    app_id: app_id.clone(),
                    empty_reason: "This app's Home tab isn't available here. Open it in Slack to interact with it.".to_owned(),
                    ..AppHome::default()
                }),
            }
        } else {
            None
        };

        Ok(AppView {
            profile: AppProfile {
                app_id,
                bot_id,
                name,
                description,
                commands,
                links,
                installed_by,
                open_url,
                directory_url,
                home_enabled,
            },
            home,
        })
    }

    /// Built-in Slack slash commands plus workspace app commands from `commands.list` (cached 24 h
    /// in store notes, same policy as `emoji.list`).
    pub(super) fn slash_commands(&self) -> Result<Vec<SlashCommand>, Fault> {
        let mut out = builtin_slash_commands();
        let now = crate::hub::now();
        let cached_at = self.ctx.note("commands:at").and_then(|s| s.parse::<i64>().ok());
        let workspace: Vec<SlashCommand> = if cached_at.is_some_and(|at| now.saturating_sub(at) < 86_400) {
            self.ctx
                .note("commands:json")
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default()
        } else {
            self.ctx.set_note("commands:at", &now.to_string());
            let mut fetched = Vec::new();
            if let Ok(resp) = self.api("commands.list", &[]) {
                for raw in resp["commands"].as_array().into_iter().flatten().take(200) {
                    let Some(cmd) = raw["command"]
                        .as_str()
                        .or_else(|| raw["name"].as_str())
                        .filter(|s| !s.is_empty())
                    else {
                        continue;
                    };
                    let command = if cmd.starts_with('/') { cmd.to_owned() } else { format!("/{cmd}") };
                    if out.iter().any(|b| b.command.eq_ignore_ascii_case(&command)) {
                        continue;
                    }
                    fetched.push(SlashCommand {
                        command,
                        description: raw["description"]
                            .as_str()
                            .or_else(|| raw["desc"].as_str())
                            .unwrap_or("")
                            .trim()
                            .to_owned(),
                        usage: raw["usage"]
                            .as_str()
                            .or_else(|| raw["usage_hint"].as_str())
                            .unwrap_or("")
                            .trim()
                            .to_owned(),
                        builtin: false,
                    });
                }
            }
            if let Ok(json) = serde_json::to_string(&fetched) {
                self.ctx.set_note("commands:json", &json);
            }
            fetched
        };
        out.extend(workspace);
        Ok(out)
    }

    /// Fetches unread messages for the newest unread conversations (All Unreads, #606).
    ///
    /// Runs ONLY when the user opens All Unreads (never at connect), is capped to at most 15
    /// conversations per call, and paces every `conversations.history` request through `self.api`
    /// (`self.pace`, half the documented tier rate). Upgrades coarse `has_unreads` counts to the
    /// exact unread message count.
    pub(super) fn all_unreads(&self, limit: u32) -> Result<Vec<UnreadGroup>, Fault> {
        const MAX_CHANNELS: u32 = 15;
        let cap = if limit == 0 { MAX_CHANNELS } else { limit.clamp(1, MAX_CHANNELS) };
        let unread = self.ctx.unread_conversations(cap).map_err(|err| Fault::Net(err.to_string()))?;
        let mut groups = Vec::with_capacity(unread.len());
        for mut conv in unread {
            let last_read = conv.last_read.as_deref().filter(|s| !s.is_empty());
            let mut params = vec![("channel", conv.key.id.as_str()), ("limit", "50")];
            if let Some(oldest) = last_read {
                params.push(("oldest", oldest));
            }
            let history = match self.api("conversations.history", &params) {
                Ok(history) => history,
                Err(fault @ (Fault::Auth(_) | Fault::Limited(_))) => return Err(fault),
                Err(_) => continue,
            };
            let entries = history["messages"].as_array().map_or(&[][..], Vec::as_slice);
            self.authors(entries);
            let mut messages: Vec<Message> = entries
                .iter()
                .filter_map(|entry| self.message(&conv.key.id, entry))
                .filter(|msg| last_read.is_none_or(|lr| msg.id.as_str() > lr))
                .collect();
            messages.sort_by(|a, b| a.id.cmp(&b.id));
            if !messages.is_empty() {
                let _ = self.ctx.history(&conv.key.id, &messages);
            }
            let real_unread = if !messages.is_empty() {
                messages.len() as u32
            } else {
                conv.unread
            };
            let latest_sent = messages.last().map_or(conv.updated, |m| m.sent.max(conv.updated));
            if real_unread != conv.unread {
                let _ = self.ctx.counts(&conv.key.id, real_unread, conv.mentions, last_read, latest_sent);
                conv.unread = real_unread;
                conv.updated = latest_sent;
            }
            groups.push(UnreadGroup {
                conversation: conv,
                messages,
                total_unread: real_unread,
            });
        }
        Ok(groups)
    }
}

pub(super) fn builtin_slash_commands() -> Vec<SlashCommand> {
    [
        ("/me", "Display action text", "[text]"),
        ("/shrug", "Append ¯\\_(ツ)_/¯ to your message", "[text]"),
        ("/topic", "Set the channel topic", "[text]"),
        ("/invite", "Invite someone to this channel", "@user"),
        ("/kick", "Remove someone from this channel", "@user"),
        ("/remove", "Remove someone from this channel", "@user"),
        ("/leave", "Leave this channel", ""),
        ("/join", "Join a channel", "#channel"),
        ("/archive", "Archive this channel", ""),
        ("/mute", "Mute this channel", ""),
        ("/status", "Set your status", "[:emoji:] [text]"),
        ("/dnd", "Pause notifications", "[minutes | off]"),
        ("/away", "Set yourself away", ""),
        ("/active", "Set yourself active", ""),
        ("/remind", "Set a reminder", "[text] [in N minutes]"),
    ]
    .into_iter()
    .map(|(command, description, usage)| SlashCommand {
        command: command.to_owned(),
        description: description.to_owned(),
        usage: usage.to_owned(),
        builtin: true,
    })
    .collect()
}
