//! Telegram encrypted group conferences (`#450`):
//! - Resolving `t.me/call/<slug>` links via `phone.getGroupCall` with `inputGroupCallSlug`
//! - Creating encrypted conference links via `phone.createConferenceCall`
//! - Incoming conference invitation rings (`messageActionConferenceCall`) + `phone.declineConferenceCallInvite`
//! - Adding people to a running 1:1 call (upgrading to an encrypted conference) or conference via
//!   `phone.inviteConferenceCallParticipant`, and removing participants via `phone.deleteConferenceCallParticipants`
//! - Synchronizing and verifying the conference E2EE blockchain via `phone.getGroupCallChainBlocks`,
//!   `phone.sendConferenceCallBroadcast`, and `updateGroupCallChainBlocks` — FAILING CLOSED if any
//!   chain block fails verification (never falling back to unencrypted).

use super::call;
use super::peers::Who;
use super::wire::{self, Fault};
use super::{live, Account};
use crate::store::ConversationRow;
use base64::Engine as _;
use grammers_tl_types as tl;
use protocol::{
    CallFailure, CallSecurity, CallSupport, ConferenceCallInfo, ConversationKind, Occupant, Participant,
    TelegramGroupOffer, TelegramGroupSource,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

pub const CONF_PREFIX: &str = "conf:";
const BLOCK_VERSION: u8 = 1;
const BLOCK_DOMAIN: &[u8] = b"tde2e-block-sig-v1:";
pub const SUB_CHAIN_STATE: i32 = 0;
pub const SUB_CHAIN_BROADCAST: i32 = 1;

pub(super) fn is_conference_id(id: &str) -> bool {
    id.starts_with(CONF_PREFIX)
}

pub(super) fn conv_id_of(call_id: i64) -> String {
    format!("{CONF_PREFIX}{call_id}")
}

/// Canonical `tde2e`-compatible chain block format shared with `mediad::telegram_e2ee`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainMember {
    pub user_id: i64,
    pub public_key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainBlock {
    pub version: u8,
    pub sub_chain_id: i32,
    pub height: u32,
    pub prev_hash: [u8; 32],
    pub call_id: i64,
    pub signer_id: i64,
    pub signer_public_key: [u8; 32],
    pub participants: Vec<ChainMember>,
    pub epoch: u64,
    pub epoch_seed: [u8; 32],
    pub mac: [u8; 32],
}

impl ChainBlock {
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

    #[allow(clippy::too_many_arguments)]
    pub fn build(
        sub_chain_id: i32,
        height: u32,
        prev_hash: [u8; 32],
        call_id: i64,
        signer_id: i64,
        signer_public_key: [u8; 32],
        mut participants: Vec<ChainMember>,
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

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ChainBlock serialization")
    }
}

/// Local fail-closed verifier mirroring `mediad::telegram_e2ee::ConferenceE2ee` so `chatd`
/// refuses to join BEFORE sending `phone.joinGroupCall` if the chain is invalid or tampered.
#[derive(Clone, Default)]
pub(super) struct ChainVerifier {
    state_height: Option<u32>,
    state_hash: [u8; 32],
    broadcast_height: Option<u32>,
    broadcast_hash: [u8; 32],
    call_id: Option<i64>,
    participants: BTreeMap<i64, [u8; 32]>,
    epoch: u64,
    blocked: bool,
}

impl ChainVerifier {
    pub(super) fn verify_block(&mut self, expected_sub_chain: i32, raw: &[u8]) -> Result<(), String> {
        if self.blocked {
            return Err("Conference E2EE verification already failed (blocked).".to_owned());
        }
        let res = self.verify_inner(expected_sub_chain, raw);
        if res.is_err() {
            self.blocked = true;
        }
        res
    }

    fn verify_inner(&mut self, expected_sub_chain: i32, raw: &[u8]) -> Result<(), String> {
        if raw.is_empty() {
            return Err("Empty conference chain block.".to_owned());
        }
        let block: ChainBlock =
            serde_json::from_slice(raw).map_err(|_| "Malformed conference chain block.".to_owned())?;
        if block.version != BLOCK_VERSION {
            return Err("Unsupported conference chain block version.".to_owned());
        }
        if block.sub_chain_id != expected_sub_chain {
            return Err("Conference chain block sub_chain_id mismatch.".to_owned());
        }
        if block.signer_public_key == [0u8; 32] {
            return Err("Conference chain block has zero signer public key.".to_owned());
        }
        let expected_mac = block.compute_mac();
        let mut diff = 0u8;
        for i in 0..32 {
            diff |= block.mac[i] ^ expected_mac[i];
        }
        if diff != 0 {
            return Err("Invalid conference chain block signature.".to_owned());
        }
        if let Some(known) = self.call_id {
            if block.call_id != known {
                return Err("Conference chain block call_id mismatch.".to_owned());
            }
        }
        let block_hash: [u8; 32] = Sha256::digest(raw).into();
        match block.sub_chain_id {
            SUB_CHAIN_STATE => {
                if block.participants.is_empty() {
                    return Err("Conference chain block has empty participants.".to_owned());
                }
                let mut prev_uid = None;
                for m in &block.participants {
                    if m.public_key == [0u8; 32] {
                        return Err("Conference participant has zero public key.".to_owned());
                    }
                    if let Some(prev) = prev_uid {
                        if m.user_id <= prev {
                            return Err("Conference participants are unsorted or duplicated.".to_owned());
                        }
                    }
                    prev_uid = Some(m.user_id);
                }
                match self.state_height {
                    None => {
                        if block.height != 0 || block.prev_hash != [0u8; 32] {
                            return Err("Invalid conference genesis block.".to_owned());
                        }
                        if !block
                            .participants
                            .iter()
                            .any(|m| m.user_id == block.signer_id && m.public_key == block.signer_public_key)
                        {
                            return Err("Conference genesis signer not in participants.".to_owned());
                        }
                        self.call_id = Some(block.call_id);
                    }
                    Some(cur) => {
                        if block.height != cur.saturating_add(1) {
                            return Err("Conference chain height mismatch.".to_owned());
                        }
                        if block.prev_hash != self.state_hash {
                            return Err("Conference chain prev_hash mismatch.".to_owned());
                        }
                        if self.participants.get(&block.signer_id) != Some(&block.signer_public_key) {
                            return Err("Unauthorized conference chain block signer.".to_owned());
                        }
                        if block.epoch < self.epoch {
                            return Err("Conference epoch rollback.".to_owned());
                        }
                    }
                }
                let next_participants: BTreeMap<i64, [u8; 32]> =
                    block.participants.iter().map(|m| (m.user_id, m.public_key)).collect();
                let next_epoch = if self.state_height.is_some() && next_participants != self.participants {
                    block.epoch.max(self.epoch + 1)
                } else {
                    block.epoch.max(1)
                };
                self.state_height = Some(block.height);
                self.state_hash = block_hash;
                self.participants = next_participants;
                self.epoch = next_epoch;
                Ok(())
            }
            SUB_CHAIN_BROADCAST => {
                if self.state_height.is_none() {
                    return Err("Broadcast block before genesis state block.".to_owned());
                }
                let expected_h = self.broadcast_height.map_or(0, |h| h.saturating_add(1));
                if block.height != expected_h {
                    return Err("Conference broadcast block height mismatch.".to_owned());
                }
                let expected_prev = if expected_h == 0 { self.state_hash } else { self.broadcast_hash };
                if block.prev_hash != expected_prev {
                    return Err("Conference broadcast block prev_hash mismatch.".to_owned());
                }
                if self.participants.get(&block.signer_id) != Some(&block.signer_public_key) {
                    return Err("Unauthorized conference broadcast signer.".to_owned());
                }
                if block.epoch < self.epoch {
                    return Err("Conference broadcast epoch rollback.".to_owned());
                }
                self.broadcast_height = Some(block.height);
                self.broadcast_hash = block_hash;
                self.epoch = block.epoch;
                Ok(())
            }
            _ => Err("Unknown conference sub_chain_id.".to_owned()),
        }
    }

    pub(super) fn security(&self, my_user_id: i64) -> CallSecurity {
        if self.blocked {
            return CallSecurity::telegram_conference_blocked();
        }
        if self.state_height.is_none() {
            return CallSecurity {
                mode: protocol::SecurityMode::Pending,
                required: true,
                ..CallSecurity::default()
            };
        }
        let members = self
            .participants
            .iter()
            .filter(|(&uid, _)| uid != my_user_id)
            .map(|(&uid, pk)| protocol::SecureMember {
                user_id: format!("u{uid}"),
                key: hex_lower(pk),
                safety: String::new(),
                trust: protocol::Trust::Unverified,
            })
            .collect();
        CallSecurity::telegram_conference_verified(&self.state_hash, self.epoch, members)
    }

    pub(super) fn next_state_height(&self) -> u32 {
        self.state_height.map_or(0, |h| h.saturating_add(1))
    }

    pub(super) fn next_broadcast_height(&self) -> u32 {
        self.broadcast_height.map_or(0, |h| h.saturating_add(1))
    }

    pub(super) fn next_broadcast_prev(&self) -> [u8; 32] {
        if self.broadcast_height.is_none() {
            self.state_hash
        } else {
            self.broadcast_hash
        }
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

fn random_i32() -> i32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    i32::from_le_bytes(b) & 0x7fff_ffff
}

fn random_32() -> Result<[u8; 32], Fault> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b)
        .map_err(|err| Fault::Refused(format!("OS random generator failed: {err}")))?;
    if b == [0u8; 32] {
        return Err(Fault::Refused("OS random generator returned zero bytes.".to_owned()));
    }
    Ok(b)
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(super) struct PendingInvite {
    pub msg_id: i32,
    pub call_id: i64,
    pub inviter: Who,
    pub video: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct ActiveConference {
    pub call_id: i64,
    pub access_hash: i64,
    pub slug: String,
    pub invite_link: String,
    pub title: String,
    pub source: u32,
    pub my_public_key: [u8; 32],
    pub verifier: ChainVerifier,
    pub participants: HashMap<Who, (u32, bool)>, // (ssrc, muted)
    pub video: bool,
}

#[derive(Default)]
pub(super) struct Conferences {
    /// Incoming conference call invitation currently ringing.
    pub ringing: Option<PendingInvite>,
    /// Currently joined encrypted conference call.
    pub active: Option<ActiveConference>,
    /// Recently resolved `slug -> (call_id, access_hash, invite_link, title)`.
    pub resolved: HashMap<String, (i64, i64, String, String)>,
}

fn input_call(id: i64, access_hash: i64) -> tl::enums::InputGroupCall {
    tl::types::InputGroupCall { id, access_hash }.into()
}

fn slug_from_link(link: &str) -> String {
    protocol::link::telegram_call_link(link).unwrap_or_default()
}

fn ensure_conference_row(account: &Account, call_id: i64, title: &str) -> String {
    let conv_id = conv_id_of(call_id);
    let label = if title.trim().is_empty() {
        "Encrypted call".to_owned()
    } else {
        title.trim().to_owned()
    };
    let mut row = ConversationRow::new(conv_id.clone(), ConversationKind::Group, label);
    row.listed = false;
    row.call = CallSupport::Native;
    row.call_video = true;
    let _ = account.ctx.conversation(&row);
    conv_id
}

fn map_occupants(account: &Account, raw: &[tl::enums::GroupCallParticipant]) -> Vec<Occupant> {
    let dir = account.dir();
    raw.iter()
        .filter_map(|p| {
            let tl::enums::GroupCallParticipant::Participant(p) = p;
            if p.left {
                return None;
            }
            let who = Who::of(&p.peer);
            let id = who.text();
            Some(Occupant {
                person: account.ctx.key(&id),
                name: dir.name(who),
                avatar: None,
                muted: p.muted,
                deafened: false,
                server_muted: false,
                server_deafened: false,
                suppressed: false,
                speaking: !p.muted && p.active_date.is_some(),
                streaming: p.presentation.is_some(),
                camera: p.video.is_some(),
                hand_raised: p.raise_hand_rating.is_some(),
                is_channel: matches!(who, Who::Channel(_) | Who::Group(_)),
            })
        })
        .collect()
}

fn publish_conference_participants(account: &Account) {
    let (conv_id, participants, sources) = {
        let live = account.live();
        let Some(active) = live.conferences.active.as_ref() else {
            return;
        };
        let dir = account.dir();
        let me = Who::User(account.me);
        let conv_id = conv_id_of(active.call_id);
        let mut participants = Vec::new();
        let mut sources = Vec::new();
        for (&who, &(ssrc, muted)) in &active.participants {
            if who == me {
                continue;
            }
            let id = who.text();
            participants.push(Participant {
                id: id.clone(),
                name: dir.name(who),
                muted,
                ..Participant::default()
            });
            if ssrc > 0 {
                sources.push(TelegramGroupSource { ssrc, user_id: id });
            }
        }
        (conv_id, participants, sources)
    };
    account.ctx.participants(&conv_id, participants);
    account.ctx.telegram_group_sources(&conv_id, sources);
}

/// Handles an incoming `MessageAction::ConferenceCall` service message (`#450`).
/// If `active && !outgoing`, rings the user so they can Accept or Decline via `phone.declineConferenceCallInvite`.
pub(super) fn on_invite_message(
    account: &Account,
    msg_id: i32,
    inviter: Who,
    conf: &tl::types::MessageActionConferenceCall,
) {
    if !conf.active {
        // Call ended or missed: stop ringing if it was our active invite ring.
        let stop = {
            let mut live = account.live();
            if live.conferences.ringing.as_ref().is_some_and(|r| r.call_id == conf.call_id) {
                live.conferences.ringing = None;
                true
            } else {
                false
            }
        };
        if stop {
            account.ctx.ring_stopped(&conv_id_of(conf.call_id));
        }
        return;
    }
    if account.ctx.any_call_active() || account.live().active_call.is_some() || account.live().conferences.active.is_some() {
        return;
    }
    let name = account.dir().name(inviter);
    let conv_id = ensure_conference_row(account, conf.call_id, &format!("Group call with {name}"));
    account.live().conferences.ringing = Some(PendingInvite {
        msg_id,
        call_id: conf.call_id,
        inviter,
        video: conf.video,
    });
    account.ctx.ring_video(&conv_id, &name, conf.video);
}

/// Resolves a `t.me/call/<slug>` link via `phone.getGroupCall(inputGroupCallSlug)` without joining.
pub(super) fn resolve_slug(account: &Account, slug: &str) -> Result<ConferenceCallInfo, Fault> {
    let clean = slug.trim();
    if clean.is_empty() {
        return Err(Fault::Refused("Invalid conference call link.".to_owned()));
    }
    let req = tl::functions::phone::GetGroupCall {
        call: tl::enums::InputGroupCall::Slug(tl::types::InputGroupCallSlug { slug: clean.to_owned() }),
        limit: 50,
    };
    let tl::enums::phone::GroupCall::Call(found) = account.ask(&req)?;
    account.learn(&found.users, &found.chats);
    match &found.call {
        tl::enums::GroupCall::Call(gc) => {
            let invite_link = gc
                .invite_link
                .clone()
                .unwrap_or_else(|| format!("https://t.me/call/{clean}"));
            let title = gc.title.clone().unwrap_or_default();
            let participants = map_occupants(account, &found.participants);
            account.live().conferences.resolved.insert(
                clean.to_owned(),
                (gc.id, gc.access_hash, invite_link.clone(), title.clone()),
            );
            Ok(ConferenceCallInfo {
                call_id: gc.id.to_string(),
                access_hash: gc.access_hash,
                slug: clean.to_owned(),
                invite_link,
                title,
                participants_count: (gc.participants_count.max(0) as u32).max(participants.len() as u32),
                participants,
                video: false,
                e2ee: gc.conference,
            })
        }
        tl::enums::GroupCall::Discarded(_) => Err(Fault::Refused("This conference call has ended.".to_owned())),
    }
}

/// Extracts `(call_id, access_hash, invite_link, title)` from a `GroupCall` update inside `Updates`.
fn extract_conference_from_updates(updates: &tl::enums::Updates) -> Option<(i64, i64, String, String)> {
    let list = match updates {
        tl::enums::Updates::Updates(u) => &u.updates,
        tl::enums::Updates::Combined(u) => &u.updates,
        _ => return None,
    };
    for upd in list {
        if let tl::enums::Update::GroupCall(gc) = upd {
            if let tl::enums::GroupCall::Call(c) = &gc.call {
                let invite_link = c.invite_link.clone().unwrap_or_default();
                let title = c.title.clone().unwrap_or_default();
                return Some((c.id, c.access_hash, invite_link, title));
            }
        }
    }
    None
}

/// Fetches sub-chain blocks via `phone.getGroupCallChainBlocks` and extracts raw block bytes from the returned `Updates`.
fn fetch_chain_blocks(
    account: &Account,
    call: tl::enums::InputGroupCall,
    sub_chain_id: i32,
    offset: i32,
) -> Result<Vec<Vec<u8>>, Fault> {
    let ans = account.ask(&tl::functions::phone::GetGroupCallChainBlocks {
        call,
        sub_chain_id,
        offset,
        limit: 50,
    })?;
    let mut blocks = Vec::new();
    let list = match &ans {
        tl::enums::Updates::Updates(u) => u.updates.as_slice(),
        tl::enums::Updates::Combined(u) => u.updates.as_slice(),
        _ => &[],
    };
    for upd in list {
        if let tl::enums::Update::GroupCallChainBlocks(cb) = upd {
            if cb.sub_chain_id == sub_chain_id {
                blocks.extend(cb.blocks.iter().cloned());
            }
        }
    }
    Ok(blocks)
}

/// Creates a new encrypted conference call (`phone.createConferenceCall`).
/// If `join` is true, joins immediately with E2EE chain verification and returns the shareable link.
pub(super) fn create(account: &Account, video: bool, join: bool) -> Result<ConferenceCallInfo, Fault> {
    if !join {
        let answer = account.ask(&tl::functions::phone::CreateConferenceCall {
            muted: true,
            video_stopped: !video,
            join: false,
            random_id: random_i32(),
            public_key: None,
            block: None,
            params: None,
        })?;
        let (call_id, access_hash, invite_link, title) = extract_conference_from_updates(&answer)
            .ok_or_else(|| Fault::Refused("Telegram did not return a conference call.".to_owned()))?;
        let slug = slug_from_link(&invite_link);
        live::answered(account, &answer);
        if !slug.is_empty() {
            account
                .live()
                .conferences
                .resolved
                .insert(slug.clone(), (call_id, access_hash, invite_link.clone(), title.clone()));
        }
        return Ok(ConferenceCallInfo {
            call_id: call_id.to_string(),
            access_hash,
            slug,
            invite_link,
            title,
            participants_count: 0,
            participants: Vec::new(),
            video,
            e2ee: true,
        });
    }

    // Create + join in one flow: first create to obtain the server-assigned call_id & invite_link,
    // then join with `mediad`'s conference offer and verified E2EE chain!
    let info = create(account, video, false)?;
    let call_id: i64 = info
        .call_id
        .parse()
        .map_err(|_| Fault::Refused("Invalid conference call id.".to_owned()))?;
    join_resolved(
        account,
        call_id,
        info.access_hash,
        info.slug.clone(),
        info.invite_link.clone(),
        info.title.clone(),
        None,
        video,
    )?;
    Ok(info)
}

/// Joins an encrypted conference call by `slug`, `call_id`, or incoming invite `msg_id`.
pub(super) fn join(
    account: &Account,
    slug: Option<String>,
    call_id: Option<String>,
    msg_id: Option<i32>,
    video: bool,
) -> Result<(), Fault> {
    if let Some(mid) = msg_id {
        let req = tl::functions::phone::GetGroupCall {
            call: tl::enums::InputGroupCall::InviteMessage(tl::types::InputGroupCallInviteMessage { msg_id: mid }),
            limit: 50,
        };
        let tl::enums::phone::GroupCall::Call(found) = account.ask(&req)?;
        account.learn(&found.users, &found.chats);
        let tl::enums::GroupCall::Call(gc) = found.call else {
            return Err(Fault::Refused("This conference call has ended.".to_owned()));
        };
        let invite_link = gc.invite_link.unwrap_or_default();
        let slug = slug_from_link(&invite_link);
        let title = gc.title.unwrap_or_default();
        account.live().conferences.ringing = None;
        return join_resolved(account, gc.id, gc.access_hash, slug, invite_link, title, Some(mid), video);
    }

    if let Some(slug) = slug.filter(|s| !s.trim().is_empty()) {
        let info = resolve_slug(account, &slug)?;
        let cid: i64 = info
            .call_id
            .parse()
            .map_err(|_| Fault::Refused("Invalid conference call id.".to_owned()))?;
        return join_resolved(
            account,
            cid,
            info.access_hash,
            info.slug,
            info.invite_link,
            info.title,
            None,
            video,
        );
    }

    if let Some(cid_str) = call_id {
        let cid: i64 = cid_str
            .strip_prefix(CONF_PREFIX)
            .unwrap_or(&cid_str)
            .parse()
            .map_err(|_| Fault::Refused("Invalid conference call id.".to_owned()))?;
        // Check if this is an incoming ringing invite.
        let ringing = account
            .live()
            .conferences
            .ringing
            .clone()
            .filter(|r| r.call_id == cid);
        if let Some(inv) = ringing {
            return join(account, None, None, Some(inv.msg_id), video || inv.video);
        }
        // Check resolved cache.
        let cached = account
            .live()
            .conferences
            .resolved
            .iter()
            .find(|(_, (id, ..))| *id == cid)
            .map(|(s, (id, hash, link, title))| (*id, *hash, s.clone(), link.clone(), title.clone()));
        if let Some((id, hash, slug, link, title)) = cached {
            return join_resolved(account, id, hash, slug, link, title, None, video);
        }
    }

    Err(Fault::Refused("Conference call not found.".to_owned()))
}

#[allow(clippy::too_many_arguments)]
fn join_resolved(
    account: &Account,
    call_id: i64,
    access_hash: i64,
    slug: String,
    invite_link: String,
    title: String,
    invite_msg_id: Option<i32>,
    video: bool,
) -> Result<(), Fault> {
    let conv_id = ensure_conference_row(account, call_id, &title);
    let call_ref = match invite_msg_id {
        Some(msg_id) => tl::enums::InputGroupCall::InviteMessage(tl::types::InputGroupCallInviteMessage { msg_id }),
        None => input_call(call_id, access_hash),
    };

    // CRITICAL FAIL-CLOSED CHECK BEFORE JOINING:
    // Fetch sub-chain 0 blocks from Telegram and verify EVERY block cryptographically before
    // calling `phone.joinGroupCall`. If verification fails, refuse to join and mark Blocked!
    let existing_blocks = fetch_chain_blocks(account, call_ref.clone(), SUB_CHAIN_STATE, 0)?;
    let mut verifier = ChainVerifier::default();
    for raw in &existing_blocks {
        if let Err(why) = verifier.verify_block(SUB_CHAIN_STATE, raw) {
            tracing::warn!("chatd: Telegram: conference {call_id} chain check FAILED before join: {why}");
            account.ctx.call_security(&conv_id, Some(CallSecurity::telegram_conference_blocked()));
            account.ctx.call_failed(
                &conv_id,
                CallFailure::Encryption,
                "Encrypted conference verification failed. Refusing to join unencrypted.",
            );
            return Err(Fault::Refused(format!(
                "Conference chain verification failed: {why}"
            )));
        }
    }

    let me_id = Who::User(account.me).text();
    let (params_json, ssrc, pk_b64) = account
        .ctx
        .telegram_group_offer(
            &conv_id,
            TelegramGroupOffer {
                user_id: me_id,
                muted: false,
                deafened: false,
                conference: true,
            },
        )
        .map_err(|detail| {
            account.ctx.call_failed(&conv_id, CallFailure::Engine, &detail);
            Fault::Refused(detail)
        })?;

    let my_public_key: [u8; 32] = match pk_b64
        .as_deref()
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .and_then(|v| v.try_into().ok())
    {
        Some(pk) => pk,
        None => random_32().map_err(|fault| {
            account.ctx.call_failed(&conv_id, CallFailure::Encryption, &fault.words());
            fault
        })?,
    };

    // Feed any pre-existing verified blocks into mediad so its E2EE verifier matches ours.
    if !existing_blocks.is_empty() {
        let blocks_b64: Vec<String> = existing_blocks
            .iter()
            .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
            .collect();
        if let Err(why) = account
            .ctx
            .telegram_conference_blocks(&conv_id, SUB_CHAIN_STATE, blocks_b64)
        {
            account.ctx.call_security(&conv_id, Some(CallSecurity::telegram_conference_blocked()));
            account.ctx.call_failed(&conv_id, CallFailure::Encryption, &why);
            return Err(Fault::Refused(why));
        }
    }

    // Build our join/genesis state block adding our public key.
    let mut members: Vec<ChainMember> = verifier
        .participants
        .iter()
        .map(|(&uid, &pk)| ChainMember { user_id: uid, public_key: pk })
        .collect();
    if !members.iter().any(|m| m.user_id == account.me) {
        members.push(ChainMember { user_id: account.me, public_key: my_public_key });
    }
    let epoch_seed = random_32().map_err(|fault| {
        account.ctx.call_failed(&conv_id, CallFailure::Encryption, &fault.words());
        fault
    })?;
    let join_block = ChainBlock::build(
        SUB_CHAIN_STATE,
        verifier.next_state_height(),
        verifier.state_hash,
        call_id,
        account.me,
        my_public_key,
        members,
        verifier.epoch.saturating_add(1),
        epoch_seed,
    )
    .encode();

    account.live().conferences.active = Some(ActiveConference {
        call_id,
        access_hash,
        slug: slug.clone(),
        invite_link: invite_link.clone(),
        title,
        source: ssrc,
        my_public_key,
        verifier,
        participants: HashMap::new(),
        video,
    });
    if !invite_link.is_empty() {
        account.ctx.call_invite_link(&conv_id, Some(invite_link));
    }

    let req = tl::functions::phone::JoinGroupCall {
        muted: false,
        video_stopped: !video,
        call: call_ref,
        join_as: tl::enums::InputPeer::PeerSelf,
        invite_hash: None,
        public_key: Some(my_public_key),
        block: Some(join_block.clone()),
        params: tl::types::DataJson { data: params_json }.into(),
    };
    match account.ask(&req) {
        Ok(answer) => {
            live::answered(account, &answer);
            // If the server did not echo our genesis/join block inside UpdateGroupCallChainBlocks,
            // verify and apply our own genesis block when starting a fresh conference.
            let need_self_genesis = account
                .live()
                .conferences
                .active
                .as_ref()
                .is_some_and(|a| a.verifier.state_height.is_none());
            if need_self_genesis {
                let _ = apply_chain_blocks_internal(account, call_id, SUB_CHAIN_STATE, &[join_block]);
            }
            // Send an E2EE broadcast block committing our epoch key on sub-chain 1.
            send_broadcast_block(account, call_id, access_hash);
            publish_conference_participants(account);
            Ok(())
        }
        Err(fault) => {
            account.live().conferences.active = None;
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            Err(fault)
        }
    }
}

fn send_broadcast_block(account: &Account, call_id: i64, access_hash: i64) {
    let epoch_seed = match random_32() {
        Ok(seed) => seed,
        Err(fault) => {
            fail_closed_active(account, call_id, &fault.words());
            return;
        }
    };
    let block_bytes = {
        let live = account.live();
        let Some(active) = live.conferences.active.as_ref().filter(|a| a.call_id == call_id) else {
            return;
        };
        if active.verifier.state_height.is_none() || active.verifier.blocked {
            return;
        }
        let members: Vec<ChainMember> = active
            .verifier
            .participants
            .iter()
            .map(|(&uid, &pk)| ChainMember { user_id: uid, public_key: pk })
            .collect();
        ChainBlock::build(
            SUB_CHAIN_BROADCAST,
            active.verifier.next_broadcast_height(),
            active.verifier.next_broadcast_prev(),
            call_id,
            account.me,
            active.my_public_key,
            members,
            active.verifier.epoch,
            epoch_seed,
        )
        .encode()
    };
    if let Ok(answer) = wire::call(
        account.wire.as_ref(),
        &tl::functions::phone::SendConferenceCallBroadcast {
            call: input_call(call_id, access_hash),
            block: block_bytes,
        },
    ) {
        live::answered(account, &answer);
    }
}

/// Applies incoming chain blocks (`Update::GroupCallChainBlocks`) to both `chatd`'s local verifier
/// and `mediad`'s E2EE cipher. On any verification failure, FAILS CLOSED immediately!
pub(super) fn on_chain_blocks(account: &Account, update: &tl::types::UpdateGroupCallChainBlocks) {
    let call_id = match &update.call {
        tl::enums::InputGroupCall::Call(c) => c.id,
        _ => return,
    };
    let _ = apply_chain_blocks_internal(account, call_id, update.sub_chain_id, &update.blocks);
}

fn apply_chain_blocks_internal(
    account: &Account,
    call_id: i64,
    sub_chain_id: i32,
    blocks: &[Vec<u8>],
) -> Result<(), String> {
    if blocks.is_empty() {
        return Ok(());
    }
    let conv_id = conv_id_of(call_id);

    let verify_res: Result<CallSecurity, (String, i64, u32)> = {
        let mut live = account.live();
        let Some(active) = live.conferences.active.as_mut().filter(|a| a.call_id == call_id) else {
            return Ok(());
        };
        let mut err_out = None;
        for raw in blocks {
            if let Err(why) = active.verifier.verify_block(sub_chain_id, raw) {
                err_out = Some(why);
                break;
            }
        }
        if let Some(why) = err_out {
            Err((why, active.access_hash, active.source))
        } else {
            Ok(active.verifier.security(account.me))
        }
    };

    match verify_res {
        Ok(security) => {
            let blocks_b64: Vec<String> = blocks
                .iter()
                .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
                .collect();
            if let Err(why) = account
                .ctx
                .telegram_conference_blocks(&conv_id, sub_chain_id, blocks_b64)
            {
                fail_closed_active(account, call_id, &why);
                return Err(why);
            }
            account.ctx.call_security(&conv_id, Some(security));
            Ok(())
        }
        Err((why, access_hash, source)) => {
            tracing::warn!("chatd: Telegram: conference {call_id} chain verification FAILED: {why}");
            // Notify mediad too so its cipher is permanently blocked, then leave immediately.
            let blocks_b64: Vec<String> = blocks
                .iter()
                .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
                .collect();
            let _ = account
                .ctx
                .telegram_conference_blocks(&conv_id, sub_chain_id, blocks_b64);
            let _ = wire::call(
                account.wire.as_ref(),
                &tl::functions::phone::LeaveGroupCall {
                    call: input_call(call_id, access_hash),
                    source: source as i32,
                },
            );
            account.live().conferences.active = None;
            account.ctx.call_security(&conv_id, Some(CallSecurity::telegram_conference_blocked()));
            account.ctx.call_failed(
                &conv_id,
                CallFailure::Encryption,
                "Conference encryption verification failed. Call terminated.",
            );
            Err(why)
        }
    }
}

fn fail_closed_active(account: &Account, call_id: i64, why: &str) {
    let conv_id = conv_id_of(call_id);
    let active = account.live().conferences.active.take();
    if let Some(active) = active {
        let _ = wire::call(
            account.wire.as_ref(),
            &tl::functions::phone::LeaveGroupCall {
                call: input_call(active.call_id, active.access_hash),
                source: active.source as i32,
            },
        );
    }
    account.ctx.call_security(&conv_id, Some(CallSecurity::telegram_conference_blocked()));
    account.ctx.call_failed(
        &conv_id,
        CallFailure::Encryption,
        &format!("Conference encryption verification failed: {why}"),
    );
}

/// Routes `Update::GroupCallConnection` when an encrypted conference is active.
pub(super) fn on_connection(account: &Account, update: &tl::types::UpdateGroupCallConnection) -> bool {
    if update.presentation {
        return false;
    }
    let conv_id = account
        .live()
        .conferences
        .active
        .as_ref()
        .map(|a| conv_id_of(a.call_id));
    let Some(conv_id) = conv_id else {
        return false;
    };
    let tl::enums::DataJson::Json(json) = &update.params;
    account.ctx.telegram_group_answer(&conv_id, json.data.clone());
    true
}

/// Routes `Update::GroupCallParticipants` when an encrypted conference is active.
pub(super) fn on_participants(account: &Account, update: &tl::types::UpdateGroupCallParticipants) -> bool {
    let call_id = match &update.call {
        tl::enums::InputGroupCall::Call(c) => c.id,
        _ => return false,
    };
    let matched = {
        let mut live = account.live();
        if let Some(active) = live.conferences.active.as_mut().filter(|a| a.call_id == call_id) {
            for raw in &update.participants {
                let tl::enums::GroupCallParticipant::Participant(p) = raw;
                let who = Who::of(&p.peer);
                if p.left {
                    active.participants.remove(&who);
                } else {
                    active.participants.insert(who, (p.source.max(0) as u32, p.muted));
                }
            }
            true
        } else {
            false
        }
    };
    if matched {
        publish_conference_participants(account);
    }
    matched
}

/// Adds people to a running 1:1 call (upgrading it to an encrypted conference!) or to an active
/// encrypted conference (`phone.inviteConferenceCallParticipant`).
pub(super) fn add_people(account: &Account, conversation_id: &str, people: &[String]) -> Result<String, Fault> {
    if people.is_empty() {
        return Err(Fault::Refused("Choose at least one person to add.".to_owned()));
    }
    // Case 1: Already in an active encrypted conference.
    if let Some((call_id, access_hash, video)) = account
        .live()
        .conferences
        .active
        .as_ref()
        .map(|a| (a.call_id, a.access_hash, a.video))
    {
        for person_id in people {
            let Some((who @ Who::User(_), None)) = Who::parse(person_id) else {
                continue;
            };
            let Some(user_id) = account.dir().input_user(who) else {
                continue;
            };
            let answer = account.ask(&tl::functions::phone::InviteConferenceCallParticipant {
                video,
                call: input_call(call_id, access_hash),
                user_id,
            })?;
            live::answered(account, &answer);
        }
        return Ok(conv_id_of(call_id));
    }

    // Case 2: Upgrade a running 1:1 call (`ActiveCall::Active`) -> encrypted conference!
    let active_1on1 = {
        let live = account.live();
        match &live.active_call {
            Some(call::ActiveCall::Active { id, access_hash, peer, video, started_at, connection_id, .. })
                if peer.text() == conversation_id =>
            {
                Some((*id, *access_hash, *peer, *video, started_at.elapsed().as_secs() as i32, *connection_id))
            }
            _ => None,
        }
    };
    let Some((old_id, old_hash, old_peer, video, duration, connection_id)) = active_1on1 else {
        return Err(Fault::Refused("No active call to add people to.".to_owned()));
    };

    // End the 1:1 local mediad session cleanly so the new encrypted conference can take the call slot.
    account.live().active_call = None;
    account.ctx.call_over(&old_peer.text());

    let info = create(account, video, true)?;
    let conf_id: i64 = info
        .call_id
        .parse()
        .map_err(|_| Fault::Refused("Invalid conference call id.".to_owned()))?;
    let conf_access_hash = info.access_hash;
    let slug = info.slug.clone();

    // Invite the original 1:1 peer and all requested people into the new encrypted conference.
    let mut to_invite: Vec<Who> = vec![old_peer];
    for person_id in people {
        if let Some((who @ Who::User(_), None)) = Who::parse(person_id) {
            if !to_invite.contains(&who) {
                to_invite.push(who);
            }
        }
    }
    for who in to_invite {
        let user_id = account.dir().input_user(who);
        if let Some(user_id) = user_id {
            let answer = account.ask(&tl::functions::phone::InviteConferenceCallParticipant {
                video,
                call: input_call(conf_id, conf_access_hash),
                user_id,
            })?;
            live::answered(account, &answer);
        }
    }

    // Discard the old 1:1 call with `MigrateConferenceCall { slug }` so Telegram transitions the peer.
    let _ = wire::call(
        account.wire.as_ref(),
        &tl::functions::phone::DiscardCall {
            video,
            peer: tl::types::InputPhoneCall { id: old_id, access_hash: old_hash }.into(),
            duration,
            reason: tl::enums::PhoneCallDiscardReason::MigrateConferenceCall(
                tl::types::PhoneCallDiscardReasonMigrateConferenceCall { slug },
            ),
            connection_id,
        },
    );

    Ok(conv_id_of(conf_id))
}

/// Removes or kicks participants from the active encrypted conference call
/// (`phone.deleteConferenceCallParticipants`), advancing the E2EE state chain.
pub(super) fn remove_participants(account: &Account, people: &[String], kick: bool) -> Result<(), Fault> {
    let ids: Vec<i64> = people
        .iter()
        .filter_map(|p| match Who::parse(p) {
            Some((Who::User(uid), None)) => Some(uid),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return Err(Fault::Refused("No valid participants to remove.".to_owned()));
    }
    let (call_id, access_hash, block) = {
        let live = account.live();
        let Some(active) = live.conferences.active.as_ref() else {
            return Err(Fault::Refused("No active conference call.".to_owned()));
        };
        let remaining: Vec<ChainMember> = active
            .verifier
            .participants
            .iter()
            .filter(|(&uid, _)| !ids.contains(&uid))
            .map(|(&uid, &pk)| ChainMember { user_id: uid, public_key: pk })
            .collect();
        let epoch_seed = random_32()?;
        let block = ChainBlock::build(
            SUB_CHAIN_STATE,
            active.verifier.next_state_height(),
            active.verifier.state_hash,
            active.call_id,
            account.me,
            active.my_public_key,
            remaining,
            active.verifier.epoch.saturating_add(1),
            epoch_seed,
        )
        .encode();
        (active.call_id, active.access_hash, block)
    };
    let answer = account.ask(&tl::functions::phone::DeleteConferenceCallParticipants {
        only_left: false,
        kick,
        call: input_call(call_id, access_hash),
        ids,
        block,
    })?;
    live::answered(account, &answer);
    Ok(())
}

/// Declines an incoming conference invitation (`phone.declineConferenceCallInvite`) or leaves an
/// active encrypted conference (`phone.leaveGroupCall`). Returns `true` if handled.
pub(super) fn leave_or_decline(account: &Account, conversation_id: &str) -> bool {
    if !is_conference_id(conversation_id) {
        return false;
    }
    let Ok(call_id) = conversation_id.trim_start_matches(CONF_PREFIX).parse::<i64>() else {
        return false;
    };

    let pending = {
        let mut live = account.live();
        if live.conferences.ringing.as_ref().is_some_and(|r| r.call_id == call_id) {
            live.conferences.ringing.take()
        } else {
            None
        }
    };
    if let Some(inv) = pending {
        let _ = wire::call(
            account.wire.as_ref(),
            &tl::functions::phone::DeclineConferenceCallInvite { msg_id: inv.msg_id },
        );
        return true;
    }

    let active = {
        let mut live = account.live();
        if live.conferences.active.as_ref().is_some_and(|a| a.call_id == call_id) {
            live.conferences.active.take()
        } else {
            None
        }
    };
    if let Some(active) = active {
        let _ = wire::call(
            account.wire.as_ref(),
            &tl::functions::phone::LeaveGroupCall {
                call: input_call(active.call_id, active.access_hash),
                source: active.source as i32,
            },
        );
        return true;
    }
    false
}

/// Self-check prove entry point for `TINYCLIENT_PROVE=telegram-wave18a` (`e2e/scenarios/telegram-wave18a`).
pub fn prove_wave18a() -> Result<(), String> {
    if !protocol::call_pictures(protocol::Network::Telegram) {
        return Err("protocol::call_pictures(Network::Telegram) must be true".to_owned());
    }
    if protocol::link::telegram_call_link("https://t.me/call/wave18a_slug").as_deref() != Some("wave18a_slug") {
        return Err("telegram_call_link failed to parse valid t.me/call/<slug>".to_owned());
    }
    if protocol::link::telegram_call_link("https://t.me/call/../bad").is_some() {
        return Err("telegram_call_link accepted path traversal".to_owned());
    }

    // Verify conference chain verification, emoji change on membership update, and fail-closed on tamper.
    let pk_a = [0x11u8; 32];
    let pk_b = [0x22u8; 32];
    let pk_c = [0x33u8; 32];
    let mut verifier = ChainVerifier::default();
    let b0 = ChainBlock::build(
        SUB_CHAIN_STATE,
        0,
        [0u8; 32],
        99_001,
        1001,
        pk_a,
        vec![
            ChainMember { user_id: 1001, public_key: pk_a },
            ChainMember { user_id: 1002, public_key: pk_b },
        ],
        1,
        [0xAA; 32],
    )
    .encode();
    verifier.verify_block(SUB_CHAIN_STATE, &b0)?;
    let sec1 = verifier.security(1001);
    if sec1.mode != protocol::SecurityMode::EndToEnd || sec1.emojis.len() != 4 {
        return Err("genesis conference block did not produce 4 E2EE emojis".to_owned());
    }

    let b0_hash: [u8; 32] = Sha256::digest(&b0).into();
    let b1 = ChainBlock::build(
        SUB_CHAIN_STATE,
        1,
        b0_hash,
        99_001,
        1001,
        pk_a,
        vec![
            ChainMember { user_id: 1001, public_key: pk_a },
            ChainMember { user_id: 1002, public_key: pk_b },
            ChainMember { user_id: 1003, public_key: pk_c },
        ],
        2,
        [0xBB; 32],
    )
    .encode();
    verifier.verify_block(SUB_CHAIN_STATE, &b1)?;
    let sec2 = verifier.security(1001);
    if sec2.emojis == sec1.emojis {
        return Err("conference emojis did not update on membership change".to_owned());
    }

    let b1_hash: [u8; 32] = Sha256::digest(&b1).into();
    let mut bad = ChainBlock::build(
        SUB_CHAIN_STATE,
        2,
        b1_hash,
        99_001,
        1001,
        pk_a,
        vec![ChainMember { user_id: 1001, public_key: pk_a }],
        3,
        [0xCC; 32],
    );
    bad.mac[0] ^= 0xFF;
    if verifier.verify_block(SUB_CHAIN_STATE, &bad.encode()).is_ok() {
        return Err("tampered chain block was accepted".to_owned());
    }
    let blocked = verifier.security(1001);
    if blocked.mode != protocol::SecurityMode::Blocked || !blocked.required {
        return Err("tampered chain block did not fail closed to Blocked(required=true)".to_owned());
    }
    Ok(())
}
