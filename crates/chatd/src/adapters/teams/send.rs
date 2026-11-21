//! Files in a Teams chat: each is uploaded to the user's own OneDrive, into "Microsoft Teams
//! Chat Files" as Teams itself does, shared with the organization, and named by the message in
//! `properties.files`. OneDrive answers to a token for Microsoft Graph, the second audience the
//! sign-in's refresh token mints (ruled in: Teams cannot send files without it).
//!
//! Unverified, in the order most likely to be wrong:
//! - The shape of `properties.files` Teams wants (written here as the web client's messages
//!   read back carry it), and that the chat service keeps a message that names it at all:
//!   Teams answers 201 and drops a message with fields it does not expect.
//! - That Teams' own client id may mint a Graph token from the same refresh token.
//! - Sharing: Teams grants the chat's members access one by one; an organization link is
//!   asked for here instead, which a tenant may forbid.

use super::{escaped, Audience, Auth, Body, Fault, Teams};
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
        if with.voice.is_some() && with.files.len() == 1 {
            upload::spawn(&self.ctx, conversation, client_id, &with.files, move |up| {
                teams.send_voice(&conversation_id, &client, reply.as_deref(), &outgoing, up)
            });
            return;
        }
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
            let (entry, attachment) = self.one_file(up, at)?;
            named.push(entry);
            attachments.push(attachment);
        }
        up.check()?;
        let properties = json!({"files": Value::Array(named).to_string()});
        let mut message = self.posted_with(conversation, client_id, text, reply_to, with, Some(properties)).map_err(super::reason)?;
        message.attachments = attachments;
        Ok(message)
    }

    /// Uploads a recorded voice message to Teams' AMS media store and posts a
    /// `RichText/Media_AudioMsg` referencing it.
    fn send_voice(&self, conversation: &str, client_id: &str, reply_to: Option<&str>, with: &Outgoing, up: &mut Upload) -> Result<Message, String> {
        let (name, bytes, mime) = {
            let file = &up.files()[0];
            (file.name.clone(), file.bytes, file.mime)
        };
        if bytes == 0 {
            return Err(format!("{name} is empty."));
        }
        let (skype_token, place) = self.skype().map_err(super::reason)?;
        let Some(ams) = place.ams.as_deref() else {
            return Err("Teams did not advertise a media store for voice messages.".to_owned());
        };
        let thread = super::thread_of(conversation);
        let failed = |fault: Fault| format!("{name} could not be sent: {}", super::reason(fault));
        let create_url = format!("{ams}/v1/objects");
        let create_body = json!({
            "type": "pish/audio",
            "permissions": { thread: ["read"] },
            "filename": name,
        });
        let created = self
            .call("POST", &create_url, Auth::Ams, Body::Json(&create_body), false)
            .map_err(failed)?;
        let Some(object_id) = created.body["id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned) else {
            return Err(format!("{name} could not be sent: Teams did not name the uploaded voice message."));
        };
        up.check()?;
        let upload_url = format!("{ams}/v1/objects/{}/content/audio", escaped(&object_id));
        let target = Target::put(upload_url)
            .header("Authorization", &format!("skype_token {skype_token}"))
            .header("Content-Type", mime);
        up.send(0, &target)?;
        up.check()?;
        let voice = with.voice.as_ref();
        let duration_secs = voice.map_or(0, |v| (u64::from(v.duration) + 500) / 1000) as u32;
        let waveform = voice.map_or_else(Vec::new, |v| v.waveform.clone());
        let object_uri = format!("{ams}/v1/objects/{}", escaped(&object_id));
        let content = format!(
            "<URIObject uri=\"{uri}\" type=\"Audio.1\" doc_id=\"{doc}\">\
             <Title></Title><Description></Description>\
             <OriginalName v=\"{orig}\"/><FileSize v=\"{bytes}\"/><Duration v=\"{dur}\"/>\
             </URIObject>",
            uri = xml_attr(&object_uri),
            doc = xml_attr(&object_id),
            orig = xml_attr(&name),
            bytes = bytes,
            dur = format_hms(duration_secs),
        );
        let mut props = json!({ "amsreferences": [object_id] });
        if !waveform.is_empty() {
            props["waveform"] = json!(waveform);
        }
        let file = MediaFile {
            bytes,
            ..MediaFile::new(format!("voice:sent:{}", escaped(&object_id)), MediaKind::Audio)
        };
        let fetch_url = format!("{object_uri}/views/audio");
        if let Err(err) = self.ctx.media(&[(file.clone(), fetch_url)]) {
            tracing::warn!("chatd: Teams: {err}");
        }
        let attachment = Attachment {
            mime: mime.to_owned(),
            duration: duration_secs,
            waveform,
            ..Attachment::new(name, file)
        };
        self.post_raw_message(
            conversation,
            client_id,
            "RichText/Media_AudioMsg",
            &content,
            reply_to,
            with,
            Some(props),
            vec![attachment],
        )
        .map_err(super::reason)
    }

    /// One file to OneDrive, in an upload session, and the entry the message names it by.
    fn one_file(&self, up: &mut Upload, at: usize) -> Result<(Value, Attachment), String> {
        let (name, bytes) = (up.files()[at].name.clone(), up.files()[at].bytes);
        let failed = |fault: Fault| format!("{name} could not be sent: {}", super::reason(fault));
        if bytes == 0 {
            return Err(format!("{name} is empty."));
        }
        let path = format!("{}/v1.0/me/drive/root:/{}/{}:/createUploadSession", self.graph, escaped(FOLDER), escaped(&name));
        let session = self.call("POST", &path, Auth::Bearer(Audience::Graph), Body::Json(&json!({"item": {"@microsoft.graph.conflictBehavior": "rename"}})), true).map_err(failed)?;
        let Some(url) = session.body["uploadUrl"].as_str().map(str::to_owned) else {
            return Err(format!("{name} could not be sent: OneDrive gave no place to put it."));
        };
        // The session's address is signed: it takes no token, and goes nowhere but OneDrive.
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
        let shared = format!("{}/v1.0/me/drive/items/{}/createLink", self.graph, escaped(id));
        let link = self.call("POST", &shared, Auth::Bearer(Audience::Graph), Body::Json(&json!({"type": "view", "scope": "organization"})), true).map_err(failed)?;
        let share = link.body["link"]["webUrl"].as_str().unwrap_or(web).to_owned();
        let kind = name.rsplit_once('.').map_or("", |(_, ext)| ext).to_ascii_lowercase();
        let site = web.split("/Documents/").next().unwrap_or(web);
        let entry = json!({
            "@type": "http://schema.skype.com/File", "version": 2, "id": id, "baseUrl": format!("{site}/"),
            "type": kind, "title": name, "state": "active", "objectUrl": web, "providerData": "", "itemid": id,
            "fileName": name, "fileType": kind,
            "fileInfo": {"itemId": null, "fileUrl": web, "siteUrl": format!("{site}/"), "serverRelativeUrl": "", "shareUrl": share, "shareId": null},
            "botFileProperties": {}, "permissionScope": "users", "filePreview": {},
            "fileChicletState": {"serviceName": "p2p", "state": "active"},
        });
        let file = MediaFile { bytes, ..MediaFile::new(format!("file:sent:{}", escaped(id)), MediaKind::File) };
        if let Err(err) = self.ctx.media(&[(file.clone(), share)]) {
            tracing::warn!("chatd: Teams: {err}");
        }
        Ok((entry, Attachment::new(name, file)))
    }
}

fn xml_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn format_hms(secs: u32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::super::{iso_utc, seconds};
    use super::{format_hms, xml_attr};

    #[test]
    fn iso_utc_round_trips_with_seconds() {
        let ts = 1_800_000_000;
        let formatted = iso_utc(ts);
        assert_eq!(formatted, "2027-01-15T08:00:00.000Z");
        assert_eq!(seconds(&formatted), ts);
    }

    #[test]
    fn voice_uri_helpers_escape_and_format_duration() {
        assert_eq!(xml_attr("a&b\"<c>"), "a&amp;b&quot;&lt;c&gt;");
        assert_eq!(format_hms(14), "0:00:14");
        assert_eq!(format_hms(3665), "1:01:05");
    }
}
