//! A device's identity, cross-signatures between a person's devices, and the verifier a client
//! runs over the device list the server hands out (`docs/e2ee-text.md`, section 3).
//!
//! The server is not trusted to say which devices a person has. A person's first device is
//! trusted on first use; every later device must carry a cross-signature, made by a device the
//! client already trusts, over `"tiny-e2ee-v1 device-add" || user_id || device_id || signature_key`
//! (ECDSA P-256 over SHA-256). A device the server invented has no such signature, and is
//! refused.

use crate::error::{Error, Result};
use crate::ids::{credential_bytes, DeviceId, DeviceRef, UserId};
use crate::wire::Bytes;
use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

/// The domain-separation prefix of what a cross-signature signs.
pub const CROSS_SIGN_LABEL: &[u8] = b"tiny-e2ee-v1 device-add";
/// Length of an uncompressed P-256 public key.
pub const SIGNATURE_KEY_LEN: usize = 65;

/// A device's signing key: P-256, made on the device, never sent anywhere. It signs MLS
/// handshakes and application messages as the leaf's key, and cross-signatures for new devices.
/// Zeroed when dropped.
pub struct DeviceKey(SigningKey);

impl DeviceKey {
    pub fn generate() -> Result<DeviceKey> {
        loop {
            let mut bytes = Zeroizing::new([0u8; 32]);
            getrandom::fill(&mut *bytes).map_err(|_| Error::Random)?;
            // A random 32 bytes is a valid scalar unless it is zero or at least the group order:
            // about 2^-32. Try again then.
            if let Ok(key) = SigningKey::from_slice(&*bytes) {
                return Ok(DeviceKey(key));
            }
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<DeviceKey> {
        SigningKey::from_slice(bytes)
            .map(DeviceKey)
            .map_err(|_| Error::Malformed("signing key"))
    }

    pub(crate) fn secret_bytes(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(self.0.to_bytes().to_vec())
    }

    /// The public key, uncompressed (65 bytes).
    pub fn public_key(&self) -> Vec<u8> {
        self.0
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }

    /// ECDSA P-256/SHA-256 over `message`, DER encoded (as MLS encodes its signatures).
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.0.sign(message);
        signature.to_der().as_bytes().to_vec()
    }
}

impl openmls_traits::signatures::Signer for DeviceKey {
    fn sign(
        &self,
        payload: &[u8],
    ) -> std::result::Result<Vec<u8>, openmls_traits::signatures::SignerError> {
        Ok(DeviceKey::sign(self, payload))
    }

    fn signature_scheme(&self) -> openmls_traits::types::SignatureScheme {
        openmls_traits::types::SignatureScheme::ECDSA_SECP256R1_SHA256
    }
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceKey(..)")
    }
}

/// This device: who, which, and its signing key.
#[derive(Debug)]
pub struct DeviceIdentity {
    pub user: UserId,
    pub device: DeviceId,
    pub key: DeviceKey,
}

impl DeviceIdentity {
    /// A new device for `user`, with a random device id and a fresh key.
    pub fn generate(user: UserId) -> Result<DeviceIdentity> {
        Ok(DeviceIdentity {
            user,
            device: DeviceId::random()?,
            key: DeviceKey::generate()?,
        })
    }

    pub fn credential(&self) -> [u8; 32] {
        credential_bytes(self.user, self.device)
    }

    pub fn device_ref(&self) -> DeviceRef {
        DeviceRef {
            user: self.user,
            device: self.device,
            signature_key: self.key.public_key(),
        }
    }

    /// Approves a new device of the same person: signs its id and key. The result goes in
    /// `PUT /v1/e2ee/devices/{id}` as `cross_signature`, carried to the new device by whatever
    /// pairing flow the app has. Only a device that is itself trusted should be asked to.
    pub fn cross_sign(&self, new_device: DeviceId, new_signature_key: &[u8]) -> Result<Vec<u8>> {
        check_key(new_signature_key)?;
        Ok(self.key.sign(&cross_signature_message(
            self.user,
            new_device,
            new_signature_key,
        )))
    }
}

/// What a cross-signature signs.
pub fn cross_signature_message(user: UserId, device: DeviceId, signature_key: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(CROSS_SIGN_LABEL.len() + 32 + signature_key.len());
    message.extend_from_slice(CROSS_SIGN_LABEL);
    message.extend_from_slice(&user.0);
    message.extend_from_slice(&device.0);
    message.extend_from_slice(signature_key);
    message
}

fn check_key(key: &[u8]) -> Result<VerifyingKey> {
    if key.len() != SIGNATURE_KEY_LEN || key[0] != 0x04 {
        return Err(Error::Malformed("signature key"));
    }
    VerifyingKey::from_sec1_bytes(key).map_err(|_| Error::Malformed("signature key"))
}

/// Checks that `signer_key` signed the addition of `device` with `signature_key` to `user`'s
/// devices. The signature is DER, or the 64 byte `r || s` form; both are accepted.
pub fn verify_cross_signature(
    signer_key: &[u8],
    user: UserId,
    device: DeviceId,
    signature_key: &[u8],
    signature: &[u8],
) -> Result<()> {
    let signer = check_key(signer_key)?;
    check_key(signature_key)?;
    let signature = match signature.len() {
        64 => Signature::from_slice(signature),
        0..=72 => Signature::from_der(signature),
        _ => return Err(Error::Malformed("signature")),
    }
    .map_err(|_| Error::Malformed("signature"))?;
    signer
        .verify(
            &cross_signature_message(user, device, signature_key),
            &signature,
        )
        .map_err(|_| Error::BadSignature)
}

/// One entry of `GET /v1/e2ee/users/{id}/devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub device_id: DeviceId,
    /// Uncompressed P-256, 65 bytes.
    pub signature_key: Vec<u8>,
    /// Absent for a person's first device.
    pub cross_signature: Option<Vec<u8>>,
    /// Any increasing number: the server's creation time in milliseconds. Only the order matters.
    pub created_at: i64,
}

/// Why a device on a list was not trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// A key that is not an uncompressed P-256 point.
    BadKey,
    /// The device id is one this client already trusts, with another key.
    KeyChanged,
    /// No cross-signature, or one that no trusted device made.
    NotCrossSigned,
}

/// What [`Trust::verify_list`] decided about one list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainReport {
    /// The devices now trusted for the person.
    pub trusted: Vec<DeviceRef>,
    /// Devices on the list that were refused, and why. Show these: they are the sign of a server
    /// adding devices of its own.
    pub rejected: Vec<(DeviceId, Rejection)>,
    /// Trusted now, not before.
    pub added: Vec<DeviceId>,
    /// Trusted before, off the list now (revoked).
    pub removed: Vec<DeviceId>,
    /// This was the first list seen for this person: the root device was trusted on first use.
    pub first_contact: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UserTrust {
    /// A root has been pinned. Stays set when every device is revoked, so a later unsigned
    /// "first" device is reported as a new identity and not trusted silently.
    pinned: bool,
    /// Trusted, active devices: id to key.
    devices: BTreeMap<DeviceId, Bytes>,
}

/// Which devices of which people this client trusts. Persisted inside the engine's state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trust {
    users: BTreeMap<UserId, UserTrust>,
}

impl Trust {
    pub fn new() -> Trust {
        Trust::default()
    }

    /// True when `user`'s device `device` is trusted with exactly this key.
    pub fn is_trusted(&self, user: UserId, device: DeviceId, signature_key: &[u8]) -> bool {
        self.users
            .get(&user)
            .and_then(|trust| trust.devices.get(&device))
            .is_some_and(|key| key[..] == *signature_key)
    }

    /// The trusted devices of `user`, in id order.
    pub fn devices(&self, user: UserId) -> Vec<DeviceRef> {
        self.users
            .get(&user)
            .map(|trust| {
                trust
                    .devices
                    .iter()
                    .map(|(device, key)| DeviceRef {
                        user,
                        device: *device,
                        signature_key: key.0.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True once any device of `user` has been trusted.
    pub fn knows(&self, user: UserId) -> bool {
        self.users.get(&user).is_some_and(|trust| trust.pinned)
    }

    /// Forgets `user`: their next list is trusted on first use again. Call it after the person
    /// has been told that this contact's identity changed and agrees, never before.
    pub fn accept_new_identity(&mut self, user: UserId) {
        self.users.remove(&user);
    }

    /// Verifies the list the server gave for `user` and updates what is trusted.
    ///
    /// * No device of `user` is known: the earliest device on the list is the root (trusted on
    ///   first use), and every other device must chain to it.
    /// * Otherwise a device is trusted if this client already trusted it with the same key, or if
    ///   a cross-signature over it verifies against a device trusted before this call or accepted
    ///   earlier in it. A device that was trusted and is off the list is revoked and trusts nobody.
    /// * If devices are known, the list has devices, and none could be trusted, the person has
    ///   no chain to what this client pinned: [`Error::IdentityChanged`], nothing is updated.
    ///   The same when every device was revoked and a fresh unsigned one appears.
    ///
    /// A device that is refused does not fail the call: it is in [`ChainReport::rejected`] and is
    /// never trusted, so an Add for it is refused too.
    pub fn verify_list(&mut self, user: UserId, list: &[DeviceRecord]) -> Result<ChainReport> {
        let entry = self.users.get(&user).cloned().unwrap_or_default();
        let before: BTreeMap<DeviceId, Vec<u8>> = entry
            .devices
            .iter()
            .map(|(device, key)| (*device, key.0.clone()))
            .collect();
        let mut accepted: BTreeMap<DeviceId, Vec<u8>> = BTreeMap::new();
        let mut rejected: Vec<(DeviceId, Rejection)> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending: Vec<&DeviceRecord> = Vec::new();
        let mut first_contact = false;

        let mut ordered: Vec<&DeviceRecord> = list.iter().collect();
        ordered.sort_by_key(|record| (record.created_at, record.device_id));
        for record in ordered {
            if !seen.insert(record.device_id) {
                // The same device twice on one list: the later entry is not believed.
                rejected.push((record.device_id, Rejection::KeyChanged));
                continue;
            }
            if check_key(&record.signature_key).is_err() {
                rejected.push((record.device_id, Rejection::BadKey));
                continue;
            }
            match before.get(&record.device_id) {
                Some(key) if *key == record.signature_key => {
                    accepted.insert(record.device_id, record.signature_key.clone());
                }
                Some(_) => rejected.push((record.device_id, Rejection::KeyChanged)),
                None => pending.push(record),
            }
        }

        if !entry.pinned {
            // The root: the earliest device, trusted on first use.
            if let Some(root) = pending.first().copied() {
                accepted.insert(root.device_id, root.signature_key.clone());
                pending.retain(|record| record.device_id != root.device_id);
                first_contact = true;
            }
        }

        // Signers: what was trusted before this call, and what this call has accepted so far.
        loop {
            let mut progressed = false;
            let mut still = Vec::new();
            for record in std::mem::take(&mut pending) {
                let signed = record.cross_signature.as_deref().is_some_and(|signature| {
                    before.iter().chain(accepted.iter()).any(|(_, signer)| {
                        verify_cross_signature(
                            signer,
                            user,
                            record.device_id,
                            &record.signature_key,
                            signature,
                        )
                        .is_ok()
                    })
                });
                if signed {
                    accepted.insert(record.device_id, record.signature_key.clone());
                    progressed = true;
                } else {
                    still.push(record);
                }
            }
            pending = still;
            if !progressed || pending.is_empty() {
                break;
            }
        }
        for record in pending {
            rejected.push((record.device_id, Rejection::NotCrossSigned));
        }

        if entry.pinned && accepted.is_empty() && !list.is_empty() {
            return Err(Error::IdentityChanged);
        }

        let added: Vec<DeviceId> = accepted
            .keys()
            .filter(|device| !before.contains_key(device))
            .copied()
            .collect();
        let removed: Vec<DeviceId> = before
            .keys()
            .filter(|device| !accepted.contains_key(device))
            .copied()
            .collect();
        let trusted = accepted
            .iter()
            .map(|(device, key)| DeviceRef {
                user,
                device: *device,
                signature_key: key.clone(),
            })
            .collect();
        let pinned = entry.pinned || !accepted.is_empty();
        let devices = accepted
            .iter()
            .map(|(device, key)| (*device, Bytes(key.clone())))
            .collect();
        self.users.insert(user, UserTrust { pinned, devices });
        Ok(ChainReport {
            trusted,
            rejected,
            added,
            removed,
            first_contact,
        })
    }
}
