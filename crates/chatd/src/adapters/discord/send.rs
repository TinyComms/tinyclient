//! A message with files or options: attachments through Discord's upload addresses, spoilers,
//! silent and unfurl-less sends, the reply ping, a GIF or a sticker from the picker.
//!
//! Files go the way the web client sends them (the community's documentation, unconfirmed
//! against a real account): `POST /channels/{id}/attachments` names each file and its size and
//! is answered with an address per file; each file is `PUT` there straight from its path; the
//! message then names the uploaded files. A cancelled send deletes what it already uploaded.

use super::model::{self, Posted};
use super::Discord;
use crate::adapters::Refusal;
use crate::blobs::upload::{Target, Upload};
use base64::Engine;
use protocol::{CatalogKind, ConversationKind, Message, Outgoing, VoiceNote};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Discord's own message flags.
const SUPPRESS_EMBEDS: u64 = 1 << 2;
const SUPPRESS_NOTIFICATIONS: u64 = 1 << 12;
const IS_VOICE_MESSAGE: u64 = 1 << 13;
/// Most files one message carries.
const FILES: usize = 10;
/// Filename Discord's clients use for Ogg/Opus voice messages.
const VOICE_FILENAME: &str = "voice-message.ogg";

/// Encodes a voice message's loudness bars (capped at `protocol::WAVEFORM_LIMIT` = 256 bytes)
/// as base64, matching the format `content::waveform()` decodes.
pub(super) fn encode_waveform(bars: &[u8]) -> String {
    let capped = &bars[..bars.len().min(protocol::WAVEFORM_LIMIT)];
    let slice = if capped.is_empty() { &[0u8][..] } else { capped };
    base64::engine::general_purpose::STANDARD.encode(slice)
}

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

/// A custom emoji this account cannot send, rewritten to Discord's public CDN. The address is
/// the public one, not `TINYCLIENT_DISCORD_CDN`, so a lab fake cannot change what a real
/// message would carry. Animated emoji are always a GIF. A plain emoji the account may use
/// stays as `<name:id>`.
pub(super) fn rewrite_locked_emoji(content: &str, allowed: &HashSet<String>) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let token = &rest[start..];
        let animated = token.starts_with("<a:");
        let plain = token.starts_with("<:");
        let Some(end) = token.find('>') else {
            out.push_str(token);
            break;
        };
        if !animated && !plain {
            out.push('<');
            rest = &token[1..];
            continue;
        }
        let inner = &token[if animated { 3 } else { 2 }..end];
        let Some((name, id)) = inner.rsplit_once(':') else {
            out.push_str(&token[..=end]);
            rest = &token[end + 1..];
            continue;
        };
        let snowflake = !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit()) && !name.is_empty();
        if !snowflake {
            out.push_str(&token[..=end]);
        } else if animated {
            out.push_str("https://cdn.discordapp.com/emojis/");
            out.push_str(id);
            out.push_str(".gif");
        } else if allowed.contains(id) {
            out.push_str(&token[..=end]);
        } else {
            out.push_str("https://cdn.discordapp.com/emojis/");
            out.push_str(id);
            out.push_str(".webp");
        }
        rest = &token[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A sticker's public picture. Format 4 is a GIF, 3 is Lottie JSON, and the rest are PNG.
pub(super) fn sticker_link(id: &str, format: u8) -> String {
    let ext = match format {
        4 => "gif",
        3 => "json",
        _ => "png",
    };
    format!("https://media.discordapp.net/stickers/{id}.{ext}")
}

/// The name Discord shows: a spoiler is a file whose name says so.
fn shown_name(name: &str, spoiler: bool) -> String {
    match spoiler && !name.starts_with("SPOILER_") {
        true => format!("SPOILER_{name}"),
        false => name.to_owned(),
    }
}

/// Upload size limits (checked 2026-11):
/// - Free plan: 10 MiB; Nitro Basic / Classic (1, 3): 50 MiB; Nitro (2): 500 MiB.
/// - Server boost tiers: tier 0/1 = 10 MiB, tier 2 = 50 MiB, tier 3 = 100 MiB.
/// Effective limit is max(plan, server tier).
pub(super) fn plan_upload_limit(premium: u8) -> u64 {
    match premium {
        2 => 500 << 20,
        1 | 3 => 50 << 20,
        _ => 10 << 20,
    }
}

pub(super) fn tier_upload_limit(tier: u8) -> u64 {
    match tier {
        3 => 100 << 20,
        2 => 50 << 20,
        _ => 10 << 20,
    }
}

pub(super) fn upload_limit(premium: u8, tier: u8) -> u64 {
    plan_upload_limit(premium).max(tier_upload_limit(tier))
}

impl Discord {
    /// Whether this account has Discord's `SEND_VOICE_MESSAGES` permission in `channel`.
    /// Direct and group messages always allow voice messages; for a server channel (or thread
    /// under one), check the kept channel's computed permissions.
    fn can_send_voice_messages(&self, channel: &str, guild: Option<&str>, parent: Option<&str>) -> bool {
        let Some(guild) = guild else {
            return true;
        };
        let me = self.me();
        let kept: Vec<model::Kept> = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|kept| serde_json::from_str(&kept).ok())
            .unwrap_or_default();
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild) else {
            return true;
        };
        let wanted = |id: &str| id == channel || parent == Some(id);
        let Some(found) = kept.into_iter().filter(|kept| wanted(&kept.id)).min_by_key(|kept| kept.id != channel) else {
            return true;
        };
        info.permissions(guild, &me, &found.channel()) & model::SEND_VOICE_MESSAGES != 0
    }

    /// Custom emoji this channel's server may send without Nitro: its own, not animated, and
    /// not locked to a role we lack.
    pub(super) fn sendable_emoji(&self, guild: Option<&str>) -> HashSet<String> {
        let mut allowed = HashSet::new();
        let Some(guild) = guild else {
            return allowed;
        };
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild) else {
            return allowed;
        };
        for emoji in &info.emojis {
            if emoji.animated {
                continue;
            }
            let roles_ok = emoji.roles.is_empty() || emoji.roles.iter().any(|role| info.my_roles.contains(role));
            if roles_ok {
                allowed.insert(emoji.id.clone());
            }
        }
        allowed
    }

    /// Nitro may send any sticker id and Discord decides. Without it, only a sticker of the
    /// server the message is said in.
    fn sticker_sendable(&self, id: &str, nitro: bool, guild: Option<&str>) -> bool {
        if nitro {
            return true;
        }
        let state = crate::hub::lock(&self.state);
        guild.is_some_and(|guild| {
            state.guilds.get(guild).is_some_and(|info| info.stickers.iter().any(|sticker| sticker.id == id))
        })
    }

    fn sticker_format(&self, id: &str) -> u8 {
        let state = crate::hub::lock(&self.state);
        for info in state.guilds.values() {
            if let Some(sticker) = info.stickers.iter().find(|sticker| sticker.id == id) {
                return sticker.format;
            }
        }
        state.standard_stickers.iter().find(|sticker| sticker.id == id).map(|sticker| sticker.format).unwrap_or(1)
    }

    /// The work of one `SendWith`, run by the upload helper on a thread of its own, which reads
    /// the files and ends the send through `Context::sent`.
    pub(super) fn send_with(&self, channel: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: &Outgoing, up: &mut Upload) -> Result<Message, String> {
        let premium = self.state.lock().unwrap().premium;
        if let Some(why) = super::too_long(premium, text) {
            return Err(why);
        }
        if with.schedule.is_some() {
            return Err("Discord cannot schedule a message.".to_owned());
        }
        let conversation = self.ctx.known(channel).map_err(|err| err.to_string())?;
        let conversation = conversation.ok_or_else(|| "This conversation is gone.".to_owned())?;
        let guild = self.guild_of(&conversation);
        let tier = guild
            .as_deref()
            .and_then(|guild| self.state.lock().unwrap().guilds.get(guild).map(|info| info.premium_tier))
            .unwrap_or(0);
        let most = upload_limit(premium, tier);
        if up.files().len() > FILES {
            return Err(format!("Discord takes {FILES} files in one message."));
        }
        if let Some(big) = up.files().iter().find(|file| file.bytes > most) {
            return Err(format!("{} is too big for Discord (limit {} MB).", big.name, most >> 20));
        }

        if with.voice.is_some() {
            if up.files().len() != 1 {
                return Err("A Discord voice message must carry exactly one audio recording.".to_owned());
            }
            if !self.can_send_voice_messages(channel, guild.as_deref(), conversation.parent.as_deref()) {
                return Err("You don't have permission to send voice messages in this channel.".to_owned());
            }
        }

        let nitro = matches!(premium, 1..=3);
        let mut content = if with.voice.is_some() { String::new() } else { text.to_owned() };
        if with.voice.is_none() && !nitro {
            content = rewrite_locked_emoji(&content, &self.sendable_emoji(guild.as_deref()));
        }
        let mut stickers = Vec::new();
        if with.voice.is_none() {
            match &with.pick {
                Some(pick) if pick.kind == CatalogKind::Gifs => {
                    content = if content.is_empty() { pick.id.clone() } else { format!("{content}\n{}", pick.id) };
                }
                Some(pick) if pick.kind == CatalogKind::Stickers => {
                    if self.sticker_sendable(&pick.id, nitro, guild.as_deref()) {
                        stickers.push(pick.id.clone());
                    } else {
                        let link = sticker_link(&pick.id, self.sticker_format(&pick.id));
                        content = if content.is_empty() { link } else { format!("{content}\n{link}") };
                    }
                }
                _ => {}
            }
        }
        let attachments = self.upload(channel, up, with.voice.as_ref())?;
        up.check()?;

        let mut flags = 0;
        if with.silent {
            flags |= SUPPRESS_NOTIFICATIONS;
        }
        if with.suppress_embeds {
            flags |= SUPPRESS_EMBEDS;
        }
        if with.voice.is_some() {
            flags |= IS_VOICE_MESSAGE;
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
        let asked = self.call("POST", &format!("/channels/{channel}/messages"), Some(body));
        self.note_send_slowmode(channel, &conversation, &asked);
        let accepted = asked.map_err(|refused| refused.reason())?;
        let posted: Posted = serde_json::from_str(&accepted).map_err(|err| format!("Discord's answer was unreadable: {err}"))?;
        let (message, files) = self.convert(&posted, guild.as_deref(), conversation.kind);
        let _ = files.register(&self.ctx);
        Ok(message)
    }

    /// Uploads each file to the address Discord hands out for it and answers what the message
    /// names them by. Nothing is asked when there are no files. What already went up is taken
    /// back when a later file fails or the send is cancelled, as the web client does.
    pub(super) fn upload(&self, channel: &str, up: &mut Upload, voice: Option<&VoiceNote>) -> Result<Vec<Value>, String> {
        if up.files().is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<String> = up
            .files()
            .iter()
            .map(|file| {
                if voice.is_some() {
                    VOICE_FILENAME.to_owned()
                } else {
                    shown_name(&file.name, file.spoiler)
                }
            })
            .collect();
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
        let content_type = if voice.is_some() { "audio/ogg" } else { "application/octet-stream" };
        let mut attachments = Vec::with_capacity(names.len());
        for (at, (slot, name)) in slots.attachments.iter().zip(&names).enumerate() {
            // The address is the storage bucket's own: the session's headers do not go there.
            let target = Target::put(&slot.upload_url).header("Content-Type", content_type).header("User-Agent", &agent);
            if let Err(why) = up.send(at, &target) {
                for gone in &slots.attachments[..at] {
                    let _ = self.call("DELETE", &format!("/attachments/{}", gone.upload_filename), None);
                }
                return Err(why);
            }
            let mut entry = json!({"id": at.to_string(), "filename": name, "uploaded_filename": slot.upload_filename});
            // A description goes on the message only, and only when there is one.
            if let Some(alt) = up.files().get(at).map(|file| file.alt.as_str()).filter(|alt| !alt.is_empty()) {
                entry["description"] = json!(alt);
            }
            if let Some(voice) = voice {
                entry["duration_secs"] = json!(f64::from(voice.duration) / 1000.0);
                entry["waveform"] = json!(encode_waveform(&voice.waveform));
            }
            attachments.push(entry);
        }
        Ok(attachments)
    }

    /// A new post in a forum or media channel. One request, and only when the user asked:
    /// Discord watches thread creation. The body is the web client's nested shape
    /// (`use_nested_fields`). A 429 is not retried, and a captcha does not lock the session.
    pub(super) fn forum_post(&self, forum: &str, title: &str, text: &str, tags: &[String], with: &Outgoing) -> Result<String, Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        if title.trim().is_empty() {
            return Err(refuse("A post needs a title.".to_owned()));
        }
        let name = super::thread_name(title);
        let parent = self.ctx.known(forum).map_err(|err| refuse(err.to_string()))?.ok_or_else(|| refuse("This conversation is gone.".to_owned()))?;
        if parent.kind != ConversationKind::Forum {
            return Err(refuse("This is not a forum.".to_owned()));
        }
        if parent.read_only {
            let why = if parent.read_only_reason.is_empty() {
                format!("You can read {} but not post in it.", parent.title)
            } else {
                parent.read_only_reason.clone()
            };
            return Err(refuse(why));
        }
        let guild = self.guild_of(&parent);
        let kept = guild.as_ref().and_then(|guild| self.ctx.note(&format!("kept:{guild}")));
        let kept: Vec<model::Kept> = kept.and_then(|kept| serde_json::from_str(&kept).ok()).unwrap_or_default();
        let kept = kept.into_iter().find(|kept| kept.id == forum).ok_or_else(|| refuse("This forum is not here.".to_owned()))?;
        if kept.flags & model::REQUIRE_TAG != 0 && tags.is_empty() {
            return Err(refuse("This forum requires a tag.".to_owned()));
        }
        if kept.kind == model::MEDIA_FORUM && with.files.is_empty() {
            return Err(refuse("A media channel needs a file.".to_owned()));
        }
        if !tags.is_empty() {
            let known = !kept.available_tags.is_empty();
            let allowed = |id: &str| kept.available_tags.iter().any(|tag| tag.id == id);
            if !known || tags.iter().any(|id| !allowed(id)) {
                return Err(refuse("That tag is not on this forum.".to_owned()));
            }
        }
        let text = text.trim();
        if text.is_empty() && with.files.is_empty() {
            return Err(refuse("Write a message or attach a file.".to_owned()));
        }
        if with.schedule.is_some() {
            return Err(refuse("Discord cannot schedule a message.".to_owned()));
        }
        let premium = self.state.lock().unwrap().premium;
        if let Some(why) = super::too_long(premium, text) {
            return Err(refuse(why));
        }
        let tier = guild
            .as_deref()
            .and_then(|guild| self.state.lock().unwrap().guilds.get(guild).map(|info| info.premium_tier))
            .unwrap_or(0);
        let most = upload_limit(premium, tier);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let mut upload = if with.files.is_empty() {
            None
        } else {
            let up = Upload::new(&self.ctx, forum, &format!("post-{forum}-{stamp}"), &with.files).map_err(refuse)?;
            if up.files().len() > FILES {
                return Err(refuse(format!("Discord takes {FILES} files in one message.")));
            }
            if let Some(big) = up.files().iter().find(|file| file.bytes > most) {
                return Err(refuse(format!("{} is too big for Discord (limit {} MB).", big.name, most >> 20)));
            }
            Some(up)
        };
        let attachments = match upload.as_mut() {
            Some(up) => self.upload(forum, up, None).map_err(refuse)?,
            None => Vec::new(),
        };
        if let Some(up) = upload.as_ref() {
            up.check().map_err(refuse)?;
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
        let mut flags = 0u64;
        if with.silent {
            flags |= SUPPRESS_NOTIFICATIONS;
        }
        if with.suppress_embeds {
            flags |= SUPPRESS_EMBEDS;
        }
        let mut message = json!({"content": content, "flags": flags});
        if !attachments.is_empty() {
            message["attachments"] = json!(attachments.clone());
        }
        if !stickers.is_empty() {
            message["sticker_ids"] = json!(stickers);
        }
        let mut body = json!({"name": name, "auto_archive_duration": 4320, "message": message});
        if !tags.is_empty() {
            body["applied_tags"] = json!(tags);
        }
        let path = format!("/channels/{forum}/threads?use_nested_fields=true");
        let posted = self.rest.call_once_with("POST", &path, Some(body), &[]);
        if matches!(&posted, Err(super::rest::Refused::Expired)) {
            self.wake();
        }
        let accepted = match posted {
            Ok(text) => text,
            Err(refused) => {
                self.forget_uploaded(&attachments);
                return Err(super::refusal(refused));
            }
        };
        let value: Value = serde_json::from_str(&accepted).map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
        let thread: model::Channel = serde_json::from_value(value.clone()).map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
        if thread.id.is_empty() {
            self.forget_uploaded(&attachments);
            return Err(refuse("Discord did not name the post.".to_owned()));
        }
        let title = thread.name.clone().filter(|title| !title.trim().is_empty()).unwrap_or(name);
        self.record_thread(&parent, &thread, Some(&thread.id), &title)?;
        if let Some(posted) = value.get("message").cloned() {
            if let Ok(mut posted) = serde_json::from_value::<Posted>(posted) {
                if !posted.id.is_empty() {
                    if posted.channel_id.is_empty() {
                        posted.channel_id = thread.id.clone();
                    }
                    let (message, files) = self.convert(&posted, guild.as_deref(), ConversationKind::Thread);
                    let _ = files.register(&self.ctx);
                    self.ctx.history(&thread.id, &[message]).map_err(|err| refuse(err.to_string()))?;
                }
            }
        }
        if let Some(up) = upload {
            up.done();
        }
        Ok(thread.id)
    }

    /// Takes back files already uploaded when the post itself did not go.
    fn forget_uploaded(&self, attachments: &[Value]) {
        for file in attachments {
            let Some(name) = file["uploaded_filename"].as_str() else { continue };
            let _ = self.call("DELETE", &format!("/attachments/{name}"), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_limit_takes_max_of_plan_and_server_boost_tier() {
        assert_eq!(upload_limit(0, 0), 10 << 20);
        assert_eq!(upload_limit(0, 1), 10 << 20);
        assert_eq!(upload_limit(0, 2), 50 << 20);
        assert_eq!(upload_limit(0, 3), 100 << 20);
        assert_eq!(upload_limit(1, 0), 50 << 20);
        assert_eq!(upload_limit(3, 3), 100 << 20);
        assert_eq!(upload_limit(2, 0), 500 << 20);
        assert_eq!(upload_limit(2, 3), 500 << 20);
    }

    #[test]
    fn encode_waveform_caps_at_256_bytes_and_handles_empty() {
        let encoded = encode_waveform(&[12, 128, 255]);
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&encoded).unwrap(), vec![12, 128, 255]);
        let long = vec![42u8; 300];
        let capped = encode_waveform(&long);
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&capped).unwrap().len(), protocol::WAVEFORM_LIMIT);
        let empty = encode_waveform(&[]);
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&empty).unwrap(), vec![0]);
    }

    #[test]
    fn a_locked_emoji_becomes_its_cdn_link() {
        let allowed = HashSet::from(["5101".to_owned()]);
        let rewritten = rewrite_locked_emoji("hi <:ferris:5101> <a:party:5103> <:other:9999>", &allowed);
        assert_eq!(
            rewritten,
            "hi <:ferris:5101> https://cdn.discordapp.com/emojis/5103.gif https://cdn.discordapp.com/emojis/9999.webp"
        );
        assert_eq!(sticker_link("5202", 3), "https://media.discordapp.net/stickers/5202.json");
        assert_eq!(sticker_link("5201", 1), "https://media.discordapp.net/stickers/5201.png");
        assert_eq!(sticker_link("5204", 4), "https://media.discordapp.net/stickers/5204.gif");
    }
}
