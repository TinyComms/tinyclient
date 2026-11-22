//! What is read from Discord's protobuf settings: the server list and its folders, our own status,
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
            // The 10th byte only has room for bit 63. A larger payload shifts out of u64.
            if shift == 63 && byte > 1 {
                return None;
            }
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
                let len = usize::try_from(self.varint()?).ok()?;
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

/// A 64-bit little-endian fixed field (`fixed64`): its tag (wire type 1) and 8 bytes.
fn write_fixed64(into: &mut Vec<u8>, number: u64, value: u64) {
    write_varint(into, number << 3 | 1);
    into.extend_from_slice(&value.to_le_bytes());
}

/// Discord's `CustomStatus` protobuf message (field 2 of `StatusSettings`):
/// field 1 = `text`, field 2 = `emoji_id` (fixed64), field 3 = `emoji_name`,
/// field 4 = `expires_at_ms` (fixed64).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomStatus {
    pub text: String,
    pub emoji_id: Option<u64>,
    pub emoji_name: String,
    pub expires_at_ms: Option<u64>,
}

impl CustomStatus {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.emoji_name.is_empty() && self.emoji_id.is_none()
    }
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

/// What the status settings say: the presence word and the full custom status (text, emoji, expiry).
pub fn own_custom_status(status: &[u8]) -> (String, CustomStatus) {
    let word = bytes_of(status, 1)
        .and_then(|bytes| bytes_of(bytes, 1))
        .map_or_else(String::new, |inner| String::from_utf8_lossy(inner).into_owned());
    let mut custom = CustomStatus::default();
    if let Some(raw) = bytes_of(status, 2) {
        for (number, field, _) in fields(raw) {
            match (number, field) {
                (1, Field::Bytes(bytes)) => custom.text = String::from_utf8_lossy(bytes).into_owned(),
                (2, Field::Fixed64(id)) if id != 0 => custom.emoji_id = Some(id),
                (3, Field::Bytes(bytes)) => custom.emoji_name = String::from_utf8_lossy(bytes).into_owned(),
                (4, Field::Fixed64(ms)) if ms != 0 => custom.expires_at_ms = Some(ms),
                _ => {}
            }
        }
    }
    (word, custom)
}

/// What the status settings say: the presence word and the custom status line.
pub fn own_status(status: &[u8]) -> (String, String) {
    let (word, custom) = own_custom_status(status);
    (word, custom.text)
}

/// The status settings with the presence word replaced, and the custom status replaced unless
/// `custom` is `None` (an empty `CustomStatus` clears field 2). Unknown fields in both
/// `StatusSettings` and `CustomStatus` are copied intact.
pub fn with_custom_status(status: &[u8], word: &str, custom: Option<&CustomStatus>) -> Vec<u8> {
    let mut out = Vec::with_capacity(status.len() + word.len() + 32);
    let mut prior_custom: Option<&[u8]> = None;
    for (number, field, raw) in fields(status) {
        if number == 2 {
            if let Field::Bytes(bytes) = field {
                prior_custom = Some(bytes);
            }
        }
        if number != 1 && (number != 2 || custom.is_none()) {
            out.extend_from_slice(raw);
        }
    }
    let mut wrapped_word = Vec::with_capacity(word.len() + 2);
    write(&mut wrapped_word, 1, word.as_bytes());
    write(&mut out, 1, &wrapped_word);
    if let Some(custom) = custom.filter(|c| !c.is_empty()) {
        let mut inner = Vec::with_capacity(custom.text.len() + custom.emoji_name.len() + 32);
        if let Some(prev) = prior_custom {
            for (number, _, raw) in fields(prev) {
                if !matches!(number, 1 | 2 | 3 | 4) {
                    inner.extend_from_slice(raw);
                }
            }
        }
        if !custom.text.is_empty() {
            write(&mut inner, 1, custom.text.as_bytes());
        }
        if let Some(id) = custom.emoji_id.filter(|&id| id != 0) {
            write_fixed64(&mut inner, 2, id);
        }
        if !custom.emoji_name.is_empty() {
            write(&mut inner, 3, custom.emoji_name.as_bytes());
        }
        if let Some(ms) = custom.expires_at_ms.filter(|&ms| ms != 0) {
            write_fixed64(&mut inner, 4, ms);
        }
        write(&mut out, 2, &inner);
    }
    out
}

/// The status settings with the presence word replaced, and the custom status line too unless
/// `custom` is `None` (empty takes it away). Everything else in them is copied as it was.
pub fn with_status(status: &[u8], word: &str, custom: Option<&str>) -> Vec<u8> {
    let custom = custom.map(|text| CustomStatus { text: text.to_owned(), ..CustomStatus::default() });
    with_custom_status(status, word, custom.as_ref())
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

/// Our guild folders settings block (field 14) as Discord sent it. `None` when absent.
pub fn guild_folders_settings(settings: &str) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    bytes_of(&bytes, 14).map(<[u8]>::to_vec)
}

/// Replaces `folders` (field 1) and `guild_positions` (field 2) inside field 14 (`GuildFolders`),
/// copying every unknown field of `raw` byte-for-byte untouched.
pub fn with_guild_folders(raw: &[u8], folders: &[GuildFolder]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + folders.len() * 32);
    for (number, _, raw_field) in fields(raw) {
        if number != 1 && number != 2 {
            out.extend_from_slice(raw_field);
        }
    }
    let mut positions = Vec::new();
    for folder in folders {
        let mut packed = Vec::with_capacity(folder.guild_ids.len() * 8);
        for id in &folder.guild_ids {
            if let Ok(num) = id.parse::<u64>() {
                packed.extend_from_slice(&num.to_le_bytes());
                positions.extend_from_slice(&num.to_le_bytes());
            }
        }
        if packed.is_empty() {
            continue;
        }
        let mut entry = Vec::with_capacity(packed.len() + folder.name.len() + 24);
        write(&mut entry, 1, &packed);
        if let Some(id) = folder.id {
            let mut wrapped = Vec::new();
            write_varint_field(&mut wrapped, 1, id as u64);
            write(&mut entry, 2, &wrapped);
        }
        if !folder.name.is_empty() {
            let mut wrapped = Vec::new();
            write(&mut wrapped, 1, folder.name.as_bytes());
            write(&mut entry, 3, &wrapped);
        }
        if let Some(color) = folder.color {
            let mut wrapped = Vec::new();
            write_varint_field(&mut wrapped, 1, u64::from(color & 0xff_ffff));
            write(&mut entry, 4, &wrapped);
        }
        write(&mut out, 1, &entry);
    }
    if !positions.is_empty() {
        write(&mut out, 2, &positions);
    }
    out
}

/// Settings proto holding only the `guild_folders` field (14), for `PATCH /users/@me/settings-proto/1`.
pub fn settings_with_folders(folders_block: &[u8]) -> String {
    let mut whole = Vec::with_capacity(folders_block.len() + 4);
    write(&mut whole, 14, folders_block);
    base64::engine::general_purpose::STANDARD.encode(whole)
}

/// Reorders `space`, moves it into or out of a folder, or drops it onto `into_space` to make a
/// folder. `before` is a server to insert ahead of (absent: the end of that list). `folder` is
/// an existing folder id; absent means the server sits on its own. `into_space` is not combined
/// with `folder`. A folder left with one server becomes a loose server, which is how Discord
/// drops an emptied folder. `new_id` is the id of a folder this creates.
pub(super) fn move_guild(
    folders: &[GuildFolder],
    space: &str,
    before: Option<&str>,
    folder: Option<&str>,
    into_space: Option<&str>,
    new_id: i64,
) -> Result<Vec<GuildFolder>, &'static str> {
    if space.is_empty() {
        return Err("That server is not on this account.");
    }
    if into_space == Some(space) {
        return Err("A server cannot be dropped onto itself.");
    }
    if folder.is_some() && into_space.is_some() {
        return Err("Move a server into a folder or onto another server, not both.");
    }
    if into_space.is_none() && folder.is_none() && before == Some(space) {
        return Ok(folders.to_vec());
    }

    let mut entries = folders.to_vec();
    if let Some(target) = into_space {
        if target.is_empty() {
            return Err("That server is not on this account.");
        }
        let anchor = entries.iter().position(|entry| entry.guild_ids.iter().any(|id| id == target));
        let removed_before = remove_guilds(&mut entries, &[space, target], anchor);
        let insert_at = anchor.map(|index| index - removed_before).unwrap_or(0).min(entries.len());
        let guild_ids = if before == Some(target) {
            vec![space.to_owned(), target.to_owned()]
        } else {
            vec![target.to_owned(), space.to_owned()]
        };
        entries.insert(insert_at, GuildFolder { id: Some(new_id), name: String::new(), color: None, guild_ids });
        dissolve_thin_folders(&mut entries);
        return Ok(entries);
    }

    remove_guilds(&mut entries, &[space], None);
    if let Some(folder_id) = folder {
        let id = folder_id.parse::<i64>().map_err(|_| "That folder is not on this account.")?;
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == Some(id)) else {
            return Err("That folder is not on this account.");
        };
        let at = before.and_then(|id| entry.guild_ids.iter().position(|guild| guild == id)).unwrap_or(entry.guild_ids.len());
        entry.guild_ids.insert(at, space.to_owned());
    } else {
        let before = before.filter(|id| !id.is_empty());
        let insert_at = match before {
            Some(id) => entries.iter().position(|entry| entry.guild_ids.iter().any(|guild| guild == id)).unwrap_or(entries.len()),
            None => entries.len(),
        };
        entries.insert(insert_at, GuildFolder { id: None, name: String::new(), color: None, guild_ids: vec![space.to_owned()] });
    }
    dissolve_thin_folders(&mut entries);
    Ok(entries)
}

/// Renames or recolors one folder. `name` absent leaves the name; empty clears it. `color`
/// absent leaves the color; `Some(None)` clears it.
pub(super) fn edit_guild_folder(
    folders: &mut [GuildFolder],
    folder_id: i64,
    name: Option<&str>,
    color: Option<Option<u32>>,
) -> Result<(), &'static str> {
    let Some(entry) = folders.iter_mut().find(|entry| entry.id == Some(folder_id)) else {
        return Err("That folder is not on this account.");
    };
    if let Some(name) = name {
        entry.name = name.trim().chars().take(100).collect();
    }
    if let Some(color) = color {
        entry.color = color.map(|color| color & 0xff_ffff);
    }
    Ok(())
}

/// Drops `gone` out of every entry. Returns how many entries before `anchor` disappeared, so a
/// new folder can be inserted where the target used to be.
fn remove_guilds(entries: &mut Vec<GuildFolder>, gone: &[&str], anchor: Option<usize>) -> usize {
    let mut removed_before = 0;
    let mut kept = Vec::with_capacity(entries.len());
    for (index, mut entry) in entries.drain(..).enumerate() {
        entry.guild_ids.retain(|id| !gone.iter().any(|drop| id == *drop));
        if entry.guild_ids.is_empty() {
            if anchor.is_some_and(|anchor| index < anchor) {
                removed_before += 1;
            }
        } else {
            kept.push(entry);
        }
    }
    *entries = kept;
    removed_before
}

/// Discord only keeps a folder that holds two servers. One left becomes a server on its own.
fn dissolve_thin_folders(entries: &mut [GuildFolder]) {
    for entry in entries.iter_mut() {
        if entry.id.is_some() && entry.guild_ids.len() < 2 {
            entry.id = None;
            entry.name.clear();
            entry.color = None;
        }
    }
}

/// Replaces only field 14 in a whole `PreloadedUserSettings` protobuf blob, preserving every
/// other top-level field (and every unknown subfield inside field 14) byte-for-byte.
#[cfg(test)]
pub fn replace_guild_folders_in_whole(whole: &[u8], folders: &[GuildFolder]) -> Vec<u8> {
    let existing = bytes_of(whole, 14).unwrap_or(&[]);
    let updated = with_guild_folders(existing, folders);
    let mut out = Vec::with_capacity(whole.len() + updated.len());
    for (number, _, raw) in fields(whole) {
        if number != 14 {
            out.extend_from_slice(raw);
        }
    }
    write(&mut out, 14, &updated);
    out
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

/// Whether Discord's sensitive-media filter (`PreloadedUserSettings.text_and_images` field 6,
/// `explicit_content_filter` field 14: 0 show, 1 blur, 2 block) blurs explicitly flagged media.
/// Returns `false` only when the account explicitly chose `0` (Show); defaults to `true`.
pub fn blur_explicit_media(settings: &str) -> bool {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(settings.trim()) else {
        return true;
    };
    let Some(text_and_images) = bytes_of(&bytes, 6) else {
        return true;
    };
    let Some(filter) = bytes_of(text_and_images, 14) else {
        return true;
    };
    wrapped_varint(filter).is_none_or(|value| value != 0)
}
/// Friend-source bitflags (`PrivacySettings.friend_source_flags`, field 11).
/// Bit 1 mutual friends, bit 2 mutual guilds, bit 3 no relation (everyone).
pub const FRIEND_SOURCE_MUTUAL_FRIENDS: u32 = 1 << 1;
pub const FRIEND_SOURCE_MUTUAL_GUILDS: u32 = 1 << 2;
pub const FRIEND_SOURCE_ALL: u32 = 1 << 3;

/// `TextAndImagesSettings` (field 6 of `PreloadedUserSettings`). Explicit content and the DM
/// spam filter live here, not in privacy. `None` when the blob has no text-and-images block.
pub fn text_and_images_settings(settings: &str) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    bytes_of(&bytes, 6).map(<[u8]>::to_vec)
}

/// Our privacy settings as Discord sent them (field 8 of `PreloadedUserSettings`).
/// `None` when the blob does not contain a privacy block.
pub fn privacy_settings(settings: &str) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(settings.trim()).ok()?;
    bytes_of(&bytes, 8).map(<[u8]>::to_vec)
}

/// Decodes Discord's `PrivacySettings` block (field 8) into `NetworkPrivacy`.
/// Explicit content and the DM spam filter are not in this message; see [`apply_content_filters`].
/// Field numbers follow `discord-protos`:
/// - 3: `restricted_guild_ids` (repeated fixed64)
/// - 4: `default_guilds_restricted` (bool)
/// - 11: `friend_source_flags` (`UInt32Value`)
/// - 16: `message_request_restricted_guild_ids` (repeated fixed64)
/// - 17: `default_message_request_restricted` (`BoolValue`)
pub fn decode_privacy(privacy: &[u8]) -> protocol::NetworkPrivacy {
    let mut out = protocol::NetworkPrivacy::default();
    ids(privacy, 3, &mut out.restricted_spaces);
    ids(privacy, 16, &mut out.request_restricted_spaces);
    for (number, field, _) in fields(privacy) {
        match (number, field) {
            (4, Field::Varint(v)) => out.allow_dms_from_spaces = v == 0,
            (11, Field::Bytes(inner)) => {
                let flags = wrapped_varint(inner).unwrap_or(0) as u32;
                out.friend_requests_everyone = flags & FRIEND_SOURCE_ALL != 0;
                out.friend_requests_mutual_friends = flags & FRIEND_SOURCE_MUTUAL_FRIENDS != 0;
                out.friend_requests_mutual_spaces = flags & FRIEND_SOURCE_MUTUAL_GUILDS != 0;
            }
            (17, Field::Bytes(inner)) => {
                out.allow_message_requests = wrapped_varint(inner).unwrap_or(0) == 0;
            }
            _ => {}
        }
    }
    out
}

/// Reads explicit content and the DM spam filter from `TextAndImagesSettings`.
/// An empty block is left alone. Field numbers follow `discord-protos`:
/// - 19: `explicit_content_filter` (`UInt32Value`: 0 off, 1 non-friends, 2 everyone)
/// - 26: deprecated `dm_spam_filter` (`UInt32Value`, same 0/1/2 scale), used when v2 is absent
/// - 27: `dm_spam_filter_v2` (enum: 0 unset means non-friends, 1 off, 2 non-friends, 3 everyone)
pub fn apply_content_filters(text: &[u8], out: &mut protocol::NetworkPrivacy) {
    if text.is_empty() {
        return;
    }
    let mut explicit = None;
    let mut spam_v2 = None;
    let mut spam_v1 = None;
    for (number, field, _) in fields(text) {
        match (number, field) {
            (19, Field::Bytes(inner)) => explicit = Some(content_level(wrapped_varint(inner).unwrap_or(0))),
            (26, Field::Bytes(inner)) => spam_v1 = Some(content_level(wrapped_varint(inner).unwrap_or(0))),
            (27, Field::Varint(v)) => {
                spam_v2 = Some(match v {
                    0 | 2 => 1,
                    3 => 2,
                    _ => 0,
                });
            }
            _ => {}
        }
    }
    out.explicit_content_filter = explicit.unwrap_or(0);
    // Unset v2 means non-friends. A present v2 wins over the deprecated field.
    out.dm_spam_filter = spam_v2.or(spam_v1).unwrap_or(1);
}

/// `explicit_content_filter` / deprecated `dm_spam_filter`: 0 off, 1 non-friends, 2 everyone.
fn content_level(value: u64) -> u8 {
    match value {
        1 => 1,
        2 => 2,
        _ => 0,
    }
}

/// Re-encodes `privacy` with the fields in `next` updated while copying every other field
/// byte-for-byte so the rest of `PrivacySettings` is never lost.
pub fn with_privacy(privacy: &[u8], next: &protocol::NetworkPrivacy) -> Vec<u8> {
    let mut out = Vec::with_capacity(privacy.len() + 64);
    for (number, _, raw) in fields(privacy) {
        if !matches!(number, 3 | 4 | 11 | 16 | 17) {
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
    write_wrapped_varint(&mut out, 11, u64::from(flags));
    write_packed_ids(&mut out, 16, &next.request_restricted_spaces);
    write_wrapped_varint(&mut out, 17, u64::from(!next.allow_message_requests));
    out
}

/// Re-encodes `text` with explicit content and the DM spam filter from `next`.
/// Every other text-and-images field is copied. Does not invent a block: the caller
/// skips the write when Discord has not sent one, because a partial field 6 resets the rest.
pub fn with_content_filters(text: &[u8], next: &protocol::NetworkPrivacy) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 24);
    for (number, _, raw) in fields(text) {
        if !matches!(number, 19 | 26 | 27) {
            out.extend_from_slice(raw);
        }
    }
    let plain = u64::from(content_level(u64::from(next.explicit_content_filter)));
    write_wrapped_varint(&mut out, 19, plain);
    let spam = u64::from(content_level(u64::from(next.dm_spam_filter)));
    write_wrapped_varint(&mut out, 26, spam);
    let spam_v2 = match next.dm_spam_filter {
        1 => 2,
        2 => 3,
        _ => 1,
    };
    write_varint_field(&mut out, 27, spam_v2);
    out
}

/// Base64 `PreloadedUserSettings` for `PATCH /users/@me/settings-proto/1`.
/// Privacy is field 8. `text_and_images`, when present, is field 6. The privacy block is
/// never written as field 6.
pub fn privacy_update_settings(privacy: &[u8], text_and_images: Option<&[u8]>) -> String {
    let mut whole = Vec::with_capacity(privacy.len() + text_and_images.map_or(0, <[u8]>::len) + 8);
    if let Some(text) = text_and_images.filter(|text| !text.is_empty()) {
        write(&mut whole, 6, text);
    }
    write(&mut whole, 8, privacy);
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
        // PrivacySettings (outer field 8): restricted guild 4000, default_guilds_restricted,
        // detect_platform_accounts (field 8, left alone), friend sources = mutual guilds only,
        // friend_discovery_flags (field 12, left alone), message-request guild 5000,
        // default_message_request_restricted, and an unknown varint (field 99).
        let mut raw = Vec::new();
        write_packed_ids(&mut raw, 3, &["4000".to_owned()]);
        write_varint_field(&mut raw, 4, 1);
        write_wrapped_varint(&mut raw, 8, 1);
        write_wrapped_varint(&mut raw, 11, u64::from(FRIEND_SOURCE_MUTUAL_GUILDS));
        write_wrapped_varint(&mut raw, 12, 5);
        write_packed_ids(&mut raw, 16, &["5000".to_owned()]);
        write_wrapped_varint(&mut raw, 17, 1);
        write_varint_field(&mut raw, 99, 77);

        // TextAndImagesSettings (outer field 6): use_thread_sidebar left alone, explicit
        // content 1 (UInt32Value, non-friends), dm_spam_filter_v2 3 (everyone).
        let mut text = Vec::new();
        write_wrapped_varint(&mut text, 3, 1);
        write_wrapped_varint(&mut text, 19, 1);
        write_varint_field(&mut text, 27, 3);

        let encoded = privacy_update_settings(&raw, Some(&text));
        let whole = decode_settings(&encoded).unwrap();
        assert_eq!(bytes_of(&whole, 8), Some(raw.as_slice()));
        assert_eq!(bytes_of(&whole, 6), Some(text.as_slice()));
        let extracted = privacy_settings(&encoded).unwrap();
        let mut decoded = decode_privacy(&extracted);
        let text_extracted = text_and_images_settings(&encoded).unwrap();
        apply_content_filters(&text_extracted, &mut decoded);
        assert!(!decoded.friend_requests_everyone);
        assert!(!decoded.friend_requests_mutual_friends);
        assert!(decoded.friend_requests_mutual_spaces);
        assert!(!decoded.allow_dms_from_spaces);
        assert!(!decoded.allow_message_requests);
        assert_eq!(decoded.explicit_content_filter, 1);
        assert_eq!(decoded.dm_spam_filter, 2);
        assert_eq!(decoded.restricted_spaces, vec!["4000".to_owned()]);
        assert_eq!(decoded.request_restricted_spaces, vec!["5000".to_owned()]);

        // Mutate and re-encode. detect_platform_accounts, friend discovery, the unknown
        // varint, and use_thread_sidebar stay byte-for-byte. Privacy is not written as field 6.
        let mut next = decoded.clone();
        next.friend_requests_everyone = true;
        next.allow_dms_from_spaces = true;
        next.explicit_content_filter = 2;
        next.dm_spam_filter = 1;
        next.restricted_spaces.push("6000".to_owned());
        let rewritten = with_privacy(&extracted, &next);
        let rewritten_text = with_content_filters(&text_extracted, &next);
        let mut again = decode_privacy(&rewritten);
        apply_content_filters(&rewritten_text, &mut again);
        assert_eq!(again, next);
        assert_eq!(bytes_of(&rewritten, 8), Some(&[0x08, 0x01][..]), "detect_platform_accounts must survive");
        assert_eq!(bytes_of(&rewritten, 12), Some(&[0x08, 0x05][..]), "friend_discovery_flags must survive");
        assert!(
            fields(&rewritten).iter().any(|(n, f, _)| *n == 99 && matches!(f, Field::Varint(77))),
            "unknown field 99 must survive"
        );
        assert_eq!(bytes_of(&rewritten_text, 3), Some(&[0x08, 0x01][..]), "use_thread_sidebar must survive");
        assert_eq!(bytes_of(&rewritten_text, 19), Some(&[0x08, 0x02][..]));
        assert!(!fields(&rewritten).iter().any(|(n, _, _)| matches!(*n, 26 | 27)));
        let sent = decode_settings(&privacy_update_settings(&rewritten, Some(&rewritten_text))).unwrap();
        assert_eq!(bytes_of(&sent, 8), Some(rewritten.as_slice()));
        assert_ne!(bytes_of(&sent, 6), Some(rewritten.as_slice()));
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
    fn custom_status_round_trips_emoji_expiry_and_unknown_fields() {
        // Seed StatusSettings with an unknown field 3 (varint 1) and CustomStatus with an unknown field 5 (varint 7).
        let mut inner = Vec::new();
        write(&mut inner, 1, b"old");
        inner.extend([0x28, 0x07]); // field 5, wire type 0, value 7
        let mut status = Vec::new();
        write(&mut status, 1, &[0x0a, 6, b'o', b'n', b'l', b'i', b'n', b'e']);
        write(&mut status, 2, &inner);
        status.extend([0x18, 0x01]); // field 3, wire type 0, value 1

        // 1. Unicode emoji + expiry
        let unicode = CustomStatus {
            text: "In a meeting".into(),
            emoji_id: None,
            emoji_name: "📅".into(),
            expires_at_ms: Some(1_900_000_000_000),
        };
        let encoded = with_custom_status(&status, "dnd", Some(&unicode));
        assert_eq!(own_custom_status(&encoded), ("dnd".into(), unicode.clone()));
        assert!(encoded.windows(2).any(|p| p == [0x18, 0x01]), "unknown StatusSettings field dropped");
        let custom_bytes = bytes_of(&encoded, 2).expect("custom_status bytes");
        assert!(custom_bytes.windows(2).any(|p| p == [0x28, 0x07]), "unknown CustomStatus field dropped");

        // 2. Custom server emoji (id + name), no text, no expiry
        let server_emoji = CustomStatus {
            text: String::new(),
            emoji_id: Some(7001),
            emoji_name: "ferris".into(),
            expires_at_ms: None,
        };
        let encoded2 = with_custom_status(&encoded, "idle", Some(&server_emoji));
        assert_eq!(own_custom_status(&encoded2), ("idle".into(), server_emoji));

        // 3. Clearing removes field 2 completely
        let cleared = with_custom_status(&encoded2, "online", Some(&CustomStatus::default()));
        assert_eq!(own_custom_status(&cleared), ("online".into(), CustomStatus::default()));
        assert_eq!(bytes_of(&cleared, 2), None);
    }

    #[test]
    fn an_overflowing_varint_is_rejected() {
        let overflow = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02];
        assert_eq!(Reader(&overflow).varint(), None);
        let top_bit = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01];
        assert_eq!(Reader(&top_bit).varint(), Some(1 << 63));
        // A length that does not fit in usize is not a field.
        let mut huge = vec![0x0au8]; // field 1, wire type 2
        huge.extend([0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]);
        assert!(Reader(&huge).field().is_none());
    }

    #[test]
    fn explicit_content_filter_blurs_by_default_and_respects_show_setting() {
        assert!(blur_explicit_media(""));
        assert!(blur_explicit_media("not base64!"));
        let encode_filter = |mode: u8| {
            let mut wrapped = Vec::new();
            wrapped.extend([0x08, mode]);
            let mut text_and_images = Vec::new();
            write(&mut text_and_images, 14, &wrapped);
            let mut root = Vec::new();
            write(&mut root, 6, &text_and_images);
            base64::engine::general_purpose::STANDARD.encode(root)
        };
        assert!(!blur_explicit_media(&encode_filter(0)));
        assert!(blur_explicit_media(&encode_filter(1)));
        assert!(blur_explicit_media(&encode_filter(2)));
    }

    #[test]
    fn guild_folders_round_trip_preserves_unknown_fields_and_other_settings_byte_for_byte() {
        // Build a PreloadedUserSettings blob containing:
        // - field 11 (status = "dnd")
        // - field 99 (unknown top-level varint = 42)
        // - field 14 (guild_folders with an unknown subfield 7 = 1 and initial folder)
        let mut whole = Vec::new();
        let status = with_status(&[], "dnd", Some("busy"));
        write(&mut whole, 11, &status);
        write_varint_field(&mut whole, 99, 42);
        let mut initial_f14 = Vec::new();
        write_varint_field(&mut initial_f14, 7, 1);
        let initial_f14 = with_guild_folders(
            &initial_f14,
            &[GuildFolder { id: None, name: String::new(), color: None, guild_ids: vec!["4000".into()] }],
        );
        write(&mut whole, 14, &initial_f14);

        let updated_folders = vec![
            GuildFolder {
                id: Some(77),
                name: "Favorites".into(),
                color: Some(0x5865F2),
                guild_ids: vec!["5000".into(), "4000".into()],
            },
            GuildFolder {
                id: None,
                name: String::new(),
                color: None,
                guild_ids: vec!["6000".into()],
            },
        ];

        // 1. Patch-only encoding round-trips folders, order, and keeps unknown field 7 inside field 14.
        let patch_block = with_guild_folders(&initial_f14, &updated_folders);
        let patch_b64 = settings_with_folders(&patch_block);
        assert_eq!(guild_folders(&patch_b64).unwrap(), updated_folders);
        assert_eq!(guild_order(&patch_b64).unwrap(), ["5000", "4000", "6000"]);
        let round_block = guild_folders_settings(&patch_b64).unwrap();
        assert!(fields(&round_block).iter().any(|(n, f, _)| *n == 7 && matches!(f, Field::Varint(1))));

        // 2. Whole-settings replacement leaves field 11 (status) and unknown field 99 byte-for-byte identical.
        let replaced = replace_guild_folders_in_whole(&whole, &updated_folders);
        assert_eq!(bytes_of(&replaced, 11), bytes_of(&whole, 11));
        assert!(fields(&replaced).iter().any(|(n, f, _)| *n == 99 && matches!(f, Field::Varint(42))));
        let replaced_b64 = base64::engine::general_purpose::STANDARD.encode(&replaced);
        assert_eq!(guild_folders(&replaced_b64).unwrap(), updated_folders);
    }

    fn loose(id: &str) -> GuildFolder {
        GuildFolder { id: None, name: String::new(), color: None, guild_ids: vec![id.into()] }
    }

    fn named(id: i64, name: &str, guilds: &[&str]) -> GuildFolder {
        GuildFolder {
            id: Some(id),
            name: name.into(),
            color: None,
            guild_ids: guilds.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    #[test]
    fn a_server_moves_before_another_into_a_folder_and_out_and_a_drop_makes_a_folder() {
        let list = vec![loose("30"), loose("10"), loose("20")];
        let before = move_guild(&list, "30", Some("20"), None, None, 1).unwrap();
        assert_eq!(before, vec![loose("10"), loose("30"), loose("20")]);
        let end = move_guild(&list, "30", None, None, None, 1).unwrap();
        assert_eq!(end, vec![loose("10"), loose("20"), loose("30")]);
        assert_eq!(move_guild(&list, "30", Some("30"), None, None, 1).unwrap(), list);

        let made = move_guild(&list, "30", None, None, Some("10"), 77).unwrap();
        assert_eq!(made, vec![named(77, "", &["10", "30"]), loose("20")]);
        let flipped = move_guild(&list, "30", Some("10"), None, Some("10"), 77).unwrap();
        assert_eq!(flipped[0].guild_ids, ["30", "10"]);

        let joined = move_guild(&made, "20", Some("30"), Some("77"), None, 1).unwrap();
        assert_eq!(joined, vec![named(77, "", &["10", "20", "30"])]);
        let pair = vec![named(77, "Games", &["10", "20"]), loose("30")];
        let left = move_guild(&pair, "10", Some("30"), None, None, 1).unwrap();
        assert_eq!(left, vec![loose("20"), loose("10"), loose("30")], "one server left is not a folder");

        assert_eq!(move_guild(&list, "30", None, None, Some("30"), 1), Err("A server cannot be dropped onto itself."));
        assert_eq!(
            move_guild(&list, "30", None, Some("77"), Some("10"), 1),
            Err("Move a server into a folder or onto another server, not both.")
        );
        assert_eq!(move_guild(&made, "20", None, Some("nope"), None, 1), Err("That folder is not on this account."));
    }

    #[test]
    fn a_folder_can_be_renamed_and_recoloured_and_the_moved_list_keeps_unknown_bytes() {
        let mut folders = vec![named(77, "Games", &["30", "10"])];
        edit_guild_folder(&mut folders, 77, Some("  Favourites  "), Some(Some(0x15865F2))).unwrap();
        assert_eq!(folders[0].name, "Favourites");
        assert_eq!(folders[0].color, Some(0x5865F2));
        edit_guild_folder(&mut folders, 77, Some(""), Some(None)).unwrap();
        assert_eq!(folders[0].name, "");
        assert_eq!(folders[0].color, None);
        assert_eq!(edit_guild_folder(&mut folders, 3, Some("x"), None), Err("That folder is not on this account."));

        let mut raw = Vec::new();
        write_varint_field(&mut raw, 7, 1);
        let start = vec![loose("30"), loose("10"), loose("20")];
        let raw = with_guild_folders(&raw, &start);
        let moved = move_guild(&start, "30", None, None, Some("10"), 77).unwrap();
        let encoded = settings_with_folders(&with_guild_folders(&raw, &moved));
        assert_eq!(guild_folders(&encoded).unwrap(), moved);
        let block = guild_folders_settings(&encoded).unwrap();
        assert!(fields(&block).iter().any(|(n, f, _)| *n == 7 && matches!(f, Field::Varint(1))));
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
