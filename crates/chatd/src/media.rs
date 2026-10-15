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
use protocol::settings::{INPUT, JOIN_MUTED, OUTPUT};
use protocol::{
    clip, read_frame, write_frame, Call, CallFailure, CallState, Change, Cue, Key, MediaRequest,
    MediaResponse, Participant, VoiceJoin, DEVICES_LIMIT, OCCUPANTS_LIMIT,
};
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// Speakers remembered ahead of the roster that names them.
const TALKING: usize = 64;
/// A roster change this soon after joining is the roster arriving, not someone joining.
const SETTLING: i64 = 2;

/// The one sentence for a ring nobody took. The dock says it with the person's name.
pub const NO_ANSWER: &str = "Nobody picked up.";
/// What the dock shows when mediad is gone or mute; what mediad or the system said is logged.
const ENGINE_STOPPED: &str = "The call engine stopped.";
const ENGINE_SILENT: &str = "The call engine stopped answering. Try again.";
const ENGINE_MISSING: &str = "The call engine could not start. Reinstall tinyclient, then try again.";

/// Why starting or asking the call engine failed. Debug, so a test can name one.
#[derive(Debug)]
pub enum CallError {
    Active,
    NoCall,
    Media(io::Error),
}

/// A call and what is known about it beyond what the shell is told.
struct Live {
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
}

/// The running mediad and the call it exists for. Dropping it ends the call.
struct ActiveCall {
    /// Tells this call from the next one, for the threads that outlive it.
    id: u64,
    child: Child,
    stdin: Option<ChildStdin>,
    /// Replies to requests, passed on by the thread that reads mediad's stdout.
    replies: Receiver<MediaResponse>,
    /// The call as the shell sees it, also written by that thread as events arrive.
    live: Arc<Mutex<Live>>,
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

/// At most one call, across every network.
#[derive(Default)]
pub struct Calls {
    slot: Mutex<Slot>,
    next: AtomicU64,
    failed: Mutex<Option<Failed>>,
    /// The mediad started for Settings' microphone test or test sound, outside a call.
    tester: Mutex<Option<Tester>>,
}

/// A mediad with no call (`mediad test`): it opens the microphone while the meter is on and the
/// speaker while a cue plays, and exits by itself once neither has happened for a moment, or
/// after 30 s of metering. Nothing waits on its replies.
struct Tester {
    id: u64,
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Tester {
    fn start(hub: &Arc<Hub>, id: u64) -> io::Result<Tester> {
        let path = mediad_path()?;
        let mut child = protocol::launch::no_console(Command::new(&path).arg("test")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let stdin = child.stdin.take();
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
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
        inputs: Vec::new(),
        outputs: Vec::new(),
    };
    Live {
        call,
        talking: Vec::new(),
        since: Instant::now(),
        joining: false,
        connected: false,
        answered: false,
        metering: false,
        me: String::new(),
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
            MediaResponse::Devices { input, output } => {
                if (&self.call.input, &self.call.output) == (&input, &output) {
                    return false;
                }
                self.call.input = input;
                self.call.output = output;
            }
            // Levels are for the device menu's meter, and stop here unless it is open.
            MediaResponse::Levels { input, .. } => {
                if !self.metering || self.call.level == Some(input) {
                    return false;
                }
                self.call.level = Some(input.min(100));
            }
            MediaResponse::DeviceList { inputs, outputs } => {
                let bounded = |mut names: Vec<String>| {
                    names.truncate(DEVICES_LIMIT);
                    names.iter_mut().for_each(|name| clip(name, 200));
                    names
                };
                let (inputs, outputs) = (bounded(inputs), bounded(outputs));
                if (&self.call.inputs, &self.call.outputs) == (&inputs, &outputs) {
                    return false;
                }
                self.call.inputs = inputs;
                self.call.outputs = outputs;
            }
            MediaResponse::Source { .. } => return false,
            MediaResponse::Ready { .. } | MediaResponse::Streams { .. } | MediaResponse::Done | MediaResponse::Refused { .. } => {
                return false
            }
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
        let spawned = protocol::launch::no_console(Command::new(&path).arg("1")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn();
        let mut child = spawned.map_err(|err| {
            eprintln!("chatd: could not run {}: {err}", path.display());
            io::Error::other(ENGINE_MISSING)
        })?;
        let stdin = child.stdin.take();
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        // Ready is the first thing a healthy mediad says. The read waits on a thread of its own,
        // for `Calls::start` holds the one call slot across this: a helper that never speaks (a
        // stale binary, one wedged before its first word) must fail the call rather than hold
        // every Status answer and every ring with it.
        let (said, answered) = channel();
        let spawned = std::thread::Builder::new().name("mediad-ready".into()).stack_size(128 * 1024).spawn(move || {
            let frame = read_frame::<MediaResponse>(&mut stdout);
            let _ = said.send((frame, stdout));
        });
        if let Err(err) = spawned {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("chatd: the reader of {} did not start: {err}", path.display());
            return Err(io::Error::other(ENGINE_MISSING));
        }
        let (frame, mut stdout) = match answered.recv_timeout(READY) {
            Ok(arrived) => arrived,
            answer => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("chatd: {} did not say it was ready: {answer:?}", path.display());
                let silent = matches!(answer, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
                return Err(io::Error::other(if silent { ENGINE_SILENT } else { ENGINE_MISSING }));
            }
        };
        let engine_linked = match frame {
            Ok(MediaResponse::Ready { engine_linked }) => engine_linked,
            other => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("chatd: {} did not start: {other:?}", path.display());
                return Err(io::Error::other(ENGINE_MISSING));
            }
        };
        let mut live = new_call(conversation, CallState::Connecting);
        live.call.engine_linked = engine_linked;
        let live = Arc::new(Mutex::new(live));
        let (reply, replies) = channel();

        let (reader, seen) = (hub.clone(), live.clone());
        std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            // Ends when mediad closes its stdout, which is when it exits.
            while let Ok(frame) = read_frame::<MediaResponse>(&mut stdout) {
                if frame.is_reply() {
                    let _ = reply.send(frame);
                    continue;
                }
                let mut live = crate::hub::lock(&seen);
                if live.apply(frame) {
                    let call = live.call.clone();
                    drop(live);
                    reader.changed(Change::Call { call: Some(call) });
                }
            }
            reader.calls.reap(&reader, id);
        })?;

        let watcher = hub.clone();
        std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            // Ends with the call it watches.
            while watcher.calls.watch(&watcher, id) {
                sleep(WATCH);
            }
        })?;
        Ok(ActiveCall {
            id,
            child,
            stdin,
            replies,
            live,
        })
    }

    /// False once mediad has exited on its own.
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn request(&mut self, request: &MediaRequest) -> io::Result<MediaResponse> {
        let stdin = self.stdin.as_mut().expect("stdin is open until drop");
        write_frame(stdin, request)?;
        let reply = self.replies.recv_timeout(REPLY).map_err(|_| io::Error::other(ENGINE_SILENT))?;
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
        self.stdin.take();
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
        hub.changed(Change::Call { call: self.describe().0 });
    }

    /// Who hears of a call that failed: the server, which has the adapter leave on the network.
    pub fn on_failure(&self, told: impl Fn(&Key) + Send + Sync + 'static) {
        *crate::hub::lock(&self.failed) = Some(Box::new(told));
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
        // A microphone test gives way: the call needs the devices.
        let tester = crate::hub::lock(&self.tester).take();
        if tester.is_some() {
            drop(tester);
            hub.changed(Change::Meter { level: None });
        }
        let (tuning, input, output, muted) = {
            let settings = hub.settings();
            (settings.tuning(), settings.device(INPUT), settings.device(OUTPUT), settings.on(JOIN_MUTED))
        };
        {
            let mut slot = crate::hub::lock(&self.slot);
            if slot.running().is_some() {
                return Err(CallError::Active);
            }
            // The old mediad, if any, is reaped before the new one starts.
            *slot = Slot::Idle;
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let mut active = ActiveCall::start(hub, id, conversation).map_err(CallError::Media)?;
            // The user's defaults, before the voice session arrives: mediad opens the devices
            // only once it holds one. A mediad that refuses them still carries the call.
            crate::hub::lock(&active.live).call.muted = muted;
            let _ = active.request(&MediaRequest::Tune(tuning));
            if input.is_some() || output.is_some() {
                let _ = active.request(&MediaRequest::Devices { input, output });
            }
            *slot = Slot::Active(active);
        }
        self.tell(hub);
        Ok(())
    }

    /// Whether a call is starting or up with its mediad: a test then uses that mediad.
    fn in_call(&self) -> bool {
        crate::hub::lock(&self.slot).running().is_some()
    }

    /// The tuning changed in Settings: the call's mediad, or the test's, takes it at once.
    pub fn retune(&self, hub: &Hub) {
        let tuning = hub.settings().tuning();
        if let Some(active) = crate::hub::lock(&self.slot).running() {
            let _ = active.request(&MediaRequest::Tune(tuning));
        }
        if let Some(tester) = crate::hub::lock(&self.tester).as_mut() {
            let _ = tester.send(&MediaRequest::Tune(tuning));
        }
    }

    /// The running test mediad, started with the user's devices and tuning if there is none.
    fn tester<'a>(&self, hub: &Arc<Hub>, held: &'a mut Option<Tester>) -> Result<&'a mut Tester, CallError> {
        if held.is_none() {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let mut tester = Tester::start(hub, id).map_err(|err| {
                eprintln!("chatd: the test mediad did not start: {err}");
                CallError::Media(io::Error::other(ENGINE_MISSING))
            })?;
            let (tuning, input, output) = {
                let settings = hub.settings();
                (settings.tuning(), settings.device(INPUT), settings.device(OUTPUT))
            };
            let told = tester.send(&MediaRequest::Tune(tuning)).and_then(|()| tester.send(&MediaRequest::Devices { input, output }));
            told.map_err(CallError::Media)?;
            *held = Some(tester);
        }
        Ok(held.as_mut().expect("started above"))
    }

    /// Settings' microphone test. In a call it is the call's own meter; otherwise a test mediad
    /// carries it, and stops it after 30 s by itself.
    pub fn test_microphone(&self, hub: &Arc<Hub>, on: bool) -> Result<(), CallError> {
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
        let tester = self.tester(hub, &mut held)?;
        let asked = tester.send(&MediaRequest::Meter { on: true });
        if asked.is_err() {
            // It had just exited: start it again.
            *held = None;
            self.tester(hub, &mut held)?.send(&MediaRequest::Meter { on: true }).map_err(CallError::Media)?;
        }
        Ok(())
    }

    /// Settings' test sound, on the call speaker: in the call, or from a test mediad.
    pub fn test_sound(&self, hub: &Arc<Hub>) -> Result<(), CallError> {
        let test = MediaRequest::Cue { cue: Cue::Test };
        if let Some(active) = crate::hub::lock(&self.slot).running() {
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
    /// conversation it was in. Dropping the call waits for mediad to exit.
    pub fn end(&self, hub: &Hub) -> Option<Key> {
        let ended = match std::mem::take(&mut *crate::hub::lock(&self.slot)) {
            Slot::Idle => return None,
            Slot::Shown(live) => live.call.conversation.clone(),
            Slot::Active(active) => crate::hub::lock(&active.live).call.conversation.clone(),
        };
        self.tell(hub);
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
            }
        });
        match self.request(hub, &MediaRequest::Meter { on }) {
            Err(CallError::NoCall) => Err(CallError::NoCall),
            _ => Ok(()),
        }
    }

    pub fn request(&self, hub: &Hub, request: &MediaRequest) -> Result<MediaResponse, CallError> {
        let reply = match crate::hub::lock(&self.slot).running() {
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
        let (conversation, _) = self.describe();
        let conversation = conversation.ok_or(CallError::NoCall)?.conversation;
        self.request(hub, &MediaRequest::Audio { muted, deafened })?;
        self.update(hub, &conversation, |live| {
            live.call.muted = muted;
            live.call.deafened = deafened;
        });
        Ok(conversation)
    }

    // What the adapter reports.

    /// Someone is calling. Ignored while a call is up: the adapter may notify, nothing rings.
    ///
    /// The system notification for a ring (design 07 §4) is to be posted from here and withdrawn
    /// in `ring_stopped`. Not built: it needs chatd to run inside the signed bundle
    /// (`packaging/bundle.sh`), which it now can.
    pub fn ring(&self, hub: &Hub, conversation: Key, from: &str) {
        {
            let mut slot = crate::hub::lock(&self.slot);
            if matches!(&*slot, Slot::Active(_)) {
                return;
            }
            let state = CallState::RingingIn { from: from.to_owned() };
            *slot = Slot::Shown(Box::new(new_call(conversation, state)));
        }
        self.tell(hub);
    }

    /// The caller gave up, or it was answered on another device.
    pub fn ring_stopped(&self, hub: &Hub, conversation: &Key) {
        {
            let mut slot = crate::hub::lock(&self.slot);
            let ringing = matches!(&*slot, Slot::Shown(live)
                if live.call.conversation == *conversation && matches!(live.call.state, CallState::RingingIn { .. }));
            if !ringing {
                return;
            }
            *slot = Slot::Idle;
        }
        self.tell(hub);
    }

    /// The network's own word on the call: ringing out, or refused before media began.
    pub fn set_state(&self, hub: &Hub, conversation: &Key, state: CallState) {
        self.update(hub, conversation, |live| {
            // Word that we are ringing may arrive after the other side already answered.
            let answered = live.answered || (live.connected && !live.call.participants.is_empty());
            if state == CallState::RingingOut && answered {
                return;
            }
            live.enter(state);
        });
    }

    /// The other side of a ring we placed picked up.
    pub fn answered(&self, hub: &Hub, conversation: &Key) {
        self.update(hub, conversation, Live::answer);
    }

    /// Everyone in the call but us, as the network lists them. Who is speaking is mediad's to say
    /// and is carried over.
    pub fn set_participants(&self, hub: &Hub, conversation: &Key, mut participants: Vec<Participant>) {
        let mut cue = None;
        self.update(hub, conversation, |live| {
            cue = arrival(&live.call, &participants, now());
            for participant in &mut participants {
                participant.speaking = live.talking.contains(&participant.id);
            }
            live.call.participants = participants;
            // The first other person in a call we rang is the answer.
            if live.call.state == CallState::RingingOut && live.connected && !live.call.participants.is_empty() {
                live.join();
            }
        });
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
            eprintln!("chatd: mediad did not take the voice session: {err}");
            let detail = if err.kind() == io::ErrorKind::Other { err.to_string() } else { ENGINE_SILENT.to_owned() };
            self.set_state(hub, conversation, failed(CallFailure::Engine, detail));
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
        Participant {
            id: id.into(),
            name: id.to_uppercase(),
            speaking: false,
            muted: false,
            deafened: false,
        }
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

    #[test]
    fn devices_and_failures_reach_the_call_and_levels_do_not() {
        let mut live = live(CallState::Joined);
        let devices = MediaResponse::Devices { input: "Mic".into(), output: "Speakers".into() };
        assert!(live.apply(devices.clone()));
        assert!(!live.apply(devices));
        assert_eq!((live.call.input.as_str(), live.call.output.as_str()), ("Mic", "Speakers"));
        assert!(!live.apply(MediaResponse::Levels { input: 40, output: 10 }), "nobody has the device menu open");
        live.metering = true;
        assert!(live.apply(MediaResponse::Levels { input: 40, output: 10 }));
        assert!(!live.apply(MediaResponse::Levels { input: 40, output: 90 }), "only our own level is drawn");
        assert_eq!(live.call.level, Some(40));
        let listed = MediaResponse::DeviceList { inputs: vec!["Mic".into(); 40], outputs: vec!["Speakers".into()] };
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
    }
}
