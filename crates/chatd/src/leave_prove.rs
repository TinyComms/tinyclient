//! Proves the person hanging up hears the leave chime, using this build's own call code and the
//! mediad beside it. The lab sets `TINYCLIENT_PROVE=leave-chime`. A build from before the fix
//! ignores that and keeps serving, so the same driver fails there.

use crate::hub::Hub;
use crate::store::Store;
use protocol::{CallState, Dave, Key, Network, VoiceJoin};
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const PATIENCE: Duration = Duration::from_secs(20);

struct Voice {
    listener: TcpListener,
    udp: UdpSocket,
}

struct Peer {
    ws: WebSocket<TcpStream>,
    seq: u64,
}

impl Peer {
    fn send(&mut self, op: u8, d: Value) -> Result<(), String> {
        self.seq += 1;
        let frame = json!({ "op": op, "d": d, "seq": self.seq });
        self.ws.send(Message::text(frame.to_string())).map_err(|err| err.to_string())
    }

    fn expect(&mut self, op: u8) -> Result<Value, String> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("no opcode {op} from mediad"));
            }
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(text.as_str()).map_err(|err| err.to_string())?;
                    if frame["op"] == 3 {
                        let ack = json!({ "op": 6, "d": { "t": frame["d"]["t"] } });
                        self.ws.send(Message::text(ack.to_string())).map_err(|err| err.to_string())?;
                    } else if frame["op"] == op {
                        return Ok(frame["d"].clone());
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(err) => return Err(format!("voice socket: {err}")),
            }
        }
    }
}

impl Voice {
    fn new() -> Result<Voice, String> {
        let udp = UdpSocket::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
        udp.set_read_timeout(Some(PATIENCE)).map_err(|err| err.to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
        Ok(Voice { listener, udp })
    }

    fn join_info(&self) -> Result<VoiceJoin, String> {
        Ok(VoiceJoin {
            network: Network::Discord,
            endpoint: self.listener.local_addr().map_err(|err| err.to_string())?.to_string(),
            token: "voice-token".into(),
            session_id: "session-1".into(),
            user_id: "111".into(),
            server_id: "900".into(),
            channel_id: "900".into(),
            dave: Dave { max_version: 1, required: false },
            muted: false,
            deafened: false,
            stream: false,
            watch: false,
            group_id: String::new(),
        })
    }

    fn identified(&self) -> Result<Peer, String> {
        let (stream, _) = self.listener.accept().map_err(|err| err.to_string())?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|err| err.to_string())?;
        let ws = tungstenite::accept(stream).map_err(|err| err.to_string())?;
        ws.get_ref().set_read_timeout(Some(Duration::from_millis(20))).map_err(|err| err.to_string())?;
        let mut peer = Peer { ws, seq: 0 };
        peer.expect(0)?;
        Ok(peer)
    }

    fn admit(&self, peer: &mut Peer) -> Result<(), String> {
        peer.send(8, json!({ "v": 8, "heartbeat_interval": 30_000 }))?;
        let port = self.udp.local_addr().map_err(|err| err.to_string())?.port();
        let modes = ["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"];
        peer.send(2, json!({ "ssrc": 1111, "ip": "127.0.0.1", "port": port, "modes": modes }))?;
        let mut request = [0u8; 128];
        let (len, from) = self.udp.recv_from(&mut request).map_err(|err| err.to_string())?;
        if len != 74 || request[..4] != [0, 1, 0, 70] {
            return Err(format!("address discovery was {len} bytes"));
        }
        let mut answer = [0u8; 74];
        answer[..4].copy_from_slice(&[0, 2, 0, 70]);
        answer[8..17].copy_from_slice(b"127.0.0.1");
        answer[72..].copy_from_slice(&from.port().to_be_bytes());
        self.udp.send_to(&answer, from).map_err(|err| err.to_string())?;
        let select = peer.expect(1)?;
        let mode = select["data"]["mode"].as_str().unwrap_or("").to_owned();
        if mode != "aead_aes256_gcm_rtpsize" {
            return Err(format!("mediad selected {mode}"));
        }
        let key = vec![9u8; 32];
        peer.send(4, json!({ "mode": mode, "secret_key": key, "dave_protocol_version": 0 }))
    }
}

fn lounge() -> Key {
    Key { network: Network::Discord, account: "111".into(), id: "900".into() }
}

fn state(hub: &Hub) -> Option<CallState> {
    hub.calls.describe().0.map(|call| call.state)
}

fn wait_until(what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        if Instant::now() >= deadline {
            return Err(format!("{what} did not happen"));
        }
        sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Joins a localhost voice server, hangs up, and checks the sink's tail is the leave chime.
pub fn run() -> Result<(), String> {
    let sink = std::env::var_os("TINYCLIENT_MEDIAD_SINK").map(PathBuf::from).ok_or("TINYCLIENT_MEDIAD_SINK is not set")?;
    if let Some(parent) = sink.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let _ = std::fs::remove_file(&sink);
    std::env::set_var("TINYCLIENT_MEDIAD_AUDIO", "tone");
    std::env::set_var("TINYCLIENT_VOICE_PLAIN", "1");

    let dir = std::env::temp_dir().join(format!("leave-chime-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let store = Store::open(&dir.join("chat.sqlite")).map_err(|err| err.to_string())?;
    let hub = Arc::new(Hub::new(store));
    hub.set_setting("", protocol::settings::VOICE_VOLUME, Some("0".into())).map_err(|err| err.to_string())?;
    hub.set_setting("", protocol::settings::CUE_VOLUME, Some("100".into())).map_err(|err| err.to_string())?;

    let voice = Voice::new()?;
    let info = voice.join_info()?;
    hub.calls.start(&hub, lounge()).map_err(|err| format!("{err:?}"))?;
    hub.calls.join(&hub, &lounge(), info);
    let mut peer = voice.identified()?;
    voice.admit(&mut peer)?;
    wait_until("joined", || state(&hub) == Some(CallState::Joined))?;
    hub.calls.end(&hub);
    wait_until("mediad gone", || hub.calls.describe().1 == false)?;

    let bytes = std::fs::read(&sink).unwrap_or_default();
    let mut tail = bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect::<Vec<_>>();
    let start = tail.len().saturating_sub(24_000);
    tail.drain(..start);
    let peak = tail.iter().map(|sample| sample.saturating_abs()).max().unwrap_or(0);
    let _ = std::fs::remove_dir_all(&dir);
    if peak > 1000 {
        Ok(())
    } else {
        Err(format!("the leave cue was not the last thing played, peak {peak}"))
    }
}
