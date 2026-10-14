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
use std::sync::atomic::{AtomicU32, Ordering};
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
}

impl Files {
    fn start() -> Files {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || {
                    let _ = Files::answer(stream, &log);
                });
            }
        });
        Files { port, asked }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self) -> usize {
        self.asked.lock().unwrap().len()
    }

    /// `/bytes/N` is N bytes with a length; `/chunked/N` is N bytes without one; `/page` is a
    /// sign-in page; anything else is not there.
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
        let length: usize = length.parse().unwrap_or(0);
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
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // Short path: a unix socket address holds about 100 bytes.
        let dir = PathBuf::from(format!("/tmp/tinychat-blobs-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("chat.sqlite")).unwrap();
        store.set_account(&account(), "Robin", "42").unwrap();
        store.register_media(&account(), registered).unwrap();
        dir
    }

    /// Starts chatd on `dir`, trusting the file server on localhost. `env` adds the overrides
    /// a test needs.
    fn start_in(dir: PathBuf, env: &[(&str, &str)]) -> Daemon {
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .env("TINYCHAT_DIR", &dir)
            .env("TINYCHAT_MEDIA_HOSTS", "127.0.0.1")
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
        let out = Command::new("ps").args(["-o", "rss=", "-p", &self.child.id().to_string()]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
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
    let chatd = Daemon::start(&registered, &[("TINYCHAT_MEDIA_CAP", "250000"), ("TINYCHAT_MEDIA_ON_SCREEN", "0")]);
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
fn what_was_asked_for_in_the_last_minute_is_not_evicted() {
    let files = Files::start();
    let registered: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|id| (file(id, MediaKind::Image), files.url("/bytes/100000")))
        .collect();
    let chatd = Daemon::start(&registered, &[("TINYCHAT_MEDIA_CAP", "250000")]);
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
