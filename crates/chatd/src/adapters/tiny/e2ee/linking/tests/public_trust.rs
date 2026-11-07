//! Constructive continuity checks for the authenticated device link.
use super::*;

const PEER: &str = "b0b00000-0000-4000-8000-000000000002";

#[test]
fn history_none_keeps_the_approved_public_identity_without_copying_device_state() {
    let relay = Arc::new(Fake::default());
    let run = link_through(
        relay.clone(),
        &[
            ("c1", "m1", opened("private history", 1)),
            ("c2", "m2", opened("also private", 2)),
        ],
        Scope::None,
        |_, _| {},
    );
    assert_eq!(run.new_result, Ok(0));
    assert_eq!(run.old_result, Ok(()));
    let old = lock(&run.old);
    let mut new = lock(&run.new);
    let old_engine = old.engine.as_ref().unwrap();
    let new_engine = new.engine.as_mut().unwrap();
    let peer = UserId::parse(PEER).unwrap();
    assert!(
        new_engine.trust().knows(peer),
        "public peer identity must survive linking even without message history"
    );
    for device in old_engine.trust().devices(peer) {
        assert!(new_engine
            .trust()
            .is_trusted(peer, device.device, &device.signature_key));
    }
    assert!(
        new_engine.trust().is_trusted(
            old_engine.user_id(),
            old_engine.device_id(),
            &old_engine.signature_key()
        ),
        "the approving device's authenticated public signing identity is anchored"
    );
    assert_ne!(new_engine.device_id(), old_engine.device_id());
    assert_ne!(new_engine.signature_key(), old_engine.signature_key());
    let replacement = e2ee::identity::DeviceIdentity::generate(peer).unwrap();
    let changed = e2ee::identity::DeviceRecord {
        device_id: replacement.device,
        signature_key: replacement.key.public_key(),
        cross_signature: None,
        created_at: 10,
    };
    assert!(matches!(
        new_engine.verify_devices(peer, &[changed]),
        Err(e2ee::Error::IdentityChanged)
    ));
    assert!(new.cache("c1").is_none_or(|cache| cache.rows.is_empty()));
}

#[test]
fn public_pin_persistence_failure_prevents_cache_import() {
    use e2ee::store::{MemoryStore, Store};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct CheckedStore {
        inner: MemoryStore,
        fail: Arc<AtomicBool>,
    }
    impl Store for CheckedStore {
        fn load(&self, name: &str) -> e2ee::Result<Option<Vec<u8>>> {
            self.inner.load(name)
        }
        fn save(&self, name: &str, blob: &[u8]) -> e2ee::Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(e2ee::Error::Storage("unavailable".into()));
            }
            self.inner.save(name, blob)
        }
        fn remove(&self, name: &str) -> e2ee::Result<()> {
            self.inner.remove(name)
        }
    }
    let user = UserId::parse(USER).unwrap();
    let source = Engine::detached(user).unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let receiving = Engine::create(
        user,
        Box::new(CheckedStore {
            inner: MemoryStore::default(),
            fail: fail.clone(),
        }),
        StateKey::generate().unwrap(),
        "state",
    )
    .unwrap();
    let shared = device("public-persistence-failure", DeviceState::Ready);
    let mut receiver = lock(&shared);
    receiver.engine = Some(receiving);
    let key = ArchiveKey::generate().unwrap();
    let meta = Meta {
        v: archive::VERSION,
        user: USER.into(),
        from_device: source.device_id().to_string(),
        created: "0".into(),
        scope: Scope::All,
        public_trust: Some(source.public_trust().unwrap()),
    };
    let mut writer = Writer::new(&key, b"checked-import", &meta, Vec::new()).unwrap();
    writer
        .message("c1", "m1", &json!({"text":"must wait for persisted pins"}))
        .unwrap();
    let (blob, _) = writer.finish().unwrap();
    fail.store(true, Ordering::SeqCst);
    let mut target = Merging {
        device: &mut receiver,
        members: ["c1".to_owned()].into(),
        from: String::new(),
        touched: BTreeSet::new(),
        expected_signer: None,
    };
    assert!(matches!(
        archive::merge(
            &key,
            b"checked-import",
            USER,
            || Ok(blob.as_slice()),
            &mut target
        ),
        Err(e2ee::Error::Storage(_))
    ));
    assert!(receiver
        .cache("c1")
        .is_none_or(|cache| cache.rows.is_empty()));
}

#[test]
fn a_modern_link_requires_metadata_anchored_to_the_authenticated_signer() {
    let source = Engine::detached(UserId::parse(USER).unwrap()).unwrap();
    let shared = device("missing-public-anchor", DeviceState::Ready);
    let mut receiver = lock(&shared);
    let mut meta = Meta {
        v: archive::VERSION,
        user: USER.into(),
        from_device: source.device_id().to_string(),
        created: "0".into(),
        scope: Scope::None,
        public_trust: None,
    };
    let target = Merging {
        device: &mut receiver,
        members: BTreeSet::new(),
        from: String::new(),
        touched: BTreeSet::new(),
        expected_signer: Some(source.device_ref()),
    };
    assert!(target.validate_meta(&meta).is_err());
    meta.public_trust = Some(source.public_trust().unwrap());
    assert!(target.validate_meta(&meta).is_ok());
    meta.from_device = e2ee::DeviceId::random().unwrap().to_string();
    assert!(target.validate_meta(&meta).is_err());
}
