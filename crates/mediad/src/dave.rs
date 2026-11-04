//! Discord's end-to-end encryption (DAVE) for one voice connection: the MLS group through
//! `davey`, and the transitions the voice gateway steers it through.
//!
//! The gateway hands each DAVE opcode's payload to a method here and sends whatever comes back.
//! The media loop calls `encrypt` and `decrypt` on every Opus frame, and `encrypt_video` and
//! `decrypt_video` on every H.264 access unit. Protocol version 0 means the call is not
//! end-to-end encrypted and frames pass through untouched.

use crate::rtp::SILENCE;
use davey::{Codec, DaveSession, MediaType, ProposalsOperationType};
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
            tracing::warn!("mediad: dave: could not start a session at version {version}: {err}");
            return Vec::new();
        }
        if let Some(sender) = self.early_sender.take() {
            self.external_sender(&sender);
        }
        match self.session.as_mut().unwrap().create_key_package() {
            Ok(payload) => vec![Outgoing::Binary { op: KEY_PACKAGE, payload }],
            Err(err) => {
                tracing::warn!("mediad: dave: no key package: {err}");
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
            tracing::warn!("mediad: dave: external sender refused: {err}");
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
                tracing::warn!("mediad: dave: unknown proposals operation {other}");
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
                tracing::warn!("mediad: dave: proposals refused: {err}");
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
                tracing::warn!("mediad: dave: commit for transition {transition} refused: {err}");
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
                tracing::warn!("mediad: dave: welcome for transition {transition} refused: {err}");
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
                tracing::warn!("mediad: dave: frame not encrypted, dropped: {err}");
                None
            }
        }
    }

    /// One H.264 access unit, Annex B, as it goes into RTP. The NAL headers stay clear. Unencrypted
    /// until the group has keys. `None` drops the frame: it could not be sealed.
    pub fn encrypt_video(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        if !self.encrypting() {
            return Some(frame.to_vec());
        }
        match self.session.as_mut()?.encrypt(MediaType::VIDEO, Codec::H264, frame) {
            Ok(sealed) => Some(sealed.into_owned()),
            Err(err) => {
                tracing::warn!("mediad: dave: a picture was not encrypted, dropped: {err}");
                None
            }
        }
    }

    /// `user_id`'s picture as the decoder wants it. `None` drops it. A failure here does not
    /// count against the voice tolerance: a picture that will not open is not a broken group.
    pub fn decrypt_video(&mut self, user_id: Option<u64>, frame: &[u8]) -> Option<Vec<u8>> {
        let sealed = frame.ends_with(&MARKER);
        let session = match &mut self.session {
            Some(session) if session.is_ready() => session,
            _ => return (!sealed).then(|| frame.to_vec()),
        };
        let user_id = user_id?;
        if self.version == 0 && !session.can_passthrough(user_id) {
            return (!sealed).then(|| frame.to_vec());
        }
        session.decrypt(user_id, MediaType::VIDEO, frame).ok()
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
    fn a_picture_keeps_its_nal_headers_and_opens_for_the_other_member() {
        let (mut first, mut second) = grouped(7);
        let frame = [
            0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xff, 0x10, 0x22, 0x33, 0x44, 0x55,
        ];
        let sealed = first.encrypt_video(&frame).unwrap();
        assert_ne!(sealed, frame);
        assert!(sealed.ends_with(&MARKER));
        assert_eq!(sealed[4], 0x67, "the SPS NAL header stays clear");
        let idr = sealed.windows(5).position(|window| window == [0, 0, 0, 1, 0x65]).expect("idr start");
        assert_eq!(sealed[idr + 4], 0x65, "the IDR NAL header stays clear");
        assert_ne!(&sealed[idr + 5..idr + 5 + 8], &frame[13..21], "the slice body is not in clear");
        assert_eq!(second.decrypt_video(Some(111), &sealed).as_deref(), Some(&frame[..]));
        assert_eq!(second.decrypt_video(Some(333), &sealed), None);
    }

    /// The path a Discord picture actually takes: DAVE on the access unit, then H.264 packets,
    /// then the transport cipher with the one-byte extension. The other member gets the same
    /// access unit back, and a lost packet yields nothing until a keyframe.
    #[test]
    fn a_picture_round_trips_through_rtp_and_a_gap_waits_for_a_keyframe() {
        use crate::h264::{packetize, Assembler};
        use crate::rtp::{video_extension, Cipher, VideoSender, AES_GCM, H264};

        let (mut first, mut second) = grouped(7);
        let frame = [
            0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xff, 0x10, 0x22, 0x33, 0x44,
            0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
        ];
        let sealed = first.encrypt_video(&frame).unwrap();
        let key = [9u8; 32];
        let mut sender = VideoSender::new(Cipher::new(AES_GCM, &key).unwrap(), 50_000);
        let opener = Cipher::new(AES_GCM, &key).unwrap();
        let extension = video_extension(true, false);
        let pieces = packetize(&sealed, 24);
        assert!(pieces.len() > 1, "the access unit has to cross more than one packet");
        let mut assembler = Assembler::default();
        let mut recovered = None;
        for piece in &pieces {
            let (datagram, _) = sender.seal(H264, piece.marker, 9_000, &piece.payload, &extension).unwrap();
            assert_eq!(datagram[0] & 0x10, 0x10, "the extension bit is set");
            assert_ne!(datagram[1] & 0x7f, 120, "a picture is not an audio packet");
            let packet = opener.unseal(&datagram).unwrap();
            assert_eq!(packet.payload_type, H264);
            assert_eq!(packet.marker, piece.marker);
            if let Some(unit) = assembler.push(packet.sequence, packet.timestamp, packet.marker, &packet.payload) {
                recovered = Some(unit);
            }
        }
        let unit = recovered.expect("the last packet closed the access unit");
        assert_eq!(second.decrypt_video(Some(111), &unit).as_deref(), Some(&frame[..]));

        let mut broken = Assembler::default();
        let first_packet = opener.unseal(&{
            let (datagram, _) = sender.seal(H264, pieces[0].marker, 18_000, &pieces[0].payload, &extension).unwrap();
            datagram
        }).unwrap();
        assert!(broken.push(first_packet.sequence, first_packet.timestamp, first_packet.marker, &first_packet.payload).is_none());
        let skipped = first_packet.sequence.wrapping_add(2);
        let later = &pieces[pieces.len() - 1];
        assert!(broken.push(skipped, 18_000, later.marker, &later.payload).is_none(), "a gap does not invent an access unit");
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
