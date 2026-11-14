//! Encrypting and decrypting messages: MLS application messages (`PrivateMessage`) carrying the
//! JSON envelope.
//!
//! The server stores `kind` and `target_id` next to the ciphertext, outside anything the sender
//! signed, so a server could turn an edit into a message, or point an edit at another message of
//! the same author. They are therefore bound into the ciphertext: they are the MLS message's
//! authenticated data (`"tiny-e2ee-v1 msg" 0x00 kind 0x00 target_id`), and a ciphertext whose
//! authenticated data is not what the row says is [`Error::AadMismatch`].
//!
//! **Padding** (design `security.md` 2.9). The envelope's length is rounded up to the next
//! bucket of [`PADDING_BUCKETS`] (256 B to 16 KiB, the envelope cap), and MLS's own padding fills
//! the encrypted part of the `PrivateMessage` (the envelope, its length prefix, the signature and
//! the AEAD tag) to exactly that bucket plus [`PADDING_HEADROOM`]. So every message of one bucket,
//! with the same authenticated data, is the same number of bytes on the wire whatever its text and
//! whatever the length of its DER signature: the server learns the bucket, not the length. The
//! receiver strips the padding as MLS defines it (zero bytes after the content, checked by
//! OpenMLS), so nothing changes in the format and older clients read it.

use crate::engine::Engine;
use crate::envelope::{Envelope, Kind};
use crate::error::{Error, Result};
use crate::ids::{ConversationId, DeviceId, DeviceRef, UserId};
use crate::wire::guard;
use openmls::prelude as mls;
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls_traits::storage::{traits, Entity, StorageProvider as _, CURRENT_VERSION};
use openmls_traits::OpenMlsProvider as _;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const AAD_LABEL: &[u8] = b"tiny-e2ee-v1 msg";

// OpenMLS authenticates old messages against the old epoch's members, but ProcessedMessage
// exposes only their credential, not their signing key. Read just the public member records
// from its persisted message-secret store; omitted secret fields are skipped by serde. This
// projection is read-only and must never be written back as an OpenMLS message-secret store.
#[derive(Deserialize, Serialize)]
struct PastEpochMembers {
    past_epoch_trees: Vec<EpochMembers>,
}

#[derive(Deserialize, Serialize)]
struct EpochMembers {
    epoch: u64,
    leaves: Vec<mls::Member>,
}

impl Entity<CURRENT_VERSION> for PastEpochMembers {}
impl traits::MessageSecrets<CURRENT_VERSION> for PastEpochMembers {}

/// The sizes an envelope is padded up to: 256 B for most chat messages, reactions and control
/// messages, up to 16 KiB, the envelope cap ([`crate::envelope::MAX_ENVELOPE_BYTES`]).
pub const PADDING_BUCKETS: [usize; 7] = [256, 512, 1024, 2048, 4096, 8192, 16 * 1024];

/// Room in each padded size for what MLS puts next to the envelope inside the encryption: the
/// envelope's length prefix (at most 4 bytes), the signature's (2) and the signature (a DER P-256
/// ECDSA signature, at most 72), and the AEAD tag (16), 94 bytes in all. Constant, so it hides
/// nothing and leaks nothing: the bucket alone sets the size.
pub const PADDING_HEADROOM: usize = 128;

/// The size the encrypted part of a `PrivateMessage` is padded to for an envelope of `len` bytes:
/// its bucket plus [`PADDING_HEADROOM`]. An envelope over the largest bucket (refused by the
/// encoder before it gets here) is rounded up to a multiple of it.
pub fn padded_len(len: usize) -> usize {
    let largest = PADDING_BUCKETS[PADDING_BUCKETS.len() - 1];
    let bucket = PADDING_BUCKETS
        .iter()
        .copied()
        .find(|bucket| len <= *bucket)
        .unwrap_or_else(|| len.div_ceil(largest) * largest);
    bucket + PADDING_HEADROOM
}

/// `config` with MLS padding set to `padding_size`, every other setting kept exactly. OpenMLS
/// has no setter for one field of a group's configuration, and its builder cannot read the others
/// back, so this goes through the configuration's own serialized form.
fn with_padding(
    config: &mls::MlsGroupJoinConfig,
    padding_size: usize,
) -> Result<mls::MlsGroupJoinConfig> {
    let mut value = serde_json::to_value(config).map_err(|_| Error::Malformed("group config"))?;
    let field = value
        .get_mut("padding_size")
        .ok_or(Error::Malformed("group config"))?;
    *field = serde_json::Value::from(padding_size);
    let changed: mls::MlsGroupJoinConfig =
        serde_json::from_value(value).map_err(|_| Error::Malformed("group config"))?;
    if changed.padding_size() != padding_size {
        return Err(Error::Malformed("group config"));
    }
    Ok(changed)
}

/// `aad`, and when `conversation` is set, a `0x00` and that conversation id after it — the same
/// words the server checks a ciphertext against. A reaction always names its conversation, so a
/// ciphertext cannot be replayed onto another one that keeps the same group; the server demands
/// exactly this (`docs/e2ee-text.md`, section 8).
fn aad_for(kind: Kind, target_id: Option<&str>, conversation: Option<&ConversationId>) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(AAD_LABEL);
    aad.push(0);
    aad.extend_from_slice(kind.as_str().as_bytes());
    aad.push(0);
    aad.extend_from_slice(target_id.unwrap_or("").as_bytes());
    if let Some(conversation) = conversation {
        aad.push(0);
        aad.extend_from_slice(conversation.to_string().as_bytes());
    }
    aad
}

/// A ciphertext ready for `POST /v1/conversations/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// TLS encoded MLS `PrivateMessage` (base64 it for the wire).
    pub ciphertext: Vec<u8>,
    /// The epoch it was made in: the request's `epoch`.
    pub epoch: u64,
}

/// What the server's row says about a ciphertext. The MLS layer checks it against what the
/// sender signed.
#[derive(Debug, Clone, Copy)]
pub struct Claimed<'a> {
    /// The row's `author_id`, if known: refused if it is not the person the MLS sender is.
    pub author: Option<UserId>,
    /// The row's `device_id`, if known: refused if it is not the MLS sender's device.
    pub device: Option<DeviceId>,
    /// The row's `kind`.
    pub kind: Kind,
    /// The row's `target_id`, for an edit.
    pub target_id: Option<&'a str>,
}

/// A message that opened and passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The device that sent it, as the group's tree has it.
    pub sender: DeviceRef,
    /// The sender is a device this client trusts now. False for a device in the group's tree
    /// that has since been revoked, or that this client has no cross-signed record of.
    pub sender_trusted: bool,
    pub epoch: u64,
    pub kind: Kind,
    pub target_id: Option<String>,
    pub envelope: Envelope,
}

impl Incoming {
    /// An edit counts only from the person who wrote what it edits. `target_author` is the
    /// stored author of the target message.
    pub fn authorize_edit(&self, target_author: UserId) -> Result<()> {
        if self.kind == Kind::Edit && self.sender.user == target_author {
            Ok(())
        } else {
            Err(Error::SenderMismatch)
        }
    }
}

impl Engine {
    /// Encrypts a message (`Kind::Message`, no target), an edit (`Kind::Edit`, the id of the
    /// message it edits), or a reaction (`Kind::Reaction`, no target: the emoji and the message
    /// it lands on are in the envelope), for the conversation's group, in its current epoch.
    ///
    /// Refused while a Remove is pending (this device's own removal included), while any
    /// proposal is pending (commit them first: [`Engine::commit_pending`]), and while a commit of
    /// ours waits for the server. The state is written before the ciphertext is returned, so a
    /// crash cannot make this device use one message key twice.
    pub fn encrypt(
        &mut self,
        conv: ConversationId,
        kind: Kind,
        target_id: Option<&str>,
        envelope: &Envelope,
    ) -> Result<Sealed> {
        self.encrypting(conv, (kind == Kind::Reaction).then_some(&conv), kind, target_id, envelope)
    }

    /// [`Engine::encrypt`] with the authenticated data naming `bind` instead of `conv`. A
    /// reaction ciphertext is made in the group this conversation serves now, while the server
    /// checks it against the conversation it was posted to (`docs/e2ee-text.md`, section 8):
    /// after a repair the two ids differ, and a reaction can then never be replayed onto
    /// another conversation that shares the group.
    pub fn encrypt_in(
        &mut self,
        conv: ConversationId,
        bind: &ConversationId,
        kind: Kind,
        target_id: Option<&str>,
        envelope: &Envelope,
    ) -> Result<Sealed> {
        self.encrypting(conv, Some(bind), kind, target_id, envelope)
    }

    fn encrypting(
        &mut self,
        conv: ConversationId,
        bind: Option<&ConversationId>,
        kind: Kind,
        target_id: Option<&str>,
        envelope: &Envelope,
    ) -> Result<Sealed> {
        self.repair_outbound_allowed(conv)?;
        if (kind == Kind::Edit) != target_id.is_some() || (kind == Kind::Reaction && target_id.is_some()) {
            return Err(Error::Malformed(kind.as_str()));
        }
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
        if meta
            .get(&conv)
            .is_some_and(|entry| entry.self_removal_pending)
        {
            return Err(Error::PendingRemove);
        }
        if group.pending_commit().is_some() {
            return Err(Error::CommitInFlight);
        }
        if group
            .pending_proposals()
            .any(|queued| matches!(queued.proposal(), mls::Proposal::Remove(_)))
        {
            return Err(Error::PendingRemove);
        }
        if group.has_pending_proposals() {
            return Err(Error::PendingProposals);
        }
        let plaintext = Zeroizing::new(envelope.encode(kind)?);
        let padding = padded_len(plaintext.len());
        if group.configuration().padding_size() != padding {
            let config = with_padding(group.configuration(), padding)?;
            if let Err(error) = group.set_configuration(provider.storage(), &config) {
                groups.remove(&conv);
                return Err(Error::mls(error));
            }
        }
        let authenticated = aad_for(kind, target_id, bind);
        group.set_aad(authenticated);
        let message = group
            .create_message(&*provider, &identity.key, &plaintext)
            .map_err(Error::mls);
        let epoch = group.epoch().as_u64();
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                groups.remove(&conv);
                return Err(error);
            }
        };
        let ciphertext = message.tls_serialize_detached().map_err(Error::mls)?;
        self.persist(true)?;
        Ok(Sealed { ciphertext, epoch })
    }

    /// Opens a ciphertext and checks it.
    ///
    /// Refused: anything that is not a `PrivateMessage` application message for this group; a
    /// message for an epoch this device has not reached ([`Error::FutureEpoch`]: take the
    /// handshake feed first) or one it no longer holds the keys for; a replay; any altered byte
    /// ([`Error::Undecryptable`] for all of these: they are not told apart on purpose); a sender
    /// that is not the claimed author or device ([`Error::SenderMismatch`]); a kind or target other
    /// than what the sender signed ([`Error::AadMismatch`]); an envelope that does not parse.
    ///
    /// A message that is refused, for any of these reasons, spends no key: the state is put back
    /// as it was, so the genuine message for that sender and generation still opens.
    ///
    /// A message this device sent cannot be opened by it (MLS does not allow it): keep the
    /// plaintext when you send.
    pub fn decrypt(
        &mut self,
        conv: ConversationId,
        ciphertext: &[u8],
        claimed: &Claimed<'_>,
    ) -> Result<Incoming> {
        self.decrypt_in(
            conv,
            (claimed.kind == Kind::Reaction).then_some(&conv),
            ciphertext,
            claimed,
        )
    }

    /// [`Engine::decrypt`] with the authenticated data expected to name `bind`: a reaction
    /// ciphertext always names the conversation it was posted to, whatever group serves it now.
    pub fn decrypt_in(
        &mut self,
        conv: ConversationId,
        bind: Option<&ConversationId>,
        ciphertext: &[u8],
        claimed: &Claimed<'_>,
    ) -> Result<Incoming> {
        // Existing callers keep the lenient save: a receive that also writes opened history must
        // use [`Engine::decrypt_committed`] so a failed durable commit cannot leave only a consumed
        // ratchet.
        let (incoming, checkpoint) = self.decrypt_open(conv, bind, ciphertext, claimed)?;
        drop(checkpoint);
        self.persist(false)?;
        Ok(incoming)
    }

    /// Like [`Engine::decrypt`], but runs `commit` while the receive checkpoint is still
    /// reversible and before the advanced ratchet is written. If `commit` fails, the pre-read
    /// state is restored and the receive spends no key. After a successful commit, the caller
    /// must already have durable recovery data. State saving then follows [`Engine::decrypt`]:
    /// a failed save leaves the accepted receive consumed in memory and reports the error on
    /// [`Engine::flush`]. It must never rewind an accepted receive to match a lagging disk.
    pub fn decrypt_committed<F>(
        &mut self,
        conv: ConversationId,
        ciphertext: &[u8],
        claimed: &Claimed<'_>,
        commit: F,
    ) -> Result<Incoming>
    where
        F: FnOnce(&Incoming) -> Result<()>,
    {
        let (incoming, checkpoint) =
            self.decrypt_open(conv, (claimed.kind == Kind::Reaction).then_some(&conv), ciphertext, claimed)?;
        if let Err(error) = commit(&incoming) {
            self.provider.restore(checkpoint);
            self.groups.remove(&conv);
            return Err(error);
        }
        drop(checkpoint);
        self.persist(false)?;
        Ok(incoming)
    }

    /// Authenticate and open a ciphertext, leaving the provider checkpoint for the caller to
    /// drop (accept) or restore (refuse).
    fn decrypt_open(
        &mut self,
        conv: ConversationId,
        bind: Option<&ConversationId>,
        ciphertext: &[u8],
        claimed: &Claimed<'_>,
    ) -> Result<(Incoming, crate::provider::Checkpoint)> {
        self.repair_receive_allowed(conv)?;
        self.usable(conv)?;
        self.load_group(conv)?;
        let message = guard(|| mls::MlsMessageIn::tls_deserialize_exact(ciphertext))?
            .map_err(|_| Error::Malformed("ciphertext"))?;
        let protocol = message
            .try_into_protocol_message()
            .map_err(|_| Error::Malformed("ciphertext"))?;
        if protocol.group_id().as_slice() != conv.0
            || protocol.wire_format() != mls::WireFormat::PrivateMessage
            || protocol.content_type() != mls::ContentType::Application
        {
            return Err(Error::Malformed("ciphertext"));
        }
        let epoch = protocol.epoch().as_u64();
        let Engine {
            groups,
            provider,
            trust,
            ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        let local = group.epoch().as_u64();
        if epoch > local {
            return Err(Error::FutureEpoch {
                message: epoch,
                local,
            });
        }
        // OpenMLS writes the advanced secret tree to the storage once the AEAD opens, before it
        // checks the sender's signature. Any member can make a ciphertext that opens under another
        // member's leaf and generation (they all hold the tree) with a bad signature; without
        // this, it would spend the real message's key for good. So every failure from here on
        // puts the storage back as it was and reloads the group from it: a message that is not
        // accepted spends nothing.
        let checkpoint = provider.checkpoint();
        let opened = (|| -> Result<(DeviceRef, Envelope)> {
            let processed = match guard(|| group.process_message(&*provider, protocol)) {
                Ok(Ok(processed)) => processed,
                Ok(Err(_)) | Err(_) => return Err(Error::Undecryptable),
            };
            let mls::Sender::Member(leaf) = *processed.sender() else {
                return Err(Error::SenderMismatch);
            };
            let member = if epoch == local {
                group.member_at(leaf)
            } else {
                provider
                    .storage()
                    .message_secrets::<mls::GroupId, PastEpochMembers>(group.group_id())
                    .map_err(Error::mls)?
                    .and_then(|past| {
                        past.past_epoch_trees
                            .into_iter()
                            .find(|past| past.epoch == epoch)
                    })
                    .and_then(|past| past.leaves.into_iter().find(|member| member.index == leaf))
            }
            .ok_or(Error::SenderMismatch)?;
            if &member.credential != processed.credential() {
                return Err(Error::SenderMismatch);
            }
            let sender = {
                let (user, device) =
                    crate::ids::parse_credential(member.credential.serialized_content())?;
                DeviceRef {
                    user,
                    device,
                    signature_key: member.signature_key.clone(),
                }
            };
            if claimed.author.is_some_and(|author| author != sender.user)
                || claimed.device.is_some_and(|device| device != sender.device)
            {
                return Err(Error::SenderMismatch);
            }
            let binding = match claimed.kind {
                Kind::Reaction => Some(bind.copied().unwrap_or(conv)),
                Kind::Message | Kind::Edit => None,
            };
            if processed.aad() != aad_for(claimed.kind, claimed.target_id, binding.as_ref()).as_slice() {
                return Err(Error::AadMismatch);
            }
            let mls::ProcessedMessageContent::ApplicationMessage(application) =
                processed.into_content()
            else {
                return Err(Error::Malformed("ciphertext"));
            };
            let plaintext = Zeroizing::new(application.into_bytes());
            let envelope = Envelope::decode(claimed.kind, &plaintext)?;
            Ok((sender, envelope))
        })();
        let (sender, envelope) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                provider.restore(checkpoint);
                groups.remove(&conv);
                return Err(error);
            }
        };
        let sender_trusted = trust.is_trusted(sender.user, sender.device, &sender.signature_key);
        Ok((
            Incoming {
                sender,
                sender_trusted,
                epoch,
                kind: claimed.kind,
                target_id: claimed.target_id.map(str::to_owned),
                envelope,
            },
            checkpoint,
        ))
    }

    /// A ciphertext under this device's leaf, at the generation its next real message will use,
    /// signed by `signer` instead of this device's key: what a member gone bad can make for any
    /// other member's leaf, since every member holds the whole secret tree. For tests of the
    /// receiving side. Nothing is spent: this device's state is put back as it was. Built only
    /// with the `test-hooks` feature, which this crate's own tests turn on: a release build has
    /// no function that signs a message as anyone but this device.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn forge_message(
        &mut self,
        conv: ConversationId,
        signer: &crate::identity::DeviceKey,
        envelope: &Envelope,
    ) -> Result<Vec<u8>> {
        self.repair_outbound_allowed(conv)?;
        self.usable(conv)?;
        self.load_group(conv)?;
        let Engine {
            groups, provider, ..
        } = self;
        let group = groups.get_mut(&conv).ok_or(Error::NoGroup)?;
        let checkpoint = provider.checkpoint();
        let plaintext = Zeroizing::new(envelope.encode(Kind::Message)?);
        group.set_aad(aad_for(Kind::Message, None, None));
        let message = group
            .create_message(&*provider, signer, &plaintext)
            .map_err(Error::mls)
            .and_then(|message| message.tls_serialize_detached().map_err(Error::mls));
        provider.restore(checkpoint);
        groups.remove(&conv);
        message
    }

    /// The safety number between this person and `other`, over the devices each is trusted to
    /// have (this device's own included): the same for both people. See [`crate::safety`].
    pub fn safety_number(&self, other: UserId) -> Result<crate::safety::SafetyNumber> {
        let mut mine = self.trust.devices(self.identity.user);
        let own = self.identity.device_ref();
        if !mine.iter().any(|device| device.device == own.device) {
            mine.push(own);
        }
        let theirs = self.trust.devices(other);
        crate::safety::safety_number(self.identity.user, &mine, other, &theirs)
    }

    /// Like [`Engine::safety_number`], from the devices that are in the conversation's group
    /// instead of the trust lists.
    pub fn safety_number_in_group(
        &mut self,
        conv: ConversationId,
        other: UserId,
    ) -> Result<crate::safety::SafetyNumber> {
        let roster = self.roster(conv)?;
        crate::safety::safety_number(self.identity.user, &roster, other, &roster)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_the_padding_keeps_every_other_setting() {
        let config = mls::MlsGroupJoinConfig::builder()
            .wire_format_policy(mls::PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
            .use_ratchet_tree_extension(true)
            .max_past_epochs(7)
            .number_of_resumption_psks(3)
            .padding_size(128)
            .sender_ratchet_configuration(mls::SenderRatchetConfiguration::new(11, 222))
            .build();
        let changed = with_padding(&config, padded_len(300)).unwrap();
        assert_eq!(changed.padding_size(), 512 + PADDING_HEADROOM);
        assert_ne!(changed, config);
        assert_eq!(with_padding(&changed, 128).unwrap(), config);
    }
}
