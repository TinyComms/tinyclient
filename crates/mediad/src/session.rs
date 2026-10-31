//! One call: the conversation with the voice gateway, from identify to hang-up, on the voice
//! server of the `Join` and then on each one a `Rejoin` moves it to.
//!
//! Tries are bounded and never loop: at most [`ATTEMPTS`] identifies for one voice server, each
//! given [`ATTEMPT`] to get from the first byte to a session description, and at most
//! [`RESUMES`] resumes in a row when a joined call drops. After that the call has failed and
//! chatd is told why, in words the dock can show as they are. What the system or the server
//! actually said goes to stderr.

use crate::audio::Audio;
use crate::dave::{self, Dave, Outgoing};
use crate::gateway::{self, closed, Closed, Incoming, Socket};
use crate::media::{self, FromMedia, MICROPHONE_DENIED};
use crate::permission::{microphone, Microphone};
use crate::rtp::{self, Cipher, Sender};
use crate::out;
use protocol::{AudioProcessing, CallFailure, CallStats, Cue, InputMode, MediaResponse, TinyPicture, TinySlot, Tuning, VoiceJoin};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender as Reports};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Identifies for one voice server, the first included.
pub const ATTEMPTS: u32 = 3;
/// Resumes in a row before a dropped call is given up.
pub const RESUMES: u32 = 3;
/// A resumed connection that held this long was a recovery; the next drop counts from one.
const HELD: Duration = Duration::from_secs(60);
/// How long one try has, from resolving the server's name to its session description. Discord
/// answers in a few hundred milliseconds; three tries and their pauses fit in ten seconds.
pub const ATTEMPT: Duration = Duration::from_secs(3);
/// The pause before the second try; the third waits twice as long.
const BACKOFF: Duration = Duration::from_millis(250);
/// How long each address discovery waits for its answer over UDP, three times.
const DISCOVERY: Duration = Duration::from_millis(300);
/// How long a call the voice server dropped us from waits to be told where it moved to. Discord
/// closes the old connection (4014) when a call changes region, and the new voice session
/// arrives on the gateway a moment before or after.
const MOVE: Duration = Duration::from_secs(5);

const NOT_ENCRYPTED: &str = "Discord would carry this call without end-to-end encryption, and that is not allowed here.";
const UNREACHABLE: &str = "Couldn't reach Discord's voice server. Check your internet connection, then try again.";
const SILENT: &str = "Discord's voice server didn't answer. Try again in a moment.";
const NO_AUDIO_PATH: &str =
    "Discord's voice server answered, but call audio couldn't get through. A VPN or firewall may be blocking it.";
const DROPPED: &str = "The connection to Discord dropped and didn't come back. You've left the call.";
const UNUSABLE: &str = "Discord sent a voice session this tinyclient can't use. Try again, or take this call in Discord.";
const NO_CIPHER: &str = "Discord and this tinyclient share no way to encrypt call audio. Update tinyclient, or take this call in Discord.";
const ENGINE: &str = "The call engine could not start. Try again.";

pub(crate) type Devices = (Option<String>, Option<String>);

/// What chatd's requests set, read by the threads of the call.
#[derive(Default)]
pub struct Controls {
    pub muted: AtomicBool,
    pub deafened: AtomicBool,
    /// chatd closed our stdin.
    pub hangup: AtomicBool,
    /// A device choice the media loop has not applied yet.
    pub devices: Mutex<Option<Devices>>,
    /// The choice in force, for a join that comes after it.
    chosen: Mutex<Devices>,
    /// The device menu is open: levels are wanted.
    meter: AtomicBool,
    /// The voice session the call moved to, not yet taken up.
    moved: Mutex<Option<VoiceJoin>>,
    /// Set to have the devices listed again now, and what wakes the thread that lists them.
    relist: Mutex<bool>,
    wake: Condvar,
    /// Gains, the speech gate and which cues play.
    tuning: Mutex<Tuning>,
    /// The cue to play next, taken by whatever plays (the media loop, or a test).
    cue: Mutex<Option<Cue>>,
    /// A Tiny server call's next connection, answered and not yet taken up: the server
    /// restarted and the call joined again.
    pub(crate) tiny_link: Mutex<Option<crate::tiny::Link>>,
    /// A Tiny server's newest slot map, not yet applied.
    pub(crate) tiny_slots: Mutex<Option<Vec<TinySlot>>>,
    /// The camera and the share the user asked for, and whether the one picture is watched.
    share: AtomicBool,
    camera: AtomicBool,
    watch: AtomicBool,
    /// An answer to the renegotiation offer, or an empty SDP when the server refused it.
    picture_answer: Mutex<Option<String>>,
    /// Who the one video slot carries. `None` inside means the server sent no picture.
    picture_slot: Mutex<Option<Option<TinyPicture>>>,
    /// A Tiny server call's end-to-end encryption (`e2ee`), shared by chatd's requests and the
    /// call's loop.
    pub(crate) tiny_e2ee: Mutex<crate::e2ee::E2ee>,
    /// How the microphone opens. `None` until chatd says: voice activation at the tuning's gate.
    input: Mutex<Option<InputMode>>,
    /// The push-to-talk keys.
    keys: Mutex<crate::talk::Keys>,
    /// Each person's volume for me, and who holds priority speech.
    ear: Mutex<crate::hear::Ear>,
    /// Echo cancellation, noise suppression, automatic gain. `None` until chatd says: off.
    processing: Mutex<Option<AudioProcessing>>,
    /// What our voice is encoded at, in kb/s. 0: the call's own default.
    bitrate: AtomicU32,
    /// The connection over the last second, from the loop that carries the call.
    stats: Mutex<Option<CallStats>>,
}

impl Controls {
    pub fn choose(&self, input: Option<String>, output: Option<String>) {
        *self.chosen.lock().unwrap() = (input.clone(), output.clone());
        *self.devices.lock().unwrap() = Some((input, output));
    }

    /// The device menu opened or closed. Opening it has the devices listed at once.
    pub fn meter(&self, on: bool) {
        self.meter.store(on, Ordering::Relaxed);
        if on {
            *self.relist.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }

    /// True while `Levels` are wanted.
    pub fn metering(&self) -> bool {
        self.meter.load(Ordering::Relaxed)
    }

    /// Waits until the devices should be looked at again: `every`, or sooner when the menu
    /// opens. True when the list is wanted whether or not it changed.
    pub fn listing_due(&self, every: Duration) -> bool {
        let relist = self.relist.lock().unwrap();
        let (mut relist, _) = self.wake.wait_timeout_while(relist, every, |asked| !*asked).unwrap();
        std::mem::take(&mut *relist)
    }

    /// A Tiny server call joined again: its loop takes up this connection next.
    pub fn tiny_rejoin(&self, link: crate::tiny::Link) {
        *self.tiny_link.lock().unwrap() = Some(link);
    }

    /// A Tiny server's new slot map, for its loop to apply.
    pub fn tiny_slots(&self, map: Vec<TinySlot>) {
        *self.tiny_slots.lock().unwrap() = Some(map);
    }

    pub fn set_share(&self, on: bool) {
        self.share.store(on, Ordering::Relaxed);
    }

    pub fn share(&self) -> bool {
        self.share.load(Ordering::Relaxed)
    }

    pub fn set_camera(&self, on: bool) {
        self.camera.store(on, Ordering::Relaxed);
    }

    pub fn camera(&self) -> bool {
        self.camera.load(Ordering::Relaxed)
    }

    pub fn set_watch(&self, on: bool) {
        self.watch.store(on, Ordering::Relaxed);
    }

    pub fn watch(&self) -> bool {
        self.watch.load(Ordering::Relaxed)
    }

    /// The answer to the last renegotiation, taken once by the call's loop.
    pub fn picture_answer(&self, sdp: String) {
        *self.picture_answer.lock().unwrap() = Some(sdp);
    }

    pub(crate) fn take_picture_answer(&self) -> Option<String> {
        self.picture_answer.lock().unwrap().take()
    }

    /// Who the video slot carries now. The outer `Some` means the server said; the inner is
    /// `None` when it sent `video: null`.
    pub fn set_picture(&self, picture: Option<TinyPicture>) {
        *self.picture_slot.lock().unwrap() = Some(picture);
    }

    pub(crate) fn take_picture(&self) -> Option<Option<TinyPicture>> {
        self.picture_slot.lock().unwrap().take()
    }

    /// The devices chosen so far, for a call that opens them now. Any choice waiting for the
    /// media loop is part of it.
    pub(crate) fn take_chosen(&self) -> Devices {
        self.devices.lock().unwrap().take();
        self.chosen.lock().unwrap().clone()
    }

    pub fn tune(&self, tuning: Tuning) {
        *self.tuning.lock().unwrap() = tuning;
    }

    pub fn tuning(&self) -> Tuning {
        *self.tuning.lock().unwrap()
    }

    /// Asks for a cue, if the user wants that kind heard. Someone joining or leaving is not
    /// heard while deafened; the test sound always is. A new cue replaces one not yet begun.
    pub fn cue(&self, cue: Cue) {
        let tuning = self.tuning();
        let wanted = match cue {
            Cue::Test => true,
            Cue::Joined | Cue::Left => tuning.joins && !self.deafened.load(Ordering::Relaxed),
            // Our own room changed: as a join, but heard while deafened, since it is ours.
            Cue::Moved => tuning.joins,
            Cue::Muted | Cue::Unmuted | Cue::Deafened | Cue::Undeafened => tuning.mine,
            Cue::Lost | Cue::Back | Cue::Disconnected => tuning.connection,
        };
        if wanted {
            *self.cue.lock().unwrap() = Some(cue);
        }
    }

    pub fn take_cue(&self) -> Option<Cue> {
        self.cue.lock().unwrap().take()
    }

    /// Mute and deafen as asked, with the cue the change calls for (S8).
    pub fn set_audio(&self, muted: bool, deafened: bool) {
        let was = (self.muted.swap(muted, Ordering::Relaxed), self.deafened.swap(deafened, Ordering::Relaxed));
        if let Some(cue) = mine(was, (muted, deafened)) {
            self.cue(cue);
        }
    }

    pub fn set_input(&self, mode: InputMode) {
        *self.input.lock().unwrap() = Some(mode);
    }

    /// How the microphone opens now.
    pub fn input(&self) -> InputMode {
        self.input.lock().unwrap().unwrap_or(InputMode::Voice { threshold: self.tuning().gate })
    }

    /// A push-to-talk key went down or up.
    pub fn talk(&self, held: bool, priority: bool) {
        self.keys.lock().unwrap().press(held, priority, std::time::Instant::now());
    }

    pub fn keys(&self) -> crate::talk::Keys {
        *self.keys.lock().unwrap()
    }

    /// One person's volume for me. 100 % and heard is forgotten, as never named.
    pub fn gain(&self, user_id: String, percent: u16, muted: bool) {
        let mut ear = self.ear.lock().unwrap();
        if percent == 100 && !muted {
            ear.gains.remove(&user_id);
        } else {
            ear.gains.insert(user_id, (percent.min(protocol::GAIN_MAX), muted));
        }
    }

    pub fn priority(&self, user_ids: Vec<String>) {
        self.ear.lock().unwrap().priority = user_ids;
    }

    /// Everyone's volume and who holds priority, for one tick's mix.
    pub fn ear(&self) -> crate::hear::Ear {
        self.ear.lock().unwrap().clone()
    }

    pub fn set_processing(&self, processing: AudioProcessing) {
        *self.processing.lock().unwrap() = Some(processing);
    }

    pub fn processing(&self) -> Option<AudioProcessing> {
        *self.processing.lock().unwrap()
    }

    pub fn set_bitrate(&self, kbps: u32) {
        self.bitrate.store(kbps, Ordering::Relaxed);
    }

    /// The bitrate asked for, in bits a second; `None` for the call's default.
    pub fn bitrate(&self) -> Option<i32> {
        let kbps = self.bitrate.load(Ordering::Relaxed);
        (kbps > 0).then(|| (kbps.clamp(6, 510) * 1000) as i32)
    }

    pub fn publish_stats(&self, stats: CallStats) {
        *self.stats.lock().unwrap() = Some(stats);
    }

    /// The connection as last measured; `None` before media has flowed for a second.
    pub fn stats(&self) -> Option<CallStats> {
        self.stats.lock().unwrap().clone()
    }

    /// Whether a cue is waiting to be played.
    pub fn cue_waiting(&self) -> bool {
        self.cue.lock().unwrap().is_some()
    }

    /// The call moved to another voice server or session: the voice thread takes it from here.
    pub fn rejoin(&self, join: VoiceJoin) {
        *self.moved.lock().unwrap() = Some(join);
    }

    fn moving(&self) -> bool {
        self.moved.lock().unwrap().is_some()
    }
}

/// The cue for going from `was` to `now`, each (muted, deafened). Deafening mutes too, and is
/// the one heard.
pub fn mine(was: (bool, bool), now: (bool, bool)) -> Option<Cue> {
    match (was.1, now.1) {
        (false, true) => return Some(Cue::Deafened),
        (true, false) => return Some(Cue::Undeafened),
        _ => {}
    }
    match (was.0, now.0) {
        (false, true) => Some(Cue::Muted),
        (true, false) => Some(Cue::Unmuted),
        _ => None,
    }
}

/// Why a try got nowhere, for the words the dock shows when every try did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    /// No connection to the voice server at all: the name, TCP or TLS.
    Unreachable,
    /// Connected, and it stopped answering or went away.
    Silent,
    /// The websocket works and UDP does not.
    Udp,
}

enum End {
    /// chatd hung up.
    Hangup,
    /// The other side ended the call.
    Ended,
    Fatal(CallFailure, String),
    /// The socket is gone and the session may not be. The words are for the log.
    Dropped(Why, String),
    /// chatd handed over the voice session the call moved to.
    Moved,
    /// The voice server put us out (4014): moved to another server, or removed for good.
    Removed,
}

struct Media {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Audio>,
}

/// What `Ready` gave us, held until the session description completes it.
struct Transport {
    socket: UdpSocket,
    ssrc: u32,
}

struct Call {
    join: VoiceJoin,
    controls: Arc<Controls>,
    seq_ack: Option<u64>,
    transport: Option<Transport>,
    ssrc: u32,
    dave: Arc<Mutex<Dave>>,
    dave_version: u16,
    users: Arc<Mutex<HashMap<u32, String>>>,
    /// Everyone the gateway says is connected, us included: who DAVE may add to the group.
    connected: BTreeSet<u64>,
    media: Option<Media>,
    /// The devices, kept open between one voice server and the next.
    audio: Option<Audio>,
    reports: Reports<FromMedia>,
    from_media: Receiver<FromMedia>,
    microphone_unasked: bool,
    /// The network confirmed the join at least once.
    joined: bool,
    heartbeat: Option<Duration>,
    next_beat: Instant,
    unacked: u32,
    deadline: Option<Instant>,
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| value.as_u64().map(|n| n.to_string()))
}

fn dropped(err: std::io::Error) -> End {
    End::Dropped(Why::Silent, err.to_string())
}

enum Waited {
    Done,
    Hangup,
    Moved,
}

impl Call {
    /// Stops the audio loop and keeps the devices it held, open, for the next connection.
    fn stop_media(&mut self) {
        if let Some(media) = self.media.take() {
            media.stop.store(true, Ordering::Relaxed);
            if let Ok(audio) = media.thread.join() {
                self.audio = Some(audio);
            }
        }
    }

    /// Takes up the voice session the call moved to. The old connection's socket, UDP path,
    /// keys and SSRCs are gone; the devices stay open, mute and deafen stay as they are, and the
    /// end-to-end identity stays, with its group formed again on the new connection (a voice
    /// connection starts DAVE from a fresh key package, whatever came before).
    fn move_to(&mut self, next: VoiceJoin) {
        self.stop_media();
        eprintln!("mediad: the call moved to {}", next.endpoint);
        self.join = next;
        self.seq_ack = None;
        self.transport = None;
        self.joined = false;
        self.users.lock().unwrap().clear();
        let me = self.join.user_id.parse().ok();
        self.connected = me.into_iter().collect();
        // Reports from the loop that just stopped are about the old connection.
        while self.from_media.try_recv().is_ok() {}
    }

    fn deliver(&mut self, socket: &mut Socket, outgoing: Vec<Outgoing>) -> Result<(), End> {
        for frame in outgoing {
            match frame {
                Outgoing::Json { op, d } => socket.send(op, d),
                Outgoing::Binary { op, payload } => socket.send_binary(op, &payload),
            }
            .map_err(dropped)?;
        }
        Ok(())
    }

    /// The UDP path: a socket to the voice server, and the address it sees us at.
    fn ready(&mut self, socket: &mut Socket, d: &Value) -> Result<(), End> {
        let malformed = || End::Dropped(Why::Silent, "the voice server's ready frame was not understood".to_owned());
        let ssrc = d["ssrc"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(malformed)?;
        let ip = d["ip"].as_str().ok_or_else(malformed)?;
        let port = d["port"].as_u64().and_then(|n| u16::try_from(n).ok()).ok_or_else(malformed)?;
        let offered: Vec<String> = d["modes"].as_array().into_iter().flatten().filter_map(text).collect();
        let Some(mode) = rtp::choose_mode(&offered) else {
            eprintln!("mediad: no transport mode in common; offered {offered:?}");
            return Err(End::Fatal(CallFailure::Encryption, NO_CIPHER.to_owned()));
        };

        let udp = UdpSocket::bind("0.0.0.0:0").and_then(|udp| udp.connect((ip, port)).map(|()| udp)).map_err(dropped)?;
        udp.set_read_timeout(Some(DISCOVERY)).map_err(dropped)?;
        let mut answer = [0u8; 128];
        let mut seen = None;
        for _ in 0..3 {
            udp.send(&rtp::discovery_request(ssrc)).map_err(dropped)?;
            if let Ok(len) = udp.recv(&mut answer) {
                seen = rtp::discovery_response(&answer[..len]);
                if seen.is_some() {
                    break;
                }
            }
            if self.controls.hangup.load(Ordering::Relaxed) {
                return Err(End::Hangup);
            }
            if self.controls.moving() {
                return Err(End::Moved);
            }
        }
        let Some((address, port)) = seen else {
            return Err(End::Dropped(Why::Udp, format!("no answer over UDP from {ip}:{port}")));
        };
        let data = json!({ "address": address, "port": port, "mode": mode });
        let codecs = json!([{ "name": "opus", "type": "audio", "priority": 1000, "payload_type": rtp::PAYLOAD_TYPE }]);
        socket
            .send(gateway::SELECT_PROTOCOL, json!({ "protocol": "udp", "data": data, "codecs": codecs }))
            .map_err(dropped)?;
        self.transport = Some(Transport { socket: udp, ssrc });
        Ok(())
    }

    /// The secret key arrived: open the devices and start moving audio.
    fn described(&mut self, socket: &mut Socket, d: &Value) -> Result<(), End> {
        let malformed = || End::Dropped(Why::Silent, "the voice server's session description was not understood".to_owned());
        let Some(transport) = self.transport.take() else {
            // Sent again on a session that is already running: nothing changes.
            return Ok(());
        };
        let mode = d["mode"].as_str().ok_or_else(malformed)?;
        let key: Vec<u8> = d["secret_key"]
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .filter_map(|byte| byte.as_u64().and_then(|b| u8::try_from(b).ok()))
            .collect();
        let (Some(seal), Some(open)) = (Cipher::new(mode, &key), Cipher::new(mode, &key)) else {
            eprintln!("mediad: the voice server chose {mode:?} with a {} byte key", key.len());
            return Err(End::Fatal(CallFailure::Encryption, NO_CIPHER.to_owned()));
        };
        let version = number(&d["dave_protocol_version"]).unwrap_or(0) as u16;
        if version == 0 && self.join.dave.required {
            return Err(End::Fatal(CallFailure::Encryption, NOT_ENCRYPTED.to_owned()));
        }

        // The devices open only now, so a join that fails never lights the microphone indicator.
        // After a move they are open already; a choice made meanwhile is applied by the loop.
        let audio = match self.audio.take() {
            Some(audio) => audio,
            None => {
                let (input, output) = self.controls.take_chosen();
                Audio::open(input, output).map_err(|err| {
                    let (reason, detail) = media::failure(err);
                    End::Fatal(reason, detail)
                })?
            }
        };

        let first = self.dave.lock().unwrap().start(version);
        self.deliver(socket, first)?;
        self.dave_version = version;
        self.ssrc = transport.ssrc;
        let stop = Arc::new(AtomicBool::new(false));
        let setup = media::Setup {
            socket: transport.socket,
            sender: Sender::new(seal, transport.ssrc, rand::random(), rand::random()),
            opener: open,
            ssrc: transport.ssrc,
            user_id: self.join.user_id.clone(),
            dave: self.dave.clone(),
            users: self.users.clone(),
            controls: self.controls.clone(),
            report: self.reports.clone(),
            audio,
            stop: stop.clone(),
            microphone_unasked: self.microphone_unasked,
        };
        let thread = std::thread::Builder::new()
            .name("media".to_owned())
            .spawn(move || media::run(setup))
            .map_err(|err| {
                eprintln!("mediad: the media thread did not start: {err}");
                End::Fatal(CallFailure::Engine, ENGINE.to_owned())
            })?;
        self.media = Some(Media { stop, thread });

        // Announces our SSRC. Speaking itself is told when it starts.
        let quiet = json!({ "speaking": 0, "delay": 0, "ssrc": self.ssrc });
        socket.send(gateway::SPEAKING, quiet).map_err(dropped)?;
        self.joined = true;
        self.deadline = None;
        out::send(&MediaResponse::Connected { ssrc: self.ssrc, dave_version: version });
        Ok(())
    }

    fn source(&mut self, user: &Value, ssrc: &Value) {
        let (Some(user_id), Some(ssrc)) = (text(user), ssrc.as_u64().and_then(|n| u32::try_from(n).ok())) else {
            return;
        };
        if ssrc == 0 || user_id == self.join.user_id {
            return;
        }
        let known = self.users.lock().unwrap().insert(ssrc, user_id.clone());
        if known.as_ref() != Some(&user_id) {
            out::send(&MediaResponse::Source { user_id, ssrc });
        }
    }

    /// DAVE may not drop to "none" on a call that requires it.
    fn encrypted(&self) -> Result<(), End> {
        let version = self.dave.lock().unwrap().version();
        if self.joined && version == 0 && self.join.dave.required {
            return Err(End::Fatal(CallFailure::Encryption, NOT_ENCRYPTED.to_owned()));
        }
        Ok(())
    }

    fn json(&mut self, socket: &mut Socket, op: u8, d: Value) -> Result<(), End> {
        match op {
            gateway::HELLO => {
                let interval = Duration::from_millis(d["heartbeat_interval"].as_f64().unwrap_or(13_750.) as u64);
                self.heartbeat = Some(interval);
                self.next_beat = Instant::now() + interval;
            }
            gateway::READY => self.ready(socket, &d)?,
            gateway::SESSION_DESCRIPTION => self.described(socket, &d)?,
            gateway::HEARTBEAT_ACK => self.unacked = 0,
            gateway::RESUMED => {
                self.deadline = None;
                self.controls.cue(Cue::Back);
                out::send(&MediaResponse::Connected { ssrc: self.ssrc, dave_version: self.dave_version });
            }
            gateway::SPEAKING => self.source(&d["user_id"], &d["ssrc"]),
            gateway::VIDEO => self.source(&d["user_id"], &d["audio_ssrc"]),
            gateway::CLIENTS_CONNECT => {
                let ids = d["user_ids"].as_array().into_iter().flatten().filter_map(number);
                self.connected.extend(ids);
            }
            gateway::CLIENT_DISCONNECT => {
                if let Some(user_id) = text(&d["user_id"]) {
                    if let Ok(id) = user_id.parse() {
                        self.connected.remove(&id);
                    }
                    self.users.lock().unwrap().retain(|_, user| *user != user_id);
                    out::send(&MediaResponse::Gone { user_id });
                }
            }
            dave::PREPARE_TRANSITION => {
                let (transition, version) = (number(&d["transition_id"]), number(&d["protocol_version"]));
                if let (Some(transition), Some(version)) = (transition, version) {
                    let answer = self.dave.lock().unwrap().prepare_transition(transition as u16, version as u16);
                    self.deliver(socket, answer)?;
                    self.encrypted()?;
                }
            }
            dave::EXECUTE_TRANSITION => {
                if let Some(transition) = number(&d["transition_id"]) {
                    self.dave.lock().unwrap().execute_transition(transition as u16);
                    self.encrypted()?;
                }
            }
            dave::PREPARE_EPOCH => {
                if let (Some(epoch), Some(version)) = (number(&d["epoch"]), number(&d["protocol_version"])) {
                    let answer = self.dave.lock().unwrap().prepare_epoch(epoch, version as u16);
                    self.deliver(socket, answer)?;
                }
            }
            // Media sink wants, flags, platform and the rest carry nothing an audio call needs.
            _ => {}
        }
        Ok(())
    }

    fn binary(&mut self, socket: &mut Socket, op: u8, payload: &[u8]) -> Result<(), End> {
        let answer = {
            let mut dave = self.dave.lock().unwrap();
            match op {
                dave::EXTERNAL_SENDER => {
                    dave.external_sender(payload);
                    Vec::new()
                }
                dave::PROPOSALS => {
                    let connected: Vec<u64> = self.connected.iter().copied().collect();
                    dave.proposals(payload, &connected)
                }
                dave::ANNOUNCE_COMMIT => dave.commit(payload),
                dave::WELCOME => dave.welcome(payload),
                _ => Vec::new(),
            }
        };
        self.deliver(socket, answer)
    }

    fn converse(&mut self, socket: &mut Socket) -> End {
        loop {
            if self.controls.hangup.load(Ordering::Relaxed) {
                return End::Hangup;
            }
            if self.controls.moving() {
                return End::Moved;
            }
            while let Ok(report) = self.from_media.try_recv() {
                let sent = match report {
                    FromMedia::Speaking(on) => {
                        let d = json!({ "speaking": u8::from(on), "delay": 0, "ssrc": self.ssrc });
                        socket.send(gateway::SPEAKING, d).map_err(dropped)
                    }
                    FromMedia::Dave(outgoing) => self.deliver(socket, outgoing),
                    FromMedia::Fatal(reason, detail) => Err(End::Fatal(reason, detail)),
                };
                if let Err(end) = sent {
                    return end;
                }
            }
            let now = Instant::now();
            if let Some(interval) = self.heartbeat {
                if now >= self.next_beat {
                    if self.unacked >= 2 {
                        return End::Dropped(Why::Silent, "the voice server stopped answering heartbeats".to_owned());
                    }
                    let nonce = std::time::UNIX_EPOCH.elapsed().map_or(0, |since| since.as_millis() as u64);
                    if let Err(err) = socket.heartbeat(nonce) {
                        return dropped(err);
                    }
                    self.unacked += 1;
                    self.next_beat = now + interval;
                }
            }
            if self.deadline.is_some_and(|deadline| now > deadline) {
                return End::Dropped(Why::Silent, "the voice server did not answer in time".to_owned());
            }
            let handled = match socket.read() {
                Ok(Incoming::Quiet) => Ok(()),
                Ok(Incoming::Json { op, d }) => self.json(socket, op, d),
                Ok(Incoming::Binary { op, payload }) => self.binary(socket, op, &payload),
                Ok(Incoming::Closed(code)) => {
                    eprintln!("mediad: the voice server closed the socket ({code:?})");
                    return match closed(code) {
                        Closed::Resume => End::Dropped(Why::Silent, format!("the voice server closed the connection ({code:?})")),
                        Closed::Ended => End::Ended,
                        Closed::Removed => End::Removed,
                        Closed::Fatal(reason, detail) => End::Fatal(reason, detail.to_owned()),
                    };
                }
                Err(err) => Err(dropped(err)),
            };
            if let Err(end) = handled {
                return end;
            }
        }
    }

    /// One socket's life: identify or resume, then serve it until it ends.
    fn connection(&mut self, resume: bool) -> End {
        let deadline = Instant::now() + ATTEMPT;
        let mut socket = match Socket::connect(&self.join.endpoint, self.seq_ack, ATTEMPT) {
            Ok(socket) => socket,
            Err(err) => return End::Dropped(Why::Unreachable, format!("the voice server could not be reached: {err}")),
        };
        let join = &self.join;
        let hello = if resume {
            let mut d = json!({ "server_id": join.server_id, "session_id": join.session_id, "token": join.token });
            if let Some(seq) = self.seq_ack {
                d["seq_ack"] = json!(seq);
            }
            socket.send(gateway::RESUME, d)
        } else {
            let d = json!({
                "server_id": join.server_id,
                "user_id": join.user_id,
                "session_id": join.session_id,
                "token": join.token,
                "max_dave_protocol_version": join.dave.max_version.min(dave::MAX_VERSION),
            });
            socket.send(gateway::IDENTIFY, d)
        };
        if let Err(err) = hello {
            return dropped(err);
        }
        self.heartbeat = None;
        self.unacked = 0;
        self.deadline = Some(deadline);
        let end = self.converse(&mut socket);
        self.seq_ack = socket.seq_ack;
        match end {
            // The session may live: the audio goes on over UDP while the socket is resumed.
            End::Dropped(..) => {}
            // The server already closed; there is nobody to say goodbye to.
            End::Removed => self.stop_media(),
            // Over on this server: the last frames of speech go out, then the goodbye.
            _ => {
                self.stop_media();
                socket.close();
            }
        }
        end
    }

    /// Waits between tries, or for word of where the call moved.
    fn wait(&self, how_long: Duration, for_move: bool) -> Waited {
        let until = Instant::now() + how_long;
        loop {
            if self.controls.hangup.load(Ordering::Relaxed) {
                return Waited::Hangup;
            }
            if self.controls.moving() {
                return Waited::Moved;
            }
            if Instant::now() >= until {
                return Waited::Done;
            }
            std::thread::sleep(Duration::from_millis(if for_move { 5 } else { 20 }));
        }
    }
}

/// Runs the call to its end. Returns when chatd hung up or the other side ended it; a failure
/// does not return (`out::fail` exits).
pub fn run(join: VoiceJoin, controls: Arc<Controls>) {
    let (Ok(user), Ok(room)) = (join.user_id.parse::<u64>(), join.channel_id.parse::<u64>()) else {
        eprintln!("mediad: the voice session's ids are not numbers ({:?}, {:?})", join.user_id, join.channel_id);
        out::fail(CallFailure::Refused, UNUSABLE);
    };
    if join.dave.required && join.dave.max_version.min(dave::MAX_VERSION) == 0 {
        out::fail(CallFailure::Encryption, NOT_ENCRYPTED);
    }
    let mut microphone_unasked = false;
    if !Audio::simulated() {
        match microphone() {
            Some(Microphone::Denied) => out::fail(CallFailure::Audio, MICROPHONE_DENIED),
            Some(Microphone::Unasked) => microphone_unasked = true,
            Some(Microphone::Allowed) | None => {}
        }
    }
    let (reports, from_media) = channel();
    let mut call = Call {
        join,
        controls,
        seq_ack: None,
        transport: None,
        ssrc: 0,
        dave: Arc::new(Mutex::new(Dave::new(user, room))),
        dave_version: 0,
        users: Arc::default(),
        connected: BTreeSet::from([user]),
        media: None,
        audio: None,
        reports,
        from_media,
        microphone_unasked,
        joined: false,
        heartbeat: None,
        next_beat: Instant::now(),
        unacked: 0,
        deadline: None,
    };

    // Tries on the voice server in hand, and resumes since the connection last held.
    let (mut attempt, mut resumes) = (0, 0);
    let mut why = Why::Silent;
    loop {
        let next = call.controls.moved.lock().unwrap().take();
        if let Some(next) = next {
            if call.joined {
                out::send(&MediaResponse::Interrupted);
            }
            call.move_to(next);
            (attempt, resumes) = (0, 0);
        }
        let resume = call.joined;
        let pause = if resume {
            if resumes == RESUMES {
                call.stop_media();
                out::fail(CallFailure::Lost, DROPPED);
            }
            resumes += 1;
            eprintln!("mediad: resume {resumes} of {RESUMES}");
            out::send(&MediaResponse::Interrupted);
            // Heard once per drop, not once per try.
            if resumes == 1 {
                call.controls.cue(Cue::Lost);
            }
            Duration::from_secs(u64::from(resumes))
        } else {
            if attempt == ATTEMPTS {
                let words = match why {
                    Why::Unreachable => UNREACHABLE,
                    Why::Silent => SILENT,
                    Why::Udp => NO_AUDIO_PATH,
                };
                out::fail(CallFailure::Lost, words);
            }
            attempt += 1;
            eprintln!("mediad: joining {}, try {attempt} of {ATTEMPTS}", call.join.endpoint);
            BACKOFF * (attempt - 1)
        };
        match call.wait(pause, false) {
            Waited::Done => {}
            Waited::Moved => continue,
            Waited::Hangup => {
                call.stop_media();
                return;
            }
        }
        let began = Instant::now();
        match call.connection(resume) {
            End::Hangup | End::Ended => return,
            End::Fatal(reason, detail) => out::fail(reason, detail),
            End::Moved => {}
            End::Removed => {
                eprintln!("mediad: the voice server dropped us; waiting {MOVE:?} to hear where the call moved");
                out::send(&MediaResponse::Interrupted);
                match call.wait(MOVE, true) {
                    Waited::Moved => {}
                    Waited::Hangup => return,
                    Waited::Done => out::fail(CallFailure::Refused, gateway::REMOVED),
                }
            }
            End::Dropped(kind, detail) => {
                eprintln!("mediad: connection dropped: {detail}");
                if resume && began.elapsed() > HELD {
                    resumes = 0;
                }
                if !call.joined {
                    call.transport = None;
                    why = kind;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_change_of_my_own_audio_has_its_cue_and_deafening_outranks_muting() {
        assert_eq!(mine((false, false), (true, false)), Some(Cue::Muted));
        assert_eq!(mine((true, false), (false, false)), Some(Cue::Unmuted));
        assert_eq!(mine((false, false), (true, true)), Some(Cue::Deafened));
        assert_eq!(mine((true, true), (false, false)), Some(Cue::Undeafened));
        assert_eq!(mine((true, true), (true, false)), Some(Cue::Undeafened));
        assert_eq!(mine((true, false), (true, false)), None);
    }

    #[test]
    fn a_cue_is_heard_only_when_its_kind_is_on_and_joins_not_while_deafened() {
        let controls = Controls::default();
        controls.cue(Cue::Joined);
        assert_eq!(controls.take_cue(), Some(Cue::Joined));
        controls.tune(Tuning { joins: false, mine: false, connection: false, ..Tuning::default() });
        for cue in [Cue::Joined, Cue::Muted, Cue::Lost] {
            controls.cue(cue);
            assert_eq!(controls.take_cue(), None, "{cue:?}");
        }
        controls.cue(Cue::Test);
        assert_eq!(controls.take_cue(), Some(Cue::Test), "the test sound always plays");
        controls.tune(Tuning::default());
        controls.set_audio(true, true);
        assert_eq!(controls.take_cue(), Some(Cue::Deafened));
        controls.cue(Cue::Left);
        assert_eq!(controls.take_cue(), None, "deafened, nobody is heard leaving");
    }
}
