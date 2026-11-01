// Discord's side of DAVE's MLS for tests: the voice server as the group's external sender.
// Included by the unit tests of `dave` and by the fake voice server.

use openmls::prelude::tls_codec::{Serialize, VLBytes};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

/// The ciphersuite of DAVE protocol version 1.
const SUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256;

pub struct Server {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
}

impl Server {
    pub fn new() -> Server {
        Server {
            provider: OpenMlsRustCrypto::default(),
            signer: SignatureKeyPair::new(SUITE.signature_algorithm()).unwrap(),
        }
    }

    /// The payload of opcode 25.
    pub fn package(&self) -> Vec<u8> {
        let credential = BasicCredential::new(b"voice-server".to_vec());
        ExternalSender::new(self.signer.public().into(), credential.into()).tls_serialize_detached().unwrap()
    }

    /// The payload of opcode 27 that proposes adding the owner of `key_package` (the payload of
    /// an opcode 26) to the group of `channel` at `epoch`.
    pub fn add(&self, key_package: &[u8], channel: u64, epoch: u64) -> Vec<u8> {
        let key_package = KeyPackageIn::tls_deserialize_exact_bytes(key_package)
            .unwrap()
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .unwrap();
        let proposal = ExternalProposal::new_add::<OpenMlsRustCrypto>(
            key_package,
            GroupId::from_slice(&channel.to_be_bytes()),
            GroupEpoch::from(epoch),
            &self.signer,
            SenderExtensionIndex::new(0),
        )
        .unwrap();
        let messages = VLBytes::new(proposal.tls_serialize_detached().unwrap());
        // Operation 0 appends proposals.
        [&[0u8][..], &messages.tls_serialize_detached().unwrap()].concat()
    }
}

/// The commit and the welcome of an opcode 28 payload.
pub fn split(payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (_, welcome) = MlsMessageIn::tls_deserialize_bytes(payload).unwrap();
    (payload[..payload.len() - welcome.len()].to_vec(), welcome.to_vec())
}

// Tiny's server as the same external sender: one proposal at a time, and batches of them. Only
// `e2ee`'s tests use these.
#[allow(dead_code)]
impl Server {
    /// One external proposal, as an MLSMessage: add the owner of `key_package`.
    pub fn add_message(&self, key_package: &[u8], channel: u64, epoch: u64) -> Vec<u8> {
        let key_package = KeyPackageIn::tls_deserialize_exact_bytes(key_package)
            .unwrap()
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .unwrap();
        ExternalProposal::new_add::<OpenMlsRustCrypto>(
            key_package,
            GroupId::from_slice(&channel.to_be_bytes()),
            GroupEpoch::from(epoch),
            &self.signer,
            SenderExtensionIndex::new(0),
        )
        .unwrap()
        .tls_serialize_detached()
        .unwrap()
    }

    /// One external proposal, as an MLSMessage: remove the member in `leaf`.
    pub fn remove_message(&self, leaf: u32, channel: u64, epoch: u64) -> Vec<u8> {
        ExternalProposal::new_remove::<OpenMlsRustCrypto>(
            LeafNodeIndex::new(leaf),
            GroupId::from_slice(&channel.to_be_bytes()),
            GroupEpoch::from(epoch),
            &self.signer,
            SenderExtensionIndex::new(0),
        )
        .unwrap()
        .tls_serialize_detached()
        .unwrap()
    }
}

#[allow(dead_code)]
/// The payload of opcode 27 (Tiny's `proposals`) that appends `messages`, in order.
pub fn append(messages: &[Vec<u8>]) -> Vec<u8> {
    let messages = VLBytes::new(messages.concat());
    [&[0u8][..], &messages.tls_serialize_detached().unwrap()].concat()
}
