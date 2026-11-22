//! Discord server emoji and sticker management (#411).
//!
//! Soundboard sounds are intentionally deferred until calls play soundboard audio.
//! Files are validated locally (path safety via `upload::allowed`, byte size, format, and
//! image dimensions) before any upload is sent to Discord. Uploads use one-shot HTTP calls so
//! Discord's 429 rate-limit wait surfaces directly instead of retrying silently.

use super::super::Refusal;
use super::model::{
    self, number, safe_discord_id, CREATE_GUILD_EXPRESSIONS, MANAGE_GUILD_EXPRESSIONS,
};
use super::{refusal, Discord, Pickable};
use crate::blobs::upload;
use base64::Engine;
use protocol::{MediaFile, SpaceEmoji, SpaceExpressions, SpaceRoleChoice, SpaceSticker};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::io::{Cursor, Read};

const EMOJI_MAX_BYTES: usize = 256 * 1024;
const STICKER_MAX_BYTES: usize = 512 * 1024;
const STICKER_DIM: u32 = 320;

/// Static (and separately animated) custom emoji slot limit by guild `premium_tier`.
pub(super) fn emoji_slot_limit(premium_tier: u8) -> u32 {
    match premium_tier {
        0 => 50,
        1 => 100,
        2 => 150,
        _ => 250,
    }
}

/// Custom sticker slot limit by guild `premium_tier`.
pub(super) fn sticker_slot_limit(premium_tier: u8) -> u32 {
    match premium_tier {
        0 => 5,
        1 => 15,
        2 => 30,
        _ => 60,
    }
}

#[derive(Deserialize, Default)]
struct RawCreator {
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
}

#[derive(Deserialize)]
struct RawEmoji {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    animated: bool,
    #[serde(default = "yes")]
    available: bool,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    user: Option<RawCreator>,
}

#[derive(Deserialize)]
struct RawSticker {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    format_type: u8,
    #[serde(default = "yes")]
    available: bool,
    #[serde(default)]
    user: Option<RawCreator>,
}

fn yes() -> bool {
    true
}

fn creator_name(user: Option<RawCreator>) -> String {
    user.map(|u| u.global_name.filter(|s| !s.is_empty()).unwrap_or(u.username))
        .unwrap_or_default()
}

fn map_emoji(raw: RawEmoji, media: &mut Vec<(MediaFile, String)>) -> SpaceEmoji {
    let pair = model::emoji(&raw.id, raw.animated);
    let image = Some(pair.0.clone());
    media.push(pair);
    SpaceEmoji {
        id: raw.id,
        name: raw.name,
        animated: raw.animated,
        available: raw.available,
        roles: raw.roles,
        image,
        creator: creator_name(raw.user),
    }
}

fn map_sticker(raw: RawSticker, media: &mut Vec<(MediaFile, String)>) -> SpaceSticker {
    let image = model::sticker(&raw.id, raw.format_type).map(|pair| {
        let file = pair.0.clone();
        media.push(pair);
        file
    });
    SpaceSticker {
        id: raw.id,
        name: raw.name,
        description: raw.description.unwrap_or_default(),
        tags: raw.tags.unwrap_or_default(),
        format: raw.format_type,
        available: raw.available,
        image,
        creator: creator_name(raw.user),
    }
}

pub(super) fn validate_emoji_name(raw: &str) -> Result<String, Refusal> {
    let name = raw.trim();
    let len = name.len();
    if !(2..=32).contains(&len) || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(Refusal::Refused(
            "Emoji name must be 2 to 32 letters, numbers, or underscores.".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

fn one_line(raw: &str) -> bool {
    !raw.chars().any(|c| c == '\n' || c == '\r' || c == '\0')
}

// A line break anywhere in what was typed is refused, the ends included: these go into multipart fields.
pub(super) fn validate_sticker_name(raw: &str) -> Result<String, Refusal> {
    let name = raw.trim();
    let len = name.chars().count();
    if !(2..=30).contains(&len) || !one_line(raw) {
        return Err(Refusal::Refused("Sticker name must be between 2 and 30 characters.".to_owned()));
    }
    Ok(name.to_owned())
}

pub(super) fn validate_sticker_tags(raw: &str) -> Result<String, Refusal> {
    let tags = raw.trim();
    let len = tags.chars().count();
    if len == 0 || len > 200 || !one_line(raw) {
        return Err(Refusal::Refused("Sticker needs a related emoji or tag (1 to 200 characters).".to_owned()));
    }
    Ok(tags.to_owned())
}

pub(super) fn validate_sticker_description(raw: &str) -> Result<String, Refusal> {
    let desc = raw.trim();
    if desc.chars().count() > 100 || !one_line(raw) {
        return Err(Refusal::Refused("Sticker description must be 100 characters or fewer.".to_owned()));
    }
    Ok(desc.to_owned())
}

fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32), Refusal> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| Refusal::Refused("That image could not be read.".to_owned()))?;
    reader
        .into_dimensions()
        .map_err(|_| Refusal::Refused("That image appears damaged or unsupported.".to_owned()))
}

/// Validates emoji bytes (size <= 256 KB, PNG/JPEG/GIF/WebP, readable image dimensions)
/// and returns its `data:<mime>;base64,...` URI.
pub(super) fn validate_emoji_bytes(bytes: &[u8]) -> Result<String, Refusal> {
    if bytes.is_empty() {
        return Err(Refusal::Refused("That emoji file is empty.".to_owned()));
    }
    if bytes.len() > EMOJI_MAX_BYTES {
        return Err(Refusal::Refused("Emoji files must be 256 KB or smaller.".to_owned()));
    }
    let Some(mime) = sniff_image_mime(bytes) else {
        return Err(Refusal::Refused("Emoji must be a PNG, JPEG, GIF, or WebP image.".to_owned()));
    };
    let (w, h) = image_dimensions(bytes)?;
    if w == 0 || h == 0 {
        return Err(Refusal::Refused("Emoji image has invalid dimensions.".to_owned()));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StickerFileKind {
    Png,
    Lottie,
}

impl StickerFileKind {
    fn mime(self) -> &'static str {
        match self {
            StickerFileKind::Png => "image/png",
            StickerFileKind::Lottie => "application/json",
        }
    }

    fn ext(self) -> &'static str {
        match self {
            StickerFileKind::Png => "png",
            StickerFileKind::Lottie => "json",
        }
    }
}

/// Validates sticker bytes (size <= 512 KB, PNG/APNG at 320x320 or Lottie JSON).
pub(super) fn validate_sticker_bytes(bytes: &[u8]) -> Result<StickerFileKind, Refusal> {
    if bytes.is_empty() {
        return Err(Refusal::Refused("That sticker file is empty.".to_owned()));
    }
    if bytes.len() > STICKER_MAX_BYTES {
        return Err(Refusal::Refused("Sticker files must be 512 KB or smaller.".to_owned()));
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (w, h) = image_dimensions(bytes)?;
        if w != STICKER_DIM || h != STICKER_DIM {
            return Err(Refusal::Refused(format!(
                "PNG/APNG stickers must be exactly {STICKER_DIM}x{STICKER_DIM} pixels (this file is {w}x{h})."
            )));
        }
        return Ok(StickerFileKind::Png);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if let Ok(val) = serde_json::from_str::<Value>(text) {
            if val.is_object() && val.get("v").is_some() && val.get("layers").is_some() {
                return Ok(StickerFileKind::Lottie);
            }
        }
    }
    Err(Refusal::Refused(
        "Stickers must be a 320x320 PNG/APNG image or a Lottie JSON file.".to_owned(),
    ))
}

fn read_allowed_file(path: &str, max_bytes: usize) -> Result<Vec<u8>, Refusal> {
    let (real, _) = upload::allowed(path).map_err(|why| Refusal::Refused(format!("That file {why}")))?;
    let mut bytes = Vec::new();
    std::fs::File::open(&real)
        .and_then(|f| f.take((max_bytes as u64) + 1).read_to_end(&mut bytes))
        .map_err(|_| Refusal::Refused("That file could not be read.".to_owned()))?;
    Ok(bytes)
}

pub(super) fn build_sticker_multipart(
    boundary: &str,
    name: &str,
    description: &str,
    tags: &str,
    kind: StickerFileKind,
    bytes: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + 512);
    let safe_file: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let filename = format!("{}.{}", if safe_file.is_empty() { "sticker" } else { &safe_file }, kind.ext());
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n{name}\r\n").as_bytes(),
    );
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"description\"\r\n\r\n{description}\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"tags\"\r\n\r\n{tags}\r\n").as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n",
            kind.mime()
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

impl Discord {
    fn expression_perms(&self, guild_id: &str) -> Result<(bool, bool, u8, Vec<SpaceRoleChoice>), Refusal> {
        if !safe_discord_id(guild_id) {
            return Err(Refusal::Refused("Invalid server id.".to_owned()));
        }
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild_id) else {
            return Err(Refusal::Refused("This server is not available.".to_owned()));
        };
        let perms = info.base_permissions(guild_id);
        let can_manage = perms & MANAGE_GUILD_EXPRESSIONS != 0;
        let can_create = can_manage || (perms & CREATE_GUILD_EXPRESSIONS != 0);
        let mut roles: Vec<SpaceRoleChoice> = info
            .roles
            .iter()
            .filter(|(rid, _)| **rid != number(guild_id))
            .map(|(rid, r)| SpaceRoleChoice { id: rid.to_string(), name: r.name.clone() })
            .collect();
        roles.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok((can_manage, can_create, info.premium_tier, roles))
    }

    fn sync_guild_emoji_add_or_edit(&self, guild_id: &str, emoji: &SpaceEmoji) {
        let mut state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get_mut(guild_id) else {
            return;
        };
        let pickable = Pickable {
            id: emoji.id.clone(),
            name: emoji.name.clone(),
            animated: emoji.animated,
            format: 0,
            roles: emoji.roles.iter().map(|r| number(r)).collect(),
        };
        if let Some(slot) = info.emojis.iter_mut().find(|e| e.id == emoji.id) {
            *slot = pickable;
        } else {
            info.emojis.push(pickable);
        }
    }

    fn sync_guild_sticker_add_or_edit(&self, guild_id: &str, sticker: &SpaceSticker) {
        let mut state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get_mut(guild_id) else {
            return;
        };
        let pickable = Pickable {
            id: sticker.id.clone(),
            name: sticker.name.clone(),
            animated: sticker.format != 1,
            format: sticker.format,
            roles: Box::default(),
        };
        if let Some(slot) = info.stickers.iter_mut().find(|s| s.id == sticker.id) {
            *slot = pickable;
        } else {
            info.stickers.push(pickable);
        }
    }

    /// Reads custom emoji, stickers, slot counts, and role choices for `guild_id`.
    pub(super) fn space_expressions(&self, guild_id: &str) -> Result<SpaceExpressions, Refusal> {
        let (can_manage, can_create, premium_tier, roles) = self.expression_perms(guild_id)?;
        if !can_manage && !can_create {
            return Err(Refusal::Refused(
                "You need permission to manage emoji and stickers in this server.".to_owned(),
            ));
        }
        let mut media = Vec::new();
        let emojis_body = self
            .call("GET", &format!("/guilds/{guild_id}/emojis"), None)
            .map_err(refusal)?;
        let raw_emojis: Vec<RawEmoji> = serde_json::from_str(&emojis_body)
            .map_err(|err| Refusal::Refused(format!("Discord's emoji list was unreadable: {err}")))?;
        let emojis: Vec<SpaceEmoji> = raw_emojis.into_iter().map(|e| map_emoji(e, &mut media)).collect();

        let stickers_body = self
            .call("GET", &format!("/guilds/{guild_id}/stickers"), None)
            .map_err(refusal)?;
        let raw_stickers: Vec<RawSticker> = serde_json::from_str(&stickers_body)
            .map_err(|err| Refusal::Refused(format!("Discord's sticker list was unreadable: {err}")))?;
        let stickers: Vec<SpaceSticker> = raw_stickers.into_iter().map(|s| map_sticker(s, &mut media)).collect();

        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        let emoji_limit = emoji_slot_limit(premium_tier);
        Ok(SpaceExpressions {
            emojis,
            stickers,
            roles,
            static_emoji_limit: emoji_limit,
            animated_emoji_limit: emoji_limit,
            sticker_limit: sticker_slot_limit(premium_tier),
            can_manage,
            can_create,
            premium_tier,
        })
    }

    /// Uploads a new custom emoji (`POST /guilds/{id}/emojis`, base64 data URI, no 429 retry).
    pub(super) fn create_emoji(&self, guild_id: &str, name: &str, path: &str) -> Result<SpaceEmoji, Refusal> {
        let (_, can_create, _, _) = self.expression_perms(guild_id)?;
        if !can_create {
            return Err(Refusal::Refused("You need permission to create custom emoji in this server.".to_owned()));
        }
        let name = validate_emoji_name(name)?;
        let bytes = read_allowed_file(path, EMOJI_MAX_BYTES)?;
        let data_uri = validate_emoji_bytes(&bytes)?;
        let body = self
            .rest
            .call_once_with(
                "POST",
                &format!("/guilds/{guild_id}/emojis"),
                Some(json!({ "name": name, "image": data_uri, "roles": [] })),
                &[],
            )
            .map_err(refusal)?;
        let raw: RawEmoji = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's emoji answer was unreadable: {err}")))?;
        let mut media = Vec::new();
        let emoji = map_emoji(raw, &mut media);
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        self.sync_guild_emoji_add_or_edit(guild_id, &emoji);
        Ok(emoji)
    }

    /// Renames or changes role restrictions on an existing custom emoji (`PATCH /guilds/{id}/emojis/{e}`).
    pub(super) fn edit_emoji(
        &self,
        guild_id: &str,
        emoji_id: &str,
        name: Option<String>,
        roles: Option<Vec<String>>,
    ) -> Result<SpaceEmoji, Refusal> {
        let (can_manage, _, _, _) = self.expression_perms(guild_id)?;
        if !can_manage {
            return Err(Refusal::Refused("You need Manage Expressions permission to edit emoji.".to_owned()));
        }
        if !safe_discord_id(emoji_id) {
            return Err(Refusal::Refused("Invalid emoji id.".to_owned()));
        }
        let mut patch = Map::new();
        if let Some(name) = name {
            patch.insert("name".to_owned(), Value::String(validate_emoji_name(&name)?));
        }
        if let Some(roles) = roles {
            for role in &roles {
                if !safe_discord_id(role) {
                    return Err(Refusal::Refused("Invalid role id.".to_owned()));
                }
            }
            patch.insert("roles".to_owned(), Value::Array(roles.into_iter().map(Value::String).collect()));
        }
        if patch.is_empty() {
            return Err(Refusal::Refused("Nothing to change on this emoji.".to_owned()));
        }
        let body = self
            .call("PATCH", &format!("/guilds/{guild_id}/emojis/{emoji_id}"), Some(Value::Object(patch)))
            .map_err(refusal)?;
        let raw: RawEmoji = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's emoji answer was unreadable: {err}")))?;
        let mut media = Vec::new();
        let emoji = map_emoji(raw, &mut media);
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        self.sync_guild_emoji_add_or_edit(guild_id, &emoji);
        Ok(emoji)
    }

    /// Deletes a custom emoji (`DELETE /guilds/{id}/emojis/{e}`).
    pub(super) fn delete_emoji(&self, guild_id: &str, emoji_id: &str) -> Result<(), Refusal> {
        let (can_manage, _, _, _) = self.expression_perms(guild_id)?;
        if !can_manage {
            return Err(Refusal::Refused("You need Manage Expressions permission to delete emoji.".to_owned()));
        }
        if !safe_discord_id(emoji_id) {
            return Err(Refusal::Refused("Invalid emoji id.".to_owned()));
        }
        self.call("DELETE", &format!("/guilds/{guild_id}/emojis/{emoji_id}"), None)
            .map_err(refusal)?;
        if let Some(info) = crate::hub::lock(&self.state).guilds.get_mut(guild_id) {
            info.emojis.retain(|e| e.id != emoji_id);
        }
        Ok(())
    }

    /// Uploads a new custom sticker (`POST /guilds/{id}/stickers`, multipart, no 429 retry).
    pub(super) fn create_sticker(
        &self,
        guild_id: &str,
        name: &str,
        tags: &str,
        description: &str,
        path: &str,
    ) -> Result<SpaceSticker, Refusal> {
        let (_, can_create, _, _) = self.expression_perms(guild_id)?;
        if !can_create {
            return Err(Refusal::Refused("You need permission to create stickers in this server.".to_owned()));
        }
        let name = validate_sticker_name(name)?;
        let tags = validate_sticker_tags(tags)?;
        let description = validate_sticker_description(description)?;
        let bytes = read_allowed_file(path, STICKER_MAX_BYTES)?;
        let kind = validate_sticker_bytes(&bytes)?;
        let boundary = format!("----discordsticker{}", crate::hub::now());
        let form = build_sticker_multipart(&boundary, &name, &description, &tags, kind, &bytes);
        let body = self
            .rest
            .call_multipart_once(&format!("/guilds/{guild_id}/stickers"), &boundary, &form)
            .map_err(refusal)?;
        let raw: RawSticker = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's sticker answer was unreadable: {err}")))?;
        let mut media = Vec::new();
        let sticker = map_sticker(raw, &mut media);
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        self.sync_guild_sticker_add_or_edit(guild_id, &sticker);
        Ok(sticker)
    }

    /// Renames or edits tags/description on an existing custom sticker (`PATCH /guilds/{id}/stickers/{s}`).
    pub(super) fn edit_sticker(
        &self,
        guild_id: &str,
        sticker_id: &str,
        name: Option<String>,
        tags: Option<String>,
        description: Option<String>,
    ) -> Result<SpaceSticker, Refusal> {
        let (can_manage, _, _, _) = self.expression_perms(guild_id)?;
        if !can_manage {
            return Err(Refusal::Refused("You need Manage Expressions permission to edit stickers.".to_owned()));
        }
        if !safe_discord_id(sticker_id) {
            return Err(Refusal::Refused("Invalid sticker id.".to_owned()));
        }
        let mut patch = Map::new();
        if let Some(name) = name {
            patch.insert("name".to_owned(), Value::String(validate_sticker_name(&name)?));
        }
        if let Some(tags) = tags {
            patch.insert("tags".to_owned(), Value::String(validate_sticker_tags(&tags)?));
        }
        if let Some(desc) = description {
            patch.insert("description".to_owned(), Value::String(validate_sticker_description(&desc)?));
        }
        if patch.is_empty() {
            return Err(Refusal::Refused("Nothing to change on this sticker.".to_owned()));
        }
        let body = self
            .call("PATCH", &format!("/guilds/{guild_id}/stickers/{sticker_id}"), Some(Value::Object(patch)))
            .map_err(refusal)?;
        let raw: RawSticker = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's sticker answer was unreadable: {err}")))?;
        let mut media = Vec::new();
        let sticker = map_sticker(raw, &mut media);
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }
        self.sync_guild_sticker_add_or_edit(guild_id, &sticker);
        Ok(sticker)
    }

    /// Deletes a custom sticker (`DELETE /guilds/{id}/stickers/{s}`).
    pub(super) fn delete_sticker(&self, guild_id: &str, sticker_id: &str) -> Result<(), Refusal> {
        let (can_manage, _, _, _) = self.expression_perms(guild_id)?;
        if !can_manage {
            return Err(Refusal::Refused("You need Manage Expressions permission to delete stickers.".to_owned()));
        }
        if !safe_discord_id(sticker_id) {
            return Err(Refusal::Refused("Invalid sticker id.".to_owned()));
        }
        self.call("DELETE", &format!("/guilds/{guild_id}/stickers/{sticker_id}"), None)
            .map_err(refusal)?;
        if let Some(info) = crate::hub::lock(&self.state).guilds.get_mut(guild_id) {
            info.stickers.retain(|s| s.id != sticker_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(w, h, image::Rgba([20, 120, 220, 255]))
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn slot_limits_match_boost_tiers() {
        assert_eq!((emoji_slot_limit(0), sticker_slot_limit(0)), (50, 5));
        assert_eq!((emoji_slot_limit(1), sticker_slot_limit(1)), (100, 15));
        assert_eq!((emoji_slot_limit(2), sticker_slot_limit(2)), (150, 30));
        assert_eq!((emoji_slot_limit(3), sticker_slot_limit(3)), (250, 60));
    }

    #[test]
    fn validates_emoji_names_and_image_rules() {
        assert!(validate_emoji_name("a").is_err());
        assert!(validate_emoji_name("bad-name").is_err());
        assert!(validate_emoji_name("has space").is_err());
        assert_eq!(validate_emoji_name("ferris_01").unwrap(), "ferris_01");

        let ok_png = png_bytes(64, 64);
        let uri = validate_emoji_bytes(&ok_png).unwrap();
        assert!(uri.starts_with("data:image/png;base64,"));

        let too_big = vec![0u8; EMOJI_MAX_BYTES + 1];
        match validate_emoji_bytes(&too_big) {
            Err(Refusal::Refused(msg)) => assert!(msg.contains("256 KB"), "got: {msg}"),
            other => panic!("expected size refusal, got {other:?}"),
        }
        match validate_emoji_bytes(b"not an image at all") {
            Err(Refusal::Refused(msg)) => assert!(msg.contains("PNG"), "got: {msg}"),
            other => panic!("expected format refusal, got {other:?}"),
        }
    }

    #[test]
    fn validates_sticker_dimensions_and_lottie_and_multipart() {
        let wrong_dim = png_bytes(128, 128);
        match validate_sticker_bytes(&wrong_dim) {
            Err(Refusal::Refused(msg)) => assert!(msg.contains("320x320") && msg.contains("128x128"), "got: {msg}"),
            other => panic!("expected dimension refusal, got {other:?}"),
        }

        assert!(validate_sticker_name("bad\nname").is_err());
        assert!(validate_sticker_tags("wave\r").is_err());
        assert!(validate_sticker_description("hi\n").is_err());

        let good_png = png_bytes(320, 320);
        assert_eq!(validate_sticker_bytes(&good_png).unwrap(), StickerFileKind::Png);

        let lottie = br#"{"v":"5.7.4","fr":60,"w":320,"h":320,"layers":[]}"#;
        assert_eq!(validate_sticker_bytes(lottie).unwrap(), StickerFileKind::Lottie);

        let bad_json = br#"{"hello":"world"}"#;
        assert!(validate_sticker_bytes(bad_json).is_err());

        let form = build_sticker_multipart("BOUND", "Wave", "Hi there", "wave", StickerFileKind::Png, &good_png);
        let head = String::from_utf8_lossy(&form[..form.len().min(300)]);
        assert!(head.contains("name=\"name\"\r\n\r\nWave\r\n"));
        assert!(head.contains("name=\"tags\"\r\n\r\nwave\r\n"));
        assert!(head.contains("Content-Type: image/png"));
    }
}
