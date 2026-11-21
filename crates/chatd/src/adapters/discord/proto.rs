//! What is read from Discord's protobuf settings: the order of the server list, our own status,
//! privacy, and the picker's frecency (`settings-proto/2`). Status, privacy, and favourite
//! emoji, sticker, and GIF lists are written back. Frecency scores are read and never written:
//! a wrong score would replace the account's frecency on every device.
//!
//! Sessions that identify as the current web client get their settings as base64 protobuf
//! (`user_settings_proto`) instead of JSON. The field numbers are from the community's
//! `discord-protos` and unconfirmed against a real account: `PreloadedUserSettings.guild_folders`
//! is field 14, holding `folders` (1, each with packed `guild_ids` 1) and `guild_positions` (2);
//! `PreloadedUserSettings.status` is field 11, holding `status` (1, a wrapped string: `online`,
//! `idle`, `dnd`, `invisible`) and `custom_status` (2, whose `text` is field 1).

use base64::Engine;

/// One field of a message: its number, and its payload for the wire types used here.
enum Field<'a> {
    Bytes(&'a [u8]),
    Fixed64(u64),
    Varint(u64),
    Other,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.0.split_first()?;
            self.0 = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.0.len() < len {
            return None;
        }
        let (taken, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(taken)
    }

    fn field(&mut self) -> Option<(u64, Field<'a>)> {
        let tag = self.varint()?;
        let field = match tag & 7 {
            0 => Field::Varint(self.varint()?),
            1 => Field::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            2 => {
                let len = self.varint()? as usize;
                Field::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Field::Other
            }
            _ => return None,
        };
        Some((tag >> 3, field))
    }
}

/// Every field of a message with the bytes it is written in, to copy the ones left alone.
fn fields(message: &[u8]) -> Vec<(u64, Field<'_>, &[u8])> {
    let mut reader = Reader(message);
    let mut found = Vec::new();
    loop {
        let before = reader.0;
        let Some((number, field)) = reader.field() else {
            return found;
        };
        found.push((number, field, &before[..before.len() - reader.0.len()]));
    }
}

fn bytes_of(message: &[u8], wanted: u64) -> Option<&[u8]> {
    fields(message).into_iter().find_map(|(number, field, _)| match field {
        Field::Bytes(bytes) if number == wanted => Some(bytes),
        _ => None,
    })
}

/// Encodes an unsigned varint into `into`.
fn write_varint(into: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            into.push(byte);
            break;
        }
        into.push(byte | 0x80);
    }
}

/// A length-delimited field: its tag, its length, its bytes.
fn write(into: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    write_varint(into, number << 3 | 2);
    write_varint(into, bytes.len() as u64);
    into.extend_from_slice(bytes);
}

/// A varint field: its tag and value.
fn write_varint_field(into: &mut Vec<u8>, number: u64, value: u64) {
    write_varint(into, number << 3);
    write_varint(into, value);
}

/// A wrapped scalar (`google.protobuf.BoolValue` / `UInt32Value`): length-delimited field
/// containing varint field 1 when non-zero (empty message for 0/false).
fn write_wrapped_varint(into: &mut Vec<u8>, number: u64, value: u64) {
    let mut inner = Vec::new();
    if value != 0 {
        write_varint_field(&mut inner, 1, value);
    }
    write(into, number, &inner);
}

/// Packed repeated `fixed64` field for a list of numeric snowflake ids.
fn write_packed_ids(into: &mut Vec<u8>, number: u64, list: &[String]) {
    let mut packed = Vec::with_capacity(list.len() * 8);
    for id in list {
        if let Ok(parsed) = id.parse::<u64>() {
            packed.extend_from_slice(&parsed.to_le_bytes());
        }
    }
    if !packed.is_empty() {
        write(into, number, &packed);
    }
}

/// Our status settings as Discord sent them, out of the whole settings (or out of an update
/// that carries them). `None` when they are not in it.
pub fn status_settings(settings: &str) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    bytes_of(&bytes, 11).map(<[u8]>::to_vec)
}

/// What the status settings say: the presence word and the custom status line.
pub fn own_status(status: &[u8]) -> (String, String) {
    let text = |bytes: Option<&[u8]>| {
        let inner = bytes.and_then(|bytes| bytes_of(bytes, 1));
        inner.map_or_else(String::new, |inner| String::from_utf8_lossy(inner).into_owned())
    };
    (text(bytes_of(status, 1)), text(bytes_of(status, 2)))
}

/// The status settings with the presence word replaced, and the custom status line too unless
/// `custom` is `None` (empty takes it away). Everything else in them is copied as it was.
pub fn with_status(status: &[u8], word: &str, custom: Option<&str>) -> Vec<u8> {
    let mut out = Vec::with_capacity(status.len() + word.len() + 8);
    for (number, _, raw) in fields(status) {
        if number != 1 && (number != 2 || custom.is_none()) {
            out.extend_from_slice(raw);
        }
    }
    let wrapped = |text: &str| {
        let mut inner = Vec::with_capacity(text.len() + 2);
        write(&mut inner, 1, text.as_bytes());
        inner
    };
    write(&mut out, 1, &wrapped(word));
    if let Some(custom) = custom.filter(|custom| !custom.is_empty()) {
        write(&mut out, 2, &wrapped(custom));
    }
    out
}

/// Settings that hold only the status settings, as the body of a settings update.
pub fn settings(status: &[u8]) -> String {
    let mut whole = Vec::with_capacity(status.len() + 4);
    write(&mut whole, 11, status);
    base64::engine::general_purpose::STANDARD.encode(whole)
}

/// Repeated `fixed64` field `wanted` of a message, packed or not.
fn ids(message: &[u8], wanted: u64, into: &mut Vec<String>) {
    let mut reader = Reader(message);
    while let Some((number, field)) = reader.field() {
        match field {
            Field::Fixed64(id) if number == wanted => into.push(id.to_string()),
            Field::Bytes(packed) if number == wanted => {
                let (whole, _) = packed.as_chunks::<8>();
                into.extend(whole.iter().map(|id| u64::from_le_bytes(*id).to_string()));
            }
            _ => {}
        }
    }
}

/// One of the user's server folders. A server outside any folder comes as a folder of one with
/// no id, which is not a folder at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuildFolder {
    pub id: Option<i64>,
    pub name: String,
    pub color: Option<u32>,
    pub guild_ids: Vec<String>,
}

/// A wrapped scalar (`google.protobuf.Int64Value` and the like): its field 1.
fn wrapped_varint(message: &[u8]) -> Option<u64> {
    let mut reader = Reader(message);
    while let Some((number, field)) = reader.field() {
        if let (1, Field::Varint(value)) = (number, field) {
            return Some(value);
        }
    }
    None
}

/// The user's server folders, in their order. Unverified against a real account: the field
/// numbers are the community's (`GuildFolder`: 1 `guild_ids` packed fixed64, 2 `id` Int64Value,
/// 3 `name` StringValue, 4 `color` UInt64Value). `None` when the settings do not say.
pub fn guild_folders(settings: &str) -> Option<Vec<GuildFolder>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    let folders = bytes_of(&bytes, 14)?;
    let mut found = Vec::new();
    let mut reader = Reader(folders);
    while let Some((number, field)) = reader.field() {
        let (1, Field::Bytes(folder)) = (number, field) else {
            continue;
        };
        let mut parsed = GuildFolder::default();
        ids(folder, 1, &mut parsed.guild_ids);
        for (number, field, _) in fields(folder) {
            match (number, field) {
                (2, Field::Bytes(id)) => parsed.id = wrapped_varint(id).map(|id| id as i64),
                (3, Field::Bytes(name)) => {
                    parsed.name = bytes_of(name, 1).map(|name| String::from_utf8_lossy(name).into_owned()).unwrap_or_default();
                }
                (4, Field::Bytes(color)) => parsed.color = wrapped_varint(color).map(|color| (color & 0xff_ffff) as u32),
                _ => {}
            }
        }
        found.push(parsed);
    }
    (!found.is_empty()).then_some(found)
}

/// The server ids in the user's own order, folders flattened. `None` when the settings do not
/// say (or are not what this expects).
pub fn guild_order(settings: &str) -> Option<Vec<String>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    let mut reader = Reader(&bytes);
    let mut order = Vec::new();
    while let Some((number, field)) = reader.field() {
        let Field::Bytes(folders) = field else {
            continue;
        };
        if number != 14 {
            continue;
        }
        let mut inner = Reader(folders);
        let mut positions = Vec::new();
        while let Some((number, field)) = inner.field() {
            if let (1, Field::Bytes(folder)) = (number, field) {
                ids(folder, 1, &mut order);
            }
        }
        ids(folders, 2, &mut positions);
        if order.is_empty() {
            order = positions;
        }
    }
    (!order.is_empty()).then_some(order)
}

/// Friend-source bitflags (`PrivacySettings.friend_source_flags`, field 12).
pub const FRIEND_SOURCE_ALL: u32 = 1 << 1;
pub const FRIEND_SOURCE_MUTUAL_FRIENDS: u32 = 1 << 2;
pub const FRIEND_SOURCE_MUTUAL_GUILDS: u32 = 1 << 3;

/// Our privacy settings as Discord sent them (field 6 of `PreloadedUserSettings`).
/// `None` when the blob does not contain a privacy block.
pub fn privacy_settings(settings: &str) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    bytes_of(&bytes, 6).map(<[u8]>::to_vec)
}

/// Decodes Discord's `PreloadedUserSettings.privacy` block (field 6) into `NetworkPrivacy`.
/// Field numbers follow `discord-protos`:
/// - 3: `restricted_guild_ids` (repeated fixed64)
/// - 4: `default_guilds_restricted` (bool varint)
/// - 12: `friend_source_flags` (`UInt32Value`: bit 1 all, bit 2 mutual friends, bit 3 mutual guilds)
/// - 17: `message_request_restricted_guild_ids` (repeated fixed64)
/// - 18: `default_message_request_restricted` (`BoolValue`)
/// - 26: `explicit_content_filter` (enum varint: 0/1 off, 2 non-friends, 3 everyone)
/// - 27: `dm_spam_filter_v2` (enum varint: 0/1 off, 2 non-friends, 3 everyone)
pub fn decode_privacy(privacy: &[u8]) -> protocol::NetworkPrivacy {
    let mut out = protocol::NetworkPrivacy::default();
    ids(privacy, 3, &mut out.restricted_spaces);
    ids(privacy, 17, &mut out.request_restricted_spaces);
    for (number, field, _) in fields(privacy) {
        match (number, field) {
            (4, Field::Varint(v)) => out.allow_dms_from_spaces = v == 0,
            (12, Field::Bytes(inner)) => {
                let flags = wrapped_varint(inner).unwrap_or(0) as u32;
                out.friend_requests_everyone = flags & FRIEND_SOURCE_ALL != 0;
                out.friend_requests_mutual_friends = flags & FRIEND_SOURCE_MUTUAL_FRIENDS != 0;
                out.friend_requests_mutual_spaces = flags & FRIEND_SOURCE_MUTUAL_GUILDS != 0;
            }
            (18, Field::Bytes(inner)) => {
                out.allow_message_requests = wrapped_varint(inner).unwrap_or(0) == 0;
            }
            (26, Field::Varint(v)) => {
                out.explicit_content_filter = match v {
                    2 => 1,
                    3 => 2,
                    _ => 0,
                };
            }
            (27, Field::Varint(v)) => {
                out.dm_spam_filter = match v {
                    2 => 1,
                    3 => 2,
                    _ => 0,
                };
            }
            _ => {}
        }
    }
    out
}

/// Re-encodes `privacy` with the fields in `next` updated while copying every unknown field
/// byte-for-byte so other Discord settings in the block are never lost.
pub fn with_privacy(privacy: &[u8], next: &protocol::NetworkPrivacy) -> Vec<u8> {
    let mut out = Vec::with_capacity(privacy.len() + 64);
    for (number, _, raw) in fields(privacy) {
        if !matches!(number, 3 | 4 | 12 | 17 | 18 | 26 | 27) {
            out.extend_from_slice(raw);
        }
    }
    write_packed_ids(&mut out, 3, &next.restricted_spaces);
    if !next.allow_dms_from_spaces {
        write_varint_field(&mut out, 4, 1);
    }
    let mut flags = 0u32;
    if next.friend_requests_everyone {
        flags |= FRIEND_SOURCE_ALL;
    }
    if next.friend_requests_mutual_friends {
        flags |= FRIEND_SOURCE_MUTUAL_FRIENDS;
    }
    if next.friend_requests_mutual_spaces {
        flags |= FRIEND_SOURCE_MUTUAL_GUILDS;
    }
    write_wrapped_varint(&mut out, 12, u64::from(flags));
    write_packed_ids(&mut out, 17, &next.request_restricted_spaces);
    write_wrapped_varint(&mut out, 18, u64::from(!next.allow_message_requests));
    let explicit_wire = match next.explicit_content_filter {
        1 => 2,
        2 => 3,
        _ => 1,
    };
    write_varint_field(&mut out, 26, explicit_wire);
    let spam_wire = match next.dm_spam_filter {
        1 => 2,
        2 => 3,
        _ => 1,
    };
    write_varint_field(&mut out, 27, spam_wire);
    out
}

/// Wraps a `PrivacySettings` block in `PreloadedUserSettings` field 6 and base64-encodes it
/// for `PATCH /users/@me/settings-proto/1`.
pub fn privacy_update_settings(privacy: &[u8]) -> String {
    let mut whole = Vec::with_capacity(privacy.len() + 4);
    write(&mut whole, 6, privacy);
    base64::engine::general_purpose::STANDARD.encode(whole)
}

/// Base64 protobuf as Discord puts it in `settings` or a gateway `proto` field.
pub fn decode_settings(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text.trim()).ok()
}

/// `FrecencyUserSettings` as the body of `PATCH /users/@me/settings-proto/2`.
pub fn encode_settings(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// One saved GIF. The map key on the wire is [`url`](FavoriteGif::url), the page that is sent.
/// `format` is Discord's `GIFType`: 0 none, 1 image, 2 video.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FavoriteGif {
    pub url: String,
    pub src: String,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub order: u32,
}

/// Favourite lists and frecency scores from `FrecencyUserSettings` (settings-proto/2).
/// Field numbers follow the community `discord-protos`:
/// - 2 `favorite_gifs` (map of page URL to `FavoriteGIF`, plus `hide_tooltip`)
/// - 3 `favorite_stickers.sticker_ids` (repeated fixed64)
/// - 4 `sticker_frecency` (map of fixed64 to `FrecencyItem`)
/// - 5 `favorite_emojis.emojis` (repeated string)
/// - 6 `emoji_frecency` (map of string to `FrecencyItem`)
/// `FrecencyItem.score` is field 4; `frecency` (field 3) is used when score is absent.
/// Scores are only read. Favourite writes replace only their own list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrecencyLists {
    pub favorite_gifs: Vec<FavoriteGif>,
    pub favorite_emojis: Vec<String>,
    pub favorite_stickers: Vec<String>,
    /// `(key, score)`, wire order. The picker sorts.
    pub emoji_frecency: Vec<(String, i32)>,
    pub sticker_frecency: Vec<(String, i32)>,
}

/// Decodes a `FrecencyUserSettings` message. Unknown fields are ignored here; the raw bytes
/// are what a later write copies.
pub fn decode_frecency(bytes: &[u8]) -> FrecencyLists {
    let mut lists = FrecencyLists::default();
    for (number, field, _) in fields(bytes) {
        let Field::Bytes(inner) = field else {
            continue;
        };
        match number {
            2 => lists.favorite_gifs = favorite_gif_list(inner),
            3 => ids(inner, 1, &mut lists.favorite_stickers),
            4 => lists.sticker_frecency = frecency_map(inner, true),
            5 => lists.favorite_emojis = strings(inner, 1),
            6 => lists.emoji_frecency = frecency_map(inner, false),
            _ => {}
        }
    }
    lists
}

/// Replaces favourite emoji (field 5) and copies every other field, including frecency scores
/// and anything this client does not know.
pub fn with_favorite_emojis(bytes: &[u8], emojis: &[String]) -> Vec<u8> {
    replace_message(bytes, 5, &strings_message(emojis))
}

/// Replaces favourite stickers (field 3) and copies every other field, including frecency scores.
pub fn with_favorite_stickers(bytes: &[u8], ids: &[String]) -> Vec<u8> {
    let mut inner = Vec::new();
    write_packed_ids(&mut inner, 1, ids);
    replace_message(bytes, 3, &inner)
}

/// Replaces favourite GIFs (field 2) and copies every other field, including frecency scores.
/// Inside the GIF message, map entries (field 1) are replaced and the rest is kept, so
/// `hide_tooltip` survives. An empty list is still written: clearing favourites must clear them.
pub fn with_favorite_gifs(bytes: &[u8], gifs: &[FavoriteGif]) -> Vec<u8> {
    let existing = bytes_of(bytes, 2).unwrap_or(&[]);
    let mut inner = Vec::new();
    for (number, _, raw) in fields(existing) {
        if number != 1 {
            inner.extend_from_slice(raw);
        }
    }
    for gif in gifs {
        let mut value = Vec::new();
        if gif.format != 0 {
            write_varint_field(&mut value, 1, u64::from(gif.format));
        }
        if !gif.src.is_empty() {
            write(&mut value, 2, gif.src.as_bytes());
        }
        if gif.width != 0 {
            write_varint_field(&mut value, 3, u64::from(gif.width));
        }
        if gif.height != 0 {
            write_varint_field(&mut value, 4, u64::from(gif.height));
        }
        if gif.order != 0 {
            write_varint_field(&mut value, 5, u64::from(gif.order));
        }
        let mut entry = Vec::new();
        write(&mut entry, 1, gif.url.as_bytes());
        write(&mut entry, 2, &value);
        write(&mut inner, 1, &entry);
    }
    replace_message(bytes, 2, &inner)
}

/// `FavoriteGIFs.gifs`, sorted by `order`. Anything that is not a map entry is ignored.
fn favorite_gif_list(message: &[u8]) -> Vec<FavoriteGif> {
    let mut out = Vec::new();
    for (number, field, _) in fields(message) {
        let Field::Bytes(entry) = field else {
            continue;
        };
        if number != 1 {
            continue;
        }
        let mut gif = FavoriteGif::default();
        for (number, field, _) in fields(entry) {
            match (number, field) {
                (1, Field::Bytes(bytes)) => gif.url = String::from_utf8_lossy(bytes).into_owned(),
                (2, Field::Bytes(value)) => {
                    for (number, field, _) in fields(value) {
                        match (number, field) {
                            (1, Field::Varint(format)) => gif.format = format as u32,
                            (2, Field::Bytes(src)) => gif.src = String::from_utf8_lossy(src).into_owned(),
                            (3, Field::Varint(width)) => gif.width = width as u32,
                            (4, Field::Varint(height)) => gif.height = height as u32,
                            (5, Field::Varint(order)) => gif.order = order as u32,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if !gif.url.is_empty() {
            out.push(gif);
        }
    }
    out.sort_by_key(|gif| gif.order);
    out
}

fn replace_message(bytes: &[u8], number: u64, inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + inner.len() + 8);
    for (found, _, raw) in fields(bytes) {
        if found != number {
            out.extend_from_slice(raw);
        }
    }
    write(&mut out, number, inner);
    out
}

fn strings_message(list: &[String]) -> Vec<u8> {
    let mut inner = Vec::new();
    for text in list {
        write(&mut inner, 1, text.as_bytes());
    }
    inner
}

fn strings(message: &[u8], wanted: u64) -> Vec<String> {
    let mut out = Vec::new();
    let mut reader = Reader(message);
    while let Some((number, field)) = reader.field() {
        if number == wanted {
            if let Field::Bytes(bytes) = field {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    if !text.is_empty() {
                        out.push(text.to_owned());
                    }
                }
            }
        }
    }
    out
}

/// Map field 1 of an `EmojiFrecency` or `StickerFrecency` message. `fixed_key` is a sticker id.
fn frecency_map(message: &[u8], fixed_key: bool) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let mut reader = Reader(message);
    while let Some((number, field)) = reader.field() {
        let Field::Bytes(entry) = field else {
            continue;
        };
        if number != 1 {
            continue;
        }
        let mut key = String::new();
        let mut score = 0i32;
        let mut inner = Reader(entry);
        while let Some((number, field)) = inner.field() {
            match (number, field) {
                (1, Field::Bytes(bytes)) if !fixed_key => key = String::from_utf8_lossy(bytes).into_owned(),
                (1, Field::Fixed64(id)) if fixed_key => key = id.to_string(),
                (2, Field::Bytes(item)) => score = frecency_score(item),
                _ => {}
            }
        }
        if !key.is_empty() {
            out.push((key, score));
        }
    }
    out
}

/// `FrecencyItem.score` (field 4), else `frecency` (field 3).
fn frecency_score(item: &[u8]) -> i32 {
    let mut score = None;
    let mut frecency = 0i32;
    for (number, field, _) in fields(item) {
        if let Field::Varint(value) = field {
            if number == 4 {
                score = Some(value as i32);
            } else if number == 3 {
                frecency = value as i32;
            }
        }
    }
    score.unwrap_or(frecency)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_settings_round_trip_and_preserve_unknown_fields() {
        // Build a PrivacySettings block with restricted guild 4000, default_guilds_restricted=true,
        // friend_source_flags = mutual_guilds only (8), default_message_request_restricted=true,
        // explicit_content_filter = 2 (non-friends), dm_spam_filter = 3 (everyone),
        // PLUS two unknown fields (field 8 BoolValue=true, field 99 varint=77).
        let mut raw = Vec::new();
        write_packed_ids(&mut raw, 3, &["4000".to_owned()]);
        write_varint_field(&mut raw, 4, 1);
        write_wrapped_varint(&mut raw, 8, 1); // unknown field 8
        write_wrapped_varint(&mut raw, 12, u64::from(FRIEND_SOURCE_MUTUAL_GUILDS));
        write_packed_ids(&mut raw, 17, &["5000".to_owned()]);
        write_wrapped_varint(&mut raw, 18, 1);
        write_varint_field(&mut raw, 26, 2);
        write_varint_field(&mut raw, 27, 3);
        write_varint_field(&mut raw, 99, 77); // unknown field 99

        let encoded = privacy_update_settings(&raw);
        let extracted = privacy_settings(&encoded).unwrap();
        let decoded = decode_privacy(&extracted);
        assert!(!decoded.friend_requests_everyone);
        assert!(!decoded.friend_requests_mutual_friends);
        assert!(decoded.friend_requests_mutual_spaces);
        assert!(!decoded.allow_dms_from_spaces);
        assert!(!decoded.allow_message_requests);
        assert_eq!(decoded.explicit_content_filter, 1);
        assert_eq!(decoded.dm_spam_filter, 2);
        assert_eq!(decoded.restricted_spaces, vec!["4000".to_owned()]);
        assert_eq!(decoded.request_restricted_spaces, vec!["5000".to_owned()]);

        // Mutate and re-encode: unknown fields 8 and 99 must remain intact byte-for-byte.
        let mut next = decoded.clone();
        next.friend_requests_everyone = true;
        next.allow_dms_from_spaces = true;
        next.restricted_spaces.push("6000".to_owned());
        let rewritten = with_privacy(&extracted, &next);
        let again = decode_privacy(&rewritten);
        assert_eq!(again, next);
        assert!(fields(&rewritten).iter().any(|(n, _, _)| *n == 8), "unknown field 8 must survive");
        assert!(
            fields(&rewritten).iter().any(|(n, f, _)| *n == 99 && matches!(f, Field::Varint(77))),
            "unknown field 99 must survive"
        );
    }

    #[test]
    fn folders_flatten_into_one_order() {
        let packed = |ids: &[u64]| ids.iter().flat_map(|id| id.to_le_bytes()).collect::<Vec<u8>>();
        // Two folders: [30, 10] and [20].
        let mut folders = Vec::new();
        for ids in [&[30u64, 10][..], &[20][..]] {
            let ids = packed(ids);
            let folder = [&[0x0a, ids.len() as u8][..], &ids].concat();
            folders.extend([&[0x0a, folder.len() as u8][..], &folder].concat());
        }
        // An unrelated varint field first, then field 14.
        let settings = [&[0x08, 0x01, 0x72, folders.len() as u8][..], &folders].concat();
        let encoded = base64::engine::general_purpose::STANDARD.encode(settings);
        assert_eq!(guild_order(&encoded).unwrap(), ["30", "10", "20"]);
        assert_eq!(guild_order("not base64!"), None);
    }

    #[test]
    fn folders_keep_their_id_name_and_colour_and_a_loose_server_has_none() {
        let mut folders = Vec::new();
        // A named, coloured folder of [30, 10], then a loose server 20.
        let mut folder = Vec::new();
        write(&mut folder, 1, &[30u64, 10].iter().flat_map(|id| id.to_le_bytes()).collect::<Vec<u8>>());
        write(&mut folder, 2, &[0x08, 0x07]);
        let mut name = Vec::new();
        write(&mut name, 1, b"Games");
        write(&mut folder, 3, &name);
        write(&mut folder, 4, &[0x08, 0xff, 0x01]);
        write(&mut folders, 1, &folder);
        let mut loose = Vec::new();
        write(&mut loose, 1, &20u64.to_le_bytes());
        write(&mut folders, 1, &loose);
        let mut settings = Vec::new();
        write(&mut settings, 14, &folders);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&settings);
        let parsed = guild_folders(&encoded).unwrap();
        assert_eq!(parsed[0], GuildFolder { id: Some(7), name: "Games".into(), color: Some(255), guild_ids: vec!["30".into(), "10".into()] });
        assert_eq!(parsed[1], GuildFolder { id: None, name: String::new(), color: None, guild_ids: vec!["20".into()] });
        assert_eq!(guild_order(&encoded).unwrap(), ["30", "10", "20"]);
    }

    #[test]
    fn the_status_is_changed_and_the_rest_of_its_settings_kept() {
        // Status settings: `dnd`, a custom line, and a field this does not know (3, a varint).
        let mut status = Vec::new();
        write(&mut status, 1, &[0x0a, 3, b'd', b'n', b'd']);
        write(&mut status, 2, &[0x0a, 5, b'l', b'u', b'n', b'c', b'h', 0x21, 9, 9, 9, 9, 9, 9, 9, 9]);
        status.extend([0x18, 0x01]);
        let sent = status_settings(&settings(&status)).unwrap();
        assert_eq!(own_status(&sent), ("dnd".to_owned(), "lunch".to_owned()));

        // Presence alone: the line stays, with its other fields.
        let idle = with_status(&status, "idle", None);
        assert_eq!(own_status(&idle), ("idle".to_owned(), "lunch".to_owned()));
        assert!(idle.windows(2).any(|pair| pair == [0x18, 0x01]), "the unknown field was dropped");
        assert_eq!(bytes_of(&idle, 2), bytes_of(&status, 2));
        // A new line, then none.
        assert_eq!(own_status(&with_status(&idle, "online", Some("back"))), ("online".to_owned(), "back".to_owned()));
        let cleared = with_status(&idle, "online", Some(""));
        assert_eq!((own_status(&cleared).1.as_str(), bytes_of(&cleared, 2)), ("", None));
        assert_eq!(status_settings("AAAA"), None);
    }

    #[test]
    fn frecency_favourites_round_trip_and_leave_scores_and_unknown_fields() {
        // Favourite emoji, favourite stickers, both frecency maps, a stand-in for favorite_gifs
        // (field 2), and an unknown varint (field 99).
        let mut emojis = Vec::new();
        write(&mut emojis, 1, "😀".as_bytes());
        write(&mut emojis, 1, b"ferris:5101");
        let mut stickers = Vec::new();
        write_packed_ids(&mut stickers, 1, &["111".into(), "222".into()]);
        let mut item = Vec::new();
        write_varint_field(&mut item, 1, 3);
        write_varint_field(&mut item, 4, 40);
        let mut entry = Vec::new();
        write(&mut entry, 1, "🎉".as_bytes());
        write(&mut entry, 2, &item);
        let mut emoji_frecency = Vec::new();
        write(&mut emoji_frecency, 1, &entry);
        let mut scored = Vec::new();
        write_varint_field(&mut scored, 3, 9);
        let mut sticker_entry = Vec::new();
        sticker_entry.push(0x09);
        sticker_entry.extend_from_slice(&333u64.to_le_bytes());
        write(&mut sticker_entry, 2, &scored);
        let mut sticker_frecency = Vec::new();
        write(&mut sticker_frecency, 1, &sticker_entry);
        let mut whole = Vec::new();
        write(&mut whole, 2, b"gif-unknown");
        write(&mut whole, 3, &stickers);
        write(&mut whole, 4, &sticker_frecency);
        write(&mut whole, 5, &emojis);
        write(&mut whole, 6, &emoji_frecency);
        write_varint_field(&mut whole, 99, 77);

        let lists = decode_frecency(&whole);
        assert_eq!(lists.favorite_emojis, ["😀", "ferris:5101"]);
        assert_eq!(lists.favorite_stickers, ["111", "222"]);
        assert_eq!(lists.emoji_frecency, vec![("🎉".to_owned(), 40)]);
        assert_eq!(lists.sticker_frecency, vec![("333".to_owned(), 9)]);

        let next = with_favorite_emojis(&whole, &["ferris:5101".into(), "wave:1".into()]);
        let again = decode_frecency(&next);
        assert_eq!(again.favorite_emojis, ["ferris:5101", "wave:1"]);
        assert_eq!(again.favorite_stickers, lists.favorite_stickers);
        assert_eq!(again.emoji_frecency, lists.emoji_frecency);
        assert_eq!(again.sticker_frecency, lists.sticker_frecency);
        assert!(
            fields(&next).iter().any(|(n, field, _)| *n == 2 && matches!(field, Field::Bytes(bytes) if bytes == b"gif-unknown")),
            "favorite_gifs must survive an emoji favourite write"
        );
        assert!(
            fields(&next).iter().any(|(n, field, _)| *n == 99 && matches!(field, Field::Varint(77))),
            "unknown field 99 must survive"
        );

        let stick = with_favorite_stickers(&next, &["222".into()]);
        let stick_lists = decode_frecency(&stick);
        assert_eq!(stick_lists.favorite_stickers, ["222"]);
        assert_eq!(stick_lists.favorite_emojis, again.favorite_emojis);
        assert_eq!(stick_lists.emoji_frecency, lists.emoji_frecency, "frecency scores are not rewritten");
        assert_eq!(stick_lists.sticker_frecency, lists.sticker_frecency, "frecency scores are not rewritten");
        let encoded = encode_settings(&stick);
        assert_eq!(decode_frecency(&decode_settings(&encoded).unwrap()).favorite_stickers, ["222"]);
    }

    #[test]
    fn favorite_gifs_round_trip_and_leave_tooltip_scores_and_unknown_fields() {
        // Two GIFs out of order, hide_tooltip, an emoji favourite, and an unknown varint.
        let gif = |url: &str, src: &str, order: u64| {
            let mut value = Vec::new();
            write_varint_field(&mut value, 1, 1);
            write(&mut value, 2, src.as_bytes());
            write_varint_field(&mut value, 3, 220);
            write_varint_field(&mut value, 4, 120);
            write_varint_field(&mut value, 5, order);
            let mut entry = Vec::new();
            write(&mut entry, 1, url.as_bytes());
            write(&mut entry, 2, &value);
            entry
        };
        let mut gifs = Vec::new();
        write_varint_field(&mut gifs, 2, 1);
        write(&mut gifs, 1, &gif("https://tenor.com/view/later", "https://media.tenor.com/later.gif", 2));
        write(&mut gifs, 1, &gif("https://tenor.com/view/first", "https://media.tenor.com/first.gif", 1));
        let mut emojis = Vec::new();
        write(&mut emojis, 1, "😀".as_bytes());
        let mut whole = Vec::new();
        write(&mut whole, 2, &gifs);
        write(&mut whole, 5, &emojis);
        write_varint_field(&mut whole, 99, 77);

        let lists = decode_frecency(&whole);
        assert_eq!(
            lists.favorite_gifs.iter().map(|gif| gif.url.as_str()).collect::<Vec<_>>(),
            ["https://tenor.com/view/first", "https://tenor.com/view/later"],
            "favourites are ordered by their order field"
        );
        assert_eq!(lists.favorite_gifs[0].src, "https://media.tenor.com/first.gif");
        assert_eq!((lists.favorite_gifs[0].width, lists.favorite_gifs[0].height, lists.favorite_gifs[0].format), (220, 120, 1));

        let next = with_favorite_gifs(&whole, &lists.favorite_gifs[..1]);
        let again = decode_frecency(&next);
        assert_eq!(again.favorite_gifs.len(), 1);
        assert_eq!(again.favorite_emojis, ["😀"]);
        let inner = bytes_of(&next, 2).unwrap();
        assert!(
            fields(inner).iter().any(|(n, field, _)| *n == 2 && matches!(field, Field::Varint(1))),
            "hide_tooltip survives a GIF favourite write"
        );
        assert!(
            fields(&next).iter().any(|(n, field, _)| *n == 99 && matches!(field, Field::Varint(77))),
            "unknown field 99 must survive a GIF favourite write"
        );
        let emoji_write = with_favorite_emojis(&next, &["😀".into()]);
        assert_eq!(decode_frecency(&emoji_write).favorite_gifs, again.favorite_gifs, "an emoji write leaves GIF favourites");
        let cleared = with_favorite_gifs(&next, &[]);
        assert!(decode_frecency(&cleared).favorite_gifs.is_empty(), "an empty list is written, so a clear is a clear");
        assert!(bytes_of(&cleared, 2).is_some());
        assert!(fields(bytes_of(&cleared, 2).unwrap()).iter().any(|(n, _, _)| *n == 2));
    }
}
