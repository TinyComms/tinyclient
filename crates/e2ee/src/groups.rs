//! Group state: creating a group, taking the server's external proposals, committing, applying
//! commits and Welcomes. Every rule that stops the server from adding a ghost device, or this
//! device from following a rewritten history, is here.

use crate::engine::{capabilities, Config, ConvMeta, Engine, CIPHERSUITE};
use crate::error::{Error, Result};
use crate::identity::SIGNATURE_KEY_LEN;
use crate::ids::{parse_credential, ConversationId, DeviceRef};
use crate::wire::{guard, read_vl, sha256, Bytes};
use openmls::prelude as mls;
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls_traits::storage::StorageProvider as _;
use openmls_traits::OpenMlsProvider as _;
use p256::ecdsa::VerifyingKey;
use serde::{Deserialize, Serialize};

/// The credential the server's key carries in the group's external senders extension when it is
/// given as a bare public key.
pub const SERVER_CREDENTIAL: &[u8] = b"tinyserver";
/// The most a handshake message (proposals, commit, Welcome) may be.
pub const MAX_HANDSHAKE_BYTES: usize = 8 * 1024 * 1024;
/// The sizes the server accepts (`docs/e2ee-text.md` and its `e2ee_text` code), so that a commit it
/// would refuse is refused here, in words, and not left in flight.
pub mod limits {
    /// A commit.
    pub const COMMIT: usize = 256 * 1024;
    /// One Welcome.
    pub const WELCOME: usize = 1024 * 1024;
    /// All the Welcomes of one commit together (each device has its own copy of the bytes).
    pub const WELCOMES: usize = 4 * 1024 * 1024;
    /// Welcomes in one commit: devices a single commit may add.
    pub const WELCOMES_PER_COMMIT: usize = 100;
    /// A key package.
    pub const KEY_PACKAGE: usize = 8 * 1024;
    /// An application ciphertext.
    pub const CIPHERTEXT: usize = 20 * 1024;
}

fn check_limits(commit: &[u8], welcomes: &[WelcomeFor]) -> Result<()> {
    if commit.len() > limits::COMMIT {
        return Err(Error::TooLarge("commit"));
    }
    if welcomes.len() > limits::WELCOMES_PER_COMMIT {
        return Err(Error::TooLarge("welcomes in one commit"));
    }
    let total: usize = welcomes.iter().map(|welcome| welcome.data.len()).sum();
    if welcomes
        .iter()
        .any(|welcome| welcome.data.len() > limits::WELCOME)
        || total > limits::WELCOMES
    {
        return Err(Error::TooLarge("welcomes"));
    }
    Ok(())
}

const MAX_PROPOSALS: usize = 4096;
/// Leaves the group gives itself, as far as OpenMLS lets a leaf's lifetime run.
const LEAF_LIFETIME: u64 = 60 * 60 * 24 * 84;

/// One thing a proposals message asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalKind {
    Add(DeviceRef),
    Remove(DeviceRef),
}

/// Why a proposal was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// An Add for a device with no trusted, cross-signed record here yet. Fetch the person's
    /// device list, [`Engine::verify_devices`], and feed the same message again.
    UnknownDevice,
    /// An Add whose key is not the one trusted for that device: a forged device.
    KeyMismatch,
    /// An Add for a device that is already in the group.
    AlreadyMember,
    /// A Remove naming a leaf that is not in the roster.
    NotInRoster,
    /// Anything but an Add or a Remove.
    NotAllowed,
    /// Not signed by the pinned server key as an external sender, or not for this group, or
    /// malformed.
    Invalid,
    /// For an epoch this group is past.
    StaleEpoch,
    /// The proposal store is full.
    TooMany,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub kind: Option<ProposalKind>,
    pub reason: RejectReason,
}

/// What [`Engine::handle_proposals`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposalOutcome {
    /// Newly pending: commit them.
    pub accepted: Vec<ProposalKind>,
    /// Already pending from an earlier message.
    pub already: usize,
    pub rejected: Vec<Rejected>,
    /// A Remove names this device. It cannot commit it; someone else will.
    pub removes_self: bool,
}

/// Proposals waiting to be committed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub adds: Vec<DeviceRef>,
    pub removes: Vec<DeviceRef>,
}

/// A Welcome for a device that the commit adds. Every added device gets the same bytes: one
/// Welcome holds the secrets of all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeFor {
    pub device: DeviceRef,
    pub data: Vec<u8>,
}

/// A commit made and waiting for the server's answer: `POST /v1/e2ee/conversations/{id}/commit`
/// with `epoch`, `commit` and `welcomes`. Then [`Engine::confirm_commit`] on `204`, or
/// [`Engine::abort_commit`] on `409 stale_epoch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBundle {
    /// The epoch the commit applies to (the group's epoch now).
    pub epoch: u64,
    /// A MLS message, public (`PublicMessage`), TLS encoded.
    pub commit: Vec<u8>,
    /// A TLS `MLSMessage` holding the `Welcome`, one entry per added device (all the same bytes:
    /// one Welcome holds the secrets of every device the commit adds); empty if nobody is added.
    pub welcomes: Vec<WelcomeFor>,
    pub added: Vec<DeviceRef>,
    pub removed: Vec<DeviceRef>,
}

/// A [`CommitBundle`] as the state keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredBundle {
    epoch: u64,
    commit: Bytes,
    welcome: Option<Bytes>,
    added: Vec<StoredDevice>,
    removed: Vec<StoredDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDevice {
    user: crate::ids::UserId,
    device: crate::ids::DeviceId,
    key: Bytes,
}

impl StoredDevice {
    fn from_ref(device: &DeviceRef) -> StoredDevice {
        StoredDevice {
            user: device.user,
            device: device.device,
            key: Bytes(device.signature_key.clone()),
        }
    }

    fn to_ref(&self) -> DeviceRef {
        DeviceRef {
            user: self.user,
            device: self.device,
            signature_key: self.key.0.clone(),
        }
    }
}

impl StoredBundle {
    fn from_bundle(bundle: &CommitBundle) -> StoredBundle {
        StoredBundle {
            epoch: bundle.epoch,
            commit: Bytes(bundle.commit.clone()),
            welcome: bundle
                .welcomes
                .first()
                .map(|welcome| Bytes(welcome.data.clone())),
            added: bundle.added.iter().map(StoredDevice::from_ref).collect(),
            removed: bundle.removed.iter().map(StoredDevice::from_ref).collect(),
        }
    }

    fn to_bundle(&self) -> CommitBundle {
        let added: Vec<DeviceRef> = self.added.iter().map(StoredDevice::to_ref).collect();
        let welcomes = match &self.welcome {
            Some(data) => added
                .iter()
                .map(|device| WelcomeFor {
                    device: device.clone(),
                    data: data.0.clone(),
                })
                .collect(),
            None => Vec::new(),
        };
        CommitBundle {
            epoch: self.epoch,
            commit: self.commit.0.clone(),
            welcomes,
            added,
            removed: self.removed.iter().map(StoredDevice::to_ref).collect(),
        }
    }
}

/// What applying a commit (ours, once confirmed, or another's) did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitOutcome {
    /// The group's epoch afterwards.
    pub epoch: u64,
    pub added: Vec<DeviceRef>,
    pub removed: Vec<DeviceRef>,
    /// This device was removed: its group state is deleted.
    pub removed_self: bool,
    /// The commit had been applied already; nothing changed.
    pub duplicate: bool,
}

/// What joining from a Welcome did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinOutcome {
    pub epoch: u64,
    pub members: Vec<DeviceRef>,
    /// The server key was pinned by this Welcome (first sight of the conversation), not checked
    /// against an earlier pin: show it for verification if it matters.
    pub pinned_now: bool,
}

pub(crate) fn parse_server_key(bytes: &[u8]) -> Result<(Vec<u8>, mls::ExternalSender)> {
    let (key, sender) = if bytes.len() == SIGNATURE_KEY_LEN && bytes[0] == 0x04 {
        let credential =
            mls::Credential::from(mls::BasicCredential::new(SERVER_CREDENTIAL.to_vec()));
        (
            bytes.to_vec(),
            mls::ExternalSender::new(mls::SignaturePublicKey::from(bytes.to_vec()), credential),
        )
    } else {
        let sender = guard(|| mls::ExternalSender::tls_deserialize_exact(bytes))?
            .map_err(|_| Error::Malformed("external sender"))?;
        (external_sender_key(&sender)?, sender)
    };
    if key.len() != SIGNATURE_KEY_LEN
        || key[0] != 0x04
        || VerifyingKey::from_sec1_bytes(&key).is_err()
    {
        return Err(Error::Malformed("external sender key"));
    }
    Ok((key, sender))
}

/// The signature key inside a TLS `ExternalSender` (a variable-length key, then a credential).
fn external_sender_key(sender: &mls::ExternalSender) -> Result<Vec<u8>> {
    let bytes = sender.tls_serialize_detached().map_err(Error::mls)?;
    let (key, _rest) = read_vl(&bytes).ok_or(Error::Malformed("external sender"))?;
    Ok(key.to_vec())
}

fn group_server_key(group_extensions: &mls::Extensions<mls::GroupContext>) -> Result<Vec<u8>> {
    let senders = group_extensions
        .external_senders()
        .ok_or(Error::Malformed("group without an external sender"))?;
    match senders.as_slice() {
        [only] => external_sender_key(only),
        _ => Err(Error::Malformed("group with more than one external sender")),
    }
}

fn device_of_credential(credential: &mls::Credential, signature_key: &[u8]) -> Result<DeviceRef> {
    if credential.credential_type() != mls::CredentialType::Basic {
        return Err(Error::Malformed("credential type"));
    }
    let (user, device) = parse_credential(credential.serialized_content())?;
    Ok(DeviceRef {
        user,
        device,
        signature_key: signature_key.to_vec(),
    })
}

fn device_of_member(member: &mls::Member) -> Result<DeviceRef> {
    // A basic credential is TLS encoded as a variable-length vector inside the Credential: the
    // `serialized_content` is the identity bytes alone.
    device_of_credential(&member.credential, &member.signature_key)
}

fn device_of_key_package(package: &mls::KeyPackage) -> Result<DeviceRef> {
    let leaf = package.leaf_node();
    device_of_credential(leaf.credential(), leaf.signature_key().as_slice())
}

/// Every proposals form the server may send: a TLS vector of proposal messages (as for calls);
/// that vector after a one byte operation (DAVE's opcode 27 payload); or one bare message.
fn parse_proposal_messages(data: &[u8]) -> Result<Vec<mls::MlsMessageIn>> {
    if data.len() > MAX_HANDSHAKE_BYTES {
        return Err(Error::TooLarge("proposals"));
    }
    let messages = guard(|| {
        Vec::<mls::MlsMessageIn>::tls_deserialize_exact(data)
            .ok()
            .or_else(|| match data.split_first() {
                Some((0 | 1, rest)) => Vec::<mls::MlsMessageIn>::tls_deserialize_exact(rest).ok(),
                _ => None,
            })
            .or_else(|| {
                mls::MlsMessageIn::tls_deserialize_exact(data)
                    .ok()
                    .map(|message| vec![message])
            })
    })?
    .ok_or(Error::Malformed("proposals"))?;
    if messages.len() > MAX_PROPOSALS {
        return Err(Error::TooLarge("proposals"));
    }
    Ok(messages)
}

fn create_config(
    config: &Config,
    extensions: mls::Extensions<mls::GroupContext>,
) -> mls::MlsGroupCreateConfig {
    mls::MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .wire_format_policy(mls::PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .max_past_epochs(config.max_past_epochs)
        .padding_size(config.padding)
        .sender_ratchet_configuration(mls::SenderRatchetConfiguration::new(
            config.out_of_order_tolerance,
            config.maximum_forward_distance,
        ))
        .lifetime(mls::Lifetime::new(LEAF_LIFETIME))
        .capabilities(capabilities())
        .with_group_context_extensions(extensions)
        .build()
}

fn join_config(config: &Config) -> mls::MlsGroupJoinConfig {
    mls::MlsGroupJoinConfig::builder()
        .wire_format_policy(mls::PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .max_past_epochs(config.max_past_epochs)
        .padding_size(config.padding)
        .sender_ratchet_configuration(mls::SenderRatchetConfiguration::new(
            config.out_of_order_tolerance,
            config.maximum_forward_distance,
        ))
        .build()
}

fn group_id(conv: ConversationId) -> mls::GroupId {
    mls::GroupId::from_slice(&conv.0)
}

impl Engine {
    pub(crate) fn meta_mut(&mut self, conv: ConversationId) -> Result<&mut ConvMeta> {
        self.meta.get_mut(&conv).ok_or(Error::NoGroup)
    }

    /// Whether this conversation can be used at all right now.
    pub(crate) fn usable(&self, conv: ConversationId) -> Result<()> {
        let meta = self.meta.get(&conv).ok_or(Error::NoGroup)?;
        if meta.removed {
            return Err(Error::NotAMember);
        }
        if meta.frozen.is_some() {
            return Err(Error::Frozen("epoch went backwards"));
        }
        if meta.server_key_alert {
            return Err(Error::ServerKeyChanged);
        }
        Ok(())
    }

    pub(crate) fn load_group(&mut self, conv: ConversationId) -> Result<()> {
        if self.groups.contains_key(&conv) {
            return Ok(());
        }
        let group = mls::MlsGroup::load(self.provider.storage(), &group_id(conv))
            .map_err(Error::mls)?
            .ok_or(Error::NoGroup)?;
        self.groups.insert(conv, group);
        Ok(())
    }

    fn freeze(&mut self, conv: ConversationId, why: &str) {
        if let Some(meta) = self.meta.get_mut(&conv) {
            meta.frozen = Some(why.to_owned());
        }
        let _ = self.persist(false);
    }

    /// True if there is a usable group for the conversation.
    pub fn has_group(&self, conv: ConversationId) -> bool {
        self.meta.get(&conv).is_some_and(|meta| !meta.removed)
    }

    /// The group's epoch.
    pub fn epoch(&mut self, conv: ConversationId) -> Result<u64> {
        self.load_group(conv)?;
        Ok(self
            .groups
            .get(&conv)
            .ok_or(Error::NoGroup)?
            .epoch()
            .as_u64())
    }

    /// Why the conversation's group is frozen, if it is. A frozen group does nothing further: it
    /// is rebuilt (a new group, a new Welcome), and what was said in it is not carried over.
    pub fn frozen(&self, conv: ConversationId) -> Option<&str> {
        self.meta.get(&conv).and_then(|meta| meta.frozen.as_deref())
    }

    /// The pinned server external sender key (65 bytes), if the conversation is known.
    pub fn server_key(&self, conv: ConversationId) -> Option<Vec<u8>> {
        self.meta.get(&conv).map(|meta| meta.server_key.0.clone())
    }

    /// Starts the group of a conversation at epoch 0 with this device as its only member, and pins
    /// the server's external sender key (the `external_sender` of `GET /v1/e2ee/conversations/{id}`:
    /// a bare 65 byte key or a TLS `ExternalSender`). Do this only when the server says this device
    /// starts the group (`POST /v1/e2ee/conversations/{id}/group`).
    pub fn create_group(&mut self, conv: ConversationId, server_key: &[u8]) -> Result<()> {
        if self.has_group(conv) {
            return Err(Error::GroupExists);
        }
        let (key, sender) = parse_server_key(server_key)?;
        let extensions = mls::Extensions::single(mls::Extension::ExternalSenders(vec![sender]))
            .map_err(Error::mls)?;
        let config = create_config(&self.config, extensions);
        // A group of ours from before (we left it) is replaced.
        self.groups.remove(&conv);
        let group = mls::MlsGroup::new_with_group_id(
            &self.provider,
            &self.identity.key,
            &config,
            group_id(conv),
            self.credential(),
        )
        .map_err(Error::mls)?;
        self.groups.insert(conv, group);
        self.meta.insert(
            conv,
            ConvMeta {
                server_key: key.into(),
                ..ConvMeta::default()
            },
        );
        self.persist(true)
    }

    /// Forgets a conversation entirely: its group, its keys and its pin.
    pub fn forget_conversation(&mut self, conv: ConversationId) -> Result<()> {
        if let Some(mut group) = self.groups.remove(&conv) {
            group.delete(self.provider.storage()).map_err(Error::mls)?;
        } else if let Some(mut group) =
            mls::MlsGroup::load(self.provider.storage(), &group_id(conv)).map_err(Error::mls)?
        {
            group.delete(self.provider.storage()).map_err(Error::mls)?;
        }
        self.meta.remove(&conv);
        self.persist(true)
    }

    /// The members' devices, as the group's tree has them.
    pub fn roster(&mut self, conv: ConversationId) -> Result<Vec<DeviceRef>> {
        self.load_group(conv)?;
        let group = self.groups.get(&conv).ok_or(Error::NoGroup)?;
        group
            .members()
            .map(|member| device_of_member(&member))
            .collect()
    }

    /// What the server says the epoch is now: an epoch below ours means the server's database went
    /// back in time, and the group is frozen.
    pub fn observe_server_epoch(&mut self, conv: ConversationId, shown: u64) -> Result<()> {
        let local =
            self.meta
                .get(&conv)
                .ok_or(Error::NoGroup)?
                .high_epoch
                .max(if self.has_group(conv) {
                    self.epoch(conv)?
                } else {
                    0
                });
        if shown < local {
            self.freeze(conv, "epoch went backwards");
            return Err(Error::EpochBackwards { local, shown });
        }
        Ok(())
    }

    /// What the server now says its external sender key is (`GET /v1/e2ee/conversations/{id}`).
    /// A key other than the pinned one is [`Error::ServerKeyChanged`] and blocks the conversation
    /// until [`Engine::accept_server_key`].
    pub fn observe_server_key(&mut self, conv: ConversationId, shown: &[u8]) -> Result<()> {
        let (key, _) = parse_server_key(shown)?;
        let meta = self.meta.get_mut(&conv).ok_or(Error::NoGroup)?;
        if meta.server_key.0 != key {
            meta.server_key_alert = true;
            let _ = self.persist(false);
            return Err(Error::ServerKeyChanged);
        }
        Ok(())
    }

    /// After the person has been told the server's key changed and accepts it. The group's own
    /// copy of the key, in its context, changes only with a commit; this re-pins what is shown.
    pub fn accept_server_key(&mut self, conv: ConversationId, new_key: &[u8]) -> Result<()> {
        let (key, _) = parse_server_key(new_key)?;
        let meta = self.meta_mut(conv)?;
        meta.server_key = key.into();
        meta.server_key_alert = false;
        self.persist(true)
    }

    /// Proposals waiting to be committed.
    pub fn pending(&mut self, conv: ConversationId) -> Result<Pending> {
        self.load_group(conv)?;
        let group = self.groups.get(&conv).ok_or(Error::NoGroup)?;
        let mut pending = Pending::default();
        for queued in group.pending_proposals() {
            match queued.proposal() {
                mls::Proposal::Add(add) => {
                    pending.adds.push(device_of_key_package(add.key_package())?)
                }
                mls::Proposal::Remove(remove) => {
                    if let Some(member) = group.member_at(remove.removed()) {
                        pending.removes.push(device_of_member(&member)?);
                    }
                }
                _ => {}
            }
        }
        Ok(pending)
    }

    /// Takes a `proposals` item of the handshake feed.
    ///
    /// Each proposal must be signed by the pinned server key as an external sender, for this group
    /// and this epoch, and be one of two things, or it is refused: an Add whose key package's
    /// credential is `user_id || device_id` of a device this client trusts (cross-signed, see
    /// [`Engine::verify_devices`]) with exactly the trusted signing key, or a Remove of a leaf in
    /// the roster. Accepted proposals are pending until [`Engine::commit_pending`].
    pub fn handle_proposals(
        &mut self,
        conv: ConversationId,
        data: &[u8],
    ) -> Result<ProposalOutcome> {
        self.usable(conv)?;
        self.load_group(conv)?;
        let messages = parse_proposal_messages(data)?;
        let mut outcome = ProposalOutcome::default();
        let Engine {
            groups,
            provider,
            trust,
            meta,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        let own_leaf = group.own_leaf_index();
        let local_epoch = group.epoch().as_u64();
        let mut changed = false;
        let mut discard = false;
        for message in messages {
            let reject = |outcome: &mut ProposalOutcome,
                          kind: Option<ProposalKind>,
                          reason: RejectReason| {
                outcome.rejected.push(Rejected { kind, reason })
            };
            let Ok(protocol) = message.try_into_protocol_message() else {
                reject(&mut outcome, None, RejectReason::Invalid);
                continue;
            };
            if protocol.group_id().as_slice() != conv.0
                || protocol.wire_format() != mls::WireFormat::PublicMessage
                || protocol.content_type() != mls::ContentType::Proposal
            {
                reject(&mut outcome, None, RejectReason::Invalid);
                continue;
            }
            let epoch = protocol.epoch().as_u64();
            if epoch < local_epoch {
                reject(&mut outcome, None, RejectReason::StaleEpoch);
                continue;
            }
            if epoch > local_epoch {
                return Err(Error::FutureEpoch {
                    message: epoch,
                    local: local_epoch,
                });
            }
            let processed = match guard(|| group.process_message(&*provider, protocol)) {
                Ok(Ok(processed)) => processed,
                Ok(Err(_)) => {
                    reject(&mut outcome, None, RejectReason::Invalid);
                    continue;
                }
                Err(_) => {
                    // A panic inside the library: the group may be half changed in memory.
                    reject(&mut outcome, None, RejectReason::Invalid);
                    discard = true;
                    break;
                }
            };
            if !matches!(processed.sender(), mls::Sender::External(_)) {
                reject(&mut outcome, None, RejectReason::Invalid);
                continue;
            }
            let mls::ProcessedMessageContent::ProposalMessage(queued) = processed.into_content()
            else {
                reject(&mut outcome, None, RejectReason::Invalid);
                continue;
            };
            let (kind, verdict) = vet_proposal(group, trust, &queued);
            if let Err(reason) = verdict {
                reject(&mut outcome, kind, reason);
                continue;
            }
            let Some(kind) = kind else { continue };
            if let mls::Proposal::Remove(remove) = queued.proposal() {
                if remove.removed() == own_leaf {
                    if let Some(entry) = meta.get_mut(&conv) {
                        entry.self_removal_pending = true;
                    }
                    outcome.removes_self = true;
                    changed = true;
                    // Stored all the same: the commit that evicts this device refers to it.
                }
            }
            if group
                .pending_proposals()
                .any(|pending| pending.proposal_reference_ref() == queued.proposal_reference_ref())
            {
                outcome.already += 1;
                continue;
            }
            if group.pending_proposals().count() >= MAX_PROPOSALS {
                reject(&mut outcome, Some(kind), RejectReason::TooMany);
                continue;
            }
            group
                .store_pending_proposal(provider.storage(), *queued)
                .map_err(Error::mls)?;
            outcome.accepted.push(kind);
            changed = true;
        }
        if discard {
            groups.remove(&conv);
        }
        if changed {
            self.persist(false)?;
        }
        Ok(outcome)
    }

    /// Commits the pending proposals: a commit for the server and a Welcome for every device it
    /// adds. `Ok(None)` if nothing is pending. The group stays at its epoch, with the commit in
    /// flight, until [`Engine::confirm_commit`] or [`Engine::abort_commit`]; nothing is sent
    /// meanwhile.
    pub fn commit_pending(&mut self, conv: ConversationId) -> Result<Option<CommitBundle>> {
        self.usable(conv)?;
        self.load_group(conv)?;
        let Engine {
            groups,
            provider,
            identity,
            meta,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        if group.pending_commit().is_some() {
            return Err(Error::CommitInFlight);
        }
        if meta
            .get(&conv)
            .is_some_and(|entry| entry.self_removal_pending)
        {
            return Err(Error::SelfRemoval);
        }
        if !group.has_pending_proposals() {
            return Ok(None);
        }
        let epoch = group.epoch().as_u64();
        let staged = group
            .commit_builder()
            .consume_proposal_store(true)
            .load_psks(provider.storage())
            .map_err(Error::mls)?
            .build(provider.rand(), provider.crypto(), &identity.key, |_| true)
            .map_err(Error::mls)?
            .stage_commit(&*provider)
            .map_err(Error::mls);
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                groups.remove(&conv);
                return Err(error);
            }
        };
        let (commit, welcome, _group_info) = staged.into_contents();
        let commit = commit.tls_serialize_detached().map_err(Error::mls)?;
        let (added, removed) = {
            let group = groups.get(&conv).ok_or(Error::NoGroup)?;
            let staged = group.pending_commit().ok_or(Error::Malformed("commit"))?;
            changes_of(group, staged)?
        };
        // An MLSMessage holding the Welcome: what the server parses (and what a client reads back).
        let welcome = welcome
            .map(|welcome| {
                mls::MlsMessageOut::from_welcome(welcome, mls::ProtocolVersion::Mls10)
                    .tls_serialize_detached()
                    .map_err(Error::mls)
            })
            .transpose()?;
        let welcomes = match welcome {
            Some(data) => added
                .iter()
                .map(|device| WelcomeFor {
                    device: device.clone(),
                    data: data.clone(),
                })
                .collect(),
            None => Vec::new(),
        };
        if let Err(error) = check_limits(&commit, &welcomes) {
            // The server would refuse it; do not leave it in flight. The proposals stay pending.
            if let Some(group) = groups.get_mut(&conv) {
                group
                    .clear_pending_commit(provider.storage())
                    .map_err(Error::mls)?;
            }
            return Err(error);
        }
        let bundle = CommitBundle {
            epoch,
            commit,
            welcomes,
            added,
            removed,
        };
        if let Some(entry) = meta.get_mut(&conv) {
            entry.pending_commit = Some(StoredBundle::from_bundle(&bundle));
        }
        self.persist(true)?;
        Ok(Some(bundle))
    }

    /// The server accepted our commit (`204`): merge it. The epoch advances.
    pub fn confirm_commit(&mut self, conv: ConversationId) -> Result<CommitOutcome> {
        self.usable(conv)?;
        self.load_group(conv)?;
        let Engine {
            groups,
            provider,
            meta,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        let staged = group
            .pending_commit()
            .ok_or(Error::Malformed("no commit is in flight"))?;
        let (added, removed) = changes_of(group, staged)?;
        let before = group.epoch().as_u64();
        if let Err(error) = group.merge_pending_commit(&*provider) {
            groups.remove(&conv);
            return Err(Error::mls(error));
        }
        let epoch = group.epoch().as_u64();
        let entry = meta.get_mut(&conv).ok_or(Error::NoGroup)?;
        let hash = entry
            .pending_commit
            .take()
            .map(|pending| sha256(&pending.commit).to_vec())
            .unwrap_or_default();
        record_applied(entry, before, hash, epoch, self.config.commits_remembered);
        self.persist(true)?;
        Ok(CommitOutcome {
            epoch,
            added,
            removed,
            removed_self: false,
            duplicate: false,
        })
    }

    /// The server refused our commit (`409 stale_epoch`: another member's won). Our pending
    /// proposals stay; process the winning commit from the feed next.
    pub fn abort_commit(&mut self, conv: ConversationId) -> Result<()> {
        self.load_group(conv)?;
        let Engine {
            groups,
            provider,
            meta,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        group
            .clear_pending_commit(provider.storage())
            .map_err(Error::mls)?;
        if let Some(entry) = meta.get_mut(&conv) {
            entry.pending_commit = None;
        }
        self.persist(true)
    }

    /// Applies a `commit` item of the handshake feed made at `message_epoch` (the item's `epoch`).
    ///
    /// * A commit this device has applied (or made) is [`CommitOutcome::duplicate`], not an error.
    /// * A commit for an epoch below ours that is not one we applied is a rewritten history: the
    ///   group is frozen and the result is [`Error::EpochBackwards`].
    /// * The committer must be a member. Only Add and Remove proposals are allowed; every added
    ///   device must be one this client trusts ([`Error::UntrustedDevices`] says which to look up);
    ///   no member's credential or signing key may change; the external sender key may not change.
    pub fn handle_commit(
        &mut self,
        conv: ConversationId,
        message_epoch: u64,
        data: &[u8],
    ) -> Result<CommitOutcome> {
        if data.len() > MAX_HANDSHAKE_BYTES {
            return Err(Error::TooLarge("commit"));
        }
        self.usable(conv)?;
        self.load_group(conv)?;
        let hash = sha256(data);
        let local = self
            .groups
            .get(&conv)
            .ok_or(Error::NoGroup)?
            .epoch()
            .as_u64();
        let entry = self.meta.get(&conv).ok_or(Error::NoGroup)?;
        if entry
            .pending_commit
            .as_ref()
            .is_some_and(|pending| sha256(&pending.commit) == hash)
            && message_epoch == local
        {
            // Our own commit came back through the feed: the server took it.
            return self.confirm_commit(conv);
        }
        if message_epoch < local {
            let forgotten = (local - message_epoch) as usize > self.config.commits_remembered;
            if message_epoch < entry.joined_epoch
                || forgotten
                || entry
                    .applied
                    .iter()
                    .any(|(epoch, applied)| *epoch == message_epoch && applied.0 == hash)
            {
                return Ok(CommitOutcome {
                    epoch: local,
                    duplicate: true,
                    ..CommitOutcome::default()
                });
            }
            self.freeze(conv, "epoch went backwards");
            return Err(Error::EpochBackwards {
                local,
                shown: message_epoch,
            });
        }
        if message_epoch > local {
            return Err(Error::MissingCommit {
                message: message_epoch,
                local,
            });
        }

        let message = guard(|| mls::MlsMessageIn::tls_deserialize_exact(data))?
            .map_err(|_| Error::Malformed("commit"))?;
        let protocol = message
            .try_into_protocol_message()
            .map_err(|_| Error::Malformed("commit"))?;
        if protocol.group_id().as_slice() != conv.0
            || protocol.wire_format() != mls::WireFormat::PublicMessage
            || protocol.content_type() != mls::ContentType::Commit
        {
            return Err(Error::ForbiddenCommit("not a public commit for this group"));
        }
        if protocol.epoch().as_u64() != message_epoch {
            return Err(Error::Malformed("commit epoch"));
        }

        let Engine {
            groups,
            provider,
            trust,
            meta,
            identity,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        let entry = meta.get_mut(&conv).ok_or(Error::NoGroup)?;
        if group.pending_commit().is_some() {
            // Another member's commit for the epoch we committed at: ours lost.
            group
                .clear_pending_commit(provider.storage())
                .map_err(Error::mls)?;
            entry.pending_commit = None;
        }
        let processed = match guard(|| group.process_message(&*provider, protocol)) {
            Ok(Ok(processed)) => processed,
            Ok(Err(error)) => {
                groups.remove(&conv);
                return Err(Error::mls(error));
            }
            Err(error) => {
                groups.remove(&conv);
                return Err(error);
            }
        };
        let mls::Sender::Member(committer) = *processed.sender() else {
            return Err(Error::ForbiddenCommit("committer is not a member"));
        };
        let mls::ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content()
        else {
            return Err(Error::ForbiddenCommit("not a commit"));
        };
        let own = identity.device_ref();
        let (added, removed) = vet_commit(group, trust, &own, entry, committer, &staged)?;
        let removed_self = staged.self_removed();
        let before = group.epoch().as_u64();
        if let Err(error) = group.merge_staged_commit(&*provider, *staged) {
            groups.remove(&conv);
            return Err(Error::mls(error));
        }
        let epoch = group.epoch().as_u64();
        entry.pending_commit = None;
        entry.self_removal_pending = false;
        record_applied(
            entry,
            before,
            hash.to_vec(),
            epoch,
            self.config.commits_remembered,
        );
        if removed_self {
            let mut group = groups.remove(&conv).ok_or(Error::NoGroup)?;
            group.delete(provider.storage()).map_err(Error::mls)?;
            entry.removed = true;
        }
        self.persist(false)?;
        Ok(CommitOutcome {
            epoch,
            added,
            removed,
            removed_self,
            duplicate: false,
        })
    }

    /// Joins a conversation from a `welcome` item of the handshake feed (a `MLSMessage` holding the
    /// `Welcome`, or a raw TLS `Welcome`).
    ///
    /// Every device in the group's tree must be one this client trusts (this device's own included,
    /// by its identity): [`Error::UntrustedDevices`] lists those that are not; verify their
    /// people's device lists and call again (the key package this Welcome uses is not spent by a
    /// refusal). The group's external sender key must be exactly one key, and must match the key
    /// already pinned for the conversation, or `server_key` (from `GET /v1/e2ee/conversations/{id}`),
    /// or, with neither, it is pinned now. A Welcome for an epoch this device has already been in is
    /// [`Error::Duplicate`] and changes nothing. Messages from before this epoch can never be opened.
    pub fn handle_welcome(
        &mut self,
        conv: ConversationId,
        data: &[u8],
        server_key: Option<&[u8]>,
    ) -> Result<JoinOutcome> {
        if data.len() > MAX_HANDSHAKE_BYTES {
            return Err(Error::TooLarge("welcome"));
        }
        let welcome = guard(|| match mls::Welcome::tls_deserialize_exact(data) {
            Ok(welcome) => Some(welcome),
            Err(_) => match mls::MlsMessageIn::tls_deserialize_exact(data)
                .map(mls::MlsMessageIn::extract)
            {
                Ok(mls::MlsMessageBodyIn::Welcome(welcome)) => Some(welcome),
                _ => None,
            },
        })?
        .ok_or(Error::Malformed("welcome"))?;
        if let Some(entry) = self.meta.get(&conv) {
            if entry.frozen.is_some() {
                return Err(Error::Frozen("epoch went backwards"));
            }
            if entry.server_key_alert {
                return Err(Error::ServerKeyChanged);
            }
        }
        let expected_pin = match (
            self.meta
                .get(&conv)
                .filter(|entry| !entry.server_key.is_empty()),
            server_key,
        ) {
            (Some(entry), _) => Some(entry.server_key.0.clone()),
            (None, Some(shown)) => Some(parse_server_key(shown)?.0),
            (None, None) => None,
        };
        let config = join_config(&self.config);
        let active = self.meta.get(&conv).is_some_and(|entry| !entry.removed);
        // Staging spends the key package this Welcome names. A refusal below must give it back.
        let spent = welcome.secrets().iter().find_map(|secret| {
            let reference = secret.new_member();
            self.provider
                .storage()
                .key_package::<mls::KeyPackageRef, mls::KeyPackageBundle>(&reference)
                .ok()
                .flatten()
                .map(|bundle| (reference, bundle))
        });
        let restore = |engine: &Engine| {
            if let Some((reference, bundle)) = &spent {
                let _ = engine
                    .provider
                    .storage()
                    .write_key_package(reference, bundle);
            }
        };
        let staged = match mls::StagedWelcome::build_from_welcome(&self.provider, &config, welcome)
        {
            Ok(builder) => builder
                .replace_old_group()
                .skip_lifetime_validation()
                .build(),
            Err(error) => {
                restore(self);
                // A Welcome for a key package that is gone, in a conversation this device is already
                // in: the one it joined by, delivered again.
                if active && matches!(error, mls::WelcomeError::NoMatchingKeyPackage) {
                    return Err(Error::Duplicate);
                }
                return Err(Error::mls(error));
            }
        };
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                restore(self);
                if active && matches!(error, mls::WelcomeError::NoMatchingKeyPackage) {
                    return Err(Error::Duplicate);
                }
                return Err(Error::mls(error));
            }
        };

        let checks = (|| -> Result<(u64, Vec<DeviceRef>, Vec<u8>)> {
            let context = staged.group_context();
            if context.group_id().as_slice() != conv.0 || context.ciphersuite() != CIPHERSUITE {
                return Err(Error::Malformed("welcome for another group"));
            }
            let key = group_server_key(context.extensions())?;
            if expected_pin.as_ref().is_some_and(|pin| *pin != key) {
                return Err(Error::ServerKeyChanged);
            }
            let epoch = context.epoch().as_u64();
            let members = staged
                .members()
                .map(|member| device_of_member(&member))
                .collect::<Result<Vec<_>>>()?;
            let own = self.identity.device_ref();
            let untrusted: Vec<DeviceRef> = members
                .iter()
                .filter(|device| {
                    **device != own
                        && !self
                            .trust
                            .is_trusted(device.user, device.device, &device.signature_key)
                })
                .cloned()
                .collect();
            if !untrusted.is_empty() {
                return Err(Error::UntrustedDevices(untrusted));
            }
            if !members.contains(&own) {
                return Err(Error::Malformed(
                    "welcome that does not include this device",
                ));
            }
            Ok((epoch, members, key))
        })();
        let (epoch, members, key) = match checks {
            Ok(checked) => checked,
            Err(error) => {
                restore(self);
                return Err(error);
            }
        };

        // A Welcome for an epoch this device has already been in is one it has had before (a feed
        // read again, or an old Welcome replayed to a device whose last-resort key package still
        // opens it). Nothing is changed, so nothing is frozen: it is only not taken.
        if let Some(entry) = self.meta.get(&conv) {
            if entry.high_epoch > 0 && epoch <= entry.high_epoch {
                restore(self);
                return Err(Error::Duplicate);
            }
        }
        let group = match staged.into_group(&self.provider) {
            Ok(group) => group,
            Err(error) => {
                restore(self);
                return Err(Error::mls(error));
            }
        };
        let pinned_now = expected_pin.is_none();
        self.groups.insert(conv, group);
        self.meta.insert(
            conv,
            ConvMeta {
                server_key: key.into(),
                high_epoch: epoch,
                joined_epoch: epoch,
                ..ConvMeta::default()
            },
        );
        self.persist(true)?;
        Ok(JoinOutcome {
            epoch,
            members,
            pinned_now,
        })
    }
}

/// Which devices a staged commit adds and removes (removals by the leaves' current credentials).
fn changes_of(
    group: &mls::MlsGroup,
    staged: &mls::StagedCommit,
) -> Result<(Vec<DeviceRef>, Vec<DeviceRef>)> {
    let added = staged
        .add_proposals()
        .map(|queued| device_of_key_package(queued.add_proposal().key_package()))
        .collect::<Result<Vec<_>>>()?;
    let mut removed = Vec::new();
    for queued in staged.remove_proposals() {
        if let Some(member) = group.member_at(queued.remove_proposal().removed()) {
            removed.push(device_of_member(&member)?);
        }
    }
    Ok((added, removed))
}

fn record_applied(entry: &mut ConvMeta, made_at: u64, hash: Vec<u8>, now: u64, keep: usize) {
    entry.applied.push((made_at, hash.into()));
    if entry.applied.len() > keep {
        let excess = entry.applied.len() - keep;
        entry.applied.drain(..excess);
    }
    entry.high_epoch = entry.high_epoch.max(now);
}

/// Decides one external proposal: what it is, and whether it may be taken.
fn vet_proposal(
    group: &mls::MlsGroup,
    trust: &crate::identity::Trust,
    queued: &mls::QueuedProposal,
) -> (Option<ProposalKind>, std::result::Result<(), RejectReason>) {
    match queued.proposal() {
        mls::Proposal::Add(add) => {
            let package = add.key_package();
            let Ok(device) = device_of_key_package(package) else {
                return (None, Err(RejectReason::Invalid));
            };
            let kind = Some(ProposalKind::Add(device.clone()));
            if package.ciphersuite() != CIPHERSUITE {
                return (kind, Err(RejectReason::Invalid));
            }
            if group.members().any(|member| {
                device_of_member(&member).is_ok_and(|existing| {
                    existing.user == device.user && existing.device == device.device
                })
            }) {
                return (kind, Err(RejectReason::AlreadyMember));
            }
            if trust.is_trusted(device.user, device.device, &device.signature_key) {
                return (kind, Ok(()));
            }
            // Known device, other key: forged. Not known: not yet fetched.
            let known = trust
                .devices(device.user)
                .iter()
                .any(|trusted| trusted.device == device.device);
            (
                kind,
                Err(if known {
                    RejectReason::KeyMismatch
                } else {
                    RejectReason::UnknownDevice
                }),
            )
        }
        mls::Proposal::Remove(remove) => match group
            .member_at(remove.removed())
            .and_then(|member| device_of_member(&member).ok())
        {
            Some(device) => (Some(ProposalKind::Remove(device)), Ok(())),
            None => (None, Err(RejectReason::NotInRoster)),
        },
        _ => (None, Err(RejectReason::NotAllowed)),
    }
}

/// The rules a received commit must meet before it is merged. Returns the devices it adds and
/// removes.
fn vet_commit(
    group: &mls::MlsGroup,
    trust: &crate::identity::Trust,
    own: &DeviceRef,
    entry: &mut ConvMeta,
    committer: mls::LeafNodeIndex,
    staged: &mls::StagedCommit,
) -> Result<(Vec<DeviceRef>, Vec<DeviceRef>)> {
    for queued in staged.queued_proposals() {
        match queued.proposal() {
            mls::Proposal::Add(_) | mls::Proposal::Remove(_) => {}
            _ => {
                return Err(Error::ForbiddenCommit(
                    "only Add and Remove proposals are allowed",
                ))
            }
        }
        // Who is in the group is the server's decision (it proposes, we vet): a member does not
        // add or remove anyone on its own, so a proposal a commit carries by value, or that a member
        // made, is refused.
        if !matches!(queued.sender(), mls::Sender::External(_)) {
            return Err(Error::ForbiddenCommit(
                "membership changes come from the server's proposals only",
            ));
        }
    }
    // The committer keeps its identity and its key: a new path leaf may only rotate encryption keys.
    if let Some(leaf) = staged.update_path_leaf_node() {
        let before = group
            .member_at(committer)
            .ok_or(Error::ForbiddenCommit("unknown committer"))?;
        if leaf.credential() != &before.credential
            || leaf.signature_key().as_slice() != before.signature_key.as_slice()
        {
            return Err(Error::ForbiddenCommit(
                "a member's credential or signing key changed",
            ));
        }
    }
    let (added, removed) = changes_of(group, staged)?;
    let untrusted: Vec<DeviceRef> = added
        .iter()
        .filter(|device| {
            *device != own && !trust.is_trusted(device.user, device.device, &device.signature_key)
        })
        .cloned()
        .collect();
    if !untrusted.is_empty() {
        return Err(Error::UntrustedDevices(untrusted));
    }
    if added
        .iter()
        .any(|device| device.user == own.user && device.device == own.device)
    {
        return Err(Error::ForbiddenCommit("adds this device again"));
    }
    let keys = group_server_key(staged.group_context().extensions())?;
    if keys != entry.server_key.0 {
        entry.server_key_alert = true;
        return Err(Error::ServerKeyChanged);
    }
    Ok((added, removed))
}

/// Feeds `bytes` to every parser that takes bytes from the server, in a fresh group of a
/// throw-away engine kept per thread. For fuzz targets and property tests: nothing here may panic.
#[doc(hidden)]
pub fn fuzz_all(bytes: &[u8]) {
    use std::cell::RefCell;
    thread_local! {
        static STATE: RefCell<Option<(Engine, ConversationId)>> = const { RefCell::new(None) };
    }
    STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let built = (|| -> Result<(Engine, ConversationId)> {
                let mut engine = Engine::detached(crate::ids::UserId::random()?)?;
                let conv = ConversationId::random()?;
                let server = crate::identity::DeviceKey::generate()?;
                engine.create_group(conv, &server.public_key())?;
                Ok((engine, conv))
            })();
            *slot = built.ok();
        }
        let Some((engine, conv)) = slot.as_mut() else {
            return;
        };
        let conv = *conv;
        let epoch = engine.epoch(conv).unwrap_or(0);
        let _ = engine.handle_proposals(conv, bytes);
        let _ = engine.handle_commit(conv, epoch, bytes);
        let _ = engine.handle_welcome(conv, bytes, None);
        let _ = engine.decrypt(
            conv,
            bytes,
            &crate::messaging::Claimed {
                author: None,
                device: None,
                kind: crate::envelope::Kind::Message,
                target_id: None,
            },
        );
        let _ = parse_server_key(bytes);
    });
}

impl Engine {
    /// The leaf index of a member's device in the group's tree: what a Remove proposal names. The
    /// server reads it from the public tree; this is for tests that play the server.
    #[doc(hidden)]
    pub fn leaf_index(
        &mut self,
        conv: ConversationId,
        device: crate::ids::DeviceId,
    ) -> Result<Option<u32>> {
        self.load_group(conv)?;
        let group = self.groups.get(&conv).ok_or(Error::NoGroup)?;
        Ok(group
            .members()
            .find(|member| device_of_member(member).is_ok_and(|found| found.device == device))
            .map(|member| member.index.u32()))
    }
}

/// Commits that a malicious (or buggy) member could make, to test the receiving side. They are
/// built, not kept: the forging engine's group stays at its epoch.
#[doc(hidden)]
pub enum Forge<'a> {
    /// A path leaf with another credential or another signing key than the committer's.
    ReplaceIdentity(&'a crate::identity::DeviceIdentity),
    /// A group context change that swaps the external sender for this key.
    ChangeServerKey(&'a [u8]),
    /// An Add, by value, of a key package: a device nobody vouched for.
    AddKeyPackage(&'a [u8]),
}

impl Engine {
    #[doc(hidden)]
    pub fn forge_commit(&mut self, conv: ConversationId, forge: Forge<'_>) -> Result<Vec<u8>> {
        self.load_group(conv)?;
        let Engine {
            groups,
            provider,
            identity,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        let bundle = match forge {
            Forge::ReplaceIdentity(other) => {
                let credential_with_key = mls::CredentialWithKey {
                    credential: mls::BasicCredential::new(other.credential().to_vec()).into(),
                    signature_key: other.key.public_key().into(),
                };
                let new_signer = mls::NewSignerBundle {
                    signer: &other.key,
                    credential_with_key,
                };
                group
                    .self_update_with_new_signer(
                        &*provider,
                        &identity.key,
                        new_signer,
                        mls::LeafNodeParameters::default(),
                    )
                    .map_err(Error::mls)?
            }
            Forge::ChangeServerKey(key) => {
                let (_, sender) = parse_server_key(key)?;
                let extensions =
                    mls::Extensions::single(mls::Extension::ExternalSenders(vec![sender]))
                        .map_err(Error::mls)?;
                group
                    .commit_builder()
                    .propose_group_context_extensions(extensions)
                    .map_err(Error::mls)?
                    .load_psks(provider.storage())
                    .map_err(Error::mls)?
                    .build(provider.rand(), provider.crypto(), &identity.key, |_| true)
                    .map_err(Error::mls)?
                    .stage_commit(&*provider)
                    .map_err(Error::mls)?
            }
            Forge::AddKeyPackage(bytes) => {
                let package = mls::KeyPackageIn::tls_deserialize_exact(bytes)
                    .map_err(|_| Error::Malformed("key package"))?
                    .validate(provider.crypto(), mls::ProtocolVersion::Mls10)
                    .map_err(Error::mls)?;
                group
                    .commit_builder()
                    .propose_adds(Some(package))
                    .load_psks(provider.storage())
                    .map_err(Error::mls)?
                    .build(provider.rand(), provider.crypto(), &identity.key, |_| true)
                    .map_err(Error::mls)?
                    .stage_commit(&*provider)
                    .map_err(Error::mls)?
            }
        };
        let commit = bundle
            .commit()
            .tls_serialize_detached()
            .map_err(Error::mls)?;
        group
            .clear_pending_commit(provider.storage())
            .map_err(Error::mls)?;
        Ok(commit)
    }
}

impl Engine {
    /// The commit of ours that waits for the server's answer, whole, as `commit_pending` returned
    /// it: after a restart or a reply that never came, send it again. The server decides by epoch,
    /// so sending it twice is safe. Do not guess from a `409`: read the handshake feed and give the
    /// `commit` item to [`Engine::handle_commit`], which confirms ours if it is the one that won
    /// and drops ours and applies the winner if it is not.
    pub fn pending_commit(&self, conv: ConversationId) -> Option<CommitBundle> {
        self.meta
            .get(&conv)
            .and_then(|entry| entry.pending_commit.as_ref())
            .map(StoredBundle::to_bundle)
    }
}
