//! The OpenMLS provider: RustCrypto for the primitives and an in-memory storage that the engine
//! snapshots (sealed) to a [`crate::store::Store`]. The group state, the key packages' private
//! halves and the epoch secrets live in that storage.

use openmls_rust_crypto::{MemoryStorage, RustCrypto};
use openmls_traits::OpenMlsProvider;

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
