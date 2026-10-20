//! The shell keeps chatd running (`protocol::launch`): it starts it when the socket is not
//! there, keeps the last snapshot on screen behind "Reconnecting…" when it goes away, gives up
//! after three starts within 30 s or five within 10 minutes, and reads everything again when `Status.started` changes or a
//! `Wait` answers `reset`. What the window then says is [`Shown`], one of the window states of
//! `polish/01-system.md` §5, in its exact words.

use protocol::{Keychain, Network};
use std::time::{Duration, Instant};

/// Three starts inside this window, and the shell stops starting chatd.
pub const RESTART_WINDOW: Duration = Duration::from_secs(30);
pub const RESTARTS: usize = 3;
/// Five starts inside this longer window also stop it: a chatd that dies a minute after each
/// start never trips the short one, and would otherwise be started again for ever.
pub const RESTART_WINDOW_LONG: Duration = Duration::from_secs(600);
pub const RESTARTS_LONG: usize = 5;
/// A started chatd that has not answered by now is still starting, or stuck.
pub const SLOW: Duration = Duration::from_secs(3);
/// Skeleton rows wait this long before they show.
pub const SKELETON: Duration = Duration::from_millis(150);

/// When the shell started chatd, to count restarts.
#[derive(Default, Debug)]
pub struct Restarts {
    at: Vec<Instant>,
}

impl Restarts {
    /// Whether another start is allowed at `now`; records it when it is.
    pub fn allow(&mut self, now: Instant) -> bool {
        self.at.retain(|at| now.saturating_duration_since(*at) < RESTART_WINDOW_LONG);
        let recent = self.at.iter().filter(|at| now.saturating_duration_since(**at) < RESTART_WINDOW).count();
        if recent >= RESTARTS || self.at.len() >= RESTARTS_LONG {
            return false;
        }
        self.at.push(now);
        true
    }

    /// The user's "Try again": count from nothing.
    pub fn clear(&mut self) {
        self.at.clear();
    }
}

/// How the connection to chatd stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// No answer yet since the window opened, or since the shell started chatd.
    Starting,
    /// chatd answers.
    Live,
    /// The socket went away mid-session; chatd is being started again.
    Reconnecting,
    /// Three starts failed, chatd could not be started, or it is another version.
    Failed,
}

/// Why the shell stopped trying, for the failed banner's tooltip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stopped {
    /// It was started too often in too short a time and kept stopping.
    KeepsStopping,
    /// There is no chatd beside tinyclient, or it would not start.
    CannotStart,
    /// A chatd of another version answers and could not be replaced.
    OtherVersion,
}

impl Stopped {
    pub fn words(self) -> &'static str {
        match self {
            Stopped::KeepsStopping => "tinyclient's background service stopped several times in a few minutes, so it is not being started again. Try again, or copy the diagnostics from the Help menu.",
            Stopped::CannotStart => "tinyclient's background service could not be started. Try again, or reinstall tinyclient.",
            Stopped::OtherVersion => "An older copy of tinyclient's background service is still running and could not be replaced. Quit tinyclient and open it again.",
        }
    }
}

/// What the shell knows of chatd beyond the snapshot.
#[derive(Debug)]
pub struct Service {
    pub phase: Phase,
    /// Since when the phase has held: the 3 s and 150 ms thresholds count from here.
    pub since: Instant,
    pub keychain: Keychain,
    /// `Status.started` as last seen: a new value means chatd restarted.
    pub started: u64,
    pub restarts: Restarts,
    /// The pid of the chatd this shell started, when it did, and when.
    pub pid: Option<u32>,
    pub launched: Option<Instant>,
    /// When the snapshot on screen was read, unix seconds, for "Showing what was saved at …".
    pub saved_at: Option<i64>,
    /// The shell may start chatd: off for `--shot` runs unless asked, so a test leaves nothing.
    pub may_start: bool,
    /// With `Failed`: why.
    pub stopped: Option<Stopped>,
    /// A chatd of another version was asked to make way once already this launch.
    pub replaced: bool,
}

impl Service {
    pub fn new(may_start: bool) -> Service {
        Service {
            phase: Phase::Starting,
            since: Instant::now(),
            keychain: Keychain::default(),
            started: 0,
            restarts: Restarts::default(),
            pid: None,
            launched: None,
            saved_at: None,
            may_start,
            stopped: None,
            replaced: false,
        }
    }

    pub fn enter(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase;
            self.since = Instant::now();
        }
    }

    /// A `Status` arrived. Answers whether chatd restarted since the last one, so everything on
    /// screen must be read again.
    pub fn answered(&mut self, started: u64, keychain: Keychain, now_unix: i64) -> bool {
        let restarted = self.started != 0 && started != 0 && started != self.started;
        self.started = started;
        self.keychain = keychain;
        self.saved_at = Some(now_unix);
        self.enter(Phase::Live);
        restarted
    }
}

/// Stops a chatd of another version that holds the socket, so the matching one can be started.
/// Only the process at the other end of the socket is stopped, and only when it runs as this
/// user and its program is a chatd. Waits at most 3 s for it to go. Answers whether the socket
/// is free now. The caller asks once per launch (or per "Try again"), never in a loop.
pub fn stop_other_version() -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::net::UnixStream;
        let Ok(stream) = UnixStream::connect(protocol::chat_sock_path()) else {
            return true;
        };
        let Some((pid, uid)) = peer::of(&stream) else {
            return false;
        };
        drop(stream);
        // SAFETY: plain system calls on integers.
        let ours = uid == unsafe { libc::getuid() } && pid > 1 && pid as u32 != std::process::id();
        if !ours || !peer::is_chatd(pid) {
            return false;
        }
        // SAFETY: as above. SIGTERM ends chatd; its lock and socket go with the process.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            return false;
        }
        for _ in 0..30 {
            // SAFETY: signal 0 only asks whether the process is there.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod peer {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    /// The pid and user id of the process at the other end of a local socket.
    #[cfg(target_os = "macos")]
    pub fn of(stream: &UnixStream) -> Option<(libc::pid_t, libc::uid_t)> {
        let fd = stream.as_raw_fd();
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: the out-pointers are valid for the sizes given.
        let found = unsafe {
            libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len) == 0
                && libc::getpeereid(fd, &mut uid, &mut gid) == 0
        };
        found.then_some((pid, uid))
    }

    #[cfg(target_os = "linux")]
    pub fn of(stream: &UnixStream) -> Option<(libc::pid_t, libc::uid_t)> {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the out-pointer is valid for the size given.
        let found = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len) == 0 };
        found.then_some((cred.pid, cred.uid))
    }

    /// Whether the process's program is named chatd.
    pub fn is_chatd(pid: libc::pid_t) -> bool {
        program(pid).is_some_and(|path| path.file_stem().is_some_and(|name| name == "chatd"))
    }

    #[cfg(target_os = "macos")]
    fn program(pid: libc::pid_t) -> Option<std::path::PathBuf> {
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is as long as the size given.
        let len = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        (len > 0).then(|| std::path::PathBuf::from(String::from_utf8_lossy(&buffer[..len as usize]).into_owned()))
    }

    #[cfg(target_os = "linux")]
    fn program(pid: libc::pid_t) -> Option<std::path::PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
}

/// The window states and their words (`polish/01-system.md` §5).
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// Nothing to say: live, or within the first 3 s of starting.
    Quiet,
    /// Skeleton rows, no words: the first 3 s, after 150 ms.
    Skeleton,
    /// Still no answer at 3 s.
    Starting,
    /// macOS is showing its keychain prompt.
    KeychainWaiting,
    /// The keychain refused or is locked: "Try again" sends `ReadSessions`.
    KeychainDenied(Vec<Network>),
    /// The last snapshot stays on screen behind this.
    Reconnecting,
    /// Not receiving: "Try again"; Help › Copy Diagnostics.
    Failed,
}

/// What the window says, from the phase, how long it has held, and the keychain.
pub fn shown(service: &Service, now: Instant) -> Shown {
    let held = now.saturating_duration_since(service.since);
    let keychain = &service.keychain;
    match service.phase {
        Phase::Failed => Shown::Failed,
        Phase::Reconnecting => Shown::Reconnecting,
        Phase::Live if keychain.waiting.is_some() => Shown::KeychainWaiting,
        Phase::Live if !keychain.denied.is_empty() => Shown::KeychainDenied(keychain.denied.clone()),
        Phase::Live => Shown::Quiet,
        // chatd answers before it reads the keychain, so only its own `Status.keychain` says it
        // waits on macOS; no answer at 3 s is still starting.
        Phase::Starting if held >= SLOW => Shown::Starting,
        Phase::Starting if held >= SKELETON => Shown::Skeleton,
        Phase::Starting => Shown::Quiet,
    }
}

impl Shown {
    /// The sidebar banner's words, when there is one.
    pub fn banner(&self) -> Option<&'static str> {
        match self {
            Shown::Quiet | Shown::Skeleton => None,
            Shown::Starting => Some("Starting tinyclient…"),
            Shown::KeychainWaiting => Some(crate::host::STORE_WAITING),
            Shown::KeychainDenied(_) => None,
            Shown::Reconnecting => Some("Reconnecting…"),
            Shown::Failed => Some("Not receiving messages."),
        }
    }

    /// The conversation region's empty state, when this state takes it: glyph, title, line.
    pub fn empty_state(&self) -> Option<(&'static str, &'static str, &'static str)> {
        match self {
            Shown::KeychainWaiting => Some(("icons/key.svg", crate::host::STORE_WAITING, crate::host::STORE_WAITING_LINE)),
            Shown::KeychainDenied(_) => Some((
                "icons/key.svg",
                "tinyclient can't read its sign-ins",
                "Your accounts are still signed in. Allow access to continue.",
            )),
            _ => None,
        }
    }
}

/// "Showing what was saved at 10:42", the reconnecting banner's tooltip.
pub fn saved_words(clock: &str) -> String {
    format!("Showing what was saved at {clock}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_starts_in_thirty_seconds_and_then_no_more_until_the_window_passes() {
        let t0 = Instant::now();
        let mut restarts = Restarts::default();
        assert!(restarts.allow(t0));
        assert!(restarts.allow(t0 + Duration::from_secs(5)));
        assert!(restarts.allow(t0 + Duration::from_secs(10)));
        assert!(!restarts.allow(t0 + Duration::from_secs(20)), "a fourth start within 30 s is refused");
        // Once the first start is 30 s old there is room for one more.
        assert!(restarts.allow(t0 + Duration::from_secs(30)));
        assert!(!restarts.allow(t0 + Duration::from_secs(31)));
        restarts.clear();
        assert!(restarts.allow(t0 + Duration::from_secs(32)));
    }

    #[test]
    fn a_chatd_that_dies_a_minute_after_each_start_is_started_five_times_in_ten_minutes() {
        let t0 = Instant::now();
        let mut restarts = Restarts::default();
        let minute = Duration::from_secs(60);
        for n in 0..5 {
            assert!(restarts.allow(t0 + minute * n), "start {n}");
        }
        assert!(!restarts.allow(t0 + minute * 5), "a sixth start within 10 minutes is refused");
        assert!(!restarts.allow(t0 + minute * 9));
        // Once the first is 10 minutes old, one more.
        assert!(restarts.allow(t0 + minute * 10));
        assert!(Stopped::KeepsStopping.words().ends_with('.') && !Stopped::KeepsStopping.words().contains("chatd"));
    }

    #[test]
    fn the_window_says_nothing_for_three_seconds_then_names_the_wait() {
        let mut service = Service::new(true);
        let t0 = service.since;
        assert_eq!(shown(&service, t0), Shown::Quiet);
        assert_eq!(shown(&service, t0 + Duration::from_millis(200)), Shown::Skeleton);
        assert_eq!(shown(&service, t0 + SLOW), Shown::Starting);
        assert_eq!(Shown::Starting.banner(), Some("Starting tinyclient…"));
        // We started it and it holds still: still starting. Only chatd says it waits on macOS.
        service.pid = Some(42);
        assert_eq!(shown(&service, t0 + SLOW), Shown::Starting);

        // chatd says so itself.
        let waiting = Keychain { reading: true, waiting: Some(Network::Slack), denied: vec![] };
        assert!(!service.answered(100, waiting, 0));
        assert_eq!(shown(&service, Instant::now()), Shown::KeychainWaiting);
        let (_, title, line) = Shown::KeychainWaiting.empty_state().unwrap();
        assert_eq!(title, crate::host::STORE_WAITING);
        assert_eq!(line, crate::host::STORE_WAITING_LINE);

        let denied = Keychain { denied: vec![Network::Discord], ..Keychain::default() };
        service.answered(100, denied, 0);
        assert_eq!(shown(&service, Instant::now()), Shown::KeychainDenied(vec![Network::Discord]));
        service.answered(100, Keychain::default(), 0);
        assert_eq!(shown(&service, Instant::now()), Shown::Quiet);
    }

    #[test]
    fn a_new_start_time_means_everything_is_read_again() {
        let mut service = Service::new(true);
        assert!(!service.answered(100, Keychain::default(), 0), "the first answer is not a restart");
        assert!(!service.answered(100, Keychain::default(), 0));
        assert!(service.answered(200, Keychain::default(), 0));
        // An old chatd that does not say when it started never reads as restarted.
        assert!(!service.answered(0, Keychain::default(), 0));
        service.enter(Phase::Reconnecting);
        assert_eq!(shown(&service, Instant::now()).banner(), Some("Reconnecting…"));
        service.enter(Phase::Failed);
        assert_eq!(shown(&service, Instant::now()).banner(), Some("Not receiving messages."));
    }
}
