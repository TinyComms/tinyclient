//! The engine: one device's whole client-side state (identity, trust, MLS groups, key packages)
//! and the operations on it. The other halves of `impl Engine` are in [`crate::groups`] and
//! [`crate::messaging`].
//!
//! State lives in memory and, when the engine is opened with a [`Store`], is written through it as
//! one sealed snapshot (AES-256-GCM, a key the caller keeps in the OS keychain) before any
//! operation that must not be repeated returns: making key packages, creating a group, committing
//! and encrypting. Receiving operations write too, but report a failed write on the next
//! [`Engine::flush`] and not as a failure of what they decrypted.

use crate::error::{Error, Result};
use crate::identity::{DeviceIdentity, DeviceKey, Trust};
use crate::ids::{ConversationId, DeviceId, DeviceRef, UserId};
use crate::provider::TinyProvider;
use crate::store::{open_state, seal_state, StateKey, Store};
use crate::wire::Bytes;
use openmls::prelude as mls;
use openmls::prelude::tls_codec::Serialize as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use zeroize::Zeroizing;

/// `MLS_128_DHKEMP256_AES128GCM_SHA256_P256` (0x0002), the ciphersuite of calls.
pub const CIPHERSUITE: mls::Ciphersuite = mls::Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256;
/// The most key packages one upload may hold (the server's limit).
pub const MAX_KEY_PACKAGES: usize = 50;
/// Regular key packages are good for 84 days (OpenMLS's own range limit for a leaf's lifetime).
const KEY_PACKAGE_LIFETIME: u64 = 60 * 60 * 24 * 84;
const SNAPSHOT_VERSION: u32 = 1;

/// What every leaf of ours says it supports: this ciphersuite, basic credentials, and the last-resort
/// key package extension (a key package that carries an extension must list it).
pub(crate) fn capabilities() -> mls::Capabilities {
    mls::Capabilities::new(
        None,
        Some(&[CIPHERSUITE]),
        Some(&[mls::ExtensionType::LastResort]),
        None,
        Some(&[mls::CredentialType::Basic]),
    )
}

/// Tunables, with the values the protocol was designed with. Not persisted; set them after
/// opening if you need other numbers.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Past epochs whose message keys are kept so that a message sent just before a commit still
    /// opens. Each is a window in which an old key could leak; 4 covers a normal reorder.
    pub max_past_epochs: usize,
    /// How far out of order, within one epoch and sender, a message may arrive.
    pub out_of_order_tolerance: u32,
    /// How many messages ahead of the last one seen from a sender are skipped.
    pub maximum_forward_distance: u32,
    /// How many applied commits each conversation remembers (by hash), to tell a commit shown again
    /// from a rewritten history. A commit for an epoch older than these is taken for shown again:
    /// it cannot be told apart, and `observe_server_epoch` is what notices a server gone back.
    pub commits_remembered: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            max_past_epochs: 4,
            out_of_order_tolerance: 64,
            maximum_forward_distance: 2000,
            commits_remembered: 256,
        }
    }
}

/// What this device knows about one conversation, besides the MLS group in the storage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ConvMeta {
    /// The pinned server external sender signing key (65 bytes).
    pub server_key: Bytes,
    /// The highest epoch this device has been in.
    pub high_epoch: u64,
    /// The epoch this device joined at (0 for the creator). Commits made before it are history
    /// the handshake feed still shows a new device, not a rewritten one.
    pub joined_epoch: u64,
    /// Set when the group cannot be trusted any more (the epoch went backwards).
    pub frozen: Option<String>,
    /// This device was removed (or never joined).
    pub removed: bool,
    /// A Remove naming this device is pending: it cannot commit, and must not send.
    pub self_removal_pending: bool,
    /// The server showed another external sender key than the pinned one.
    pub server_key_alert: bool,
    /// `(epoch the commit was made at, SHA-256 of the commit)`, oldest first.
    pub applied: Vec<(u64, Bytes)>,
    /// Our commit that waits for the server's answer, kept whole so that a restart (or a lost
    /// reply) can send it again: see [`Engine::pending_commit`].
    pub pending_commit: Option<crate::groups::StoredBundle>,
}

/// Where a device keeps its state and under which key.
struct Persistence {
    store: Box<dyn Store + Send>,
    key: StateKey,
    name: String,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    user: UserId,
    device: DeviceId,
    signing_key: Bytes,
    trust: Trust,
    meta: BTreeMap<ConversationId, ConvMeta>,
    storage: Vec<(Bytes, Bytes)>,
}

pub struct Engine {
    pub(crate) identity: DeviceIdentity,
    pub(crate) trust: Trust,
    pub(crate) provider: TinyProvider,
    pub(crate) meta: BTreeMap<ConversationId, ConvMeta>,
    /// Groups loaded from the storage. Dropped after any failure that could have half-changed
    /// one, so the next use reloads the last stored state.
    pub(crate) groups: HashMap<ConversationId, mls::MlsGroup>,
    pub(crate) config: Config,
    persistence: Option<Persistence>,
    dirty: bool,
    save_error: Option<String>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("user", &self.identity.user)
            .field("device", &self.identity.device)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// A new device for `user`, kept only in memory. Use [`Engine::create`] to persist it.
    pub fn detached(user: UserId) -> Result<Engine> {
        Ok(Engine::with_identity(DeviceIdentity::generate(user)?))
    }

    /// A device with the given identity, kept only in memory.
    pub fn with_identity(identity: DeviceIdentity) -> Engine {
        Engine {
            identity,
            trust: Trust::new(),
            provider: TinyProvider::default(),
            meta: BTreeMap::new(),
            groups: HashMap::new(),
            config: Config::default(),
            persistence: None,
            dirty: true,
            save_error: None,
        }
    }

    /// A new device for `user` that writes its state to `store` under `name` (a plain file
    /// name), sealed with `key`. Fails if `store` already holds state under `name`: open it.
    pub fn create(
        user: UserId,
        store: Box<dyn Store + Send>,
        key: StateKey,
        name: &str,
    ) -> Result<Engine> {
        if store.load(name)?.is_some() {
            return Err(Error::Storage("state already exists".into()));
        }
        let mut engine = Engine::detached(user)?;
        engine.persistence = Some(Persistence {
            store,
            key,
            name: name.to_owned(),
        });
        engine.persist(true)?;
        Ok(engine)
    }

    /// Opens the state under `name`: `None` if there is none. A blob that does not authenticate
    /// (the wrong key, a damaged or swapped file) is an error and is never treated as empty.
    pub fn open(store: Box<dyn Store + Send>, key: StateKey, name: &str) -> Result<Option<Engine>> {
        let Some(blob) = store.load(name)? else {
            return Ok(None);
        };
        let mut engine = Engine::from_snapshot(&key, name, &blob)?;
        engine.persistence = Some(Persistence {
            store,
            key,
            name: name.to_owned(),
        });
        Ok(Some(engine))
    }

    /// The sealed snapshot of everything, for callers that keep the bytes themselves.
    pub fn snapshot(&self, key: &StateKey, name: &str) -> Result<Vec<u8>> {
        let plain = self.snapshot_plain()?;
        seal_state(key, name, &plain)
    }

    pub fn from_snapshot(key: &StateKey, name: &str, blob: &[u8]) -> Result<Engine> {
        let plain = open_state(key, name, blob)?;
        let snapshot: Snapshot =
            serde_json::from_slice(&plain).map_err(|_| Error::Malformed("state"))?;
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(Error::UnsupportedVersion);
        }
        let signing = DeviceKey::from_bytes(&snapshot.signing_key)?;
        let entries = snapshot
            .storage
            .into_iter()
            .map(|(key, value)| (key.0, value.0))
            .collect();
        Ok(Engine {
            identity: DeviceIdentity {
                user: snapshot.user,
                device: snapshot.device,
                key: signing,
            },
            trust: snapshot.trust,
            provider: TinyProvider::from_entries(entries),
            meta: snapshot.meta,
            groups: HashMap::new(),
            config: Config::default(),
            persistence: None,
            dirty: false,
            save_error: None,
        })
    }

    fn snapshot_plain(&self) -> Result<Zeroizing<Vec<u8>>> {
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            user: self.identity.user,
            device: self.identity.device,
            signing_key: Bytes(self.identity.key.secret_bytes().to_vec()),
            trust: self.trust.clone(),
            meta: self.meta.clone(),
            storage: self
                .provider
                .entries()
                .into_iter()
                .map(|(key, value)| (Bytes(key), Bytes(value)))
                .collect(),
        };
        serde_json::to_vec(&snapshot)
            .map(Zeroizing::new)
            .map_err(|_| Error::Malformed("state"))
    }

    /// Writes the state if anything changed since the last write; returns the last failure of a
    /// lenient write, if any. Call it after a batch of receiving operations.
    pub fn flush(&mut self) -> Result<()> {
        if self.dirty {
            self.persist(true)?;
        }
        match self.save_error.take() {
            Some(error) => Err(Error::Storage(error)),
            None => Ok(()),
        }
    }

    /// Replaces the tunables (see [`Config`]). Takes effect for groups made or joined afterwards.
    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    /// Writes the state through the store. `strict` returns a failure; otherwise it is kept for
    /// [`Engine::flush`].
    pub(crate) fn persist(&mut self, strict: bool) -> Result<()> {
        self.dirty = true;
        if self.persistence.is_none() {
            self.dirty = false;
            return Ok(());
        }
        let plain = self.snapshot_plain()?;
        let result = match &self.persistence {
            Some(persistence) => seal_state(&persistence.key, &persistence.name, &plain)
                .and_then(|blob| persistence.store.save(&persistence.name, &blob)),
            None => Ok(()),
        };
        match result {
            Ok(()) => {
                self.dirty = false;
                self.save_error = None;
                Ok(())
            }
            Err(error) if strict => Err(error),
            Err(error) => {
                self.save_error = Some(error.to_string());
                Ok(())
            }
        }
    }

    // --- Identity ---

    pub fn user_id(&self) -> UserId {
        self.identity.user
    }

    pub fn device_id(&self) -> DeviceId {
        self.identity.device
    }

    /// This device's signing public key, uncompressed (65 bytes): the `signature_key` of
    /// `PUT /v1/e2ee/devices/{id}`.
    pub fn signature_key(&self) -> Vec<u8> {
        self.identity.key.public_key()
    }

    pub fn device_ref(&self) -> DeviceRef {
        self.identity.device_ref()
    }

    /// Approves a new device of this person: see [`DeviceIdentity::cross_sign`].
    pub fn cross_sign(&self, new_device: DeviceId, new_signature_key: &[u8]) -> Result<Vec<u8>> {
        self.identity.cross_sign(new_device, new_signature_key)
    }

    /// What is trusted. Verify a device list with [`Engine::verify_devices`] (it persists).
    pub fn trust(&self) -> &Trust {
        &self.trust
    }

    /// Verifies the server's device list for `user` and records the result (see
    /// [`Trust::verify_list`]). Call it for every member whenever the server says their devices
    /// changed (`e2ee.device`) and before a conversation is joined or committed to.
    pub fn verify_devices(
        &mut self,
        user: UserId,
        list: &[crate::identity::DeviceRecord],
    ) -> Result<crate::identity::ChainReport> {
        let report = self.trust.verify_list(user, list)?;
        self.persist(true)?;
        Ok(report)
    }

    /// After the person agrees that `user` has a new identity.
    pub fn accept_new_identity(&mut self, user: UserId) -> Result<()> {
        self.trust.accept_new_identity(user);
        self.persist(true)
    }

    // --- Key packages ---

    pub(crate) fn credential(&self) -> mls::CredentialWithKey {
        mls::CredentialWithKey {
            credential: mls::BasicCredential::new(self.identity.credential().to_vec()).into(),
            signature_key: self.identity.key.public_key().into(),
        }
    }

    /// `count` (1 to 50) fresh key packages, as raw TLS `KeyPackage` bytes (what DAVE and
    /// `POST /v1/e2ee/key_packages` carry). Their private halves are in the state, which is
    /// written before this returns: upload only what this returned.
    pub fn key_packages(&mut self, count: usize) -> Result<Vec<Vec<u8>>> {
        if count == 0 || count > MAX_KEY_PACKAGES {
            return Err(Error::TooLarge("key package batch"));
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.build_key_package(false)?);
        }
        self.persist(true)?;
        Ok(out)
    }

    /// The last-resort key package: the server keeps it and may hand it out more than once, so
    /// its private half stays in the state for good. It is good for 84 days: replace it before
    /// that.
    pub fn last_resort_key_package(&mut self) -> Result<Vec<u8>> {
        let package = self.build_key_package(true)?;
        self.persist(true)?;
        Ok(package)
    }

    fn build_key_package(&mut self, last_resort: bool) -> Result<Vec<u8>> {
        let mut builder = mls::KeyPackage::builder()
            .key_package_lifetime(mls::Lifetime::new(KEY_PACKAGE_LIFETIME))
            .leaf_node_capabilities(capabilities());
        if last_resort {
            builder = builder.mark_as_last_resort();
        }
        let bundle = builder
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.identity.key,
                self.credential(),
            )
            .map_err(Error::mls)?;
        bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(Error::mls)
    }
}
