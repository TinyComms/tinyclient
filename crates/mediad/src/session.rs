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
use crate::discord_video::{self, Remote};
use crate::gateway::{self, closed, Closed, Incoming, Socket};
use crate::media::{self, FromMedia, MICROPHONE_DENIED};
use crate::permission::{microphone, Microphone};
use crate::picture::{CAMERA_HIGH, CAMERA_HIGH_BPS, SHARE_BPS, SHARE_MAX};
use crate::rtp::{self, Cipher, Sender};
use crate::out;
use protocol::{AudioProcessing, CallFailure, CallStats, Cue, InputMode, MediaResponse, TinyPicture, TinySlot, Tuning, VoiceJoin, VoiceQuality};
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
    /// Stops a Go Live leg without hanging up the voice call. The voice loop does not read these.
    share_leg_stop: AtomicBool,
    watch_leg_stop: AtomicBool,
    /// Whose camera to paint, when the shell named one.
    speaker: Mutex<Option<String>>,
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
    /// How clearly to encode (Tiny). Standard until chatd says.
    quality: Mutex<VoiceQuality>,
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

    pub fn set_speaker(&self, source: Option<String>) {
        *self.speaker.lock().unwrap() = source;
    }

    /// Whom to paint, when one person was chosen. `None` keeps whoever is already showing.
    pub fn speaker(&self) -> Option<String> {
        self.speaker.lock().unwrap().clone()
    }

    /// Asks a Go Live leg to return. The voice call stays up.
    pub fn stop_leg(&self, watch: bool) {
        self.leg_flag(watch).store(true, Ordering::Relaxed);
    }

    /// A new leg is about to start, so the stop from the one it replaces is cleared.
    pub fn clear_leg(&self, watch: bool) {
        self.leg_flag(watch).store(false, Ordering::Relaxed);
    }

    /// The leg should return: chatd hung up, or this leg was stopped.
    pub fn leg_done(&self, watch: bool) -> bool {
        self.hangup.load(Ordering::Relaxed) || self.leg_flag(watch).load(Ordering::Relaxed)
    }

    fn leg_flag(&self, watch: bool) -> &AtomicBool {
        if watch { &self.watch_leg_stop } else { &self.share_leg_stop }
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

    pub fn set_quality(&self, quality: VoiceQuality) {
        *self.quality.lock().unwrap() = quality;
    }

    pub fn quality(&self) -> VoiceQuality {
        *self.quality.lock().unwrap()
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

enum Running {
    Voice { stop: Arc<AtomicBool>, thread: JoinHandle<Audio> },
    /// A Go Live connection. It returns; it does not keep a device.
    Stream { stop: Arc<AtomicBool>, thread: JoinHandle<()> },
}

/// What `Ready` gave us, held until the session description completes it.
struct Transport {
    socket: UdpSocket,
    ssrc: u32,
    /// The video SSRC and its retransmission, when Ready named a stream.
    video: Option<(u32, u32)>,
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
    media: Option<Running>,
    /// The video SSRCs Ready gave us, once the session description has taken the transport.
    assigned: Option<(u32, u32)>,
    /// Video SSRCs of other people, filled from opcode 12 and read by the media thread.
    videos: Arc<Mutex<HashMap<u32, Remote>>>,
    /// Set after opcode 12 goes out. The media thread sends picture packets only while it is set.
    announced: Arc<AtomicBool>,
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
        let Some(running) = self.media.take() else {
            return;
        };
        match running {
            Running::Voice { stop, thread } => {
                stop.store(true, Ordering::Relaxed);
                if let Ok(audio) = thread.join() {
                    self.audio = Some(audio);
                }
            }
            Running::Stream { stop, thread } => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
            }
        }
    }

    /// The voice call ends on hang-up. A Go Live leg also ends when its own flag is set, which
    /// does not hang up the voice call.
    fn stopping(&self) -> bool {
        if self.join.stream {
            self.controls.leg_done(self.join.watch)
        } else {
            self.controls.hangup.load(Ordering::Relaxed)
        }
    }

    /// A voice-server move belongs to the voice leg. A stream leg ignores it.
    fn shifting(&self) -> bool {
        !self.join.stream && self.controls.moving()
    }

    /// Takes up the voice session the call moved to. The old connection's socket, UDP path,
    /// keys and SSRCs are gone; the devices stay open, mute and deafen stay as they are, and the
    /// end-to-end identity stays, with its group formed again on the new connection (a voice
    /// connection starts DAVE from a fresh key package, whatever came before).
    fn move_to(&mut self, next: VoiceJoin) {
        self.stop_media();
        tracing::warn!("mediad: the call moved to {}", next.endpoint);
        self.join = next;
        self.seq_ack = None;
        self.transport = None;
        self.joined = false;
        self.users.lock().unwrap().clear();
        self.videos.lock().unwrap().clear();
        self.assigned = None;
        self.announced.store(false, Ordering::Relaxed);
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
            tracing::warn!("mediad: no transport mode in common; offered {offered:?}");
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
            if self.stopping() {
                return Err(End::Hangup);
            }
            if self.shifting() {
                return Err(End::Moved);
            }
        }
        let Some((address, port)) = seen else {
            return Err(End::Dropped(Why::Udp, format!("no answer over UDP from {ip}:{port}")));
        };
        let data = json!({ "address": address, "port": port, "mode": mode });
        let codecs = codecs(!self.join.watch);
        socket
            .send(gateway::SELECT_PROTOCOL, json!({ "protocol": "udp", "data": data, "codecs": codecs }))
            .map_err(dropped)?;
        self.transport = Some(Transport { socket: udp, ssrc, video: assigned_stream(d) });
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
            tracing::warn!("mediad: the voice server chose {mode:?} with a {} byte key", key.len());
            return Err(End::Fatal(CallFailure::Encryption, NO_CIPHER.to_owned()));
        };
        let version = number(&d["dave_protocol_version"]).unwrap_or(0) as u16;
        if version == 0 && self.join.dave.required {
            return Err(End::Fatal(CallFailure::Encryption, NOT_ENCRYPTED.to_owned()));
        }
        if self.join.stream {
            return self.start_stream(socket, transport, mode, &key, open, version);
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
            pipe: transport.video.and_then(|(ssrc, rtx)| discord_video::Pipe::new(mode, &key, ssrc, rtx)),
            videos: self.videos.clone(),
            announced: self.announced.clone(),
        };
        self.assigned = transport.video;
        let thread = out::spawn(std::thread::Builder::new()
            .name("media".to_owned()),
            move || media::run(setup))
            .map_err(|err| {
                tracing::warn!("mediad: the media thread did not start: {err}");
                End::Fatal(CallFailure::Engine, ENGINE.to_owned())
            })?;
        self.media = Some(Running::Voice { stop, thread });

        // Announces our SSRC. Speaking itself is told when it starts. The sink wants follow,
        // so a camera already in the channel is forwarded; the fake server matches speaking
        // by opcode and is not bothered by the extra frame.
        let quiet = json!({ "speaking": 0, "delay": 0, "ssrc": self.ssrc });
        socket.send(gateway::SPEAKING, quiet).map_err(dropped)?;
        socket.send(gateway::SINK, json!({ "any": 100 })).map_err(dropped)?;
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
            gateway::VIDEO => {
                self.source(&d["user_id"], &d["audio_ssrc"]);
                self.note_video(&d);
            }
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
                    self.videos.lock().unwrap().retain(|_, remote| remote.user_id != user_id);
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
            if self.stopping() {
                return End::Hangup;
            }
            if self.shifting() {
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
                    FromMedia::Picture { share, on } => self.announce_video(socket, share, on),
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
                    tracing::warn!("mediad: the voice server closed the socket ({code:?})");
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
            socket.send(gateway::IDENTIFY, identify(join))
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
            if self.stopping() {
                return Waited::Hangup;
            }
            if self.shifting() {
                return Waited::Moved;
            }
            if Instant::now() >= until {
                return Waited::Done;
            }
            std::thread::sleep(Duration::from_millis(if for_move { 5 } else { 20 }));
        }
    }

    /// A Go Live leg: the same gateway, and a picture loop instead of the microphone. It sends
    /// no speaking indication and no `Connected`. A failure here is a notice, not the end of
    /// the voice call.
    fn start_stream(&mut self, socket: &mut Socket, transport: Transport, mode: &str, key: &[u8], opener: Cipher, version: u16) -> Result<(), End> {
        let first = self.dave.lock().unwrap().start(version);
        self.deliver(socket, first)?;
        self.dave_version = version;
        self.ssrc = transport.ssrc;
        self.assigned = transport.video;
        let pipe = transport.video.and_then(|(ssrc, rtx)| discord_video::Pipe::new(mode, key, ssrc, rtx));
        let stop = Arc::new(AtomicBool::new(false));
        let leg = discord_video::Leg {
            socket: transport.socket,
            opener,
            dave: self.dave.clone(),
            controls: self.controls.clone(),
            report: self.reports.clone(),
            stop: stop.clone(),
            videos: self.videos.clone(),
            announced: self.announced.clone(),
            user_id: self.join.user_id.clone(),
            pipe,
            watch: self.join.watch,
        };
        let thread = out::spawn(std::thread::Builder::new().name("stream".to_owned()), move || discord_video::run(leg)).map_err(|err| {
            tracing::warn!("mediad: the stream thread did not start: {err}");
            End::Fatal(CallFailure::Engine, ENGINE.to_owned())
        })?;
        self.media = Some(Running::Stream { stop, thread });
        // Viewers and the sender both say what they want. A stream does not speak.
        socket.send(gateway::SINK, json!({ "any": 100 })).map_err(dropped)?;
        self.joined = true;
        self.deadline = None;
        Ok(())
    }

    /// Opcode 12 named someone else's video SSRCs. Ours are ignored; the media thread routes
    /// by this map and must not open a packet we sent.
    fn note_video(&mut self, d: &Value) {
        let Some(user_id) = text(&d["user_id"]) else {
            return;
        };
        if user_id == self.join.user_id {
            return;
        }
        let mut videos = self.videos.lock().unwrap();
        videos.retain(|_, remote| remote.user_id != user_id);
        let Some(streams) = d["streams"].as_array() else {
            return;
        };
        for stream in streams {
            let Some(ssrc) = number(&stream["ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0) else {
                continue;
            };
            let rtx = number(&stream["rtx_ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0);
            videos.insert(ssrc, Remote { user_id: user_id.clone(), repair_of: None });
            if let Some(rtx) = rtx {
                videos.insert(rtx, Remote { user_id: user_id.clone(), repair_of: Some(ssrc) });
            }
        }
    }

    /// The capture thread asked for a picture, or to stop. The announcement goes out before
    /// any packet: the flag is set only after the send returns. A stream leg ignores a camera
    /// report, and the voice leg ignores a share.
    fn announce_video(&mut self, socket: &mut Socket, share: bool, on: bool) -> Result<(), End> {
        if share != self.join.stream {
            return Ok(());
        }
        if !on {
            self.announced.store(false, Ordering::Relaxed);
            if self.assigned.is_some() {
                socket.send(gateway::VIDEO, self.video_body(false)).map_err(dropped)?;
            }
            return Ok(());
        }
        if self.assigned.is_none() {
            if self.join.stream {
                self.controls.set_share(false);
            } else {
                self.controls.set_camera(false);
            }
            out::send(&MediaResponse::Notice { detail: discord_video::NO_ASSIGNED.to_owned() });
            out::send(&MediaResponse::Sending { share: self.controls.share(), camera: self.controls.camera() });
            return Ok(());
        }
        socket.send(gateway::VIDEO, self.video_body(true)).map_err(dropped)?;
        self.announced.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Opcode 12's body. Off, or never assigned, is a video SSRC of 0 and no streams. The
    /// negotiated type is `video` on both legs; identify is what said `screen`.
    fn video_body(&self, active: bool) -> Value {
        let Some((ssrc, rtx)) = self.assigned.filter(|_| active) else {
            return json!({ "audio_ssrc": self.ssrc, "video_ssrc": 0, "rtx_ssrc": 0, "streams": [] });
        };
        let screen = self.join.stream && !self.join.watch;
        let ((width, height), bitrate) = if screen { (SHARE_MAX, SHARE_BPS) } else { (CAMERA_HIGH, CAMERA_HIGH_BPS) };
        json!({
            "audio_ssrc": self.ssrc,
            "video_ssrc": ssrc,
            "rtx_ssrc": rtx,
            "streams": [{
                "type": "video",
                "rid": "100",
                "ssrc": ssrc,
                "rtx_ssrc": rtx,
                "active": true,
                "quality": 100,
                "max_bitrate": bitrate,
                "max_framerate": 15,
                "max_resolution": { "type": "fixed", "width": width, "height": height },
            }],
        })
    }
}

/// What we can send and receive. One H.264 layer. A watch leg does not encode.
fn codecs(encode: bool) -> Value {
    json!([
        { "name": "opus", "type": "audio", "priority": 1000, "payload_type": rtp::PAYLOAD_TYPE },
        {
            "name": "H264",
            "type": "video",
            "priority": 1000,
            "payload_type": rtp::H264,
            "rtx_payload_type": rtp::RTX,
            "encode": encode,
            "decode": true,
        },
    ])
}

/// The voice identify. A camera offers one video layer. Go Live offers a screen layer. A
/// viewer offers video and no stream of its own. No channel id: that field is gateway v9.
fn identify(join: &VoiceJoin) -> Value {
    let mut d = json!({
        "server_id": join.server_id,
        "user_id": join.user_id,
        "session_id": join.session_id,
        "token": join.token,
        "max_dave_protocol_version": join.dave.max_version.min(dave::MAX_VERSION),
        "video": true,
    });
    if !join.watch {
        let kind = if join.stream { "screen" } else { "video" };
        d["streams"] = json!([{ "type": kind, "rid": "100", "quality": 100 }]);
    }
    d
}

/// The SSRC Ready assigned to the layer we offered, and its retransmission. A missing
/// retransmission is the next number. No stream, or a zero SSRC, means this call has no picture.
fn assigned_stream(d: &Value) -> Option<(u32, u32)> {
    let streams = d["streams"].as_array()?;
    let chosen = streams.iter().find(|stream| stream["rid"].as_str() == Some("100")).or(streams.first())?;
    let ssrc = number(&chosen["ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0)?;
    let rtx = number(&chosen["rtx_ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0).unwrap_or(ssrc.saturating_add(1));
    Some((ssrc, rtx))
}

/// Runs the call to its end. Returns when chatd hung up or the other side ended it; a failure
/// of the voice call does not return (`out::fail` exits). A Go Live leg returns instead, and
/// says why with a notice: ending the process would take the voice call with it.
pub fn run(join: VoiceJoin, controls: Arc<Controls>) {
    let stream = join.stream;
    // A phone's audio session, for as long as the voice call runs. A stream leg shares the
    // voice call's session and must not open another. Nothing on a desktop.
    let _audio_session = (!stream).then(crate::platform::call_audio);
    let fail = |reason, detail: &str| -> bool {
        if stream {
            out::send(&MediaResponse::Notice { detail: detail.to_owned() });
            true
        } else {
            out::fail(reason, detail);
        }
    };
    let room = if join.group_id.is_empty() { join.channel_id.parse::<u64>() } else { join.group_id.parse::<u64>() };
    let (Ok(user), Ok(room)) = (join.user_id.parse::<u64>(), room) else {
        tracing::warn!("mediad: the voice session's ids are not numbers ({:?}, {:?})", join.user_id, join.channel_id);
        if fail(CallFailure::Refused, UNUSABLE) {
            return;
        }
        unreachable!();
    };
    if join.dave.required && join.dave.max_version.min(dave::MAX_VERSION) == 0 {
        if fail(CallFailure::Encryption, NOT_ENCRYPTED) {
            return;
        }
    }
    let mut microphone_unasked = false;
    if !stream && !Audio::simulated() {
        match microphone() {
            Some(Microphone::Denied) => {
                if fail(CallFailure::Audio, MICROPHONE_DENIED) {
                    return;
                }
            }
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
        assigned: None,
        videos: Arc::default(),
        announced: Arc::new(AtomicBool::new(false)),
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
        let next = if call.join.stream { None } else { call.controls.moved.lock().unwrap().take() };
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
                if fail(CallFailure::Lost, DROPPED) {
                    return;
                }
            }
            resumes += 1;
            tracing::warn!("mediad: resume {resumes} of {RESUMES}");
            if !call.join.stream {
                out::send(&MediaResponse::Interrupted);
            }
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
                if fail(CallFailure::Lost, words) {
                    return;
                }
            }
            attempt += 1;
            tracing::warn!("mediad: joining {}, try {attempt} of {ATTEMPTS}", call.join.endpoint);
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
            End::Fatal(reason, detail) => {
                if fail(reason, &detail) {
                    return;
                }
            }
            End::Moved => {}
            End::Removed => {
                tracing::warn!("mediad: the voice server dropped us; waiting {MOVE:?} to hear where the call moved");
                if !call.join.stream {
                    out::send(&MediaResponse::Interrupted);
                }
                match call.wait(MOVE, true) {
                    Waited::Moved => {}
                    Waited::Hangup => return,
                    Waited::Done => {
                        if call.join.stream {
                            out::send(&MediaResponse::Notice { detail: gateway::REMOVED.to_owned() });
                            return;
                        }
                        out::fail(CallFailure::Refused, gateway::REMOVED);
                    }
                }
            }
            End::Dropped(kind, detail) => {
                tracing::warn!("mediad: connection dropped: {detail}");
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

    fn sample_join(stream: bool, watch: bool) -> VoiceJoin {
        VoiceJoin {
            network: protocol::Network::Discord,
            endpoint: "voice.example:443".into(),
            token: "t".into(),
            session_id: "s".into(),
            user_id: "1".into(),
            server_id: "9".into(),
            channel_id: "9".into(),
            dave: protocol::Dave { max_version: 1, required: true },
            muted: false,
            deafened: false,
            stream,
            watch,
            group_id: String::new(),
        }
    }

    #[test]
    fn identify_offers_one_camera_layer_a_screen_on_go_live_and_nothing_to_send_when_watching() {
        let voice = identify(&sample_join(false, false));
        assert_eq!(voice["video"], json!(true));
        assert_eq!(voice["streams"][0]["type"], "video");
        assert_eq!(voice["streams"][0]["rid"], "100");
        assert_eq!(voice["streams"][0]["quality"], 100);
        assert!(voice.get("channel_id").is_none());
        assert_eq!(identify(&sample_join(true, false))["streams"][0]["type"], "screen");
        assert!(identify(&sample_join(true, true)).get("streams").is_none());
    }

    #[test]
    fn codecs_offer_opus_and_one_h264_layer_and_a_viewer_does_not_encode() {
        let offered = codecs(true);
        assert_eq!(offered[0]["name"], "opus");
        assert_eq!(offered[0]["payload_type"], json!(rtp::PAYLOAD_TYPE));
        assert_eq!(offered[1]["name"], "H264");
        assert_eq!(offered[1]["payload_type"], json!(rtp::H264));
        assert_eq!(offered[1]["rtx_payload_type"], json!(rtp::RTX));
        assert_eq!(offered[1]["encode"], true);
        assert_eq!(offered[1]["decode"], true);
        assert_eq!(codecs(false)[1]["encode"], false);
    }

    #[test]
    fn ready_names_the_layer_we_send_and_a_missing_retransmission_is_the_next_ssrc() {
        let ready = json!({ "streams": [{ "rid": "100", "ssrc": 7, "rtx_ssrc": 8 }] });
        assert_eq!(assigned_stream(&ready), Some((7, 8)));
        assert_eq!(assigned_stream(&json!({ "streams": [{ "rid": "50", "ssrc": 1 }, { "rid": "100", "ssrc": "4" }] })), Some((4, 5)));
        assert_eq!(assigned_stream(&json!({})), None);
        assert_eq!(assigned_stream(&json!({ "streams": [{ "ssrc": 0 }] })), None);
    }
}
