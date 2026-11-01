mod common;

use common::*;
use e2ee::engine::Engine;
use e2ee::envelope::Kind;
use e2ee::store::{
    open_cache_value, seal_cache_value, CacheKey, FileStore, MemoryStore, StateKey, Store,
};
use e2ee::{Claimed, Error, UserId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A store that several handles share, so a test can look at what the engine wrote.
#[derive(Clone, Default)]
struct Shared {
    inner: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    fail: Arc<AtomicBool>,
    saves: Arc<std::sync::atomic::AtomicUsize>,
}

impl Store for Shared {
    fn load(&self, name: &str) -> e2ee::Result<Option<Vec<u8>>> {
        Ok(self.inner.lock().unwrap().get(name).cloned())
    }
    fn save(&self, name: &str, blob: &[u8]) -> e2ee::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Storage("disk full".into()));
        }
        self.saves.fetch_add(1, Ordering::SeqCst);
        self.inner
            .lock()
            .unwrap()
            .insert(name.to_owned(), blob.to_vec());
        Ok(())
    }
    fn remove(&self, name: &str) -> e2ee::Result<()> {
        self.inner.lock().unwrap().remove(name);
        Ok(())
    }
}

#[test]
fn state_survives_a_restart_and_the_group_goes_on() {
    let store = Shared::default();
    let key = StateKey::generate().unwrap();
    let user = UserId::random().unwrap();
    let mut alice =
        Engine::create(user, Box::new(store.clone()), key.clone(), "e2ee-alice").unwrap();
    let alice_ids = (alice.user_id(), alice.device_id(), alice.signature_key());

    // Bob is an in-memory engine; the group is made with Alice's persisted engine.
    let mut world = World::new();
    let bob = world.person("bob");
    let bob_user = world.clients[bob].user();
    let list = world.directory[&bob_user].clone();
    alice.verify_devices(bob_user, &list).unwrap();
    let alice_list = vec![e2ee::identity::DeviceRecord {
        device_id: alice.device_id(),
        signature_key: alice.signature_key(),
        cross_signature: None,
        created_at: 1,
    }];
    world.clients[bob]
        .engine
        .verify_devices(alice.user_id(), &alice_list)
        .unwrap();

    let conv = world.conv();
    alice.create_group(conv, &world.ds.public()).unwrap();
    let package = world.clients[bob].engine.key_packages(1).unwrap().remove(0);
    alice
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], 0))
        .unwrap();
    let bundle = alice.commit_pending(conv).unwrap().unwrap();
    alice.confirm_commit(conv).unwrap();
    world.clients[bob]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&world.ds.public()))
        .unwrap();

    let first = alice
        .encrypt(conv, Kind::Message, None, &envelope("before the restart"))
        .unwrap();
    // Restart: only the store and the key remain.
    drop(alice);
    let mut alice = Engine::open(Box::new(store.clone()), key.clone(), "e2ee-alice")
        .unwrap()
        .expect("state was stored");
    assert_eq!(
        (alice.user_id(), alice.device_id(), alice.signature_key()),
        alice_ids
    );
    assert_eq!(alice.epoch(conv).unwrap(), 1);
    assert_eq!(alice.roster(conv).unwrap().len(), 2);
    assert!(alice.trust().is_trusted(
        bob_user,
        world.clients[bob].device(),
        &world.clients[bob].engine.signature_key()
    ));

    // Bob reads both messages, in order, one from before the restart and one from after.
    let claimed = Claimed {
        author: Some(alice.user_id()),
        device: Some(alice.device_id()),
        kind: Kind::Message,
        target_id: None,
    };
    let second = alice
        .encrypt(conv, Kind::Message, None, &envelope("after the restart"))
        .unwrap();
    assert_eq!(
        world.clients[bob]
            .engine
            .decrypt(conv, &first.ciphertext, &claimed)
            .unwrap()
            .envelope
            .text,
        "before the restart"
    );
    assert_eq!(
        world.clients[bob]
            .engine
            .decrypt(conv, &second.ciphertext, &claimed)
            .unwrap()
            .envelope
            .text,
        "after the restart"
    );
    // And Alice reads Bob's.
    let reply = world.clients[bob]
        .engine
        .encrypt(conv, Kind::Message, None, &envelope("hi alice"))
        .unwrap();
    let claimed = Claimed {
        author: Some(bob_user),
        device: None,
        kind: Kind::Message,
        target_id: None,
    };
    assert_eq!(
        alice
            .decrypt(conv, &reply.ciphertext, &claimed)
            .unwrap()
            .envelope
            .text,
        "hi alice"
    );
}

#[test]
fn encrypting_and_making_key_packages_fail_when_the_state_cannot_be_written() {
    // encrypt must not return a ciphertext whose key the stored state does not already count as spent.
    let store = Shared::default();
    let key = StateKey::generate().unwrap();
    let mut alice = Engine::create(
        UserId::random().unwrap(),
        Box::new(store.clone()),
        key.clone(),
        "s",
    )
    .unwrap();
    let ds = Ds::new();
    alice.create_group(ds.conv, &ds.public()).unwrap();
    // The disk fills up: encrypting fails instead of handing out a ciphertext.
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        alice.encrypt(ds.conv, Kind::Message, None, &envelope("x")),
        Err(Error::Storage(_))
    ));
    assert!(matches!(alice.key_packages(1), Err(Error::Storage(_))));
    store.fail.store(false, Ordering::SeqCst);
    // And with a working disk, every ciphertext's key is in the stored state before it is returned.
    let before = store.saves.load(Ordering::SeqCst);
    alice
        .encrypt(ds.conv, Kind::Message, None, &envelope("y"))
        .unwrap();
    assert_eq!(store.saves.load(Ordering::SeqCst), before + 1);
}

#[test]
fn receiving_survives_a_failed_write_and_reports_it_on_flush() {
    let store = Shared::default();
    let key = StateKey::generate().unwrap();
    let mut world = World::new();
    let a = world.person("a");
    let mut bob =
        Engine::create(UserId::random().unwrap(), Box::new(store.clone()), key, "s").unwrap();
    world.sync_trust();
    let alice_user = world.clients[a].user();
    bob.verify_devices(alice_user, &world.directory[&alice_user].clone())
        .unwrap();
    world.start(a);
    let conv = world.conv();
    let bob_record = e2ee::identity::DeviceRecord {
        device_id: bob.device_id(),
        signature_key: bob.signature_key(),
        cross_signature: None,
        created_at: 1,
    };
    world.clients[a]
        .engine
        .verify_devices(bob.user_id(), &[bob_record])
        .unwrap();
    let package = bob.key_packages(1).unwrap().remove(0);
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], 0))
        .unwrap();
    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    world.clients[a].engine.confirm_commit(conv).unwrap();
    bob.handle_welcome(conv, &bundle.welcomes[0].data, Some(&world.ds.public()))
        .unwrap();
    let sent = world.send(a, "decrypts even if the disk is full");

    store.fail.store(true, Ordering::SeqCst);
    let claimed = Claimed {
        author: Some(alice_user),
        device: None,
        kind: Kind::Message,
        target_id: None,
    };
    let opened = bob
        .decrypt(conv, &sent, &claimed)
        .expect("a decrypted message is not lost to a failed write");
    assert_eq!(opened.envelope.text, "decrypts even if the disk is full");
    assert!(bob.flush().is_err());
    store.fail.store(false, Ordering::SeqCst);
    bob.flush().unwrap();
}

#[test]
fn the_stored_state_is_authenticated_and_bound_to_its_name_and_key() {
    let store = Shared::default();
    let key = StateKey::generate().unwrap();
    let engine = Engine::create(
        UserId::random().unwrap(),
        Box::new(store.clone()),
        key.clone(),
        "state-a",
    )
    .unwrap();
    let device = engine.device_id();
    drop(engine);
    let blob = store.load("state-a").unwrap().unwrap();

    // The wrong key.
    assert!(matches!(
        Engine::open(
            Box::new(store.clone()),
            StateKey::generate().unwrap(),
            "state-a"
        ),
        Err(Error::Authentication)
    ));
    // A flipped bit anywhere.
    for at in [0, 1, 17, 30, blob.len() / 2, blob.len() - 1] {
        let tampered = Shared::default();
        let mut bad = blob.clone();
        bad[at] ^= 0x01;
        tampered.save("state-a", &bad).unwrap();
        assert!(
            Engine::open(Box::new(tampered), key.clone(), "state-a").is_err(),
            "byte {at}"
        );
    }
    // Truncated or empty.
    for cut in [0, 10, blob.len() - 1] {
        let truncated = Shared::default();
        truncated.save("state-a", &blob[..cut]).unwrap();
        assert!(Engine::open(Box::new(truncated), key.clone(), "state-a").is_err());
    }
    // A blob moved to another name does not open there (the name is authenticated data).
    let moved = Shared::default();
    moved.save("state-b", &blob).unwrap();
    assert!(matches!(
        Engine::open(Box::new(moved), key.clone(), "state-b"),
        Err(Error::Authentication)
    ));
    // Nothing stored is not an error: there is no state.
    assert!(
        Engine::open(Box::new(MemoryStore::default()), key.clone(), "state-a")
            .unwrap()
            .is_none()
    );
    // Creating over existing state is refused.
    assert!(Engine::create(
        UserId::random().unwrap(),
        Box::new(store.clone()),
        key.clone(),
        "state-a"
    )
    .is_err());
    // The right key and name open it.
    let reopened = Engine::open(Box::new(store), key, "state-a")
        .unwrap()
        .unwrap();
    assert_eq!(reopened.device_id(), device);
}

#[test]
fn the_blob_holds_no_plaintext_key_material() {
    let store = Shared::default();
    let key = StateKey::generate().unwrap();
    let engine =
        Engine::create(UserId::random().unwrap(), Box::new(store.clone()), key, "s").unwrap();
    let blob = store.load("s").unwrap().unwrap();
    let public = engine.signature_key();
    assert!(
        !blob
            .windows(public.len())
            .any(|window| window == public.as_slice()),
        "the public key appears in the clear"
    );
    assert!(
        !blob
            .windows(16)
            .any(|window| window == engine.device_id().as_bytes()),
        "the device id appears in the clear"
    );
    assert!(
        !blob.windows(4).any(|window| window == b"user"),
        "field names appear in the clear"
    );
}

#[test]
fn a_snapshot_for_the_caller_to_keep_round_trips_too() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let key = StateKey::generate().unwrap();
    let blob = world.clients[b].engine.snapshot(&key, "snap").unwrap();
    let mut restored = Engine::from_snapshot(&key, "snap", &blob).unwrap();
    let conv = world.conv();
    let sent = world.send(a, "to the restored b");
    let claimed = Claimed {
        author: Some(world.clients[a].user()),
        device: None,
        kind: Kind::Message,
        target_id: None,
    };
    assert_eq!(
        restored
            .decrypt(conv, &sent, &claimed)
            .unwrap()
            .envelope
            .text,
        "to the restored b"
    );
    assert!(Engine::from_snapshot(&key, "other", &blob).is_err());
}

#[test]
fn the_file_store_writes_atomically_and_privately_and_refuses_odd_names() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path().join("nested"));
    assert!(store.load("a").unwrap().is_none());
    store.save("a", b"one").unwrap();
    store.save("a", b"two").unwrap();
    assert_eq!(store.load("a").unwrap().unwrap(), b"two");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("nested/a"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    // No temp file is left next to it.
    let names: Vec<_> = std::fs::read_dir(dir.path().join("nested"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a".to_owned()]);
    for name in [
        "",
        "../x",
        "a/b",
        "a\\b",
        ".hidden",
        "x\0y",
        &"n".repeat(200),
    ] {
        assert!(store.save(name, b"x").is_err(), "{name:?}");
        assert!(store.load(name).is_err(), "{name:?}");
    }
    store.remove("a").unwrap();
    store.remove("a").unwrap();
    assert!(store.load("a").unwrap().is_none());

    // An engine works through it.
    let key = StateKey::generate().unwrap();
    let engine = Engine::create(
        UserId::random().unwrap(),
        Box::new(FileStore::new(dir.path())),
        key.clone(),
        "engine.state",
    )
    .unwrap();
    let device = engine.device_id();
    drop(engine);
    assert_eq!(
        Engine::open(Box::new(FileStore::new(dir.path())), key, "engine.state")
            .unwrap()
            .unwrap()
            .device_id(),
        device
    );
}

#[test]
fn the_message_cache_seals_a_column_to_its_row() {
    let key = CacheKey::generate().unwrap();
    let plaintext = "the decrypted text of message 42".as_bytes();
    let sealed = seal_cache_value(&key, b"message-42", plaintext).unwrap();
    assert!(!sealed
        .windows(plaintext.len())
        .any(|window| window == plaintext));
    assert_eq!(
        open_cache_value(&key, b"message-42", &sealed)
            .unwrap()
            .as_slice(),
        plaintext
    );
    // Another row's id, another key, any altered byte, a short blob: refused.
    assert!(matches!(
        open_cache_value(&key, b"message-43", &sealed),
        Err(Error::Authentication)
    ));
    assert!(matches!(
        open_cache_value(&CacheKey::generate().unwrap(), b"message-42", &sealed),
        Err(Error::Authentication)
    ));
    for at in 0..sealed.len() {
        let mut bad = sealed.clone();
        bad[at] ^= 0x80;
        assert!(
            open_cache_value(&key, b"message-42", &bad).is_err(),
            "byte {at}"
        );
    }
    assert!(open_cache_value(&key, b"message-42", &sealed[..10]).is_err());
    assert!(open_cache_value(&key, b"message-42", &[]).is_err());
    // A fresh nonce each time: the same text seals differently, so equal messages are not visible.
    let again = seal_cache_value(&key, b"message-42", plaintext).unwrap();
    assert_ne!(sealed, again);
    // Empty plaintext and a large one.
    assert!(
        open_cache_value(&key, b"r", &seal_cache_value(&key, b"r", b"").unwrap())
            .unwrap()
            .is_empty()
    );
    let large = vec![7u8; 1 << 20];
    assert_eq!(
        open_cache_value(&key, b"r", &seal_cache_value(&key, b"r", &large).unwrap())
            .unwrap()
            .as_slice(),
        large.as_slice()
    );
    // The two keys are not interchangeable with the state key: different labels.
    let state = StateKey::from_bytes(*key.as_bytes());
    let blob = e2ee_state_blob(&state);
    assert!(open_cache_value(&key, b"s", &blob).is_err());
}

fn e2ee_state_blob(key: &StateKey) -> Vec<u8> {
    let engine = Engine::detached(UserId::random().unwrap()).unwrap();
    engine.snapshot(key, "s").unwrap()
}

#[test]
fn keys_are_redacted_in_debug_output() {
    let key = StateKey::generate().unwrap();
    assert_eq!(format!("{key:?}"), "StateKey(..)");
    assert_eq!(
        format!("{:?}", CacheKey::generate().unwrap()),
        "CacheKey(..)"
    );
    let engine = Engine::detached(UserId::random().unwrap()).unwrap();
    assert!(!format!("{engine:?}").to_lowercase().contains("key"));
}

#[test]
fn the_engine_can_move_to_another_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<Engine>();
    assert_send::<e2ee::identity::Trust>();
    assert_send::<e2ee::attachment::AttachmentKey>();
}
