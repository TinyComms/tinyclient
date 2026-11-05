//! The history archive one device of a person hands another: what the old device's cache opened,
//! and the safety numbers the person verified, sealed under a fresh key that only the link channel
//! carries ([`crate::link::ArchiveRef`]). The server relays the blob and cannot read it.
//!
//! ```text
//! "TNYH1" || nonce prefix (7 random bytes) || chunk_0 || ... || chunk_n
//! ```
//!
//! The chunks are STREAM over AES-256-GCM (`aead::stream`, `EncryptorBE32`), as in
//! [`crate::attachment`]: 64 KiB of plaintext each, the last 0 to 64 KiB and flagged as the last,
//! each 16 bytes longer (the tag). Every chunk's associated data is
//! `"tiny-archive-v1" || link_id || header`, so a blob made for one link does not open for another.
//! A chunk moved, repeated or dropped fails on its counter, a blob cut short on the missing last
//! flag, a changed bit or the wrong key on the tag.
//!
//! The plaintext is a stream of records, each a 4 byte big-endian length and that much JSON:
//!
//! * `{"r":"meta","v":1,"user":...,"from_device":...,"created":...,"scope":...}` first,
//! * `{"r":"msg","conv":...,"id":...,"opened":...}` per cached message (`opened` as the cache keeps
//!   it, opaque here),
//! * `{"r":"verified","user":...,"number":...}` per verified safety number,
//! * `{"r":"end","msgs":N}` last, with the number of `msg` records. Nothing may follow it, and a
//!   stream without it is refused.
//!
//! Both directions stream: memory is two chunks and one record, whatever the size. [`read`] hands
//! records out as their chunks authenticate, before the end is seen, so [`merge`] reads the blob
//! twice: once to know the whole of it is sound, then to merge.

use crate::error::{Error, Result};
use crate::link::Scope;
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::stream::{DecryptorBE32, EncryptorBE32};
use aes_gcm::aead::{KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const MAGIC: &[u8; 5] = b"TNYH1";
const NONCE_PREFIX_LEN: usize = 7;
pub const HEADER_LEN: usize = MAGIC.len() + NONCE_PREFIX_LEN;
pub const CHUNK: usize = 64 * 1024;
const TAG_LEN: usize = 16;
const LABEL: &[u8] = b"tiny-archive-v1";
/// The longest record: a message's text and its attachment references are far smaller.
pub const MAX_RECORD: usize = 1024 * 1024;
pub const VERSION: u32 = 1;

/// The archive's key: 256 random bits per archive, zeroed when dropped, never printed.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ArchiveKey([u8; 32]);

impl ArchiveKey {
    pub fn generate() -> Result<ArchiveKey> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Random)?;
        Ok(ArchiveKey(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> ArchiveKey {
        ArchiveKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for ArchiveKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArchiveKey(..)")
    }
}

/// The first record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub v: u32,
    pub user: String,
    pub from_device: String,
    pub created: String,
    pub scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "r", rename_all = "snake_case")]
pub enum Record {
    Meta(Meta),
    Msg {
        conv: String,
        id: String,
        opened: Value,
    },
    Verified {
        user: String,
        number: String,
    },
    End {
        msgs: u64,
    },
}

/// What a whole archive came to: `bytes`, `sha256` and `chunks` go in the `approve` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub bytes: u64,
    pub sha256: [u8; 32],
    pub chunks: u64,
    pub msgs: u64,
}

fn aad(link_id: &[u8], header: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(LABEL.len() + link_id.len() + header.len());
    aad.extend_from_slice(LABEL);
    aad.extend_from_slice(link_id);
    aad.extend_from_slice(header);
    aad
}

fn cipher(key: &ArchiveKey) -> Aes256Gcm {
    Aes256Gcm::new(GenericArray::from_slice(key.as_bytes()))
}

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|err| Error::Io(err.to_string()))
}

/// Writes an archive as records are given to it.
pub struct Writer<W: Write> {
    out: W,
    encryptor: Option<EncryptorBE32<Aes256Gcm>>,
    aad: Vec<u8>,
    /// Plaintext not sealed yet: always under two chunks.
    pending: Zeroizing<Vec<u8>>,
    hasher: Sha256,
    bytes: u64,
    chunks: u64,
    msgs: u64,
}

impl<W: Write> Writer<W> {
    /// The header and the meta record, under a fresh nonce prefix.
    pub fn new(key: &ArchiveKey, link_id: &[u8], meta: &Meta, out: W) -> Result<Writer<W>> {
        let mut prefix = [0u8; NONCE_PREFIX_LEN];
        getrandom::fill(&mut prefix).map_err(|_| Error::Random)?;
        Self::with_prefix(key, link_id, meta, prefix, out)
    }

    /// [`Writer::new`] with a given nonce prefix, for tests. A prefix must never be used twice with
    /// one key.
    pub fn with_prefix(
        key: &ArchiveKey,
        link_id: &[u8],
        meta: &Meta,
        prefix: [u8; NONCE_PREFIX_LEN],
        mut out: W,
    ) -> Result<Writer<W>> {
        let mut header = [0u8; HEADER_LEN];
        header[..MAGIC.len()].copy_from_slice(MAGIC);
        header[MAGIC.len()..].copy_from_slice(&prefix);
        io(out.write_all(&header))?;
        let mut hasher = Sha256::new();
        hasher.update(header);
        let mut writer = Writer {
            out,
            encryptor: Some(EncryptorBE32::from_aead(
                cipher(key),
                GenericArray::from_slice(&prefix),
            )),
            aad: aad(link_id, &header),
            pending: Zeroizing::new(Vec::with_capacity(2 * CHUNK)),
            hasher,
            bytes: HEADER_LEN as u64,
            chunks: 0,
            msgs: 0,
        };
        writer.record(&Record::Meta(meta.clone()))?;
        Ok(writer)
    }

    fn emit(&mut self, sealed: &[u8]) -> Result<()> {
        io(self.out.write_all(sealed))?;
        self.hasher.update(sealed);
        self.bytes += sealed.len() as u64;
        self.chunks += 1;
        Ok(())
    }

    fn record(&mut self, record: &Record) -> Result<()> {
        let json = Zeroizing::new(
            serde_json::to_vec(record).map_err(|_| Error::Malformed("archive record"))?,
        );
        if json.len() > MAX_RECORD {
            return Err(Error::TooLarge("archive record"));
        }
        self.pending
            .extend_from_slice(&(json.len() as u32).to_be_bytes());
        self.pending.extend_from_slice(&json);
        // A full chunk is sealed only once more follows: the last chunk is flagged as the last.
        while self.pending.len() > CHUNK {
            let rest = Zeroizing::new(self.pending.split_off(CHUNK));
            let encryptor = self.encryptor.as_mut().ok_or(Error::Malformed("archive"))?;
            let sealed = encryptor
                .encrypt_next(Payload {
                    msg: &self.pending,
                    aad: &self.aad,
                })
                .map_err(|_| Error::Authentication)?;
            self.emit(&sealed)?;
            self.pending.zeroize();
            self.pending.extend_from_slice(&rest);
        }
        Ok(())
    }

    pub fn message(&mut self, conv: &str, id: &str, opened: &Value) -> Result<()> {
        self.record(&Record::Msg {
            conv: conv.to_owned(),
            id: id.to_owned(),
            opened: opened.clone(),
        })?;
        self.msgs += 1;
        Ok(())
    }

    pub fn verified(&mut self, user: &str, number: &str) -> Result<()> {
        self.record(&Record::Verified {
            user: user.to_owned(),
            number: number.to_owned(),
        })
    }

    /// The end record and the last chunk.
    pub fn finish(mut self) -> Result<(W, Summary)> {
        self.record(&Record::End { msgs: self.msgs })?;
        let encryptor = self.encryptor.take().ok_or(Error::Malformed("archive"))?;
        let sealed = encryptor
            .encrypt_last(Payload {
                msg: &self.pending,
                aad: &self.aad,
            })
            .map_err(|_| Error::Authentication)?;
        self.emit(&sealed)?;
        io(self.out.flush())?;
        let summary = Summary {
            bytes: self.bytes,
            sha256: self.hasher.clone().finalize().into(),
            chunks: self.chunks,
            msgs: self.msgs,
        };
        Ok((self.out, summary))
    }
}

/// Reads until `buffer` holds `want` bytes or the reader ends.
fn fill<R: Read>(reader: &mut R, buffer: &mut Vec<u8>, want: usize) -> Result<()> {
    buffer.clear();
    buffer.resize(want, 0);
    let mut filled = 0;
    while filled < want {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(Error::Io(err.to_string())),
        }
    }
    buffer.truncate(filled);
    Ok(())
}

/// Splits the opened plaintext into records and checks their order.
struct Records {
    buffer: Zeroizing<Vec<u8>>,
    seen_meta: bool,
    ended: bool,
    msgs: u64,
}

impl Records {
    fn feed(&mut self, plain: &[u8], visit: &mut dyn FnMut(Record) -> Result<()>) -> Result<()> {
        self.buffer.extend_from_slice(plain);
        let mut at = 0;
        loop {
            let rest = &self.buffer[at..];
            if rest.len() < 4 {
                break;
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            if len > MAX_RECORD {
                return Err(Error::TooLarge("archive record"));
            }
            if rest.len() < 4 + len {
                break;
            }
            let record: Record = serde_json::from_slice(&self.buffer[at + 4..at + 4 + len])
                .map_err(|_| Error::Malformed("archive record"))?;
            at += 4 + len;
            self.check(&record)?;
            visit(record)?;
        }
        let rest = Zeroizing::new(self.buffer[at..].to_vec());
        self.buffer.zeroize();
        self.buffer.extend_from_slice(&rest);
        Ok(())
    }

    fn check(&mut self, record: &Record) -> Result<()> {
        if self.ended {
            return Err(Error::Malformed("archive: a record after the end"));
        }
        match record {
            Record::Meta(meta) => {
                if self.seen_meta {
                    return Err(Error::Malformed("archive: a second meta record"));
                }
                if meta.v != VERSION {
                    return Err(Error::UnsupportedVersion);
                }
                self.seen_meta = true;
            }
            _ if !self.seen_meta => return Err(Error::Malformed("archive: no meta record first")),
            Record::Msg { .. } => self.msgs += 1,
            Record::Verified { .. } => {}
            Record::End { msgs } => {
                if *msgs != self.msgs {
                    return Err(Error::Malformed("archive: the count does not match"));
                }
                self.ended = true;
            }
        }
        Ok(())
    }
}

/// Opens an archive and hands each record to `visit` in order, as its chunk authenticates. `Ok`
/// only if the whole blob authenticated and its records were whole and in order, ending in `end`;
/// a caller that gets an error must undo what it did with the records (see [`merge`]).
pub fn read<R: Read>(
    key: &ArchiveKey,
    link_id: &[u8],
    mut input: R,
    mut visit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    let mut header = Vec::new();
    fill(&mut input, &mut header, HEADER_LEN)?;
    if header.len() != HEADER_LEN || &header[..MAGIC.len()] != MAGIC {
        return Err(Error::Malformed("archive header"));
    }
    let mut hasher = Sha256::new();
    hasher.update(&header);
    let aad = aad(link_id, &header);
    let mut decryptor = Some(DecryptorBE32::from_aead(
        cipher(key),
        GenericArray::from_slice(&header[MAGIC.len()..]),
    ));
    let mut records = Records {
        buffer: Zeroizing::new(Vec::new()),
        seen_meta: false,
        ended: false,
        msgs: 0,
    };
    let mut bytes = HEADER_LEN as u64;
    let mut chunks = 0u64;
    let mut current = Vec::with_capacity(CHUNK + TAG_LEN);
    let mut next = Vec::with_capacity(CHUNK + TAG_LEN);
    fill(&mut input, &mut current, CHUNK + TAG_LEN)?;
    loop {
        if current.len() < TAG_LEN {
            return Err(Error::Malformed("archive: cut short"));
        }
        fill(&mut input, &mut next, CHUNK + TAG_LEN)?;
        hasher.update(&current);
        bytes += current.len() as u64;
        chunks += 1;
        let last = next.is_empty();
        let payload = Payload {
            msg: &current,
            aad: &aad,
        };
        let plain = Zeroizing::new(
            if last {
                decryptor
                    .take()
                    .ok_or(Error::Malformed("archive"))?
                    .decrypt_last(payload)
            } else {
                if current.len() != CHUNK + TAG_LEN {
                    return Err(Error::Malformed("archive: a short chunk before the end"));
                }
                decryptor
                    .as_mut()
                    .ok_or(Error::Malformed("archive"))?
                    .decrypt_next(payload)
            }
            .map_err(|_| Error::Authentication)?,
        );
        records.feed(&plain, &mut visit)?;
        if last {
            break;
        }
        std::mem::swap(&mut current, &mut next);
    }
    if !records.ended || !records.buffer.is_empty() {
        return Err(Error::Malformed("archive: no end record"));
    }
    Ok(Summary {
        bytes,
        sha256: hasher.finalize().into(),
        chunks,
        msgs: records.msgs,
    })
}

/// Where an archive is merged: the new device's sealed cache, as the caller keeps it.
pub trait Target {
    /// The archive checked out and is about to be merged: its first record.
    fn start(&mut self, _meta: &Meta) {}
    /// Whether the person is in this conversation. Records for any other are dropped.
    fn is_member(&mut self, conv: &str) -> bool;
    /// Whether this device already has the message: it is never overwritten.
    fn has(&mut self, conv: &str, id: &str) -> bool;
    fn put(&mut self, conv: &str, id: &str, opened: Value) -> Result<()>;
    /// A safety number the person verified on the other device.
    fn verified(&mut self, user: &str, number: &str) -> Result<()>;
}

/// What a merge did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Merged {
    pub added: u64,
    /// Already on this device (or twice in the archive): left as they were.
    pub kept: u64,
    /// For conversations the person is not in.
    pub refused: u64,
    pub verified: u64,
}

/// Merges an archive for `user` into `target`: first a full read that only checks (the whole blob
/// authenticates, its records are in order, it is for this person), then a second that merges.
/// `open` gives the blob from its start each time. Idempotent: a second merge adds nothing.
pub fn merge<R: Read>(
    key: &ArchiveKey,
    link_id: &[u8],
    user: &str,
    mut open: impl FnMut() -> Result<R>,
    target: &mut dyn Target,
) -> Result<(Meta, Merged)> {
    let mut meta = None;
    read(key, link_id, open()?, |record| {
        if let Record::Meta(found) = record {
            if found.user != user {
                return Err(Error::Malformed("archive: another person's"));
            }
            meta = Some(found);
        }
        Ok(())
    })?;
    let meta = meta.ok_or(Error::Malformed("archive: no meta record first"))?;
    target.start(&meta);
    let mut merged = Merged::default();
    read(key, link_id, open()?, |record| {
        match record {
            Record::Meta(again) if again != meta => {
                return Err(Error::Malformed("archive: changed between reads"))
            }
            Record::Msg { conv, id, opened } => {
                if !target.is_member(&conv) {
                    merged.refused += 1;
                } else if target.has(&conv, &id) {
                    merged.kept += 1;
                } else {
                    target.put(&conv, &id, opened)?;
                    merged.added += 1;
                }
            }
            Record::Verified { user, number } => {
                target.verified(&user, &number)?;
                merged.verified += 1;
            }
            Record::Meta(_) | Record::End { .. } => {}
        }
        Ok(())
    })?;
    Ok((meta, merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        Meta {
            v: VERSION,
            user: "u".into(),
            from_device: "d".into(),
            created: "2026-11-05T00:00:00Z".into(),
            scope: Scope::All,
        }
    }

    #[test]
    fn an_empty_archive_is_one_chunk_and_reads_back() {
        let key = ArchiveKey::generate().unwrap();
        let writer = Writer::new(&key, b"link", &meta(), Vec::new()).unwrap();
        let (blob, summary) = writer.finish().unwrap();
        assert_eq!(summary.chunks, 1);
        assert_eq!(summary.bytes, blob.len() as u64);
        let mut seen = Vec::new();
        let read_back = read(&key, b"link", blob.as_slice(), |record| {
            seen.push(record);
            Ok(())
        })
        .unwrap();
        assert_eq!(read_back, summary);
        assert_eq!(seen, vec![Record::Meta(meta()), Record::End { msgs: 0 }]);
    }
}
