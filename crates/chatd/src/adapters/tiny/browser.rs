//! The browser half of a Tiny sign-in (RFC 8252): the PKCE pair and the state, opening the
//! system browser, and the one request it makes back to this computer. chatd listens on
//! 127.0.0.1 at a port the system picks, takes one `GET /callback`, answers it with a short page,
//! and stops listening. Nothing here is logged: the query holds a one-time code.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Opens a page in the system browser. Tests answer the page themselves.
pub type Opener = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// How often a wait looks at the listener and at whether it was stopped.
const TICK: Duration = Duration::from_millis(50);
/// One request from the browser: its head arrives at once, or not at all.
const READ: Duration = Duration::from_secs(2);
/// The most of a request head that is read. A callback is far shorter.
const HEAD_MAX: usize = 8 * 1024;

/// 32 random bytes, base64url without padding: 43 letters.
pub fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "tinyclient couldn't make a sign-in key on this computer.".to_owned())?;
    Ok(b64(&bytes))
}

/// The S256 challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    b64(&Sha256::digest(verifier.as_bytes()))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The same bytes, in a time that does not depend on where they differ.
pub fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |differ, (x, y)| differ | (x ^ y)) == 0
}

/// A page the browser may be sent to: http or https, and nothing a shell or a handler could take
/// as something else.
pub fn web_link(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() <= 8 * 1024
        && !url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// The system browser, at `url`. Only a web link is opened.
pub fn system() -> Opener {
    Arc::new(|url: &str| {
        if !web_link(url) {
            return Err("That isn't a web page.".to_owned());
        }
        system_open(url)
    })
}

#[cfg(target_os = "macos")]
fn system_open(url: &str) -> Result<(), String> {
    run_opener("open", url)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_open(url: &str) -> Result<(), String> {
    run_opener("xdg-open", url)
}

#[cfg(unix)]
fn run_opener(program: &str, url: &str) -> Result<(), String> {
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| NOT_OPENED.to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return if status.success() { Ok(()) } else { Err(NOT_OPENED.to_owned()) },
            Ok(None) if Instant::now() < deadline => std::thread::sleep(TICK),
            result => {
                let failed = result.is_err();
                // Some handlers remain alive for the browser's entire lifetime.
                // Reap them in the background and keep listening for the callback.
                std::thread::spawn(move || { let _ = child.wait(); });
                return if failed { Err(NOT_OPENED.to_owned()) } else { Ok(()) };
            }
        }
    }
}

/// ShellExecuteW, not `cmd /C start`: cmd splits a link at its `&` and runs the rest as a command.
#[cfg(windows)]
fn system_open(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, file) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call; the rest may be null.
    let code = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
    } as isize;
    // Above 32 a handler took it.
    if code > 32 {
        Ok(())
    } else {
        Err(NOT_OPENED.to_owned())
    }
}

const NOT_OPENED: &str = "Your browser didn't open. Choose Open the page again, or set a default browser.";

/// How the wait for the browser ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Back {
    /// The one-time code, with the state this sign-in sent.
    Code(String),
    /// The server's `error`: `not_member`, `denied`, or `failed`.
    Error(String),
    /// Nothing came back in time.
    TimedOut,
    /// The sheet was closed. The listener is closed with it.
    Stopped,
}

/// Waits for the browser's one request to `/callback`, for `patience` at most, and answers it.
/// A request for anything else, or with another state, is answered and does not end the wait.
/// The listener is dropped, and so closed, when this returns.
pub fn wait(listener: TcpListener, state: &str, patience: Duration, stop: &AtomicBool) -> Back {
    if listener.set_nonblocking(true).is_err() {
        return Back::Error("failed".to_owned());
    }
    let deadline = Instant::now() + patience;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Back::Stopped;
        }
        if Instant::now() >= deadline {
            return Back::TimedOut;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(back) = answer(stream, state) {
                    return back;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(TICK),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Back::Error("failed".to_owned()),
        }
    }
}

/// One request. `None` when it was not the callback this sign-in waits for.
fn answer(mut stream: TcpStream, state: &str) -> Option<Back> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(READ));
    let _ = stream.set_write_timeout(Some(READ));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while head.len() < HEAD_MAX && !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&buf[..read]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let line = head.lines().next().unwrap_or("");
    let Some(query) = callback_query(line) else {
        reply(&mut stream, "404 Not Found", &page(OTHER_PAGE));
        return None;
    };
    let field = |name: &str| field(query, name);
    if !field("state").is_some_and(|theirs| same(&theirs, state)) {
        // Not this sign-in's: a stale tab, or a page that should not be here. The wait goes on.
        reply(&mut stream, "400 Bad Request", &page(STALE_PAGE));
        return None;
    }
    if let Some(error) = field("error") {
        let words = if error == "not_member" { NOT_MEMBER_PAGE } else { FAILED_PAGE };
        reply(&mut stream, "200 OK", &page(words));
        return Some(Back::Error(error));
    }
    match field("code").filter(|code| !code.is_empty()) {
        Some(code) => {
            reply(&mut stream, "200 OK", &page(DONE_PAGE));
            Some(Back::Code(code))
        }
        None => {
            reply(&mut stream, "200 OK", &page(FAILED_PAGE));
            Some(Back::Error("failed".to_owned()))
        }
    }
}

/// The query of `GET /callback?… HTTP/1.x`, or `None` for any other request.
fn callback_query(line: &str) -> Option<&str> {
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method != "GET" || !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (path == "/callback").then_some(query)
}

/// One field of a query, decoded. The first one wins.
fn field(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(key)? == name).then(|| decode(value)).flatten()
    })
}

/// `%XX` and `+`. `None` when it is not UTF-8 once decoded.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'+' => out.push(b' '),
            b'%' if at + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        at += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8(out).ok()
}

const DONE_PAGE: &str = "You can close this tab and return to tinyclient.";
const NOT_MEMBER_PAGE: &str = "You're not a member of this server yet. Ask for an invite. You can close this tab.";
const FAILED_PAGE: &str = "tinyclient couldn't finish signing in. You can close this tab and try again from tinyclient.";
const STALE_PAGE: &str = "This sign-in didn't start in tinyclient, or it has ended. Start again from tinyclient.";
const OTHER_PAGE: &str = "Nothing is here.";

/// A page of one sentence. The words are escaped, though every one is this file's own.
pub fn page(words: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <title>tinyclient</title><style>body{{font:16px/1.5 system-ui,sans-serif;max-width:28em;margin:20vh auto;padding:0 1.5em;\
         color:#1d1e22;background:#fff}}@media (prefers-color-scheme:dark){{body{{color:#e8e8ea;background:#16171b}}}}</style></head>\
         <body><p>{}</p></body></html>",
        escape(words)
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn reply(stream: &mut TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pair_is_s256_and_the_state_compares_whole() {
        let verifier = random().unwrap();
        assert_eq!(verifier.len(), 43);
        assert_ne!(verifier, random().unwrap());
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        // RFC 7636, appendix B.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
        assert!(!same("", "a"));
    }

    #[test]
    fn only_the_callback_is_read_and_its_fields_are_decoded() {
        assert_eq!(callback_query("GET /callback?code=a&state=b HTTP/1.1"), Some("code=a&state=b"));
        assert_eq!(callback_query("GET /callback HTTP/1.1"), Some(""));
        assert_eq!(callback_query("GET /favicon.ico HTTP/1.1"), None);
        assert_eq!(callback_query("POST /callback?code=a HTTP/1.1"), None);
        assert_eq!(callback_query("GET /callbackx?code=a HTTP/1.1"), None);
        assert_eq!(callback_query("GET /callback?code=a"), None);
        assert_eq!(field("code=a%2Bb%3D&state=s+t", "code").as_deref(), Some("a+b="));
        assert_eq!(field("code=a&state=s+t", "state").as_deref(), Some("s t"));
        assert_eq!(field("code=a&code=b", "code").as_deref(), Some("a"));
        assert_eq!(field("error=not_member", "code"), None);
        assert_eq!(field("code=%zz", "code").as_deref(), Some("%zz"));
        assert_eq!(field("code=%4", "code").as_deref(), Some("%4"));
        assert_eq!(field("code=%ff", "code"), None);
    }

    #[test]
    fn a_page_is_escaped_and_a_link_is_only_a_web_page() {
        let shown = page("<b>&\"'");
        assert!(shown.contains("&lt;b&gt;&amp;&quot;&#39;"));
        assert!(!shown.contains("<b>"));
        assert!(web_link("https://auth.example/authorize?a=1&b=2"));
        assert!(web_link("HTTP://127.0.0.1:9/authorize"));
        assert!(!web_link("file:///etc/passwd"));
        assert!(!web_link("-https://x"));
        assert!(!web_link("https://x/ y"));
        assert!(!web_link("javascript:alert(1)"));
        assert!(!web_link("https://x/\"a"));
    }
}
