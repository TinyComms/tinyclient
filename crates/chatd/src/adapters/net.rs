//! HTTPS and websockets over the system TLS.

use std::io::{self, ErrorKind};
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use ureq::tls::{TlsConfig, TlsProvider};

/// Where a network is reached. `var` names an environment variable that replaces `default`, so a
/// test can point an adapter at a local fake replaying a recorded session (`http://` and `ws://`
/// work). Used only when set:
///
/// | Variable | Replaces |
/// | --- | --- |
/// | `TINYCLIENT_DISCORD_API` | `https://discord.com/api/v10` |
/// | `TINYCLIENT_DISCORD_GATEWAY` | `wss://gateway.discord.gg/?v=10&encoding=json` |
/// | `TINYCLIENT_DISCORD_CDN` | `https://cdn.discordapp.com` and the media proxy |
/// | `TINYCLIENT_DISCORD_LOGIN` | the page that names the web client's build, read only when set beside a fake API |
/// | `TINYCLIENT_SLACK_API` | the workspace's own address, `https://example.slack.com/` |
/// | `TINYCLIENT_SLACK_SOCKET` | the websocket address Slack hands out |
///
/// The session itself comes from `TINYCLIENT_SESSION_DISCORD` / `TINYCLIENT_SESSION_SLACK` (see
/// `session::load`), which are read only in a scratch directory.
pub fn address(var: &str, default: &str) -> String {
    // Only a scratch directory may point an adapter elsewhere: a stray variable must never send a
    // real session to some other server.
    if crate::session::enabled() {
        return default.to_owned();
    }
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

/// Who every request says it is: the browser the sign-in window presented (`protocol::browser`).
/// One identity for a session, on the API, the socket and every media fetch alike.
pub fn browser() -> &'static str {
    static BROWSER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BROWSER.get_or_init(protocol::browser::user_agent)
}

/// Takes anything that could be a credential out of text that is about to be logged. One rule
/// for every process: the panic hook uses the same one (`protocol::redact`).
pub fn redact(text: &str) -> String {
    protocol::redact(text)
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .user_agent(browser())
        .build()
        .into()
}

pub struct Socket(WebSocket<MaybeTlsStream<TcpStream>>);

impl Socket {
    /// `tick` bounds how long `read` blocks, so the caller can send its heartbeat.
    pub fn connect(url: &str, headers: &[(&'static str, &str)], tick: Duration) -> io::Result<Socket> {
        let mut request = url.into_client_request().map_err(io::Error::other)?;
        for (name, value) in headers {
            let value = HeaderValue::from_str(value).map_err(io::Error::other)?;
            request.headers_mut().insert(*name, value);
        }
        let (socket, _) = tungstenite::connect(request).map_err(io::Error::other)?;
        let tcp = match socket.get_ref() {
            MaybeTlsStream::NativeTls(tls) => tls.get_ref(),
            MaybeTlsStream::Plain(tcp) => tcp,
            _ => return Err(io::Error::other("unexpected stream")),
        };
        tcp.set_read_timeout(Some(tick))?;
        Ok(Socket(socket))
    }

    /// The next text frame, or `None` if `tick` passed with nothing to read.
    pub fn read(&mut self) -> io::Result<Option<String>> {
        loop {
            match self.0.read() {
                Ok(Message::Text(text)) => return Ok(Some(text.as_str().to_owned())),
                Ok(Message::Close(frame)) => {
                    let why = frame.map_or_else(String::new, |f| format!("{} {}", f.code, f.reason));
                    return Err(io::Error::new(ErrorKind::ConnectionAborted, why));
                }
                // Pings are answered by tungstenite on the next read or write.
                Ok(_) => {}
                Err(tungstenite::Error::Io(err))
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return Ok(None)
                }
                Err(err) => return Err(io::Error::other(err)),
            }
        }
    }

    pub fn send(&mut self, text: String) -> io::Result<()> {
        self.0.send(Message::text(text)).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn a_logged_address_loses_its_token() {
        let logged = redact("Unable to connect to wss://wss-primary.slack.com/?token=xoxc-12-ab_C&batch=1 (timed out)");
        assert_eq!(logged, "Unable to connect to wss://wss-primary.slack.com/?token=[redacted]&batch=1 (timed out)");
        assert_eq!(redact("cookie d=xoxd-abc%2Fdef rejected"), "cookie d=[redacted] rejected");
        assert_eq!(redact("410 Gone"), "410 Gone");
    }
}

/// A gzip member unpacked, at most `limit` bytes of it. Documents that arrive packed
/// (Telegram's emoji files, Teams' push bodies) hold what a shell draws or reads.
pub fn gunzip(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    if data.len() < 18 || data[..3] != [0x1f, 0x8b, 8] {
        return None;
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + usize::from(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]));
    }
    // The file name, then the comment: each runs to a zero byte.
    for flag in [8, 16] {
        if flags & flag != 0 {
            at += data.get(at..)?.iter().position(|byte| *byte == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec_with_limit(data.get(at..)?, limit).ok()
}
