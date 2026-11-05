//! The device link: its code, its channel and its messages (`e2ee::link`). What the server in the
//! middle can do is played here: it relays, drops, repeats, reorders and swaps what it relays, and
//! it may put its own key in place of the old device's.

use e2ee::link::{self, Channel, LinkCode, Message, Pending, Role, Scope};
use proptest::prelude::*;

fn linked(link_id: [u8; 16]) -> (Pending, Channel, Channel) {
    let pending = Pending::create(link_id).unwrap();
    let carried = LinkCode::parse(&pending.code().encode()).unwrap();
    let (old_pub, old) = link::join(&carried).unwrap();
    let new = pending.accept(&old_pub).unwrap();
    (pending, new, old)
}

#[test]
fn a_code_is_the_prefix_and_97_bytes_and_reads_back_whitespace_and_all() {
    let pending = Pending::create([3u8; 16]).unwrap();
    let text = pending.code().encode();
    assert!(text.starts_with("tiny-link-1."));
    // base64url of 97 bytes, no padding.
    assert_eq!(text.len(), "tiny-link-1.".len() + 130);
    assert_eq!(&LinkCode::parse(&text).unwrap(), pending.code());
    let wrapped: String = text
        .chars()
        .enumerate()
        .flat_map(|(n, c)| {
            if n % 20 == 19 {
                vec![c, '\n', ' ']
            } else {
                vec![c]
            }
        })
        .collect();
    assert_eq!(
        &LinkCode::parse(&format!("  {wrapped}\r\n")).unwrap(),
        pending.code()
    );
    assert_eq!(
        link::parse_link_id(&pending.code().link_id_text()).unwrap(),
        pending.code().link_id
    );
    assert!(link::parse_link_id("AAAA").is_err());
}

#[test]
fn what_is_not_a_code_is_refused() {
    let good = Pending::create([3u8; 16]).unwrap().code().encode();
    let body = good.strip_prefix("tiny-link-1.").unwrap();
    for bad in [
        String::new(),
        "tiny-link-1.".to_owned(),
        format!("tiny-link-2.{body}"),
        format!("tiny-device-{body}"),
        format!("tiny-link-1.{}", &body[..body.len() - 2]),
        format!("tiny-link-1.{body}AA"),
        format!("tiny-link-1.{}", body.replace('A', "+")),
    ] {
        assert!(LinkCode::parse(&bad).is_err(), "{bad}");
    }
    // A key that is not on the curve: the point's x changed.
    use base64::Engine as _;
    let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .unwrap();
    bytes[16 + 1] ^= 1;
    let off_curve = format!(
        "tiny-link-1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
    );
    assert!(LinkCode::parse(&off_curve).is_err());
    bytes[16] = 2;
    assert!(
        LinkCode::parse(&format!(
            "tiny-link-1.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
        ))
        .is_err(),
        "only the uncompressed form"
    );
}

#[test]
fn the_whole_conversation_runs_and_both_screens_show_one_number() {
    let (_, mut new, mut old) = linked([9u8; 16]);
    assert_eq!(new.role(), Role::New);
    assert_eq!(old.role(), Role::Old);
    assert_eq!(new.check_digits(), old.check_digits());
    assert!(new.check_digits().bytes().all(|b| b.is_ascii_digit()));
    let hello = Message::Hello {
        device_id: "8b0c2a51-0000-4000-8000-000000000001".into(),
        signature_key: "BAAA".into(),
        label: "Pixel".into(),
        os: "Android".into(),
    };
    let sent = new.send(&hello).unwrap();
    assert_eq!(old.receive(&sent).unwrap(), hello);
    let approve = Message::Approve {
        cross_signature: "sig".into(),
        signer: None,
        history: Scope::Days(30),
        archive: Some(link::ArchiveRef {
            key: "k".into(),
            bytes: 10,
            sha256: "h".into(),
            chunks: 1,
        }),
    };
    assert_eq!(new.receive(&old.send(&approve).unwrap()).unwrap(), approve);
    assert_eq!(
        old.receive(&new.send(&Message::Done {}).unwrap()).unwrap(),
        Message::Done {}
    );
}

#[test]
fn two_links_never_share_a_check_or_keys() {
    let (_, mut a_new, _) = linked([1u8; 16]);
    let (_, _, mut b_old) = linked([1u8; 16]);
    let sealed = a_new.seal(b"hello").unwrap();
    assert!(b_old.open(&sealed).is_err());
}

#[test]
fn a_server_in_the_middle_with_its_own_key_and_a_guessed_secret_gets_nothing() {
    // The server knows the link id and sees `old_pub`. Say it even learned `new_pub` (it never
    // sees the code): with a secret of its own guessing it derives other keys.
    let pending = Pending::create([5u8; 16]).unwrap();
    let real = pending.code().clone();
    let (old_pub, mut old) = link::join(&real).unwrap();

    let mut forged_bytes = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(real.encode().strip_prefix("tiny-link-1.").unwrap())
            .unwrap()
    };
    let last = forged_bytes.len() - 1;
    forged_bytes[last] ^= 0x55;
    let forged = LinkCode::parse(&{
        use base64::Engine as _;
        format!(
            "tiny-link-1.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&forged_bytes)
        )
    })
    .unwrap();
    // The server, as an old device, answers the new one with its own key.
    let (mallory_pub, mut mallory) = link::join(&forged).unwrap();
    let mut new = pending.accept(&mallory_pub).unwrap();
    assert_ne!(new.check_digits(), mallory.check_digits());
    let hello = new.seal(b"hello").unwrap();
    assert!(
        mallory.open(&hello).is_err(),
        "it cannot read the new device"
    );
    let lie = mallory.seal(b"approve").unwrap();
    assert!(new.open(&lie).is_err(), "nor speak for the old one");
    // And the real old device's answer, held back, no longer matches the new device.
    let held = old.seal(b"approve").unwrap();
    assert!(new.open(&held).is_err());
    let _ = old_pub;
}

#[test]
fn a_key_that_is_not_a_point_is_refused_before_anything() {
    let pending = Pending::create([5u8; 16]).unwrap();
    assert!(pending.accept(&[4u8; 65]).is_err());
    assert!(pending.accept(&[0u8; 0]).is_err());
    assert!(pending.accept(&[4u8; 64]).is_err());
}

#[test]
fn replays_reorders_and_reflections_are_dropped() {
    let (_, mut new, mut old) = linked([2u8; 16]);
    let one = new.seal(b"one").unwrap();
    let two = new.seal(b"two").unwrap();
    let three = new.seal(b"three").unwrap();
    assert_eq!(&*old.open(&two).unwrap(), b"two", "a gap is allowed");
    assert!(
        old.open(&one).is_err(),
        "an older counter after a newer one"
    );
    assert!(old.open(&two).is_err(), "a replay");
    assert_eq!(&*old.open(&three).unwrap(), b"three");
    assert!(new.open(&three).is_err(), "a reflection");
    // A message whose counter was changed, in either direction, does not authenticate.
    let mut four = new.seal(b"four").unwrap();
    four[7] = four[7].wrapping_add(1);
    assert!(old.open(&four).is_err());
}

#[test]
fn too_long_a_message_is_refused_and_the_longest_fits_the_relay() {
    let (_, mut new, _) = linked([2u8; 16]);
    assert!(new.seal(&vec![0u8; link::MAX_PLAINTEXT + 1]).is_err());
    let sealed = new.seal(&vec![0u8; link::MAX_PLAINTEXT]).unwrap();
    assert_eq!(sealed.len(), 8 * 1024);
}

#[test]
fn a_message_that_opens_but_is_not_one_is_malformed() {
    let (_, mut new, mut old) = linked([2u8; 16]);
    assert!(old
        .receive(&new.seal(b"{\"t\":\"steal\"}").unwrap())
        .is_err());
    assert!(old.receive(&new.seal(b"not json").unwrap()).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn any_message_goes_both_ways_and_any_changed_bit_is_refused(
        link_id in any::<[u8; 16]>(),
        texts in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..300), 1..6),
        flip in any::<usize>(),
    ) {
        let (_, mut new, mut old) = linked(link_id);
        for text in &texts {
            let sealed = new.seal(text).unwrap();
            let mut bent = sealed.clone();
            let at = flip % bent.len();
            bent[at] ^= 1 << (flip % 8);
            prop_assert!(old.open(&bent).is_err());
            prop_assert_eq!(&*old.open(&sealed).unwrap(), text.as_slice());
            let back = old.seal(text).unwrap();
            prop_assert_eq!(&*new.open(&back).unwrap(), text.as_slice());
        }
    }

    #[test]
    fn a_cut_message_never_opens(text in proptest::collection::vec(any::<u8>(), 0..200), cut in any::<usize>()) {
        let (_, mut new, mut old) = linked([4u8; 16]);
        let sealed = new.seal(&text).unwrap();
        let cut = cut % sealed.len();
        prop_assert!(old.open(&sealed[..cut]).is_err());
        prop_assert!(old.open(&sealed).is_ok());
    }

    #[test]
    fn parsing_anything_returns(text in ".{0,300}") {
        let _ = LinkCode::parse(&text);
        let _ = Scope::parse(&text);
        let _ = link::parse_link_id(&text);
    }

    #[test]
    fn opening_anything_returns(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        e2ee::parsers::link(&bytes);
    }
}
