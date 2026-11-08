//! End-to-end encrypted text on a Tiny server (tinyserver's `docs/e2ee-text.md`), on top of the
//! `e2ee` crate, which holds the MLS groups and does every piece of cryptography. This module is
//! the part with a network and a disk: it registers this install as a device of the signed-in
//! person, keeps the device's key packages topped up, walks each encrypted conversation's
//! handshake feed (proposals, commits, Welcomes), encrypts what is sent and opens what arrives,
//! and keeps what it opened in a sealed cache, because MLS cannot open a message twice and a
//! device cannot open its own.
//!
//! Where things are:
//!
//! - The two keys (the state key that seals the engine's snapshot and the cache key that seals
//!   each opened message) are in the account's keychain item, inside the Tiny session JSON
//!   (`Saved::e2ee`). Never on disk beside what they seal.
//! - The engine's sealed snapshot, the sealed cache and a small plain file of handshake positions
//!   and verifications are under `<support>/e2ee/Tiny-<account>/text/`, a directory only this
//!   user can read (0700 on Unix; a protected owner-only ACL on Windows).
//!
//! What the server sees is ciphertext, public handshake messages and public keys. A first device
//! is trusted on first use; a person's later devices need a cross-signature from one they already
//! have, which this module makes when the person pastes the new device's link code
//! (`E2eeAsk::Approve`).
//!
//! Known gap, said here because it is the caller's: chatd's own message store keeps what this
//! module opened in plain text, as it keeps every network's messages. Sealing that store is
//! follow-up work.

use super::Saved;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use e2ee::attachment::AttachmentKey;
use e2ee::identity::DeviceRecord;
use e2ee::store::{CacheKey, FileStore, StateKey};
use e2ee::{
    AttachmentRef, Claimed, ConversationId, DeviceId, Engine, Envelope, Error, Kind, UserId,
};
use protocol::{DeviceState, E2eeDevice, E2eePerson, E2eeStatus, Trust};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The engine's snapshot under the store.
const STATE: &str = "device.state";
/// Key packages kept on the server: refilled to this when it says it is low, or below half.
const KEY_PACKAGES: usize = 50;
/// Handshake rows asked for at once (the server gives at most 100).
const HANDSHAKE_PAGE: usize = 100;
/// Passes over the handshake feed in one sync, so a busy group cannot hold the thread for good.
const PASSES: usize = 8;
/// Maximum opened messages per conversation selected by the history exporter.
/// Local durable history must keep saving beyond this transfer limit.
const CACHED: usize = 20_000;
/// How long a send waits for the people of a conversation to join its group. The server signs
/// their Adds only after its commit batch (`e2ee_commit_batch_secs`, 30 s by default, counted from
/// when the group started), and a message sent before then is for the devices already in it,
/// which the others can never open. Past this the message goes to the group as it is.
const JOIN_WAIT: Duration = Duration::from_secs(45);
/// How often a waiting send asks again. Asking is what has the server sign the batch.
fn join_poll() -> Duration {
    // Unit tests and the lab prove use a short poll so a held commit batch finishes quickly.
    if cfg!(test) || std::env::var_os("TINYCLIENT_PROVE").is_some() {
        Duration::from_millis(10)
    } else {
        Duration::from_secs(2)
    }
}

/// The keys the keychain keeps for this device, inside the session JSON. Not `Debug`.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Secrets {
    /// Base64 of 32 bytes: the state key.
    state: String,
    /// Base64 of 32 bytes: the cache key.
    cache: String,
}

impl Secrets {
    pub(crate) fn generate() -> Result<Secrets, String> {
        let state = StateKey::generate().map_err(|err| err.to_string())?;
        let cache = CacheKey::generate().map_err(|err| err.to_string())?;
        Ok(Secrets {
            state: STANDARD.encode(state.as_bytes()),
            cache: STANDARD.encode(cache.as_bytes()),
        })
    }

    fn keys(&self) -> Option<(StateKey, CacheKey)> {
        let take = |text: &str| -> Option<[u8; 32]> { STANDARD.decode(text).ok()?.try_into().ok() };
        Some((
            StateKey::from_bytes(take(&self.state)?),
            CacheKey::from_bytes(take(&self.cache)?),
        ))
    }
}

/// The session with `secrets` set on the server `saved` names. `None` leaves it as it is.
pub(crate) fn with_secrets(session: &str, saved: &Saved, secrets: &Secrets) -> Option<String> {
    let mut value: Value = serde_json::from_str(session).ok()?;
    let secrets = serde_json::to_value(secrets).ok()?;
    let matches = |row: &Value| {
        row.get("user_id").and_then(Value::as_str) == Some(saved.user_id.as_str())
            && row.get("base_url").and_then(Value::as_str) == Some(saved.base_url.as_str())
    };
    let mut changed = false;
    if let Value::Array(rows) = &mut value {
        for row in rows
            .iter_mut()
            .filter(|row| matches(row) && row.get("e2ee").is_none())
        {
            row["e2ee"] = secrets.clone();
            changed = true;
        }
    } else if value.is_object() && matches(&value) && value.get("e2ee").is_none() {
        value["e2ee"] = secrets;
        changed = true;
    }
    changed.then(|| value.to_string())
}

/// The owner-only directory this account keeps its encrypted text state in.
fn dir_of(saved: &Saved) -> PathBuf {
    dir_for(&saved.user_id)
}

fn dir_for(user_id: &str) -> PathBuf {
    let safe: String = format!("Tiny-{user_id}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    base().join("e2ee").join(safe).join("text")
}

fn base() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("chatd-test-e2ee-text-{}", std::process::id()));
    }
    protocol::support_dir()
}

/// Makes `dir` and limits it to this user: 0700 on Unix, and on Windows a protected access list
/// for the current process user and the system (no inherited entries, so another account on the
/// machine with read access to the profile still cannot list it). The directory owner can differ
/// from the process user; owner-rights alone would lock the interactive user out.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    windows_acl::current_user_only(dir)?;
    Ok(())
}

#[cfg(windows)]
mod windows_acl {
    use std::mem::{align_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, FALSE, HANDLE, HLOCAL};
    #[cfg(test)]
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, SetFileSecurityW, TokenUser, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: opened for this guard.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    struct Local(*mut u16);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by ConvertSidToStringSidW / ConvertSecurityDescriptorToString….
                unsafe { LocalFree(self.0 as HLOCAL) };
            }
        }
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// SID string of the process token user (`TokenUser`), fail-closed on any API error.
    pub(crate) fn process_user_sid_string() -> std::io::Result<String> {
        let mut token = Handle(std::ptr::null_mut());
        // SAFETY: GetCurrentProcess is a pseudo-handle; token is closed by `Handle`.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token.0) };
        if opened == FALSE {
            return Err(std::io::Error::last_os_error());
        }
        let mut needed = 0u32;
        // SAFETY: size probe; ERROR_INSUFFICIENT_BUFFER is expected.
        unsafe {
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 || (needed as usize) < size_of::<TOKEN_USER>() {
            return Err(std::io::Error::last_os_error());
        }
        // u64 words keep the TOKEN_USER pointer field naturally aligned on 64-bit Windows.
        let words = (needed as usize + size_of::<u64>() - 1) / size_of::<u64>();
        let mut buf = vec![0u64; words];
        debug_assert!(align_of::<u64>() >= align_of::<TOKEN_USER>());
        let mut got = 0u32;
        // SAFETY: buffer is aligned for TOKEN_USER and at least `needed` bytes.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &mut got,
            )
        };
        if ok == FALSE || got == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: GetTokenInformation wrote a TOKEN_USER; Sid points into `buf`.
        let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
        if user.User.Sid.is_null() {
            return Err(std::io::Error::other("token user SID missing"));
        }
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        // SAFETY: Sid is valid until `buf` drops; string freed by `Local`.
        let converted = unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_str) };
        if converted == FALSE || sid_str.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let owned = Local(sid_str);
        // SAFETY: ConvertSidToStringSidW writes a NUL-terminated UTF-16 string.
        let wide = unsafe {
            let mut len = 0usize;
            const MAX: usize = 256;
            while len < MAX && *owned.0.add(len) != 0 {
                len += 1;
            }
            if len == MAX {
                return Err(std::io::Error::other("SID string too long"));
            }
            std::slice::from_raw_parts(owned.0, len)
        };
        String::from_utf16(wide).map_err(|_| std::io::Error::other("SID string not UTF-16"))
    }

    /// DACL-only SDDL for `path` (no owner/group), fail-closed on API errors.
    #[cfg(test)]
    pub(crate) fn dacl_sddl(path: &Path) -> std::io::Result<String> {
        let wide_path = wide(path.as_os_str());
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: path is NUL-terminated; descriptor freed below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 || descriptor.is_null() {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let result = descriptor_dacl_sddl(descriptor);
        // SAFETY: matching LocalFree for GetNamedSecurityInfoW allocation.
        unsafe { LocalFree(descriptor as HLOCAL) };
        result
    }

    /// Windows may serialize a numeric SID as an SDDL alias (for example, `LA`).
    #[cfg(test)]
    pub(crate) fn canonical_dacl_sddl(sddl: &str) -> std::io::Result<String> {
        let sddl = wide(std::ffi::OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: input is NUL-terminated; the allocated descriptor is freed below.
        let made = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if made == FALSE {
            return Err(std::io::Error::last_os_error());
        }
        let result = descriptor_dacl_sddl(descriptor);
        // SAFETY: matching LocalFree for ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(descriptor as HLOCAL) };
        result
    }

    #[cfg(test)]
    fn descriptor_dacl_sddl(descriptor: PSECURITY_DESCRIPTOR) -> std::io::Result<String> {
        let mut sddl_ptr: *mut u16 = std::ptr::null_mut();
        let mut sddl_chars = 0u32;
        // SAFETY: descriptor from GetNamedSecurityInfoW; string freed by LocalFree.
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut sddl_ptr,
                &mut sddl_chars,
            )
        };
        let convert_err = std::io::Error::last_os_error();
        if ok == FALSE || sddl_ptr.is_null() {
            return Err(convert_err);
        }
        let owned = Local(sddl_ptr);
        let cap = if sddl_chars > 0 {
            (sddl_chars as usize).min(4096)
        } else {
            4096
        };
        // SAFETY: API wrote a NUL-terminated SDDL; bound the scan by the reported length.
        let wide = unsafe {
            let mut len = 0usize;
            while len < cap && *owned.0.add(len) != 0 {
                len += 1;
            }
            if len == cap && cap == 4096 {
                return Err(std::io::Error::other("DACL SDDL too long"));
            }
            std::slice::from_raw_parts(owned.0, len)
        };
        String::from_utf16(wide).map_err(|_| std::io::Error::other("DACL SDDL not UTF-16"))
    }

    /// Protected DACL: full control for the current process user and SYSTEM. Not owner-rights
    /// (`OW`): the folder owner can be Administrators after elevated staging or restore.
    pub fn current_user_only(dir: &Path) -> std::io::Result<()> {
        let sid = process_user_sid_string()?;
        // Fail closed: refuse anything that is not a plain SID string from the token API.
        if !sid.starts_with("S-1-")
            || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || sid.len() > 256
        {
            return Err(std::io::Error::other("refusing unexpected process SID"));
        }
        let sddl = wide(std::ffi::OsStr::new(&format!(
            "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"
        )));
        let path = wide(dir.as_os_str());
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: both strings end in NUL; the descriptor is freed below.
        let made = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if made == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `descriptor` is the one just made, `path` ends in NUL.
        let set = unsafe {
            SetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            )
        };
        let error = std::io::Error::last_os_error();
        // SAFETY: made by the conversion above.
        unsafe { LocalFree(descriptor as HLOCAL) };
        if set == 0 {
            return Err(error);
        }
        Ok(())
    }
}

/// A file of this directory, written beside and moved over, readable by the owner only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no directory"))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| err.error)?;
    Ok(())
}

/// What this module remembers in the clear: positions in the handshake feeds, who was met, and
/// what was verified. Nothing that opens anything.
#[derive(Default, Serialize, Deserialize)]
struct Notes {
    /// Conversation id to the last handshake seq applied.
    #[serde(default)]
    seqs: BTreeMap<String, i64>,
    /// Users met in encrypted conversations.
    #[serde(default)]
    met: BTreeSet<String>,
    /// User id to the safety number the person compared with them.
    #[serde(default)]
    verified: BTreeMap<String, String>,
    /// People whose devices no longer chain to the ones pinned.
    #[serde(default)]
    changed: BTreeSet<String>,
    /// The recovery key's check value: a random salt and HKDF over the key with it. The key itself
    /// is shown once and never kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery: Option<(String, String)>,
    /// Conversations whose server key changed: nothing is sent there until it is looked at.
    #[serde(default)]
    key_changed: BTreeSet<String>,
    /// This person's devices as last listed: one not among them is "New device added".
    #[serde(default)]
    mine: BTreeSet<String>,
    /// The person's devices added since they last looked, for the notice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    added: Vec<E2eeDevice>,
    /// The person's own conversation (`POST /v1/e2ee/self`), where their devices talk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    self_conv: Option<String>,
    /// A confirmed removal applies to this identity, never a later fresh identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_device: Option<String>,
}

/// One opened message, as the cache keeps it.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Opened {
    #[serde(default)]
    pub text: String,
    /// Authenticated group generation for a locally opened/sent row. Legacy caches omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Authentication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_attribution: Option<cache_auth::SavedAttribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Sender's Unix milliseconds, authenticated inside the encrypted envelope. Absent in
    /// older caches; never copied to server-visible message metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<OpenedFile>,
    /// An edit: the message it edits. Its text replaces the target's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<String>,
    /// The newest edit folded into this message, by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_by: Option<String>,
    /// When it was sent (Unix seconds), as the server dated it; 0 for a message opened before
    /// this was kept. What a device link's "last N days" goes by.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: i64,
    /// Not opened here: taken in from this device of the person by a device link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// An attachment of an opened message: enough to fetch and open it again.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OpenedFile {
    pub id: String,
    /// Base64 of the 32 byte key.
    pub key: String,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    /// Base64 of SHA-256 of the sealed blob.
    pub sha256: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

impl OpenedFile {
    fn of(file: &AttachmentRef) -> OpenedFile {
        OpenedFile {
            id: file.id.clone(),
            key: STANDARD.encode(file.key.as_bytes()),
            name: file.name.clone(),
            mime: file.mime.clone(),
            bytes: file.bytes,
            sha256: STANDARD.encode(file.sha256),
            width: file.width,
            height: file.height,
        }
    }

    fn to_ref(&self) -> Option<AttachmentRef> {
        let key: [u8; 32] = STANDARD.decode(&self.key).ok()?.try_into().ok()?;
        let sha256: [u8; 32] = STANDARD.decode(&self.sha256).ok()?.try_into().ok()?;
        Some(AttachmentRef {
            id: self.id.clone(),
            key: AttachmentKey::from_bytes(key),
            name: self.name.clone(),
            mime: self.mime.clone(),
            bytes: self.bytes,
            sha256,
            width: self.width,
            height: self.height,
        })
    }

    pub fn sealed(&self) -> Option<crate::adapters::SealedFile> {
        let key: [u8; 32] = STANDARD.decode(&self.key).ok()?.try_into().ok()?;
        let sha256: [u8; 32] = STANDARD.decode(&self.sha256).ok()?.try_into().ok()?;
        Some(crate::adapters::SealedFile {
            key,
            sha256,
            plain_len: self.bytes,
            context: Vec::new(),
            mime: self.mime.clone(),
            name: self.name.clone(),
        })
    }
}

/// What one message of an encrypted conversation came to.
#[derive(Clone)]
pub(crate) enum Read {
    Opened(Opened),
    /// Saved plaintext from before sender authentication was retained. Never a wire identity.
    Legacy(Opened),
    /// An edit, folded into its target: not a row of its own.
    Edit,
    Undecryptable,
}

/// A message row as the server sends it, the fields this module reads.
pub(crate) struct Row<'a> {
    pub id: &'a str,
    pub author_id: &'a str,
    pub kind: &'a str,
    pub ciphertext: Option<&'a str>,
    pub epoch: u64,
    pub generation: Option<u64>,
    pub group_id: Option<&'a str>,
    pub device_id: &'a str,
    pub target_id: Option<&'a str>,
    pub client_id: Option<&'a str>,
    /// The server's `created_at`.
    pub created_at: &'a str,
}

/// The sealed cache of one conversation: an append-only file of `id base64(sealed)` lines, each
/// sealed under the cache key with the message id as associated data.
struct Cache {
    path: PathBuf,
    rows: HashMap<String, Opened>,
    /// Opened by client id, for our own message before the server's id is known.
    pending: HashMap<String, Opened>,
}

impl Cache {
    fn load(dir: &Path, conv: &str, key: &CacheKey) -> Cache {
        let path = dir.join(format!("cache-{conv}.log"));
        let mut rows = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let Some((id, blob)) = line.split_once(' ') else {
                    continue;
                };
                // A torn last line, or one that does not open, is skipped: never trusted.
                let Ok(blob) = STANDARD.decode(blob) else {
                    continue;
                };
                let Ok(plain) = e2ee::store::open_cache_value(key, id.as_bytes(), &blob) else {
                    continue;
                };
                if let Ok(opened) = serde_json::from_slice::<Opened>(&plain) {
                    rows.insert(id.to_owned(), opened);
                }
            }
        }
        Cache {
            path,
            rows,
            pending: HashMap::new(),
        }
    }

    fn put(&mut self, key: &CacheKey, id: &str, opened: Opened) {
        if !super::safe_id(id) {
            return;
        }
        // MLS cannot open the same incoming ciphertext again after its ratchet advances.
        // This is durable history: dropping new rows at a size limit loses them on restart.
        if let Ok(plain) = serde_json::to_vec(&opened) {
            if let Ok(blob) = e2ee::store::seal_cache_value(key, id.as_bytes(), &plain) {
                // An interrupted append may leave no newline. Fence the next complete record
                // from that tail; readers already skip empty lines and invalid records.
                let line = format!("\n{id} {}\n", STANDARD.encode(blob));
                let written = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .and_then(|mut file| file.write_all(line.as_bytes()));
                if let Err(err) = written {
                    tracing::warn!("chatd: Tiny e2ee: the cache could not be written: {err}");
                }
            }
        }
        self.rows.insert(id.to_owned(), opened);
    }
}

/// Daemon regression: a long conversation keeps newly opened history across restarts.
pub fn prove_cache_history_limit() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let key = CacheKey::generate().map_err(|err| err.to_string())?;
    {
        let mut cache = Cache::load(dir.path(), "long-conversation", &key);
        // Cross the old limit through real sealed appends, rather than a fabricated row count.
        for index in 0..=20_000 {
            cache.put(
                &key,
                &format!("m{index}"),
                Opened {
                    text: format!("saved words {index}"),
                    ..Opened::default()
                },
            );
        }
    }
    let restored = Cache::load(dir.path(), "long-conversation", &key);
    for index in 0..=20_000 {
        let id = format!("m{index}");
        let expected = format!("saved words {index}");
        if restored.rows.get(&id).map(|row| row.text.as_str()) != Some(expected.as_str()) {
            return Err(format!("sealed history lost {id} after restart"));
        }
    }
    Ok(())
}

/// Lab proof: interrupted cache writes cannot swallow the next complete encrypted record.
pub fn prove_cache_append_tail() -> Result<(), String> {
    for complete_tail in [false, true] {
        let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
        let key = CacheKey::generate().map_err(|err| err.to_string())?;
        let mut cache = Cache::load(dir.path(), "c1", &key);
        cache.put(
            &key,
            "m1",
            Opened {
                text: "earlier words".into(),
                ..Opened::default()
            },
        );
        let tail = if complete_tail {
            std::fs::read_to_string(&cache.path)
                .map_err(|err| err.to_string())?
                .trim()
                .to_owned()
        } else {
            "torn-id unfinished".to_owned()
        };
        std::fs::OpenOptions::new()
            .append(true)
            .open(&cache.path)
            .and_then(|mut file| file.write_all(tail.as_bytes()))
            .map_err(|err| err.to_string())?;
        // Reopen as after a restart, then append a genuinely encrypted new record.
        let mut cache = Cache::load(dir.path(), "c1", &key);
        cache.put(
            &key,
            "m2",
            Opened {
                text: "later words".into(),
                ..Opened::default()
            },
        );
        let restored = Cache::load(dir.path(), "c1", &key);
        for (id, words) in [("m1", "earlier words"), ("m2", "later words")] {
            if restored.rows.get(id).map(|row| row.text.as_str()) != Some(words) {
                return Err(format!(
                    "cache lost {id} after unterminated tail (complete_tail={complete_tail})"
                ));
            }
        }
    }
    Ok(())
}

/// Lab proof: renewing a session still opens the original device and sealed history.
pub fn prove_reauthenticated_history() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let user = UserId::random().map_err(|err| err.to_string())?;
    let secrets = Secrets::generate()?;
    let session = json!({
        "base_url": "https://reauth.example", "user_id": user.to_string(), "token": "old-token",
        "display_name": "Ada", "role": "member", "e2ee": &secrets,
    });
    let (state_key, cache_key) = secrets.keys().ok_or("initial keys unreadable")?;
    let engine = Engine::create(user, Box::new(FileStore::new(dir.path())), state_key, STATE)
        .map_err(|err| err.to_string())?;
    let device_id = engine.device_id();
    drop(engine);
    let mut cache = Cache::load(dir.path(), "c1", &cache_key);
    cache.put(&cache_key, "m1", Opened { text: "history before signing in again".into(), ..Opened::default() });
    drop(cache);
    let state_before = std::fs::read(dir.path().join(STATE)).map_err(|err| err.to_string())?;
    let cache_before = std::fs::read(dir.path().join("cache-c1.log")).map_err(|err| err.to_string())?;
    let mut fresh = session.clone();
    fresh.as_object_mut().ok_or("session is not an object")?.remove("e2ee");
    fresh["token"] = json!("renewed-token");
    let renewed = super::combine(&session.to_string(), &fresh.to_string())?;
    let renewed = super::parse(&renewed)?;
    if renewed.token != "renewed-token" {
        return Err("signing in did not renew the token".into());
    }
    let (state_key, cache_key) = renewed.e2ee.as_ref()
        .and_then(Secrets::keys).ok_or("signing in removed this install's encryption keys")?;
    let reopened = Engine::open(Box::new(FileStore::new(dir.path())), state_key, STATE)
        .map_err(|err| err.to_string())?.ok_or("original device state disappeared")?;
    if reopened.device_id() != device_id {
        return Err("signing in replaced the device identity".into());
    }
    let cache = Cache::load(dir.path(), "c1", &cache_key);
    if cache.rows.get("m1").map(|row| row.text.as_str()) != Some("history before signing in again") {
        return Err("signing in lost previously opened history".into());
    }
    if std::fs::read(dir.path().join(STATE)).map_err(|err| err.to_string())? != state_before
        || std::fs::read(dir.path().join("cache-c1.log")).map_err(|err| err.to_string())? != cache_before
    {
        return Err("signing in rewrote existing encrypted files".into());
    }
    Ok(())
}

/// This install as a device of one person on one server.
pub(crate) struct Device {
    saved: Saved,
    dir: PathBuf,
    engine: Option<Engine>,
    cache_key: Option<CacheKey>,
    state: DeviceState,
    mode: String,
    notes: Notes,
    caches: HashMap<String, Cache>,
    /// Conversation id to whether it is encrypted, as the server said.
    encrypted: HashMap<String, bool>,
    /// The sealed attachments seen, by their id, with their conversation: to open them when
    /// fetched.
    files: HashMap<String, (String, OpenedFile)>,
    /// People whose device list was checked since the server last said it changed
    /// (`e2ee.device`): a sync does not ask for theirs again.
    checked: BTreeSet<String>,
    /// Conversations where someone the server is adding has no device in the group yet, as the
    /// last sync saw them.
    joining: BTreeSet<String>,
    /// Conversations with a row that did not open because this device was not in the group, or
    /// not at its epoch, yet. Read again when the group's handshake next comes.
    unopened: BTreeSet<String>,
    /// The server relays device links (`e2ee_device_link`).
    linking: bool,
    /// The adapter's inbox, once the account started.
    commands: Option<std::sync::mpsc::Sender<crate::adapters::Command>>,
    /// The largest archive one link carries (`e2ee_link_archive_max`).
    link_max: u64,
    /// The server has the person's own conversation (`e2ee_self`): their devices fill each
    /// other's gaps there.
    self_capable: bool,
    /// The server fences independently signed cryptographic group generations.
    generations: bool,
    /// Scoped to a repair job while it holds this device lock.
    operation_deadline: Option<std::time::Instant>,
    siblings: siblings::Siblings,
}

type Shared = Arc<Mutex<Device>>;

fn devices() -> &'static Mutex<HashMap<(String, String), Shared>> {
    static DEVICES: OnceLock<Mutex<HashMap<(String, String), Shared>>> = OnceLock::new();
    DEVICES.get_or_init(Mutex::default)
}

/// Keys made in a scratch directory, where nothing is written to the keychain: kept for the
/// life of the process so a test's chatd can open its own state again.
fn scratch_secrets() -> &'static Mutex<HashMap<(String, String), Secrets>> {
    static KEPT: OnceLock<Mutex<HashMap<(String, String), Secrets>>> = OnceLock::new();
    KEPT.get_or_init(Mutex::default)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// This account's device, made and registered the first time. `None` when the server does not
/// speak encrypted text.
pub(crate) fn device(saved: &Saved) -> Shared {
    let key = (saved.base_url.clone(), saved.user_id.clone());
    // Never retain the registry guard while waiting for a device. Repair jobs briefly read
    // the registry while holding their device to check that the account still owns it.
    let found = { lock(devices()).get(&key).cloned() };
    if let Some(found) = found {
        lock(&found).saved.token.clone_from(&saved.token);
        return found;
    }
    let made = Arc::new(Mutex::new(Device::new(saved)));
    lock(devices()).entry(key).or_insert(made).clone()
}

/// The device if it was started, without starting one.
pub(crate) fn started(saved: &Saved) -> Option<Shared> {
    lock(devices())
        .get(&(saved.base_url.clone(), saved.user_id.clone()))
        .cloned()
}

/// Opened plaintext this device already has for `message_id`, from the sealed cache.
/// Works even when the MLS device is not started yet: the cache key lives in the session.
pub(crate) fn opened_row(saved: &Saved, conversation_id: &str, message_id: &str) -> Option<Opened> {
    if let Some(shared) = started(saved) {
        let mut device = lock(&shared);
        if let Some(opened) = device
            .cache(conversation_id)
            .and_then(|cache| cache.rows.get(message_id).cloned())
        {
            return Some(opened);
        }
    }
    let secrets = saved.e2ee.as_ref()?;
    let (_, cache_key) = secrets.keys()?;
    let cache = Cache::load(&dir_of(saved), conversation_id, &cache_key);
    cache.rows.get(message_id).cloned()
}

/// Previously opened nonempty text, for the inbox preview.
pub(crate) fn opened_text(
    saved: &Saved,
    conversation_id: &str,
    message_id: &str,
) -> Option<String> {
    if let Some(shared) = started(saved) {
        let mut device = lock(&shared);
        if let Some(text) = device
            .cache(conversation_id)
            .and_then(|cache| cache.rows.get(message_id).cloned())
            .map(|opened| opened.text)
            .filter(|text| !text.trim().is_empty())
        {
            return Some(text);
        }
    }
    let secrets = saved.e2ee.as_ref()?;
    let (_, cache_key) = secrets.keys()?;
    let cache = Cache::load(&dir_of(saved), conversation_id, &cache_key);
    cache
        .rows
        .get(message_id)
        .map(|opened| opened.text.clone())
        .filter(|text| !text.trim().is_empty())
}

/// Writes one opened line into the sealed cache (lab prove and unit tests).
pub(crate) fn write_opened_cache(
    saved: &Saved,
    conversation_id: &str,
    message_id: &str,
    text: &str,
) -> Result<(), String> {
    write_opened_row(
        saved,
        conversation_id,
        message_id,
        Opened {
            text: text.to_owned(),
            ..Opened::default()
        },
    )
}

/// Writes one authenticated opened value into the sealed cache (lab prove and unit tests).
pub(crate) fn write_opened_row(
    saved: &Saved,
    conversation_id: &str,
    message_id: &str,
    opened: Opened,
) -> Result<(), String> {
    let secrets = saved
        .e2ee
        .as_ref()
        .ok_or("This device has no encryption keys.")?;
    let (_, cache_key) = secrets.keys().ok_or("This device's keys are unreadable.")?;
    private_dir(&dir_of(saved)).map_err(|err| err.to_string())?;
    let mut cache = Cache::load(&dir_of(saved), conversation_id, &cache_key);
    cache.put(&cache_key, message_id, opened);
    Ok(())
}

/// Signs this install in as a device: called when the account starts. Never fails the account.
/// `commands` is the adapter's own inbox: what this module took in by itself (a device link's
/// history) is shown by asking the adapter to read those conversations again.
pub(crate) fn begin(saved: &Saved, commands: std::sync::mpsc::Sender<crate::adapters::Command>) {
    let shared = device(saved);
    let mut device = lock(&shared);
    device.commands = Some(commands);
    device.enrol();
}

/// Signing out: the device is revoked on the server and its state forgotten here. What it opened
/// stays, sealed under the cache key [`keep_history`] keeps in the keychain, unless the person
/// chose to erase it ([`erase_history`]); a later sign-in here is a new device that reads it.
pub(crate) fn sign_out(saved: &Saved) {
    let key = (saved.base_url.clone(), saved.user_id.clone());
    if let Some(shared) = started(saved) {
        let device = lock(&shared);
        // First, while it is quick: the key, from the keys this device held.
        if let Err(why) = keep_history(&device.saved) {
            tracing::warn!("chatd: Tiny e2ee: the history key could not be kept: {why}");
        }
        if let Some(engine) = device.engine.as_ref() {
            let id = engine.device_id().to_string();
            let _ = device.ask("DELETE", &format!("/v1/e2ee/devices/{id}"), None);
        }
    }
    lock(devices()).remove(&key);
    lock(scratch_secrets()).remove(&key);
    start_over(&dir_of(saved));
}

/// The keychain item that keeps the cache keys of accounts signed out with their history kept:
/// `{"<user id> <server>": "<base64 cache key>"}`. Taken back by the next sign-in of that account
/// on this install, and dropped from here then.
const KEPT: &str = "Tiny kept history";

fn kept_name(user_id: &str, base_url: &str) -> String {
    format!("{user_id} {base_url}")
}

/// What [`KEPT`] holds. An item that could not be read is an error, never taken for empty: it
/// would be overwritten.
fn kept() -> Result<BTreeMap<String, String>, String> {
    Ok(crate::session::item::read(KEPT)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

fn save_kept(kept: &BTreeMap<String, String>) -> Result<(), String> {
    if kept.is_empty() {
        crate::session::item::forget(KEPT);
        return Ok(());
    }
    let text = serde_json::to_string(kept).map_err(|err| err.to_string())?;
    crate::session::item::save(KEPT, &text)
}

/// Signing out, keeping the history: the cache key of `saved` goes to [`KEPT`] before its
/// session, which held it, is forgotten.
pub(crate) fn keep_history(saved: &Saved) -> Result<(), String> {
    let Some(secrets) = &saved.e2ee else {
        return Ok(());
    };
    let mut all = kept()?;
    all.insert(
        kept_name(&saved.user_id, &saved.base_url),
        secrets.cache.clone(),
    );
    save_kept(&all)
}

/// Signing out, erasing: this account's folder (what was opened, the notes) and its kept key.
pub(crate) fn erase_history(user_id: &str) {
    let _ = std::fs::remove_dir_all(dir_for(user_id));
    if let Ok(mut all) = kept() {
        let before = all.len();
        all.retain(|name, _| !name.starts_with(&format!("{user_id} ")));
        if all.len() != before {
            let _ = save_kept(&all);
        }
    }
}

/// Removes everything a device of this folder kept but what it opened and what the person
/// verified: the next device made here is a new one, and its handshake positions start again.
fn start_over(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("cache-") && name.ends_with(".log") {
            continue;
        }
        if name == "notes.json" {
            let notes: Notes = std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let kept = Notes {
                met: notes.met,
                verified: notes.verified,
                recovery: notes.recovery,
                ..Notes::default()
            };
            if let Ok(bytes) = serde_json::to_vec(&kept) {
                let _ = write_private(&entry.path(), &bytes);
            }
            continue;
        }
        let _ = if entry.path().is_dir() {
            std::fs::remove_dir_all(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
    }
}

fn uuid(text: &str) -> Option<[u8; 16]> {
    ConversationId::parse(text).ok().map(|id| id.0)
}

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
}

/// The first 16 bytes of SHA-256 over a signing key, in groups of four hex digits.
pub(crate) fn fingerprint(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    digest[..16]
        .chunks(2)
        .map(|pair| format!("{:02x}{:02x}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Milliseconds for the trust verifier's order: the server's `created_at`. Only the order counts.
fn order_of(created_at: &str) -> i64 {
    super::messages::unix_of(created_at)
        .unwrap_or(0)
        .saturating_mul(1000)
}

/// What the person pastes on a device they already have to approve this one.
fn link_code(device: DeviceId, key: &[u8]) -> String {
    let mut bytes = device.as_bytes().to_vec();
    bytes.extend_from_slice(key);
    format!("tiny-device-{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn parse_link_code(code: &str) -> Option<(DeviceId, Vec<u8>)> {
    let rest: String = code
        .trim()
        .strip_prefix("tiny-device-")?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = URL_SAFE_NO_PAD.decode(rest).ok()?;
    if bytes.len() != 16 + 65 || bytes[16] != 4 {
        return None;
    }
    Some((
        DeviceId::from_slice(&bytes[..16]).ok()?,
        bytes[16..].to_vec(),
    ))
}

/// A 128-bit recovery key: 26 base32 digits (RFC 4648 alphabet, no padding), shown in six groups
/// as `XXXX-XXXX-XXXX-XXXX-XXXX-XXXXXX`. From the system's random source; never a passphrase.
fn new_recovery_key() -> Result<(String, [u8; 16]), String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "No randomness is available.".to_owned())?;
    Ok((format_recovery(&bytes), bytes))
}

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn format_recovery(bytes: &[u8; 16]) -> String {
    let mut digits = String::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            digits.push(BASE32[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        digits.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    let groups = [4usize, 4, 4, 4, 4, 6];
    let mut out = Vec::new();
    let mut at = 0;
    for len in groups {
        out.push(&digits[at..at + len]);
        at += len;
    }
    out.join("-")
}

fn parse_recovery(text: &str) -> Option<[u8; 16]> {
    let digits: Vec<u8> = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase() as u8)
        .collect();
    if digits.len() != 26 {
        return None;
    }
    let mut out = Vec::with_capacity(17);
    let mut buffer = 0u32;
    let mut bits = 0;
    for digit in digits {
        let value = BASE32.iter().position(|&b| b == digit)? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            out.push(((buffer >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
        }
    }
    out.truncate(16);
    out.try_into().ok()
}

/// HKDF-SHA-256 of the recovery key with `salt`: what is kept to check a typed key.
fn recovery_check(key: &[u8; 16], salt: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let _ =
        hkdf::Hkdf::<Sha256>::new(Some(salt), key).expand(b"tiny-e2ee-v1 recovery check", &mut out);
    out
}

/// The words for a failure of the engine, for a person.
fn words(error: &Error) -> String {
    match error {
        Error::IdentityChanged => "Someone in this conversation has a new identity. Check it in Encryption before you send.".to_owned(),
        Error::ServerKeyChanged => "The server's key for this conversation changed. Nothing is sent until you look at it.".to_owned(),
        Error::Frozen(_) | Error::EpochBackwards { .. } => "This conversation's encryption went back in time on the server, so it is stopped. Ask the server's owner.".to_owned(),
        Error::NotAMember => "This device isn't in this conversation's encryption any more.".to_owned(),
        Error::NoGroup => "This device isn't in this conversation's encryption yet. Try again in a moment.".to_owned(),
        Error::PendingRemove | Error::PendingProposals | Error::CommitInFlight => "This conversation's members are changing. Try again in a moment.".to_owned(),
        Error::UntrustedDevices(_) => "A device in this conversation isn't approved by its owner, so nothing was sent.".to_owned(),
        _ => "The message couldn't be encrypted.".to_owned(),
    }
}

impl Device {
    fn revoked(&self) -> bool {
        self.engine.as_ref().is_some_and(|engine| {
            self.notes.revoked_device.as_deref() == Some(engine.device_id().to_string().as_str())
        })
    }

    fn mark_revoked(&mut self) -> bool {
        let Some(engine) = self.engine.as_ref() else { return false };
        let id = engine.device_id().to_string();
        let changed = self.notes.revoked_device.as_ref() != Some(&id) || self.state == DeviceState::Ready;
        self.notes.revoked_device = Some(id);
        self.state = DeviceState::Failed {
            why: "This device was removed. Saved messages are still available.".to_owned(),
        };
        if changed { self.save_notes(); }
        changed
    }

    fn new(saved: &Saved) -> Device {
        Device {
            saved: saved.clone(),
            dir: dir_of(saved),
            engine: None,
            cache_key: None,
            state: DeviceState::Off,
            mode: String::new(),
            notes: Notes::default(),
            caches: HashMap::new(),
            encrypted: HashMap::new(),
            files: HashMap::new(),
            checked: BTreeSet::new(),
            joining: BTreeSet::new(),
            unopened: BTreeSet::new(),
            linking: false,
            commands: None,
            link_max: linking::ARCHIVE_MAX,
            self_capable: false,
            generations: false,
            operation_deadline: None,
            siblings: siblings::Siblings::default(),
        }
    }

    fn ask(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, Value), String> {
        if let Some(deadline) = self.operation_deadline {
            let remaining = deadline.checked_duration_since(std::time::Instant::now()).ok_or("Encryption update timed out. Try again to continue safely.")?;
            return super::call_with_timeout(method, &super::endpoint(&self.saved.base_url, path), Some(&self.saved.token), body, remaining.min(super::TIMEOUT)).map_err(|_| super::UNREACHABLE.to_owned());
        }
        super::asked(method, &self.saved, path, body).map_err(str::to_owned)
    }

    fn save_notes(&self) {
        if let Ok(bytes) = serde_json::to_vec(&self.notes) {
            if let Err(err) = write_private(&self.dir.join("notes.json"), &bytes) {
                tracing::warn!("chatd: Tiny e2ee: notes could not be written: {err}");
            }
        }
    }

    /// The keys for this device: from the session, else made now and put in the keychain before
    /// anything is sealed with them.
    fn secrets(&mut self) -> Result<Secrets, String> {
        let key = (self.saved.base_url.clone(), self.saved.user_id.clone());
        if let Some(secrets) = &self.saved.e2ee {
            return Ok(secrets.clone());
        }
        if let Some(secrets) = lock(scratch_secrets()).get(&key) {
            return Ok(secrets.clone());
        }
        // Another start may have made them since this session was read: never replace those.
        if let Some(kept) = crate::session::load(protocol::Network::Tiny)
            .and_then(|session| super::saved_rows(&session).ok())
            .and_then(|rows| {
                rows.into_iter().find(|row| {
                    row.user_id == self.saved.user_id && row.base_url == self.saved.base_url
                })
            })
            .and_then(|row| row.e2ee)
        {
            self.saved.e2ee = Some(kept.clone());
            return Ok(kept);
        }
        let mut secrets = Secrets::generate()?;
        // Signed out here before with the history kept: the cache key comes back, so what was
        // opened then opens again. A state sealed under a key no session holds any more never
        // opens: this is a new device.
        let name = kept_name(&self.saved.user_id, &self.saved.base_url);
        let mut kept = kept().unwrap_or_default();
        let reused = kept.remove(&name);
        if let Some(cache) = &reused {
            secrets.cache.clone_from(cache);
        }
        start_over(&self.dir);
        self.notes = std::fs::read(self.dir.join("notes.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if crate::session::enabled() {
            let saved = self.saved.clone();
            let fresh = secrets.clone();
            let kept = crate::session::rewrite(protocol::Network::Tiny, |session| {
                with_secrets(session, &saved, &fresh)
            })?;
            if !kept {
                return Err("The keychain did not keep this device's keys.".to_owned());
            }
        } else {
            lock(scratch_secrets()).insert(key, secrets.clone());
        }
        if reused.is_some() {
            if let Err(why) = save_kept(&kept) {
                tracing::warn!(
                    "chatd: Tiny e2ee: the kept history key could not be dropped: {why}"
                );
            }
        }
        self.saved.e2ee = Some(secrets.clone());
        Ok(secrets)
    }

    /// Asks the server whether it speaks encrypted text, opens or makes the engine, registers the
    /// device and uploads key packages.
    fn enrol(&mut self) {
        match self.try_enrol() {
            Ok(state) => self.state = state,
            Err(why) => {
                tracing::warn!("chatd: Tiny e2ee: {why}");
                self.state = DeviceState::Failed { why };
            }
        }
        if self.ready() {
            // Startup can run while approval is still pending. Every successful enrollment,
            // including the one after linking, must initialize the hidden sibling group.
            let gained = self.catch_up().into_iter().collect();
            self.reload(&gained);
        }
    }

    fn try_enrol(&mut self) -> Result<DeviceState, String> {
        let (status, body) = self.ask("GET", "/v1/capabilities", None)?;
        let speaks = status == 200
            && body
                .get("features")
                .and_then(Value::as_array)
                .is_some_and(|all| all.iter().any(|f| f.as_str() == Some("e2ee_text")));
        if !speaks {
            self.mode.clear();
            return Ok(DeviceState::Off);
        }
        self.generations = body.get("features").and_then(Value::as_array)
            .is_some_and(|all| all.iter().any(|feature| feature.as_str() == Some("e2ee_group_generations")));
        self.self_capable = body
            .get("features")
            .and_then(Value::as_array)
            .is_some_and(|all| all.iter().any(|f| f.as_str() == Some("e2ee_self")));
        self.link_max = body
            .get("e2ee_link_archive_max")
            .and_then(Value::as_u64)
            .filter(|most| *most > 0)
            .unwrap_or(linking::ARCHIVE_MAX);
        self.linking = body
            .get("features")
            .and_then(Value::as_array)
            .is_some_and(|all| all.iter().any(|f| f.as_str() == Some("e2ee_device_link")));
        self.mode = body
            .get("e2ee_text")
            .and_then(Value::as_str)
            .unwrap_or("optional")
            .to_owned();
        if self.engine.is_none() {
            private_dir(&self.dir)
                .map_err(|err| format!("The encryption folder could not be made: {err}"))?;
            if let Ok(bytes) = std::fs::read(self.dir.join("notes.json")) {
                self.notes = serde_json::from_slice(&bytes).unwrap_or_default();
            }
            let secrets = self.secrets()?;
            let (state_key, cache_key) =
                secrets.keys().ok_or("This device's keys are unreadable.")?;
            let user = UserId::parse(&self.saved.user_id)
                .map_err(|_| "This account's id is not a UUID.".to_owned())?;
            let store =
                || Box::new(FileStore::new(&self.dir)) as Box<dyn e2ee::store::Store + Send>;
            let engine = match Engine::open(store(), state_key.clone(), STATE) {
                Ok(Some(engine)) => engine,
                Ok(None) => Engine::create(user, store(), state_key, STATE)
                    .map_err(|err| format!("This device could not be made: {err}"))?,
                // A blob that does not open under this key is never taken for empty.
                Err(err) => {
                    return Err(format!(
                        "This device's encryption state could not be opened: {err}"
                    ))
                }
            };
            if engine.user_id() != user {
                return Err("This device's state belongs to another account.".to_owned());
            }
            self.engine = Some(engine);
            self.cache_key = Some(cache_key);
        }
        if self.revoked() {
            self.mark_revoked();
            return Ok(self.state.clone());
        }
        self.register()
    }

    /// Makes or opens the engine (and with it the cache key) unless it is ready.
    fn enrol_if_needed(&mut self) {
        if self.engine.is_none() || self.state != DeviceState::Ready {
            self.enrol();
        }
    }

    fn engine(&mut self) -> Result<&mut Engine, String> {
        self.engine
            .as_mut()
            .ok_or_else(|| "Encryption isn't set up on this device.".to_owned())
    }

    /// `PUT /v1/e2ee/devices/{id}`. A first device needs no cross-signature; a later one waits
    /// for a device the person already has to approve it.
    fn register(&mut self) -> Result<DeviceState, String> {
        let (device_id, key) = {
            let engine = self.engine()?;
            (engine.device_id(), engine.signature_key())
        };
        if self.mine_registered(device_id, &key)? {
            self.top_up(false)?;
            return Ok(DeviceState::Ready);
        }
        let body = json!({ "signature_key": b64(&key), "label": device_label() });
        let (status, answer) =
            self.ask("PUT", &format!("/v1/e2ee/devices/{device_id}"), Some(body))?;
        match status {
            200 | 201 => {
                self.top_up(true)?;
                Ok(DeviceState::Ready)
            }
            409 if answer.get("error").and_then(Value::as_str) != Some("device_exists") => {
                Ok(DeviceState::AwaitingApproval {
                    code: link_code(device_id, &key),
                })
            }
            409 => {
                // Registered already, maybe by the last try: it is ours if the key matches.
                if self.mine_registered(device_id, &key)? {
                    self.top_up(false)?;
                    Ok(DeviceState::Ready)
                } else {
                    Err("Another device has this device's id on the server.".to_owned())
                }
            }
            400 if answer.get("error").and_then(Value::as_str)
                == Some("cross_signature_required") =>
            {
                Ok(DeviceState::AwaitingApproval {
                    code: link_code(device_id, &key),
                })
            }
            400 if answer.get("error").and_then(Value::as_str) == Some("bad_device") => {
                // This identity cannot be registered again after server-side removal.
                self.mark_revoked();
                Ok(self.state.clone())
            }
            status => Err(format!("The server refused this device ({status}).")),
        }
    }

    /// This person's devices as the server lists them.
    fn list(&self, user_id: &str) -> Result<Vec<Value>, String> {
        match self.ask("GET", &format!("/v1/e2ee/users/{user_id}/devices"), None)? {
            (200, Value::Array(rows)) => Ok(rows),
            (200, _) => Err(super::UNREADABLE.to_owned()),
            (status, _) => Err(format!("The server did not list the devices ({status}).")),
        }
    }

    fn mine_registered(&self, device_id: DeviceId, key: &[u8]) -> Result<bool, String> {
        let id = device_id.to_string();
        let rows = self.list(&self.saved.user_id)?;
        Ok(rows.iter().any(|row| {
            row.get("device_id").and_then(Value::as_str) == Some(id.as_str())
                && row
                    .get("signature_key")
                    .and_then(Value::as_str)
                    .and_then(unb64)
                    .as_deref()
                    == Some(key)
        }))
    }

    /// Keeps the server's supply of this device's key packages at [`KEY_PACKAGES`], with a
    /// last-resort one. `fresh` uploads without asking first.
    fn top_up(&mut self, fresh: bool) -> Result<(), String> {
        let device_id = self.engine()?.device_id();
        let remaining = if fresh {
            0
        } else {
            match self.ask(
                "GET",
                &format!("/v1/e2ee/key_packages/count?device_id={device_id}"),
                None,
            )? {
                (200, body) => body.get("remaining").and_then(Value::as_u64).unwrap_or(0) as usize,
                (status, _) => {
                    return Err(format!("The server did not count key packages ({status})."))
                }
            }
        };
        if remaining >= KEY_PACKAGES / 2 {
            return Ok(());
        }
        let engine = self.engine()?;
        let count = KEY_PACKAGES - remaining;
        let packages = engine
            .key_packages(count)
            .map_err(|err| format!("Key packages could not be made: {err}"))?;
        let mut body = json!({ "device_id": device_id.to_string(), "key_packages": packages.iter().map(|p| b64(p)).collect::<Vec<_>>() });
        if fresh || remaining == 0 {
            let last = engine
                .last_resort_key_package()
                .map_err(|err| format!("Key packages could not be made: {err}"))?;
            body["last_resort"] = Value::String(b64(&last));
        }
        match self.ask("POST", "/v1/e2ee/key_packages", Some(body))? {
            (200 | 204, _) => Ok(()),
            (status, _) => Err(format!("The server refused key packages ({status}).")),
        }
    }

    /// Verifies `user_id`'s device list from the server into the trust store.
    fn verify_user(&mut self, user_id: &str) -> Result<(), String> {
        let rows = self.list(user_id)?;
        self.verify_rows(user_id, &rows)
    }

    fn verify_rows(&mut self, user_id: &str, rows: &[Value]) -> Result<(), String> {
        let user = UserId::parse(user_id).map_err(|_| super::CHECK.to_owned())?;
        let records: Vec<DeviceRecord> = rows
            .iter()
            .filter_map(|row| {
                Some(DeviceRecord {
                    device_id: DeviceId::parse(row.get("device_id")?.as_str()?).ok()?,
                    signature_key: unb64(row.get("signature_key")?.as_str()?)?,
                    cross_signature: row
                        .get("cross_signature")
                        .and_then(Value::as_str)
                        .and_then(unb64),
                    created_at: order_of(
                        row.get("created_at").and_then(Value::as_str).unwrap_or(""),
                    ),
                })
            })
            .collect();
        let engine = self.engine()?;
        match engine.verify_devices(user, &records) {
            Ok(report) => {
                if !report.rejected.is_empty() {
                    tracing::warn!("chatd: Tiny e2ee: {} device(s) of a member are not cross-signed; they are not trusted", report.rejected.len());
                }
                self.checked.insert(user_id.to_owned());
                if self.notes.changed.remove(user_id) | self.notes.met.insert(user_id.to_owned()) {
                    self.save_notes();
                }
                Ok(())
            }
            Err(Error::IdentityChanged) => {
                if self.notes.changed.insert(user_id.to_owned()) {
                    self.save_notes();
                }
                Err(words(&Error::IdentityChanged))
            }
            Err(err) => Err(format!("Their devices could not be checked: {err}")),
        }
    }

    /// Whether `conversation_id` is end-to-end encrypted, as the server says. Asked once.
    pub(crate) fn is_encrypted(&mut self, conversation_id: &str) -> Result<bool, String> {
        if self.engine.is_none() { return Ok(self.encrypted.get(conversation_id).copied().unwrap_or(false)); }
        if ConversationId::parse(conversation_id).is_ok() {
            // Durable cryptographic state wins over missing labels or an unavailable server.
            let active = self.active(conversation_id)?;
            if active.head_hash.is_some() { return Ok(true); }
        }
        if let Some(known) = self.encrypted.get(conversation_id) { return Ok(*known); }
        let known = if self.generations {
            let (_, body) = self.group_info(conversation_id)?;
            body.get("e2ee").and_then(Value::as_bool).unwrap_or(false)
        } else {
            // Unknown legacy/plaintext conversations retain their original discovery behavior.
            match self.ask("GET", &format!("/v1/e2ee/conversations/{conversation_id}"), None) {
                Ok((200, body)) => body.get("e2ee").and_then(Value::as_bool).unwrap_or(false),
                _ => false,
            }
        };
        self.encrypted.insert(conversation_id.to_owned(), known);
        Ok(known)
    }

    pub(crate) fn note_encrypted(&mut self, conversation_id: &str, encrypted: bool) {
        self.encrypted.insert(conversation_id.to_owned(), encrypted);
    }

    /// What the server lets new conversations be: `off`, `optional`, `new_required`; empty when
    /// it does not speak encrypted text.
    pub(crate) fn mode(&self) -> &str {
        &self.mode
    }

    /// The attachments of a message this device opened, to keep them through an edit.
    pub(crate) fn attachments_of(
        &mut self,
        conversation_id: &str,
        message_id: &str,
    ) -> Vec<AttachmentRef> {
        let Some(opened) = self
            .cache(conversation_id)
            .and_then(|cache| cache.rows.get(message_id).cloned())
        else {
            return Vec::new();
        };
        opened
            .attachments
            .iter()
            .filter_map(OpenedFile::to_ref)
            .collect()
    }

    pub(crate) fn editable_attachments(&mut self, conversation_id: &str, message_id: &str) -> Result<Vec<AttachmentRef>, String> {
        let user = self.engine()?.user_id();
        let authenticated = self.cache(conversation_id).and_then(|cache| cache.rows.get(message_id))
            .and_then(|opened| opened.auth.as_ref()).is_some_and(|auth| auth.author == user
                && auth.kind == "message" && auth.conversation_id.to_string() == conversation_id);
        if !authenticated { return Err("This saved message can't be edited.".to_owned()); }
        Ok(self.attachments_of(conversation_id, message_id))
    }

    pub(crate) fn ready(&self) -> bool {
        self.state == DeviceState::Ready && self.engine.is_some()
    }

    /// Brings this device's view of one conversation's group up to date: verifies the members,
    /// starts the group if nobody has, applies the handshake feed, and commits what is pending.
    /// Has the adapter read these conversations' newest messages again, which now open from the
    /// cache: they gained rows this device did not open itself.
    fn reload(&self, conversations: &BTreeSet<String>) {
        let Some(commands) = &self.commands else {
            return;
        };
        for conversation in conversations {
            let _ = commands.send(crate::adapters::Command::Backfill(conversation.clone()));
        }
    }

    /// The encrypted conversations this device knows of: the ones the server said are, and the
    /// ones it has opened messages of.
    fn known_encrypted(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .encrypted
            .iter()
            .filter(|(_, encrypted)| **encrypted)
            .map(|(id, _)| id.clone())
            .collect();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(conv) = name
                    .strip_prefix("cache-")
                    .and_then(|rest| rest.strip_suffix(".log"))
                {
                    if super::safe_id(conv) {
                        out.insert(conv.to_owned());
                    }
                }
            }
        }
        out
    }

    pub(crate) fn sync(&mut self, conversation_id: &str) -> Result<(), String> {
        if !self.ready() {
            return Err("This device isn't set up for encrypted messages yet.".to_owned());
        }
        for _ in 0..PASSES {
            let (active, info) = self.group_info(conversation_id)?;
            let conv = active.group_id;
            if info.get("e2ee").and_then(Value::as_bool) != Some(true) {
                self.note_encrypted(conversation_id, false);
                self.joining.remove(conversation_id);
                return Ok(());
            }
            self.note_encrypted(conversation_id, true);
            if joining(&info) {
                self.joining.insert(conversation_id.to_owned());
            } else {
                self.joining.remove(conversation_id);
            }
            let server_key = info
                .get("external_sender")
                .and_then(Value::as_str)
                .and_then(unb64)
                .ok_or(super::UNREADABLE)?;
            // Everyone the server says is or will be in the group: their devices, checked here.
            let mut users: BTreeSet<String> = BTreeSet::new();
            for list in ["members", "pending"] {
                for row in info
                    .get(list)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(user) = row.get("user_id").and_then(Value::as_str) {
                        users.insert(user.to_owned());
                    }
                }
            }
            users.insert(self.saved.user_id.clone());
            for user in users
                .iter()
                .filter(|user| !self.checked.contains(*user))
                .cloned()
                .collect::<Vec<_>>()
            {
                if let Err(why) = self.verify_user(&user) {
                    tracing::warn!("chatd: Tiny e2ee: {conversation_id}: {why}");
                }
            }
            let has = self.engine()?.has_group(conv);
            let mut started = false;
            if !has {
                if info.get("epoch").is_none_or(Value::is_null) {
                    // Nobody has started it: this device does.
                    let device_id = self.engine()?.device_id().to_string();
                    let mut body = json!({ "device_id": device_id });
                    self.generation_body(&mut body, active.generation);
                    let (status, _) = self.ask("POST", &format!("/v1/e2ee/conversations/{conversation_id}/group"), Some(body))?;
                    match status {
                        // The server adds the others now: the next pass reads who is waiting.
                        201 | 200 => {
                            self.engine()?
                                .create_group(conv, &server_key)
                                .map_err(|err| format!("The group could not be started: {err}"))?;
                            started = true;
                        }
                        // Someone else did first: their Welcome is in the feed.
                        409 => {}
                        status => {
                            return Err(format!("The server did not start the group ({status})."))
                        }
                    }
                }
            } else {
                let engine = self.engine()?;
                if let Err(err) = engine.observe_server_key(conv, &server_key) {
                    self.notes.key_changed.insert(conversation_id.to_owned());
                    self.save_notes();
                    return Err(words(&err));
                }
                if let Some(epoch) = info.get("epoch").and_then(Value::as_u64) {
                    engine
                        .observe_server_epoch(conv, epoch)
                        .map_err(|err| words(&err))?;
                }
            }
            let moved = self.feed(conversation_id, conv, &server_key)?;
            let committed = self.commit(conversation_id, conv)?;
            if !moved && !committed && !started {
                break;
            }
        }
        let _ = self.engine()?.flush();
        Ok(())
    }

    /// Applies the handshake feed from where this conversation was last read. True when anything
    /// was applied.
    fn feed(
        &mut self,
        conversation_id: &str,
        conv: ConversationId,
        server_key: &[u8],
    ) -> Result<bool, String> {
        let active = self.active(conversation_id)?;
        if active.group_id != conv { return Err("Encryption changed. Try again.".to_owned()); }
        self.feed_at(conversation_id, conv, active.generation, server_key)
    }

    fn feed_at(&mut self, conversation_id: &str, conv: ConversationId, generation: u64, server_key: &[u8]) -> Result<bool, String> {
        self.active(conversation_id)?;
        let outer = ConversationId::parse(conversation_id).map_err(|_| super::CHECK.to_owned())?;
        if self.engine()?.group_at(outer, generation).map_err(|err| words(&err))? != conv { return Err("Encryption changed. Try again.".to_owned()); }
        let cursor = if generation == 0 { conversation_id.to_owned() } else { format!("{conversation_id}:{}", generation) };
        let device_id = self.engine()?.device_id().to_string();
        let mut moved = false;
        for _ in 0..PASSES {
            let after = self.notes.seqs.get(&cursor).copied().unwrap_or(0);
            let path = self.generation_query(format!("/v1/e2ee/conversations/{conversation_id}/handshake?after={after}&device_id={device_id}"), generation);
            let (status, rows) = self.ask("GET", &path, None)?;
            if status != 200 {
                return Err(format!(
                    "The server did not give the group's changes ({status})."
                ));
            }
            let rows = rows.as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                return Ok(moved);
            }
            let count = rows.len();
            for row in rows {
                if self.generations && (row["generation"].as_u64() != Some(generation)
                    || row["group_id"].as_str().is_none_or(|id| ConversationId::parse(id).is_err())
                    || (generation > 0 && row["group_id"].as_str() != Some(conv.to_string().as_str()))) {
                    return Err("The group's changes do not match this encryption update.".to_owned());
                }
                let seq = row.get("seq").and_then(Value::as_i64).unwrap_or(after);
                let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
                let epoch = row.get("epoch").and_then(Value::as_u64).unwrap_or(0);
                let Some(data) = row.get("data").and_then(Value::as_str).and_then(unb64) else {
                    self.advance(&cursor, seq)?;
                    continue;
                };
                match self.apply(conv, kind, epoch, &data, server_key) {
                    Applied::Done => moved = true,
                    Applied::Skip => {}
                    // A device whose owner's list was not checked yet: check, then once more.
                    Applied::Untrusted(users) => {
                        for user in users {
                            let _ = self.verify_user(&user);
                        }
                        match self.apply(conv, kind, epoch, &data, server_key) {
                            Applied::Done => moved = true,
                            Applied::Skip => {}
                            Applied::Untrusted(_) => {
                                return Err(words(&Error::UntrustedDevices(Vec::new())))
                            }
                            Applied::Stop(why) => return Err(why),
                        }
                    }
                    Applied::Stop(why) => return Err(why),
                }
                self.advance(&cursor, seq)?;
            }
            if count < HANDSHAKE_PAGE {
                return Ok(moved);
            }
        }
        Err("This conversation is still updating. Try again shortly.".to_owned())
    }

    fn advance(&mut self, conversation_id: &str, seq: i64) -> Result<(), String> {
        // Never acknowledge a handshake beyond the MLS snapshot on disk. Welcome errors can
        // leave a joined group only in memory, and commits/proposals use lenient persistence.
        // A failed flush keeps the feed position retryable, including after a cold restart.
        self.engine()?.flush().map_err(|err| {
            format!("The conversation's encryption state could not be saved: {err}")
        })?;
        let at = self
            .notes
            .seqs
            .entry(conversation_id.to_owned())
            .or_insert(0);
        if seq > *at {
            *at = seq;
            self.save_notes();
        }
        Ok(())
    }

    fn apply(
        &mut self,
        conv: ConversationId,
        kind: &str,
        epoch: u64,
        data: &[u8],
        server_key: &[u8],
    ) -> Applied {
        let Ok(engine) = self.engine() else {
            return Applied::Stop("Encryption isn't set up on this device.".to_owned());
        };
        let untrusted = |devices: &[e2ee::DeviceRef]| {
            Applied::Untrusted(
                devices
                    .iter()
                    .map(|device| device.user.to_string())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            )
        };
        match kind {
            "welcome" => match engine.handle_welcome(conv, data, Some(server_key)) {
                Ok(_) => Applied::Done,
                Err(Error::Duplicate | Error::GroupExists) => Applied::Skip,
                Err(Error::UntrustedDevices(devices)) => untrusted(&devices),
                // A Welcome for another key package of ours, or one that does not open: not ours.
                Err(err) => {
                    tracing::warn!("chatd: Tiny e2ee: a Welcome was not taken: {err}");
                    Applied::Skip
                }
            },
            "commit" => {
                if !engine.has_group(conv) {
                    return Applied::Skip;
                }
                match engine.handle_commit(conv, epoch, data) {
                    Ok(outcome) => {
                        if outcome.removed_self {
                            tracing::info!(
                                "chatd: Tiny e2ee: this device left a conversation's group"
                            );
                        }
                        Applied::Done
                    }
                    Err(Error::Duplicate) => Applied::Skip,
                    Err(Error::UntrustedDevices(devices)) => untrusted(&devices),
                    Err(err @ (Error::EpochBackwards { .. } | Error::Frozen(_))) => {
                        Applied::Stop(words(&err))
                    }
                    Err(Error::MissingCommit { .. }) => Applied::Stop(
                        "A change to this conversation's members is missing on the server."
                            .to_owned(),
                    ),
                    Err(err) => {
                        tracing::warn!("chatd: Tiny e2ee: a commit was not applied: {err}");
                        Applied::Skip
                    }
                }
            }
            "proposals" => {
                if !engine.has_group(conv) {
                    return Applied::Skip;
                }
                let Ok(current) = engine.epoch(conv) else {
                    return Applied::Skip;
                };
                if epoch != current {
                    return Applied::Skip;
                }
                match engine.handle_proposals(conv, data) {
                    Ok(outcome) => {
                        let unknown: Vec<e2ee::DeviceRef> = outcome
                            .rejected
                            .iter()
                            .filter(|rejected| {
                                rejected.reason == e2ee::groups::RejectReason::UnknownDevice
                            })
                            .filter_map(|rejected| match &rejected.kind {
                                Some(e2ee::groups::ProposalKind::Add(device)) => {
                                    Some(device.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        if !unknown.is_empty() {
                            return untrusted(&unknown);
                        }
                        if !outcome.rejected.is_empty() {
                            // A forged or unchained device: the group waits rather than let it in.
                            tracing::warn!(
                                "chatd: Tiny e2ee: {} proposal(s) refused; the group waits",
                                outcome.rejected.len()
                            );
                        }
                        Applied::Done
                    }
                    Err(err) => {
                        tracing::warn!("chatd: Tiny e2ee: proposals were not taken: {err}");
                        Applied::Skip
                    }
                }
            }
            _ => Applied::Skip,
        }
    }

    /// Commits the pending proposals if any. True when a commit went through.
    fn commit(&mut self, conversation_id: &str, conv: ConversationId) -> Result<bool, String> {
        let engine = self.engine()?;
        if !engine.has_group(conv) {
            return Ok(false);
        }
        let bundle = match engine.pending_commit(conv) {
            Some(bundle) => bundle,
            None => match engine.commit_pending(conv) {
                Ok(Some(bundle)) => bundle,
                Ok(None) => return Ok(false),
                // This device is the one being removed: another member commits it.
                Err(Error::SelfRemoval) => return Ok(false),
                Err(err) => {
                    return Err(format!("The members' change could not be committed: {err}"))
                }
            },
        };
        let device_id = engine.device_id().to_string();
        let welcomes: Vec<Value> = bundle.welcomes.iter().map(|welcome| json!({ "device_id": welcome.device.device.to_string(), "data": b64(&welcome.data) })).collect();
        let mut body = json!({ "device_id": device_id, "epoch": bundle.epoch, "commit": b64(&bundle.commit), "welcomes": welcomes });
        let active = self.active(conversation_id)?;
        if active.group_id != conv { return Err("Encryption changed. Try again.".to_owned()); }
        self.generation_body(&mut body, active.generation);
        let (status, answer) = self.ask(
            "POST",
            &format!("/v1/e2ee/conversations/{conversation_id}/commit"),
            Some(body),
        )?;
        let engine = self.engine()?;
        match status {
            200 | 204 => {
                engine
                    .confirm_commit(conv)
                    .map_err(|err| format!("The commit could not be kept: {err}"))?;
                Ok(true)
            }
            // Another member's commit won, or the proposals changed: the feed settles ours.
            409 => {
                if answer["error"] == "generation_changed" {
                    self.discover_generations(conversation_id)?;
                    return Ok(true);
                }
                if answer.get("error").and_then(Value::as_str) == Some("proposals_changed") {
                    let _ = engine.abort_commit(conv);
                }
                Ok(true)
            }
            400 => {
                let _ = engine.abort_commit(conv);
                Err("The server refused this device's commit.".to_owned())
            }
            status => Err(format!("The server did not take the commit ({status}).")),
        }
    }

    fn cache(&mut self, conversation_id: &str) -> Option<&mut Cache> {
        // Previously opened text stays readable while enrolment is unavailable or off.
        if self.cache_key.is_none() {
            self.cache_key = self
                .saved
                .e2ee
                .as_ref()
                .and_then(Secrets::keys)
                .map(|(_, key)| key);
        }
        let key = self.cache_key.as_ref()?;
        if !self.caches.contains_key(conversation_id) {
            let cache = Cache::load(&self.dir, conversation_id, key);
            for opened in cache.rows.values() {
                for file in &opened.attachments {
                    self.files
                        .insert(file.id.clone(), (conversation_id.to_owned(), file.clone()));
                }
            }
            self.caches.insert(conversation_id.to_owned(), cache);
        }
        self.caches.get_mut(conversation_id)
    }

    pub(super) fn remember(&mut self, conversation_id: &str, id: &str, opened: Opened) {
        for file in &opened.attachments {
            self.files
                .insert(file.id.clone(), (conversation_id.to_owned(), file.clone()));
        }
        let Some(key) = self.cache_key.clone() else {
            return;
        };
        if let Some(cache) = self.cache(conversation_id) {
            cache.put(&key, id, opened);
        }
    }

    /// What a page of rows says, oldest first, the edits folded into their targets.
    pub(crate) fn read(&mut self, conversation_id: &str, rows: &[Row]) -> HashMap<String, Read> {
        let mut out = HashMap::new();
        let Some(conv) = uuid(conversation_id).map(ConversationId) else {
            return out;
        };
        // One sync attempt per encryption group on this page. A blocked historical
        // generation must not spend the current group's Welcome attempt.
        let mut synced = BTreeSet::new();
        let mut ordered: Vec<&Row> = rows.iter().collect();
        ordered.sort_by_key(|row| (row.generation.unwrap_or(0), row.epoch));
        for row in ordered {
            let read = self.read_one(conversation_id, conv, row, &mut synced);
            out.insert(row.id.to_owned(), read);
        }
        // An edit counts once its target is known, and only from the target's author.
        let edits: Vec<(String, String, Opened, UserId)> = rows.iter()
            .filter(|row| matches!(out.get(row.id), Some(Read::Edit)))
            .filter_map(|row| {
                let opened = self.cache(conversation_id)?.rows.get(row.id)?.clone();
                let auth = opened.auth.as_ref()?;
                if auth.kind != "edit" || !auth.matches(conversation_id, row) { return None; }
                Some((row.id.to_owned(), auth.target_id.clone()?, opened.clone(), auth.author))
            }).collect();
        for (edit_id, target, edit, author) in edits {
            let Some(mut opened) = self.cache(conversation_id).and_then(|cache| cache.rows.get(&target).cloned()) else { continue; };
            let Some(target_auth) = opened.auth.as_ref() else { continue; };
            if target_auth.author != author || target_auth.kind != "message" || target_auth.conversation_id.to_string() != conversation_id { continue; }
            // A malformed current target row must not be restored through the edit path.
            if rows.iter().any(|row| row.id == target && !target_auth.matches(conversation_id, row)) { continue; }
            if opened.edited_by.as_deref().is_some_and(|by| by >= edit_id.as_str()) { continue; }
            opened.text = edit.text;
            opened.attachments = edit.attachments;
            opened.edited_by = Some(edit_id);
            out.insert(target.clone(), Read::Opened(opened.clone()));
            self.remember(conversation_id, &target, opened);
        }
        let _ = self.engine.as_mut().map(Engine::flush);
        // What did not open here may have on another device of the person.
        let missing: Vec<String> = rows
            .iter()
            .filter(|row| {
                row.kind != "edit" && matches!(out.get(row.id), Some(Read::Undecryptable))
            })
            .map(|row| row.id.to_owned())
            .collect();
        if !missing.is_empty() {
            self.want(conversation_id, missing);
        }
        out
    }

    fn read_one(
        &mut self,
        conversation_id: &str,
        _conv: ConversationId,
        row: &Row,
        synced: &mut BTreeSet<ConversationId>,
    ) -> Read {
        let cached = self.cache(conversation_id).and_then(|cache| cache.rows.get(row.id).cloned());
        if let Some(opened) = &cached {
            if let Some(auth) = &opened.auth {
                if !auth.matches(conversation_id, row) { return Read::Undecryptable; }
                return if auth.kind == "edit" { Read::Edit } else { Read::Opened(opened.clone()) };
            }
        }
        let legacy = cached.filter(|opened| opened.auth.is_none() && opened.edits.is_none());
        let fallback = || legacy.clone().map_or(Read::Undecryptable, Read::Legacy);
        // Pending plaintext belongs to the exact local sender and authenticated row context.
        if let Some(client_id) = row.client_id {
            if let Some(opened) = self.cache(conversation_id).and_then(|cache| cache.pending.get(client_id).cloned()) {
                if !opened.auth.as_ref().is_some_and(|auth| auth.matches(conversation_id, row)) { return Read::Undecryptable; }
                if let Some(cache) = self.cache(conversation_id) { cache.pending.remove(client_id); }
                let edit = opened.edits.is_some();
                self.remember(conversation_id, row.id, opened.clone());
                return if edit { Read::Edit } else { Read::Opened(opened) };
            }
        }
        let conv = match self.row_group(conversation_id, row) {
            Ok(conv) => conv,
            Err(why) => { tracing::debug!("chatd: Tiny e2ee: {why}"); return fallback(); }
        };
        let Some(ciphertext) = row.ciphertext.and_then(unb64) else {
            return fallback();
        };
        let (Ok(kind), Ok(author), Ok(device)) = (Kind::parse(row.kind), UserId::parse(row.author_id), DeviceId::parse(row.device_id)) else { return fallback(); };
        let claimed = Claimed { author: Some(author), device: Some(device), kind, target_id: row.target_id };
        let Some(engine) = self.engine.as_mut() else {
            return fallback();
        };
        let mut opened = engine.decrypt(conv, &ciphertext, &claimed);
        // FutureEpoch: behind on handshake. NoGroup / NotAMember: accepted a repair but not
        // the Welcome yet. Limit one sync attempt per encryption group on this page.
        if matches!(
            opened,
            Err(Error::FutureEpoch { .. } | Error::NoGroup | Error::NotAMember)
        ) && synced.insert(conv)
        {
            let generation = row.generation.unwrap_or(0);
            let outer = ConversationId::parse(conversation_id).ok();
            let active = outer.and_then(|outer| self.engine.as_ref()?.active_group(outer).ok());
            let historical_key = active.filter(|active| generation < active.generation)
                .and_then(|_| self.engine.as_ref()?.server_key(conv));
            let sync = if let Some(key) = historical_key {
                self.feed_at(conversation_id, conv, generation, &key).map(|_| ())
            } else { self.sync(conversation_id) };
            if let Err(why) = sync { tracing::warn!("chatd: Tiny e2ee: {conversation_id}: {why}"); }
            let Some(engine) = self.engine.as_mut() else {
                return fallback();
            };
            opened = engine.decrypt(conv, &ciphertext, &claimed);
        }
        if matches!(opened, Err(Error::FutureEpoch { .. } | Error::NoGroup)) {
            self.unopened.insert(conversation_id.to_owned());
        }
        match opened {
            Ok(incoming) => {
                let opened = Opened {
                    text: incoming.envelope.text.clone(),
                    generation: Some(row.generation.unwrap_or(0)),
                    auth: Some(Authentication::new(ConversationId::parse(conversation_id).expect("checked conversation"), conv, row.generation.unwrap_or(0), incoming.epoch, &incoming.sender, incoming.kind, incoming.target_id.as_deref())),
                    saved_attribution: None,
                    reply_to: incoming.envelope.reply_to.clone(),
                    sent_at: incoming.envelope.sent_at,
                    attachments: incoming
                        .envelope
                        .attachments
                        .iter()
                        .map(OpenedFile::of)
                        .collect(),
                    edits: incoming.target_id.clone().filter(|_| kind == Kind::Edit),
                    edited_by: None,
                    at: super::messages::unix_of(row.created_at).unwrap_or(0),
                    imported_from: None,
                };
                if legacy.as_ref().is_some_and(|saved| saved.text != opened.text || saved.attachments != opened.attachments || saved.edited_by.is_some()) {
                    return fallback();
                }
                self.remember(conversation_id, row.id, opened.clone());
                if kind == Kind::Edit {
                    Read::Edit
                } else {
                    Read::Opened(opened)
                }
            }
            Err(err) => {
                tracing::debug!("chatd: Tiny e2ee: a message did not open: {err}");
                fallback()
            }
        }
    }

    /// Encrypts a message or an edit and posts it. The plaintext is kept in the cache, under the
    /// id the server gives it.
    pub(crate) fn post(
        &mut self,
        conversation_id: &str,
        client_id: &str,
        kind: Kind,
        target_id: Option<&str>,
        envelope: &Envelope,
        attachment_ids: &[String],
    ) -> Result<Value, String> {
        if self.notes.key_changed.contains(conversation_id) {
            return Err(words(&Error::ServerKeyChanged));
        }
        let _ = self.active(conversation_id)?;
        let mut opened = Opened {
            text: envelope.text.clone(),
            generation: None,
            auth: None,
            saved_attribution: None,
            reply_to: envelope.reply_to.clone(),
            sent_at: envelope.sent_at,
            attachments: envelope.attachments.iter().map(OpenedFile::of).collect(),
            edits: target_id.map(str::to_owned),
            edited_by: None,
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs() as i64),
            imported_from: None,
        };
        for _ in 0..PASSES {
            // Removals and joins first: a message is never sent to someone already leaving.
            self.sync(conversation_id)?;
            let active = self.active(conversation_id)?;
            let conv = active.group_id;
            let sealed = match self.engine()?.encrypt(conv, kind, target_id, envelope) {
                Ok(sealed) => sealed,
                Err(Error::PendingProposals | Error::PendingRemove | Error::CommitInFlight) => continue,
                Err(err) => return Err(words(&err)),
            };
            opened.generation = Some(active.generation);
            opened.auth = Some(Authentication::new(ConversationId::parse(conversation_id).expect("checked conversation"), conv, active.generation, sealed.epoch, &self.engine()?.device_ref(), kind, target_id));
            if let Some(cache) = self.cache(conversation_id) { cache.pending.insert(client_id.to_owned(), opened.clone()); }
            let device_id = self.engine()?.device_id().to_string();
            let mut body = json!({
                "ciphertext": b64(&sealed.ciphertext),
                "epoch": sealed.epoch,
                "device_id": device_id,
                "client_id": client_id,
                "kind": kind.as_str(),
            });
            self.generation_body(&mut body, active.generation);
            if let Some(target) = target_id {
                body["target_id"] = Value::String(target.to_owned());
            }
            if !attachment_ids.is_empty() {
                body["attachment_ids"] = json!(attachment_ids);
            }
            let (status, answer) = self.ask(
                "POST",
                &format!("/v1/conversations/{conversation_id}/messages"),
                Some(body),
            )?;
            match status {
                200 | 201 => {
                    let id = answer
                        .get("id")
                        .or_else(|| answer.pointer("/message/id"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    if super::safe_id(&id) {
                        if let Some(cache) = self.cache(conversation_id) {
                            cache.pending.remove(client_id);
                        }
                        self.remember(conversation_id, &id, opened);
                    }
                    return Ok(answer);
                }
                409 if matches!(answer.get("error").and_then(Value::as_str), Some("proposals_changed" | "stale_epoch" | "generation_changed")) => continue,
                400 if answer.get("error").and_then(Value::as_str) == Some("e2ee_group_too_large") => {
                    return Err("This encrypted conversation has too many devices to send a message.".to_owned());
                }
                status => return Err(super::explain(status, &answer, super::Missing::Plain).to_owned()),
            }
        }
        Err("This conversation is still updating its devices. Your message was kept; try sending it again shortly.".to_owned())
    }

    /// Opening details for a sealed attachment this device has seen in a message.
    pub(crate) fn sealed_file(&self, id: &str) -> Option<crate::adapters::SealedFile> {
        let (conversation_id, file) = self.files.get(id)?;
        let mut sealed = file.sealed()?;
        sealed.context = attachment_context(conversation_id, &file.id)?;
        Some(sealed)
    }

    /// The person's devices as the server lists them now, against the last list: a device not
    /// seen before (and not this one) is added to the "New device added" notice, a device gone
    /// is dropped from it. True when the notice changed. The first list ever is taken as known.
    fn note_devices(&mut self, rows: &[Value]) -> bool {
        let this = self
            .engine
            .as_ref()
            .map(|engine| engine.device_id().to_string());
        let listed = self.devices_of(&self.saved.user_id.clone(), rows, this.as_deref());
        let ids: BTreeSet<String> = listed
            .iter()
            .map(|device| device.device_id.clone())
            .collect();
        let revoked = self.ready() && this.as_ref().is_some_and(|id| !ids.contains(id));
        let revoked_changed = revoked && self.mark_revoked();
        let before = self.notes.added.clone();
        let first = self.notes.mine.is_empty();
        self.notes
            .added
            .retain(|device| ids.contains(&device.device_id));
        if !first {
            for device in listed {
                if !device.this
                    && !self.notes.mine.contains(&device.device_id)
                    && !self
                        .notes
                        .added
                        .iter()
                        .any(|known| known.device_id == device.device_id)
                {
                    self.notes.added.push(device);
                }
            }
        }
        let changed = self.notes.added != before;
        if changed || self.notes.mine != ids {
            self.notes.mine = ids;
            self.save_notes();
        }
        changed || revoked_changed
    }

    /// The person saw the notice about this device of theirs.
    fn seen_device(&mut self, device_id: &str) {
        self.notes
            .added
            .retain(|device| device.device_id != device_id);
        self.save_notes();
    }

    /// The live socket said something about this protocol. True when the "New device added"
    /// notice changed.
    pub(crate) fn heard(&mut self, kind: &str, frame: &Value) -> bool {
        let field = |name: &str| {
            frame
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match kind {
            "e2ee.handshake" => {
                let conversation_id = field("conversation_id");
                if super::safe_id(&conversation_id) && self.ready() {
                    match self.sync(&conversation_id) {
                        Ok(()) => {
                            self.unopened.remove(&conversation_id);
                            if self.is_self(&conversation_id) {
                                let gained = self.catch_up().into_iter().collect();
                                self.reload(&gained);
                            } else {
                                self.reload(&BTreeSet::from([conversation_id]));
                            }
                        }
                        Err(why) => tracing::warn!("chatd: Tiny e2ee: {conversation_id}: {why}"),
                    }
                }
            }
            "e2ee.keys_low" => {
                let mine = self
                    .engine
                    .as_ref()
                    .map(|engine| engine.device_id().to_string());
                if mine.as_deref() == Some(field("device_id").as_str()) {
                    if let Err(why) = self.top_up(false) {
                        tracing::warn!("chatd: Tiny e2ee: {why}");
                    }
                }
            }
            "e2ee.device" => {
                let user = field("user_id");
                self.checked.remove(&user);
                if user == self.saved.user_id && !self.ready() {
                    // Approved from another device, maybe.
                    self.enrol();
                } else if self.notes.met.contains(&user) || user == self.saved.user_id {
                    if let Err(why) = self.verify_user(&user) {
                        tracing::warn!("chatd: Tiny e2ee: {why}");
                    }
                }
                if user == self.saved.user_id && self.ready() {
                    if let Ok(rows) = self.list(&user) {
                        let devices_changed = self.note_devices(&rows);
                        let sync_changed = self.sibling_notice();
                        return devices_changed || sync_changed;
                    }
                }
            }
            _ => {}
        }
        self.sibling_notice()
    }

    /// `E2eeAsk::Status`.
    pub(crate) fn status(&mut self, names: &HashMap<String, String>) -> E2eeStatus {
        let mut status = E2eeStatus {
            group_generations: self.generations,
            repair: repair::view(&self.saved),
            mode: self.mode.clone(),
            device: self.state.clone(),
            recovery_set: self.notes.recovery.is_some(),
            backup: false,
            linking: self.linking,
            ..E2eeStatus::default()
        };
        let Some(engine) = self.engine.as_ref() else {
            return status;
        };
        let this = engine.device_id().to_string();
        status.device_id = this.clone();
        status.fingerprint = fingerprint(&engine.signature_key());
        if let Ok(rows) = self.list(&self.saved.user_id) {
            status.devices = self.devices_of(&self.saved.user_id.clone(), &rows, Some(&this));
            if self.ready() {
                self.note_devices(&rows);
            }
        }
        status.added = self.notes.added.clone();
        status.device = self.state.clone();
        status.revoked = self.revoked();
        let met: Vec<String> = self
            .notes
            .met
            .iter()
            .filter(|user| **user != self.saved.user_id)
            .cloned()
            .collect();
        for user in met {
            status.people.push(self.person_known(&user, names));
        }
        status
    }

    fn devices_of(&self, user_id: &str, rows: &[Value], this: Option<&str>) -> Vec<E2eeDevice> {
        let user = UserId::parse(user_id).ok();
        rows.iter()
            .filter_map(|row| {
                let device_id = row.get("device_id")?.as_str()?.to_owned();
                let key = unb64(row.get("signature_key")?.as_str()?)?;
                let trusted = match (user, DeviceId::parse(&device_id), self.engine.as_ref()) {
                    (Some(user), Ok(device), Some(engine)) => {
                        engine.trust().is_trusted(user, device, &key)
                    }
                    _ => false,
                };
                Some(E2eeDevice {
                    this: this == Some(device_id.as_str()),
                    untrusted: !trusted && this != Some(device_id.as_str()),
                    label: row
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    created_at: row
                        .get("created_at")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    fingerprint: fingerprint(&key),
                    device_id,
                })
            })
            .collect()
    }

    /// Someone, from what is already trusted here: no request to the server.
    fn person_known(&self, user_id: &str, names: &HashMap<String, String>) -> E2eePerson {
        let mut person = E2eePerson {
            user_id: user_id.to_owned(),
            name: names.get(user_id).cloned().unwrap_or_default(),
            identity_changed: self.notes.changed.contains(user_id),
            ..E2eePerson::default()
        };
        let (Some(engine), Ok(user)) = (self.engine.as_ref(), UserId::parse(user_id)) else {
            return person;
        };
        person.devices = engine
            .trust()
            .devices(user)
            .iter()
            .map(|device| E2eeDevice {
                device_id: device.device.to_string(),
                fingerprint: fingerprint(&device.signature_key),
                ..E2eeDevice::default()
            })
            .collect();
        if !person.devices.is_empty() {
            if let Ok(number) = engine.safety_number(user) {
                person.safety = number.digits().to_owned();
            }
        }
        person.trust = match self.notes.verified.get(user_id) {
            Some(digits) if *digits == person.safety && !person.safety.is_empty() => {
                Trust::Verified
            }
            Some(_) => Trust::Changed,
            None => Trust::Unverified,
        };
        person
    }

    /// `E2eeAsk::Person`: their list fetched and checked now.
    pub(crate) fn person(
        &mut self,
        user_id: &str,
        names: &HashMap<String, String>,
    ) -> Result<E2eePerson, String> {
        if !self.ready() {
            return Err("This device isn't set up for encrypted messages yet.".to_owned());
        }
        let verified = self.verify_user(user_id);
        let rows = self.list(user_id)?;
        let mut person = self.person_known(user_id, names);
        person.devices = self.devices_of(user_id, &rows, None);
        if let Err(why) = verified {
            if !person.identity_changed {
                return Err(why);
            }
        }
        Ok(person)
    }

    pub(crate) fn verify(&mut self, user_id: &str, verified: bool) -> Result<(), String> {
        if verified {
            let person = self.person_known(user_id, &HashMap::new());
            if person.safety.is_empty() {
                return Err(
                    "No device of theirs is trusted yet, so there is no safety number to verify."
                        .to_owned(),
                );
            }
            self.notes
                .verified
                .insert(user_id.to_owned(), person.safety.clone());
            self.save_notes();
            self.tell_verified(user_id, Some(person.safety));
        } else {
            self.notes.verified.remove(user_id);
            self.save_notes();
            self.tell_verified(user_id, None);
        }
        Ok(())
    }

    pub(crate) fn accept_identity(&mut self, user_id: &str) -> Result<(), String> {
        let user = UserId::parse(user_id).map_err(|_| super::CHECK.to_owned())?;
        self.engine()?
            .accept_new_identity(user)
            .map_err(|err| err.to_string())?;
        self.notes.changed.remove(user_id);
        self.notes.verified.remove(user_id);
        self.save_notes();
        self.verify_user(user_id)
    }

    /// Signs and registers a new device of this person from its link code.
    pub(crate) fn approve(&mut self, code: &str, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        if cancelled() { return Err("Device approval timed out.".to_owned()); }
        if !self.ready() {
            return Err("This device has to be set up before it can approve another.".to_owned());
        }
        let (device, key) = parse_link_code(code)
            .ok_or("That isn't a device link code. Copy it again from the new device.")?;
        let signature = self
            .engine()?
            .cross_sign(device, &key)
            .map_err(|err| format!("It couldn't be approved: {err}"))?;
        let body = json!({ "signature_key": b64(&key), "cross_signature": b64(&signature), "label": "Approved device" });
        if cancelled() { return Err("Device approval timed out.".to_owned()); }
        match self.ask("PUT", &format!("/v1/e2ee/devices/{device}"), Some(body))? {
            (200 | 201, _) => {
                let me = self.saved.user_id.clone();
                let _ = self.verify_user(&me);
                Ok(())
            }
            (409, _) => Err("That device is already approved.".to_owned()),
            (status, answer) => {
                Err(super::explain(status, &answer, super::Missing::Plain).to_owned())
            }
        }
    }

    pub(crate) fn revoke(&mut self, device_id: &str) -> Result<(), String> {
        if !super::safe_id(device_id) {
            return Err(super::CHECK.to_owned());
        }
        match self.ask("DELETE", &format!("/v1/e2ee/devices/{device_id}"), None)? {
            (200 | 204, _) => {
                self.seen_device(device_id);
                let mine = self
                    .engine
                    .as_ref()
                    .map(|engine| engine.device_id().to_string());
                if mine.as_deref() == Some(device_id) {
                    self.mark_revoked();
                }
                let me = self.saved.user_id.clone();
                let _ = self.verify_user(&me);
                Ok(())
            }
            (status, answer) => {
                Err(super::explain(status, &answer, super::Missing::Plain).to_owned())
            }
        }
    }

    /// Makes a recovery key, keeps only what checks it, and answers it once.
    pub(crate) fn recovery_key(&mut self) -> Result<String, String> {
        if self.engine.is_none() {
            return Err("Encryption isn't set up on this device.".to_owned());
        }
        let (text, bytes) = new_recovery_key()?;
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|_| "No randomness is available.".to_owned())?;
        let check = recovery_check(&bytes, &salt);
        self.notes.recovery = Some((b64(&salt), b64(&check)));
        self.save_notes();
        Ok(text)
    }

    pub(crate) fn check_recovery(&self, typed: &str) -> Result<(), String> {
        let Some((salt, check)) = &self.notes.recovery else {
            return Err("No recovery key was made on this device.".to_owned());
        };
        let key = parse_recovery(typed).ok_or("A recovery key is 26 letters and digits.")?;
        let (Some(salt), Some(check)) = (unb64(salt), unb64(check)) else {
            return Err("The recovery key's check is unreadable.".to_owned());
        };
        let typed = recovery_check(&key, &salt);
        if subtle_eq(&typed, &check) {
            Ok(())
        } else {
            Err("That isn't this account's recovery key.".to_owned())
        }
    }
}

fn subtle_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

enum Applied {
    Done,
    Skip,
    Untrusted(Vec<String>),
    Stop(String),
}

/// What this device is called in its owner's device list: the computer's name, if it has one.
fn device_label() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let host: String = host.chars().filter(|c| !c.is_control()).take(40).collect();
    if host.is_empty() {
        "tinyclient".to_owned()
    } else {
        format!("tinyclient on {host}")
    }
}

/// The "New device added" notice of this account, from what is known here (no request).
pub(crate) fn added(saved: &Saved) -> Vec<E2eeDevice> {
    started(saved)
        .map(|shared| lock(&shared).notes.added.clone())
        .unwrap_or_default()
}

/// Serves `Request::TinyE2ee`. `told` is called when something the shell shows changed by itself
/// (a device link moved on).
pub(crate) fn answer(
    saved: &Saved,
    ask: protocol::E2eeAsk,
    names: &HashMap<String, String>,
    told: linking::Told,
) -> protocol::Response {
    use protocol::{E2eeAsk, ErrorCode, Response};
    // Owner authentication belongs before the device lock, including the old-server fallback.
    match &ask {
        E2eeAsk::Approve { code } => return approval::approve(saved, code),
        E2eeAsk::LinkJoin { code } if code.trim().starts_with("tiny-device-") => {
            return approval::approve(saved, code);
        }
        _ => {}
    }
    // A link's own steps never wait for this device's lock, which a link holds now and then.
    match ask {
        E2eeAsk::Repair { conversation_id } => {
            return match repair::start(saved, &conversation_id, told) {
                Ok(()) => Response::Ok,
                Err(detail) => Response::Error { code: ErrorCode::Refused, detail },
            };
        }
        E2eeAsk::Link => {
            return Response::TinyE2eeLink {
                link: linking::view(saved),
            }
        }
        E2eeAsk::LinkConfirm { history, check } => {
            return match linking::confirm(saved, &history, &check) {
                Ok(()) => Response::Ok,
                Err(detail) => Response::Error {
                    code: ErrorCode::Refused,
                    detail,
                },
            }
        }
        E2eeAsk::LinkCancel => {
            linking::cancel(saved);
            return Response::Ok;
        }
        // A second of Argon2 and a whole cache: not under the device's lock, which they take
        // only to read the keys and to merge.
        E2eeAsk::ExportBackup { path, passphrase } => {
            let passphrase = e2ee::Zeroizing::new(passphrase);
            let shared = device(saved);
            lock(&shared).enrol_if_needed();
            return match backup::export(&shared, Path::new(&path), &passphrase) {
                Ok(messages) => Response::TinyBackup { messages },
                Err(detail) => Response::Error {
                    code: ErrorCode::Refused,
                    detail,
                },
            };
        }
        E2eeAsk::ImportBackup { path, passphrase } => {
            let passphrase = e2ee::Zeroizing::new(passphrase);
            let shared = device(saved);
            lock(&shared).enrol_if_needed();
            let Some(members) = super::messages::conversation_ids(saved) else {
                return Response::Error {
                    code: ErrorCode::Refused,
                    detail: "Couldn't ask the server which conversations are yours. Check your connection and try again.".to_owned(),
                };
            };
            return match backup::import(&shared, Path::new(&path), &passphrase, members) {
                Ok(messages) => Response::TinyBackup { messages },
                Err(detail) => Response::Error {
                    code: ErrorCode::Refused,
                    detail,
                },
            };
        }
        _ => {}
    }
    let shared = device(saved);
    let mut device = lock(&shared);
    if device.engine.is_none() || device.state != DeviceState::Ready {
        device.enrol();
    }
    let done = |result: Result<(), String>| match result {
        Ok(()) => Response::Ok,
        Err(detail) => Response::Error {
            code: ErrorCode::Refused,
            detail,
        },
    };
    match ask {
        E2eeAsk::Status => Response::TinyE2ee {
            status: device.status(names),
        },
        E2eeAsk::Person { user_id } => match device.person(&user_id, names) {
            Ok(person) => Response::TinyE2eePerson { person },
            Err(detail) => Response::Error {
                code: ErrorCode::Refused,
                detail,
            },
        },
        E2eeAsk::Approve { .. } => unreachable!("approval is handled before taking the device lock"),
        E2eeAsk::Revoke { device_id } => {
            let revoked = device.revoke(&device_id);
            drop(device);
            if revoked.is_ok() {
                // Out of every group soon, not when someone next happens to send.
                linking::nudge(shared.clone());
            }
            done(revoked)
        }
        E2eeAsk::Verify { user_id, verified } => done(device.verify(&user_id, verified)),
        E2eeAsk::AcceptIdentity { user_id } => done(device.accept_identity(&user_id)),
        E2eeAsk::RecoveryKey => match device.recovery_key() {
            Ok(key) => Response::TinyRecoveryKey { key },
            Err(detail) => Response::Error {
                code: ErrorCode::Refused,
                detail,
            },
        },
        E2eeAsk::CheckRecovery { key } => done(device.check_recovery(&key)),
        E2eeAsk::SeenDevice { device_id } => {
            device.seen_device(&device_id);
            Response::Ok
        }
        E2eeAsk::LinkStart => {
            let linking = device.linking;
            drop(device);
            done(linking::start_new(saved, &shared, linking, told))
        }
        E2eeAsk::LinkJoin { code } => {
            drop(device);
            done(linking::start_old(saved, &shared, &code, told))
        }
        E2eeAsk::Link
        | E2eeAsk::LinkConfirm { .. }
        | E2eeAsk::LinkCancel
        | E2eeAsk::ExportBackup { .. }
        | E2eeAsk::ImportBackup { .. } => Response::Ok,
        E2eeAsk::Repair { .. } => unreachable!("repair starts before taking the device lock"),
    }
}

/// The associated data a sealed attachment's first chunk carries besides its header: the 16 bytes
/// of the conversation's id, which the sender knows before it uploads (the attachment's own id is
/// the server's and comes after; see `docs/e2ee-core-notes.md` 1.1). A blob moved into another
/// conversation does not open there.
pub(crate) fn attachment_context(conversation_id: &str, attachment_id: &str) -> Option<Vec<u8>> {
    let mut context = uuid(conversation_id)?.to_vec();
    context.extend_from_slice(&uuid(attachment_id)?);
    Some(context)
}

/// A fresh attachment id, chosen by the sender before it seals the blob: a random (version 4) UUID.
pub(crate) fn attachment_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "No randomness is available.".to_owned())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(ConversationId(bytes).to_string())
}

/// A new attachment key, for a file about to be sealed.
pub(crate) fn attachment_key() -> Result<AttachmentKey, String> {
    AttachmentKey::generate().map_err(|_| "No randomness is available.".to_owned())
}

/// Whether the server is adding someone who has no device in the group yet: a message sent now
/// is one they can never open.
fn joining(info: &Value) -> bool {
    let rows = |list: &str| {
        info.get(list)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let members: BTreeSet<String> = rows("members")
        .iter()
        .filter_map(|row| row.get("user_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    rows("pending").iter().any(|row| {
        row.get("op").and_then(Value::as_str) == Some("add")
            && row
                .get("user_id")
                .and_then(Value::as_str)
                .is_some_and(|user| !members.contains(user))
    })
}

/// Sends a message or an edit to an encrypted conversation ([`Device::post`]), once the people
/// the server is adding are in its group, waiting up to [`JOIN_WAIT`] for that. The wait is off
/// the device's lock, so reading and the live socket go on.
pub(crate) fn send(
    shared: &Shared,
    conversation_id: &str,
    client_id: &str,
    kind: Kind,
    target_id: Option<&str>,
    envelope: &Envelope,
    attachment_ids: &[String],
) -> Result<Value, String> {
    let until = Instant::now() + JOIN_WAIT;
    loop {
        let mut device = lock(shared);
        device.sync(conversation_id)?;
        if !device.joining.contains(conversation_id) {
            break;
        }
        if Instant::now() >= until {
            tracing::warn!(
                "chatd: Tiny e2ee: {conversation_id}: sent while someone is still joining"
            );
            break;
        }
        drop(device);
        std::thread::sleep(join_poll());
    }
    lock(shared).post(
        conversation_id,
        client_id,
        kind,
        target_id,
        envelope,
        attachment_ids,
    )
}

/// In-process tinyserver for the first-message prove and its unit tests. Compiled into chatd so
/// `TINYCLIENT_PROVE=first-encrypted-message` can run without `cargo test`.
mod fake;

/// Lab prove: see [`fake::prove_first_encrypted_message`].
pub fn prove_first_encrypted_message() -> Result<(), String> {
    fake::prove_first_encrypted_message()
}
mod backup;
mod linking;
mod approval;
mod repair;
mod cache_auth;
use cache_auth::Authentication;
mod export_rows;
pub(crate) mod siblings;

#[cfg(test)]
mod live;

#[cfg(test)]
mod receive_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use e2ee::store::Store;

    fn posting_device(name: &str, reject: usize) -> (Device, Arc<Mutex<Vec<Value>>>) {
        let server_key = e2ee::identity::DeviceKey::generate().unwrap().public_key();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let posted = requests.clone();
        let public = server_key.clone();
        let base = super::super::tests::serve(move |request| {
            let (status, body) = if request.starts_with("GET /v1/e2ee/conversations/") && request.contains("/handshake?") {
                if request.contains("after=0&") {
                    (200, json!([{"seq":1,"kind":"unknown","epoch":0,"data":""}]))
                } else {
                    (200, json!([]))
                }
            } else if request.starts_with("GET /v1/e2ee/conversations/") {
                (200, json!({"e2ee": true, "epoch": 0, "external_sender": b64(&public)}))
            } else if request.starts_with("POST /v1/conversations/") {
                let body = request.split_once("\r\n\r\n").unwrap().1;
                let mut posts = lock(&posted);
                posts.push(serde_json::from_str(body).unwrap());
                if posts.len() <= reject {
                    (409, json!({"error": "proposals_changed"}))
                } else {
                    (201, json!({"id": "accepted"}))
                }
            } else {
                (200, json!([]))
            };
            super::super::tests::http(status, "Test", &body.to_string())
        });
        let saved = Saved {
            base_url: base, token: "t".into(), user_id: "11111111-1111-1111-1111-111111111111".into(),
            email: None, display_name: "Ada".into(), role: "member".into(), plain_http_ok: false, e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir = std::env::temp_dir().join(format!("chatd-post-{}-{name}", std::process::id()));
        private_dir(&device.dir).unwrap();
        let mut engine = Engine::detached(UserId::parse(&saved.user_id).unwrap()).unwrap();
        engine.create_group(ConversationId::parse("22222222-2222-2222-2222-222222222222").unwrap(), &server_key).unwrap();
        device.engine = Some(engine);
        device.cache_key = Some(CacheKey::generate().unwrap());
        device.state = DeviceState::Ready;
        (device, requests)
    }

    #[test]
    fn device_membership_changes_are_settled_immediately() {
        let (mut device, _) = posting_device("nudge", 0);
        let conversation = "22222222-2222-2222-2222-222222222222";
        device.note_encrypted(conversation, true);
        let shared = Arc::new(Mutex::new(device));
        linking::nudge(shared.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if lock(&shared).notes.seqs.get(conversation) == Some(&1) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "a linked device must not wait for the old batching timer to get its Welcome");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        lock(&shared).state = DeviceState::Failed { why: "test complete".into() };
    }

    #[test]
    fn a_removed_device_stops_claiming_ready_without_losing_its_local_keys() {
        let (mut device, _) = posting_device("status-revoked", 0);
        let own_id = device.engine.as_ref().unwrap().device_id();
        let conversation = "22222222-2222-2222-2222-222222222222";
        let message = "33333333-3333-3333-3333-333333333333";
        device.remember(conversation, message, Opened { text: "saved before removal".into(), ..Opened::default() });
        let status = device.status(&HashMap::new());
        assert_ne!(status.device, DeviceState::Ready, "the confirmed device list no longer contains this device");
        assert!(status.revoked);
        assert!(!device.ready());
        assert_eq!(device.engine.as_ref().unwrap().device_id(), own_id);
        assert!(device.cache_key.is_some(), "removing future access must keep saved history keys");
        let notes: Value = serde_json::from_slice(&std::fs::read(device.dir.join("notes.json")).unwrap()).unwrap();
        assert_eq!(notes["revoked_device"], own_id.to_string());
        let row = Row { id: message, author_id: &device.saved.user_id.clone(), kind: "message",
            ciphertext: None, epoch: 0, generation: None, group_id: None, device_id: "", target_id: None, client_id: None, created_at: "" };
        let mut read = device.read(conversation, &[row]);
        assert!(matches!(read.remove(message), Some(Read::Legacy(opened)) if opened.text == "saved before removal"));
    }

    #[test]
    fn explicit_self_removal_keeps_saved_history_and_does_not_register_again() {
        let (mut device, _) = posting_device("explicit-self-removal", 0);
        let own_id = device.engine.as_ref().unwrap().device_id();
        let conversation = "22222222-2222-2222-2222-222222222222";
        let message = "33333333-3333-3333-3333-333333333333";
        device.remember(conversation, message, Opened { text: "saved before self removal".into(), ..Opened::default() });
        let registrations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = registrations.clone();
        device.saved.base_url = super::super::tests::serve(move |request| {
            if request.starts_with("GET /v1/capabilities ") {
                return super::super::tests::http(200, "OK", r#"{"features":["e2ee_text"],"e2ee_text":"optional"}"#);
            }
            if request.starts_with("PUT ") {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return super::super::tests::http(409, "Conflict", r#"{"error":"device_exists"}"#);
            }
            super::super::tests::http(200, "OK", "[]")
        });
        device.revoke("44444444-4444-4444-4444-444444444444").unwrap();
        assert!(!device.revoked(), "removing another device must keep this device approved");
        assert_eq!(device.state, DeviceState::Ready);
        device.revoke(&own_id.to_string()).unwrap();
        assert!(device.revoked(), "explicit self removal must persist the same terminal marker as external removal");
        assert!(matches!(device.try_enrol().unwrap(), DeviceState::Failed { .. }));
        assert_eq!(registrations.load(std::sync::atomic::Ordering::SeqCst), 0);
        let status = device.status(&HashMap::new());
        assert!(status.revoked);
        assert!(matches!(status.device, DeviceState::Failed { ref why } if why == "This device was removed. Saved messages are still available."));
        assert_eq!(device.engine.as_ref().unwrap().device_id(), own_id);
        assert!(device.cache_key.is_some());
        let notes: Value = serde_json::from_slice(&std::fs::read(device.dir.join("notes.json")).unwrap()).unwrap();
        assert_eq!(notes["revoked_device"], own_id.to_string());
        let row = Row { id: message, author_id: &device.saved.user_id.clone(), kind: "message",
            ciphertext: None, epoch: 0, generation: None, group_id: None, device_id: "", target_id: None, client_id: None, created_at: "" };
        assert!(matches!(device.read(conversation, &[row]).remove(message), Some(Read::Legacy(opened)) if opened.text == "saved before self removal"));
    }

    #[test]
    fn a_failed_device_list_request_does_not_claim_revocation() {
        let (mut device, _) = posting_device("status-offline", 0);
        device.saved.base_url = super::super::tests::serve(|_| super::super::tests::http(503, "Unavailable", "{}"));
        let status = device.status(&HashMap::new());
        assert_eq!(status.device, DeviceState::Ready);
        assert!(!status.revoked);
        assert!(device.ready());
    }

    #[test]
    fn a_remembered_removed_identity_is_not_registered_again() {
        let (mut device, _) = posting_device("revoked-reopen", 0);
        device.mark_revoked();
        device.notes = serde_json::from_slice(&std::fs::read(device.dir.join("notes.json")).unwrap()).unwrap();
        device.state = DeviceState::Off;
        let registrations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = registrations.clone();
        device.saved.base_url = super::super::tests::serve(move |request| {
            if request.starts_with("PUT ") { observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
            super::super::tests::http(200, "OK", r#"{"features":["e2ee_text"],"e2ee_text":"optional"}"#)
        });
        assert!(matches!(device.try_enrol().unwrap(), DeviceState::Failed { .. }));
        assert!(device.revoked());
        assert_eq!(registrations.load(std::sync::atomic::Ordering::SeqCst), 0);
        device.engine = Some(Engine::detached(UserId::parse(&device.saved.user_id).unwrap()).unwrap());
        assert!(!device.revoked(), "the remembered removal belongs only to the old identity");
    }

    #[test]
    fn a_membership_race_reseals_the_same_outgoing_message_before_retrying() {
        let (mut device, posts) = posting_device("retry", 1);
        let mut envelope = Envelope::message("private retry");
        envelope.sent_at = Some(1_704_164_645_900);
        let result = device.post("22222222-2222-2222-2222-222222222222", "same-client", Kind::Message, None, &envelope, &[]);
        assert!(result.is_ok(), "{result:?}");
        let posts = lock(&posts);
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0]["client_id"], "same-client");
        assert_eq!(posts[1]["client_id"], "same-client");
        assert_ne!(posts[0]["ciphertext"], posts[1]["ciphertext"], "refreshing must re-encrypt rather than resend stale ciphertext");
        assert!(posts.iter().all(|post| post.get("sent_at").is_none() && post.get("body").is_none()));
        let opened = device.cache("22222222-2222-2222-2222-222222222222").unwrap().rows.get("accepted").unwrap();
        assert_eq!(opened.text, "private retry");
        assert_eq!(opened.sent_at, Some(1_704_164_645_900));
    }

    #[test]
    fn an_unsettled_audience_fails_closed_and_keeps_the_outgoing_plaintext() {
        let (mut device, posts) = posting_device("unsettled", usize::MAX);
        let result = device.post("22222222-2222-2222-2222-222222222222", "pending", Kind::Message, None, &Envelope::message("kept for retry"), &[]);
        assert!(result.is_err());
        assert_eq!(lock(&posts).len(), PASSES, "membership retries are bounded");
        let cache = device.cache("22222222-2222-2222-2222-222222222222").unwrap();
        assert_eq!(cache.pending.get("pending").unwrap().text, "kept for retry");
        assert!(!cache.rows.contains_key("accepted"));
    }

    #[test]
    fn a_handshake_schedules_a_reread_of_messages_that_may_now_open() {
        let (mut device, _) = posting_device("handshake-reread", 0);
        let (send, receive) = std::sync::mpsc::channel();
        device.commands = Some(send);
        device.heard("e2ee.handshake", &json!({"conversation_id":"22222222-2222-2222-2222-222222222222"}));
        assert!(matches!(receive.try_recv(), Ok(crate::adapters::Command::Backfill(id))
            if id == "22222222-2222-2222-2222-222222222222"));
    }

    #[test]
    fn encrypted_records_appended_after_an_unterminated_tail_survive_restart() {
        assert_eq!(prove_cache_append_tail(), Ok(()));
    }

    #[test]
    fn encrypted_history_beyond_twenty_thousand_messages_survives_restart() {
        assert_eq!(prove_cache_history_limit(), Ok(()));
    }

    #[test]
    fn a_recovery_key_is_128_bits_in_six_groups_and_reads_back() {
        let (text, bytes) = new_recovery_key().unwrap();
        assert_eq!(text.len(), 26 + 5);
        assert_eq!(text.split('-').count(), 6);
        assert_eq!(parse_recovery(&text), Some(bytes));
        assert_eq!(
            parse_recovery(&text.to_lowercase().replace('-', " ")),
            Some(bytes)
        );
        assert_eq!(parse_recovery("too short"), None);
        let (other, _) = new_recovery_key().unwrap();
        assert_ne!(text, other, "two keys are not the same");
    }

    #[test]
    fn a_link_code_carries_the_device_and_its_key_and_nothing_else_reads_as_one() {
        let device = DeviceId::random().unwrap();
        let mut key = vec![4u8];
        key.extend_from_slice(&[9u8; 64]);
        let code = link_code(device, &key);
        assert_eq!(parse_link_code(&code), Some((device, key.clone())));
        assert_eq!(parse_link_code(&format!("  {code}\n")), Some((device, key)));
        assert_eq!(parse_link_code("tiny-device-AAAA"), None);
        assert_eq!(parse_link_code("hello"), None);
    }

    #[test]
    fn the_keys_go_into_the_right_server_of_a_session() {
        let saved = |base: &str, user: &str| Saved {
            base_url: base.into(),
            token: "t".into(),
            user_id: user.into(),
            email: None,
            display_name: String::new(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let secrets = Secrets::generate().unwrap();
        let one = r#"{"base_url":"https://a","token":"t","user_id":"u1","display_name":"A","role":"owner"}"#;
        let written = with_secrets(one, &saved("https://a", "u1"), &secrets).unwrap();
        let back = super::super::parse(&written).unwrap();
        assert!(back.e2ee.as_ref().and_then(Secrets::keys).is_some());
        assert!(
            with_secrets(
                &written,
                &saved("https://a", "u1"),
                &Secrets::generate().unwrap()
            )
            .is_none(),
            "keys already kept are never replaced"
        );
        assert!(with_secrets(one, &saved("https://b", "u1"), &secrets).is_none());
        let two = format!("[{one},{}]", one.replace("https://a", "https://b"));
        let written: Value =
            serde_json::from_str(&with_secrets(&two, &saved("https://b", "u1"), &secrets).unwrap())
                .unwrap();
        assert!(written[0].get("e2ee").is_none() && written[1].get("e2ee").is_some());
    }

    #[test]
    fn a_fingerprint_is_stable_and_short() {
        let print = fingerprint(&[4u8; 65]);
        assert_eq!(print, fingerprint(&[4u8; 65]));
        assert_eq!(print.split(' ').count(), 8);
        assert_ne!(print, fingerprint(&[5u8; 65]));
    }

    #[cfg(windows)]
    #[test]
    fn dacl_comparison_accepts_sid_aliases_without_accepting_other_trustees() {
        let canonical = windows_acl::canonical_dacl_sddl;
        let numeric = canonical("D:P(A;OICI;FA;;;S-1-5-18)").unwrap();
        assert_eq!(numeric, canonical("D:P(A;OICI;FA;;;SY)").unwrap());
        assert!(!numeric.contains("S-1-5-18"), "Windows emits the SY alias");
        assert_ne!(numeric, canonical("D:P(A;OICI;FA;;;OW)").unwrap());
        assert_ne!(
            numeric,
            canonical("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BU)").unwrap()
        );
    }

    #[test]
    fn the_folder_is_the_owners_alone() {
        let dir = std::env::temp_dir().join(format!("chatd-e2ee-private-{}", std::process::id()));
        private_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        #[cfg(windows)]
        {
            let sid = windows_acl::process_user_sid_string().unwrap();
            let dacl = windows_acl::dacl_sddl(&dir).unwrap();
            let expected = windows_acl::canonical_dacl_sddl(&format!(
                "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"
            ))
            .unwrap();
            assert_eq!(
                dacl, expected,
                "protected DACL must grant only the process user and SYSTEM full control"
            );
            assert!(
                dacl.contains(";;;SY)"),
                "protected DACL must keep SYSTEM: {dacl}"
            );
            assert!(
                !dacl.contains(";;;OW)"),
                "owner-rights OW locks out when the directory owner is not the process user: {dacl}"
            );
            for broad in [";;;WD)", ";;;BU)", ";;;BA)"] {
                assert!(
                    !dacl.contains(broad),
                    "protected DACL must not grant {broad}: {dacl}"
                );
            }
            let store = FileStore::new(&dir);
            store.save("device.state", b"state-one").unwrap();
            store.save("device.state", b"state-two").unwrap();
            assert_eq!(
                store.load("device.state").unwrap().as_deref(),
                Some(&b"state-two"[..])
            );
            write_private(&dir.join("marker"), b"keep").unwrap();
            assert_eq!(std::fs::read(dir.join("marker")).unwrap(), b"keep");
            let again = windows_acl::dacl_sddl(&dir).unwrap();
            assert_eq!(again, expected);
        }
        #[cfg(not(windows))]
        {
            write_private(&dir.join("x"), b"y").unwrap();
            assert_eq!(std::fs::read(dir.join("x")).unwrap(), b"y");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Administrators-owned directory that initially grants the interactive user full control.
    /// Set `TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE` to that directory. Run under a limited
    /// interactive token; do not take ownership or repair already-locked product folders.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE from elevated setup; run with --ignored"]
    fn administrators_owned_fixture_keeps_user_bytes_and_persist() {
        let raw = std::env::var("TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE")
            .expect("TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE must point at the prepared directory");
        let dir = PathBuf::from(raw);
        assert!(
            dir.is_absolute() && dir.is_dir(),
            "fixture must be an absolute existing directory"
        );
        let marker = std::fs::read(dir.join("marker.bin")).expect("preexisting marker.bin");
        assert_eq!(
            marker,
            [1, 2, 3, 4, 5, 9, 9, 9],
            "dedicated permission-test fixture required; never use a real profile"
        );
        let state = std::fs::read(dir.join("device.state")).expect("preexisting device.state");
        let cache = std::fs::read(dir.join("cache-c1.log")).expect("preexisting cache-c1.log");
        private_dir(&dir).expect("private_dir must succeed while the user still has WRITE_DAC");
        assert_eq!(std::fs::read(dir.join("marker.bin")).unwrap(), marker);
        assert_eq!(std::fs::read(dir.join("device.state")).unwrap(), state);
        assert_eq!(std::fs::read(dir.join("cache-c1.log")).unwrap(), cache);
        let sid = windows_acl::process_user_sid_string().unwrap();
        let dacl = windows_acl::dacl_sddl(&dir).unwrap();
        let expected =
            windows_acl::canonical_dacl_sddl(&format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"))
                .unwrap();
        assert_eq!(dacl, expected);
        assert!(dacl.contains(";;;SY)"), "DACL missing SYSTEM: {dacl}");
        assert!(!dacl.contains(";;;OW)"), "DACL must not use OW: {dacl}");
        for broad in [";;;WD)", ";;;BU)", ";;;BA)"] {
            assert!(!dacl.contains(broad), "DACL must not grant {broad}: {dacl}");
        }
        let store = FileStore::new(&dir);
        store.save("device.state", b"after-one").unwrap();
        store.save("device.state", b"after-two").unwrap();
        assert_eq!(
            store.load("device.state").unwrap().as_deref(),
            Some(&b"after-two"[..])
        );
        assert_eq!(std::fs::read(dir.join("marker.bin")).unwrap(), marker);
        assert_eq!(std::fs::read(dir.join("cache-c1.log")).unwrap(), cache);
        let reopen = FileStore::new(&dir);
        assert_eq!(
            reopen.load("device.state").unwrap().as_deref(),
            Some(&b"after-two"[..])
        );
        let again = windows_acl::dacl_sddl(&dir).unwrap();
        assert_eq!(again, expected);
    }

    /// A device of the test session's account that has opened one message, `m1` in `c1`.
    fn a_device_that_read_something(account: &protocol::AccountKey) -> Saved {
        let mut saved =
            super::super::parse(&crate::session::load(protocol::Network::Tiny).unwrap()).unwrap();
        assert_eq!(saved.user_id, account.account);
        let _ = std::fs::remove_dir_all(dir_of(&saved));
        saved.e2ee = Some(Secrets::generate().unwrap());
        let shared = device(&saved);
        let mut device = lock(&shared);
        device.saved.e2ee.clone_from(&saved.e2ee);
        private_dir(&device.dir).unwrap();
        device.cache_key = saved
            .e2ee
            .as_ref()
            .and_then(Secrets::keys)
            .map(|(_, cache)| cache);
        device.remember(
            "c1",
            "m1",
            Opened {
                text: "kept".into(),
                ..Opened::default()
            },
        );
        device.notes.seqs.insert("c1".into(), 7);
        device.notes.verified.insert("u2".into(), "123".into());
        device.save_notes();
        std::fs::write(device.dir.join(STATE), b"a state sealed under the old key").unwrap();
        saved
    }

    /// What a new device of the same account, signed in here again, finds of `m1`.
    fn signed_in_again(saved: &Saved) -> (Device, Option<String>) {
        let mut again = saved.clone();
        again.e2ee = None;
        let mut device = Device::new(&again);
        let (_, cache) = device.secrets().unwrap().keys().unwrap();
        let text = Cache::load(&device.dir, "c1", &cache)
            .rows
            .get("m1")
            .map(|opened| opened.text.clone());
        (device, text)
    }

    #[test]
    fn signing_out_keeps_what_was_read_and_signing_in_again_reads_it() {
        super::super::tests::with_session("http://127.0.0.1:9", "member", |account| {
            let saved = a_device_that_read_something(&account);
            sign_out(&saved);
            let dir = dir_of(&saved);
            assert!(
                !dir.join(STATE).exists(),
                "the device itself is gone: the next one is new"
            );
            assert!(started(&saved).is_none());
            let (device, text) = signed_in_again(&saved);
            assert_eq!(
                text.as_deref(),
                Some("kept"),
                "the history opens under the kept key"
            );
            assert!(
                device.notes.seqs.is_empty(),
                "a new device walks the handshakes again"
            );
            assert_eq!(
                device.notes.verified.get("u2").map(String::as_str),
                Some("123")
            );
            assert!(
                kept().unwrap().is_empty(),
                "the kept key moved back into the session"
            );
            let _ = std::fs::remove_dir_all(dir);
        });
    }

    #[test]
    fn signing_out_and_erasing_leaves_nothing_to_read() {
        super::super::tests::with_session("http://127.0.0.1:9", "member", |account| {
            let saved = a_device_that_read_something(&account);
            sign_out(&saved);
            assert!(!kept().unwrap().is_empty());
            erase_history(&account.account);
            assert!(!dir_of(&saved).exists());
            assert!(kept().unwrap().is_empty());
            let (_, text) = signed_in_again(&saved);
            assert_eq!(text, None);
            let _ = std::fs::remove_dir_all(dir_of(&saved));
        });
    }

    #[test]
    fn a_state_whose_key_is_gone_is_started_over_not_a_failure() {
        super::super::tests::with_session("http://127.0.0.1:9", "member", |account| {
            let saved = a_device_that_read_something(&account);
            // The session lost its keys without a sign-out (an older client's, or a keychain
            // restored from before): the folder is still there.
            lock(devices()).remove(&(saved.base_url.clone(), saved.user_id.clone()));
            lock(scratch_secrets()).clear();
            let (device, text) = signed_in_again(&saved);
            assert!(!device.dir.join(STATE).exists());
            assert_eq!(text, None, "without the kept key the old rows stay sealed");
            let _ = std::fs::remove_dir_all(dir_of(&saved));
        });
    }

    #[test]
    fn a_device_of_the_person_that_was_not_there_before_is_noticed_until_seen() {
        let saved = Saved {
            base_url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            user_id: "7e57ada0-0000-4000-8000-0000000000aa".into(),
            email: None,
            display_name: String::new(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir = std::env::temp_dir().join(format!("chatd-e2ee-notice-{}", std::process::id()));
        private_dir(&device.dir).unwrap();
        let user = UserId::parse(&saved.user_id).unwrap();
        device.engine = Some(Engine::detached(user).unwrap());
        let this = device.engine.as_ref().unwrap().device_id().to_string();
        let row = |id: &str| json!({ "device_id": id, "signature_key": b64(&[4u8; 65]), "label": "Phone" });
        let other = DeviceId::random().unwrap().to_string();
        let third = DeviceId::random().unwrap().to_string();
        assert!(
            !device.note_devices(&[row(&this), row(&other)]),
            "the first list is what the person had"
        );
        assert!(device.notes.added.is_empty());
        assert!(device.note_devices(&[row(&this), row(&other), row(&third)]));
        assert_eq!(device.notes.added.len(), 1);
        assert_eq!(device.notes.added[0].device_id, third);
        assert_eq!(device.notes.added[0].label, "Phone");
        assert!(
            !device.note_devices(&[row(&this), row(&other), row(&third)]),
            "said once"
        );
        device.seen_device(&third);
        assert!(device.notes.added.is_empty());
        // One revoked elsewhere before the person looked takes its notice with it.
        let fourth = DeviceId::random().unwrap().to_string();
        assert!(device.note_devices(&[row(&this), row(&fourth)]));
        assert!(device.note_devices(&[row(&this)]));
        assert!(device.notes.added.is_empty());
        let _ = std::fs::remove_dir_all(&device.dir);
    }

    #[test]
    fn opened_text_reads_the_sealed_cache_when_the_store_row_is_gone() {
        let secrets = Secrets::generate().unwrap();
        let saved = Saved {
            base_url: "http://127.0.0.1:9".to_owned(),
            token: "t".to_owned(),
            user_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
            email: None,
            display_name: "Ada".to_owned(),
            role: "owner".to_owned(),
            plain_http_ok: false,
            e2ee: Some(secrets),
        };
        let conversation = "c0c0c0c0-c0c0-4c0c-8c0c-c0c0c0c0c0c0";
        let message = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        write_opened_cache(&saved, conversation, message, "still here").unwrap();
        assert_eq!(
            opened_text(&saved, conversation, message).as_deref(),
            Some("still here")
        );
        assert_eq!(
            opened_text(&saved, conversation, "cccccccc-cccc-4ccc-8ccc-cccccccccccc"),
            None
        );
        let _ = std::fs::remove_dir_all(dir_of(&saved));
    }

    #[test]
    fn the_cache_opens_only_its_own_rows_under_its_own_key() {
        let dir = std::env::temp_dir().join(format!("chatd-e2ee-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = CacheKey::generate().unwrap();
        let mut cache = Cache::load(&dir, "c1", &key);
        cache.put(
            &key,
            "m1",
            Opened {
                text: "hello".into(),
                ..Opened::default()
            },
        );
        let raw = std::fs::read_to_string(dir.join("cache-c1.log")).unwrap();
        assert!(!raw.contains("hello"), "the plaintext is sealed on disk");
        assert_eq!(
            Cache::load(&dir, "c1", &key)
                .rows
                .get("m1")
                .map(|o| o.text.clone())
                .as_deref(),
            Some("hello")
        );
        assert!(
            Cache::load(&dir, "c1", &CacheKey::generate().unwrap())
                .rows
                .is_empty(),
            "another key opens nothing"
        );
        // A row copied under another id does not open there.
        let (_, blob) = raw.trim().split_once(' ').unwrap();
        std::fs::write(dir.join("cache-c1.log"), format!("m2 {blob}\n")).unwrap();
        assert!(Cache::load(&dir, "c1", &key).rows.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
