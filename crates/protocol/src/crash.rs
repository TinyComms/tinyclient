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
///
/// What goes: Slack tokens (`xoxc-`, `xoxd-`, `xoxp-`, `xoxb-`, `xoxe-` and the like), Discord
/// tokens (three base64url parts joined by dots, no prefix) and JWTs, the values of the
/// `Authorization`, `X-Skypetoken` and `Authentication` headers and of `Bearer`, the values of
/// signed or credential URL parameters (`token=`, `hm=`, `tempauth=`, `sig=`,
/// `X-Amz-Signature=`, ...) and of JSON fields such as `"token"` and `"skypetoken"`.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let boundary = text[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.');
        if boundary {
            if let Some((keep, skip)) = secret_at(&text[at..]) {
                out.push_str(&text[at..at + keep]);
                out.push_str(REDACTED);
                at += skip;
                continue;
            }
        }
        let c = text[at..].chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

const REDACTED: &str = "[redacted]";

/// Prefixes that are the start of a credential: the whole word goes.
const PREFIXED: [&str; 11] = ["xoxc-", "xoxd-", "xoxs-", "xoxp-", "xoxb-", "xoxe-", "xoxe.", "xoxa-", "xoxr-", "xapp-", "mfa."];
/// Headers whose value is a credential: the name stays, the value (scheme included) goes.
const HEADERS: [&str; 5] = ["authorization:", "proxy-authorization:", "x-skypetoken:", "authentication:", "x-ms-skypetoken:"];
/// URL or form parameters whose value is a credential or a signature: the name stays.
const PARAMS: [&str; 13] = [
    "token=", "access_token=", "refresh_token=", "id_token=", "skypetoken=", "hm=", "tempauth=", "sig=",
    "x-amz-signature=", "x-amz-credential=", "x-amz-security-token=", "signature=", "code=",
];
/// JSON fields whose string value is a credential.
const FIELDS: [&str; 7] = ["token", "access_token", "refresh_token", "id_token", "skypetoken", "authorization", "password"];

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// Where a value ends: at white space, or at a character that closes a parameter, a quote or
/// a bracket.
fn value_end(text: &str, from: usize) -> usize {
    text[from..].find(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ')' | ',' | ';' | '<' | '>' | ']' | '}')).map_or(text.len(), |end| from + end)
}

fn base64url(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// A credential starting at the front of `text`, at a word boundary: how many bytes to keep as
/// they are (a header's or a parameter's name), and how many it takes in all.
fn secret_at(text: &str) -> Option<(usize, usize)> {
    if let Some(prefix) = PREFIXED.iter().find(|prefix| text.starts_with(**prefix)) {
        let end = value_end(text, prefix.len());
        return (end > prefix.len()).then_some((0, end));
    }
    if let Some(header) = HEADERS.iter().find(|header| starts_with_ignore_case(text, header)) {
        let keep = header.len() + text[header.len()..].len() - text[header.len()..].trim_start_matches([' ', '\t']).len();
        let mut from = keep;
        // The scheme goes with the value: `Bearer abc`, `Basic abc`, `skypetoken=abc`.
        let word = value_end(text, from);
        if word < text.len() && text[word..].starts_with(' ') && text[from..word].chars().all(|c| c.is_ascii_alphabetic()) {
            from = word + 1;
        }
        let end = value_end(text, from);
        return (end > keep).then_some((keep, end));
    }
    if starts_with_ignore_case(text, "bearer ") {
        let end = value_end(text, "bearer ".len());
        return (end > "bearer ".len()).then_some((0, end));
    }
    if let Some(param) = PARAMS.iter().find(|param| starts_with_ignore_case(text, param)) {
        let end = value_end(text, param.len());
        return (end > param.len()).then_some((param.len(), end));
    }
    if let Some(rest) = text.strip_prefix('"') {
        for field in FIELDS {
            if !starts_with_ignore_case(rest, field) || !rest[field.len()..].starts_with('"') {
                continue;
            }
            let after = &rest[field.len() + 1..];
            let value = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
            let keep = text.len() - value.len();
            let mut escaped = false;
            let close = value.find(|c: char| {
                let close = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                close
            })?;
            return (close > 0).then_some((keep, keep + close));
        }
    }
    dotted_token(text).map(|end| (0, end))
}

/// A Discord token or a JWT: three base64url parts joined by dots, long enough that no version
/// number, address or file name is one. Answers where it ends.
fn dotted_token(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut parts = [0usize; 3];
    for (ix, part) in parts.iter_mut().enumerate() {
        let start = at;
        while at < bytes.len() && base64url(bytes[at]) {
            at += 1;
        }
        *part = at - start;
        if ix < 2 {
            if bytes.get(at) != Some(&b'.') {
                return None;
            }
            at += 1;
        }
    }
    let ends_clean = bytes.get(at).is_none_or(|c| !base64url(*c) && *c != b'.');
    (parts[0] >= 18 && parts[1] >= 5 && parts[2] >= 20 && ends_clean).then_some(at)
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
        assert_eq!(logged, "Unable to connect to wss://wss-primary.slack.com/?token=[redacted]&batch=1 (timed out)");
        assert_eq!(redact("cookie d=xoxd-abc%2Fdef rejected"), "cookie d=[redacted] rejected");
        assert_eq!(redact("authorization: Bearer eyJabc.def rejected"), "authorization: [redacted] rejected");
        assert_eq!(redact("410 Gone"), "410 Gone");
    }

    #[test]
    fn every_kind_of_credential_goes_and_ordinary_text_stays() {
        // Slack, every prefix.
        for token in ["xoxe-1-abc", "xoxe.xoxp-1-abc", "xoxp-12-34-ab", "xoxb-12-34-ab"] {
            assert_eq!(redact(&format!("sent {token} here")), "sent [redacted] here", "{token}");
        }
        // Discord: three base64url parts, no prefix, wherever they sit.
        let discord = "aaaaaaaaaaaaaaaaaa.bbbbb.cccccccccccccccccccc";
        assert_eq!(redact(&format!("Authorization header was {discord}")), "Authorization header was [redacted]");
        assert_eq!(redact(&format!("{{\"t\":\"{discord}\"}}")), "{\"t\":\"[redacted]\"}");
        // Headers keep their name.
        assert_eq!(redact("X-Skypetoken: eyJhbGciOi.abc.def next"), "X-Skypetoken: [redacted] next");
        assert_eq!(redact("Authorization: skypetoken=abc123 sent"), "Authorization: [redacted] sent");
        assert_eq!(redact("AUTHORIZATION:Basic dXNlcjpwYXNz"), "AUTHORIZATION:[redacted]");
        assert_eq!(redact("with bearer abc.def"), "with [redacted]");
        // Signed addresses keep the parameter's name, lose its value.
        assert_eq!(
            redact("https://cdn.discordapp.com/a.png?ex=1&is=2&hm=0f9e8d7c&size=64"),
            "https://cdn.discordapp.com/a.png?ex=1&is=2&hm=[redacted]&size=64"
        );
        assert_eq!(redact("https://x.sharepoint.com/f?tempauth=v1.eyJ0eXAi&x=1"), "https://x.sharepoint.com/f?tempauth=[redacted]&x=1");
        assert_eq!(redact("https://a.blob.core.windows.net/b?sv=2020&sig=Zm9v%2B"), "https://a.blob.core.windows.net/b?sv=2020&sig=[redacted]");
        assert_eq!(
            redact("https://s3.amazonaws.com/k?X-Amz-Date=1&X-Amz-Signature=abcdef0123"),
            "https://s3.amazonaws.com/k?X-Amz-Date=1&X-Amz-Signature=[redacted]"
        );
        // JSON fields.
        assert_eq!(redact(r#"{"skypetoken": "abc\"def", "ok": true}"#), r#"{"skypetoken": "[redacted]", "ok": true}"#);
        // Nothing ordinary is taken: versions, hosts, words ending like a key.
        for plain in [
            "chatd 0.4.0 on macos 15.1.2",
            "wss-primary.slack.com closed",
            "algorithm=sha256 design=flat",
            "the file name.tar.gz",
            "Slack: rate limited for 30 s",
        ] {
            assert_eq!(redact(plain), plain);
        }
    }
}
