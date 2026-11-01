use e2ee::identity::{
    cross_signature_message, verify_cross_signature, DeviceIdentity, DeviceKey, DeviceRecord,
    Rejection, Trust, CROSS_SIGN_LABEL,
};
use e2ee::safety::safety_number;
use e2ee::{DeviceId, DeviceRef, Error, UserId};

fn user() -> UserId {
    UserId::random().unwrap()
}

fn device(user: UserId) -> DeviceIdentity {
    DeviceIdentity::generate(user).unwrap()
}

fn record(identity: &DeviceIdentity, signer: Option<&DeviceIdentity>, at: i64) -> DeviceRecord {
    let cross_signature = signer.map(|signer| {
        signer
            .cross_sign(identity.device, &identity.key.public_key())
            .unwrap()
    });
    DeviceRecord {
        device_id: identity.device,
        signature_key: identity.key.public_key(),
        cross_signature,
        created_at: at,
    }
}

#[test]
fn a_cross_signature_verifies_and_signs_exactly_the_documented_bytes() {
    let u = user();
    let (old, new) = (device(u), device(u));
    let signature = old.cross_sign(new.device, &new.key.public_key()).unwrap();
    verify_cross_signature(
        &old.key.public_key(),
        u,
        new.device,
        &new.key.public_key(),
        &signature,
    )
    .unwrap();

    // The message is the label, the user id, the device id and the key, in that order.
    let message = cross_signature_message(u, new.device, &new.key.public_key());
    assert_eq!(
        &message[..CROSS_SIGN_LABEL.len()],
        b"tiny-e2ee-v1 device-add"
    );
    assert_eq!(CROSS_SIGN_LABEL, b"tiny-e2ee-v1 device-add");
    assert_eq!(&message[23..39], u.as_bytes());
    assert_eq!(&message[39..55], new.device.as_bytes());
    assert_eq!(&message[55..], new.key.public_key().as_slice());
    assert_eq!(message.len(), 23 + 16 + 16 + 65);

    // The same signature checked as ECDSA P-256/SHA-256 by a standard verifier, over those bytes.
    use p256::ecdsa::signature::Verifier;
    let verifying = p256::ecdsa::VerifyingKey::from_sec1_bytes(&old.key.public_key()).unwrap();
    let parsed = p256::ecdsa::Signature::from_der(&signature).unwrap();
    verifying.verify(&message, &parsed).unwrap();

    // The 64 byte r||s form of the same signature is accepted too.
    verify_cross_signature(
        &old.key.public_key(),
        u,
        new.device,
        &new.key.public_key(),
        &parsed.to_bytes(),
    )
    .unwrap();
}

#[test]
fn a_forged_or_misapplied_cross_signature_fails() {
    let u = user();
    let (old, new, attacker, other_user) = (device(u), device(u), device(u), device(user()));
    let good = old.cross_sign(new.device, &new.key.public_key()).unwrap();
    let verify =
        |signer: &DeviceIdentity, user: UserId, device: DeviceId, key: &[u8], signature: &[u8]| {
            verify_cross_signature(&signer.key.public_key(), user, device, key, signature)
        };

    // By someone else's key.
    let forged = attacker
        .cross_sign(new.device, &new.key.public_key())
        .unwrap();
    assert!(matches!(
        verify(&old, u, new.device, &new.key.public_key(), &forged),
        Err(Error::BadSignature)
    ));
    // For another device id, another user, another key.
    assert!(matches!(
        verify(
            &old,
            u,
            DeviceId::random().unwrap(),
            &new.key.public_key(),
            &good
        ),
        Err(Error::BadSignature)
    ));
    assert!(matches!(
        verify(
            &old,
            other_user.user,
            new.device,
            &new.key.public_key(),
            &good
        ),
        Err(Error::BadSignature)
    ));
    assert!(matches!(
        verify(&old, u, new.device, &attacker.key.public_key(), &good),
        Err(Error::BadSignature)
    ));
    // A signature that was made over the right fields but without the label (a signature from another protocol).
    let mut bare = Vec::new();
    bare.extend_from_slice(u.as_bytes());
    bare.extend_from_slice(new.device.as_bytes());
    bare.extend_from_slice(&new.key.public_key());
    let unlabeled = old.key.sign(&bare);
    assert!(verify(&old, u, new.device, &new.key.public_key(), &unlabeled).is_err());
    // Mangled signatures and keys.
    let mut flipped = good.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert!(verify(&old, u, new.device, &new.key.public_key(), &flipped).is_err());
    assert!(verify(
        &old,
        u,
        new.device,
        &new.key.public_key(),
        &good[..good.len() - 1]
    )
    .is_err());
    assert!(verify(&old, u, new.device, &new.key.public_key(), &[]).is_err());
    assert!(verify(&old, u, new.device, &new.key.public_key(), &[0u8; 64]).is_err());
    assert!(
        verify_cross_signature(&[4u8; 65], u, new.device, &new.key.public_key(), &good).is_err()
    );
    assert!(verify_cross_signature(
        &old.key.public_key()[..64],
        u,
        new.device,
        &new.key.public_key(),
        &good
    )
    .is_err());
    assert!(
        verify_cross_signature(&old.key.public_key(), u, new.device, &[1u8; 33], &good).is_err()
    );
    // Signing for a malformed key is refused up front.
    assert!(old.cross_sign(new.device, &[0u8; 65]).is_err());
}

#[test]
fn the_first_device_is_trusted_on_first_use_and_the_rest_must_chain_to_it() {
    let u = user();
    let (first, second, third) = (device(u), device(u), device(u));
    let mut trust = Trust::new();
    assert!(!trust.knows(u));

    let report = trust.verify_list(u, &[record(&first, None, 1)]).unwrap();
    assert!(report.first_contact);
    assert_eq!(report.trusted.len(), 1);
    assert!(trust.is_trusted(u, first.device, &first.key.public_key()));

    // A second device signed by the first.
    let report = trust
        .verify_list(
            u,
            &[record(&first, None, 1), record(&second, Some(&first), 2)],
        )
        .unwrap();
    assert!(!report.first_contact);
    assert_eq!(report.added, vec![second.device]);
    assert!(report.rejected.is_empty());
    // A third signed by the second (a chain, not a star).
    let report = trust
        .verify_list(
            u,
            &[
                record(&first, None, 1),
                record(&second, Some(&first), 2),
                record(&third, Some(&second), 3),
            ],
        )
        .unwrap();
    assert_eq!(report.added, vec![third.device]);
    assert_eq!(trust.devices(u).len(), 3);
}

#[test]
fn a_chain_that_arrives_in_one_list_is_followed_in_any_order() {
    let u = user();
    let (a, b, c, d) = (device(u), device(u), device(u), device(u));
    let mut trust = Trust::new();
    // Listed newest first, as a server might.
    let list = [
        record(&d, Some(&c), 4),
        record(&c, Some(&b), 3),
        record(&b, Some(&a), 2),
        record(&a, None, 1),
    ];
    let report = trust.verify_list(u, &list).unwrap();
    assert_eq!(report.trusted.len(), 4);
    assert!(report.rejected.is_empty());
}

#[test]
fn a_device_the_server_invented_is_rejected() {
    let u = user();
    let (first, ghost) = (device(u), device(u));
    let mut trust = Trust::new();
    trust.verify_list(u, &[record(&first, None, 1)]).unwrap();
    // No cross-signature at all.
    let report = trust
        .verify_list(u, &[record(&first, None, 1), record(&ghost, None, 2)])
        .unwrap();
    assert_eq!(
        report.rejected,
        vec![(ghost.device, Rejection::NotCrossSigned)]
    );
    assert!(!trust.is_trusted(u, ghost.device, &ghost.key.public_key()));
    // A cross-signature by the ghost itself, or by a key that is no one's.
    let by_self = DeviceRecord {
        cross_signature: Some(
            ghost
                .cross_sign(ghost.device, &ghost.key.public_key())
                .unwrap(),
        ),
        ..record(&ghost, None, 2)
    };
    let stranger = device(u);
    let by_stranger = record(&ghost, Some(&stranger), 2);
    for forged in [by_self, by_stranger] {
        let report = trust
            .verify_list(u, &[record(&first, None, 1), forged])
            .unwrap();
        assert_eq!(
            report.rejected,
            vec![(ghost.device, Rejection::NotCrossSigned)]
        );
    }
    // A cross-signature by a trusted device of another person does not count.
    let other = device(user());
    let report = trust
        .verify_list(
            u,
            &[record(&first, None, 1), record(&ghost, Some(&other), 2)],
        )
        .unwrap();
    assert_eq!(report.rejected.len(), 1);
    // A signature copied from another device's record is bound to that device.
    let real = device(u);
    let signed = record(&real, Some(&first), 3);
    let replayed = DeviceRecord {
        cross_signature: signed.cross_signature.clone(),
        ..record(&ghost, None, 4)
    };
    let report = trust
        .verify_list(u, &[record(&first, None, 1), signed, replayed])
        .unwrap();
    assert_eq!(
        report.rejected,
        vec![(ghost.device, Rejection::NotCrossSigned)]
    );
    assert!(trust.is_trusted(u, real.device, &real.key.public_key()));
}

#[test]
fn a_known_device_id_with_another_key_is_refused() {
    let u = user();
    let (first, impostor_key) = (device(u), DeviceKey::generate().unwrap());
    let mut trust = Trust::new();
    trust.verify_list(u, &[record(&first, None, 1)]).unwrap();
    let swapped = DeviceRecord {
        signature_key: impostor_key.public_key(),
        cross_signature: None,
        ..record(&first, None, 1)
    };
    // The only device is "replaced": nothing chains, so the person looks new.
    assert!(matches!(
        trust.verify_list(u, std::slice::from_ref(&swapped)),
        Err(Error::IdentityChanged)
    ));
    // Alongside a real second device, the swapped entry is rejected and the rest stands.
    let second = device(u);
    trust
        .verify_list(
            u,
            &[record(&first, None, 1), record(&second, Some(&first), 2)],
        )
        .unwrap();
    let report = trust
        .verify_list(u, &[swapped, record(&second, Some(&first), 2)])
        .unwrap();
    assert_eq!(report.rejected, vec![(first.device, Rejection::KeyChanged)]);
    assert!(!trust.is_trusted(u, first.device, &impostor_key.public_key()));
    assert_eq!(report.removed, vec![first.device]);
}

#[test]
fn revoking_a_device_removes_trust_and_its_signatures_stop_counting() {
    let u = user();
    let (a, b, c) = (device(u), device(u), device(u));
    let mut trust = Trust::new();
    trust
        .verify_list(u, &[record(&a, None, 1), record(&b, Some(&a), 2)])
        .unwrap();
    // a is revoked.
    let report = trust.verify_list(u, &[record(&b, Some(&a), 2)]).unwrap();
    assert_eq!(report.removed, vec![a.device]);
    assert!(!trust.is_trusted(u, a.device, &a.key.public_key()));
    // Then a (a stolen key, say) cross-signs a new device: refused, a is no longer trusted.
    let report = trust
        .verify_list(u, &[record(&b, Some(&a), 2), record(&c, Some(&a), 3)])
        .unwrap();
    assert_eq!(report.rejected, vec![(c.device, Rejection::NotCrossSigned)]);
    // b can still approve devices.
    let report = trust
        .verify_list(u, &[record(&b, Some(&a), 2), record(&c, Some(&b), 3)])
        .unwrap();
    assert_eq!(report.added, vec![c.device]);
}

#[test]
fn a_device_revoked_on_the_same_list_cannot_sign_in_a_new_one() {
    let u = user();
    let (a, b, thief) = (device(u), device(u), device(u));
    let mut trust = Trust::new();
    trust
        .verify_list(u, &[record(&a, None, 1), record(&b, Some(&a), 2)])
        .unwrap();
    // The list that drops a (its key stolen) also carries a device that only a vouches for: the
    // thief's. a is off the list, so its signature counts for nothing.
    let report = trust
        .verify_list(u, &[record(&b, Some(&a), 2), record(&thief, Some(&a), 3)])
        .unwrap();
    assert_eq!(report.removed, vec![a.device]);
    assert_eq!(
        report.rejected,
        vec![(thief.device, Rejection::NotCrossSigned)]
    );
    assert!(report.added.is_empty());
    assert!(!trust.is_trusted(u, thief.device, &thief.key.public_key()));
    assert!(!trust.is_trusted(u, a.device, &a.key.public_key()));
    assert!(trust.is_trusted(u, b.device, &b.key.public_key()));

    // Every device revoked at once, with a new one signed only by a revoked device: no chain to
    // what was pinned, so it is a new identity, not a silent hand-over.
    let mut trust = Trust::new();
    trust.verify_list(u, &[record(&a, None, 1)]).unwrap();
    assert!(matches!(
        trust.verify_list(u, &[record(&thief, Some(&a), 3)]),
        Err(Error::IdentityChanged)
    ));
    assert!(!trust.is_trusted(u, thief.device, &thief.key.public_key()));
}

#[test]
fn a_new_identity_is_reported_and_needs_the_person_to_agree() {
    let u = user();
    let (a, fresh) = (device(u), device(u));
    let mut trust = Trust::new();
    trust.verify_list(u, &[record(&a, None, 1)]).unwrap();
    // The person lost every device and starts again: a fresh unsigned first device.
    assert!(matches!(
        trust.verify_list(u, &[record(&fresh, None, 9)]),
        Err(Error::IdentityChanged)
    ));
    assert!(
        !trust.is_trusted(u, fresh.device, &fresh.key.public_key()),
        "a new identity was trusted silently"
    );
    // The same after every device was revoked first.
    trust.verify_list(u, &[]).unwrap();
    assert!(matches!(
        trust.verify_list(u, &[record(&fresh, None, 9)]),
        Err(Error::IdentityChanged)
    ));
    // After the person agrees, the new first device is trusted on first use.
    trust.accept_new_identity(u);
    let report = trust.verify_list(u, &[record(&fresh, None, 9)]).unwrap();
    assert!(report.first_contact);
    assert!(trust.is_trusted(u, fresh.device, &fresh.key.public_key()));
}

#[test]
fn malformed_entries_are_refused_without_error() {
    let u = user();
    let a = device(u);
    let mut trust = Trust::new();
    let bad_key = DeviceRecord {
        signature_key: vec![4u8; 65],
        ..record(&device(u), None, 0)
    };
    let short_key = DeviceRecord {
        signature_key: vec![4u8; 10],
        ..record(&device(u), None, 0)
    };
    let report = trust
        .verify_list(
            u,
            &[bad_key.clone(), short_key.clone(), record(&a, None, 1)],
        )
        .unwrap();
    assert_eq!(report.trusted.len(), 1);
    assert_eq!(report.rejected.len(), 2);
    assert!(report
        .rejected
        .iter()
        .all(|(_, why)| *why == Rejection::BadKey));
    // The same device twice on a list: the second is not believed.
    let report = trust
        .verify_list(u, &[record(&a, None, 1), record(&a, None, 2)])
        .unwrap();
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.trusted.len(), 1);
}

#[test]
fn each_person_is_trusted_separately() {
    let (u, v) = (user(), user());
    let (a, b) = (device(u), device(v));
    let mut trust = Trust::new();
    trust.verify_list(u, &[record(&a, None, 1)]).unwrap();
    trust.verify_list(v, &[record(&b, None, 1)]).unwrap();
    assert!(trust.is_trusted(u, a.device, &a.key.public_key()));
    assert!(!trust.is_trusted(v, a.device, &a.key.public_key()));
    assert!(!trust.is_trusted(u, b.device, &b.key.public_key()));
}

fn refs(identities: &[&DeviceIdentity]) -> Vec<DeviceRef> {
    identities
        .iter()
        .map(|identity| identity.device_ref())
        .collect()
}

#[test]
fn the_safety_number_is_symmetric_sixty_digits_and_changes_with_any_device() {
    let (u, v) = (user(), user());
    let (a1, a2, b1) = (device(u), device(u), device(v));
    let one = safety_number(u, &refs(&[&a1]), v, &refs(&[&b1])).unwrap();
    let flipped = safety_number(v, &refs(&[&b1]), u, &refs(&[&a1])).unwrap();
    assert_eq!(one, flipped, "both sides must see the same number");
    assert_eq!(one.digits().len(), 60);
    assert!(one.digits().bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(one.grouped().split(' ').count(), 12);
    assert!(one.grouped().split(' ').all(|group| group.len() == 5));
    // Deterministic, and independent of the order devices are listed in.
    assert_eq!(
        one,
        safety_number(u, &refs(&[&a1]), v, &refs(&[&b1])).unwrap()
    );
    let two = safety_number(u, &refs(&[&a1, &a2]), v, &refs(&[&b1])).unwrap();
    assert_eq!(
        two,
        safety_number(v, &refs(&[&b1]), u, &refs(&[&a2, &a1])).unwrap()
    );
    // A new device changes it; so does a different key for the same device.
    assert_ne!(one, two);
    let moved = DeviceIdentity {
        user: u,
        device: a1.device,
        key: DeviceKey::generate().unwrap(),
    };
    assert_ne!(
        one,
        safety_number(u, &refs(&[&moved]), v, &refs(&[&b1])).unwrap()
    );
    // Another pair of people has another number; a person with themself works.
    let w = user();
    let c1 = device(w);
    assert_ne!(
        one,
        safety_number(u, &refs(&[&a1]), w, &refs(&[&c1])).unwrap()
    );
    assert!(safety_number(u, &refs(&[&a1]), u, &refs(&[&a1])).is_ok());
    // Devices of other people mixed into a list do not count.
    assert_eq!(
        one,
        safety_number(u, &refs(&[&a1, &b1]), v, &refs(&[&b1, &a1])).unwrap()
    );
}
