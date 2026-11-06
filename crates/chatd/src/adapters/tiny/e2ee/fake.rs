//! An in-process tinyserver for encrypted text: as much of one as a direct message's first
//! messages need (devices, key packages, the group, the handshake feed, the messages), with the
//! real MLS flow on both ends. It keeps the rule of the real server that matters here: Adds wait
//! out a commit batch before the server signs them (`e2ee_commit_batch_secs`, 30 s by default),
//! and they are signed only when someone asks for the conversation or its feed. The batch is
//! counted in those asks here, so a test does not depend on the clock.

use super::*;
use e2ee::identity::DeviceKey;
use openmls::prelude::tls_codec::{DeserializeBytes as _, Serialize as _};
use openmls::prelude::{
    ExternalProposal, GroupEpoch, GroupId, KeyPackageIn, MlsMessageOut, ProtocolVersion,
    SenderExtensionIndex,
};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;

const CONVERSATION: &str = "c0c0c0c0-c0c0-4c0c-8c0c-c0c0c0c0c0c0";
const ADA: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const BEA: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

struct Add {
    user: String,
    device: String,
    package: Vec<u8>,
    issued: bool,
}

struct Handshake {
    seq: i64,
    kind: &'static str,
    epoch: u64,
    data: Vec<u8>,
    for_device: Option<String>,
}

struct Server {
    key: DeviceKey,
    /// Asks that find unsigned Adds and leave them so: the commit batch not yet waited out.
    hold: usize,
    /// Asks for the conversation that fail, as a dropped connection does.
    fail: usize,
    devices: BTreeMap<String, Vec<Value>>,
    packages: HashMap<String, Vec<Vec<u8>>>,
    epoch: Option<u64>,
    leaves: Vec<(String, String)>,
    joined: HashMap<String, u64>,
    pending: Vec<Add>,
    feed: Vec<Handshake>,
    messages: Vec<Value>,
}

struct Fake {
    server: Arc<Mutex<Server>>,
    base: String,
}

impl Fake {
    fn start(hold: usize) -> Fake {
        let server = Arc::new(Mutex::new(Server {
            key: DeviceKey::generate().unwrap(),
            hold,
            fail: 0,
            devices: BTreeMap::new(),
            packages: HashMap::new(),
            epoch: None,
            leaves: Vec::new(),
            joined: HashMap::new(),
            pending: Vec::new(),
            feed: Vec::new(),
            messages: Vec::new(),
        }));
        let serving = server.clone();
        let base = crate::adapters::tiny::serve(move |request| {
            let (status, body) = lock(&serving).answer(request);
            crate::adapters::tiny::http(status, "-", &body.to_string())
        });
        Fake { server, base }
    }

    /// The conversation's rows, as the read takes them.
    fn read(&self, device: &mut Device) -> HashMap<String, Read> {
        let messages = lock(&self.server).messages.clone();
        let rows: Vec<Row> = messages
            .iter()
            .map(|row| Row {
                id: row["id"].as_str().unwrap(),
                author_id: row["author_id"].as_str().unwrap(),
                kind: "message",
                ciphertext: row["ciphertext"].as_str(),
                epoch: row["epoch"].as_u64().unwrap(),
                device_id: row["device_id"].as_str().unwrap(),
                target_id: None,
                client_id: row["client_id"].as_str(),
            })
            .collect();
        device.read(CONVERSATION, &rows)
    }
}

impl Server {
    fn answer(&mut self, request: &str) -> (u16, Value) {
        let (head, body) = request.split_once("\r\n\r\n").unwrap_or((request, ""));
        let mut line = head.lines().next().unwrap_or("").split(' ');
        let (method, target) = (line.next().unwrap_or(""), line.next().unwrap_or(""));
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let param = |name: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                .unwrap_or("")
                .to_owned()
        };
        let caller = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                let token = value.trim().strip_prefix("Bearer ")?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| token.to_owned())
            })
            .unwrap_or_default();
        let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let conversation = format!("/v1/e2ee/conversations/{CONVERSATION}");
        match method {
            "GET" if path == "/v1/capabilities" => (
                200,
                json!({ "features": ["e2ee_text"], "e2ee_text": "optional" }),
            ),
            "GET" if path.starts_with("/v1/e2ee/users/") => {
                let user = path
                    .trim_start_matches("/v1/e2ee/users/")
                    .trim_end_matches("/devices");
                let rows = self.devices.get(user).cloned().unwrap_or_default();
                (200, Value::Array(rows))
            }
            "PUT" if path.starts_with("/v1/e2ee/devices/") => {
                let id = path.trim_start_matches("/v1/e2ee/devices/").to_owned();
                let rows = self.devices.entry(caller).or_default();
                if !rows.is_empty() {
                    return (400, json!({ "error": "cross_signature_required" }));
                }
                rows.push(json!({ "device_id": id, "signature_key": body["signature_key"], "cross_signature": null, "created_at": "2026-11-05T00:00:00Z" }));
                (201, json!({}))
            }
            "GET" if path == "/v1/e2ee/key_packages/count" => {
                let remaining = self.packages.get(&param("device_id")).map_or(0, Vec::len);
                (200, json!({ "remaining": remaining }))
            }
            "POST" if path == "/v1/e2ee/key_packages" => {
                let device = body["device_id"].as_str().unwrap_or("").to_owned();
                let packages = body["key_packages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|package| unb64(package.as_str()?));
                self.packages.entry(device).or_default().extend(packages);
                (200, json!({}))
            }
            "GET" if path == conversation => {
                if self.fail > 0 {
                    self.fail -= 1;
                    return (503, json!({ "error": "unavailable" }));
                }
                self.issue();
                (200, self.info())
            }
            "POST" if path == format!("{conversation}/group") => {
                if self.epoch.is_some() {
                    return (409, json!({ "error": "group_exists" }));
                }
                let device = body["device_id"].as_str().unwrap_or("").to_owned();
                self.epoch = Some(0);
                self.leaves.push((caller, device.clone()));
                self.joined.insert(device, 0);
                self.issue();
                (201, json!({ "epoch": 0 }))
            }
            "GET" if path == format!("{conversation}/handshake") => {
                self.issue();
                let after: i64 = param("after").parse().unwrap_or(0);
                let device = param("device_id");
                let joined = self.joined.get(&device).copied();
                let rows: Vec<Value> = self
                    .feed
                    .iter()
                    .filter(|row| row.seq > after)
                    .filter(|row| match row.kind {
                        "welcome" => row.for_device.as_deref() == Some(device.as_str()),
                        _ => joined.is_some_and(|joined| row.epoch >= joined),
                    })
                    .map(|row| json!({ "seq": row.seq, "kind": row.kind, "epoch": row.epoch, "data": b64(&row.data) }))
                    .collect();
                (200, Value::Array(rows))
            }
            "POST" if path == format!("{conversation}/commit") => self.commit(&body),
            "POST" if path == format!("/v1/conversations/{CONVERSATION}/messages") => {
                self.post(&caller, &body)
            }
            _ => (404, json!({ "error": "not_found" })),
        }
    }

    fn push(&mut self, kind: &'static str, epoch: u64, data: Vec<u8>, for_device: Option<String>) {
        let seq = self.feed.len() as i64 + 1;
        self.feed.push(Handshake {
            seq,
            kind,
            epoch,
            data,
            for_device,
        });
    }

    /// Every device of the two people that is neither in the group nor waiting gets an Add, with
    /// one of its key packages.
    fn reconcile(&mut self) {
        if self.epoch.is_none() {
            return;
        }
        for user in [ADA, BEA] {
            for row in self.devices.get(user).cloned().unwrap_or_default() {
                let device = row["device_id"].as_str().unwrap().to_owned();
                if self.leaves.iter().any(|(_, leaf)| *leaf == device)
                    || self.pending.iter().any(|add| add.device == device)
                {
                    continue;
                }
                let Some(package) = self
                    .packages
                    .get_mut(&device)
                    .filter(|all| !all.is_empty())
                    .map(|all| all.remove(0))
                else {
                    continue;
                };
                self.pending.push(Add {
                    user: user.to_owned(),
                    device,
                    package,
                    issued: false,
                });
            }
        }
    }

    /// Signs the waiting Adds into one proposals message, once the batch is waited out.
    fn issue(&mut self) {
        self.reconcile();
        if !self.pending.iter().any(|add| !add.issued) {
            return;
        }
        if self.hold > 0 {
            self.hold -= 1;
            return;
        }
        let epoch = self.epoch.unwrap();
        let crypto = OpenMlsRustCrypto::default();
        let group = GroupId::from_slice(&uuid(CONVERSATION).unwrap());
        let mut batch: Vec<MlsMessageOut> = Vec::new();
        for add in self.pending.iter_mut().filter(|add| !add.issued) {
            let package = KeyPackageIn::tls_deserialize_exact_bytes(&add.package)
                .unwrap()
                .validate(crypto.crypto(), ProtocolVersion::Mls10)
                .unwrap();
            batch.push(
                ExternalProposal::new_add::<OpenMlsRustCrypto>(
                    package,
                    group.clone(),
                    GroupEpoch::from(epoch),
                    &self.key,
                    SenderExtensionIndex::new(0),
                )
                .unwrap(),
            );
            add.issued = true;
        }
        let data = batch.tls_serialize_detached().unwrap();
        self.push("proposals", epoch, data, None);
    }

    fn info(&self) -> Value {
        let members: Vec<Value> = self
            .leaves
            .iter()
            .enumerate()
            .map(|(leaf, (user, device))| json!({ "user_id": user, "device_id": device, "leaf_index": leaf }))
            .collect();
        let pending: Vec<Value> = self
            .pending
            .iter()
            .map(|add| json!({ "op": "add", "user_id": add.user, "device_id": add.device, "issued": add.issued }))
            .collect();
        json!({
            "e2ee": true,
            "epoch": self.epoch,
            "external_sender": b64(&self.key.public_key()),
            "members": members,
            "pending": pending,
        })
    }

    fn commit(&mut self, body: &Value) -> (u16, Value) {
        let Some(epoch) = self
            .epoch
            .filter(|epoch| body["epoch"].as_u64() == Some(*epoch))
        else {
            return (409, json!({ "error": "stale_epoch" }));
        };
        if !self.pending.iter().any(|add| add.issued) {
            return (400, json!({ "error": "no_proposals" }));
        }
        let commit = unb64(body["commit"].as_str().unwrap_or("")).unwrap();
        self.push("commit", epoch, commit, None);
        for welcome in body["welcomes"].as_array().into_iter().flatten() {
            let data = unb64(welcome["data"].as_str().unwrap()).unwrap();
            let device = welcome["device_id"].as_str().unwrap().to_owned();
            self.push("welcome", epoch + 1, data, Some(device));
        }
        let (added, waiting): (Vec<Add>, Vec<Add>) =
            self.pending.drain(..).partition(|add| add.issued);
        self.pending = waiting;
        for add in added {
            self.joined.insert(add.device.clone(), epoch + 1);
            self.leaves.push((add.user, add.device));
        }
        self.epoch = Some(epoch + 1);
        (200, json!({}))
    }

    fn post(&mut self, caller: &str, body: &Value) -> (u16, Value) {
        let device = body["device_id"].as_str().unwrap_or("").to_owned();
        if body["epoch"].as_u64() != self.epoch {
            return (409, json!({ "error": "stale_epoch" }));
        }
        if !self.leaves.iter().any(|(_, leaf)| *leaf == device) {
            return (403, json!({ "error": "not_in_group" }));
        }
        let id = format!("00000000-0000-4000-8000-{:012}", self.messages.len() + 1);
        self.messages.push(json!({ "id": id, "author_id": caller, "ciphertext": body["ciphertext"], "epoch": body["epoch"], "device_id": device, "client_id": body["client_id"] }));
        (201, json!({ "id": id }))
    }
}

/// A device of `user` against the fake, in a directory of its own, with keys of its own.
fn device(fake: &Fake, user: &str, name: &str) -> Device {
    let saved = Saved {
        base_url: fake.base.clone(),
        token: user.to_owned(),
        user_id: user.to_owned(),
        email: None,
        display_name: name.to_owned(),
        role: "member".to_owned(),
        plain_http_ok: false,
        e2ee: Some(Secrets::generate().unwrap()),
    };
    let mut device = Device::new(&saved);
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    device.dir = std::env::temp_dir().join(format!(
        "chatd-e2ee-fake-{}-{name}-{made}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&device.dir);
    device.enrol();
    assert_eq!(device.state, DeviceState::Ready, "{name} is a first device");
    device
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
    answer["id"].as_str().unwrap().to_owned()
}

/// Lab prove: Ada's first message opens for Bea when the send waits out the commit batch.
pub(crate) fn prove_first_encrypted_message() -> Result<(), String> {
    let fake = Fake::start(3);
    let ada: Shared = Arc::new(Mutex::new(device(&fake, ADA, "ada")));
    let mut bea = device(&fake, BEA, "bea");
    let hello = Envelope::message("hello Bea");
    let answer = send(&ada, CONVERSATION, "c-1", Kind::Message, None, &hello, &[])
        .map_err(|err| format!("Ada's first message: {err}"))?;
    let first = id_of(&answer);
    lock(&fake.server).hold = 0;
    lock(&ada)
        .sync(CONVERSATION)
        .map_err(|err| format!("Ada commits the Adds: {err}"))?;
    let read = fake.read(&mut bea);
    let opened = text(&read, &first);
    if opened != "hello Bea" {
        return Err(format!("Bea saw {opened:?}, not hello Bea"));
    }
    let reply = Envelope::message("hi Ada");
    let answer = bea
        .post(CONVERSATION, "c-2", Kind::Message, None, &reply, &[])
        .map_err(|err| format!("Bea answers: {err}"))?;
    let read = fake.read(&mut lock(&ada));
    if text(&read, &id_of(&answer)) != "hi Ada" {
        return Err("Ada did not open Bea's reply".into());
    }
    if text(&read, &first) != "hello Bea" {
        return Err("Ada lost her own first message".into());
    }
    Ok(())
}

/// The server signs the Adds of a new conversation only after its commit batch: the first
/// message waits for the other person to be in the group, rather than go to a group of one.
#[test]
fn the_first_message_of_a_new_conversation_opens_for_the_other_person() {
    prove_first_encrypted_message().expect("first message opens for the other person");
}

/// A row that did not open because this device was not in the group yet (here, the sync of the
/// read failed) is read again when the group arrives, not only when another message does.
#[test]
fn a_message_that_did_not_open_for_want_of_the_group_is_read_again_when_it_arrives() {
    let fake = Fake::start(0);
    let ada: Shared = Arc::new(Mutex::new(device(&fake, ADA, "ada")));
    let mut bea = device(&fake, BEA, "bea");
    let hello = Envelope::message("hello Bea");
    let first = id_of(
        &send(&ada, CONVERSATION, "c-1", Kind::Message, None, &hello, &[])
            .expect("Ada's first message goes"),
    );
    lock(&fake.server).fail = 1;
    let read = fake.read(&mut bea);
    assert_eq!(text(&read, &first), protocol::UNDECRYPTABLE);
    let reread = bea.heard(
        "e2ee.handshake",
        &json!({ "conversation_id": CONVERSATION }),
    );
    assert_eq!(
        reread.as_deref(),
        Some(CONVERSATION),
        "the handshake that brings the group asks for the rows again"
    );
    let read = fake.read(&mut bea);
    assert_eq!(text(&read, &first), "hello Bea");
    let again = bea.heard(
        "e2ee.handshake",
        &json!({ "conversation_id": CONVERSATION }),
    );
    assert_eq!(again, None, "and only once: everything opened");
}
