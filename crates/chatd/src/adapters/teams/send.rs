//! Files in Teams:
//! - In chats: uploaded to the user's own OneDrive under `"Microsoft Teams Chat Files"` via a
//!   chunked Graph upload session, then shared strictly with the chat's human members via
//!   `POST /v1.0/me/drive/items/{id}/invite` (never an organization-wide `createLink`).
//! - In channels (standard, private, or shared): uploaded directly into the channel's SharePoint
//!   folder resolved via `GET /v1.0/teams/{groupId}/channels/{channelId}/filesFolder`, where
//!   channel members already have access.
//! - The sent message names each uploaded file in `properties.files`.

use super::{escaped, Auth, Body, Fault, Teams, GRAPH};
use crate::blobs::upload::{self, Target, Upload};
use protocol::{Attachment, MediaFile, Message, Outgoing};
use serde_json::{json, Value};
use std::sync::Arc;

/// One request of an upload session: a multiple of 320 KiB, as OneDrive requires.
const CHUNK: u64 = 10 * 320 * 1024;
/// Where Teams keeps the files sent in chats.
const FOLDER: &str = "Microsoft Teams Chat Files";

impl Teams {
    pub(super) fn send_with(self: &Arc<Self>, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: Outgoing) {
        if let Some(at) = with.schedule {
            let outcome = self.schedule_chat(conversation, client_id, text, reply_to, at, &with);
            self.ctx.sent(conversation, client_id, outcome);
            return;
        }
        if with.files.is_empty() {
            self.post_with_outgoing(conversation, client_id, text, reply_to, &with);
            return;
        }
        let (teams, conversation_id, client, text, reply, outgoing) = (
            self.clone(),
            conversation.to_owned(),
            client_id.to_owned(),
            text.to_owned(),
            reply_to.map(str::to_owned),
            with.clone(),
        );
        upload::spawn(&self.ctx, conversation, client_id, &with.files, move |up| {
            teams.with_files(&conversation_id, &client, &text, reply.as_deref(), &outgoing, up)
        });
    }

    /// Schedules a text message for future delivery in a Teams chat.
    /// Teams supports scheduled send in chats only; channels and reply chains are refused.
    fn schedule_chat(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, at: i64, with: &Outgoing) -> Result<Message, String> {
        let thread = super::thread_of(conversation);
        if super::tree::is_channel(thread) || super::chain_of(conversation).is_some() {
            return Err("Teams only supports scheduling messages in chats, not channels.".to_owned());
        }
        if !with.files.is_empty() {
            return Err("Teams scheduled messages support text only.".to_owned());
        }
        let now = crate::hub::now();
        if at <= now {
            return Err("Scheduled send time must be in the future.".to_owned());
        }
        let scheduled_iso = super::iso_utc(at);
        let props = json!({"scheduledTime": scheduled_iso});
        let mut message = self
            .posted_with(conversation, client_id, text, reply_to, with, Some(props))
            .map_err(super::reason)?;
        let raw_id = message.id.clone();
        self.state().scheduled.insert(raw_id.clone(), at);
        message.id = format!("scheduled:{raw_id}");
        message.sent = at;
        Ok(message)
    }

    pub(super) fn cancel_send(&self, client_id: &str) {
        upload::cancel(&self.ctx.account, client_id);
    }

    fn with_files(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &Outgoing, up: &mut Upload) -> Result<Message, String> {
        let mut named = Vec::new();
        let mut attachments = Vec::new();
        for at in 0..up.files().len() {
            let (entry, attachment) = self.one_file(conversation, up, at)?;
            named.push(entry);
            attachments.push(attachment);
        }
        up.check()?;
        let properties = json!({"files": Value::Array(named).to_string()});
        let mut message = self.posted_with(conversation, client_id, text, reply_to, with, Some(properties)).map_err(super::reason)?;
        message.attachments = attachments;
        Ok(message)
    }

    fn upload_url_ok(&self, url: &str) -> bool {
        url.starts_with("https://")
            || self.moved.as_ref().is_some_and(|base| url.starts_with(base.as_str()))
            || (self.graph != GRAPH && url.starts_with(self.graph.as_str()))
    }

    /// Resolves the SharePoint `(driveId, folderId)` for a Teams channel (standard, private, or shared).
    fn channel_folder(&self, thread: &str) -> Result<(String, String), String> {
        let row = self.ctx.known(thread).map_err(|err| err.to_string())?;
        let mut group_id = row
            .as_ref()
            .and_then(|r| r.space.as_deref())
            .and_then(|space| self.ctx.note(&format!("group:{space}")))
            .or_else(|| self.ctx.note(&format!("group:{thread}")));
        if group_id.as_deref().is_none_or(str::is_empty) {
            let path = format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread));
            if let Ok(answer) = self.chat("GET", &path, Body::None, true) {
                group_id = answer.body["properties"]["groupId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
        }
        let Some(group_id) = group_id.filter(|s| !s.is_empty()) else {
            return Err("Channel SharePoint folder could not be resolved (missing team group id).".to_owned());
        };
        let url = format!(
            "{}/v1.0/teams/{}/channels/{}/filesFolder",
            self.graph,
            escaped(&group_id),
            escaped(thread)
        );
        let folder = self
            .call("GET", &url, Auth::Bearer(GRAPH), Body::None, true)
            .map_err(|fault| format!("Channel SharePoint folder could not be reached: {}", super::reason(fault)))?
            .body;
        let drive_id = folder["parentReference"]["driveId"].as_str().unwrap_or("").to_owned();
        let folder_id = folder["id"].as_str().unwrap_or("").to_owned();
        if drive_id.is_empty() || folder_id.is_empty() {
            return Err("Teams returned no SharePoint folder for this channel.".to_owned());
        }
        Ok((drive_id, folder_id))
    }

    /// Resolves the email addresses of the other human members in a chat thread so a OneDrive file
    /// can be shared only with them via Graph `/invite`.
    fn chat_recipients(&self, thread: &str) -> Result<Vec<String>, String> {
        if thread == "48:notes" {
            return Ok(Vec::new());
        }
        let me = self.me();
        let path = format!("/v1/threads/{}?view=msnp24Equivalent", escaped(thread));
        let mut other_ids: Vec<String> = match self.chat("GET", &path, Body::None, true) {
            Ok(answer) => answer.body["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str())
                .filter(|id| *id != me && !id.starts_with("28:"))
                .map(str::to_owned)
                .collect(),
            Err(_) => Vec::new(),
        };
        if other_ids.is_empty() {
            if let Ok(Some(row)) = self.ctx.known(thread) {
                if let Some(peer) = row.peer.filter(|p| p != &me && !p.starts_with("28:")) {
                    other_ids.push(peer);
                }
            }
        }
        other_ids.sort();
        other_ids.dedup();
        if other_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.lookup(&other_ids);
        let mut emails: Vec<String> = other_ids
            .iter()
            .filter_map(|id| self.cached(id))
            .map(|k| k.mail)
            .filter(|mail| !mail.is_empty())
            .collect();
        emails.sort();
        emails.dedup();
        if emails.is_empty() {
            return Err("Could not resolve chat members' addresses to share this file.".to_owned());
        }
        Ok(emails)
    }

    /// Uploads one file (to the channel's SharePoint folder, or to OneDrive with per-member
    /// `/invite` sharing for chats) and returns the `properties.files` entry + local attachment.
    fn one_file(&self, conversation: &str, up: &mut Upload, at: usize) -> Result<(Value, Attachment), String> {
        let (name, bytes) = (up.files()[at].name.clone(), up.files()[at].bytes);
        let failed = |fault: Fault| format!("{name} could not be sent: {}", super::reason(fault));
        if bytes == 0 {
            return Err(format!("{name} is empty."));
        }
        let thread = super::thread_of(conversation);
        let is_channel = super::tree::is_channel(thread);
        let session_path = if is_channel {
            let (drive_id, folder_id) = self.channel_folder(thread)?;
            format!(
                "{}/v1.0/drives/{}/items/{}:/{}:/createUploadSession",
                self.graph,
                escaped(&drive_id),
                escaped(&folder_id),
                escaped(&name)
            )
        } else {
            format!(
                "{}/v1.0/me/drive/root:/{}/{}:/createUploadSession",
                self.graph,
                escaped(FOLDER),
                escaped(&name)
            )
        };
        let session = self
            .call(
                "POST",
                &session_path,
                Auth::Bearer(GRAPH),
                Body::Json(&json!({"item": {"@microsoft.graph.conflictBehavior": "rename"}})),
                true,
            )
            .map_err(failed)?;
        let Some(url) = session.body["uploadUrl"].as_str().map(str::to_owned).filter(|u| self.upload_url_ok(u)) else {
            return Err(format!("{name} could not be sent: Microsoft gave no valid upload address."));
        };
        // The session's address is signed: it takes no token, and goes nowhere else.
        let target = Target::put(url);
        let mut start = 0;
        let mut item = Value::Null;
        while start < bytes {
            let len = CHUNK.min(bytes - start);
            let answer = up.send_range(at, &target, start, len)?;
            start += len;
            if start >= bytes {
                item = serde_json::from_str(&answer.body).unwrap_or(Value::Null);
            }
        }
        let (Some(id), Some(web)) = (item["id"].as_str(), item["webUrl"].as_str()) else {
            return Err(format!("{name} could not be sent: OneDrive did not say where it put it."));
        };
        let (permission_scope, service_name) = if is_channel {
            ("channel", "sharepoint")
        } else {
            let emails = self.chat_recipients(thread)?;
            if !emails.is_empty() {
                let recipients: Vec<Value> = emails.iter().map(|email| json!({"email": email})).collect();
                let invite_url = format!("{}/v1.0/me/drive/items/{}/invite", self.graph, escaped(id));
                let body = json!({
                    "recipients": recipients,
                    "requireSignIn": true,
                    "sendInvitation": false,
                    "roles": ["read"],
                });
                if let Err(fault) = self.call("POST", &invite_url, Auth::Bearer(GRAPH), Body::Json(&body), true) {
                    return match fault {
                        Fault::Refused { status: 400 | 403, message } => {
                            let detail = if message.is_empty() { String::new() } else { format!(" ({message})") };
                            Err(format!(
                                "{name} could not be shared with external or guest chat members — organization sharing policy refused it{detail}."
                            ))
                        }
                        other => Err(failed(other)),
                    };
                }
            }
            ("users", "p2p")
        };
        let share = web.to_owned();
        let kind = name.rsplit_once('.').map_or("", |(_, ext)| ext).to_ascii_lowercase();
        let site = web
            .split("/Shared Documents/")
            .next()
            .and_then(|s| s.split("/Documents/").next())
            .unwrap_or(web);
        let entry = json!({
            "@type": "http://schema.skype.com/File", "version": 2, "id": id, "baseUrl": format!("{site}/"),
            "type": kind, "title": name, "state": "active", "objectUrl": web, "providerData": "", "itemid": id,
            "fileName": name, "fileType": kind, "fileSize": bytes,
            "fileInfo": {"itemId": id, "fileUrl": web, "siteUrl": format!("{site}/"), "serverRelativeUrl": "", "shareUrl": share, "shareId": null},
            "botFileProperties": {}, "permissionScope": permission_scope, "filePreview": {},
            "fileChicletState": {"serviceName": service_name, "state": "active"},
        });
        let media_kind = super::message::shared_kind(&name);
        let file = MediaFile { bytes, ..MediaFile::new(format!("file:sent:{}", escaped(id)), media_kind) };
        if let Err(err) = self.ctx.media(&[(file.clone(), super::graph_content(&self.graph, &share))]) {
            tracing::warn!("chatd: Teams: {err}");
        }
        Ok((entry, Attachment::new(name, file)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{iso_utc, seconds};

    #[test]
    fn iso_utc_round_trips_with_seconds() {
        let ts = 1_800_000_000;
        let formatted = iso_utc(ts);
        assert_eq!(formatted, "2027-01-15T08:00:00.000Z");
        assert_eq!(seconds(&formatted), ts);
    }
}
