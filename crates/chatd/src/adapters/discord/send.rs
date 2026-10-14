//! A message with files or options: attachments through Discord's upload addresses, spoilers,
//! silent and unfurl-less sends, the reply ping, a GIF or a sticker from the picker.
//!
//! Files go the way the web client sends them (the community's documentation, unconfirmed
//! against a real account): `POST /channels/{id}/attachments` names each file and its size and
//! is answered with an address per file; each file is `PUT` there straight from its path; the
//! message then names the uploaded files. A cancelled send deletes what it already uploaded.

use super::model::Posted;
use super::{Discord, LONGEST};
use crate::blobs::upload::{Target, Upload};
use protocol::{CatalogKind, Message, Outgoing};
use serde::Deserialize;
use serde_json::{json, Value};

/// Discord's own message flags.
const SUPPRESS_EMBEDS: u64 = 1 << 2;
const SUPPRESS_NOTIFICATIONS: u64 = 1 << 12;
/// Most files one message carries.
const FILES: usize = 10;

#[derive(Deserialize)]
struct Slot {
    upload_url: String,
    upload_filename: String,
}

#[derive(Deserialize)]
struct Slots {
    #[serde(default)]
    attachments: Vec<Slot>,
}

/// The name Discord shows: a spoiler is a file whose name says so.
fn shown_name(name: &str, spoiler: bool) -> String {
    match spoiler && !name.starts_with("SPOILER_") {
        true => format!("SPOILER_{name}"),
        false => name.to_owned(),
    }
}

impl Discord {
    /// The work of one `SendWith`, run by the upload helper on a thread of its own, which reads
    /// the files and ends the send through `Context::sent`.
    pub(super) fn send_with(&self, channel: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &Outgoing, up: &mut Upload) -> Result<Message, String> {
        let premium = self.state.lock().unwrap().premium;
        let longest = if premium == 2 { 2 * LONGEST } else { LONGEST };
        if text.chars().count() > longest {
            return Err(format!("Message too long: Discord allows {longest} characters."));
        }
        if with.schedule.is_some() {
            return Err("Discord cannot schedule a message.".to_owned());
        }
        let conversation = self.ctx.known(channel).map_err(|err| err.to_string())?;
        let conversation = conversation.ok_or_else(|| "This conversation is gone.".to_owned())?;
        let guild = self.guild_of(&conversation);
        let most: u64 = match premium {
            2 => 500 << 20,
            1 | 3 => 50 << 20,
            _ => 10 << 20,
        };
        if up.files().len() > FILES {
            return Err(format!("Discord takes {FILES} files in one message."));
        }
        if let Some(big) = up.files().iter().find(|file| file.bytes > most) {
            return Err(format!("{} is too big for Discord (limit {} MB).", big.name, most >> 20));
        }

        let mut content = text.to_owned();
        let mut stickers = Vec::new();
        match &with.pick {
            Some(pick) if pick.kind == CatalogKind::Gifs => {
                content = if content.is_empty() { pick.id.clone() } else { format!("{content}\n{}", pick.id) };
            }
            Some(pick) if pick.kind == CatalogKind::Stickers => stickers.push(pick.id.clone()),
            _ => {}
        }
        let attachments = self.upload(channel, up)?;
        up.check()?;

        let mut flags = 0;
        if with.silent {
            flags |= SUPPRESS_NOTIFICATIONS;
        }
        if with.suppress_embeds {
            flags |= SUPPRESS_EMBEDS;
        }
        let mut body = json!({
            "content": content,
            "nonce": client_id,
            "tts": false,
            "flags": flags,
            "mobile_network_type": "unknown",
        });
        if !attachments.is_empty() {
            body["attachments"] = json!(attachments);
        }
        if !stickers.is_empty() {
            body["sticker_ids"] = json!(stickers);
        }
        if let Some(reply_to) = reply_to {
            let mut reference = json!({"channel_id": channel, "message_id": reply_to});
            if let Some(guild) = &guild {
                reference["guild_id"] = json!(guild);
            }
            body["message_reference"] = reference;
            // The ping toggle: the author of the original is notified only when it is on.
            body["allowed_mentions"] = json!({"parse": ["users", "roles", "everyone"], "replied_user": with.ping});
        }
        let accepted = self.call("POST", &format!("/channels/{channel}/messages"), Some(body)).map_err(|refused| refused.reason())?;
        let posted: Posted = serde_json::from_str(&accepted).map_err(|err| format!("Discord's answer was unreadable: {err}"))?;
        let (message, files) = self.convert(&posted, guild.as_deref(), conversation.kind);
        let _ = files.register(&self.ctx);
        Ok(message)
    }

    /// Uploads each file to the address Discord hands out for it and answers what the message
    /// names them by. Nothing is asked when there are no files. What already went up is taken
    /// back when a later file fails or the send is cancelled, as the web client does.
    fn upload(&self, channel: &str, up: &mut Upload) -> Result<Vec<Value>, String> {
        if up.files().is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<String> = up.files().iter().map(|file| shown_name(&file.name, file.spoiler)).collect();
        let named: Vec<Value> = up
            .files()
            .iter()
            .zip(&names)
            .enumerate()
            .map(|(at, (file, name))| json!({"id": at.to_string(), "filename": name, "file_size": file.bytes, "is_clip": false}))
            .collect();
        let asked = self.call("POST", &format!("/channels/{channel}/attachments"), Some(json!({"files": named})));
        let slots: Slots = serde_json::from_str(&asked.map_err(|refused| refused.reason())?).map_err(|err| format!("Discord's answer was unreadable: {err}"))?;
        if slots.attachments.len() != names.len() {
            return Err("Discord did not take every file.".to_owned());
        }
        let agent = self.rest.identity().user_agent.clone();
        let mut attachments = Vec::with_capacity(names.len());
        for (at, (slot, name)) in slots.attachments.iter().zip(&names).enumerate() {
            // The address is the storage bucket's own: the session's headers do not go there.
            let target = Target::put(&slot.upload_url).header("Content-Type", "application/octet-stream").header("User-Agent", &agent);
            if let Err(why) = up.send(at, &target) {
                for gone in &slots.attachments[..at] {
                    let _ = self.call("DELETE", &format!("/attachments/{}", gone.upload_filename), None);
                }
                return Err(why);
            }
            attachments.push(json!({"id": at.to_string(), "filename": name, "uploaded_filename": slot.upload_filename}));
        }
        Ok(attachments)
    }
}
