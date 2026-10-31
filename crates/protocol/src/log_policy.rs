//! Local logs. One render path in front of the operator file and the in-memory ring, so a field
//! name or a secret in a message cannot reach a sink the redactor would have dropped.
//!
//! `RUST_LOG` is not read. The ceiling is [`LogPolicy`], from `TINYCLIENT_LOG_*` until a stored
//! setting replaces it.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

use crate::redact;

const VALUE_LIMIT: usize = 300;
const RING_LIMIT: usize = 2000;
const RING_TTL: Duration = Duration::from_secs(60 * 60);
const ROLL_BYTES: u64 = 2 * 1024 * 1024;
const BUNDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const DENY_EXACT: &[&str] = &[
    "body",
    "content",
    "plaintext",
    "text",
    "caption",
    "filename",
    "file_name",
    "media",
    "ciphertext",
    "password",
    "authorization",
    "cookie",
    "token",
    "secret",
    "private_key",
    "invite",
    "oauth_code",
    "code",
    "verifier",
    "nonce",
    "refresh_token",
    "id_token",
    "access_token",
    "key",
    "ice_pwd",
    "fingerprint",
    "sdp",
    "email",
    "display_name",
    "subject",
    "phone",
    "phone_number",
    "vault",
    "sig",
    "detail",
];

const DENY_SUFFIX: &[&str] = &["_token", "_secret", "_password", "_cookie", "_key"];

const FIRST_PARTY: &[&str] = &["tinyclient", "chatd", "mediad", "login", "protocol"];
const DEPENDENCIES: &[&str] = &["reqwest", "hyper", "h2", "sqlx", "tower", "str0m"];

static SESSION: Mutex<Option<&'static str>> = Mutex::new(None);
static GUARD: OnceLock<&'static LogGuard> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct LogPolicy {
    operator: Level,
    ring: bool,
    export: bool,
}

impl Default for LogPolicy {
    fn default() -> Self {
        Self {
            operator: Level::INFO,
            ring: true,
            export: true,
        }
    }
}

impl LogPolicy {
    pub fn from_env() -> Self {
        Self::policy_from_values(
            env_nonempty("TINYCLIENT_LOG_LEVEL").as_deref(),
            env_nonempty("TINYCLIENT_LOG_RING").as_deref(),
            env_nonempty("TINYCLIENT_LOG_EXPORT").as_deref(),
        )
    }

    /// `None` and unrecognised text are the defaults. `RUST_LOG` is not an input.
    pub fn policy_from_values(level: Option<&str>, ring: Option<&str>, export: Option<&str>) -> Self {
        let mut policy = Self::default();
        if let Some(level) = level.and_then(parse_level) {
            policy.operator = level;
        }
        if let Some(ring) = ring.and_then(parse_switch) {
            policy.ring = ring;
        }
        if let Some(export) = export.and_then(parse_switch) {
            policy.export = export;
        }
        policy
    }

    /// A stored setting. Empty text and garbage are the defaults.
    pub fn apply_settings(level: &str, ring: &str, export: &str) -> Self {
        fn given(value: &str) -> Option<&str> {
            let value = value.trim();
            (!value.is_empty()).then_some(value)
        }
        Self::policy_from_values(given(level), given(ring), given(export))
    }

    pub fn operator_level(&self) -> Level {
        self.operator
    }

    pub fn ring_enabled(&self) -> bool {
        self.ring
    }

    pub fn export_enabled(&self) -> bool {
        self.export
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn parse_level(value: &str) -> Option<Level> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "error" => Level::ERROR,
        "warn" => Level::WARN,
        "info" => Level::INFO,
        "debug" => Level::DEBUG,
        "trace" => Level::TRACE,
        _ => return None,
    })
}

fn parse_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Some(true),
        "off" | "0" | "false" => Some(false),
        _ => None,
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sink {
    Operator,
    Ring,
}

pub struct EventParts<'a> {
    pub target: &'a str,
    pub level: Level,
    pub message: &'a str,
    pub fields: &'a [(&'a str, &'a str)],
}

pub fn target_ceiling(policy: &LogPolicy, sink: Sink, target: &str) -> Level {
    if is_target(target, DEPENDENCIES) {
        return Level::WARN;
    }
    if is_target(target, FIRST_PARTY) {
        return match sink {
            Sink::Operator => policy.operator,
            Sink::Ring if policy.ring => Level::DEBUG,
            Sink::Ring => policy.operator,
        };
    }
    Level::WARN
}

fn is_target(target: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        target == *name || target.strip_prefix(*name).is_some_and(|rest| rest.starts_with("::"))
    })
}

pub fn render(policy: &LogPolicy, sink: Sink, event: &EventParts<'_>) -> Option<String> {
    let ceiling = target_ceiling(policy, sink, event.target);
    if event.level > ceiling {
        return None;
    }
    let mut line = format!(
        "{} {} {}",
        level_name(event.level),
        event.target,
        cut(&redact(event.message))
    );
    for (name, value) in event.fields {
        if drop_field(name) {
            continue;
        }
        line.push(' ');
        line.push_str(&name.to_ascii_lowercase());
        line.push('=');
        line.push_str(cut(&redact(value)));
    }
    Some(line)
}

fn drop_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name == "peer" {
        return true;
    }
    if DENY_EXACT.contains(&name.as_str()) {
        return true;
    }
    DENY_SUFFIX.iter().any(|suffix| name.ends_with(suffix))
}

fn cut(text: &str) -> &str {
    if text.len() <= VALUE_LIMIT {
        return text;
    }
    let mut end = VALUE_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 32 hex characters shared by the window, chatd, and mediad. An invalid `TINYCLIENT_SESSION`
/// is ignored and a new id is generated.
pub fn session_id() -> &'static str {
    let mut slot = SESSION.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(id) = *slot {
        return id;
    }
    let id = load_session();
    let leaked: &'static str = Box::leak(id.into_boxed_str());
    *slot = Some(leaked);
    leaked
}

/// The window takes chatd's id when `Status.session` is non-empty, including a chatd that
/// systemd started without this process's environment.
pub fn adopt_session(id: &str) {
    if !is_session(id) {
        return;
    }
    let mut slot = SESSION.lock().unwrap_or_else(|err| err.into_inner());
    if slot.is_some_and(|current| current == id) {
        return;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    *slot = Some(leaked);
}

fn load_session() -> String {
    if let Some(env) = env_nonempty("TINYCLIENT_SESSION") {
        if is_session(&env) {
            return env;
        }
    }
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        bytes = ticks.to_le_bytes();
    }
    hex_encode(&bytes)
}

fn is_session(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_encode(bytes: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

pub fn logs_dir() -> PathBuf {
    crate::support_dir().join("logs")
}

fn generation(path: &Path, n: u8) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{n}"));
    path.with_file_name(name)
}

fn rotate_log(path: &Path) -> io::Result<()> {
    let one = generation(path, 1);
    let two = generation(path, 2);
    if two.exists() {
        fs::remove_file(&two)?;
    }
    if one.exists() {
        fs::rename(&one, &two)?;
    }
    if path.exists() {
        fs::rename(path, &one)?;
    }
    Ok(())
}

/// Appends `bytes`. When the live file is non-empty and this write would pass 2 MiB, the live
/// file becomes `.1`, the previous `.1` becomes `.2`, and the write starts a new live file.
pub fn append_rolled(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if len > 0 && len.saturating_add(bytes.len() as u64) > ROLL_BYTES {
        rotate_log(path)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

struct LogFile {
    path: PathBuf,
    file: Option<File>,
    len: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        Ok(Self {
            path,
            file: Some(file),
            len,
        })
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let extra = (line.len() + 1) as u64;
        if self.len > 0 && self.len.saturating_add(extra) > ROLL_BYTES {
            if let Some(file) = self.file.take() {
                drop(file);
            }
            rotate_log(&self.path)?;
            self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
            self.len = 0;
        }
        let file = match self.file.as_mut() {
            Some(file) => file,
            None => {
                self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
                self.len = 0;
                match self.file.as_mut() {
                    Some(file) => file,
                    None => return Err(io::Error::other("log file did not open")),
                }
            }
        };
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        self.len = self.len.saturating_add(extra);
        Ok(())
    }
}

#[derive(Default)]
struct Ring {
    lines: VecDeque<(SystemTime, String)>,
}

impl Ring {
    fn push(&mut self, now: SystemTime, line: String) {
        self.lines
            .retain(|(then, _)| now.duration_since(*then).unwrap_or_default() < RING_TTL);
        self.lines.push_back((now, line));
        while self.lines.len() > RING_LIMIT {
            self.lines.pop_front();
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for (_, line) in &self.lines {
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }
}

pub fn export_lines(dir: &Path, name: &str, lines: &str, enabled: bool) -> io::Result<Option<PathBuf>> {
    if !enabled || name.is_empty() || name.contains(['/', '\\']) {
        return Ok(None);
    }
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let mut file = OpenOptions::new().create_new(true).write(true).open(&path)?;
    file.write_all(lines.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(Some(path))
}

pub fn sweep_dir(dir: &Path, ttl: Duration, now: SystemTime) -> io::Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.ends_with(".log") {
            continue;
        }
        let Some(age) = bundle_age(&name, now) else {
            continue;
        };
        if age > ttl {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn bundle_age(name: &str, now: SystemTime) -> Option<Duration> {
    let stem = name.strip_suffix(".log")?;
    let stamp = stem.rsplit(['-', '.']).next()?;
    let secs = stamp.parse::<u64>().ok()?;
    now.duration_since(UNIX_EPOCH + Duration::from_secs(secs)).ok()
}

struct Recorder {
    message: String,
    fields: Vec<(String, String)>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
        }
    }

    fn store(&mut self, name: &str, value: String) {
        if name == "message" {
            self.message = value;
        } else {
            self.fields.push((name.to_owned(), value));
        }
    }
}

impl tracing::field::Visit for Recorder {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.store(field.name(), value.to_owned());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let mut text = format!("{value:?}");
        if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
            text = text[1..text.len() - 1].to_owned();
        }
        self.store(field.name(), text);
    }
}

struct RedactLayer {
    policy: Arc<Mutex<LogPolicy>>,
    sink: Sink,
    ring: Arc<Mutex<Ring>>,
    file: Arc<Mutex<Option<LogFile>>>,
    stderr: bool,
}

impl<S> Layer<S> for RedactLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let policy = match self.policy.lock() {
            Ok(policy) => policy.clone(),
            Err(err) => err.into_inner().clone(),
        };
        let mut recorder = Recorder::new();
        event.record(&mut recorder);
        let borrowed: Vec<(&str, &str)> = recorder
            .fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let parts = EventParts {
            target: event.metadata().target(),
            level: *event.metadata().level(),
            message: &recorder.message,
            fields: &borrowed,
        };
        let Some(mut line) = render(&policy, self.sink, &parts) else {
            return;
        };
        line.push_str(" session=");
        line.push_str(session_id());
        match self.sink {
            Sink::Operator => {
                if let Some(file) = self.file.lock().unwrap_or_else(|err| err.into_inner()).as_mut() {
                    let _ = file.write_line(&line);
                }
                if self.stderr {
                    let _ = writeln!(io::stderr(), "{line}");
                }
            }
            Sink::Ring if policy.ring => {
                let mut ring = self.ring.lock().unwrap_or_else(|err| err.into_inner());
                ring.push(SystemTime::now(), line);
            }
            Sink::Ring => {}
        }
    }
}

pub struct LogGuard {
    process: &'static str,
    policy: Arc<Mutex<LogPolicy>>,
    ring: Arc<Mutex<Ring>>,
}

impl LogGuard {
    /// Replaces the ceilings and the export flag. The open file stays open.
    pub fn apply(&self, policy: LogPolicy) {
        *self.policy.lock().unwrap_or_else(|err| err.into_inner()) = policy;
    }

    /// Writes the ring when export is on. `Ok(None)` writes nothing.
    pub fn export_bundle(&self) -> io::Result<Option<PathBuf>> {
        let policy = self.policy.lock().unwrap_or_else(|err| err.into_inner()).clone();
        if !policy.export || !policy.ring {
            return Ok(None);
        }
        let text = self.ring.lock().unwrap_or_else(|err| err.into_inner()).text();
        let dir = logs_dir().join("bundles");
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let name = format!("{}-{}-{stamp}.log", session_id(), self.process);
        let written = export_lines(&dir, &name, &text, true)?;
        let _ = sweep_dir(&dir, BUNDLE_TTL, SystemTime::now());
        Ok(written)
    }
}

fn filter_for(_policy: &LogPolicy) -> EnvFilter {
    // Install runs before the database opens, and try_init cannot replace
    // this filter. First-party stays at trace so a later settings change
    // can raise the ceiling. render() drops what the ceiling does not allow.
    // Dependencies stay warn even if the ceiling is trace.
    let mut directive =
        "warn,tinyclient=trace,chatd=trace,mediad=trace,login=trace,protocol=trace".to_owned();
    for name in DEPENDENCIES {
        directive.push_str(&format!(",{name}=warn"));
    }
    EnvFilter::try_new(directive).unwrap_or_else(|_| EnvFilter::new("warn"))
}

fn stderr_too() -> bool {
    io::stderr().is_terminal() || env_nonempty("JOURNAL_STREAM").is_some()
}

/// Installs the process subscriber. Call once, after `--version` and before other work.
/// A second call returns the guard already installed.
pub fn install(process: &'static str, policy: LogPolicy) -> &'static LogGuard {
    if let Some(existing) = GUARD.get() {
        return existing;
    }
    let policy = Arc::new(Mutex::new(policy));
    let ring = Arc::new(Mutex::new(Ring::default()));
    let path = logs_dir().join(format!("{process}.log"));
    let file = Arc::new(Mutex::new(LogFile::open(path).ok()));
    let stderr = stderr_too();
    let operator = RedactLayer {
        policy: Arc::clone(&policy),
        sink: Sink::Operator,
        ring: Arc::clone(&ring),
        file: Arc::clone(&file),
        stderr,
    };
    let memory = RedactLayer {
        policy: Arc::clone(&policy),
        sink: Sink::Ring,
        ring: Arc::clone(&ring),
        file: Arc::clone(&file),
        stderr: false,
    };
    let rust_log = env_nonempty("RUST_LOG").is_some();
    let _ = tracing_subscriber::registry()
        .with(filter_for(&policy.lock().unwrap_or_else(|err| err.into_inner())))
        .with(operator)
        .with(memory)
        .try_init();
    let span = Box::leak(Box::new(tracing::info_span!("process", process, session = session_id())));
    let _entered = Box::leak(Box::new(span.enter()));
    if rust_log {
        tracing::info!("RUST_LOG is ignored; set TINYCLIENT_LOG_LEVEL");
    }
    let guard = Box::leak(Box::new(LogGuard { process, policy, ring }));
    let _ = GUARD.set(guard);
    guard
}

pub fn current() -> Option<&'static LogGuard> {
    GUARD.get().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(value: Option<String>) -> String {
        match value {
            Some(value) => value,
            None => panic!("expected a log line"),
        }
    }

    fn io_of<T>(value: io::Result<T>) -> T {
        match value {
            Ok(value) => value,
            Err(error) => panic!("io error: {error}"),
        }
    }

    #[test]
    fn drops_body_and_access_token_keeps_user_id() {
        let policy = LogPolicy::default();
        let event = EventParts {
            target: "chatd",
            level: Level::INFO,
            message: "message failed",
            fields: &[("body", "hello"), ("access_token", "z"), ("user_id", "018f")],
        };
        let line = line_of(render(&policy, Sink::Operator, &event));
        assert!(!line.contains("hello"));
        assert!(!line.contains("access_token"));
        assert!(line.contains("user_id=018f"));
    }

    #[test]
    fn redacts_bearer_pem_jwt_ice_and_userinfo() {
        let policy = LogPolicy::default();
        let message = "Bearer abcdef -----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY----- eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.aaaa.bbbb a=ice-pwd:secret https://user:pass@host/ xoxc-123";
        let event = EventParts {
            target: "chatd",
            level: Level::WARN,
            message,
            fields: &[("error", "code=abc")],
        };
        let line = line_of(render(&policy, Sink::Operator, &event));
        for raw in ["abcdef", "-----BEGIN", "eyJhbGci", "ice-pwd:secret", "user:pass", "xoxc-123", "code=abc"] {
            assert!(!line.contains(raw), "{raw} still in {line}");
        }
        assert!(line.contains("[redacted]"));
    }

    #[test]
    fn default_ceiling_and_dependency_cap() {
        let policy = LogPolicy::default();
        let debug = EventParts {
            target: "chatd",
            level: Level::DEBUG,
            message: "detail",
            fields: &[],
        };
        assert!(render(&policy, Sink::Operator, &debug).is_none());
        assert!(render(&policy, Sink::Ring, &debug).is_some());
        let trace = LogPolicy::policy_from_values(Some("trace"), None, None);
        let reqwest = EventParts {
            target: "reqwest::connect",
            level: Level::DEBUG,
            message: "body",
            fields: &[],
        };
        let ice = EventParts {
            target: "str0m::ice",
            level: Level::DEBUG,
            message: "addr",
            fields: &[],
        };
        assert!(render(&trace, Sink::Operator, &reqwest).is_none());
        assert!(render(&trace, Sink::Ring, &ice).is_none());
    }

    #[test]
    fn subscriber_filter_admits_first_party_trace_and_caps_dependencies() {
        // Settings can raise the ceiling after install. The EnvFilter stays at
        // trace for our crates; render() is the live gate. Dependencies stay warn.
        let quiet = LogPolicy::policy_from_values(Some("error"), Some("off"), Some("off"));
        let directive = filter_for(&quiet).to_string();
        for name in ["tinyclient", "chatd", "mediad", "login", "protocol"] {
            assert!(directive.contains(&format!("{name}=trace")), "{directive}");
        }
        for name in DEPENDENCIES {
            assert!(directive.contains(&format!("{name}=warn")), "{directive}");
        }
    }

    #[test]
    fn peer_is_dropped() {
        let policy = LogPolicy::policy_from_values(Some("debug"), Some("on"), None);
        let event = EventParts {
            target: "mediad",
            level: Level::DEBUG,
            message: "media",
            fields: &[("peer", "203.0.113.5:50000")],
        };
        let line = line_of(render(&policy, Sink::Ring, &event));
        assert!(!line.contains("peer="));
        assert!(!line.contains("203.0.113.5"));
    }

    #[test]
    fn key_id_survives_and_long_values_cut_at_300() {
        let policy = LogPolicy::default();
        let long = "a".repeat(500);
        let event = EventParts {
            target: "chatd",
            level: Level::INFO,
            message: "ok",
            fields: &[("key_id", "kid"), ("error", &long)],
        };
        let line = line_of(render(&policy, Sink::Operator, &event));
        assert!(line.contains("key_id=kid"));
        let error = match line.split("error=").nth(1) {
            Some(error) => error,
            None => panic!("expected an error field"),
        };
        assert_eq!(error.len(), 300);
    }

    #[test]
    fn policy_values_ignore_garbage_and_rust_log() {
        let debug = LogPolicy::policy_from_values(Some("debug"), None, None);
        assert_eq!(debug.operator_level(), Level::DEBUG);
        let garbage = LogPolicy::policy_from_values(Some("nope"), Some("nope"), Some("nope"));
        assert_eq!(garbage.operator_level(), Level::INFO);
        assert!(garbage.ring_enabled());
        assert!(garbage.export_enabled());
        let off = LogPolicy::policy_from_values(Some("info"), Some("off"), Some("off"));
        assert!(!off.ring_enabled());
        assert!(!off.export_enabled());
    }

    #[test]
    fn log_settings_default_and_reject_garbage() {
        assert_eq!(LogPolicy::apply_settings("", "", "").operator_level(), Level::INFO);
        assert_eq!(LogPolicy::apply_settings("nope", "nope", "nope").operator_level(), Level::INFO);
        let set = LogPolicy::apply_settings("debug", "off", "off");
        assert_eq!(set.operator_level(), Level::DEBUG);
        assert!(!set.ring_enabled());
        assert!(!set.export_enabled());
    }

    #[test]
    fn rolls_at_two_mebibytes() {
        let dir = std::env::temp_dir().join(format!("tiny-roll-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("chatd.log");
        io_of(append_rolled(&path, &vec![b'a'; ROLL_BYTES as usize]));
        io_of(append_rolled(&path, b"b"));
        assert!(generation(&path, 1).is_file());
        assert!(fs::metadata(&path).map(|meta| meta.len()).unwrap_or(u64::MAX) < ROLL_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_logs_round_trip() {
        let request = serde_json::to_string(&crate::Request::SaveLogs).expect("request");
        let back: crate::Request = serde_json::from_str(&request).expect("request back");
        assert!(matches!(back, crate::Request::SaveLogs));
        let response = crate::Response::LogsSaved { paths: vec![] };
        let text = serde_json::to_string(&response).expect("response");
        let back: crate::Response = serde_json::from_str(&text).expect("response back");
        assert!(matches!(back, crate::Response::LogsSaved { paths } if paths.is_empty()));
    }

    #[test]
    fn status_without_session_still_parses() {
        let status: crate::Response = serde_json::from_str(
            r#"{"op":"status","networks":[],"accounts":[],"media":"stopped","call":null,"seq":3}"#,
        )
        .expect("status");
        match status {
            crate::Response::Status { session, .. } => assert!(session.is_empty()),
            _ => panic!("status"),
        }
    }

    #[test]
    fn export_off_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("tiny-bundle-off-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = io_of(export_lines(&dir, "ignored.log", "info chatd hello\n", false));
        assert!(path.is_none());
        assert!(!dir.exists());
    }
}
