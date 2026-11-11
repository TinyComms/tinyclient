//! The one call: its state for the shell, and the mediad that carries it. chatd does signalling
//! and bookkeeping here; no media, codec or audio code ever enters this process.
//!
//! A call is drawn as joined only when mediad said the voice connection is up, and one we rang
//! only once the adapter said the other side answered. A call that fails, from the adapter, from
//! mediad or by running out of time here, stops mediad at once and stays on screen as the failure
//! until it is dismissed or the next call starts. A mediad that dies is such a failure, whenever
//! it dies. Every failure is also told to whoever asked to hear of it (`Calls::on_failure`): the
//! server has the adapter leave on the network, so nobody is left ringing or shown in a channel.

use crate::hub::{now, Hub};
use protocol::media_host::MediaChild;
use protocol::settings::{CAMERA, GLOBAL, INPUT, JOIN_MUTED, OUTPUT, REQUIRE_CALL_E2EE};
use protocol::{
    clip, read_frame, write_frame, AudioDevice, Call, CallFailure, CallPicture, CallState, Change,
    Content, Cue, Direction, Key, MediaRequest, MediaResponse, Participant, PictureKind, TinyOffer,
    TinySlot, VideoStream, VoiceJoin, DEVICES_LIMIT, OCCUPANTS_LIMIT,
};
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long mediad has to answer a request.
const REPLY: Duration = Duration::from_secs(5);
/// How long mediad has to say it is ready, which is its first act. Past this the engine is
/// killed and the call fails: a helper that never speaks must not hold the one call slot, which
/// every Status answer and every ring waits on.
pub const READY: Duration = Duration::from_secs(10);
/// How long the network has to hand out a voice session after the user pressed Call.
const SESSION: Duration = Duration::from_secs(10);
/// How long mediad has to connect once it holds the session. mediad bounds its own tries (three
/// of three seconds, with their pauses: under ten in all) and says why itself; this is for a
/// mediad that hangs.
const CONNECT: Duration = Duration::from_secs(15);
/// How long an outgoing ring lasts when the network never says how it ended.
const RING: Duration = Duration::from_secs(60);
/// How often a call's deadlines are looked at. Only while a mediad is running.
const WATCH: Duration = Duration::from_millis(250);
/// How long a mediad started ahead of a call waits for one before it goes.
const WARM_FOR: Duration = Duration::from_secs(10 * 60);
/// A join that takes longer than this from the click to media is logged as slow, with where the
/// time went.
const SLOW_JOIN: Duration = Duration::from_secs(3);
/// Speakers remembered ahead of the roster that names them.
const TALKING: usize = 64;
/// A roster change this soon after joining is the roster arriving, not someone joining.
const SETTLING: i64 = 2;

/// The one sentence for a ring nobody took. The dock says it with the person's name.
pub const NO_ANSWER: &str = "Nobody picked up.";
/// What the dock shows when mediad is gone or mute; what mediad or the system said is logged.
const ENGINE_STOPPED: &str = "The call engine stopped.";
const ENGINE_SILENT: &str = "The call engine stopped answering. Try again.";
const ENGINE_MISSING: &str =
    "The call engine could not start. Reinstall tinyclient, then try again.";
/// A Tiny server call asked for while another network's call is up.
pub const IN_ANOTHER_CALL: &str = "Leave the call you're in first.";

/// Why starting or asking the call engine failed. Debug, so a test can name one.
#[derive(Debug)]
pub enum CallError {
    Active,
    NoCall,
    Media(io::Error),
}

/// A call and what is known about it beyond what the shell is told.
struct Live {
    intent: u64,
    call: Call,
    /// Who mediad says is speaking, by user id. The roster comes from the adapter and may name
    /// them only later.
    talking: Vec<String>,
    /// When the state last changed, or the voice session was handed over: what deadlines run from.
    since: Instant,
    /// mediad holds the voice session and is connecting.
    joining: bool,
    /// mediad said the voice connection is up.
    connected: bool,
    /// The adapter said the other side of a ring we placed picked up.
    answered: bool,
    /// The shell has the device menu open, so levels are passed on.
    metering: bool,
    /// Our own user id in the voice session, to tell our own speaking from everyone else's.
    me: String,
    /// The room requires push-to-talk (a Tiny room's `ptt_only`, or no voice activation
    /// there), whatever the user chose.
    push_to_talk: bool,
    /// A Tiny room's bitrate ceiling in kb/s, when known. Quality presets are applied against it.
    room_bitrate: Option<u32>,
    /// How mediad last said the call is protected, before whom the user verified is applied.
    secured: Option<protocol::CallSecurity>,
    /// A call has this mediad (`ActiveCall::adopt`, or a mediad started for one). Until then it
    /// is an engine waiting: what it says (its device list, again at every hot-plug) is learned
    /// but never drawn or announced as a call.
    adopted: bool,
}

/// What is said to a mediad and what it answers, one request and reply at a time. It has a lock
/// of its own so that asking mediad never holds the slot: a slow mediad must not hold every
/// Status answer and every ring with it.
struct Pipe {
    stdin: Option<Box<dyn std::io::Write + Send>>,
    /// Replies to requests, passed on by the thread that reads mediad's stdout.
    replies: Receiver<MediaResponse>,
}

/// The way to ask a running mediad, held without the slot's lock.
#[derive(Clone)]
struct Handle {
    pipe: Arc<Mutex<Pipe>>,
    live: Arc<Mutex<Live>>,
}

/// The running mediad and the call it exists for. Dropping it ends the call.
struct ActiveCall {
    /// Tells this call from the next one, for the threads that outlive it.
    id: u64,
    child: MediaChild,
    pipe: Arc<Mutex<Pipe>>,
    /// The call as the shell sees it, also written by that thread as events arrive.
    live: Arc<Mutex<Live>>,
}

/// A mediad started before any call asked for it, with the session's settings already applied:
/// it holds no device, no camera and no connection until a join gives it a call. It leaves with
/// chatd (its input closes), or after [`WARM_FOR`] unused.
struct Warm {
    active: ActiveCall,
    /// The settings it was given, so a call sends only what has changed since.
    applied: Vec<MediaRequest>,
    /// When it was started, or last asked for again.
    since: Instant,
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// A call with no mediad: an incoming ring, or a failure waiting to be dismissed.
    Shown(Box<Live>),
    Active(ActiveCall),
}

/// Told the conversation of a call that just failed.
type Failed = Box<dyn Fn(&Key) + Send + Sync>;
/// Told a frame mediad has for the network's socket, and the conversation of its call.
type Signal = Box<dyn Fn(&Key, String) + Send + Sync>;

/// At most one call, across every network.
#[derive(Default)]
pub struct Calls {
    slot: Mutex<Slot>,
    /// An incoming ring that arrived while a call is already up. It has no mediad, and it does
    /// not replace the call that is up.
    aside: Mutex<Option<Call>>,
    /// The outgoing ring `place` is watching. A later one does not fail the earlier.
    placed: AtomicU64,
    next: AtomicU64,
    failed: Mutex<Option<Failed>>,
    signal: Mutex<Option<Signal>>,
    /// Who sends a Tiny renegotiation offer. The same shape as `signal`.
    offer: Mutex<Option<Signal>>,
    /// Tiny media dropped, or came back. The live socket resumes the call; the same shape as `failed`.
    tiny_down: Mutex<Option<Failed>>,
    tiny_up: Mutex<Option<Failed>>,
    /// The mediad started for Settings' microphone test or test sound, outside a call.
    tester: Mutex<Option<Tester>>,
    /// A mediad started ahead of the next call (`prewarm`).
    warm: Mutex<Option<Warm>>,
    /// A mediad is being started ahead of a call; another is not started beside it.
    warming: AtomicBool,
    /// Where the time of the join being made goes (`clicked`, `stage`).
    timeline: Mutex<Option<Timeline>>,
}

/// The stages of one join, from the click. Only their names and times are kept.
struct Timeline {
    conversation: Key,
    clicked: Instant,
    stages: Vec<(&'static str, u64)>,
}

/// A mediad with no call (`mediad test`): it opens the microphone while the meter is on and the
/// speaker while a cue plays, and exits by itself once neither has happened for a moment, or
/// after 30 s of metering. Nothing waits on its replies.
struct Tester {
    id: u64,
    child: MediaChild,
    stdin: Option<Box<dyn std::io::Write + Send>>,
}

impl Tester {
    fn start(hub: &Arc<Hub>, id: u64) -> io::Result<Tester> {
        let path = mediad_path()?;
        let mut command = Command::new(&path);
        command.arg("test");
        protocol::launch::no_console(&mut command);
        protocol::log_policy::share_session(&mut command);
        // The process on a desktop; mediad's threads in this process on a phone.
        let protocol::media_host::Spawned { child, stdin, stdout } = protocol::media_host::spawn(&mut command)?;
        let mut stdout = BufReader::new(stdout.expect("stdout is piped"));
        let reader = hub.clone();
        std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            // Ends when mediad exits.
            while let Ok(frame) = read_frame::<MediaResponse>(&mut stdout) {
                if let MediaResponse::Levels { input, .. } = frame {
                    reader.changed(Change::Meter { level: Some(input.min(100)) });
                }
            }
            reader.calls.tester_gone(&reader, id);
        })?;
        Ok(Tester { id, child, stdin })
    }

    fn send(&mut self, request: &MediaRequest) -> io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| io::Error::other(ENGINE_SILENT))?;
        write_frame(stdin, request)
    }
}

impl Drop for Tester {
    fn drop(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) {
            if Instant::now() > deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            sleep(Duration::from_millis(5));
        }
    }
}

/// `TINYCLIENT_MEDIAD`, or the `mediad` next to this binary: in `target/`, and in
/// `tinyclient.app/Contents/MacOS`, where `packaging/bundle.sh` puts every executable so that
/// each finds the others this way. A bundle that keeps helpers apart has them in
/// `Contents/Helpers`, which is looked in second.
pub(crate) fn mediad_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("TINYCLIENT_MEDIAD") {
        return Ok(PathBuf::from(path));
    }
    let beside = std::env::current_exe()?.with_file_name(format!("mediad{}", std::env::consts::EXE_SUFFIX));
    let helper = beside.parent().and_then(|dir| dir.parent()).map(|contents| contents.join(format!("Helpers/mediad{}", std::env::consts::EXE_SUFFIX)));
    Ok(helper.filter(|helper| !beside.exists() && helper.exists()).unwrap_or(beside))
}

fn new_call(conversation: Key, state: CallState) -> Live {
    static NEXT_INTENT: AtomicU64 = AtomicU64::new(1);
    let call = Call {
        conversation,
        state,
        participants: Vec::new(),
        muted: false,
        deafened: false,
        joined: 0,
        input: String::new(),
        output: String::new(),
        engine_linked: false,
        video: Vec::new(),
        speaking: false,
        level: None,
        output_level: None,
        input_id: String::new(),
        output_id: String::new(),
        device_notice: None,
        device_stuck: false,
        inputs: Vec::new(),
        outputs: Vec::new(),
        security: None,
        picture: None,
        self_view: None,
        share_view: None,
    };
    Live {
        intent: NEXT_INTENT.fetch_add(1, Ordering::Relaxed),
        call,
        talking: Vec::new(),
        since: Instant::now(),
        joining: false,
        connected: false,
        answered: false,
        metering: false,
        me: String::new(),
        push_to_talk: false,
        room_bitrate: None,
        secured: None,
        adopted: false,
    }
}

/// How the microphone opens: the user's choice, unless the room requires push-to-talk.
fn input_mode(settings: &protocol::settings::Values, push_to_talk: bool) -> protocol::InputMode {
    if push_to_talk {
        settings.push_to_talk()
    } else {
        settings.input_mode()
    }
}

/// The key a mediad started ahead of any call carries (`warm_up`).
fn nobody() -> Key {
    Key { network: protocol::Network::Tiny, account: String::new(), id: String::new() }
}

fn conversation_is_nobody(key: &Key) -> bool {
    *key == nobody()
}

/// Whether a call may start in `slot`: for a direct call, only from the ring that is showing,
/// and never over a mediad that is running.
fn admit(slot: &mut Slot, conversation: &Key, intent: Option<u64>) -> Result<(), CallError> {
    if intent.is_some()
        && !matches!(&*slot, Slot::Shown(live)
            if Some(live.intent) == intent && live.call.conversation == *conversation && !matches!(live.call.state, CallState::Failed { .. }))
    {
        return Err(CallError::NoCall);
    }
    if slot.running().is_some() {
        return Err(CallError::Active);
    }
    Ok(())
}

/// What a call starts with, from the user's settings. The order is the order it is told.
fn session_requests(settings: &protocol::settings::Values) -> Vec<MediaRequest> {
    let (input, output, camera) = (settings.device(INPUT), settings.device(OUTPUT), settings.device(CAMERA));
    let mut wanted = vec![
        MediaRequest::Tune(settings.tuning()),
        MediaRequest::Input(input_mode(settings, false)),
        MediaRequest::Processing(settings.processing()),
        MediaRequest::Quality(settings.voice_quality()),
        MediaRequest::Devices { input, output, camera },
    ];
    if let Some(guard) = protocol::log_policy::current() {
        wanted.push(MediaRequest::LogLevel { level: guard.policy().level_word().to_owned() });
    }
    wanted
}

/// Tells a mediad `wanted`. A mediad that already has `applied` from before is told only what is
/// different now; a new one is not told what it starts with anyway (the automatic devices, the
/// log level it was started under). One that refuses a setting still carries the call.
fn send_session(handle: &Handle, wanted: &[MediaRequest], applied: Option<&[MediaRequest]>) {
    for request in wanted {
        let had = applied.is_some_and(|applied| applied.contains(request));
        let default = applied.is_none()
            && matches!(request, MediaRequest::LogLevel { .. } | MediaRequest::Devices { input: None, output: None, camera: None });
        if !had && !default {
            let _ = handle.request(request);
        }
    }
}

fn state_name(state: &CallState) -> &'static str {
    match state {
        CallState::RingingOut => "ringing_out",
        CallState::RingingIn { .. } => "ringing_in",
        CallState::Connecting => "connecting",
        CallState::Joined => "joined",
        CallState::Reconnecting => "reconnecting",
        CallState::Failed { .. } => "failed",
    }
}

fn failed(reason: CallFailure, detail: impl Into<String>) -> CallState {
    CallState::Failed {
        reason,
        detail: detail.into(),
    }
}

impl Live {
    fn enter(&mut self, state: CallState) {
        if self.call.state != state {
            tracing::info!(
                network = self.call.conversation.network.label(),
                from = state_name(&self.call.state),
                to = state_name(&state),
                "call state"
            );
            self.call.state = state;
            self.since = Instant::now();
        }
    }

    /// In the call: media is up, and on a call we rang somebody has answered.
    fn join(&mut self) {
        self.enter(CallState::Joined);
        if self.call.joined == 0 {
            self.call.joined = now();
        }
    }

    /// An outgoing ring is answered when the adapter says so, or when someone else is in the
    /// call. Until then our own voice connection being up does not make it a call.
    fn unanswered(&self) -> bool {
        self.call.state == CallState::RingingOut && !self.answered && self.call.participants.is_empty()
    }

    /// The other side picked up. It is a call once our media is up too; until then it is
    /// connecting, with the deadlines of any join.
    fn answer(&mut self) {
        self.answered = true;
        if self.call.state != CallState::RingingOut {
            return;
        }
        if self.connected {
            self.join();
        } else {
            self.enter(CallState::Connecting);
        }
    }

    fn speaking(&mut self, user_id: &str, speaking: bool) -> bool {
        if !self.me.is_empty() && user_id == self.me {
            let changed = self.call.speaking != speaking;
            self.call.speaking = speaking;
            return changed;
        }
        self.talking.retain(|id| id != user_id);
        if speaking && self.talking.len() < TALKING {
            self.talking.push(user_id.to_owned());
        }
        match self.call.participants.iter_mut().find(|p| p.id == user_id) {
            Some(who) if who.speaking != speaking => {
                who.speaking = speaking;
                true
            }
            _ => false,
        }
    }

    /// An event from mediad, taken in. The call to draw, when the event changes it and a call
    /// has this mediad; an engine waiting for one draws and announces nothing.
    fn hear(&mut self, event: MediaResponse) -> Option<Call> {
        let draw = self.apply(event);
        (draw && self.adopted).then(|| self.call.clone())
    }

    /// What an event from mediad does to the call. False when it changes nothing the shell draws.
    fn apply(&mut self, event: MediaResponse) -> bool {
        match event {
            MediaResponse::Connected { .. } => {
                self.connected = true;
                if self.unanswered() || self.call.state == CallState::Joined {
                    return false;
                }
                self.join();
            }
            // Only a call that was up can be interrupted. While it still rings or connects,
            // mediad changing voice server is part of getting there.
            MediaResponse::Interrupted if self.call.state == CallState::Joined => self.enter(CallState::Reconnecting),
            MediaResponse::Interrupted => return false,
            MediaResponse::Failed { reason, detail } => self.enter(CallState::Failed { reason, detail }),
            MediaResponse::Speaking { user_id, speaking } => return self.speaking(&user_id, speaking),
            // The roster is the adapter's to change. Someone who left is no longer speaking.
            MediaResponse::Gone { user_id } => return self.speaking(&user_id, false),
            MediaResponse::Devices { mut input, mut output, mut input_id, mut output_id, notice, stuck } => {
                for text in [&mut input, &mut output, &mut input_id, &mut output_id] {
                    clip(text, 400);
                }
                let notice = notice.map(|mut words| {
                    clip(&mut words, 600);
                    words
                });
                let call = &self.call;
                if (&call.input, &call.output, &call.input_id, &call.output_id, &call.device_notice, call.device_stuck) == (&input, &output, &input_id, &output_id, &notice, stuck) {
                    return false;
                }
                self.call.input = input;
                self.call.output = output;
                self.call.input_id = input_id;
                self.call.output_id = output_id;
                self.call.device_notice = notice;
                self.call.device_stuck = stuck;
            }
            // Levels are for the device menu's meters, and stop here unless it is open.
            MediaResponse::Levels { input, output } => {
                let levels = (Some(input.min(100)), Some(output.min(100)));
                if !self.metering || (self.call.level, self.call.output_level) == levels {
                    return false;
                }
                (self.call.level, self.call.output_level) = levels;
            }
            MediaResponse::DeviceList { inputs, outputs } => {
                let bounded = |mut devices: Vec<AudioDevice>| {
                    devices.truncate(DEVICES_LIMIT);
                    for device in &mut devices {
                        clip(&mut device.name, 200);
                        clip(&mut device.id, 400);
                    }
                    devices
                };
                let (inputs, outputs) = (bounded(inputs), bounded(outputs));
                if (&self.call.inputs, &self.call.outputs) == (&inputs, &outputs) {
                    return false;
                }
                self.call.inputs = inputs;
                self.call.outputs = outputs;
            }
            // Whom the user verified is chatd's to know; a call that is not a Tiny one never
            // hears this.
            MediaResponse::TinySecurity(security) => {
                let account = self.call.conversation.account();
                self.secured = Some(security.clone());
                let security = Some(crate::verified::trusted(&account, security));
                if self.call.security == security {
                    return false;
                }
                self.call.security = security;
            }
            MediaResponse::Source { .. } | MediaResponse::TinyE2ee { .. } | MediaResponse::TinyRenegotiate { .. } => return false,
            MediaResponse::Picture { path, width, height, generation, user_id, kind } => {
                let picture = (!path.is_empty()).then(|| CallPicture { path, width, height, generation, user_id, kind });
                if self.call.picture == picture {
                    return false;
                }
                self.call.picture = picture;
            }
            MediaResponse::SelfView { path, width, height, generation, share } => {
                let kind = if share { PictureKind::Share } else { PictureKind::Camera };
                let picture = (!path.is_empty()).then(|| CallPicture { path, width, height, generation, user_id: String::new(), kind });
                let slot = if share { &mut self.call.share_view } else { &mut self.call.self_view };
                if *slot == picture {
                    return false;
                }
                *slot = picture;
            }
            MediaResponse::Sending { share, camera } => {
                let mut video: Vec<VideoStream> = self.call.video.iter().filter(|stream| stream.direction != Direction::Encode).cloned().collect();
                if camera {
                    video.insert(0, VideoStream { direction: Direction::Encode, content: Content::Camera, source: String::new() });
                }
                if share {
                    video.insert(0, VideoStream { direction: Direction::Encode, content: Content::Share, source: String::new() });
                }
                // My preview of a share that is no longer sent goes with it.
                let dropped = !share && self.call.share_view.take().is_some();
                if video == self.call.video && !dropped {
                    return false;
                }
                self.call.video = video;
            }
            MediaResponse::Notice { detail } => {
                let notice = Some(detail);
                if self.call.device_notice == notice {
                    return false;
                }
                self.call.device_notice = notice;
            }
            MediaResponse::Ready { .. }
            | MediaResponse::Streams { .. }
            | MediaResponse::Done
            | MediaResponse::Offer { .. }
            | MediaResponse::Stats(_)
            | MediaResponse::Refused { .. }
            | MediaResponse::LogsSaved { .. } => return false,
        }
        true
    }

    /// The failure a deadline has turned this call into, if one has passed.
    fn overdue(&self) -> Option<CallState> {
        let network = self.call.conversation.network.label();
        let waited = self.since.elapsed();
        match self.call.state {
            CallState::Connecting if self.joining && waited > CONNECT => Some(failed(CallFailure::Engine, ENGINE_SILENT)),
            CallState::Connecting if !self.joining && waited > SESSION => {
                let seconds = SESSION.as_secs();
                Some(failed(CallFailure::Lost, format!("No reply from {network} after {seconds} seconds.")))
            }
            CallState::RingingOut if waited > RING => Some(failed(CallFailure::NoAnswer, NO_ANSWER)),
            _ => None,
        }
    }
}

impl ActiveCall {
    fn start(hub: &Arc<Hub>, id: u64, conversation: Key) -> io::Result<ActiveCall> {
        let path = mediad_path()?;
        let mut command = Command::new(&path);
        command.arg("1");
        protocol::launch::no_console(&mut command);
        protocol::log_policy::share_session(&mut command);
        // The process on a desktop; mediad's threads in this process on a phone.
        let spawned = protocol::media_host::spawn(&mut command);
        let protocol::media_host::Spawned {
            mut child,
            stdin,
            stdout,
        } = spawned.map_err(|err| {
            tracing::warn!("chatd: could not run {}: {err}", path.display());
            io::Error::other(ENGINE_MISSING)
        })?;
        let mut stdout = BufReader::new(stdout.expect("stdout is piped"));
        // Ready is the first thing a healthy mediad says. The read waits on a thread of its own,
        // for `Calls::start` holds the one call slot across this: a helper that never speaks (a
        // stale binary, one wedged before its first word) must fail the call rather than hold
        // every Status answer and every ring with it.
        let (said, answered) = channel();
        let spawned = std::thread::Builder::new()
            .name("mediad-ready".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let frame = read_frame::<MediaResponse>(&mut stdout);
                let _ = said.send((frame, stdout));
            });
        if let Err(err) = spawned {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!(
                "chatd: the reader of {} did not start: {err}",
                path.display()
            );
            return Err(io::Error::other(ENGINE_MISSING));
        }
        let (frame, mut stdout) = match answered.recv_timeout(READY) {
            Ok(arrived) => arrived,
            answer => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("mediad frame was not ready");
                let silent = matches!(answer, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
                return Err(io::Error::other(if silent {
                    ENGINE_SILENT
                } else {
                    ENGINE_MISSING
                }));
            }
        };
        let engine_linked = match frame {
            Ok(MediaResponse::Ready { engine_linked }) => engine_linked,
            other => {
                let _ = child.kill();
                let _ = child.wait();
                let variant = match &other {
                    Ok(frame) => frame.name(),
                    Err(_) => "error",
                };
                tracing::warn!(variant, "mediad frame was {variant}");
                return Err(io::Error::other(ENGINE_MISSING));
            }
        };
        let mut live = new_call(conversation, CallState::Connecting);
        live.call.engine_linked = engine_linked;
        // A mediad started ahead of any call is started with no conversation (`warm_up`).
        live.adopted = !conversation_is_nobody(&live.call.conversation);
        let live = Arc::new(Mutex::new(live));
        let (reply, replies) = channel();

        let (reader, seen) = (hub.clone(), live.clone());
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                // Ends when mediad closes its stdout, which is when it exits.
                while let Ok(frame) = read_frame::<MediaResponse>(&mut stdout) {
                    if frame.is_reply() {
                        let _ = reply.send(frame);
                        continue;
                    }
                    if let MediaResponse::DeviceList { inputs, outputs } = &frame {
                        learn_devices(&reader, inputs, outputs);
                    }
                    // A frame for the server: the adapter of the call's account sends it.
                    if let MediaResponse::TinyE2ee { frame } = &frame {
                        let conversation = crate::hub::lock(&seen).call.conversation.clone();
                        reader.calls.signal(&conversation, frame.clone());
                        continue;
                    }
                    if let MediaResponse::TinyRenegotiate { sdp } = &frame {
                        let conversation = crate::hub::lock(&seen).call.conversation.clone();
                        reader.calls.offer(&conversation, sdp.clone());
                        continue;
                    }
                    let notify = match &frame {
                        MediaResponse::Interrupted => Some(false),
                        MediaResponse::Connected { .. } => Some(true),
                        _ => None,
                    };
                    let mut live = crate::hub::lock(&seen);
                    let conversation = live.call.conversation.clone();
                    let call = live.hear(frame);
                    let adopted = live.adopted;
                    let state_now = live.call.state.clone();
                    drop(live);
                    // Outside the call lock. A drop while already reconnecting does not redraw, and
                    // still has to be resumed; the first connect is not one.
                    if adopted && conversation.network == protocol::Network::Tiny {
                        match notify {
                            Some(false) if matches!(state_now, CallState::Reconnecting) => {
                                reader.calls.tiny_down(&conversation)
                            }
                            Some(true) => {
                                reader.calls.joined(&conversation);
                                reader.calls.tiny_up(&conversation)
                            }
                            _ => {}
                        }
                    }
                    if let Some(call) = call {
                        reader.changed(Change::Call {
                            call: Some(call),
                            ring: reader.calls.aside(),
                        });
                    }
                }
                reader.calls.reap(&reader, id);
            })?;

        Ok(ActiveCall {
            id,
            child,
            pipe: Arc::new(Mutex::new(Pipe { stdin, replies })),
            live,
        })
    }

    /// Starts the clock on this call's deadlines. Only a call in the slot has any.
    fn watch(hub: &Arc<Hub>, id: u64) {
        let watcher = hub.clone();
        let spawned = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            // Ends with the call it watches.
            while watcher.calls.watch(&watcher, id) {
                sleep(WATCH);
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("chatd: the call's clock did not start: {err}");
        }
    }

    /// False once mediad has exited on its own.
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn handle(&self) -> Handle {
        Handle { pipe: self.pipe.clone(), live: self.live.clone() }
    }

    /// The call this mediad was started ahead of is now `conversation`'s. What it learned of
    /// the system's devices stays; everything else about the call starts afresh.
    fn adopt(&self, conversation: Key, intent: Option<u64>, muted: bool) {
        let mut live = crate::hub::lock(&self.live);
        let (engine_linked, inputs, outputs) = (live.call.engine_linked, std::mem::take(&mut live.call.inputs), std::mem::take(&mut live.call.outputs));
        *live = new_call(conversation, CallState::Connecting);
        live.adopted = true;
        live.call.engine_linked = engine_linked;
        (live.call.inputs, live.call.outputs) = (inputs, outputs);
        live.call.muted = muted;
        if let Some(intent) = intent {
            live.intent = intent;
        }
    }
}

impl Handle {
    fn request(&self, request: &MediaRequest) -> io::Result<MediaResponse> {
        let mut pipe = crate::hub::lock(&self.pipe);
        let stdin = pipe.stdin.as_mut().ok_or_else(|| io::Error::other(ENGINE_SILENT))?;
        write_frame(stdin, request)?;
        let reply = pipe.replies.recv_timeout(REPLY).map_err(|_| io::Error::other(ENGINE_SILENT))?;
        drop(pipe);
        match reply {
            MediaResponse::Streams { video } => {
                crate::hub::lock(&self.live).call.video = video.clone();
                Ok(MediaResponse::Streams { video })
            }
            MediaResponse::Refused { detail } => Err(io::Error::other(detail)),
            reply => Ok(reply),
        }
    }
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        // Closing stdin is the hang-up. Kill only if mediad ignores it.
        crate::hub::lock(&self.pipe).stdin.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.alive() {
            if Instant::now() > deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            sleep(Duration::from_millis(5));
        }
    }
}

impl Slot {
    /// The call's mediad, if there is one and it is still running.
    fn running(&mut self) -> Option<&mut ActiveCall> {
        let Slot::Active(active) = self else {
            return None;
        };
        active.alive().then_some(active)
    }

    /// The way to ask that mediad, to be used after the slot is let go of.
    fn handle(&mut self) -> Option<Handle> {
        self.running().map(|active| active.handle())
    }

    /// A call that failed needs no mediad: the failure stays, the process goes. Gives the mediad
    /// to drop, which waits for it to exit and so must happen outside the lock.
    fn demote(&mut self) -> Option<ActiveCall> {
        let Slot::Active(active) = self else {
            return None;
        };
        let mut call = crate::hub::lock(&active.live).call.clone();
        if !matches!(call.state, CallState::Failed { .. }) {
            return None;
        }
        // Nobody is heard on a call that is over.
        call.speaking = false;
        call.level = None;
        let mut shown = new_call(call.conversation.clone(), call.state.clone());
        shown.call = call;
        match std::mem::replace(self, Slot::Shown(Box::new(shown))) {
            Slot::Active(active) => Some(active),
            _ => unreachable!("the slot was active"),
        }
    }
}

impl Calls {
    /// The call as the shell should see it now, and whether mediad is running. A mediad that has
    /// just exited is settled by the thread that reads it, once it has read its last word: here
    /// the call is only reported.
    pub fn describe(&self) -> (Option<Call>, bool) {
        match &mut *crate::hub::lock(&self.slot) {
            Slot::Idle => (None, false),
            Slot::Shown(live) => (Some(live.call.clone()), false),
            Slot::Active(active) => {
                let running = active.alive();
                (Some(crate::hub::lock(&active.live).call.clone()), running)
            }
        }
    }

    fn tell(&self, hub: &Hub) {
        hub.changed(Change::Call {
            call: self.describe().0,
            ring: self.aside(),
        });
    }

    /// The ring waiting beside the call, if someone called while one was up.
    pub fn aside(&self) -> Option<Call> {
        crate::hub::lock(&self.aside).clone()
    }

    /// Who hears of a call that failed: the server, which has the adapter leave on the network.
    pub fn on_failure(&self, told: impl Fn(&Key) + Send + Sync + 'static) {
        *crate::hub::lock(&self.failed) = Some(Box::new(told));
    }

    /// Who sends what mediad has for a network's socket: the server, through the adapter.
    pub fn on_signal(&self, told: impl Fn(&Key, String) + Send + Sync + 'static) {
        *crate::hub::lock(&self.signal) = Some(Box::new(told));
    }

    /// Who sends a renegotiation offer (`call.offer`) for the call's account.
    pub fn on_offer(&self, told: impl Fn(&Key, String) + Send + Sync + 'static) {
        *crate::hub::lock(&self.offer) = Some(Box::new(told));
    }

    /// Who hears that a Tiny call's media dropped, so the live socket can join again.
    pub fn on_tiny_down(&self, told: impl Fn(&Key) + Send + Sync + 'static) {
        *crate::hub::lock(&self.tiny_down) = Some(Box::new(told));
    }

    /// Who hears that a Tiny call's media is flowing again.
    pub fn on_tiny_up(&self, told: impl Fn(&Key) + Send + Sync + 'static) {
        *crate::hub::lock(&self.tiny_up) = Some(Box::new(told));
    }

    fn signal(&self, conversation: &Key, frame: String) {
        if let Some(told) = &*crate::hub::lock(&self.signal) {
            told(conversation, frame);
        }
    }

    fn offer(&self, conversation: &Key, sdp: String) {
        if let Some(told) = &*crate::hub::lock(&self.offer) {
            told(conversation, sdp);
        }
    }

    fn tiny_down(&self, conversation: &Key) {
        if let Some(told) = &*crate::hub::lock(&self.tiny_down) {
            told(conversation);
        }
    }

    fn tiny_up(&self, conversation: &Key) {
        if let Some(told) = &*crate::hub::lock(&self.tiny_up) {
            told(conversation);
        }
    }

    /// The user verified someone, or took it back: the call on `account` shows it at once.
    pub fn reverify(&self, hub: &Hub, account: &protocol::AccountKey) {
        let Some(call) = self.describe().0.filter(|call| call.conversation.account() == *account) else {
            return;
        };
        let refreshed = self.update(hub, &call.conversation, |live| {
            live.call.security = live.secured.clone().map(|security| crate::verified::trusted(account, security));
        });
        if refreshed {
            self.tell(hub);
        }
    }

    /// A call with a mediad just turned into a failure. Drops the mediad, which waits for it to
    /// exit, and says so; both outside the slot's lock.
    fn stopped(&self, hub: &Hub, stopped: Option<ActiveCall>) {
        let conversation = stopped.as_ref().map(|active| crate::hub::lock(&active.live).call.conversation.clone());
        drop(stopped);
        self.tell(hub);
        if let Some(conversation) = conversation {
            self.gave_up(&conversation);
        }
    }

    fn gave_up(&self, conversation: &Key) {
        if let Some(told) = &*crate::hub::lock(&self.failed) {
            told(conversation);
        }
    }

    /// Starts mediad for a call in `conversation`. A ring or a failure being shown gives way.
    pub fn start(&self, hub: &Arc<Hub>, conversation: Key) -> Result<(), CallError> {
        self.start_with_intent(hub, conversation, None)
    }

    fn start_with_intent(&self, hub: &Arc<Hub>, conversation: Key, intent: Option<u64>) -> Result<(), CallError> {
        // A microphone test gives way: the call needs the devices.
        let tester = crate::hub::lock(&self.tester).take();
        if tester.is_some() {
            drop(tester);
            hub.changed(Change::Meter { level: None });
        }
        // The slot is looked at, and let go of: starting mediad and telling it the user's
        // settings are slow, and nothing else may wait on the slot meanwhile.
        admit(&mut crate::hub::lock(&self.slot), &conversation, intent)?;
        let (wanted, muted) = {
            let settings = hub.settings();
            (session_requests(&settings), settings.on(JOIN_MUTED))
        };
        // A mediad started ahead of the call has its settings already; a cold start waits for
        // the process.
        let (active, applied) = match self.take_warm_waiting() {
            Some(warm) => (warm.active, Some(warm.applied)),
            None => {
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                (ActiveCall::start(hub, id, conversation.clone()).map_err(CallError::Media)?, None)
            }
        };
        active.adopt(conversation.clone(), intent, muted);
        self.stage(&conversation, if applied.is_some() { "mediad_ready_warm" } else { "mediad_ready_cold" });
        // The user's defaults, before the voice session arrives: mediad opens the devices
        // only once it holds one. A mediad that refuses them still carries the call.
        let handle = active.handle();
        send_session(&handle, &wanted, applied.as_deref());
        self.stage(&conversation, "settings_applied");
        // V9: everyone on this account whose volume for me is not the default.
        let gains = hub.settings().gains(&conversation.account());
        for (user_id, percent, muted) in gains {
            let _ = handle.request(&MediaRequest::Gain { user_id, percent, muted });
        }
        let id = active.id;
        let old = {
            let mut slot = crate::hub::lock(&self.slot);
            // Looked at again: the slot was free a moment ago, not necessarily now.
            admit(&mut slot, &conversation, intent)?;
            std::mem::replace(&mut *slot, Slot::Active(active))
        };
        // The old mediad, if any, is reaped outside the slot's lock: it can take two seconds.
        drop(old);
        ActiveCall::watch(hub, id);
        self.tell(hub);
        Ok(())
    }

    /// The mediad started ahead of this call. One that is still being started is waited for a
    /// moment: it began before this call did, so it is ready before a new one would be.
    fn take_warm_waiting(&self) -> Option<Warm> {
        let deadline = Instant::now() + READY;
        while self.warming.load(Ordering::SeqCst) && Instant::now() < deadline {
            sleep(Duration::from_millis(2));
        }
        self.take_warm()
    }

    /// The mediad started ahead of this call, if one is still running.
    fn take_warm(&self) -> Option<Warm> {
        let mut warm = crate::hub::lock(&self.warm).take()?;
        warm.active.alive().then_some(warm)
    }

    /// The person pressed Join, or answered: the clock of this join starts. Nothing here is
    /// personal: stage names and milliseconds.
    pub fn clicked(&self, conversation: &Key) {
        tracing::info!(network = conversation.network.label(), "call join: clicked");
        *crate::hub::lock(&self.timeline) = Some(Timeline { conversation: conversation.clone(), clicked: Instant::now(), stages: Vec::new() });
    }

    /// One stage of the join in `conversation` is done, `elapsed_ms` after the click. The stages
    /// mediad reaches after this point it logs itself, from its own offer (`mediad.log`).
    fn stage(&self, conversation: &Key, name: &'static str) {
        let mut line = crate::hub::lock(&self.timeline);
        let Some(timeline) = line.as_mut().filter(|timeline| timeline.conversation == *conversation) else {
            return;
        };
        let elapsed_ms = timeline.clicked.elapsed().as_millis() as u64;
        timeline.stages.push((name, elapsed_ms));
        tracing::info!(stage = name, elapsed_ms, "call join");
    }

    /// Media is up: the join is over, and is told as a whole. Slow ones are a warning.
    fn joined(&self, conversation: &Key) {
        let mut line = crate::hub::lock(&self.timeline);
        if !line.as_ref().is_some_and(|timeline| timeline.conversation == *conversation) {
            return;
        }
        let Some(timeline) = line.take() else {
            return;
        };
        drop(line);
        let elapsed = timeline.clicked.elapsed();
        let stages = timeline.stages.iter().map(|(name, ms)| format!("{name}={ms}")).collect::<Vec<_>>().join(" ");
        let elapsed_ms = elapsed.as_millis() as u64;
        if elapsed > SLOW_JOIN {
            tracing::warn!(stage = "connected", elapsed_ms, stages = stages.as_str(), "call join was slow");
        } else {
            tracing::info!(stage = "connected", elapsed_ms, stages = stages.as_str(), "call join");
        }
    }

    /// Starts a mediad ahead of the next call, so that joining finds one running with the
    /// session's settings applied: a ring in or out, or the shell opening a place that can call.
    /// It holds no sound device, camera or connection, and goes when chatd does or after
    /// [`WARM_FOR`] unused. Without a mediad to start, on a phone (where mediad is part of this
    /// process), or with `TINYCLIENT_PREWARM=off`, it does nothing. Returns at once.
    pub fn prewarm(&self, hub: &Arc<Hub>) {
        if protocol::media_host::in_process()
            || std::env::var("TINYCLIENT_PREWARM").is_ok_and(|value| value == "off")
            || !mediad_path().is_ok_and(|path| path.exists())
        {
            return;
        }
        {
            let mut warm = crate::hub::lock(&self.warm);
            if let Some(warm) = warm.as_mut() {
                if warm.active.alive() {
                    warm.since = Instant::now();
                    return;
                }
            }
        }
        if self.warming.swap(true, Ordering::SeqCst) {
            return;
        }
        let starter = hub.clone();
        let spawned = std::thread::Builder::new().name("mediad-warm".into()).stack_size(256 * 1024).spawn(move || {
            starter.calls.warm_up(&starter);
            starter.calls.warming.store(false, Ordering::SeqCst);
        });
        if let Err(err) = spawned {
            self.warming.store(false, Ordering::SeqCst);
            tracing::warn!("chatd: the call engine was not started ahead of a call: {err}");
        }
    }

    fn warm_up(&self, hub: &Arc<Hub>) {
        // A call has its own mediad, and one test at a time holds the devices.
        if crate::hub::lock(&self.slot).running().is_some() {
            return;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let active = match ActiveCall::start(hub, id, nobody()) {
            Ok(active) => active,
            Err(err) => {
                tracing::warn!("chatd: the call engine was not started ahead of a call: {err}");
                return;
            }
        };
        let wanted = session_requests(&hub.settings());
        send_session(&active.handle(), &wanted, None);
        tracing::info!(elapsed_ms = started.elapsed().as_millis() as u64, "call engine ready ahead of a call");
        let old = crate::hub::lock(&self.warm).replace(Warm { active, applied: wanted, since: Instant::now() });
        drop(old);
        // Nobody asked for it in a while: it goes.
        let keeper = hub.clone();
        let _ = std::thread::Builder::new().name("mediad-warm-clock".into()).stack_size(128 * 1024).spawn(move || loop {
            sleep(WARM_FOR);
            let mut warm = crate::hub::lock(&keeper.calls.warm);
            match warm.as_ref() {
                Some(held) if held.active.id == id && held.since.elapsed() >= WARM_FOR => {
                    let expired = warm.take();
                    drop(warm);
                    drop(expired);
                    return;
                }
                Some(held) if held.active.id == id => {}
                _ => return,
            }
        });
    }

    /// Whether a call is starting or up with its mediad: a test then uses that mediad.
    fn in_call(&self) -> bool {
        crate::hub::lock(&self.slot).running().is_some()
    }

    /// The tuning changed in Settings: the call's mediad, or the test's, takes it at once.
    pub fn retune(&self, hub: &Hub) {
        let (tuning, settings) = {
            let settings = hub.settings();
            (settings.tuning(), settings.clone())
        };
        let running = crate::hub::lock(&self.slot).handle();
        if let Some(active) = running {
            let push_to_talk = crate::hub::lock(&active.live).push_to_talk;
            let _ = active.request(&MediaRequest::Tune(tuning));
            let _ = active.request(&MediaRequest::Input(input_mode(&settings, push_to_talk)));
            let _ = active.request(&MediaRequest::Processing(settings.processing()));
            let quality = settings.voice_quality();
            let _ = active.request(&MediaRequest::Quality(quality));
            // Re-apply the room bitrate under the new quality cap when we know the room's.
            let room_bitrate = crate::hub::lock(&active.live).room_bitrate;
            if let Some(kbps) = room_bitrate {
                let _ = active.request(&MediaRequest::Bitrate { kbps: quality.apply_room(kbps) });
            }
        }
        if let Some(tester) = crate::hub::lock(&self.tester).as_mut() {
            let _ = tester.send(&MediaRequest::Tune(tuning));
            let _ = tester.send(&MediaRequest::Processing(settings.processing()));
        }
    }

    /// Verbose logging changed: the call's mediad, or the test's, logs at the new ceiling from
    /// its next line. A mediad started later reads it from its environment (`share_session`).
    pub fn relevel(&self) {
        let Some(guard) = protocol::log_policy::current() else {
            return;
        };
        let level = MediaRequest::LogLevel { level: guard.policy().level_word().to_owned() };
        let running = crate::hub::lock(&self.slot).handle();
        if let Some(active) = running {
            let _ = active.request(&level);
        }
        if let Some(tester) = crate::hub::lock(&self.tester).as_mut() {
            let _ = tester.send(&level);
        }
    }

    /// V1 or V2 changed: the call's mediad, or the test's, opens the new choice at once, and
    /// the next call starts with it too.
    pub fn redevice(&self, hub: &Hub) {
        let (input, output, camera) = {
            let settings = hub.settings();
            (settings.device(INPUT), settings.device(OUTPUT), settings.device(CAMERA))
        };
        let devices = MediaRequest::Devices { input, output, camera };
        let running = crate::hub::lock(&self.slot).handle();
        if let Some(active) = running {
            let _ = active.request(&devices);
        }
        if let Some(tester) = crate::hub::lock(&self.tester).as_mut() {
            let _ = tester.send(&devices);
        }
    }

    /// One person's volume for me (V9), for the call if it is on `account`.
    pub fn gain(&self, hub: &Hub, account: &protocol::AccountKey, user_id: String, percent: u16, muted: bool) {
        let (call, running) = self.describe();
        if running && call.is_some_and(|call| call.conversation.account() == *account) {
            let _ = self.request(hub, &MediaRequest::Gain { user_id, percent, muted });
        }
    }

    /// The running test mediad, started with the user's devices and tuning if there is none.
    fn tester<'a>(&self, hub: &Arc<Hub>, held: &'a mut Option<Tester>) -> Result<&'a mut Tester, CallError> {
        if held.is_none() {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let mut tester = Tester::start(hub, id).map_err(|err| {
                tracing::warn!("chatd: the test mediad did not start: {err}");
                CallError::Media(io::Error::other(ENGINE_MISSING))
            })?;
            let (tuning, input, output, processing) = {
                let settings = hub.settings();
                (settings.tuning(), settings.device(INPUT), settings.device(OUTPUT), settings.processing())
            };
            // The meter is of the microphone as a call would send it: processed.
            let told = tester
                .send(&MediaRequest::Tune(tuning))
                .and_then(|()| tester.send(&MediaRequest::Devices { input, output, camera: None }))
                .and_then(|()| tester.send(&MediaRequest::Processing(processing)));
            told.map_err(CallError::Media)?;
            *held = Some(tester);
        }
        Ok(held.as_mut().expect("started above"))
    }

    /// Settings' microphone test. In a call it is the call's own meter; otherwise a test mediad
    /// carries it, and stops it after 30 s by itself. `loopback` also plays the microphone back,
    /// processed, on the speaker; only outside a call, where nobody else is listening.
    pub fn test_microphone(&self, hub: &Arc<Hub>, on: bool, loopback: bool) -> Result<(), CallError> {
        if self.in_call() {
            return self.meter(hub, on);
        }
        let mut held = crate::hub::lock(&self.tester);
        if !on {
            if let Some(tester) = held.as_mut() {
                // Gone already is as good as stopped.
                let _ = tester.send(&MediaRequest::Meter { on: false });
            }
            return Ok(());
        }
        let ask = |tester: &mut Tester| tester.send(&MediaRequest::Meter { on: true }).and_then(|()| tester.send(&MediaRequest::Loopback { on: loopback }));
        let tester = self.tester(hub, &mut held)?;
        if ask(tester).is_err() {
            // It had just exited: start it again.
            *held = None;
            ask(self.tester(hub, &mut held)?).map_err(CallError::Media)?;
        }
        Ok(())
    }

    /// Settings' test sound, on the call speaker: in the call, or from a test mediad.
    pub fn test_sound(&self, hub: &Arc<Hub>) -> Result<(), CallError> {
        let test = MediaRequest::Cue { cue: Cue::Test };
        let running = crate::hub::lock(&self.slot).handle();
        if let Some(active) = running {
            return active.request(&test).map(drop).map_err(CallError::Media);
        }
        let mut held = crate::hub::lock(&self.tester);
        if self.tester(hub, &mut held)?.send(&test).is_err() {
            *held = None;
            self.tester(hub, &mut held)?.send(&test).map_err(CallError::Media)?;
        }
        Ok(())
    }

    /// The test mediad exited. The meter is over.
    fn tester_gone(&self, hub: &Hub, id: u64) {
        let mut held = crate::hub::lock(&self.tester);
        if held.as_ref().is_some_and(|tester| tester.id == id) {
            let gone = held.take();
            drop(held);
            drop(gone);
            hub.changed(Change::Meter { level: None });
        }
    }

    /// An outgoing direct ring, with no mediad. The shell shows it until the other person
    /// answers or a minute passes. A call that is already up is left as it is.
    pub fn place(&self, hub: &Arc<Hub>, conversation: Key) -> Result<u64, CallError> {
        let muted = hub.settings().on(JOIN_MUTED);
        let id;
        {
            let mut slot = crate::hub::lock(&self.slot);
            if slot.running().is_some() {
                return Err(CallError::Active);
            }
            if let Slot::Shown(live) = &*slot {
                if live.call.conversation == conversation
                    && matches!(live.call.state, CallState::RingingOut)
                {
                    return Ok(self.placed.load(Ordering::Relaxed));
                }
            }
            let mut live = new_call(conversation, CallState::RingingOut);
            live.call.muted = muted;
            id = live.intent;
            *slot = Slot::Shown(Box::new(live));
            self.placed.store(id, Ordering::Relaxed);
        }
        self.tell(hub);
        // The other side picking up is likely: the engine is started now, not at the answer.
        self.prewarm(hub);
        let watcher = hub.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("call-ring".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                while watcher.calls.watch_shown(&watcher, id) {
                    sleep(WATCH);
                }
            })
        {
            tracing::warn!("chatd: the ring's clock did not start: {err}");
        }
        Ok(id)
    }

    pub fn call_intent(&self, conversation: &Key) -> Option<u64> {
        let slot = crate::hub::lock(&self.slot);
        let live = match &*slot {
            Slot::Shown(live) => live,
            Slot::Active(active) => {
                let live = crate::hub::lock(&active.live);
                return (live.call.conversation == *conversation && !matches!(live.call.state, CallState::Failed { .. })).then_some(live.intent);
            }
            Slot::Idle => return None,
        };
        (live.call.conversation == *conversation && !matches!(live.call.state, CallState::Failed { .. })).then_some(live.intent)
    }

    pub fn ring_attempt(&self) -> u64 { self.placed.load(Ordering::Relaxed) }

    pub fn fail_ring_attempt(&self, hub: &Hub, conversation: &Key, attempt: u64, reason: CallFailure, detail: &str) {
        self.update(hub, conversation, |live| {
            if live.intent == attempt { live.enter(failed(reason, detail)); }
        });
        self.tell(hub);
    }

    /// A ring we placed, with no mediad, ran out of time. False once that ring is no longer the one shown.
    fn watch_shown(&self, hub: &Hub, id: u64) -> bool {
        if self.placed.load(Ordering::Relaxed) != id {
            return false;
        }
        let conversation = {
            let mut slot = crate::hub::lock(&self.slot);
            let Slot::Shown(live) = &mut *slot else {
                return false;
            };
            if !matches!(live.call.state, CallState::RingingOut) {
                return false;
            }
            let Some(overdue) = live.overdue() else {
                return true;
            };
            let conversation = live.call.conversation.clone();
            live.enter(overdue);
            conversation
        };
        self.tell(hub);
        self.gave_up(&conversation);
        false
    }

    /// A failure that has to be on screen even when no call was up yet. A running call is left alone.
    pub fn show_failed(
        &self,
        hub: &Hub,
        conversation: Key,
        reason: CallFailure,
        detail: impl Into<String>,
    ) {
        let state = failed(reason, detail);
        if self.update(hub, &conversation, |live| live.enter(state.clone())) {
            self.tell(hub);
            return;
        }
        {
            let mut slot = crate::hub::lock(&self.slot);
            if slot.running().is_some() {
                return;
            }
            *slot = Slot::Shown(Box::new(new_call(conversation, state)));
        }
        self.tell(hub);
    }

    /// The incoming ring to answer, promoting one that waited beside a call once that call is gone.
    /// Does not start mediad.
    pub fn incoming(&self, hub: &Hub) -> Option<Key> {
        let promoted = {
            let mut slot = crate::hub::lock(&self.slot);
            if let Slot::Shown(live) = &*slot {
                if matches!(live.call.state, CallState::RingingIn { .. }) {
                    return Some(live.call.conversation.clone());
                }
            }
            if !matches!(*slot, Slot::Idle) {
                return None;
            }
            let ring = crate::hub::lock(&self.aside).take()?;
            if !matches!(ring.state, CallState::RingingIn { .. }) {
                return None;
            }
            let key = ring.conversation.clone();
            let mut live = new_call(key.clone(), ring.state.clone());
            live.call = ring;
            *slot = Slot::Shown(Box::new(live));
            key
        };
        self.tell(hub);
        Some(promoted)
    }

    /// Declines only the ring that is waiting beside the call. The call that is up stays up.
    pub fn decline_waiting(&self, hub: &Hub) -> Option<Key> {
        let key = {
            let mut aside = crate::hub::lock(&self.aside);
            let ring = aside.as_ref()?;
            if !matches!(ring.state, CallState::RingingIn { .. }) {
                return None;
            }
            aside.take().map(|ring| ring.conversation)
        }?;
        self.tell(hub);
        Some(key)
    }

    /// Answers the ring. Gives the conversation, for the adapter to join on the network.
    pub fn accept(&self, hub: &Arc<Hub>) -> Result<Key, CallError> {
        let ringing = match &*crate::hub::lock(&self.slot) {
            Slot::Shown(live) if matches!(live.call.state, CallState::RingingIn { .. }) => live.call.conversation.clone(),
            _ => return Err(CallError::NoCall),
        };
        if let Err(err) = self.start(hub, ringing.clone()) {
            // The ring is gone from the screen and mediad never started: the caller must not
            // be left ringing a phone nobody can answer.
            self.tell(hub);
            self.gave_up(&ringing);
            return Err(err);
        }
        Ok(ringing)
    }

    /// Ends whatever is in the slot: hangs up, declines the ring, dismisses the failure. Gives the
    /// conversation it was in. Dropping the call waits for mediad to exit. A call that had
    /// joined plays the leave chime first, while mediad can still store it.
    pub fn end(&self, hub: &Hub) -> Option<Key> {
        self.cue_if_joined(hub, None);
        let slot = std::mem::take(&mut *crate::hub::lock(&self.slot));
        let ended = match &slot {
            Slot::Idle => return None,
            Slot::Shown(live) => live.call.conversation.clone(),
            Slot::Active(active) => crate::hub::lock(&active.live).call.conversation.clone(),
        };
        // The shell hears at once; the mediad is waited for after, off the slot's lock.
        self.tell(hub);
        drop(slot);
        Some(ended)
    }

    /// mediad closed its stdout, so everything it had to say has been read. A failure it reported
    /// stays on screen until dismissed. One that died without a word is a failure too, whether
    /// it held a voice session or was still waiting for one, so pressing Call never ends in
    /// nothing. Only one that ended the call itself and exited cleanly leaves no call.
    fn reap(&self, hub: &Hub, id: u64) {
        let patience = Instant::now() + Duration::from_secs(2);
        loop {
            let mut slot = crate::hub::lock(&self.slot);
            let Slot::Active(active) = &mut *slot else {
                return;
            };
            if active.id != id {
                return;
            }
            if active.alive() {
                // Its stdout closes a moment before it is gone.
                if Instant::now() < patience {
                    drop(slot);
                    sleep(Duration::from_millis(5));
                    continue;
                }
                let _ = active.child.kill();
                let _ = active.child.wait();
            }
            let clean = active.child.try_wait().ok().flatten().is_some_and(|status| status.success());
            let mut live = crate::hub::lock(&active.live);
            // A failure mediad named itself is the better word; this is for one that said nothing.
            let silent = !matches!(live.call.state, CallState::Failed { .. });
            if silent && !clean {
                live.enter(failed(CallFailure::Engine, ENGINE_STOPPED));
            }
            drop(live);
            let stopped = slot.demote();
            if stopped.is_none() {
                *slot = Slot::Idle;
            }
            drop(slot);
            return self.stopped(hub, stopped);
        }
    }

    /// Looks at the deadlines of call `id`. False once that call is over, which ends its watcher.
    fn watch(&self, hub: &Hub, id: u64) -> bool {
        let stopped = {
            let mut slot = crate::hub::lock(&self.slot);
            let Slot::Active(active) = &mut *slot else {
                return false;
            };
            if active.id != id {
                return false;
            }
            let mut live = crate::hub::lock(&active.live);
            let Some(overdue) = live.overdue() else {
                return true;
            };
            live.enter(overdue);
            drop(live);
            slot.demote()
        };
        self.stopped(hub, stopped);
        false
    }

    /// The device menu opened or closed. Levels reach the shell only while it is open. A mediad
    /// that does not know the request yet is not an error: the menu simply has no meter.
    pub fn meter(&self, hub: &Hub, on: bool) -> Result<(), CallError> {
        let (call, _) = self.describe();
        let conversation = call.ok_or(CallError::NoCall)?.conversation;
        self.update(hub, &conversation, |live| {
            live.metering = on;
            if !on {
                live.call.level = None;
                live.call.output_level = None;
            }
        });
        match self.request(hub, &MediaRequest::Meter { on }) {
            Err(CallError::NoCall) => Err(CallError::NoCall),
            _ => Ok(()),
        }
    }

    pub fn request(&self, hub: &Hub, request: &MediaRequest) -> Result<MediaResponse, CallError> {
        // The slot is let go of before mediad is asked: a slow answer holds only this request.
        let running = crate::hub::lock(&self.slot).handle();
        let reply = match running {
            Some(active) => active.request(request).map_err(CallError::Media)?,
            None => return Err(CallError::NoCall),
        };
        self.tell(hub);
        Ok(reply)
    }

    /// Changes the call in place, whichever form it is held in. False when there is none in
    /// `conversation`. A change that fails the call stops its mediad.
    fn update(&self, hub: &Hub, conversation: &Key, change: impl FnOnce(&mut Live)) -> bool {
        let stopped = {
            let mut slot = crate::hub::lock(&self.slot);
            match &mut *slot {
                Slot::Idle => return false,
                Slot::Shown(live) => {
                    if live.call.conversation != *conversation {
                        return false;
                    }
                    change(live);
                }
                Slot::Active(active) => {
                    let mut live = crate::hub::lock(&active.live);
                    if live.call.conversation != *conversation {
                        return false;
                    }
                    change(&mut live);
                }
            }
            slot.demote()
        };
        self.stopped(hub, stopped);
        true
    }

    /// Our own mute and deafen, told to mediad. The adapter tells the network.
    pub fn set_audio(&self, hub: &Hub, muted: bool, deafened: bool) -> Result<Key, CallError> {
        let (call, running) = self.describe();
        let call = call.ok_or(CallError::NoCall)?;
        let conversation = call.conversation.clone();
        // A direct ring has no mediad yet. The choice is kept and applied when one starts.
        if !running {
            if !matches!(
                call.state,
                CallState::RingingOut | CallState::RingingIn { .. }
            ) {
                return Err(CallError::NoCall);
            }
            self.update(hub, &conversation, |live| {
                live.call.muted = muted;
                live.call.deafened = deafened;
            });
            self.tell(hub);
            return Ok(conversation);
        }
        self.request(hub, &MediaRequest::Audio { muted, deafened })?;
        self.update(hub, &conversation, |live| {
            live.call.muted = muted;
            live.call.deafened = deafened;
        });
        Ok(conversation)
    }

    // What the adapter reports.

    /// Someone is calling. While a call is up, or we are already ringing out, the new ring waits
    /// beside it. It does not replace the call, and it does not start mediad. A second one while
    /// one is already waiting is dropped.
    pub fn ring(&self, hub: &Arc<Hub>, conversation: Key, from: &str) {
        let state = CallState::RingingIn {
            from: from.to_owned(),
        };
        let hold = {
            let slot = crate::hub::lock(&self.slot);
            match &*slot {
                Slot::Active(_) => true,
                Slot::Shown(live) if matches!(live.call.state, CallState::RingingOut) => true,
                _ => false,
            }
        };
        if hold {
            let mut aside = crate::hub::lock(&self.aside);
            if aside.is_none() {
                *aside = Some(new_call(conversation, state).call);
            }
            drop(aside);
            self.tell(hub);
            return;
        }
        {
            let mut slot = crate::hub::lock(&self.slot);
            if matches!(&*slot, Slot::Shown(live) if live.call.conversation == conversation && matches!(live.call.state, CallState::RingingIn { .. })) { return; }
            *slot = Slot::Shown(Box::new(new_call(conversation, state)));
        }
        self.tell(hub);
        // Answering is likely: the engine is started now, not at the answer.
        self.prewarm(hub);
    }

    /// The caller gave up, or it was answered on another device.
    pub fn ring_stopped(&self, hub: &Hub, conversation: &Key) {
        let mut stopped = false;
        {
            let mut aside = crate::hub::lock(&self.aside);
            if aside
                .as_ref()
                .is_some_and(|call| call.conversation == *conversation)
            {
                *aside = None;
                stopped = true;
            }
        }
        {
            let mut slot = crate::hub::lock(&self.slot);
            let ringing = matches!(&*slot, Slot::Shown(live)
                if live.call.conversation == *conversation && matches!(live.call.state, CallState::RingingIn { .. }));
            if ringing {
                *slot = Slot::Idle;
                stopped = true;
            }
        }
        if stopped {
            self.tell(hub);
        }
    }

    /// The network's own word on the call: ringing out, or refused before media began.
    pub fn set_state(&self, hub: &Hub, conversation: &Key, state: CallState) {
        let changed = self.update(hub, conversation, |live| {
            // Word that we are ringing may arrive after the other side already answered.
            let answered = live.answered || (live.connected && !live.call.participants.is_empty());
            if state == CallState::RingingOut && answered {
                return;
            }
            live.enter(state);
        });
        if changed {
            self.tell(hub);
        }
    }

    /// The other side of a ring we placed picked up.
    pub fn answered(&self, hub: &Hub, conversation: &Key) {
        if self.update(hub, conversation, Live::answer) {
            self.tell(hub);
        }
    }

    /// Everyone in the call but us, as the network lists them. Who is speaking is mediad's to say
    /// and is carried over.
    pub fn set_participants(
        &self,
        hub: &Hub,
        conversation: &Key,
        mut participants: Vec<Participant>,
    ) {
        let mut cue = None;
        let known = self.update(hub, conversation, |live| {
            cue = arrival(&live.call, &participants, now());
            for participant in &mut participants {
                participant.speaking = live.talking.contains(&participant.id);
            }
            live.call.participants = participants;
            // The first other person in a call we rang is the answer.
            if live.call.state == CallState::RingingOut
                && live.connected
                && !live.call.participants.is_empty()
            {
                live.join();
            }
        });
        if known {
            self.tell(hub);
        }
        // mediad decides whether it is heard (S7, deafen).
        if let Some(cue) = cue {
            let _ = self.request(hub, &MediaRequest::Cue { cue });
        }
    }

    /// The network handed out a voice session. mediad takes it from here. A second one for the
    /// same call (the voice server moved) is a rejoin: mediad keeps the call and swaps the
    /// connection.
    pub fn join(&self, hub: &Hub, conversation: &Key, mut voice: VoiceJoin) {
        // Mute and deafen set before the session arrived are ours to keep, whatever the adapter
        // last saw; and the wait is now for mediad, not for the network.
        let mut again = false;
        let known = self.update(hub, conversation, |live| {
            voice.muted = live.call.muted;
            voice.deafened = live.call.deafened;
            again = live.joining;
            live.joining = true;
            live.me = voice.user_id.clone();
            live.since = Instant::now();
        });
        if !known {
            return;
        }
        let request = if again { MediaRequest::Rejoin(voice) } else { MediaRequest::Join(voice) };
        // What mediad refuses it refuses in words (`Refused`); a pipe that broke is not words.
        if let Err(CallError::Media(err)) = self.request(hub, &request) {
            tracing::warn!("chatd: mediad did not take the voice session: {err}");
            let detail = if err.kind() == io::ErrorKind::Other { err.to_string() } else { ENGINE_SILENT.to_owned() };
            self.set_state(hub, conversation, failed(CallFailure::Engine, detail));
        }
    }
}

/// Shown when the person only joins end-to-end encrypted calls and the server cannot encrypt them.
pub const SERVER_CANNOT_ENCRYPT: &str =
    "This server cannot encrypt calls end to end, and you chose to join only encrypted calls. Turn that off in Settings › Privacy to join anyway.";

/// What "only join end-to-end encrypted calls" asks of an offer. Off: the offer is as the adapter
/// made it. On: the offer carries `required`, so mediad blocks the call if the server later turns
/// encryption off, and an offer with no encryption at all (the server lacks it) is refused here
/// in words, before mediad is started, so nothing is sent to a server that cannot encrypt.
fn require_end_to_end(required: bool, offer: &mut TinyOffer) -> Result<(), String> {
    if !required {
        return Ok(());
    }
    match offer.e2ee.as_mut() {
        Some(e2ee) => {
            e2ee.required = true;
            Ok(())
        }
        None => Err(SERVER_CANNOT_ENCRYPT.to_owned()),
    }
}

impl Calls {
    /// A Tiny server call: mediad makes the offer for the room `conversation` names, started
    /// first if it is not running for that room. The same room again is a rejoin, and keeps the
    /// call. A Tiny call in another room of the same account gives way, since the server moves
    /// the seat; any other call is in the way. What failed is said in words. A person who only
    /// joins end-to-end encrypted calls (`REQUIRE_CALL_E2EE`) has `required` set on the offer, and
    /// is refused here when the offer has no encryption.
    pub fn tiny_offer(&self, hub: &Arc<Hub>, conversation: &Key, offer: TinyOffer) -> Result<String, String> {
        self.tiny_offer_with(hub, conversation, offer, None)
    }

    /// A direct can promote only its current visible ring, never create a call after hangup.
    pub fn tiny_direct_offer(&self, hub: &Arc<Hub>, conversation: &Key, offer: TinyOffer, intent: u64) -> Result<String, String> {
        self.tiny_offer_with(hub, conversation, offer, Some(intent))
    }

    fn tiny_offer_with(&self, hub: &Arc<Hub>, conversation: &Key, mut offer: TinyOffer, intent: Option<u64>) -> Result<String, String> {
        let direct = intent.is_some();
        let required = hub.settings().on(REQUIRE_CALL_E2EE);
        require_end_to_end(required, &mut offer)?;
        let (call, running) = self.describe();
        let here = call.as_ref().is_some_and(|call| call.conversation == *conversation) && running;
        if !here {
            if direct && call.as_ref().is_none_or(|call| call.conversation != *conversation || matches!(call.state, CallState::Failed { .. })) {
                return Err("That call is gone.".to_owned());
            }
            match call {
                Some(call) if running && call.conversation.account() != conversation.account() => return Err(IN_ANOTHER_CALL.to_owned()),
                Some(call) if running => drop(self.end_if(hub, &call.conversation)),
                _ => {}
            }
            self.start_with_intent(hub, conversation.clone(), intent).map_err(|err| match err {
                CallError::Active => IN_ANOTHER_CALL.to_owned(),
                CallError::NoCall => ENGINE_STOPPED.to_owned(),
                CallError::Media(err) => err.to_string(),
            })?;
        }
        self.update(hub, conversation, |live| {
            if intent.is_some() && intent != Some(live.intent) { return; }
            offer.muted = live.call.muted;
            offer.deafened = live.call.deafened;
            live.me = offer.user_id.clone();
        });
        let request = MediaRequest::TinyOffer(offer);
        let answer = if direct {
            let running = crate::hub::lock(&self.slot).handle().filter(|active| {
                let live = crate::hub::lock(&active.live);
                live.call.conversation == *conversation && Some(live.intent) == intent
            });
            let result = match running {
                Some(active) => active.request(&request).map_err(CallError::Media),
                None => Err(CallError::NoCall),
            };
            self.tell(hub);
            result
        } else { self.request(hub, &request) };
        match answer {
            Ok(MediaResponse::Offer { sdp }) => {
                self.stage(conversation, "offer_built");
                Ok(sdp)
            }
            answer => {
                let variant = match &answer {
                    Ok(frame) => frame.name(),
                    Err(_) => "error",
                };
                tracing::warn!(variant, "mediad frame was {variant}");
                if !here {
                    if let Some(intent) = intent { self.end_intent(hub, conversation, intent); }
                    else { self.end_if(hub, conversation); }
                }
                Err(match answer {
                    Err(CallError::Media(err)) if err.kind() == io::ErrorKind::Other => err.to_string(),
                    _ => ENGINE_SILENT.to_owned(),
                })
            }
        }
    }

    /// The server's answer to the last offer. From here the wait is for mediad to connect.
    pub fn tiny_answer(&self, hub: &Hub, conversation: &Key, sdp: String) {
        self.stage(conversation, "answer_received");
        let known = self.update(hub, conversation, |live| {
            live.joining = true;
            live.since = Instant::now();
        });
        if !known {
            return;
        }
        if let Err(CallError::Media(err)) = self.request(hub, &MediaRequest::TinyAnswer { sdp }) {
            tracing::warn!("chatd: mediad did not take the answer: {err}");
            let detail = if err.kind() == io::ErrorKind::Other { err.to_string() } else { ENGINE_SILENT.to_owned() };
            self.set_state(hub, conversation, failed(CallFailure::Engine, detail));
        }
    }

    /// Who each receive slot carries, for the call in `conversation`.
    pub fn tiny_slots(&self, hub: &Hub, conversation: &Key, audio: Vec<TinySlot>) {
        let (call, running) = self.describe();
        if running && call.is_some_and(|call| call.conversation == *conversation) {
            let _ = self.request(hub, &MediaRequest::TinySlots { audio });
        }
    }

    /// What a Tiny room asks of the call in it: the bitrate to encode at, and whether only
    /// push-to-talk opens the microphone there. Sent when the call joins the room and whenever
    /// the room changes.
    pub fn tiny_room(&self, hub: &Hub, conversation: &Key, kbps: u32, push_to_talk: bool) {
        let mut changed = false;
        if !self.update(hub, conversation, |live| {
            changed = live.push_to_talk != push_to_talk;
            live.push_to_talk = push_to_talk;
            live.room_bitrate = Some(kbps);
        }) {
            return;
        }
        let quality = hub.settings().voice_quality();
        let _ = self.request(hub, &MediaRequest::Bitrate { kbps: quality.apply_room(kbps) });
        let _ = self.request(hub, &MediaRequest::Quality(quality));
        if changed {
            let mode = input_mode(&hub.settings(), push_to_talk);
            let _ = self.request(hub, &MediaRequest::Input(mode));
        }
    }

    /// The call in `from` is now the call in `to`, with the same mediad, devices, mute and
    /// deafen: a Tiny server moved us. Its roster is the new room's, which the adapter tells.
    /// False when the call was not in `from`.
    pub fn retarget(&self, hub: &Hub, from: &Key, to: Key) -> bool {
        let moved = self.update(hub, from, |live| {
            live.call.conversation = to;
            live.call.participants.clear();
        });
        if moved {
            self.tell(hub);
            let _ = self.request(hub, &MediaRequest::Cue { cue: Cue::Moved });
        }
        moved
    }

    fn end_intent(&self, hub: &Hub, conversation: &Key, intent: u64) {
        let ended = {
            let mut slot = crate::hub::lock(&self.slot);
            let matches = match &*slot {
                Slot::Shown(live) => live.intent == intent && live.call.conversation == *conversation,
                Slot::Active(active) => {
                    let live = crate::hub::lock(&active.live);
                    live.intent == intent && live.call.conversation == *conversation
                }
                Slot::Idle => false,
            };
            if !matches { return; }
            std::mem::take(&mut *slot)
        };
        self.tell(hub);
        drop(ended);
    }

    /// Ends the call if it is in `conversation`, in whatever state, without telling the
    /// adapter: the network already knows. A joined call plays the leave chime for the
    /// person hanging up. True if there was one.
    pub fn end_if(&self, hub: &Hub, conversation: &Key) -> bool {
        self.cue_if_joined(hub, Some(conversation));
        let ended = {
            let mut slot = crate::hub::lock(&self.slot);
            let here = match &*slot {
                Slot::Idle => false,
                Slot::Shown(live) => live.call.conversation == *conversation,
                Slot::Active(active) => crate::hub::lock(&active.live).call.conversation == *conversation,
            };
            if !here {
                return false;
            }
            std::mem::take(&mut *slot)
        };
        self.tell(hub);
        drop(ended);
        true
    }

    /// Stores the leave chime before mediad is dropped. Hang-up closes its input, and a cue
    /// written after that never plays. A ring, or a call that never joined, stays quiet.
    fn cue_if_joined(&self, hub: &Hub, conversation: Option<&Key>) {
        let joined = {
            let slot = crate::hub::lock(&self.slot);
            match &*slot {
                Slot::Active(active) => {
                    let live = crate::hub::lock(&active.live);
                    live.call.state == CallState::Joined && conversation.is_none_or(|id| live.call.conversation == *id)
                }
                _ => false,
            }
        };
        if joined {
            let _ = self.request(hub, &MediaRequest::Cue { cue: Cue::Left });
        }
    }
}

/// The cue a new roster calls for: someone joined or left a call that has been up a moment,
/// and at most [`OCCUPANTS_LIMIT`] people are in it (a busy channel would chime all day).
fn arrival(call: &Call, roster: &[Participant], now: i64) -> Option<Cue> {
    if call.state != CallState::Joined || call.joined == 0 || now - call.joined < SETTLING || roster.len() > OCCUPANTS_LIMIT {
        return None;
    }
    let had = |id: &str| call.participants.iter().any(|p| p.id == id);
    let has = |id: &str| roster.iter().any(|p| p.id == id);
    if roster.iter().any(|p| !had(&p.id)) {
        Some(Cue::Joined)
    } else if call.participants.iter().any(|p| !has(&p.id)) {
        Some(Cue::Left)
    } else {
        None
    }
}

/// The devices the system lists, against V1 and V2: a choice stored by name alone (before ids
/// were kept) gains the id of the device of that name, and a device the system renamed keeps
/// being found under its new name. Stored at once, so the choice survives a restart either way.
fn learn_devices(hub: &Hub, inputs: &[AudioDevice], outputs: &[AudioDevice]) {
    for (key, listed) in [(INPUT, inputs), (OUTPUT, outputs)] {
        let better = hub.settings().device_choice(key).upgrade(listed);
        if let Some(better) = better {
            if let Err(err) = hub.set_setting(GLOBAL, key, better.value()) {
                tracing::warn!("chatd: {key}: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Network;

    fn live(state: CallState) -> Live {
        let conversation = Key {
            network: Network::Discord,
            account: "me".into(),
            id: "c1".into(),
        };
        new_call(conversation, state)
    }

    fn person(id: &str) -> Participant {
        Participant { id: id.into(), name: id.to_uppercase(), ..Participant::default() }
    }

    #[test]
    fn a_stale_direct_offer_cannot_promote_or_fail_a_replacement_ring() {
        for incoming in [false, true] {
            let hub = Arc::new(Hub::new(crate::store::Store::open(std::path::Path::new(":memory:")).unwrap()));
            let key = Key { network: Network::Tiny, account: "me".into(), id: "direct".into() };
            if incoming { hub.calls.ring(&hub, key.clone(), "Bea"); }
            else { hub.calls.place(&hub, key.clone()).unwrap(); }
            let old = hub.calls.call_intent(&key).unwrap();
            hub.calls.end(&hub);
            let next = hub.calls.place(&hub, key.clone()).unwrap();
            assert_ne!(old, next);
            let offer = TinyOffer { user_id: "me".into(), host: "127.0.0.1".into(), slots: 1,
                muted: false, deafened: false, e2ee: None };
            assert!(hub.calls.tiny_direct_offer(&hub, &key, offer, old).is_err());
            hub.calls.fail_ring_attempt(&hub, &key, old, CallFailure::Refused, "old refusal");
            hub.calls.end_intent(&hub, &key, old);
            assert_eq!(hub.calls.call_intent(&key), Some(next));
            let (call, media) = hub.calls.describe();
            assert_eq!(call.unwrap().state, CallState::RingingOut);
            assert!(!media, "a stale offer never starts the engine for a new unaccepted ring");
            hub.calls.end(&hub);
        }
    }

    #[test]
    fn an_engine_waiting_for_a_call_draws_and_announces_nothing_but_a_call_that_has_it_does() {
        let devices = |name: &str| MediaResponse::DeviceList { inputs: vec![AudioDevice { name: name.into(), ..AudioDevice::default() }], outputs: Vec::new() };
        let mut waiting = live(CallState::Connecting);
        assert!(!waiting.adopted);
        // The first list, and a hot-plug after it: learned, not drawn.
        assert!(waiting.hear(devices("Mic")).is_none());
        assert!(waiting.hear(devices("Headset")).is_none());
        assert_eq!(waiting.call.inputs.len(), 1, "the list is still kept for the call that comes");
        // A call takes it: the same event is drawn as before.
        waiting.adopted = true;
        assert!(waiting.hear(devices("Mic")).is_some());
        assert!(waiting.hear(devices("Mic")).is_none(), "no change, nothing to draw");
        // A cold start for a real conversation is a call from the first word; one for nobody is not.
        assert!(conversation_is_nobody(&nobody()));
        assert!(!conversation_is_nobody(&Key { network: Network::Tiny, account: "me".into(), id: "lounge".into() }));
    }

    #[test]
    fn a_join_is_timed_from_the_click_for_its_own_conversation_and_told_once() {
        let calls = Calls::default();
        let key = Key { network: Network::Tiny, account: "me".into(), id: "lounge".into() };
        let other = Key { id: "den".into(), ..key.clone() };
        calls.stage(&key, "offer_built");
        assert!(crate::hub::lock(&calls.timeline).is_none(), "no click, no timeline");
        calls.clicked(&key);
        calls.stage(&key, "offer_built");
        calls.stage(&other, "answer_received");
        assert_eq!(crate::hub::lock(&calls.timeline).as_ref().unwrap().stages.len(), 1, "another room's stage is not this join's");
        calls.joined(&other);
        assert!(crate::hub::lock(&calls.timeline).is_some());
        calls.joined(&key);
        assert!(crate::hub::lock(&calls.timeline).is_none(), "told once, then gone");
    }

    #[test]
    fn a_direct_call_starts_only_from_the_ring_showing_and_nothing_starts_over_a_running_engine() {
        let key = Key { network: Network::Tiny, account: "me".into(), id: "direct".into() };
        let ring = new_call(key.clone(), CallState::RingingOut);
        let intent = ring.intent;
        let mut slot = Slot::Shown(Box::new(ring));
        assert!(admit(&mut slot, &key, Some(intent)).is_ok());
        assert!(matches!(admit(&mut slot, &key, Some(intent + 1)), Err(CallError::NoCall)), "a ring that is gone");
        assert!(admit(&mut slot, &key, None).is_ok(), "a room join needs no ring");
        let mut idle = Slot::Idle;
        assert!(matches!(admit(&mut idle, &key, Some(intent)), Err(CallError::NoCall)));
    }

    #[test]
    fn my_camera_and_my_share_keep_their_own_self_views() {
        let view = |path: &str, generation: u64, share: bool| MediaResponse::SelfView { path: path.into(), width: 640, height: 360, generation, share };
        let mut live = live(CallState::Joined);
        assert!(live.apply(view("camera.jpg", 1, false)));
        assert!(live.apply(view("share.jpg", 1, true)));
        let camera = live.call.self_view.clone().unwrap();
        let share = live.call.share_view.clone().unwrap();
        assert_eq!((camera.path.as_str(), camera.kind), ("camera.jpg", PictureKind::Camera));
        assert_eq!((share.path.as_str(), share.kind), ("share.jpg", PictureKind::Share));
        assert!(!live.apply(view("share.jpg", 1, true)), "the same picture is no news");

        // The share going off takes only its own view.
        assert!(live.apply(view("", 0, true)));
        assert!(live.call.share_view.is_none());
        assert!(live.call.self_view.is_some(), "the camera is still on");
    }

    #[test]
    fn a_share_that_is_no_longer_sent_takes_its_view_with_it() {
        let mut live = live(CallState::Joined);
        assert!(live.apply(MediaResponse::SelfView { path: "share.jpg".into(), width: 640, height: 360, generation: 1, share: true }));
        live.apply(MediaResponse::Sending { share: true, camera: false });
        assert!(live.call.share_view.is_some(), "still sending");
        assert!(live.apply(MediaResponse::Sending { share: false, camera: false }));
        assert!(live.call.share_view.is_none());
    }

    #[test]
    fn only_the_voice_connection_being_up_makes_a_call_joined() {
        let mut live = live(CallState::Connecting);
        assert!(!live.apply(MediaResponse::Source { user_id: "a".into(), ssrc: 7 }));
        assert_eq!(live.call.state, CallState::Connecting);
        assert_eq!(live.call.joined, 0);
        assert!(live.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }));
        assert_eq!(live.call.state, CallState::Joined);
        let joined = live.call.joined;
        assert!(joined > 0);

        // Only a call that is up can be interrupted: mediad moving server before that is not news.
        let mut early = self::live(CallState::RingingOut);
        assert!(!early.apply(MediaResponse::Interrupted));
        assert_eq!(early.call.state, CallState::RingingOut);
        assert!(!early.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }));
        assert_eq!(early.call.state, CallState::RingingOut, "and it is still not an answer");

        // A drop and a resume do not restart the clock.
        assert!(live.apply(MediaResponse::Interrupted));
        assert_eq!(live.call.state, CallState::Reconnecting);
        assert!(live.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }));
        assert_eq!((live.call.state.clone(), live.call.joined), (CallState::Joined, joined));
        assert!(!live.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }), "nothing new to draw");

        // A second drop while already reconnecting stays there. It is not a failure, and not drawn again.
        assert!(live.apply(MediaResponse::Interrupted));
        assert_eq!(live.call.state, CallState::Reconnecting);
        assert!(!live.apply(MediaResponse::Interrupted), "already reconnecting is not drawn again");
        assert!(matches!(live.call.state, CallState::Reconnecting), "the call is still up");
    }

    #[test]
    fn a_ring_we_placed_is_a_call_only_once_someone_answers() {
        let mut live = live(CallState::RingingOut);
        assert!(!live.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }));
        assert_eq!(live.call.state, CallState::RingingOut, "our own media being up is not an answer");
        assert!(live.unanswered());
        live.call.participants = vec![person("a")];
        assert!(!live.unanswered());

        // The adapter's own word for it: answered before our media is up is still not a call.
        let mut live = self::live(CallState::RingingOut);
        live.answer();
        assert_eq!(live.call.state, CallState::Connecting);
        assert!(live.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }));
        assert_eq!(live.call.state, CallState::Joined);
        let mut live = self::live(CallState::RingingOut);
        assert!(!live.apply(MediaResponse::Connected { ssrc: 1, dave_version: 1 }));
        live.answer();
        assert_eq!(live.call.state, CallState::Joined);
    }

    #[test]
    fn speaking_is_merged_by_user_id_and_kept_for_a_roster_that_comes_later() {
        let mut live = live(CallState::Joined);
        // Nobody by that id is listed yet: nothing to draw, but it is remembered.
        assert!(!live.apply(MediaResponse::Speaking { user_id: "a".into(), speaking: true }));
        assert_eq!(live.talking, ["a"]);
        live.call.participants = vec![person("a"), person("b")];
        assert!(live.apply(MediaResponse::Speaking { user_id: "b".into(), speaking: true }));
        assert!(!live.apply(MediaResponse::Speaking { user_id: "b".into(), speaking: true }), "no change, no repaint");
        assert!(live.call.participants[1].speaking);
        assert!(live.apply(MediaResponse::Gone { user_id: "b".into() }), "who left stops speaking");
        assert!(!live.call.participants[1].speaking);
        assert_eq!(live.talking, ["a"]);
        // Our own speaking is the call's, once the voice session has said who we are.
        assert!(!live.apply(MediaResponse::Speaking { user_id: "me".into(), speaking: true }));
        live.me = "me".into();
        assert!(live.apply(MediaResponse::Speaking { user_id: "me".into(), speaking: true }));
        assert!(live.call.speaking);
        assert!(live.call.participants.iter().all(|who| who.id != "me"));
    }

    fn tiny_offer(e2ee: Option<protocol::TinyE2ee>) -> TinyOffer {
        TinyOffer { user_id: "me".into(), host: "127.0.0.1".into(), slots: 11, muted: false, deafened: false, e2ee }
    }

    fn sealed_offer() -> TinyOffer {
        tiny_offer(Some(protocol::TinyE2ee { room_id: "room".into(), identity: "/keys/me".into(), required: false }))
    }

    #[test]
    fn the_setting_sets_required_on_an_offer_that_has_encryption() {
        let mut offer = sealed_offer();
        require_end_to_end(false, &mut offer).unwrap();
        assert_eq!(offer.e2ee.as_ref().map(|e2ee| e2ee.required), Some(false), "off leaves the offer as the adapter made it");
        require_end_to_end(true, &mut offer).unwrap();
        assert_eq!(offer.e2ee.as_ref().map(|e2ee| e2ee.required), Some(true));
        // Off with no encryption on the server is a call that carries on, as before.
        let mut plain = tiny_offer(None);
        require_end_to_end(false, &mut plain).unwrap();
        assert!(plain.e2ee.is_none());
    }

    #[test]
    fn a_person_who_requires_encryption_does_not_join_a_server_that_lacks_it() {
        let mut plain = tiny_offer(None);
        assert_eq!(require_end_to_end(true, &mut plain), Err(SERVER_CANNOT_ENCRYPT.to_owned()));
        assert!(SERVER_CANNOT_ENCRYPT.contains("Settings"), "it says where to change it");
    }

    #[test]
    fn tiny_offer_reads_the_setting_and_refuses_before_mediad_is_started() {
        let dir = std::env::temp_dir().join(format!("tc-media-require-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hub = Arc::new(Hub::new(crate::store::Store::open(&dir.join("chat.sqlite")).unwrap()));
        let key = Key { network: Network::Tiny, account: "me".into(), id: "room".into() };
        // Required, and the offer has no encryption: refused in words, with no call started.
        hub.set_setting(GLOBAL, REQUIRE_CALL_E2EE, Some("on".into())).unwrap();
        assert!(hub.settings().on(REQUIRE_CALL_E2EE));
        assert_eq!(hub.calls.tiny_offer(&hub, &key, tiny_offer(None)), Err(SERVER_CANNOT_ENCRYPT.to_owned()));
        let (call, running) = hub.calls.describe();
        assert!(call.is_none() && !running, "nothing was started for a server that cannot encrypt");
        drop(hub);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_device_saved_by_name_learns_its_id_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("tc-media-devices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.sqlite");
        const HEADSET: &str = "Headset Microphone (CORSAIR HS55 Wireless Core Receiver)";
        let id = "wasapi:{0.0.1.00000000}.{e1332b3d-ea19-49ca-a920-d17e2523f4ee}";
        {
            let hub = Hub::new(crate::store::Store::open(&path).unwrap());
            // As 0.2.19 stored it: the name alone. The speaker was never chosen.
            hub.set_setting(GLOBAL, INPUT, Some(HEADSET.into())).unwrap();
            let device = |id: &str, name: &str| AudioDevice { id: id.into(), name: name.into(), ..AudioDevice::default() };
            let inputs = [device("k", "Krisp Microphone (Krisp Audio)"), device(id, HEADSET)];
            learn_devices(&hub, &inputs, &[device("s", "Speakers")]);
            assert_eq!(hub.settings().device_choice(INPUT), protocol::settings::DeviceChoice::device(HEADSET, id));
            assert_eq!(hub.settings().device(OUTPUT), None, "nothing chosen is nothing stored");
        }
        let hub = Hub::new(crate::store::Store::open(&path).unwrap());
        assert_eq!(hub.settings().device(INPUT), Some(format!("{HEADSET}\t{id}")));
        drop(hub);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devices_and_failures_reach_the_call_and_levels_do_not() {
        let mut live = live(CallState::Joined);
        let devices = MediaResponse::Devices { input: "Mic".into(), output: "Speakers".into(), input_id: "m".into(), output_id: "s".into(), notice: None, stuck: false };
        assert!(live.apply(devices.clone()));
        assert!(!live.apply(devices));
        assert_eq!((live.call.input.as_str(), live.call.output.as_str()), ("Mic", "Speakers"));
        assert_eq!((live.call.input_id.as_str(), live.call.output_id.as_str()), ("m", "s"));
        // A stand-in is said in words until the chosen device is back.
        let notice = Some("Headset isn't connected. Using Mic until it's back.".to_owned());
        let standing_in = MediaResponse::Devices { input: "Mic".into(), output: "Speakers".into(), input_id: "m".into(), output_id: "s".into(), notice: notice.clone(), stuck: false };
        assert!(live.apply(standing_in));
        assert_eq!(live.call.device_notice, notice);
        assert!(!live.apply(MediaResponse::Levels { input: 40, output: 10 }), "nobody has the device menu open");
        live.metering = true;
        assert!(live.apply(MediaResponse::Levels { input: 40, output: 10 }));
        assert!(!live.apply(MediaResponse::Levels { input: 40, output: 10 }), "nothing changed");
        assert!(live.apply(MediaResponse::Levels { input: 40, output: 90 }), "the speaker has a meter too");
        assert_eq!((live.call.level, live.call.output_level), (Some(40), Some(90)));
        let device = |name: &str| AudioDevice { id: format!("id-{name}"), name: name.into(), ..AudioDevice::default() };
        let listed = MediaResponse::DeviceList { inputs: vec![device("Mic"); 40], outputs: vec![device("Speakers")] };
        assert!(live.apply(listed.clone()));
        assert!(!live.apply(listed));
        assert_eq!((live.call.inputs.len(), live.call.outputs.len()), (DEVICES_LIMIT, 1));
        let failure = MediaResponse::Failed { reason: CallFailure::Encryption, detail: "why".into() };
        assert!(live.apply(failure));
        assert_eq!(live.call.state, failed(CallFailure::Encryption, "why"));
    }

    #[test]
    fn someone_joining_or_leaving_a_settled_call_is_a_cue_and_the_first_roster_is_not() {
        let mut live = live(CallState::Joined);
        live.call.joined = 1_000;
        live.call.participants = vec![person("a")];
        assert_eq!(arrival(&live.call, &[person("a"), person("b")], 1_001), None, "the roster is still arriving");
        assert_eq!(arrival(&live.call, &[person("a"), person("b")], 1_010), Some(Cue::Joined));
        assert_eq!(arrival(&live.call, &[], 1_010), Some(Cue::Left));
        assert_eq!(arrival(&live.call, &[person("a")], 1_010), None);
        let crowd: Vec<Participant> = (0..30).map(|n| person(&n.to_string())).collect();
        assert_eq!(arrival(&live.call, &crowd, 1_010), None, "a crowded channel stays quiet");
        live.enter(CallState::Reconnecting);
        assert_eq!(arrival(&live.call, &[person("a"), person("b")], 1_010), None);
    }

    #[test]
    fn each_waiting_state_has_its_deadline_and_a_joined_call_has_none() {
        let ago = |live: &mut Live, waited: Duration| live.since = Instant::now().checked_sub(waited).unwrap();
        let mut live = live(CallState::Connecting);
        assert_eq!(live.overdue(), None);
        ago(&mut live, SESSION + Duration::from_secs(1));
        assert_eq!(live.overdue(), Some(failed(CallFailure::Lost, "No reply from Discord after 10 seconds.")));

        // Once mediad holds the session the wait is the longer one.
        live.joining = true;
        assert_eq!(live.overdue(), None);
        ago(&mut live, CONNECT + Duration::from_secs(1));
        assert_eq!(live.overdue(), Some(failed(CallFailure::Engine, "The call engine stopped answering. Try again.")));

        live.enter(CallState::RingingOut);
        assert_eq!(live.overdue(), None, "entering a state restarts the clock");
        ago(&mut live, RING + Duration::from_secs(1));
        assert_eq!(live.overdue(), Some(failed(CallFailure::NoAnswer, "Nobody picked up.")));

        live.join();
        ago(&mut live, Duration::from_secs(3600));
        assert_eq!(live.overdue(), None);

        live.enter(CallState::Reconnecting);
        ago(&mut live, Duration::from_secs(3600));
        assert_eq!(live.overdue(), None, "reconnecting has no clock");
    }

    #[test]
    fn a_ring_beside_an_outgoing_one_waits_and_ending_does_not_promote_it() {
        let hub = std::sync::Arc::new(crate::hub::Hub::new(
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("store"),
        ));
        let outgoing = Key {
            network: Network::Discord,
            account: "me".into(),
            id: "room".into(),
        };
        let waiting = Key {
            network: Network::Tiny,
            account: "me".into(),
            id: "dm".into(),
        };
        let later = Key {
            network: Network::Tiny,
            account: "me".into(),
            id: "other".into(),
        };
        hub.calls.place(&hub, outgoing.clone()).expect("placed");
        hub.calls.ring(&hub, waiting.clone(), "Bea");
        hub.calls.ring(&hub, later, "Cara");
        let (call, running) = hub.calls.describe();
        assert!(!running);
        let call = call.expect("the outgoing ring");
        assert_eq!(call.conversation, outgoing);
        assert!(matches!(call.state, CallState::RingingOut));
        assert_eq!(
            hub.calls
                .aside()
                .expect("the first waiting ring")
                .conversation,
            waiting
        );
        assert_eq!(hub.calls.end(&hub), Some(outgoing));
        assert!(hub.calls.describe().0.is_none());
        assert_eq!(
            hub.calls
                .aside()
                .expect("ending does not take the waiting ring")
                .conversation,
            waiting
        );
        assert_eq!(hub.calls.incoming(&hub), Some(waiting));
        let (call, running) = hub.calls.describe();
        assert!(!running);
        assert!(matches!(
            call.expect("the promoted ring").state,
            CallState::RingingIn { .. }
        ));
        assert!(hub.calls.aside().is_none());
    }
}
