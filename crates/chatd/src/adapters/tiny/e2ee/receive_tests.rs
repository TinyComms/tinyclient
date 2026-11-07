//! Real MLS receive-only recovery across mixed cryptographic generations.

use super::*;
use e2ee::groups::CommitBundle;
use e2ee::identity::DeviceKey;
use e2ee::store::Store;
use openmls::prelude::tls_codec::{DeserializeBytes as _, Serialize as _};
use openmls::prelude::{
    ExternalProposal, GroupEpoch, GroupId, KeyPackageIn, MlsMessageOut, ProtocolVersion,
    SenderExtensionIndex,
};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
struct FailingState {
    dir: PathBuf,
    fail: Arc<AtomicBool>,
}
impl Store for FailingState {
    fn load(&self, name: &str) -> e2ee::Result<Option<Vec<u8>>> {
        FileStore::new(&self.dir).load(name)
    }
    fn remove(&self, name: &str) -> e2ee::Result<()> {
        FileStore::new(&self.dir).remove(name)
    }
    fn save(&self, name: &str, bytes: &[u8]) -> e2ee::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Storage(
                "injected handshake state write failure".into(),
            ));
        }
        FileStore::new(&self.dir).save(name, bytes)
    }
}

#[test]
fn failed_welcome_save_keeps_the_feed_retryable_after_restart_or_in_memory() {
    for cold in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("receiver");
        private_dir(&dir).unwrap();
        let store = FailingState {
            dir: dir.clone(),
            fail: Arc::new(AtomicBool::new(false)),
        };
        let state_key = StateKey::generate().unwrap();
        let alice = UserId::random().unwrap();
        let bea = UserId::random().unwrap();
        let conv = ConversationId::random().unwrap();
        let conversation = conv.to_string();
        let server = DeviceKey::generate().unwrap();
        let public = server.public_key();
        let mut a = Engine::detached(alice).unwrap();
        let mut b = Engine::create(bea, Box::new(store.clone()), state_key.clone(), STATE).unwrap();
        a.verify_devices(bea, &[record(&b, None, 1)]).unwrap();
        b.verify_devices(alice, &[record(&a, None, 1)]).unwrap();
        a.create_group(conv, &public).unwrap();
        let package = b.key_packages(1).unwrap().remove(0);
        let (bundle, _) = add(&mut a, conv, &server, &package);
        let welcome = bundle
            .welcomes
            .iter()
            .find(|w| w.device.device == b.device_id())
            .unwrap()
            .data
            .clone();
        let base = super::super::tests::serve(move |request| {
            assert!(
                request.contains("/handshake?"),
                "unexpected fixture request"
            );
            let rows = if request.contains("after=0&") {
                json!([{"seq":1,"kind":"welcome","epoch":1,"data":b64(&welcome)}])
            } else {
                json!([])
            };
            super::super::tests::http(200, "OK", &rows.to_string())
        });
        let saved = Saved {
            base_url: base,
            token: "synthetic".into(),
            user_id: bea.to_string(),
            email: None,
            display_name: "Bea".into(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir = dir.clone();
        device.engine = Some(b);
        device.save_notes();
        let before = std::fs::read(dir.join(STATE)).unwrap();
        store.fail.store(true, Ordering::SeqCst);
        let refused = device.feed_at(&conversation, conv, 0, &public);
        let cursor_after_failure = device.notes.seqs.get(&conversation).copied().unwrap_or(0);
        assert_eq!(
            std::fs::read(dir.join(STATE)).unwrap(),
            before,
            "failed save must leave old disk snapshot"
        );
        if cold {
            drop(device);
            device = Device::new(&saved);
            device.dir = dir.clone();
            device.notes =
                serde_json::from_slice(&std::fs::read(dir.join("notes.json")).unwrap()).unwrap();
            device.engine =
                Engine::open(Box::new(store.clone()), state_key.clone(), STATE).unwrap();
            assert!(
                !device.engine().unwrap().has_group(conv),
                "Welcome was not saved before restart"
            );
        }
        store.fail.store(false, Ordering::SeqCst);
        device.feed_at(&conversation, conv, 0, &public).unwrap();
        let persisted = Engine::open(Box::new(store.clone()), state_key, STATE)
            .unwrap()
            .unwrap();
        assert!(persisted.has_group(conv), "Welcome must remain retryable after failed save (cold={cold}, refused={}, advanced_cursor={cursor_after_failure})", refused.is_err());
        assert!(refused.is_err(), "storage failure must be reported");
        assert_eq!(cursor_after_failure, 0, "do not skip the unsaved Welcome");
        assert_eq!(device.notes.seqs.get(&conversation), Some(&1));
        let notes: Notes =
            serde_json::from_slice(&std::fs::read(dir.join("notes.json")).unwrap()).unwrap();
        assert_eq!(notes.seqs.get(&conversation), Some(&1));
    }
}

#[test]
fn failed_commit_save_keeps_new_ciphertext_readable_after_restart_or_in_memory() {
    for cold in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("receiver");
        private_dir(&dir).unwrap();
        let store = FailingState {
            dir: dir.clone(),
            fail: Arc::new(AtomicBool::new(false)),
        };
        let state_key = StateKey::generate().unwrap();
        let alice = UserId::random().unwrap();
        let bea = UserId::random().unwrap();
        let carol = UserId::random().unwrap();
        let conv = ConversationId::random().unwrap();
        let conversation = conv.to_string();
        let server = DeviceKey::generate().unwrap();
        let public = server.public_key();
        let mut a = Engine::detached(alice).unwrap();
        let mut b = Engine::create(bea, Box::new(store.clone()), state_key.clone(), STATE).unwrap();
        let mut c = Engine::detached(carol).unwrap();
        a.verify_devices(bea, &[record(&b, None, 1)]).unwrap();
        b.verify_devices(alice, &[record(&a, None, 1)]).unwrap();
        a.create_group(conv, &public).unwrap();
        let (first, _) = add(&mut a, conv, &server, &b.key_packages(1).unwrap().remove(0));
        let welcome = first
            .welcomes
            .iter()
            .find(|w| w.device.device == b.device_id())
            .unwrap();
        b.handle_welcome(conv, &welcome.data, Some(&public))
            .unwrap();
        a.verify_devices(carol, &[record(&c, None, 1)]).unwrap();
        b.verify_devices(carol, &[record(&c, None, 1)]).unwrap();
        let (commit, proposals) = add(&mut a, conv, &server, &c.key_packages(1).unwrap().remove(0));
        assert!(b
            .handle_proposals(conv, &proposals)
            .unwrap()
            .rejected
            .is_empty());
        assert_eq!(b.epoch(conv).unwrap(), 1);
        let sealed = a
            .encrypt(
                conv,
                Kind::Message,
                None,
                &Envelope::message("after the saved membership change"),
            )
            .unwrap();
        assert_eq!(sealed.epoch, 2);
        let base = super::super::tests::serve(move |request| {
            assert!(
                request.contains("/handshake?"),
                "unexpected fixture request"
            );
            let rows = if request.contains("after=0&") {
                json!([{"seq":1,"kind":"commit","epoch":commit.epoch,"data":b64(&commit.commit)}])
            } else {
                json!([])
            };
            super::super::tests::http(200, "OK", &rows.to_string())
        });
        let saved = Saved {
            base_url: base,
            token: "synthetic".into(),
            user_id: bea.to_string(),
            email: None,
            display_name: "Bea".into(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = Device::new(&saved);
        device.dir = dir.clone();
        device.engine = Some(b);
        device.save_notes();
        let before = std::fs::read(dir.join(STATE)).unwrap();
        store.fail.store(true, Ordering::SeqCst);
        let refused = device.feed_at(&conversation, conv, 0, &public);
        let cursor_after_failure = device.notes.seqs.get(&conversation).copied().unwrap_or(0);
        assert_eq!(
            device.engine().unwrap().epoch(conv).unwrap(),
            2,
            "commit was accepted in memory"
        );
        assert_eq!(
            std::fs::read(dir.join(STATE)).unwrap(),
            before,
            "failed save must leave old disk snapshot"
        );
        if cold {
            drop(device);
            device = Device::new(&saved);
            device.dir = dir.clone();
            device.notes =
                serde_json::from_slice(&std::fs::read(dir.join("notes.json")).unwrap()).unwrap();
            device.engine =
                Engine::open(Box::new(store.clone()), state_key.clone(), STATE).unwrap();
            assert_eq!(device.engine().unwrap().epoch(conv).unwrap(), 1);
        }
        store.fail.store(false, Ordering::SeqCst);
        device.feed_at(&conversation, conv, 0, &public).unwrap();
        let mut persisted = Engine::open(Box::new(store.clone()), state_key, STATE)
            .unwrap()
            .unwrap();
        let opened = persisted.decrypt(
            conv,
            &sealed.ciphertext,
            &Claimed {
                author: Some(alice),
                device: Some(a.device_id()),
                kind: Kind::Message,
                target_id: None,
            },
        );
        assert!(opened.is_ok(), "new ciphertext must remain readable after failed commit save (cold={cold}, refused={}, advanced_cursor={cursor_after_failure}): {:?}", refused.is_err(), opened.as_ref().err());
        assert_eq!(
            opened.unwrap().envelope.text,
            "after the saved membership change"
        );
        assert!(refused.is_err(), "storage failure must be reported");
        assert_eq!(cursor_after_failure, 0);
        assert_eq!(device.notes.seqs.get(&conversation), Some(&1));
    }
}

fn engine(name: &str, user: UserId) -> Engine {
    let dir =
        std::env::temp_dir().join(format!("tiny-mixed-receive-{}-{name}", std::process::id()));
    private_dir(&dir).unwrap();
    Engine::create(
        user,
        Box::new(FileStore::new(dir)),
        StateKey::generate().unwrap(),
        STATE,
    )
    .unwrap()
}

fn record(engine: &Engine, signature: Option<Vec<u8>>, at: i64) -> DeviceRecord {
    DeviceRecord {
        device_id: engine.device_id(),
        signature_key: engine.signature_key(),
        cross_signature: signature,
        created_at: at,
    }
}

fn add(
    engine: &mut Engine,
    group: ConversationId,
    server: &DeviceKey,
    package: &[u8],
) -> (CommitBundle, Vec<u8>) {
    let crypto = OpenMlsRustCrypto::default();
    let parsed = KeyPackageIn::tls_deserialize_exact_bytes(package)
        .unwrap()
        .validate(crypto.crypto(), ProtocolVersion::Mls10)
        .unwrap();
    let proposal: MlsMessageOut = ExternalProposal::new_add::<OpenMlsRustCrypto>(
        parsed,
        GroupId::from_slice(&group.0),
        GroupEpoch::from(engine.epoch(group).unwrap()),
        server,
        SenderExtensionIndex::new(0),
    )
    .unwrap();
    let proposals = vec![proposal].tls_serialize_detached().unwrap();
    let outcome = engine.handle_proposals(group, &proposals).unwrap();
    assert!(outcome.rejected.is_empty());
    let bundle = engine.commit_pending(group).unwrap().unwrap();
    engine.confirm_commit(group).unwrap();
    (bundle, proposals)
}

/// A failed sealed-cache append must not leave only a consumed receive ratchet. After storage is
/// repaired, the same ciphertext opens and survives a cold restart with the same keys.
#[test]
fn opened_history_survives_a_failed_cache_append_and_restart() {
    assert_eq!(prove_opened_history_cache_commit(), Ok(()));
}

/// Cache durable + state save lagging: restart serves the sealed row; a mismatched claim still
/// spends no key and does not replace authenticated cache metadata.
#[test]
fn sealed_cache_survives_state_save_lag_and_rejects_mismatched_replay() {
    let fixture = tempfile::tempdir().unwrap();
    let a_dir = fixture.path().join("sender");
    let b_dir = fixture.path().join("receiver");
    private_dir(&a_dir).unwrap();
    private_dir(&b_dir).unwrap();
    let alice = UserId::random().unwrap();
    let bea = UserId::random().unwrap();
    let mut a = Engine::create(
        alice,
        Box::new(FileStore::new(&a_dir)),
        StateKey::generate().unwrap(),
        STATE,
    )
    .unwrap();
    let state_key = StateKey::generate().unwrap();
    let cache_key = CacheKey::generate().unwrap();
    let mut b = Engine::create(
        bea,
        Box::new(FileStore::new(&b_dir)),
        state_key.clone(),
        STATE,
    )
    .unwrap();
    a.verify_devices(bea, &[record(&b, None, 1)]).unwrap();
    b.verify_devices(alice, &[record(&a, None, 1)]).unwrap();
    let conv = ConversationId::random().unwrap();
    let server = DeviceKey::generate().unwrap();
    let public = server.public_key();
    a.create_group(conv, &public).unwrap();
    let (bundle, _) = add(&mut a, conv, &server, &b.key_packages(1).unwrap().remove(0));
    b.handle_welcome(conv, &bundle.welcomes[0].data, Some(&public))
        .unwrap();
    let sent = a
        .encrypt(
            conv,
            Kind::Message,
            None,
            &Envelope::message("lagged state history"),
        )
        .unwrap();
    let id = conv.to_string();
    let before = std::fs::read(b_dir.join(STATE)).unwrap();
    let saved = Saved {
        base_url: "https://diagnostic.invalid".into(),
        token: "synthetic".into(),
        user_id: bea.to_string(),
        email: None,
        display_name: "Fixture".into(),
        role: "member".into(),
        plain_http_ok: false,
        e2ee: None,
    };
    let author = alice.to_string();
    let from = a.device_id().to_string();
    let ciphertext = b64(&sent.ciphertext);
    let row = Row {
        id: "m1",
        author_id: &author,
        device_id: &from,
        kind: "message",
        ciphertext: Some(&ciphertext),
        epoch: sent.epoch,
        generation: None,
        group_id: None,
        target_id: None,
        client_id: None,
        created_at: "2026-01-01T00:00:00Z",
    };
    let mut device = {
        let mut device = Device::new(&saved);
        device.dir = b_dir.clone();
        device.engine = Some(b);
        device.cache_key = Some(cache_key.clone());
        device.state = DeviceState::Ready;
        device
    };
    let wrong_epoch = Row { epoch: sent.epoch + 1, ..row };
    let refused = device.read(&id, std::slice::from_ref(&wrong_epoch));
    assert!(matches!(refused.get("m1"), Some(Read::Undecryptable)),
        "wire context must match before the receive is durably accepted");
    assert_eq!(std::fs::read(b_dir.join(STATE)).unwrap(), before,
        "a mismatched row must not consume the genuine ciphertext");
    assert!(device.cache(&id).unwrap().rows.get("m1").is_none());
    let opened = device.read(&id, std::slice::from_ref(&row));
    assert!(
        matches!(opened.get("m1"), Some(Read::Opened(o)) if o.text == "lagged state history")
    );
    // Simulate the opposite cut: durable cache present, on-disk ratchet not yet advanced.
    std::fs::write(b_dir.join(STATE), &before).unwrap();
    drop(device);
    let b = Engine::open(Box::new(FileStore::new(&b_dir)), state_key.clone(), STATE)
        .unwrap()
        .unwrap();
    let mut restarted = {
        let mut device = Device::new(&saved);
        device.dir = b_dir.clone();
        device.engine = Some(b);
        device.cache_key = Some(cache_key.clone());
        device.state = DeviceState::Ready;
        device
    };
    let after = restarted.read(&id, std::slice::from_ref(&row));
    assert!(
        matches!(after.get("m1"), Some(Read::Opened(o)) if o.text == "lagged state history"),
        "sealed cache must serve history when the ratchet snapshot lags"
    );
    let other = UserId::random().unwrap().to_string();
    let mismatched = Row {
        id: "m1",
        author_id: &other,
        device_id: &from,
        kind: "message",
        ciphertext: Some(&ciphertext),
        epoch: sent.epoch,
        generation: None,
        group_id: None,
        target_id: None,
        client_id: None,
        created_at: "2026-01-01T00:00:00Z",
    };
    let refused = restarted.read(&id, std::slice::from_ref(&mismatched));
    assert!(
        matches!(refused.get("m1"), Some(Read::Undecryptable)),
        "cached auth must refuse a mismatched claim without replacing the sealed row"
    );
    let again = restarted.read(&id, std::slice::from_ref(&row));
    assert!(
        matches!(again.get("m1"), Some(Read::Opened(o)) if o.text == "lagged state history")
    );
}

#[test]
fn mixed_generation_receive_does_not_spend_current_welcome_on_blocked_historical_rows() {
    let alice = UserId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let bea = UserId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let outer = ConversationId::parse("33333333-3333-4333-8333-333333333333").unwrap();
    let mut a = engine("a", alice);
    let mut b = engine("b", bea);
    let mut x = engine("x", alice);
    let mut y = engine("y", alice);
    let ar = record(&a, None, 1);
    let br = record(&b, None, 1);
    let xr = record(
        &x,
        Some(a.cross_sign(x.device_id(), &x.signature_key()).unwrap()),
        2,
    );
    let yr = record(
        &y,
        Some(a.cross_sign(y.device_id(), &y.signature_key()).unwrap()),
        3,
    );
    a.verify_devices(alice, &[ar.clone(), xr.clone(), yr.clone()])
        .unwrap();
    a.verify_devices(bea, &[br.clone()]).unwrap();
    b.verify_devices(alice, &[ar.clone(), xr.clone()]).unwrap();
    b.verify_devices(bea, &[br.clone()]).unwrap();
    let server = DeviceKey::generate().unwrap();
    let public = server.public_key();
    a.create_group(outer, &public).unwrap();
    let (first, _) = add(
        &mut a,
        outer,
        &server,
        &b.key_packages(1).unwrap().remove(0),
    );
    b.handle_welcome(
        outer,
        &first
            .welcomes
            .iter()
            .find(|welcome| welcome.device.device == b.device_id())
            .unwrap()
            .data,
        Some(&public),
    )
    .unwrap();
    let (second, proposals) = add(
        &mut a,
        outer,
        &server,
        &x.key_packages(1).unwrap().remove(0),
    );
    assert!(b
        .handle_proposals(outer, &proposals)
        .unwrap()
        .rejected
        .is_empty());
    b.handle_commit(outer, second.epoch, &second.commit)
        .unwrap();
    assert_eq!(b.epoch(outer).unwrap(), 2);
    // A legitimately committed Y while approved. B never learned that approval; now X/Y
    // are revoked in the ordinary directory, so replaying this historical Add must stop.
    // A member's authenticated by-value Add remains stageable without a proposal queue.
    // X is a real old member; A trusts Y when accepting it, while B never approved Y.
    x.verify_devices(alice, &[ar.clone(), xr.clone(), yr.clone()])
        .unwrap();
    x.verify_devices(bea, &[br.clone()]).unwrap();
    x.handle_welcome(
        outer,
        &second
            .welcomes
            .iter()
            .find(|welcome| welcome.device.device == x.device_id())
            .unwrap()
            .data,
        Some(&public),
    )
    .unwrap();
    // Server-signed Add of Y: A trusts Y and takes it; B never approved Y, so catch-up stops.
    let (blocked_bundle, blocked_proposals) = add(
        &mut a,
        outer,
        &server,
        &y.key_packages(1).unwrap().remove(0),
    );
    let blocked_epoch = blocked_bundle.epoch;
    let blocked = blocked_bundle.commit;
    assert_eq!(a.epoch(outer).unwrap(), 3);
    let old1 = a
        .encrypt(
            outer,
            Kind::Message,
            None,
            &Envelope::message("unread old one"),
        )
        .unwrap();
    let old2 = a
        .encrypt(
            outer,
            Kind::Message,
            None,
            &Envelope::message("unread old two"),
        )
        .unwrap();
    a.verify_devices(alice, &[ar.clone()]).unwrap();
    b.verify_devices(alice, &[ar.clone()]).unwrap();
    // B never approved Y, so the server Add is refused and B stays on the old epoch.
    assert!(
        !b.handle_proposals(outer, &blocked_proposals)
            .unwrap()
            .rejected
            .is_empty(),
        "B must refuse Y's Add so historical catch-up stays blocked"
    );
    assert_eq!(b.epoch(outer).unwrap(), 2);
    let binding = a.prepare_repair(outer, 0, 3).unwrap();
    a.mark_repair_submitted(outer, binding.payload_hash().unwrap())
        .unwrap();
    a.accept_repair(&binding).unwrap();
    b.accept_repair(&binding).unwrap();
    let fresh = binding.group_id;
    assert!(!b.has_group(fresh));
    let (joined, _) = add(
        &mut a,
        fresh,
        &server,
        &b.key_packages(1).unwrap().remove(0),
    );
    let welcome = joined
        .welcomes
        .iter()
        .find(|welcome| welcome.device.device == b.device_id())
        .unwrap()
        .data
        .clone();
    let new1 = a
        .encrypt(
            fresh,
            Kind::Message,
            None,
            &Envelope::message("current receive one"),
        )
        .unwrap();
    let new2 = a
        .encrypt(
            fresh,
            Kind::Message,
            None,
            &Envelope::message("current receive two"),
        )
        .unwrap();
    let author = alice.to_string();
    let from = a.device_id().to_string();
    let current_group = fresh.to_string();
    let old_group = outer.to_string();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    let directory_a = json!([{"device_id":ar.device_id.to_string(),"signature_key":b64(&ar.signature_key),"created_at":"2026-01-01T00:00:00Z"}]);
    let directory_b = json!([{"device_id":br.device_id.to_string(),"signature_key":b64(&br.signature_key),"created_at":"2026-01-01T00:00:00Z"}]);
    let head = b64(&binding.payload_hash().unwrap());
    let base = super::super::tests::serve(move |request| {
        lock(&seen).push(request.to_owned());
        let body = if request.contains("/handshake?")
            && request.lines().next().unwrap().contains("generation=0 ")
        {
            json!([{"seq":1,"kind":"commit","epoch":blocked_epoch,"data":b64(&blocked),"generation":0,"group_id":outer.to_string()}])
        } else if request.contains("/handshake?") && request.contains("after=0&") {
            json!([{"seq":1,"kind":"welcome","epoch":joined.epoch,"data":b64(&welcome),"generation":1,"group_id":fresh.to_string()}])
        } else if request.contains("/handshake?") {
            json!([])
        } else if request.starts_with(&format!("GET /v1/e2ee/users/{alice}/")) {
            directory_a.clone()
        } else if request.starts_with(&format!("GET /v1/e2ee/users/{bea}/")) {
            directory_b.clone()
        } else if request.starts_with("GET /v1/e2ee/conversations/") {
            json!({"e2ee":true,"epoch":1,"external_sender":b64(&public),"generation":1,"group_id":fresh.to_string(),"head_hash":head,"pending":[],"members":[{"user_id":alice.to_string()},{"user_id":bea.to_string()}]})
        } else {
            panic!(
                "receive-only regression must not send or repair: {}",
                request.lines().next().unwrap()
            );
        };
        super::super::tests::http(200, "Test", &body.to_string())
    });
    let saved = Saved {
        base_url: base,
        token: "synthetic".into(),
        user_id: bea.to_string(),
        email: None,
        display_name: "Bea".into(),
        role: "member".into(),
        plain_http_ok: false,
        e2ee: None,
    };
    let mut device = Device::new(&saved);
    device.dir =
        std::env::temp_dir().join(format!("tiny-mixed-receive-{}-adapter", std::process::id()));
    private_dir(&device.dir).unwrap();
    device.engine = Some(b);
    device.cache_key = Some(CacheKey::generate().unwrap());
    device.state = DeviceState::Ready;
    device.generations = true;
    let old1 = b64(&old1.ciphertext);
    let old2 = b64(&old2.ciphertext);
    let new1 = b64(&new1.ciphertext);
    let new2 = b64(&new2.ciphertext);
    let row = |id, generation, group, epoch, ciphertext| Row {
        id,
        author_id: &author,
        device_id: &from,
        kind: "message",
        ciphertext: Some(ciphertext),
        epoch,
        generation: Some(generation),
        group_id: Some(group),
        target_id: None,
        client_id: None,
        created_at: "2026-01-01T00:00:00Z",
    };
    let read = device.read(
        &old_group,
        &[
            row("new-one", 1, &current_group, 1, &new1),
            row("old-one", 0, &old_group, 3, &old1),
            row("old-two", 0, &old_group, 3, &old2),
            row("new-two", 1, &current_group, 1, &new2),
        ],
    );
    assert!(matches!(read.get("old-one"), Some(Read::Undecryptable)));
    assert!(matches!(read.get("old-two"), Some(Read::Undecryptable)));
    assert!(
        matches!(read.get("new-one"), Some(Read::Opened(opened)) if opened.text == "current receive one"),
        "old strict catch-up must not consume the new internal group's Welcome attempt"
    );
    assert!(
        matches!(read.get("new-two"), Some(Read::Opened(opened)) if opened.text == "current receive two")
    );
    let b = device.engine().unwrap();
    assert_eq!(b.epoch(outer).unwrap(), 2);
    assert_eq!(b.epoch(fresh).unwrap(), 1);
    assert!(b
        .roster(outer)
        .unwrap()
        .iter()
        .all(|member| member.device != y.device_id()));
    let roster = b.roster(fresh).unwrap();
    assert_eq!(roster.len(), 2);
    assert!(roster
        .iter()
        .all(|member| member.device != x.device_id() && member.device != y.device_id()));
    assert!(!b
        .trust()
        .is_trusted(alice, y.device_id(), &y.signature_key()));
    let requests = lock(&requests);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("/handshake?")
                && request.lines().next().unwrap().contains("generation=0 "))
            .count(),
        1,
        "historical sync stays limited to one attempt per group/page"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("/handshake?")
                && request.contains("after=0&")
                && request.lines().next().unwrap().contains("generation=1 "))
            .count(),
        1,
        "current Welcome sync runs once for both new rows"
    );
}
