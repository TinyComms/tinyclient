//! A message with files or options: files through Slack's external upload, a link preview
//! left off, a message Slack itself sends later.
//!
//! Files go the way Slack documents for apps, which session clients are reported to use too
//! (unverified against a real workspace with this kind of session): `files.getUploadURLExternal`
//! names each file and its size and answers an address; the bytes are posted there straight
//! from the path; one `files.completeUploadExternal` shares them all in the conversation with
//! the text as their comment. Slack names the message only on the socket, so the send ends when
//! the socket shows a message holding the first file.

use super::{escape, thread_of, Slack};
use crate::blobs::upload::{Target, Upload};
use protocol::{Message, Outgoing};
use serde_json::{json, Value};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

/// Slack's limit on one file.
pub(super) const LARGEST: u64 = 1 << 30;
/// Files one message carries.
pub(super) const FILES: usize = 10;
/// How long the socket is given to show the shared files before the send ends without it.
const SHOWN_WITHIN: Duration = Duration::from_secs(20);
/// Slack schedules no further ahead than this.
const SCHEDULE_AHEAD: i64 = 120 * 86_400;

impl Slack {
    /// The work of one `SendWith`, run by the upload helper on a thread of its own, which reads
    /// the files and ends the send through `Context::sent`.
    pub(super) fn send_with(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &Outgoing, up: &mut Upload) -> Result<Message, String> {
        if with.pick.is_some() {
            return Err("Slack has no GIFs or stickers to pick from. Paste a GIF's link, or attach the file.".to_owned());
        }
        if let Some(at) = with.schedule {
            if !up.files().is_empty() {
                return Err("Slack schedules text only, not files.".to_owned());
            }
            return self.schedule(conversation, text, at, with.suppress_embeds);
        }
        if up.files().is_empty() {
            return self.post(conversation, client_id, text, reply_to, with.suppress_embeds);
        }
        if up.files().len() > FILES {
            return Err(format!("Slack takes {FILES} files in one message."));
        }
        if let Some(big) = up.files().iter().find(|file| file.bytes > LARGEST) {
            return Err(format!("{} is too big for Slack (limit 1 GB).", big.name));
        }
        let (channel, thread) = match thread_of(conversation) {
            Some((channel, root)) => (channel, Some(root)),
            None => (conversation, reply_to),
        };
        let ids = self.upload(up)?;
        up.check()?;

        // The socket names the message that holds the files: it is watched for before Slack is
        // told to share them, so it cannot pass unseen.
        let (shown, seen) = std::sync::mpsc::channel();
        self.state().shares.insert(ids[0].clone(), shown);
        let files: Vec<Value> = ids.iter().zip(up.files()).map(|(id, file)| json!({"id": id, "title": file.name})).collect();
        let files = json!(files).to_string();
        let comment = escape(text);
        let mut asked = vec![("files", files.as_str()), ("channel_id", channel), ("initial_comment", comment.as_str())];
        if let Some(root) = thread {
            asked.push(("thread_ts", root));
        }
        let completed = match self.api("files.completeUploadExternal", &asked) {
            Ok(completed) => completed,
            Err(fault) => {
                self.state().shares.remove(&ids[0]);
                return Err(super::reason(fault));
            }
        };
        let deadline = Instant::now() + SHOWN_WITHIN;
        let message = loop {
            match seen.recv_timeout(Duration::from_millis(500)) {
                Ok(message) => break Some(message),
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline && !self.ctx.stopped() => {}
                Err(_) => break None,
            }
        };
        self.state().shares.remove(&ids[0]);
        if let Some(message) = message {
            return Ok(message);
        }
        // CONTRACT: the socket did not show it in time. Slack took the files, so the send is not
        // failed (a retry would post them twice): it ends as the message Slack's answer
        // describes, named by the share's timestamp where it gives one. Should the socket show
        // it later under another name, it is listed twice until reopened.
        let shares = &completed["files"][0]["shares"];
        let ts = ["public", "private"].iter().find_map(|access| shares[*access][channel][0]["ts"].as_str());
        let ts = ts.map(str::to_owned).unwrap_or_else(|| format!("{}.000000", crate::hub::now()));
        let me = self.me.get().cloned().unwrap_or_default();
        let event = json!({"type": "message", "ts": ts, "user": me, "text": comment, "files": completed["files"]});
        self.message(channel, &event).ok_or_else(|| "Slack took the files but described no message.".to_owned())
    }

    /// Asks for an address per file and posts each file's bytes there from its path. Answers
    /// the file ids, in order.
    fn upload(&self, up: &mut Upload) -> Result<Vec<String>, String> {
        let mut ids = Vec::with_capacity(up.files().len());
        for at in 0..up.files().len() {
            up.check()?;
            let (name, length) = (up.files()[at].name.clone(), up.files()[at].bytes.to_string());
            let address = self.api("files.getUploadURLExternal", &[("filename", name.as_str()), ("length", length.as_str())]);
            let address = address.map_err(|fault| format!("{name} could not be sent: {}", super::reason(fault)))?;
            let (Some(url), Some(id)) = (address["upload_url"].as_str(), address["file_id"].as_str()) else {
                return Err(format!("{name} could not be sent: Slack gave no address for it."));
            };
            // As the session's own browser, with its cookie: the address is on Slack's host.
            let target = Target::post(url)
                .header("Content-Type", "application/octet-stream")
                .header("User-Agent", &self.session.user_agent)
                .header("Cookie", &self.cookie());
            up.send(at, &target)?;
            ids.push(id.to_owned());
        }
        Ok(ids)
    }

    /// Hands Slack a message to send at `at` (unix seconds). Nothing here runs a timer.
    ///
    /// CONTRACT: there is no state for a scheduled message, so it ends as sent, dated when it
    /// will go, under Slack's id for the schedule. When Slack posts it, the socket brings it
    /// under its own timestamp, which is a second row until the scheduled one ages out.
    fn schedule(&self, conversation: &str, text: &str, at: i64, suppress: bool) -> Result<Message, String> {
        let now = crate::hub::now();
        if at <= now || at > now + SCHEDULE_AHEAD {
            return Err("Slack schedules a message up to 120 days ahead.".to_owned());
        }
        let (channel, thread) = match thread_of(conversation) {
            Some((channel, root)) => (channel, Some(root)),
            None => (conversation, None),
        };
        let (escaped, post_at) = (escape(text), at.to_string());
        let mut asked = vec![("channel", channel), ("text", escaped.as_str()), ("post_at", post_at.as_str())];
        if let Some(root) = thread {
            asked.push(("thread_ts", root));
        }
        if suppress {
            asked.extend([("unfurl_links", "false"), ("unfurl_media", "false")]);
        }
        let scheduled = self.api("chat.scheduleMessage", &asked).map_err(super::reason)?;
        let id = scheduled["scheduled_message_id"].as_str().ok_or("Slack did not say it was scheduled.")?;
        let me = self.me.get().cloned().unwrap_or_default();
        let event = json!({"type": "message", "user": me, "text": escaped, "ts": format!("{at}.000000")});
        let mut message = self.message(channel, &event).ok_or("Slack did not say it was scheduled.")?;
        message.id = format!("scheduled:{id}");
        message.sent = scheduled["post_at"].as_i64().unwrap_or(at);
        Ok(message)
    }

    /// A message on the socket that holds a file a send of ours is waiting to see shared.
    /// True when it was handed to that send, which records it as its own.
    pub(super) fn shared_here(&self, event: &Value, message: &Message) -> bool {
        let files = event["files"].as_array().into_iter().flatten().filter_map(|file| file["id"].as_str());
        let mut state = self.state();
        for id in files {
            if let Some(waiting) = state.shares.remove(id) {
                return waiting.send(message.clone()).is_ok();
            }
        }
        false
    }
}
