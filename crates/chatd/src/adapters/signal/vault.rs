//! The Signal state of one account, as presage asks for it: identity and prekeys, sessions, group
//! keys, contacts, profile keys. One file under `signal/` in the support directory, a table of
//! names and values, each value sealed (AES-256-GCM-SIV, the name as associated data) with the key
//! the keychain holds. It is SQLite through chatd's own rusqlite, in a file of its own: chatd's
//! store never sees it, and nothing here is in chatd's store.
//!
//! Messages are not kept: they go to chatd's store as they arrive. Avatars and sticker packs are
//! not kept either. Names (who a session or key is for) are left in the clear so a prefix can be
//! listed; what they name is sealed.
//!
//! A new identity key for someone is trusted, as presage's own stores do by default: tinyclient
//! does not show safety numbers yet.

use aes_gcm_siv::aead::{Aead, KeyInit, Payload};
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use async_trait::async_trait;
use presage::libsignal_service::libsignal_account_keys::AccountEntropyPool;
use presage::libsignal_service::pre_keys::{KyberPreKeyStoreExt, PreKeysStore};
use presage::libsignal_service::prelude::{Content, MasterKey, ProfileKey, Uuid};
use presage::libsignal_service::protocol::{
    Direction, GenericSignedPreKey, IdentityChange, IdentityKey, IdentityKeyPair, IdentityKeyStore, KyberPreKeyId,
    KyberPreKeyRecord, KyberPreKeyStore, PreKeyId, PreKeyRecord, PreKeyStore, ProtocolAddress, ProtocolStore, PublicKey,
    SenderCertificate, SenderKeyRecord, SenderKeyStore, ServiceId, SessionRecord, SessionStore, SignalProtocolError,
    SignedPreKeyId, SignedPreKeyRecord, SignedPreKeyStore,
};
use presage::libsignal_service::protocol::DeviceId;
use presage::libsignal_service::session_store::SessionStoreExt;
use presage::libsignal_service::zkgroup::GroupMasterKeyBytes;
use presage::libsignal_service::Profile;
use presage::manager::RegistrationData;
use presage::model::contacts::Contact;
use presage::model::groups::Group;
use presage::store::{ContentsStore, StateStore, StickerPack, Store, StoreError, Thread};
use presage::AvatarBytes;
use rusqlite::{params, Connection, OptionalExtension};
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

/// Highest id Signal takes for a prekey (24 bits).
const MAX_KEY_ID: u32 = 0xFF_FFFF;

#[derive(Debug)]
pub struct VaultError(String);

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for VaultError {}
impl StoreError for VaultError {}

impl From<rusqlite::Error> for VaultError {
    fn from(err: rusqlite::Error) -> Self {
        VaultError(err.to_string())
    }
}

impl From<serde_json::Error> for VaultError {
    fn from(err: serde_json::Error) -> Self {
        VaultError(err.to_string())
    }
}

impl From<SignalProtocolError> for VaultError {
    fn from(err: SignalProtocolError) -> Self {
        VaultError(err.to_string())
    }
}

impl From<VaultError> for SignalProtocolError {
    fn from(err: VaultError) -> Self {
        SignalProtocolError::InvalidState("signal state", err.0)
    }
}

type Held<T> = Result<T, VaultError>;

struct Inner {
    db: Mutex<Connection>,
    cipher: Aes256GcmSiv,
    path: PathBuf,
}

/// One account's state. Cloned freely: every clone is the same file.
#[derive(Clone)]
pub struct Vault(Arc<Inner>);

/// Where an account's state lives.
pub fn path_of(name: &str) -> PathBuf {
    protocol::support_dir().join("signal").join(format!("{name}.state"))
}

/// The file and what SQLite keeps beside it.
pub fn remove(path: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}

/// A fresh key for a new state, 32 random bytes.
pub fn new_key() -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|err| err.to_string())?;
    Ok(key)
}

impl Vault {
    pub fn open(path: &Path, key: &[u8]) -> Result<Vault, String> {
        let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|_| "the Signal state key is the wrong length".to_owned())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let db = Connection::open(path).map_err(|err| err.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        db.execute_batch(
            "PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL; PRAGMA cache_size = -256;
             CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID;",
        )
        .map_err(|err| err.to_string())?;
        let vault = Vault(Arc::new(Inner { db: Mutex::new(db), cipher, path: path.to_owned() }));
        // A key that does not open what is there is the wrong key: say so now, not mid-message.
        if let Some(name) = vault.first_name().map_err(|err| err.0)? {
            vault.get(&name).map_err(|_| "the Signal state does not open with the key in the keychain".to_owned())?;
        }
        Ok(vault)
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.0.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn first_name(&self) -> Held<Option<String>> {
        Ok(self.db().query_row("SELECT k FROM kv LIMIT 1", [], |row| row.get(0)).optional()?)
    }

    fn seal(&self, name: &str, value: &[u8]) -> Held<Vec<u8>> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|err| VaultError(err.to_string()))?;
        let sealed = self.0.cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: value, aad: name.as_bytes() });
        let sealed = sealed.map_err(|_| VaultError("could not seal a value".into()))?;
        Ok([&nonce[..], &sealed].concat())
    }

    fn open_value(&self, name: &str, sealed: &[u8]) -> Held<Vec<u8>> {
        if sealed.len() < 12 {
            return Err(VaultError(format!("{name}: too short")));
        }
        let (nonce, body) = sealed.split_at(12);
        let opened = self.0.cipher.decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad: name.as_bytes() });
        opened.map_err(|_| VaultError(format!("{name}: does not open")))
    }

    pub fn get(&self, name: &str) -> Held<Option<Vec<u8>>> {
        let sealed: Option<Vec<u8>> = self.db().query_row("SELECT v FROM kv WHERE k = ?1", [name], |row| row.get(0)).optional()?;
        sealed.map(|sealed| self.open_value(name, &sealed)).transpose()
    }

    pub fn put(&self, name: &str, value: &[u8]) -> Held<()> {
        let sealed = self.seal(name, value)?;
        self.db().execute("INSERT OR REPLACE INTO kv (k, v) VALUES (?1, ?2)", params![name, sealed])?;
        Ok(())
    }

    /// True if it was there.
    pub fn remove(&self, name: &str) -> Held<bool> {
        Ok(self.db().execute("DELETE FROM kv WHERE k = ?1", [name])? > 0)
    }

    fn remove_prefix(&self, prefix: &str) -> Held<usize> {
        Ok(self.db().execute("DELETE FROM kv WHERE substr(k, 1, length(?1)) = ?1", [prefix])?)
    }

    /// The names under a prefix, in order.
    fn names(&self, prefix: &str) -> Held<Vec<String>> {
        let db = self.db();
        let mut query = db.prepare("SELECT k FROM kv WHERE substr(k, 1, length(?1)) = ?1 ORDER BY k")?;
        let names = query.query_map([prefix], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
        Ok(names)
    }

    fn values(&self, prefix: &str) -> Held<Vec<(String, Vec<u8>)>> {
        let rows: Vec<(String, Vec<u8>)> = {
            let db = self.db();
            let mut query = db.prepare("SELECT k, v FROM kv WHERE substr(k, 1, length(?1)) = ?1 ORDER BY k")?;
            let rows = query.query_map([prefix], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            rows
        };
        rows.into_iter().map(|(name, sealed)| Ok((name.clone(), self.open_value(&name, &sealed)?))).collect()
    }

    fn json<T: serde::de::DeserializeOwned>(&self, name: &str) -> Held<Option<T>> {
        self.get(name)?.map(|bytes| serde_json::from_slice(&bytes).map_err(VaultError::from)).transpose()
    }

    fn put_json<T: serde::Serialize>(&self, name: &str, value: &T) -> Held<()> {
        self.put(name, &serde_json::to_vec(value)?)
    }

    /// A small flag the wire keeps for itself: asked once, done.
    pub fn flag(&self, name: &str) -> bool {
        self.get(&format!("flag.{name}")).ok().flatten().is_some()
    }

    pub fn set_flag(&self, name: &str) {
        let _ = self.put(&format!("flag.{name}"), b"1");
    }

    /// The registration's account password, for this device's own requests outside presage
    /// (unlinking itself). Never logged.
    pub fn password(&self) -> Option<String> {
        let reg: serde_json::Value = self.json("reg").ok().flatten()?;
        reg.get("password")?.as_str().map(str::to_owned)
    }

    /// Every group the account is in, with its master key.
    pub fn group_keys(&self) -> Held<Vec<(GroupMasterKeyBytes, Group)>> {
        let mut groups = Vec::new();
        for (name, bytes) in self.values("group.")? {
            let Some(key) = name.strip_prefix("group.").and_then(unhex32) else {
                continue;
            };
            groups.push((key, serde_json::from_slice(&bytes)?));
        }
        Ok(groups)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (at, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(at * 2..at * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn id_of(name: &str) -> Option<u32> {
    name.rsplit('.').next()?.parse().ok()
}

// The account's own state.

impl StateStore for Vault {
    type StateStoreError = VaultError;

    async fn load_registration_data(&self) -> Held<Option<RegistrationData>> {
        self.json("reg")
    }

    async fn set_aci_identity_key_pair(&self, key_pair: IdentityKeyPair) -> Held<()> {
        self.put("aci.identity", &key_pair.serialize())
    }

    async fn set_pni_identity_key_pair(&self, key_pair: IdentityKeyPair) -> Held<()> {
        self.put("pni.identity", &key_pair.serialize())
    }

    async fn save_registration_data(&mut self, state: &RegistrationData) -> Held<()> {
        self.put_json("reg", state)
    }

    async fn sender_certificate(&self) -> Held<Option<SenderCertificate>> {
        Ok(self.get("sender_cert")?.map(|bytes| SenderCertificate::deserialize(&bytes)).transpose()?)
    }

    async fn save_sender_certificate(&self, certificate: &SenderCertificate) -> Held<()> {
        self.put("sender_cert", certificate.serialized()?)
    }

    async fn is_registered(&self) -> bool {
        self.get("reg").ok().flatten().is_some()
    }

    async fn clear_registration(&mut self) -> Held<()> {
        for name in ["reg", "sender_cert", "master_key", "aep"] {
            self.remove(name)?;
        }
        self.remove_prefix("aci.")?;
        self.remove_prefix("pni.")?;
        Ok(())
    }

    async fn fetch_master_key(&self) -> Held<Option<MasterKey>> {
        let bytes = self.get("master_key")?;
        bytes.map(|bytes| MasterKey::from_slice(&bytes).map_err(|err| VaultError(err.to_string()))).transpose()
    }

    async fn store_master_key(&self, master_key: Option<&MasterKey>) -> Held<()> {
        match master_key {
            Some(key) => self.put("master_key", &key.inner),
            None => self.remove("master_key").map(drop),
        }
    }

    async fn fetch_account_entropy_pool(&self) -> Held<Option<AccountEntropyPool>> {
        let Some(bytes) = self.get("aep")? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|err| VaultError(err.to_string()))?;
        AccountEntropyPool::from_str(&text).map(Some).map_err(|err| VaultError(format!("{err:?}")))
    }

    async fn store_account_entropy_pool(&self, aep: Option<&AccountEntropyPool>) -> Held<()> {
        match aep {
            Some(aep) => self.put("aep", aep.to_string().as_bytes()),
            None => self.remove("aep").map(drop),
        }
    }
}

// What presage would keep of conversations. Only what sending and naming need is kept.

impl ContentsStore for Vault {
    type ContentsStoreError = VaultError;
    type ContactsIter = std::vec::IntoIter<Held<Contact>>;
    type GroupsIter = std::vec::IntoIter<Held<(GroupMasterKeyBytes, Group)>>;
    type MessagesIter = std::vec::IntoIter<Held<Content>>;
    type StickerPacksIter = std::vec::IntoIter<Held<StickerPack>>;

    async fn clear_profiles(&mut self) -> Held<()> {
        self.remove_prefix("profile.")?;
        Ok(())
    }

    async fn clear_contents(&mut self) -> Held<()> {
        for prefix in ["contact.", "group.", "profile.", "profile_key."] {
            self.remove_prefix(prefix)?;
        }
        Ok(())
    }

    async fn clear_messages(&mut self) -> Held<()> {
        Ok(())
    }

    async fn clear_thread(&mut self, _thread: &Thread) -> Held<()> {
        Ok(())
    }

    /// Messages live in chatd's store, not here.
    async fn save_message(&self, _thread: &Thread, _message: Content) -> Held<()> {
        Ok(())
    }

    async fn delete_message(&mut self, _thread: &Thread, _timestamp: u64) -> Held<bool> {
        Ok(false)
    }

    async fn message(&self, _thread: &Thread, _timestamp: u64) -> Held<Option<Content>> {
        Ok(None)
    }

    async fn thread_for_sender_and_timestamp(&self, _sender: &ServiceId, _timestamp: u64) -> Held<Option<Thread>> {
        Ok(None)
    }

    async fn messages(&self, _thread: &Thread, _range: impl RangeBounds<u64>) -> Held<Self::MessagesIter> {
        Ok(Vec::new().into_iter())
    }

    async fn clear_contacts(&mut self) -> Held<()> {
        self.remove_prefix("contact.")?;
        Ok(())
    }

    async fn save_contact(&mut self, contact: &Contact) -> Held<()> {
        self.put_json(&format!("contact.{}", contact.uuid), contact)
    }

    async fn contacts(&self) -> Held<Self::ContactsIter> {
        let contacts: Vec<Held<Contact>> =
            self.values("contact.")?.into_iter().map(|(_, bytes)| serde_json::from_slice(&bytes).map_err(VaultError::from)).collect();
        Ok(contacts.into_iter())
    }

    async fn contact_by_id(&self, id: &ServiceId) -> Held<Option<Contact>> {
        self.json(&format!("contact.{}", id.raw_uuid()))
    }

    async fn clear_groups(&mut self) -> Held<()> {
        self.remove_prefix("group.")?;
        Ok(())
    }

    async fn save_group(&self, master_key: GroupMasterKeyBytes, group: impl Into<Group>) -> Held<()> {
        self.put_json(&format!("group.{}", hex(&master_key)), &group.into())
    }

    async fn groups(&self) -> Held<Self::GroupsIter> {
        Ok(self.group_keys()?.into_iter().map(Ok).collect::<Vec<_>>().into_iter())
    }

    async fn group(&self, master_key: GroupMasterKeyBytes) -> Held<Option<Group>> {
        self.json(&format!("group.{}", hex(&master_key)))
    }

    async fn save_group_avatar(&self, _master_key: GroupMasterKeyBytes, _avatar: &AvatarBytes) -> Held<()> {
        Ok(())
    }

    async fn group_avatar(&self, _master_key: GroupMasterKeyBytes) -> Held<Option<AvatarBytes>> {
        Ok(None)
    }

    async fn upsert_profile_key(&mut self, uuid: &Uuid, key: ProfileKey) -> Held<bool> {
        let name = format!("profile_key.{uuid}");
        let bytes = key.get_bytes();
        if self.get(&name)?.as_deref() == Some(&bytes[..]) {
            return Ok(false);
        }
        self.put(&name, &bytes)?;
        Ok(true)
    }

    async fn profile_key(&self, service_id: &ServiceId) -> Held<Option<ProfileKey>> {
        let Some(bytes) = self.get(&format!("profile_key.{}", service_id.raw_uuid()))? else {
            return Ok(None);
        };
        let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| VaultError("a profile key is the wrong length".into()))?;
        Ok(Some(ProfileKey::create(bytes)))
    }

    async fn save_profile(&mut self, uuid: Uuid, key: ProfileKey, profile: Profile) -> Held<()> {
        self.put_json(&format!("profile.{uuid}.{}", hex(&key.get_bytes())), &profile)
    }

    async fn profile(&self, uuid: Uuid, key: ProfileKey) -> Held<Option<Profile>> {
        self.json(&format!("profile.{uuid}.{}", hex(&key.get_bytes())))
    }

    async fn save_profile_avatar(&mut self, _uuid: Uuid, _key: ProfileKey, _profile: &AvatarBytes) -> Held<()> {
        Ok(())
    }

    async fn profile_avatar(&self, _uuid: Uuid, _key: ProfileKey) -> Held<Option<AvatarBytes>> {
        Ok(None)
    }

    async fn add_sticker_pack(&mut self, _pack: &StickerPack) -> Held<()> {
        Ok(())
    }

    async fn sticker_pack(&self, _id: &[u8]) -> Held<Option<StickerPack>> {
        Ok(None)
    }

    async fn remove_sticker_pack(&mut self, _id: &[u8]) -> Held<bool> {
        Ok(false)
    }

    async fn sticker_packs(&self) -> Held<Self::StickerPacksIter> {
        Ok(Vec::new().into_iter())
    }
}

impl Store for Vault {
    type Error = VaultError;
    type AciStore = Keys;
    type PniStore = Keys;

    async fn clear(&mut self) -> Held<()> {
        self.db().execute("DELETE FROM kv", [])?;
        Ok(())
    }

    fn aci_protocol_store(&self) -> Keys {
        Keys { vault: self.clone(), side: "aci" }
    }

    fn pni_protocol_store(&self) -> Keys {
        Keys { vault: self.clone(), side: "pni" }
    }
}

// The protocol's keys and sessions, for one of the account's two identities.

/// The keys of one identity: the account's (`aci`) or its phone number's (`pni`).
#[derive(Clone)]
pub struct Keys {
    vault: Vault,
    side: &'static str,
}

type Signal<T> = Result<T, SignalProtocolError>;

impl Keys {
    fn name(&self, what: &str) -> String {
        format!("{}.{what}", self.side)
    }

    fn get(&self, what: &str) -> Signal<Option<Vec<u8>>> {
        Ok(self.vault.get(&self.name(what))?)
    }

    fn put(&self, what: &str, value: &[u8]) -> Signal<()> {
        Ok(self.vault.put(&self.name(what), value)?)
    }

    fn session_name(address: &ProtocolAddress) -> String {
        format!("session.{}.{:03}", address.name(), u32::from(address.device_id()))
    }

    /// Ids handed out only go up, so a key consumed and removed is never issued again.
    fn next(&self, counter: &str) -> Signal<u32> {
        let next = self.get(&format!("next.{counter}"))?.and_then(|bytes| Some(u32::from_be_bytes(bytes.try_into().ok()?)));
        Ok(next.filter(|id| (1..=MAX_KEY_ID).contains(id)).unwrap_or(1))
    }

    fn issued(&self, counter: &str, id: u32) -> Signal<()> {
        let next = if id >= MAX_KEY_ID { 1 } else { id + 1 };
        if next > self.next(counter)? || id >= MAX_KEY_ID {
            self.put(&format!("next.{counter}"), &next.to_be_bytes())?;
        }
        Ok(())
    }

    fn ids(&self, kind: &str) -> Signal<Vec<u32>> {
        Ok(self.vault.names(&self.name(&format!("{kind}.")))?.iter().filter_map(|name| id_of(name)).collect())
    }

    fn last_resort(&self, id: u32) -> Signal<bool> {
        Ok(self.get(&format!("kyber_last.{id:08}"))?.is_some())
    }

    fn one_time_kyber(&self) -> Signal<Vec<u32>> {
        let mut ids = Vec::new();
        for id in self.ids("kyber")? {
            if !self.last_resort(id)? {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn registration_id(&self) -> Signal<u32> {
        let reg: Option<RegistrationData> = self.vault.json("reg")?;
        let reg = reg.ok_or_else(|| SignalProtocolError::InvalidState("signal state", "not registered".into()))?;
        Ok(match self.side {
            "pni" => reg.pni_registration_id.unwrap_or(reg.registration_id),
            _ => reg.registration_id,
        })
    }
}

#[async_trait(?Send)]
impl IdentityKeyStore for Keys {
    async fn get_identity_key_pair(&self) -> Signal<IdentityKeyPair> {
        let bytes = self.get("identity")?.ok_or_else(|| SignalProtocolError::InvalidState("signal state", "no identity key".into()))?;
        IdentityKeyPair::try_from(bytes.as_slice())
    }

    async fn get_local_registration_id(&self) -> Signal<u32> {
        self.registration_id()
    }

    async fn save_identity(&mut self, address: &ProtocolAddress, identity: &IdentityKey) -> Signal<IdentityChange> {
        let name = format!("trusted.{}", address.name());
        let before = self.get(&name)?;
        self.put(&name, &identity.serialize())?;
        let replaced = before.is_some_and(|before| before.as_slice() != &*identity.serialize());
        Ok(IdentityChange::from_changed(replaced))
    }

    async fn is_trusted_identity(&self, _address: &ProtocolAddress, _identity: &IdentityKey, _direction: Direction) -> Signal<bool> {
        Ok(true)
    }

    async fn get_identity(&self, address: &ProtocolAddress) -> Signal<Option<IdentityKey>> {
        self.get(&format!("trusted.{}", address.name()))?.map(|bytes| IdentityKey::decode(&bytes)).transpose()
    }
}

#[async_trait(?Send)]
impl PreKeyStore for Keys {
    async fn get_pre_key(&self, prekey_id: PreKeyId) -> Signal<PreKeyRecord> {
        let id = u32::from(prekey_id);
        let bytes = self.get(&format!("prekey.{id:08}"))?.ok_or(SignalProtocolError::InvalidPreKeyId)?;
        PreKeyRecord::deserialize(&bytes)
    }

    async fn save_pre_key(&mut self, prekey_id: PreKeyId, record: &PreKeyRecord) -> Signal<()> {
        let id = u32::from(prekey_id);
        self.put(&format!("prekey.{id:08}"), &record.serialize()?)?;
        self.issued("prekey", id)
    }

    async fn remove_pre_key(&mut self, prekey_id: PreKeyId) -> Signal<()> {
        self.vault.remove(&self.name(&format!("prekey.{:08}", u32::from(prekey_id))))?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl SignedPreKeyStore for Keys {
    async fn get_signed_pre_key(&self, signed_prekey_id: SignedPreKeyId) -> Signal<SignedPreKeyRecord> {
        let id = u32::from(signed_prekey_id);
        let bytes = self.get(&format!("signed.{id:08}"))?.ok_or(SignalProtocolError::InvalidSignedPreKeyId)?;
        SignedPreKeyRecord::deserialize(&bytes)
    }

    async fn save_signed_pre_key(&mut self, signed_prekey_id: SignedPreKeyId, record: &SignedPreKeyRecord) -> Signal<()> {
        let id = u32::from(signed_prekey_id);
        self.put(&format!("signed.{id:08}"), &record.serialize()?)?;
        self.put("signed_current", &id.to_be_bytes())?;
        self.issued("signed", id)
    }
}

#[async_trait(?Send)]
impl KyberPreKeyStore for Keys {
    async fn get_kyber_pre_key(&self, kyber_prekey_id: KyberPreKeyId) -> Signal<KyberPreKeyRecord> {
        let id = u32::from(kyber_prekey_id);
        let bytes = self.get(&format!("kyber.{id:08}"))?.ok_or(SignalProtocolError::InvalidKyberPreKeyId)?;
        KyberPreKeyRecord::deserialize(&bytes)
    }

    async fn save_kyber_pre_key(&mut self, kyber_prekey_id: KyberPreKeyId, record: &KyberPreKeyRecord) -> Signal<()> {
        let id = u32::from(kyber_prekey_id);
        self.put(&format!("kyber.{id:08}"), &record.serialize()?)?;
        self.issued("kyber", id)
    }

    /// A one-time key goes once used. A last-resort key stays, and the same use of it twice (a
    /// replayed message) is refused.
    async fn mark_kyber_pre_key_used(&mut self, kyber_prekey_id: KyberPreKeyId, ec_prekey_id: SignedPreKeyId, base_key: &PublicKey) -> Signal<()> {
        let id = u32::from(kyber_prekey_id);
        if !self.last_resort(id)? {
            self.vault.remove(&self.name(&format!("kyber.{id:08}")))?;
            self.vault.remove(&self.name(&format!("kyber_stale.{id:08}")))?;
            return Ok(());
        }
        let seen = format!("kyber_used.{id:08}.{}.{}", u32::from(ec_prekey_id), hex(&base_key.serialize()));
        if self.get(&seen)?.is_some() {
            return Err(SignalProtocolError::InvalidMessage(presage::libsignal_service::protocol::CiphertextMessageType::PreKey, "a last-resort key was used twice the same way"));
        }
        self.put(&seen, b"")
    }
}

#[async_trait(?Send)]
impl KyberPreKeyStoreExt for Keys {
    async fn store_last_resort_kyber_pre_key(&mut self, kyber_prekey_id: KyberPreKeyId, record: &KyberPreKeyRecord) -> Signal<()> {
        let id = u32::from(kyber_prekey_id);
        self.save_kyber_pre_key(kyber_prekey_id, record).await?;
        self.put(&format!("kyber_last.{id:08}"), b"")?;
        self.put("kyber_last_current", &id.to_be_bytes())
    }

    async fn load_last_resort_kyber_pre_keys(&self) -> Signal<Vec<KyberPreKeyRecord>> {
        let mut records = Vec::new();
        for id in self.ids("kyber")? {
            if self.last_resort(id)? {
                if let Some(bytes) = self.get(&format!("kyber.{id:08}"))? {
                    records.push(KyberPreKeyRecord::deserialize(&bytes)?);
                }
            }
        }
        Ok(records)
    }

    async fn remove_kyber_pre_key(&mut self, kyber_prekey_id: KyberPreKeyId) -> Signal<()> {
        let id = u32::from(kyber_prekey_id);
        for what in ["kyber", "kyber_last", "kyber_stale"] {
            self.vault.remove(&self.name(&format!("{what}.{id:08}")))?;
        }
        self.vault.remove_prefix(&self.name(&format!("kyber_used.{id:08}.")))?;
        Ok(())
    }

    async fn mark_all_one_time_kyber_pre_keys_stale_if_necessary(&mut self, stale_time: chrono::DateTime<chrono::Utc>) -> Signal<()> {
        for id in self.one_time_kyber()? {
            let name = format!("kyber_stale.{id:08}");
            if self.get(&name)?.is_none() {
                self.put(&name, &stale_time.timestamp_millis().to_be_bytes())?;
            }
        }
        Ok(())
    }

    /// Stale one-time keys older than `threshold` go, oldest first, while more than `min_count`
    /// one-time keys are left.
    async fn delete_all_stale_one_time_kyber_pre_keys(&mut self, threshold: chrono::DateTime<chrono::Utc>, min_count: usize) -> Signal<()> {
        let one_time = self.one_time_kyber()?;
        let mut left = one_time.len();
        for id in one_time {
            if left <= min_count {
                break;
            }
            let stale = self.get(&format!("kyber_stale.{id:08}"))?.and_then(|bytes| Some(i64::from_be_bytes(bytes.try_into().ok()?)));
            if stale.is_some_and(|stale| stale < threshold.timestamp_millis()) {
                self.remove_kyber_pre_key(KyberPreKeyId::from(id)).await?;
                left -= 1;
            }
        }
        Ok(())
    }
}

#[async_trait(?Send)]
impl PreKeysStore for Keys {
    async fn next_pre_key_id(&self) -> Signal<u32> {
        self.next("prekey")
    }

    async fn next_signed_pre_key_id(&self) -> Signal<u32> {
        self.next("signed")
    }

    async fn next_pq_pre_key_id(&self) -> Signal<u32> {
        self.next("kyber")
    }

    async fn signed_pre_keys_count(&self) -> Signal<usize> {
        Ok(self.ids("signed")?.len())
    }

    async fn kyber_pre_keys_count(&self, last_resort: bool) -> Signal<usize> {
        let mut count = 0;
        for id in self.ids("kyber")? {
            if self.last_resort(id)? == last_resort {
                count += 1;
            }
        }
        Ok(count)
    }

    async fn signed_prekey_id(&self) -> Signal<Option<SignedPreKeyId>> {
        let current = self.get("signed_current")?.and_then(|bytes| Some(u32::from_be_bytes(bytes.try_into().ok()?)));
        Ok(current.filter(|id| self.ids("signed").is_ok_and(|ids| ids.contains(id))).map(SignedPreKeyId::from))
    }

    async fn last_resort_kyber_prekey_id(&self) -> Signal<Option<KyberPreKeyId>> {
        let current = self.get("kyber_last_current")?.and_then(|bytes| Some(u32::from_be_bytes(bytes.try_into().ok()?)));
        Ok(current.filter(|id| self.last_resort(*id).unwrap_or(false)).map(KyberPreKeyId::from))
    }
}

#[async_trait(?Send)]
impl SessionStore for Keys {
    async fn load_session(&self, address: &ProtocolAddress) -> Signal<Option<SessionRecord>> {
        self.get(&Self::session_name(address))?.map(|bytes| SessionRecord::deserialize(&bytes)).transpose()
    }

    async fn store_session(&mut self, address: &ProtocolAddress, record: &SessionRecord) -> Signal<()> {
        self.put(&Self::session_name(address), &record.serialize()?)
    }
}

#[async_trait(?Send)]
impl SessionStoreExt for Keys {
    async fn get_sub_device_sessions(&self, name: &ServiceId) -> Signal<Vec<DeviceId>> {
        let prefix = self.name(&format!("session.{}.", name.service_id_string()));
        let devices = self.vault.names(&prefix)?.iter().filter_map(|session| id_of(session)).filter(|device| *device != 1).collect::<Vec<u32>>();
        Ok(devices.into_iter().filter_map(|device| DeviceId::try_from(device).ok()).collect())
    }

    async fn delete_session(&self, address: &ProtocolAddress) -> Signal<()> {
        if !self.vault.remove(&self.name(&Self::session_name(address)))? {
            return Err(SignalProtocolError::SessionNotFound(address.clone()));
        }
        Ok(())
    }

    async fn delete_all_sessions(&self, address: &ServiceId) -> Signal<usize> {
        Ok(self.vault.remove_prefix(&self.name(&format!("session.{}.", address.service_id_string())))?)
    }
}

#[async_trait(?Send)]
impl SenderKeyStore for Keys {
    async fn store_sender_key(&mut self, sender: &ProtocolAddress, distribution_id: Uuid, record: &SenderKeyRecord) -> Signal<()> {
        self.put(&format!("sender_key.{}.{:03}.{distribution_id}", sender.name(), u32::from(sender.device_id())), &record.serialize()?)
    }

    async fn load_sender_key(&mut self, sender: &ProtocolAddress, distribution_id: Uuid) -> Signal<Option<SenderKeyRecord>> {
        let name = format!("sender_key.{}.{:03}.{distribution_id}", sender.name(), u32::from(sender.device_id()));
        self.get(&name)?.map(|bytes| SenderKeyRecord::deserialize(&bytes)).transpose()
    }
}

impl ProtocolStore for Keys {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_opens_only_with_its_key_and_under_its_own_name() {
        let path = std::env::temp_dir().join(format!("tinyclient-vault-{}.state", std::process::id()));
        remove(&path);
        let key = new_key().unwrap();
        let vault = Vault::open(&path, &key).unwrap();
        vault.put("aci.identity", b"secret bytes").unwrap();
        assert_eq!(vault.get("aci.identity").unwrap().as_deref(), Some(&b"secret bytes"[..]));
        // On disk the value is sealed.
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(12).any(|window| window == b"secret bytes"));
        // A value moved to another name does not open there.
        let sealed: Vec<u8> = vault.db().query_row("SELECT v FROM kv WHERE k = 'aci.identity'", [], |row| row.get(0)).unwrap();
        vault.db().execute("INSERT INTO kv (k, v) VALUES ('pni.identity', ?1)", [sealed]).unwrap();
        assert!(vault.get("pni.identity").is_err());
        drop(vault);
        assert!(Vault::open(&path, &new_key().unwrap()).is_err(), "another key does not open it");
        remove(&path);
    }

    #[test]
    fn key_ids_only_go_up() {
        let path = std::env::temp_dir().join(format!("tinyclient-vault-ids-{}.state", std::process::id()));
        remove(&path);
        let keys = Vault::open(&path, &new_key().unwrap()).unwrap().aci_protocol_store();
        assert_eq!(keys.next("prekey").unwrap(), 1);
        keys.issued("prekey", 41).unwrap();
        keys.issued("prekey", 7).unwrap();
        assert_eq!(keys.next("prekey").unwrap(), 42);
        keys.issued("prekey", MAX_KEY_ID).unwrap();
        assert_eq!(keys.next("prekey").unwrap(), 1);
        remove(&path);
    }
}
