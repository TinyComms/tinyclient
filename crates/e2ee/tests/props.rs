mod common;
mod streams;

use common::*;
use e2ee::attachment::{self, AttachmentKey, Expect, Header};
use e2ee::envelope::{AttachmentRef, Envelope, Kind, MAX_ATTACHMENTS};
use e2ee::identity::{verify_cross_signature, DeviceKey, DeviceRecord, Trust};
use e2ee::store::{open_cache_value, CacheKey, StateKey};
use e2ee::{Claimed, ConversationId, DeviceId, Engine, UserId};
use proptest::prelude::*;
use streams::*;

const CHUNK: usize = 1 << 16;

// --- The envelope codec ---

fn attachment_strategy() -> impl Strategy<Value = AttachmentRef> {
    (
        "[a-zA-Z0-9-]{1,40}",
        any::<[u8; 32]>(),
        ".{0,60}",
        "[a-z]{1,10}/[a-z0-9.+-]{1,20}",
        any::<u64>(),
        any::<[u8; 32]>(),
        any::<u32>(),
        any::<u32>(),
    )
        .prop_map(
            |(id, key, name, mime, bytes, sha256, width, height)| AttachmentRef {
                id,
                key: AttachmentKey::from_bytes(key),
                name,
                mime,
                bytes,
                sha256,
                width,
                height,
            },
        )
}

fn envelope_strategy() -> impl Strategy<Value = Envelope> {
    (
        ".{0,3000}",
        proptest::option::of("[a-zA-Z0-9-]{1,40}"),
        proptest::collection::vec(attachment_strategy(), 0..=MAX_ATTACHMENTS.min(3)),
        proptest::option::of(any::<i64>()),
    )
        .prop_map(|(text, reply_to, attachments, sent_at)| Envelope {
            text,
            reply_to,
            attachments,
            sent_at,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn an_envelope_round_trips(envelope in envelope_strategy()) {
        let bytes = envelope.encode(Kind::Message).unwrap();
        prop_assert!(bytes.len() <= e2ee::envelope::MAX_ENVELOPE_BYTES);
        prop_assert_eq!(Envelope::decode(Kind::Message, &bytes).unwrap(), envelope.clone());
        // An edit keeps text and attachments and drops the rest.
        let edit = envelope.encode(Kind::Edit).unwrap();
        let decoded = Envelope::decode(Kind::Edit, &edit).unwrap();
        prop_assert_eq!(decoded.text, envelope.text);
        prop_assert_eq!(decoded.attachments, envelope.attachments);
        prop_assert!(decoded.reply_to.is_none() && decoded.sent_at.is_none());
    }

    #[test]
    fn the_envelope_decoder_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..2000)) {
        e2ee::parsers::envelope(&bytes);
    }

    #[test]
    fn the_envelope_decoder_never_panics_on_mangled_json(envelope in envelope_strategy(), at in any::<prop::sample::Index>(), byte in any::<u8>(), cut in any::<prop::sample::Index>()) {
        let mut bytes = envelope.encode(Kind::Message).unwrap();
        let spot = at.index(bytes.len());
        bytes[spot] = byte;
        e2ee::parsers::envelope(&bytes);
        bytes.truncate(cut.index(bytes.len() + 1));
        e2ee::parsers::envelope(&bytes);
    }
}

// --- Attachments ---

fn sizes() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(0usize),
        Just(1),
        Just(CHUNK - 1),
        Just(CHUNK),
        Just(CHUNK + 1),
        Just(2 * CHUNK - 1),
        Just(2 * CHUNK),
        Just(2 * CHUNK + 1),
        0usize..(3 * CHUNK + 10),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    /// Whatever the read sizes, in or out, sealing and opening give the same bytes: chunk
    /// boundaries do not depend on how the reader happens to deliver.
    #[test]
    fn short_and_odd_reads_change_nothing(len in sizes(), seed in any::<u64>(), in_sizes in proptest::collection::vec(1usize..70_000, 1..6), out_sizes in proptest::collection::vec(1usize..70_000, 1..6), write_size in 1usize..100_000) {
        let key = AttachmentKey::from_bytes([5; 32]);
        let header = Header { chunk_log2: 16, nonce_prefix: [1, 2, 3, 4, 5, 6, 7] };
        let plain = pattern_bytes(seed, len);

        let mut whole = Vec::new();
        let info = attachment::seal_with_header(&key, b"ctx", header, plain.as_slice(), &mut whole).unwrap();
        let mut sliced = ShortWriter { out: Vec::new(), size: write_size };
        let sliced_info = attachment::seal_with_header(&key, b"ctx", header, Short::new(&plain, in_sizes), &mut sliced).unwrap();
        prop_assert_eq!(&sliced.out, &whole);
        prop_assert_eq!(info, sliced_info);
        prop_assert_eq!(Some(whole.len() as u64), attachment::sealed_len(len as u64));

        let mut opened = ShortWriter { out: Vec::new(), size: write_size };
        let result = attachment::open(&key, b"ctx", Short::new(&whole, out_sizes), &mut opened, Expect { sha256: Some(&info.sha256), plain_len: len as u64 }).unwrap();
        prop_assert_eq!(result.plain_len, len as u64);
        prop_assert_eq!(opened.out, plain);
    }

    /// A blob cut anywhere, or with any one byte changed, is refused, at any read size.
    #[test]
    fn a_damaged_blob_is_always_refused(len in sizes(), seed in any::<u64>(), at in any::<prop::sample::Index>(), cut in any::<prop::sample::Index>(), sizes in proptest::collection::vec(1usize..70_000, 1..5)) {
        let key = AttachmentKey::from_bytes([6; 32]);
        let plain = pattern_bytes(seed, len);
        let mut sealed = Vec::new();
        attachment::seal(&key, b"", plain.as_slice(), &mut sealed).unwrap();
        // Cut anywhere short of the whole.
        let cut_at = cut.index(sealed.len());
        prop_assert!(attachment::open(&key, b"", Short::new(&sealed[..cut_at], sizes.clone()), std::io::sink(), Expect::length(len as u64)).is_err());
        // Any one changed bit.
        let spot = at.index(sealed.len());
        let mut bad = sealed.clone();
        bad[spot] ^= 1 << (seed % 8);
        prop_assert!(attachment::open(&key, b"", Short::new(&bad, sizes), std::io::sink(), Expect::length(len as u64)).is_err());
    }

    #[test]
    fn sealed_len_inverts_to_the_padded_len(plain in 0u64..(1 << 40)) {
        let padded = attachment::padded_len(plain).unwrap();
        prop_assert_eq!(attachment::padded_len_of(attachment::sealed_len(plain).unwrap()), Some(padded));
    }

    /// Padme rounds up, by under 12% (the worst is 15 bytes on 129), and its output is a fixed
    /// point.
    #[test]
    fn padme_rounds_up_by_little_and_is_stable(len in 0u64..(1 << 50)) {
        let padded = attachment::padme(len).unwrap();
        prop_assert!(padded >= len);
        prop_assert!((padded - len) * 100 <= len * 12, "{} -> {}", len, padded);
        prop_assert_eq!(attachment::padme(padded), Some(padded));
    }
}

// --- Parsers never panic ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn the_blob_parsers_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
        e2ee::parsers::blob_header(&bytes);
        e2ee::parsers::sealed_blob(&bytes);
        // A header that parses is followed by garbage chunks.
        let mut with_header = b"TNYA1\x10abcdefg".to_vec();
        with_header.extend_from_slice(&bytes);
        e2ee::parsers::sealed_blob(&with_header);
    }

    #[test]
    fn the_small_parsers_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..300), text in ".{0,80}") {
        let _ = UserId::parse(&text);
        let _ = DeviceId::from_slice(&bytes);
        let _ = Kind::parse(&text);
        let _ = DeviceKey::from_bytes(&bytes);
        let _ = e2ee::ids::parse_credential(&bytes);
        let user = UserId([1; 16]);
        let _ = verify_cross_signature(&bytes, user, DeviceId([2; 16]), &bytes, &bytes);
        let key = CacheKey::from_bytes([3; 32]);
        let _ = open_cache_value(&key, b"row", &bytes);
        let _ = Engine::from_snapshot(&StateKey::from_bytes([4; 32]), "s", &bytes);
        let mut trust = Trust::new();
        let record = DeviceRecord { device_id: DeviceId([5; 16]), signature_key: bytes.clone(), cross_signature: Some(bytes.clone()), created_at: bytes.len() as i64 };
        let _ = trust.verify_list(user, &[record.clone(), record]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// Arbitrary bytes into every MLS parser the engine exposes: proposals, commits, Welcomes,
    /// ciphertexts, keys.
    #[test]
    fn the_mls_parsers_never_panic_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
        e2ee::parsers::mls_message(&bytes);
    }

    /// Structured garbage: a varint prefix (the thing that makes tls_codec assert) then noise.
    #[test]
    fn the_mls_parsers_never_panic_on_hostile_prefixes(prefix in prop::sample::select(vec![0x00u8, 0x3f, 0x40, 0x7f, 0x80, 0xbf, 0xc0, 0xff]), len in any::<u8>(), tail in proptest::collection::vec(any::<u8>(), 0..300)) {
        let mut bytes = vec![prefix, len];
        bytes.extend_from_slice(&tail);
        e2ee::parsers::mls_message(&bytes);
        let mut dave = vec![0u8];
        dave.extend_from_slice(&bytes);
        e2ee::parsers::mls_message(&dave);
    }
}

/// Real artifacts of a real group, damaged: a flipped byte, a cut, a splice. Whether a damaged one
/// is refused is the group tests' business; here nothing may panic, and the state must stay usable.
#[test]
fn damaged_real_handshakes_and_ciphertexts_never_panic_and_do_not_stop_the_group() {
    let mut world = World::new();
    let (a, b, c) = (world.person("a"), world.person("b"), world.person("c"));
    world.sync_trust();
    world.start(a);
    world.change(&[a], a, &[b], &[]);
    let conv = world.conv();

    // Artifacts for the next change: proposals, commit, welcome, and a ciphertext of this epoch.
    let package = world.clients[c].engine.key_packages(1).unwrap().remove(0);
    let proposals = world.ds.proposals(&[&package], &[], 1);
    let ciphertext = world.send(a, "a real message");
    world.clients[a]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    let bundle = world.clients[a]
        .engine
        .commit_pending(conv)
        .unwrap()
        .unwrap();
    world.clients[a].engine.abort_commit(conv).unwrap();
    let artifacts = [
        ("proposals", proposals.clone()),
        ("commit", bundle.commit.clone()),
        ("welcome", bundle.welcomes[0].data.clone()),
        ("ciphertext", ciphertext.clone()),
    ];

    let mut state = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let epoch = world.epoch(b);
    let key = world.ds.public();
    let claimed = Claimed {
        author: None,
        device: None,
        kind: Kind::Message,
        target_id: None,
    };
    for (name, artifact) in &artifacts {
        for round in 0..150 {
            let mut bad = artifact.clone();
            match round % 3 {
                0 => {
                    let at = (next() as usize) % bad.len();
                    bad[at] ^= 1 << (next() % 8);
                }
                1 => bad.truncate((next() as usize) % bad.len()),
                _ => {
                    let at = (next() as usize) % bad.len();
                    bad.splice(at..at, [next() as u8, next() as u8]);
                }
            }
            let engine = &mut world.clients[b].engine;
            let _ = engine.handle_proposals(conv, &bad);
            let _ = engine.handle_commit(conv, epoch, &bad);
            let _ = engine.handle_welcome(ConversationId::random().unwrap(), &bad, Some(&key));
            let _ = engine.decrypt(conv, &bad, &claimed);
            let _ = name;
        }
    }
    // After all that the group still works: proposals, commit, and the message itself.
    world.clients[b]
        .engine
        .handle_proposals(conv, &proposals)
        .unwrap();
    let opened = world
        .open(b, &ciphertext, a)
        .expect("the genuine message still opens after the forgeries");
    assert_eq!(opened.envelope.text, "a real message");
    let outcome = world.clients[b]
        .engine
        .handle_commit(conv, bundle.epoch, &bundle.commit);
    // Our own `abort` above means a made a commit nobody applied; b applies it as a normal commit.
    assert!(outcome.is_ok(), "{outcome:?}");
    world.clients[c]
        .engine
        .handle_welcome(conv, &bundle.welcomes[0].data, Some(&key))
        .unwrap();
}
