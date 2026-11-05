//! The encrypted backup file (`e2ee::backup` around `e2ee::archive`): it reads back with the
//! passphrase and nothing else, and a header changed in the file (a cheaper cost, another salt)
//! makes it fail to open rather than open weaker.

use e2ee::archive::{self, Merged, Meta, Target, Writer};
use e2ee::backup::{Header, Params, HEADER_LEN};
use e2ee::link::Scope;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn cheap() -> Params {
    Params {
        memory_kib: 64,
        passes: 1,
        lanes: 1,
    }
}

fn file(passphrase: &str, user: &str) -> Vec<u8> {
    let header = Header::new(cheap()).unwrap();
    let key = header.key(passphrase).unwrap();
    let meta = Meta {
        v: archive::VERSION,
        public_trust: None,
        user: user.into(),
        from_device: "d".into(),
        created: "0".into(),
        scope: Scope::All,
    };
    let mut out = header.to_bytes().to_vec();
    let mut writer = Writer::new(&key, &header.to_bytes(), &meta, &mut out).unwrap();
    writer
        .message("c1", "m1", &json!({ "text": "kept safe" }))
        .unwrap();
    writer.verified("bea", "12345").unwrap();
    writer.finish().unwrap();
    out
}

#[derive(Default)]
struct Cache {
    rows: BTreeMap<String, Value>,
}

impl Target for Cache {
    fn is_member(&mut self, _conv: &str) -> bool {
        true
    }
    fn has(&mut self, conv: &str, id: &str) -> bool {
        self.rows.contains_key(&format!("{conv}/{id}"))
    }
    fn put(&mut self, conv: &str, id: &str, opened: Value) -> e2ee::Result<()> {
        self.rows.insert(format!("{conv}/{id}"), opened);
        Ok(())
    }
    fn verified(&mut self, _user: &str, _number: &str) -> e2ee::Result<()> {
        Ok(())
    }
}

fn import(bytes: &[u8], passphrase: &str, user: &str, cache: &mut Cache) -> e2ee::Result<Merged> {
    let header = Header::parse(bytes)?;
    let key = header.key(passphrase)?;
    let body = &bytes[HEADER_LEN..];
    archive::merge(&key, &bytes[..HEADER_LEN], user, || Ok(body), cache).map(|(_, merged)| merged)
}

#[test]
fn a_backup_reads_back_with_its_passphrase_only() {
    let bytes = file("a long passphrase of mine", "ada");
    let mut cache = Cache::default();
    assert!(import(&bytes, "a long passphrase of mine!", "ada", &mut cache).is_err());
    assert!(cache.rows.is_empty());
    let merged = import(&bytes, "a long passphrase of mine", "ada", &mut cache).unwrap();
    assert_eq!((merged.added, merged.verified), (1, 1));
    assert_eq!(cache.rows["c1/m1"], json!({ "text": "kept safe" }));
    assert_eq!(
        import(&bytes, "a long passphrase of mine", "ada", &mut cache)
            .unwrap()
            .added,
        0
    );
}

#[test]
fn a_header_changed_in_the_file_does_not_open() {
    let bytes = file("passphrase", "ada");
    for at in 5..HEADER_LEN {
        let mut bent = bytes.clone();
        bent[at] ^= 1;
        let mut cache = Cache::default();
        assert!(
            import(&bent, "passphrase", "ada", &mut cache).is_err(),
            "byte {at}"
        );
        assert!(cache.rows.is_empty());
    }
}

#[test]
fn another_persons_backup_is_refused() {
    let bytes = file("passphrase", "bea");
    assert!(import(&bytes, "passphrase", "ada", &mut Cache::default()).is_err());
}
