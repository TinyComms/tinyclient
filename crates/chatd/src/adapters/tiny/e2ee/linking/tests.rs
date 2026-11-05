//! Both halves of a device link, run against an in-process relay that plays the server: it relays
//! what it is given, and in some tests changes it on the way.

use super::super::Cache;
use super::*;
use crate::adapters::tiny::Saved;
use e2ee::store::{FileStore, StateKey};
use e2ee::{Engine, UserId};
use protocol::DeviceState;
mod approval_tests;
use std::collections::BTreeMap;

const USER: &str = "7e57ada0-0000-4000-8000-000000000001";

/// What the server holds of one link.
#[derive(Default, Clone)]
struct Held {
    messages: Vec<(i64, String, Vec<u8>)>,
    archive: Option<Vec<u8>>,
}

type Registered = (String, Vec<u8>, Vec<u8>);

type Rewrite = Box<dyn FnMut(&str, Vec<u8>) -> Vec<u8> + Send>;

#[derive(Default)]
struct Fake {
    links: Mutex<BTreeMap<String, Held>>,
    moved: Condvar,
    /// Each registration: the device, its key, the cross-signature.
    registered: Mutex<Vec<Registered>>,
    published: Mutex<BTreeSet<String>>,
    deleted: Mutex<Vec<String>>,
    members: BTreeSet<String>,
    /// The server in the middle: rewrites what it relays, by sender.
    rewrite: Mutex<Option<Rewrite>>,
    /// Flips a byte of the archive it serves.
    bent_archive: bool,
}

impl Relay for Fake {
    fn publish_keys(
        &self,
        device_id: &str,
        packages: &[Vec<u8>],
        last_resort: &[u8],
    ) -> Result<(), Failed> {
        assert_eq!(packages.len(), super::super::KEY_PACKAGES);
        assert!(packages.iter().all(|key| !key.is_empty()) && !last_resort.is_empty());
        assert!(lock(&self.registered)
            .iter()
            .any(|(id, _, _)| id == device_id));
        lock(&self.published).insert(device_id.to_owned());
        Ok(())
    }
    fn create(&self) -> Result<(String, String), Failed> {
        let id = link::LinkCode::parse(&Pending::create([0; 16]).unwrap().code().encode())
            .unwrap()
            .link_id;
        let mut random = id;
        getrandom::fill(&mut random).unwrap();
        let text =
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, random);
        lock(&self.links).insert(text.clone(), Held::default());
        Ok((text, "2026-11-05T12:05:00Z".to_owned()))
    }

    fn post(&self, link: &str, from: Role, data: &[u8]) -> Result<(), Failed> {
        let data = match lock(&self.rewrite).as_mut() {
            Some(rewrite) => rewrite(from.as_str(), data.to_vec()),
            None => data.to_vec(),
        };
        let mut links = lock(&self.links);
        let held = links.get_mut(link).ok_or(Failed::Gone)?;
        let seq = held.messages.len() as i64 + 1;
        held.messages.push((seq, from.as_str().to_owned(), data));
        self.moved.notify_all();
        Ok(())
    }

    fn read(
        &self,
        link: &str,
        after: i64,
        wait: u64,
    ) -> Result<Vec<(i64, String, Vec<u8>)>, Failed> {
        let deadline = Instant::now() + Duration::from_millis(wait.min(1) * 200);
        let mut links = lock(&self.links);
        loop {
            let held = links.get(link).ok_or(Failed::Gone)?;
            let after: Vec<_> = held
                .messages
                .iter()
                .filter(|(seq, _, _)| *seq > after)
                .cloned()
                .collect();
            let left = deadline.saturating_duration_since(Instant::now());
            if !after.is_empty() || left.is_zero() {
                return Ok(after);
            }
            links = self.moved.wait_timeout(links, left).unwrap().0;
        }
    }

    fn put_archive(&self, link: &str, path: &Path, bytes: u64) -> Result<(), Failed> {
        let blob = std::fs::read(path).unwrap();
        assert_eq!(blob.len() as u64, bytes);
        let mut links = lock(&self.links);
        let held = links.get_mut(link).ok_or(Failed::Gone)?;
        held.archive = Some(blob);
        Ok(())
    }

    fn get_archive(
        &self,
        link: &str,
        to: &Path,
        max: u64,
        moved: &dyn Fn(u64),
    ) -> Result<u64, Failed> {
        let mut blob = lock(&self.links)
            .get(link)
            .and_then(|held| held.archive.clone())
            .ok_or(Failed::Gone)?;
        if self.bent_archive {
            let at = blob.len() / 2;
            blob[at] ^= 1;
        }
        blob.truncate(max as usize + 1);
        std::fs::write(to, &blob).unwrap();
        moved(blob.len() as u64);
        Ok(blob.len() as u64)
    }

    fn delete(&self, link: &str) {
        lock(&self.links).remove(link);
        lock(&self.deleted).push(link.to_owned());
        self.moved.notify_all();
    }

    fn register(
        &self,
        device_id: &str,
        signature_key: &[u8],
        cross_signature: &[u8],
    ) -> Result<(), Failed> {
        lock(&self.registered).push((
            device_id.to_owned(),
            signature_key.to_vec(),
            cross_signature.to_vec(),
        ));
        Ok(())
    }

    fn conversations(&self) -> Result<BTreeSet<String>, Failed> {
        Ok(self.members.clone())
    }
}

fn saved() -> Saved {
    Saved {
        // Nothing listens there: what a device asks the server outside the relay fails at once.
        base_url: "http://127.0.0.1:9".into(),
        token: "t".into(),
        user_id: USER.into(),
        email: None,
        display_name: "Ada".into(),
        role: "member".into(),
        plain_http_ok: false,
        e2ee: None,
    }
}

/// A device of the person in a folder of its own.
fn device(name: &str, state: DeviceState) -> Shared {
    let mut device = Device::new(&saved());
    device.dir =
        std::env::temp_dir().join(format!("chatd-e2ee-link-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&device.dir);
    private_dir(&device.dir).unwrap();
    let store = Box::new(FileStore::new(&device.dir));
    let user = UserId::parse(USER).unwrap();
    device.engine = Some(
        Engine::create(
            user,
            store,
            StateKey::generate().unwrap(),
            super::super::STATE,
        )
        .unwrap(),
    );
    device.cache_key = Some(CacheKey::generate().unwrap());
    device.state = state;
    device.linking = true;
    Arc::new(Mutex::new(device))
}

fn opened(text: &str, days_ago: i64) -> Opened {
    Opened {
        text: text.into(),
        at: now_secs() - days_ago * 86_400,
        ..Opened::default()
    }
}

#[test]
fn a_semantically_invalid_archive_imports_no_earlier_messages() {
    let shared = device("invalid-archive-row", DeviceState::Ready);
    let key = ArchiveKey::generate().unwrap();
    let meta = Meta {
        v: archive::VERSION,
        user: USER.into(),
        from_device: "old".into(),
        created: "now".into(),
        scope: Scope::All,
    };
    let mut writer = Writer::new(&key, b"semantic-check", &meta, Vec::new()).unwrap();
    writer
        .message("conversation", "first", &json!({"text": "valid"}))
        .unwrap();
    writer
        .message("conversation", "second", &json!({"text": 123}))
        .unwrap();
    let (blob, _) = writer.finish().unwrap();
    let mut device = lock(&shared);
    let mut target = Merging {
        device: &mut device,
        members: BTreeSet::from(["conversation".to_owned()]),
        from: String::new(),
        touched: BTreeSet::new(),
    };
    let result = archive::merge(
        &key,
        b"semantic-check",
        USER,
        || Ok(blob.as_slice()),
        &mut target,
    );
    assert!(result.is_err());
    assert!(
        device.cache("conversation").unwrap().rows.is_empty(),
        "a rejected archive must not persist earlier valid rows"
    );
}

#[test]
fn export_keeps_the_latest_valid_cache_record_and_ignores_a_torn_tail() {
    let shared = device("export-cache-versions", DeviceState::Ready);
    let (dir, cache_key) = {
        let device = lock(&shared);
        (device.dir.clone(), device.cache_key.clone().unwrap())
    };
    let mut cache = Cache::load(&dir, "conversation", &cache_key);
    cache.put(&cache_key, "one", opened("old", 2));
    cache.put(&cache_key, "one", opened("updated", 0));
    cache.put(&cache_key, "two", opened("other", 1));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&cache.path)
        .unwrap()
        .write_all(b"one this-is-not-a-valid-sealed-record\n")
        .unwrap();
    let key = ArchiveKey::generate().unwrap();
    let meta = Meta {
        v: archive::VERSION,
        user: USER.into(),
        from_device: "old".into(),
        created: "now".into(),
        scope: Scope::All,
    };
    let (blob, summary, omitted) = pack(
        Vec::new(),
        &key,
        b"cache-versions",
        &dir,
        &cache_key,
        &BTreeMap::new(),
        u64::MAX,
        &meta,
    )
    .unwrap();
    assert_eq!((summary.msgs, omitted), (2, 0));
    let mut messages = Vec::new();
    archive::read(&key, b"cache-versions", blob.as_slice(), |record| {
        if let archive::Record::Msg { id, opened, .. } = record {
            messages.push((id, opened["text"].as_str().unwrap().to_owned()));
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        messages,
        [
            ("one".to_owned(), "updated".to_owned()),
            ("two".to_owned(), "other".to_owned())
        ]
    );
}

#[cfg(windows)]
#[test]
#[ignore = "isolated export memory budget; run with --ignored --test-threads=1"]
fn exporting_many_conversations_does_not_hold_all_plaintext_in_memory() {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    fn peak() -> usize {
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        assert_ne!(
            unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) },
            0
        );
        counters.PeakWorkingSetSize
    }
    let shared = device("archive-memory-budget", DeviceState::Ready);
    let (dir, cache_key) = {
        let device = lock(&shared);
        (device.dir.clone(), device.cache_key.clone().unwrap())
    };
    for conv in 0..128 {
        let mut cache = Cache::load(&dir, &format!("memory-{conv}"), &cache_key);
        for row in 0..64 {
            cache.put(
                &cache_key,
                &format!("row-{row}"),
                opened(&"x".repeat(8192), 0),
            );
        }
    }
    let before = peak();
    assert_eq!(size_of(&dir, &cache_key, Scope::All, now_secs()).0, 8192);
    let key = ArchiveKey::generate().unwrap();
    let meta = Meta {
        v: archive::VERSION,
        user: USER.into(),
        from_device: "old".into(),
        created: "now".into(),
        scope: Scope::All,
    };
    let (_, summary, omitted) = pack(
        std::io::sink(),
        &key,
        b"memory-budget",
        &dir,
        &cache_key,
        &BTreeMap::new(),
        u64::MAX,
        &meta,
    )
    .unwrap();
    assert_eq!((summary.msgs, omitted), (8192, 0));
    let growth = peak().saturating_sub(before);
    assert!(
        growth < 24 * 1024 * 1024,
        "export retained too much plaintext: peak working set grew by {growth} bytes"
    );
}

fn quiet() -> Told {
    Arc::new(|| {})
}

/// These tests exercise link cryptography and relay behavior, with owner authentication
/// supplied per job. Native UI is exercised separately and is never opened by unit tests.
fn owner_approved_job() -> Linking {
    let mut job = Linking::new(Role::Old, quiet());
    job.approval =
        Arc::new(|_: &str, _: &dyn Fn() -> bool| Ok(protocol::device_approval::Outcome::Verified));
    job
}

/// Waits until `job` shows `stage`, or fails the test.
fn reaches(job: &Linking, stage: LinkStage) -> E2eeLink {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let view = lock(&job.state).view.clone();
        if view.stage == stage {
            return view;
        }
        assert!(
            Instant::now() < deadline && view.stage != LinkStage::Failed,
            "never reached {stage:?}: {view:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn say_yes(job: &Linking, scope: Scope) {
    lock(&job.state).confirmed = Some(scope);
    job.wake.notify_all();
}

struct Run {
    new_result: Result<u64, String>,
    old_result: Result<(), String>,
    new: Shared,
    old: Shared,
}

/// A whole link from a device that read `rows` to a new one, the person choosing `scope`, through
/// `relay`. `between` runs once both show their digits, before the person says yes.
fn link_through(
    relay: Arc<Fake>,
    rows: &[(&str, &str, Opened)],
    scope: Scope,
    between: impl FnOnce(&E2eeLink, &E2eeLink),
) -> Run {
    link_through_with(relay, rows, scope, None, between)
}

fn link_through_with(
    relay: Arc<Fake>,
    rows: &[(&str, &str, Opened)],
    scope: Scope,
    commands: Option<std::sync::mpsc::Sender<crate::adapters::Command>>,
    between: impl FnOnce(&E2eeLink, &E2eeLink),
) -> Run {
    let tag = format!("{:?}{}", scope, rows.len());
    let new = device(
        &format!("new-{tag}"),
        DeviceState::AwaitingApproval {
            code: String::new(),
        },
    );
    let old = device(&format!("old-{tag}"), DeviceState::Ready);
    lock(&new).commands = commands;
    {
        let mut old = lock(&old);
        for (conv, id, opened) in rows {
            old.remember(conv, id, opened.clone());
        }
        old.notes.verified.insert(
            "b0b00000-0000-4000-8000-000000000002".into(),
            "12345".into(),
        );
    }
    let new_job = Arc::new(Linking::new(Role::New, quiet()));
    let old_job = Arc::new(owner_approved_job());
    let new_thread = {
        let (job, device, relay) = (new_job.clone(), new.clone(), relay.clone());
        std::thread::spawn(move || run_new(&job, &device, relay.as_ref()))
    };
    let code = loop {
        let view = lock(&new_job.state).view.clone();
        if !view.code.is_empty() {
            break LinkCode::parse(&view.code).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let old_thread = {
        let (job, device, relay) = (old_job.clone(), old.clone(), relay.clone());
        std::thread::spawn(move || run_old(&job, &device, relay.as_ref(), &code))
    };
    let at_old = reaches(&old_job, LinkStage::Confirm);
    let at_new = reaches(&new_job, LinkStage::Confirm);
    between(&at_new, &at_old);
    say_yes(&old_job, scope);
    Run {
        new_result: new_thread.join().unwrap(),
        old_result: old_thread.join().unwrap(),
        new,
        old,
    }
}

#[test]
fn a_new_device_is_signed_and_gets_the_last_month_of_what_it_may_read() {
    let relay = Arc::new(Fake {
        members: ["c1".to_owned(), "c2".to_owned()].into(),
        ..Fake::default()
    });
    let rows = [
        ("c1", "m1", opened("this week", 2)),
        ("c1", "m2", opened("last year", 400)),
        ("c2", "m3", opened("yesterday", 1)),
        ("c9", "m4", opened("not theirs", 1)),
    ];
    let run = link_through(relay.clone(), &rows, Scope::Days(30), |new, old| {
        assert_eq!(new.check, old.check, "both screens show one number");
        assert_eq!(old.check.len(), 6);
        assert!(!old.label.is_empty());
        assert_eq!(old.history.len(), 4);
        let all = old
            .history
            .iter()
            .find(|choice| choice.history == "all")
            .unwrap();
        let month = old
            .history
            .iter()
            .find(|choice| choice.history == "days:30")
            .unwrap();
        assert_eq!((all.messages, month.messages), (4, 3));
        assert!(all.bytes > month.bytes);
    });
    assert_eq!(run.new_result, Ok(2), "{:?}", run.old_result);
    assert_eq!(run.old_result, Ok(()));

    // The new device registered with the old one's signature over its own key.
    let registered = lock(&relay.registered).clone();
    assert_eq!(registered.len(), 1);
    let (id, key, signature) = &registered[0];
    let (new_id, new_key) = {
        let new = lock(&run.new);
        let engine = new.engine.as_ref().unwrap();
        (engine.device_id(), engine.signature_key())
    };
    assert_eq!(
        (id.as_str(), key.as_slice()),
        (new_id.to_string().as_str(), new_key.as_slice())
    );
    let old_key = lock(&run.old).engine.as_ref().unwrap().signature_key();
    e2ee::identity::verify_cross_signature(
        &old_key,
        UserId::parse(USER).unwrap(),
        new_id,
        key,
        signature,
    )
    .unwrap();

    // What the person may read from the last 30 days, and nothing else.
    let mut new = lock(&run.new);
    let text = |device: &mut Device, conv: &str, id: &str| {
        device
            .cache(conv)
            .and_then(|cache| cache.rows.get(id).cloned())
    };
    assert_eq!(
        text(&mut new, "c1", "m1").map(|o| o.text),
        Some("this week".to_owned())
    );
    assert_eq!(
        text(&mut new, "c2", "m3").map(|o| o.text),
        Some("yesterday".to_owned())
    );
    assert!(
        text(&mut new, "c1", "m2").is_none(),
        "older than the month chosen"
    );
    assert!(
        text(&mut new, "c9", "m4").is_none(),
        "a conversation the person is not in"
    );
    let old_id = lock(&run.old)
        .engine
        .as_ref()
        .unwrap()
        .device_id()
        .to_string();
    assert_eq!(
        text(&mut new, "c1", "m1").and_then(|o| o.imported_from),
        Some(old_id)
    );
    assert_eq!(
        new.notes
            .verified
            .get("b0b00000-0000-4000-8000-000000000002")
            .map(String::as_str),
        Some("12345")
    );
    // The archive is gone from both folders, and the link from the relay.
    assert!(!new.dir.join(ARCHIVE_FILE).exists());
    assert!(!lock(&run.old).dir.join(ARCHIVE_FILE).exists());
    assert!(lock(&relay.links).is_empty());
    // The old device knows the new one is the person's doing: no notice there.
    assert!(lock(&run.old).notes.mine.contains(&new_id.to_string()));
}

#[test]
fn no_history_sends_no_archive() {
    let relay = Arc::new(Fake::default());
    let run = link_through(
        relay.clone(),
        &[("c1", "m1", opened("x", 1))],
        Scope::None,
        |_, _| {},
    );
    assert_eq!(run.new_result, Ok(0));
    assert_eq!(lock(&relay.registered).len(), 1);
    let id = lock(&run.new)
        .engine
        .as_ref()
        .unwrap()
        .device_id()
        .to_string();
    assert!(
        lock(&relay.published).contains(&id),
        "link completion must wait until the new device's public key packages are available"
    );
}

#[test]
fn a_server_that_puts_its_own_key_in_the_middle_gets_nothing_and_both_stop() {
    let fake = Fake {
        rewrite: Mutex::new(Some(Box::new(|from: &str, data: Vec<u8>| {
            // The old device's first message is its key, in the clear: replace it with another.
            if from == "old" && data.len() == link::PUBLIC_LEN {
                let pending = Pending::create([0; 16]).unwrap();
                let (theirs, _) = link::join(pending.code()).unwrap();
                theirs.to_vec()
            } else {
                data
            }
        }))),
        members: ["c1".to_owned()].into(),
        ..Fake::default()
    };
    let relay = Arc::new(fake);
    let new = device(
        "mitm-new",
        DeviceState::AwaitingApproval {
            code: String::new(),
        },
    );
    let old = device("mitm-old", DeviceState::Ready);
    lock(&old).remember("c1", "m1", opened("secret", 1));
    let new_job = Arc::new(Linking::new(Role::New, quiet()));
    let old_job = Arc::new(owner_approved_job());
    let new_thread = {
        let (job, device, relay) = (new_job.clone(), new.clone(), relay.clone());
        std::thread::spawn(move || run_new(&job, &device, relay.as_ref()))
    };
    let code = loop {
        let code = lock(&new_job.state).view.code.clone();
        if !code.is_empty() {
            break LinkCode::parse(&code).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let old_result = run_old(&old_job, &old, relay.as_ref(), &code);
    assert!(
        old_result.is_err(),
        "the new device's hello does not open for the old one"
    );
    let new_result = new_thread.join().unwrap();
    assert!(new_result.is_err());
    assert!(lock(&relay.registered).is_empty(), "nothing was signed");
    assert!(lock(&relay.links).is_empty(), "and the link is gone");
}

#[test]
fn an_archive_changed_on_the_server_is_not_taken_in() {
    let fake = Fake {
        bent_archive: true,
        members: ["c1".to_owned()].into(),
        ..Fake::default()
    };
    let relay = Arc::new(fake);
    let run = link_through(
        relay.clone(),
        &[("c1", "m1", opened("x", 1))],
        Scope::All,
        |_, _| {},
    );
    assert!(
        run.new_result
            .as_ref()
            .unwrap_err()
            .contains("couldn't be checked"),
        "{:?}",
        run.new_result
    );
    assert!(
        run.old_result.is_err(),
        "the old device hears the link failed"
    );
    let mut new = lock(&run.new);
    assert!(new.cache("c1").is_none_or(|cache| cache.rows.is_empty()));
    assert!(!new.dir.join(ARCHIVE_FILE).exists());
    assert!(
        lock(&relay.registered).is_empty(),
        "invalid history must be refused before registering the new device"
    );
}

#[test]
fn wrong_digits_stop_the_link_on_both_devices() {
    let relay = Arc::new(Fake::default());
    let mut saved = saved();
    saved.base_url = "http://127.0.0.1:9/wrong-digits".into();
    let new = device(
        "digits-new",
        DeviceState::AwaitingApproval {
            code: String::new(),
        },
    );
    let old = device("digits-old", DeviceState::Ready);
    let new_job = Arc::new(Linking::new(Role::New, quiet()));
    let old_job = begin(&saved, Role::Old, quiet()).unwrap();
    let new_thread = {
        let (job, device, relay) = (new_job.clone(), new.clone(), relay.clone());
        std::thread::spawn(move || run_new(&job, &device, relay.as_ref()))
    };
    let code = loop {
        let code = lock(&new_job.state).view.code.clone();
        if !code.is_empty() {
            break LinkCode::parse(&code).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let old_thread = {
        let (job, device, relay) = (old_job.clone(), old.clone(), relay.clone());
        std::thread::spawn(move || run_old(&job, &device, relay.as_ref(), &code))
    };
    let view = reaches(&old_job, LinkStage::Confirm);
    let wrong = if view.check == "000000" {
        "000001"
    } else {
        "000000"
    };
    assert!(confirm(&saved, "all", wrong).is_err());
    assert!(old_thread.join().unwrap().is_err());
    let new_result = new_thread.join().unwrap();
    assert!(
        new_result.unwrap_err().contains("stopped"),
        "the new device hears why"
    );
    assert!(lock(&relay.registered).is_empty());
    // Typing the right digits after that does nothing: the link is over.
    assert!(confirm(&saved, "all", &view.check).is_err());
}

#[test]
fn a_new_device_that_cancels_while_waiting_deletes_the_link() {
    let relay = Arc::new(Fake::default());
    let new = device(
        "cancel-new",
        DeviceState::AwaitingApproval {
            code: String::new(),
        },
    );
    let job = Arc::new(Linking::new(Role::New, quiet()));
    let thread = {
        let (job, device, relay) = (job.clone(), new.clone(), relay.clone());
        std::thread::spawn(move || run_new(&job, &device, relay.as_ref()))
    };
    reaches(&job, LinkStage::Waiting);
    while lock(&job.state).view.code.is_empty() {
        std::thread::sleep(Duration::from_millis(5));
    }
    lock(&job.state).stop = true;
    assert_eq!(thread.join().unwrap(), Err(STOPPED.to_owned()));
    assert!(lock(&relay.links).is_empty());
    assert_eq!(lock(&relay.deleted).len(), 1);
}

#[test]
fn a_server_without_linking_is_told_in_words_and_nothing_starts() {
    let new = device(
        "no-linking",
        DeviceState::AwaitingApproval {
            code: String::new(),
        },
    );
    let mut saved = saved();
    saved.base_url = "http://127.0.0.1:9/no-linking".into();
    let refused = start_new(&saved, &new, false, quiet()).unwrap_err();
    assert!(refused.contains("can't link devices"), "{refused}");
    assert!(view(&saved).is_none());
}

#[test]
fn a_code_that_is_not_one_is_refused_before_anything() {
    let old = device("bad-code", DeviceState::Ready);
    let mut saved = saved();
    saved.base_url = "http://127.0.0.1:9/bad-code".into();
    assert!(start_old(&saved, &old, "tiny-link-1.nope", quiet())
        .unwrap_err()
        .contains("isn't a link code"));
    assert!(view(&saved).is_none());
}

#[test]
fn history_older_than_dates_goes_only_with_all() {
    let undated = Opened {
        text: "kept before dates".into(),
        ..Opened::default()
    };
    assert!(within(&undated, Scope::All, now_secs()));
    assert!(!within(&undated, Scope::Days(3650), now_secs()));
    assert!(!within(&opened("x", 0), Scope::None, now_secs()));
}

#[test]
fn history_over_the_servers_limit_sends_the_newest_and_says_how_many_stayed() {
    let old = device("cap-old", DeviceState::Ready);
    let (dir, cache_key) = {
        let mut old = lock(&old);
        for n in 0..200i64 {
            old.remember("c1", &format!("m{n:03}"), opened(&"x".repeat(200), 200 - n));
        }
        (old.dir.clone(), old.cache_key.clone().unwrap())
    };
    let meta = Meta {
        v: archive::VERSION,
        user: USER.into(),
        from_device: "d".into(),
        created: "0".into(),
        scope: Scope::All,
    };
    let key = ArchiveKey::generate().unwrap();
    let path = dir.join("capped.archive");
    let most = 8 * 1024;
    let (summary, left_out) = write_archive(
        &path,
        &key,
        b"link",
        &dir,
        &cache_key,
        &BTreeMap::new(),
        most,
        &meta,
    )
    .unwrap();
    assert!(summary.bytes <= most, "{} > {most}", summary.bytes);
    assert!(left_out > 150 && left_out < 200, "{left_out}");
    assert_eq!(summary.msgs + left_out, 200);
    let mut ids = Vec::new();
    archive::read(
        &key,
        b"link",
        std::fs::File::open(&path).unwrap(),
        |record| {
            if let archive::Record::Msg { id, .. } = record {
                ids.push(id);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        ids.first().map(String::as_str),
        Some("m199"),
        "the newest goes first"
    );
    assert!(!ids.contains(&"m000".to_owned()), "the oldest stays");
    // With room for all of it, nothing stays.
    let (_, none_left) = write_archive(
        &path,
        &key,
        b"link",
        &dir,
        &cache_key,
        &BTreeMap::new(),
        ARCHIVE_MAX,
        &meta,
    )
    .unwrap();
    assert_eq!(none_left, 0);
}

/// The whole link against a real tinyserver with `e2ee_device_link`: two sessions of one person in
/// `TINY_LINK_SESSIONS` (a JSON array of Tiny sessions), and nobody's device registered for them
/// yet. The first becomes their first device, reads (here: is given) a message of a conversation
/// they are in, and links the second, which must come out registered, approved by the first, with
/// the message. Both are revoked at the end, so the person is left as they were.
///
/// `cargo test -p chatd --no-default-features --lib linking::tests::live -- --ignored --nocapture`
#[test]
#[ignore]
fn live_a_real_server_links_a_device_with_its_history() {
    let Ok(text) = std::env::var("TINY_LINK_SESSIONS") else {
        eprintln!("TINY_LINK_SESSIONS is not set");
        return;
    };
    let rows: Vec<Value> = serde_json::from_str(&text).expect("a JSON array");
    let sessions: Vec<Saved> = rows
        .iter()
        .map(|row| crate::adapters::tiny::parse(&row.to_string()).expect("a Tiny session"))
        .collect();
    let (one, two) = (&sessions[0], &sessions[1]);
    assert_eq!(one.user_id, two.user_id, "two sessions of one person");
    let make = |saved: &Saved, name: &str| -> Shared {
        let mut saved = saved.clone();
        saved.e2ee = Some(super::super::Secrets::generate().unwrap());
        let mut device = Device::new(&saved);
        device.dir = std::env::temp_dir().join(format!(
            "chatd-e2ee-link-live-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&device.dir);
        Arc::new(Mutex::new(device))
    };
    let first = make(one, "first");
    let active = lock(&first).list(&one.user_id).unwrap();
    if !active.is_empty() {
        eprintln!(
            "this person already has {} device(s): not touching them",
            active.len()
        );
        return;
    }
    lock(&first).enrol();
    assert_eq!(
        lock(&first).state,
        DeviceState::Ready,
        "the first device is trusted on first use"
    );
    assert!(lock(&first).linking, "the server links devices");
    let second = make(two, "second");
    lock(&second).enrol();
    assert!(matches!(
        lock(&second).state,
        DeviceState::AwaitingApproval { .. }
    ));

    let relay_one: Arc<dyn Relay> = Arc::new(Http { saved: one.clone() });
    let relay_two: Arc<dyn Relay> = Arc::new(Http { saved: two.clone() });
    let member_of = relay_one.conversations().unwrap();
    let conv = member_of
        .iter()
        .next()
        .cloned()
        .expect("the person is in a conversation");
    lock(&first).remember(
        &conv,
        "00000000-0000-4000-8000-00000000c0de",
        opened("from before the link", 1),
    );

    let new_job = Arc::new(Linking::new(Role::New, quiet()));
    let old_job = Arc::new(owner_approved_job());
    let new_thread = {
        let (job, device, relay) = (new_job.clone(), second.clone(), relay_two.clone());
        std::thread::spawn(move || run_new(&job, &device, relay.as_ref()))
    };
    let code = loop {
        let view = lock(&new_job.state).view.clone();
        assert_ne!(view.stage, LinkStage::Failed, "{}", view.error);
        if !view.code.is_empty() {
            break LinkCode::parse(&view.code).unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let old_thread = {
        let (job, device, relay) = (old_job.clone(), first.clone(), relay_one.clone());
        std::thread::spawn(move || run_old(&job, &device, relay.as_ref(), &code))
    };
    let at_old = reaches(&old_job, LinkStage::Confirm);
    assert_eq!(at_old.check, reaches(&new_job, LinkStage::Confirm).check);
    say_yes(&old_job, Scope::All);
    let new_result = new_thread.join().unwrap();
    let old_result = old_thread.join().unwrap();
    eprintln!("new: {new_result:?}, old: {old_result:?}");

    let mut second_device = lock(&second);
    second_device.enrol();
    let ready = second_device.state == DeviceState::Ready;
    let read = second_device.cache(&conv).and_then(|cache| {
        cache
            .rows
            .get("00000000-0000-4000-8000-00000000c0de")
            .map(|o| o.text.clone())
    });
    // Leave the person as they were, whatever happened.
    for device in [&*second_device, &*lock(&first)] {
        if let Some(engine) = device.engine.as_ref() {
            let _ = device.ask(
                "DELETE",
                &format!("/v1/e2ee/devices/{}", engine.device_id()),
                None,
            );
        }
    }
    for dir in [second_device.dir.clone(), lock(&first).dir.clone()] {
        let _ = std::fs::remove_dir_all(dir);
    }
    assert_eq!(new_result, Ok(1));
    assert_eq!(old_result, Ok(()));
    assert!(ready, "the new device is registered and trusted");
    assert_eq!(read.as_deref(), Some("from before the link"));
}

#[test]
fn what_a_link_brings_in_is_read_again_and_the_groups_it_touches_are_known() {
    let relay = Arc::new(Fake {
        members: ["c1".to_owned(), "c2".to_owned()].into(),
        ..Fake::default()
    });
    let rows = [
        ("c1", "m1", opened("one", 1)),
        ("c2", "m2", opened("two", 1)),
    ];
    let (commands, inbox) = std::sync::mpsc::channel();
    let run = link_through_with(relay, &rows, Scope::All, Some(commands), |_, _| {});
    assert_eq!(run.new_result, Ok(2));
    let mut reloaded: Vec<String> = inbox
        .try_iter()
        .filter_map(|command| match command {
            crate::adapters::Command::Backfill(id) => Some(id),
            _ => None,
        })
        .collect();
    reloaded.sort();
    assert_eq!(
        reloaded,
        vec!["c1".to_owned(), "c2".to_owned()],
        "each conversation that gained rows"
    );
    // What a nudge after the link would ask about on the old device.
    let known = lock(&run.old).known_encrypted();
    assert!(known.contains("c1") && known.contains("c2"), "{known:?}");
}
