//! Runs the real mediad binary against a fake Discord voice server on localhost: a websocket
//! that performs the documented version 8 handshake and a UDP socket that carries RTP.
//!
//! The websocket is `wss`, as Discord's is: the fake holds a certificate made for the test run
//! (the `openssl` command line makes it; macOS ships one) and mediad is told to trust it through
//! `TINYCLIENT_VOICE_TRUST`, which it honors only in a scratch `TINYCLIENT_DIR`. So every test here
//! goes through the real TLS handshake, reads with the 20 ms timeout on a TLS stream at both
//! ends, and closes through TLS. `Dribble` stands between the two for the tests that need the
//! server's bytes to arrive a few at a time.
//!
//! The fake does its own transport crypto straight on the cipher crates, so a packet that opens
//! here was sealed the way the documentation says, not merely the way mediad's own code reads.
//! No sound device is opened: `TINYCLIENT_MEDIAD_AUDIO` gives mediad a tone or silence to hear and
//! `TINYCLIENT_MEDIAD_SINK` a file to play into.

#[path = "support/mls.rs"]
mod mls;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::XChaCha20Poly1305;
use mediad::opus::{Decoder, Encoder, MAX_FRAME};
use protocol::{read_frame, write_frame, CallFailure, Dave, MediaRequest, MediaResponse, Network, VoiceJoin};
use serde_json::{json, Value};
use native_tls::{Identity, TlsAcceptor, TlsStream};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};

fn process_rss_kb(pid: u32) -> u64 {
    #[cfg(windows)]
    {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("(Get-Process -Id {pid}).WorkingSet64")])
            .output()
            .expect("working set");
        let bytes: u64 = String::from_utf8(out.stdout).unwrap().trim().parse().unwrap();
        return bytes / 1024;
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
    }
}
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const AES: &str = "aead_aes256_gcm_rtpsize";
const XCHACHA: &str = "aead_xchacha20_poly1305_rtpsize";
const KEY: [u8; 32] = [0x5a; 32];
const OUR_SSRC: u32 = 1111;
const THEIR_SSRC: u32 = 2222;
const ME: &str = "111";
const THEM: &str = "222";
const CHANNEL: u64 = 900;
const SILENCE: [u8; 3] = [0xf8, 0xff, 0xfe];
const PATIENCE: Duration = Duration::from_secs(8);

// ---- the certificate ----

/// What the fake voice server presents, and where mediad finds it to trust it.
struct Tls {
    acceptor: TlsAcceptor,
    /// A scratch `TINYCLIENT_DIR`: the trust override below is ignored without one.
    dir: PathBuf,
    certificate: PathBuf,
}

/// A self-signed certificate for 127.0.0.1, made once per test run and good for a few days.
fn tls() -> &'static Tls {
    static TLS: OnceLock<Tls> = OnceLock::new();
    TLS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tc-voice-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("openssl.cnf");
        let request = "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n\
            [dn]\nCN=tinyclient test voice server\n\
            [ext]\nsubjectAltName=IP:127.0.0.1,DNS:localhost\nkeyUsage=digitalSignature,keyEncipherment\n\
            extendedKeyUsage=serverAuth\n";
        std::fs::write(&config, request).unwrap();
        let (key, key8, certificate) = (dir.join("key.pem"), dir.join("key8.pem"), dir.join("cert.pem"));
        let openssl = |args: &[&str]| {
            let ran = Command::new("openssl").args(args).current_dir(&dir).output();
            let ran = ran.expect("these tests make their TLS certificate with the `openssl` command");
            assert!(ran.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        };
        openssl(&["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "7", "-config", "openssl.cnf", "-keyout", "key.pem", "-out", "cert.pem"]);
        // Whatever form this openssl writes keys in, the system's TLS wants PKCS#8.
        openssl(&["pkcs8", "-topk8", "-nocrypt", "-in", "key.pem", "-out", "key8.pem"]);
        let identity = Identity::from_pkcs8(&std::fs::read(&certificate).unwrap(), &std::fs::read(&key8).unwrap()).unwrap();
        let _ = std::fs::remove_file(key);
        Tls { acceptor: TlsAcceptor::new(identity).unwrap(), dir, certificate }
    })
}

// ---- mediad, as chatd runs it ----

struct Mediad {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<MediaResponse>,
    /// Events read while waiting for something else.
    seen: Vec<MediaResponse>,
}

impl Mediad {
    fn start(hears: &str, sink: Option<&PathBuf>) -> Mediad {
        let tls = tls();
        Mediad::start_in(hears, sink, &[("TINYCLIENT_DIR", &tls.dir), ("TINYCLIENT_VOICE_TRUST", &tls.certificate)])
    }

    fn start_in(hears: &str, sink: Option<&PathBuf>, env: &[(&str, &PathBuf)]) -> Mediad {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mediad"));
        command
            .arg("1")
            .env_remove("TINYCLIENT_VOICE_PLAIN")
            .env_remove("TINYCLIENT_DIR")
            .env_remove("TINYCLIENT_VOICE_TRUST")
            .env("TINYCLIENT_MEDIAD_AUDIO", hears)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(sink) = sink {
            command.env("TINYCLIENT_MEDIAD_SINK", sink);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (found, frames) = channel();
        std::thread::spawn(move || {
            while let Ok(frame) = read_frame::<MediaResponse>(&mut stdout) {
                if found.send(frame).is_err() {
                    break;
                }
            }
        });
        let mediad = Mediad { child, stdin, frames, seen: Vec::new() };
        let ready = mediad.frames.recv_timeout(PATIENCE).unwrap();
        assert!(matches!(ready, MediaResponse::Ready { .. }), "{ready:?}");
        mediad
    }

    /// One request, and the reply that answers it.
    fn ask(&mut self, request: MediaRequest) -> MediaResponse {
        write_frame(self.stdin.as_mut().unwrap(), &request).unwrap();
        loop {
            let frame = self.frames.recv_timeout(PATIENCE).expect("mediad did not answer");
            if frame.is_reply() {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    fn join(&mut self, endpoint: &str, dave: Dave) {
        assert_eq!(self.ask(MediaRequest::Join(session(endpoint, "session-1", "voice-token", dave))), MediaResponse::Done);
    }

    /// The first event that matches, already seen or still to come.
    fn event(&mut self, what: &str, matches: impl Fn(&MediaResponse) -> bool) -> MediaResponse {
        if let Some(at) = self.seen.iter().position(&matches) {
            return self.seen.remove(at);
        }
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.checked_duration_since(Instant::now());
            let frame = left.and_then(|left| self.frames.recv_timeout(left).ok());
            let Some(frame) = frame else {
                panic!("no {what} event; saw {:?}", self.seen);
            };
            if matches(&frame) {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    /// As [`Mediad::event`], with the fake's end of the websocket kept going meanwhile: its
    /// frames are read and its heartbeats answered, as a voice server would.
    fn event_serving(&mut self, peer: &mut Peer, what: &str, matches: impl Fn(&MediaResponse) -> bool) -> MediaResponse {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.seen.extend(self.frames.try_iter());
            if let Some(at) = self.seen.iter().position(&matches) {
                return self.seen.remove(at);
            }
            assert!(Instant::now() < deadline, "no {what} event; saw {:?}", self.seen);
            peer.pump();
        }
    }

    fn connected(&mut self) -> (u32, u16) {
        match self.event("Connected", |e| matches!(e, MediaResponse::Connected { .. })) {
            MediaResponse::Connected { ssrc, dave_version } => (ssrc, dave_version),
            _ => unreachable!(),
        }
    }

    fn speaking(&mut self, who: &str, on: bool) {
        let wanted = MediaResponse::Speaking { user_id: who.to_owned(), speaking: on };
        self.event(&format!("{wanted:?}"), |e| *e == wanted);
    }

    fn failed(&mut self) -> (CallFailure, String) {
        match self.event("Failed", |e| matches!(e, MediaResponse::Failed { .. })) {
            MediaResponse::Failed { reason, detail } => (reason, detail),
            _ => unreachable!(),
        }
    }

    /// How mediad exits on its own, and how long that took from now.
    fn exit(&mut self, within: Duration) -> (ExitStatus, Duration) {
        let began = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return (status, began.elapsed());
            }
            assert!(began.elapsed() < within, "mediad still running after {within:?}");
            sleep(Duration::from_millis(5));
        }
    }

    /// Closes stdin, which is the hang-up.
    fn hang_up(&mut self) -> (ExitStatus, Duration) {
        self.stdin.take();
        self.exit(Duration::from_secs(5))
    }
}

impl Drop for Mediad {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn session(endpoint: &str, session_id: &str, token: &str, dave: Dave) -> VoiceJoin {
    VoiceJoin {
        network: Network::Discord,
        endpoint: endpoint.to_owned(),
        token: token.to_owned(),
        session_id: session_id.to_owned(),
        user_id: ME.to_owned(),
        server_id: CHANNEL.to_string(),
        channel_id: CHANNEL.to_string(),
        dave,
        muted: false,
        deafened: false,
    }
}

// ---- the fake voice server ----

struct Fake {
    listener: TcpListener,
    udp: UdpSocket,
    /// Where mediad's UDP socket is, learned from its address discovery.
    client: Option<SocketAddr>,
    mode: &'static str,
    counter: u32,
}

/// The server's end of one websocket.
struct Peer {
    ws: WebSocket<TlsStream<TcpStream>>,
    seq: u64,
    /// Frames read while waiting for another opcode.
    held: Vec<(u8, Value)>,
    held_binary: Vec<(u8, Vec<u8>)>,
    heartbeats: Vec<Value>,
}

/// One RTP packet from mediad, opened.
struct Rtp {
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    payload: Vec<u8>,
}

fn nonce<const N: usize>(counter: &[u8]) -> [u8; N] {
    let mut nonce = [0u8; N];
    nonce[..4].copy_from_slice(counter);
    nonce
}

impl Peer {
    fn send(&mut self, op: u8, d: Value) {
        self.seq += 1;
        let frame = json!({ "op": op, "d": d, "seq": self.seq });
        self.ws.send(Message::text(frame.to_string())).unwrap();
    }

    fn send_binary(&mut self, op: u8, payload: &[u8]) {
        self.seq += 1;
        let mut frame = (self.seq as u16).to_be_bytes().to_vec();
        frame.push(op);
        frame.extend_from_slice(payload);
        self.ws.send(Message::binary(frame)).unwrap();
    }

    /// Reads one frame into the held lists. Heartbeats are answered. False when nothing came.
    fn pump(&mut self) -> bool {
        match self.ws.read() {
            Ok(Message::Text(text)) => {
                let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                let op = frame["op"].as_u64().unwrap() as u8;
                if op == 3 {
                    self.heartbeats.push(frame["d"].clone());
                    let ack = json!({ "op": 6, "d": { "t": frame["d"]["t"] } });
                    self.ws.send(Message::text(ack.to_string())).unwrap();
                } else {
                    self.held.push((op, frame["d"].clone()));
                }
                true
            }
            Ok(Message::Binary(bytes)) => {
                self.held_binary.push((bytes[0], bytes[1..].to_vec()));
                true
            }
            Ok(_) => true,
            Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => false,
            Err(err) => panic!("the fake's socket: {err}"),
        }
    }

    fn expect(&mut self, op: u8) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(at) = self.held.iter().position(|(held, _)| *held == op) {
                return self.held.remove(at).1;
            }
            assert!(Instant::now() < deadline, "no opcode {op} from mediad; held {:?}", self.held);
            self.pump();
        }
    }

    fn expect_binary(&mut self, op: u8) -> Vec<u8> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(at) = self.held_binary.iter().position(|(held, _)| *held == op) {
                return self.held_binary.remove(at).1;
            }
            assert!(Instant::now() < deadline, "no binary opcode {op} from mediad");
            self.pump();
        }
    }

    /// Lets mediad's frames in for a while, answering heartbeats.
    fn serve(&mut self, how_long: Duration) {
        let until = Instant::now() + how_long;
        while Instant::now() < until {
            self.pump();
        }
    }

    /// The close code mediad's side ended the socket with, if it said goodbye.
    fn closed(&mut self) -> Option<u16> {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            match self.ws.read() {
                Ok(Message::Close(frame)) => return frame.map(|frame| u16::from(frame.code)),
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return None,
            }
        }
        panic!("mediad kept the socket open");
    }

    fn close(mut self, code: u16) {
        let frame = tungstenite::protocol::CloseFrame { code: code.into(), reason: "".into() };
        let _ = self.ws.close(Some(frame));
        let _ = self.ws.flush();
        // Give the close frame time to be read before the socket goes.
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until && self.ws.read().is_ok() {}
    }
}

/// Notes the path the client asked for.
struct Asked<'a>(&'a mut String);

impl tungstenite::handshake::server::Callback for Asked<'_> {
    fn on_request(
        self,
        request: &tungstenite::handshake::server::Request,
        response: tungstenite::handshake::server::Response,
    ) -> Result<tungstenite::handshake::server::Response, tungstenite::handshake::server::ErrorResponse> {
        *self.0 = request.uri().to_string();
        Ok(response)
    }
}

impl Fake {
    fn new() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        Fake { listener, udp, client: None, mode: AES, counter: 0 }
    }

    fn endpoint(&self) -> String {
        self.listener.local_addr().unwrap().to_string()
    }

    /// The next websocket mediad opens, if it does within `within`.
    fn accept_within(&self, within: Duration) -> Option<Peer> {
        let stream = self.accept_tcp(within)?;
        let stream = tls().acceptor.accept(stream).expect("mediad did not complete the TLS handshake");
        let mut url = String::new();
        let ws = tungstenite::accept_hdr(stream, Asked(&mut url)).unwrap();
        assert_eq!(url, "/?v=8", "the voice gateway version is asked for in the URL");
        // From here both ends read with a 20 ms timeout on a TLS stream.
        ws.get_ref().get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        Some(Peer { ws, seq: 0, held: Vec::new(), held_binary: Vec::new(), heartbeats: Vec::new() })
    }

    /// The next connection mediad opens, before any TLS.
    fn accept_tcp(&self, within: Duration) -> Option<TcpStream> {
        let deadline = Instant::now() + within;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    return Some(stream);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() > deadline {
                        return None;
                    }
                    sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("accept: {err}"),
            }
        }
    }

    fn accept(&self) -> Peer {
        self.accept_within(PATIENCE).expect("mediad did not connect")
    }

    /// Identify, hello, ready, address discovery, select protocol, session description.
    /// Gives the identify mediad sent.
    fn handshake(&mut self, peer: &mut Peer, offered: &[&str], dave_version: u16, heartbeat_ms: u64) -> Value {
        let identify = peer.expect(0);
        peer.send(8, json!({ "v": 8, "heartbeat_interval": heartbeat_ms }));
        let port = self.udp.local_addr().unwrap().port();
        peer.send(2, json!({ "ssrc": OUR_SSRC, "ip": "127.0.0.1", "port": port, "modes": offered, "experiments": [] }));

        // Address discovery: 74 bytes, type 1, length 70, our SSRC. The answer is type 2.
        let mut request = [0u8; 128];
        let deadline = Instant::now() + PATIENCE;
        let from = loop {
            assert!(Instant::now() < deadline, "no address discovery from mediad");
            if let Ok((len, from)) = self.udp.recv_from(&mut request) {
                assert_eq!(len, 74);
                assert_eq!(request[..8], [0, 1, 0, 70, 0, 0, 0x04, 0x57], "discovery for ssrc 1111");
                break from;
            }
        };
        let mut answer = [0u8; 74];
        answer[..4].copy_from_slice(&[0, 2, 0, 70]);
        answer[4..8].copy_from_slice(&OUR_SSRC.to_be_bytes());
        answer[8..17].copy_from_slice(b"127.0.0.1");
        answer[72..].copy_from_slice(&from.port().to_be_bytes());
        self.udp.send_to(&answer, from).unwrap();
        self.client = Some(from);

        let select = peer.expect(1);
        assert_eq!(select["protocol"], "udp");
        assert_eq!(select["data"]["address"], "127.0.0.1");
        assert_eq!(select["data"]["port"], from.port());
        let mode = select["data"]["mode"].as_str().unwrap();
        self.mode = if mode == AES { AES } else { XCHACHA };
        assert_eq!(mode, self.mode, "a mode this server knows");
        peer.send(4, json!({ "mode": mode, "secret_key": KEY, "dave_protocol_version": dave_version }));
        identify
    }

    fn open(&self, datagram: &[u8]) -> Option<Rtp> {
        // 12 clear bytes, the sealed frame with its 16-byte tag, the 4-byte nonce counter.
        if datagram.len() < 12 + 16 + 4 || datagram[0] != 0x80 || datagram[1] != 120 {
            return None;
        }
        let end = datagram.len() - 4;
        let sealed = Payload { msg: &datagram[12..end], aad: &datagram[..12] };
        let payload = if self.mode == AES {
            Aes256Gcm::new_from_slice(&KEY).unwrap().decrypt(&nonce::<12>(&datagram[end..]).into(), sealed)
        } else {
            XChaCha20Poly1305::new_from_slice(&KEY).unwrap().decrypt(&nonce::<24>(&datagram[end..]).into(), sealed)
        };
        Some(Rtp {
            sequence: u16::from_be_bytes([datagram[2], datagram[3]]),
            timestamp: u32::from_be_bytes(datagram[4..8].try_into().unwrap()),
            ssrc: u32::from_be_bytes(datagram[8..12].try_into().unwrap()),
            payload: payload.expect("mediad's packet did not open with the session key"),
        })
    }

    /// Every RTP packet mediad sends in `how_long`. Keep-alives are not RTP and are skipped.
    fn hear(&self, how_long: Duration) -> Vec<Rtp> {
        let until = Instant::now() + how_long;
        let mut heard = Vec::new();
        let mut datagram = [0u8; 2048];
        while Instant::now() < until {
            if let Ok((len, _)) = self.udp.recv_from(&mut datagram) {
                heard.extend(self.open(&datagram[..len]));
            }
        }
        heard
    }

    /// Waits for mediad's next RTP packet.
    fn hear_one(&self) -> Rtp {
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(Instant::now() < deadline, "mediad sent no audio");
            if let Some(rtp) = self.hear(Duration::from_millis(20)).into_iter().next() {
                return rtp;
            }
        }
    }

    /// One audio packet from another member of the call.
    fn say(&mut self, sequence: u16, frame: &[u8]) {
        let mut datagram = vec![0x80, 120];
        datagram.extend_from_slice(&sequence.to_be_bytes());
        datagram.extend_from_slice(&(u32::from(sequence) * 960).to_be_bytes());
        datagram.extend_from_slice(&THEIR_SSRC.to_be_bytes());
        let counter = self.counter.to_be_bytes();
        self.counter += 1;
        let payload = Payload { msg: frame, aad: &datagram };
        let sealed = if self.mode == AES {
            Aes256Gcm::new_from_slice(&KEY).unwrap().encrypt(&nonce::<12>(&counter).into(), payload)
        } else {
            XChaCha20Poly1305::new_from_slice(&KEY).unwrap().encrypt(&nonce::<24>(&counter).into(), payload)
        };
        datagram.extend_from_slice(&sealed.unwrap());
        datagram.extend_from_slice(&counter);
        self.udp.send_to(&datagram, self.client.unwrap()).unwrap();
    }

    /// Another member talking: a packet every 20 ms by the clock, as a sender's audio clock
    /// paces them. Sleeping 20 ms after each would fall behind wherever sleeps run long (a busy
    /// CI runner makes them three times that), and mediad's playout would run dry and conceal.
    fn talk(&mut self, packets: &[(u16, &[u8])]) {
        let began = Instant::now();
        for (n, &(sequence, frame)) in packets.iter().enumerate() {
            sleep((began + Duration::from_millis(20) * n as u32).saturating_duration_since(Instant::now()));
            self.say(sequence, frame);
        }
    }
}

/// Stands between mediad and the fake and passes bytes on. What the server sends can be cut
/// into pieces that arrive one at a time, further apart than mediad's 20 ms read timeout, so a
/// read gives up inside a TLS record and inside a websocket frame.
struct Dribble {
    address: SocketAddr,
    /// Bytes per piece from the server. 0 passes everything on at once.
    piece: Arc<AtomicUsize>,
}

impl Dribble {
    fn before(fake: &Fake) -> Dribble {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = fake.listener.local_addr().unwrap();
        let piece = Arc::new(AtomicUsize::new(0));
        let size = piece.clone();
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let (Ok(client), Ok(upstream)) = (client, TcpStream::connect(server)) else {
                    return;
                };
                let (mut to_server, mut from_client) = (upstream.try_clone().unwrap(), client.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut from_client, &mut to_server);
                    let _ = to_server.shutdown(Shutdown::Write);
                });
                let (mut from_server, mut to_client, size) = (upstream, client, size.clone());
                std::thread::spawn(move || {
                    let mut bytes = [0u8; 4096];
                    while let Ok(len @ 1..) = from_server.read(&mut bytes) {
                        let piece = size.load(Ordering::Relaxed);
                        let pieces = if piece == 0 { bytes[..len].chunks(len) } else { bytes[..len].chunks(piece) };
                        for part in pieces {
                            if to_client.write_all(part).is_err() {
                                return;
                            }
                            if piece != 0 {
                                sleep(Duration::from_millis(25));
                            }
                        }
                    }
                    let _ = to_client.shutdown(Shutdown::Write);
                });
            }
        });
        Dribble { address, piece }
    }
}

// ---- audio the fake makes and judges ----

/// 20 ms frames of a 440 Hz tone, encoded.
fn tone_frames(count: usize) -> Vec<Vec<u8>> {
    let mut encoder = Encoder::new().unwrap();
    (0..count)
        .map(|frame| {
            let pcm: Vec<i16> = (0..960)
                .flat_map(|n| {
                    let t = (frame * 960 + n) as f64 / 48_000.;
                    [((t * 440. * std::f64::consts::TAU).sin() * 8000.) as i16; 2]
                })
                .collect();
            let mut packet = [0u8; 1275];
            let len = encoder.encode(&pcm, &mut packet).unwrap();
            packet[..len].to_vec()
        })
        .collect()
}

fn rms(pcm: &[i16]) -> f64 {
    (pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt()
}

/// Decodes frames in order with libopus. Panics on one that is not Opus. Gives the level of
/// what came out, past the codec's start.
fn decoded_level(frames: &[Vec<u8>]) -> f64 {
    let mut decoder = Decoder::new().unwrap();
    let mut heard = Vec::new();
    let mut pcm = vec![0i16; MAX_FRAME * 2];
    for frame in frames {
        let samples = decoder.decode(Some(frame), 0, &mut pcm).expect("not a valid Opus packet");
        assert_eq!(samples, 960, "20 ms at 48 kHz");
        heard.extend_from_slice(&pcm[..samples * 2]);
    }
    rms(&heard[heard.len() / 2..])
}

fn sink(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tc-s7-{}-{name}.pcm", std::process::id()))
}

fn played(sink: &PathBuf) -> Vec<i16> {
    let bytes = std::fs::read(sink).unwrap();
    bytes.as_chunks::<2>().0.iter().map(|&pair| i16::from_le_bytes(pair)).collect()
}

/// The tone's level is 8000 / sqrt(2), about 5657. The codec is allowed some of it.
fn is_the_tone(level: f64) -> bool {
    (4000. ..7000.).contains(&level)
}

const NO_DAVE: Dave = Dave { max_version: 1, required: false };

// ---- what is proven ----

#[test]
fn the_handshake_completes_and_the_tone_arrives_as_opus_over_aes_gcm() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("tone", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    let identify = fake.handshake(&mut peer, &[XCHACHA, AES, "xsalsa20_poly1305"], 0, 30_000);
    assert_eq!(identify["server_id"], "900");
    assert_eq!(identify["user_id"], ME);
    assert_eq!(identify["session_id"], "session-1");
    assert_eq!(identify["token"], "voice-token");
    assert_eq!(identify["max_dave_protocol_version"], 1);
    assert_eq!(fake.mode, AES, "AES-GCM is chosen when offered");

    assert_eq!(mediad.connected(), (OUR_SSRC, 0));
    let devices = mediad.event("Devices", |e| matches!(e, MediaResponse::Devices { .. }));
    assert_eq!(devices, MediaResponse::Devices { input: "tone".into(), output: "null".into() });
    mediad.speaking(ME, true);

    // Our SSRC is announced, then speaking is told to the gateway.
    assert_eq!(peer.expect(5), json!({ "speaking": 0, "delay": 0, "ssrc": OUR_SSRC }));
    assert_eq!(peer.expect(5), json!({ "speaking": 1, "delay": 0, "ssrc": OUR_SSRC }));

    let heard = fake.hear(Duration::from_millis(700));
    assert!(heard.len() >= 25, "{} packets in 700 ms", heard.len());
    for pair in heard.windows(2) {
        assert_eq!(pair[1].ssrc, OUR_SSRC);
        assert_eq!(pair[1].sequence, pair[0].sequence.wrapping_add(1), "sequence rises by one");
        assert_eq!(pair[1].timestamp, pair[0].timestamp.wrapping_add(960), "timestamp rises by 20 ms");
    }
    let frames: Vec<Vec<u8>> = heard.into_iter().map(|rtp| rtp.payload).collect();
    let level = decoded_level(&frames);
    assert!(is_the_tone(level), "level {level}");

    let (status, took) = mediad.hang_up();
    assert_eq!(status.code(), Some(0));
    assert!(took < Duration::from_secs(2), "hang-up took {took:?}");
    assert_eq!(peer.closed(), Some(1000), "mediad said goodbye to the gateway");
    // The last thing on the wire after speech is silence frames.
    let last = fake.hear(Duration::from_millis(100));
    assert!(last.iter().rev().take(3).all(|rtp| rtp.payload == SILENCE) && !last.is_empty());
}

#[test]
fn xchacha_is_used_when_aes_is_not_offered() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("tone", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[XCHACHA], 0, 30_000);
    assert_eq!(fake.mode, XCHACHA);
    mediad.connected();
    let frames: Vec<Vec<u8>> = fake.hear(Duration::from_millis(500)).into_iter().map(|rtp| rtp.payload).collect();
    assert!(frames.len() >= 15);
    assert!(is_the_tone(decoded_level(&frames)));
}

#[test]
fn heartbeats_acknowledge_the_last_sequence_number() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 150);
    mediad.connected();
    peer.serve(Duration::from_millis(700));
    assert!((3..=6).contains(&peer.heartbeats.len()), "{} heartbeats in 700 ms at 150 ms", peer.heartbeats.len());
    // Hello, ready and the session description were frames 1 to 3.
    assert!(peer.heartbeats.iter().all(|beat| beat["t"].is_u64()), "{:?}", peer.heartbeats);
    assert_eq!(peer.heartbeats.last().unwrap()["seq_ack"], 3, "{:?}", peer.heartbeats);
    // With silence to hear, nothing is sent.
    assert!(fake.hear(Duration::from_millis(300)).is_empty());
}

#[test]
fn another_member_is_announced_decoded_and_seen_speaking() {
    let mut fake = Fake::new();
    let played_to = sink("incoming");
    let mut mediad = Mediad::start("null", Some(&played_to));
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();

    peer.send(11, json!({ "user_ids": [THEM] }));
    peer.send(5, json!({ "user_id": THEM, "ssrc": THEIR_SSRC, "speaking": 1 }));
    let source = mediad.event("Source", |e| matches!(e, MediaResponse::Source { .. }));
    assert_eq!(source, MediaResponse::Source { user_id: THEM.into(), ssrc: THEIR_SSRC });

    // One second of tone with packet 20 lost and 30 and 31 swapped, then the silence trail.
    let frames = tone_frames(50);
    let mut order: Vec<usize> = (0..50).filter(|&n| n != 20).collect();
    order.swap(28, 29);
    let tone: Vec<(u16, &[u8])> = order.into_iter().map(|n| (n as u16, &frames[n][..])).collect();
    fake.talk(&tone);
    mediad.speaking(THEM, true);
    let trail: Vec<(u16, &[u8])> = (50..55).map(|n| (n, &SILENCE[..])).collect();
    fake.talk(&trail);
    mediad.speaking(THEM, false);
    peer.send(13, json!({ "user_id": THEM }));
    assert_eq!(mediad.event("Gone", |e| matches!(e, MediaResponse::Gone { .. })), MediaResponse::Gone { user_id: THEM.into() });

    assert_eq!(mediad.hang_up().0.code(), Some(0));
    let pcm = played(&played_to);
    // The mix holds about a second of the tone: find its loudest 400 ms.
    let loudest = pcm.chunks(960 * 2 * 20).map(rms).fold(0., f64::max);
    assert!(is_the_tone(loudest), "played level {loudest}");
    let _ = std::fs::remove_file(played_to);
}

#[test]
fn mute_sends_a_trail_of_silence_and_then_nothing_and_deafen_plays_nothing() {
    let mut fake = Fake::new();
    let played_to = sink("deafen");
    let mut mediad = Mediad::start("tone", Some(&played_to));
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();
    mediad.speaking(ME, true);
    fake.hear_one();

    assert_eq!(mediad.ask(MediaRequest::Audio { muted: true, deafened: false }), MediaResponse::Done);
    mediad.speaking(ME, false);
    let after = fake.hear(Duration::from_millis(400));
    let silence = after.iter().rev().take_while(|rtp| rtp.payload == SILENCE).count();
    assert_eq!(silence, 5, "five silence frames end the spurt");
    assert!(after.len() <= 5 + 10, "{} packets after mute", after.len());
    assert!(fake.hear(Duration::from_millis(500)).is_empty(), "muted means no packets");
    peer.serve(Duration::from_millis(50));
    assert_eq!(peer.expect(5)["speaking"], 0);
    assert_eq!(peer.expect(5)["speaking"], 1);
    assert_eq!(peer.expect(5)["speaking"], 0, "the gateway is told we stopped");

    // Deafened: the other member's packets arrive and nothing is played.
    assert_eq!(mediad.ask(MediaRequest::Audio { muted: true, deafened: true }), MediaResponse::Done);
    peer.send(5, json!({ "user_id": THEM, "ssrc": THEIR_SSRC, "speaking": 1 }));
    sleep(Duration::from_millis(100));
    let frames = tone_frames(25);
    let tone: Vec<(u16, &[u8])> = frames.iter().enumerate().map(|(n, frame)| (n as u16, &frame[..])).collect();
    fake.talk(&tone);

    // Unmuted, the tone is sent again.
    assert_eq!(mediad.ask(MediaRequest::Audio { muted: false, deafened: true }), MediaResponse::Done);
    mediad.speaking(ME, true);
    let again: Vec<Vec<u8>> = fake.hear(Duration::from_millis(400)).into_iter().map(|rtp| rtp.payload).collect();
    assert!(again.len() >= 12 && is_the_tone(decoded_level(&again)));

    assert_eq!(mediad.hang_up().0.code(), Some(0));
    let loudest = played(&played_to).chunks(960 * 2 * 5).map(rms).fold(0., f64::max);
    assert!(loudest < 1., "deafened, yet level {loudest} was played");
    let _ = std::fs::remove_file(played_to);
}

#[test]
fn close_codes_fail_the_call_in_words_and_mediad_exits() {
    let cases = [
        (4017, CallFailure::Encryption, "encryption requirement"),
        (4006, CallFailure::Refused, "Call again to rejoin"),
        (4004, CallFailure::Refused, "Discord refused the voice connection"),
        (4016, CallFailure::Encryption, "no way to encrypt"),
    ];
    for (code, reason, words) in cases {
        let fake = Fake::new();
        let mut mediad = Mediad::start("null", None);
        mediad.join(&fake.endpoint(), NO_DAVE);
        let mut peer = fake.accept();
        peer.expect(0);
        peer.close(code);
        let (got, detail) = mediad.failed();
        assert_eq!(got, reason, "{code}");
        assert!(detail.contains(words), "{code}: {detail}");
        assert!(!detail.contains(&code.to_string()), "{code}: the code is for the log, not the card: {detail}");
        let (status, _) = mediad.exit(Duration::from_secs(2));
        assert_ne!(status.code(), Some(0), "{code}: a failed call is not a clean exit");
        assert!(fake.accept_within(Duration::from_millis(300)).is_none(), "{code}: no retry after a refusal");
    }
}

#[test]
fn a_call_that_must_be_encrypted_fails_rather_than_run_without() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("tone", None);
    mediad.join(&fake.endpoint(), Dave { max_version: 1, required: true });
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    let (reason, detail) = mediad.failed();
    assert_eq!(reason, CallFailure::Encryption);
    assert!(detail.contains("without end-to-end encryption"), "{detail}");
    mediad.exit(Duration::from_secs(2));
    assert!(mediad.seen.iter().all(|e| !matches!(e, MediaResponse::Connected { .. })), "never reported as joined");
    assert!(fake.hear(Duration::from_millis(200)).is_empty(), "no audio was sent");
}

#[test]
fn a_dropped_socket_is_resumed_and_the_audio_never_stops() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("tone", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();
    fake.hear_one();
    let sent = peer.seq;
    // The server vanishes without a close frame.
    drop(peer);

    mediad.event("Interrupted", |e| *e == MediaResponse::Interrupted);
    assert!(fake.hear(Duration::from_millis(300)).len() >= 10, "audio goes on over UDP meanwhile");
    let mut peer = fake.accept();
    let resume = peer.expect(7);
    assert_eq!(resume["server_id"], "900");
    assert_eq!(resume["session_id"], "session-1");
    assert_eq!(resume["token"], "voice-token");
    assert_eq!(resume["seq_ack"], sent, "the last frame it saw");
    peer.send(8, json!({ "v": 8, "heartbeat_interval": 30_000 }));
    peer.send(9, json!(null));
    assert_eq!(mediad.connected(), (OUR_SSRC, 0));

    // 4015 is the voice server crashing: resume again, not a failure.
    peer.close(4015);
    mediad.event("Interrupted", |e| *e == MediaResponse::Interrupted);
    let mut peer = fake.accept();
    peer.expect(7);
    peer.send(9, json!(null));
    mediad.connected();
    assert_eq!(mediad.hang_up().0.code(), Some(0));
}

#[test]
fn a_server_that_never_takes_us_is_tried_three_times_and_no_more() {
    let fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    let began = Instant::now();
    mediad.join(&fake.endpoint(), NO_DAVE);
    for attempt in 1..=3 {
        let mut peer = fake.accept();
        peer.expect(0);
        // Gone before ready, with no close code: nothing says do not try again.
        drop(peer);
        if attempt == 2 {
            assert!(began.elapsed() >= Duration::from_millis(250), "the second try waits for the first backoff");
        }
    }
    let (reason, detail) = mediad.failed();
    assert_eq!(reason, CallFailure::Lost);
    assert_eq!(detail, "Discord's voice server didn't answer. Try again in a moment.");
    assert!(began.elapsed() >= Duration::from_millis(750), "backoff between tries: {:?}", began.elapsed());
    mediad.exit(Duration::from_secs(2));
    assert!(fake.accept_within(Duration::from_secs(2)).is_none(), "a fourth try");
    assert!(mediad.seen.iter().all(|e| !matches!(e, MediaResponse::Connected { .. })));
}

/// The worst case for "I pressed Call and nothing happens": the server takes the connection and
/// then says nothing at all. Three tries of three seconds and their pauses, and the user knows.
#[test]
fn a_voice_server_that_goes_silent_is_given_up_within_ten_seconds() {
    let fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    let began = Instant::now();
    mediad.join(&fake.endpoint(), NO_DAVE);
    // Held open and never answered, not even with TLS.
    let held: Vec<TcpStream> = (0..3).map(|_| fake.accept_tcp(PATIENCE).expect("a try")).collect();
    let (reason, detail) = match mediad.frames.recv_timeout(Duration::from_secs(11)) {
        Ok(MediaResponse::Failed { reason, detail }) => (reason, detail),
        other => panic!("no failure within ten seconds of the join: {other:?} after {:?}", began.elapsed()),
    };
    let took = began.elapsed();
    assert!(took < Duration::from_millis(10_300), "{took:?}");
    assert!(took > Duration::from_secs(8), "three whole tries: {took:?}");
    assert_eq!(reason, CallFailure::Lost);
    assert_eq!(detail, "Couldn't reach Discord's voice server. Check your internet connection, then try again.");
    drop(held);
    mediad.exit(Duration::from_secs(2));
}

/// The websocket works and UDP does not: a VPN or firewall. Said as that, and fast.
#[test]
fn a_voice_server_that_answers_but_whose_udp_is_blocked_says_so() {
    let fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    let began = Instant::now();
    mediad.join(&fake.endpoint(), NO_DAVE);
    // A port nothing answers on: bound here, never read.
    let deaf = UdpSocket::bind("127.0.0.1:0").unwrap();
    for _ in 0..3 {
        let mut peer = fake.accept();
        peer.expect(0);
        peer.send(8, json!({ "v": 8, "heartbeat_interval": 30_000 }));
        let port = deaf.local_addr().unwrap().port();
        peer.send(2, json!({ "ssrc": OUR_SSRC, "ip": "127.0.0.1", "port": port, "modes": [AES] }));
        // mediad gives the try up and goes, without a goodbye.
        assert_eq!(peer.closed(), None);
    }
    let (reason, detail) = mediad.failed();
    assert_eq!(reason, CallFailure::Lost);
    assert!(detail.contains("call audio couldn't get through") && detail.contains("VPN or firewall"), "{detail}");
    assert!(began.elapsed() < Duration::from_secs(8), "{:?}", began.elapsed());
    mediad.exit(Duration::from_secs(2));
}

#[test]
fn a_dropped_call_that_cannot_resume_is_lost_after_three_resumes() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();
    drop(peer);
    for _ in 0..3 {
        let mut peer = fake.accept();
        peer.expect(7);
        drop(peer);
    }
    let (reason, detail) = mediad.failed();
    assert_eq!(reason, CallFailure::Lost);
    assert!(detail.contains("didn't come back"), "{detail}");
    mediad.exit(Duration::from_secs(2));
    assert!(fake.accept_within(Duration::from_secs(1)).is_none(), "a fourth resume");
}

// ---- TLS ----

/// The trust override is for scratch directories only. Outside one, a voice server the system
/// does not trust is not talked to: no identify, no token, no call.
#[test]
fn a_certificate_the_system_does_not_trust_is_refused_unless_in_a_scratch_directory() {
    let trust = &tls().certificate;
    for env in [&[][..], &[("TINYCLIENT_VOICE_TRUST", trust)][..]] {
        let fake = Fake::new();
        let mut mediad = Mediad::start_in("null", None, env);
        mediad.join(&fake.endpoint(), NO_DAVE);
        for _ in 0..3 {
            let stream = fake.accept_tcp(PATIENCE).expect("a try");
            let refused = tls().acceptor.accept(stream);
            assert!(refused.is_err(), "mediad accepted a certificate nobody vouches for");
        }
        let (reason, detail) = mediad.failed();
        assert_eq!(reason, CallFailure::Lost);
        assert!(detail.starts_with("Couldn't reach Discord's voice server."), "{detail}");
        assert!(!detail.to_lowercase().contains("certificate"), "what TLS said is for the log: {detail}");
        mediad.exit(Duration::from_secs(2));
    }
}

/// Frames that arrive a few bytes at a time, further apart than the read timeout: every read
/// gives up inside a TLS record or between the records of one frame, many times per frame.
/// Nothing may be lost or taken twice, and our own heartbeats must go out meanwhile.
#[test]
fn frames_that_arrive_in_pieces_over_tls_are_read_whole_while_heartbeats_go_on() {
    let mut fake = Fake::new();
    let dribble = Dribble::before(&fake);
    let mut mediad = Mediad::start("null", None);
    mediad.join(&dribble.address.to_string(), NO_DAVE);
    let mut peer = fake.accept();
    // mediad gives a server up after two heartbeats go unanswered. The answers come back
    // through the dribble too, behind whatever is still in it.
    fake.handshake(&mut peer, &[AES], 0, 1500);
    mediad.connected();

    // 16 bytes every 25 ms: a frame's TLS record of about a hundred bytes comes in six pieces,
    // each later than the 20 ms a read waits. Each frame is sent once the one before arrived,
    // so an answer to a heartbeat never waits behind more than one of them: on a runner where
    // the 25 ms sleeps run long, all of them at once could hold it back past mediad's patience.
    dribble.piece.store(16, Ordering::Relaxed);
    let began = Instant::now();
    for n in 0..4u32 {
        peer.send(5, json!({ "user_id": (500 + n).to_string(), "ssrc": 5000 + n, "speaking": 1 }));
        let wanted = MediaResponse::Source { user_id: (500 + n).to_string(), ssrc: 5000 + n };
        mediad.event_serving(&mut peer, "a Source that arrived in pieces", |e| *e == wanted);
    }
    // And one that takes dozens of reads to arrive.
    let crowd: Vec<String> = (0..20).map(|n| (10_000_000_000_000_000u64 + n).to_string()).collect();
    peer.send(11, json!({ "user_ids": crowd }));
    peer.send(5, json!({ "user_id": THEM, "ssrc": THEIR_SSRC, "speaking": 1 }));
    let last = MediaResponse::Source { user_id: THEM.into(), ssrc: THEIR_SSRC };
    mediad.event_serving(&mut peer, "the Source after the large frame", |e| *e == last);
    let took = began.elapsed();
    assert!(took > Duration::from_secs(1), "the frames were not cut up: {took:?}");
    let sources = mediad.seen.iter().filter(|e| matches!(e, MediaResponse::Source { .. })).count();
    assert_eq!(sources, 0, "each frame was taken once");
    assert!(mediad.seen.iter().all(|e| *e != MediaResponse::Interrupted), "the connection held: {:?}", mediad.seen);

    // While all that trickled in, mediad kept its own side going.
    peer.serve(Duration::from_millis(100));
    assert!(!peer.heartbeats.is_empty(), "no heartbeat in {took:?} at one every 1.5 s");
    // Six frames since the session description, which was the third.
    peer.heartbeats.clear();
    peer.serve(Duration::from_millis(1800));
    assert_eq!(peer.heartbeats.last().expect("a heartbeat")["seq_ack"], 9, "every frame was counted");

    // A close that arrives in pieces is still a close with its code.
    peer.close(4017);
    assert_eq!(mediad.failed().0, CallFailure::Encryption);
    mediad.exit(Duration::from_secs(3));
}

// ---- the call moves ----

/// Discord moves a call to another voice server (a region change) or hands out a new session:
/// chatd sends `Rejoin`. The old connection goes, the new one comes up, and the call, the
/// devices and what was already played stay.
#[test]
fn a_call_that_moves_to_another_voice_server_keeps_its_devices_and_goes_on() {
    let (mut old, mut new) = (Fake::new(), Fake::new());
    let played_to = sink("moved");
    let mut mediad = Mediad::start("tone", Some(&played_to));
    mediad.join(&old.endpoint(), NO_DAVE);
    let mut first = old.accept();
    old.handshake(&mut first, &[AES], 0, 30_000);
    assert_eq!(mediad.connected(), (OUR_SSRC, 0));
    mediad.speaking(ME, true);
    // Someone is heard on the first server, so there is something played before the move.
    first.send(5, json!({ "user_id": THEM, "ssrc": THEIR_SSRC, "speaking": 1 }));
    mediad.event("Source", |e| matches!(e, MediaResponse::Source { .. }));
    let frames = tone_frames(25);
    let tone: Vec<(u16, &[u8])> = frames.iter().enumerate().map(|(n, frame)| (n as u16, &frame[..])).collect();
    old.talk(&tone);
    mediad.speaking(THEM, true);
    let devices = mediad.seen.iter().filter(|e| matches!(e, MediaResponse::Devices { .. })).count();
    assert_eq!(devices, 1, "the devices were opened once");

    let moved = session(&new.endpoint(), "session-2", "voice-token-2", NO_DAVE);
    assert_eq!(mediad.ask(MediaRequest::Rejoin(moved)), MediaResponse::Done);
    mediad.event("Interrupted", |e| *e == MediaResponse::Interrupted);
    assert_eq!(first.closed(), Some(1000), "the old voice server is told we left it");
    mediad.speaking(ME, false);
    old.hear(Duration::from_millis(100));

    let mut second = new.accept();
    let identify = new.handshake(&mut second, &[AES], 0, 30_000);
    assert_eq!(identify["session_id"], "session-2", "a fresh identify, not a resume of the old session");
    assert_eq!(identify["token"], "voice-token-2");
    assert!(identify.get("seq_ack").is_none());
    assert_eq!(mediad.connected(), (OUR_SSRC, 0));
    mediad.speaking(ME, true);
    let frames: Vec<Vec<u8>> = new.hear(Duration::from_millis(500)).into_iter().map(|rtp| rtp.payload).collect();
    assert!(frames.len() >= 15 && is_the_tone(decoded_level(&frames)), "{} packets on the new server", frames.len());
    assert!(old.hear(Duration::from_millis(100)).is_empty(), "nothing more goes to the old one");

    // Mute still works, and a second move is as good as the first.
    assert_eq!(mediad.ask(MediaRequest::Audio { muted: true, deafened: false }), MediaResponse::Done);
    let back = session(&old.endpoint(), "session-3", "voice-token-3", NO_DAVE);
    assert_eq!(mediad.ask(MediaRequest::Rejoin(back)), MediaResponse::Done);
    let mut third = old.accept();
    assert_eq!(old.handshake(&mut third, &[AES], 0, 30_000)["session_id"], "session-3");
    mediad.connected();
    assert!(old.hear(Duration::from_millis(300)).is_empty(), "muted before the move, muted after");

    assert_eq!(mediad.hang_up().0.code(), Some(0));
    // Each connection says which devices it uses; they are the same ones throughout.
    let same = MediaResponse::Devices { input: "tone".into(), output: "null".into() };
    assert!(mediad.seen.iter().all(|e| !matches!(e, MediaResponse::Failed { .. })), "{:?}", mediad.seen);
    assert!(mediad.seen.iter().filter(|e| matches!(e, MediaResponse::Devices { .. })).all(|e| *e == same));
    // One sink file for the whole call: what was heard before the move is still in it.
    let loudest = played(&played_to).chunks(960 * 2 * 10).map(rms).fold(0., f64::max);
    assert!(is_the_tone(loudest), "played level {loudest}");
    let _ = std::fs::remove_file(played_to);
}

/// On a region change Discord closes the old voice connection with 4014, and the new session
/// comes over the gateway a moment later. The call waits for it instead of ending.
#[test]
fn a_voice_server_that_puts_us_out_is_a_move_when_a_new_session_follows_and_a_removal_when_not() {
    let (mut old, mut new) = (Fake::new(), Fake::new());
    let mut mediad = Mediad::start("tone", None);
    mediad.join(&old.endpoint(), NO_DAVE);
    let mut first = old.accept();
    old.handshake(&mut first, &[AES], 0, 30_000);
    mediad.connected();
    first.close(4014);
    mediad.event("Interrupted", |e| *e == MediaResponse::Interrupted);
    sleep(Duration::from_millis(700));
    assert!(mediad.seen.iter().all(|e| !matches!(e, MediaResponse::Failed { .. })), "it waits to hear");
    let moved = session(&new.endpoint(), "session-2", "voice-token-2", NO_DAVE);
    assert_eq!(mediad.ask(MediaRequest::Rejoin(moved)), MediaResponse::Done);
    let mut second = new.accept();
    new.handshake(&mut second, &[AES], 0, 30_000);
    mediad.connected();
    assert!(new.hear(Duration::from_millis(300)).len() >= 8);

    // Put out again, and this time nobody says where to: removed.
    let began = Instant::now();
    second.close(4014);
    let (reason, detail) = mediad.failed();
    assert_eq!(reason, CallFailure::Refused);
    assert_eq!(detail, "You were removed from the call, or moved to another channel.");
    assert!(began.elapsed() >= Duration::from_secs(4), "it waited for word of a move: {:?}", began.elapsed());
    mediad.exit(Duration::from_secs(2));
}

/// The end-to-end group belongs to a voice connection. After a move the identity is kept and a
/// fresh key package is offered on the new connection.
#[test]
fn a_move_offers_a_fresh_key_package_on_the_new_connection() {
    let (mut old, mut new) = (Fake::new(), Fake::new());
    let dave = Dave { max_version: 1, required: true };
    let mut mediad = Mediad::start("tone", None);
    mediad.join(&old.endpoint(), dave);
    let mut first = old.accept();
    old.handshake(&mut first, &[AES], 1, 30_000);
    assert_eq!(mediad.connected(), (OUR_SSRC, 1));
    let package = first.expect_binary(26);

    assert_eq!(mediad.ask(MediaRequest::Rejoin(session(&new.endpoint(), "session-2", "t2", dave))), MediaResponse::Done);
    let mut second = new.accept();
    let identify = new.handshake(&mut second, &[AES], 1, 30_000);
    assert_eq!(identify["max_dave_protocol_version"], 1);
    assert_eq!(mediad.connected(), (OUR_SSRC, 1));
    let again = second.expect_binary(26);
    assert!(!again.is_empty() && again != package, "a new key package for the new connection");
    assert_eq!(mediad.hang_up().0.code(), Some(0));
}

/// A `Rejoin` that comes while the first join is still trying replaces it.
#[test]
fn a_move_before_the_first_join_finished_is_followed() {
    let (old, mut new) = (Fake::new(), Fake::new());
    let mut mediad = Mediad::start("null", None);
    mediad.join(&old.endpoint(), NO_DAVE);
    let mut first = old.accept();
    first.expect(0);
    assert_eq!(mediad.ask(MediaRequest::Rejoin(session(&new.endpoint(), "session-2", "t2", NO_DAVE))), MediaResponse::Done);
    let mut second = new.accept();
    assert_eq!(new.handshake(&mut second, &[AES], 0, 30_000)["session_id"], "session-2");
    mediad.connected();
    assert!(mediad.seen.iter().all(|e| *e != MediaResponse::Interrupted), "nothing was up to interrupt");
    assert_eq!(mediad.hang_up().0.code(), Some(0));
}

// ---- devices, the meter, our own voice ----

#[test]
fn devices_are_listed_at_start_and_the_meter_runs_only_while_asked_for() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("tone", None);
    // Before any join: the device menu can be opened while a call still rings.
    let listed = MediaResponse::DeviceList { inputs: vec!["tone".into()], outputs: vec!["null".into()] };
    mediad.event("DeviceList", |e| *e == listed);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();
    mediad.speaking(ME, true);
    sleep(Duration::from_millis(400));
    while let Ok(frame) = mediad.frames.try_recv() {
        mediad.seen.push(frame);
    }
    assert!(mediad.seen.iter().all(|e| !matches!(e, MediaResponse::Levels { .. })), "nobody asked for levels");

    // The menu opens: the list again, at once, and our level.
    assert_eq!(mediad.ask(MediaRequest::Meter { on: true }), MediaResponse::Done);
    mediad.event("DeviceList", |e| *e == listed);
    let MediaResponse::Levels { input, .. } = mediad.event("Levels", |e| matches!(e, MediaResponse::Levels { .. })) else {
        unreachable!()
    };
    assert!((55..75).contains(&input), "the tone reads {input}");
    // Muted, the meter stays live so the microphone can be tried, and nothing is sent.
    assert_eq!(mediad.ask(MediaRequest::Audio { muted: true, deafened: false }), MediaResponse::Done);
    mediad.speaking(ME, false);
    fake.hear(Duration::from_millis(300));
    assert!(fake.hear(Duration::from_millis(300)).is_empty());

    assert_eq!(mediad.ask(MediaRequest::Meter { on: false }), MediaResponse::Done);
    sleep(Duration::from_millis(150));
    while mediad.frames.try_recv().is_ok() {}
    sleep(Duration::from_millis(400));
    assert!(mediad.frames.try_recv().is_err(), "the menu closed and something is still reported");
    assert_eq!(mediad.hang_up().0.code(), Some(0));
}

/// The soft start of a word is under the gate. It is sent anyway, ahead of the frame that
/// opened the gate and stamped with the time it was heard.
#[test]
fn the_start_of_a_word_is_sent_though_it_was_too_quiet_to_open_the_gate() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("onset", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();
    // Two words' worth, cut into spurts where the timestamps jump.
    let heard = fake.hear(Duration::from_millis(2600));
    let mut spurts: Vec<Vec<&Rtp>> = Vec::new();
    for rtp in &heard {
        match spurts.last_mut() {
            Some(spurt) if spurt.last().unwrap().timestamp.wrapping_add(960) == rtp.timestamp => spurt.push(rtp),
            _ => spurts.push(vec![rtp]),
        }
    }
    assert!(heard.windows(2).all(|pair| pair[1].sequence == pair[0].sequence.wrapping_add(1)), "no packet out of order");
    // A whole spurt: one that is neither cut off by the start of listening nor by its end.
    let whole = spurts.iter().find(|spurt| spurt.len() > 20 && spurt.last().unwrap().payload == SILENCE);
    let whole = whole.unwrap_or_else(|| panic!("no whole spurt among {:?}", spurts.iter().map(Vec::len).collect::<Vec<_>>()));
    let mut decoder = Decoder::new().unwrap();
    let mut pcm = vec![0i16; MAX_FRAME * 2];
    let levels: Vec<f64> = whole[..4]
        .iter()
        .map(|rtp| {
            let samples = decoder.decode(Some(&rtp.payload), 0, &mut pcm).unwrap();
            rms(&pcm[..samples * 2])
        })
        .collect();
    // 40 ms of the faint lead-in (about 70), then the word (about 5657).
    assert!(levels[0] > 5. && levels[0] < 400. && levels[1] < 400., "the lead-in was not sent first: {levels:?}");
    assert!(levels[3] > 3000., "then the word: {levels:?}");
    assert_eq!(mediad.hang_up().0.code(), Some(0));
}

#[test]
fn video_is_refused_in_words_on_a_voice_call_and_reports_no_stream() {
    let mut fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    // Before a join there is no engine in the call and the request is only noted.
    assert!(matches!(mediad.ask(MediaRequest::ShareSend { on: true }), MediaResponse::Streams { video } if video.len() == 1));
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 0, 30_000);
    mediad.connected();
    assert_eq!(mediad.ask(MediaRequest::Streams), MediaResponse::Streams { video: vec![] }, "a voice call holds no encode");
    for request in [
        MediaRequest::ShareSend { on: true },
        MediaRequest::ShareWatch { source: Some(THEM.into()) },
        MediaRequest::Speaker { source: Some(THEM.into()) },
    ] {
        let MediaResponse::Refused { detail } = mediad.ask(request) else {
            panic!("video was accepted on a voice call");
        };
        assert!(detail.contains("voice only"), "{detail}");
    }
    assert_eq!(mediad.ask(MediaRequest::ShareSend { on: false }), MediaResponse::Streams { video: vec![] });
    assert_eq!(mediad.hang_up().0.code(), Some(0));
}

#[test]
fn hang_up_while_still_connecting_exits_cleanly() {
    let fake = Fake::new();
    let mut mediad = Mediad::start("null", None);
    mediad.join(&fake.endpoint(), NO_DAVE);
    let mut peer = fake.accept();
    peer.expect(0);
    let (status, took) = mediad.hang_up();
    assert_eq!(status.code(), Some(0));
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn requests_before_and_without_a_join_keep_their_one_reply() {
    let mut mediad = Mediad::start("null", None);
    assert_eq!(mediad.ask(MediaRequest::Audio { muted: true, deafened: false }), MediaResponse::Done);
    assert_eq!(mediad.ask(MediaRequest::Devices { input: None, output: None }), MediaResponse::Done);
    assert_eq!(mediad.ask(MediaRequest::Streams), MediaResponse::Streams { video: vec![] });
    let (status, took) = mediad.hang_up();
    assert_eq!(status.code(), Some(0));
    assert!(took < Duration::from_millis(500), "nothing to say goodbye to: {took:?}");
}

/// The whole end-to-end encrypted path: the fake is the voice server (MLS external sender) and
/// a second member of the call, which runs its own `davey` session.
#[test]
fn dave_two_members_exchange_end_to_end_encrypted_audio() {
    use davey::{DaveSession, MediaType};
    let mut fake = Fake::new();
    let played_to = sink("dave");
    let mut mediad = Mediad::start("tone", Some(&played_to));
    mediad.join(&fake.endpoint(), Dave { max_version: 1, required: true });
    let mut peer = fake.accept();
    fake.handshake(&mut peer, &[AES], 1, 30_000);
    assert_eq!(mediad.connected(), (OUR_SSRC, 1));

    // mediad offers its key package; the server names itself the external sender.
    let server = mls::Server::new();
    assert!(!peer.expect_binary(26).is_empty());
    peer.send_binary(25, &server.package());

    // The second member joins: the server proposes it, mediad commits and welcomes it.
    let version = std::num::NonZeroU16::new(1).unwrap();
    let mut them = DaveSession::new(version, 222, CHANNEL, None).unwrap();
    them.set_external_sender(&server.package()).unwrap();
    let package = them.create_key_package().unwrap();
    peer.send(11, json!({ "user_ids": [THEM] }));
    peer.send_binary(27, &server.add(&package, CHANNEL, 0));
    let (commit, welcome) = mls::split(&peer.expect_binary(28));
    assert!(!welcome.is_empty(), "a commit that adds a member carries its welcome");
    peer.send_binary(29, &[&7u16.to_be_bytes()[..], &commit].concat());
    assert_eq!(peer.expect(23), json!({ "transition_id": 7 }));
    them.process_welcome(&welcome).unwrap();
    peer.send(22, json!({ "transition_id": 7 }));
    peer.send(5, json!({ "user_id": THEM, "ssrc": THEIR_SSRC, "speaking": 1 }));

    // mediad's frames are now sealed for the group: only a member opens them.
    fake.hear(Duration::from_millis(300));
    let sealed: Vec<Vec<u8>> = fake.hear(Duration::from_millis(600)).into_iter().map(|rtp| rtp.payload).collect();
    assert!(sealed.len() >= 20, "{} packets", sealed.len());
    assert!(sealed.iter().all(|frame| frame.ends_with(&[0xfa, 0xfa])), "every frame carries the DAVE marker");
    let opened: Vec<Vec<u8>> = sealed.iter().map(|frame| them.decrypt(111, MediaType::AUDIO, frame).unwrap()).collect();
    assert!(is_the_tone(decoded_level(&opened)), "what the member decrypts is the tone");

    // And the member's sealed frames are heard by mediad.
    let sealed: Vec<Vec<u8>> = tone_frames(50)
        .iter()
        .map(|frame| {
            let sealed = them.encrypt_opus(frame).unwrap();
            assert_ne!(&sealed[..], &frame[..]);
            sealed.into_owned()
        })
        .collect();
    let tone: Vec<(u16, &[u8])> = sealed.iter().enumerate().map(|(n, frame)| (n as u16, &frame[..])).collect();
    fake.talk(&tone);
    mediad.speaking(THEM, true);

    // The budget for mediad in a voice call is about 40 MB. This is the whole engine at work
    // (gateway, MLS group, both codecs, the mix) with no sound device open.
    let rss_kb = process_rss_kb(mediad.child.id());
    eprintln!("mediad RSS in an end-to-end encrypted call: {rss_kb} KB");
    assert!(rss_kb < 40 * 1024, "{rss_kb} KB");

    assert_eq!(mediad.hang_up().0.code(), Some(0));
    let loudest = played(&played_to).chunks(960 * 2 * 20).map(rms).fold(0., f64::max);
    assert!(is_the_tone(loudest), "played level {loudest}");
    let _ = std::fs::remove_file(played_to);
}
