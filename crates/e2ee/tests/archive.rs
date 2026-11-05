//! The history archive (`e2ee::archive`): what is written reads back, record for record, at any size
//! and any chunk boundary; anything else (a changed bit, a cut, a chunk moved or repeated, the wrong
//! key or link, records out of order, a count that lies) is refused; merging is idempotent, never
//! overwrites, and drops what is not the person's.

use e2ee::archive::{self, ArchiveKey, Merged, Meta, Record, Target, Writer, CHUNK, HEADER_LEN};
use e2ee::link::Scope;
use proptest::prelude::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const TAG: usize = 16;
const LINK: &[u8] = b"0123456789abcdef";

fn meta(user: &str) -> Meta {
    Meta {
        v: archive::VERSION,
        public_trust: None,
        user: user.into(),
        from_device: "old-device".into(),
        created: "2026-11-05T12:00:00Z".into(),
        scope: Scope::All,
    }
}

#[test]
fn older_metadata_without_public_trust_still_deserializes() {
    let meta: Meta = serde_json::from_value(json!({
        "v": 1, "user": "ada", "from_device": "d", "created": "0", "scope": "all"
    }))
    .unwrap();
    assert!(meta.public_trust.is_none());
}

#[test]
fn malformed_public_metadata_is_rejected_before_any_message_merges() {
    let user = e2ee::UserId::random().unwrap();
    let engine = e2ee::Engine::detached(user).unwrap();
    let mut public = serde_json::to_value(engine.public_trust().unwrap()).unwrap();
    public["users"][user.to_string()]["devices"][engine.device_id().to_string()] = json!("AA==");
    let mut meta = meta("ada");
    meta.public_trust = Some(serde_json::from_value(public).unwrap());
    let key = ArchiveKey::generate().unwrap();
    let mut writer = Writer::new(&key, LINK, &meta, Vec::new()).unwrap();
    writer
        .message("conv-0", "m", &json!({"text":"kept out"}))
        .unwrap();
    let (blob, _) = writer.finish().unwrap();
    let mut cache = Cache {
        member_of: ["conv-0".to_owned()].into(),
        ..Cache::default()
    };
    assert!(archive::merge(&key, LINK, "ada", || Ok(blob.as_slice()), &mut cache).is_err());
    assert!(cache.rows.is_empty());
}

#[test]
fn public_metadata_over_the_archive_record_limit_is_refused_without_truncation() {
    use base64::Engine as _;
    let user = e2ee::UserId::random().unwrap();
    let engine = e2ee::Engine::detached(user).unwrap();
    let key = base64::engine::general_purpose::STANDARD.encode(engine.signature_key());
    let devices: serde_json::Map<String, Value> = (0u128..10_000)
        .map(|n| (e2ee::DeviceId(n.to_be_bytes()).to_string(), json!(key)))
        .collect();
    let public = json!({"users": { user.to_string(): {"pinned": true, "devices": devices} }});
    let mut meta = meta("ada");
    meta.public_trust = Some(serde_json::from_value(public).unwrap());
    let archive_key = ArchiveKey::generate().unwrap();
    assert!(matches!(
        Writer::new(&archive_key, LINK, &meta, Vec::new()),
        Err(e2ee::Error::TooLarge("archive record"))
    ));
}

type Msg = (String, String, Value);

fn build(
    key: &ArchiveKey,
    msgs: &[Msg],
    verified: &[(String, String)],
) -> (Vec<u8>, archive::Summary) {
    let mut writer = Writer::new(key, LINK, &meta("ada"), Vec::new()).unwrap();
    for (conv, id, opened) in msgs {
        writer.message(conv, id, opened).unwrap();
    }
    for (user, number) in verified {
        writer.verified(user, number).unwrap();
    }
    writer.finish().unwrap()
}

fn records(key: &ArchiveKey, link: &[u8], blob: &[u8]) -> e2ee::Result<Vec<Record>> {
    let mut out = Vec::new();
    archive::read(key, link, blob, |record| {
        out.push(record);
        Ok(())
    })?;
    Ok(out)
}

fn msg(n: usize, text_len: usize) -> Msg {
    (
        format!("conv-{}", n % 3),
        format!("id-{n:05}"),
        json!({ "text": "x".repeat(text_len), "attachments": [{ "id": "a", "key": "k", "name": "n", "mime": "image/png", "bytes": 3, "sha256": "s" }] }),
    )
}

/// The blob split back into its chunks, after the header.
fn chunks(blob: &[u8]) -> Vec<&[u8]> {
    blob[HEADER_LEN..].chunks(CHUNK + TAG).collect()
}

fn join(header: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut out = header.to_vec();
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

#[test]
fn several_chunks_read_back_with_their_summary() {
    let key = ArchiveKey::generate().unwrap();
    let msgs: Vec<Msg> = (0..300).map(|n| msg(n, 700)).collect();
    let verified = vec![("bea".to_owned(), "12345 67890".to_owned())];
    let (blob, summary) = build(&key, &msgs, &verified);
    assert!(summary.chunks >= 3, "{summary:?}");
    assert_eq!(summary.bytes, blob.len() as u64);
    assert_eq!(summary.msgs, 300);
    use sha2::Digest as _;
    assert_eq!(
        summary.sha256,
        <[u8; 32]>::from(sha2::Sha256::digest(&blob))
    );
    let read = records(&key, LINK, &blob).unwrap();
    assert_eq!(read.first(), Some(&Record::Meta(meta("ada"))));
    assert_eq!(read.last(), Some(&Record::End { msgs: 300 }));
    assert_eq!(read.len(), 300 + 1 + 2);
    for (record, (conv, id, opened)) in read[1..301].iter().zip(&msgs) {
        assert_eq!(
            record,
            &Record::Msg {
                conv: conv.clone(),
                id: id.clone(),
                opened: opened.clone()
            }
        );
    }
}

#[test]
fn the_wrong_key_or_the_wrong_link_opens_nothing() {
    let key = ArchiveKey::generate().unwrap();
    let (blob, _) = build(&key, &[msg(1, 10)], &[]);
    assert!(records(&ArchiveKey::generate().unwrap(), LINK, &blob).is_err());
    assert!(records(&key, b"another link id!", &blob).is_err());
    assert!(records(&key, LINK, &blob).is_ok());
}

#[test]
fn chunks_dropped_repeated_swapped_or_cut_are_refused() {
    let key = ArchiveKey::generate().unwrap();
    let msgs: Vec<Msg> = (0..400).map(|n| msg(n, 600)).collect();
    let (blob, _) = build(&key, &msgs, &[]);
    let header = &blob[..HEADER_LEN];
    let parts = chunks(&blob);
    assert!(parts.len() >= 4);
    let n = parts.len();
    let mut swapped = parts.clone();
    swapped.swap(0, 1);
    let mut repeated = parts.clone();
    repeated.insert(1, parts[1]);
    let mut dropped = parts.clone();
    dropped.remove(1);
    let without_last = &parts[..n - 1];
    let last_moved_up: Vec<&[u8]> = [&parts[..1], &parts[n - 1..]].concat();
    for (what, bad) in [
        ("swapped", join(header, &swapped)),
        ("repeated", join(header, &repeated)),
        ("dropped", join(header, &dropped)),
        ("without its last chunk", join(header, without_last)),
        ("the last chunk early", join(header, &last_moved_up)),
        ("header only", header.to_vec()),
        ("nothing", Vec::new()),
        ("another magic", [b"TNYA1".as_slice(), &blob[5..]].concat()),
        ("bytes appended", [blob.as_slice(), b"x"].concat()),
    ] {
        assert!(records(&key, LINK, &bad).is_err(), "{what}");
    }
}

/// An archive built record by record by hand, sealed properly: for refusing what a well-sealed
/// but ill-formed stream says.
fn sealed_records(key: &ArchiveKey, records: &[Value]) -> Vec<u8> {
    // The writer always starts with its own meta; a blob of other records is sealed directly
    // with the same construction as `Writer` uses.
    use aes_gcm::aead::generic_array::GenericArray;
    use aes_gcm::aead::stream::EncryptorBE32;
    use aes_gcm::aead::{KeyInit, Payload};
    let prefix = [9u8; 7];
    let mut header = b"TNYH1".to_vec();
    header.extend_from_slice(&prefix);
    let mut aad = b"tiny-archive-v1".to_vec();
    aad.extend_from_slice(LINK);
    aad.extend_from_slice(&header);
    let mut plain = Vec::new();
    for record in records {
        let json = serde_json::to_vec(record).unwrap();
        plain.extend_from_slice(&(json.len() as u32).to_be_bytes());
        plain.extend_from_slice(&json);
    }
    assert!(plain.len() <= CHUNK);
    let cipher = aes_gcm::Aes256Gcm::new(GenericArray::from_slice(key.as_bytes()));
    let sealed = EncryptorBE32::from_aead(cipher, GenericArray::from_slice(&prefix))
        .encrypt_last(Payload {
            msg: &plain,
            aad: &aad,
        })
        .unwrap();
    [header, sealed].concat()
}

#[test]
fn well_sealed_streams_that_break_the_record_rules_are_refused() {
    let key = ArchiveKey::generate().unwrap();
    let meta = serde_json::to_value(Record::Meta(meta("ada"))).unwrap();
    let a_msg = json!({ "r": "msg", "conv": "c", "id": "i", "opened": {} });
    let end = |n: u64| json!({ "r": "end", "msgs": n });
    assert!(records(
        &key,
        LINK,
        &sealed_records(&key, &[meta.clone(), a_msg.clone(), end(1)])
    )
    .is_ok());
    let mut v2 = meta.clone();
    v2["v"] = json!(2);
    for (what, stream) in [
        ("no end", vec![meta.clone(), a_msg.clone()]),
        (
            "a count that lies",
            vec![meta.clone(), a_msg.clone(), end(2)],
        ),
        ("no meta first", vec![a_msg.clone(), meta.clone(), end(1)]),
        ("two metas", vec![meta.clone(), meta.clone(), end(0)]),
        ("after the end", vec![meta.clone(), end(0), a_msg.clone()]),
        (
            "an unknown record",
            vec![meta.clone(), json!({ "r": "steal" }), end(0)],
        ),
        ("version 2", vec![v2, end(0)]),
        ("nothing", vec![]),
    ] {
        assert!(
            records(&key, LINK, &sealed_records(&key, &stream)).is_err(),
            "{what}"
        );
    }
}

/// A cache in memory, with the conversations the person is in.
#[derive(Default)]
struct Cache {
    member_of: BTreeSet<String>,
    rows: BTreeMap<(String, String), Value>,
    verified: BTreeMap<String, String>,
}

impl Target for Cache {
    fn is_member(&mut self, conv: &str) -> bool {
        self.member_of.contains(conv)
    }

    fn has(&mut self, conv: &str, id: &str) -> bool {
        self.rows.contains_key(&(conv.to_owned(), id.to_owned()))
    }

    fn put(&mut self, conv: &str, id: &str, opened: Value) -> e2ee::Result<()> {
        self.rows.insert((conv.to_owned(), id.to_owned()), opened);
        Ok(())
    }

    fn verified(&mut self, user: &str, number: &str) -> e2ee::Result<()> {
        self.verified
            .entry(user.to_owned())
            .or_insert_with(|| number.to_owned());
        Ok(())
    }
}

#[test]
fn a_merge_adds_what_is_missing_keeps_what_is_there_and_drops_strangers() {
    let key = ArchiveKey::generate().unwrap();
    let msgs: Vec<Msg> = (0..9).map(|n| msg(n, 5)).collect();
    let (blob, _) = build(&key, &msgs, &[("bea".into(), "111".into())]);
    let mut cache = Cache {
        member_of: ["conv-0".to_owned(), "conv-1".to_owned()].into(),
        ..Cache::default()
    };
    // One row the device has already, with its own words: never overwritten.
    cache.rows.insert(
        ("conv-0".into(), "id-00000".into()),
        json!({ "text": "mine" }),
    );
    let open = || Ok(blob.as_slice());
    let (meta_read, merged) = archive::merge(&key, LINK, "ada", open, &mut cache).unwrap();
    assert_eq!(meta_read, meta("ada"));
    assert_eq!(
        merged,
        Merged {
            added: 5,
            kept: 1,
            refused: 3,
            verified: 1
        }
    );
    assert_eq!(
        cache.rows[&("conv-0".to_owned(), "id-00000".to_owned())],
        json!({ "text": "mine" })
    );
    assert!(!cache.rows.keys().any(|(conv, _)| conv == "conv-2"));
    let again = archive::merge(&key, LINK, "ada", open, &mut cache)
        .unwrap()
        .1;
    assert_eq!(again.added, 0, "a second merge adds nothing");
    assert_eq!(again.kept, 6);
}

#[test]
fn a_merge_of_another_persons_archive_or_a_broken_one_changes_nothing() {
    let key = ArchiveKey::generate().unwrap();
    let (blob, _) = build(&key, &[msg(0, 5)], &[]);
    let mut cache = Cache {
        member_of: ["conv-0".to_owned()].into(),
        ..Cache::default()
    };
    assert!(archive::merge(&key, LINK, "bea", || Ok(blob.as_slice()), &mut cache).is_err());
    let cut = &blob[..blob.len() - 1];
    assert!(archive::merge(&key, LINK, "ada", || Ok(cut), &mut cache).is_err());
    assert!(
        cache.rows.is_empty(),
        "nothing was merged from a blob that did not check out"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn any_archive_reads_back_and_any_changed_bit_or_cut_is_refused(
        sizes in proptest::collection::vec(0usize..9000, 0..40),
        flip in any::<usize>(),
        cut in any::<usize>(),
    ) {
        let key = ArchiveKey::generate().unwrap();
        let msgs: Vec<Msg> = sizes.iter().enumerate().map(|(n, len)| msg(n, *len)).collect();
        let (blob, summary) = build(&key, &msgs, &[]);
        let read = records(&key, LINK, &blob).unwrap();
        prop_assert_eq!(read.len(), msgs.len() + 2);
        prop_assert_eq!(summary.msgs, msgs.len() as u64);
        let mut bent = blob.clone();
        let at = flip % bent.len();
        bent[at] ^= 1 << (flip % 8);
        prop_assert!(records(&key, LINK, &bent).is_err());
        let cut = cut % blob.len();
        prop_assert!(records(&key, LINK, &blob[..cut]).is_err());
    }

    #[test]
    fn a_record_at_every_chunk_boundary_reads_back(pad in 0usize..64) {
        // Records sized so that the chunk boundary falls before, inside and after a length prefix.
        let key = ArchiveKey::generate().unwrap();
        let msgs: Vec<Msg> = (0..24).map(|n| msg(n, 2700 + pad + n)).collect();
        let (blob, _) = build(&key, &msgs, &[]);
        prop_assert_eq!(records(&key, LINK, &blob).unwrap().len(), 26);
    }

    #[test]
    fn opening_anything_returns(bytes in proptest::collection::vec(any::<u8>(), 0..2000)) {
        e2ee::parsers::archive(&bytes);
    }
}
