//! The two things read from Discord's protobuf settings: the order of the server list, and our
//! own status. The status is also the one thing written.
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
            0 => {
                self.varint()?;
                Field::Other
            }
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

/// A length-delimited field: its tag, its length, its bytes.
fn write(into: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    let mut varint = |mut value: u64| loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            into.push(byte);
            break;
        }
        into.push(byte | 0x80);
    };
    varint(number << 3 | 2);
    varint(bytes.len() as u64);
    into.extend_from_slice(bytes);
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
}
