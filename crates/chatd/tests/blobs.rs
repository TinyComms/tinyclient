//! The media cache, from outside: a real chatd in a scratch directory, a file server on
//! localhost standing in for a network's, and nothing asserted but what the socket answers and
//! what is on disk.

use chatd::store::Store;
use protocol::{
    read_frame, write_frame, AccountKey, Change, MediaFile, MediaKind, Network, Request, Response,
};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use protocol::ChatStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

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
        String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
    }
}
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

const MB: usize = 1024 * 1024;

/// The bytes `/bytes/N` serves: a pattern, so a file that lands can be told from any other.
fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|at| (at % 251) as u8).collect()
}

/// Stands in for a network's file host. Remembers every path it was asked for.
struct Files {
    port: u16,
    asked: Arc<Mutex<Vec<String>>>,
    /// Requests being answered now, and the most there ever were at once.
    busy: Arc<(AtomicUsize, AtomicUsize)>,
}

impl Files {
    fn start() -> Files {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let busy: Arc<(AtomicUsize, AtomicUsize)> = Arc::default();
        let (log, load) = (asked.clone(), busy.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, load) = (log.clone(), load.clone());
                std::thread::spawn(move || {
                    let now = load.0.fetch_add(1, Ordering::SeqCst) + 1;
                    load.1.fetch_max(now, Ordering::SeqCst);
                    let _ = Files::answer(stream, &log);
                    load.0.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Files { port, asked, busy }
    }

    /// The most requests that were ever being answered at once.
    fn most_at_once(&self) -> usize {
        self.busy.1.load(Ordering::SeqCst)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self) -> usize {
        self.asked.lock().unwrap().len()
    }

    /// `/bytes/N` is N bytes with a length; `/slow/N` the same after 600 ms; `/cdn/...` a small
    /// picture; `/chunked/N` is N bytes without one; `/page` is a sign-in page; anything else is
    /// not there.
    fn answer(stream: TcpStream, log: &Mutex<Vec<String>>) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request)?;
        let mut line = String::new();
        while reader.read_line(&mut line)? > 2 {
            line.clear();
        }
        let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
        log.lock().unwrap().push(path.clone());
        let mut stream = reader.into_inner();
        let (kind, length) = path.trim_start_matches('/').split_once('/').unwrap_or((&path[1..], "0"));
        let length = length.split('?').next().unwrap_or("0");
        let (kind, length): (&str, usize) = match kind {
            "slow" => {
                sleep(Duration::from_millis(600));
                ("bytes", length.parse().unwrap_or(0))
            }
            "cdn" => ("bytes", 300),
            _ => (kind, length.parse().unwrap_or(0)),
        };
        let head = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type:";
        match kind {
            "bytes" => {
                write!(stream, "{head} image/png\r\nContent-Length: {length}\r\n\r\n")?;
                // In pieces, so a large file is never whole in this process either.
                let piece = pattern(251 * 256);
                let mut left = length;
                while left > 0 {
                    let now = left.min(piece.len());
                    stream.write_all(&piece[..now])?;
                    left -= now;
                }
                Ok(())
            }
            "chunked" => {
                write!(stream, "{head} image/png\r\nTransfer-Encoding: chunked\r\n\r\n")?;
                let piece = vec![7u8; 64 * 1024];
                for _ in 0..length / piece.len() {
                    write!(stream, "{:x}\r\n", piece.len())?;
                    stream.write_all(&piece)?;
                    stream.write_all(b"\r\n")?;
                }
                stream.write_all(b"0\r\n\r\n")
            }
            "page" => {
                let page = "<html>Sign in</html>";
                write!(stream, "{head} text/html\r\nContent-Length: {}\r\n\r\n{page}", page.len())
            }
            _ => stream.write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"),
        }
    }
}

fn account() -> AccountKey {
    AccountKey {
        network: Network::Discord,
        account: "42".to_owned(),
    }
}

fn file(id: &str, kind: MediaKind) -> MediaFile {
    MediaFile::new(id, kind)
}

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    /// A scratch directory holding one account and the files `registered` names.
    fn scratch(registered: &[(MediaFile, String)]) -> PathBuf {
        Daemon::scratch_with(registered, |_| {})
    }

    /// The same, and whatever else `seed` puts in the store.
    fn scratch_with(registered: &[(MediaFile, String)], seed: impl FnOnce(&mut Store)) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // Short path: a unix socket address holds about 100 bytes. On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!("/tmp/tinyclient-blobs-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("chat.sqlite")).unwrap();
        store.set_account(&account(), "Robin", "42").unwrap();
        store.register_media(&account(), registered).unwrap();
        seed(&mut store);
        dir
    }

    /// Starts chatd on `dir`, trusting the file server on localhost. `env` adds the overrides
    /// a test needs.
    fn start_in(dir: PathBuf, env: &[(&str, &str)]) -> Daemon {
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_MEDIA_HOSTS", "127.0.0.1")
            .envs(env.iter().copied())
            .spawn()
            .unwrap();
        let daemon = Daemon { child, dir };
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&daemon.dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        daemon
    }

    fn start(registered: &[(MediaFile, String)], env: &[(&str, &str)]) -> Daemon {
        Daemon::start_in(Daemon::scratch(registered), env)
    }

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    fn seq(&self) -> u64 {
        match self.ask(Request::Status) {
            Response::Status { seq, .. } => seq,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// What `Fetch` answers at once.
    fn fetch(&self, ids: &[&str]) -> Vec<MediaFile> {
        let ids = ids.iter().map(|id| (*id).to_owned()).collect();
        match self.ask(Request::Fetch { retry: false, size: None, account: account(), ids }) {
            Response::Media { files } => files,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The `Media` change for a file, heard after `since`.
    fn landed(&self, mut since: u64, id: &str) -> MediaFile {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let Response::Changes { seq, changes, .. } = self.ask(Request::Wait { since }) else {
                panic!("wait did not answer with changes");
            };
            since = seq;
            for change in changes {
                match change {
                    Change::Media { account: of, file } if of == account() && file.id == id => return file,
                    _ => {}
                }
            }
        }
        panic!("no media change for {id}");
    }

    /// Asks for one file and waits for it to land or fail.
    fn fetched(&self, id: &str) -> MediaFile {
        let since = self.seq();
        let at_once = self.fetch(&[id]);
        assert_eq!(at_once.len(), 1, "{id} is registered");
        assert_eq!(at_once[0].path, None, "{id} is not on disk yet");
        self.landed(since, id)
    }

    fn media_dir(&self) -> PathBuf {
        self.dir.join("media/discord/42")
    }

    /// Everything in the account's media directory, part files included.
    fn on_disk(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.media_dir()) else {
            return Vec::new();
        };
        entries.flatten().map(|entry| entry.path()).collect()
    }

    fn rss_kb(&self) -> u64 {
        process_rss_kb(self.child.id())
    }

    /// Stops chatd and keeps its directory, for starting it again.
    fn stop(mut self) -> PathBuf {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::take(&mut self.dir)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.dir != Path::new("") {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_file_is_answered_without_a_path_then_lands_with_the_bytes_served_and_is_fetched_once() {
    let files = Files::start();
    let chatd = Daemon::start(&[(file("avatar-1", MediaKind::Avatar), files.url("/bytes/3000"))], &[]);

    let since = chatd.seq();
    // An id nobody registered is left out rather than fetched.
    let at_once = chatd.fetch(&["avatar-1", "https://example.org/from-a-message.png"]);
    assert_eq!(at_once.len(), 1);
    assert_eq!((at_once[0].id.as_str(), &at_once[0].path), ("avatar-1", &None));

    let landed = chatd.landed(since, "avatar-1");
    let path = PathBuf::from(landed.path.expect("a path once it is on disk"));
    assert!(path.starts_with(chatd.media_dir()), "{path:?}");
    assert_eq!(std::fs::read(&path).unwrap(), pattern(3000));
    assert_eq!(landed.bytes, 3000);
    assert_eq!(chatd.on_disk(), vec![path.clone()], "no part file is left");
    assert_eq!(files.hits(), 1);

    // Asked again: the path at once, and the network is not touched.
    let again = chatd.fetch(&["avatar-1"]);
    assert_eq!(again[0].path.as_deref(), path.to_str());
    sleep(Duration::from_millis(300));
    assert_eq!(files.hits(), 1);
}

#[test]
fn a_body_over_the_cap_is_refused_and_leaves_no_file() {
    let files = Files::start();
    let registered = [
        // An avatar may be 1 MB. One says so in its headers, one only as it arrives.
        (file("declared", MediaKind::Avatar), files.url(&format!("/bytes/{}", 2 * MB))),
        (file("streamed", MediaKind::Avatar), files.url(&format!("/chunked/{}", 2 * MB))),
        // And what is not the file at all.
        (file("page", MediaKind::File), files.url("/page")),
        (file("gone", MediaKind::Image), files.url("/gone")),
    ];
    let chatd = Daemon::start(&registered, &[]);
    for id in ["declared", "streamed", "page", "gone"] {
        let failed = chatd.fetched(id);
        assert_eq!(failed.path, None, "{id} was kept");
        assert_eq!(chatd.on_disk(), Vec::<PathBuf>::new(), "{id} left something behind");
        assert_eq!(chatd.fetch(&[id])[0].path, None);
    }
}

#[test]
fn a_failure_says_why_and_is_not_tried_again_until_the_user_asks() {
    let files = Files::start();
    let chatd = Daemon::start(&[(file("gone", MediaKind::Image), files.url("/gone"))], &[]);
    let failed = chatd.fetched("gone");
    assert_eq!((failed.path, failed.failed.as_deref()), (None, Some("The file is no longer there (404).")));
    assert_eq!(files.hits(), 1);

    // However often the shell asks, and however long after: the reason, and no request.
    for _ in 0..5 {
        let again = chatd.fetch(&["gone"]);
        assert_eq!(again[0].failed.as_deref(), Some("The file is no longer there (404)."));
    }
    sleep(Duration::from_secs(6));
    chatd.fetch(&["gone"]);
    sleep(Duration::from_millis(300));
    assert_eq!(files.hits(), 1, "a failed file was fetched again unasked");

    // The user asks again: once more, and the failure stands again after it.
    let since = chatd.seq();
    let retried = chatd.ask(Request::Fetch { retry: true, size: None, account: account(), ids: vec!["gone".to_owned()] });
    assert!(matches!(&retried, Response::Media { files } if files[0].failed.is_none()), "{retried:?}");
    assert!(chatd.landed(since, "gone").failed.is_some());
    assert_eq!(files.hits(), 2);
    chatd.fetch(&["gone"]);
    sleep(Duration::from_millis(300));
    assert_eq!(files.hits(), 2);
}

#[test]
fn the_size_asked_for_picks_the_smallest_registered_one_that_is_large_enough() {
    let files = Files::start();
    let thumb = MediaFile {
        width: 400,
        height: 225,
        ..file("thumb", MediaKind::Image)
    };
    let dir = Daemon::scratch(&[(thumb, files.url("/bytes/4000"))]);
    {
        let mut store = Store::open(&dir.join("chat.sqlite")).unwrap();
        let sizes = [(600, files.url("/bytes/6000")), (800, files.url("/bytes/8000")), (1200, files.url("/bytes/12000"))];
        store.register_variants(&account(), "thumb", &sizes).unwrap();
    }
    let chatd = Daemon::start_in(dir, &[]);
    let sized = |size: Option<u32>| {
        let since = chatd.seq();
        let asked = Request::Fetch { retry: false, size, account: account(), ids: vec!["thumb".to_owned()] };
        match chatd.ask(asked) {
            Response::Media { files } if files[0].path.is_some() => files[0].clone(),
            Response::Media { .. } => chatd.landed(since, "thumb"),
            other => panic!("unexpected {other:?}"),
        }
    };

    // No size: the one it was registered at.
    let small = sized(None);
    assert_eq!(small.bytes, 4000);
    // 700 pixels: 600 is too small, 800 is the smallest that is not. A new path, and the
    // smaller file is gone from disk.
    let larger = sized(Some(700));
    assert_eq!(larger.bytes, 8000);
    assert_ne!(larger.path, small.path);
    until("the smaller file to go", || chatd.on_disk().len() == 1);
    assert_eq!(chatd.on_disk(), vec![PathBuf::from(larger.path.clone().unwrap())]);
    // Smaller again: what is held is large enough, so nothing is fetched.
    assert_eq!(sized(Some(400)).path, larger.path);
    // Larger than anything registered: the largest there is.
    assert_eq!(sized(Some(4000)).bytes, 12000);
    assert_eq!(*files.asked.lock().unwrap(), ["/bytes/4000", "/bytes/8000", "/bytes/12000"]);
}

#[test]
fn a_host_the_network_does_not_serve_files_from_is_never_contacted() {
    let files = Files::start();
    // The same server under a name nobody allowed, and a link as a message might hold one.
    let registered = [
        (file("other-host", MediaKind::Avatar), format!("http://localhost:{}/bytes/3000", files.port)),
        (file("user-part", MediaKind::Avatar), format!("http://127.0.0.1@localhost:{}/bytes/3000", files.port)),
        (file("slack-file", MediaKind::Image), "https://files.slack.com/files-pri/T1-F1/a.png".to_owned()),
    ];
    let chatd = Daemon::start(&registered, &[]);
    for id in ["other-host", "user-part", "slack-file"] {
        assert_eq!(chatd.fetched(id).path, None, "{id} was fetched");
    }
    assert_eq!(files.hits(), 0);
    assert_eq!(chatd.on_disk(), Vec::<PathBuf>::new());
}

#[test]
fn the_least_recently_used_file_goes_when_the_cache_is_over_its_cap() {
    let files = Files::start();
    let registered: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|id| (file(id, MediaKind::Image), files.url("/bytes/100000")))
        .collect();
    // Room for two of the three, and nothing counts as on screen.
    let chatd = Daemon::start(&registered, &[("TINYCLIENT_MEDIA_CAP", "250000"), ("TINYCLIENT_MEDIA_ON_SCREEN", "0")]);
    let path = |file: MediaFile| PathBuf::from(file.path.expect("landed"));
    let a = path(chatd.fetched("a"));
    // Use is kept to the second.
    sleep(Duration::from_millis(1100));
    let b = path(chatd.fetched("b"));
    sleep(Duration::from_millis(1100));
    // Asking for `a` again makes `b` the least recently used.
    assert_eq!(chatd.fetch(&["a"])[0].path.as_deref(), a.to_str());
    sleep(Duration::from_millis(1100));
    let c = path(chatd.fetched("c"));

    until("b was not evicted", || !b.exists());
    assert!(a.exists() && c.exists());
    let known = chatd.fetch(&["a", "c"]);
    assert!(known.iter().all(|file| file.path.is_some()), "{known:?}");
    // The evicted file is fetched again when it is asked for.
    assert!(chatd.fetched("b").path.is_some());
}

#[test]
fn a_smaller_cap_chosen_in_settings_evicts_at_once() {
    let ids = ["a", "b", "c"];
    let registered: Vec<_> = ids.iter().map(|id| (file(id, MediaKind::Image), "http://127.0.0.1:9/never".to_owned())).collect();
    let dir = Daemon::scratch(&registered);
    // Three files on disk that the store counts as 100 MB each: 300 MB, under the default
    // 500 MB and over the smallest choice, 250 MB. `a` is the least recently used.
    let media = dir.join("media/discord/42");
    std::fs::create_dir_all(&media).unwrap();
    let mut store = Store::open(&dir.join("chat.sqlite")).unwrap();
    let paths: Vec<PathBuf> = ids.iter().map(|id| media.join(format!("cached-{id}"))).collect();
    for (at, (id, path)) in ids.iter().zip(&paths).enumerate() {
        std::fs::write(path, b"x").unwrap();
        store.media_landed(&account(), id, path.to_str().unwrap(), 100 * 1024 * 1024, 1_000 + at as i64).unwrap();
    }
    drop(store);
    let chatd = Daemon::start_in(dir, &[("TINYCLIENT_MEDIA_ON_SCREEN", "0")]);
    sleep(Duration::from_millis(300));
    assert!(paths.iter().all(|path| path.exists()), "300 MB is under the default cap");

    let set = Request::Set { scope: String::new(), key: protocol::settings::CACHE_MB.to_owned(), value: Some("250".to_owned()) };
    assert_eq!(chatd.ask(set), Response::Ok);
    until("the oldest file to go under the new cap", || !paths[0].exists());
    assert!(paths[1].exists() && paths[2].exists(), "only what the cap needs goes");
}

#[test]
fn what_was_asked_for_in_the_last_minute_is_not_evicted() {
    let files = Files::start();
    let registered: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|id| (file(id, MediaKind::Image), files.url("/bytes/100000")))
        .collect();
    let chatd = Daemon::start(&registered, &[("TINYCLIENT_MEDIA_CAP", "250000")]);
    for id in ["a", "b", "c"] {
        assert!(chatd.fetched(id).path.is_some());
    }
    sleep(Duration::from_millis(500));
    assert_eq!(chatd.on_disk().len(), 3);
}

#[test]
fn signing_out_removes_the_accounts_directory() {
    let files = Files::start();
    let chatd = Daemon::start(&[(file("avatar-1", MediaKind::Avatar), files.url("/bytes/3000"))], &[]);
    assert!(chatd.fetched("avatar-1").path.is_some());
    assert!(chatd.media_dir().exists());

    let signed_out = chatd.ask(Request::SignOut { account: account() });
    assert!(matches!(signed_out, Response::Status { .. }), "{signed_out:?}");
    assert!(!chatd.media_dir().exists());
    // Its rows went with it, so the file is no longer known.
    assert_eq!(chatd.fetch(&["avatar-1"]), Vec::new());
}

#[test]
fn the_cache_survives_a_restart_and_is_made_to_agree_with_the_directory() {
    let files = Files::start();
    let registered = [
        (file("kept", MediaKind::Image), files.url("/bytes/3000")),
        (file("lost", MediaKind::Image), files.url("/bytes/4000")),
    ];
    let chatd = Daemon::start(&registered, &[]);
    let kept = PathBuf::from(chatd.fetched("kept").path.unwrap());
    let lost = PathBuf::from(chatd.fetched("lost").path.unwrap());
    assert_eq!(files.hits(), 2);
    let dir = chatd.stop();

    // While chatd was down one file went missing and two nobody knows appeared.
    std::fs::remove_file(&lost).unwrap();
    let orphan = kept.with_file_name("0123456789abcdef");
    let part = kept.with_file_name(".0123456789abcdef.part");
    std::fs::write(&orphan, b"orphan").unwrap();
    std::fs::write(&part, b"half a download").unwrap();

    let chatd = Daemon::start_in(dir, &[]);
    until("orphans were not removed", || !orphan.exists() && !part.exists());
    assert_eq!(chatd.fetch(&["kept"])[0].path.as_deref(), kept.to_str());
    assert_eq!(std::fs::read(&kept).unwrap(), pattern(3000));
    assert_eq!(files.hits(), 2, "a file on disk was fetched again");

    // The missing one is known to be missing, and comes back when asked for.
    let back = chatd.fetched("lost");
    assert_eq!(std::fs::read(back.path.unwrap()).unwrap(), pattern(4000));
    assert_eq!(files.hits(), 3);
}

#[test]
fn a_large_download_is_streamed_to_disk_and_not_held_in_memory() {
    let files = Files::start();
    let large = MediaFile {
        bytes: 20 * MB as u64,
        ..file("large", MediaKind::File)
    };
    let registered = [
        (file("warm", MediaKind::Image), files.url("/bytes/200000")),
        (large, files.url(&format!("/bytes/{}", 20 * MB))),
    ];
    let chatd = Daemon::start(&registered, &[]);
    // One fetch first, so the worker's buffer and connection are already paid for.
    assert!(chatd.fetched("warm").path.is_some());
    let before = chatd.rss_kb();

    let since = chatd.seq();
    chatd.fetch(&["large"]);
    let mut peak = before;
    let landed = loop {
        peak = peak.max(chatd.rss_kb());
        if let Some(file) = chatd.fetch(&["large"]).pop().filter(|file| file.path.is_some()) {
            break file;
        }
        sleep(Duration::from_millis(5));
    };
    assert_eq!(chatd.landed(since, "large").path, landed.path);
    assert_eq!(std::fs::metadata(landed.path.unwrap()).unwrap().len(), 20 * MB as u64);
    let after = chatd.rss_kb();
    assert!(peak < before + 4 * 1024, "RSS rose from {before} KB to {peak} KB during a 20 MB download");
    assert!(after < before + 4 * 1024, "RSS rose from {before} KB to {after} KB after a 20 MB download");
}

/// Says what the shell has on screen.
fn watch_rows(chatd: &Daemon, rows: &[&str]) {
    let rows = rows.iter().map(|id| protocol::Key::new(&account(), *id)).collect();
    let watching = protocol::Watching { rows, ..protocol::Watching::default() };
    let answer = chatd.ask(Request::Watch(watching));
    assert!(matches!(answer, Response::Watching { .. }), "{answer:?}");
}

#[test]
fn rows_pictures_are_fetched_four_at_a_time_and_not_once_their_rows_scrolled_away() {
    let files = Files::start();
    let rows: Vec<String> = (1..=8).map(|at| format!("c{at}")).collect();
    let registered: Vec<(MediaFile, String)> = (1..=8).map(|at| (file(&format!("row-{at}"), MediaKind::Avatar), files.url(&format!("/slow/200?row={at}")))).collect();
    let dir = Daemon::scratch_with(&registered, |store| {
        for (at, id) in rows.iter().enumerate() {
            let mut row = chatd::store::ConversationRow::new(id.clone(), protocol::ConversationKind::Direct, format!("Row {}", at + 1));
            row.avatar = Some(format!("row-{}", at + 1));
            store.upsert_conversation(&account(), &row).unwrap();
        }
    });
    let chatd = Daemon::start_in(dir, &[]);

    // Eight rows on screen; the shell asks for their pictures in one go. The newest asked go
    // first, four at once.
    watch_rows(&chatd, &["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"]);
    let since = chatd.seq();
    let asked: Vec<String> = (1..=8).map(|at| format!("row-{at}")).collect();
    assert_eq!(chatd.fetch(&asked.iter().map(String::as_str).collect::<Vec<_>>()).len(), 8);
    // Then the list scrolls: only the last four rows are on screen.
    watch_rows(&chatd, &["c5", "c6", "c7", "c8"]);
    for at in 5..=8 {
        assert!(chatd.landed(since, &format!("row-{at}")).path.is_some());
    }
    sleep(Duration::from_millis(1500));
    let mut fetched: Vec<String> = files.asked();
    fetched.sort();
    assert_eq!(fetched, (5..=8).map(|at| format!("/slow/200?row={at}")).collect::<Vec<_>>(), "nothing for rows that scrolled away");
    assert_eq!(files.most_at_once(), 4, "four at once, no more");

    // Scrolled back: they are fetched now, still four at most at once.
    let since = chatd.seq();
    watch_rows(&chatd, &["c1", "c2", "c3", "c4"]);
    for at in 1..=4 {
        assert!(chatd.landed(since, &format!("row-{at}")).path.is_some());
    }
    assert_eq!(files.hits(), 8, "each picture once");
    assert_eq!(files.most_at_once(), 4);
}

#[test]
fn picker_previews_live_in_their_own_directory_and_go_when_the_picker_closes() {
    let files = Files::start();
    let sticker = chatd::blobs::pick("st-1");
    let still = chatd::blobs::pick("gif-1");
    let registered = [
        (file(&sticker, MediaKind::Image), files.url("/bytes/3000")),
        (file(&still, MediaKind::Image), files.url("/bytes/4000")),
        (file("emoji-1", MediaKind::Emoji), files.url("/bytes/500")),
    ];
    // A picker counts as closed after 5 s without asking. Roomy, so a slow or cold machine
    // cannot overtake the last section, where a preview is asked for again.
    let chatd = Daemon::start(&registered, &[("TINYCLIENT_PICK_IDLE", "5")]);
    let pick_dir = chatd.dir.join("media/discord/pick/42");
    let landed = [chatd.fetched(&sticker), chatd.fetched(&still)];
    for file in &landed {
        let path = PathBuf::from(file.path.clone().unwrap());
        assert!(path.starts_with(&pick_dir), "{path:?} is in the picker's directory");
        assert!(path.exists());
    }
    // A custom emoji is not a pick: it stays in the cache.
    let emoji = PathBuf::from(chatd.fetched("emoji-1").path.unwrap());
    assert!(emoji.starts_with(chatd.media_dir()));

    until("the picker's files to go once it closed", || !pick_dir.exists());
    assert!(!chatd.dir.join("media/discord/pick").exists());
    assert!(emoji.exists(), "the emoji stays");
    // Opened again: the previews are fetched again, from the network.
    assert!(chatd.fetched(&sticker).path.is_some());
    assert_eq!(files.hits(), 4);
}

/// A gateway that says hello and answers an identify with the READY of Discord's fixture.
fn discord_gateway() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let gateway = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let address = gateway.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let address = address.clone();
            std::thread::spawn(move || {
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    return;
                };
                let _ = socket.send(r#"{"op":10,"d":{"heartbeat_interval":1500}}"#.into());
                while let Ok(frame) = socket.read() {
                    let tungstenite::Message::Text(text) = frame else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text.as_str()).unwrap_or_default();
                    let answer = match frame["op"].as_u64() {
                        Some(1) => r#"{"op":11}"#.to_owned(),
                        Some(2) => {
                            let ready = include_str!("fixtures/discord/ready.json").replace("{{GATEWAY}}", &address);
                            format!(r#"{{"op":0,"t":"READY","s":1,"d":{ready}}}"#)
                        }
                        _ => continue,
                    };
                    if socket.send(answer.into()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    gateway
}

#[test]
fn the_accounts_own_picture_lands_after_it_connects_without_the_shell_asking() {
    let files = Files::start();
    let gateway = discord_gateway();
    // The session's token names user 1000, which the READY says we are.
    let me = AccountKey {
        network: Network::Discord,
        account: "1000".to_owned(),
    };
    let dir = Daemon::scratch_with(&[], |store| {
        store.set_account(&me, "Justin", "1000").unwrap();
        store.register_media(&me, &[(file("me-pic", MediaKind::Avatar), files.url("/bytes/900"))]).unwrap();
        let person = chatd::store::PersonRow {
            id: "1000".to_owned(),
            name: "Justin".to_owned(),
            handle: "justin".to_owned(),
            avatar: Some("me-pic".to_owned()),
            bot: false,
            status: String::new(),
            link: String::new(),
        };
        store.upsert_people(&me, &[person]).unwrap();
    });
    let session = r#"{"token":"MTAwMA.Zx9aBc.not-a-real-signature"}"#;
    let chatd = Daemon::start_in(
        dir,
        &[
            ("TINYCLIENT_SESSION_DISCORD", session),
            ("TINYCLIENT_DISCORD_API", &files.url("/api")),
            ("TINYCLIENT_DISCORD_GATEWAY", &format!("{gateway}/?encoding=json&v=9")),
            ("TINYCLIENT_DISCORD_CDN", &files.url("/cdn")),
        ],
    );
    // Nothing but `Status` is asked: no `Fetch`.
    let deadline = Instant::now() + Duration::from_secs(20);
    let (link, avatar) = loop {
        let Response::Status { accounts, .. } = chatd.ask(Request::Status) else {
            panic!("status");
        };
        let own = accounts.into_iter().find(|status| status.key == me).expect("the account is listed");
        if own.link == protocol::Link::Connected && own.avatar.as_ref().is_some_and(|avatar| avatar.path.is_some()) {
            break (own.link, own.avatar.unwrap());
        }
        assert!(Instant::now() < deadline, "the own picture did not land: {:?} {:?}", own.link, own.avatar);
        sleep(Duration::from_millis(100));
    };
    assert_eq!(link, protocol::Link::Connected);
    assert!(Path::new(&avatar.path.unwrap()).exists());
    let pictures = files.asked().into_iter().filter(|path| path.starts_with("/bytes/") || path.starts_with("/cdn/")).count();
    assert_eq!(pictures, 1, "the own picture, once");
}
