mod common;

use common::*;
use e2ee::envelope::Kind;
use e2ee::groups::{ProposalKind, RejectReason};
use e2ee::identity::{DeviceKey, DeviceRecord};
use e2ee::{Claimed, Error, UserId};

#[test]
fn three_devices_add_remove_and_epoch_change() {
    let mut world = World::new();
    let (a, b, c) = (
        world.person("alice"),
        world.person("bob"),
        world.person("carol"),
    );
    world.sync_trust();
    world.start(a);
    assert_eq!(world.epoch(a), 0);

    // Add both in one commit.
    world.change(&[a], a, &[b, c], &[]);
    assert_eq!(world.epoch(a), 1);
    assert_eq!(world.epoch(b), 1);
    assert_eq!(world.epoch(c), 1);
    let conv = world.conv();
    assert_eq!(world.clients[b].engine.roster(conv).unwrap().len(), 3);

    world.say(a, &[b, c], "hello from alice");
    world.say(b, &[a, c], "hello from bob");
    world.say(c, &[a, b], "hello from carol");

    // Remove bob: carol commits.
    world.change(&[a, b, c], c, &[], &[b]);
    assert_eq!(world.epoch(a), 2);
    assert_eq!(world.epoch(c), 2);
    assert_eq!(world.clients[a].engine.roster(conv).unwrap().len(), 2);
    world.say(a, &[c], "bob is gone");
    world.say(c, &[a], "yes");
}

#[test]
fn a_removed_device_cannot_read_later_messages_and_a_new_one_cannot_read_earlier() {
    let mut world = World::new();
    let (a, b, c, d) = (
        world.person("a"),
        world.person("b"),
        world.person("c"),
        world.person("d"),
    );
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);
    let before = world.send(a, "before d joined");
    world.say(a, &[b, c], "visible to b and c");

    world.change(&[a, b, c], a, &[], &[b]);
    let after_removal = world.send(a, "after b left");
    assert!(
        world.open(b, &after_removal, a).is_err(),
        "a removed device read a later message"
    );
    // b is evicted: its group state is gone.
    assert!(
        !world.clients[b].engine.has_group(world.conv())
            || world.open(b, &after_removal, a).is_err()
    );
    world.open(c, &after_removal, a).unwrap();

    world.change(&[a, c], a, &[d], &[]);
    let err = world
        .open(d, &before, a)
        .expect_err("a new device read an earlier message");
    assert!(
        matches!(err, Error::Undecryptable | Error::NoGroup),
        "{err:?}"
    );
    world.say(a, &[c, d], "everyone present can read this");
}

#[test]
fn twenty_devices_add_remove_and_commit() {
    let (mut world, all) = group_of(20);
    let epoch = world.epoch(all[0]);
    assert_eq!(epoch, 19);
    // Everyone reads everyone.
    for from in [all[0], all[7], all[19]] {
        let others: Vec<usize> = all.iter().copied().filter(|at| *at != from).collect();
        world.say(from, &others, &format!("hello from {from}"));
    }
    // Remove five at once, committed by one of the others.
    let gone = [all[3], all[4], all[10], all[11], all[18]];
    let stay: Vec<usize> = all
        .iter()
        .copied()
        .filter(|at| !gone.contains(at))
        .collect();
    world.change(&all, all[5], &[], &gone);
    assert_eq!(world.epoch(all[0]), epoch + 1);
    let ciphertext = world.send(all[0], "after the purge");
    for at in &gone {
        assert!(world.open(*at, &ciphertext, all[0]).is_err());
    }
    for at in stay.iter().filter(|at| **at != all[0]) {
        world.open(*at, &ciphertext, all[0]).unwrap();
    }
    assert_eq!(world.roster_len(all[0]), 15);
}

#[test]
fn replay_tamper_and_wrong_sender_are_refused() {
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);

    // Replay: the same ciphertext twice.
    let ciphertext = world.send(a, "once");
    world.open(b, &ciphertext, a).unwrap();
    assert!(matches!(
        world.open(b, &ciphertext, a),
        Err(Error::Undecryptable)
    ));

    // Tamper: flipping any byte fails (in parsing or in the AEAD, before any key is spent), and
    // the real message still opens. A forgery that gets past the AEAD is
    // `a_forged_signature_does_not_spend_the_real_message_key`.
    let ciphertext = world.send(a, "genuine");
    for at in [0, 9, ciphertext.len() / 2, ciphertext.len() - 1] {
        let mut bad = ciphertext.clone();
        bad[at] ^= 0x01;
        assert!(
            world.open(b, &bad, a).is_err(),
            "flipped byte {at} was accepted"
        );
    }
    let opened = world
        .open(b, &ciphertext, a)
        .expect("the real message still opens after tampered copies");
    assert_eq!(opened.envelope.text, "genuine");

    // Truncation.
    let ciphertext = world.send(a, "cut");
    assert!(world
        .open(c, &ciphertext[..ciphertext.len() - 1], a)
        .is_err());
    world.open(c, &ciphertext, a).unwrap();

    // Wrong sender: the server claims another author, or another device.
    let ciphertext = world.send(a, "from alice");
    let conv = world.conv();
    let (bob_user, bob_device) = (world.clients[b].user(), world.clients[b].device());
    let err = world.clients[c].engine.decrypt(
        conv,
        &ciphertext,
        &Claimed {
            author: Some(bob_user),
            device: None,
            kind: Kind::Message,
            target_id: None,
        },
    );
    assert!(matches!(err, Err(Error::SenderMismatch)), "{err:?}");
    // A refused message spends no key: under the true claims the same ciphertext opens.
    let opened = world.open(c, &ciphertext, a).unwrap();
    assert_eq!(opened.envelope.text, "from alice");
    let ciphertext = world.send(a, "from alice again");
    let err = world.clients[c].engine.decrypt(
        conv,
        &ciphertext,
        &Claimed {
            author: None,
            device: Some(bob_device),
            kind: Kind::Message,
            target_id: None,
        },
    );
    assert!(matches!(err, Err(Error::SenderMismatch)), "{err:?}");

    // Not an application message at all.
    assert!(world.clients[c]
        .engine
        .decrypt(
            conv,
            &[1, 2, 3],
            &Claimed {
                author: None,
                device: None,
                kind: Kind::Message,
                target_id: None
            }
        )
        .is_err());
}

#[test]
fn edits_are_bound_to_their_kind_target_and_author() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    let (alice, bob) = (world.clients[a].user(), world.clients[b].user());

    let edit = world.clients[a]
        .engine
        .encrypt(
            conv,
            Kind::Edit,
            Some("msg-1"),
            &e2ee::Envelope::message("fixed"),
        )
        .unwrap();
    let opened = world.clients[b]
        .engine
        .decrypt(
            conv,
            &edit.ciphertext,
            &Claimed {
                author: Some(alice),
                device: None,
                kind: Kind::Edit,
                target_id: Some("msg-1"),
            },
        )
        .unwrap();
    assert_eq!(opened.envelope.text, "fixed");
    // Only the target's author may edit it.
    opened.authorize_edit(alice).unwrap();
    assert!(matches!(
        opened.authorize_edit(bob),
        Err(Error::SenderMismatch)
    ));

    // A server that relabels the row (another target, or an edit as a message) is caught.
    for (kind, target) in [(Kind::Edit, Some("msg-2")), (Kind::Message, None)] {
        let again = world.clients[a]
            .engine
            .encrypt(
                conv,
                Kind::Edit,
                Some("msg-1"),
                &e2ee::Envelope::message("again"),
            )
            .unwrap();
        let err = world.clients[b].engine.decrypt(
            conv,
            &again.ciphertext,
            &Claimed {
                author: None,
                device: None,
                kind,
                target_id: target,
            },
        );
        assert!(matches!(err, Err(Error::AadMismatch)), "{err:?}");
    }
    // An edit must name its target and a message must not.
    assert!(world.clients[a]
        .engine
        .encrypt(conv, Kind::Edit, None, &e2ee::Envelope::message("x"))
        .is_err());
    assert!(world.clients[a]
        .engine
        .encrypt(
            conv,
            Kind::Message,
            Some("t"),
            &e2ee::Envelope::message("x")
        )
        .is_err());
}

#[test]
fn an_add_of_a_device_that_is_not_cross_signed_is_rejected() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    let epoch = world.epoch(a);

    // The server invents a device for Bob: a fresh key, no cross-signature.
    let bob = world.clients[b].user();
    let mut ghost =
        e2ee::Engine::with_identity(e2ee::identity::DeviceIdentity::generate(bob).unwrap());
    let package = ghost.key_packages(1).unwrap().remove(0);
    let ghost_record = DeviceRecord {
        device_id: ghost.device_id(),
        signature_key: ghost.signature_key(),
        cross_signature: None,
        created_at: 99,
    };
    let proposals = world.ds.proposals(&[&package], &[], epoch);

    // The client has not heard of it: the Add is refused as unknown.
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert!(outcome.accepted.is_empty());
    assert_eq!(outcome.rejected.len(), 1);
    assert_eq!(outcome.rejected[0].reason, RejectReason::UnknownDevice);

    // The server also lists the ghost with Bob's devices: the chain verifier refuses it.
    let mut list = world.directory[&bob].clone();
    list.push(ghost_record);
    let report = world.clients[a].engine.verify_devices(bob, &list).unwrap();
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(
        report.rejected[0].1,
        e2ee::identity::Rejection::NotCrossSigned
    );
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert!(
        outcome.accepted.is_empty(),
        "an un-cross-signed device was accepted"
    );
    assert!(
        world.clients[a]
            .engine
            .commit_pending(conv)
            .unwrap()
            .is_none(),
        "a rejected Add left something to commit"
    );

    // A ghost that borrows a trusted device's id, with its own key, is refused as a mismatch.
    let mut impostor = e2ee::Engine::with_identity(e2ee::identity::DeviceIdentity {
        user: bob,
        device: world.clients[b].device(),
        key: DeviceKey::generate().unwrap(),
    });
    let package = impostor.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], epoch);
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    // Bob's real device is already a member: the Add is refused for that before anything else.
    assert!(matches!(
        outcome.rejected[0].reason,
        RejectReason::AlreadyMember | RejectReason::KeyMismatch
    ));
    assert!(outcome.accepted.is_empty());
}

#[test]
fn proposals_not_signed_by_the_pinned_server_key_are_invalid() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    let attacker = DeviceKey::generate().unwrap();
    let proposals = world.ds.proposals_signed_by(&attacker, &[&package], &[], 0);
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert!(outcome.accepted.is_empty());
    assert_eq!(outcome.rejected[0].reason, RejectReason::Invalid);

    // The real server's, for the same key package, is taken.
    let proposals = world.ds.proposals(&[&package], &[], 0);
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert!(matches!(
        outcome.accepted.as_slice(),
        [ProposalKind::Add(_)]
    ));
    // Feeding it again changes nothing.
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert_eq!(outcome.already, 1);
    assert!(outcome.accepted.is_empty());
}

#[test]
fn a_remove_of_a_leaf_not_in_the_roster_is_rejected() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    let proposals = world.ds.proposals(&[], &[17], 1);
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert!(outcome.accepted.is_empty());
    assert_eq!(outcome.rejected[0].reason, RejectReason::NotInRoster);
}

#[test]
fn proposals_arrive_in_each_of_the_three_forms_and_stale_ones_are_ignored() {
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    let vector = world.ds.proposals(&[&package], &[], 0);

    // DAVE's form: a one byte operation, then the vector.
    let mut dave = vec![0u8];
    dave.extend_from_slice(&vector);
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &dave)
        .unwrap();
    assert_eq!(outcome.accepted.len(), 1);
    // The vector alone: already pending.
    assert_eq!(
        world.clients[a]
            .engine
            .handle_proposals(conv, &vector)
            .unwrap()
            .already,
        1
    );
    // A message from a later epoch is a gap to fill first; one from an earlier is stale.
    let package_c = world.clients[c].engine.key_packages(1).unwrap().remove(0);
    let future = world.ds.proposals(&[&package_c], &[], 5);
    assert!(matches!(
        world.clients[a].engine.handle_proposals(conv, &future),
        Err(Error::FutureEpoch { .. })
    ));
    // Garbage.
    assert!(world.clients[a]
        .engine
        .handle_proposals(conv, &[0xff; 40])
        .is_err());
}

#[test]
fn a_pending_remove_blocks_sending_until_it_is_committed() {
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);
    let conv = world.conv();
    let leaf = world.leaf(a, c);
    let proposals = world.ds.proposals(&[], &[leaf], 1);
    world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    world.clients[b]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();

    let pending = world.clients[a].engine.pending(conv).unwrap();
    assert_eq!(pending.removes.len(), 1);
    let err = world.clients[a]
        .engine
        .encrypt(conv, Kind::Message, None, &envelope("too soon"));
    assert!(matches!(err, Err(Error::PendingRemove)), "{err:?}");

    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    // While the commit is in flight nothing is sent either.
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, Kind::Message, None, &envelope("in flight")),
        Err(Error::CommitInFlight)
    ));
    world.clients[a].engine.confirm_commit(conv).unwrap();
    world.clients[b]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();
    world.say(a, &[b], "now it is fine");
}

#[test]
fn the_device_being_removed_cannot_commit_and_is_evicted_by_the_commit() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    let leaf = world.leaf(a, b);
    let proposals = world.ds.proposals(&[], &[leaf], 1);
    world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    let outcome = world.clients[b]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert!(outcome.removes_self);
    assert!(matches!(
        world.clients[b].engine.commit_pending(conv),
        Err(Error::SelfRemoval)
    ));
    assert!(matches!(
        world.clients[b]
            .engine
            .encrypt(conv, Kind::Message, None, &envelope("x")),
        Err(Error::PendingRemove)
    ));

    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    world.clients[a].engine.confirm_commit(conv).unwrap();
    let outcome = world.clients[b]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();
    assert!(outcome.removed_self);
    assert!(matches!(
        world.clients[b]
            .engine
            .encrypt(conv, Kind::Message, None, &envelope("x")),
        Err(Error::NotAMember)
    ));
    assert!(!world.clients[b].engine.has_group(conv));
}

#[test]
fn two_commits_for_one_epoch_one_wins() {
    let mut world = World::new();
    let (a, b, c, d) = (
        world.person("a"),
        world.person("b"),
        world.person("c"),
        world.person("d"),
    );
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);
    let conv = world.conv();
    let package = world.clients[d].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], 1);
    for at in [a, b, c] {
        world.clients[at]
            .engine
            .handle_proposals(conv, &proposals)
            .unwrap();
    }
    let first = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    let second = world.clients[b]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    // The server takes a's: b is told 409 and falls in line.
    world.clients[a].engine.confirm_commit(conv).unwrap();
    world.clients[b].engine.abort_commit(conv).unwrap();
    world.clients[b]
        .engine
        .handle_commit(conv, first.epoch, &first.commit)
        .unwrap();
    world.clients[c]
        .engine
        .handle_commit(conv, first.epoch, &first.commit)
        .unwrap();
    let key = world.ds.public();
    world.clients[d]
        .engine
        .handle_welcome(conv, &first.welcomes[0].data, Some(&key))
        .unwrap();
    world.say(b, &[a, c, d], "b follows a");
    // b's losing commit is of an epoch everyone has passed: it is not applied (and is not a rollback
    // either, since b applied a different commit for that epoch... it is refused as history rewriting).
    let late = world.clients[c]
        .engine
        .handle_commit(conv, second.epoch, &second.commit);
    assert!(
        matches!(late, Err(Error::EpochBackwards { .. })),
        "{late:?}"
    );
}

#[test]
fn a_repeated_commit_and_our_own_commit_coming_back_are_harmless() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let bundle = world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    // The feed shows a its own commit again.
    let echoed = world.clients[a]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();
    assert!(echoed.duplicate);
    // And b is told twice.
    let again = world.clients[b]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();
    assert!(again.duplicate);
    world.say(a, &[b], "still fine");
}

#[test]
fn a_commit_that_arrives_when_the_confirmation_was_lost_confirms_it() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], 0);
    world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    // The 204 never reached a; the feed delivers the commit instead.
    let outcome = world.clients[a]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();
    assert_eq!(outcome.epoch, 1);
    assert_eq!(outcome.added.len(), 1);
    let key = world.ds.public();
    world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key))
        .unwrap();
    world.say(a, &[b], "recovered");
}

#[test]
fn a_changed_server_key_is_reported_and_blocks_until_accepted() {
    let mut world = World::new();
    let (a, _b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let pinned = world.clients[a].engine.server_key(conv).unwrap();
    assert_eq!(pinned, world.ds.public());

    // The same key is fine.
    world.clients[a]
        .engine
        .observe_server_key(conv, &pinned)
        .unwrap();
    // Another key is a change: reported, and the conversation stops.
    let other = DeviceKey::generate().unwrap().public_key();
    assert!(matches!(
        world.clients[a].engine.observe_server_key(conv, &other),
        Err(Error::ServerKeyChanged)
    ));
    assert!(matches!(
        world.clients[a]
            .engine
            .encrypt(conv, Kind::Message, None, &envelope("x")),
        Err(Error::ServerKeyChanged)
    ));
    world.clients[a]
        .engine
        .accept_server_key(conv, &other)
        .unwrap();
    world.clients[a]
        .engine
        .encrypt(conv, Kind::Message, None, &envelope("x"))
        .unwrap();

    // A Welcome whose group carries another key than the one shown is refused too. Bob learns the
    // key from a: but the server shows him a different one.
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], 0))
        .unwrap();
    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    let forged = DeviceKey::generate().unwrap().public_key();
    let err = world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&forged));
    assert!(matches!(err, Err(Error::ServerKeyChanged)), "{err:?}");
    // The refusal gave the key package back: the right pin works.
    let key = world.ds.public();
    let joined = world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key))
        .unwrap();
    assert!(!joined.pinned_now);
}

#[test]
fn an_epoch_that_goes_backwards_is_detected_and_freezes_the_group() {
    let mut world = World::new();
    let (a, b, c, d) = (
        world.person("a"),
        world.person("b"),
        world.person("c"),
        world.person("d"),
    );
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    // a's state at epoch 1, kept.
    let key = e2ee::store::StateKey::generate().unwrap();
    let saved = world.clients[a].engine.snapshot(&key, "state").unwrap();
    world.change(&[a, b], a, &[c], &[]);
    assert_eq!(world.epoch(b), 2);

    // The server is restored from a backup: it tells c (who joined at epoch 2) the group is at 1.
    assert!(matches!(
        world.clients[c].engine.observe_server_epoch(conv, 1),
        Err(Error::EpochBackwards { local: 2, shown: 1 })
    ));
    assert!(world.clients[c].engine.frozen(conv).is_some());
    assert!(matches!(
        world.clients[c]
            .engine
            .encrypt(conv, Kind::Message, None, &envelope("x")),
        Err(Error::Frozen(_))
    ));

    // And the rewritten history: from the old state, a commits something else for epoch 1; b, who
    // was in the group then and is at epoch 2, is shown that commit.
    let mut old_a = e2ee::Engine::from_snapshot(&key, "state", &saved).unwrap();
    let package = world.clients[d].engine.key_packages(1).unwrap().remove(0);
    old_a
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], 1))
        .unwrap();
    let rewritten = old_a.commit_pending(conv).unwrap().unwrap();
    assert_eq!(rewritten.epoch, 1);
    let err = world.clients[b]
        .engine
        .handle_commit(conv, rewritten.epoch, &rewritten.commit);
    assert!(
        matches!(err, Err(Error::EpochBackwards { local: 2, shown: 1 })),
        "{err:?}"
    );
    assert!(matches!(
        world.clients[b]
            .engine
            .encrypt(conv, Kind::Message, None, &envelope("x")),
        Err(Error::Frozen(_))
    ));
}

#[test]
fn a_commit_that_adds_a_device_this_client_does_not_trust_is_held_until_it_is_known() {
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();

    // A new person joins the server. a and the committer b know them; carol-side member a has not
    // fetched their list yet.
    let d = world.person("d");
    let list = world.directory.clone();
    for (user, devices) in &list {
        world.clients[b]
            .engine
            .verify_devices(*user, devices)
            .unwrap();
        world.clients[d]
            .engine
            .verify_devices(*user, devices)
            .unwrap();
    }
    let package = world.clients[d].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], 1);
    // b takes it (d is trusted by b); a (who has not heard of d) refuses to store it.
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    assert_eq!(outcome.rejected[0].reason, RejectReason::UnknownDevice);
    world.clients[b]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    let bundle = world.clients[b]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    world.clients[b].engine.confirm_commit(conv).unwrap();

    // a cannot apply b's commit: it names a device a does not trust (and a lacks the proposal).
    let err = world.clients[a]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit);
    assert!(err.is_err(), "a commit with an unknown device was applied");
    // Once a knows d, the proposals can be fed again and the commit follows.
    for (user, devices) in &list {
        world.clients[a]
            .engine
            .verify_devices(*user, devices)
            .unwrap();
    }
    world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    let key = world.ds.public();
    world.clients[d]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key))
        .unwrap();
    world.clients[a]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();
    world.say(a, &[b, d], "now d is known");
    let _ = c;
}

#[test]
fn a_welcome_with_unknown_members_is_refused_without_spending_the_key_package() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    // c is new: a and b know them, c knows only themself.
    let c = world.person("c");
    let carol = world.clients[c].user();
    let list = world.directory[&carol].clone();
    for at in [a, b, c] {
        world.clients[at]
            .engine
            .verify_devices(carol, &list)
            .unwrap();
    }
    let package = world.clients[c].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], 1);
    for at in [a, b] {
        world.clients[at]
            .engine
            .handle_proposals(conv, &proposals)
            .unwrap();
    }
    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    world.clients[a].engine.confirm_commit(conv).unwrap();
    world.clients[b]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit)
        .unwrap();

    let key = world.ds.public();
    let err = world.clients[c]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key));
    let Err(Error::UntrustedDevices(unknown)) = err else {
        panic!("expected UntrustedDevices, got {err:?}")
    };
    assert_eq!(unknown.len(), 2);
    // c fetches the lists and tries again with the same Welcome.
    let lists = world.directory.clone();
    for (user, devices) in &lists {
        world.clients[c]
            .engine
            .verify_devices(*user, devices)
            .unwrap();
    }
    world.clients[c]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key))
        .unwrap();
    world.say(a, &[b, c], "c is in");
}

#[test]
fn a_last_resort_key_package_serves_more_than_one_group() {
    let (ds1, ds2) = (Ds::new(), Ds::new());
    let mut a1 = e2ee::Engine::detached(UserId::random().unwrap()).unwrap();
    let mut a2 = e2ee::Engine::detached(UserId::random().unwrap()).unwrap();
    let mut x = e2ee::Engine::detached(UserId::random().unwrap()).unwrap();
    let records: Vec<(UserId, DeviceRecord)> = [&a1, &a2, &x]
        .iter()
        .map(|engine| {
            (
                engine.user_id(),
                DeviceRecord {
                    device_id: engine.device_id(),
                    signature_key: engine.signature_key(),
                    cross_signature: None,
                    created_at: 1,
                },
            )
        })
        .collect();
    for engine in [&mut a1, &mut a2, &mut x] {
        for (user, record) in &records {
            engine
                .verify_devices(*user, std::slice::from_ref(record))
                .unwrap();
        }
    }
    a1.create_group(ds1.conv, &ds1.public()).unwrap();
    a2.create_group(ds2.conv, &ds2.public()).unwrap();
    let package = x.last_resort_key_package().unwrap();
    for (creator, ds) in [(&mut a1, &ds1), (&mut a2, &ds2)] {
        creator
            .handle_proposals(ds.conv, &ds.proposals(&[&package], &[], 0))
            .unwrap();
        let bundle = creator.commit_pending(ds.conv).unwrap().unwrap();
        creator.confirm_commit(ds.conv).unwrap();
        x.handle_welcome(ds.conv, &bundle.welcomes[0].data, Some(&ds.public()))
            .unwrap();
        let sealed = creator
            .encrypt(ds.conv, Kind::Message, None, &envelope("hi"))
            .unwrap();
        let opened = x
            .decrypt(
                ds.conv,
                &sealed.ciphertext,
                &Claimed {
                    author: Some(creator.user_id()),
                    device: None,
                    kind: Kind::Message,
                    target_id: None,
                },
            )
            .unwrap();
        assert_eq!(opened.envelope.text, "hi");
    }
}

#[test]
fn key_packages_have_the_ciphersuite_credential_and_key_the_server_checks() {
    use openmls::prelude::tls_codec::DeserializeBytes as _;
    use openmls::prelude::{CredentialType, KeyPackageIn, ProtocolVersion};
    use openmls_traits::OpenMlsProvider as _;
    let mut engine = e2ee::Engine::detached(UserId::random().unwrap()).unwrap();
    let crypto = openmls_rust_crypto::OpenMlsRustCrypto::default();
    let batch = engine.key_packages(5).unwrap();
    let last = engine.last_resort_key_package().unwrap();
    assert!(engine.key_packages(0).is_err());
    assert!(engine.key_packages(51).is_err());
    for (at, bytes) in batch.iter().chain(std::iter::once(&last)).enumerate() {
        let package = KeyPackageIn::tls_deserialize_exact_bytes(bytes)
            .unwrap()
            .validate(crypto.crypto(), ProtocolVersion::Mls10)
            .unwrap();
        assert_eq!(package.ciphersuite(), e2ee::CIPHERSUITE);
        assert_eq!(u16::from(package.ciphersuite()), 0x0002);
        let leaf = package.leaf_node();
        assert_eq!(leaf.credential().credential_type(), CredentialType::Basic);
        let content = leaf.credential().serialized_content();
        assert_eq!(content.len(), 32);
        assert_eq!(&content[..16], engine.user_id().as_bytes());
        assert_eq!(&content[16..], engine.device_id().as_bytes());
        assert_eq!(
            leaf.signature_key().as_slice(),
            engine.signature_key().as_slice()
        );
        assert_eq!(package.last_resort(), at == batch.len());
        assert!(package.life_time().is_valid());
    }
    // All different.
    let mut sorted = batch.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 5);
}

#[test]
fn commits_a_malicious_member_could_make_are_refused() {
    use e2ee::groups::Forge;
    use e2ee::identity::DeviceIdentity;
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);
    let conv = world.conv();
    let epoch = world.epoch(a);
    let carol = world.clients[c].user();
    let carol_device = world.clients[c].device();

    // A new identity under the committer's own credential name: same user, same device id, another key.
    let same_name_new_key = DeviceIdentity {
        user: carol,
        device: carol_device,
        key: DeviceKey::generate().unwrap(),
    };
    // A new device id for the same person: another credential.
    let new_device = DeviceIdentity::generate(carol).unwrap();
    let other_server = DeviceKey::generate().unwrap().public_key();
    let mut ghost = e2ee::Engine::with_identity(DeviceIdentity::generate(carol).unwrap());
    let ghost_package = ghost.key_packages(1).unwrap().remove(0);

    let forged = [
        (
            "new key under the old name",
            world.clients[c]
                .engine
                .forge_commit(conv, Forge::ReplaceIdentity(&same_name_new_key))
                .unwrap(),
        ),
        (
            "new credential",
            world.clients[c]
                .engine
                .forge_commit(conv, Forge::ReplaceIdentity(&new_device))
                .unwrap(),
        ),
        (
            "a new external sender",
            world.clients[c]
                .engine
                .forge_commit(conv, Forge::ChangeServerKey(&other_server))
                .unwrap(),
        ),
        (
            "an add of a device by value",
            world.clients[c]
                .engine
                .forge_commit(conv, Forge::AddKeyPackage(&ghost_package))
                .unwrap(),
        ),
    ];
    for (what, commit) in &forged {
        for at in [a, b] {
            let err = world.clients[at].engine.handle_commit(conv, epoch, commit);
            assert!(
                matches!(
                    err,
                    Err(Error::ForbiddenCommit(_)) | Err(Error::ServerKeyChanged)
                ),
                "{what}: {err:?}"
            );
            assert_eq!(world.epoch(at), epoch, "{what}: the epoch moved");
        }
    }
    // The group is untouched and still works, and the forging member's own state too.
    world.say(a, &[b, c], "still here");
    world.say(c, &[a, b], "still here too");
}

#[test]
fn a_forged_cross_signature_does_not_get_a_device_added() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();
    let epoch = world.epoch(a);
    let bob = world.clients[b].user();

    // The server makes a device for Bob and "cross-signs" it with a key of its own.
    let mut ghost =
        e2ee::Engine::with_identity(e2ee::identity::DeviceIdentity::generate(bob).unwrap());
    let package = ghost.key_packages(1).unwrap().remove(0);
    let server_made = e2ee::identity::DeviceIdentity::generate(bob).unwrap();
    let forged_signature = server_made
        .cross_sign(ghost.device_id(), &ghost.signature_key())
        .unwrap();
    let mut list = world.directory[&bob].clone();
    list.push(DeviceRecord {
        device_id: ghost.device_id(),
        signature_key: ghost.signature_key(),
        cross_signature: Some(forged_signature),
        created_at: 50,
    });
    let report = world.clients[a].engine.verify_devices(bob, &list).unwrap();
    assert_eq!(report.rejected.len(), 1);
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], epoch))
        .unwrap();
    assert!(
        outcome.accepted.is_empty(),
        "a device with a forged cross-signature was accepted"
    );
    assert!(world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .is_none());
    // And a ghost Welcome cannot be joined into a group of this client's friends either: the ghost
    // would have to trust everyone, which it has no basis to do (it has no trusted lists).
    let key = world.ds.public();
    assert!(ghost.handle_welcome(conv, &[0u8; 8], Some(&key)).is_err());
}

/// The proposal references a commit applies, read from the bytes of the MLSMessage as the server
/// reads them (its `e2ee_text::mls::commit_references`): version, wire format, group id, epoch,
/// sender, authenticated data, content type, then the list of proposals, each `2` (a reference)
/// and a 32 byte hash. `None` if any is by value.
fn references_of(data: &[u8]) -> Option<Vec<[u8; 32]>> {
    fn vector(bytes: &[u8], at: &mut usize) -> Option<Vec<u8>> {
        let first = *bytes.get(*at)?;
        let (len, header) = match first >> 6 {
            0 => (usize::from(first & 0x3f), 1),
            1 => (
                usize::from(first & 0x3f) << 8 | usize::from(*bytes.get(*at + 1)?),
                2,
            ),
            2 => {
                let rest = bytes.get(*at + 1..*at + 4)?;
                (
                    usize::from(first & 0x3f) << 24
                        | usize::from(rest[0]) << 16
                        | usize::from(rest[1]) << 8
                        | usize::from(rest[2]),
                    4,
                )
            }
            _ => return None,
        };
        let out = bytes.get(*at + header..*at + header + len)?.to_vec();
        *at += header + len;
        Some(out)
    }
    let mut at = 0;
    let mut take = |n: usize| -> Option<Vec<u8>> {
        let out = data.get(at..at + n)?.to_vec();
        at += n;
        Some(out)
    };
    if take(2)? != [0, 1] || take(2)? != [0, 1] {
        return None;
    }
    let mut cursor = at;
    vector(data, &mut cursor)?; // group id
    cursor += 8; // epoch
    if *data.get(cursor)? != 1 {
        return None; // not a member sender
    }
    cursor += 5; // sender type and leaf
    vector(data, &mut cursor)?; // authenticated data
    if *data.get(cursor)? != 3 {
        return None; // not a commit
    }
    cursor += 1;
    let list = vector(data, &mut cursor)?;
    let mut inner = 0;
    let mut out = Vec::new();
    while inner < list.len() {
        if list[inner] != 2 {
            return None;
        }
        inner += 1;
        let hash = vector(&list, &mut inner)?;
        out.push(<[u8; 32]>::try_from(hash.as_slice()).ok()?);
    }
    Some(out)
}

#[test]
fn the_commit_and_the_welcome_have_the_shape_the_server_checks() {
    use openmls::prelude::tls_codec::DeserializeBytes as _;
    use openmls::prelude::{ContentType, MlsMessageBodyIn, MlsMessageIn, ProtocolMessage, Sender};
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let packages: Vec<Vec<u8>> = [b, c]
        .iter()
        .map(|at| world.clients[*at].engine.key_packages(1).unwrap().remove(0))
        .collect();
    let refs: Vec<&[u8]> = packages.iter().map(Vec::as_slice).collect();
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&refs, &[], 0))
        .unwrap();
    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();

    // The commit: an MLSMessage holding a PublicMessage commit for this group and epoch, from a
    // member's leaf, that applies the two proposals by reference and none by value.
    let message = MlsMessageIn::tls_deserialize_exact_bytes(&bundle.commit).unwrap();
    let ProtocolMessage::PublicMessage(public) = message.try_into_protocol_message().unwrap()
    else {
        panic!("not a public message")
    };
    assert_eq!(public.content_type(), ContentType::Commit);
    assert_eq!(public.group_id().as_slice(), conv.0);
    assert_eq!(public.epoch().as_u64(), 0);
    assert!(matches!(public.sender(), Sender::Member(_)));
    assert_eq!(
        references_of(&bundle.commit)
            .expect("proposals by reference")
            .len(),
        2
    );

    // The Welcome: an MLSMessage holding a Welcome, one entry per added device, naming each
    // device's key package (what the server matches the entries against).
    assert_eq!(bundle.welcomes.len(), 2);
    let MlsMessageBodyIn::Welcome(welcome) =
        MlsMessageIn::tls_deserialize_exact_bytes(&bundle.welcomes[0].data)
            .unwrap()
            .extract()
    else {
        panic!("not a Welcome")
    };
    assert_eq!(welcome.secrets().len(), 2);
    let added: Vec<_> = bundle.welcomes.iter().map(|w| w.device.device).collect();
    assert!(
        added.contains(&world.clients[b].device()) && added.contains(&world.clients[c].device())
    );
    // Raw `Welcome` bytes are read too.
    use openmls::prelude::tls_codec::Serialize as _;
    let raw = welcome.tls_serialize_detached().unwrap();
    world.clients[a].engine.confirm_commit(conv).unwrap();
    let key = world.ds.public();
    world.clients[b]
        .engine
        .handle_welcome(conv, &raw, Some(&key))
        .unwrap();
    world.clients[c]
        .engine
        .handle_welcome(conv, &bundle.welcomes[1].data, Some(&key))
        .unwrap();
    world.say(b, &[a, c], "both forms join");
    // Within every size the server accepts.
    assert!(bundle.commit.len() <= e2ee::groups::limits::COMMIT);
}

#[test]
fn a_commit_the_server_would_refuse_for_size_is_refused_here_and_not_left_in_flight() {
    let mut world = World::new();
    let a = world.person("a");
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    // 101 devices at once: one more than a commit may carry Welcomes for.
    let joiners: Vec<usize> = (0..101).map(|n| world.person(&format!("j{n}"))).collect();
    world.sync_trust();
    let packages: Vec<Vec<u8>> = joiners
        .iter()
        .map(|at| world.clients[*at].engine.key_packages(1).unwrap().remove(0))
        .collect();
    let refs: Vec<&[u8]> = packages.iter().map(Vec::as_slice).collect();
    let outcome = world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&refs, &[], 0))
        .unwrap();
    assert_eq!(outcome.accepted.len(), 101);
    let err = world.clients[a].engine.commit_pending(conv);
    assert!(
        matches!(err, Err(Error::TooLarge("welcomes in one commit"))),
        "{err:?}"
    );
    // Nothing is in flight: the group is where it was and can be asked again.
    assert!(matches!(
        world.clients[a].engine.commit_pending(conv),
        Err(Error::TooLarge(_))
    ));
    assert!(matches!(
        world.clients[a].engine.confirm_commit(conv),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn the_external_sender_may_be_given_as_a_tls_external_sender_or_as_a_bare_key() {
    use openmls::prelude::tls_codec::Serialize as _;
    use openmls::prelude::{BasicCredential, ExternalSender};
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    let conv = world.conv();
    // As the server hands it out in `GET /v1/e2ee/conversations/{id}`: the TLS ExternalSender.
    let tls = ExternalSender::new(
        world.ds.public().into(),
        BasicCredential::new(b"tinyserver".to_vec()).into(),
    )
    .tls_serialize_detached()
    .unwrap();
    world.clients[a].engine.create_group(conv, &tls).unwrap();
    assert_eq!(
        world.clients[a].engine.server_key(conv).unwrap(),
        world.ds.public()
    );
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
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
    // The joiner is shown the bare key and the group's context holds the TLS one: the same key.
    let bare = world.ds.public();
    world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&bare))
        .unwrap();
    // Observing it in either form agrees; another key in either form does not.
    world.clients[b]
        .engine
        .observe_server_key(conv, &tls)
        .unwrap();
    world.clients[b]
        .engine
        .observe_server_key(conv, &bare)
        .unwrap();
    let other = DeviceKey::generate().unwrap().public_key();
    let other_tls = ExternalSender::new(
        other.clone().into(),
        BasicCredential::new(b"tinyserver".to_vec()).into(),
    )
    .tls_serialize_detached()
    .unwrap();
    assert!(matches!(
        world.clients[b].engine.observe_server_key(conv, &other_tls),
        Err(Error::ServerKeyChanged)
    ));
    // Junk is malformed, not a key.
    assert!(world.clients[a]
        .engine
        .observe_server_key(conv, &[1, 2, 3])
        .is_err());
    assert!(world.clients[a]
        .engine
        .observe_server_key(conv, &[0u8; 65])
        .is_err());
    world.say(a, &[], "the group is fine");
}

#[test]
fn a_welcome_delivered_again_is_not_taken_and_does_not_disturb_the_group() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    // b joins with a last-resort key package, which a replayed Welcome can still open.
    let key = world.ds.public();
    let package = world.clients[b].engine.last_resort_key_package().unwrap();
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
    world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key))
        .unwrap();
    world.say(a, &[b], "first");
    // The Welcome shows up again in the feed: refused, not an error that freezes anything.
    let again = world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key));
    assert!(matches!(again, Err(Error::Duplicate)), "{again:?}");
    assert!(world.clients[b].engine.frozen(conv).is_none());
    world.say(a, &[b], "still fine");
    // The same for a Welcome of a regular key package, which is gone by now.
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let key = world.ds.public();
    let bundle = world.change(&[a], a, &[b], &[]);
    let again = world.clients[b]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key));
    assert!(matches!(again, Err(Error::Duplicate)), "{again:?}");
    world.say(b, &[a], "unharmed");
}

#[test]
fn a_commit_in_flight_survives_a_restart_and_is_sent_again_whole() {
    let mut world = World::new();
    let (a, b) = (world.person("a"), world.person("b"));
    world.sync_trust();
    world.start(a);
    let conv = world.conv();
    let package = world.clients[b].engine.key_packages(1).unwrap().remove(0);
    world.clients[a]
        .engine
        .handle_proposals(conv, &world.ds.proposals(&[&package], &[], 0))
        .unwrap();
    let made = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    assert_eq!(
        world.clients[a].engine.pending_commit(conv),
        Some(made.clone())
    );
    // The process is killed before the answer. From the stored state it is all still there.
    let key = e2ee::store::StateKey::generate().unwrap();
    let blob = world.clients[a].engine.snapshot(&key, "s").unwrap();
    let mut restarted = e2ee::Engine::from_snapshot(&key, "s", &blob).unwrap();
    assert_eq!(restarted.pending_commit(conv), Some(made.clone()));
    // The server had taken it: the commit comes back through the feed and settles it.
    let outcome = restarted
        .handle_commit(conv, made.epoch, &made.commit)
        .unwrap();
    assert_eq!(outcome.epoch, 1);
    assert!(restarted.pending_commit(conv).is_none());
    let sealed = restarted
        .encrypt(conv, Kind::Message, None, &envelope("after restart"))
        .unwrap();
    let key = world.ds.public();
    world.clients[b]
        .engine
        .handle_welcome(conv, &made.welcomes[0].data, Some(&key))
        .unwrap();
    let claimed = Claimed {
        author: Some(restarted.user_id()),
        device: None,
        kind: Kind::Message,
        target_id: None,
    };
    assert_eq!(
        world.clients[b]
            .engine
            .decrypt(conv, &sealed.ciphertext, &claimed)
            .unwrap()
            .envelope
            .text,
        "after restart"
    );
}

#[test]
fn a_commit_older_than_the_ones_remembered_is_taken_for_shown_again_not_for_a_rewrite() {
    let mut world = World::new();
    let all: Vec<usize> = (0..8).map(|n| world.person(&format!("d{n}"))).collect();
    world.sync_trust();
    world.start(all[0]);
    let conv = world.conv();
    // Remember only 3 commits.
    for at in &all {
        let config = e2ee::Config {
            commits_remembered: 3,
            ..e2ee::Config::default()
        };
        world.clients[*at].engine.set_config(config);
    }
    let first = world.change(&[all[0]], all[0], &[all[1]], &[]);
    let mut members = vec![all[0], all[1]];
    for next in &all[2..] {
        world.change(&members.clone(), all[0], &[*next], &[]);
        members.push(*next);
    }
    assert_eq!(world.epoch(all[1]), 7);
    // The oldest commit comes up again: all[1] joined with it (it is history for them), and all[0]
    // made it and has forgotten it. Neither is told a rewrite, neither is frozen.
    for at in [all[0], all[1]] {
        let outcome = world.clients[at]
            .engine
            .handle_commit(conv, first.epoch, &first.commit)
            .unwrap();
        assert!(outcome.duplicate);
        assert!(world.clients[at].engine.frozen(conv).is_none());
    }
    world.say(all[0], &[all[1], all[7]], "fine");
}

#[test]
fn a_bad_commit_at_our_epoch_does_not_throw_away_our_commit_in_flight() {
    use e2ee::groups::Forge;
    let mut world = World::new();
    let (a, b, c, d) = (
        world.person("a"),
        world.person("b"),
        world.person("c"),
        world.person("d"),
    );
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);
    let conv = world.conv();
    let epoch = world.epoch(a);
    // c, a member gone bad, makes a commit for this epoch that swaps its own identity (made
    // before c holds the server's proposals, so the commit carries nothing else).
    let impostor = e2ee::identity::DeviceIdentity::generate(world.clients[c].user()).unwrap();
    let forged = world.clients[c]
        .engine
        .forge_commit(conv, Forge::ReplaceIdentity(&impostor))
        .unwrap();
    let package = world.clients[d].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], epoch);
    for at in [a, b, c] {
        world.clients[at]
            .engine
            .handle_proposals(conv, &proposals)
            .unwrap();
    }
    let ours = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    // b commits the same proposals at the same time, and will lose.
    let theirs = world.clients[b]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    assert_ne!(theirs.commit, ours.commit);

    // The server shows a, at the epoch a committed at: a commit by a member that breaks the rules,
    // and bytes that are no commit at all. Both are refused, and a's commit is still in flight.
    let mut flipped = ours.commit.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    for (what, junk) in [
        ("a forbidden commit", forged.as_slice()),
        ("our commit with a flipped bit", flipped.as_slice()),
        ("garbage", &[0u8, 1, 2, 3][..]),
    ] {
        assert!(
            world.clients[a]
                .engine
                .handle_commit(conv, epoch, junk)
                .is_err(),
            "{what} was taken"
        );
        let kept = world.clients[a].engine.pending_commit(conv);
        assert_eq!(
            kept.map(|bundle| bundle.commit),
            Some(ours.commit.clone()),
            "{what} threw away our commit in flight"
        );
        assert_eq!(world.epoch(a), epoch, "{what}: the epoch moved");
    }
    // The real answer still settles it: the server took ours, and the feed shows it.
    let outcome = world.clients[a]
        .engine
        .handle_commit(conv, ours.epoch, &ours.commit)
        .unwrap();
    assert_eq!(outcome.epoch, epoch + 1);
    for at in [b, c] {
        world.clients[at]
            .engine
            .handle_commit(conv, ours.epoch, &ours.commit)
            .unwrap();
    }
    // b's own commit lost to a valid one, which is what drops it.
    assert!(world.clients[b].engine.pending_commit(conv).is_none());
    assert_eq!(world.epoch(b), epoch + 1);
    let key = world.ds.public();
    world.clients[d]
        .engine
        .handle_welcome(conv, &ours.welcomes[0].data, Some(&key))
        .unwrap();
    world.say(a, &[b, c, d], "our commit survived the junk");
}

#[test]
fn bytes_shown_for_an_old_epoch_that_are_no_commit_do_not_freeze_the_group() {
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    world.change(&[a, b], a, &[c], &[]);
    let conv = world.conv();
    assert_eq!(world.epoch(b), 2);
    for junk in [&[][..], &[0u8; 40][..], b"not a commit"] {
        let err = world.clients[b].engine.handle_commit(conv, 1, junk);
        assert!(
            matches!(
                err,
                Err(Error::Malformed(_)) | Err(Error::ForbiddenCommit(_))
            ),
            "{err:?}"
        );
        assert!(
            world.clients[b].engine.frozen(conv).is_none(),
            "junk froze the group"
        );
    }
    // A message for this group under the wrong epoch label is refused too, without freezing.
    let message = world.send(a, "a ciphertext, not a commit");
    let err = world.clients[b].engine.handle_commit(conv, 1, &message);
    assert!(err.is_err(), "{err:?}");
    assert!(world.clients[b].engine.frozen(conv).is_none());
    world.say(a, &[b, c], "nothing froze");
}

#[test]
fn a_forged_signature_does_not_spend_the_real_message_key() {
    // Every member holds the whole secret tree, so a member gone bad can make a ciphertext that
    // opens under another member's leaf and next generation; only the signature is wrong. OpenMLS
    // stores the advanced key before it checks the signature: the refusal must put it back, or
    // the real message for that generation can never be opened.
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b, c], &[]);
    world.say(a, &[b, c], "a first message, so the generation is not zero");
    let conv = world.conv();
    let wrong_key = DeviceKey::generate().unwrap();
    let forged = world.clients[a]
        .engine
        .forge_message(conv, &wrong_key, &envelope("forged"))
        .unwrap();
    for at in [b, c] {
        for _ in 0..2 {
            assert!(
                matches!(world.open(at, &forged, a), Err(Error::Undecryptable)),
                "a forged signature was accepted"
            );
        }
    }
    // The real message uses the generation the forgery claimed.
    let genuine = world.send(a, "genuine");
    for at in [b, c] {
        let opened = world
            .open(at, &genuine, a)
            .expect("the real message opens after a forgery under its key");
        assert_eq!(opened.envelope.text, "genuine");
        assert!(matches!(
            world.open(at, &genuine, a),
            Err(Error::Undecryptable)
        ));
    }
    // And the forgery is refused even now that its generation has been used, and in a restarted
    // engine: the refused message left nothing behind in the state.
    let key = e2ee::store::StateKey::generate().unwrap();
    let saved = world.clients[b].engine.snapshot(&key, "state").unwrap();
    let mut restarted = e2ee::Engine::from_snapshot(&key, "state", &saved).unwrap();
    let claimed = Claimed {
        author: Some(world.clients[a].user()),
        device: Some(world.clients[a].device()),
        kind: Kind::Message,
        target_id: None,
    };
    assert!(restarted.decrypt(conv, &forged, &claimed).is_err());
    world.say(a, &[b, c], "and on it goes");
}
