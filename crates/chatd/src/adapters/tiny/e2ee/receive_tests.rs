//! Real MLS receive-only recovery across mixed cryptographic generations.

use super::*;
use e2ee::groups::CommitBundle;
use e2ee::identity::DeviceKey;
use openmls::prelude::tls_codec::{DeserializeBytes as _, Serialize as _};
use openmls::prelude::{
    ExternalProposal, GroupEpoch, GroupId, KeyPackageIn, MlsMessageOut, ProtocolVersion,
    SenderExtensionIndex,
};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;

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
