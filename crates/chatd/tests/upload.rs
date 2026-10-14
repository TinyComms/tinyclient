//! The upload helper, from outside: each case runs in a process of its own (this test binary,
//! started again with `UPLOAD_CHILD` naming the case) with a scratch `TINYCLIENT_DIR` and `HOME`,
//! so its memory is measured alone. A sink on localhost stands in for a network's upload host
//! and records what arrived. Only what the sink saw, what the process said and what is on disk
//! is asserted.
//!
//! No adapter calls the helper here: the adapters are other slices' and use it through
//! `upload::spawn`, so these drive it as an adapter would.

use chatd::blobs::upload::{self, Target, Upload, CANCELLED};
use protocol::{AccountKey, Network, OutgoingFile};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

const MB: u64 = 1024 * 1024;

fn process_rss_kb(pid: u32) -> u64 {
    #[cfg(windows)]
    {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("(Get-Process -Id {pid}).WorkingSet64")])
            .output()
            .expect("working set");
        let bytes: u64 = String::from_utf8(out.stdout).unwrap().trim().parse().unwrap();
        return bytes / 1024;
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().parse().unwrap_or(0)
    }
}

fn fnv(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x0100_0000_01b3))
}

const FNV: u64 = 0xcbf2_9ce4_8422_2325;

/// Writes `length` bytes of a pattern a piece at a time, and answers their hash.
fn write_file(path: &Path, length: u64) -> u64 {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut out = std::fs::File::create(path).unwrap();
    let piece: Vec<u8> = (0..MB).map(|at| (at % 251) as u8).collect();
    let (mut left, mut hash) = (length, FNV);
    while left > 0 {
        let now = left.min(MB) as usize;
        out.write_all(&piece[..now]).unwrap();
        hash = fnv(hash, &piece[..now]);
        left -= now as u64;
    }
    hash
}

/// One request the sink saw.
#[derive(Debug, Clone, Default)]
struct Seen {
    method: String,
    path: String,
    length: Option<u64>,
    chunked: bool,
    received: u64,
    hash: u64,
    /// The whole body arrived and was answered.
    completed: bool,
    head: Vec<u8>,
    tail: Vec<u8>,
}

/// Stands in for a network's upload host. `?pace=N` sleeps N microseconds after each 64 KB, so
/// a large body takes long enough to watch.
struct Sink {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Sink {
    fn start() -> Sink {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || Sink::take(stream, &log));
            }
        });
        Sink { port, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn take(stream: TcpStream, log: &Mutex<Vec<Seen>>) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut words = line.split_whitespace();
        let mut seen = Seen {
            method: words.next().unwrap_or("").to_owned(),
            path: words.next().unwrap_or("").to_owned(),
            hash: FNV,
            ..Seen::default()
        };
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) <= 2 {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(length) = lower.strip_prefix("content-length:") {
                seen.length = length.trim().parse().ok();
            }
            seen.chunked |= lower.starts_with("transfer-encoding:") && lower.contains("chunked");
        }
        let pace = seen.path.split("pace=").nth(1).and_then(|pace| pace.parse::<u64>().ok()).unwrap_or(0);
        let at = {
            let mut log = log.lock().unwrap();
            log.push(seen.clone());
            log.len() - 1
        };
        let want = seen.length.unwrap_or(0);
        let mut buffer = vec![0u8; 64 * 1024];
        while seen.received < want {
            let most = buffer.len().min((want - seen.received) as usize);
            match reader.read(&mut buffer[..most]) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let piece = &buffer[..read];
                    seen.hash = fnv(seen.hash, piece);
                    if seen.head.len() < 512 {
                        seen.head.extend_from_slice(&piece[..read.min(512 - seen.head.len())]);
                    }
                    seen.tail.extend_from_slice(piece);
                    let keep = seen.tail.len().saturating_sub(128);
                    seen.tail.drain(..keep);
                    seen.received += read as u64;
                }
            }
            log.lock().unwrap()[at] = seen.clone();
            if pace > 0 {
                sleep(Duration::from_micros(pace));
            }
        }
        if seen.received == want && !seen.chunked {
            let body = json!({"ok": true, "bytes": want}).to_string();
            let answer = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            if reader.into_inner().write_all(answer.as_bytes()).is_ok() {
                seen.completed = true;
            }
        }
        log.lock().unwrap()[at] = seen;
    }
}

/// A scratch support directory and home directory, removed afterwards.
struct Scratch {
    dir: PathBuf,
    home: PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!("tinyclient-upload-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, home) = (root.join("support"), root.join("home"));
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Scratch { dir, home }
    }

    fn outbox(&self, name: &str) -> PathBuf {
        self.dir.join("outbox").join(name)
    }

    /// Runs `case` in its own process. `each` hears every line it prints, with its pid, as it
    /// prints it; the `RESULT` line is answered.
    fn run(&self, case: &str, env: &[(&str, String)], mut each: impl FnMut(u32, &str)) -> Value {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["child", "--exact", "--nocapture", "--test-threads", "1"])
            .env("UPLOAD_CHILD", case)
            .env("TINYCLIENT_DIR", &self.dir)
            .env("HOME", &self.home)
            .env_remove("TINYCLIENT_MEDIA_HOSTS")
            .envs(env.iter().map(|(name, value)| (*name, value.as_str())))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut result = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line.unwrap();
            each(pid, &line);
            // libtest may have put "test child ... " before it on the same line.
            if let Some((_, json)) = line.split_once("RESULT ") {
                result = Some(serde_json::from_str(json).unwrap());
            }
        }
        assert!(child.wait().unwrap().success(), "{case} ran to the end");
        result.unwrap_or_else(|| panic!("{case} said nothing"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap());
    }
}

fn account() -> AccountKey {
    AccountKey {
        network: Network::Slack,
        account: "T1".to_owned(),
    }
}

fn staged(path: &Path, name: &str) -> OutgoingFile {
    OutgoingFile {
        path: path.to_string_lossy().into_owned(),
        name: name.to_owned(),
        spoiler: false,
        as_file: false,
        original: false,
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn result(value: Value) {
    println!("RESULT {value}");
}

/// The cases, run only in a child process (`UPLOAD_CHILD`); a no-op otherwise.
#[test]
fn child() {
    let Ok(case) = std::env::var("UPLOAD_CHILD") else {
        return;
    };
    let file = PathBuf::from(var("UPLOAD_FILE"));
    let url = var("UPLOAD_URL");
    let started = Instant::now();
    let told: Arc<Mutex<Vec<(u64, u64, u64)>>> = Arc::default();
    let log = told.clone();
    let report = move |sent, total| log.lock().unwrap().push((started.elapsed().as_millis() as u64, sent, total));
    match case.as_str() {
        "stream" => {
            // One small upload first, so the connection code and its buffers are already paid
            // for when the large one is measured.
            let warm = PathBuf::from(var("UPLOAD_WARM"));
            let mut first = Upload::with_progress(&account(), "c-warm", &[staged(&warm, "")], |_, _| {}).unwrap();
            first.send(0, &Target::put(url.split('?').next().unwrap_or_default())).unwrap();
            println!("BEGIN");
            sleep(Duration::from_millis(300));
            let mut up = Upload::with_progress(&account(), "c-big", &[staged(&file, "big.bin")], report).unwrap();
            let answer = up.send(0, &Target::put(url).header("Authorization", "Bearer xoxc-1"));
            println!("END");
            let ok = answer.is_ok();
            let body = answer.map(|answer| answer.body).unwrap_or_else(|why| why);
            if ok {
                up.done();
            }
            result(json!({"ok": ok, "body": body, "told": *told.lock().unwrap()}));
        }
        "cancel" => {
            let account = account();
            let report = move |sent: u64, _total: u64| {
                // The user presses Cancel once some of it has gone.
                if sent >= 5 * MB {
                    upload::cancel(&account, "c-cancel");
                }
            };
            let mut up = Upload::with_progress(&self::account(), "c-cancel", &[staged(&file, "")], report).unwrap();
            let answer = up.send(0, &Target::put(url));
            result(json!({"answer": answer.map(|answer| answer.body).unwrap_or_else(|why| format!("Err {why}")), "cancelled": up.cancelled()}));
        }
        "early" => {
            // Cancel arrives before the send's upload begins.
            upload::cancel(&account(), "c-early");
            let mut up = Upload::with_progress(&account(), "c-early", &[staged(&file, "")], |_, _| {}).unwrap();
            let answer = up.send(0, &Target::put(url));
            result(json!({"answer": answer.map(|answer| answer.body).unwrap_or_else(|why| format!("Err {why}"))}));
        }
        "paths" => {
            let outcomes: Vec<Value> = var("UPLOAD_PATHS")
                .split('\n')
                .map(|path| match Upload::with_progress(&account(), "c-paths", &[staged(Path::new(path), "")], |_, _| {}) {
                    Ok(_) => json!({"path": path, "ok": true}),
                    Err(why) => json!({"path": path, "ok": false, "why": why}),
                })
                .collect();
            // A file that may be read, sent to a host that is not the network's.
            let mut up = Upload::with_progress(&account(), "c-host", &[staged(&file, "notes.txt")], |_, _| {}).unwrap();
            let elsewhere: Vec<String> = ["https://example.org/upload", "http://example.org/upload", "https://files.slack.com.example.org/upload"]
                .iter()
                .map(|url| up.send(0, &Target::put(*url)).err().unwrap_or_default())
                .collect();
            result(json!({"outcomes": outcomes, "elsewhere": elsewhere}));
        }
        "form" => {
            let mut up = Upload::with_progress(&account(), "c-form", &[staged(&file, "cat.png")], report).unwrap();
            let answer = up.send_form(0, &Target::post(url), "file", &[("channels", "C1")]).map(|answer| answer.status);
            result(json!({"status": answer.unwrap_or(0), "told": *told.lock().unwrap()}));
        }
        "parts" => {
            let mut up = Upload::with_progress(&account(), "c-parts", &[staged(&file, "")], report).unwrap();
            let mut parts = Vec::new();
            let mut hash = FNV;
            up.parts(0, 512 * 1024, 0, |part| {
                parts.push((part.index, part.count, part.bytes.len()));
                hash = fnv(hash, part.bytes);
                Ok(())
            })
            .unwrap();
            // Resumed from the fourth part, as Telegram resumes.
            let mut resumed = Vec::new();
            up.parts(0, 512 * 1024, 3, |part| {
                resumed.push(part.index);
                Ok(())
            })
            .unwrap();
            result(json!({"parts": parts, "hash": hash.to_string(), "resumed": resumed, "told": *told.lock().unwrap()}));
        }
        other => panic!("no case {other}"),
    }
}

#[test]
fn a_50_mb_file_streams_from_disk_with_memory_flat_and_progress_a_few_times_a_second() {
    let scratch = Scratch::new();
    let sink = Sink::start();
    let big = scratch.outbox("Pasted image.bin");
    let hash = write_file(&big, 50 * MB);
    let warm = scratch.home.join("warm.bin");
    write_file(&warm, 4096);
    // About 2 s for the whole body: long enough to watch, short enough to wait for.
    let env = [
        ("UPLOAD_FILE", big.to_string_lossy().into_owned()),
        ("UPLOAD_WARM", warm.to_string_lossy().into_owned()),
        ("UPLOAD_URL", sink.url("/upload/v1/big?pace=2000")),
    ];
    let (mut before, mut peak) = (0, 0);
    let outcome = scratch.run("stream", &env, |pid, line| {
        if line.ends_with("BEGIN") {
            before = process_rss_kb(pid);
            peak = before;
            // Sampled from here on by the thread below until the body is through.
            let sampler = std::thread::spawn(move || {
                let (mut peak, mut samples) = (0, 0);
                loop {
                    let rss = process_rss_kb(pid);
                    if rss == 0 {
                        return (peak, samples);
                    }
                    peak = peak.max(rss);
                    samples += 1;
                    sleep(Duration::from_millis(20));
                }
            });
            SAMPLER.lock().unwrap().replace(sampler);
        }
    });
    let (sampled, samples) = SAMPLER.lock().unwrap().take().expect("sampled").join().unwrap();
    peak = peak.max(sampled);
    assert!(samples >= 20, "RSS was sampled {samples} times");
    assert_eq!(outcome["ok"], true, "{outcome}");

    // The sink got the whole file, byte for byte, with its length up front, as signed upload
    // addresses require, and the session's header the adapter named.
    let seen = sink.seen();
    let request = seen.iter().find(|seen| seen.path.starts_with("/upload/v1/big?")).unwrap();
    assert_eq!((request.method.as_str(), request.length, request.chunked), ("PUT", Some(50 * MB), false));
    assert_eq!((request.received, request.hash, request.completed), (50 * MB, hash, true));

    // Memory: one buffer, never the file.
    eprintln!("upload of 50 MB: RSS {before} KB before, {peak} KB at most, {samples} samples");
    assert!(peak < before + 3 * 1024, "RSS rose from {before} KB to {peak} KB during a 50 MB upload");

    // Progress: a few times a second, never per buffer, and once at the very end.
    let told: Vec<(u64, u64, u64)> = serde_json::from_value(outcome["told"].clone()).unwrap();
    let (last_at, last, total) = *told.last().unwrap();
    assert_eq!((last, total), (50 * MB, 50 * MB));
    let span = last_at - told[0].0;
    assert!(told.len() >= 4, "progress was told {} times over {span} ms", told.len());
    assert!(told.len() as u64 <= span / 250 + 2, "progress was told {} times over {span} ms", told.len());
    for pair in told[..told.len() - 1].windows(2) {
        assert!(pair[1].0 - pair[0].0 >= 240, "two reports {} ms apart", pair[1].0 - pair[0].0);
        assert!(pair[1].1 > pair[0].1, "progress only goes up");
    }

    // The pasted file was the shell's copy in outbox/: once sent it is gone.
    assert!(!big.exists(), "the outbox file was removed after the send");
    assert!(warm.exists(), "a file the user picked is left where it was");
}

static SAMPLER: Mutex<Option<std::thread::JoinHandle<(u64, u32)>>> = Mutex::new(None);

#[test]
fn cancel_stops_the_body_midway_and_no_request_completes() {
    let scratch = Scratch::new();
    let sink = Sink::start();
    let file = scratch.outbox("clip.mov");
    write_file(&file, 40 * MB);
    let env = [("UPLOAD_FILE", file.to_string_lossy().into_owned()), ("UPLOAD_URL", sink.url("/upload/v1/clip?pace=2000"))];
    let outcome = scratch.run("cancel", &env, |_, _| {});
    assert_eq!(outcome["answer"], format!("Err {CANCELLED}"));
    assert_eq!(outcome["cancelled"], true);
    // Give the sink a moment to notice the connection went.
    sleep(Duration::from_millis(300));
    let seen = sink.seen();
    assert_eq!(seen.len(), 1, "one request was begun");
    let request = &seen[0];
    assert!(!request.completed, "the cut-off request was never whole, so never answered");
    assert!(request.received >= 4 * MB && request.received < 40 * MB, "{} bytes arrived", request.received);
    // A cancelled send can be retried, so the staged file stays.
    assert!(file.exists());

    // Cancelled before its upload began: nothing is asked of the network at all.
    let sink = Sink::start();
    let env = [("UPLOAD_FILE", file.to_string_lossy().into_owned()), ("UPLOAD_URL", sink.url("/upload/v1/early"))];
    let outcome = scratch.run("early", &env, |_, _| {});
    assert_eq!(outcome["answer"], format!("Err {CANCELLED}"));
    assert!(sink.seen().is_empty());
}

#[test]
fn only_files_the_user_could_pick_or_the_outbox_are_read_and_only_the_networks_hosts_are_sent_to() {
    let scratch = Scratch::new();
    let sink = Sink::start();
    let home = &scratch.home;
    let place = |path: PathBuf| {
        write_file(&path, 100);
        path
    };
    let picked = place(home.join("Documents/notes.txt"));
    let store = place(scratch.dir.join("chat.sqlite"));
    let session = place(scratch.dir.join("session.slack"));
    let cached = place(scratch.dir.join("media/slack/T1/0123456789abcdef"));
    let key = place(home.join(".ssh/id_ed25519"));
    let keychain = place(home.join("Library/Keychains/login.keychain-db"));
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    #[cfg(unix)]
    let links = {
        let into_outbox = scratch.outbox("link.png");
        std::os::unix::fs::symlink(&picked, &into_outbox).unwrap();
        let to_store = home.join("Desktop/innocent.png");
        std::os::unix::fs::symlink(&store, &to_store).unwrap();
        vec![into_outbox, to_store]
    };
    #[cfg(not(unix))]
    let links: Vec<PathBuf> = Vec::new();
    let refused = [store.clone(), session, cached, key, keychain, home.join("Desktop"), PathBuf::from("Documents/notes.txt"), home.join("gone.txt")];
    let paths: Vec<String> = std::iter::once(&picked).chain(refused.iter()).chain(links.iter()).map(|path| path.to_string_lossy().into_owned()).collect();
    let env = [
        ("UPLOAD_FILE", picked.to_string_lossy().into_owned()),
        ("UPLOAD_PATHS", paths.join("\n")),
        ("UPLOAD_URL", sink.url("/unused")),
    ];
    let outcome = scratch.run("paths", &env, |_, _| {});
    let outcomes = outcome["outcomes"].as_array().unwrap();
    assert_eq!(outcomes[0]["ok"], true, "a file the user picked is read: {}", outcomes[0]);
    for refused in &outcomes[1..] {
        assert_eq!(refused["ok"], false, "{refused}");
        // The refusal names the file, in words.
        let path = refused["path"].as_str().unwrap();
        let name = Path::new(path).file_name().unwrap().to_string_lossy();
        let why = refused["why"].as_str().unwrap();
        assert!(why.starts_with(&*name), "{why:?} names {name}");
    }
    let folder = outcomes.iter().find(|outcome| outcome["path"].as_str().unwrap().ends_with("Desktop")).unwrap();
    assert!(folder["why"].as_str().unwrap().contains("Folders can't be sent"), "{folder}");
    for why in outcome["elsewhere"].as_array().unwrap() {
        assert!(why.as_str().unwrap().starts_with("notes.txt could not be sent"), "{why}");
    }
    assert!(sink.seen().is_empty(), "nothing was sent anywhere");
    assert!(store.exists() && picked.exists());
}

#[test]
fn a_form_upload_carries_the_file_as_a_named_field_and_parts_come_one_buffer_at_a_time() {
    let scratch = Scratch::new();
    let sink = Sink::start();
    let picture = scratch.home.join("Pictures/cat.png");
    let hash = write_file(&picture, 2_400_000);
    let env = [("UPLOAD_FILE", picture.to_string_lossy().into_owned()), ("UPLOAD_URL", sink.url("/api/files.upload"))];
    let outcome = scratch.run("form", &env, |_, _| {});
    assert_eq!(outcome["status"], 200);
    let request = sink.seen().pop().unwrap();
    assert_eq!(request.method, "POST");
    assert!(request.completed && request.length == Some(request.received));
    let head = String::from_utf8_lossy(&request.head);
    assert!(head.contains("name=\"channels\"\r\n\r\nC1\r\n"), "{head}");
    assert!(head.contains("name=\"file\"; filename=\"cat.png\"\r\nContent-Type: image/png\r\n\r\n"), "{head}");
    let tail = String::from_utf8_lossy(&request.tail);
    assert!(tail.trim_end().ends_with("--"), "{tail}");
    assert!(request.received > 2_400_000 && request.received < 2_400_000 + 600);

    let outcome = scratch.run("parts", &env, |_, _| {});
    let parts: Vec<(u32, u32, usize)> = serde_json::from_value(outcome["parts"].clone()).unwrap();
    assert_eq!(parts.len(), 5);
    for (at, (index, count, length)) in parts.iter().enumerate() {
        assert_eq!((*index as usize, *count), (at, 5));
        assert_eq!(*length, if at < 4 { 512 * 1024 } else { 2_400_000 - 4 * 512 * 1024 });
    }
    assert_eq!(outcome["hash"], hash.to_string(), "the parts are the file, in order");
    assert_eq!(outcome["resumed"], json!([3, 4]));
    let told: Vec<(u64, u64, u64)> = serde_json::from_value(outcome["told"].clone()).unwrap();
    assert_eq!(told.last().map(|(_, sent, total)| (*sent, *total)), Some((2_400_000, 2_400_000)));
    // Not a file of the outbox, so it stays.
    assert!(picture.exists());
}
