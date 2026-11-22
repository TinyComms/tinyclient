//! What is read from Discord's protobuf settings, and what is written back: the server list
//! and its folders, and our own status.
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

fn write_varint_field(into: &mut Vec<u8>, number: u64, value: u64) {
    write_varint(into, number << 3);
    write_varint(into, value);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
