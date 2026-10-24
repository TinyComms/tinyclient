//! Playing a voice message through chatd's player and the real `mediad --play`, which opens no
//! sound device here (`TINYCLIENT_MEDIAD_AUDIO`). The file is a second of Ogg Opus, as Telegram
//! and Discord send voice messages, placed in a scratch media cache.

use chatd::hub::Hub;
use chatd::store::Store;
use protocol::{AccountKey, Change, Key, Network, PlayRequest, PlayState, Playback};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::thread::sleep;
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(8);

fn environment() {
    static READY: Once = Once::new();
    READY.call_once(|| {
        let chatd = Path::new(env!("CARGO_BIN_EXE_chatd"));
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let built = Command::new(cargo).args(["build", "-p", "mediad", "--bin", "mediad"]).status().unwrap();
        assert!(built.success(), "could not build mediad");
        std::env::set_var("TINYCLIENT_MEDIAD", chatd.with_file_name(format!("mediad{}", std::env::consts::EXE_SUFFIX)));
        std::env::set_var("TINYCLIENT_MEDIAD_AUDIO", "null");
    });
}

/// The tests count mediad processes under this one, so they take turns.
fn alone() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct World {
    hub: Arc<Hub>,
    dir: PathBuf,
    voice: PathBuf,
}

fn world() -> World {
    environment();
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::path::absolute(format!("/tmp/tinyclient-play-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
    let cache = dir.join("media/telegram/1");
    std::fs::create_dir_all(&cache).unwrap();
    let voice = cache.join("voice");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../mediad/tests/fixtures/tone.opus"), &voice).unwrap();
    let store = Store::open(&dir.join("chat.sqlite")).unwrap();
    World { hub: Arc::new(Hub::new(store)), dir, voice }
}

impl World {
    fn ask(&self, request: PlayRequest) -> Result<(), String> {
        self.hub.player.request(&self.hub, &self.dir.join("media"), request)
    }

    fn play(&self, at: Option<u32>, rate: u16) {
        self.ask(start(&self.voice, at, rate)).unwrap();
    }

    fn shown(&self) -> Option<Playback> {
        self.hub.player.shown()
    }

    fn until(&self, what: &str, done: impl Fn(&Option<Playback>) -> bool) -> Option<Playback> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let shown = self.shown();
            if done(&shown) {
                return shown;
            }
            assert!(Instant::now() < deadline, "{what} did not happen: {shown:?}");
            sleep(Duration::from_millis(5));
        }
    }

    fn until_state(&self, state: PlayState) -> Playback {
        self.until(&format!("{state:?}"), |shown| shown.as_ref().is_some_and(|shown| shown.state == state)).unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn account() -> AccountKey {
    AccountKey { network: Network::Telegram, account: "1".into() }
}

fn start(path: &Path, at: Option<u32>, rate: u16) -> PlayRequest {
    PlayRequest::Start {
        account: account(),
        file: "d6100".into(),
        path: path.display().to_string(),
        conversation: Key { network: Network::Telegram, account: "1".into(), id: "7002".into() },
        message: "15".into(),
        title: "Mara Okafor".into(),
        at,
        rate,
        output: None,
    }
}

/// mediad processes this test process has spawned.
fn mediads() -> usize {
    #[cfg(windows)]
    {
        // PowerShell's process list takes longer than this one-second clip, so it reported
        // none while mediad was still playing. The snapshot is the process list itself.
        windows_mediads()
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("pgrep").args(["-P", &std::process::id().to_string(), "mediad"]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().lines().count()
    }
}

/// `mediad.exe` whose parent is this test. A toolhelp snapshot, not PowerShell.
#[cfg(windows)]
fn windows_mediads() -> usize {
    use std::ffi::c_void;
    #[repr(C)]
    struct Entry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Process32FirstW(snap: *mut c_void, entry: *mut Entry) -> i32;
        fn Process32NextW(snap: *mut c_void, entry: *mut Entry) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    const SNAPPROCESS: u32 = 2;
    unsafe {
        let snap = CreateToolhelp32Snapshot(SNAPPROCESS, 0);
        assert_ne!(snap as isize, -1, "could not list processes");
        let mut entry: Entry = std::mem::zeroed();
        entry.size = std::mem::size_of::<Entry>() as u32;
        let mut count = 0usize;
        let parent = std::process::id();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry.exe.iter().position(|unit| *unit == 0).unwrap_or(entry.exe.len());
                let name = String::from_utf16_lossy(&entry.exe[..end]);
                if entry.parent == parent && name.eq_ignore_ascii_case("mediad.exe") {
                    count += 1;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        count
    }
}

fn until_no_mediad() {
    let deadline = Instant::now() + PATIENCE;
    while mediads() > 0 {
        assert!(Instant::now() < deadline, "mediad is still running");
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_voice_message_plays_to_the_end_and_leaves_no_process() {
    let _turn = alone();
    let world = world();
    let since = world.hub.seq();
    world.play(None, 100);
    assert_eq!(world.shown().unwrap().state, PlayState::Loading, "told at once, before mediad answers");
    let playing = world.until_state(PlayState::Playing);
    assert!((980..=1010).contains(&playing.duration), "{} ms", playing.duration);
    assert_eq!(mediads(), 1);
    let ended = world.until_state(PlayState::Ended);
    assert_eq!(ended.at, ended.duration);
    until_no_mediad();
    // The shell was told by changes alone, the whole value each time.
    let (_, _, changes) = world.hub.wait(since, Duration::ZERO);
    let told: Vec<&Change> = changes.iter().filter(|change| matches!(change, Change::Playback { .. })).collect();
    assert_eq!(told.len(), 1, "one entry, replaced each time: {told:?}");
    assert!(matches!(told[0], Change::Playback { playback: Some(Playback { state: PlayState::Ended, .. }) }));

    // Play on an ended message starts it again from the top.
    world.play(None, 200);
    let again = world.until_state(PlayState::Playing);
    assert_eq!((again.at, again.rate), (0, 200));
    world.ask(PlayRequest::Stop).unwrap();
    assert_eq!(world.shown(), None);
    until_no_mediad();
}

#[test]
fn a_pause_lets_the_process_go_and_play_goes_on_from_there() {
    let _turn = alone();
    let world = world();
    world.play(None, 100);
    world.until_state(PlayState::Playing);
    sleep(Duration::from_millis(400));
    world.ask(PlayRequest::Pause).unwrap();
    let paused = world.until_state(PlayState::Paused);
    assert!((250..=600).contains(&paused.at), "paused at {}", paused.at);
    until_no_mediad();
    let at = world.shown().unwrap().at;
    assert_eq!(world.shown().unwrap().position(u64::MAX), at, "a paused position does not move");

    // A seek while paused only moves the place; play goes on from it.
    world.ask(PlayRequest::Seek { at: 700 }).unwrap();
    assert_eq!(world.shown().unwrap().at, 700);
    assert_eq!(mediads(), 0);
    world.play(None, 100);
    let resumed = world.until_state(PlayState::Playing);
    assert_eq!(resumed.at, 700);
    world.until_state(PlayState::Ended);
    until_no_mediad();
}

#[test]
fn a_second_file_replaces_the_first_and_speed_and_seek_reach_mediad() {
    let _turn = alone();
    let world = world();
    world.play(None, 100);
    world.until_state(PlayState::Playing);
    world.ask(PlayRequest::Rate { rate: 150 }).unwrap();
    assert_eq!(world.shown().unwrap().rate, 150, "the speed shows at once");
    world.until("mediad at 1.5x", |shown| shown.as_ref().is_some_and(|shown| shown.rate == 150 && shown.state == PlayState::Playing));
    world.ask(PlayRequest::Seek { at: 900 }).unwrap();
    world.until_state(PlayState::Ended);

    let other = world.voice.with_file_name("other");
    std::fs::copy(&world.voice, &other).unwrap();
    world.play(None, 100);
    world.until_state(PlayState::Playing);
    let mut second = start(&other, None, 100);
    if let PlayRequest::Start { file, .. } = &mut second {
        *file = "d6101".into();
    }
    world.ask(second).unwrap();
    let playing = world.until_state(PlayState::Playing);
    assert_eq!(playing.file, "d6101");
    // One file at a time: the first one's mediad went. The second file is a second long, so
    // it may have ended before this looks. None left is that.
    world.until("at most one mediad", |_| mediads() <= 1);
    world.ask(PlayRequest::Stop).unwrap();
    until_no_mediad();
}

#[test]
fn only_a_file_in_the_media_cache_is_played_and_a_bad_one_fails_in_words() {
    let _turn = alone();
    let world = world();
    let outside = world.dir.join("chat.sqlite");
    assert_eq!(world.ask(start(&outside, None, 100)), Err("That file is not in tinyclient's media cache.".to_owned()));
    assert_eq!(world.shown(), None);
    assert_eq!(mediads(), 0);

    let broken = world.voice.with_file_name("broken");
    std::fs::write(&broken, b"not a voice message").unwrap();
    world.ask(start(&broken, None, 100)).unwrap();
    let failed = world.until("a failure", |shown| shown.as_ref().is_some_and(|shown| matches!(shown.state, PlayState::Failed { .. })));
    assert_eq!(failed.unwrap().state, PlayState::Failed { detail: "This file can't be played.".into() });
    until_no_mediad();
}
