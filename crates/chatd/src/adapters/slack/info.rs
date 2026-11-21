//! What the panes, the picker and the lists ask of Slack beyond reading and sending: a
//! conversation's info, its notifications, its pins and files, the workspace's emoji, the
//! threads the user follows, and the newest message of rows on screen. Each runs because the
//! user opened or clicked something, or (previews) because the rows are on screen, paced.

use super::{emoji, fnv, seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{ActivityPage, CatalogPage, ConversationAbout, Failure, SharedPage};
use crate::store::Filing;
use protocol::{ActivityItem, ActivityItemKind, Bookmark, BookmarkKind, CatalogItem, ConversationKind, MediaFile, MediaKind, Message, Notify, SharedItem, SharedKind};
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
        let muted = self.state().muted.contains(channel);
        // Unverified: a channel set to mentions only is not told apart from one that is not;
        // `all_notifications_prefs` would say, and is not read for this.
        let notify = if muted { Notify::Nothing } else { Notify::All };
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
        let bookmarks = self.bookmarks(channel_of(conversation));
        Ok(ConversationAbout {
            description,
            members: channel["num_members"].as_u64().map(|count| count as u32),
            created: channel["created"].as_i64().filter(|created| *created > 0),
            creator: creator.unwrap_or_default(),
            notify,
            external: names.join(", "),
            bookmarks,
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

    /// The pinned messages, as `pins.list` gives them. Files pinned on their own are not
    /// messages and are left out.
    pub(super) fn pinned(&self, conversation: &str) -> Result<Vec<Message>, Fault> {
        let channel = channel_of(conversation);
        let pins = self.api("pins.list", &[("channel", channel)])?;
        let items = pins["items"].as_array().map_or(&[][..], Vec::as_slice);
        let messages: Vec<Value> = items.iter().filter(|item| item["type"] == "message").map(|item| item["message"].clone()).collect();
        self.authors(&messages);
        let mut pinned: Vec<Message> = messages.iter().filter_map(|message| self.message(channel, message)).collect();
        for message in &mut pinned {
            message.pinned = true;
        }
        pinned.sort_by_key(|message| std::cmp::Reverse(message.sent));
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
                    url: String::new(),
                    title: String::new(),
                });
            }
        }
        let paging = &listed["paging"];
        let more = paging["page"].as_u64().unwrap_or(1) < paging["pages"].as_u64().unwrap_or(1);
        // The total counts files of every kind asked for; for "all" that includes pictures.
        let total = paging["total"].as_u64().filter(|_| before.is_none() && kind == SharedKind::Media).map(|total| total as u32);
        Ok(SharedPage { items, more, total })
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
            });
        }
        self.ctx.media(&files).map_err(|err| Fault::Net(err.to_string()))?;
        let more = matching.next().is_some();
        Ok(CatalogPage { items, cursor: more.then(|| walked.to_string()) })
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
            self.state().previewed = Some(Instant::now());
            self.report(self.newest_of(&id));
        }
    }

    fn newest_of(&self, channel: &str) -> Result<(), Failure> {
        let history = self.api("conversations.history", &[("channel", channel), ("limit", "1")])?;
        let entries = history["messages"].as_array().map_or(&[][..], Vec::as_slice);
        self.authors(entries);
        let messages: Vec<Message> = entries.iter().filter_map(|entry| self.message(channel, entry)).collect();
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
}
