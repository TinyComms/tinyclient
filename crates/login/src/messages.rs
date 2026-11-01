//! Google Messages account sign-in using an isolated installed Chromium browser.
//!
//! Follow https://docs.mau.fi/bridges/go/gmessages/authentication.html: stop at config,
//! disable device-bound sessions, and never reuse the browser profile after extraction.

use serde_json::{Map, Value, json};
use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const START: &str =
    "https://accounts.google.com/AccountChooser?continue=https://messages.google.com/web/config";
const REQUIRED: [&str; 6] = ["SID", "HSID", "SSID", "OSID", "APISID", "SAPISID"];
const OPTIONAL: &str = "__Secure-1PSIDTS";
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub(super) fn run() -> ! {
    let result = acquire();
    match result {
        Ok(session) => {
            println!("{session}");
            std::process::exit(0);
        }
        Err((code, why)) => {
            tracing::warn!("tinyclient-login: {why}");
            std::process::exit(code);
        }
    }
}

type Failure = (i32, &'static str);
fn failed(why: &'static str) -> Failure {
    (super::LOAD_FAILED, why)
}

fn is_config(url: &str) -> bool {
    url.split(['?', '#']).next() == Some("https://messages.google.com/web/config")
}

fn session_cookies(cookies: &Value) -> Option<Map<String, Value>> {
    let mut selected = Map::new();
    for cookie in cookies.as_array()? {
        let domain = cookie["domain"].as_str()?.trim_start_matches('.');
        if !matches!(
            domain,
            "google.com" | "accounts.google.com" | "messages.google.com"
        ) {
            continue;
        }
        let name = cookie["name"].as_str()?;
        let value = cookie["value"].as_str()?;
        if (REQUIRED.contains(&name) || name == OPTIONAL) && !value.is_empty() {
            selected.insert(name.to_owned(), Value::String(value.to_owned()));
        }
    }
    REQUIRED
        .iter()
        .all(|name| selected.contains_key(*name))
        .then_some(selected)
}

fn browser() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(windows)]
    for root in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(root) {
            for relative in [
                "Google/Chrome/Application/chrome.exe",
                "Microsoft/Edge/Application/msedge.exe",
                "Chromium/Application/chrome.exe",
            ] {
                candidates.push(PathBuf::from(&root).join(relative));
            }
        }
    }
    #[cfg(target_os = "macos")]
    for app in [
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "Chromium.app/Contents/MacOS/Chromium",
    ] {
        candidates.push(Path::new("/Applications").join(app));
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join("Applications").join(app));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in [
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
                "microsoft-edge",
                "chrome.exe",
                "msedge.exe",
            ] {
                candidates.push(dir.join(name));
            }
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

struct Browser {
    child: Child,
    profile: PathBuf,
    #[cfg(windows)]
    _job: job::Job,
    cleanup: Option<(Child, std::process::ChildStdin)>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(windows)]
        self._job.terminate();
        // Chrome subprocesses can briefly retain profile files after the main process exits.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some((mut worker, pipe)) = self.cleanup.take() {
            drop(pipe);
            let _ = worker.wait();
        }
    }
}

fn launch(exe: &Path) -> Result<Browser, Failure> {
    // create_dir (rather than create_dir_all) refuses collisions, including pre-existing links.
    let profile = (0..100)
        .find_map(|counter| {
            let path = std::env::temp_dir().join(format!(
                "tinyclient-messages-{}-{}-{counter}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).ok().map(|_| path)
        })
        .ok_or(failed("could not create a private browser profile"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).is_err() {
            let _ = std::fs::remove_dir(&profile);
            return Err(failed("could not protect the private browser profile"));
        }
    }
    let mut command = Command::new(exe);
    command
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--remote-debugging-port=0",
            "--remote-debugging-address=127.0.0.1",
            "--disable-features=DeviceBoundSessions,EnableBoundSessionCredentials",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-mode",
            "--new-window",
            START,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW (browser still shows its GUI).
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&profile);
            return Err(failed("could not start Chrome, Edge or Chromium"));
        }
    };
    #[cfg(windows)]
    let job = match job::Job::attach(&child) {
        Ok(job) => job,
        Err(_) => {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&profile);
            return Err(failed("could not supervise the sign-in browser"));
        }
    };
    let mut browser = Browser {
        child,
        profile,
        #[cfg(windows)]
        _job: job,
        cleanup: None,
    };
    // A separate process owns no credentials and waits for EOF. OS termination of this helper
    // closes the pipe too, so cancel/daemon shutdown still removes the private profile.
    let mut watcher = Command::new(
        std::env::current_exe()
            .map_err(|_| failed("could not locate the browser cleanup helper"))?,
    );
    watcher
        .arg("--messages-browser-cleanup")
        .arg(browser.child.id().to_string())
        .arg(&browser.profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        watcher.creation_flags(0x08000000);
    }
    let mut worker = watcher
        .spawn()
        .map_err(|_| failed("could not start the browser cleanup helper"))?;
    let pipe = worker
        .stdin
        .take()
        .ok_or(failed("could not supervise browser cleanup"))?;
    browser.cleanup = Some((worker, pipe));
    Ok(browser)
}

/// Internal lifetime watcher. Only accepts a direct, non-symlink temporary profile directory.
pub(super) fn cleanup_worker(args: &[String]) {
    if args.len() != 3 {
        return;
    }
    let Ok(pid) = args[1].parse::<u32>() else {
        return;
    };
    let profile = Path::new(&args[2]);
    if profile.parent() != Some(std::env::temp_dir().as_path())
        || !profile
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("tinyclient-messages-"))
        || std::fs::symlink_metadata(profile)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
    {
        return;
    }
    use std::io::Read;
    let _ = std::io::stdin().read(&mut [0u8; 1]);
    #[cfg(unix)]
    unsafe {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // Each sign-in browser has its own process group, including its renderer children.
        if pid > 1 && pid <= i32::MAX as u32 {
            kill(-(pid as i32), 9);
        }
    }
    #[cfg(not(unix))]
    let _ = pid; // Windows kill-on-close Job Object owns the browser tree.
    for _ in 0..100 {
        if !profile.exists() || std::fs::remove_dir_all(profile).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Cdp {
    socket: tungstenite::WebSocket<TcpStream>,
    next: u64,
}
impl Cdp {
    fn command(&mut self, method: &str, params: Value) -> Result<Value, Failure> {
        self.next += 1;
        self.socket
            .send(tungstenite::Message::Text(
                json!({"id":self.next,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .map_err(|_| failed("the sign-in browser disconnected"))?;
        loop {
            let message = self
                .socket
                .read()
                .map_err(|_| failed("the sign-in browser disconnected"))?;
            let tungstenite::Message::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(text.as_ref())
                .map_err(|_| failed("invalid browser response"))?;
            if value["id"].as_u64() != Some(self.next) {
                continue;
            }
            if value.get("error").is_some() {
                return Err(failed("the sign-in browser refused a debugging command"));
            }
            return Ok(value["result"].clone());
        }
    }
}

fn acquire() -> Result<Value, Failure> {
    let exe = browser().ok_or(failed(
        "Google Messages sign-in requires installed Google Chrome, Microsoft Edge or Chromium",
    ))?;
    let mut browser = launch(&exe)?;
    let started = Instant::now();
    let port_file = browser.profile.join("DevToolsActivePort");
    let (port, endpoint) = loop {
        if browser.child.try_wait().ok().flatten().is_some() {
            return Err((super::CLOSED, "Google Messages sign-in was cancelled"));
        }
        if let Ok(contents) = std::fs::read_to_string(&port_file) {
            let mut lines = contents.lines();
            if let (Some(port), Some(endpoint)) = (
                lines.next().and_then(|s| s.parse::<u16>().ok()),
                lines.next(),
            ) {
                if endpoint.starts_with("/devtools/browser/") {
                    break (port, endpoint.to_owned());
                }
            }
        }
        if started.elapsed() > Duration::from_secs(30) {
            return Err(failed(
                "the sign-in browser did not start its local connection",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(5),
    )
    .map_err(|_| failed("could not connect to the sign-in browser"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| failed("could not configure the browser connection"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| failed("could not configure the browser connection"))?;
    let (socket, _) = tungstenite::client(format!("ws://127.0.0.1:{port}{endpoint}"), stream)
        .map_err(|_| failed("could not open the browser debugging connection"))?;
    let mut cdp = Cdp { socket, next: 0 };
    let version = cdp.command("Browser.getVersion", json!({}))?;
    let user_agent = version["userAgent"]
        .as_str()
        .filter(|ua| !ua.is_empty())
        .ok_or(failed("the browser did not report its user agent"))?
        .to_owned();
    let mut config_reached = None;
    loop {
        if started.elapsed() > TIMEOUT {
            return Err((super::CLOSED, "Google Messages sign-in timed out"));
        }
        if browser.child.try_wait().ok().flatten().is_some() {
            return Err((super::CLOSED, "Google Messages sign-in was cancelled"));
        }
        let targets = cdp.command("Target.getTargets", json!({}))?;
        let pages: Vec<_> = targets["targetInfos"]
            .as_array()
            .ok_or(failed("the browser did not report its open pages"))?
            .iter()
            .filter(|t| t["type"] == "page")
            .collect();
        if pages.is_empty() {
            return Err((super::CLOSED, "Google Messages sign-in was cancelled"));
        }
        if pages
            .iter()
            .any(|t| t["url"].as_str().is_some_and(is_config))
        {
            let reached = config_reached.get_or_insert_with(Instant::now);
            let result = cdp.command("Storage.getCookies", json!({}))?;
            if let Some(cookies) = session_cookies(&result["cookies"]) {
                // Close before returning the credentials: this profile must never rotate them.
                let _ = cdp.command("Browser.close", json!({}));
                return Ok(json!({"cookies": cookies, "user_agent": user_agent}));
            }
            if reached.elapsed() > Duration::from_secs(30) {
                return Err((
                    super::NO_TOKEN,
                    "Google sign-in completed without the cookies required by Google Messages",
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(windows)]
mod job {
    use std::{ffi::c_void, os::windows::io::AsRawHandle, process::Child};
    type Handle = *mut c_void;
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_working: usize,
        max_working: usize,
        active: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Extended {
        basic: Basic,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process: usize,
        peak_job: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }
    pub(super) struct Job(Handle);
    impl Job {
        pub(super) fn attach(child: &Child) -> Result<Self, ()> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(());
                }
                let job = Self(handle);
                let mut info = Extended::default();
                info.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                if SetInformationJobObject(
                    handle,
                    9,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<Extended>() as u32,
                ) == 0
                    || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
                {
                    return Err(());
                }
                Ok(job)
            }
        }
        pub(super) fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }
    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_must_be_exact_google_https_destination() {
        assert!(is_config("https://messages.google.com/web/config"));
        assert!(is_config("https://messages.google.com/web/config?x=1"));
        for url in [
            "http://messages.google.com/web/config",
            "https://messages.google.com.evil/web/config",
            "https://messages.google.com/web/config/other",
        ] {
            assert!(!is_config(url));
        }
    }

    #[test]
    fn requires_all_cookies_and_ignores_other_domains_and_names() {
        let mut cookies: Vec<_> = REQUIRED
            .iter()
            .map(|name| json!({"name":name,"value":"secret","domain":".google.com"}))
            .collect();
        assert!(session_cookies(&json!(cookies)).is_some());
        cookies.pop();
        cookies.push(json!({"name":"SAPISID","value":"wrong","domain":"evil.google.com"}));
        assert!(session_cookies(&json!(cookies)).is_none());
        cookies.push(json!({"name":"SAPISID","value":"right","domain":".google.com"}));
        cookies.push(json!({"name":"unrelated","value":"private","domain":".google.com"}));
        cookies.push(json!({"name":"__Secure-1PSIDTS","value":"optional","domain":".google.com"}));
        let result = session_cookies(&json!(cookies)).unwrap();
        assert_eq!(result.len(), 7);
        assert_eq!(result["SAPISID"], "right");
        assert_eq!(result["__Secure-1PSIDTS"], "optional");
    }

    #[test]
    fn empty_required_cookie_is_not_a_session() {
        let cookies: Vec<_> = REQUIRED.iter().map(|name| json!({"name":name,"value":if *name == "SID" { "" } else { "secret" },"domain":".google.com"})).collect();
        assert!(session_cookies(&json!(cookies)).is_none());
    }

    #[test]
    fn cdp_ignores_events_and_correlates_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["method"], "Browser.getVersion");
            for response in [
                json!({"method":"Target.targetCreated"}),
                json!({"id":99,"result":{}}),
                json!({"id":request["id"],"result":{"userAgent":"test-browser"}}),
            ] {
                socket
                    .send(tungstenite::Message::Text(response.to_string().into()))
                    .unwrap();
            }
        });
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (socket, _) = tungstenite::client(format!("ws://{address}/"), stream).unwrap();
        let mut cdp = Cdp { socket, next: 0 };
        assert_eq!(
            cdp.command("Browser.getVersion", json!({})).unwrap()["userAgent"],
            "test-browser"
        );
        server.join().unwrap();
    }

    #[test]
    fn cdp_errors_do_not_expose_browser_payload() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket.read().unwrap();
            socket
                .send(tungstenite::Message::Text(
                    json!({"id":1,"error":{"message":"private-cookie"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (socket, _) = tungstenite::client(format!("ws://{address}/"), stream).unwrap();
        let mut cdp = Cdp { socket, next: 0 };
        assert!(
            !cdp.command("Storage.getCookies", json!({}))
                .unwrap_err()
                .1
                .contains("private-cookie")
        );
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn closing_job_terminates_browser_process() {
        use std::os::windows::process::CommandExt;
        let mut child = Command::new("C:\\Windows\\System32\\ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let job = job::Job::attach(&child).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("closing the job did not terminate the browser process");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
