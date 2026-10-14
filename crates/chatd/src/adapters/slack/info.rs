//! What the panes, the picker and the lists ask of Slack beyond reading and sending: a
//! conversation's info, its notifications, its pins and files, the workspace's emoji, the
//! threads the user follows, and the newest message of rows on screen. Each runs because the
//! user opened or clicked something, or (previews) because the rows are on screen, paced.

use super::{fnv, seconds, thread_id, thread_of, Fault, Slack};
use crate::adapters::{CatalogPage, ConversationAbout, Failure, SharedPage};
use crate::store::Filing;
use protocol::{CatalogItem, ConversationKind, MediaFile, MediaKind, Message, Notify, SharedItem, SharedKind};
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

fn channel_of(conversation: &str) -> &str {
    thread_of(conversation).map_or(conversation, |(channel, _)| channel)
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
        Ok(ConversationAbout {
            description: said("purpose").or(said("topic")).unwrap_or("").to_owned(),
            members: channel["num_members"].as_u64().map(|count| count as u32),
            created: channel["created"].as_i64().filter(|created| *created > 0),
            creator: creator.unwrap_or_default(),
            notify,
            // Unverified: the other organization's name needs a lookup per team; the shared
            // channel is marked on its row, and its name is left out here.
            external: String::new(),
            ..ConversationAbout::default()
        })
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
            SharedKind::Links => return None,
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
}
