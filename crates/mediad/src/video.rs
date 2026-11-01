//! The picture's two threads, so that nothing a picture costs lands on the call's tick. One
//! captures and encodes on a fixed schedule; the other opens, decodes and paints. The tick keeps
//! `Rtc` and only moves bytes: what it hears from str0m goes on [`Video::picture`], what it sends
//! comes off [`Video::next`]. Both threads start the first time a picture is wanted, live until
//! the call ends and carry over a reconnect, so a resume starts nothing new. They run below the
//! tick (`rt::worker_thread`).
//!
//! A compressed frame cannot be skipped the way a sample of the screen can: the next one is
//! built on it. So capture is latest by construction (a late round captures now, not what was
//! missed), and it is the painted picture in, not the frames, that is limited to ten a second.
//! Every received frame is decoded, through a bounded queue; a queue that overflows is thrown out
//! whole and restarts from a keyframe. Likewise an encoded frame that cannot be sealed asks for
//! a keyframe, and a stalled tick holds the encoder back rather than losing a frame.

use crate::picture::{
    self, Ask, Encoded, Incoming, Outgoing, Painted, CAMERA_HIGH_BPS, CAMERA_LOW_BPS, SHARE_BPS,
};
use crate::session::Controls;
use crate::{out, rt};
use protocol::{MediaResponse, PictureKind};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Frames a second, when encoding keeps up.
const FPS: u32 = 15;
/// Half the rate at most this many times: 15, 7.5, 3.75.
const SLOWEST: u32 = 4;
/// Rounds in a row over their deadline before the rate halves.
const OVERRUN: u32 = 5;
/// Rounds in a row well inside their deadline before it doubles again.
const RELAXED: u32 = 45;
/// Forced keyframes on one layer, at most this often. A keyframe costs ten or more frames' worth
/// of bits, and every watcher's trouble asks for one.
const KEY_EVERY: Duration = Duration::from_secs(1);
/// Encoded frames waiting for the tick. It takes them every 20 ms.
const ENCODED: usize = 4;
/// Received frames waiting to be decoded: three seconds at the rate anyone sends.
const RECEIVED: usize = 45;
/// The picture on screen is replaced at most this often.
const PAINT_EVERY: Duration = Duration::from_millis(100);
/// How long a hang-up waits for each thread to finish what it is doing. A camera read is the
/// longest of it. The process is exiting anyway: this is so a file is not left behind.
const STOP_WITHIN: Duration = Duration::from_millis(300);

/// The three things that can be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Share,
    High,
    Low,
}

/// What each track may spend, in bits a second. 0 stops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    pub share: u32,
    pub high: u32,
    pub low: u32,
}

impl Default for Rates {
    fn default() -> Self {
        Rates {
            share: SHARE_BPS,
            high: CAMERA_HIGH_BPS,
            low: CAMERA_LOW_BPS,
        }
    }
}

/// Kept for the voice and the packets' headers out of what the estimate says the path can take.
const VOICE_BPS: u64 = 96_000;
/// The share of the estimate that is spent. The estimate lags a path that is getting worse.
const SPEND_PERCENT: u64 = 85;
/// Below these a track says more by stopping: a smear of text or of a face helps nobody.
const SHARE_FLOOR: u64 = 250_000;
const HIGH_FLOOR: u64 = 200_000;
const LOW_FLOOR: u64 = 60_000;
/// What a stopped track needs over its floor to start again, so a path sitting on the line does
/// not switch it on and off.
const RESTART_PERCENT: u64 = 125;
/// A rate moves in steps of this, so a wandering estimate does not reconfigure encoders.
const STEP: u64 = 10_000;

/// What each track may spend on a path good for `estimate` bits a second. The voice comes first
/// and is never asked to give way, then the camera's low layer, which a watcher can always fall
/// back on, then a share, because text matters more than a face, as the server's own plan has
/// it; the high layer gets what is left. So it is the high layer and then the share that stop.
pub fn allocate(estimate: u64, share: bool, camera: bool, was: Rates) -> Rates {
    let mut left = (estimate * SPEND_PERCENT / 100).saturating_sub(VOICE_BPS);
    let mut take = |wanted: bool, most: u32, floor: u64, was: u32| -> u32 {
        let needs = if was > 0 { floor } else { floor * RESTART_PERCENT / 100 };
        if !wanted || left < needs {
            return 0;
        }
        let got = left.min(u64::from(most)) / STEP * STEP;
        left -= got;
        got as u32
    };
    let low = if camera { take(true, CAMERA_LOW_BPS, 0, 1).max(LOW_FLOOR as u32) } else { 0 };
    let share = take(share, SHARE_BPS, SHARE_FLOOR, was.share);
    let high = take(camera, CAMERA_HIGH_BPS, HIGH_FLOOR, was.high);
    Rates { share, high, low }
}

/// What to have the estimator look for: what these tracks would spend, with the voice.
pub fn desired(share: bool, camera: bool) -> u64 {
    let share = if share { u64::from(SHARE_BPS) } else { 0 };
    let camera = if camera { u64::from(CAMERA_HIGH_BPS + CAMERA_LOW_BPS) } else { 0 };
    let video = share + camera;
    if video == 0 {
        return 0;
    }
    (video + VOICE_BPS) * 100 / SPEND_PERCENT
}

/// What the tick wants sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wants {
    pub share: bool,
    pub camera: bool,
    /// The camera stays open: it is only waiting for a renegotiation. Off, it is closed.
    pub keep_camera: bool,
    pub rates: Rates,
}

impl Wants {
    fn any(&self) -> bool {
        self.share || self.camera
    }
}

/// What the capture thread hands the tick.
#[derive(Debug, PartialEq)]
pub enum Word {
    /// One frame, sealed for the room. `h` and `l` of one capture have the same `rtp` and `at`.
    Frame {
        track: Track,
        payload: Vec<u8>,
        rtp: u64,
        at: Instant,
    },
    /// The share or the camera cannot go on, in words.
    Failed { share: bool, words: String },
}

/// The RTP time at 90 kHz of a capture `since` the first one.
fn rtp_time(since: Duration) -> u64 {
    (since.as_micros() * 9 / 100) as u64
}

/// When the next capture is due. A fixed deadline: the period does not stretch by what the work
/// cost. Encoding that cannot make its deadline halves the rate, and gets it back once it
/// plainly could.
struct Schedule {
    next: Instant,
    /// How many times the rate was halved, as a divisor.
    slower: u32,
    over: u32,
    easy: u32,
}

impl Schedule {
    fn new(now: Instant) -> Schedule {
        Schedule {
            next: now,
            slower: 1,
            over: 0,
            easy: 0,
        }
    }

    fn period(&self) -> Duration {
        Duration::from_secs(1) * self.slower / FPS
    }

    fn fps(&self) -> f32 {
        FPS as f32 / self.slower as f32
    }

    /// Starts again from now, after nothing was wanted for a while.
    fn restart(&mut self, now: Instant) {
        self.next = now;
        self.over = 0;
        self.easy = 0;
    }

    /// A round that began at `began` ended at `ended`.
    fn done(&mut self, began: Instant, ended: Instant) {
        let work = ended.saturating_duration_since(began);
        let period = self.period();
        self.over = if work > period { self.over + 1 } else { 0 };
        self.easy = if work * 3 < period { self.easy + 1 } else { 0 };
        if self.over >= OVERRUN && self.slower < SLOWEST {
            self.slower *= 2;
            self.over = 0;
            self.easy = 0;
        } else if self.easy >= RELAXED && self.slower > 1 {
            self.slower /= 2;
            self.easy = 0;
        }
        self.next += self.period();
        // Missed rounds are gone. The next one goes now, and from now on the clock again.
        if self.next < ended {
            self.next = ended;
        }
    }
}

/// When a track may next be made to send a keyframe. A request is never forgotten, only held
/// until the track may: whoever asked still gets its picture within [`KEY_EVERY`].
#[derive(Default)]
struct KeyGate {
    asked: bool,
    last: Option<Instant>,
}

impl KeyGate {
    fn ask(&mut self) {
        self.asked = true;
    }

    /// The track starts, or the keyframe it was told to make did not come: no waiting.
    fn force(&mut self) {
        self.asked = true;
        self.last = None;
    }

    fn take(&mut self, now: Instant) -> bool {
        let due = self.asked
            && self
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= KEY_EVERY);
        if due {
            self.asked = false;
            self.last = Some(now);
        }
        due
    }
}

/// Encoders and their capture. `Outgoing`, or a stand-in for a test.
pub trait Source {
    fn share(&mut self, ask: Ask) -> Result<Encoded, String>;
    /// High then low. A layer with no `Ask` is stopped, and comes back empty.
    fn camera(&mut self, high: Option<Ask>, low: Option<Ask>)
        -> Result<(Encoded, Encoded), String>;
    fn close_camera(&mut self);
}

impl Source for Outgoing {
    fn share(&mut self, ask: Ask) -> Result<Encoded, String> {
        Outgoing::share(self, ask)
    }

    fn camera(
        &mut self,
        high: Option<Ask>,
        low: Option<Ask>,
    ) -> Result<(Encoded, Encoded), String> {
        Outgoing::camera(self, high, low)
    }

    fn close_camera(&mut self) {
        Outgoing::close_camera(self);
    }
}

#[derive(Default)]
struct Order {
    wants: Wants,
    keys: [bool; 3],
    stop: bool,
}

#[derive(Default)]
struct Orders {
    order: Mutex<Order>,
    changed: Condvar,
}

/// The capture thread's own state.
struct Capture<S> {
    source: S,
    controls: Arc<Controls>,
    tell: SyncSender<Word>,
    /// Share, high, low.
    gates: [KeyGate; 3],
    epoch: Option<Instant>,
}

impl<S: Source> Capture<S> {
    fn gate(&mut self, track: Track) -> &mut KeyGate {
        &mut self.gates[track as usize]
    }

    /// `Ask` for a track at `bps`, if it runs.
    fn ask(&mut self, track: Track, bps: u32, at: Duration, now: Instant, fps: f32) -> Option<Ask> {
        (bps > 0).then(|| Ask {
            key: self.gate(track).take(now),
            bps,
            at,
            fps,
        })
    }

    /// Seals one layer and hands it to the tick. False once the tick is gone.
    fn hand(
        &mut self,
        track: Track,
        asked: Option<Ask>,
        coded: Encoded,
        rtp: u64,
        at: Instant,
    ) -> bool {
        let wanted_key = asked.is_some_and(|ask| ask.key);
        if coded.bytes.is_empty() {
            // The encoder skipped the frame, to keep to its rate. A skipped keyframe is still owed.
            if wanted_key {
                self.gate(track).force();
            }
            return true;
        }
        let Some(payload) = self.controls.e2ee(|e2ee| e2ee.seal_video(&coded.bytes)) else {
            // Held back: nobody will have this frame, so whoever is watching needs a fresh start.
            self.gate(track).ask();
            return true;
        };
        self.tell
            .send(Word::Frame {
                track,
                payload,
                rtp,
                at,
            })
            .is_ok()
    }

    /// One round: a capture for each thing wanted, with one clock. False once the tick is gone.
    fn round(&mut self, wants: &Wants, at: Instant, fps: f32) -> bool {
        let epoch = *self.epoch.get_or_insert(at);
        let since = at.saturating_duration_since(epoch);
        let rtp = rtp_time(since);
        if wants.share {
            if let Some(ask) = self.ask(Track::Share, wants.rates.share, since, at, fps) {
                let shared = self.source.share(ask);
                let alive = match shared {
                    Ok(coded) => self.hand(Track::Share, Some(ask), coded, rtp, at),
                    Err(words) => self.refused(Track::Share, ask, words, true),
                };
                if !alive {
                    return false;
                }
            }
        }
        if wants.camera {
            let high = self.ask(Track::High, wants.rates.high, since, at, fps);
            let low = self.ask(Track::Low, wants.rates.low, since, at, fps);
            match self.source.camera(high, low) {
                Ok((high_coded, low_coded)) => {
                    if !self.hand(Track::High, high, high_coded, rtp, at)
                        || !self.hand(Track::Low, low, low_coded, rtp, at)
                    {
                        return false;
                    }
                }
                Err(words) => {
                    if let Some(ask) = high {
                        self.refused(Track::High, ask, String::new(), false);
                    }
                    if !self.refused(Track::Low, low.unwrap_or_default(), words, false) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// A capture that failed. The codec on its way is not a failure, and is not told.
    fn refused(&mut self, track: Track, ask: Ask, words: String, share: bool) -> bool {
        if ask.key {
            self.gate(track).force();
        }
        if words.is_empty() || words == picture::CODEC_LATER {
            return true;
        }
        self.tell.send(Word::Failed { share, words }).is_ok()
    }
}

/// Waits until the next round is due, or what is wanted changes. `None` to stop.
fn wait(
    orders: &Orders,
    seen: &Wants,
    schedule: &mut Schedule,
    gates: &mut [KeyGate; 3],
) -> Option<(Wants, bool)> {
    let mut order = orders.order.lock().unwrap();
    loop {
        if order.stop {
            return None;
        }
        for (asked, gate) in order.keys.iter_mut().zip(gates.iter_mut()) {
            if std::mem::take(asked) {
                gate.ask();
            }
        }
        if order.wants != *seen {
            return Some((order.wants, false));
        }
        if !order.wants.any() {
            order = orders.changed.wait(order).unwrap();
            continue;
        }
        let now = Instant::now();
        if now >= schedule.next {
            return Some((order.wants, true));
        }
        order = orders
            .changed
            .wait_timeout(order, schedule.next - now)
            .unwrap()
            .0;
    }
}

fn capture<S: Source>(mut work: Capture<S>, orders: &Orders) {
    rt::worker_thread();
    picture::warm();
    let mut seen = Wants::default();
    let mut schedule = Schedule::new(Instant::now());
    while let Some((wants, due)) = wait(orders, &seen, &mut schedule, &mut work.gates) {
        if !due {
            let now = Instant::now();
            if !seen.any() {
                schedule.restart(now);
            }
            let starts = |was: bool, rate_was: u32, is: bool, rate: u32| {
                is && rate > 0 && !(was && rate_was > 0)
            };
            if starts(seen.share, seen.rates.share, wants.share, wants.rates.share) {
                work.gate(Track::Share).force();
            }
            if starts(seen.camera, seen.rates.high, wants.camera, wants.rates.high) {
                work.gate(Track::High).force();
            }
            if starts(seen.camera, seen.rates.low, wants.camera, wants.rates.low) {
                work.gate(Track::Low).force();
            }
            if seen.keep_camera && !wants.keep_camera {
                work.source.close_camera();
            }
            seen = wants;
            continue;
        }
        let began = Instant::now();
        if !work.round(&wants, began, schedule.fps()) {
            return;
        }
        schedule.done(began, Instant::now());
    }
}

/// One received frame, not yet opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    pub data: Arc<[u8]>,
    pub key: bool,
    pub contiguous: bool,
    pub user: String,
    pub kind: PictureKind,
}

/// What the tick hands the decode thread, in order.
#[derive(Debug, PartialEq)]
enum In {
    Frame(Received),
    /// Someone else's picture from here on, or a new connection.
    Reset,
    /// Nobody is watching: the picture comes off the screen.
    Clear,
}

#[derive(Default)]
struct Queue {
    frames: VecDeque<In>,
    stop: bool,
}

#[derive(Default)]
struct Inbox {
    queue: Mutex<Queue>,
    arrived: Condvar,
    /// The decoder needs a keyframe. The tick owns the wire, so it asks.
    want_key: AtomicBool,
}

impl Inbox {
    /// False when the queue overflowed. It starts again from a keyframe.
    fn put(&self, frame: Received) -> bool {
        let mut queue = self.queue.lock().unwrap();
        let fits = queue.frames.len() < RECEIVED;
        if fits {
            queue.frames.push_back(In::Frame(frame));
        } else {
            queue.frames.clear();
            queue.frames.push_back(In::Reset);
        }
        self.arrived.notify_one();
        fits
    }

    /// Goes ahead of every frame: those belong to what this replaces.
    fn replace(&self, word: In) {
        let mut queue = self.queue.lock().unwrap();
        queue.frames.clear();
        queue.frames.push_back(word);
        self.arrived.notify_one();
    }

    fn take(&self) -> Option<In> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if queue.stop {
                return None;
            }
            if let Some(next) = queue.frames.pop_front() {
                return Some(next);
            }
            queue = self.arrived.wait(queue).unwrap();
        }
    }
}

/// The decoder and what it paints. `Incoming`, or a stand-in for a test.
pub trait Screen {
    /// Decodes one frame, always, and paints it if `paint`. `Err` when it did not decode.
    fn decode(&mut self, annex_b: &[u8], paint: bool) -> Result<Option<Painted>, ()>;
    fn reset(&mut self);
    /// The painted file goes.
    fn forget(&mut self);
}

impl Screen for Incoming {
    fn decode(&mut self, annex_b: &[u8], paint: bool) -> Result<Option<Painted>, ()> {
        Incoming::decode(self, annex_b, paint)
    }

    fn reset(&mut self) {
        Incoming::reset(self);
    }

    fn forget(&mut self) {
        Incoming::forget(self);
    }
}

/// The decode thread's own state.
struct Decode<P, F> {
    screen: P,
    controls: Arc<Controls>,
    inbox: Arc<Inbox>,
    show: F,
    wait_key: bool,
    painted: Option<Instant>,
}

impl<P: Screen, F: FnMut(MediaResponse)> Decode<P, F> {
    fn lost(&mut self) {
        self.wait_key = true;
        self.inbox.want_key.store(true, Ordering::Relaxed);
    }

    fn frame(&mut self, frame: Received) {
        if !frame.contiguous || (self.wait_key && !frame.key) {
            self.lost();
            return;
        }
        let Some(opened) = self
            .controls
            .e2ee(|e2ee| e2ee.open_video(&frame.user, &frame.data))
        else {
            self.lost();
            return;
        };
        if frame.key {
            self.wait_key = false;
        }
        let paint = self.painted.is_none_or(|at| at.elapsed() >= PAINT_EVERY);
        match self.screen.decode(&opened, paint) {
            Ok(Some(painted)) => {
                self.painted = Some(Instant::now());
                (self.show)(MediaResponse::Picture {
                    path: painted.path.display().to_string(),
                    width: painted.width,
                    height: painted.height,
                    generation: painted.generation,
                    user_id: frame.user,
                    kind: frame.kind,
                });
            }
            Ok(None) => {}
            Err(()) => self.lost(),
        }
    }

    fn run(&mut self) {
        while let Some(next) = self.inbox.take() {
            match next {
                In::Frame(frame) => self.frame(frame),
                In::Reset => {
                    self.screen.reset();
                    self.wait_key = true;
                    self.inbox.want_key.store(true, Ordering::Relaxed);
                }
                In::Clear => {
                    self.screen.reset();
                    self.screen.forget();
                    self.wait_key = true;
                    self.painted = None;
                    (self.show)(MediaResponse::Picture {
                        path: String::new(),
                        width: 0,
                        height: 0,
                        generation: 0,
                        user_id: String::new(),
                        kind: PictureKind::Camera,
                    });
                }
            }
        }
        self.screen.forget();
    }
}

/// The capture thread, seen from the tick.
struct Capturing {
    orders: Arc<Orders>,
    words: Receiver<Word>,
    /// Closes when the thread ends.
    done: Receiver<()>,
}

/// The decode thread, seen from the tick.
struct Watcher {
    inbox: Arc<Inbox>,
    done: Receiver<()>,
}

fn start_capture<S: Source>(
    make: impl FnOnce() -> S + std::marker::Send + 'static,
    controls: Arc<Controls>,
) -> std::io::Result<Capturing> {
    let orders = Arc::new(Orders::default());
    let (tell, words) = sync_channel(ENCODED);
    let (ended, done) = channel::<()>();
    let theirs = orders.clone();
    std::thread::Builder::new()
        .name("video-out".to_owned())
        .spawn(move || {
            let _ended: Sender<()> = ended;
            let work = Capture {
                source: make(),
                controls,
                tell,
                gates: Default::default(),
                epoch: None,
            };
            capture(work, &theirs);
        })?;
    Ok(Capturing {
        orders,
        words,
        done,
    })
}

fn start_decode<P: Screen>(
    make: impl FnOnce() -> P + std::marker::Send + 'static,
    controls: Arc<Controls>,
    show: impl FnMut(MediaResponse) + std::marker::Send + 'static,
) -> std::io::Result<Watcher> {
    let inbox = Arc::new(Inbox::default());
    let (ended, done) = channel::<()>();
    let theirs = inbox.clone();
    std::thread::Builder::new()
        .name("video-in".to_owned())
        .spawn(move || {
            let _ended: Sender<()> = ended;
            rt::worker_thread();
            picture::warm();
            Decode {
                screen: make(),
                controls,
                inbox: theirs,
                show,
                wait_key: true,
                painted: None,
            }
            .run();
        })?;
    Ok(Watcher { inbox, done })
}

/// Waits for a thread to end, up to [`STOP_WITHIN`].
fn ended(done: &Receiver<()>) {
    match done.recv_timeout(STOP_WITHIN) {
        Err(RecvTimeoutError::Timeout) => {
            tracing::warn!("mediad: a video thread was still busy when the call ended")
        }
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
    }
}

/// The call's two picture threads, as the tick drives them.
pub struct Video {
    controls: Arc<Controls>,
    sender: Option<Capturing>,
    watcher: Option<Watcher>,
    wants: Wants,
    /// A thread that could not start is not tried again on every turn of the loop.
    refused: bool,
}

impl Video {
    pub fn new(controls: Arc<Controls>) -> Video {
        Video {
            controls,
            sender: None,
            watcher: None,
            wants: Wants::default(),
            refused: false,
        }
    }

    fn start(&mut self, started: std::io::Result<()>) {
        if let Err(err) = started {
            self.refused = true;
            tracing::warn!("mediad: a video thread did not start: {err}");
        }
    }

    /// What should be sent now. Costs a lock only when it changes.
    pub fn want(&mut self, wants: Wants) {
        if wants == self.wants {
            return;
        }
        self.wants = wants;
        if self.sender.is_none() && wants.any() && !self.refused {
            let started = start_capture(Outgoing::default, self.controls.clone());
            let started = started.map(|sender| self.sender = Some(sender));
            self.start(started);
        }
        if let Some(sender) = &self.sender {
            sender.orders.order.lock().unwrap().wants = wants;
            sender.orders.changed.notify_all();
        }
    }

    /// The server asked this track for a keyframe.
    pub fn ask_key(&self, track: Track) {
        if let Some(sender) = &self.sender {
            sender.orders.order.lock().unwrap().keys[track as usize] = true;
        }
    }

    /// The next thing the capture thread has for the tick. Never waits.
    pub fn next(&self) -> Option<Word> {
        self.sender.as_ref()?.words.try_recv().ok()
    }

    /// Someone may be about to show a picture: the decode thread starts, and the codec with it.
    pub fn watch(&mut self) {
        if self.watcher.is_none() && !self.refused {
            let started = start_decode(Incoming::default, self.controls.clone(), |frame| {
                out::send(&frame)
            });
            let started = started.map(|watcher| self.watcher = Some(watcher));
            self.start(started);
        }
    }

    /// One received frame for the decoder. False when the queue overflowed.
    pub fn picture(&self, frame: Received) -> bool {
        self.watcher
            .as_ref()
            .is_none_or(|watcher| watcher.inbox.put(frame))
    }

    /// The picture changed hands, or the connection did.
    pub fn reset(&self) {
        if let Some(watcher) = &self.watcher {
            watcher.inbox.replace(In::Reset);
        }
    }

    /// Nobody is watching any more.
    pub fn clear(&self) {
        if let Some(watcher) = &self.watcher {
            watcher.inbox.replace(In::Clear);
        }
    }

    /// The decoder asked for a keyframe since this was last asked.
    pub fn key_wanted(&self) -> bool {
        self.watcher
            .as_ref()
            .is_some_and(|watcher| watcher.inbox.want_key.swap(false, Ordering::Relaxed))
    }

    /// The call is over: both threads finish what they hold and end.
    pub fn stop(self) {
        let sender = self.sender.map(|sender| {
            sender.orders.order.lock().unwrap().stop = true;
            sender.orders.changed.notify_all();
            // A thread waiting for room to hand a frame over is let go.
            drop(sender.words);
            sender.done
        });
        let watcher = self.watcher.map(|watcher| {
            watcher.inbox.queue.lock().unwrap().stop = true;
            watcher.inbox.arrived.notify_all();
            watcher.done
        });
        for done in sender.iter().chain(&watcher) {
            ended(done);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2ee::E2ee;
    use protocol::TinyE2ee;
    use std::path::PathBuf;

    #[test]
    fn a_good_path_gets_everything() {
        let rates = allocate(5_000_000, true, true, Rates::default());
        assert_eq!(rates, Rates::default());
        assert!(desired(true, true) > 2_150_000);
    }

    #[test]
    fn a_share_alone_follows_the_estimate_down_and_then_stops() {
        let was = Rates::default();
        let rates = allocate(1_000_000, true, false, was);
        assert_eq!(rates.share, 750_000, "85% of the estimate, less the voice");
        assert_eq!((rates.high, rates.low), (0, 0));
        assert_eq!(allocate(300_000, true, false, rates).share, 0, "under its floor a share stops");
    }

    #[test]
    fn the_high_layer_stops_first_and_the_low_layer_never_does() {
        let was = Rates::default();
        let rates = allocate(600_000, false, true, was);
        assert_eq!(rates.low, CAMERA_LOW_BPS);
        assert_eq!(rates.high, 260_000);
        let rates = allocate(400_000, false, true, rates);
        assert_eq!((rates.high, rates.low), (0, CAMERA_LOW_BPS), "h gives way before l");
        let rates = allocate(50_000, false, true, rates);
        assert_eq!(rates.low, LOW_FLOOR as u32, "l is what there is to fall back on");
    }

    #[test]
    fn with_both_on_the_share_outranks_the_high_layer() {
        let rates = allocate(1_500_000, true, true, Rates::default());
        assert_eq!(rates.low, CAMERA_LOW_BPS);
        assert!(rates.share > rates.high, "{rates:?}");
        let rates = allocate(900_000, true, true, rates);
        assert_eq!(rates.high, 0, "{rates:?}");
        assert!(rates.share > 0, "{rates:?}");
    }

    #[test]
    fn a_stopped_track_needs_a_margin_to_start_again() {
        let stopped = Rates { share: 0, high: 0, low: 0 };
        let barely = (SHARE_FLOOR + VOICE_BPS) * 100 / SPEND_PERCENT + 10_000;
        assert_eq!(allocate(barely, true, false, stopped).share, 0, "still stopped at its floor");
        assert!(allocate(barely, true, false, Rates::default()).share > 0, "while one running stays on");
    }

    #[test]
    fn nothing_to_send_looks_for_no_bandwidth() {
        assert_eq!(desired(false, false), 0, "a voice call never probes");
    }

    #[test]
    fn rtp_time_comes_from_the_clock_at_90_khz() {
        assert_eq!(rtp_time(Duration::ZERO), 0);
        assert_eq!(rtp_time(Duration::from_secs(1)), 90_000);
        assert_eq!(rtp_time(Duration::from_millis(100)), 9_000);
    }

    #[test]
    fn the_schedule_holds_its_deadlines_whatever_the_work_costs() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        let period = schedule.period();
        for n in 0..10 {
            let began = start + period * n;
            schedule.done(began, began + period / 2);
            assert_eq!(
                schedule.next,
                start + period * (n + 1),
                "the cost of the work is not added to the period"
            );
        }
    }

    #[test]
    fn encoding_that_overruns_halves_the_rate_and_it_comes_back() {
        let mut now = Instant::now();
        let mut schedule = Schedule::new(now);
        let period = schedule.period();
        for _ in 0..OVERRUN {
            let ended = now + period * 2;
            schedule.done(now, ended);
            now = ended;
        }
        assert_eq!(schedule.slower, 2);
        assert_eq!(schedule.fps(), 7.5);
        for _ in 0..OVERRUN * 3 {
            let ended = now + period * 3;
            schedule.done(now, ended);
            now = ended;
        }
        assert_eq!(schedule.slower, SLOWEST, "and it stops halving");
        for _ in 0..RELAXED * 2 {
            schedule.done(now, now + period / 4);
            now += period * 2;
        }
        assert_eq!(schedule.slower, 1);
    }

    #[test]
    fn a_keyframe_asked_for_early_is_held_not_forgotten() {
        let start = Instant::now();
        let mut gate = KeyGate::default();
        assert!(!gate.take(start), "nobody asked");
        gate.ask();
        assert!(gate.take(start));
        gate.ask();
        assert!(!gate.take(start + KEY_EVERY / 2), "at most one a second");
        assert!(
            gate.take(start + KEY_EVERY),
            "the one asked for still comes"
        );
        assert!(!gate.take(start + KEY_EVERY * 3), "once");
        gate.force();
        assert!(
            gate.take(start + KEY_EVERY * 3),
            "a track that starts does not wait"
        );
    }

    /// Stands in for the encoders. Every key asked for is noted.
    #[derive(Default)]
    struct Fake {
        share: Vec<Ask>,
        high: Vec<Ask>,
        low: Vec<Ask>,
        skip_next: bool,
        refuse: Option<String>,
        closed: u32,
    }

    fn coded(ask: Ask, name: u8) -> Encoded {
        Encoded {
            bytes: vec![name, ask.key as u8],
            key: ask.key,
        }
    }

    impl Source for Fake {
        fn share(&mut self, ask: Ask) -> Result<Encoded, String> {
            self.share.push(ask);
            if let Some(words) = self.refuse.clone() {
                return Err(words);
            }
            if std::mem::take(&mut self.skip_next) {
                return Ok(Encoded::default());
            }
            Ok(coded(ask, 1))
        }

        fn camera(
            &mut self,
            high: Option<Ask>,
            low: Option<Ask>,
        ) -> Result<(Encoded, Encoded), String> {
            self.high.extend(high);
            self.low.extend(low);
            let one = |ask: Option<Ask>, name| ask.map(|ask| coded(ask, name)).unwrap_or_default();
            Ok((one(high, 2), one(low, 3)))
        }

        fn close_camera(&mut self) {
            self.closed += 1;
        }
    }

    fn work(controls: Arc<Controls>) -> (Capture<Fake>, Receiver<Word>) {
        let (tell, words) = sync_channel(64);
        (
            Capture {
                source: Fake::default(),
                controls,
                tell,
                gates: Default::default(),
                epoch: None,
            },
            words,
        )
    }

    fn wanting(share: bool, camera: bool) -> Wants {
        Wants {
            share,
            camera,
            keep_camera: camera,
            rates: Rates::default(),
        }
    }

    #[test]
    fn a_keyframe_request_for_one_layer_leaves_the_other_alone() {
        let (mut capture, words) = work(Arc::default());
        let wants = wanting(false, true);
        let start = Instant::now();
        capture.gate(Track::Low).ask();
        assert!(capture.round(&wants, start, 15.));
        assert_eq!(capture.source.low.last().map(|ask| ask.key), Some(true));
        assert_eq!(
            capture.source.high.last().map(|ask| ask.key),
            Some(false),
            "h was not asked"
        );
        let frames: Vec<Word> = words.try_iter().collect();
        assert_eq!(frames.len(), 2);
        let (
            Word::Frame {
                rtp: high,
                at: high_at,
                ..
            },
            Word::Frame {
                rtp: low,
                at: low_at,
                ..
            },
        ) = (&frames[0], &frames[1])
        else {
            panic!("{frames:?}");
        };
        assert_eq!(
            (high, high_at),
            (low, low_at),
            "h and l of one capture share their RTP time"
        );
    }

    #[test]
    fn a_frame_sealing_drops_asks_again_and_a_skipped_keyframe_is_still_owed() {
        let controls: Arc<Controls> = Arc::default();
        let (mut capture, words) = work(controls.clone());
        let wants = wanting(true, false);
        let start = Instant::now();

        capture.gate(Track::Share).force();
        capture.source.skip_next = true;
        capture.round(&wants, start, 15.);
        assert_eq!(capture.source.share[0].key, true);
        assert_eq!(words.try_iter().count(), 0, "a skipped frame sends nothing");
        capture.round(&wants, start, 15.);
        assert_eq!(
            capture.source.share[1].key, true,
            "the keyframe that was skipped comes on the next frame"
        );
        assert_eq!(words.try_iter().count(), 1);

        // The call is to be end to end and has no keys: held back, and owed.
        let mut waiting = E2ee::default();
        waiting.offer(
            "me",
            Some(&TinyE2ee {
                room_id: "room".into(),
                identity: String::new(),
                required: false,
            }),
        );
        *controls.tiny_e2ee.lock().unwrap() = waiting;
        capture.round(&wants, start + KEY_EVERY, 15.);
        assert_eq!(
            words.try_iter().count(),
            0,
            "nothing without keys, never in clear"
        );
        *controls.tiny_e2ee.lock().unwrap() = E2ee::default();
        capture.round(&wants, start + KEY_EVERY * 2, 15.);
        assert_eq!(
            capture.source.share.last().map(|ask| ask.key),
            Some(true),
            "the dropped frame's watchers get a keyframe"
        );
    }

    #[test]
    fn a_failed_capture_says_why_and_the_codec_on_its_way_says_nothing() {
        let (mut capture, words) = work(Arc::default());
        let wants = wanting(true, false);
        capture.source.refuse = Some(picture::CODEC_LATER.to_owned());
        capture.gate(Track::Share).force();
        capture.round(&wants, Instant::now(), 15.);
        assert_eq!(words.try_iter().count(), 0);
        assert!(capture.gates[0].asked, "the first keyframe is still owed");
        capture.source.refuse = Some("No screen.".to_owned());
        capture.round(&wants, Instant::now(), 15.);
        assert_eq!(
            words.try_recv(),
            Ok(Word::Failed {
                share: true,
                words: "No screen.".to_owned()
            })
        );
    }

    #[test]
    fn the_capture_thread_runs_what_is_wanted_closes_the_camera_and_ends_when_told() {
        let shared = Arc::new(Mutex::new(0u32));
        struct Counted(Fake, Arc<Mutex<u32>>);
        impl Source for Counted {
            fn share(&mut self, ask: Ask) -> Result<Encoded, String> {
                self.0.share(ask)
            }
            fn camera(
                &mut self,
                high: Option<Ask>,
                low: Option<Ask>,
            ) -> Result<(Encoded, Encoded), String> {
                self.0.camera(high, low)
            }
            fn close_camera(&mut self) {
                *self.1.lock().unwrap() += 1;
            }
        }
        let closed = shared.clone();
        let sender =
            start_capture(move || Counted(Fake::default(), closed), Arc::default()).unwrap();
        let tell = |wants: Wants| {
            sender.orders.order.lock().unwrap().wants = wants;
            sender.orders.changed.notify_all();
        };
        tell(wanting(false, true));
        let first = sender.words.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(
                first,
                Word::Frame {
                    track: Track::High,
                    ..
                }
            ),
            "{first:?}"
        );
        tell(Wants::default());
        // What was in flight drains, and the thread goes quiet with the camera closed.
        while sender
            .words
            .recv_timeout(Duration::from_millis(200))
            .is_ok()
        {}
        assert_eq!(*shared.lock().unwrap(), 1, "the camera was closed");
        sender.orders.order.lock().unwrap().stop = true;
        sender.orders.changed.notify_all();
        assert_eq!(
            sender.done.recv_timeout(Duration::from_secs(5)),
            Err(RecvTimeoutError::Disconnected),
            "the thread ended"
        );
    }

    #[test]
    fn a_camera_waiting_on_a_renegotiation_stays_open() {
        let (mut work, _words) = work(Arc::default());
        let orders = Orders::default();
        orders.order.lock().unwrap().wants = Wants {
            keep_camera: true,
            ..Wants::default()
        };
        let seen = Wants {
            camera: true,
            keep_camera: true,
            ..Wants::default()
        };
        let mut schedule = Schedule::new(Instant::now());
        let (wants, due) = wait(&orders, &seen, &mut schedule, &mut work.gates).unwrap();
        assert!(
            !due && wants.keep_camera,
            "a reconnect turns sending off, not the camera"
        );
    }

    /// Stands in for the decoder. `fails` lists frames that do not decode, by first byte.
    #[derive(Default)]
    struct Glass {
        decoded: Arc<Mutex<Vec<(u8, bool)>>>,
        fails: Vec<u8>,
        resets: Arc<Mutex<u32>>,
        forgotten: Arc<Mutex<u32>>,
    }

    impl Screen for Glass {
        fn decode(&mut self, annex_b: &[u8], paint: bool) -> Result<Option<Painted>, ()> {
            self.decoded.lock().unwrap().push((annex_b[0], paint));
            if self.fails.contains(&annex_b[0]) {
                return Err(());
            }
            Ok(paint.then(|| Painted {
                path: PathBuf::from("frame.jpg"),
                width: 2,
                height: 2,
                generation: annex_b[0] as u64,
            }))
        }

        fn reset(&mut self) {
            *self.resets.lock().unwrap() += 1;
        }

        fn forget(&mut self) {
            *self.forgotten.lock().unwrap() += 1;
        }
    }

    fn frame(name: u8, key: bool) -> Received {
        Received {
            data: vec![name].into(),
            key,
            contiguous: true,
            user: "ada".into(),
            kind: PictureKind::Share,
        }
    }

    fn decode(
        glass: Glass,
    ) -> (
        Decode<Glass, impl FnMut(MediaResponse)>,
        Arc<Mutex<Vec<MediaResponse>>>,
    ) {
        let shown = Arc::new(Mutex::new(Vec::new()));
        let sink = shown.clone();
        let decode = Decode {
            screen: glass,
            controls: Arc::default(),
            inbox: Arc::default(),
            show: move |frame| sink.lock().unwrap().push(frame),
            wait_key: true,
            painted: None,
        };
        (decode, shown)
    }

    #[test]
    fn every_frame_is_decoded_and_only_the_paint_is_held_to_ten_a_second() {
        let glass = Glass::default();
        let decoded = glass.decoded.clone();
        let (mut decode, shown) = decode(glass);
        decode.frame(frame(1, true));
        decode.frame(frame(2, false));
        decode.frame(frame(3, false));
        assert_eq!(
            *decoded.lock().unwrap(),
            [(1, true), (2, false), (3, false)],
            "frames inside the gate still decode"
        );
        assert_eq!(shown.lock().unwrap().len(), 1);
        assert!(
            !decode.inbox.want_key.load(Ordering::Relaxed),
            "and nobody asked for a keyframe"
        );
    }

    #[test]
    fn a_keyframe_inside_the_paint_gate_still_ends_the_wait_for_one() {
        let (mut decode, _) = decode(Glass::default());
        decode.painted = Some(Instant::now());
        decode.frame(frame(1, true));
        assert!(!decode.wait_key);
    }

    #[test]
    fn nothing_decodes_until_a_keyframe_and_a_failure_starts_the_wait_again() {
        let glass = Glass {
            fails: vec![3],
            ..Glass::default()
        };
        let decoded = glass.decoded.clone();
        let (mut decode, _) = decode(glass);
        decode.frame(frame(1, false));
        assert!(decoded.lock().unwrap().is_empty(), "wait_key holds it back");
        assert!(
            decode.inbox.want_key.swap(false, Ordering::Relaxed),
            "and asks"
        );
        decode.frame(frame(2, true));
        decode.frame(frame(3, false));
        assert!(
            decode.wait_key,
            "a frame that did not decode breaks the chain"
        );
        assert!(decode.inbox.want_key.swap(false, Ordering::Relaxed));
        decode.frame(frame(4, false));
        assert_eq!(
            decoded.lock().unwrap().len(),
            2,
            "the next P-frame is not tried"
        );
        let mut gap = frame(5, true);
        gap.contiguous = false;
        decode.frame(gap);
        assert_eq!(decoded.lock().unwrap().len(), 2, "a gap is a loss");
    }

    #[test]
    fn a_queue_that_overflows_starts_again_from_a_keyframe() {
        let inbox = Inbox::default();
        for n in 0..RECEIVED {
            assert!(inbox.put(frame(n as u8, false)));
        }
        assert!(!inbox.put(frame(0, false)), "the tick is told");
        assert_eq!(inbox.take(), Some(In::Reset), "what was queued is gone");
        inbox.queue.lock().unwrap().stop = true;
        assert_eq!(inbox.take(), None);
    }

    #[test]
    fn a_reset_goes_ahead_of_the_frames_it_replaces() {
        let inbox = Inbox::default();
        inbox.put(frame(1, false));
        inbox.replace(In::Clear);
        assert_eq!(inbox.take(), Some(In::Clear));
    }

    #[test]
    fn stopping_watching_clears_the_screen_and_the_file_and_the_thread_ends_across_resets() {
        let glass = Glass::default();
        let (forgotten, resets) = (glass.forgotten.clone(), glass.resets.clone());
        let (tell, heard) = channel();
        let watcher = start_decode(
            move || glass,
            Arc::default(),
            move |frame| tell.send(frame).unwrap(),
        )
        .unwrap();
        watcher.inbox.replace(In::Reset);
        watcher.inbox.replace(In::Clear);
        let word = heard.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(word, MediaResponse::Picture { ref path, .. } if path.is_empty()),
            "{word:?}"
        );
        assert_eq!(*forgotten.lock().unwrap(), 1);
        assert!(*resets.lock().unwrap() >= 1);
        watcher.inbox.queue.lock().unwrap().stop = true;
        watcher.inbox.arrived.notify_all();
        assert_eq!(
            watcher.done.recv_timeout(Duration::from_secs(5)),
            Err(RecvTimeoutError::Disconnected),
            "the thread ended"
        );
        assert_eq!(
            *forgotten.lock().unwrap(),
            2,
            "and took its last file with it"
        );
    }

    /// The real thing on a desktop that has a screen and the OpenH264 binary.
    #[test]
    #[ignore = "needs a screen and the OpenH264 binary"]
    fn the_capture_thread_shares_a_real_screen_through_the_real_encoder() {
        let mut video = Video::new(Arc::default());
        video.want(Wants {
            share: true,
            rates: Rates::default(),
            ..Wants::default()
        });
        let (mut frames, mut bytes, started) = (0u32, 0usize, Instant::now());
        let mut first = None;
        while started.elapsed() < Duration::from_secs(3) {
            match video.next() {
                Some(Word::Frame {
                    track,
                    payload,
                    rtp,
                    ..
                }) => {
                    assert_eq!(track, Track::Share);
                    first.get_or_insert(rtp);
                    frames += 1;
                    bytes += payload.len();
                }
                Some(Word::Failed { words, .. }) => panic!("{words}"),
                None => std::thread::yield_now(),
            }
        }
        eprintln!("{frames} frames, {bytes} bytes in 3 s");
        assert!(frames >= 30, "about 15 a second: {frames}");
        video.stop();
    }

    #[test]
    fn a_resume_starts_no_new_thread_and_hang_up_ends_them_all() {
        let mut video = Video::new(Arc::default());
        // Nothing wanted: no thread at all.
        video.want(Wants::default());
        assert!(video.sender.is_none() && video.watcher.is_none());
        video.watch();
        let first = video
            .watcher
            .as_ref()
            .map(|watcher| Arc::as_ptr(&watcher.inbox));
        // What a reconnect does.
        video.reset();
        video.watch();
        assert_eq!(
            video
                .watcher
                .as_ref()
                .map(|watcher| Arc::as_ptr(&watcher.inbox)),
            first,
            "the same thread"
        );
        let done = video
            .watcher
            .as_ref()
            .map(|watcher| Arc::clone(&watcher.inbox))
            .unwrap();
        video.stop();
        assert!(done.queue.lock().unwrap().stop);
        assert_eq!(
            Arc::strong_count(&done),
            1,
            "the thread let go of it: it ended"
        );
    }
}
