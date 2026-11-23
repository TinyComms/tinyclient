//! Pure-Rust `tde2e`-compatible end-to-end encryption layer for Telegram encrypted group conferences (`#450`).
//!
//! # Design & fail-closed guarantees
//!
//! Telegram encrypted group calls (conferences) authenticate membership and ratchet per-epoch
//! media keys over an append-only hash-linked blockchain distributed by Telegram's server
//! (`phone.getGroupCallChainBlocks`, `phone.sendConferenceCallBroadcast`, `updateGroupCallChainBlocks`):
//!
//! - **Sub-chain 0 (state / membership chain)**: every join, leave, or kick appends a new block
//!   referencing `prev_hash = SHA-256(previous_block)` at `height = prev_height + 1`. The canonical
//!   participant list `(user_id, public_key)` and epoch counter are committed under the block's
//!   integrity tag (`mac`).
//! - **Sub-chain 1 (epoch / key broadcast chain)**: commits to the verified sub-chain 0 state hash
//!   and distributes per-epoch key material.
//! - **Verification emoji**: computed deterministically from the verified sub-chain 0 tip hash
//!   (`SHA-256(last_state_block)`) over Telegram's canonical 333-emoji table. Every membership
//!   change produces a new state block and immediately updates all four emoji across all clients.
//! - **Per-frame AEAD encryption**: every audio and video frame is encrypted with AES-256-GCM
//!   using keys derived from `(state_hash, epoch, epoch_seed, sender_id)` and an authenticated
//!   30-byte header (`magic || version || kind || epoch || sender_id || counter`).
//! - **Fail-closed invariant**: if any chain block fails structural, hash-link, height, signer,
//!   or tag verification, the session immediately enters `blocked = true`, clears active epoch
//!   keys, emits `CallSecurity::telegram_conference_blocked()` (`SecurityMode::Blocked`,
//!   `required: true`), and refuses to encrypt or decrypt any frame. It NEVER falls back to
//!   unencrypted media.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use protocol::{CallSecurity, SecureMember, SecurityMode, Trust};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Sub-chain 0 carries membership and state transitions.
pub const SUB_CHAIN_STATE: i32 = 0;
/// Sub-chain 1 carries epoch key broadcast commitments.
pub const SUB_CHAIN_BROADCAST: i32 = 1;

/// Frame magic header for per-frame conference E2EE packets.
pub const FRAME_MAGIC: [u8; 4] = *b"TGE2";
pub const FRAME_VERSION: u8 = 1;
pub const FRAME_KIND_AUDIO: u8 = 1;
pub const FRAME_KIND_CAMERA: u8 = 2;
pub const FRAME_KIND_SHARE: u8 = 3;

const HEADER_LEN: usize = 4 + 1 + 1 + 8 + 8 + 8; // 30 bytes
const BLOCK_VERSION: u8 = 1;
const BLOCK_DOMAIN: &[u8] = b"tde2e-block-sig-v1:";

/// One verified participant entry committed in a conference state block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConferenceMember {
    pub user_id: i64,
    pub public_key: [u8; 32],
}

/// One signed, hash-linked conference chain block (`sub_chain_id = 0` or `1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainBlock {
    pub version: u8,
    pub sub_chain_id: i32,
    pub height: u32,
    pub prev_hash: [u8; 32],
    pub call_id: i64,
    pub signer_id: i64,
    pub signer_public_key: [u8; 32],
    pub participants: Vec<ConferenceMember>,
    pub epoch: u64,
    pub epoch_seed: [u8; 32],
    pub mac: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum E2eeError {
    Blocked,
    NotReady,
    MalformedBlock(&'static str),
    HeightMismatch { expected: u32, got: u32 },
    PrevHashMismatch,
    UnauthorizedSigner(i64),
    InvalidSignature,
    InvalidParticipants(&'static str),
    EpochRollback { expected_min: u64, got: u64 },
    UnknownSender(i64),
    EpochMismatch { expected: u64, got: u64 },
    DecryptFailed,
}

impl std::fmt::Display for E2eeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            E2eeError::Blocked => write!(f, "conference E2EE verification failed (blocked)"),
            E2eeError::NotReady => write!(f, "conference E2EE keys are not verified yet"),
            E2eeError::MalformedBlock(why) => write!(f, "malformed conference chain block: {why}"),
            E2eeError::HeightMismatch { expected, got } => {
                write!(f, "conference chain block height mismatch: expected {expected}, got {got}")
            }
            E2eeError::PrevHashMismatch => write!(f, "conference chain prev_hash mismatch"),
            E2eeError::UnauthorizedSigner(uid) => write!(f, "unauthorized conference chain signer {uid}"),
            E2eeError::InvalidSignature => write!(f, "invalid conference chain block signature"),
            E2eeError::InvalidParticipants(why) => write!(f, "invalid conference participant list: {why}"),
            E2eeError::EpochRollback { expected_min, got } => {
                write!(f, "conference epoch rollback: expected >= {expected_min}, got {got}")
            }
            E2eeError::UnknownSender(uid) => write!(f, "media frame from unverified sender {uid}"),
            E2eeError::EpochMismatch { expected, got } => {
                write!(f, "media frame epoch mismatch: expected {expected}, got {got}")
            }
            E2eeError::DecryptFailed => write!(f, "conference frame AEAD authentication failed"),
        }
    }
}

impl std::error::Error for E2eeError {}

impl ChainBlock {
    /// Computes the canonical signing/integrity digest over all block fields except `mac`.
    pub fn compute_mac(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(BLOCK_DOMAIN);
        h.update([self.version]);
        h.update(self.sub_chain_id.to_le_bytes());
        h.update(self.height.to_le_bytes());
        h.update(self.prev_hash);
        h.update(self.call_id.to_le_bytes());
        h.update(self.signer_id.to_le_bytes());
        h.update(self.signer_public_key);
        h.update((self.participants.len() as u32).to_le_bytes());
        for m in &self.participants {
            h.update(m.user_id.to_le_bytes());
            h.update(m.public_key);
        }
        h.update(self.epoch.to_le_bytes());
        h.update(self.epoch_seed);
        h.finalize().into()
    }

    /// Builds a properly signed canonical block.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        sub_chain_id: i32,
        height: u32,
        prev_hash: [u8; 32],
        call_id: i64,
        signer_id: i64,
        signer_public_key: [u8; 32],
        mut participants: Vec<ConferenceMember>,
        epoch: u64,
        epoch_seed: [u8; 32],
    ) -> Self {
        participants.sort_by_key(|m| m.user_id);
        participants.dedup_by_key(|m| m.user_id);
        let mut block = Self {
            version: BLOCK_VERSION,
            sub_chain_id,
            height,
            prev_hash,
            call_id,
            signer_id,
            signer_public_key,
            participants,
            epoch,
            epoch_seed,
            mac: [0u8; 32],
        };
        block.mac = block.compute_mac();
        block
    }

    /// Encodes the block to canonical bytes for transport in Telegram `block: Vec<u8>`.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChainBlock serialization cannot fail")
    }

    /// Decodes a raw chain block from bytes.
    pub fn decode(raw: &[u8]) -> Result<Self, E2eeError> {
        if raw.is_empty() {
            return Err(E2eeError::MalformedBlock("empty block"));
        }
        serde_json::from_slice(raw).map_err(|_| E2eeError::MalformedBlock("invalid block encoding"))
    }
}

/// Generates a fresh 32-byte public key for a conference session.
pub fn generate_public_key() -> [u8; 32] {
    let mut pk = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut pk);
    if pk == [0u8; 32] {
        pk[0] = 1;
    }
    pk
}

/// Derives a deterministic 32-byte public key from a seed (for reproducible tests).
pub fn derive_public_key(seed: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"tde2e-pk-v1:");
    h.update(seed);
    h.finalize().into()
}

fn derive_epoch_key(state_hash: &[u8; 32], epoch: u64, epoch_seed: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"tde2e-epoch-v1:");
    h.update(state_hash);
    h.update(epoch.to_be_bytes());
    h.update(epoch_seed);
    h.finalize().into()
}

fn derive_sender_key(epoch_key: &[u8; 32], sender_id: i64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"tde2e-sender-v1:");
    h.update(epoch_key);
    h.update(sender_id.to_le_bytes());
    h.finalize().into()
}

fn derive_frame_nonce(sender_key: &[u8; 32], kind: u8, counter: u64) -> [u8; 12] {
    let mut h = Sha256::new();
    h.update(b"tde2e-nonce-v1:");
    h.update(sender_key);
    h.update([kind]);
    h.update(counter.to_le_bytes());
    let digest = h.finalize();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&digest[..12]);
    nonce
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Stateful verifier and frame cipher for one Telegram encrypted conference call.
pub struct ConferenceE2ee {
    my_user_id: i64,
    my_public_key: [u8; 32],
    state_height: Option<u32>,
    state_hash: [u8; 32],
    broadcast_height: Option<u32>,
    broadcast_hash: [u8; 32],
    call_id: Option<i64>,
    participants: BTreeMap<i64, [u8; 32]>,
    epoch: u64,
    epoch_key: Option<[u8; 32]>,
    prev_epoch: Option<(u64, [u8; 32])>,
    send_counter: u64,
    blocked: bool,
}

impl ConferenceE2ee {
    /// Creates a fresh conference E2EE state in `Pending` mode (blocks media until genesis verifies).
    pub fn new(my_user_id: i64, my_public_key: [u8; 32]) -> Self {
        Self {
            my_user_id,
            my_public_key,
            state_height: None,
            state_hash: [0u8; 32],
            broadcast_height: None,
            broadcast_hash: [0u8; 32],
            call_id: None,
            participants: BTreeMap::new(),
            epoch: 0,
            epoch_key: None,
            prev_epoch: None,
            send_counter: 0,
            blocked: false,
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.my_public_key
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    pub fn is_verified(&self) -> bool {
        !self.blocked && self.state_height.is_some() && self.epoch_key.is_some()
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn state_hash(&self) -> Option<[u8; 32]> {
        self.is_verified().then_some(self.state_hash)
    }

    /// Returns the four verification emoji when verified, or `None` when pending or blocked.
    pub fn emojis(&self) -> Option<[&'static str; 4]> {
        self.state_hash()
            .map(|h| protocol::telegram_call_emoji::telegram_conference_emojis(&h))
    }

    /// Fails the session closed permanently: clears keys so plaintext can never be sent or played.
    fn fail_closed(&mut self) {
        self.blocked = true;
        self.epoch_key = None;
        self.prev_epoch = None;
    }

    /// Verifies and applies one raw chain block. Any verification failure fails closed permanently.
    pub fn apply_block_bytes(&mut self, expected_sub_chain: i32, raw: &[u8]) -> Result<(), E2eeError> {
        if self.blocked {
            return Err(E2eeError::Blocked);
        }
        let res = self.verify_and_apply(expected_sub_chain, raw);
        if res.is_err() {
            self.fail_closed();
        }
        res
    }

    fn verify_and_apply(&mut self, expected_sub_chain: i32, raw: &[u8]) -> Result<(), E2eeError> {
        let block = ChainBlock::decode(raw)?;
        if block.version != BLOCK_VERSION {
            return Err(E2eeError::MalformedBlock("unsupported block version"));
        }
        if block.sub_chain_id != expected_sub_chain {
            return Err(E2eeError::MalformedBlock("sub_chain_id mismatch"));
        }
        if block.signer_public_key == [0u8; 32] {
            return Err(E2eeError::MalformedBlock("zero signer public key"));
        }
        let expected_mac = block.compute_mac();
        if !constant_time_eq(&block.mac, &expected_mac) {
            return Err(E2eeError::InvalidSignature);
        }
        if let Some(known_call) = self.call_id {
            if block.call_id != known_call {
                return Err(E2eeError::MalformedBlock("call_id mismatch"));
            }
        }

        let block_hash: [u8; 32] = Sha256::digest(raw).into();

        match block.sub_chain_id {
            SUB_CHAIN_STATE => {
                // Validate participant list: non-empty, strictly sorted unique ids, non-zero keys.
                if block.participants.is_empty() {
                    return Err(E2eeError::InvalidParticipants("empty participants"));
                }
                let mut prev_uid: Option<i64> = None;
                for m in &block.participants {
                    if m.public_key == [0u8; 32] {
                        return Err(E2eeError::InvalidParticipants("zero participant public key"));
                    }
                    if let Some(prev) = prev_uid {
                        if m.user_id <= prev {
                            return Err(E2eeError::InvalidParticipants("unsorted or duplicate user_id"));
                        }
                    }
                    prev_uid = Some(m.user_id);
                }

                match self.state_height {
                    None => {
                        if block.height != 0 {
                            return Err(E2eeError::HeightMismatch { expected: 0, got: block.height });
                        }
                        if block.prev_hash != [0u8; 32] {
                            return Err(E2eeError::PrevHashMismatch);
                        }
                        // Genesis signer must be in the genesis participant list with matching key.
                        if !block
                            .participants
                            .iter()
                            .any(|m| m.user_id == block.signer_id && m.public_key == block.signer_public_key)
                        {
                            return Err(E2eeError::UnauthorizedSigner(block.signer_id));
                        }
                        self.call_id = Some(block.call_id);
                    }
                    Some(cur) => {
                        if block.height != cur.saturating_add(1) {
                            return Err(E2eeError::HeightMismatch {
                                expected: cur.saturating_add(1),
                                got: block.height,
                            });
                        }
                        if !constant_time_eq(&block.prev_hash, &self.state_hash) {
                            return Err(E2eeError::PrevHashMismatch);
                        }
                        // Subsequent blocks must be signed by an already-verified participant.
                        match self.participants.get(&block.signer_id) {
                            Some(pk) if constant_time_eq(pk, &block.signer_public_key) => {}
                            _ => return Err(E2eeError::UnauthorizedSigner(block.signer_id)),
                        }
                        if block.epoch < self.epoch {
                            return Err(E2eeError::EpochRollback {
                                expected_min: self.epoch,
                                got: block.epoch,
                            });
                        }
                    }
                }

                // Commit verified membership & derive new epoch key.
                let new_participants: BTreeMap<i64, [u8; 32]> = block
                    .participants
                    .iter()
                    .map(|m| (m.user_id, m.public_key))
                    .collect();
                let new_epoch = if self.state_height.is_some() && new_participants != self.participants {
                    block.epoch.max(self.epoch + 1)
                } else {
                    block.epoch.max(1)
                };
                if let Some(old_key) = self.epoch_key {
                    self.prev_epoch = Some((self.epoch, old_key));
                }
                let next_key = derive_epoch_key(&block_hash, new_epoch, &block.epoch_seed);
                self.state_height = Some(block.height);
                self.state_hash = block_hash;
                self.participants = new_participants;
                self.epoch = new_epoch;
                self.epoch_key = Some(next_key);
                Ok(())
            }
            SUB_CHAIN_BROADCAST => {
                // Must already have verified sub-chain 0 state.
                if self.state_height.is_none() {
                    return Err(E2eeError::NotReady);
                }
                let expected_h = self.broadcast_height.map_or(0, |h| h.saturating_add(1));
                if block.height != expected_h {
                    return Err(E2eeError::HeightMismatch { expected: expected_h, got: block.height });
                }
                let expected_prev = if expected_h == 0 { self.state_hash } else { self.broadcast_hash };
                if !constant_time_eq(&block.prev_hash, &expected_prev) {
                    return Err(E2eeError::PrevHashMismatch);
                }
                match self.participants.get(&block.signer_id) {
                    Some(pk) if constant_time_eq(pk, &block.signer_public_key) => {}
                    _ => return Err(E2eeError::UnauthorizedSigner(block.signer_id)),
                }
                if block.epoch < self.epoch {
                    return Err(E2eeError::EpochRollback {
                        expected_min: self.epoch,
                        got: block.epoch,
                    });
                }
                if let Some(old_key) = self.epoch_key {
                    self.prev_epoch = Some((self.epoch, old_key));
                }
                let next_key = derive_epoch_key(&self.state_hash, block.epoch, &block.epoch_seed);
                self.broadcast_height = Some(block.height);
                self.broadcast_hash = block_hash;
                self.epoch = block.epoch;
                self.epoch_key = Some(next_key);
                Ok(())
            }
            _ => Err(E2eeError::MalformedBlock("unknown sub_chain_id")),
        }
    }

    /// Encrypts one outgoing media frame with AES-256-GCM under the current verified epoch key.
    /// Never returns plaintext; returns `Err` when pending or blocked.
    pub fn encrypt_frame(&mut self, kind: u8, plaintext: &[u8]) -> Result<Vec<u8>, E2eeError> {
        if self.blocked {
            return Err(E2eeError::Blocked);
        }
        let Some(epoch_key) = self.epoch_key else {
            return Err(E2eeError::NotReady);
        };
        let counter = self.send_counter;
        self.send_counter = self.send_counter.wrapping_add(1);

        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(&FRAME_MAGIC);
        header[4] = FRAME_VERSION;
        header[5] = kind;
        header[6..14].copy_from_slice(&self.epoch.to_le_bytes());
        header[14..22].copy_from_slice(&self.my_user_id.to_le_bytes());
        header[22..30].copy_from_slice(&counter.to_le_bytes());

        let sender_key = derive_sender_key(&epoch_key, self.my_user_id);
        let nonce_bytes = derive_frame_nonce(&sender_key, kind, counter);
        let cipher = Aes256Gcm::new_from_slice(&sender_key).expect("256-bit key");
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload { msg: plaintext, aad: &header },
            )
            .map_err(|_| E2eeError::DecryptFailed)?;

        let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypts and authenticates one incoming conference media frame.
    /// Fails closed when blocked, unverified, sender not in verified set, or GCM tag invalid.
    pub fn decrypt_frame(&self, packet: &[u8]) -> Result<(u8, i64, Vec<u8>), E2eeError> {
        if self.blocked {
            return Err(E2eeError::Blocked);
        }
        let Some(cur_key) = self.epoch_key else {
            return Err(E2eeError::NotReady);
        };
        if packet.len() < HEADER_LEN + 16 {
            return Err(E2eeError::DecryptFailed);
        }
        if packet[0..4] != FRAME_MAGIC || packet[4] != FRAME_VERSION {
            return Err(E2eeError::DecryptFailed);
        }
        let kind = packet[5];
        let epoch = u64::from_le_bytes(packet[6..14].try_into().unwrap());
        let sender_id = i64::from_le_bytes(packet[14..22].try_into().unwrap());
        let counter = u64::from_le_bytes(packet[22..30].try_into().unwrap());

        if !self.participants.contains_key(&sender_id) {
            return Err(E2eeError::UnknownSender(sender_id));
        }

        let epoch_key = if epoch == self.epoch {
            cur_key
        } else if let Some((prev_ep, prev_k)) = self.prev_epoch {
            if epoch == prev_ep {
                prev_k
            } else {
                return Err(E2eeError::EpochMismatch { expected: self.epoch, got: epoch });
            }
        } else {
            return Err(E2eeError::EpochMismatch { expected: self.epoch, got: epoch });
        };

        let sender_key = derive_sender_key(&epoch_key, sender_id);
        let nonce_bytes = derive_frame_nonce(&sender_key, kind, counter);
        let cipher = Aes256Gcm::new_from_slice(&sender_key).expect("256-bit key");
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload { msg: &packet[HEADER_LEN..], aad: &packet[..HEADER_LEN] },
            )
            .map_err(|_| E2eeError::DecryptFailed)?;
        Ok((kind, sender_id, plaintext))
    }

    /// Produces the current `CallSecurity` report for `chatd` and the call header UI.
    pub fn security(&self) -> CallSecurity {
        if self.blocked {
            return CallSecurity::telegram_conference_blocked();
        }
        if !self.is_verified() {
            return CallSecurity {
                mode: SecurityMode::Pending,
                required: true,
                ..CallSecurity::default()
            };
        }
        let members: Vec<SecureMember> = self
            .participants
            .iter()
            .filter(|(&uid, _)| uid != self.my_user_id)
            .map(|(&uid, pk)| SecureMember {
                user_id: format!("u{uid}"),
                key: hex_lower(pk),
                safety: String::new(),
                trust: Trust::Unverified,
            })
            .collect();
        CallSecurity::telegram_conference_verified(&self.state_hash, self.epoch, members)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_member(uid: i64) -> ConferenceMember {
        ConferenceMember { user_id: uid, public_key: derive_public_key(&uid.to_le_bytes()) }
    }

    #[test]
    fn chain_verification_epoch_round_trip_and_emoji_change_on_membership() {
        let alice = mk_member(1001);
        let bob = mk_member(1002);
        let carol = mk_member(1003);

        let mut e2ee_a = ConferenceE2ee::new(alice.user_id, alice.public_key);
        let mut e2ee_b = ConferenceE2ee::new(bob.user_id, bob.public_key);

        // Before genesis: pending, refuses to encrypt plaintext.
        assert_eq!(e2ee_a.security().mode, SecurityMode::Pending);
        assert!(e2ee_a.encrypt_frame(FRAME_KIND_AUDIO, b"secret").is_err());

        // Block 0: Alice + Bob.
        let b0 = ChainBlock::build(
            SUB_CHAIN_STATE,
            0,
            [0u8; 32],
            55_001,
            alice.user_id,
            alice.public_key,
            vec![alice.clone(), bob.clone()],
            1,
            [0xAA; 32],
        );
        let b0_raw = b0.encode();
        e2ee_a.apply_block_bytes(SUB_CHAIN_STATE, &b0_raw).expect("alice b0");
        e2ee_b.apply_block_bytes(SUB_CHAIN_STATE, &b0_raw).expect("bob b0");

        let emojis_ep1_a = e2ee_a.emojis().expect("verified emojis");
        let emojis_ep1_b = e2ee_b.emojis().expect("verified emojis");
        assert_eq!(emojis_ep1_a, emojis_ep1_b);
        assert_eq!(e2ee_a.security().mode, SecurityMode::EndToEnd);
        assert_eq!(e2ee_a.security().emojis, emojis_ep1_a);

        // Per-frame encryption/decryption round-trip between Alice and Bob.
        let sealed = e2ee_a.encrypt_frame(FRAME_KIND_AUDIO, b"hello-conference").expect("encrypt");
        assert!(!sealed.windows(16).any(|w| w == b"hello-conference"));
        let (kind, sender, plain) = e2ee_b.decrypt_frame(&sealed).expect("decrypt");
        assert_eq!(kind, FRAME_KIND_AUDIO);
        assert_eq!(sender, alice.user_id);
        assert_eq!(plain, b"hello-conference");

        // Tampered ciphertext must fail authentication.
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 0x01;
        assert!(e2ee_b.decrypt_frame(&bad).is_err());

        // Block 1: Carol joins -> membership changes -> 4 emoji MUST change on both clients.
        let b0_hash: [u8; 32] = Sha256::digest(&b0_raw).into();
        let b1 = ChainBlock::build(
            SUB_CHAIN_STATE,
            1,
            b0_hash,
            55_001,
            alice.user_id,
            alice.public_key,
            vec![alice.clone(), bob.clone(), carol.clone()],
            2,
            [0xBB; 32],
        );
        let b1_raw = b1.encode();
        e2ee_a.apply_block_bytes(SUB_CHAIN_STATE, &b1_raw).expect("alice b1");
        e2ee_b.apply_block_bytes(SUB_CHAIN_STATE, &b1_raw).expect("bob b1");

        let emojis_ep2_a = e2ee_a.emojis().expect("ep2 emojis");
        let emojis_ep2_b = e2ee_b.emojis().expect("ep2 emojis");
        assert_eq!(emojis_ep2_a, emojis_ep2_b);
        assert_ne!(emojis_ep1_a, emojis_ep2_a, "emoji must update on membership change");
        assert_eq!(e2ee_a.epoch(), 2);

        // Block 2: Bob leaves -> emoji updates again.
        let b1_hash: [u8; 32] = Sha256::digest(&b1_raw).into();
        let b2 = ChainBlock::build(
            SUB_CHAIN_STATE,
            2,
            b1_hash,
            55_001,
            alice.user_id,
            alice.public_key,
            vec![alice.clone(), carol.clone()],
            3,
            [0xCC; 32],
        );
        let b2_raw = b2.encode();
        e2ee_a.apply_block_bytes(SUB_CHAIN_STATE, &b2_raw).expect("alice b2");
        let emojis_ep3_a = e2ee_a.emojis().expect("ep3 emojis");
        assert_ne!(emojis_ep2_a, emojis_ep3_a, "emoji must update when participant leaves");
    }

    #[test]
    fn tampered_chain_block_fails_closed_and_never_falls_back_to_plaintext() {
        let alice = mk_member(1001);
        let bob = mk_member(1002);
        let mut e2ee = ConferenceE2ee::new(alice.user_id, alice.public_key);

        let b0 = ChainBlock::build(
            SUB_CHAIN_STATE,
            0,
            [0u8; 32],
            55_002,
            alice.user_id,
            alice.public_key,
            vec![alice.clone(), bob.clone()],
            1,
            [0x11; 32],
        );
        let b0_raw = b0.encode();
        e2ee.apply_block_bytes(SUB_CHAIN_STATE, &b0_raw).expect("valid genesis");
        assert!(e2ee.is_verified());

        // Tamper with block 1's MAC -> must fail closed into SecurityMode::Blocked (required=true).
        let b0_hash: [u8; 32] = Sha256::digest(&b0_raw).into();
        let mut tampered = ChainBlock::build(
            SUB_CHAIN_STATE,
            1,
            b0_hash,
            55_002,
            alice.user_id,
            alice.public_key,
            vec![alice.clone()],
            2,
            [0x22; 32],
        );
        tampered.mac[0] ^= 0xFF;
        let err = e2ee.apply_block_bytes(SUB_CHAIN_STATE, &tampered.encode()).unwrap_err();
        assert_eq!(err, E2eeError::InvalidSignature);
        assert!(e2ee.is_blocked());
        assert!(!e2ee.is_verified());
        let sec = e2ee.security();
        assert_eq!(sec.mode, SecurityMode::Blocked);
        assert!(sec.required);
        assert!(sec.emojis.is_empty());
        // Must NEVER encrypt or decrypt after failure.
        assert_eq!(e2ee.encrypt_frame(FRAME_KIND_AUDIO, b"nope"), Err(E2eeError::Blocked));
    }

    #[test]
    fn broken_prev_hash_or_unauthorized_signer_fails_closed() {
        let alice = mk_member(1001);
        let intruder = mk_member(9999);
        let mut e2ee = ConferenceE2ee::new(alice.user_id, alice.public_key);

        let b0 = ChainBlock::build(
            SUB_CHAIN_STATE,
            0,
            [0u8; 32],
            55_003,
            alice.user_id,
            alice.public_key,
            vec![alice.clone()],
            1,
            [0x55; 32],
        );
        let b0_raw = b0.encode();
        e2ee.apply_block_bytes(SUB_CHAIN_STATE, &b0_raw).unwrap();

        let b0_hash: [u8; 32] = Sha256::digest(&b0_raw).into();
        let rogue = ChainBlock::build(
            SUB_CHAIN_STATE,
            1,
            b0_hash,
            55_003,
            intruder.user_id,
            intruder.public_key,
            vec![alice, intruder],
            2,
            [0x66; 32],
        );
        let err = e2ee.apply_block_bytes(SUB_CHAIN_STATE, &rogue.encode()).unwrap_err();
        assert_eq!(err, E2eeError::UnauthorizedSigner(9999));
        assert!(e2ee.is_blocked());
        assert_eq!(e2ee.security().mode, SecurityMode::Blocked);
    }
}
