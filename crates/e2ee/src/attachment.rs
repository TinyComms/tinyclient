//! The sealed attachment blob (`docs/e2ee-text.md`, section 5, "Attachments").
//!
//! ```text
//! "TNYA1" || log2(chunk size) (1 byte, 16) || nonce prefix (7 random bytes)
//!         || chunk_0 || chunk_1 || ... || chunk_n
//! ```
//!
//! Every chunk is the AES-256-GCM encryption of up to 64 KiB of the file, in the STREAM
//! construction of Hoang, Reyhanitabar, Rogaway and Vizar as RustCrypto's `aead::stream`
//! implements it (`EncryptorBE32`): the GCM nonce of a chunk is `prefix || counter (u32, big
//! endian) || last-chunk flag (1 byte)`. All chunks but the last carry exactly 64 KiB, the last
//! carries 0 to 64 KiB, and each is 16 bytes longer than its plaintext (the tag). The first
//! chunk's associated data is `header || context`; the others have none. `context` is what the
//! caller binds the blob to (the attachment's id, if it is known when the blob is made; see
//! `docs/e2ee-core-notes.md`).
//!
//! The key is 256 random bits per attachment and lives only inside the message. The STREAM
//! counter detects a chunk moved, repeated or dropped from the middle, the last-chunk flag a blob
//! cut short (or one with more appended), and GCM any changed bit, the wrong key or the wrong
//! context. Both directions hold two chunks in memory (the next one is read to know whether the
//! current one is the last) and nothing else, so memory does not grow with the file.
//!
//! **Plaintext leaves chunk by chunk, before the end of the blob has been seen.** A blob cut at a
//! chunk boundary fails only at its end. So the writer a caller passes to [`open`] receives data
//! that may still turn out to be a prefix: a caller that gets an error must discard everything
//! written. [`decrypt_to_temp`] does that: it writes a private temp file in the caller's
//! directory and removes it unless the whole blob authenticated and the caller `persist`s it.
//! [`open_verified`] checks the sealed blob's SHA-256 against the message's before it
//! decrypts a byte.

use crate::error::{Error, Result};
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::stream::{DecryptorBE32, EncryptorBE32};
use aes_gcm::aead::KeyInit;
use aes_gcm::Aes256Gcm;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const MAGIC: &[u8; 5] = b"TNYA1";
pub const HEADER_LEN: usize = 13;
/// What this crate writes: 64 KiB chunks.
pub const CHUNK_LOG2: u8 = 16;
const MIN_CHUNK_LOG2: u8 = 10;
const MAX_CHUNK_LOG2: u8 = 20;
const TAG_LEN: usize = 16;
const NONCE_PREFIX_LEN: usize = 7;

/// The per-attachment AES-256 key. Zeroed when dropped; equality is constant time; never printed.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct AttachmentKey([u8; 32]);

impl AttachmentKey {
    pub fn generate() -> Result<AttachmentKey> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Random)?;
        Ok(AttachmentKey(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> AttachmentKey {
        AttachmentKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl PartialEq for AttachmentKey {
    fn eq(&self, other: &AttachmentKey) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for AttachmentKey {}

impl std::fmt::Debug for AttachmentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttachmentKey(..)")
    }
}

/// The 13 byte header of a sealed blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub chunk_log2: u8,
    pub nonce_prefix: [u8; NONCE_PREFIX_LEN],
}

impl Header {
    /// Parses and checks a header: the magic, a chunk size between 1 KiB and 1 MiB (bounds the
    /// memory a blob can ask for), exactly 13 bytes. Never panics.
    pub fn parse(bytes: &[u8]) -> Result<Header> {
        if bytes.len() < HEADER_LEN || &bytes[..5] != MAGIC {
            return Err(Error::Malformed("attachment header"));
        }
        let chunk_log2 = bytes[5];
        if !(MIN_CHUNK_LOG2..=MAX_CHUNK_LOG2).contains(&chunk_log2) {
            return Err(Error::Malformed("attachment chunk size"));
        }
        let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
        nonce_prefix.copy_from_slice(&bytes[6..HEADER_LEN]);
        Ok(Header {
            chunk_log2,
            nonce_prefix,
        })
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..5].copy_from_slice(MAGIC);
        out[5] = self.chunk_log2;
        out[6..].copy_from_slice(&self.nonce_prefix);
        out
    }

    pub fn chunk_len(&self) -> usize {
        1usize << self.chunk_log2
    }
}

/// What sealing produced; `sha256` and the lengths go in the message's attachment entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealInfo {
    /// SHA-256 of the whole sealed blob, header included: what the server stores.
    pub sha256: [u8; 32],
    pub sealed_len: u64,
    pub plain_len: u64,
}

/// The length of the sealed blob for `plain_len` bytes of file, at 64 KiB chunks.
pub fn sealed_len(plain_len: u64) -> u64 {
    let chunk = 1u64 << CHUNK_LOG2;
    // A file of exact multiples still ends in a full last chunk; an empty file has one empty chunk.
    let chunks = (plain_len / chunk) + 1;
    let chunks = if plain_len > 0 && plain_len.is_multiple_of(chunk) {
        chunks - 1
    } else {
        chunks
    };
    HEADER_LEN as u64 + plain_len + chunks * TAG_LEN as u64
}

/// What `sealed_len` inverts: the plaintext length of a blob of this size, or `None` if no file
/// seals to this size.
pub fn plain_len_of(sealed_len_value: u64) -> Option<u64> {
    let body = sealed_len_value.checked_sub(HEADER_LEN as u64)?;
    let full = (1u64 << CHUNK_LOG2) + TAG_LEN as u64;
    let chunks = body.div_ceil(full).max(1);
    let plain = body.checked_sub(chunks * TAG_LEN as u64)?;
    (sealed_len(plain) == sealed_len_value).then_some(plain)
}

/// Reads until `buffer` has `want` bytes or the reader is at its end.
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

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|err| Error::Io(err.to_string()))
}

fn cipher(key: &AttachmentKey) -> Aes256Gcm {
    Aes256Gcm::new(GenericArray::from_slice(key.as_bytes()))
}

fn first_aad(header: &[u8], context: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + context.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(context);
    aad
}

/// Seals everything `reader` yields into `writer`, with a fresh random nonce prefix. Memory is two
/// 64 KiB buffers whatever the length.
pub fn seal<R: Read, W: Write>(
    key: &AttachmentKey,
    context: &[u8],
    reader: R,
    writer: W,
) -> Result<SealInfo> {
    let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
    getrandom::fill(&mut nonce_prefix).map_err(|_| Error::Random)?;
    seal_with_header(
        key,
        context,
        Header {
            chunk_log2: CHUNK_LOG2,
            nonce_prefix,
        },
        reader,
        writer,
    )
}

/// [`seal`] with a given header (deterministic, for known-answer tests). A nonce prefix must
/// never be used twice with one key: callers outside tests use [`seal`].
pub fn seal_with_header<R: Read, W: Write>(
    key: &AttachmentKey,
    context: &[u8],
    header: Header,
    mut reader: R,
    mut writer: W,
) -> Result<SealInfo> {
    let chunk = header.chunk_len();
    let header_bytes = header.to_bytes();
    let mut hasher = Sha256::new();
    io(writer.write_all(&header_bytes))?;
    hasher.update(header_bytes);
    let mut sealed_len = HEADER_LEN as u64;
    let mut plain_len = 0u64;

    let aad_first = first_aad(&header_bytes, context);
    let mut encryptor = Some(EncryptorBE32::from_aead(
        cipher(key),
        GenericArray::from_slice(&header.nonce_prefix),
    ));
    let mut current: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(chunk + TAG_LEN));
    let mut next: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(chunk + TAG_LEN));
    fill(&mut reader, &mut current, chunk)?;
    let mut first = true;
    loop {
        let last = if current.len() < chunk {
            true
        } else {
            fill(&mut reader, &mut next, chunk)?;
            next.is_empty()
        };
        plain_len += current.len() as u64;
        let aad: &[u8] = if first { &aad_first } else { &[] };
        if last {
            encryptor
                .take()
                .ok_or(Error::Authentication)?
                .encrypt_last_in_place(aad, &mut *current)
                .map_err(|_| Error::Authentication)?;
        } else {
            encryptor
                .as_mut()
                .ok_or(Error::Authentication)?
                .encrypt_next_in_place(aad, &mut *current)
                .map_err(|_| Error::Authentication)?;
        }
        io(writer.write_all(&current))?;
        hasher.update(&*current);
        sealed_len += current.len() as u64;
        if last {
            break;
        }
        std::mem::swap(&mut current, &mut next);
        first = false;
    }
    io(writer.flush())?;
    Ok(SealInfo {
        sha256: hasher.finalize().into(),
        sealed_len,
        plain_len,
    })
}

/// What a caller already knows about a blob and wants checked while it is opened.
#[derive(Debug, Clone, Copy, Default)]
pub struct Expect<'a> {
    /// SHA-256 of the sealed blob, from the message. Checked, in constant time, once the whole
    /// blob has been read.
    pub sha256: Option<&'a [u8; 32]>,
    /// Length of the plaintext, from the message.
    pub plain_len: Option<u64>,
}

/// What opening produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenInfo {
    pub sha256: [u8; 32],
    pub plain_len: u64,
}

/// Opens a sealed blob from `reader` into `writer`, chunk by chunk. Any wrong key, wrong
/// `context`, changed bit, cut, reorder or repeat is [`Error::Authentication`]. See the module
/// notes: on an error the bytes already written are not to be used.
pub fn open<R: Read, W: Write>(
    key: &AttachmentKey,
    context: &[u8],
    mut reader: R,
    mut writer: W,
    expect: Expect<'_>,
) -> Result<OpenInfo> {
    let mut header_bytes = [0u8; HEADER_LEN];
    {
        let mut buffer = Vec::with_capacity(HEADER_LEN);
        fill(&mut reader, &mut buffer, HEADER_LEN)?;
        if buffer.len() < HEADER_LEN {
            return Err(Error::Malformed("attachment header"));
        }
        header_bytes.copy_from_slice(&buffer);
    }
    let header = Header::parse(&header_bytes)?;
    let sealed_chunk = header.chunk_len() + TAG_LEN;
    let mut hasher = Sha256::new();
    hasher.update(header_bytes);

    let aad_first = first_aad(&header_bytes, context);
    let mut decryptor = Some(DecryptorBE32::from_aead(
        cipher(key),
        GenericArray::from_slice(&header.nonce_prefix),
    ));
    let mut current: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(sealed_chunk));
    let mut next: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(sealed_chunk));
    fill(&mut reader, &mut current, sealed_chunk)?;
    if current.len() < TAG_LEN {
        // Not even one tag: a blob cut inside its only chunk, or inside the header's follower.
        return Err(Error::Authentication);
    }
    let mut plain_len = 0u64;
    let mut first = true;
    loop {
        hasher.update(&*current);
        let last = if current.len() < sealed_chunk {
            true
        } else {
            fill(&mut reader, &mut next, sealed_chunk)?;
            next.is_empty()
        };
        if !last && next.len() < TAG_LEN {
            // A following chunk too short to hold a tag: cannot be a whole chunk.
            return Err(Error::Authentication);
        }
        let aad: &[u8] = if first { &aad_first } else { &[] };
        if last {
            decryptor
                .take()
                .ok_or(Error::Authentication)?
                .decrypt_last_in_place(aad, &mut *current)
                .map_err(|_| Error::Authentication)?;
        } else {
            decryptor
                .as_mut()
                .ok_or(Error::Authentication)?
                .decrypt_next_in_place(aad, &mut *current)
                .map_err(|_| Error::Authentication)?;
        }
        io(writer.write_all(&current))?;
        plain_len += current.len() as u64;
        if last {
            break;
        }
        std::mem::swap(&mut current, &mut next);
        first = false;
    }
    io(writer.flush())?;
    let sha256: [u8; 32] = hasher.finalize().into();
    if let Some(expected) = expect.sha256 {
        if !bool::from(sha256.ct_eq(expected)) {
            return Err(Error::Authentication);
        }
    }
    if expect
        .plain_len
        .is_some_and(|expected| expected != plain_len)
    {
        return Err(Error::Authentication);
    }
    Ok(OpenInfo { sha256, plain_len })
}

/// SHA-256 of everything `reader` yields.
pub fn sha256_of<R: Read>(mut reader: R) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(Error::Io(err.to_string())),
        }
    }
    Ok(hasher.finalize().into())
}

/// [`open`] for a blob that can be read twice (a downloaded file): the first pass hashes the
/// sealed blob and compares it, in constant time, with `sha256` from the message; nothing is
/// decrypted, and nothing reaches `writer`, unless it matches.
pub fn open_verified<R: Read + Seek, W: Write>(
    key: &AttachmentKey,
    context: &[u8],
    mut reader: R,
    writer: W,
    sha256: &[u8; 32],
    plain_len: Option<u64>,
) -> Result<OpenInfo> {
    io(reader.seek(SeekFrom::Start(0)))?;
    let actual = sha256_of(&mut reader)?;
    if !bool::from(actual.ct_eq(sha256)) {
        return Err(Error::Authentication);
    }
    io(reader.seek(SeekFrom::Start(0)))?;
    open(
        key,
        context,
        reader,
        writer,
        Expect {
            sha256: Some(sha256),
            plain_len,
        },
    )
}

/// A decrypted attachment in a private temp file. Dropping it removes the file; only
/// [`PlainTemp::persist`] keeps it.
#[derive(Debug)]
pub struct PlainTemp {
    file: tempfile::NamedTempFile,
}

/// Temp files made by [`decrypt_to_temp`] start with this, so a crash's leftovers can be found.
pub const TEMP_PREFIX: &str = ".tnya-";

impl PlainTemp {
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Moves the file to `path` (on the same filesystem it was made on) and keeps it.
    pub fn persist(self, path: &Path) -> Result<std::fs::File> {
        self.file
            .persist(path)
            .map_err(|err| Error::Io(err.error.to_string()))
    }
}

/// Opens a sealed blob into a new private temp file in `dir` (mode 0600 on Unix, in the
/// caller's directory so a rename stays on one filesystem). If anything fails, or the call is
/// dropped, or the thread panics, the file is removed: no plaintext of a blob that did not
/// authenticate is left behind. A process killed in between leaves a `.tnya-*` file;
/// [`remove_stale_temps`] clears those at start-up.
pub fn decrypt_to_temp<R: Read>(
    key: &AttachmentKey,
    context: &[u8],
    reader: R,
    dir: &Path,
    expect: Expect<'_>,
) -> Result<PlainTemp> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(TEMP_PREFIX);
    let mut file = builder
        .tempfile_in(dir)
        .map_err(|err| Error::Io(err.to_string()))?;
    {
        let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file.as_file_mut());
        open(key, context, reader, &mut writer, expect)?;
        io(writer.flush())?;
    }
    io(file.as_file().sync_all())?;
    Ok(PlainTemp { file })
}

/// Removes `.tnya-*` files in `dir` older than `older_than`: temp files a killed process left.
/// Returns how many were removed.
pub fn remove_stale_temps(dir: &Path, older_than: std::time::Duration) -> Result<usize> {
    let mut removed = 0;
    let now = std::time::SystemTime::now();
    for entry in io(std::fs::read_dir(dir))? {
        let entry = io(entry)?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= older_than);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}
