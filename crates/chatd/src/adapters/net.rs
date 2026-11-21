//! HTTPS and websockets over the system TLS.

use base64::Engine as _;
use protocol::settings::{ProxyConfig, ProxyMode, ProxyScheme, Values};
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::RwLock;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

static PROXY: RwLock<ProxyConfig> = RwLock::new(ProxyConfig {
    mode: ProxyMode::System,
    scheme: ProxyScheme::Http,
    host: String::new(),
    port: 0,
    user: None,
});

/// Updates the shared proxy configuration used by every outbound HTTP and WebSocket client in chatd.
pub fn configure_proxy(values: &Values) {
    let config = values.proxy();
    if let Ok(mut guard) = PROXY.write() {
        *guard = config;
    }
}

/// Returns the currently active proxy configuration.
pub fn proxy_config() -> ProxyConfig {
    PROXY.read().map(|g| g.clone()).unwrap_or_default()
}

/// Applies the active proxy configuration to a `ureq` agent builder.
/// Fails closed if `ProxyMode::Custom` is selected with an incomplete or unparseable proxy URL.
pub fn apply_proxy(
    builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    let cfg = proxy_config();
    match cfg.mode {
        ProxyMode::System => builder.proxy(ureq::Proxy::try_from_env()),
        ProxyMode::None => builder.proxy(None),
        ProxyMode::Custom => {
            let proxy = cfg
                .custom_url(None)
                .ok()
                .flatten()
                .and_then(|url| ureq::Proxy::new(&url).ok())
                // Fail closed: never leak directly to the internet if Custom proxy is invalid.
                .or_else(|| ureq::Proxy::new("http://127.0.0.1:1").ok());
            builder.proxy(proxy)
        }
    }
}

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
    apply_proxy(ureq::Agent::config_builder())
        .tls_config(crate::tls::config())
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .user_agent(browser())
        .build()
        .into()
}

/// Parses `ws://` or `wss://` target `(host, port, is_tls)`.
fn parse_ws_target(url: &str) -> io::Result<(String, u16, bool)> {
    let (is_tls, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err(io::Error::new(ErrorKind::InvalidInput, "unsupported websocket scheme"));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(io::Error::new(ErrorKind::InvalidInput, "missing websocket host"));
    }
    let default_port = if is_tls { 443 } else { 80 };
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed
            .split_once(']')
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "invalid IPv6 host"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse::<u16>().map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid port"))?,
            None => default_port,
        };
        Ok((host.to_owned(), port, is_tls))
    } else if let Some((host, port_str)) = authority.rsplit_once(':') {
        let port = port_str.parse::<u16>().map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid port"))?;
        Ok((host.to_owned(), port, is_tls))
    } else {
        Ok((authority.to_owned(), default_port, is_tls))
    }
}

/// Tunnels a TCP stream to `(target_host, target_port)` through an HTTP CONNECT proxy.
pub fn tunnel_http_connect(
    stream: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    auth: Option<(&str, &str)>,
) -> io::Result<()> {
    let host_hdr = if target_host.contains(':') {
        format!("[{target_host}]:{target_port}")
    } else {
        format!("{target_host}:{target_port}")
    };
    let mut req = format!("CONNECT {host_hdr} HTTP/1.1\r\nHost: {host_hdr}\r\n");
    if let Some((user, pass)) = auth {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.flush()?;
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while buf.len() < 8192 {
        stream.read_exact(&mut byte)?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header = String::from_utf8_lossy(&buf);
    let status_line = header.lines().next().unwrap_or("");
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (200..300).contains(&code));
    if !ok {
        return Err(io::Error::new(
            ErrorKind::ConnectionRefused,
            format!("HTTP CONNECT proxy rejected tunnel: {status_line}"),
        ));
    }
    Ok(())
}

/// Tunnels a TCP stream to `(target_host, target_port)` through a SOCKS5 proxy (RFC 1928 / RFC 1929).
pub fn tunnel_socks5(
    stream: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    auth: Option<(&str, &str)>,
) -> io::Result<()> {
    let methods: &[u8] = if auth.is_some() { &[0x05, 0x02, 0x00, 0x02] } else { &[0x05, 0x01, 0x00] };
    stream.write_all(methods)?;
    stream.flush()?;
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply)?;
    if reply[0] != 0x05 {
        return Err(io::Error::new(ErrorKind::InvalidData, "invalid SOCKS5 greeting response"));
    }
    match reply[1] {
        0x00 => {}
        0x02 => {
            let Some((user, pass)) = auth else {
                return Err(io::Error::new(ErrorKind::PermissionDenied, "SOCKS5 proxy requires credentials"));
            };
            let (ub, pb) = (user.as_bytes(), pass.as_bytes());
            if ub.len() > 255 || pb.len() > 255 {
                return Err(io::Error::new(ErrorKind::InvalidInput, "SOCKS5 credentials too long"));
            }
            let mut pkt = Vec::with_capacity(3 + ub.len() + pb.len());
            pkt.push(0x01);
            pkt.push(ub.len() as u8);
            pkt.extend_from_slice(ub);
            pkt.push(pb.len() as u8);
            pkt.extend_from_slice(pb);
            stream.write_all(&pkt)?;
            stream.flush()?;
            let mut auth_reply = [0u8; 2];
            stream.read_exact(&mut auth_reply)?;
            if auth_reply[1] != 0x00 {
                return Err(io::Error::new(ErrorKind::PermissionDenied, "SOCKS5 authentication failed"));
            }
        }
        _ => return Err(io::Error::new(ErrorKind::ConnectionRefused, "SOCKS5 proxy rejected auth method")),
    }
    let host_bytes = target_host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(io::Error::new(ErrorKind::InvalidInput, "target hostname too long for SOCKS5"));
    }
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&req)?;
    stream.flush()?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    if head[0] != 0x05 || head[1] != 0x00 {
        return Err(io::Error::new(
            ErrorKind::ConnectionRefused,
            format!("SOCKS5 connect failed with code {}", head[1]),
        ));
    }
    // Consume bound address + port.
    match head[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            stream.read_exact(&mut skip)?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut skip = vec![0u8; usize::from(len[0]) + 2];
            stream.read_exact(&mut skip)?;
        }
        0x04 => {
            let mut skip = [0u8; 18];
            stream.read_exact(&mut skip)?;
        }
        _ => return Err(io::Error::new(ErrorKind::InvalidData, "invalid SOCKS5 atyp")),
    }
    Ok(())
}

/// Dials a TCP stream to `(target_host, target_port)` through `cfg` when `ProxyMode::Custom`.
/// Always fails closed if `Custom` is incomplete or unreachable.
pub fn dial_via_proxy(cfg: &ProxyConfig, target_host: &str, target_port: u16) -> io::Result<Option<TcpStream>> {
    if cfg.mode != ProxyMode::Custom {
        return Ok(None);
    }
    let host = cfg.host.trim();
    if host.is_empty() || cfg.port == 0 {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "custom proxy enabled without host or port (failing closed)",
        ));
    }
    let mut stream = TcpStream::connect((host, cfg.port))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    let auth = cfg.user.as_deref().map(|u| (u, ""));
    match cfg.scheme {
        ProxyScheme::Http => tunnel_http_connect(&mut stream, target_host, target_port, auth)?,
        ProxyScheme::Socks5 => tunnel_socks5(&mut stream, target_host, target_port, auth)?,
    }
    Ok(Some(stream))
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
        let cfg = proxy_config();
        let socket = if cfg.mode == ProxyMode::Custom {
            let (host, port, _is_tls) = parse_ws_target(url)?;
            let tcp = dial_via_proxy(&cfg, &host, port)?
                .ok_or_else(|| io::Error::new(ErrorKind::ConnectionRefused, "proxy dial failed closed"))?;
            let (ws, _) = tungstenite::client_tls(request, tcp).map_err(io::Error::other)?;
            ws
        } else {
            let (ws, _) = tungstenite::connect(request).map_err(io::Error::other)?;
            ws
        };
        let Some(tcp) = crate::tls::tcp(socket.get_ref()) else {
            return Err(io::Error::other("unexpected stream"));
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
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_logged_address_loses_its_token() {
        let logged = redact("Unable to connect to wss://wss-primary.slack.com/?token=xoxc-12-ab_C&batch=1 (timed out)");
        assert_eq!(logged, "Unable to connect to wss://wss-primary.slack.com/?token=[redacted]&batch=1 (timed out)");
        assert_eq!(redact("cookie d=xoxd-abc%2Fdef rejected"), "cookie d=[redacted] rejected");
        assert_eq!(redact("410 Gone"), "410 Gone");
    }

    #[test]
    fn custom_proxy_fails_closed_when_incomplete() {
        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            scheme: ProxyScheme::Socks5,
            host: String::new(),
            port: 0,
            user: None,
        };
        assert!(dial_via_proxy(&cfg, "example.com", 443).is_err());
    }

    #[test]
    fn http_connect_tunnel_sends_connect_and_checks_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 256];
            let n = sock.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").unwrap();
            req
        });
        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            scheme: ProxyScheme::Http,
            host: "127.0.0.1".into(),
            port,
            user: Some("alice".into()),
        };
        let stream = dial_via_proxy(&cfg, "gateway.discord.gg", 443).unwrap();
        assert!(stream.is_some());
        let seen = server.join().unwrap();
        assert!(seen.starts_with("CONNECT gateway.discord.gg:443 HTTP/1.1\r\n"), "{seen}");
        assert!(seen.contains("Proxy-Authorization: Basic "), "{seen}");
    }

    #[test]
    fn socks5_tunnel_completes_rfc1928_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut greet = [0u8; 3];
            sock.read_exact(&mut greet).unwrap();
            assert_eq!(greet, [0x05, 0x01, 0x00]);
            sock.write_all(&[0x05, 0x00]).unwrap();
            let mut head = [0u8; 5];
            sock.read_exact(&mut head).unwrap();
            assert_eq!(&head[..4], &[0x05, 0x01, 0x00, 0x03]);
            let mut rest = vec![0u8; usize::from(head[4]) + 2];
            sock.read_exact(&mut rest).unwrap();
            let target = String::from_utf8_lossy(&rest[..usize::from(head[4])]).into_owned();
            let target_port = u16::from_be_bytes([rest[rest.len() - 2], rest[rest.len() - 1]]);
            sock.write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x1f, 0x90]).unwrap();
            (target, target_port)
        });
        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            scheme: ProxyScheme::Socks5,
            host: "127.0.0.1".into(),
            port,
            user: None,
        };
        let stream = dial_via_proxy(&cfg, "wss-primary.slack.com", 443).unwrap();
        assert!(stream.is_some());
        assert_eq!(server.join().unwrap(), ("wss-primary.slack.com".to_owned(), 443));
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
