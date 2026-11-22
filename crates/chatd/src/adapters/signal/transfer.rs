//! Signal linked-device message history transfer (`#724`).
//!
//! When a new device links to a primary Signal phone with `capabilities=transfer`, the primary
//! phone sends a 32-byte `ephemeralBackupKey` in `ProvisionMessage` (field 14), encrypts a
//! Signal Message Backup archive (`backup.proto`), and uploads it to Signal's CDN.
//!
//! This module implements the streaming decryption, RFC 1952 gzip decompression, varint-delimited
//! `signal.backup` protobuf frame parsing, and idempotent import into `chatd`'s SQLite store.
//! Everything streams in bounded memory buffers so large archives are never held in RAM.

use super::files;
use super::style::{self, Mark, Range};
use super::wire::{self, Peer, Pointer};
use crate::hub::Hub;
use crate::store::{Filing, FolderRow, PersonDetails, PersonRow, Stored};
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZFlush, MZStatus};
use protocol::{
    AccountKey, Attachment, Change, Key, Mark as PersonMark, MediaFile, MediaKind, Message,
    MessageKind, Poll, PollBallot, Preview, Reaction, Relation, ReplyTo, SharedContact, Span,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Magic header prefix for modern forward-secrecy Signal backup files (`SBACKUP\x01`).
pub const MAGIC_NUMBER: &[u8; 8] = b"SBACKUP\x01";
const AES_IV_LEN: usize = 16;
const HMAC_LEN: usize = 32;
/// Maximum single protobuf frame size accepted from a transfer archive (16 MiB).
const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;
/// Streaming I/O chunk size (64 KiB, multiple of 16-byte AES block size).
const CHUNK_SIZE: usize = 64 * 1024;
/// Maximum total encrypted archive size accepted (4 GiB safety cap).
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Derived 32-byte HMAC-SHA256 key and 32-byte AES-256-CBC key for a Signal message backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBackupKey {
    pub hmac_key: [u8; 32],
    pub aes_key: [u8; 32],
}

/// Derives the 16-byte Signal `BackupId` from a 32-byte backup key (or ephemeral transfer key)
/// and the account's ACI UUID (`libsignal-account-keys/src/backup.rs`).
pub fn derive_backup_id(backup_key: &[u8; 32], aci: &str) -> Result<[u8; 16], String> {
    let aci_bytes = wire::aci_uuid_bytes(aci).ok_or_else(|| "Invalid Signal account id.".to_owned())?;
    let mut id = [0u8; 16];
    Hkdf::<Sha256>::new(None, backup_key)
        .expand_multi_info(&[b"20241024_SIGNAL_BACKUP_ID:", &aci_bytes], &mut id)
        .map_err(|_| "Could not derive Signal backup id.".to_owned())?;
    Ok(id)
}

/// Derives `(hmac_key, aes_key)` for a Signal message backup archive
/// (`libsignal/rust/message-backup/src/key.rs`).
pub fn derive_message_backup_key(
    backup_key: &[u8; 32],
    backup_id: &[u8; 16],
    forward_secrecy_token: Option<&[u8; 32]>,
) -> MessageBackupKey {
    let mut out = [0u8; 64];
    let (salt, dst): (Option<&[u8]>, &[u8]) = match forward_secrecy_token {
        Some(token) => (Some(token.as_slice()), b"20250708_SIGNAL_BACKUP_ENCRYPT_MESSAGE_BACKUP:"),
        None => (None, b"20241007_SIGNAL_BACKUP_ENCRYPT_MESSAGE_BACKUP:"),
    };
    Hkdf::<Sha256>::new(salt, backup_key)
        .expand_multi_info(&[dst, backup_id], &mut out)
        .expect("64-byte HKDF output");
    let mut hmac_key = [0u8; 32];
    let mut aes_key = [0u8; 32];
    hmac_key.copy_from_slice(&out[..32]);
    aes_key.copy_from_slice(&out[32..64]);
    MessageBackupKey { hmac_key, aes_key }
}

/// Derives the 32-byte Signal Groups v2 public identifier (hex) from a 32-byte `GroupMasterKey`.
/// Matches `zkgroup::GroupSecretParams::derive_from_master_key(master_key).get_group_identifier()`
/// bit-for-bit using Signal's `ShoHmacSha256` construction, without requiring `feature = "signal"`.
pub fn derive_group_id(master_key: &[u8; 32]) -> String {
    const LABEL: &[u8] = b"Signal_ZKGroup_20200424_GroupMasterKey_GroupSecretParams_DeriveFromMasterKey";
    let mut mac0 = <Hmac<Sha256> as Mac>::new_from_slice(&[0u8; 32]).expect("32-byte key");
    mac0.update(LABEL);
    mac0.update(&[0x00]);
    let cv0: [u8; 32] = mac0.finalize().into_bytes().into();

    let mut mac1 = <Hmac<Sha256> as Mac>::new_from_slice(&cv0).expect("32-byte key");
    mac1.update(master_key);
    mac1.update(&[0x00]);
    let cv1: [u8; 32] = mac1.finalize().into_bytes().into();

    let mut mac2 = <Hmac<Sha256> as Mac>::new_from_slice(&cv1).expect("32-byte key");
    mac2.update(&0u64.to_be_bytes());
    mac2.update(&[0x01]);
    let gid: [u8; 32] = mac2.finalize().into_bytes().into();
    hex_lower(&gid)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn uuid_bytes_to_aci(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 {
        return None;
    }
    let s = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    );
    wire::is_aci(&s).then_some(s)
}

/// Summary returned after streaming and importing a Signal history transfer archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub chats: u32,
    pub messages: u32,
    pub new_messages: u32,
    pub skipped_expired: u32,
    pub attachments: u32,
    pub skipped_by_user: bool,
}

/// Persisted attachment pointer record stored at `{support}/signal/{aci}.attachments` so imported
/// and live Signal attachments can be lazily downloaded on demand (`Command::Wanted`) across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeptAttachment {
    pub id: String,
    pub pointer: Pointer,
    pub file: MediaFile,
    pub address: String,
    pub conversation: String,
    pub message_id: String,
}

pub fn attachments_path(aci: &str) -> std::path::PathBuf {
    protocol::support_dir().join("signal").join(format!("{aci}.attachments"))
}

pub fn read_kept_attachments(path: &Path) -> Vec<KeptAttachment> {
    const LIMIT: u64 = 8 * 1024 * 1024;
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > LIMIT {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn write_kept_attachments(path: &Path, rows: &[KeptAttachment]) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(bytes) = serde_json::to_vec(rows) else {
        return;
    };
    let tmp = path.with_extension("attachments.tmp");
    if std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ---------------------------------------------------------------------------
// Streaming Archive Verification, AES-256-CBC Decryption & RFC 1952 Gunzip
// ---------------------------------------------------------------------------

/// Verifies the archive's optional `SBACKUP\x01` metadata framing and trailing 32-byte
/// HMAC-SHA256 in a single streaming pass over `path` (64 KiB buffer).
/// Returns `(encrypted_start_offset, ciphertext_body_len)` excluding IV and trailing MAC.
fn verify_archive_file(path: &Path, hmac_key: &[u8; 32]) -> Result<(u64, u64), String> {
    let meta = std::fs::metadata(path).map_err(|err| format!("Could not read transfer archive: {err}"))?;
    let file_len = meta.len();
    if file_len > MAX_ARCHIVE_BYTES {
        return Err("Transfer archive exceeds size limit.".to_owned());
    }
    let mut file = File::open(path).map_err(|err| format!("Could not open transfer archive: {err}"))?;
    let mut magic = [0u8; 8];
    let enc_start = if file_len >= 8 {
        file.read_exact(&mut magic).map_err(|err| err.to_string())?;
        if &magic == MAGIC_NUMBER {
            let meta_len = read_varint32_from_reader(&mut file)?
                .ok_or_else(|| "Transfer archive metadata is truncated.".to_owned())?;
            let meta_len = u64::from(meta_len);
            if meta_len > 1024 * 1024 {
                return Err("Transfer archive metadata is oversized.".to_owned());
            }
            let pos = file
                .seek(SeekFrom::Current(meta_len as i64))
                .map_err(|err| err.to_string())?;
            if pos > file_len {
                return Err("Transfer archive metadata is truncated.".to_owned());
            }
            pos
        } else {
            file.seek(SeekFrom::Start(0)).map_err(|err| err.to_string())?;
            0
        }
    } else {
        return Err("Transfer archive is truncated.".to_owned());
    };

    let enc_total = file_len
        .checked_sub(enc_start)
        .ok_or_else(|| "Transfer archive is truncated.".to_owned())?;
    if enc_total < (AES_IV_LEN + 16 + HMAC_LEN) as u64 {
        return Err("Transfer archive is truncated.".to_owned());
    }
    let mac_covered = enc_total - HMAC_LEN as u64;
    let body_len = mac_covered - AES_IV_LEN as u64;
    if !body_len.is_multiple_of(16) {
        return Err("Transfer archive ciphertext is not block-aligned.".to_owned());
    }

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(hmac_key)
        .map_err(|_| "Invalid backup HMAC key.".to_owned())?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut left = mac_covered;
    while left > 0 {
        let want = (left as usize).min(buf.len());
        file.read_exact(&mut buf[..want])
            .map_err(|err| format!("Transfer archive read error: {err}"))?;
        mac.update(&buf[..want]);
        left -= want as u64;
    }
    let mut expected_mac = [0u8; HMAC_LEN];
    file.read_exact(&mut expected_mac)
        .map_err(|err| format!("Transfer archive MAC read error: {err}"))?;
    if mac.verify_slice(&expected_mac).is_err() {
        return Err("Transfer archive authentication failed (invalid HMAC).".to_owned());
    }
    Ok((enc_start, body_len))
}

/// Streaming AES-256-CBC-PKCS7 decrypting `Read` over an already HMAC-verified file slice.
struct AesCbcFileReader {
    file: File,
    aes: Aes256,
    prev: [u8; 16],
    remaining: u64,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
}

impl AesCbcFileReader {
    fn open(path: &Path, enc_start: u64, body_len: u64, aes_key: &[u8; 32]) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|err| err.to_string())?;
        file.seek(SeekFrom::Start(enc_start)).map_err(|err| err.to_string())?;
        let mut iv = [0u8; AES_IV_LEN];
        file.read_exact(&mut iv).map_err(|err| err.to_string())?;
        let aes = Aes256::new_from_slice(aes_key).map_err(|_| "Invalid backup AES key.".to_owned())?;
        Ok(Self {
            file,
            aes,
            prev: iv,
            remaining: body_len,
            buf: vec![0u8; CHUNK_SIZE],
            pos: 0,
            len: 0,
        })
    }
}

impl Read for AesCbcFileReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len {
            if self.remaining == 0 {
                return Ok(0);
            }
            let want = (self.remaining as usize).min(self.buf.len());
            let want = (want / 16) * 16;
            self.file.read_exact(&mut self.buf[..want])?;
            self.remaining -= want as u64;
            for block in self.buf[..want].chunks_exact_mut(16) {
                let ct: [u8; 16] = block.try_into().expect("16 bytes");
                self.aes.decrypt_block(block.into());
                for i in 0..16 {
                    block[i] ^= self.prev[i];
                }
                self.prev = ct;
            }
            self.pos = 0;
            self.len = if self.remaining == 0 {
                let pad = self.buf[want - 1] as usize;
                if pad == 0
                    || pad > 16
                    || !self.buf[want - pad..want].iter().all(|&b| b as usize == pad)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Invalid PKCS#7 padding in transfer archive",
                    ));
                }
                want - pad
            } else {
                want
            };
            if self.len == 0 {
                return Ok(0);
            }
        }
        let avail = self.len - self.pos;
        let take = avail.min(out.len());
        out[..take].copy_from_slice(&self.buf[self.pos..self.pos + take]);
        self.pos += take;
        Ok(take)
    }
}

/// IEEE 802.3 CRC-32 used by RFC 1952 gzip headers and trailers.
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xffff_ffff)
    }

    fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        self.0 = crc;
    }

    fn finalize(&self) -> u32 {
        !self.0
    }
}

/// Streaming RFC 1952 gzip decompressor built on `miniz_oxide`'s streaming raw deflate state.
/// Verifies CRC32 and ISIZE against the 8-byte trailer and cleanly stops after the member so
/// trailing zero padding in Signal backup archives is ignored.
struct GzipStreamReader<R: Read> {
    inner: R,
    state: Box<InflateState>,
    in_buf: Vec<u8>,
    in_pos: usize,
    in_len: usize,
    out_buf: Vec<u8>,
    out_pos: usize,
    out_len: usize,
    crc: Crc32,
    isize_mod: u32,
    eof: bool,
    header_done: bool,
}

impl<R: Read> GzipStreamReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            state: InflateState::new_boxed(DataFormat::Raw),
            in_buf: vec![0u8; 32 * 1024],
            in_pos: 0,
            in_len: 0,
            out_buf: vec![0u8; 32 * 1024],
            out_pos: 0,
            out_len: 0,
            crc: Crc32::new(),
            isize_mod: 0,
            eof: false,
            header_done: false,
        }
    }

    fn read_byte(&mut self) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact_raw(&mut b)?;
        Ok(b[0])
    }

    fn read_exact_raw(&mut self, mut dst: &mut [u8]) -> std::io::Result<()> {
        while !dst.is_empty() {
            if self.in_pos < self.in_len {
                let n = (self.in_len - self.in_pos).min(dst.len());
                dst[..n].copy_from_slice(&self.in_buf[self.in_pos..self.in_pos + n]);
                self.in_pos += n;
                dst = &mut dst[n..];
            } else {
                let n = self.inner.read(&mut self.in_buf)?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Truncated gzip stream",
                    ));
                }
                self.in_pos = 0;
                self.in_len = n;
            }
        }
        Ok(())
    }

    fn parse_header(&mut self) -> std::io::Result<()> {
        let mut hdr = [0u8; 10];
        self.read_exact_raw(&mut hdr)?;
        if hdr[0] != 0x1f || hdr[1] != 0x8b || hdr[2] != 8 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Transfer archive is not a valid gzip stream",
            ));
        }
        let flg = hdr[3];
        if flg & 0x04 != 0 {
            let mut xlen = [0u8; 2];
            self.read_exact_raw(&mut xlen)?;
            let skip = u16::from_le_bytes(xlen) as usize;
            let mut tmp = vec![0u8; skip];
            self.read_exact_raw(&mut tmp)?;
        }
        if flg & 0x08 != 0 {
            while self.read_byte()? != 0 {}
        }
        if flg & 0x10 != 0 {
            while self.read_byte()? != 0 {}
        }
        if flg & 0x02 != 0 {
            let mut crc16 = [0u8; 2];
            self.read_exact_raw(&mut crc16)?;
        }
        self.header_done = true;
        Ok(())
    }
}

impl<R: Read> Read for GzipStreamReader<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if !self.header_done {
            self.parse_header()?;
        }
        loop {
            if self.out_pos < self.out_len {
                let take = (self.out_len - self.out_pos).min(out.len());
                out[..take].copy_from_slice(&self.out_buf[self.out_pos..self.out_pos + take]);
                self.out_pos += take;
                return Ok(take);
            }
            if self.eof {
                return Ok(0);
            }
            if self.in_pos >= self.in_len {
                let n = self.inner.read(&mut self.in_buf)?;
                self.in_pos = 0;
                self.in_len = n;
            }
            let res = inflate(
                &mut self.state,
                &self.in_buf[self.in_pos..self.in_len],
                &mut self.out_buf,
                MZFlush::None,
            );
            self.in_pos += res.bytes_consumed;
            if res.bytes_written > 0 {
                self.crc.update(&self.out_buf[..res.bytes_written]);
                self.isize_mod = self.isize_mod.wrapping_add(res.bytes_written as u32);
                self.out_pos = 0;
                self.out_len = res.bytes_written;
            }
            match res.status {
                Ok(MZStatus::Ok) => {
                    if self.out_pos < self.out_len {
                        continue;
                    }
                    if res.bytes_consumed == 0 && self.in_len == 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "Truncated gzip deflate stream",
                        ));
                    }
                }
                Ok(MZStatus::StreamEnd) => {
                    let mut trailer = [0u8; 8];
                    self.read_exact_raw(&mut trailer)?;
                    let expected_crc = u32::from_le_bytes(trailer[0..4].try_into().expect("4 bytes"));
                    let expected_isize = u32::from_le_bytes(trailer[4..8].try_into().expect("4 bytes"));
                    if self.crc.finalize() != expected_crc || self.isize_mod != expected_isize {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Transfer archive gzip checksum mismatch",
                        ));
                    }
                    self.eof = true;
                    if self.out_pos < self.out_len {
                        continue;
                    }
                    return Ok(0);
                }
                Ok(MZStatus::NeedDict) | Err(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Corrupt gzip stream in transfer archive",
                    ));
                }
            }
        }
    }
}

/// Compresses `raw` into an RFC 1952 gzip member (used by archive encoding helpers in tests).
fn gzip_compress(raw: &[u8]) -> Vec<u8> {
    let deflated = miniz_oxide::deflate::compress_to_vec(raw, 6);
    let mut crc = Crc32::new();
    crc.update(raw);
    let mut out = Vec::with_capacity(10 + deflated.len() + 8);
    out.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255]);
    out.extend_from_slice(&deflated);
    out.extend_from_slice(&crc.finalize().to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out
}

/// Encodes varint-delimited `backup_info` + `frames` into a spec-compliant encrypted Signal
/// message backup archive (`IV || AES-256-CBC-PKCS7(gzip(stream) || padding) || HMAC-SHA256`).
pub fn encode_encrypted_archive(
    ephemeral_key: &[u8; 32],
    aci: &str,
    backup_info: &[u8],
    frames: &[Vec<u8>],
    iv: [u8; 16],
    trailing_zero_pad: usize,
) -> Result<Vec<u8>, String> {
    let backup_id = derive_backup_id(ephemeral_key, aci)?;
    let keys = derive_message_backup_key(ephemeral_key, &backup_id, None);

    let mut plain = Vec::new();
    push_varint(&mut plain, backup_info.len() as u64);
    plain.extend_from_slice(backup_info);
    for frame in frames {
        push_varint(&mut plain, frame.len() as u64);
        plain.extend_from_slice(frame);
    }

    let mut compressed = gzip_compress(&plain);
    if trailing_zero_pad > 0 {
        compressed.resize(compressed.len() + trailing_zero_pad, 0);
    }

    let aes = Aes256::new_from_slice(&keys.aes_key).expect("32-byte AES key");
    let pad = 16 - (compressed.len() % 16);
    let mut out = Vec::with_capacity(AES_IV_LEN + compressed.len() + pad + HMAC_LEN);
    out.extend_from_slice(&iv);
    let mut prev = iv;
    let mut chunks = compressed.chunks_exact(16);
    for chunk in &mut chunks {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        aes.encrypt_block((&mut block).into());
        out.extend_from_slice(&block);
        prev = block;
    }
    let rem = chunks.remainder();
    let mut last = [pad as u8; 16];
    last[..rem.len()].copy_from_slice(rem);
    for i in 0..16 {
        last[i] ^= prev[i];
    }
    aes.encrypt_block((&mut last).into());
    out.extend_from_slice(&last);

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&keys.hmac_key).expect("32-byte HMAC key");
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    Ok(out)
}

fn read_varint32_from_reader<R: Read>(reader: &mut R) -> Result<Option<u32>, String> {
    let mut out = 0u32;
    let mut shift = 0u32;
    for idx in 0..5 {
        let mut b = [0u8; 1];
        match reader.read(&mut b) {
            Ok(0) => {
                return if idx == 0 {
                    Ok(None)
                } else {
                    Err("Truncated frame length varint.".to_owned())
                };
            }
            Ok(_) => {
                let byte = b[0];
                out |= u32::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    return Ok(Some(out));
                }
                shift += 7;
            }
            Err(err) => return Err(format!("Failed reading frame length: {err}")),
        }
    }
    Err("Malformed frame length varint.".to_owned())
}

fn read_next_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let Some(len) = read_varint32_from_reader(reader)? else {
        return Ok(None);
    };
    let len = len as usize;
    if len > MAX_FRAME_LEN {
        return Err(format!("Backup frame exceeds size limit ({len} bytes)."));
    }
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|err| format!("Truncated backup frame: {err}"))?;
    Ok(Some(buf))
}

// ---------------------------------------------------------------------------
// Minimal Protobuf Wire Reader & Encoder (works without protoc / prost)
// ---------------------------------------------------------------------------

pub fn push_varint(out: &mut Vec<u8>, mut val: u64) {
    while val >= 0x80 {
        out.push(((val & 0x7f) as u8) | 0x80);
        val >>= 7;
    }
    out.push(val as u8);
}

pub fn pb_varint(out: &mut Vec<u8>, field: u32, val: u64) {
    push_varint(out, u64::from(field << 3));
    push_varint(out, val);
}

pub fn pb_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    push_varint(out, u64::from((field << 3) | 2));
    push_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

pub fn pb_str(out: &mut Vec<u8>, field: u32, text: &str) {
    pb_bytes(out, field, text.as_bytes());
}

enum WireValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64(u64),
    Fixed32(u32),
}

struct PbCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PbCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_varint(&mut self) -> Result<u64, String> {
        let mut out = 0u64;
        let mut shift = 0u32;
        loop {
            let b = *self.data.get(self.pos).ok_or_else(|| "Truncated protobuf varint".to_owned())?;
            self.pos += 1;
            if shift < 64 {
                out |= u64::from(b & 0x7f) << shift;
            }
            if b & 0x80 == 0 {
                return Ok(out);
            }
            shift += 7;
            if shift > 70 {
                return Err("Malformed protobuf varint".to_owned());
            }
        }
    }

    fn next_field(&mut self) -> Result<Option<(u32, WireValue<'a>)>, String> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let tag = self.read_varint()?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        if field == 0 {
            return Err("Invalid protobuf field 0".to_owned());
        }
        let value = match wire {
            0 => WireValue::Varint(self.read_varint()?),
            1 => {
                let slice = self
                    .data
                    .get(self.pos..self.pos + 8)
                    .ok_or_else(|| "Truncated fixed64".to_owned())?;
                self.pos += 8;
                WireValue::Fixed64(u64::from_le_bytes(slice.try_into().expect("8 bytes")))
            }
            2 => {
                let len = self.read_varint()? as usize;
                let slice = self
                    .data
                    .get(self.pos..self.pos + len)
                    .ok_or_else(|| "Truncated length-delimited field".to_owned())?;
                self.pos += len;
                WireValue::Bytes(slice)
            }
            5 => {
                let slice = self
                    .data
                    .get(self.pos..self.pos + 4)
                    .ok_or_else(|| "Truncated fixed32".to_owned())?;
                self.pos += 4;
                WireValue::Fixed32(u32::from_le_bytes(slice.try_into().expect("4 bytes")))
            }
            _ => return Err(format!("Unsupported protobuf wire type {wire}")),
        };
        Ok(Some((field, value)))
    }
}

fn pb_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn read_packed_varints(bytes: &[u8]) -> Result<Vec<u64>, String> {
    let mut cur = PbCursor::new(bytes);
    let mut out = Vec::new();
    while cur.pos < cur.data.len() {
        out.push(cur.read_varint()?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Decoded signal.backup Model
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct BackupInfo {
    version: u64,
    media_root_backup_key: Vec<u8>,
}

#[derive(Debug, Clone)]
enum BackupRecipient {
    Contact(BackupContact),
    Group(BackupGroup),
    SelfRecipient,
    CallLink { root_key: Vec<u8>, name: String },
    Other,
}

#[derive(Debug, Clone, Default)]
struct BackupContact {
    aci: Option<String>,
    username: String,
    e164: Option<u64>,
    blocked: bool,
    profile_given: String,
    profile_family: String,
    nickname_given: String,
    nickname_family: String,
    note: String,
    system_given: String,
    system_family: String,
    system_nickname: String,
    identity_state: u64,
}

impl BackupContact {
    fn display_name(&self) -> String {
        let join = |a: &str, b: &str| {
            [a.trim(), b.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let nick = join(&self.nickname_given, &self.nickname_family);
        if !nick.is_empty() {
            return nick;
        }
        if !self.system_nickname.trim().is_empty() {
            return self.system_nickname.trim().to_owned();
        }
        let prof = join(&self.profile_given, &self.profile_family);
        if !prof.is_empty() {
            return prof;
        }
        let sys = join(&self.system_given, &self.system_family);
        if !sys.is_empty() {
            return sys;
        }
        if !self.username.trim().is_empty() {
            return self.username.trim().to_owned();
        }
        self.e164.map(|n| format!("+{n}")).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default)]
struct BackupGroup {
    id: String,
    title: String,
    description: String,
    timer_secs: u32,
    blocked: bool,
    announcements_only: bool,
    terminated: bool,
    members: Vec<(String, bool)>,
}

#[derive(Debug, Clone)]
struct ImportedChat {
    peer: Peer,
    conversation_id: String,
    archived: bool,
    pinned_order: Option<u32>,
    expiration_timer_secs: u32,
    mute_until_ms: u64,
    marked_unread: bool,
}

#[derive(Debug, Clone, Default)]
struct BackupText {
    body: String,
    ranges: Vec<BackupBodyRange>,
}

#[derive(Debug, Clone)]
struct BackupBodyRange {
    start: u32,
    length: u32,
    mention_aci: Option<String>,
    style: Option<Mark>,
}

#[derive(Debug, Clone, Default)]
struct BackupFilePointer {
    content_type: String,
    file_name: String,
    width: u32,
    height: u32,
    caption: String,
    blur_hash: String,
    key: Vec<u8>,
    size: u32,
    transit_cdn_key: String,
    transit_cdn_number: u32,
    transit_upload_ts: u64,
    encrypted_digest: Vec<u8>,
    plaintext_hash: Vec<u8>,
}

impl BackupFilePointer {
    fn to_wire_pointer(&self, flags: u32) -> Pointer {
        Pointer {
            cdn_number: self.transit_cdn_number,
            cdn_id: None,
            cdn_key: (!self.transit_cdn_key.is_empty()).then(|| self.transit_cdn_key.clone()),
            content_type: self.content_type.clone(),
            key: self.key.clone(),
            size: self.size,
            digest: if !self.encrypted_digest.is_empty() {
                self.encrypted_digest.clone()
            } else {
                self.plaintext_hash.clone()
            },
            file_name: self.file_name.clone(),
            flags,
            width: self.width,
            height: self.height,
            caption: self.caption.clone(),
            blurhash: (!self.blur_hash.is_empty()).then(|| self.blur_hash.clone()),
            thumbnail: Vec::new(),
            upload_timestamp: self.transit_upload_ts,
            url: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BackupMessageAttachment {
    pointer: BackupFilePointer,
    flag: u64,
}

impl BackupMessageAttachment {
    fn wire_flags(&self) -> u32 {
        match self.flag {
            1 => wire::FLAG_VOICE_MESSAGE,
            2 => wire::FLAG_BORDERLESS,
            3 => wire::FLAG_GIF,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BackupReaction {
    emoji: String,
    author_id: u64,
    sort_order: u64,
}

#[derive(Debug, Clone, Default)]
struct BackupQuote {
    target_sent_ts: u64,
    author_id: u64,
    text: Option<BackupText>,
    attachments: Vec<(String, String, Option<BackupMessageAttachment>)>,
}

#[derive(Debug, Clone, Default)]
struct BackupLinkPreview {
    url: String,
    title: String,
    description: String,
}

#[derive(Debug, Clone, Default)]
struct BackupSticker {
    pack_id: Vec<u8>,
    pack_key: Vec<u8>,
    sticker_id: u32,
    emoji: String,
    data: BackupFilePointer,
}

#[derive(Debug, Clone, Default)]
struct BackupPollOption {
    option: String,
    votes: Vec<(u64, u32)>,
}

#[derive(Debug, Clone, Default)]
struct BackupPoll {
    question: String,
    allow_multiple: bool,
    options: Vec<BackupPollOption>,
    has_ended: bool,
    reactions: Vec<BackupReaction>,
}

#[derive(Debug, Clone, Default)]
struct BackupPinDetails {
    pinned_at_ms: u64,
    expires_at_ms: Option<u64>,
    never_expires: bool,
}

#[derive(Debug, Clone)]
enum BackupChatItemBody {
    Standard {
        quote: Option<BackupQuote>,
        text: Option<BackupText>,
        attachments: Vec<BackupMessageAttachment>,
        previews: Vec<BackupLinkPreview>,
        has_long_text: bool,
        reactions: Vec<BackupReaction>,
    },
    Contact {
        contact: SharedContact,
        reactions: Vec<BackupReaction>,
    },
    Sticker {
        sticker: BackupSticker,
        reactions: Vec<BackupReaction>,
    },
    Deleted,
    Update(String),
    Payment {
        note: Option<String>,
    },
    GiftBadge,
    ViewOnce {
        content_type: Option<String>,
        reactions: Vec<BackupReaction>,
    },
    DirectStoryReply {
        text: Option<BackupText>,
        emoji: Option<String>,
        reactions: Vec<BackupReaction>,
    },
    Poll(BackupPoll),
}

#[derive(Debug, Clone)]
struct BackupChatItem {
    chat_id: u64,
    author_id: u64,
    date_sent: u64,
    expire_start_date: Option<u64>,
    expires_in_ms: Option<u64>,
    revisions: Vec<BackupChatItem>,
    outgoing: bool,
    directionless: bool,
    pin: Option<BackupPinDetails>,
    body: Option<BackupChatItemBody>,
}

#[derive(Debug, Clone, Default)]
struct BackupChatFolder {
    id: String,
    name: String,
    show_only_unread: bool,
    show_muted: bool,
    include_all_individual: bool,
    include_all_groups: bool,
    folder_type: u64,
    included_recipients: Vec<u64>,
    excluded_recipients: Vec<u64>,
}

fn decode_backup_info(bytes: &[u8]) -> Result<BackupInfo, String> {
    let mut cur = PbCursor::new(bytes);
    let mut info = BackupInfo::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Varint(v)) => info.version = v,
            (3, WireValue::Bytes(b)) => info.media_root_backup_key = b.to_vec(),
            _ => {}
        }
    }
    Ok(info)
}

fn decode_contact(bytes: &[u8]) -> Result<BackupContact, String> {
    let mut cur = PbCursor::new(bytes);
    let mut c = BackupContact::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => c.aci = uuid_bytes_to_aci(b),
            (3, WireValue::Bytes(b)) => c.username = pb_string(b),
            (4, WireValue::Varint(v)) => c.e164 = Some(v),
            (5, WireValue::Varint(v)) => c.blocked = v != 0,
            (11, WireValue::Bytes(b)) => c.profile_given = pb_string(b),
            (12, WireValue::Bytes(b)) => c.profile_family = pb_string(b),
            (15, WireValue::Varint(v)) => c.identity_state = v,
            (16, WireValue::Bytes(b)) => {
                let mut nc = PbCursor::new(b);
                while let Some((nf, nv)) = nc.next_field()? {
                    match (nf, nv) {
                        (1, WireValue::Bytes(s)) => c.nickname_given = pb_string(s),
                        (2, WireValue::Bytes(s)) => c.nickname_family = pb_string(s),
                        _ => {}
                    }
                }
            }
            (17, WireValue::Bytes(b)) => c.note = pb_string(b),
            (18, WireValue::Bytes(b)) => c.system_given = pb_string(b),
            (19, WireValue::Bytes(b)) => c.system_family = pb_string(b),
            (20, WireValue::Bytes(b)) => c.system_nickname = pb_string(b),
            _ => {}
        }
    }
    Ok(c)
}

fn decode_group(bytes: &[u8]) -> Result<Option<BackupGroup>, String> {
    let mut cur = PbCursor::new(bytes);
    let mut master_key: Option<[u8; 32]> = None;
    let mut group = BackupGroup::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => master_key = b.try_into().ok(),
            (5, WireValue::Bytes(b)) => {
                let mut sc = PbCursor::new(b);
                while let Some((sf, sv)) = sc.next_field()? {
                    match (sf, sv) {
                        (2, WireValue::Bytes(ab)) => {
                            let mut ac = PbCursor::new(ab);
                            while let Some((af, av)) = ac.next_field()? {
                                if let (1, WireValue::Bytes(s)) = (af, av) {
                                    group.title = pb_string(s);
                                }
                            }
                        }
                        (4, WireValue::Bytes(ab)) => {
                            let mut ac = PbCursor::new(ab);
                            while let Some((af, av)) = ac.next_field()? {
                                if let (3, WireValue::Varint(secs)) = (af, av) {
                                    group.timer_secs = secs as u32;
                                }
                            }
                        }
                        (7, WireValue::Bytes(mb)) => {
                            let mut mc = PbCursor::new(mb);
                            let mut uid = None;
                            let mut admin = false;
                            while let Some((mf, mv)) = mc.next_field()? {
                                match (mf, mv) {
                                    (1, WireValue::Bytes(u)) => uid = uuid_bytes_to_aci(u),
                                    (2, WireValue::Varint(role)) => admin = role == 2,
                                    _ => {}
                                }
                            }
                            if let Some(uid) = uid {
                                group.members.push((uid, admin));
                            }
                        }
                        (11, WireValue::Bytes(ab)) => {
                            let mut ac = PbCursor::new(ab);
                            while let Some((af, av)) = ac.next_field()? {
                                if let (4, WireValue::Bytes(s)) = (af, av) {
                                    group.description = pb_string(s);
                                }
                            }
                        }
                        (12, WireValue::Varint(v)) => group.announcements_only = v != 0,
                        (14, WireValue::Varint(v)) => group.terminated = v != 0,
                        _ => {}
                    }
                }
            }
            (6, WireValue::Varint(v)) => group.blocked = v != 0,
            _ => {}
        }
    }
    let Some(mk) = master_key else {
        return Ok(None);
    };
    group.id = derive_group_id(&mk);
    Ok(Some(group))
}

fn decode_recipient(bytes: &[u8]) -> Result<(u64, BackupRecipient), String> {
    let mut cur = PbCursor::new(bytes);
    let mut id = 0u64;
    let mut dest = BackupRecipient::Other;
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Varint(v)) => id = v,
            (2, WireValue::Bytes(b)) => dest = BackupRecipient::Contact(decode_contact(b)?),
            (3, WireValue::Bytes(b)) => {
                if let Some(g) = decode_group(b)? {
                    dest = BackupRecipient::Group(g);
                }
            }
            (5, WireValue::Bytes(_)) => dest = BackupRecipient::SelfRecipient,
            (7, WireValue::Bytes(b)) => {
                let mut cc = PbCursor::new(b);
                let mut root_key = Vec::new();
                let mut name = String::new();
                while let Some((cf, cv)) = cc.next_field()? {
                    match (cf, cv) {
                        (1, WireValue::Bytes(rk)) => root_key = rk.to_vec(),
                        (3, WireValue::Bytes(n)) => name = pb_string(n),
                        _ => {}
                    }
                }
                dest = BackupRecipient::CallLink { root_key, name };
            }
            _ => {}
        }
    }
    Ok((id, dest))
}

fn decode_text(bytes: &[u8]) -> Result<BackupText, String> {
    let mut cur = PbCursor::new(bytes);
    let mut out = BackupText::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => out.body = pb_string(b),
            (2, WireValue::Bytes(b)) => {
                let mut rc = PbCursor::new(b);
                let mut start = 0u32;
                let mut length = 0u32;
                let mut mention_aci = None;
                let mut style = None;
                while let Some((rf, rv)) = rc.next_field()? {
                    match (rf, rv) {
                        (1, WireValue::Varint(v)) => start = v as u32,
                        (2, WireValue::Varint(v)) => length = v as u32,
                        (3, WireValue::Bytes(aci)) => mention_aci = uuid_bytes_to_aci(aci),
                        (4, WireValue::Varint(s)) => {
                            style = match s {
                                1 => Some(Mark::Bold),
                                2 => Some(Mark::Italic),
                                3 => Some(Mark::Spoiler),
                                4 => Some(Mark::Strike),
                                5 => Some(Mark::Monospace),
                                _ => None,
                            };
                        }
                        _ => {}
                    }
                }
                if mention_aci.is_some() || style.is_some() {
                    out.ranges.push(BackupBodyRange { start, length, mention_aci, style });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn decode_file_pointer(bytes: &[u8]) -> Result<BackupFilePointer, String> {
    let mut cur = PbCursor::new(bytes);
    let mut fp = BackupFilePointer::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (4, WireValue::Bytes(b)) => fp.content_type = pb_string(b),
            (7, WireValue::Bytes(b)) => fp.file_name = pb_string(b),
            (8, WireValue::Varint(v)) => fp.width = v as u32,
            (9, WireValue::Varint(v)) => fp.height = v as u32,
            (10, WireValue::Bytes(b)) => fp.caption = pb_string(b),
            (11, WireValue::Bytes(b)) => fp.blur_hash = pb_string(b),
            (13, WireValue::Bytes(b)) => {
                let mut lc = PbCursor::new(b);
                while let Some((lf, lv)) = lc.next_field()? {
                    match (lf, lv) {
                        (1, WireValue::Bytes(k)) => fp.key = k.to_vec(),
                        (3, WireValue::Varint(s)) => fp.size = s as u32,
                        (4, WireValue::Bytes(k)) => fp.transit_cdn_key = pb_string(k),
                        (5, WireValue::Varint(n)) => fp.transit_cdn_number = n as u32,
                        (6, WireValue::Varint(ts)) => fp.transit_upload_ts = ts,
                        (10, WireValue::Bytes(h)) => fp.plaintext_hash = h.to_vec(),
                        (11, WireValue::Bytes(d)) => fp.encrypted_digest = d.to_vec(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(fp)
}

fn decode_message_attachment(bytes: &[u8]) -> Result<BackupMessageAttachment, String> {
    let mut cur = PbCursor::new(bytes);
    let mut att = BackupMessageAttachment::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => att.pointer = decode_file_pointer(b)?,
            (2, WireValue::Varint(v)) => att.flag = v,
            _ => {}
        }
    }
    Ok(att)
}

fn decode_reaction(bytes: &[u8]) -> Result<BackupReaction, String> {
    let mut cur = PbCursor::new(bytes);
    let mut r = BackupReaction::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => r.emoji = pb_string(b),
            (2, WireValue::Varint(v)) => r.author_id = v,
            (4, WireValue::Varint(v)) => r.sort_order = v,
            _ => {}
        }
    }
    Ok(r)
}

fn decode_quote(bytes: &[u8]) -> Result<BackupQuote, String> {
    let mut cur = PbCursor::new(bytes);
    let mut q = BackupQuote::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Varint(v)) => q.target_sent_ts = v,
            (2, WireValue::Varint(v)) => q.author_id = v,
            (3, WireValue::Bytes(b)) => q.text = Some(decode_text(b)?),
            (4, WireValue::Bytes(b)) => {
                let mut ac = PbCursor::new(b);
                let mut ct = String::new();
                let mut fname = String::new();
                let mut thumb = None;
                while let Some((af, av)) = ac.next_field()? {
                    match (af, av) {
                        (1, WireValue::Bytes(s)) => ct = pb_string(s),
                        (2, WireValue::Bytes(s)) => fname = pb_string(s),
                        (3, WireValue::Bytes(tb)) => thumb = Some(decode_message_attachment(tb)?),
                        _ => {}
                    }
                }
                q.attachments.push((ct, fname, thumb));
            }
            _ => {}
        }
    }
    Ok(q)
}

fn decode_contact_attachment(bytes: &[u8]) -> Result<SharedContact, String> {
    let mut cur = PbCursor::new(bytes);
    let mut given = String::new();
    let mut family = String::new();
    let mut org = String::new();
    let mut phones = Vec::new();
    let mut emails = Vec::new();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => {
                let mut nc = PbCursor::new(b);
                while let Some((nf, nv)) = nc.next_field()? {
                    match (nf, nv) {
                        (1, WireValue::Bytes(s)) => given = pb_string(s),
                        (2, WireValue::Bytes(s)) => family = pb_string(s),
                        _ => {}
                    }
                }
            }
            (3, WireValue::Bytes(b)) => {
                let mut pc = PbCursor::new(b);
                while let Some((pf, pv)) = pc.next_field()? {
                    if let (1, WireValue::Bytes(s)) = (pf, pv) {
                        let v = pb_string(s);
                        if !v.is_empty() {
                            phones.push(v);
                        }
                    }
                }
            }
            (4, WireValue::Bytes(b)) => {
                let mut ec = PbCursor::new(b);
                while let Some((ef, ev)) = ec.next_field()? {
                    if let (1, WireValue::Bytes(s)) = (ef, ev) {
                        let v = pb_string(s);
                        if !v.is_empty() {
                            emails.push(v);
                        }
                    }
                }
            }
            (7, WireValue::Bytes(b)) => org = pb_string(b),
            _ => {}
        }
    }
    let name = [given.trim(), family.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(SharedContact {
        name: if name.is_empty() { org.clone() } else { name },
        phones,
        emails,
        organization: org,
        ..SharedContact::default()
    })
}

fn decode_chat_update(bytes: &[u8]) -> Result<String, String> {
    let mut cur = PbCursor::new(bytes);
    while let Some((field, val)) = cur.next_field()? {
        if let WireValue::Bytes(b) = val {
            match field {
                1 => {
                    let mut sc = PbCursor::new(b);
                    let mut kind = 0u64;
                    while let Some((sf, sv)) = sc.next_field()? {
                        if let (1, WireValue::Varint(v)) = (sf, sv) {
                            kind = v;
                        }
                    }
                    return Ok(match kind {
                        1 => "Joined Signal".to_owned(),
                        2 => "Safety number changed".to_owned(),
                        3 => "Safety number verified".to_owned(),
                        4 => "Safety number marked unverified".to_owned(),
                        5 => "Changed phone number".to_owned(),
                        7 => "Session reset".to_owned(),
                        14 => "Blocked".to_owned(),
                        15 => "Unblocked".to_owned(),
                        16 => "Message request accepted".to_owned(),
                        _ => String::new(),
                    });
                }
                2 => {
                    return decode_group_change_update(b);
                }
                3 => {
                    let mut tc = PbCursor::new(b);
                    let mut ms = 0u64;
                    while let Some((tf, tv)) = tc.next_field()? {
                        if let (1, WireValue::Varint(v)) = (tf, tv) {
                            ms = v;
                        }
                    }
                    let secs = (ms / 1_000) as u32;
                    return Ok(if secs == 0 {
                        "Disappearing messages are off".to_owned()
                    } else {
                        format!("Disappearing messages are set to {}", super::duration(secs))
                    });
                }
                4 => {
                    let mut pc = PbCursor::new(b);
                    let mut prev = String::new();
                    let mut next = String::new();
                    while let Some((pf, pv)) = pc.next_field()? {
                        match (pf, pv) {
                            (1, WireValue::Bytes(s)) => prev = pb_string(s),
                            (2, WireValue::Bytes(s)) => next = pb_string(s),
                            _ => {}
                        }
                    }
                    if !prev.is_empty() && !next.is_empty() {
                        return Ok(format!("{prev} changed their name to {next}"));
                    }
                }
                7 => {
                    let mut cc = PbCursor::new(b);
                    let mut call_type = 1u64;
                    let mut dir = 1u64;
                    let mut state = 1u64;
                    while let Some((cf, cv)) = cc.next_field()? {
                        match (cf, cv) {
                            (2, WireValue::Varint(v)) => call_type = v,
                            (3, WireValue::Varint(v)) => dir = v,
                            (4, WireValue::Varint(v)) => state = v,
                            _ => {}
                        }
                    }
                    let kind = if call_type == 2 { "video call" } else { "voice call" };
                    return Ok(match (dir, state) {
                        (2, 1) => format!("Outgoing {kind}"),
                        (2, _) => format!("Unanswered {kind}"),
                        (_, 3 | 4) => format!("Missed {kind}"),
                        (_, 2) => format!("Declined {kind}"),
                        _ => format!("Incoming {kind}"),
                    });
                }
                8 => {
                    let mut gc = PbCursor::new(b);
                    let mut state = 1u64;
                    while let Some((gf, gv)) = gc.next_field()? {
                        if let (2, WireValue::Varint(v)) = (gf, gv) {
                            state = v;
                        }
                    }
                    return Ok(match state {
                        6 | 7 => "Missed group call".to_owned(),
                        5 => "Declined group call".to_owned(),
                        2 | 4 => "Joined group call".to_owned(),
                        _ => "Group call".to_owned(),
                    });
                }
                10 => {
                    let mut tc = PbCursor::new(b);
                    let mut question = String::new();
                    while let Some((tf, tv)) = tc.next_field()? {
                        if let (2, WireValue::Bytes(s)) = (tf, tv) {
                            question = pb_string(s);
                        }
                    }
                    return Ok(if question.is_empty() {
                        "Poll ended".to_owned()
                    } else {
                        format!("Poll ended: {question}")
                    });
                }
                11 => return Ok("Pinned a message".to_owned()),
                _ => {}
            }
        }
    }
    Ok(String::new())
}

fn decode_group_change_update(bytes: &[u8]) -> Result<String, String> {
    let mut cur = PbCursor::new(bytes);
    let mut lines = Vec::new();
    while let Some((field, val)) = cur.next_field()? {
        if let (1, WireValue::Bytes(ub)) = (field, val) {
            let mut uc = PbCursor::new(ub);
            while let Some((uf, uv)) = uc.next_field()? {
                if let WireValue::Bytes(inner) = uv {
                    let line = match uf {
                        2 => "Group created".to_owned(),
                        3 => {
                            let mut nc = PbCursor::new(inner);
                            let mut title = String::new();
                            while let Some((nf, nv)) = nc.next_field()? {
                                if let (2, WireValue::Bytes(s)) = (nf, nv) {
                                    title = pb_string(s);
                                }
                            }
                            if title.is_empty() {
                                "The group name was removed".to_owned()
                            } else {
                                format!("The group name is now {title}")
                            }
                        }
                        4 => "The group photo changed".to_owned(),
                        5 => {
                            let mut dc = PbCursor::new(inner);
                            let mut desc = String::new();
                            while let Some((df, dv)) = dc.next_field()? {
                                if let (2, WireValue::Bytes(s)) = (df, dv) {
                                    desc = pb_string(s);
                                }
                            }
                            if desc.is_empty() {
                                "The group description was cleared".to_owned()
                            } else {
                                format!("The group description is now {desc}")
                            }
                        }
                        9 => "Group admin roles changed".to_owned(),
                        10 => "A member left the group".to_owned(),
                        11 => "A member was removed from the group".to_owned(),
                        17 | 18 | 28 => "A member joined the group".to_owned(),
                        25 => "The group link is on".to_owned(),
                        27 => "The group link is off".to_owned(),
                        34 => {
                            let mut tc = PbCursor::new(inner);
                            let mut ms = 0u64;
                            while let Some((tf, tv)) = tc.next_field()? {
                                if let (1, WireValue::Varint(v)) = (tf, tv) {
                                    ms = v;
                                }
                            }
                            let secs = (ms / 1_000) as u32;
                            if secs == 0 {
                                "Disappearing messages are off".to_owned()
                            } else {
                                format!("Disappearing messages are set to {}", super::duration(secs))
                            }
                        }
                        _ => "The group was updated".to_owned(),
                    };
                    if !line.is_empty() {
                        lines.push(line);
                    }
                }
            }
        }
    }
    Ok(lines.join(" · "))
}

fn decode_chat_item(bytes: &[u8]) -> Result<BackupChatItem, String> {
    let mut cur = PbCursor::new(bytes);
    let mut item = BackupChatItem {
        chat_id: 0,
        author_id: 0,
        date_sent: 0,
        expire_start_date: None,
        expires_in_ms: None,
        revisions: Vec::new(),
        outgoing: false,
        directionless: false,
        pin: None,
        body: None,
    };
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Varint(v)) => item.chat_id = v,
            (2, WireValue::Varint(v)) => item.author_id = v,
            (3, WireValue::Varint(v)) => item.date_sent = v,
            (4, WireValue::Varint(v)) => item.expire_start_date = Some(v),
            (5, WireValue::Varint(v)) => item.expires_in_ms = Some(v),
            (6, WireValue::Bytes(b)) => item.revisions.push(decode_chat_item(b)?),
            (8, WireValue::Bytes(_)) => item.outgoing = false,
            (9, WireValue::Bytes(_)) => item.outgoing = true,
            (10, WireValue::Bytes(_)) => item.directionless = true,
            (11, WireValue::Bytes(b)) => {
                let mut sc = PbCursor::new(b);
                let mut quote = None;
                let mut text = None;
                let mut attachments = Vec::new();
                let mut previews = Vec::new();
                let mut has_long_text = false;
                let mut reactions = Vec::new();
                while let Some((sf, sv)) = sc.next_field()? {
                    if let WireValue::Bytes(sb) = sv {
                        match sf {
                            1 => quote = Some(decode_quote(sb)?),
                            2 => text = Some(decode_text(sb)?),
                            3 => attachments.push(decode_message_attachment(sb)?),
                            4 => {
                                let mut pc = PbCursor::new(sb);
                                let mut lp = BackupLinkPreview::default();
                                while let Some((pf, pv)) = pc.next_field()? {
                                    match (pf, pv) {
                                        (1, WireValue::Bytes(s)) => lp.url = pb_string(s),
                                        (2, WireValue::Bytes(s)) => lp.title = pb_string(s),
                                        (4, WireValue::Bytes(s)) => lp.description = pb_string(s),
                                        _ => {}
                                    }
                                }
                                if !lp.url.is_empty() {
                                    previews.push(lp);
                                }
                            }
                            5 => has_long_text = true,
                            6 => reactions.push(decode_reaction(sb)?),
                            _ => {}
                        }
                    }
                }
                item.body = Some(BackupChatItemBody::Standard {
                    quote,
                    text,
                    attachments,
                    previews,
                    has_long_text,
                    reactions,
                });
            }
            (12, WireValue::Bytes(b)) => {
                let mut cc = PbCursor::new(b);
                let mut contact = SharedContact::default();
                let mut reactions = Vec::new();
                while let Some((cf, cv)) = cc.next_field()? {
                    if let WireValue::Bytes(cb) = cv {
                        match cf {
                            1 => contact = decode_contact_attachment(cb)?,
                            2 => reactions.push(decode_reaction(cb)?),
                            _ => {}
                        }
                    }
                }
                item.body = Some(BackupChatItemBody::Contact { contact, reactions });
            }
            (13, WireValue::Bytes(b)) => {
                let mut sc = PbCursor::new(b);
                let mut sticker = BackupSticker::default();
                let mut reactions = Vec::new();
                while let Some((sf, sv)) = sc.next_field()? {
                    if let WireValue::Bytes(sb) = sv {
                        match sf {
                            1 => {
                                let mut tc = PbCursor::new(sb);
                                while let Some((tf, tv)) = tc.next_field()? {
                                    match (tf, tv) {
                                        (1, WireValue::Bytes(v)) => sticker.pack_id = v.to_vec(),
                                        (2, WireValue::Bytes(v)) => sticker.pack_key = v.to_vec(),
                                        (3, WireValue::Varint(v)) => sticker.sticker_id = v as u32,
                                        (4, WireValue::Bytes(v)) => sticker.emoji = pb_string(v),
                                        (5, WireValue::Bytes(v)) => sticker.data = decode_file_pointer(v)?,
                                        _ => {}
                                    }
                                }
                            }
                            2 => reactions.push(decode_reaction(sb)?),
                            _ => {}
                        }
                    }
                }
                item.body = Some(BackupChatItemBody::Sticker { sticker, reactions });
            }
            (14 | 22, WireValue::Bytes(_)) => item.body = Some(BackupChatItemBody::Deleted),
            (15, WireValue::Bytes(b)) => {
                let line = decode_chat_update(b)?;
                if !line.is_empty() {
                    item.body = Some(BackupChatItemBody::Update(line));
                }
            }
            (16, WireValue::Bytes(b)) => {
                let mut pc = PbCursor::new(b);
                let mut note = None;
                while let Some((pf, pv)) = pc.next_field()? {
                    if let (3, WireValue::Bytes(s)) = (pf, pv) {
                        note = Some(pb_string(s));
                    }
                }
                item.body = Some(BackupChatItemBody::Payment { note });
            }
            (17, WireValue::Bytes(_)) => item.body = Some(BackupChatItemBody::GiftBadge),
            (18, WireValue::Bytes(b)) => {
                let mut vc = PbCursor::new(b);
                let mut content_type = None;
                let mut reactions = Vec::new();
                while let Some((vf, vv)) = vc.next_field()? {
                    if let WireValue::Bytes(vb) = vv {
                        match vf {
                            1 => {
                                let att = decode_message_attachment(vb)?;
                                if !att.pointer.content_type.is_empty() {
                                    content_type = Some(att.pointer.content_type);
                                }
                            }
                            2 => reactions.push(decode_reaction(vb)?),
                            _ => {}
                        }
                    }
                }
                item.body = Some(BackupChatItemBody::ViewOnce { content_type, reactions });
            }
            (19, WireValue::Bytes(b)) => {
                let mut rc = PbCursor::new(b);
                let mut text = None;
                let mut emoji = None;
                let mut reactions = Vec::new();
                while let Some((rf, rv)) = rc.next_field()? {
                    if let WireValue::Bytes(rb) = rv {
                        match rf {
                            1 => {
                                let mut tc = PbCursor::new(rb);
                                while let Some((tf, tv)) = tc.next_field()? {
                                    if let (1, WireValue::Bytes(tb)) = (tf, tv) {
                                        text = Some(decode_text(tb)?);
                                    }
                                }
                            }
                            2 => emoji = Some(pb_string(rb)),
                            3 => reactions.push(decode_reaction(rb)?),
                            _ => {}
                        }
                    }
                }
                item.body = Some(BackupChatItemBody::DirectStoryReply { text, emoji, reactions });
            }
            (20, WireValue::Bytes(b)) => {
                let mut pc = PbCursor::new(b);
                let mut poll = BackupPoll::default();
                while let Some((pf, pv)) = pc.next_field()? {
                    match (pf, pv) {
                        (1, WireValue::Bytes(s)) => poll.question = pb_string(s),
                        (2, WireValue::Varint(v)) => poll.allow_multiple = v != 0,
                        (3, WireValue::Bytes(ob)) => {
                            let mut oc = PbCursor::new(ob);
                            let mut opt = BackupPollOption::default();
                            while let Some((of, ov)) = oc.next_field()? {
                                if let WireValue::Bytes(vb) = ov {
                                    match of {
                                        1 => opt.option = pb_string(vb),
                                        2 => {
                                            let mut vc = PbCursor::new(vb);
                                            let mut voter_id = 0u64;
                                            let mut count = 1u32;
                                            while let Some((vf, vv)) = vc.next_field()? {
                                                match (vf, vv) {
                                                    (1, WireValue::Varint(v)) => voter_id = v,
                                                    (2, WireValue::Varint(v)) => count = v as u32,
                                                    _ => {}
                                                }
                                            }
                                            opt.votes.push((voter_id, count));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            poll.options.push(opt);
                        }
                        (4, WireValue::Varint(v)) => poll.has_ended = v != 0,
                        (5, WireValue::Bytes(rb)) => poll.reactions.push(decode_reaction(rb)?),
                        _ => {}
                    }
                }
                item.body = Some(BackupChatItemBody::Poll(poll));
            }
            (21, WireValue::Bytes(b)) => {
                let mut pc = PbCursor::new(b);
                let mut pin = BackupPinDetails::default();
                while let Some((pf, pv)) = pc.next_field()? {
                    match (pf, pv) {
                        (1, WireValue::Varint(v)) => pin.pinned_at_ms = v,
                        (2, WireValue::Varint(v)) => pin.expires_at_ms = Some(v),
                        (3, WireValue::Varint(v)) => pin.never_expires = v != 0,
                        _ => {}
                    }
                }
                item.pin = Some(pin);
            }
            _ => {}
        }
    }
    Ok(item)
}

fn decode_chat_folder(bytes: &[u8]) -> Result<BackupChatFolder, String> {
    let mut cur = PbCursor::new(bytes);
    let mut folder = BackupChatFolder::default();
    while let Some((field, val)) = cur.next_field()? {
        match (field, val) {
            (1, WireValue::Bytes(b)) => folder.name = pb_string(b),
            (2, WireValue::Varint(v)) => folder.show_only_unread = v != 0,
            (3, WireValue::Varint(v)) => folder.show_muted = v != 0,
            (4, WireValue::Varint(v)) => folder.include_all_individual = v != 0,
            (5, WireValue::Varint(v)) => folder.include_all_groups = v != 0,
            (6, WireValue::Varint(v)) => folder.folder_type = v,
            (7, WireValue::Varint(v)) => folder.included_recipients.push(v),
            (7, WireValue::Bytes(b)) => folder.included_recipients.extend(read_packed_varints(b)?),
            (8, WireValue::Varint(v)) => folder.excluded_recipients.push(v),
            (8, WireValue::Bytes(b)) => folder.excluded_recipients.extend(read_packed_varints(b)?),
            (9, WireValue::Bytes(b)) => folder.id = hex_lower(b),
            _ => {}
        }
    }
    Ok(folder)
}

// ---------------------------------------------------------------------------
// Idempotent Streaming Importer into chatd's SQLite Store
// ---------------------------------------------------------------------------

struct ImportState<'a> {
    hub: &'a Arc<Hub>,
    account: &'a AccountKey,
    me_aci: &'a str,
    me_name: String,
    now_secs: i64,
    recipients: HashMap<u64, BackupRecipient>,
    chats: HashMap<u64, ImportedChat>,
    names_by_aci: HashMap<String, String>,
    pinned_chats: Vec<(u32, String)>,
    folders: Vec<wire::ChatFolder>,
    kept_attachments: Vec<KeptAttachment>,
    kept_attachment_ids: HashSet<String>,
    kept_reactions: Vec<super::KeptReaction>,
    kept_pins: Vec<super::PinRecord>,
    kept_stickers: Vec<wire::StickerPackInfo>,
    touched_conversations: HashSet<String>,
    any_expiring: bool,
    summary: ImportSummary,
}

fn key_for(account: &AccountKey, id: &str) -> Key {
    Key {
        network: account.network,
        account: account.account.clone(),
        id: id.to_owned(),
    }
}

impl<'a> ImportState<'a> {
    fn recipient_aci(&self, id: u64) -> Option<String> {
        match self.recipients.get(&id)? {
            BackupRecipient::SelfRecipient => Some(self.me_aci.to_owned()),
            BackupRecipient::Contact(c) => c.aci.clone(),
            _ => None,
        }
    }

    fn recipient_peer(&self, id: u64) -> Option<Peer> {
        match self.recipients.get(&id)? {
            BackupRecipient::SelfRecipient => Some(Peer::Direct(self.me_aci.to_owned())),
            BackupRecipient::Contact(c) => c.aci.clone().map(Peer::Direct),
            BackupRecipient::Group(g) if !g.id.is_empty() => Some(Peer::Group(g.id.clone())),
            _ => None,
        }
    }

    fn name_for_aci(&self, aci: &str) -> String {
        if aci == self.me_aci {
            return if self.me_name.is_empty() { "You".to_owned() } else { self.me_name.clone() };
        }
        self.names_by_aci
            .get(aci)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| super::NO_NAME.to_owned())
    }

    fn apply_text(&self, text: &BackupText) -> (String, Vec<Span>, bool) {
        let ranges: Vec<Range> = text
            .ranges
            .iter()
            .filter_map(|r| {
                if let Some(aci) = &r.mention_aci {
                    let label = if aci == self.me_aci {
                        "@you".to_owned()
                    } else {
                        let name = self.name_for_aci(aci);
                        format!("@{name}")
                    };
                    Some(Range::Mention {
                        start: r.start,
                        aci: aci.clone(),
                        label,
                    })
                } else {
                    r.style.map(|style| Range::Style {
                        start: r.start,
                        length: r.length,
                        style,
                    })
                }
            })
            .collect();
        let styled = style::apply(&text.body, &ranges, self.me_aci);
        (styled.text, styled.spans, styled.mentions_me)
    }

    fn record_reactions(&mut self, msg_id: &str, reactions: &[BackupReaction]) -> Vec<Reaction> {
        // Group per author keeping highest sort_order (Signal allows one reaction per author).
        let mut by_author: HashMap<String, (u64, String)> = HashMap::new();
        for r in reactions {
            if r.emoji.is_empty() {
                continue;
            }
            let Some(author) = self.recipient_aci(r.author_id) else {
                continue;
            };
            let replace = by_author
                .get(&author)
                .is_none_or(|(prev_order, _)| r.sort_order >= *prev_order);
            if replace {
                by_author.insert(author, (r.sort_order, r.emoji.clone()));
            }
        }
        let mut by_emoji: HashMap<String, (Vec<String>, bool)> = HashMap::new();
        let mut sorted: Vec<(String, String)> = by_author
            .into_iter()
            .map(|(author, (_, emoji))| (author, emoji))
            .collect();
        sorted.sort();
        for (author, emoji) in sorted {
            let mine = author == self.me_aci;
            self.kept_reactions.push(super::KeptReaction {
                message: msg_id.to_owned(),
                person: author.clone(),
                emoji: emoji.clone(),
            });
            let entry = by_emoji.entry(emoji).or_insert_with(|| (Vec::new(), false));
            entry.0.push(author);
            entry.1 |= mine;
        }
        let mut out: Vec<Reaction> = by_emoji
            .into_iter()
            .map(|(emoji, (who, mine))| Reaction {
                emoji,
                file: None,
                count: who.len() as u32,
                mine,
                who,
                ..Reaction::default()
            })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.emoji.cmp(&b.emoji)));
        out
    }

    fn register_blurhash_preview(&self, media_id: &str, ptr: &Pointer) -> Option<MediaFile> {
        let hash = ptr.blurhash.as_deref()?;
        let bytes = files::decode_blurhash_jpeg(hash, ptr.width, ptr.height)?;
        let mut prev = MediaFile::new(format!("{media_id}:preview"), MediaKind::Image);
        prev.bytes = bytes.len() as u64;
        let media_root = protocol::support_dir().join("media");
        let path = crate::blobs::path_of(&media_root, self.account, &prev.id);
        let part = path.with_extension("part");
        std::fs::create_dir_all(crate::blobs::dir_under(&media_root, self.account)).ok()?;
        std::fs::write(&part, &bytes).ok()?;
        if std::fs::rename(&part, &path).is_err() {
            let _ = std::fs::remove_file(&part);
            return None;
        }
        let addr = format!("signal://preview/{media_id}");
        let mut store = self.hub.store();
        store.register_media(self.account, &[(prev.clone(), addr)]).ok()?;
        store
            .media_landed(
                self.account,
                &prev.id,
                &path.to_string_lossy(),
                bytes.len() as u64,
                self.now_secs,
            )
            .ok()?
    }

    fn handle_recipient(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (id, rec) = decode_recipient(bytes)?;
        match &rec {
            BackupRecipient::Contact(c) => {
                if let Some(aci) = &c.aci {
                    let display = c.display_name();
                    if !display.is_empty() {
                        self.names_by_aci.insert(aci.clone(), display.clone());
                    }
                    let row = PersonRow {
                        id: aci.clone(),
                        name: if display.is_empty() { super::NO_NAME.to_owned() } else { display },
                        handle: c.username.clone(),
                        avatar: None,
                        bot: false,
                        status: String::new(),
                        link: String::new(),
                    };
                    let rel = if c.blocked { Relation::Blocked } else { Relation::Contact };
                    let mark = (c.identity_state == 1).then_some(PersonMark::Verified);
                    let mut store = self.hub.store();
                    let _ = store.upsert_people(self.account, &[row]);
                    let _ = store.set_relation(&key_for(self.account, aci), rel);
                    if mark.is_some() {
                        let _ = store.set_details(
                            self.account,
                            &[(
                                aci.clone(),
                                PersonDetails {
                                    mark,
                                    ..PersonDetails::default()
                                },
                            )],
                        );
                    }
                }
            }
            BackupRecipient::Group(_) | BackupRecipient::SelfRecipient | BackupRecipient::CallLink { .. } | BackupRecipient::Other => {}
        }
        self.recipients.insert(id, rec);
        Ok(())
    }

    fn handle_chat(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut cur = PbCursor::new(bytes);
        let mut id = 0u64;
        let mut recipient_id = 0u64;
        let mut archived = false;
        let mut pinned_order = None;
        let mut expiration_timer_ms = 0u64;
        let mut mute_until_ms = 0u64;
        let mut marked_unread = false;
        while let Some((field, val)) = cur.next_field()? {
            match (field, val) {
                (1, WireValue::Varint(v)) => id = v,
                (2, WireValue::Varint(v)) => recipient_id = v,
                (3, WireValue::Varint(v)) => archived = v != 0,
                (4, WireValue::Varint(v)) => pinned_order = Some(v as u32),
                (5, WireValue::Varint(v)) => expiration_timer_ms = v,
                (6, WireValue::Varint(v)) => mute_until_ms = v,
                (7, WireValue::Varint(v)) => marked_unread = v != 0,
                _ => {}
            }
        }
        let Some(rec) = self.recipients.get(&recipient_id).cloned() else {
            return Ok(());
        };
        let (peer, mut conv_row) = match rec {
            BackupRecipient::SelfRecipient => {
                let peer = Peer::Direct(self.me_aci.to_owned());
                let row = super::row(&peer, "Note to Self");
                (peer, row)
            }
            BackupRecipient::Contact(c) => {
                let Some(aci) = c.aci.clone() else {
                    return Ok(());
                };
                let peer = Peer::Direct(aci);
                let title = c.display_name();
                let row = super::row(&peer, &title);
                (peer, row)
            }
            BackupRecipient::Group(g) if !g.id.is_empty() => {
                let peer = Peer::Group(g.id.clone());
                let mut row = super::row(&peer, &g.title);
                row.topic = g.description.chars().take(300).collect();
                let is_member = g.members.is_empty() || g.members.iter().any(|(m, _)| m == self.me_aci);
                let is_admin = g.members.iter().any(|(m, admin)| m == self.me_aci && *admin);
                row.read_only = g.terminated || !is_member || (g.announcements_only && !is_admin);
                if g.announcements_only && !is_admin && is_member && !g.terminated {
                    row.read_only_reason = "Only admins can send messages".to_owned();
                }
                (peer, row)
            }
            _ => return Ok(()),
        };

        let conv_id = peer.conversation();
        let conv_key = key_for(self.account, &conv_id);
        let timer_secs = (expiration_timer_ms / 1_000) as u32;
        let now_ms = (self.now_secs.max(0) as u64) * 1_000;
        let muted = mute_until_ms > now_ms;

        {
            let mut store = self.hub.store();
            if let Ok(Some(existing)) = store.conversation(&conv_key) {
                conv_row.avatar = existing.avatar.map(|a| a.id);
            }
            let _ = store.upsert_conversation(self.account, &conv_row);
            if timer_secs > 0 {
                let _ = store.set_disappearing(&conv_key, Some(timer_secs));
            }
            if muted {
                let _ = store.set_muted(&conv_key, true);
            }
            if marked_unread {
                let _ = store.mark_unread_badge(&conv_key);
            }
        }

        if let Some(order) = pinned_order {
            self.pinned_chats.push((order, conv_id.clone()));
        }
        self.touched_conversations.insert(conv_id.clone());
        self.chats.insert(
            id,
            ImportedChat {
                peer,
                conversation_id: conv_id,
                archived,
                pinned_order,
                expiration_timer_secs: timer_secs,
                mute_until_ms,
                marked_unread,
            },
        );
        self.summary.chats += 1;
        Ok(())
    }

    fn handle_chat_item(&mut self, bytes: &[u8]) -> Result<(), String> {
        let item = decode_chat_item(bytes)?;
        let Some(chat) = self.chats.get(&item.chat_id).cloned() else {
            return Ok(());
        };
        let Some(body) = item.body.clone() else {
            return Ok(());
        };
        let author_aci = self
            .recipient_aci(item.author_id)
            .unwrap_or_else(|| if item.outgoing { self.me_aci.to_owned() } else { chat.conversation_id.clone() });

        // Respect disappearing message timers (#717): never import messages that have already expired.
        let expires_at = if let Some(expires_in_ms) = item.expires_in_ms.filter(|&ms| ms > 0) {
            let start_ms = item
                .expire_start_date
                .filter(|&s| s > 0)
                .or_else(|| item.outgoing.then_some(item.date_sent).filter(|&s| s > 0));
            if let Some(start_ms) = start_ms {
                let exp_secs = ((start_ms.saturating_add(expires_in_ms)) / 1_000) as i64;
                if exp_secs <= self.now_secs {
                    self.summary.skipped_expired += 1;
                    return Ok(());
                }
                self.any_expiring = true;
                Some(exp_secs)
            } else {
                None
            }
        } else {
            None
        };

        let sent_secs = i64::try_from(item.date_sent / 1_000).unwrap_or(0);
        let msg_id = if wire::is_aci(&author_aci) {
            wire::message_id(item.date_sent, &author_aci)
        } else {
            format!("signal-hist-{}-{}", item.date_sent, item.chat_id)
        };
        let sender_name = if item.directionless {
            String::new()
        } else {
            self.name_for_aci(&author_aci)
        };

        let mut msg = Message::plain(msg_id.clone(), sender_name, String::new(), sent_secs, item.outgoing);
        msg.sender.id = author_aci.clone();
        msg.expires_at = expires_at;

        // If the message was edited (Signal stores prior versions in `revisions`, oldest to newest),
        // mark it edited and capture the previous body as `prior_text`.
        if !item.revisions.is_empty() {
            msg.edited = true;
            msg.edited_at = Some(sent_secs);
            if let Some(prev) = item.revisions.last() {
                if let Some(BackupChatItemBody::Standard { text: Some(t), .. }) = &prev.body {
                    let (prior, _, _) = self.apply_text(t);
                    if !prior.is_empty() {
                        msg.prior_text = Some(prior);
                    }
                }
            }
        }

        let mut media_to_register: Vec<(MediaFile, String)> = Vec::new();

        match body {
            BackupChatItemBody::Standard {
                quote,
                text,
                attachments,
                previews,
                has_long_text,
                reactions,
            } => {
                if let Some(t) = &text {
                    let (mut applied, spans, mentions_me) = self.apply_text(t);
                    if has_long_text && !applied.is_empty() {
                        applied.push_str(wire::CONTINUES);
                    }
                    msg.text = applied;
                    msg.spans = spans;
                    msg.mentions_me = mentions_me && !item.outgoing;
                }
                if let Some(q) = quote {
                    let q_author = self.recipient_aci(q.author_id).unwrap_or_default();
                    let q_sender = self.name_for_aci(&q_author);
                    let mut snippet = q
                        .text
                        .as_ref()
                        .map(|t| {
                            let (s, sp, _) = self.apply_text(t);
                            crate::store::without_spoilers(&s, &sp)
                        })
                        .unwrap_or_default();
                    if snippet.trim().is_empty() {
                        if let Some((ct, fname, _)) = q.attachments.first() {
                            snippet = files::attachment_label(ct, fname, 0);
                        }
                    }
                    let target_id = if q.target_sent_ts > 0 && wire::is_aci(&q_author) {
                        wire::message_id(q.target_sent_ts, &q_author)
                    } else {
                        String::new()
                    };
                    msg.reply_to = Some(ReplyTo {
                        id: target_id,
                        sender: q_sender,
                        snippet: snippet.chars().take(200).collect(),
                        person: wire::is_aci(&q_author).then(|| key_for(self.account, &q_author)),
                        deleted: false,
                        thumb: None,
                        story: false,
                    });
                }
                for (idx, att) in attachments.iter().enumerate() {
                    let ptr = att.pointer.to_wire_pointer(att.wire_flags());
                    let media_id = format!("signal:att:{}:{}:{idx}", item.date_sent, author_aci);
                    let address = format!("signal://attachment/{media_id}");
                    let kind = files::attachment_kind(&ptr.content_type, ptr.flags);
                    let mut mf = MediaFile::new(media_id.clone(), kind);
                    mf.width = ptr.width;
                    mf.height = ptr.height;
                    mf.bytes = u64::from(ptr.size);
                    media_to_register.push((mf.clone(), address.clone()));
                    if ptr.downloadable() && self.kept_attachment_ids.insert(media_id.clone()) {
                        self.kept_attachments.push(KeptAttachment {
                            id: media_id.clone(),
                            pointer: ptr.clone(),
                            file: mf.clone(),
                            address,
                            conversation: chat.conversation_id.clone(),
                            message_id: msg_id.clone(),
                        });
                    }
                    let preview = self.register_blurhash_preview(&media_id, &ptr);
                    let att_name = if !ptr.file_name.trim().is_empty() {
                        ptr.file_name.trim().to_owned()
                    } else {
                        files::default_filename(&ptr.content_type, ptr.flags).to_owned()
                    };
                    if msg.text.is_empty() && !ptr.caption.is_empty() {
                        msg.text = ptr.caption.clone();
                    }
                    let mut proto_att = Attachment::new(att_name, mf);
                    proto_att.mime = ptr.content_type.clone();
                    if kind == MediaKind::Video {
                        proto_att.thumb = preview.clone();
                    }
                    if ptr.is_voice() {
                        proto_att.duration = 1;
                        proto_att.waveform = vec![24; 32];
                    }
                    proto_att.preview = preview;
                    msg.attachments.push(proto_att);
                    self.summary.attachments += 1;
                }
                for lp in previews {
                    if !wire::preview_url_in_body(&msg.text, &lp.url) {
                        continue;
                    }
                    msg.previews.push(Preview {
                        url: lp.url.clone(),
                        site: crate::adapters::tiny::preview::real_host(&lp.url),
                        title: lp.title,
                        description: lp.description,
                        image: None,
                        duration: 0,
                        opens_in_browser: false,
                    });
                }
                msg.reactions = self.record_reactions(&msg_id, &reactions);
            }
            BackupChatItemBody::Contact { contact, reactions } => {
                msg.contacts.push(contact);
                msg.reactions = self.record_reactions(&msg_id, &reactions);
            }
            BackupChatItemBody::Sticker { sticker, reactions } => {
                let ptr = sticker.data.to_wire_pointer(0);
                let media_id = format!("signal:att:{}:{}:sticker", item.date_sent, author_aci);
                let address = format!("signal://attachment/{media_id}");
                let mut mf = MediaFile::new(media_id.clone(), MediaKind::Image);
                mf.width = if ptr.width > 0 { ptr.width } else { 512 };
                mf.height = if ptr.height > 0 { ptr.height } else { 512 };
                mf.bytes = u64::from(ptr.size);
                media_to_register.push((mf.clone(), address.clone()));
                if ptr.downloadable() && self.kept_attachment_ids.insert(media_id.clone()) {
                    self.kept_attachments.push(KeptAttachment {
                        id: media_id,
                        pointer: ptr.clone(),
                        file: mf.clone(),
                        address,
                        conversation: chat.conversation_id.clone(),
                        message_id: msg_id.clone(),
                    });
                }
                let name = if !sticker.emoji.is_empty() {
                    sticker.emoji
                } else {
                    "sticker.webp".to_owned()
                };
                let mut proto_att = Attachment::new(name, mf);
                proto_att.mime = if ptr.content_type.is_empty() {
                    "image/webp".to_owned()
                } else {
                    ptr.content_type
                };
                proto_att.sticker = true;
                msg.attachments.push(proto_att);
                msg.reactions = self.record_reactions(&msg_id, &reactions);
                self.summary.attachments += 1;
            }
            BackupChatItemBody::Deleted => {
                msg.deleted = true;
                msg.text.clear();
            }
            BackupChatItemBody::Update(line) => {
                msg.kind = MessageKind::System;
                msg.sender.id.clear();
                msg.sender.name.clear();
                msg.text = line;
            }
            BackupChatItemBody::Payment { note } => {
                msg.kind = MessageKind::Unsupported { link: String::new() };
                msg.text = wire::payment_label(note.as_deref());
            }
            BackupChatItemBody::GiftBadge => {
                msg.kind = MessageKind::Unsupported { link: String::new() };
                msg.text = wire::gift_badge_label().to_owned();
            }
            BackupChatItemBody::ViewOnce { content_type, reactions } => {
                msg.kind = MessageKind::Unsupported { link: String::new() };
                msg.text = wire::view_once_label(content_type.as_deref()).to_owned();
                msg.reactions = self.record_reactions(&msg_id, &reactions);
            }
            BackupChatItemBody::DirectStoryReply { text, emoji, reactions } => {
                if let Some(t) = text {
                    let (applied, spans, mentions_me) = self.apply_text(&t);
                    msg.text = applied;
                    msg.spans = spans;
                    msg.mentions_me = mentions_me && !item.outgoing;
                } else if let Some(e) = emoji {
                    msg.text = e;
                }
                msg.reply_to = Some(ReplyTo {
                    id: String::new(),
                    sender: String::new(),
                    snippet: "Story".to_owned(),
                    person: None,
                    deleted: false,
                    thumb: None,
                    story: true,
                });
                msg.reactions = self.record_reactions(&msg_id, &reactions);
            }
            BackupChatItemBody::Poll(bp) => {
                let mut poll = Poll::open(
                    bp.question.clone(),
                    bp.options.iter().map(|o| o.option.clone()).collect::<Vec<_>>(),
                    bp.allow_multiple,
                );
                poll.closed = bp.has_ended;
                let mut ballots_by_voter: HashMap<String, Vec<u32>> = HashMap::new();
                for (opt_idx, opt) in bp.options.iter().enumerate() {
                    if let Some(choice) = poll.choices.get_mut(opt_idx) {
                        for &(voter_id, vote_count) in &opt.votes {
                            if vote_count == 0 {
                                continue;
                            }
                            choice.count = choice.count.saturating_add(1);
                            if let Some(voter_aci) = self.recipient_aci(voter_id) {
                                if voter_aci == self.me_aci && !poll.mine.contains(&(opt_idx as u32)) {
                                    poll.mine.push(opt_idx as u32);
                                }
                                ballots_by_voter
                                    .entry(voter_aci)
                                    .or_default()
                                    .push(opt_idx as u32);
                            }
                        }
                    }
                }
                let mut ballots: Vec<PollBallot> = ballots_by_voter
                    .into_iter()
                    .map(|(person, mut choices)| {
                        choices.sort_unstable();
                        choices.dedup();
                        PollBallot {
                            person,
                            choices,
                            serial: 1,
                        }
                    })
                    .collect();
                ballots.sort_by(|a, b| a.person.cmp(&b.person));
                poll.ballots = ballots;
                msg.text = bp.question;
                msg.poll = Some(Box::new(poll));
                msg.reactions = self.record_reactions(&msg_id, &bp.reactions);
            }
        }

        // Apply pin metadata if still active.
        if let Some(pin) = item.pin {
            let expired = !pin.never_expires
                && pin
                    .expires_at_ms
                    .is_some_and(|exp| (exp / 1_000) as i64 <= self.now_secs);
            if !expired {
                let pinned_at = (pin.pinned_at_ms / 1_000) as i64;
                let pin_exp = (!pin.never_expires)
                    .then(|| pin.expires_at_ms.map(|ms| (ms / 1_000) as i64))
                    .flatten();
                msg.pinned = true;
                msg.pinned_at = Some(pinned_at);
                self.kept_pins.push(super::PinRecord {
                    conversation: chat.conversation_id.clone(),
                    message_id: msg_id.clone(),
                    pinned_by: String::new(),
                    pinned_at,
                    expires_at: pin_exp,
                });
            }
        }

        let conv_key = key_for(self.account, &chat.conversation_id);
        let stored = {
            let mut store = self.hub.store();
            if !media_to_register.is_empty() {
                let _ = store.register_media(self.account, &media_to_register);
            }
            let outcome = store
                .ingest(&conv_key, &msg, false)
                .map_err(|err| format!("Failed writing imported message: {err}"))?;
            if msg.pinned {
                let _ = store.set_pinned(&conv_key, &msg.id, true);
            }
            outcome
        };

        self.summary.messages += 1;
        if stored == Stored::New {
            self.summary.new_messages += 1;
        }
        self.touched_conversations.insert(chat.conversation_id);
        Ok(())
    }

    fn handle_sticker_pack(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut cur = PbCursor::new(bytes);
        let mut pack_id = Vec::new();
        let mut pack_key = Vec::new();
        while let Some((field, val)) = cur.next_field()? {
            match (field, val) {
                (1, WireValue::Bytes(b)) => pack_id = b.to_vec(),
                (2, WireValue::Bytes(b)) => pack_key = b.to_vec(),
                _ => {}
            }
        }
        if pack_id.len() == 16 && pack_key.len() == 32 {
            let id = hex_lower(&pack_id);
            if !self.kept_stickers.iter().any(|p| p.id == id) {
                self.kept_stickers.push(wire::StickerPackInfo {
                    id,
                    key: hex_lower(&pack_key),
                    title: String::new(),
                    author: String::new(),
                    cover_id: None,
                    stickers: Vec::new(),
                    installed: true,
                });
            }
        }
        Ok(())
    }

    fn handle_chat_folder(&mut self, bytes: &[u8]) -> Result<(), String> {
        let raw = decode_chat_folder(bytes)?;
        if raw.folder_type == 1 || raw.name.trim().is_empty() {
            return Ok(());
        }
        let id = if raw.id.is_empty() {
            format!("f-{}", self.folders.len())
        } else {
            raw.id
        };
        let included = raw
            .included_recipients
            .iter()
            .filter_map(|&rid| self.recipient_peer(rid))
            .collect();
        let excluded = raw
            .excluded_recipients
            .iter()
            .filter_map(|&rid| self.recipient_peer(rid))
            .collect();
        self.folders.push(wire::ChatFolder {
            id,
            name: raw.name,
            position: self.folders.len() as u32,
            show_only_unread: raw.show_only_unread,
            show_muted: raw.show_muted,
            include_all_individual: raw.include_all_individual,
            include_all_groups: raw.include_all_groups,
            included,
            excluded,
        });
        Ok(())
    }

    fn finalize(&mut self) {
        // Persist folders and per-conversation filing (archive, pinned order, folders).
        self.pinned_chats.sort_by_key(|(order, _)| *order);
        let pinned_ids: Vec<String> = self.pinned_chats.iter().map(|(_, id)| id.clone()).collect();
        let folder_rows: Vec<FolderRow> = self
            .folders
            .iter()
            .enumerate()
            .map(|(at, f)| FolderRow {
                id: f.id.clone(),
                name: f.name.clone(),
                position: at as i32,
                color: None,
                spaces: Vec::new(),
                pinned: Vec::new(),
            })
            .collect();

        {
            let mut store = self.hub.store();
            if !folder_rows.is_empty() {
                let _ = store.replace_folders(self.account, &folder_rows);
            }
            let now_ms = (self.now_secs.max(0) as u64) * 1_000;
            for chat in self.chats.values() {
                let is_group = matches!(chat.peer, Peer::Group(_));
                let muted = chat.mute_until_ms > now_ms;
                let folders = self
                    .folders
                    .iter()
                    .filter(|f| super::folder_holds(f, &chat.conversation_id, is_group, muted, chat.marked_unread))
                    .map(|f| f.id.clone())
                    .collect();
                let filing = Filing {
                    folders,
                    archived: chat.archived,
                    pin: pinned_ids.iter().position(|id| id == &chat.conversation_id).map(|at| at as i32),
                    hidden: false,
                    followed: false,
                };
                if filing.archived || filing.pin.is_some() || !filing.folders.is_empty() {
                    let _ = store.set_filing(&key_for(self.account, &chat.conversation_id), &filing);
                }
            }
        }

        // Persist imported attachment pointers for on-demand download (`Command::Wanted`).
        if !self.kept_attachments.is_empty() {
            let path = attachments_path(self.me_aci);
            let mut existing = read_kept_attachments(&path);
            let mut by_id: HashMap<String, KeptAttachment> = existing
                .drain(..)
                .map(|a| (a.id.clone(), a))
                .collect();
            for att in self.kept_attachments.drain(..) {
                by_id.insert(att.id.clone(), att);
            }
            let mut merged: Vec<KeptAttachment> = by_id.into_values().collect();
            merged.sort_by(|a, b| a.id.cmp(&b.id));
            if merged.len() > super::REACTIONS_KEPT {
                let extra = merged.len() - super::REACTIONS_KEPT;
                merged.drain(..extra);
            }
            write_kept_attachments(&path, &merged);
        }

        // Merge imported reactions into `{aci}.reactions` so live reaction updates align.
        if !self.kept_reactions.is_empty() {
            let path = super::reactions_path(self.me_aci);
            let existing = super::read_reactions(&path);
            let mut order = std::collections::VecDeque::new();
            let mut book = HashMap::new();
            for row in existing.into_iter().chain(self.kept_reactions.drain(..)) {
                let key = (row.message, row.person);
                if !book.contains_key(&key) {
                    while order.len() >= super::REACTIONS_KEPT {
                        if let Some(old) = order.pop_front() {
                            book.remove(&old);
                        }
                    }
                    order.push_back(key.clone());
                }
                book.insert(key, row.emoji);
            }
            super::write_reactions(&path, &order, &book);
        }

        // Merge imported pinned messages into `{aci}.pins`.
        if !self.kept_pins.is_empty() {
            let path = super::pins_path(self.me_aci);
            let mut rows = super::read_pins(&path);
            for pin in self.kept_pins.drain(..) {
                rows.retain(|p| !(p.conversation == pin.conversation && p.message_id == pin.message_id));
                rows.push(pin);
            }
            if rows.len() > 512 {
                let extra = rows.len() - 512;
                rows.drain(..extra);
            }
            super::write_pins(&path, &rows);
        }

        // Merge imported sticker packs into `{aci}.stickers`.
        if !self.kept_stickers.is_empty() {
            let path = super::stickers_path(self.me_aci);
            let mut packs = super::read_sticker_packs(&path);
            for pack in self.kept_stickers.drain(..) {
                if !packs.iter().any(|p| p.id == pack.id) && packs.len() < 128 {
                    packs.push(pack);
                }
            }
            super::write_sticker_packs(&path, &packs);
        }

        // Notify retention thread and connected shells.
        if self.any_expiring {
            self.hub.expiring();
        }
        for conv_id in &self.touched_conversations {
            let key = key_for(self.account, conv_id);
            self.hub.changed(Change::Messages { conversation: key.clone() });
            self.hub.changed(Change::Conversation { conversation: key });
        }
        if !self.touched_conversations.is_empty() {
            self.hub.changed(Change::People { account: self.account.clone(), person: None });
            self.hub.changed(Change::Status);
        }
    }
}

/// Streams, authenticates, decrypts, decompresses, parses, and idempotently imports a Signal
/// message history transfer archive at `archive_path` into `hub`'s local store.
///
/// Calls `on_progress(chats_imported, messages_imported)` periodically and stops promptly if
/// `skip` becomes true. Never loads the whole archive into memory.
#[allow(clippy::too_many_arguments)]
pub fn import_archive(
    hub: &Arc<Hub>,
    account: &AccountKey,
    me_aci: &str,
    me_name: &str,
    ephemeral_key: &[u8; 32],
    archive_path: &Path,
    on_progress: &dyn Fn(u32, u32),
    skip: &AtomicBool,
) -> Result<ImportSummary, String> {
    let backup_id = derive_backup_id(ephemeral_key, me_aci)?;
    let keys = derive_message_backup_key(ephemeral_key, &backup_id, None);
    let (enc_start, body_len) = verify_archive_file(archive_path, &keys.hmac_key)?;
    if skip.load(Ordering::Relaxed) {
        return Ok(ImportSummary { skipped_by_user: true, ..ImportSummary::default() });
    }

    let aes_reader = AesCbcFileReader::open(archive_path, enc_start, body_len, &keys.aes_key)?;
    let gzip_reader = GzipStreamReader::new(aes_reader);
    let mut stream = BufReader::with_capacity(CHUNK_SIZE, gzip_reader);

    // Frame 0 is BackupInfo.
    let info_bytes = read_next_frame(&mut stream)?
        .ok_or_else(|| "Transfer archive is empty (missing BackupInfo).".to_owned())?;
    let _info = decode_backup_info(&info_bytes)?;

    let mut state = ImportState {
        hub,
        account,
        me_aci,
        me_name: me_name.to_owned(),
        now_secs: crate::hub::now(),
        recipients: HashMap::new(),
        chats: HashMap::new(),
        names_by_aci: HashMap::new(),
        pinned_chats: Vec::new(),
        folders: Vec::new(),
        kept_attachments: Vec::new(),
        kept_attachment_ids: HashSet::new(),
        kept_reactions: Vec::new(),
        kept_pins: Vec::new(),
        kept_stickers: Vec::new(),
        touched_conversations: HashSet::new(),
        any_expiring: false,
        summary: ImportSummary::default(),
    };

    let mut last_reported_msgs = 0u32;
    let mut last_reported_chats = 0u32;

    while let Some(frame_bytes) = read_next_frame(&mut stream)? {
        if skip.load(Ordering::Relaxed) {
            state.summary.skipped_by_user = true;
            break;
        }
        let mut fc = PbCursor::new(&frame_bytes);
        while let Some((field, val)) = fc.next_field()? {
            let WireValue::Bytes(payload) = val else {
                continue;
            };
            match field {
                1 => {
                    // AccountData: capture our display name if missing.
                    if state.me_name.is_empty() {
                        let mut ac = PbCursor::new(payload);
                        let mut given = String::new();
                        let mut family = String::new();
                        while let Some((af, av)) = ac.next_field()? {
                            match (af, av) {
                                (4, WireValue::Bytes(s)) => given = pb_string(s),
                                (5, WireValue::Bytes(s)) => family = pb_string(s),
                                _ => {}
                            }
                        }
                        let full = [given.trim(), family.trim()]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !full.is_empty() {
                            state.me_name = full;
                        }
                    }
                }
                2 => state.handle_recipient(payload)?,
                3 => state.handle_chat(payload)?,
                4 => state.handle_chat_item(payload)?,
                5 => state.handle_sticker_pack(payload)?,
                8 => state.handle_chat_folder(payload)?,
                _ => {}
            }
        }
        if state.summary.chats != last_reported_chats
            || state.summary.messages.saturating_sub(last_reported_msgs) >= 25
        {
            last_reported_chats = state.summary.chats;
            last_reported_msgs = state.summary.messages;
            on_progress(state.summary.chats, state.summary.messages);
        }
    }

    state.finalize();
    on_progress(state.summary.chats, state.summary.messages);
    Ok(state.summary)
}

/// Builds a small valid Signal history transfer archive for tests and e2e prove drivers.
#[allow(clippy::too_many_arguments)]
pub fn build_fixture_archive(
    ephemeral_key: &[u8; 32],
    me_aci: &str,
    peer_aci: &str,
    peer_name: &str,
    group_master_key: &[u8; 32],
    group_title: &str,
    now_ms: u64,
    extra_messages: usize,
) -> Result<(Vec<u8>, String), String> {
    let peer_uuid = wire::aci_uuid_bytes(peer_aci).ok_or("bad peer aci")?;
    let me_uuid = wire::aci_uuid_bytes(me_aci).ok_or("bad me aci")?;
    let group_id = derive_group_id(group_master_key);

    let mut info = Vec::new();
    pb_varint(&mut info, 1, 1); // version = 1
    pb_varint(&mut info, 2, now_ms); // backupTimeMs

    let mut frames = Vec::new();

    // Frame 1: AccountData
    {
        let mut acct = Vec::new();
        pb_str(&mut acct, 4, "Ada");
        pb_str(&mut acct, 5, "Quill");
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 1, &acct);
        frames.push(frame);
    }

    // Frame 2: Recipient 1 = Self
    {
        let mut rec = Vec::new();
        pb_varint(&mut rec, 1, 1);
        pb_bytes(&mut rec, 5, &[]);
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 2, &rec);
        frames.push(frame);
    }

    // Frame 3: Recipient 2 = Peer Contact
    {
        let mut contact = Vec::new();
        pb_bytes(&mut contact, 1, &peer_uuid);
        pb_str(&mut contact, 3, "grace.42");
        pb_str(&mut contact, 11, peer_name);
        pb_varint(&mut contact, 15, 1); // Verified
        let mut rec = Vec::new();
        pb_varint(&mut rec, 1, 2);
        pb_bytes(&mut rec, 2, &contact);
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 2, &rec);
        frames.push(frame);
    }

    // Frame 4: Recipient 3 = Group
    {
        let mut title_blob = Vec::new();
        pb_str(&mut title_blob, 1, group_title);
        let mut m_me = Vec::new();
        pb_bytes(&mut m_me, 1, &me_uuid);
        pb_varint(&mut m_me, 2, 2); // Admin
        let mut m_peer = Vec::new();
        pb_bytes(&mut m_peer, 1, &peer_uuid);
        pb_varint(&mut m_peer, 2, 1); // Default
        let mut snap = Vec::new();
        pb_bytes(&mut snap, 2, &title_blob);
        pb_bytes(&mut snap, 7, &m_me);
        pb_bytes(&mut snap, 7, &m_peer);
        let mut grp = Vec::new();
        pb_bytes(&mut grp, 1, group_master_key);
        pb_varint(&mut grp, 2, 1); // whitelisted
        pb_bytes(&mut grp, 5, &snap);
        let mut rec = Vec::new();
        pb_varint(&mut rec, 1, 3);
        pb_bytes(&mut rec, 3, &grp);
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 2, &rec);
        frames.push(frame);
    }

    // Frame 5: Chat 10 (Direct with Peer, pinned order 0, timer 3600s)
    {
        let mut chat = Vec::new();
        pb_varint(&mut chat, 1, 10);
        pb_varint(&mut chat, 2, 2);
        pb_varint(&mut chat, 4, 0); // pinnedOrder
        pb_varint(&mut chat, 5, 3_600_000); // 1 hour timer
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 3, &chat);
        frames.push(frame);
    }

    // Frame 6: Chat 11 (Group)
    {
        let mut chat = Vec::new();
        pb_varint(&mut chat, 1, 11);
        pb_varint(&mut chat, 2, 3);
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 3, &chat);
        frames.push(frame);
    }

    // ChatItem 1: Incoming message in Chat 10 with bold style, attachment pointer, reaction, and edit revision
    let t1 = now_ms.saturating_sub(600_000);
    {
        let mut range = Vec::new();
        pb_varint(&mut range, 1, 0);
        pb_varint(&mut range, 2, 5);
        pb_varint(&mut range, 4, 1); // BOLD
        let mut text = Vec::new();
        pb_str(&mut text, 1, "Hello from before linking");
        pb_bytes(&mut text, 2, &range);

        let mut loc = Vec::new();
        pb_bytes(&mut loc, 1, &[0x33u8; 64]);
        pb_varint(&mut loc, 3, 128);
        pb_str(&mut loc, 4, "hist-photo-1");
        pb_varint(&mut loc, 5, 3);
        let mut fp = Vec::new();
        pb_str(&mut fp, 4, "image/png");
        pb_str(&mut fp, 7, "trip.png");
        pb_varint(&mut fp, 8, 640);
        pb_varint(&mut fp, 9, 480);
        pb_str(&mut fp, 11, "00TI:j");
        pb_bytes(&mut fp, 13, &loc);
        let mut matt = Vec::new();
        pb_bytes(&mut matt, 1, &fp);

        let mut react = Vec::new();
        pb_str(&mut react, 1, "❤️");
        pb_varint(&mut react, 2, 1); // me
        pb_varint(&mut react, 4, 1);

        let mut std_msg = Vec::new();
        pb_bytes(&mut std_msg, 2, &text);
        pb_bytes(&mut std_msg, 3, &matt);
        pb_bytes(&mut std_msg, 6, &react);

        // Prior revision before edit
        let mut prev_text = Vec::new();
        pb_str(&mut prev_text, 1, "Hello typo");
        let mut prev_std = Vec::new();
        pb_bytes(&mut prev_std, 2, &prev_text);
        let mut prev_item = Vec::new();
        pb_varint(&mut prev_item, 1, 10);
        pb_varint(&mut prev_item, 2, 2);
        pb_varint(&mut prev_item, 3, t1);
        pb_bytes(&mut prev_item, 8, &[]);
        pb_bytes(&mut prev_item, 11, &prev_std);

        let mut ci = Vec::new();
        pb_varint(&mut ci, 1, 10);
        pb_varint(&mut ci, 2, 2);
        pb_varint(&mut ci, 3, t1);
        pb_bytes(&mut ci, 6, &prev_item);
        pb_bytes(&mut ci, 8, &[]); // incoming
        pb_bytes(&mut ci, 11, &std_msg);

        let mut frame = Vec::new();
        pb_bytes(&mut frame, 4, &ci);
        frames.push(frame);
    }

    // ChatItem 2: Outgoing quote reply in Chat 10 with active disappearing timer (expires in future)
    let t2 = now_ms.saturating_sub(300_000);
    {
        let mut qtext = Vec::new();
        pb_str(&mut qtext, 1, "Hello from before linking");
        let mut quote = Vec::new();
        pb_varint(&mut quote, 1, t1);
        pb_varint(&mut quote, 2, 2);
        pb_bytes(&mut quote, 3, &qtext);

        let mut text = Vec::new();
        pb_str(&mut text, 1, "Glad history came over!");
        let mut std_msg = Vec::new();
        pb_bytes(&mut std_msg, 1, &quote);
        pb_bytes(&mut std_msg, 2, &text);

        let mut ci = Vec::new();
        pb_varint(&mut ci, 1, 10);
        pb_varint(&mut ci, 2, 1);
        pb_varint(&mut ci, 3, t2);
        pb_varint(&mut ci, 4, now_ms); // expireStartDate = now
        pb_varint(&mut ci, 5, 3_600_000); // expiresInMs = 1h (active!)
        pb_bytes(&mut ci, 9, &[]); // outgoing
        pb_bytes(&mut ci, 11, &std_msg);

        let mut frame = Vec::new();
        pb_bytes(&mut frame, 4, &ci);
        frames.push(frame);
    }

    // ChatItem 3: Already-expired disappearing message in Chat 10 (MUST be skipped!)
    let t3 = now_ms.saturating_sub(120_000);
    {
        let mut text = Vec::new();
        pb_str(&mut text, 1, "Should not survive expiry");
        let mut std_msg = Vec::new();
        pb_bytes(&mut std_msg, 2, &text);
        let mut ci = Vec::new();
        pb_varint(&mut ci, 1, 10);
        pb_varint(&mut ci, 2, 2);
        pb_varint(&mut ci, 3, t3);
        pb_varint(&mut ci, 4, t3);
        pb_varint(&mut ci, 5, 30_000); // expired 90s ago
        pb_bytes(&mut ci, 8, &[]);
        pb_bytes(&mut ci, 11, &std_msg);

        let mut frame = Vec::new();
        pb_bytes(&mut frame, 4, &ci);
        frames.push(frame);
    }

    // ChatItem 4: Group poll in Chat 11
    let t4 = now_ms.saturating_sub(200_000);
    {
        let mut vote = Vec::new();
        pb_varint(&mut vote, 1, 2);
        pb_varint(&mut vote, 2, 1);
        let mut opt0 = Vec::new();
        pb_str(&mut opt0, 1, "Tuesday");
        pb_bytes(&mut opt0, 2, &vote);
        let mut opt1 = Vec::new();
        pb_str(&mut opt1, 1, "Thursday");
        let mut poll = Vec::new();
        pb_str(&mut poll, 1, "Next meeting?");
        pb_bytes(&mut poll, 3, &opt0);
        pb_bytes(&mut poll, 3, &opt1);
        let mut ci = Vec::new();
        pb_varint(&mut ci, 1, 11);
        pb_varint(&mut ci, 2, 2);
        pb_varint(&mut ci, 3, t4);
        pb_bytes(&mut ci, 8, &[]);
        pb_bytes(&mut ci, 20, &poll);
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 4, &ci);
        frames.push(frame);
    }

    // Optional extra messages (e.g. for multi-chunk streaming tests)
    for i in 0..extra_messages {
        let ts = now_ms.saturating_sub(500_000).saturating_add(i as u64 + 1);
        let mut text = Vec::new();
        pb_str(&mut text, 1, &format!("Historical batch message #{i:04} — streaming check"));
        let mut std_msg = Vec::new();
        pb_bytes(&mut std_msg, 2, &text);
        let mut ci = Vec::new();
        pb_varint(&mut ci, 1, 11);
        pb_varint(&mut ci, 2, 2);
        pb_varint(&mut ci, 3, ts);
        pb_bytes(&mut ci, 8, &[]);
        pb_bytes(&mut ci, 11, &std_msg);
        let mut frame = Vec::new();
        pb_bytes(&mut frame, 4, &ci);
        frames.push(frame);
    }

    let archive = encode_encrypted_archive(ephemeral_key, me_aci, &info, &frames, [0x42u8; 16], 37)?;
    Ok((archive, group_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use protocol::{Network, Page};

    fn hex_to_array<const N: usize>(s: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for i in 0..N {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn key_derivation_matches_libsignal_message_backup_test_vectors() {
        // Exact test vector from libsignal/rust/account-keys/src/backup.rs & message-backup/src/key.rs
        let backup_key: [u8; 32] =
            hex_to_array("ea26a2ddb5dba5ef9e34e1b8dea1f5ae7f255306a6d2d883e542306eaa9fe985");
        let aci = "659aa5f4-a28d-fcc1-1ea1-b997537a3d95";
        let backup_id = derive_backup_id(&backup_key, aci).unwrap();
        assert_eq!(hex_lower(&backup_id), "8a624fbc45379043f39f1391cddc5fe8");

        let legacy = derive_message_backup_key(&backup_key, &backup_id, None);
        assert_eq!(
            hex_lower(&legacy.hmac_key),
            "f425e22a607c529717e1e1b29f9fe139f9d1c7e7d01e371c7753c544a3026649"
        );
        assert_eq!(
            hex_lower(&legacy.aes_key),
            "e143f4ad5668d8bfed2f88562f0693f53bda2c0e55c9d71730f30e24695fd6df"
        );

        let token: [u8; 32] =
            hex_to_array("69207061737320746865206b6e69666520746f207468652061786f6c6f746c21");
        let modern = derive_message_backup_key(&backup_key, &backup_id, Some(&token));
        assert_eq!(
            hex_lower(&modern.hmac_key),
            "20e6ab57b87e051f3e695e953cf8a261dd307e4f92ae2921673f1d397e07887b"
        );
        assert_eq!(
            hex_lower(&modern.aes_key),
            "602af6ecfc09d695a8d58da9f18225e967979c5e03543ca0224a03cca3d9735e"
        );
    }

    #[cfg(feature = "signal")]
    #[test]
    fn group_id_matches_zkgroup_bit_for_bit() {
        use presage::libsignal_service::prelude::{GroupMasterKey, GroupSecretParams};
        let mk = [0x5au8; 32];
        let expected = hex_lower(&GroupSecretParams::derive_from_master_key(GroupMasterKey::new(mk)).get_group_identifier());
        assert_eq!(derive_group_id(&mk), expected);
    }

    #[test]
    fn streaming_import_is_idempotent_handles_expiry_attachments_and_rejects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.db")).unwrap();
        let hub = Arc::new(Hub::new(store));
        let me = "a1a1a1a1-0000-4000-8000-000000000001";
        let peer = "b2b2b2b2-0000-4000-8000-000000000002";
        let account = AccountKey { network: Network::Signal, account: me.into() };
        let key = [0x7bu8; 32];
        let mk = [0x22u8; 32];
        let now_ms = (crate::hub::now() as u64) * 1_000;

        let (archive_bytes, gid) =
            build_fixture_archive(&key, me, peer, "Grace Ombra", &mk, "Bridge club", now_ms, 80).unwrap();
        let archive_path = dir.path().join("transfer.bin");
        std::fs::write(&archive_path, &archive_bytes).unwrap();

        let skip = AtomicBool::new(false);
        let progress_calls = std::sync::atomic::AtomicUsize::new(0);
        let summary = import_archive(
            &hub,
            &account,
            me,
            "Ada Quill",
            &key,
            &archive_path,
            &|_, _| {
                progress_calls.fetch_add(1, Ordering::Relaxed);
            },
            &skip,
        )
        .expect("valid import");
        assert_eq!(summary.chats, 2);
        assert_eq!(summary.messages, 83); // 2 in direct + 1 poll + 80 extra in group (expired skipped)
        assert_eq!(summary.new_messages, 83);
        assert_eq!(summary.skipped_expired, 1);
        assert_eq!(summary.attachments, 1);
        assert!(!summary.skipped_by_user);
        assert!(progress_calls.load(Ordering::Relaxed) >= 2);

        // Verify direct chat messages, IDs, styles, quote, edit, reaction, attachment, and active timer.
        let dm_msgs = hub
            .store()
            .messages(&key_for(&account, peer), &Page::Newest, 50, crate::hub::now())
            .unwrap()
            .map(|p| p.messages)
            .unwrap_or_default();
        assert_eq!(dm_msgs.len(), 2, "expired disappearing message must not be imported");
        let first = dm_msgs.iter().find(|m| m.text == "Hello from before linking").expect("first msg");
        assert!(first.id.ends_with(&format!("-{peer}")));
        assert!(first.edited);
        assert_eq!(first.prior_text.as_deref(), Some("Hello typo"));
        assert_eq!(first.reactions.len(), 1);
        assert_eq!(first.reactions[0].emoji, "❤️");
        assert_eq!(first.attachments.len(), 1);
        assert_eq!(first.attachments[0].name, "trip.png");
        assert!(!first.spans.is_empty());

        let second = dm_msgs.iter().find(|m| m.text == "Glad history came over!").expect("second msg");
        assert!(second.outgoing);
        assert_eq!(second.reply_to.as_ref().map(|q| q.id.as_str()), Some(first.id.as_str()));
        assert!(second.expires_at.is_some_and(|exp| exp > crate::hub::now()));

        // Verify group poll imported with vote.
        let grp_conv = Peer::Group(gid).conversation();
        let grp_msgs = hub
            .store()
            .messages(&key_for(&account, &grp_conv), &Page::Newest, 100, crate::hub::now())
            .unwrap()
            .map(|p| p.messages)
            .unwrap_or_default();
        assert_eq!(grp_msgs.len(), 81);
        let poll_msg = grp_msgs.iter().find(|m| m.poll.is_some()).expect("poll");
        let poll = poll_msg.poll.as_ref().unwrap();
        assert_eq!(poll.question, "Next meeting?");
        assert_eq!(poll.choices[0].count, 1);

        // Re-importing the exact same archive is 100% idempotent.
        let again = import_archive(&hub, &account, me, "Ada Quill", &key, &archive_path, &|_, _| {}, &skip)
            .expect("idempotent re-import");
        assert_eq!(again.new_messages, 0);
        assert_eq!(
            hub.store()
                .messages(&key_for(&account, peer), &Page::Newest, 50, crate::hub::now())
                .unwrap()
                .map(|p| p.messages.len())
                .unwrap_or(0),
            2
        );

        // Tampered archive is rejected before touching the store.
        let mut tampered = archive_bytes;
        tampered[24] ^= 0xff;
        let bad_path = dir.path().join("tampered.bin");
        std::fs::write(&bad_path, &tampered).unwrap();
        let err = import_archive(&hub, &account, me, "Ada Quill", &key, &bad_path, &|_, _| {}, &skip)
            .expect_err("tampered HMAC must fail");
        assert!(err.contains("HMAC"), "{err}");
    }
}
