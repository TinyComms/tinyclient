//! A Discord voice join driven through chatd's call state, with the real mediad and a fake voice
//! server on localhost. The adapter is not involved: the test calls what `Context::voice`,
//! `Context::participants` and `Context::call_failed` call, with what the adapter would pass.
//!
//! mediad hears a generated tone and opens no sound device (`TINYCLIENT_MEDIAD_AUDIO`).

use chatd::hub::Hub;
use chatd::media::CallError;
use chatd::store::Store;
use protocol::{
    read_frame, write_frame, Call, CallFailure, CallState, Change, Dave, Key, MediaRequest, Network,
    Participant, Request, Response, VoiceJoin,
};
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream, UdpSocket};
use protocol::ChatStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const PATIENCE: Duration = Duration::from_secs(8);

/// Builds mediad (another package's binary) and points chatd's spawn at it, once.
fn environment() {
    static READY: Once = Once::new();
    READY.call_once(|| {
        let chatd = Path::new(env!("CARGO_BIN_EXE_chatd"));
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let built = Command::new(cargo).args(["build", "-p", "mediad", "--bin", "mediad"]).status().unwrap();
        assert!(built.success(), "could not build mediad");
        std::env::set_var("TINYCLIENT_MEDIAD", chatd.with_file_name(format!("mediad{}", std::env::consts::EXE_SUFFIX)));
        std::env::set_var("TINYCLIENT_MEDIAD_AUDIO", "tone");
        std::env::set_var("TINYCLIENT_VOICE_PLAIN", "1");
    });
}

/// The tests count mediad processes under this one, so they take turns.
fn alone() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn hub() -> (Arc<Hub>, PathBuf) {
    environment();
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // On Windows "/tmp" has no drive; chatd takes only absolute paths.
    let dir = std::path::absolute(format!("/tmp/tinyclient-calls-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("chat.sqlite")).unwrap();
    (Arc::new(Hub::new(store)), dir)
}

fn lounge() -> Key {
    Key {
        network: Network::Discord,
        account: "111".into(),
        id: "900".into(),
    }
}

/// mediad processes this test process has spawned, from the process table.
fn mediads() -> usize {
    child_pids(std::process::id(), "mediad").len()
}

/// The one mediad this test process runs.
fn mediad_pid() -> String {
    child_pids(std::process::id(), "mediad").first().map(|pid| pid.to_string()).unwrap_or_default()
}

fn child_pids(parent: u32, name: &str) -> Vec<u32> {
    #[cfg(windows)]
    {
        let script = format!(
            "(Get-CimInstance Win32_Process -Filter \"Name = '{name}.exe'\" | Where-Object {{ $_.ParentProcessId -eq {parent} }}).ProcessId"
        );
        let out = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
        return String::from_utf8(out.stdout).unwrap().lines().filter_map(|line| line.trim().parse().ok()).collect();
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("pgrep").args(["-P", &parent.to_string(), name]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().lines().filter_map(|pid| pid.trim().parse().ok()).collect()
    }
}

fn kill_pid(pid: &str) -> bool {
    #[cfg(windows)]
    {
        Command::new("taskkill").args(["/F", "/PID", pid]).status().unwrap().success()
    }
    #[cfg(not(windows))]
    {
        Command::new("kill").args(["-9", pid]).status().unwrap().success()
    }
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        sleep(Duration::from_millis(10));
    }
}

fn state(hub: &Hub) -> Option<CallState> {
    hub.calls.describe().0.map(|call| call.state)
}

/// A voice server that takes one client through the version 8 handshake, unencrypted end to end.
struct Voice {
    listener: TcpListener,
    udp: UdpSocket,
}

struct Peer {
    ws: WebSocket<TcpStream>,
    seq: u64,
}

impl Peer {
    fn send(&mut self, op: u8, d: Value) {
        self.seq += 1;
        let frame = json!({ "op": op, "d": d, "seq": self.seq });
        self.ws.send(Message::text(frame.to_string())).unwrap();
    }

    /// The next frame with this opcode. Heartbeats on the way are answered.
    fn expect(&mut self, op: u8) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            assert!(Instant::now() < deadline, "no opcode {op} from mediad");
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                    if frame["op"] == 3 {
                        let ack = json!({ "op": 6, "d": { "t": frame["d"]["t"] } });
                        self.ws.send(Message::text(ack.to_string())).unwrap();
                    } else if frame["op"] == op {
                        return frame["d"].clone();
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(err) => panic!("the voice server's socket: {err}"),
            }
        }
    }

    fn close(mut self, code: u16) {
        let frame = tungstenite::protocol::CloseFrame { code: code.into(), reason: "".into() };
        let _ = self.ws.close(Some(frame));
        let _ = self.ws.flush();
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until && self.ws.read().is_ok() {}
    }
}

impl Voice {
    fn new() -> Voice {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(PATIENCE)).unwrap();
        Voice { listener: TcpListener::bind("127.0.0.1:0").unwrap(), udp }
    }

    fn join(&self) -> VoiceJoin {
        VoiceJoin {
            network: Network::Discord,
            endpoint: self.listener.local_addr().unwrap().to_string(),
            token: "voice-token".into(),
            session_id: "session-1".into(),
            user_id: "111".into(),
            server_id: "900".into(),
            channel_id: "900".into(),
            dave: Dave { max_version: 1, required: false },
            // The adapter's view of our mute is older than chatd's; chatd's wins.
            muted: false,
            deafened: false,
        }
    }

    /// Accepts mediad and stops after reading its identify.
    fn identified(&self) -> (Peer, Value) {
        let (stream, _) = self.listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let ws = tungstenite::accept(stream).unwrap();
        ws.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut peer = Peer { ws, seq: 0 };
        let identify = peer.expect(0);
        (peer, identify)
    }

    /// The rest of the handshake: hello, ready, address discovery, select protocol, and the
    /// session description that makes the call joined.
    fn admit(&self, peer: &mut Peer) {
        peer.send(8, json!({ "v": 8, "heartbeat_interval": 30_000 }));
        let port = self.udp.local_addr().unwrap().port();
        let modes = ["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"];
        peer.send(2, json!({ "ssrc": 1111, "ip": "127.0.0.1", "port": port, "modes": modes }));
        let mut request = [0u8; 128];
        let (len, from) = self.udp.recv_from(&mut request).unwrap();
        assert_eq!((len, &request[..4]), (74, &[0u8, 1, 0, 70][..]), "address discovery");
        let mut answer = [0u8; 74];
        answer[..4].copy_from_slice(&[0, 2, 0, 70]);
        answer[8..17].copy_from_slice(b"127.0.0.1");
        answer[72..].copy_from_slice(&from.port().to_be_bytes());
        self.udp.send_to(&answer, from).unwrap();
        let select = peer.expect(1);
        assert_eq!(select["data"]["mode"], "aead_aes256_gcm_rtpsize");
        let key = vec![9u8; 32];
        peer.send(4, json!({ "mode": select["data"]["mode"], "secret_key": key, "dave_protocol_version": 0 }));
    }
}

#[test]
fn a_join_is_drawn_as_a_call_only_once_the_voice_server_admits_us_and_a_refusal_ends_it() {
    let _turn = alone();
    let (hub, dir) = hub();
    let voice = Voice::new();
    let seq = hub.seq();

    assert!(hub.calls.start(&hub, lounge()).is_ok());
    assert!(matches!(hub.calls.start(&hub, lounge()), Err(CallError::Active)), "one call at a time");
    assert!(hub.calls.describe().1, "mediad runs from connecting");
    assert_eq!(mediads(), 1);
    // Muted before the network has answered: it must survive the join.
    assert!(hub.calls.set_audio(&hub, true, false).is_ok());
    let alice = Participant {
        id: "222".into(),
        name: "Alice".into(),
        speaking: false,
        muted: false,
        deafened: false,
    };
    hub.calls.set_participants(&hub, &lounge(), vec![alice]);

    // The adapter has both gateway events and hands the session over.
    hub.calls.join(&hub, &lounge(), voice.join());
    let (mut peer, identify) = voice.identified();
    assert_eq!(identify["token"], "voice-token");
    assert_eq!(identify["max_dave_protocol_version"], 1);
    let call = hub.calls.describe().0.unwrap();
    assert_eq!(call.state, CallState::Connecting, "identified is not joined");
    assert_eq!(call.joined, 0);
    assert!(call.engine_linked, "a voice engine took the session");

    voice.admit(&mut peer);
    until("joined", || state(&hub) == Some(CallState::Joined));
    let call = hub.calls.describe().0.unwrap();
    assert!(call.joined > 0);
    assert!(call.muted, "mute set while connecting is kept");
    assert_eq!(call.participants[0].name, "Alice");
    until("devices", || hub.calls.describe().0.is_some_and(|call| call.input == "tone"));
    // Muted from the start: the gateway is never told we speak, though the tone is loud.
    assert_eq!(peer.expect(5)["speaking"], 0, "only the announcement of our SSRC");

    // The shell was told, with the whole call, without asking.
    let (_, _, changes) = hub.wait(seq, Duration::from_millis(10));
    assert!(changes.iter().any(|change| matches!(change,
        Change::Call { call: Some(call) } if call.state == CallState::Joined && call.input == "tone")));

    // Requests reach mediad and each gets its one reply.
    assert!(hub.calls.set_audio(&hub, false, false).is_ok());
    assert_eq!(peer.expect(5)["speaking"], 1, "unmuted, the tone is speech");
    assert!(hub.calls.request(&hub, &MediaRequest::Devices { input: None, output: None }).is_ok());

    // Discord ends it: DAVE required. The reason is shown and mediad is gone.
    peer.close(4017);
    until("failed", || matches!(state(&hub), Some(CallState::Failed { reason: CallFailure::Encryption, .. })));
    until("mediad to exit", || mediads() == 0 && !hub.calls.describe().1);
    assert!(matches!(hub.calls.set_audio(&hub, true, false), Err(CallError::NoCall)));
    // It stays until dismissed.
    sleep(Duration::from_millis(300));
    assert!(matches!(state(&hub), Some(CallState::Failed { .. })));
    assert_eq!(hub.calls.end(&hub), Some(lounge()));
    assert_eq!(hub.calls.describe(), (None, false));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failure_from_the_adapter_stops_mediad_stays_on_screen_and_gives_way_to_a_new_call() {
    let _turn = alone();
    let (hub, dir) = hub();
    assert!(hub.calls.start(&hub, lounge()).is_ok());
    assert_eq!(mediads(), 1);
    // What `Context::call_failed` does.
    let full = CallState::Failed { reason: CallFailure::Refused, detail: "Lounge is full.".into() };
    hub.calls.set_state(&hub, &lounge(), full.clone());
    assert!(!hub.calls.describe().1, "a failed call has no mediad");
    assert_eq!(mediads(), 0);
    assert_eq!(state(&hub), Some(full));

    // "Try again" replaces the failure with a new call.
    assert!(hub.calls.start(&hub, lounge()).is_ok());
    assert_eq!(state(&hub), Some(CallState::Connecting));
    assert_eq!(mediads(), 1);
    assert_eq!(hub.calls.end(&hub), Some(lounge()));
    assert_eq!(mediads(), 0, "hang-up waits for mediad to exit");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_mediad_that_dies_holding_a_voice_session_is_a_failure_in_words_not_a_vanished_call() {
    let _turn = alone();
    let (hub, dir) = hub();
    let voice = Voice::new();
    assert!(hub.calls.start(&hub, lounge()).is_ok());
    hub.calls.join(&hub, &lounge(), voice.join());
    let (_peer, _) = voice.identified();
    let pid = mediad_pid();
    assert!(kill_pid(&pid));
    until("the failure", || matches!(state(&hub), Some(CallState::Failed { .. })));
    let expected = CallState::Failed {
        reason: CallFailure::Engine,
        detail: "The call engine stopped.".into(),
    };
    assert_eq!(state(&hub), Some(expected));
    assert!(!hub.calls.describe().1);
    assert_eq!(hub.calls.end(&hub), Some(lounge()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_call_the_network_never_answers_fails_in_ten_seconds_and_leaves_no_mediad() {
    let _turn = alone();
    let (hub, dir) = hub();
    let began = Instant::now();
    assert!(hub.calls.start(&hub, lounge()).is_ok());
    let deadline = Instant::now() + Duration::from_secs(12);
    while !matches!(state(&hub), Some(CallState::Failed { .. })) {
        assert!(Instant::now() < deadline, "still {:?}", state(&hub));
        sleep(Duration::from_millis(50));
    }
    assert!(began.elapsed() >= Duration::from_secs(10), "{:?}", began.elapsed());
    let expected = CallState::Failed {
        reason: CallFailure::Lost,
        detail: "No reply from Discord after 10 seconds.".into(),
    };
    assert_eq!(state(&hub), Some(expected));
    assert!(!hub.calls.describe().1);
    assert_eq!(mediads(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_second_voice_session_for_a_call_that_is_up_moves_the_call_and_keeps_it() {
    let _turn = alone();
    let (hub, dir) = hub();
    let (old, new) = (Voice::new(), Voice::new());
    assert!(hub.calls.start(&hub, lounge()).is_ok());
    let alice = Participant {
        id: "222".into(),
        name: "Alice".into(),
        speaking: false,
        muted: false,
        deafened: false,
    };
    hub.calls.set_participants(&hub, &lounge(), vec![alice]);
    hub.calls.join(&hub, &lounge(), old.join());
    let (mut first, _) = old.identified();
    old.admit(&mut first);
    until("joined", || state(&hub) == Some(CallState::Joined));
    let before = hub.calls.describe().0.unwrap();
    let pid = mediad_pid();
    assert!(hub.calls.set_audio(&hub, true, false).is_ok());
    let seq = hub.seq();

    // The voice server moved (a region change): the adapter hands over a new session for the
    // same call. chatd sends it as `Rejoin`; mediad drops the old connection and takes it.
    let moved = VoiceJoin { session_id: "session-2".into(), token: "voice-token-2".into(), ..new.join() };
    hub.calls.join(&hub, &lounge(), moved);
    let (mut second, identify) = new.identified();
    assert_eq!((&identify["session_id"], &identify["token"]), (&json!("session-2"), &json!("voice-token-2")));
    assert_eq!(state(&hub), Some(CallState::Reconnecting), "between the two servers the call is reconnecting, not gone");
    new.admit(&mut second);
    until("joined again", || state(&hub) == Some(CallState::Joined));

    let after = hub.calls.describe().0.unwrap();
    assert_eq!(after.joined, before.joined, "the same call: its clock did not restart");
    assert_eq!(after.participants[0].name, "Alice");
    assert!(after.muted, "mute is kept across the move");
    assert_eq!((mediads(), mediad_pid()), (1, pid), "the same mediad carried it over");
    let (_, _, changes) = hub.wait(seq, Duration::from_millis(10));
    assert!(changes.iter().all(|change| !matches!(change, Change::Call { call: None })), "the call never left the screen");
    // The old voice server heard a goodbye, not a dropped socket.
    let goodbye = loop {
        match first.ws.read() {
            Ok(Message::Close(frame)) => break frame.map(|frame| u16::from(frame.code)),
            Ok(_) => {}
            Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break None,
        }
    };
    assert_eq!(goodbye, Some(1000));

    assert_eq!(hub.calls.end(&hub), Some(lounge()));
    assert_eq!(mediads(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// chatd itself, serving the demo, whose adapter hands mediad the voice server it is told of.
struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn start(voice: &Voice, answer_ms: &str) -> Daemon {
        environment();
        // On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!("/tmp/tinyclient-ring-{}", std::process::id())).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .arg("--seed-demo")
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_DEMO_VOICE", voice.listener.local_addr().unwrap().to_string())
            .env("TINYCLIENT_DEMO_ANSWER_MS", answer_ms)
            .spawn()
            .unwrap();
        let daemon = Daemon { child, dir };
        until("chatd's socket", || ChatStream::connect(&daemon.dir.join("chat.sock")).is_ok());
        daemon
    }

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    fn call(&self) -> Option<Call> {
        match self.ask(Request::Status) {
            Response::Status { call, .. } => call,
            other => panic!("not a status: {other:?}"),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_ring_we_placed_is_not_a_call_until_the_other_side_answers_whatever_our_media_does() {
    let _turn = alone();
    let voice = Voice::new();
    // The demo's network answers a direct call after this long.
    let chatd = Daemon::start(&voice, "2500");
    let ines = Key {
        network: Network::Discord,
        account: "210000000000000000".into(),
        id: "500000000000000001".into(),
    };
    let rang = Instant::now();
    assert!(matches!(chatd.ask(Request::CallStart { conversation: ines.clone() }), Response::Status { call: Some(_), .. }));

    // Our own side connects first: the voice server admits us and media flows.
    let (mut peer, identify) = voice.identified();
    assert_eq!(identify["token"], "demo-voice-token");
    voice.admit(&mut peer);
    peer.expect(5);
    until("our devices", || chatd.call().is_some_and(|call| call.input == "tone"));
    assert!(rang.elapsed() < Duration::from_millis(2_000), "the answer was not outrun: {:?}", rang.elapsed());
    let call = chatd.call().expect("the ring");
    assert_eq!((call.state, call.joined), (CallState::RingingOut, 0), "media being up is not an answer");
    assert!(call.participants.is_empty());

    // They pick up: now it is a call, with them in it.
    until("the answer", || chatd.call().is_some_and(|call| call.state == CallState::Joined));
    assert!(rang.elapsed() >= Duration::from_millis(2_400), "joined before anyone answered: {:?}", rang.elapsed());
    let call = chatd.call().unwrap();
    assert!(call.joined > 0);
    until("the roster", || chatd.call().is_some_and(|call| call.participants.iter().any(|who| who.name == "Ines Marchetti")));
    assert_eq!(call.conversation, ines);

    assert!(matches!(chatd.ask(Request::CallEnd), Response::Status { call: None, .. }));
}

#[test]
fn the_microphone_test_outside_a_call_meters_ten_times_a_second_and_its_mediad_leaves_when_it_stops() {
    let _turn = alone();
    let (hub, dir) = hub();
    let seq = hub.seq();
    assert!(hub.calls.test_microphone(&hub, true).is_ok());
    assert_eq!(mediads(), 1, "one mediad, for the test");
    let mut levels = Vec::new();
    let mut since = seq;
    let began = Instant::now();
    while levels.len() < 5 {
        assert!(began.elapsed() < PATIENCE, "no levels");
        let (seq, _, changes) = hub.wait(since, Duration::from_millis(200));
        since = seq;
        levels.extend(changes.iter().filter_map(|change| match change {
            Change::Meter { level: Some(level) } => Some(*level),
            _ => None,
        }));
    }
    assert!(levels.iter().all(|level| (60..90).contains(level)), "the tone's level: {levels:?}");
    // Ten a second: five took about half a second, not less.
    assert!(began.elapsed() >= Duration::from_millis(350), "{:?} for five", began.elapsed());

    let stopped = Instant::now();
    assert!(hub.calls.test_microphone(&hub, false).is_ok());
    until("the test mediad to exit", || mediads() == 0);
    assert!(stopped.elapsed() < Duration::from_secs(3), "gone {:?} after the test ended", stopped.elapsed());
    let (_, _, changes) = hub.wait(since, Duration::from_millis(10));
    assert!(changes.iter().any(|change| matches!(change, Change::Meter { level: None })), "the meter is told it is over");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_test_sound_outside_a_call_plays_on_a_mediad_of_its_own_that_then_exits() {
    let _turn = alone();
    let (hub, dir) = hub();
    let played = dir.join("played.pcm");
    std::env::set_var("TINYCLIENT_MEDIAD_SINK", &played);
    hub.set_setting("", protocol::settings::CUE_VOLUME, Some("100".into())).unwrap();
    assert_eq!(hub.settings().number(protocol::settings::CUE_VOLUME), 100);

    let pressed = Instant::now();
    assert!(hub.calls.test_sound(&hub).is_ok());
    assert_eq!(mediads(), 1);
    until("the test mediad to exit", || mediads() == 0);
    std::env::remove_var("TINYCLIENT_MEDIAD_SINK");
    assert!(pressed.elapsed() < Duration::from_secs(3), "{:?}", pressed.elapsed());
    let bytes = std::fs::read(&played).unwrap();
    let peak = bytes.as_chunks::<2>().0.iter().map(|&pair| i16::from_le_bytes(pair).saturating_abs()).max().unwrap_or(0);
    let db = 20. * (f64::from(peak) / 32767.).log10();
    assert!((db + 20.).abs() < 1., "the join cue at 100 % peaks at -20 dBFS, not {db}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_call_starts_muted_when_the_user_joins_muted_and_a_test_gives_way_to_it() {
    let _turn = alone();
    let (hub, dir) = hub();
    hub.set_setting("", protocol::settings::JOIN_MUTED, Some("on".into())).unwrap();
    assert!(hub.calls.test_microphone(&hub, true).is_ok());
    assert_eq!(mediads(), 1);
    assert!(hub.calls.start(&hub, lounge()).is_ok());
    // The test's mediad went; the call's is the one left.
    until("one mediad", || mediads() == 1);
    assert!(hub.calls.describe().0.unwrap().muted, "joined muted");
    // In a call the microphone test is the call's own meter.
    assert!(hub.calls.test_microphone(&hub, true).is_ok());
    assert_eq!(mediads(), 1);
    assert!(hub.calls.end(&hub).is_some());
    until("no mediad", || mediads() == 0);
    let _ = std::fs::remove_dir_all(dir);
}
