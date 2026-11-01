//! Encrypting and decrypting messages: MLS application messages (`PrivateMessage`) carrying the
//! JSON envelope.
//!
//! The server stores `kind` and `target_id` next to the ciphertext, outside anything the sender
//! signed, so a server could turn an edit into a message, or point an edit at another message of
//! the same author. They are therefore bound into the ciphertext: they are the MLS message's
//! authenticated data (`"tiny-e2ee-v1 msg" 0x00 kind 0x00 target_id`), and a ciphertext whose
//! authenticated data is not what the row says is [`Error::AadMismatch`].

use crate::engine::Engine;
use crate::envelope::{Envelope, Kind};
use crate::error::{Error, Result};
use crate::ids::{ConversationId, DeviceId, DeviceRef, UserId};
use crate::wire::guard;
use openmls::prelude as mls;
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use zeroize::Zeroizing;

const AAD_LABEL: &[u8] = b"tiny-e2ee-v1 msg";

fn aad(kind: Kind, target_id: Option<&str>) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(AAD_LABEL);
    aad.push(0);
    aad.extend_from_slice(kind.as_str().as_bytes());
    aad.push(0);
    aad.extend_from_slice(target_id.unwrap_or("").as_bytes());
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
    /// Encrypts a message (`Kind::Message`, no target) or an edit (`Kind::Edit`, the id of the
    /// message it edits) for the conversation's group, in its current epoch.
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
        if (kind == Kind::Edit) != target_id.is_some() {
            return Err(Error::Malformed("edit target"));
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
        group.set_aad(aad(kind, target_id));
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
    /// A message this device sent cannot be opened by it (MLS does not allow it): keep the
    /// plaintext when you send.
    pub fn decrypt(
        &mut self,
        conv: ConversationId,
        ciphertext: &[u8],
        claimed: &Claimed<'_>,
    ) -> Result<Incoming> {
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
        let processed = match guard(|| group.process_message(&*provider, protocol)) {
            Ok(Ok(processed)) => processed,
            Ok(Err(_)) | Err(_) => {
                // The key may have been advanced in memory before the check failed; the stored
                // state has not. Reload it, so a forged message cannot spend a real one's key.
                groups.remove(&conv);
                return Err(Error::Undecryptable);
            }
        };
        let mls::Sender::Member(leaf) = *processed.sender() else {
            return Err(Error::SenderMismatch);
        };
        let member = group.member_at(leaf).ok_or(Error::SenderMismatch)?;
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
        if processed.aad() != aad(claimed.kind, claimed.target_id).as_slice() {
            return Err(Error::AadMismatch);
        }
        let mls::ProcessedMessageContent::ApplicationMessage(application) =
            processed.into_content()
        else {
            return Err(Error::Malformed("ciphertext"));
        };
        let plaintext = Zeroizing::new(application.into_bytes());
        let envelope = Envelope::decode(claimed.kind, &plaintext)?;
        let sender_trusted = trust.is_trusted(sender.user, sender.device, &sender.signature_key);
        self.persist(false)?;
        Ok(Incoming {
            sender,
            sender_trusted,
            epoch,
            kind: claimed.kind,
            target_id: claimed.target_id.map(str::to_owned),
            envelope,
        })
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
