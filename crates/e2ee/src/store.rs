//! Where state goes. The crate writes no file of its own: a [`Store`] holds opaque, already
//! encrypted blobs, and the caller picks where (a database row, a file, memory). The two keys here
//! are 32 random bytes the caller keeps in the OS keychain, never beside the data they protect.

use crate::error::{Error, Result};
use crate::sealed;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use zeroize::{Zeroize, ZeroizeOnDrop};

const STATE_LABEL: &[u8] = b"tiny-e2ee-v1 state";
const CACHE_LABEL: &[u8] = b"tiny-e2ee-v1 cache";

macro_rules! key_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Zeroize, ZeroizeOnDrop)]
        pub struct $name([u8; 32]);

        impl $name {
            pub fn generate() -> Result<$name> {
                let mut bytes = [0u8; 32];
                getrandom::fill(&mut bytes).map_err(|_| Error::Random)?;
                Ok($name(bytes))
            }

            pub fn from_bytes(bytes: [u8; 32]) -> $name {
                $name(bytes)
            }

            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    };
}

key_type!(
    /// Seals the MLS state snapshot (`Engine::save`). Lose it and the state is gone: the device
    /// starts over as a new device.
    StateKey
);
key_type!(
    /// Seals the decrypted message cache's plaintext column (`seal_cache_value`).
    CacheKey
);

/// A named blob store. Implementations must make `save` atomic (a reader sees the old blob or
/// the new, never a mixture); the crate treats a blob that does not authenticate as absent state
/// and never trusts partial contents.
pub trait Store {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>>;
    fn save(&self, name: &str, blob: &[u8]) -> Result<()>;
    fn remove(&self, name: &str) -> Result<()>;
}

/// Blobs in memory, for tests and for callers that keep the bytes themselves.
#[derive(Default)]
pub struct MemoryStore {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl Store for MemoryStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .blobs
            .lock()
            .map_err(|_| Error::Storage("poisoned".into()))?
            .get(name)
            .cloned())
    }

    fn save(&self, name: &str, blob: &[u8]) -> Result<()> {
        self.blobs
            .lock()
            .map_err(|_| Error::Storage("poisoned".into()))?
            .insert(name.to_owned(), blob.to_vec());
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<()> {
        self.blobs
            .lock()
            .map_err(|_| Error::Storage("poisoned".into()))?
            .remove(name);
        Ok(())
    }
}

/// Blobs as files in one directory: written to a private temp file in the same directory and
/// renamed over the old one. The only code in this crate besides [`crate::attachment`]'s temp file
/// that touches the filesystem.
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> FileStore {
        FileStore { dir: dir.into() }
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        // A name is a file name: letters, digits, `.`, `_`, `-`.
        let plain = !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !plain {
            return Err(Error::Storage("not a plain name".into()));
        }
        Ok(self.dir.join(name))
    }
}

impl Store for FileStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn save(&self, name: &str, blob: &[u8]) -> Result<()> {
        use std::io::Write;
        let path = self.path(name)?;
        std::fs::create_dir_all(&self.dir)?;
        // `tempfile` creates the file 0600 on Unix.
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
        temp.write_all(blob)?;
        temp.as_file().sync_all()?;
        temp.persist(&path)
            .map_err(|err| Error::Storage(err.error.to_string()))?;
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<()> {
        match std::fs::remove_file(self.path(name)?) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

pub(crate) fn seal_state(key: &StateKey, name: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    sealed::seal(key.as_bytes(), STATE_LABEL, name.as_bytes(), plaintext)
}

pub(crate) fn open_state(
    key: &StateKey,
    name: &str,
    blob: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    sealed::open(key.as_bytes(), STATE_LABEL, name.as_bytes(), blob)
}

/// Encrypts a decrypted message's plaintext for the local cache. `row_id` is the row's id (a
/// message id): it is the associated data, so a ciphertext copied into another row does not
/// open. Anyone with the key and the database can read it, which is the point; the key is
/// elsewhere.
pub fn seal_cache_value(key: &CacheKey, row_id: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    sealed::seal(key.as_bytes(), CACHE_LABEL, row_id, plaintext)
}

/// The reverse of [`seal_cache_value`]; any other key, row id or any altered byte is refused.
pub fn open_cache_value(
    key: &CacheKey,
    row_id: &[u8],
    blob: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    sealed::open(key.as_bytes(), CACHE_LABEL, row_id, blob)
}
