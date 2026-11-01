//! Android's sign-in store: a keyring backend that keeps each entry in `sessions.json` in the app's
//! private data directory, mode 0600. No keyring is linked on Android, so without this the
//! platform default is keyring's in-memory mock, which keeps nothing past the process.
//!
//! Every read and write holds one lock; a write replaces the whole file, through a temporary file
//! and a rename, so a crash leaves the old contents or the new ones.

use base64::Engine;
use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence};
use keyring::{Error, Result};
use std::any::Any;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static FILE: OnceLock<PathBuf> = OnceLock::new();
static ONE: Mutex<()> = Mutex::new(());

/// Makes this store the default for `keyring::Entry`, with `sessions.json` in `dir`.
pub fn install(dir: &Path) {
    if FILE.set(dir.join("sessions.json")).is_ok() {
        keyring::set_default_credential_builder(Box::new(Builder));
    }
}

/// Whether [`install`] ran, so the store keeps sign-ins until they are deleted.
pub fn installed() -> bool {
    FILE.get().is_some()
}

fn failed(err: impl std::fmt::Display) -> Error {
    Error::PlatformFailure(Box::new(std::io::Error::other(err.to_string())))
}

fn path() -> Result<&'static Path> {
    FILE.get().map(PathBuf::as_path).ok_or_else(|| failed("the app's sign-in store is not installed"))
}

/// Every entry in the file. A missing file holds none.
fn read_all(path: &Path) -> Result<BTreeMap<String, String>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(failed),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(err) => Err(failed(err)),
    }
}

fn write_all(path: &Path, entries: &BTreeMap<String, String>) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&temp);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp).map_err(failed)?;
    file.write_all(&serde_json::to_vec(entries).map_err(failed)?).map_err(failed)?;
    file.sync_all().map_err(failed)?;
    std::fs::rename(&temp, path).map_err(failed)
}

#[derive(Debug)]
struct Builder;

impl CredentialBuilderApi for Builder {
    fn build(&self, _target: Option<&str>, service: &str, user: &str) -> Result<Box<Credential>> {
        Ok(Box::new(Slot(format!("{service}/{user}"))))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::UntilDelete
    }
}

#[derive(Debug)]
struct Slot(String);

impl CredentialApi for Slot {
    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        let path = path()?;
        let _one = ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = read_all(path)?;
        entries.insert(self.0.clone(), base64::engine::general_purpose::STANDARD.encode(secret));
        write_all(path, &entries)
    }

    fn get_secret(&self) -> Result<Vec<u8>> {
        let path = path()?;
        let _one = ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries = read_all(path)?;
        let encoded = entries.get(&self.0).ok_or(Error::NoEntry)?;
        base64::engine::general_purpose::STANDARD.decode(encoded).map_err(failed)
    }

    fn delete_credential(&self) -> Result<()> {
        let path = path()?;
        let _one = ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = read_all(path)?;
        if entries.remove(&self.0).is_none() {
            return Err(Error::NoEntry);
        }
        write_all(path, &entries)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
