//! Discord's end-to-end encryption (DAVE) for one voice connection: the MLS group through
//! `davey`, and the transitions the voice gateway steers it through.
//!
//! The gateway hands each DAVE opcode's payload to a method here and sends whatever comes back.
//! The media loop calls `encrypt` and `decrypt` on every Opus frame. Protocol version 0 means the
//! call is not end-to-end encrypted and frames pass through untouched.

use crate::rtp::SILENCE;
use davey::{DaveSession, MediaType, ProposalsOperationType};
use serde_json::{json, Value};
use std::num::NonZeroU16;

/// The highest version this build speaks.
pub const MAX_VERSION: u16 = davey::DAVE_PROTOCOL_VERSION;

pub const PREPARE_TRANSITION: u8 = 21;
pub const EXECUTE_TRANSITION: u8 = 22;
pub const TRANSITION_READY: u8 = 23;
pub const PREPARE_EPOCH: u8 = 24;
pub const EXTERNAL_SENDER: u8 = 25;
pub const KEY_PACKAGE: u8 = 26;
pub const PROPOSALS: u8 = 27;
pub const COMMIT_WELCOME: u8 = 28;
pub const ANNOUNCE_COMMIT: u8 = 29;
pub const WELCOME: u8 = 30;
pub const INVALID_COMMIT_WELCOME: u8 = 31;

/// Seconds old keys, or unencrypted frames, are still accepted after a transition.
const EXPIRY: u32 = 10;
/// The same while a downgrade is announced and not yet executed.
const EXPIRY_DOWNGRADE: u32 = 24;
/// Frames in a row that may fail to decrypt before the group is taken to be broken.
const TOLERANCE: u32 = 36;
/// Every end-to-end encrypted frame ends with this.
const MARKER: [u8; 2] = [0xfa, 0xfa];

/// What the gateway must send for us.
#[derive(Debug, PartialEq)]
pub enum Outgoing {
    Json { op: u8, d: Value },
    Binary { op: u8, payload: Vec<u8> },
}

pub struct Dave {
    user_id: u64,
    channel_id: u64,
    /// The protocol version in effect. 0 is none.
    version: u16,
    session: Option<DaveSession>,
    /// The external sender, if it arrived before the session it belongs to.
    early_sender: Option<Vec<u8>>,
    /// A transition announced and not yet executed: its id and the version it leads to.
    pending: Option<(u16, u16)>,
    downgraded: bool,
    reinitializing: bool,
    last_transition: Option<u16>,
    failures: u32,
}

fn ready(transition: u16) -> Outgoing {
    Outgoing::Json { op: TRANSITION_READY, d: json!({ "transition_id": transition }) }
}

impl Dave {
    pub fn new(user_id: u64, channel_id: u64) -> Dave {
        Dave {
            user_id,
            channel_id,
            version: 0,
            session: None,
            early_sender: None,
            pending: None,
            downgraded: false,
            reinitializing: false,
            last_transition: None,
            failures: 0,
        }
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    /// True once the group has keys and our frames go out encrypted.
    pub fn encrypting(&self) -> bool {
        self.version != 0 && self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    /// The five-group code both ends can compare, once the group has an epoch.
    pub fn privacy_code(&self) -> Option<&str> {
        self.session.as_ref().and_then(DaveSession::voice_privacy_code)
    }

    /// The voice server picked `version` in its session description.
    pub fn start(&mut self, version: u16) -> Vec<Outgoing> {
        self.version = version;
        self.reinit()
    }

    /// A fresh group state and key package for the version in effect.
    fn reinit(&mut self) -> Vec<Outgoing> {
        let Some(version) = NonZeroU16::new(self.version) else {
            if let Some(session) = &mut self.session {
                let _ = session.reset();
                session.set_passthrough_mode(true, Some(EXPIRY));
            }
            return Vec::new();
        };
        let made = match &mut self.session {
            Some(session) => session.reinit(version, self.user_id, self.channel_id, None).map_err(|e| e.to_string()),
            None => DaveSession::new(version, self.user_id, self.channel_id, None)
                .map(|session| self.session = Some(session))
                .map_err(|e| e.to_string()),
        };
        if let Err(err) = made {
            eprintln!("mediad: dave: could not start a session at version {version}: {err}");
            return Vec::new();
        }
        if let Some(sender) = self.early_sender.take() {
            self.external_sender(&sender);
        }
        match self.session.as_mut().unwrap().create_key_package() {
            Ok(payload) => vec![Outgoing::Binary { op: KEY_PACKAGE, payload }],
            Err(err) => {
                eprintln!("mediad: dave: no key package: {err}");
                Vec::new()
            }
        }
    }

    /// Opcode 25: who may propose changes to the group (the voice server).
    pub fn external_sender(&mut self, payload: &[u8]) {
        let Some(session) = &mut self.session else {
            self.early_sender = Some(payload.to_vec());
            return;
        };
        if let Err(err) = session.set_external_sender(payload) {
            eprintln!("mediad: dave: external sender refused: {err}");
        }
    }

    /// Opcode 21: a change of protocol version is coming. Version 0 is a downgrade to none.
    pub fn prepare_transition(&mut self, transition: u16, version: u16) -> Vec<Outgoing> {
        self.pending = Some((transition, version));
        if transition == 0 {
            self.execute_transition(0);
            return Vec::new();
        }
        if version == 0 {
            if let Some(session) = &mut self.session {
                session.set_passthrough_mode(true, Some(EXPIRY_DOWNGRADE));
            }
        }
        vec![ready(transition)]
    }

    /// Opcode 22: the announced transition takes effect now. True if it did.
    pub fn execute_transition(&mut self, transition: u16) -> bool {
        let Some((pending, version)) = self.pending else {
            return false;
        };
        if pending != transition {
            return false;
        }
        let before = std::mem::replace(&mut self.version, version);
        if before != version && version == 0 {
            self.downgraded = true;
        } else if transition > 0 && self.downgraded {
            self.downgraded = false;
            if let Some(session) = &mut self.session {
                session.set_passthrough_mode(true, Some(EXPIRY));
            }
        }
        self.reinitializing = false;
        self.last_transition = Some(transition);
        self.pending = None;
        true
    }

    /// Opcode 24: a new epoch is coming. Epoch 1 means the group is being made anew.
    pub fn prepare_epoch(&mut self, epoch: u64, version: u16) -> Vec<Outgoing> {
        if epoch != 1 {
            return Vec::new();
        }
        self.version = version;
        self.reinit()
    }

    /// The group could not follow: say so, and offer a new key package.
    fn recover(&mut self, transition: u16) -> Vec<Outgoing> {
        if self.reinitializing {
            return Vec::new();
        }
        self.reinitializing = true;
        self.failures = 0;
        let mut out = vec![Outgoing::Json {
            op: INVALID_COMMIT_WELCOME,
            d: json!({ "transition_id": transition }),
        }];
        out.extend(self.reinit());
        out
    }

    /// Opcode 27: members to add or remove. `connected` is everyone the gateway says is in the
    /// call; a proposal to add anyone else is refused. May answer with a commit (opcode 28).
    pub fn proposals(&mut self, payload: &[u8], connected: &[u64]) -> Vec<Outgoing> {
        let (Some(session), Some((&kind, proposals))) = (&mut self.session, payload.split_first()) else {
            return Vec::new();
        };
        let kind = match kind {
            0 => ProposalsOperationType::APPEND,
            1 => ProposalsOperationType::REVOKE,
            other => {
                eprintln!("mediad: dave: unknown proposals operation {other}");
                return Vec::new();
            }
        };
        match session.process_proposals(kind, proposals, Some(connected)) {
            Ok(Some(made)) => {
                let mut payload = made.commit;
                payload.extend(made.welcome.unwrap_or_default());
                vec![Outgoing::Binary { op: COMMIT_WELCOME, payload }]
            }
            Ok(None) => Vec::new(),
            Err(err) => {
                eprintln!("mediad: dave: proposals refused: {err}");
                Vec::new()
            }
        }
    }

    /// The group moved to a new epoch under `transition`.
    fn joined_epoch(&mut self, transition: u16) -> Vec<Outgoing> {
        self.failures = 0;
        if transition == 0 {
            self.reinitializing = false;
            self.last_transition = Some(0);
            return Vec::new();
        }
        self.pending = Some((transition, self.version));
        vec![ready(transition)]
    }

    /// Opcode 29: the commit that won, ours or another member's.
    pub fn commit(&mut self, payload: &[u8]) -> Vec<Outgoing> {
        let (Some(session), Some((id, commit))) = (&mut self.session, payload.split_first_chunk::<2>()) else {
            return Vec::new();
        };
        let transition = u16::from_be_bytes(*id);
        match session.process_commit(commit) {
            Ok(()) => self.joined_epoch(transition),
            Err(err) => {
                eprintln!("mediad: dave: commit for transition {transition} refused: {err}");
                self.recover(transition)
            }
        }
    }

    /// Opcode 30: we were added to the group.
    pub fn welcome(&mut self, payload: &[u8]) -> Vec<Outgoing> {
        let (Some(session), Some((id, welcome))) = (&mut self.session, payload.split_first_chunk::<2>()) else {
            return Vec::new();
        };
        let transition = u16::from_be_bytes(*id);
        match session.process_welcome(welcome) {
            Ok(()) => self.joined_epoch(transition),
            Err(err) => {
                eprintln!("mediad: dave: welcome for transition {transition} refused: {err}");
                self.recover(transition)
            }
        }
    }

    /// Our Opus frame as it goes into RTP. Unencrypted until the group has keys, as Discord's own
    /// clients do; silence is never encrypted.
    pub fn encrypt(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        if !self.encrypting() || frame == SILENCE {
            return Some(frame.to_vec());
        }
        match self.session.as_mut()?.encrypt_opus(frame) {
            Ok(sealed) => Some(sealed.into_owned()),
            Err(err) => {
                eprintln!("mediad: dave: frame not encrypted, dropped: {err}");
                None
            }
        }
    }

    /// `user_id`'s frame as the Opus decoder wants it. `None` drops the frame: it could not be
    /// decrypted, or it is encrypted and there are no keys for it yet. `again` holds what the
    /// gateway must send when too many frames failed and the group has to be rebuilt.
    pub fn decrypt(&mut self, user_id: Option<u64>, frame: &[u8], again: &mut Vec<Outgoing>) -> Option<Vec<u8>> {
        if frame == SILENCE {
            return Some(frame.to_vec());
        }
        let sealed = frame.ends_with(&MARKER);
        let session = match &mut self.session {
            Some(session) if session.is_ready() => session,
            // No group: clear frames are all there can be. A sealed one is not Opus.
            _ => return (!sealed).then(|| frame.to_vec()),
        };
        let user_id = user_id?;
        if self.version == 0 && !session.can_passthrough(user_id) {
            return (!sealed).then(|| frame.to_vec());
        }
        match session.decrypt(user_id, MediaType::AUDIO, frame) {
            Ok(opus) => {
                self.failures = 0;
                Some(opus)
            }
            Err(_) => {
                if !self.reinitializing && self.pending.is_none() {
                    self.failures += 1;
                    if self.failures > TOLERANCE {
                        if let Some(transition) = self.last_transition {
                            again.extend(self.recover(transition));
                        }
                    }
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod mls {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mls.rs"));
    }

    const CHANNEL: u64 = 900;
    const TONE: &[u8] = &[0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    fn binary(out: Vec<Outgoing>, op: u8) -> Vec<u8> {
        match out.into_iter().next() {
            Some(Outgoing::Binary { op: got, payload }) if got == op => payload,
            other => panic!("expected opcode {op}, got {other:?}"),
        }
    }

    /// Two members brought into one group the way the voice gateway does it: `first` is alone,
    /// the server proposes `second`, `first` commits, the server announces the commit under
    /// `transition` and welcomes `second`.
    fn grouped(transition: u16) -> (Dave, Dave) {
        let server = mls::Server::new();
        let (mut first, mut second) = (Dave::new(111, CHANNEL), Dave::new(222, CHANNEL));
        binary(first.start(1), KEY_PACKAGE);
        let package = binary(second.start(1), KEY_PACKAGE);
        first.external_sender(&server.package());
        second.external_sender(&server.package());
        assert!(!first.encrypting(), "alone, there is no group to encrypt to");

        let proposals = server.add(&package, CHANNEL, 0);
        let (commit, welcome) = mls::split(&binary(first.proposals(&proposals, &[111, 222]), COMMIT_WELCOME));
        let announce = |bytes: &[u8]| [&transition.to_be_bytes()[..], bytes].concat();
        let ready = vec![Outgoing::Json { op: TRANSITION_READY, d: json!({ "transition_id": transition }) }];
        let expected = if transition == 0 { Vec::new() } else { ready };
        assert_eq!(first.commit(&announce(&commit)), expected);
        assert_eq!(second.welcome(&announce(&welcome)), expected);
        first.execute_transition(transition);
        second.execute_transition(transition);
        (first, second)
    }

    #[test]
    fn two_members_hear_each_other_and_nobody_else_does() {
        let (mut first, mut second) = grouped(7);
        assert!(first.encrypting() && second.encrypting());
        assert_eq!(first.privacy_code(), second.privacy_code());
        assert!(first.privacy_code().is_some());

        let mut again = Vec::new();
        let sealed = first.encrypt(TONE).unwrap();
        assert_ne!(sealed, TONE);
        assert!(sealed.ends_with(&MARKER), "every encrypted frame ends with the marker");
        assert_eq!(second.decrypt(Some(111), &sealed, &mut again).as_deref(), Some(TONE));
        let back = second.encrypt(TONE).unwrap();
        assert_eq!(first.decrypt(Some(222), &back, &mut again).as_deref(), Some(TONE));

        // Without the sender's identity, or with the wrong one, the frame is dropped.
        let sealed = first.encrypt(TONE).unwrap();
        assert_eq!(second.decrypt(None, &sealed, &mut again), None);
        assert_eq!(second.decrypt(Some(333), &sealed, &mut again), None);
        // An outsider with no group gets nothing it would mistake for Opus.
        let mut outsider = Dave::new(333, CHANNEL);
        assert_eq!(outsider.decrypt(Some(111), &sealed, &mut again), None);
        assert!(again.is_empty());
    }

    #[test]
    fn transition_zero_takes_effect_at_once() {
        let (mut first, mut second) = grouped(0);
        let mut again = Vec::new();
        let sealed = first.encrypt(TONE).unwrap();
        assert_eq!(second.decrypt(Some(111), &sealed, &mut again).as_deref(), Some(TONE));
    }

    #[test]
    fn silence_is_never_encrypted() {
        let (mut first, mut second) = grouped(1);
        let mut again = Vec::new();
        assert_eq!(first.encrypt(&SILENCE).unwrap(), SILENCE);
        assert_eq!(second.decrypt(Some(111), &SILENCE, &mut again).as_deref(), Some(&SILENCE[..]));
    }

    #[test]
    fn a_proposal_for_someone_not_in_the_call_is_refused() {
        let server = mls::Server::new();
        let (mut first, mut ghost) = (Dave::new(111, CHANNEL), Dave::new(666, CHANNEL));
        first.start(1);
        first.external_sender(&server.package());
        let package = binary(ghost.start(1), KEY_PACKAGE);
        let proposals = server.add(&package, CHANNEL, 0);
        assert_eq!(first.proposals(&proposals, &[111, 222]), Vec::new());
    }

    #[test]
    fn a_commit_that_does_not_apply_is_reported_and_a_new_key_package_offered() {
        let (mut first, _) = grouped(3);
        let out = first.commit(&[0, 9, 1, 2, 3]);
        assert_eq!(out[0], Outgoing::Json { op: INVALID_COMMIT_WELCOME, d: json!({ "transition_id": 9 }) });
        assert!(matches!(&out[1], Outgoing::Binary { op: KEY_PACKAGE, payload } if !payload.is_empty()));
        // Once is enough until the group is rebuilt.
        assert_eq!(first.commit(&[0, 9, 1, 2, 3]), Vec::new());
    }

    #[test]
    fn a_downgrade_is_announced_then_executed_and_frames_pass_through() {
        let (mut first, mut second) = grouped(4);
        let ready = vec![Outgoing::Json { op: TRANSITION_READY, d: json!({ "transition_id": 5 }) }];
        assert_eq!(first.prepare_transition(5, 0), ready);
        assert_eq!(first.version(), 1, "announced is not executed");
        assert!(first.encrypting());
        assert!(!first.execute_transition(6), "another transition's id changes nothing");
        assert!(first.execute_transition(5));
        assert_eq!(first.version(), 0);
        assert!(!first.encrypting());
        assert_eq!(first.encrypt(TONE).unwrap(), TONE);

        // The member that has not executed yet still accepts the clear frame meanwhile.
        second.prepare_transition(5, 0);
        let mut again = Vec::new();
        assert_eq!(second.decrypt(Some(111), TONE, &mut again).as_deref(), Some(TONE));
    }

    #[test]
    fn version_zero_from_the_start_is_plain_passthrough() {
        let mut dave = Dave::new(111, CHANNEL);
        assert_eq!(dave.start(0), Vec::new());
        let mut again = Vec::new();
        assert_eq!(dave.encrypt(TONE).unwrap(), TONE);
        assert_eq!(dave.decrypt(None, TONE, &mut again).as_deref(), Some(TONE));
    }

    #[test]
    fn the_external_sender_may_arrive_before_the_session() {
        let server = mls::Server::new();
        let (mut early, mut second) = (Dave::new(111, CHANNEL), Dave::new(222, CHANNEL));
        early.external_sender(&server.package());
        early.start(1);
        let package = binary(second.start(1), KEY_PACKAGE);
        let out = early.proposals(&server.add(&package, CHANNEL, 0), &[111, 222]);
        assert!(matches!(&out[0], Outgoing::Binary { op: COMMIT_WELCOME, .. }));
    }

    #[test]
    fn a_new_epoch_one_rebuilds_the_group_from_a_fresh_key_package() {
        let (mut first, _) = grouped(2);
        assert_eq!(first.prepare_epoch(5, 1), Vec::new(), "only epoch 1 is a rebuild");
        let out = first.prepare_epoch(1, 1);
        assert!(matches!(&out[0], Outgoing::Binary { op: KEY_PACKAGE, .. }));
        assert!(!first.encrypting());
    }
}
