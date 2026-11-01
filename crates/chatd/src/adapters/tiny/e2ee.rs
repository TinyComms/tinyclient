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

/// The engine's snapshot under the store.
const STATE: &str = "device.state";
/// Key packages kept on the server: refilled to this when it says it is low, or below half.
const KEY_PACKAGES: usize = 50;
/// Handshake rows asked for at once (the server gives at most 100).
const HANDSHAKE_PAGE: usize = 100;
/// Passes over the handshake feed in one sync, so a busy group cannot hold the thread for good.
const PASSES: usize = 8;
/// Opened messages kept per conversation in the cache.
const CACHED: usize = 20_000;

/// The keys the keychain keeps for this device, inside the session JSON. Not `Debug`.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Secrets {
    /// Base64 of 32 bytes: the state key.
    state: String,
    /// Base64 of 32 bytes: the cache key.
    cache: String,
}

impl Secrets {
    fn generate() -> Result<Secrets, String> {
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
        for row in rows.iter_mut().filter(|row| matches(row)) {
            row["e2ee"] = secrets.clone();
            changed = true;
        }
    } else if value.is_object() && matches(&value) {
        value["e2ee"] = secrets;
        changed = true;
    }
    changed.then(|| value.to_string())
}

/// The owner-only directory this account keeps its encrypted text state in.
fn dir_of(saved: &Saved) -> PathBuf {
    let safe: String = format!("Tiny-{}", saved.user_id)
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
/// that holds only the owner and the system (no inherited entries, so another account on the
/// machine with read access to the profile still cannot list it).
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    windows_acl::owner_only(dir)?;
    Ok(())
}

#[cfg(windows)]
mod windows_acl {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Full control for the owner (`OW`, owner rights) and the system, inherited by what is made
    /// inside; nothing inherited from above.
    pub fn owner_only(dir: &Path) -> std::io::Result<()> {
        let sddl = wide(std::ffi::OsStr::new("D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"));
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
}

/// One opened message, as the cache keeps it.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Opened {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<OpenedFile>,
    /// An edit: the message it edits. Its text replaces the target's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<String>,
    /// The newest edit folded into this message, by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_by: Option<String>,
}

/// An attachment of an opened message: enough to fetch and open it again.
#[derive(Clone, Default, Serialize, Deserialize)]
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
    pub device_id: &'a str,
    pub target_id: Option<&'a str>,
    pub client_id: Option<&'a str>,
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
        if self.rows.len() >= CACHED && !self.rows.contains_key(id) {
            return;
        }
        if let Ok(plain) = serde_json::to_vec(&opened) {
            if let Ok(blob) = e2ee::store::seal_cache_value(key, id.as_bytes(), &plain) {
                let line = format!("{id} {}\n", STANDARD.encode(blob));
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
    if let Some(found) = lock(devices()).get(&key) {
        let found = found.clone();
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

/// Signs this install in as a device: called when the account starts. Never fails the account.
pub(crate) fn begin(saved: &Saved) {
    let shared = device(saved);
    let mut device = lock(&shared);
    device.enrol();
}

/// Signing out: the device is revoked on the server, then forgotten here.
pub(crate) fn sign_out(saved: &Saved) {
    if let Some(shared) = started(saved) {
        let device = lock(&shared);
        if let Some(engine) = device.engine.as_ref() {
            let id = engine.device_id().to_string();
            let _ = device.ask("DELETE", &format!("/v1/e2ee/devices/{id}"), None);
        }
    }
    forget(saved);
}

/// Forgets the account's device: its state, its cache and its notes.
pub(crate) fn forget(saved: &Saved) {
    lock(devices()).remove(&(saved.base_url.clone(), saved.user_id.clone()));
    let _ = std::fs::remove_dir_all(dir_of(saved));
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
        }
    }

    fn ask(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, Value), String> {
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
    fn secrets(&self) -> Result<Secrets, String> {
        let key = (self.saved.base_url.clone(), self.saved.user_id.clone());
        if let Some(secrets) = &self.saved.e2ee {
            return Ok(secrets.clone());
        }
        if let Some(secrets) = lock(scratch_secrets()).get(&key) {
            return Ok(secrets.clone());
        }
        let secrets = Secrets::generate()?;
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
        self.register()
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
        let user = UserId::parse(user_id).map_err(|_| super::CHECK.to_owned())?;
        let rows = self.list(user_id)?;
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
    pub(crate) fn is_encrypted(&mut self, conversation_id: &str) -> bool {
        if let Some(known) = self.encrypted.get(conversation_id) {
            return *known;
        }
        let known = match self.ask(
            "GET",
            &format!("/v1/e2ee/conversations/{conversation_id}"),
            None,
        ) {
            Ok((200, body)) => body.get("e2ee").and_then(Value::as_bool).unwrap_or(false),
            _ => return false,
        };
        self.encrypted.insert(conversation_id.to_owned(), known);
        known
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

    pub(crate) fn ready(&self) -> bool {
        self.state == DeviceState::Ready && self.engine.is_some()
    }

    /// Brings this device's view of one conversation's group up to date: verifies the members,
    /// starts the group if nobody has, applies the handshake feed, and commits what is pending.
    pub(crate) fn sync(&mut self, conversation_id: &str) -> Result<(), String> {
        if !self.ready() {
            return Err("This device isn't set up for encrypted messages yet.".to_owned());
        }
        let conv = ConversationId(uuid(conversation_id).ok_or(super::CHECK)?);
        for _ in 0..PASSES {
            let (status, info) = self.ask(
                "GET",
                &format!("/v1/e2ee/conversations/{conversation_id}"),
                None,
            )?;
            if status != 200 {
                return Err(super::explain(status, &info, super::Missing::Plain).to_owned());
            }
            if info.get("e2ee").and_then(Value::as_bool) != Some(true) {
                self.note_encrypted(conversation_id, false);
                return Ok(());
            }
            self.note_encrypted(conversation_id, true);
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
            for user in &users {
                if let Err(why) = self.verify_user(user) {
                    tracing::warn!("chatd: Tiny e2ee: {conversation_id}: {why}");
                }
            }
            let has = self.engine()?.has_group(conv);
            if !has {
                if info.get("epoch").is_none_or(Value::is_null) {
                    // Nobody has started it: this device does.
                    let device_id = self.engine()?.device_id().to_string();
                    let (status, _) = self.ask(
                        "POST",
                        &format!("/v1/e2ee/conversations/{conversation_id}/group"),
                        Some(json!({ "device_id": device_id })),
                    )?;
                    match status {
                        201 | 200 => self
                            .engine()?
                            .create_group(conv, &server_key)
                            .map_err(|err| format!("The group could not be started: {err}"))?,
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
            if !moved && !committed {
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
        let device_id = self.engine()?.device_id().to_string();
        let mut moved = false;
        loop {
            let after = self.notes.seqs.get(conversation_id).copied().unwrap_or(0);
            let (status, rows) = self.ask("GET", &format!("/v1/e2ee/conversations/{conversation_id}/handshake?after={after}&device_id={device_id}"), None)?;
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
                let seq = row.get("seq").and_then(Value::as_i64).unwrap_or(after);
                let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
                let epoch = row.get("epoch").and_then(Value::as_u64).unwrap_or(0);
                let Some(data) = row.get("data").and_then(Value::as_str).and_then(unb64) else {
                    self.advance(conversation_id, seq);
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
                self.advance(conversation_id, seq);
            }
            if count < HANDSHAKE_PAGE {
                return Ok(moved);
            }
        }
    }

    fn advance(&mut self, conversation_id: &str, seq: i64) {
        let at = self
            .notes
            .seqs
            .entry(conversation_id.to_owned())
            .or_insert(0);
        if seq > *at {
            *at = seq;
            self.save_notes();
        }
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
        let body = json!({ "device_id": device_id, "epoch": bundle.epoch, "commit": b64(&bundle.commit), "welcomes": welcomes });
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

    fn remember(&mut self, conversation_id: &str, id: &str, opened: Opened) {
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
        let mut synced = false;
        let mut ordered: Vec<&Row> = rows.iter().collect();
        ordered.sort_by_key(|row| row.epoch);
        for row in ordered {
            let read = self.read_one(conversation_id, conv, row, &mut synced);
            out.insert(row.id.to_owned(), read);
        }
        // An edit counts once its target is known, and only from the target's author.
        let edits: Vec<(String, String, Opened, String)> = rows
            .iter()
            .filter(|row| row.kind == "edit")
            .filter_map(|row| {
                let opened = self.cache(conversation_id)?.rows.get(row.id)?.clone();
                Some((
                    row.id.to_owned(),
                    opened.edits.clone()?,
                    opened,
                    row.author_id.to_owned(),
                ))
            })
            .collect();
        for (edit_id, target, edit, author) in edits {
            let target_author = rows
                .iter()
                .find(|row| row.id == target)
                .map(|row| row.author_id.to_owned());
            let Some(mut opened) = self
                .cache(conversation_id)
                .and_then(|cache| cache.rows.get(&target).cloned())
            else {
                continue;
            };
            if target_author
                .as_deref()
                .is_some_and(|target_author| target_author != author)
            {
                continue;
            }
            if opened
                .edited_by
                .as_deref()
                .is_some_and(|by| by >= edit_id.as_str())
            {
                continue;
            }
            opened.text = edit.text;
            opened.attachments = edit.attachments;
            opened.edited_by = Some(edit_id);
            out.insert(target.clone(), Read::Opened(opened.clone()));
            self.remember(conversation_id, &target, opened);
        }
        let _ = self.engine.as_mut().map(Engine::flush);
        out
    }

    fn read_one(
        &mut self,
        conversation_id: &str,
        conv: ConversationId,
        row: &Row,
        synced: &mut bool,
    ) -> Read {
        if let Some(opened) = self
            .cache(conversation_id)
            .and_then(|cache| cache.rows.get(row.id).cloned())
        {
            return if opened.edits.is_some() {
                Read::Edit
            } else {
                Read::Opened(opened)
            };
        }
        // Our own message, sent from this device: kept when it was sent, by its client id.
        if let Some(client_id) = row.client_id {
            if let Some(opened) = self
                .cache(conversation_id)
                .and_then(|cache| cache.pending.remove(client_id))
            {
                let edit = opened.edits.is_some();
                self.remember(conversation_id, row.id, opened.clone());
                return if edit {
                    Read::Edit
                } else {
                    Read::Opened(opened)
                };
            }
        }
        let Some(ciphertext) = row.ciphertext.and_then(unb64) else {
            return Read::Undecryptable;
        };
        let kind = if row.kind == "edit" {
            Kind::Edit
        } else {
            Kind::Message
        };
        let claimed = Claimed {
            author: UserId::parse(row.author_id).ok(),
            device: DeviceId::parse(row.device_id).ok(),
            kind,
            target_id: row.target_id,
        };
        let Some(engine) = self.engine.as_mut() else {
            return Read::Undecryptable;
        };
        let mut opened = engine.decrypt(conv, &ciphertext, &claimed);
        if matches!(opened, Err(Error::FutureEpoch { .. } | Error::NoGroup)) && !*synced {
            *synced = true;
            if let Err(why) = self.sync(conversation_id) {
                tracing::warn!("chatd: Tiny e2ee: {conversation_id}: {why}");
            }
            let Some(engine) = self.engine.as_mut() else {
                return Read::Undecryptable;
            };
            opened = engine.decrypt(conv, &ciphertext, &claimed);
        }
        match opened {
            Ok(incoming) => {
                let opened = Opened {
                    text: incoming.envelope.text.clone(),
                    reply_to: incoming.envelope.reply_to.clone(),
                    attachments: incoming
                        .envelope
                        .attachments
                        .iter()
                        .map(OpenedFile::of)
                        .collect(),
                    edits: incoming.target_id.clone().filter(|_| kind == Kind::Edit),
                    edited_by: None,
                };
                self.remember(conversation_id, row.id, opened.clone());
                if kind == Kind::Edit {
                    Read::Edit
                } else {
                    Read::Opened(opened)
                }
            }
            Err(err) => {
                tracing::debug!("chatd: Tiny e2ee: a message did not open: {err}");
                if kind == Kind::Edit {
                    Read::Edit
                } else {
                    Read::Undecryptable
                }
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
        let conv = ConversationId(uuid(conversation_id).ok_or(super::CHECK)?);
        // Removals and joins first: a message is never sent to someone already leaving.
        self.sync(conversation_id)?;
        let sealed = match self.engine()?.encrypt(conv, kind, target_id, envelope) {
            Ok(sealed) => sealed,
            Err(Error::PendingProposals | Error::PendingRemove | Error::CommitInFlight) => {
                self.sync(conversation_id)?;
                self.engine()?
                    .encrypt(conv, kind, target_id, envelope)
                    .map_err(|err| words(&err))?
            }
            Err(err) => return Err(words(&err)),
        };
        let opened = Opened {
            text: envelope.text.clone(),
            reply_to: envelope.reply_to.clone(),
            attachments: envelope.attachments.iter().map(OpenedFile::of).collect(),
            edits: target_id.map(str::to_owned),
            edited_by: None,
        };
        if let Some(cache) = self.cache(conversation_id) {
            cache.pending.insert(client_id.to_owned(), opened.clone());
        }
        let device_id = self.engine()?.device_id().to_string();
        let mut body = json!({
            "ciphertext": b64(&sealed.ciphertext),
            "epoch": sealed.epoch,
            "device_id": device_id,
            "client_id": client_id,
            "kind": kind.as_str(),
        });
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
                Ok(answer)
            }
            status => Err(super::explain(status, &answer, super::Missing::Plain).to_owned()),
        }
    }

    /// Opening details for a sealed attachment this device has seen in a message.
    pub(crate) fn sealed_file(&self, id: &str) -> Option<crate::adapters::SealedFile> {
        let (conversation_id, file) = self.files.get(id)?;
        let mut sealed = file.sealed()?;
        sealed.context = attachment_context(conversation_id)?;
        Some(sealed)
    }

    /// The live socket said something about this protocol.
    pub(crate) fn heard(&mut self, kind: &str, frame: &Value) {
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
                    if let Err(why) = self.sync(&conversation_id) {
                        tracing::warn!("chatd: Tiny e2ee: {conversation_id}: {why}");
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
                if user == self.saved.user_id && !self.ready() {
                    // Approved from another device, maybe.
                    self.enrol();
                } else if self.notes.met.contains(&user) || user == self.saved.user_id {
                    if let Err(why) = self.verify_user(&user) {
                        tracing::warn!("chatd: Tiny e2ee: {why}");
                    }
                }
            }
            _ => {}
        }
    }

    /// `E2eeAsk::Status`.
    pub(crate) fn status(&mut self, names: &HashMap<String, String>) -> E2eeStatus {
        let mut status = E2eeStatus {
            mode: self.mode.clone(),
            device: self.state.clone(),
            recovery_set: self.notes.recovery.is_some(),
            backup: false,
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
        }
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
                .insert(user_id.to_owned(), person.safety);
        } else {
            self.notes.verified.remove(user_id);
        }
        self.save_notes();
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
    pub(crate) fn approve(&mut self, code: &str) -> Result<(), String> {
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
                let mine = self
                    .engine
                    .as_ref()
                    .map(|engine| engine.device_id().to_string());
                if mine.as_deref() == Some(device_id) {
                    self.state = DeviceState::Failed {
                        why: "This device was revoked. Sign in again to set it up as a new device."
                            .to_owned(),
                    };
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

/// Serves `Request::TinyE2ee`.
pub(crate) fn answer(
    saved: &Saved,
    ask: protocol::E2eeAsk,
    names: &HashMap<String, String>,
) -> protocol::Response {
    use protocol::{E2eeAsk, ErrorCode, Response};
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
        E2eeAsk::Approve { code } => done(device.approve(&code)),
        E2eeAsk::Revoke { device_id } => done(device.revoke(&device_id)),
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
    }
}

/// The associated data a sealed attachment's first chunk carries besides its header: the 16 bytes
/// of the conversation's id, which the sender knows before it uploads (the attachment's own id is
/// the server's and comes after; see `docs/e2ee-core-notes.md` 1.1). A blob moved into another
/// conversation does not open there.
pub(crate) fn attachment_context(conversation_id: &str) -> Option<Vec<u8>> {
    uuid(conversation_id).map(|id| id.to_vec())
}

/// A new attachment key, for a file about to be sealed.
pub(crate) fn attachment_key() -> Result<AttachmentKey, String> {
    AttachmentKey::generate().map_err(|_| "No randomness is available.".to_owned())
}

#[cfg(test)]
mod live;

#[cfg(test)]
mod tests {
    use super::*;

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
            e2ee: None,
        };
        let secrets = Secrets::generate().unwrap();
        let one = r#"{"base_url":"https://a","token":"t","user_id":"u1","display_name":"A","role":"owner"}"#;
        let written = with_secrets(one, &saved("https://a", "u1"), &secrets).unwrap();
        let back = super::super::parse(&written).unwrap();
        assert!(back.e2ee.as_ref().and_then(Secrets::keys).is_some());
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
        write_private(&dir.join("x"), b"y").unwrap();
        assert_eq!(std::fs::read(dir.join("x")).unwrap(), b"y");
        let _ = std::fs::remove_dir_all(dir);
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
