mod common;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use common::*;
use e2ee::repair::{genesis_hash, Binding};
use e2ee::store::{MemoryStore, StateKey, Store};
use e2ee::{ConversationId, Error};
use proptest::prelude::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone, Default)]
struct SwitchStore {
    inner: Arc<MemoryStore>,
    fail: Arc<AtomicBool>,
    writes: Arc<AtomicUsize>,
    fail_on_write: Arc<AtomicUsize>,
}

impl Store for SwitchStore {
    fn load(&self, name: &str) -> e2ee::Result<Option<Vec<u8>>> {
        self.inner.load(name)
    }
    fn save(&self, name: &str, blob: &[u8]) -> e2ee::Result<()> {
        let write = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail.load(Ordering::SeqCst) || self.fail_on_write.load(Ordering::SeqCst) == write {
            return Err(Error::Storage("injected full disk".into()));
        }
        self.inner.save(name, blob)
    }
    fn remove(&self, name: &str) -> e2ee::Result<()> {
        self.inner.remove(name)
    }
}

fn persist(world: &mut World, at: usize) -> (SwitchStore, StateKey) {
    let store = SwitchStore::default();
    let key = StateKey::generate().unwrap();
    let blob = world.clients[at].engine.snapshot(&key, "repair").unwrap();
    store.save("repair", &blob).unwrap();
    world.clients[at].engine = e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair")
        .unwrap()
        .unwrap();
    (store, key)
}

fn pair() -> (World, usize, usize) {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    (world, a, b)
}

fn vector() -> serde_json::Value {
    serde_json::from_str(include_str!("vectors/group_repair.json")).unwrap()
}

#[test]
fn canonical_binding_matches_the_independent_public_vector() {
    let vector = vector();
    let binding: Binding = serde_json::from_value(vector["binding"].clone()).unwrap();
    let payload = binding.canonical_payload().unwrap();
    assert_eq!(payload.len(), 260);
    assert_eq!(STANDARD.encode(&payload), vector["payload_base64"]);
    assert_eq!(
        STANDARD.encode(binding.payload_hash().unwrap()),
        vector["head_hash"]
    );
    binding.verify_signature().unwrap();
    let key = STANDARD
        .decode(vector["external_server_key"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        STANDARD.encode(genesis_hash(binding.conversation_id, &key).unwrap()),
        vector["genesis_hash"]
    );
    assert_eq!(serde_json::to_value(binding).unwrap(), vector["binding"]);
}

#[test]
fn altered_bindings_and_malformed_wire_values_are_refused() {
    let original: Binding = serde_json::from_value(vector()["binding"].clone()).unwrap();
    let mut changed = original.clone();
    changed.from_epoch += 1;
    assert!(matches!(
        changed.verify_signature(),
        Err(Error::BadSignature)
    ));
    let mut changed = original.clone();
    changed.group_id = ConversationId::random().unwrap();
    assert!(changed.verify_signature().is_err());
    let mut changed = original.clone();
    changed.to_generation = 3;
    assert!(changed.verify_signature().is_err());
    let mut changed = original.clone();
    changed.signature = vec![0; 73];
    assert!(changed.verify_signature().is_err());
    for (field, value) in [
        ("signature_key", serde_json::json!(STANDARD.encode([4; 64]))),
        ("signature", serde_json::json!(STANDARD.encode([0; 73]))),
        ("previous_hash", serde_json::json!(STANDARD.encode([0; 31]))),
        (
            "conversation_id",
            serde_json::json!("10000000000040008000000000000001"),
        ),
        ("to_generation", serde_json::json!(u64::MAX)),
    ] {
        let mut wire = vector()["binding"].clone();
        wire[field] = value;
        assert!(
            serde_json::from_value::<Binding>(wire).is_err(),
            "accepted malformed {field}"
        );
    }
}

#[test]
fn canonical_bindings_require_the_genesis_anchor_and_never_reuse_its_group_id() {
    let mut binding: Binding = serde_json::from_value(vector()["binding"].clone()).unwrap();
    binding.previous_hash = [0; 32];
    assert!(
        binding.canonical_payload().is_err(),
        "generation0 has a deterministic anchor"
    );
    binding.from_generation = 1;
    binding.to_generation = 2;
    binding.from_group_id = binding.group_id;
    binding.group_id = binding.conversation_id;
    assert!(
        binding.canonical_payload().is_err(),
        "a later generation cannot reuse the known genesis group"
    );
}

#[test]
fn preparation_blocks_both_groups_and_submitted_state_survives_restart() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let hash = binding.payload_hash().unwrap();
    assert_eq!(
        world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap(),
        binding
    );
    assert_eq!(
        world.clients[a]
            .engine
            .active_group(conv)
            .unwrap()
            .generation,
        0
    );
    assert!(world.clients[a]
        .engine
        .active_group(binding.group_id)
        .is_err());
    assert!(world.clients[a]
        .engine
        .group_at(binding.group_id, 0)
        .is_err());
    for group in [conv, binding.group_id] {
        assert!(matches!(
            world.clients[a]
                .engine
                .encrypt(group, e2ee::Kind::Message, None, &envelope("blocked")),
            Err(Error::RepairPending)
        ));
        assert!(matches!(
            world.clients[a].engine.commit_pending(group),
            Err(Error::RepairPending)
        ));
        assert!(matches!(
            world.clients[a].engine.confirm_commit(group),
            Err(Error::RepairPending)
        ));
        assert!(world.clients[a].engine.pending_commit(group).is_none());
        assert!(world.clients[a]
            .engine
            .create_group(group, &world.ds.public())
            .is_err());
        assert!(world.clients[a].engine.forget_conversation(group).is_err());
    }
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, hash)
        .unwrap();
    let key = e2ee::store::StateKey::generate().unwrap();
    let saved = world.clients[a].engine.snapshot(&key, "repair").unwrap();
    world.clients[a].engine = e2ee::Engine::from_snapshot(&key, "repair", &saved).unwrap();
    assert!(
        world.clients[a]
            .engine
            .prepared_repair(conv)
            .unwrap()
            .submitted
    );
    assert!(world.clients[a]
        .engine
        .discard_prepared_repair(conv, hash)
        .is_err());
    assert!(matches!(
        world.clients[a].engine.encrypt(
            conv,
            e2ee::Kind::Message,
            None,
            &envelope("still blocked")
        ),
        Err(Error::RepairPending)
    ));
    let active = world.clients[a].engine.accept_repair(&binding).unwrap();
    assert_eq!(active.group_id, binding.group_id);
    assert_eq!(active.generation, 1);
    assert_eq!(active.head_hash, Some(hash));
    assert!(world.clients[a]
        .engine
        .active_group(binding.group_id)
        .is_err());
    assert!(world.clients[a]
        .engine
        .group_at(binding.group_id, 0)
        .is_err());
    assert!(world.clients[a].engine.prepared_repair(conv).is_none());
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, e2ee::Kind::Message, None, &envelope("retired")),
        Err(Error::RetiredGroup)
    ));
    world.clients[a]
        .engine
        .encrypt(
            active.group_id,
            e2ee::Kind::Message,
            None,
            &envelope("fresh"),
        )
        .unwrap();
}

#[test]
fn peers_require_current_trust_and_old_roster_and_keep_old_history() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let old = world.send(a, "history survives");
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let pins = world.clients[b].engine.public_trust().unwrap();
    world.clients[b].engine.accept_repair(&binding).unwrap();
    assert_eq!(world.clients[b].engine.public_trust().unwrap(), pins);
    assert_eq!(world.clients[b].engine.group_at(conv, 0).unwrap(), conv);
    assert_eq!(
        world.clients[b].engine.group_at(conv, 1).unwrap(),
        binding.group_id
    );
    assert_eq!(
        world.open(b, &old, a).unwrap().envelope.text,
        "history survives"
    );
    assert!(world.clients[b]
        .engine
        .create_group(binding.group_id, &world.ds.public())
        .is_err());
    assert!(world.clients[b]
        .engine
        .bootstrap_repair(&binding, &world.ds.public())
        .is_err());
    assert_eq!(
        world.clients[b]
            .engine
            .accept_repair(&binding)
            .unwrap()
            .generation,
        1
    );
    assert!(world.clients[b]
        .engine
        .handle_welcome(conv, &[1], Some(&world.ds.public()))
        .is_err());
    assert!(matches!(
        world.clients[b]
            .engine
            .encrypt(conv, e2ee::Kind::Message, None, &envelope("retired")),
        Err(Error::RetiredGroup)
    ));
}

#[test]
fn a_prepared_attempt_can_only_be_discarded_before_submission() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    world.clients[a]
        .engine
        .discard_prepared_repair(conv, binding.payload_hash().unwrap())
        .unwrap();
    assert!(world.clients[a].engine.prepared_repair(conv).is_none());
    world.send(a, "old group is usable again");
    assert!(world.clients[a].engine.has_group(conv));
    assert!(!world.clients[a].engine.has_group(binding.group_id));
}

#[test]
fn an_unaccepted_candidate_cannot_process_membership_or_be_accepted_before_submission() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    world.ds.conv = binding.group_id;
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], 0);
    assert!(matches!(
        world.clients[a]
            .engine
            .handle_proposals(binding.group_id, &proposals),
        Err(Error::RepairPending)
    ));
    assert!(
        world.clients[a].engine.accept_repair(&binding).is_err(),
        "an unsubmitted local candidate is not a service-accepted repair"
    );
}

#[test]
fn an_initiator_revoked_while_prepared_cannot_submit_or_retry_the_binding() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let own = world.clients[a].user();
    world.directory.get_mut(&own).unwrap().clear();
    world.sync_trust();
    assert!(world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .is_err());
    assert!(world.clients[a].engine.prepare_repair(conv, 0, 7).is_err());
    assert!(world.clients[a].engine.prepared_repair(conv).is_some());
}

#[test]
fn a_new_device_bootstraps_independently_then_every_peer_joins_strictly() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let c = world.person("new device");
    world.sync_trust();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let hash = binding.payload_hash().unwrap();
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, hash)
        .unwrap();
    world.clients[a].engine.accept_repair(&binding).unwrap();
    world.clients[b].engine.accept_repair(&binding).unwrap();
    assert!(world.clients[c]
        .engine
        .active_group(conv)
        .unwrap()
        .head_hash
        .is_none());
    world.clients[c]
        .engine
        .bootstrap_repair(&binding, &world.ds.public())
        .unwrap();
    assert!(!world.clients[c].engine.has_group(binding.group_id));
    assert!(
        world.clients[c]
            .engine
            .create_group(conv, &world.ds.public())
            .is_err(),
        "outer ID cannot overwrite a bootstrapped head with generation0"
    );
    world.ds.conv = binding.group_id;
    world.change(&[a], a, &[b, c], &[]);
    world.say(a, &[b, c], "only fresh strictly approved members");
    assert!(matches!(
        world.clients[c].engine.group_at(conv, 0),
        Err(Error::NoGroup)
    ));
    assert_eq!(
        world.clients[c].engine.group_at(conv, 1).unwrap(),
        binding.group_id
    );
    assert!(world.clients[c]
        .engine
        .bootstrap_repair(&binding, &world.ds.public())
        .is_err());
}

#[test]
fn a_pending_self_removal_and_prepared_pin_changes_cannot_bypass_repair_guards() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let leaf = world.leaf(a, a);
    let epoch = world.epoch(a);
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[], &[leaf], epoch))
        .unwrap();
    assert!(world.clients[a]
        .engine
        .prepare_repair(conv, 0, epoch)
        .is_err());

    let (mut world, a, _) = pair();
    let conv = world.conv();
    world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let different_key = e2ee::identity::DeviceKey::generate().unwrap().public_key();
    assert!(world.clients[a]
        .engine
        .accept_server_key(conv, &different_key)
        .is_err());
    let binding = world.clients[a]
        .engine
        .prepared_repair(conv)
        .unwrap()
        .binding;
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .unwrap();
    let key = StateKey::generate().unwrap();
    let snapshot = world.clients[a]
        .engine
        .snapshot(&key, "uncertain-pin")
        .unwrap();
    world.clients[a].engine =
        e2ee::Engine::from_snapshot(&key, "uncertain-pin", &snapshot).unwrap();
    assert!(world.clients[a]
        .engine
        .accept_server_key(conv, &different_key)
        .is_err());
    assert_eq!(
        world.clients[a]
            .engine
            .prepared_repair(conv)
            .unwrap()
            .binding,
        binding
    );
    assert_eq!(
        world.clients[a].engine.server_key(conv).unwrap(),
        world.ds.public()
    );
}

#[test]
fn a_valid_competing_winner_resolves_an_uncertain_local_candidate() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let winner = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let loser = world.clients[b].engine.prepare_repair(conv, 0, 7).unwrap();
    world.clients[b]
        .engine
        .mark_repair_submitted(conv, loser.payload_hash().unwrap())
        .unwrap();
    world.clients[b].engine.accept_repair(&winner).unwrap();
    assert!(world.clients[b].engine.prepared_repair(conv).is_none());
    assert!(!world.clients[b].engine.has_group(loser.group_id));
    assert_eq!(
        world.clients[b].engine.active_group(conv).unwrap().group_id,
        winner.group_id
    );
    assert!(world.clients[b].engine.accept_repair(&loser).is_err());
}

#[test]
fn current_trust_alone_cannot_authorize_a_nonmember_or_a_missed_generation() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let c = world.person("trusted nonmember");
    world.sync_trust();
    world.clients[c]
        .engine
        .create_group(conv, &world.ds.public())
        .unwrap();
    let outsiders = world.clients[c].engine.prepare_repair(conv, 0, 7).unwrap();
    assert!(world.clients[b].engine.accept_repair(&outsiders).is_err());
    let first = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, first.payload_hash().unwrap())
        .unwrap();
    world.clients[a].engine.accept_repair(&first).unwrap();
    let second = world.clients[a].engine.prepare_repair(conv, 1, 0).unwrap();
    assert!(world.clients[b].engine.accept_repair(&second).is_err());
    assert!(world.clients[b]
        .engine
        .bootstrap_repair(&second, &world.ds.public())
        .is_err());
    assert_eq!(
        world.clients[b]
            .engine
            .active_group(conv)
            .unwrap()
            .generation,
        0
    );
    let a_user = world.clients[a].user();
    world.directory.get_mut(&a_user).unwrap().clear();
    world.sync_trust();
    assert!(world.clients[b].engine.accept_repair(&first).is_err());
}

#[test]
fn legacy_explicit_server_pin_changes_round_trip_but_signed_generation_pins_cannot_change() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let replacement = e2ee::identity::DeviceKey::generate().unwrap().public_key();
    world.clients[a]
        .engine
        .accept_server_key(conv, &replacement)
        .unwrap();
    let key = e2ee::store::StateKey::generate().unwrap();
    let saved = world.clients[a]
        .engine
        .snapshot(&key, "legacy-pin")
        .unwrap();
    e2ee::Engine::from_snapshot(&key, "legacy-pin", &saved).unwrap();
    // Restore the old pin, which is also the MLS context's external sender.
    world.clients[a]
        .engine
        .accept_server_key(conv, &world.ds.public())
        .unwrap();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .unwrap();
    world.clients[a].engine.accept_repair(&binding).unwrap();
    assert!(world.clients[a]
        .engine
        .accept_server_key(binding.group_id, &replacement)
        .is_err());
    let saved = world.clients[a]
        .engine
        .snapshot(&key, "modern-pin")
        .unwrap();
    e2ee::Engine::from_snapshot(&key, "modern-pin", &saved).unwrap();
}

#[test]
fn failed_prepare_and_submission_writes_never_publish_a_request_or_unlock_groups() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let (store, key) = persist(&mut world, a);
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.clients[a].engine.prepare_repair(conv, 0, 7),
        Err(Error::Storage(_))
    ));
    assert!(world.clients[a].engine.prepared_repair(conv).is_none());
    assert_eq!(
        world.clients[a]
            .engine
            .active_group(conv)
            .unwrap()
            .generation,
        0
    );
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, e2ee::Kind::Message, None, &envelope("write failed")),
        Err(Error::RepairPending)
    ));
    store.fail.store(false, Ordering::SeqCst);
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.clients[a]
            .engine
            .mark_repair_submitted(conv, binding.payload_hash().unwrap()),
        Err(Error::Storage(_))
    ));
    world.clients[a].engine = e2ee::Engine::open(Box::new(store.clone()), key, "repair")
        .unwrap()
        .unwrap();
    assert!(
        !world.clients[a]
            .engine
            .prepared_repair(conv)
            .unwrap()
            .submitted
    );
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, e2ee::Kind::Message, None, &envelope("still locked")),
        Err(Error::RepairPending)
    ));
    store.fail.store(false, Ordering::SeqCst);
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .unwrap();
}

#[test]
fn failed_acceptance_retains_the_submitted_creator_keys_across_reopen() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let (store, key) = persist(&mut world, a);
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.clients[a].engine.accept_repair(&binding),
        Err(Error::Storage(_))
    ));
    assert_eq!(
        world.clients[a]
            .engine
            .active_group(conv)
            .unwrap()
            .generation,
        0
    );
    world.clients[a].engine = e2ee::Engine::open(Box::new(store.clone()), key, "repair")
        .unwrap()
        .unwrap();
    assert_eq!(
        world.clients[a]
            .engine
            .prepared_repair(conv)
            .unwrap()
            .binding,
        binding
    );
    assert!(matches!(
        world.clients[a].engine.encrypt(
            binding.group_id,
            e2ee::Kind::Message,
            None,
            &envelope("blocked")
        ),
        Err(Error::RepairPending)
    ));
    store.fail.store(false, Ordering::SeqCst);
    world.clients[a].engine.accept_repair(&binding).unwrap();
    world.clients[b].engine.accept_repair(&binding).unwrap();
    world.ds.conv = binding.group_id;
    world.change(&[a], a, &[b], &[]);
    world.say(a, &[b], "the original prepared keys survived");
}

#[test]
fn pending_bundles_and_test_export_helpers_cannot_bypass_retirement() {
    let (mut world, a, _) = pair();
    let conv = world.conv();
    let c = world.person("waiting device");
    world.sync_trust();
    let package = world.clients[c].engine.key_packages(1).unwrap().remove(0);
    let epoch = world.epoch(a);
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], epoch))
        .unwrap();
    world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    assert!(world.clients[a].engine.pending_commit(conv).is_some());
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    assert!(world.clients[a].engine.pending_commit(conv).is_none());
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .unwrap();
    world.clients[a].engine.accept_repair(&binding).unwrap();
    assert!(world.clients[a].engine.pending_commit(conv).is_none());
    assert!(matches!(
        world.clients[a].engine.confirm_commit(conv),
        Err(Error::RetiredGroup)
    ));
    assert!(matches!(
        world.clients[a].engine.handle_welcome(conv, &[1], None),
        Err(Error::RetiredGroup)
    ));
    let signer = e2ee::identity::DeviceKey::generate().unwrap();
    assert!(matches!(
        world.clients[a]
            .engine
            .forge_message(conv, &signer, &envelope("old")),
        Err(Error::RetiredGroup)
    ));
    assert!(matches!(
        world.clients[a].engine.forge_commit(
            conv,
            e2ee::groups::Forge::ChangeServerKey(&signer.public_key())
        ),
        Err(Error::RetiredGroup)
    ));
}

#[test]
fn another_conversations_success_cannot_clear_a_failed_acceptance_fence() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let other = Ds::new();
    world.clients[a]
        .engine
        .create_group(other.conv, &other.public())
        .unwrap();
    let (store, _) = persist(&mut world, a);
    let binding = world.clients[b].engine.prepare_repair(conv, 0, 7).unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.clients[a].engine.accept_repair(&binding),
        Err(Error::Storage(_))
    ));
    store.fail.store(false, Ordering::SeqCst);
    world.clients[a]
        .engine
        .prepare_repair(other.conv, 0, 0)
        .unwrap();
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, e2ee::Kind::Message, None, &envelope("still fenced")),
        Err(Error::RepairPending)
    ));
    world.clients[a].engine.accept_repair(&binding).unwrap();
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, e2ee::Kind::Message, None, &envelope("now retired")),
        Err(Error::RetiredGroup)
    ));
}

#[test]
fn an_old_welcome_cannot_overwrite_a_positive_bootstrap_head_or_spend_its_key_package() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("new peer"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], 0))
        .unwrap();
    let old_bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    world.clients[a].engine.confirm_commit(conv).unwrap();
    let binding = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    world.clients[a]
        .engine
        .mark_repair_submitted(conv, binding.payload_hash().unwrap())
        .unwrap();
    world.clients[a].engine.accept_repair(&binding).unwrap();
    let accepted = world.clients[b]
        .engine
        .bootstrap_repair(&binding, &world.ds.public())
        .unwrap();
    assert!(
        world.clients[b]
            .engine
            .handle_welcome(conv, &old_bundle.welcomes[0].data, Some(&world.ds.public()))
            .is_err(),
        "an old valid Welcome cannot replace the accepted bootstrap head"
    );
    assert_eq!(
        world.clients[b].engine.active_group(conv).unwrap(),
        accepted
    );
    assert!(!world.clients[b].engine.has_group(conv));

    // Reuse this exact package in the new group to prove rejection did not spend its private
    // init key. Both groups are real MLS states; production servers claim packages only once.
    world.ds.conv = binding.group_id;
    world.clients[a]
        .engine
        .handle_proposals(binding.group_id, &world.ds.proposals(&[&package], &[], 0))
        .unwrap();
    let fresh = world.clients[a]
        .engine
        .commit_pending(binding.group_id)
        .unwrap()
        .unwrap();
    world.clients[a]
        .engine
        .confirm_commit(binding.group_id)
        .unwrap();
    world.clients[b]
        .engine
        .handle_welcome(
            binding.group_id,
            &fresh.welcomes[0].data,
            Some(&world.ds.public()),
        )
        .unwrap();
    let key = StateKey::generate().unwrap();
    let snapshot = world.clients[b].engine.snapshot(&key, "bootstrap").unwrap();
    world.clients[b].engine = e2ee::Engine::from_snapshot(&key, "bootstrap", &snapshot).unwrap();
    assert_eq!(
        world.clients[b].engine.active_group(conv).unwrap(),
        accepted
    );
    world.say(a, &[b], "strict new Welcome still joins");
}

#[test]
fn failed_acceptance_cannot_be_forgotten_by_preparing_or_discarding_a_candidate() {
    for had_candidate in [true, false] {
        let (mut world, a, b) = pair();
        let conv = world.conv();
        let (store, key) = persist(&mut world, a);
        let candidate =
            had_candidate.then(|| world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap());
        let observed = world.clients[b].engine.prepare_repair(conv, 0, 7).unwrap();
        store.fail.store(true, Ordering::SeqCst);
        assert!(matches!(
            world.clients[a].engine.accept_repair(&observed),
            Err(Error::Storage(_))
        ));
        store.fail.store(false, Ordering::SeqCst);
        let candidate = if let Some(candidate) = candidate {
            Some(candidate)
        } else {
            match world.clients[a].engine.prepare_repair(conv, 0, 7) {
                Ok(candidate) => Some(candidate),
                Err(Error::RepairPending) => None,
                other => panic!("unexpected preparation result: {other:?}"),
            }
        };
        if let Some(candidate) = candidate {
            world.clients[a]
                .engine
                .discard_prepared_repair(conv, candidate.payload_hash().unwrap())
                .unwrap();
        }
        assert!(
            matches!(
                world.clients[a].engine.encrypt(
                    conv,
                    e2ee::Kind::Message,
                    None,
                    &envelope("still fenced")
                ),
                Err(Error::RepairPending)
            ),
            "generic candidate operations forgot the observed successor"
        );
        world.clients[a].engine.flush().unwrap();
        world.clients[a].engine = e2ee::Engine::open(Box::new(store.clone()), key, "repair")
            .unwrap()
            .unwrap();
        assert!(matches!(
            world.clients[a].engine.encrypt(
                conv,
                e2ee::Kind::Message,
                None,
                &envelope("locked after restart")
            ),
            Err(Error::RepairPending)
        ));
        world.clients[a].engine.accept_repair(&observed).unwrap();
        assert_eq!(
            world.clients[a].engine.active_group(conv).unwrap().group_id,
            observed.group_id
        );
        assert!(matches!(
            world.clients[a]
                .engine
                .encrypt(conv, e2ee::Kind::Message, None, &envelope("retired")),
            Err(Error::RetiredGroup)
        ));
    }
}

#[test]
fn an_observed_successor_cannot_be_replaced_by_another_authenticated_immediate_fork() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let c = world.person("other old member");
    world.sync_trust();
    world.change(&[a, b], a, &[c], &[]);
    let (store, _) = persist(&mut world, a);
    let observed = world.clients[b].engine.prepare_repair(conv, 0, 7).unwrap();
    let fork = world.clients[c].engine.prepare_repair(conv, 0, 7).unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.clients[a].engine.accept_repair(&observed),
        Err(Error::Storage(_))
    ));
    store.fail.store(false, Ordering::SeqCst);
    assert!(
        world.clients[a].engine.accept_repair(&fork).is_err(),
        "an authenticated fork cannot replace acceptance evidence"
    );
    world.clients[a].engine.accept_repair(&observed).unwrap();
}

#[test]
fn acceptance_evidence_is_durable_before_activation_and_survives_candidate_discard() {
    let (mut world, a, b) = pair();
    let conv = world.conv();
    let (store, key) = persist(&mut world, a);
    let candidate = world.clients[a].engine.prepare_repair(conv, 0, 7).unwrap();
    let observed = world.clients[b].engine.prepare_repair(conv, 0, 7).unwrap();
    // The first acceptance save records the authenticated successor. Fail only the next
    // save which would publish the new active mapping and dispose of the local candidate.
    store
        .fail_on_write
        .store(store.writes.load(Ordering::SeqCst) + 2, Ordering::SeqCst);
    assert!(matches!(
        world.clients[a].engine.accept_repair(&observed),
        Err(Error::Storage(_))
    ));
    world.clients[a].engine = e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair")
        .unwrap()
        .unwrap();
    assert_eq!(
        world.clients[a]
            .engine
            .active_group(conv)
            .unwrap()
            .generation,
        0
    );
    assert_eq!(
        world.clients[a]
            .engine
            .prepared_repair(conv)
            .unwrap()
            .binding,
        candidate
    );
    world.clients[a]
        .engine
        .discard_prepared_repair(conv, candidate.payload_hash().unwrap())
        .unwrap();
    world.clients[a].engine = e2ee::Engine::open(Box::new(store.clone()), key, "repair")
        .unwrap()
        .unwrap();
    assert!(world.clients[a].engine.prepared_repair(conv).is_none());
    assert!(matches!(
        world.clients[a].engine.encrypt(
            conv,
            e2ee::Kind::Message,
            None,
            &envelope("cannot revert")
        ),
        Err(Error::RepairPending)
    ));
    assert!(
        world.clients[a].engine.accept_repair(&candidate).is_err(),
        "the discarded fork cannot replace durable acceptance evidence"
    );
    world.clients[a].engine.accept_repair(&observed).unwrap();
    assert_eq!(
        world.clients[a].engine.active_group(conv).unwrap().group_id,
        observed.group_id
    );
}

fn namespace_bindings() -> (e2ee::Engine, Binding, Binding, Binding, Vec<u8>) {
    let mut engines: Vec<_> = [11u8, 12, 13]
        .into_iter()
        .map(|scalar| {
            e2ee::Engine::with_identity(e2ee::identity::DeviceIdentity {
                user: e2ee::UserId::random().unwrap(),
                device: e2ee::DeviceId::random().unwrap(),
                key: e2ee::identity::DeviceKey::from_bytes(&[scalar; 32]).unwrap(),
            })
        })
        .collect();
    let records: Vec<_> = engines
        .iter()
        .map(|engine| {
            (
                engine.user_id(),
                e2ee::identity::DeviceRecord {
                    device_id: engine.device_id(),
                    signature_key: engine.signature_key(),
                    cross_signature: None,
                    created_at: 0,
                },
            )
        })
        .collect();
    for engine in &mut engines {
        for (user, record) in &records {
            engine
                .verify_devices(*user, std::slice::from_ref(record))
                .unwrap();
        }
    }
    let server = e2ee::identity::DeviceKey::generate().unwrap().public_key();
    let outer = ConversationId::random().unwrap();
    engines[0].create_group(outer, &server).unwrap();
    let first = engines[0].prepare_repair(outer, 0, 0).unwrap();
    engines[1].create_group(first.group_id, &server).unwrap();
    let target_as_outer = engines[1].prepare_repair(first.group_id, 0, 0).unwrap();
    let other = ConversationId::random().unwrap();
    engines[1].create_group(other, &server).unwrap();
    let mut outer_as_target = engines[1].prepare_repair(other, 0, 0).unwrap();
    outer_as_target.group_id = first.conversation_id;
    // A real independently authorized old member signs the colliding public binding.
    outer_as_target.signature = e2ee::identity::DeviceKey::from_bytes(&[12; 32])
        .unwrap()
        .sign(&outer_as_target.canonical_payload().unwrap());
    (
        engines.pop().unwrap(),
        first,
        target_as_outer,
        outer_as_target,
        server,
    )
}

fn check_observed_bootstrap_namespace(target_is_outer: bool) {
    let (mut engine, first, next, reverse, server) = namespace_bindings();
    let store = SwitchStore::default();
    let key = StateKey::generate().unwrap();
    store
        .save("repair", &engine.snapshot(&key, "repair").unwrap())
        .unwrap();
    engine = e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair")
        .unwrap()
        .unwrap();
    store
        .fail_on_write
        .store(store.writes.load(Ordering::SeqCst) + 2, Ordering::SeqCst);
    assert!(matches!(
        engine.bootstrap_repair(&first, &server),
        Err(Error::Storage(_))
    ));
    engine = e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair")
        .unwrap()
        .unwrap();
    let collision = if target_is_outer { &next } else { &reverse };
    assert!(
        engine.bootstrap_repair(collision, &server).is_err(),
        "an observed bootstrap reserves both outer and target UUIDs"
    );
    for reserved in [first.conversation_id, first.group_id] {
        assert!(engine.active_group(reserved).is_err());
        assert!(engine.group_at(reserved, 0).is_err());
        assert!(engine.prepare_repair(reserved, 0, 0).is_err());
        assert!(engine.create_group(reserved, &server).is_err());
    }
    let mut fork = first.clone();
    fork.group_id = next.group_id;
    fork.signature = e2ee::identity::DeviceKey::from_bytes(&[11; 32])
        .unwrap()
        .sign(&fork.canonical_payload().unwrap());
    assert!(engine.bootstrap_repair(&fork, &server).is_err());
    let accepted = engine.bootstrap_repair(&first, &server).unwrap();
    assert_eq!(accepted.group_id, first.group_id);
    let snapshot = engine.snapshot(&key, "repair").unwrap();
    let restored = e2ee::Engine::from_snapshot(&key, "repair", &snapshot).unwrap();
    assert_eq!(
        restored.active_group(first.conversation_id).unwrap(),
        accepted
    );
    assert!(restored.active_group(first.group_id).is_err());
}

#[test]
fn an_observed_bootstrap_target_cannot_become_another_outer_conversation() {
    check_observed_bootstrap_namespace(true);
}

#[test]
fn an_observed_bootstrap_outer_cannot_become_another_target_group() {
    check_observed_bootstrap_namespace(false);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, max_shrink_iters: 128, ..ProptestConfig::default() })]

    #[test]
    fn bounded_repair_state_sequences_always_reopen_without_namespace_aliases(
        actions in prop::collection::vec(0u8..11, 0..16)
    ) {
        let (mut engine, first, next, reverse, server) = namespace_bindings();
        let other = ConversationId::random().unwrap();
        engine.create_group(other, &server).unwrap();
        let store = SwitchStore::default();
        let key = StateKey::generate().unwrap();
        store.save("repair", &engine.snapshot(&key, "repair").unwrap()).unwrap();
        engine = e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair").unwrap().unwrap();
        // Always traverse the actual review failure before the generated suffix: candidate
        // cancellation/submission, failed second bootstrap save, both collisions, exact retry.
        let actions = [0, 2, 0, 1, 2, 6, 4, 5, 3].into_iter().chain(actions);
        for (step, action) in actions.enumerate() {
            let candidate = engine.prepared_repair(other);
            let result = match action {
                0 => {
                    let generation = engine.active_group(other).unwrap().generation;
                    engine.prepare_repair(other, generation, 0).map(|_| ())
                }
                1 => candidate.as_ref().ok_or(Error::NoGroup).and_then(|candidate|
                    engine.mark_repair_submitted(other, candidate.binding.payload_hash().unwrap())),
                2 => candidate.as_ref().ok_or(Error::NoGroup).and_then(|candidate|
                    engine.discard_prepared_repair(other, candidate.binding.payload_hash().unwrap())),
                3 => engine.bootstrap_repair(&first, &server).map(|_| ()),
                4 => engine.bootstrap_repair(&next, &server).map(|_| ()),
                5 => engine.bootstrap_repair(&reverse, &server).map(|_| ()),
                6 => {
                    store.fail_on_write.store(store.writes.load(Ordering::SeqCst) + 2, Ordering::SeqCst);
                    engine.bootstrap_repair(&first, &server).map(|_| ())
                }
                7 => engine.flush(),
                8 => e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair")
                    .and_then(|restored| { engine = restored.ok_or(Error::NoGroup)?; Ok(()) }),
                9 => engine.create_group(first.group_id, &server),
                _ => candidate.as_ref().ok_or(Error::NoGroup).and_then(|candidate| {
                    engine.mark_repair_submitted(other, candidate.binding.payload_hash().unwrap())?;
                    engine.accept_repair(&candidate.binding).map(|_| ())
                }),
            };
            store.fail_on_write.store(0, Ordering::SeqCst);
            let snapshot = engine.snapshot(&key, "repair").unwrap();
            let restored = e2ee::Engine::from_snapshot(&key, "repair", &snapshot);
            prop_assert!(restored.is_ok(), "step={step}, action={action}, result={result:?}, reopen={:?}", restored.err());
            prop_assert!(e2ee::Engine::open(Box::new(store.clone()), key.clone(), "repair").is_ok(), "durable state cannot reopen at step={step}, action={action}");
            for outer in [other, first.conversation_id, next.conversation_id, reverse.conversation_id] {
                if let Ok(active) = engine.active_group(outer) {
                    if active.generation > 0 {
                        prop_assert!(engine.active_group(active.group_id).is_err(), "a positive internal UUID became a different outer context");
                        prop_assert!(engine.group_at(active.group_id, 0).is_err(), "an internal UUID fell back to legacy0");
                    }
                }
            }
        }
    }
}
