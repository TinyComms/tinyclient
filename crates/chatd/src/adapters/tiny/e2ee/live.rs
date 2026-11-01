//! End to end against a real tinyserver with encrypted text (`hardening/e2ee-text-server`):
//! five devices of three people, an encrypted direct message, history, an edit, a sealed picture,
//! a device approved later that cannot read what came before, and a revoked one that cannot read
//! what comes after. Ignored unless `TINY_E2EE_SESSIONS` holds a JSON array of five Tiny sessions
//! on one server: Ada, Bea, Cyd, then Ada twice more (her second and third devices).
//!
//! `cargo test -p chatd --lib e2ee::live -- --ignored --nocapture`

use super::*;
use crate::adapters::tiny::sealing;

fn sessions() -> Option<Vec<Saved>> {
    let text = std::env::var("TINY_E2EE_SESSIONS").ok()?;
    let rows: Vec<Value> = serde_json::from_str(&text).expect("TINY_E2EE_SESSIONS is a JSON array");
    Some(
        rows.iter()
            .map(|row| super::super::parse(&row.to_string()).expect("a Tiny session"))
            .collect(),
    )
}

/// A device in a directory of its own, with keys of its own, outside the process's registry, so
/// two devices of one person can live in one test.
fn device(saved: &Saved, name: &str) -> Device {
    let mut saved = saved.clone();
    saved.e2ee = Some(Secrets::generate().unwrap());
    let mut device = Device::new(&saved);
    device.dir =
        std::env::temp_dir().join(format!("chatd-e2ee-live-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&device.dir);
    device.enrol();
    device
}

/// The rows of a conversation the server lists, as the read takes them.
fn page(device: &Device, conversation: &str) -> Vec<Value> {
    let (status, body) = device
        .ask(
            "GET",
            &format!("/v1/conversations/{conversation}/messages?limit=100"),
            None,
        )
        .unwrap();
    assert_eq!(status, 200, "{body}");
    let rows = body.get("messages").cloned().unwrap_or(body);
    rows.as_array().cloned().unwrap_or_default()
}

fn read_page(device: &mut Device, conversation: &str) -> HashMap<String, Read> {
    let rows = page(device, conversation);
    let wires: Vec<Row> = rows
        .iter()
        .map(|row| Row {
            id: row["id"].as_str().unwrap(),
            author_id: row["author_id"].as_str().unwrap_or(""),
            kind: row.get("kind").and_then(Value::as_str).unwrap_or("message"),
            ciphertext: row.get("ciphertext").and_then(Value::as_str),
            epoch: row.get("epoch").and_then(Value::as_u64).unwrap_or(0),
            device_id: row.get("device_id").and_then(Value::as_str).unwrap_or(""),
            target_id: row.get("target_id").and_then(Value::as_str),
            client_id: row.get("client_id").and_then(Value::as_str),
        })
        .collect();
    device.read(conversation, &wires)
}

fn text(read: &HashMap<String, Read>, id: &str) -> String {
    match read.get(id) {
        Some(Read::Opened(opened)) => opened.text.clone(),
        Some(Read::Undecryptable) => protocol::UNDECRYPTABLE.to_owned(),
        Some(Read::Edit) => "(edit)".to_owned(),
        None => "(absent)".to_owned(),
    }
}

fn id_of(answer: &Value) -> String {
    answer
        .get("id")
        .or_else(|| answer.pointer("/message/id"))
        .and_then(Value::as_str)
        .unwrap()
        .to_owned()
}

fn upload_sealed(device: &Device, path: &Path, id: &str) -> String {
    let body_file = std::fs::read(path).unwrap();
    let boundary = "tinyclient-e2e-boundary";
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"sealed\"\r\n\r\n1\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\n{id}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"sealed\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
    body.extend_from_slice(&body_file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut response = ureq::post(&format!("{}/v1/media", device.saved.base_url))
        .header("Authorization", &format!("Bearer {}", device.saved.token))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .config()
        .http_status_as_error(false)
        .build()
        .send(&body[..])
        .unwrap();
    let status = response.status().as_u16();
    let answer: Value = response.body_mut().read_json().unwrap_or(Value::Null);
    assert_eq!(status, 201, "{answer}");
    assert_eq!(
        answer["kind"], "sealed",
        "the server stores it as opaque bytes"
    );
    answer["id"].as_str().unwrap().to_owned()
}

#[test]
#[ignore]
fn live_e2ee_text_round_trip() {
    let Some(sessions) = sessions() else {
        eprintln!("TINY_E2EE_SESSIONS is not set");
        return;
    };
    let (ada_s, bea_s, cyd_s, ada2_s, ada3_s) = (
        &sessions[0],
        &sessions[1],
        &sessions[2],
        &sessions[3],
        &sessions[4],
    );
    let mut ada = device(ada_s, "ada");
    let mut bea = device(bea_s, "bea");
    let mut cyd = device(cyd_s, "cyd");
    assert_eq!(
        ada.state,
        DeviceState::Ready,
        "Ada's first device is trusted on first use"
    );
    assert_eq!(bea.state, DeviceState::Ready);
    assert_eq!(cyd.state, DeviceState::Ready);

    // Ada's second device waits for her first to approve it, by its link code.
    let mut ada2 = device(ada2_s, "ada2");
    let DeviceState::AwaitingApproval { code } = ada2.state.clone() else {
        panic!("a second device waits for approval, got {:?}", ada2.state);
    };
    assert!(
        ada.approve("tiny-device-AAAA").is_err(),
        "a code that is not one is refused"
    );
    ada.approve(&code).expect("approved");
    ada2.enrol();
    assert_eq!(
        ada2.state,
        DeviceState::Ready,
        "approved, it registers its key packages"
    );

    // An encrypted direct message, Ada to Bea.
    let (status, direct) = ada
        .ask(
            "POST",
            "/v1/directs",
            Some(json!({ "user_id": bea_s.user_id, "e2ee": true })),
        )
        .unwrap();
    assert!(status == 200 || status == 201, "{direct}");
    let dm = direct
        .get("id")
        .or_else(|| direct.pointer("/direct/id"))
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    assert!(ada.is_encrypted(&dm));
    ada.sync(&dm)
        .expect("Ada starts the group and adds everyone");
    bea.sync(&dm).expect("Bea joins from her Welcome");
    ada2.sync(&dm).expect("Ada's second device joins");
    assert!(cyd.sync(&dm).is_err(), "Cyd is not in this conversation");

    let mut hello = Envelope::message("hello Bea, this is secret");
    hello.sent_at = Some(1);
    let first = id_of(
        &ada.post(&dm, "c-1", Kind::Message, None, &hello, &[])
            .expect("sent"),
    );

    // What the server holds is ciphertext.
    let rows = page(&bea, &dm);
    let row = rows.iter().find(|row| row["id"] == first.as_str()).unwrap();
    assert_eq!(row["body"], "", "the server has no body");
    assert_eq!(row["e2ee"], true);
    let ciphertext = unb64(row["ciphertext"].as_str().unwrap()).unwrap();
    assert!(
        !ciphertext.windows(6).any(|w| w == b"secret"),
        "nor the text in the ciphertext"
    );

    let read = read_page(&mut bea, &dm);
    assert_eq!(text(&read, &first), "hello Bea, this is secret");
    let read = read_page(&mut ada2, &dm);
    assert_eq!(
        text(&read, &first),
        "hello Bea, this is secret",
        "Ada's other device reads it"
    );
    let read = read_page(&mut ada, &dm);
    assert_eq!(
        text(&read, &first),
        "hello Bea, this is secret",
        "the sender keeps what it sent"
    );

    // A reply, and an edit of the first message.
    let reply = id_of(
        &bea.post(
            &dm,
            "c-2",
            Kind::Message,
            None,
            &Envelope::message("hi Ada"),
            &[],
        )
        .expect("Bea answers"),
    );
    assert_eq!(text(&read_page(&mut ada, &dm), &reply), "hi Ada");
    ada.post(
        &dm,
        "c-3",
        Kind::Edit,
        Some(&first),
        &Envelope::message("hello Bea (edited)"),
        &[],
    )
    .expect("edited");
    let read = read_page(&mut bea, &dm);
    assert_eq!(
        text(&read, &first),
        "hello Bea (edited)",
        "the edit is folded into its target"
    );
    assert!(
        matches!(
            read.values()
                .filter(|read| matches!(read, Read::Edit))
                .count(),
            1
        ),
        "the edit is no row of its own"
    );
    // An edit from someone else is not believed.
    assert!(
        bea.post(
            &dm,
            "c-4",
            Kind::Edit,
            Some(&first),
            &Envelope::message("forged"),
            &[]
        )
        .is_err(),
        "the server refuses an edit of another's message"
    );

    // A sealed picture, with EXIF, cleaned and sealed before it goes up.
    let dir = std::env::temp_dir().join(format!("chatd-e2ee-live-{}-files", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let picture = dir.join("photo.jpg");
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(8, 6, image::Rgb([10, 200, 10]))
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let mut dirty = jpeg.into_inner();
    let exif = b"Exif\0\0GPS 51.5007N secret-camera";
    let mut marker = vec![0xFF, 0xE1];
    marker.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    marker.extend_from_slice(exif);
    dirty.splice(2..2, marker);
    std::fs::write(&picture, &dirty).unwrap();
    let sealed = sealing::seal(&picture, "photo.jpg", "image/jpeg", false, &dm).unwrap();
    let blob_id = upload_sealed(&ada, sealed.file.path(), &sealed.entry.id);
    assert_eq!(
        blob_id, sealed.entry.id,
        "the server keeps the id the sender chose"
    );
    let entry = sealed.entry;
    let mut with = Envelope::message("a picture");
    with.attachments = vec![entry];
    let pictured = id_of(
        &ada.post(
            &dm,
            "c-5",
            Kind::Message,
            None,
            &with,
            std::slice::from_ref(&blob_id),
        )
        .expect("sent with a picture"),
    );
    let read = read_page(&mut bea, &dm);
    let Some(Read::Opened(opened)) = read.get(&pictured) else {
        panic!("the picture's message opens");
    };
    assert_eq!(opened.attachments.len(), 1);
    let file = bea.sealed_file(&blob_id).expect("Bea knows how to open it");
    let mut response = ureq::get(&format!("{}/v1/media/{blob_id}", bea_s.base_url))
        .header("Authorization", &format!("Bearer {}", bea_s.token))
        .call()
        .unwrap();
    let blob = response.body_mut().read_to_vec().unwrap();
    assert!(
        !blob.windows(6).any(|w| w == b"GPS 51"),
        "the server holds no metadata"
    );
    let mut plain = Vec::new();
    e2ee::attachment::open_verified(
        &AttachmentKey::from_bytes(file.key),
        &file.context,
        std::io::Cursor::new(&blob),
        &mut plain,
        &file.sha256,
        file.plain_len,
    )
    .expect("it opens");
    assert!(
        !plain.windows(6).any(|w| w == b"GPS 51"),
        "and the opened picture has no EXIF"
    );
    assert!(plain.starts_with(&[0xFF, 0xD8, 0xFF]), "a JPEG");
    image::load_from_memory(&plain).expect("a picture");

    // A third device of Ada's, approved now, reads nothing from before it joined.
    let mut ada3 = device(ada3_s, "ada3");
    let DeviceState::AwaitingApproval { code } = ada3.state.clone() else {
        panic!("waits for approval");
    };
    ada.approve(&code).unwrap();
    ada3.enrol();
    ada.sync(&dm).expect("Ada adds her third device");
    ada3.sync(&dm).expect("it joins");
    let read = read_page(&mut ada3, &dm);
    assert_eq!(
        text(&read, &first),
        protocol::UNDECRYPTABLE,
        "history from before joining stays closed"
    );
    let after = id_of(
        &bea.post(
            &dm,
            "c-6",
            Kind::Message,
            None,
            &Envelope::message("after ada3"),
            &[],
        )
        .expect("sent"),
    );
    assert_eq!(
        text(&read_page(&mut ada3, &dm), &after),
        "after ada3",
        "what comes after opens"
    );

    // Safety numbers agree on both sides.
    let names = HashMap::new();
    let ada_sees = ada.person(&bea_s.user_id, &names).unwrap();
    let bea_sees = bea.person(&ada_s.user_id, &names).unwrap();
    assert_eq!(ada_sees.safety.len(), 60);
    assert_eq!(ada_sees.safety, bea_sees.safety, "one number for both");
    ada.verify(&bea_s.user_id, true).unwrap();
    assert_eq!(
        ada.person_known(&bea_s.user_id, &names).trust,
        Trust::Verified
    );

    // Ada revokes her second device: once the Remove is committed, it reads nothing new.
    let second = ada2.engine.as_ref().unwrap().device_id().to_string();
    ada.revoke(&second).expect("revoked");
    ada.sync(&dm).expect("the Remove is committed");
    let later = id_of(
        &bea.post(
            &dm,
            "c-7",
            Kind::Message,
            None,
            &Envelope::message("not for the revoked one"),
            &[],
        )
        .expect("sent"),
    );
    let _ = ada2.sync(&dm);
    let read = read_page(&mut ada2, &dm);
    assert_ne!(
        text(&read, &later),
        "not for the revoked one",
        "a revoked device cannot open what comes after"
    );
    // Ada changed: the safety number Ada's people verified is not the same any more.
    let ada_now = bea.person(&ada_s.user_id, &names).unwrap();
    assert_ne!(
        ada_now.safety, bea_sees.safety,
        "losing a device changes the number"
    );

    // The recovery key shown once, then checked.
    let key = ada.recovery_key().unwrap();
    ada.check_recovery(&key).expect("it is hers");
    assert!(ada
        .check_recovery("AAAA-AAAA-AAAA-AAAA-AAAA-AAAAAA")
        .is_err());
    let _ = std::fs::remove_dir_all(dir);
}
