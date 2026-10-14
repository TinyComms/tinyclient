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
/// | `TINYCHAT_DISCORD_API` | `https://discord.com/api/v10` |
/// | `TINYCHAT_DISCORD_GATEWAY` | `wss://gateway.discord.gg/?v=10&encoding=json` |
/// | `TINYCHAT_DISCORD_CDN` | `https://cdn.discordapp.com` and the media proxy |
/// | `TINYCHAT_DISCORD_LOGIN` | the page that names the web client's build, read only when set beside a fake API |
/// | `TINYCHAT_SLACK_API` | the workspace's own address, `https://example.slack.com/` |
/// | `TINYCHAT_SLACK_SOCKET` | the websocket address Slack hands out |
///
/// The session itself comes from `TINYCHAT_SESSION_DISCORD` / `TINYCHAT_SESSION_SLACK` (see
/// `session::load`), which are read only in a scratch directory.
pub fn address(var: &str, default: &str) -> String {
    // Only a scratch directory may point an adapter elsewhere: a stray variable must never send a
    // real session to some other server.
    if crate::session::enabled() {
        return default.to_owned();
    }
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

/// Who every request says it is: the Safari that is installed, which is what the sign-in window
/// presented. One identity for a session, on the API, the socket and every media fetch alike.
pub fn browser() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        static BROWSER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        return BROWSER.get_or_init(|| {
            let installed = std::process::Command::new("/usr/bin/defaults")
                .args(["read", "/Applications/Safari.app/Contents/Info", "CFBundleShortVersionString"])
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|version| version.trim().to_owned())
                .filter(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.'));
            format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/{} Safari/605.1.15",
                installed.as_deref().unwrap_or("26.0")
            )
        });
    }
    #[cfg(target_os = "windows")]
    {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
}

/// Takes anything that could be a credential out of text that is about to be logged. One rule
/// for every process: the panic hook uses the same one (`protocol::redact`).
pub fn redact(text: &str) -> String {
    protocol::redact(text)
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
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
