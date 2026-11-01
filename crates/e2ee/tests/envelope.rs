use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use e2ee::attachment::AttachmentKey;
use e2ee::envelope::{AttachmentRef, Envelope, Kind, MAX_ATTACHMENTS, MAX_ENVELOPE_BYTES};
use e2ee::Error;

fn attachment(n: u8) -> AttachmentRef {
    AttachmentRef {
        id: format!("0000000{n}-0000-4000-8000-000000000000"),
        key: AttachmentKey::from_bytes([n; 32]),
        name: format!("photo {n}.jpg"),
        mime: "image/jpeg".into(),
        bytes: 12_345 + u64::from(n),
        sha256: [n.wrapping_add(1); 32],
        width: 640,
        height: 480,
    }
}

fn decode(text: &str) -> Result<Envelope, Error> {
    Envelope::decode(Kind::Message, text.as_bytes())
}

#[test]
fn the_example_in_the_spec_decodes() {
    let key = STANDARD.encode([7u8; 32]);
    let hash = STANDARD.encode([9u8; 32]);
    let text = format!(
        r#"{{ "v": 1, "text": "hello", "reply_to": "abc", "attachments": [
            {{ "id": "att-1", "key": "{key}", "name": "a.png", "mime": "image/png", "bytes": 12345,
               "sha256": "{hash}", "width": 0, "height": 0 }} ],
            "sent_at": 1730000000000 }}"#
    );
    let envelope = decode(&text).unwrap();
    assert_eq!(envelope.text, "hello");
    assert_eq!(envelope.reply_to.as_deref(), Some("abc"));
    assert_eq!(envelope.sent_at, Some(1_730_000_000_000));
    assert_eq!(envelope.attachments.len(), 1);
    assert_eq!(envelope.attachments[0].bytes, 12345);
    assert_eq!(envelope.attachments[0].key.as_bytes(), &[7u8; 32]);
    assert_eq!(envelope.attachments[0].sha256, [9u8; 32]);
}

#[test]
fn a_message_and_an_edit_round_trip() {
    let envelope = Envelope {
        text: "héllo \u{1F600}".into(),
        reply_to: Some("m1".into()),
        attachments: vec![attachment(1), attachment(2)],
        sent_at: Some(1_730_000_000_000),
    };
    let bytes = envelope.encode(Kind::Message).unwrap();
    assert_eq!(Envelope::decode(Kind::Message, &bytes).unwrap(), envelope);
    // An edit carries v, text and attachments, and nothing else.
    let bytes = envelope.encode(Kind::Edit).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        !text.contains("reply_to") && !text.contains("sent_at"),
        "{text}"
    );
    let decoded = Envelope::decode(Kind::Edit, &bytes).unwrap();
    assert_eq!(decoded.text, envelope.text);
    assert_eq!(decoded.attachments, envelope.attachments);
    assert_eq!((decoded.reply_to, decoded.sent_at), (None, None));
    // An edit read from a message with extra fields ignores them.
    let from_message = envelope.encode(Kind::Message).unwrap();
    let decoded = Envelope::decode(Kind::Edit, &from_message).unwrap();
    assert_eq!((decoded.reply_to, decoded.sent_at), (None, None));
}

#[test]
fn unknown_fields_are_tolerated_at_every_level() {
    let key = STANDARD.encode([1u8; 32]);
    let text = format!(
        r#"{{"v":1,"text":"x","future":{{"a":[1,2,3],"b":null}},"attachments":[{{"id":"i","key":"{key}","name":"n","mime":"m","bytes":1,"sha256":"{key}","blurhash":"abc","extra":[true]}}]}}"#
    );
    let envelope = decode(&text).unwrap();
    assert_eq!(envelope.text, "x");
    assert_eq!(envelope.attachments.len(), 1);
}

#[test]
fn bad_types_are_rejected() {
    let key = STANDARD.encode([1u8; 32]);
    let one = |tail: &str| {
        format!(
            r#"{{"v":1,"attachments":[{{"id":"i","key":"{key}","name":"n","mime":"m","bytes":1,"sha256":"{key}"{tail}}}]}}"#
        )
    };
    let bad = [
        r#"{"v":"1","text":"x"}"#.to_owned(),
        r#"{"v":1.5,"text":"x"}"#.to_owned(),
        r#"{"v":1,"text":5}"#.to_owned(),
        r#"{"v":1,"text":null}"#.to_owned(),
        r#"{"v":1,"text":"x","reply_to":7}"#.to_owned(),
        r#"{"v":1,"text":"x","sent_at":"now"}"#.to_owned(),
        r#"{"v":1,"text":"x","sent_at":1.5}"#.to_owned(),
        r#"{"v":1,"text":"x","attachments":{}}"#.to_owned(),
        r#"{"v":1,"text":"x","attachments":null}"#.to_owned(),
        r#"{"v":1,"text":"x","attachments":[1]}"#.to_owned(),
        one(r#","bytes":2"#),
        r#"{"v":1,"text":"x","text":"y"}"#.to_owned(),
        one(r#","width":-1"#),
        one(r#","height":"tall""#),
        format!(r#"{{"v":1,"attachments":[{{"id":"i","key":"{key}","name":"n","mime":"m","bytes":-1,"sha256":"{key}"}}]}}"#),
        format!(r#"{{"v":1,"attachments":[{{"id":"i","key":"{key}","name":"n","mime":"m","bytes":"1","sha256":"{key}"}}]}}"#),
        format!(r#"{{"v":1,"attachments":[{{"id":5,"key":"{key}","name":"n","mime":"m","bytes":1,"sha256":"{key}"}}]}}"#),
        format!(r#"{{"v":1,"attachments":[{{"id":"","key":"{key}","name":"n","mime":"m","bytes":1,"sha256":"{key}"}}]}}"#),
        format!(r#"{{"v":1,"attachments":[{{"id":"i","key":"{key}","name":"n","mime":"m","bytes":1}}]}}"#),
        format!(r#"{{"v":1,"attachments":[{{"key":"{key}","name":"n","mime":"m","bytes":1,"sha256":"{key}"}}]}}"#),
        r#"{"v":1,"attachments":[{"id":"i","key":"AAAA","name":"n","mime":"m","bytes":1,"sha256":"AAAA"}]}"#.to_owned(),
        r#"{"v":1,"attachments":[{"id":"i","key":"!!!!","name":"n","mime":"m","bytes":1,"sha256":"!!!!"}]}"#.to_owned(),
        "[]".to_owned(),
        "null".to_owned(),
        r#""v""#.to_owned(),
        String::new(),
        "{".to_owned(),
        r#"{"v":1,"text":"x"} trailing"#.to_owned(),
    ];
    for text in bad {
        assert!(decode(&text).is_err(), "accepted: {text}");
    }
}

#[test]
fn the_version_must_be_one() {
    assert!(matches!(
        decode(r#"{"v":2,"text":"x"}"#),
        Err(Error::UnsupportedVersion)
    ));
    assert!(matches!(
        decode(r#"{"v":0,"text":"x"}"#),
        Err(Error::UnsupportedVersion)
    ));
    assert!(decode(r#"{"text":"x"}"#).is_err());
    assert!(decode(r#"{"v":1,"text":"x"}"#).is_ok());
    // A missing text is the empty message (an attachment-only post).
    assert_eq!(decode(r#"{"v":1}"#).unwrap().text, "");
}

#[test]
fn invalid_utf8_is_rejected() {
    for bytes in [
        &b"{\"v\":1,\"text\":\"\xff\"}"[..],
        b"{\"v\":1,\"text\":\"\xc3\x28\"}",
        b"\xef\xbb\xbf{\"v\":1}",
        &[0xf0, 0x28, 0x8c, 0x28],
    ] {
        assert!(Envelope::decode(Kind::Message, bytes).is_err(), "{bytes:?}");
    }
}

#[test]
fn the_size_limit_holds_both_ways() {
    let overhead = Envelope::message("").encode(Kind::Message).unwrap().len();
    let room = MAX_ENVELOPE_BYTES - overhead;
    let full = Envelope::message("a".repeat(room));
    let bytes = full.encode(Kind::Message).unwrap();
    assert_eq!(bytes.len(), MAX_ENVELOPE_BYTES);
    assert_eq!(Envelope::decode(Kind::Message, &bytes).unwrap(), full);
    // One byte more is refused to encode and to decode.
    assert!(matches!(
        Envelope::message("a".repeat(room + 1)).encode(Kind::Message),
        Err(Error::TooLarge(_))
    ));
    let mut over = bytes.clone();
    over.insert(1, b' ');
    assert_eq!(over.len(), MAX_ENVELOPE_BYTES + 1);
    assert!(matches!(
        Envelope::decode(Kind::Message, &over),
        Err(Error::TooLarge(_))
    ));
    // A hostile megabyte is refused without parsing it.
    assert!(matches!(
        Envelope::decode(Kind::Message, &vec![b'['; 1 << 20]),
        Err(Error::TooLarge(_))
    ));
}

#[test]
fn attachment_counts_and_field_lengths_are_limited() {
    let many = Envelope {
        attachments: (0..=MAX_ATTACHMENTS as u8).map(attachment).collect(),
        ..Envelope::default()
    };
    assert!(many.encode(Kind::Message).is_err());
    let ok = Envelope {
        attachments: (0..MAX_ATTACHMENTS as u8).map(attachment).collect(),
        ..Envelope::default()
    };
    let bytes = ok.encode(Kind::Message).unwrap();
    assert_eq!(
        Envelope::decode(Kind::Message, &bytes)
            .unwrap()
            .attachments
            .len(),
        MAX_ATTACHMENTS
    );
    let long_name = Envelope {
        attachments: vec![AttachmentRef {
            name: "n".repeat(256),
            ..attachment(1)
        }],
        ..Envelope::default()
    };
    assert!(long_name.encode(Kind::Message).is_err());
    let key = STANDARD.encode([1u8; 32]);
    let name = "n".repeat(256);
    let text = format!(
        r#"{{"v":1,"attachments":[{{"id":"i","key":"{key}","name":"{name}","mime":"m","bytes":1,"sha256":"{key}"}}]}}"#
    );
    assert!(decode(&text).is_err());
}

#[test]
fn deep_nesting_cannot_crash_the_decoder() {
    // Whether the decoder skips a deep unknown field or refuses it is not the contract (serde_json
    // has a recursion limit for some shapes); that it returns, within the 16 KiB limit, is.
    let deep = format!(r#"{{"v":1,"x":{}{}}}"#, "[".repeat(5000), "]".repeat(5000));
    let _ = decode(&deep);
    let deep = format!(
        r#"{{"v":1,"x":{}1{}}}"#,
        r#"{"a":"#.repeat(3000),
        "}".repeat(3000)
    );
    let _ = decode(&deep);
    let deep = format!(
        r#"{{"v":1,"attachments":{}{}}}"#,
        "[".repeat(5000),
        "]".repeat(5000)
    );
    assert!(decode(&deep).is_err());
    let deep = format!(r#"{{"v":1,"text":{}"#, "[".repeat(8000));
    assert!(decode(&deep).is_err());
}

#[test]
fn the_kind_strings_are_the_servers() {
    assert_eq!(Kind::Message.as_str(), "message");
    assert_eq!(Kind::Edit.as_str(), "edit");
    assert_eq!(Kind::parse("edit").unwrap(), Kind::Edit);
    assert!(Kind::parse("Edit").is_err());
    assert!(Kind::parse("").is_err());
}
