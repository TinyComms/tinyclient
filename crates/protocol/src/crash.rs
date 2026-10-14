//! What a panic leaves behind. Each process installs [`install_panic_hook`] first thing: a panic
//! is written, redacted, to stderr and to `crash.log` in the support directory, so "it closed on
//! its own" can be read afterwards. A panic on a thread other than `main` still ends only that
//! thread; chatd answers each connection on its own thread for that reason.

use std::io::Write;

/// Under [`crate::support_dir`]. Every process appends to it; it is cut back to nothing once it
/// passes [`CRASH_LOG_LIMIT`].
pub const CRASH_LOG: &str = "crash.log";
pub const CRASH_LOG_LIMIT: u64 = 256 * 1024;

/// Takes anything that could be a credential out of text that is about to be logged. A websocket
/// address can carry a token, and a library prints the address when it cannot connect.
pub fn redact(text: &str) -> String {
    const MARKS: [&str; 8] = ["token=", "xoxc-", "xoxd-", "xoxs-", "xoxp-", "xoxb-", "Bearer ", "skypetoken"];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = MARKS.iter().filter_map(|mark| rest.find(mark)).min() {
        out.push_str(&rest[..at]);
        out.push_str("[redacted]");
        let tail = &rest[at..];
        let skip = MARKS.iter().find(|mark| tail.starts_with(**mark)).map_or(0, |mark| mark.len());
        let end = tail[skip..].find(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ')')).map_or(tail.len(), |end| skip + end);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Replaces the default panic message with one entry, redacted, naming the process and thread,
/// on stderr and appended to `crash.log`.
pub fn install_panic_hook(process: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let backtrace = std::backtrace::Backtrace::force_capture();
        let entry = redact(&format!(
            "{process} panicked at unix {at} on thread {}: {info}\n{backtrace}\n",
            thread.name().unwrap_or("unnamed"),
        ));
        let _ = std::io::stderr().write_all(entry.as_bytes());
        let path = crate::support_dir().join(CRASH_LOG);
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > CRASH_LOG_LIMIT) {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = log.write_all(entry.as_bytes());
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn a_logged_address_or_header_loses_its_credential() {
        let logged = redact("Unable to connect to wss://wss-primary.slack.com/?token=xoxc-12-ab_C&batch=1 (timed out)");
        assert_eq!(logged, "Unable to connect to wss://wss-primary.slack.com/?[redacted]&batch=1 (timed out)");
        assert_eq!(redact("cookie d=xoxd-abc%2Fdef rejected"), "cookie d=[redacted] rejected");
        assert_eq!(redact("authorization: Bearer eyJabc.def rejected"), "authorization: [redacted] rejected");
        assert_eq!(redact("410 Gone"), "410 Gone");
    }
}
