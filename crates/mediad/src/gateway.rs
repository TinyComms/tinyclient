//! The voice gateway's socket (version 8): JSON frames, DAVE's binary frames, and what its close
//! codes mean. The conversation held over it is in `session`.

use protocol::CallFailure;
use serde_json::{json, Value};
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub const IDENTIFY: u8 = 0;
pub const SELECT_PROTOCOL: u8 = 1;
pub const READY: u8 = 2;
pub const HEARTBEAT: u8 = 3;
pub const SESSION_DESCRIPTION: u8 = 4;
pub const SPEAKING: u8 = 5;
pub const HEARTBEAT_ACK: u8 = 6;
pub const RESUME: u8 = 7;
pub const HELLO: u8 = 8;
pub const RESUMED: u8 = 9;
pub const CLIENTS_CONNECT: u8 = 11;
/// User sessions are also told each member's SSRCs this way.
pub const VIDEO: u8 = 12;
pub const CLIENT_DISCONNECT: u8 = 13;

/// How long a read waits before the caller gets to do its own work. Over TLS a read that times
/// out may stop anywhere, inside a record or between the records of one frame: what arrived
/// stays buffered (in the TLS session, then in the websocket) and the next read carries on.
const POLL: Duration = Duration::from_millis(20);

/// One frame from the voice server.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Json { op: u8, d: Value },
    /// A DAVE frame: the opcode and what follows it.
    Binary { op: u8, payload: Vec<u8> },
    /// Nothing arrived within the poll.
    Quiet,
    /// The server closed the socket, with its code if it gave one.
    Closed(Option<u16>),
}

pub struct Socket {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    /// The last sequence number the server stamped on a frame, echoed in heartbeats and resume.
    pub seq_ack: Option<u64>,
}

/// `TINYCHAT_VOICE_PLAIN` drops TLS, for a fake voice server on localhost. Discord is always TLS.
fn url(endpoint: &str) -> String {
    let scheme = if std::env::var_os("TINYCHAT_VOICE_PLAIN").is_some() { "ws" } else { "wss" };
    let endpoint = endpoint.trim_start_matches("wss://").trim_end_matches('/');
    format!("{scheme}://{endpoint}/?v=8")
}

/// The system's trust, and nothing else, decides whether a voice server is who it says. One
/// exception, for tests: in a scratch directory (`TINYCHAT_DIR` set, as every test and demo
/// runs) `TINYCHAT_VOICE_TRUST` names a PEM certificate to trust as well, so a fake voice server
/// can speak real TLS. Without `TINYCHAT_DIR` the variable is ignored.
fn connector() -> io::Result<native_tls::TlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();
    let scratch = std::env::var_os("TINYCHAT_DIR").is_some();
    if let Some(path) = std::env::var_os("TINYCHAT_VOICE_TRUST").filter(|_| scratch) {
        let certificate = native_tls::Certificate::from_pem(&std::fs::read(path)?).map_err(io::Error::other)?;
        builder.add_root_certificate(certificate);
    }
    builder.build().map_err(io::Error::other)
}

/// TLS over a connected socket, or the socket itself for a `ws` address.
///
/// The handshake is done here and not by `tungstenite::client_tls`: the socket has a read
/// timeout, and tungstenite 0.30 panics ("TLS handshake not blocked") when a handshake runs
/// into one, which a voice server that is slow to answer is enough to cause.
fn secure(url: &str, host: &str, stream: TcpStream) -> io::Result<MaybeTlsStream<TcpStream>> {
    if url.starts_with("ws://") {
        return Ok(MaybeTlsStream::Plain(stream));
    }
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    match connector()?.connect(name, stream) {
        Ok(stream) => Ok(MaybeTlsStream::NativeTls(stream)),
        Err(native_tls::HandshakeError::WouldBlock(_)) => {
            Err(io::Error::new(ErrorKind::TimedOut, "the voice server did not finish the TLS handshake in time"))
        }
        Err(native_tls::HandshakeError::Failure(err)) => Err(io::Error::other(err)),
    }
}

/// The server's address. The system resolver has no time limit of its own, so it is asked on a
/// thread that is left behind if it does not answer in `within`.
fn resolve(address: String, within: Duration) -> io::Result<SocketAddr> {
    let (found, answer) = channel();
    std::thread::Builder::new().name("resolve".to_owned()).spawn(move || {
        let _ = found.send(address.to_socket_addrs().map(|mut addresses| addresses.next()));
    })?;
    match answer.recv_timeout(within) {
        Ok(Ok(Some(address))) => Ok(address),
        Ok(Ok(None)) => Err(io::Error::new(ErrorKind::NotFound, "the voice server's name has no address")),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(io::Error::new(ErrorKind::TimedOut, "the voice server's name did not resolve in time")),
    }
}

impl Socket {
    /// Resolves, connects, and completes the TLS and websocket handshakes, all within `within`.
    pub fn connect(endpoint: &str, seq_ack: Option<u64>, within: Duration) -> io::Result<Socket> {
        let deadline = Instant::now() + within;
        let left = || deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(50));
        let host = endpoint.trim_start_matches("wss://").trim_end_matches('/');
        let address = if host.contains(':') { host.to_owned() } else { format!("{host}:443") };
        let address = resolve(address, left())?;
        let stream = TcpStream::connect_timeout(&address, left())?;
        stream.set_nodelay(true)?;
        // The handshakes share what is left of the time; reads after them poll.
        stream.set_read_timeout(Some(left()))?;
        stream.set_write_timeout(Some(left()))?;
        let url = url(endpoint);
        let stream = secure(&url, host, stream)?;
        let (ws, _) = tungstenite::client(url, stream).map_err(|err| match err {
            tungstenite::HandshakeError::Failure(err) => io::Error::other(err),
            tungstenite::HandshakeError::Interrupted(_) => {
                io::Error::new(ErrorKind::TimedOut, "the voice server did not answer the websocket handshake in time")
            }
        })?;
        match ws.get_ref() {
            MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(POLL))?,
            MaybeTlsStream::NativeTls(stream) => stream.get_ref().set_read_timeout(Some(POLL))?,
            _ => {}
        }
        Ok(Socket { ws, seq_ack })
    }

    pub fn send(&mut self, op: u8, d: Value) -> io::Result<()> {
        let frame = json!({ "op": op, "d": d });
        self.ws.send(Message::text(frame.to_string())).map_err(io::Error::other)
    }

    /// A DAVE frame from us: the opcode, then the payload. Only the server's carry a sequence.
    pub fn send_binary(&mut self, op: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(1 + payload.len());
        frame.push(op);
        frame.extend_from_slice(payload);
        self.ws.send(Message::binary(frame)).map_err(io::Error::other)
    }

    pub fn heartbeat(&mut self, nonce: u64) -> io::Result<()> {
        let mut d = json!({ "t": nonce });
        if let Some(seq) = self.seq_ack {
            d["seq_ack"] = json!(seq);
        }
        self.send(HEARTBEAT, d)
    }

    pub fn read(&mut self) -> io::Result<Incoming> {
        let message = match self.ws.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(Incoming::Quiet)
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(Incoming::Closed(None))
            }
            Err(err) => return Err(io::Error::other(err)),
        };
        match message {
            Message::Text(text) => {
                let frame: Value = serde_json::from_str(text.as_str()).map_err(io::Error::other)?;
                if let Some(seq) = frame["seq"].as_u64() {
                    self.seq_ack = Some(seq);
                }
                let op = frame["op"].as_u64().and_then(|op| u8::try_from(op).ok());
                let op = op.ok_or_else(|| io::Error::other("a voice frame without an opcode"))?;
                Ok(Incoming::Json { op, d: frame["d"].clone() })
            }
            Message::Binary(bytes) => {
                let Some((&[high, low, op], payload)) = bytes.split_first_chunk::<3>() else {
                    return Ok(Incoming::Quiet);
                };
                self.seq_ack = Some(u64::from(u16::from_be_bytes([high, low])));
                Ok(Incoming::Binary { op, payload: payload.to_vec() })
            }
            Message::Close(frame) => Ok(Incoming::Closed(frame.map(|frame| u16::from(frame.code)))),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(Incoming::Quiet),
        }
    }

    /// Says goodbye. The server takes a normal close as leaving the session for good.
    pub fn close(mut self) {
        let goodbye = CloseFrame { code: CloseCode::Normal, reason: "".into() };
        let _ = self.ws.close(Some(goodbye));
        let _ = self.ws.flush();
    }
}

/// What to do about a socket the server closed.
#[derive(Debug, PartialEq, Eq)]
pub enum Closed {
    /// The session survives: connect again and resume.
    Resume,
    /// The other side ended the call. Nothing failed.
    Ended,
    /// Put out of this voice server (4014): the call moved to another one, and chatd will say
    /// where, or we were removed from it.
    Removed,
    Fatal(CallFailure, &'static str),
}

pub const REMOVED: &str = "You were removed from the call, or moved to another channel.";
const REFUSED: &str = "Discord refused the voice connection. Try again, or take this call in Discord.";

/// Discord's voice close codes, in the words the dock shows. The code itself is for the log.
pub fn closed(code: Option<u16>) -> Closed {
    match code {
        // No code, a transport-level code, or the voice server restarting.
        None | Some(..4000) | Some(4015) => Closed::Resume,
        Some(4017) => Closed::Fatal(
            CallFailure::Encryption,
            "Discord raised its encryption requirement. Update tinychat, or take this call in Discord.",
        ),
        Some(4014) => Closed::Removed,
        Some(4022) => Closed::Ended,
        Some(4006) => Closed::Fatal(CallFailure::Refused, "Discord ended this voice session. Call again to rejoin."),
        Some(4009) => Closed::Fatal(CallFailure::Lost, "The voice session timed out. Call again to rejoin."),
        Some(4016) => Closed::Fatal(
            CallFailure::Encryption,
            "Discord and this tinychat share no way to encrypt call audio. Update tinychat, or take this call in Discord.",
        ),
        Some(4021) => Closed::Fatal(
            CallFailure::Refused,
            "Discord is limiting how often this account may join voice. Wait a few minutes before calling again.",
        ),
        // Not authenticated, authentication failed, server not found, and whatever comes next.
        Some(_) => Closed::Fatal(CallFailure::Refused, REFUSED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_codes_say_what_happened() {
        assert_eq!(closed(None), Closed::Resume);
        assert_eq!(closed(Some(1006)), Closed::Resume);
        assert_eq!(closed(Some(4015)), Closed::Resume, "the voice server crashed: resume");
        assert_eq!(closed(Some(4022)), Closed::Ended);
        assert_eq!(closed(Some(4014)), Closed::Removed, "moved or removed: wait to hear which");
        let failure = |code| match closed(Some(code)) {
            Closed::Fatal(reason, detail) => (reason, detail),
            other => panic!("{code}: {other:?}"),
        };
        assert_eq!(failure(4017).0, CallFailure::Encryption);
        assert!(failure(4017).1.contains("encryption requirement"));
        assert_eq!(failure(4006).0, CallFailure::Refused);
        assert!(failure(4006).1.contains("Call again"));
        assert_eq!(failure(4009).0, CallFailure::Lost);
        assert_eq!(failure(4016).0, CallFailure::Encryption);
        // No code, and nothing else of the protocol, reaches the words on the card.
        for code in [4001, 4002, 4003, 4004, 4005, 4006, 4009, 4011, 4012, 4016, 4017, 4020, 4021, 4999] {
            let (_, words) = failure(code);
            assert!(!words.chars().any(|c| c.is_ascii_digit()), "{code}: {words}");
            assert!(words.ends_with('.'), "{code}: {words}");
        }
    }

    #[test]
    fn the_endpoint_becomes_a_version_8_url() {
        assert_eq!(url("us-east1.discord.media:443"), "wss://us-east1.discord.media:443/?v=8");
        assert_eq!(url("wss://us-east1.discord.media/"), "wss://us-east1.discord.media/?v=8");
    }
}
