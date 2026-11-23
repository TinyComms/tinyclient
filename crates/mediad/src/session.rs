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
use crate::picture::CameraProfile;
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
    pub(crate) video_stats: Mutex<crate::video_stats::Meter>,
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
    /// Remote ICE candidates delivered for an active Signal WebRTC call.
    pub(crate) signal_ice: Mutex<Vec<String>>,
    /// The camera and the share the user asked for, and whether the one picture is watched.
    share: AtomicBool,
    /// The share's sound, on the Go Live audio SSRC. Off until a share asks for it.
    share_sound: AtomicBool,
    camera: AtomicBool,
    camera_preview: AtomicBool,
    watch: AtomicBool,
    /// Stops a Go Live leg without hanging up the voice call. The voice loop does not read these.
    share_leg_stop: AtomicBool,
    watch_leg_stops: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Whose camera to paint, when the shell named one.
    speaker: Mutex<Option<String>>,
    /// An answer to the renegotiation offer, or an empty SDP when the server refused it.
    picture_answer: Mutex<Option<String>>,
    /// Who each active video receive slot carries now (`call.slots.videos` or single `video`).
    picture_slots: Mutex<Option<Vec<TinyPicture>>>,
    /// Multi-stream watch list accepted by the decode budget.
    watch_sources: Mutex<Vec<protocol::WatchSource>>,
    pub(crate) watched_audio: Mutex<HashMap<String, discord_video::WatchAudio>>,
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
    /// Incoming Telegram 1:1 signaling packets (`updatePhoneCallSignalingData`).
    tg_signals: Mutex<std::collections::VecDeque<Vec<u8>>>,
    /// Updated Telegram group voice chat SSRC -> participant mapping.
    tg_sources: Mutex<Option<Vec<protocol::TelegramGroupSource>>>,
    /// Updated Telegram group video & presentation endpoint/SSRC mapping (#467).
    tg_video_sources: Mutex<Option<Vec<protocol::TelegramGroupVideoSource>>>,
    /// Pending Telegram presentation transport stop flag (#467).
    tg_presentation_stop: AtomicBool,
    /// Queued RTMP live stream segments (time_ms, scale, video_channel, video_quality, raw bytes) (#454).
    tg_rtmp_segments: Mutex<std::collections::VecDeque<crate::telegram::RtmpSegmentInput>>,
    /// Latest RTMP stream state (#454).
    tg_rtmp_state: Mutex<Option<protocol::RtmpStreamState>>,
    /// Pure-Rust `tde2e`-compatible E2EE state for an encrypted Telegram conference call.
    tg_conf_e2ee: Mutex<Option<crate::telegram_e2ee::ConferenceE2ee>>,
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

    pub fn push_signal_ice(&self, candidate: String) {
        self.signal_ice.lock().unwrap().push(candidate);
    }

    pub fn take_signal_ice(&self) -> Vec<String> {
        std::mem::take(&mut *self.signal_ice.lock().unwrap())
    }

    pub fn set_share(&self, on: bool) {
        self.share.store(on, Ordering::Relaxed);
    }

    pub fn share(&self) -> bool {
        self.share.load(Ordering::Relaxed)
    }

    pub fn set_share_sound(&self, on: bool) {
        self.share_sound.store(on, Ordering::Relaxed);
    }

    pub fn share_sound(&self) -> bool {
        self.share_sound.load(Ordering::Relaxed)
    }

    pub fn set_camera(&self, on: bool) {
        self.camera.store(on, Ordering::Relaxed);
    }

    pub fn camera(&self) -> bool {
        self.camera.load(Ordering::Relaxed)
    }

    pub fn set_camera_preview(&self, on: bool) {
        self.camera_preview.store(on, Ordering::Relaxed);
    }

    pub fn camera_preview(&self) -> bool {
        self.camera_preview.load(Ordering::Relaxed)
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

    /// Asks a Go Live leg to return. The voice call stays up. When `watch` is true and
    /// `stream_owner` is `Some`, only that stream's leg is stopped; `None` stops all watch legs.
    pub fn stop_leg(&self, watch: bool, stream_owner: Option<&str>) {
        if !watch {
            self.share_leg_stop.store(true, Ordering::Relaxed);
            return;
        }
        if let Some(owner) = stream_owner {
            if let Some(flag) = self.watch_leg_stops.lock().unwrap().get(owner) {
                flag.store(true, Ordering::Relaxed);
            }
            self.watched_audio.lock().unwrap().remove(owner);
        } else {
            for flag in self.watch_leg_stops.lock().unwrap().values() {
                flag.store(true, Ordering::Relaxed);
            }
            self.watched_audio.lock().unwrap().clear();
        }
    }

    /// A new leg is about to start, so the stop from the one it replaces is cleared.
    pub fn clear_leg(&self, watch: bool, stream_owner: Option<&str>) {
        if !watch {
            self.share_leg_stop.store(false, Ordering::Relaxed);
            return;
        }
        let mut stops = self.watch_leg_stops.lock().unwrap();
        if let Some(owner) = stream_owner {
            stops.insert(owner.to_owned(), Arc::new(AtomicBool::new(false)));
        } else {
            stops.clear();
        }
    }

    /// The leg should return: chatd hung up, or this leg was stopped.
    pub fn leg_done(&self, watch: bool, stream_owner: Option<&str>) -> bool {
        if self.hangup.load(Ordering::Relaxed) {
            return true;
        }
        if !watch {
            return self.share_leg_stop.load(Ordering::Relaxed);
        }
        let key = stream_owner.unwrap_or("");
        self.watch_leg_stops
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    /// The answer to the last renegotiation, taken once by the call's loop.
    pub fn picture_answer(&self, sdp: String) {
        *self.picture_answer.lock().unwrap() = Some(sdp);
    }

    pub(crate) fn take_picture_answer(&self) -> Option<String> {
        self.picture_answer.lock().unwrap().take()
    }

    /// Who the single legacy video slot carries now.
    pub fn set_picture(&self, picture: Option<TinyPicture>) {
        self.set_pictures(picture.into_iter().collect());
    }

    /// Who each active video receive slot carries now, from `call.slots.videos`.
    pub fn set_pictures(&self, pictures: Vec<TinyPicture>) {
        *self.picture_slots.lock().unwrap() = Some(pictures);
    }

    pub(crate) fn take_pictures(&self) -> Option<Vec<TinyPicture>> {
        self.picture_slots.lock().unwrap().take()
    }

    pub fn set_watch_sources(&self, sources: Vec<protocol::WatchSource>) {
        *self.watch_sources.lock().unwrap() = sources;
    }

    pub(crate) fn watch_sources(&self) -> Vec<protocol::WatchSource> {
        self.watch_sources.lock().unwrap().clone()
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
            Cue::Joined | Cue::Left | Cue::ShareStarted => tuning.joins && !self.deafened.load(Ordering::Relaxed),
            Cue::RecordingStarted => !self.deafened.load(Ordering::Relaxed),
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
        if deafened {
            self.watched_audio.lock().unwrap().clear();
        }
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

    pub fn push_tg_signal(&self, data: Vec<u8>) {
        let mut queue = self.tg_signals.lock().unwrap();
        if queue.len() >= 128 {
            queue.pop_front();
        }
        queue.push_back(data);
    }

    pub fn take_tg_signals(&self) -> Vec<Vec<u8>> {
        self.tg_signals.lock().unwrap().drain(..).collect()
    }

    pub fn set_tg_sources(&self, sources: Vec<protocol::TelegramGroupSource>) {
        *self.tg_sources.lock().unwrap() = Some(sources);
    }

    pub fn take_tg_sources(&self) -> Option<Vec<protocol::TelegramGroupSource>> {
        self.tg_sources.lock().unwrap().take()
    }

    pub fn set_tg_video_sources(&self, sources: Vec<protocol::TelegramGroupVideoSource>) {
        *self.tg_video_sources.lock().unwrap() = Some(sources);
    }

    pub fn take_tg_video_sources(&self) -> Option<Vec<protocol::TelegramGroupVideoSource>> {
        self.tg_video_sources.lock().unwrap().take()
    }

    pub fn set_tg_presentation_stop(&self, stop: bool) {
        self.tg_presentation_stop.store(stop, Ordering::Relaxed);
    }

    pub fn tg_presentation_stopped(&self) -> bool {
        self.tg_presentation_stop.load(Ordering::Relaxed)
    }

    pub fn push_tg_rtmp_segment(&self, seg: crate::telegram::RtmpSegmentInput) {
        let mut q = self.tg_rtmp_segments.lock().unwrap();
        if q.len() >= 64 {
            q.pop_front();
        }
        q.push_back(seg);
    }

    pub fn take_tg_rtmp_segments(&self) -> Vec<crate::telegram::RtmpSegmentInput> {
        self.tg_rtmp_segments.lock().unwrap().drain(..).collect()
    }

    pub fn set_tg_rtmp_state(&self, state: protocol::RtmpStreamState) {
        *self.tg_rtmp_state.lock().unwrap() = Some(state);
    }

    pub fn tg_rtmp_state(&self) -> Option<protocol::RtmpStreamState> {
        *self.tg_rtmp_state.lock().unwrap()
    }

    pub fn init_tg_conference(&self, my_user_id: i64, public_key: [u8; 32]) {
        *self.tg_conf_e2ee.lock().unwrap() = Some(crate::telegram_e2ee::ConferenceE2ee::new(my_user_id, public_key));
    }

    pub fn clear_tg_conference(&self) {
        *self.tg_conf_e2ee.lock().unwrap() = None;
    }

    pub fn tg_conference_active(&self) -> bool {
        self.tg_conf_e2ee.lock().unwrap().is_some()
    }

    pub fn apply_tg_conference_block(
        &self,
        sub_chain_id: i32,
        raw: &[u8],
    ) -> Result<protocol::CallSecurity, (crate::telegram_e2ee::E2eeError, protocol::CallSecurity)> {
        let mut guard = self.tg_conf_e2ee.lock().unwrap();
        let Some(e2ee) = guard.as_mut() else {
            return Err((
                crate::telegram_e2ee::E2eeError::NotReady,
                protocol::CallSecurity::telegram_conference_blocked(),
            ));
        };
        match e2ee.apply_block_bytes(sub_chain_id, raw) {
            Ok(()) => Ok(e2ee.security()),
            Err(err) => Err((err, e2ee.security())),
        }
    }

    pub fn tg_conference_encrypt(&self, kind: u8, plaintext: &[u8]) -> Result<Vec<u8>, crate::telegram_e2ee::E2eeError> {
        let mut guard = self.tg_conf_e2ee.lock().unwrap();
        let Some(e2ee) = guard.as_mut() else {
            return Err(crate::telegram_e2ee::E2eeError::NotReady);
        };
        e2ee.encrypt_frame(kind, plaintext)
    }

    pub fn tg_conference_decrypt(&self, packet: &[u8]) -> Result<(u8, i64, Vec<u8>), crate::telegram_e2ee::E2eeError> {
        let guard = self.tg_conf_e2ee.lock().unwrap();
        let Some(e2ee) = guard.as_ref() else {
            return Err(crate::telegram_e2ee::E2eeError::NotReady);
        };
        e2ee.decrypt_frame(packet)
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

/// The video SSRCs Ready assigned us: primary (`rid` `"100"`) and optional low camera simulcast
/// layer (`rid` `"50"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Assigned {
    high: (u32, u32),
    low: Option<(u32, u32)>,
}

/// What `Ready` gave us, held until the session description completes it.
struct Transport {
    socket: UdpSocket,
    ssrc: u32,
    /// The video SSRCs and their retransmissions, when Ready named stream(s).
    video: Option<Assigned>,
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
    assigned: Option<Assigned>,
    /// Video SSRCs of other people, filled from opcode 12 and read by the media thread.
    videos: Arc<Mutex<HashMap<u32, Remote>>>,
    /// Set after opcode 12 goes out. The media thread sends picture packets only while it is set.
    announced: Arc<AtomicBool>,
    /// The share quality the last opcode 12 named. A later pick sends opcode 12 again.
    told: Option<crate::picture::Quality>,
    /// The last op 15 media sink wants body sent on this connection.
    last_sink_wants: Option<Value>,
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
            self.controls.leg_done(self.join.watch, self.join.stream_owner.as_deref())
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
        self.last_sink_wants = None;
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
        self.transport = Some(Transport { socket: udp, ssrc, video: assigned_streams(d) });
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
            return self.start_stream(socket, transport, seal, open, version);
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
        self.dave_report();
        self.ssrc = transport.ssrc;
        let stop = Arc::new(AtomicBool::new(false));
        let setup = media::Setup {
            socket: transport.socket,
            sender: Sender::new(seal.clone(), transport.ssrc, rand::random(), rand::random()),
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
            pipe: transport.video.map(|a| discord_video::Pipe::with_rid(seal.clone(), a.high.0, a.high.1, "100")),
            low_pipe: transport.video.and_then(|a| a.low).map(|(ssrc, rtx)| discord_video::Pipe::with_rid(seal, ssrc, rtx, "50")),
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

        // Announces our SSRC. Speaking itself is told when it starts. The initial sink wants
        // follow so remote cameras already in the channel are advertised; per-SSRC wants replace
        // this as soon as op 12 names each remote video SSRC.
        let quiet = json!({ "speaking": 0, "delay": 0, "ssrc": self.ssrc });
        socket.send(gateway::SPEAKING, quiet).map_err(dropped)?;
        let initial_wants = json!({ "any": 100 });
        socket.send(gateway::SINK, initial_wants.clone()).map_err(dropped)?;
        self.last_sink_wants = Some(initial_wants);
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
        let mut users = self.users.lock().unwrap();
        if self.join.watch {
            users.retain(|&previous, user| previous == ssrc || *user != user_id);
        }
        let known = users.insert(ssrc, user_id.clone());
        drop(users);
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
                    if self.join.watch && self.join.stream_owner.as_deref() == Some(&user_id) {
                        self.controls.watched_audio.lock().unwrap().remove(&user_id);
                    } else if !self.join.stream {
                        out::send(&MediaResponse::Gone { user_id });
                    }
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
                    self.dave_report();
                    self.encrypted()?;
                }
            }
            dave::PREPARE_EPOCH => {
                if let (Some(epoch), Some(version)) = (number(&d["epoch"]), number(&d["protocol_version"])) {
                    let answer = self.dave.lock().unwrap().prepare_epoch(epoch, version as u16);
                    self.deliver(socket, answer)?;
                    self.dave_report();
                }
            }
            // Media sink wants, flags, platform and the rest carry nothing an audio call needs.
            _ => {}
        }
        Ok(())
    }

    fn binary(&mut self, socket: &mut Socket, op: u8, payload: &[u8]) -> Result<(), End> {
        let epoch = matches!(op, dave::ANNOUNCE_COMMIT | dave::WELCOME);
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
        self.deliver(socket, answer)?;
        if epoch {
            self.dave_report();
        }
        Ok(())
    }

    /// The privacy code and each pairwise verification code, after a transition or a new epoch.
    /// Scrypt runs off the dave lock: `encrypt` must not wait on it.
    fn dave_report(&self) {
        let wanted = self.dave.lock().unwrap().safety_wanted();
        let made: Vec<(u64, Vec<u8>, String)> = wanted
            .into_iter()
            .map(|(id, key, me, mine)| {
                let number = crate::e2ee::safety_number(id, &key, me, &mine);
                (id, key, number)
            })
            .collect();
        let report = {
            let mut dave = self.dave.lock().unwrap();
            for (id, key, number) in made {
                dave.add_safety(id, key, number);
            }
            dave.take_report(self.join.stream)
        };
        if let Some(security) = report {
            out::send(&MediaResponse::TinySecurity(security));
        }
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
                    // 2 is Discord's soundshare flag. 0 clears it. The microphone's 1 stays on the voice leg.
                    FromMedia::Soundshare(on) => {
                        let d = json!({ "speaking": if on { 2 } else { 0 }, "delay": 0, "ssrc": self.ssrc });
                        socket.send(gateway::SPEAKING, d).map_err(dropped)
                    }
                };
                if let Err(end) = sent {
                    return end;
                }
            }
            if let Err(end) = self.tell_share_again(socket) {
                return end;
            }
            if let Err(end) = self.tell_sink_wants(socket) {
                return end;
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
    /// no `Connected`. Sound, when the share asked for it, is Opus on this connection's audio
    /// SSRC. A failure here is a notice, not the end of the voice call.
    fn start_stream(&mut self, socket: &mut Socket, transport: Transport, seal: Cipher, opener: Cipher, version: u16) -> Result<(), End> {
        let first = self.dave.lock().unwrap().start(version);
        self.deliver(socket, first)?;
        self.dave_version = version;
        self.ssrc = transport.ssrc;
        self.assigned = transport.video;
        // The picture takes the cipher. The sound, on a sending leg, uses the same key on the
        // audio SSRC Ready assigned. A watcher only receives.
        let audio = (!self.join.watch).then(|| rtp::Sender::new(seal.clone(), transport.ssrc, 0, 0));
        let pipe = transport.video.map(|a| discord_video::Pipe::with_rid(seal, a.high.0, a.high.1, "100"));
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
            audio,
            watch: self.join.watch,
            stream_owner: self.join.stream_owner.clone(),
            users: self.users.clone(),
        };
        let thread = out::spawn(std::thread::Builder::new().name("stream".to_owned()), move || discord_video::run(leg)).map_err(|err| {
            tracing::warn!("mediad: the stream thread did not start: {err}");
            End::Fatal(CallFailure::Engine, ENGINE.to_owned())
        })?;
        self.media = Some(Running::Stream { stop, thread });
        // Viewers and the sender both say what they want. Sound speaks for itself, as flag 2.
        let initial_wants = json!({ "any": 100 });
        socket.send(gateway::SINK, initial_wants.clone()).map_err(dropped)?;
        self.last_sink_wants = Some(initial_wants);
        self.joined = true;
        self.deadline = None;
        self.dave_report();
        Ok(())
    }

    /// Opcode 12 named someone else's video SSRCs (including multiple simulcast streams/layers
    /// per user). Ours are ignored; the media thread routes by this map and must not open a
    /// packet we sent.
    fn note_video(&mut self, d: &Value) {
        let Some(user_id) = text(&d["user_id"]) else {
            return;
        };
        if user_id == self.join.user_id {
            return;
        }
        let mut videos = self.videos.lock().unwrap();
        videos.retain(|_, remote| remote.user_id != user_id);
        if let Some(streams) = d["streams"].as_array() {
            for stream in streams {
                let Some(ssrc) = number(&stream["ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0) else {
                    continue;
                };
                let rtx = number(&stream["rtx_ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0);
                let rid = stream["rid"].as_str().unwrap_or("100").to_owned();
                let quality = number(&stream["quality"]).unwrap_or(100) as u32;
                let active = stream["active"].as_bool().unwrap_or(true);
                videos.insert(
                    ssrc,
                    Remote { user_id: user_id.clone(), repair_of: None, rid: rid.clone(), quality, active },
                );
                if let Some(rtx) = rtx {
                    videos.insert(
                        rtx,
                        Remote { user_id: user_id.clone(), repair_of: Some(ssrc), rid, quality, active },
                    );
                }
            }
            return;
        }
        if let Some(ssrc) = number(&d["video_ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0) {
            let rtx = number(&d["rtx_ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0);
            videos.insert(
                ssrc,
                Remote { user_id: user_id.clone(), repair_of: None, rid: "100".to_owned(), quality: 100, active: true },
            );
            if let Some(rtx) = rtx {
                videos.insert(
                    rtx,
                    Remote { user_id: user_id.clone(), repair_of: Some(ssrc), rid: "100".to_owned(), quality: 100, active: true },
                );
            }
        }
    }

    /// Sends per-SSRC op 15 (`MEDIA_SINK_WANTS`) whenever the known remote SSRCs or desired tile
    /// sizes/focus change, scaling quality per SSRC instead of leaving only `{"any": 100}`.
    fn tell_sink_wants(&mut self, socket: &mut Socket) -> Result<(), End> {
        if !self.joined || (self.join.stream && !self.join.watch) {
            return Ok(());
        }
        let videos = self.videos.lock().unwrap().clone();
        let watch_sources = self.controls.watch_sources();
        let speaker = self.controls.speaker();
        let Some(next) = media_sink_wants(
            &videos,
            &watch_sources,
            speaker.as_deref(),
            self.join.watch,
            self.join.stream_owner.as_deref(),
        ) else {
            return Ok(());
        };
        if self.last_sink_wants.as_ref() == Some(&next) {
            return Ok(());
        }
        socket.send(gateway::SINK, next.clone()).map_err(dropped)?;
        self.last_sink_wants = Some(next);
        Ok(())
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
            self.told = None;
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
        self.told = Some(crate::picture::share_quality());
        Ok(())
    }

    /// A share already announced, whose quality has since changed, is named again. The capture
    /// thread does not report a quality change: the flag it watches only says on or off.
    fn tell_share_again(&mut self, socket: &mut Socket) -> Result<(), End> {
        if !self.join.stream || self.join.watch || !self.announced.load(Ordering::Relaxed) || self.assigned.is_none() {
            return Ok(());
        }
        let quality = crate::picture::share_quality();
        if self.told == Some(quality) {
            return Ok(());
        }
        socket.send(gateway::VIDEO, video_announcement(true, self.ssrc, self.assigned, true, quality, crate::picture::camera_profile())).map_err(dropped)?;
        self.told = Some(quality);
        Ok(())
    }

    /// Opcode 12's body. Off, or never assigned, is a video SSRC of 0 and no streams. The
    /// negotiated type is `video` on both legs; identify is what said `screen`.
    fn video_body(&self, active: bool) -> Value {
        let screen = self.join.stream && !self.join.watch;
        video_announcement(active, self.ssrc, self.assigned, screen, crate::picture::share_quality(), crate::picture::camera_profile())
    }
}

/// Builds per-SSRC op 15 (`MEDIA_SINK_WANTS`) scaled by tile size and focus:
/// - Focused / `WatchSize::Large` stream -> quality `100`
/// - Grid / `WatchSize::Small` tile -> quality `50`
/// Returns `None` while no active remote video SSRCs are known yet (so initial `{"any": 100}` stands).
fn media_sink_wants(
    videos: &HashMap<u32, Remote>,
    watch_sources: &[protocol::WatchSource],
    speaker: Option<&str>,
    watch_leg: bool,
    stream_owner: Option<&str>,
) -> Option<Value> {
    // Group active primary (non-RTX) SSRCs by remote user, selecting each user's highest-quality
    // announced primary SSRC as the key Discord's SFU matches against.
    let mut primary_by_user: std::collections::BTreeMap<&str, (u32, u32)> = std::collections::BTreeMap::new();
    for (&ssrc, remote) in videos {
        if remote.repair_of.is_some() || !remote.active {
            continue;
        }
        if watch_leg && stream_owner.is_some_and(|owner| owner != remote.user_id) {
            continue;
        }
        primary_by_user
            .entry(remote.user_id.as_str())
            .and_modify(|cur| {
                if remote.quality > cur.1 || (remote.quality == cur.1 && ssrc < cur.0) {
                    *cur = (ssrc, remote.quality);
                }
            })
            .or_insert((ssrc, remote.quality));
    }
    if primary_by_user.is_empty() {
        return None;
    }
    let kind = if watch_leg { protocol::PictureKind::Share } else { protocol::PictureKind::Camera };
    let has_explicit = watch_sources.iter().any(|s| s.kind == kind);
    let many_cameras = !watch_leg && primary_by_user.len() > 1;
    let mut map = serde_json::Map::new();
    for (user_id, (ssrc, _)) in primary_by_user {
        let quality: u32 = if let Some(entry) = watch_sources.iter().find(|s| s.user_id == user_id && s.kind == kind) {
            match entry.size {
                protocol::WatchSize::Large => 100,
                protocol::WatchSize::Small => 50,
            }
        } else if watch_leg {
            100
        } else if has_explicit {
            // Other visible cameras not explicitly focused still stay alive at small quality.
            50
        } else if speaker == Some(user_id) || !many_cameras {
            100
        } else {
            50
        };
        map.insert(ssrc.to_string(), json!(quality));
    }
    Some(Value::Object(map))
}

/// Opcode 12 for one leg. A screen names the share that was picked; a camera uses the resolved
/// [`CameraProfile`] (up to 720p@30 HD when hardware or HD is selected, 360p@15 otherwise) plus
/// a second low simulcast layer (`rid` `"50"`, 360p@15 beneath 720p or 180p@15 beneath 360p) when
/// Ready assigned a second SSRC pair. Off, or never assigned, is a video SSRC of 0 and no streams.
fn video_announcement(active: bool, audio_ssrc: u32, assigned: Option<Assigned>, screen: bool, quality: crate::picture::Quality, camera: CameraProfile) -> Value {
    let Some(assigned) = assigned.filter(|_| active) else {
        return json!({ "audio_ssrc": audio_ssrc, "video_ssrc": 0, "rtx_ssrc": 0, "streams": [] });
    };
    let (ssrc, rtx) = assigned.high;
    let (width, height, fps, bitrate) = if screen {
        (quality.max.0, quality.max.1, quality.fps, quality.bps)
    } else {
        (camera.max.0, camera.max.1, camera.fps, camera.bps)
    };
    let mut streams = vec![json!({
        "type": "video",
        "rid": "100",
        "ssrc": ssrc,
        "rtx_ssrc": rtx,
        "active": true,
        "quality": 100,
        "max_bitrate": bitrate,
        "max_framerate": fps,
        "max_resolution": { "type": "fixed", "width": width, "height": height },
    })];
    if !screen {
        if let Some((low_ssrc, low_rtx)) = assigned.low {
            let low = camera.low_layer();
            streams.push(json!({
                "type": "video",
                "rid": "50",
                "ssrc": low_ssrc,
                "rtx_ssrc": low_rtx,
                "active": true,
                "quality": 50,
                "max_bitrate": low.bps,
                "max_framerate": low.fps,
                "max_resolution": { "type": "fixed", "width": low.max.0, "height": low.max.1 },
            }));
        }
    }
    json!({
        "audio_ssrc": audio_ssrc,
        "video_ssrc": ssrc,
        "rtx_ssrc": rtx,
        "streams": streams,
    })
}

/// The lab's discord-share-quality driver. Opcode 12 names the share that was picked, and a
/// camera announces its resolved profile. A build from before the fix ignores the variable and stays up.
pub fn prove_discord_share_quality() -> Result<(), String> {
    let std_cam = CameraProfile::resolve(protocol::CameraQuality::Standard, false);
    let hd_cam = CameraProfile::resolve(protocol::CameraQuality::Hd, true);
    let one = Some(Assigned { high: (7, 8), low: None });
    let size = |quality: protocol::ShareQuality| {
        let body = video_announcement(true, 1, one, true, crate::picture::Quality::of(quality), std_cam);
        (
            body["streams"][0]["max_resolution"]["width"].as_u64().unwrap_or(0),
            body["streams"][0]["max_resolution"]["height"].as_u64().unwrap_or(0),
            body["streams"][0]["max_framerate"].as_u64().unwrap_or(0),
            body["streams"][0]["max_bitrate"].as_u64().unwrap_or(0),
        )
    };
    if size(protocol::ShareQuality::Low) != (1280, 720, 15, 1_500_000) {
        return Err("low is 720p at 15".into());
    }
    if size(protocol::ShareQuality::Balanced) != (1920, 1080, 30, 4_000_000) {
        return Err("balanced is 1080p at 30".into());
    }
    if size(protocol::ShareQuality::Best) != (2560, 1440, 60, 8_000_000) {
        return Err("best is 1440p at 60".into());
    }
    let camera = video_announcement(true, 1, one, false, crate::picture::Quality::of(protocol::ShareQuality::Best), std_cam);
    if camera["streams"][0]["max_resolution"]["width"] != 640 || camera["streams"][0]["max_framerate"] != 15 {
        return Err("standard camera is 640 by 360 at 15".into());
    }
    let camera_hd = video_announcement(true, 1, one, false, crate::picture::Quality::of(protocol::ShareQuality::Best), hd_cam);
    if camera_hd["streams"][0]["max_resolution"]["width"] != 1280 || camera_hd["streams"][0]["max_framerate"] != 30 {
        return Err("hd camera is 1280 by 720 at 30".into());
    }
    Ok(())
}

/// The lab's discord-wave15a3 driver: verifies codec negotiation (H.264 encode+decode, VP8/AV1
/// decode), simulcast camera op 12 with high+low layers, per-SSRC op 15 sink wants, and real
/// AV1 RTP depacketize + decode.
pub fn prove_discord_wave15a3() -> Result<(), String> {
    let offered = codecs(true);
    let names: Vec<&str> = offered.as_array().ok_or("codecs not array")?.iter().filter_map(|c| c["name"].as_str()).collect();
    if names != ["opus", "H264", "VP8", "AV1"] {
        return Err(format!("unexpected codec list: {names:?}"));
    }
    if offered[1]["encode"] != true || offered[2]["encode"] != false || offered[2]["decode"] != true || offered[3]["encode"] != false || offered[3]["decode"] != true {
        return Err("H264 must encode+decode and VP8/AV1 must be decode-only".into());
    }
    let hd_cam = CameraProfile::resolve(protocol::CameraQuality::Hd, true);
    let sim = video_announcement(true, 1, Some(Assigned { high: (100, 101), low: Some((200, 201)) }), false, crate::picture::Quality::of(protocol::ShareQuality::Balanced), hd_cam);
    if sim["streams"].as_array().map(Vec::len) != Some(2)
        || sim["streams"][0]["rid"] != "100"
        || sim["streams"][0]["max_resolution"]["height"] != 720
        || sim["streams"][1]["rid"] != "50"
        || sim["streams"][1]["max_resolution"]["height"] != 360
    {
        return Err(format!("camera simulcast op 12 mismatch: {sim}"));
    }
    let mut videos = HashMap::new();
    videos.insert(5001, Remote { user_id: "2001".into(), repair_of: None, rid: "100".into(), quality: 100, active: true });
    videos.insert(5002, Remote { user_id: "2002".into(), repair_of: None, rid: "100".into(), quality: 100, active: true });
    let wants = media_sink_wants(
        &videos,
        &[
            protocol::WatchSource { user_id: "2001".into(), kind: protocol::PictureKind::Camera, size: protocol::WatchSize::Large },
            protocol::WatchSource { user_id: "2002".into(), kind: protocol::PictureKind::Camera, size: protocol::WatchSize::Small },
        ],
        None,
        false,
        None,
    )
    .ok_or("missing per-SSRC sink wants")?;
    if wants["5001"] != 100 || wants["5002"] != 50 {
        return Err(format!("per-SSRC op 15 mismatch: {wants}"));
    }
    // Verify AV1 RTP depacketizer + pure-Rust AV1 decoder on the embedded keyframe.
    let pieces = crate::av1::packetize(crate::av1::SAMPLE_KEYFRAME_OBU, 16);
    let mut asm = crate::av1::Assembler::default();
    let mut frame = None;
    for (i, p) in pieces.iter().enumerate() {
        frame = asm.push(i as u16, 90_000, p.marker, &p.payload);
    }
    let frame = frame.ok_or("AV1 RTP assembler did not emit frame")?;
    let mut dec = crate::av1::Av1Decoder::new().map_err(|()| "AV1 decoder init failed")?;
    let planes = dec.decode(&frame).map_err(|()| "AV1 decode failed")?.ok_or("AV1 produced no picture")?;
    if (planes.width, planes.height) != (64, 48) {
        return Err(format!("unexpected AV1 decoded dimensions {}x{}", planes.width, planes.height));
    }
    Ok(())
}

/// The lab's discord-wave15-share driver. Share sound is a real frame with this process's
/// playback taken out, and a Go Live report is not the voice call's report. A build from
/// before this ignores the variable and stays up. The matching 30-digit code is the library test:
/// it needs the MLS harness, which this binary does not link.
pub fn prove_discord_wave15_share() -> Result<(), String> {
    std::env::set_var("TINYCLIENT_SHARE_AUDIO", "tone");
    let mut pull = crate::share_audio::open(None).map_err(|err| format!("share sound did not open: {err}"))?;
    let frame = pull.pull().ok_or("share sound produced no frame")?;
    if frame.iter().all(|sample| *sample == 0) {
        return Err("share sound is silent".into());
    }
    crate::share_audio::note_played(&frame);
    let mut heard = frame;
    crate::share_audio::subtract(&mut heard);
    let energy = |samples: &[i16]| samples.iter().map(|sample| { let value = i64::from(*sample); value * value }).sum::<i64>();
    if energy(&heard) * 4 > energy(&frame) {
        return Err("a share leaves out the call's own playback".into());
    }
    let mut voice = Dave::new(111, 7);
    voice.start(1);
    let report = voice.take_report(false).ok_or("a dave session reports once it has a version")?;
    if report.stream || report.mode != protocol::SecurityMode::Pending || voice.take_report(false).is_some() {
        return Err("the voice report is pending, once, and is not the stream".into());
    }
    let mut stream = Dave::new(111, 7);
    stream.start(1);
    let live = stream.take_report(true).ok_or("a go live leg reports")?;
    if !live.stream || live.mode != protocol::SecurityMode::Pending {
        return Err("a go live report is marked as the stream".into());
    }
    Ok(())
}

/// What we can send and receive: Opus audio, H.264 encode+decode, plus VP8 and AV1 decode-only.
/// A watch leg does not encode.
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
        {
            "name": "VP8",
            "type": "video",
            "priority": 2000,
            "payload_type": rtp::VP8,
            "rtx_payload_type": rtp::VP8_RTX,
            "encode": false,
            "decode": true,
        },
        {
            "name": "AV1",
            "type": "video",
            "priority": 3000,
            "payload_type": rtp::AV1,
            "rtx_payload_type": rtp::AV1_RTX,
            "encode": false,
            "decode": true,
        },
    ])
}

/// The voice identify. A camera offers two simulcast video layers (`rid` `"100"` high and `"50"`
/// low). Go Live offers one screen layer. A viewer offers video and no stream of its own. No
/// channel id: that field is gateway v9.
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
        if join.stream {
            d["streams"] = json!([{ "type": "screen", "rid": "100", "quality": 100 }]);
        } else {
            d["streams"] = json!([
                { "type": "video", "rid": "100", "quality": 100 },
                { "type": "video", "rid": "50", "quality": 50 },
            ]);
        }
    }
    d
}

/// The SSRCs Ready assigned to the layer(s) we offered, and their retransmissions. A missing
/// retransmission is the next number. No stream, or a zero SSRC, means this call has no picture.
fn assigned_streams(d: &Value) -> Option<Assigned> {
    let streams = d["streams"].as_array()?;
    let parse_pair = |stream: &Value| -> Option<(u32, u32)> {
        let ssrc = number(&stream["ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0)?;
        let rtx = number(&stream["rtx_ssrc"]).and_then(|n| u32::try_from(n).ok()).filter(|ssrc| *ssrc != 0).unwrap_or(ssrc.saturating_add(1));
        Some((ssrc, rtx))
    };
    let chosen = streams.iter().find(|stream| stream["rid"].as_str() == Some("100")).or(streams.first())?;
    let high = parse_pair(chosen)?;
    let low = streams
        .iter()
        .find(|stream| stream["rid"].as_str() == Some("50"))
        .and_then(parse_pair)
        .filter(|&(ssrc, _)| ssrc != high.0);
    Some(Assigned { high, low })
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
        dave: Arc::new(Mutex::new({
            let mut dave = Dave::new(user, room);
            if let Some(key) = dave::account_identity() {
                dave.use_identity(key);
            }
            dave
        })),
        dave_version: 0,
        users: Arc::default(),
        connected: BTreeSet::from([user]),
        media: None,
        assigned: None,
        videos: Arc::default(),
        announced: Arc::new(AtomicBool::new(false)),
        told: None,
        last_sink_wants: None,
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
            stream_owner: None,
            group_id: String::new(),
        }
    }

    #[test]
    fn a_share_is_announced_at_the_quality_that_was_picked() {
        let std_cam = CameraProfile::resolve(protocol::CameraQuality::Standard, false);
        let hd_cam = CameraProfile::resolve(protocol::CameraQuality::Hd, true);
        let one = Some(Assigned { high: (7, 8), low: None });
        let sim = Some(Assigned { high: (7, 8), low: Some((9, 10)) });
        let announced = |quality: protocol::ShareQuality| video_announcement(true, 1111, one, true, crate::picture::Quality::of(quality), std_cam);
        let size = |body: &Value| {
            (
                body["streams"][0]["max_resolution"]["width"].as_u64().unwrap(),
                body["streams"][0]["max_resolution"]["height"].as_u64().unwrap(),
                body["streams"][0]["max_framerate"].as_u64().unwrap(),
                body["streams"][0]["max_bitrate"].as_u64().unwrap(),
            )
        };
        assert_eq!(size(&announced(protocol::ShareQuality::Low)), (1280, 720, 15, 1_500_000));
        assert_eq!(size(&announced(protocol::ShareQuality::Balanced)), (1920, 1080, 30, 4_000_000));
        assert_eq!(size(&announced(protocol::ShareQuality::Best)), (2560, 1440, 60, 8_000_000));
        let camera = video_announcement(true, 1111, one, false, crate::picture::Quality::of(protocol::ShareQuality::Best), std_cam);
        assert_eq!(size(&camera), (640, 360, 15, 500_000), "standard camera is 360p at 15");
        let camera_hd = video_announcement(true, 1111, sim, false, crate::picture::Quality::of(protocol::ShareQuality::Best), hd_cam);
        assert_eq!(size(&camera_hd), (1280, 720, 30, 1_500_000), "hd camera announces 720p at 30");
        assert_eq!(camera_hd["streams"].as_array().unwrap().len(), 2, "simulcast announces both high and low camera layers");
        assert_eq!(camera_hd["streams"][1]["rid"], "50");
        assert_eq!(camera_hd["streams"][1]["ssrc"], 9);
        assert_eq!(camera_hd["streams"][1]["rtx_ssrc"], 10);
        assert_eq!(camera_hd["streams"][1]["max_resolution"]["width"], 640);
        assert_eq!(camera_hd["streams"][1]["max_resolution"]["height"], 360);
        assert_eq!(camera_hd["streams"][1]["max_framerate"], 15);
        let off = video_announcement(false, 1111, one, true, crate::picture::Quality::of(protocol::ShareQuality::Best), std_cam);
        assert_eq!(off["video_ssrc"], 0);
        assert!(off["streams"].as_array().unwrap().is_empty());
    }

    #[test]
    fn identify_offers_simulcast_camera_layers_a_screen_on_go_live_and_nothing_to_send_when_watching() {
        let voice = identify(&sample_join(false, false));
        assert_eq!(voice["video"], json!(true));
        assert_eq!(voice["streams"].as_array().unwrap().len(), 2);
        assert_eq!(voice["streams"][0]["type"], "video");
        assert_eq!(voice["streams"][0]["rid"], "100");
        assert_eq!(voice["streams"][0]["quality"], 100);
        assert_eq!(voice["streams"][1]["type"], "video");
        assert_eq!(voice["streams"][1]["rid"], "50");
        assert_eq!(voice["streams"][1]["quality"], 50);
        assert!(voice.get("channel_id").is_none());
        assert_eq!(identify(&sample_join(true, false))["streams"][0]["type"], "screen");
        assert!(identify(&sample_join(true, true)).get("streams").is_none());
    }

    #[test]
    fn codecs_offer_opus_h264_vp8_and_av1_and_a_viewer_does_not_encode() {
        let offered = codecs(true);
        assert_eq!(offered[0]["name"], "opus");
        assert_eq!(offered[0]["payload_type"], json!(rtp::PAYLOAD_TYPE));
        assert_eq!(offered[1]["name"], "H264");
        assert_eq!(offered[1]["payload_type"], json!(rtp::H264));
        assert_eq!(offered[1]["rtx_payload_type"], json!(rtp::RTX));
        assert_eq!(offered[1]["encode"], true);
        assert_eq!(offered[1]["decode"], true);
        assert_eq!(offered[2]["name"], "VP8");
        assert_eq!(offered[2]["payload_type"], json!(rtp::VP8));
        assert_eq!(offered[2]["rtx_payload_type"], json!(rtp::VP8_RTX));
        assert_eq!(offered[2]["encode"], false);
        assert_eq!(offered[2]["decode"], true);
        assert_eq!(offered[3]["name"], "AV1");
        assert_eq!(offered[3]["payload_type"], json!(rtp::AV1));
        assert_eq!(offered[3]["rtx_payload_type"], json!(rtp::AV1_RTX));
        assert_eq!(offered[3]["encode"], false);
        assert_eq!(offered[3]["decode"], true);
        assert_eq!(codecs(false)[1]["encode"], false);
    }

    #[test]
    fn ready_names_high_and_low_simulcast_layers_and_default_rtx() {
        let ready = json!({ "streams": [{ "rid": "100", "ssrc": 7, "rtx_ssrc": 8 }] });
        assert_eq!(assigned_streams(&ready), Some(Assigned { high: (7, 8), low: None }));
        assert_eq!(
            assigned_streams(&json!({ "streams": [{ "rid": "50", "ssrc": 1 }, { "rid": "100", "ssrc": "4" }] })),
            Some(Assigned { high: (4, 5), low: Some((1, 2)) })
        );
        assert_eq!(assigned_streams(&json!({})), None);
        assert_eq!(assigned_streams(&json!({ "streams": [{ "ssrc": 0 }] })), None);
    }

    #[test]
    fn media_sink_wants_scales_per_ssrc_by_tile_size_and_focus() {
        let mut videos = HashMap::new();
        // User 2001 announces two simulcast layers; primary high SSRC 501 should be selected.
        videos.insert(501, Remote { user_id: "2001".into(), repair_of: None, rid: "100".into(), quality: 100, active: true });
        videos.insert(502, Remote { user_id: "2001".into(), repair_of: None, rid: "50".into(), quality: 50, active: true });
        videos.insert(503, Remote { user_id: "2001".into(), repair_of: Some(501), rid: "100".into(), quality: 100, active: true });
        videos.insert(601, Remote { user_id: "2002".into(), repair_of: None, rid: "100".into(), quality: 100, active: true });
        let sources = vec![
            protocol::WatchSource { user_id: "2001".into(), kind: protocol::PictureKind::Camera, size: protocol::WatchSize::Large },
            protocol::WatchSource { user_id: "2002".into(), kind: protocol::PictureKind::Camera, size: protocol::WatchSize::Small },
        ];
        let wants = media_sink_wants(&videos, &sources, None, false, None).unwrap();
        assert_eq!(wants, json!({ "501": 100, "601": 50 }));
    }
}
