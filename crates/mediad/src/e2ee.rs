//! End-to-end encryption for a Tiny server call: an MLS group of the people in the room, kept
//! with `davey` as a Discord call's DAVE is, and the frame transform between our encoder and
//! str0m, and between str0m and each slot's decoder. tinyserver's `docs/calls.md` ("End-to-end
//! encryption") is the contract.
//!
//! The server is the group's external sender: it proposes adding whoever joins and removing
//! whoever leaves, and orders the commits, but it never holds a key that opens a frame. It hands
//! us its `call.e2ee` frames through chatd ([`E2ee::frame`]); what we answer goes back the same
//! way ([`E2ee::take_outbox`]).
//!
//! Each Opus frame is sealed with our sender key from the group's current epoch (AES-128-GCM
//! with an 8-byte tag, as DAVE does), so the RTP header and its RFC 6464 audio level stay in
//! clear for the SFU to forward and to tell who speaks. An H.264 picture uses the same group
//! and leaves the one-byte NAL header clear, so the SFU can still see a keyframe. A new epoch,
//! on every join and leave,
//! brings new keys: who left cannot open what is said after, and who joined cannot open what
//! was said before.
//!
//! Never claim more than holds: until the group has keys our microphone is held back rather
//! than sent in clear, a frame that arrives in clear while the call is end-to-end encrypted is
//! dropped rather than played, and a call the server says is transport-only says so.

use crate::rtp::SILENCE;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use davey::{Codec, DaveSession, MediaType, ProposalsOperationType, SigningKeyPair};
use protocol::{CallSecurity, SecureMember, SecurityMode, TinyE2ee};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::num::NonZeroU16;
use std::path::Path;
use std::time::{Duration, Instant};

/// The highest version of the server's `call.e2ee` this build speaks: DAVE's MLS, version 1.
pub const VERSION: u16 = 1;
/// Every sealed frame ends with this, as in DAVE.
const MARKER: [u8; 2] = [0xfa, 0xfa];
/// Frames in a row from people in the group that may fail to open before our group is taken to
/// be broken and rebuilt: three seconds of one voice.
const TOLERANCE: u32 = 150;
/// At most one rebuild of our own this often, whatever the frames say.
const RECOVER_EVERY: Duration = Duration::from_secs(10);
/// The most one `call.e2ee` payload may be, decoded, either way.
const MAX_PAYLOAD: usize = 32 * 1024;
/// Members whose safety number is kept, by their key: a room holds 12 at most.
const SAFETY_KEPT: usize = 64;

fn hashed(prefix: &[u8], id: &str) -> u64 {
    let digest = Sha256::new().chain_update(prefix).chain_update(id.as_bytes()).finalize();
    u64::from_be_bytes(digest[..8].try_into().expect("a SHA-256 digest has 8 bytes"))
}

/// The MLS identity of a user on the server: their credential holds these 8 bytes.
pub fn user_id(user: &str) -> u64 {
    hashed(b"tiny-e2ee-v1 user ", user)
}

/// The MLS group of a room's call.
pub fn group_id(room: &str) -> u64 {
    hashed(b"tiny-e2ee-v1 room ", room)
}

/// This account's identity key: made on first use and kept in `path`, so a person's key, and
/// with it everyone's safety number with them, stays the same from call to call. A file that
/// cannot be written gives a key for this call alone, and says so.
///
/// A file that exists but cannot be read is not overwritten. It is moved aside to
/// `<name>.unreadable`, so a key that was only damaged is not destroyed with its replacement,
/// and a new key is made. Everyone who verified this person then sees a changed key, which is
/// correct: it is a different key.
pub fn identity(path: &Path) -> (SigningKeyPair, bool) {
    if let Ok(text) = std::fs::read_to_string(path) {
        let mut lines = text.lines().map(str::trim);
        if let (Some(Ok(private)), Some(Ok(public))) = (lines.next().map(hex_decode), lines.next().map(hex_decode)) {
            if private.len() == 32 && public.len() == 65 {
                // A key an older build left readable by others is narrowed now it is used.
                protocol::private::tighten_file(path);
                if let Some(dir) = path.parent() {
                    protocol::private::tighten_dir(dir);
                }
                return (SigningKeyPair { private, public }, true);
            }
        }
        let aside = path.with_extension("key.unreadable");
        let moved = std::fs::rename(path, &aside).is_ok();
        tracing::warn!(
            moved,
            "mediad: e2ee: the identity key file could not be read; a new key is made and people who verified this account will see it change"
        );
    }
    let made = SigningKeyPair::generate();
    let kept = write_private(path, &format!("{}\n{}\n", hex(&made.private), hex(&made.public))).is_ok();
    if !kept {
        tracing::warn!("mediad: e2ee: the identity key could not be kept; this call uses one of its own");
    }
    (made, kept)
}

/// Writes `text` to `path` so that a reader sees the old file or the whole new one and never
/// half of it: a file of its own next to it, flushed to disk, then renamed over it. Only the
/// owner can read it where the platform says so (mode 0600, directory 0700 on Unix). On Windows
/// the file inherits the folder's access list, which for a person's profile is that person.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _ = std::fs::remove_file(&temp);
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    match written.and_then(|()| std::fs::rename(&temp, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Result<Vec<u8>, ()> {
    if !text.len().is_multiple_of(2) {
        return Err(());
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2).ok_or(())?, 16).map_err(|_| ())).collect()
}

fn decode(frame: &Value, field: &str) -> Option<Vec<u8>> {
    let bytes = STANDARD.decode(frame.get(field)?.as_str()?).ok()?;
    (bytes.len() <= MAX_PAYLOAD).then_some(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Encrypted to the server only: it has no end-to-end encryption, or someone in the room
    /// cannot do it.
    Transport,
    /// Offered, and the server has not said yet which this call is.
    Waiting,
    /// End-to-end: the group is being made, or has keys.
    Group,
    /// The person asked to join only end-to-end encrypted calls, and the server turned it off. We
    /// send nothing and play nothing until it says `start` again. Not `Transport`: that would
    /// carry on unencrypted, which is what the setting exists to refuse.
    Blocked,
}

pub struct E2ee {
    me: String,
    room: String,
    mode: Mode,
    /// The server has `e2ee_calls`.
    offered: bool,
    /// The person only joins end-to-end encrypted calls: a downgrade blocks the call.
    required: bool,
    identity: Option<SigningKeyPair>,
    session: Option<DaveSession>,
    /// Everyone in the room's call, us included, as chatd last said.
    roster: Vec<String>,
    /// Who in the room cannot encrypt end to end, as the server said when it turned it off.
    unsupported: Vec<String>,
    failures: u32,
    recovered: Option<Instant>,
    /// Safety numbers made so far, by the member's MLS id and key.
    safety: HashMap<(u64, Vec<u8>), String>,
    /// `call.e2ee` frames for the server.
    outbox: Vec<String>,
    told: Option<CallSecurity>,
}

impl Default for E2ee {
    fn default() -> E2ee {
        E2ee {
            me: String::new(),
            room: String::new(),
            mode: Mode::Transport,
            offered: false,
            required: false,
            identity: None,
            session: None,
            roster: Vec::new(),
            unsupported: Vec::new(),
            failures: 0,
            recovered: None,
            safety: HashMap::new(),
            outbox: Vec::new(),
            told: None,
        }
    }
}

impl E2ee {
    /// A fresh offer: a join, a rejoin or a move. Whatever group there was went with the old
    /// seat; the server says after its answer what this one is.
    pub fn offer(&mut self, me: &str, e2ee: Option<&TinyE2ee>) {
        self.me = me.to_owned();
        self.session = None;
        self.unsupported.clear();
        self.failures = 0;
        match e2ee {
            Some(e2ee) => {
                if self.identity.is_none() {
                    self.identity = Some(identity(Path::new(&e2ee.identity)).0);
                }
                self.room = e2ee.room_id.clone();
                self.offered = true;
                self.required = e2ee.required;
                self.mode = Mode::Waiting;
            }
            None => {
                self.offered = false;
                self.required = false;
                self.mode = Mode::Transport;
            }
        }
    }

    /// Takes an identity key directly, for tests.
    pub fn with_identity(&mut self, key: SigningKeyPair) {
        self.identity = Some(key);
    }

    /// Everyone in the room's call now.
    pub fn roster(&mut self, user_ids: Vec<String>) {
        self.roster = user_ids;
    }

    fn send(&mut self, op: &str, mut fields: Value) {
        fields["type"] = json!("call.e2ee");
        fields["op"] = json!(op);
        self.outbox.push(fields.to_string());
    }

    /// The `call.e2ee` frames to send, in order.
    pub fn take_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }

    /// One `call.e2ee` frame from the server.
    pub fn frame(&mut self, text: &str) {
        let Ok(frame) = serde_json::from_str::<Value>(text) else {
            return;
        };
        if !self.offered {
            // We never offered: nothing the server says makes this call end to end.
            return;
        }
        let epoch = frame.get("epoch").and_then(Value::as_u64).unwrap_or(0);
        match frame.get("op").and_then(Value::as_str).unwrap_or("") {
            "start" => {
                let version = frame.get("version").and_then(Value::as_u64).unwrap_or(0);
                match decode(&frame, "external_sender") {
                    Some(sender) if version == u64::from(VERSION) => self.start(&sender),
                    _ => tracing::warn!("mediad: e2ee: a start this build cannot use (version {version})"),
                }
            }
            "off" => {
                // The server turning a call to transport only is the one thing a person who
                // requires end-to-end encryption said they would not accept.
                self.mode = if self.required { Mode::Blocked } else { Mode::Transport };
                self.session = None;
                let ids = frame.get("user_ids").and_then(Value::as_array).into_iter().flatten();
                self.unsupported = ids.filter_map(Value::as_str).take(64).map(str::to_owned).collect();
            }
            "proposals" => {
                if let Some(payload) = decode(&frame, "data") {
                    self.proposals(&payload, epoch);
                }
            }
            "commit" => {
                if let Some(commit) = decode(&frame, "data") {
                    let applied = self.session.as_mut().map(|session| session.process_commit(&commit).map_err(|err| err.to_string()));
                    self.applied(applied, "commit");
                }
            }
            "welcome" => {
                if let Some(welcome) = decode(&frame, "data") {
                    let applied = self.session.as_mut().map(|session| session.process_welcome(&welcome).map_err(|err| err.to_string()));
                    self.applied(applied, "welcome");
                }
            }
            _ => {}
        }
    }

    /// A fresh group state with the server as external sender, and our key package for it.
    fn start(&mut self, sender: &[u8]) {
        self.mode = Mode::Group;
        self.unsupported.clear();
        self.failures = 0;
        let (Some(identity), Some(version)) = (&self.identity, NonZeroU16::new(VERSION)) else {
            return;
        };
        let made = DaveSession::new(version, user_id(&self.me), group_id(&self.room), Some(identity)).map_err(|err| err.to_string());
        let mut session = match made {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!("mediad: e2ee: no session: {err}");
                return;
            }
        };
        if let Err(err) = session.set_external_sender(sender) {
            tracing::warn!("mediad: e2ee: the server's external sender was refused: {err}");
            return;
        }
        match session.create_key_package() {
            Ok(package) => {
                self.session = Some(session);
                self.send("key_package", json!({ "data": STANDARD.encode(package) }));
            }
            Err(err) => tracing::warn!("mediad: e2ee: no key package: {err}"),
        }
    }

    /// Members to add or remove: commit them, unless the server proposes someone who is not
    /// in the call.
    fn proposals(&mut self, payload: &[u8], epoch: u64) {
        let expected: Vec<u64> = self.roster.iter().map(|user| user_id(user)).collect();
        let (Some(session), Some((&kind, proposals))) = (&mut self.session, payload.split_first()) else {
            return;
        };
        let kind = match kind {
            0 => ProposalsOperationType::APPEND,
            1 => ProposalsOperationType::REVOKE,
            _ => return,
        };
        match session.process_proposals(kind, proposals, Some(&expected)) {
            Ok(Some(made)) => {
                let mut fields = json!({ "epoch": epoch, "data": STANDARD.encode(&made.commit) });
                if let Some(welcome) = made.welcome {
                    fields["welcome"] = json!(STANDARD.encode(welcome));
                }
                self.send("commit", fields);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("mediad: e2ee: proposals refused: {err}"),
        }
    }

    fn applied(&mut self, applied: Option<Result<(), String>>, what: &str) {
        match applied {
            Some(Ok(())) => self.failures = 0,
            Some(Err(err)) => {
                tracing::warn!("mediad: e2ee: the {what} could not be applied: {err}");
                self.recover();
            }
            None => {}
        }
    }

    /// Our group cannot follow: tell the server, and come back in with a fresh key package.
    fn recover(&mut self) {
        let Some(sender) = self.session.as_ref().and_then(|_| self.external_sender()) else {
            return;
        };
        self.recovered = Some(Instant::now());
        self.failures = 0;
        self.send("invalid", json!({}));
        self.start(&sender);
    }

    fn external_sender(&self) -> Option<Vec<u8>> {
        use openmls::prelude::tls_codec::Serialize;
        let group = self.session.as_ref()?.group()?;
        let senders = group.extensions().external_senders()?;
        senders.first()?.tls_serialize_detached().ok()
    }

    fn ready(&self) -> bool {
        self.mode == Mode::Group && self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    /// Our Opus frame as it goes into RTP. `None` holds it back: the call is to be end to end
    /// and has no keys yet. Silence is never sealed, as in DAVE.
    pub fn seal(&mut self, opus: &[u8]) -> Option<Vec<u8>> {
        match self.mode {
            Mode::Transport => Some(opus.to_vec()),
            Mode::Waiting | Mode::Blocked => None,
            Mode::Group if opus == SILENCE => Some(opus.to_vec()),
            Mode::Group => {
                if !self.ready() {
                    return None;
                }
                match self.session.as_mut()?.encrypt_opus(opus) {
                    Ok(sealed) => Some(sealed.into_owned()),
                    Err(err) => {
                        tracing::warn!("mediad: e2ee: a frame was not sealed, and is dropped: {err}");
                        None
                    }
                }
            }
        }
    }

    /// One H.264 picture, Annex B, as it goes into RTP. The NAL header stays clear. `None`
    /// holds it back while the call is to be end to end and has no keys yet.
    pub fn seal_video(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        match self.mode {
            Mode::Transport => Some(frame.to_vec()),
            Mode::Waiting | Mode::Blocked => None,
            Mode::Group => {
                if !self.ready() {
                    return None;
                }
                match self.session.as_mut()?.encrypt(MediaType::VIDEO, Codec::H264, frame) {
                    Ok(sealed) => Some(sealed.into_owned()),
                    Err(err) => {
                        tracing::warn!("mediad: e2ee: a picture was not sealed, and is dropped: {err}");
                        None
                    }
                }
            }
        }
    }

    /// `user`'s picture as the decoder wants it, or `None` to drop it. The same rules as
    /// [`E2ee::open`]: clear media is dropped in an end-to-end call, and a sealed frame is
    /// dropped in a transport-only call.
    pub fn open_video(&mut self, user: &str, frame: &[u8]) -> Option<Vec<u8>> {
        let sealed = frame.ends_with(&MARKER);
        match self.mode {
            Mode::Transport => return (!sealed).then(|| frame.to_vec()),
            Mode::Waiting | Mode::Blocked => return None,
            Mode::Group => {}
        }
        if !sealed || user.is_empty() || !self.ready() {
            return None;
        }
        match self.session.as_mut()?.decrypt(user_id(user), MediaType::VIDEO, frame) {
            Ok(picture) => {
                self.failures = 0;
                Some(picture)
            }
            Err(_) => {
                self.failures += 1;
                let calm = self.recovered.is_none_or(|at| at.elapsed() > RECOVER_EVERY);
                if self.failures > TOLERANCE && calm {
                    tracing::warn!("mediad: e2ee: pictures stopped opening; rebuilding our group");
                    self.recover();
                }
                None
            }
        }
    }

    /// `user`'s frame as the decoder wants it, or `None` to drop it: in an end-to-end call, one
    /// that does not open with their key; in a transport-only call, one sealed for a group we
    /// are no longer in.
    pub fn open(&mut self, user: &str, frame: &[u8]) -> Option<Vec<u8>> {
        if frame == SILENCE {
            return Some(frame.to_vec());
        }
        let sealed = frame.ends_with(&MARKER);
        match self.mode {
            Mode::Transport => return (!sealed).then(|| frame.to_vec()),
            Mode::Waiting | Mode::Blocked => return None,
            Mode::Group => {}
        }
        // Clear audio in an end-to-end call is the server speaking, or someone who is not in
        // the group: never played.
        if !sealed || user.is_empty() || !self.ready() {
            return None;
        }
        let opened = self.session.as_mut()?.decrypt(user_id(user), MediaType::AUDIO, frame);
        match opened {
            Ok(opus) => {
                self.failures = 0;
                Some(opus)
            }
            Err(_) => {
                self.failures += 1;
                let calm = self.recovered.is_none_or(|at| at.elapsed() > RECOVER_EVERY);
                if self.failures > TOLERANCE && calm {
                    tracing::warn!("mediad: e2ee: frames stopped opening; rebuilding our group");
                    self.recover();
                }
                None
            }
        }
    }

    /// Members whose safety number is not made yet: their MLS id and key, with ours. Making one
    /// takes a while (scrypt), so it is done outside whatever lock holds this.
    pub fn safety_wanted(&self) -> Vec<(u64, Vec<u8>, u64, Vec<u8>)> {
        let (Some(identity), Some(group)) = (&self.identity, self.session.as_ref().filter(|s| s.is_ready()).and_then(DaveSession::group)) else {
            return Vec::new();
        };
        let own = group.own_leaf_index();
        let me = user_id(&self.me);
        group
            .members()
            .filter(|member| member.index != own)
            .filter_map(|member| Some((u64::from_be_bytes(member.credential.serialized_content().try_into().ok()?), member.signature_key)))
            .filter(|(id, key)| !self.safety.contains_key(&(*id, key.clone())))
            .map(|(id, key)| (id, key, me, identity.public.clone()))
            .collect()
    }

    pub fn add_safety(&mut self, id: u64, key: Vec<u8>, number: String) {
        if self.safety.len() >= SAFETY_KEPT {
            self.safety.clear();
        }
        self.safety.insert((id, key), number);
    }

    /// How the call is protected now.
    pub fn security(&self) -> CallSecurity {
        let mut security = CallSecurity { unsupported: self.unsupported.clone(), server_lacks: !self.offered, ..CallSecurity::default() };
        security.mode = match self.mode {
            Mode::Transport => SecurityMode::Transport,
            Mode::Waiting => SecurityMode::Pending,
            Mode::Blocked => SecurityMode::Blocked,
            Mode::Group if self.ready() => SecurityMode::EndToEnd,
            Mode::Group => SecurityMode::Pending,
        };
        if security.mode != SecurityMode::EndToEnd {
            return security;
        }
        let Some(session) = &self.session else {
            return security;
        };
        security.code = session.voice_privacy_code().unwrap_or_default().to_owned();
        security.epoch = session.epoch().map_or(0, |epoch| epoch.as_u64());
        let names: HashMap<u64, &str> = self.roster.iter().map(|user| (user_id(user), user.as_str())).collect();
        if let Some(group) = session.group() {
            let own = group.own_leaf_index();
            for member in group.members().filter(|member| member.index != own) {
                let Ok(bytes) = <[u8; 8]>::try_from(member.credential.serialized_content()) else {
                    continue;
                };
                let id = u64::from_be_bytes(bytes);
                security.members.push(SecureMember {
                    // Someone the server does not list in the call is shown as nobody we know.
                    user_id: names.get(&id).map(|name| (*name).to_owned()).unwrap_or_default(),
                    key: hex(&member.signature_key),
                    safety: self.safety.get(&(id, member.signature_key.clone())).cloned().unwrap_or_default(),
                    trust: Default::default(),
                });
            }
        }
        security
    }

    /// The protection, when it changed since it was last taken.
    pub fn changed(&mut self) -> Option<CallSecurity> {
        let now = self.security();
        if self.told.as_ref() == Some(&now) {
            return None;
        }
        self.told = Some(now.clone());
        Some(now)
    }
}

/// The safety number of a pair of keys: 45 digits, the same on both sides (DAVE's pairwise
/// verification code).
pub fn safety_number(id: u64, key: &[u8], me: u64, mine: &[u8]) -> String {
    davey::generate_pairwise_fingerprint(0, mine, me, key, id)
        .ok()
        .and_then(|fingerprint| davey::generate_displayable_code(&fingerprint, 45, 5).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

/// The encryption of the Tiny call these controls run, from chatd's thread and the call's loop.
impl crate::session::Controls {
    /// Runs `with` on the call's encryption, then sends what it has for the server.
    pub fn e2ee<R>(&self, with: impl FnOnce(&mut E2ee) -> R) -> R {
        let (answer, outbox) = {
            let mut e2ee = self.tiny_e2ee.lock().unwrap();
            let answer = with(&mut e2ee);
            (answer, e2ee.take_outbox())
        };
        for frame in outbox {
            crate::out::send(&protocol::MediaResponse::TinyE2ee { frame });
        }
        answer
    }

    /// Makes the safety numbers the group's new members need, outside the lock the call's loop
    /// takes for every frame, then tells chatd how the call is protected if that changed.
    pub fn e2ee_report(&self) {
        let wanted = self.e2ee(|e2ee| e2ee.safety_wanted());
        let made: Vec<(u64, Vec<u8>, String)> = wanted.into_iter().map(|(id, key, me, mine)| {
            let number = safety_number(id, &key, me, &mine);
            (id, key, number)
        }).collect();
        let changed = self.e2ee(|e2ee| {
            for (id, key, number) in made {
                e2ee.add_safety(id, key, number);
            }
            e2ee.changed()
        });
        if let Some(security) = changed {
            crate::out::send(&protocol::MediaResponse::TinySecurity(security));
        }
    }
}
