use e2ee::identity::{DeviceIdentity, DeviceKey, DeviceRecord, Trust};
use e2ee::{DeviceId, Engine, Error, UserId};

fn record(device: &Engine) -> DeviceRecord {
    DeviceRecord {
        device_id: device.device_id(),
        signature_key: device.signature_key(),
        cross_signature: None,
        created_at: 1,
    }
}

#[test]
fn a_failed_public_pin_save_rolls_back_memory_and_can_be_retried() {
    use e2ee::store::{MemoryStore, StateKey, Store};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    struct SwitchStore {
        values: Arc<MemoryStore>,
        fail: Arc<AtomicBool>,
    }
    impl Store for SwitchStore {
        fn load(&self, name: &str) -> e2ee::Result<Option<Vec<u8>>> {
            self.values.load(name)
        }
        fn save(&self, name: &str, blob: &[u8]) -> e2ee::Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                Err(Error::Storage("injected save failure".into()))
            } else {
                self.values.save(name, blob)
            }
        }
        fn remove(&self, name: &str) -> e2ee::Result<()> {
            self.values.remove(name)
        }
    }
    let own = UserId::random().unwrap();
    let peer_id = UserId::random().unwrap();
    let peer = Engine::detached(peer_id).unwrap();
    let mut source = Engine::detached(own).unwrap();
    source.verify_devices(peer_id, &[record(&peer)]).unwrap();
    let public = source.public_trust().unwrap();
    let values = Arc::new(MemoryStore::default());
    let fail = Arc::new(AtomicBool::new(false));
    let store = || {
        Box::new(SwitchStore {
            values: values.clone(),
            fail: fail.clone(),
        }) as Box<dyn Store + Send>
    };
    let key = StateKey::generate().unwrap();
    let mut receiver = Engine::create(own, store(), key.clone(), "state").unwrap();
    let before = receiver.public_trust().unwrap();
    fail.store(true, Ordering::SeqCst);
    assert!(receiver.import_public_trust(&public).is_err());
    assert_eq!(
        receiver.public_trust().unwrap(),
        before,
        "rejected imports cannot activate public pins in memory"
    );
    assert!(
        receiver.flush().is_ok(),
        "a failed import must not leave new dirty state to commit later"
    );
    fail.store(false, Ordering::SeqCst);
    receiver.import_public_trust(&public).unwrap();
    let reopened = Engine::open(store(), key, "state").unwrap().unwrap();
    assert!(reopened
        .trust()
        .is_trusted(peer_id, peer.device_id(), &peer.signature_key()));
}

#[test]
fn public_import_keeps_existing_pins_and_revocation_tombstones() {
    let own = UserId::random().unwrap();
    let peer = UserId::random().unwrap();
    let old_peer = Engine::detached(peer).unwrap();
    let current_peer = Engine::detached(peer).unwrap();
    let mut old = Engine::detached(own).unwrap();
    old.verify_devices(peer, &[record(&old_peer)]).unwrap();
    let mut receiver = Engine::detached(own).unwrap();
    receiver
        .verify_devices(peer, &[record(&current_peer)])
        .unwrap();
    receiver
        .import_public_trust(&old.public_trust().unwrap())
        .unwrap();
    assert!(receiver.trust().is_trusted(
        peer,
        current_peer.device_id(),
        &current_peer.signature_key()
    ));
    assert!(!receiver
        .trust()
        .is_trusted(peer, old_peer.device_id(), &old_peer.signature_key()));
    old.verify_devices(peer, &[]).unwrap();
    let mut fresh = Engine::detached(own).unwrap();
    fresh
        .import_public_trust(&old.public_trust().unwrap())
        .unwrap();
    assert!(fresh.trust().knows(peer));
    assert!(fresh.trust().devices(peer).is_empty());
    assert!(matches!(
        fresh.verify_devices(peer, &[record(&current_peer)]),
        Err(Error::IdentityChanged)
    ));
}

#[test]
fn public_export_contains_only_public_records_even_with_local_key_packages() {
    let own = UserId::random().unwrap();
    let identity = DeviceIdentity {
        user: own,
        device: DeviceId::random().unwrap(),
        key: DeviceKey::from_bytes(&[7; 32]).unwrap(),
    };
    let mut old = Engine::with_identity(identity);
    old.key_packages(2).unwrap();
    let public = old.public_trust().unwrap();
    assert!(public.is_trusted(own, old.device_id(), &old.signature_key()));
    let json = serde_json::to_value(&public).unwrap();
    let root = json.as_object().unwrap();
    assert_eq!(root.len(), 1);
    let users = root["users"].as_object().unwrap();
    for user in users.values() {
        let fields = user.as_object().unwrap();
        assert_eq!(fields.len(), 2);
        assert!(fields["pinned"].is_boolean());
        let devices = fields["devices"].as_object().unwrap();
        for key in devices.values() {
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(key.as_str().unwrap())
                .unwrap();
            assert_eq!(raw.len(), 65);
            assert_eq!(raw[0], 4);
            assert_ne!(raw, vec![7; 32]);
        }
    }
}

#[test]
fn malformed_public_import_and_own_device_key_conflict_leave_trust_untouched() {
    let own = UserId::random().unwrap();
    let mut receiver = Engine::detached(own).unwrap();
    let before = receiver.public_trust().unwrap();
    let mut malformed = serde_json::to_value(&before).unwrap();
    malformed["users"][own.to_string()]["devices"][receiver.device_id().to_string()] =
        serde_json::json!("AA==");
    let malformed: Trust = serde_json::from_value(malformed).unwrap();
    assert!(receiver.import_public_trust(&malformed).is_err());
    assert_eq!(receiver.public_trust().unwrap(), before);
    let changed = Engine::with_identity(DeviceIdentity {
        user: own,
        device: receiver.device_id(),
        key: DeviceKey::generate().unwrap(),
    });
    assert!(matches!(
        receiver.import_public_trust(&changed.public_trust().unwrap()),
        Err(Error::IdentityChanged)
    ));
    assert_eq!(receiver.public_trust().unwrap(), before);
}

#[test]
fn local_approval_verifies_exact_account_device_and_public_key() {
    use e2ee::link::ApproveSigner;
    let user = UserId::random().unwrap();
    let old = Engine::detached(user).unwrap();
    let new = Engine::detached(user).unwrap();
    let signature = old
        .cross_sign(new.device_id(), &new.signature_key())
        .unwrap();
    let signer = ApproveSigner {
        user,
        device: old.device_id(),
        signature_key: old.signature_key(),
    };
    assert_eq!(
        signer
            .verify(user, new.device_id(), &new.signature_key(), &signature)
            .unwrap(),
        old.device_ref()
    );
    assert!(signer
        .verify(
            UserId::random().unwrap(),
            new.device_id(),
            &new.signature_key(),
            &signature
        )
        .is_err());
    assert!(signer
        .verify(
            user,
            DeviceId::random().unwrap(),
            &new.signature_key(),
            &signature
        )
        .is_err());
    assert!(signer
        .verify(user, new.device_id(), &old.signature_key(), &signature)
        .is_err());
    let invalid = ApproveSigner {
        signature_key: vec![0; 65],
        ..signer
    };
    assert!(invalid
        .verify(user, new.device_id(), &new.signature_key(), &signature)
        .is_err());
}
