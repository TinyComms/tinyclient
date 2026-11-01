//! The OpenMLS provider: RustCrypto for the primitives and an in-memory storage that the engine
//! snapshots (sealed) to a [`crate::store::Store`]. The group state, the key packages' private
//! halves and the epoch secrets live in that storage.

use openmls_rust_crypto::{MemoryStorage, RustCrypto};
use openmls_traits::OpenMlsProvider;
use std::collections::HashMap;
use zeroize::Zeroize;

/// A copy of the provider's storage (see [`TinyProvider::checkpoint`]). Holds epoch and message
/// secrets, so its bytes are zeroed when it is dropped.
pub(crate) struct Checkpoint(HashMap<Vec<u8>, Vec<u8>>);

impl Drop for Checkpoint {
    fn drop(&mut self) {
        for (mut key, mut value) in self.0.drain() {
            key.zeroize();
            value.zeroize();
        }
    }
}

#[derive(Default, Debug)]
pub struct TinyProvider {
    crypto: RustCrypto,
    storage: MemoryStorage,
}

impl OpenMlsProvider for TinyProvider {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = MemoryStorage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

impl TinyProvider {
    /// The storage's entries, sorted so that two snapshots of one state are the same bytes.
    pub(crate) fn entries(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let values = self
            .storage
            .values
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        entries.sort();
        entries
    }

    /// A copy of the whole storage, to put back with [`TinyProvider::restore`] if an operation
    /// that may write to it fails part way. OpenMLS writes some state before it has finished
    /// checking a message (the advanced secret tree, before the signature), so "reload the group"
    /// alone does not undo it. The copy is zeroed when dropped.
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        let values = self
            .storage
            .values
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Checkpoint(values.clone())
    }

    /// Puts the storage back exactly as it was at `checkpoint`.
    pub(crate) fn restore(&self, mut checkpoint: Checkpoint) {
        let mut values = self
            .storage
            .values
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::swap(&mut *values, &mut checkpoint.0);
        // `checkpoint` now holds the state being thrown away; it is zeroed on drop.
    }

    pub(crate) fn from_entries(entries: Vec<(Vec<u8>, Vec<u8>)>) -> TinyProvider {
        let provider = TinyProvider::default();
        {
            let mut values = provider
                .storage
                .values
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            values.extend(entries);
        }
        provider
    }
}
