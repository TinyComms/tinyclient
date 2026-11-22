//! Files in Teams:
//! - In chats: uploaded to the user's own OneDrive under `"Microsoft Teams Chat Files"` via a
//!   chunked Graph upload session, then shared strictly with the chat's human members via
//!   `POST /v1.0/me/drive/items/{id}/invite` (never an organization-wide `createLink`).
//! - In channels (standard, private, or shared): uploaded directly into the channel's SharePoint
//!   folder resolved via `GET /v1.0/teams/{groupId}/channels/{channelId}/filesFolder`, where
//!   channel members already have access.
//! - When every attachment is an image, the bytes go to Teams' AMS media store instead,
//!   scoped to the conversation, and the message embeds them inline.
//! - The sent message names each uploaded file in `properties.files`.

use super::{escaped, Auth, Body, Fault, Teams, GRAPH};
use crate::blobs::upload::{self, Target, Upload};
use protocol::{Attachment, MediaFile, MediaKind, Message, Outgoing};
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
        if let Some(ref draft) = with.poll {
            let outcome = self.send_poll(conversation, client_id, draft, reply_to, &with);
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

    /// Creates and posts a Teams Polls Adaptive Card in `conversation`.
    fn send_poll(
        &self,
        conversation: &str,
        client_id: &str,
        draft: &protocol::PollDraft,
        reply_to: Option<&str>,
        with: &Outgoing,
    ) -> Result<Message, String> {
        let question = draft.question.trim();
        let options: Vec<String> = draft.options.iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
        if question.is_empty() {
            return Err("Poll question cannot be empty.".to_owned());
        }
        if question.chars().count() > 200 {
            return Err("Poll question must be 200 characters or fewer.".to_owned());
        }
        if options.len() < 2 || options.len() > 10 {
            return Err("Teams polls require between 2 and 10 options.".to_owned());
        }
        if options.iter().any(|opt| opt.chars().count() > 100) {
            return Err("Each poll option must be 100 characters or fewer.".to_owned());
        }
        let cleaned = protocol::PollDraft {
            question: question.to_owned(),
            options: options.clone(),
            multiple: draft.multiple,
            duration_secs: draft.duration_secs,
            anonymous: draft.anonymous,
        };
        let (card_attachment, invoke) = super::card::build_poll_card(&cleaned, client_id);
        let props = json!({"cards": Value::Array(vec![card_attachment]).to_string()});
        let mut message = self
            .posted_with(conversation, client_id, question, reply_to, with, Some(props))
            .map_err(super::reason)?;
        {
            let mut state = self.state();
            if state.polls.len() >= super::REMEMBERED {
                state.polls.clear();
            }
            state.polls.insert((conversation.to_owned(), message.id.clone()), invoke);
        }
        let mut poll = protocol::Poll::open(question, options, draft.multiple);
        poll.anonymous = draft.anonymous;
        message.poll = Some(Box::new(poll));
        Ok(message)
    }

    pub(super) fn cancel_send(&self, client_id: &str) {
        upload::cancel(&self.ctx.account, client_id);
    }

    fn with_files(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &Outgoing, up: &mut Upload) -> Result<Message, String> {
        if up.files().iter().all(|f| f.mime.starts_with("image/")) {
            return self.with_inline_images(conversation, client_id, text, reply_to, with, up);
        }
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

    /// Uploads one or more images to Teams' AMS media store and sends them inline in `RichText/Html`.
    fn with_inline_images(
        &self,
        conversation: &str,
        client_id: &str,
        text: &str,
        reply_to: Option<&str>,
        with: &Outgoing,
        up: &mut Upload,
    ) -> Result<Message, String> {
        let (skype, place) = self.skype().map_err(super::reason)?;
        let Some(ams) = place.ams.as_deref() else {
            return Err("Teams did not provide a media store address for images.".to_owned());
        };
        let thread = super::thread_of(conversation);
        let mut images_html = String::new();
        let mut attachments = Vec::new();
        for at in 0..up.files().len() {
            let (img_html, attachment) = self.one_ams_image(up, at, ams, &skype, thread)?;
            images_html.push_str(&img_html);
            attachments.push(attachment);
        }
        up.check()?;
        let mut message = self
            .posted_with_html(conversation, client_id, text, reply_to, with, None, Some(&images_html))
            .map_err(super::reason)?;
        message.attachments = attachments;
        if !images_html.is_empty() {
            let mut state = self.state();
            if state.own_meta.len() >= super::REMEMBERED {
                state.own_meta.clear();
            }
            state.own_meta.insert(
                (conversation.to_owned(), message.id.clone()),
                super::message::OwnMeta { subject: None, images_html },
            );
        }
        Ok(message)
    }

    /// Creates one AMS object, uploads the image bytes to `/content/imgpsh`, and returns its
    /// inline `<img itemtype="http://schema.skype.com/AMSImage">` HTML plus the local attachment.
    fn one_ams_image(&self, up: &mut Upload, at: usize, ams: &str, skype: &str, thread: &str) -> Result<(String, Attachment), String> {
        let (name, bytes, mime, path, media_dims) = {
            let src = &up.files()[at];
            let dims = src.media.as_ref().map(|m| (m.width.unwrap_or(0), m.height.unwrap_or(0))).unwrap_or((0, 0));
            (src.name.clone(), src.bytes, src.mime, src.path.clone(), dims)
        };
        let failed = |fault: Fault| format!("{name} could not be sent: {}", super::reason(fault));
        if bytes == 0 {
            return Err(format!("{name} is empty."));
        }
        let (width, height) = if media_dims.0 > 0 && media_dims.1 > 0 {
            media_dims
        } else {
            image_dimensions(&path).unwrap_or((0, 0))
        };
        let create_url = format!("{ams}/v1/objects");
        let create_body = json!({
            "type": "pish/image",
            "permissions": { thread: ["read"] },
            "filename": name,
        });
        let created = self
            .call("POST", &create_url, Auth::Ams, Body::Json(&create_body), true)
            .map_err(failed)?;
        let Some(id) = created.body["id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned) else {
            return Err(format!("{name} could not be sent: Teams media store returned no object id."));
        };
        let upload_url = format!("{ams}/v1/objects/{}/content/imgpsh", escaped(&id));
        let target = Target::put(upload_url)
            .header("Authorization", &format!("skype_token {skype}"))
            .header("Content-Type", mime);
        up.send(at, &target)?;

        let view_url = format!("{ams}/v1/objects/{}/views/imgo", escaped(&id));
        let escaped_alt = super::html::escape_plain(&name);
        let dims_attr = if width > 0 && height > 0 {
            format!(" width=\"{width}\" height=\"{height}\"")
        } else {
            String::new()
        };
        let img_html = format!(
            "<p><img itemscope=\"\" itemtype=\"http://schema.skype.com/AMSImage\" src=\"{view_url}\" alt=\"{escaped_alt}\"{dims_attr}></p>"
        );
        let file = MediaFile {
            width,
            height,
            bytes,
            ..MediaFile::new(format!("img:{}", escaped(&id)), MediaKind::Image)
        };
        if let Err(err) = self.ctx.media(&[(file.clone(), view_url)]) {
            tracing::warn!("chatd: Teams: {err}");
        }
        let attachment = Attachment {
            thumb: Some(file.clone()),
            mime: mime.to_owned(),
            ..Attachment::new(name, file)
        };
        Ok((img_html, attachment))
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
        let (raw_name, bytes) = (up.files()[at].name.clone(), up.files()[at].bytes);
        let name = sanitize_filename(&raw_name);
        let failed = |fault: Fault| format!("{name} could not be sent: {}", super::reason(fault));
        if bytes == 0 {
            return Err(format!("{name} is empty."));
        }
        let thread = super::thread_of(conversation);
        let is_channel = super::tree::is_channel(thread);
        // Resolve chat recipients BEFORE uploading to OneDrive so a missing recipient never leaves
        // an orphaned file behind.
        let chat_emails = if is_channel {
            Vec::new()
        } else {
            self.chat_recipients(thread)?
        };
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
            if !chat_emails.is_empty() {
                let recipients: Vec<Value> = chat_emails.iter().map(|email| json!({"email": email})).collect();
                let invite_url = format!("{}/v1.0/me/drive/items/{}/invite", self.graph, escaped(id));
                let body = json!({
                    "recipients": recipients,
                    "requireSignIn": true,
                    "sendInvitation": false,
                    "roles": ["read"],
                });
                if let Err(fault) = self.call("POST", &invite_url, Auth::Bearer(GRAPH), Body::Json(&body), true) {
                    // Best-effort cleanup so an unshared upload is not left orphaned in OneDrive.
                    let item_url = format!("{}/v1.0/me/drive/items/{}", self.graph, escaped(id));
                    if let Err(del_fault) = self.call("DELETE", &item_url, Auth::Bearer(GRAPH), Body::None, false) {
                        tracing::warn!("chatd: Teams: failed to clean up unshared OneDrive file: {}", super::reason(del_fault));
                    }
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

/// Sanitizes an attachment filename for SharePoint / OneDrive upload session paths:
/// strips any directory prefix, replaces SharePoint-disallowed and control characters with `_`,
/// trims leading/trailing spaces and dots, and enforces a safe length limit.
pub(super) fn sanitize_filename(raw: &str) -> String {
    let leaf = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '"' | '*' | ':' | '<' | '>' | '?' | '/' | '\\' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed: String = cleaned.trim_matches(|c: char| c == ' ' || c == '.').chars().take(200).collect();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "file".to_owned()
    } else {
        trimmed
    }
}

/// Reads pixel dimensions from a PNG, GIF, or JPEG header without decoding the image.
fn image_dimensions(path: &std::path::Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 32];
    let n = file.read(&mut head).ok()?;
    // PNG: 8-byte signature + 4-byte length + "IHDR" + 4-byte width + 4-byte height.
    if n >= 24 && &head[0..8] == b"\x89PNG\r\n\x1a\n" && &head[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
        let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
        return (w > 0 && h > 0).then_some((w, h));
    }
    // GIF: "GIF87a" / "GIF89a" + little-endian u16 width + u16 height.
    if n >= 10 && (&head[0..6] == b"GIF87a" || &head[0..6] == b"GIF89a") {
        let w = u32::from(u16::from_le_bytes([head[6], head[7]]));
        let h = u32::from(u16::from_le_bytes([head[8], head[9]]));
        return (w > 0 && h > 0).then_some((w, h));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::{iso_utc, seconds};
    use super::sanitize_filename;

    #[test]
    fn sanitize_filename_strips_paths_and_sharepoint_illegal_chars() {
        assert_eq!(sanitize_filename("report.txt"), "report.txt");
        assert_eq!(sanitize_filename("../../secret:file?.pdf"), "secret_file_.pdf");
        assert_eq!(sanitize_filename("C:\\Users\\a\\b*c|d.png"), "b_c_d.png");
        assert_eq!(sanitize_filename("  ...  "), "file");
        assert_eq!(sanitize_filename(".."), "file");
        assert_eq!(sanitize_filename(""), "file");
    }

    #[test]
    fn iso_utc_round_trips_with_seconds() {
        let ts = 1_800_000_000;
        let formatted = iso_utc(ts);
        assert_eq!(formatted, "2027-01-15T08:00:00.000Z");
        assert_eq!(seconds(&formatted), ts);
    }
}
