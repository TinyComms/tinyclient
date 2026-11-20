//! Screen lock. The window covers itself after it has been left alone, or when the
//! screen locks. Messages and sign-ins stay with this computer's account, as they do
//! with the lock off. This does not encrypt them.

use protocol::device_approval::{Outcome, API_UNAVAILABLE_EXIT};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Shown under the lock, so the window does not claim more than a cover.
pub const CAPTION: &str = "This covers the window. Messages and sign-ins stay with this computer's account, the same as before. The lock does not encrypt them.";

/// How long the window may sit before it covers itself. Off never does, on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Away {
    #[default]
    Off,
    Minute,
    Five,
    Hour,
}

impl Away {
    pub fn seconds(self) -> Option<u64> {
        match self {
            Away::Off => None,
            Away::Minute => Some(60),
            Away::Five => Some(5 * 60),
            Away::Hour => Some(60 * 60),
        }
    }

    fn word(self) -> &'static str {
        match self {
            Away::Off => "off",
            Away::Minute => "1m",
            Away::Five => "5m",
            Away::Hour => "1h",
        }
    }

    fn from_word(word: &str) -> Away {
        match word {
            "1m" => Away::Minute,
            "5m" => Away::Five,
            "1h" => Away::Hour,
            _ => Away::Off,
        }
    }
}

/// What Settings › Privacy remembers. The passphrase, on Linux, is only a hash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub away: Away,
    pub on_screen_lock: bool,
    pub salt: String,
    pub hash: String,
}

impl Policy {
    /// A lock that would come on. Linux needs a passphrase first, so a missed prompt
    /// cannot leave the window with no way back in.
    pub fn can_arm(&self) -> bool {
        let chosen = self.away != Away::Off || self.on_screen_lock;
        chosen && !(cfg!(target_os = "linux") && self.hash.is_empty())
    }

    pub fn set_passphrase(&mut self, phrase: &str) {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            self.salt.clear();
            self.hash.clear();
            return;
        }
        self.salt = fresh_salt();
        self.hash = digest(&self.salt, phrase);
    }

    pub fn passphrase_matches(&self, phrase: &str) -> bool {
        !self.hash.is_empty() && digest(&self.salt, phrase.trim()) == self.hash
    }

    pub(crate) fn write_lines(&self) -> String {
        let mut text = String::new();
        if self.away != Away::Off {
            text.push_str(&format!("lock_away\t{}\n", self.away.word()));
        }
        if self.on_screen_lock {
            text.push_str("lock_screen\ton\n");
        }
        if !self.salt.is_empty() {
            text.push_str(&format!("lock_salt\t{}\n", self.salt));
        }
        if !self.hash.is_empty() {
            text.push_str(&format!("lock_hash\t{}\n", self.hash));
        }
        text
    }

    pub(crate) fn read_away(&mut self, word: &str) {
        self.away = Away::from_word(word);
    }

    pub(crate) fn read_screen(&mut self, word: Option<&str>) {
        self.on_screen_lock = word == Some("on");
    }

    pub(crate) fn read_salt(&mut self, word: &str) {
        if word.chars().all(|c| c.is_ascii_hexdigit()) {
            self.salt = word.to_owned();
        }
    }

    pub(crate) fn read_hash(&mut self, word: &str) {
        if word.chars().all(|c| c.is_ascii_hexdigit()) && word.len() == 64 {
            self.hash = word.to_owned();
        }
    }
}

/// One running window's lock. Tests build their own. The window uses [`install`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub policy: Policy,
    pub held: bool,
    unlocked: bool,
}

impl Session {
    pub fn open(policy: Policy) -> Session {
        Session {
            policy,
            held: false,
            unlocked: true,
        }
    }

    /// A launch with the lock already on covers the window before the first idle minute.
    pub fn at_launch(policy: Policy) -> Session {
        let held = policy.can_arm();
        Session {
            policy,
            held,
            unlocked: !held,
        }
    }

    pub fn tick(&mut self, idle_secs: u64, screen_locked: bool) {
        if self.held || !self.policy.can_arm() {
            return;
        }
        let idle = self
            .policy
            .away
            .seconds()
            .is_some_and(|need| idle_secs >= need);
        let screen = self.policy.on_screen_lock && screen_locked;
        if idle || screen {
            self.held = true;
        }
    }

    pub fn unlock_verified(&mut self) -> bool {
        self.held = false;
        self.unlocked = true;
        true
    }

    pub fn unlock_phrase(&mut self, phrase: &str) -> bool {
        if self.policy.passphrase_matches(phrase) {
            self.held = false;
            self.unlocked = true;
            true
        } else {
            false
        }
    }

    /// What a message notification may say. A ring can still be answered.
    pub fn notice(&self, sender: &str, text: &str) -> Notice {
        if self.held {
            Notice {
                title: "New message",
                sender: String::new(),
                body: String::new(),
                reply: false,
                ring: true,
            }
        } else {
            Notice {
                title: "New message",
                sender: sender.to_owned(),
                body: text.to_owned(),
                reply: true,
                ring: true,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: &'static str,
    pub sender: String,
    pub body: String,
    pub reply: bool,
    pub ring: bool,
}

/// Which phone hides the conversation in the app switcher while the lock is armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phone {
    Android,
    Ios,
    Desktop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitcherCover {
    /// Android `WindowManager.LayoutParams.FLAG_SECURE`.
    FlagSecure,
    /// An opaque view while iOS resigns active.
    CoverView,
}

pub fn switcher_cover(policy: &Policy, phone: Phone) -> Option<SwitcherCover> {
    if !policy.can_arm() {
        return None;
    }
    match phone {
        Phone::Android => Some(SwitcherCover::FlagSecure),
        Phone::Ios => Some(SwitcherCover::CoverView),
        Phone::Desktop => None,
    }
}

/// The lab's screen-lock driver. A build without the cover fails here.
pub(crate) fn prove() -> Result<(), &'static str> {
    if !CAPTION.contains("does not encrypt") {
        return Err("the lock claims more than a cover");
    }
    let mut idle = Session::open(armed(Away::Minute, false));
    idle.tick(59, false);
    if idle.held {
        return Err("idle past a minute did not cover the window or hide the message");
    }
    idle.tick(60, false);
    if !idle.held {
        return Err("idle past a minute did not cover the window or hide the message");
    }
    let note = idle.notice("Ines", "the plan");
    if note.title != "New message" || !note.sender.is_empty() || !note.body.is_empty() || note.reply
    {
        return Err("a locked notification still names the sender or accepts a reply");
    }
    if !note.ring {
        return Err("a ring cannot be answered while the window is locked");
    }
    let mut five = Session::open(armed(Away::Five, false));
    five.tick(299, false);
    five.tick(300, false);
    if !five.held {
        return Err("five minutes away did not cover the window");
    }
    let mut hour = Session::open(armed(Away::Hour, false));
    hour.tick(3599, false);
    if hour.held {
        return Err("an hour away covered the window early");
    }
    hour.tick(3600, false);
    if !hour.held {
        return Err("an hour away did not cover the window");
    }
    let mut screen = Session::open(armed(Away::Off, true));
    screen.tick(0, false);
    if screen.held {
        return Err("the window covered itself with the screen still open");
    }
    screen.tick(0, true);
    if !screen.held {
        return Err("the window stayed open after the screen locked");
    }
    screen.tick(0, false);
    if !screen.held {
        return Err("the window opened itself when the screen did");
    }
    let mut off = Session::open(Policy::default());
    off.tick(10_000, true);
    if off.held {
        return Err("the lock covered the window while it was off");
    }
    if idle.unlock_phrase("nope") || !idle.held {
        return Err("a wrong passphrase unlocked the window");
    }
    if !idle.unlock_phrase("screen-lock") || idle.held {
        return Err("the passphrase did not unlock the window");
    }
    idle.held = true;
    if !idle.unlock_verified() || idle.held {
        return Err("the system prompt did not unlock the window");
    }
    let linux = Policy {
        away: Away::Minute,
        ..Policy::default()
    };
    if cfg!(target_os = "linux") && linux.can_arm() {
        return Err("linux armed the lock with no passphrase");
    }
    if !cfg!(target_os = "linux") && !linux.can_arm() {
        return Err("the lock stayed off after it was turned on");
    }
    let armed = armed(Away::Minute, false);
    if switcher_cover(&armed, Phone::Android) != Some(SwitcherCover::FlagSecure) {
        return Err("android does not hide the window in the app switcher");
    }
    if switcher_cover(&armed, Phone::Ios) != Some(SwitcherCover::CoverView) {
        return Err("ios does not cover the window when it resigns active");
    }
    if switcher_cover(&armed, Phone::Desktop).is_some()
        || switcher_cover(&Policy::default(), Phone::Android).is_some()
    {
        return Err("the app switcher is hidden when the lock is off");
    }
    if super::settings::rows::not_yet("touch id").is_some()
        || !super::settings::rows::search("touch id")
            .iter()
            .any(|row| row.id == "LK")
    {
        return Err("touch id still says there is no lock");
    }
    let _ = idle.unlocked;
    Ok(())
}

fn armed(away: Away, screen: bool) -> Policy {
    let mut policy = Policy {
        away,
        on_screen_lock: screen,
        ..Policy::default()
    };
    policy.set_passphrase("screen-lock");
    policy
}

struct Live {
    session: Session,
    error: String,
    asking: bool,
}

static LIVE: Mutex<Live> = Mutex::new(Live {
    session: Session {
        policy: Policy {
            away: Away::Off,
            on_screen_lock: false,
            salt: String::new(),
            hash: String::new(),
        },
        held: false,
        unlocked: true,
    },
    error: String::new(),
    asking: false,
});
static TOUCHED: Mutex<Option<Instant>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(true);
static SCREEN_LOCKED: AtomicBool = AtomicBool::new(false);
static SCREEN_POLLING: AtomicBool = AtomicBool::new(false);
static SCREEN_POLLED_AT: Mutex<Option<Instant>> = Mutex::new(None);
const SCREEN_POLL_INTERVAL: Duration = Duration::from_secs(1);

fn with_live<T>(body: impl FnOnce(&mut Live) -> T) -> T {
    let mut live = LIVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    body(&mut live)
}

pub fn install(policy: Policy) {
    with_live(|live| {
        live.session = Session::at_launch(policy);
        live.error.clear();
        live.asking = false;
    });
    let _ = TOUCHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(Instant::now());
    publish(held());
    refresh_cover();
}

pub fn set_policy(policy: Policy) {
    let armed_now = policy.can_arm();
    with_live(|live| {
        // Keep `held` when the window is already locked: a policy change (such as Lock > Off or
        // Reset all) never drops a held lock without authentication. The new policy takes effect
        // after unlock.
        live.session.policy = policy;
    });
    if armed_now {
        touch();
    }
    publish(held());
    refresh_cover();
}

pub fn policy() -> Policy {
    with_live(|live| live.session.policy.clone())
}

pub fn held() -> bool {
    with_live(|live| live.session.held)
}

pub fn hides_switcher() -> bool {
    with_live(|live| live.session.held || live.session.policy.can_arm())
}

pub fn asking() -> bool {
    with_live(|live| live.asking)
}

pub fn set_asking(on: bool) {
    with_live(|live| live.asking = on);
}

pub fn unlock_error() -> String {
    with_live(|live| live.error.clone())
}

pub fn set_unlock_error(words: impl Into<String>) {
    with_live(|live| live.error = words.into());
}

pub fn touch() {
    if held() {
        return;
    }
    *TOUCHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
}

fn idle_secs() -> u64 {
    TOUCHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|at| at.elapsed().as_secs())
        .unwrap_or(0)
}

/// Covers the window once the policy says so. Called from the frame.
pub fn tick() {
    let screen = screen_is_locked();
    let changed = with_live(|live| {
        let before = live.session.held;
        live.session.tick(idle_secs(), screen);
        live.session.held != before
    });
    if changed {
        publish(held());
        refresh_cover();
    }
}

pub fn unlock_phrase(phrase: &str) -> bool {
    let ok = with_live(|live| {
        let ok = live.session.unlock_phrase(phrase);
        if ok {
            live.error.clear();
        } else {
            live.error = "That passphrase is not the one you set.".to_owned();
        }
        ok
    });
    if ok {
        touch();
        publish(false);
        refresh_cover();
    }
    ok
}

pub fn unlock_verified() {
    with_live(|live| {
        live.session.unlock_verified();
        live.error.clear();
        live.asking = false;
    });
    touch();
    publish(false);
    refresh_cover();
}

pub fn note_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
    refresh_cover();
}

fn refresh_cover() {
    #[cfg(all(target_os = "ios", feature = "mobile"))]
    crate::mobile::set_cover(hides_switcher() && !ACTIVE.load(Ordering::Relaxed));
}

fn publish(held: bool) {
    let path = protocol::support_dir().join("shell-lock");
    if held {
        if let Err(err) = std::fs::create_dir_all(protocol::support_dir()) {
            tracing::warn!("tinyclient: the lock could not create the support dir: {err}");
        }
        if let Err(err) = std::fs::write(path, b"1\n") {
            tracing::warn!("tinyclient: the lock could not tell chatd: {err}");
        }
    } else {
        let _ = std::fs::remove_file(path);
    }
}

pub fn outcome_words(outcome: Outcome) -> String {
    match outcome {
        Outcome::Verified => String::new(),
        Outcome::NotConfigured => {
            "This computer has no fingerprint, face or PIN set up.".to_owned()
        }
        Outcome::NotPresent => "This computer has no fingerprint or face reader.".to_owned(),
        Outcome::Unsupported => {
            "This system has no fingerprint prompt. Use the passphrase.".to_owned()
        }
    }
}

pub fn accept_outcome(outcome: Outcome) -> bool {
    outcome == Outcome::Verified
}

/// Asks the registered device-approval provider, or the desktop helper beside this program.
pub fn verify_owner() -> Result<Outcome, String> {
    let reason = "Unlock tinyclient";
    if let Some(provider) = protocol::device_approval::provider() {
        return provider.verify(reason, &|| false);
    }
    if cfg!(any(target_os = "ios", target_os = "android")) {
        return Err("This phone could not ask the system to unlock.".to_owned());
    }
    desktop_helper(reason)
}

fn desktop_helper(reason: &str) -> Result<Outcome, String> {
    let Some(path) = helper_path() else {
        return Err("tinyclient could not ask the system to unlock.".to_owned());
    };
    let mut command = std::process::Command::new(path);
    protocol::launch::no_console(&mut command)
        .args(["--device-approval", reason])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| "tinyclient could not ask the system to unlock.".to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("The unlock prompt timed out.".to_owned());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "tinyclient could not ask the system to unlock.".to_owned())?
        {
            if status.code() == Some(API_UNAVAILABLE_EXIT) {
                return Err("This Windows version cannot show the unlock prompt.".to_owned());
            }
            if !status.success() {
                return Err("The system did not unlock tinyclient.".to_owned());
            }
            let mut output = Vec::new();
            child
                .stdout
                .take()
                .ok_or_else(|| "tinyclient could not ask the system to unlock.".to_owned())?
                .take(1025)
                .read_to_end(&mut output)
                .map_err(|_| "tinyclient could not ask the system to unlock.".to_owned())?;
            if output.len() > 1024 {
                return Err("tinyclient could not ask the system to unlock.".to_owned());
            }
            let outcome: Outcome = serde_json::from_slice(&output)
                .map_err(|_| "tinyclient could not ask the system to unlock.".to_owned())?;
            if outcome == Outcome::Unsupported && !cfg!(target_os = "linux") {
                return Err("tinyclient could not ask the system to unlock.".to_owned());
            }
            return Ok(outcome);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn helper_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("TINYCLIENT_LOGIN") {
        return Some(path.into());
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name(format!("tinyclient-login{}", std::env::consts::EXE_SUFFIX)))
}

/// Returns the cached OS screen-lock state and kicks off a background poll at most once per
/// second, so `tick()` (called from `Shell::render`) never blocks the UI thread on `loginctl` or
/// WindowServer XPC.
fn screen_is_locked() -> bool {
    if !policy().on_screen_lock {
        SCREEN_LOCKED.store(false, Ordering::Relaxed);
        return false;
    }
    let due = {
        let mut last = SCREEN_POLLED_AT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready = last.is_none_or(|at| at.elapsed() >= SCREEN_POLL_INTERVAL);
        if ready {
            *last = Some(Instant::now());
        }
        ready
    };
    if due
        && SCREEN_POLLING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        if std::thread::Builder::new()
            .name("tinyclient-lock-poll".into())
            .spawn(|| {
                let locked = os_screen_locked();
                SCREEN_LOCKED.store(locked, Ordering::Relaxed);
                SCREEN_POLLING.store(false, Ordering::Release);
            })
            .is_err()
        {
            SCREEN_POLLING.store(false, Ordering::Release);
        }
    }
    SCREEN_LOCKED.load(Ordering::Relaxed)
}

#[cfg(windows)]
fn os_screen_locked() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> *mut core::ffi::c_void;
        fn CloseDesktop(desktop: *mut core::ffi::c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }
    const DESKTOP_READOBJECTS: u32 = 0x0001;
    const ERROR_ACCESS_DENIED: u32 = 5;
    unsafe {
        let desk = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if !desk.is_null() {
            CloseDesktop(desk);
            false
        } else {
            GetLastError() == ERROR_ACCESS_DENIED
        }
    }
}

#[cfg(target_os = "macos")]
fn os_screen_locked() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> *const core::ffi::c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const core::ffi::c_void);
        fn CFDictionaryGetValue(
            dict: *const core::ffi::c_void,
            key: *const core::ffi::c_void,
        ) -> *const core::ffi::c_void;
        fn CFStringCreateWithCString(
            alloc: *const core::ffi::c_void,
            text: *const i8,
            encoding: u32,
        ) -> *const core::ffi::c_void;
        fn CFBooleanGetValue(value: *const core::ffi::c_void) -> bool;
    }
    const UTF8: u32 = 0x0800_0100;
    unsafe {
        let dict = CGSessionCopyCurrentDictionary();
        if dict.is_null() {
            return false;
        }
        let key =
            CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8);
        let locked = if key.is_null() {
            false
        } else {
            let value = CFDictionaryGetValue(dict, key);
            let locked = !value.is_null() && CFBooleanGetValue(value);
            CFRelease(key);
            locked
        };
        CFRelease(dict);
        locked
    }
}

#[cfg(target_os = "linux")]
fn os_screen_locked() -> bool {
    let Ok(output) = std::process::Command::new("loginctl")
        .args(["show-session", "auto", "-p", "LockedHint"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains("LockedHint=yes")
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn os_screen_locked() -> bool {
    false
}

fn digest(salt: &str, phrase: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update([0]);
    hasher.update(phrase.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fresh_salt() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
        .is_ok()
    {
        return hex(&bytes);
    }
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                alg: *mut core::ffi::c_void,
                buf: *mut u8,
                len: u32,
                flags: u32,
            ) -> i32;
        }
        const SYSTEM: u32 = 2;
        unsafe {
            if BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                SYSTEM,
            ) == 0
            {
                return hex(&bytes);
            }
        }
    }
    let ticks = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    hex(&ticks.to_le_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minute_away_covers_the_window_and_a_message_names_nothing() {
        prove().unwrap();
    }

    #[test]
    fn a_policy_change_does_not_unlock_a_held_session() {
        install(armed(Away::Minute, false));
        assert!(held());
        set_policy(Policy::default());
        assert!(held(), "turning the lock off while held must not drop the cover");
        assert!(hides_switcher(), "switcher cover must stay armed while held");
        unlock_verified();
        assert!(!held());
        assert!(!hides_switcher());
    }
}
