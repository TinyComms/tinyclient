//! The 20 ms loop of a joined call. One thread owns the UDP socket and the codecs: between ticks
//! it takes packets off the socket into each sender's jitter buffer; on the tick it sends one
//! frame of the microphone and plays one frame of everyone mixed. Opening or replacing an audio
//! device happens on another thread. The tick only swaps in a device that is already open.

use crate::audio::{level, open_call, open_fresh, route_moved, Audio, AudioError, Fresh, Health, Retired};
use crate::pick::{Backoff, Verdict, Watch};
use crate::cue::Playing;
use crate::hear::{self, Ear, Limiter};
use crate::process::Processor;
use crate::stats::{Arrivals, Counts, Meter};
use crate::dave::{Dave, Outgoing};
use crate::jitter::{Jitter, Pop};
use crate::opus::{Decoder, Encoder, MAX_FRAME};
use crate::pace;
use crate::permission::{microphone, Microphone};
use crate::discord_video;
use crate::rtp::{Cipher, Sender, PAYLOAD_TYPE, SILENCE};
use crate::session::Controls;
use crate::{out, CHANNELS, FRAME, FRAME_SAMPLES};
use protocol::{CallFailure, CallPath, MediaResponse};
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(20);
/// The level (see [`level`]) from which someone else's frame counts as speech: about -45 dBFS.
/// Our own gate is the user's (V6, `Tuning::gate`), this by default.
const VOICE: u8 = 25;
/// Quiet ticks before speaking is over: 260 ms, so a ring cannot flicker between words.
pub(crate) const HOLD: u32 = 13;
/// Silence frames sent after the last frame of speech, and then nothing at all. This is what
/// Discord's voice documentation asks for ("Voice Data Interpolation": five frames of
/// `0xF8 0xFF 0xFE` before a break, so the far decoder does not interpolate across it); muting
/// is such a break, and a muted client that kept sending would also keep its indicator lit.
const TRAIL: u32 = 5;
/// Frames from just under the gate that are sent ahead of the frame that opened it: 40 ms, so
/// the soft start of a word is not cut off.
const PREROLL: usize = 2;
const SECOND: u64 = 50;

pub const MICROPHONE_DENIED: &str =
    "Allow it in System Settings › Privacy & Security › Microphone, then call again.";

/// What the loop needs the gateway thread to do.
pub enum FromMedia {
    /// We started or stopped talking: the gateway is told before the audio means anything.
    Speaking(bool),
    Dave(Vec<Outgoing>),
    Fatal(CallFailure, String),
    /// The camera (`share` false) or the screen (`share` true) should be announced, or withdrawn.
    /// Nothing is sent on the same turn: the gateway announces the SSRC first.
    Picture { share: bool, on: bool },
}

pub struct Setup {
    pub socket: UdpSocket,
    pub sender: Sender,
    pub opener: Cipher,
    pub ssrc: u32,
    pub user_id: String,
    pub dave: Arc<Mutex<Dave>>,
    /// Whose audio each SSRC carries, kept by the gateway thread.
    pub users: Arc<Mutex<HashMap<u32, String>>>,
    pub controls: Arc<Controls>,
    pub report: mpsc::Sender<FromMedia>,
    pub audio: Audio,
    pub stop: Arc<AtomicBool>,
    /// The system has not yet been asked about the microphone: watch for the answer.
    pub microphone_unasked: bool,
    /// The video SSRCs Ready assigned, when it assigned any. The camera sends on these.
    pub pipe: Option<discord_video::Pipe>,
    /// Video SSRCs the gateway has announced for other people.
    pub videos: Arc<Mutex<HashMap<u32, discord_video::Remote>>>,
    /// Set by the gateway thread after opcode 12 has gone out. The camera sends only while it is set.
    pub announced: Arc<AtomicBool>,
}

/// One sender's audio on its way to the mix.
pub(crate) struct Stream {
    pub(crate) jitter: Jitter,
    decoder: Decoder,
    /// Decoded and not yet played.
    pcm: VecDeque<i16>,
    speaking: bool,
    quiet: u32,
    /// A packet that was not silence arrived since the last tick.
    pub(crate) heard: bool,
    pub(crate) last_packet: Instant,
    undecodable: u64,
    /// Lost frames rebuilt from the packet after them.
    pub(crate) rebuilt: u64,
    /// The voice was cut off in mid-speech, waiting for a packet.
    stalled: bool,
    /// Times it was, and the packet came.
    gaps: u64,
    /// The gain the last frame ended at (`hear`), so the next one starts from it.
    gain: Option<f32>,
    pub(crate) arrivals: Arrivals,
    /// The running counts as the connection meter last had them.
    reported: Counts,
    /// What the playout did, over the call and as the log last had it.
    pub(crate) playout: Playout,
    logged: Playout,
    /// Ticks since the log last said; and how many ticks in a row, now, something has stood in
    /// for packets in mid-speech.
    since_logged: u32,
    standing_in: u32,
    /// Frames played as they came since one was last stretched or squeezed.
    unpaced: u32,
}

/// Frames played as they come after one that was stretched or squeezed: 40 ms, so one frame in
/// three changes at most, as NetEq holds off between its own. A pitch period out of every frame
/// is a voice at up to twice its speed with a splice every 20 ms, and one put into every frame
/// is the same period over and over: after a stall that was twenty frames in a row, and it is
/// heard as a robot.
const PACE_REST: u32 = 2;
/// With this much more queued than the buffer keeps, the delay matters more than the sound, and
/// every other frame is squeezed: 160 ms, which a single stall on a clean link does not leave.
const PACE_FAR: usize = 8 * FRAME_SAMPLES;

/// Ticks between log lines about one voice's playout: 10 s. A line is written only when
/// something had to be made up in that time.
const PLAYOUT_LOG: u32 = 500;

/// What one voice's playout did. Each of these is heard, one way or another: the log says how
/// often, so a call that sounded wrong can be lined up with what happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Playout {
    /// Frames played as they came.
    pub played: u64,
    /// Ticks in mid-speech that the packet due was not there yet (late, or a reorder wait),
    /// and libopus carried the voice on.
    pub waited: u64,
    /// Frames lost and made up by libopus, and by the pitch loop because libopus would not.
    pub concealed: u64,
    pub looped: u64,
    /// Frames played a pitch period longer, so the buffer did not run dry; shorter, to give back
    /// delay in mid-speech; and quiet frames dropped whole for the same.
    pub stretched: u64,
    pub squeezed: u64,
    pub shed: u64,
    /// The longest anything stood in for packets in a row, in ticks.
    pub longest: u32,
}

impl Playout {
    fn since(&self, then: &Playout) -> Playout {
        Playout {
            played: self.played - then.played,
            waited: self.waited - then.waited,
            concealed: self.concealed - then.concealed,
            looped: self.looped - then.looped,
            stretched: self.stretched - then.stretched,
            squeezed: self.squeezed - then.squeezed,
            shed: self.shed - then.shed,
            longest: self.longest,
        }
    }
}

impl Stream {
    /// One voice of a Tiny call, whose sequence numbers are its RTP time ([`Jitter::clocked`]).
    pub(crate) fn clocked() -> Option<Stream> {
        let mut stream = Stream::new()?;
        stream.jitter.clocked = true;
        Some(stream)
    }

    pub(crate) fn new() -> Option<Stream> {
        Some(Stream {
            jitter: Jitter::default(),
            decoder: Decoder::new()?,
            pcm: VecDeque::new(),
            speaking: false,
            quiet: 0,
            heard: false,
            last_packet: Instant::now(),
            undecodable: 0,
            rebuilt: 0,
            stalled: false,
            gaps: 0,
            gain: None,
            arrivals: Arrivals::default(),
            reported: Counts::default(),
            playout: Playout::default(),
            logged: Playout::default(),
            since_logged: 0,
            standing_in: 0,
            unpaced: PACE_REST,
        })
    }

    /// Adds this tick's frame to the mix at `to`, from where the last frame ended.
    pub(crate) fn mix_into(&mut self, mix: &mut [i32; FRAME_SAMPLES], frame: &[i16], to: f32) {
        hear::add(mix, frame, self.gain.unwrap_or(to), to);
        self.gain = Some(to);
    }

    /// What the connection meter needs of this voice: what it counted since it was last asked.
    /// A voice that leaves takes its running counts with it, so a sum of those would go backwards.
    pub(crate) fn counts(&mut self) -> Counts {
        let jitter = &self.jitter;
        let now = Counts { received: jitter.received, lost: jitter.lost, rebuilt: self.rebuilt, late: jitter.late, gaps: self.gaps, jitter_ms: self.arrivals.ms() };
        let was = std::mem::replace(&mut self.reported, now);
        Counts {
            received: now.received - was.received,
            lost: now.lost - was.lost,
            rebuilt: now.rebuilt - was.rebuilt,
            late: now.late - was.late,
            gaps: now.gaps - was.gaps,
            jitter_ms: now.jitter_ms,
        }
    }

    /// Deafened: nothing is played, and nothing is kept to play later.
    pub(crate) fn drop_audio(&mut self) {
        self.jitter.clear();
        self.pcm.clear();
    }

    /// The next 20 ms, decoded, or `None` while nothing is due. `who` names the sender in the
    /// log.
    pub(crate) fn pull(&mut self, who: &dyn std::fmt::Display) -> Option<Vec<i16>> {
        let mut pcm = [0i16; MAX_FRAME * CHANNELS];
        self.jitter.aim(self.arrivals.ms(), self.arrivals.spread_ms());
        let mut stood_in = false;
        while self.pcm.len() < FRAME_SAMPLES {
            let decoded = match self.jitter.pop() {
                Pop::Packet(packet) => {
                    if std::mem::take(&mut self.stalled) {
                        self.gaps += 1;
                        self.jitter.deepen();
                    }
                    let decoded = self.played(&packet, &mut pcm);
                    // More is queued than this voice calls for. A quiet frame can go whole.
                    if decoded.is_some() && self.jitter.hushed && self.jitter.shed() {
                        self.playout.shed += 1;
                        continue;
                    }
                    if let Some(samples) = decoded {
                        self.playout.played += 1;
                        if self.paced(&pcm[..samples * CHANNELS]) {
                            continue;
                        }
                    }
                    decoded
                }
                // Not here yet, with a later packet already queued. In mid-speech libopus carries
                // the voice on; the packet, when it comes, decodes after that as after a loss.
                Pop::Hold if !self.jitter.hushed => {
                    stood_in = true;
                    self.playout.waited += 1;
                    Some(self.decoder.stand_in(FRAME, &mut pcm))
                }
                Pop::Hold => Some(self.decoder.bridge(FRAME, &mut pcm)),
                Pop::Spurt(packet) => {
                    // Cut off in mid-speech and never resumed: what came between is gone.
                    if std::mem::take(&mut self.stalled) {
                        self.decoder.missed(2);
                    }
                    let decoded = self.played(&packet, &mut pcm);
                    self.playout.played += u64::from(decoded.is_some());
                    decoded
                }
                Pop::Lost(frames) => {
                    stood_in = true;
                    self.decoder.missed(frames - 1);
                    // The packet after the hole may carry a copy of what was lost.
                    let rebuilt = self.jitter.next().and_then(|next| self.decoder.recover(next, &mut pcm));
                    self.rebuilt += u64::from(rebuilt.is_some());
                    rebuilt.or_else(|| self.decoder.decode(None, FRAME, &mut pcm))
                }
                Pop::Late => {
                    // After the quiet end of a sentence this is nothing at all, and the voice
                    // fades out. While someone is speaking it is a stall the next packet may
                    // end, and libopus carries the voice on meanwhile.
                    if self.jitter.hushed {
                        Some(self.decoder.late(FRAME, &mut pcm))
                    } else {
                        stood_in = true;
                        self.stalled = true;
                        self.playout.waited += 1;
                        Some(self.decoder.stand_in(FRAME, &mut pcm))
                    }
                }
                Pop::Empty => break,
            };
            match decoded {
                Some(samples) => self.pcm.extend(&pcm[..samples * CHANNELS]),
                None => {
                    self.undecodable += 1;
                    if self.undecodable == 1 {
                        tracing::warn!("mediad: a frame from {who} did not decode");
                    }
                }
            }
        }
        self.standing_in = if stood_in { self.standing_in + 1 } else { 0 };
        self.playout.longest = self.playout.longest.max(self.standing_in);
        // The decoder counts every frame libopus made up; those for a packet only waited for
        // are counted apart.
        self.playout.concealed = self.decoder.concealed.saturating_sub(self.playout.waited);
        self.playout.looped = self.decoder.looped;
        self.since_logged += 1;
        if self.since_logged >= PLAYOUT_LOG {
            self.log_playout(who, false);
        }
        (self.pcm.len() >= FRAME_SAMPLES).then(|| self.pcm.drain(..FRAME_SAMPLES).collect())
    }

    /// Plays a frame just decoded a pitch period longer while the buffer is short of where it
    /// should be, and shorter while it holds more than it needs in mid-speech. Whether it did
    /// either: the frame is in `pcm` then. The frames after one that was changed play as they
    /// are ([`PACE_REST`]).
    fn paced(&mut self, frame: &[i16]) -> bool {
        let queued = self.jitter.held() * FRAME_SAMPLES + self.pcm.len();
        let steady = self.jitter.target().saturating_sub(1) * FRAME_SAMPLES;
        let rest = if queued > steady + 2 * FRAME_SAMPLES + PACE_FAR { PACE_REST / 2 } else { PACE_REST };
        if self.unpaced < rest {
            self.unpaced += 1;
            return false;
        }
        let paced = if queued < steady {
            pace::expand(frame).inspect(|_| self.playout.stretched += 1)
        } else if !self.jitter.hushed && queued > steady + 2 * FRAME_SAMPLES {
            pace::accelerate(frame).inspect(|_| self.playout.squeezed += 1)
        } else {
            None
        };
        let Some(paced) = paced else {
            return false;
        };
        self.unpaced = 0;
        self.pcm.extend(paced);
        true
    }

    /// A line about what this voice's playout did: over the last 10 s, when anything had to be
    /// made up in them, or over the whole call (`whole`), always.
    pub(crate) fn log_playout(&mut self, who: &dyn std::fmt::Display, whole: bool) {
        let done = if whole { self.playout } else { self.playout.since(&self.logged) };
        self.since_logged = 0;
        self.logged = self.playout;
        self.playout.longest = 0;
        if !whole && done.waited + done.concealed + done.looped == 0 {
            return;
        }
        tracing::warn!(
            "mediad: {who} playout, {}: {} played, {} waited for, {} lost made up by libopus and {} by the pitch loop, {} stretched, {} squeezed, {} quiet dropped; longest stand-in {} ms; holding {} ms (spread {} ms, jitter {} ms); over the call {} lost ({} rebuilt), {} late",
            if whole { "call" } else { "last 10 s" },
            done.played,
            done.waited,
            done.concealed,
            done.looped,
            done.stretched,
            done.squeezed,
            done.shed,
            done.longest * 20,
            self.jitter.target() * 20,
            self.arrivals.spread_ms(),
            self.arrivals.ms(),
            self.jitter.lost,
            self.rebuilt,
            self.jitter.late,
        );
    }

    /// Decodes a packet whose turn it is, and notes how loud it was.
    fn played(&mut self, packet: &[u8], pcm: &mut [i16]) -> Option<usize> {
        let samples = self.decoder.decode(Some(packet), 0, pcm)?;
        self.jitter.hushed = level(&pcm[..samples * CHANNELS]) < VOICE;
        Some(samples)
    }

    /// The end of a tick: whether this sender started or stopped speaking.
    pub(crate) fn hear(&mut self, loud: bool) -> Option<bool> {
        self.heard = false;
        if loud {
            self.quiet = 0;
            if !self.speaking {
                self.speaking = true;
                return Some(true);
            }
        } else if self.speaking {
            self.quiet += 1;
            if self.quiet >= HOLD {
                self.speaking = false;
                return Some(false);
            }
        }
        None
    }

    pub(crate) fn speaking(&self) -> bool {
        self.speaking
    }
}

/// What the microphone's 20 ms turns into.
pub(crate) enum Send {
    Frame([i16; FRAME_SAMPLES]),
    /// One of the silence frames that end a talk spurt.
    Silence,
}

/// One tick of the speech gate.
pub(crate) struct Step {
    /// We started or stopped talking.
    pub speaking: Option<bool>,
    /// Frames heard just before the gate opened, oldest first, to go out ahead of `send`
    /// stamped with the times they were heard at.
    pub early: Vec<[i16; FRAME_SAMPLES]>,
    pub send: Option<Send>,
}

/// The microphone's gate: sent while we talk, then a trail of silence, then nothing.
#[derive(Default)]
pub(crate) struct Gate {
    /// The last frames heard while the gate was shut and nothing was being sent.
    preroll: VecDeque<[i16; FRAME_SAMPLES]>,
    talking: bool,
    quiet: u32,
    /// Silence frames still owed after the last talk spurt.
    trail: u32,
    /// The frame that closed the gate still has to go out, ramped to silence, before the trail.
    fade_out: bool,
}

impl Gate {
    pub(crate) fn talking(&self) -> bool {
        self.talking
    }

    /// `open` is whether the microphone is to be open now (`talk::opening`: voice activation
    /// at its threshold, or a push-to-talk key), and `hold` how many quiet ticks it then stays
    /// open for.
    pub(crate) fn step(&mut self, frame: &[i16; FRAME_SAMPLES], captured: bool, muted: bool, open: bool, hold: u32) -> Step {
        let mut step = Step { speaking: None, early: Vec::new(), send: None };
        let mut opened = false;
        if captured && !muted && open {
            self.quiet = 0;
            if !self.talking {
                self.talking = true;
                opened = true;
                step.speaking = Some(true);
            }
        } else if self.talking {
            // A tick the microphone did not deliver is a hole in the send, not a quiet frame.
            if captured || muted {
                self.quiet += 1;
            }
            if muted || self.quiet >= hold.max(1) {
                self.talking = false;
                self.trail = TRAIL;
                // This frame is still the voice. A silence packet here is a step from that
                // sample to zero, which is the click at the end of a word and on mute.
                self.fade_out = captured;
                step.speaking = Some(false);
            }
        }
        if muted {
            self.preroll.clear();
        }
        if self.talking && captured {
            if opened && self.trail == 0 {
                // Nothing was sent while these were heard, so their times are free.
                step.early = self.preroll.drain(..).collect();
            }
            self.trail = 0;
            self.fade_out = false;
            self.preroll.clear();
            step.send = Some(Send::Frame(*frame));
        } else if self.fade_out {
            self.fade_out = false;
            let mut faded = *frame;
            fade_down(&mut faded);
            step.send = Some(Send::Frame(faded));
        } else if self.trail > 0 {
            self.trail -= 1;
            self.preroll.clear();
            step.send = Some(Send::Silence);
        } else if captured && !muted {
            if self.preroll.len() == PREROLL {
                self.preroll.pop_front();
            }
            self.preroll.push_back(*frame);
        } else {
            // A tick with no frame is a hole in what was heard.
            self.preroll.clear();
        }
        step
    }
}

/// What one voice's playout did over [`wifi_call`].
#[derive(Debug)]
pub struct WifiCall {
    pub playout: Playout,
    pub lost: u64,
    pub rebuilt: u64,
    /// The loudest sample played, and how much was queued (packets) against what the buffer aims
    /// for at the end, after speech went on through a stall.
    pub peak: u16,
    pub held: usize,
    pub target: usize,
}

/// Three seconds of a voiced vowel at 24 kbit/s (a speech-mode, SILK or hybrid, stream) through
/// one receive stream, arriving as over Wi-Fi: each packet up to 8 ms late, one in fifty 45 ms
/// late, a 250 ms stall at 1.5 s that then delivers its backlog at once, one in 37 lost, and two
/// in a row once, which no in-band copy can rebuild.
/// Ticks are 20 ms; each takes what has arrived by then and plays a frame.
pub fn wifi_call() -> Option<WifiCall> {
    const FRAMES: usize = 150;
    let mut encoder = Encoder::at(24_000)?;
    let mut seed = 0x5eed_u32;
    let mut sent = Vec::new();
    for f in 0..FRAMES {
        let mut pcm = [0i16; FRAME_SAMPLES];
        for (n, pair) in pcm.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
            let t = (f * FRAME + n) as f64 / 48_000.;
            let swell = 0.6 + 0.4 * (t * 3. * std::f64::consts::TAU).sin();
            let voice: f64 = (1..=10).map(|k| (t * 140. * k as f64 * std::f64::consts::TAU).sin() / k as f64).sum();
            pair.fill((voice * swell * 4_000.) as i16);
        }
        let mut packet = [0u8; 1275];
        let len = encoder.encode(&pcm, &mut packet)?;
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mut due = f as u64 * 20 + u64::from(seed >> 16) % 9;
        if f % 50 == 17 {
            due += 45;
        }
        if (75..88).contains(&f) {
            // Held up behind a stall until 1.75 s, then all at once.
            due = due.max(1_750);
        }
        if f % 37 != 20 && !(100..102).contains(&f) {
            sent.push((due, f as u16, packet[..len].to_vec()));
        }
    }
    sent.sort_by_key(|(due, sequence, _)| (*due, *sequence));
    let mut stream = Stream::new()?;
    let start = Instant::now();
    let mut peak = 0u16;
    let mut next = 0;
    for tick in 0..FRAMES as u64 + 40 {
        let now = tick * 20;
        while next < sent.len() && sent[next].0 <= now {
            let (due, sequence, packet) = &sent[next];
            stream.arrivals.arrived(u32::from(*sequence) * 960, start + Duration::from_millis(*due));
            stream.jitter.insert(*sequence, packet.clone());
            next += 1;
        }
        if let Some(frame) = stream.pull(&"wifi") {
            peak = peak.max(frame.iter().map(|sample| sample.unsigned_abs()).max().unwrap_or(0));
        }
        if tick == FRAMES as u64 - 5 {
            // Speech has gone on unbroken since the stall: what it left queued should be gone.
            let (held, target) = (stream.jitter.held(), stream.jitter.target());
            return Some(WifiCall { playout: stream.playout, lost: stream.jitter.lost, rebuilt: stream.rebuilt, peak, held, target });
        }
    }
    None
}

/// The lab's voice-playout driver: a Wi-Fi call's voice is carried on by libopus and paced,
/// never looped, saturated, or left behind by a stall.
pub fn prove_voice_playout() -> Result<(), String> {
    let call = wifi_call().ok_or("the codec did not start")?;
    let fail = |what: &str| Err(format!("{what}: {call:?}"));
    if call.playout.looped > 0 {
        return fail("the pitch loop stood in for a lost packet");
    }
    if call.peak >= 30_000 {
        return fail("a concealed frame saturated");
    }
    if call.lost == 0 || call.playout.concealed == 0 {
        return fail("nothing was concealed, so nothing was proved");
    }
    if call.playout.waited > 16 {
        return fail("the buffer ran dry on the stragglers, not only on the stall");
    }
    if call.held > call.target + 1 {
        return fail("the stall's backlog was still queued in mid-speech");
    }
    Ok(())
}

/// A missed microphone tick is a hole, not a quiet frame, so it does not spend the hangover.
pub fn prove_missed_frame() -> Result<(), &'static str> {
    let mut gate = Gate::default();
    let loud = [8_000i16; FRAME_SAMPLES];
    if !matches!(gate.step(&loud, true, false, true, 13).send, Some(Send::Frame(_))) {
        return Err("the gate did not open on a loud frame");
    }
    let missed = gate.step(&loud, false, false, false, 13);
    if missed.send.is_some() {
        return Err("a hole is not a silence packet");
    }
    if !gate.talking() {
        return Err("a missed frame closed the gate");
    }
    for _ in 0..12 {
        gate.step(&[0; FRAME_SAMPLES], true, false, false, 13);
    }
    if !gate.talking() {
        return Err("one missed frame counted toward the hangover");
    }
    let closing = gate.step(&[0; FRAME_SAMPLES], true, false, false, 13);
    if gate.talking() || !matches!(closing.send, Some(Send::Frame(_))) {
        return Err("a real quiet spell still closes the gate");
    }
    Ok(())
}

/// Ramps one frame down to silence. The first sample stays, the last is zero, so the trail of
/// silence packets that follows does not start from a loud sample.
fn fade_down(frame: &mut [i16; FRAME_SAMPLES]) {
    let last = FRAME_SAMPLES.max(1) as f32;
    for (n, sample) in frame.iter_mut().enumerate() {
        let gain = 1. - (n as f32 + 1.) / last;
        *sample = (f32::from(*sample) * gain) as i16;
    }
}

/// Everyone's voices summed, to the speaker: the user's voice level, then a cue on top. What is
/// played is also the echo canceller's reference. Gives the level of the voices alone, for the
/// meter.
pub(crate) fn mixdown(mix: &[i32; FRAME_SAMPLES], controls: &Controls, cue: &mut Option<Playing>, audio: &mut Audio, processor: &mut Processor, limiter: &mut Limiter) -> u8 {
    let tuning = controls.tuning();
    // V5: everyone's voice, before anything else is added; then held under full scale.
    let mut scaled = [0i32; FRAME_SAMPLES];
    for (out, &sum) in scaled.iter_mut().zip(mix) {
        *out = sum * i32::from(tuning.voice.min(100)) / 100;
    }
    let mut mixed = [0i16; FRAME_SAMPLES];
    limiter.run(&scaled, &mut mixed);
    // The meter in the device menu measures voices, not our own cues.
    let heard = level(&mixed);
    // A cue goes on after deafen: undeafening is heard, as is the test sound.
    if let Some(next) = controls.take_cue() {
        *cue = Some(Playing::new(next, f32::from(tuning.cues.min(100)) / 100.));
    }
    if let Some(playing) = cue {
        if !playing.mix(&mut mixed) {
            *cue = None;
        }
    }
    audio.play(&mixed);
    processor.render(&mixed);
    heard
}

/// The longest a hang-up waits for a cue to finish: the one that says we were put out.
const CUE_DRAIN: Duration = Duration::from_millis(400);

/// After hanging up: plays out a cue asked for at the end (`Disconnected`), and nothing else,
/// before the devices go.
pub(crate) fn finish_cue(controls: &Controls, cue: &mut Option<Playing>, audio: &mut Audio) {
    if !controls.hangup.load(Ordering::Relaxed) {
        return;
    }
    let until = Instant::now() + CUE_DRAIN;
    let (mut processor, mut limiter) = (Processor::new(), Limiter::default());
    while (cue.is_some() || controls.cue_waiting()) && Instant::now() < until {
        mixdown(&[0; FRAME_SAMPLES], controls, cue, audio, &mut processor, &mut limiter);
        thread::sleep(TICK);
    }
}

/// The microphone's 20 ms, made ready to send: processed, measured, and through the gate as the
/// input mode says. Gives the gate's step and the level heard.
pub(crate) fn listen(controls: &Controls, audio: &mut Audio, processor: &mut Processor, gate: &mut Gate, frame: &mut [i16; FRAME_SAMPLES]) -> (Step, u8) {
    let muted = controls.muted.load(Ordering::Relaxed);
    // The meter in the device menu stays live while muted, so the microphone can be tried.
    audio.listen(!muted || controls.metering());
    let captured = audio.capture(frame);
    processor.set(controls.processing());
    if captured {
        // Mic volume before DSP so AGC/NS see the level the user chose, and the meter matches.
        crate::process::apply_input_gain(frame, controls.tuning().input);
        processor.capture(frame);
    }
    let heard = if captured { level(frame) } else { 0 };
    let (open, hold) = crate::talk::opening(controls.input(), &controls.keys(), heard, gate.talking(), Instant::now());
    // A microphone that is gone ends what was being said as muting does: the ring goes out
    // instead of staying lit over a hole that never ends.
    (gate.step(frame, captured, muted || audio.unheard(), open, hold), heard)
}

pub const NO_MICROPHONE: &str =
    "Plug in a microphone, or choose another in Microphone and speaker.";
pub const NO_SPEAKER: &str =
    "Plug in headphones or a speaker, or choose another in Microphone and speaker.";
pub const MICROPHONE_FAULT: &str =
    "The microphone didn't start. Reconnect it or choose another in Microphone and speaker, then try again.";
pub const SPEAKER_FAULT: &str =
    "The speaker didn't start. Reconnect it or choose another in Microphone and speaker, then try again.";

/// A device failure in the dock's words. What the system said goes to the log, not the card.
// CONTRACT: `CallFailure::Audio` is one reason for four different troubles (permission, no
// microphone, no speaker, a device that would not start). The dock tells them apart by these
// sentences, which is why they are constants.
pub fn failure(err: AudioError) -> (CallFailure, String) {
    tracing::warn!("mediad: audio: {err:?}");
    let detail = match err {
        AudioError::NoMicrophone => NO_MICROPHONE,
        AudioError::NoSpeaker => NO_SPEAKER,
        AudioError::Microphone(_) => MICROPHONE_FAULT,
        AudioError::Speaker(_) => SPEAKER_FAULT,
    };
    (CallFailure::Audio, detail.to_owned())
}

struct Loop {
    setup: Setup,
    encoder: Encoder,
    streams: HashMap<u32, Stream>,
    gate: Gate,
    ticks: u64,
    keepalive: u32,
    levels: (u8, u8),
    /// The cue being played under the voices.
    cue: Option<Playing>,
    /// Opens devices away from this thread.
    devices: Devices,
    processor: Processor,
    limiter: Limiter,
    meter: Meter,
    pictures: discord_video::Drive,
}

impl Loop {
    fn receive(&mut self, datagram: &[u8]) {
        self.meter.received(datagram.len());
        if let Ok(echo) = <[u8; 8]>::try_from(datagram) {
            // Our keepalive, back from the voice server: the round trip.
            self.meter.echoed(u32::from_be_bytes([echo[0], echo[1], echo[2], echo[3]]));
            return;
        }
        if self.pictures.take(datagram, &self.setup.opener, &self.setup.dave, &self.setup.socket) {
            return;
        }
        if datagram.len() < 2 || datagram[1] & 0x7f != PAYLOAD_TYPE {
            return;
        }
        let Ok(packet) = self.setup.opener.unseal(datagram) else {
            return;
        };
        if packet.ssrc == self.setup.ssrc {
            return;
        }
        let stream = match self.streams.entry(packet.ssrc) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let Some(stream) = Stream::new() else {
                    return;
                };
                entry.insert(stream)
            }
        };
        stream.last_packet = Instant::now();
        stream.arrivals.arrived(packet.timestamp, stream.last_packet);
        if self.setup.controls.deafened.load(Ordering::Relaxed) {
            // Deafened: nothing is decrypted or decoded. That packets flow is all we know.
            stream.heard |= packet.payload != SILENCE;
            return;
        }
        let user = self
            .setup
            .users
            .lock()
            .unwrap()
            .get(&packet.ssrc)
            .and_then(|id| id.parse().ok());
        let mut again = Vec::new();
        let frame = self
            .setup
            .dave
            .lock()
            .unwrap()
            .decrypt(user, &packet.payload, &mut again);
        if !again.is_empty() {
            let _ = self.setup.report.send(FromMedia::Dave(again));
        }
        if let Some(frame) = frame {
            stream.heard |= frame != SILENCE;
            if frame == SILENCE {
                // Discord's five silence frames before a pause: how long it lasts is not lateness.
                stream.arrivals.paused();
            }
            stream.jitter.insert(packet.sequence, frame);
        }
    }

    fn send(&mut self, opus: &[u8]) {
        let Some(frame) = self.setup.dave.lock().unwrap().encrypt(opus) else {
            return;
        };
        if let Some(datagram) = self.setup.sender.seal(&frame) {
            // A send that fails is a lost packet; the heartbeat notices a dead path.
            if self.setup.socket.send(&datagram).is_ok() {
                self.meter.sent(datagram.len());
            }
        }
    }

    fn say(&self, speaking: bool) {
        let _ = self.setup.report.send(FromMedia::Speaking(speaking));
        out::send(&MediaResponse::Speaking {
            user_id: self.setup.user_id.clone(),
            speaking,
        });
    }

    fn encode(&mut self, frame: &[i16; FRAME_SAMPLES]) {
        let mut packet = [0u8; 1275];
        if let Some(len) = self.encoder.encode(frame, &mut packet) {
            self.send(&packet[..len]);
        }
    }

    /// The microphone's 20 ms: sent while we talk, then a trail of silence, then nothing.
    fn capture(&mut self) -> u8 {
        let controls = self.setup.controls.clone();
        self.encoder.set_bitrate(controls.bitrate());
        let mut frame = [0i16; FRAME_SAMPLES];
        let (step, heard) = listen(&controls, &mut self.setup.audio, &mut self.processor, &mut self.gate, &mut frame);
        if let Some(speaking) = step.speaking {
            self.say(speaking);
        }
        // What was heard just before the gate opened goes first, stamped with the times it
        // was heard at.
        self.setup.sender.rewind((step.early.len() * FRAME) as u32);
        for early in &step.early {
            self.encode(early);
            self.setup.sender.advance(FRAME as u32);
        }
        match step.send {
            Some(Send::Frame(frame)) => self.encode(&frame),
            Some(Send::Silence) => self.send(&SILENCE),
            None => {}
        }
        self.setup.sender.advance(FRAME as u32);
        heard
    }

    /// Everyone's 20 ms, mixed, each at their own volume for us.
    fn play(&mut self) -> u8 {
        let deafened = self.setup.controls.deafened.load(Ordering::Relaxed);
        let ear: Ear = self.setup.controls.ear();
        let held = self.setup.users.clone();
        let users = held.lock().unwrap();
        let who = |ssrc: &u32| users.get(ssrc).map_or("", String::as_str);
        let ducked = ear.ducking(self.streams.iter().map(|(ssrc, stream)| (who(ssrc), stream.speaking())));
        let mut mix = [0i32; FRAME_SAMPLES];
        let mut changed = Vec::new();
        for (ssrc, stream) in &mut self.streams {
            let loud = if deafened {
                stream.drop_audio();
                stream.heard
            } else {
                match stream.pull(&format_args!("ssrc {ssrc}")) {
                    Some(frame) => {
                        stream.mix_into(&mut mix, &frame, ear.factor(who(ssrc), ducked));
                        level(&frame) >= VOICE
                    }
                    None => false,
                }
            };
            if let Some(speaking) = stream.hear(loud) {
                changed.push((*ssrc, speaking));
            }
        }
        for (ssrc, speaking) in changed {
            if let Some(user_id) = users.get(&ssrc).cloned() {
                out::send(&MediaResponse::Speaking { user_id, speaking });
            }
        }
        drop(users);
        mixdown(&mix, &self.setup.controls, &mut self.cue, &mut self.setup.audio, &mut self.processor, &mut self.limiter)
    }

    /// Once a second: what does not need 20 ms timing. Device reopen is not in here.
    fn housekeeping(&mut self) -> bool {
        if let Err((reason, detail)) = self.devices.permission() {
            let _ = self.setup.report.send(FromMedia::Fatal(reason, detail));
            return false;
        }
        if self.ticks.is_multiple_of(5 * SECOND) {
            // Keeps the NAT mapping open while nobody talks. The server echoes it.
            self.keepalive = self.keepalive.wrapping_add(1);
            let mut probe = [0u8; 8];
            probe[..4].copy_from_slice(&self.keepalive.to_be_bytes());
            if self.setup.socket.send(&probe).is_ok() {
                self.meter.probe(self.keepalive);
            }
        }
        self.setup.audio.log_glitches();
        self.meter.troubles = self.setup.audio.troubles();
        let stats = self.meter.second(self.streams.values_mut().map(Stream::counts));
        self.setup.controls.publish_stats(stats);
        let users = self.setup.users.lock().unwrap();
        self.streams.retain(|ssrc, stream| {
            users.contains_key(ssrc) || stream.last_packet.elapsed() < Duration::from_secs(60)
        });
        true
    }

    fn run(&mut self) {
        announce(&self.setup.audio);
        let mut datagram = [0u8; 2048];
        let mut next = Instant::now() + TICK;
        while !self.setup.stop.load(Ordering::Relaxed) && !crate::out::over() {
            let now = Instant::now();
            if now < next {
                let _ = self
                    .setup
                    .socket
                    .set_read_timeout(Some((next - now).max(Duration::from_micros(200))));
                match self.setup.socket.recv(&mut datagram) {
                    Ok(len) => self.receive(&datagram[..len]),
                    Err(err)
                        if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    // An ICMP error surfaces here once per packet we sent; it is not ours to act on.
                    Err(_) => std::thread::sleep(Duration::from_millis(1)),
                }
                continue;
            }
            next += TICK;
            if now > next + 5 * TICK {
                // The machine slept or stalled: do not replay the missed ticks.
                next = now + TICK;
            }
            self.ticks += 1;
            let input = self.capture();
            let output = self.play();
            self.pictures.tick(&self.setup.dave, &self.setup.socket, &self.setup.report);
            if self.ticks.is_multiple_of(5)
                && self.setup.controls.metering()
                && (input, output) != self.levels
            {
                self.levels = (input, output);
                out::send(&MediaResponse::Levels { input, output });
            }
            if self.ticks.is_multiple_of(SECOND) && !self.housekeeping() {
                break;
            }
            if let Err((reason, detail)) = self.devices.service(&mut self.setup.audio, &self.setup.controls, self.ticks) {
                let _ = self.setup.report.send(FromMedia::Fatal(reason, detail));
                break;
            }
        }
        if self.gate.talking() {
            for _ in 0..TRAIL {
                self.send(&SILENCE);
            }
            self.say(false);
        }
        self.pictures.finish();
        finish_cue(&self.setup.controls, &mut self.cue, &mut self.setup.audio);
        for (ssrc, stream) in &mut self.streams {
            stream.log_playout(&format_args!("ssrc {ssrc}"), true);
        }
        sign_off(&self.setup.audio);
    }
}

/// The call's audio in one line of the log, at its end: what went wrong on the devices and how
/// far their clocks were from ours.
pub(crate) fn sign_off(audio: &Audio) {
    if !audio.on_devices() {
        return;
    }
    let troubles = audio.troubles();
    let (mic, speaker) = audio.drift_ppm();
    tracing::warn!(
        "mediad: call audio: {} microphone and {} speaker glitches, {} overruns, {} gaps, {} underruns, {} skips; clocks: microphone {mic:+} ppm, speaker {speaker:+} ppm",
        troubles.mic_glitches, troubles.speaker_glitches, troubles.overruns, troubles.gaps, troubles.underruns, troubles.skips
    );
}

/// Tells chatd which devices the call is using, and what is not as chosen.
pub(crate) fn announce(audio: &Audio) {
    let (notice, stuck) = audio.notice();
    out::send(&MediaResponse::Devices {
        input: audio.input.name.clone(),
        output: audio.output.name.clone(),
        input_id: audio.input.id.clone(),
        output_id: audio.output.id.clone(),
        notice,
        stuck,
    });
}

/// Runs until `setup.stop` is set. Gives the devices back, still open, for the connection that
/// follows a move to another voice server.
pub fn run(mut setup: Setup) -> Audio {
    // The tick is woken by the system timer: at 1 ms, not Windows' default 15.6 ms.
    let _timer = crate::rt::FineTimer::hold();
    crate::rt::tick_thread();
    let Some(encoder) = Encoder::new() else {
        tracing::warn!("mediad: the Opus encoder could not be created");
        let fatal = FromMedia::Fatal(
            CallFailure::Engine,
            "The call engine could not start. Try again.".to_owned(),
        );
        let _ = setup.report.send(fatal);
        return setup.audio;
    };
    let devices = Devices::start(setup.microphone_unasked);
    let pictures = discord_video::Drive::new(
        setup.controls.clone(),
        setup.user_id.clone(),
        setup.videos.clone(),
        setup.announced.clone(),
        setup.pipe.take(),
        false,
        true,
    );
    let mut state = Loop {
        setup,
        pictures,
        encoder,
        streams: HashMap::new(),
        gate: Gate::default(),
        ticks: 0,
        keepalive: 0,
        levels: (0, 0),
        cue: None,
        devices,
        processor: Processor::new(),
        limiter: Limiter::default(),
        meter: Meter::new(CallPath::Udp),
    };
    state.run();
    state.devices.stop();
    state.setup.audio
}

/// The devices of a running call: reopened away from the 20 ms thread when the user picks
/// another, when one breaks, when the device a choice means changes (the chosen one came back,
/// the system default moved), when one opened and never ran, and when the microphone is allowed.
/// Only the side that needs it is reopened: a microphone glitch does not stop the speaker.
pub(crate) struct Devices {
    desk: DeviceDesk,
    /// Sides housekeeping asked to reopen. The tick posts it; it does not open anything itself.
    reopen_wanted: (bool, bool),
    /// Of those, the sides reopened because they never ran: a second time is reported instead.
    rebuild: (bool, bool),
    /// A rebuild asked for and not yet installed: the side is not judged meanwhile.
    rebuilding: (bool, bool),
    /// The system has not yet been asked about the microphone: watch for the answer.
    microphone_unasked: bool,
    /// Whether each stream runs, checked a moment after it opens. Set on the first tick.
    watch: Option<[Watch; 2]>,
}

impl Devices {
    pub(crate) fn start(microphone_unasked: bool) -> Devices {
        Devices {
            desk: DeviceDesk::start(),
            reopen_wanted: (false, false),
            rebuild: (false, false),
            rebuilding: (false, false),
            microphone_unasked,
            watch: None,
        }
    }

    pub(crate) fn stop(&mut self) {
        self.desk.stop();
    }

    /// Once a second, while the system has not said whether we may hear the microphone. A
    /// refusal ends the call.
    pub(crate) fn permission(&mut self) -> Result<(), (CallFailure, String)> {
        if !self.microphone_unasked {
            return Ok(());
        }
        match microphone() {
            Some(Microphone::Denied) => Err((CallFailure::Audio, MICROPHONE_DENIED.to_owned())),
            Some(Microphone::Unasked) => Ok(()),
            _ => {
                // Allowed while we were running. A stream opened before the answer may go on
                // hearing silence, so the devices are opened afresh.
                self.microphone_unasked = false;
                tracing::warn!("mediad: microphone access was granted");
                self.reopen_wanted = (true, true);
                Ok(())
            }
        }
    }

    /// Judges each stream from its count: the microphone's frames, the speaker's callbacks.
    /// A side not expected to run (a paused microphone, a side this call does not open) is
    /// left alone. Gives what to do about each.
    fn judge(&mut self, now: Instant, counts: (u64, u64), running: (bool, bool)) -> [Verdict; 2] {
        let watch = self.watch.get_or_insert_with(|| [Watch::new(now, counts.0), Watch::new(now, counts.1)]);
        let mut verdicts = [Verdict::Fine; 2];
        for (side, (count, running, rebuilding)) in [(counts.0, running.0, self.rebuilding.0), (counts.1, running.1, self.rebuilding.1)].into_iter().enumerate() {
            if !running {
                watch[side].idle(now, count);
            } else if !rebuilding {
                verdicts[side] = watch[side].check(now, count);
            }
        }
        verdicts
    }

    /// Sides just opened are watched afresh; a rebuild is remembered, so failing again is
    /// reported rather than rebuilt forever.
    fn rewatch(&mut self, now: Instant, counts: (u64, u64), sides: (bool, bool), rebuild: (bool, bool)) {
        let Some(watch) = &mut self.watch else {
            return;
        };
        if sides.0 {
            watch[0].reopened(now, counts.0, rebuild.0);
            self.rebuilding.0 = false;
        }
        if sides.1 {
            watch[1].reopened(now, counts.1, rebuild.1);
            self.rebuilding.1 = false;
        }
    }

    /// The self-check: a stream that opened without error and does not run is rebuilt once,
    /// then the user is told and asked to switch device. True when what the user is told changed.
    fn self_check(&mut self, audio: &mut Audio) -> bool {
        let sides = audio.sides();
        let running = (sides.0 && audio.mic_running(), sides.1);
        let verdicts = self.judge(Instant::now(), audio.counts(), running);
        let mut told = false;
        for (side, verdict) in verdicts.into_iter().enumerate() {
            let stuck = if side == 0 { &mut audio.stuck.0 } else { &mut audio.stuck.1 };
            match verdict {
                Verdict::Rebuild => {
                    let name = if side == 0 { &audio.input.name } else { &audio.output.name };
                    tracing::warn!("mediad: {name:?} opened and is not running; opening it again");
                    if side == 0 {
                        (self.reopen_wanted.0, self.rebuild.0, self.rebuilding.0) = (true, true, true);
                    } else {
                        (self.reopen_wanted.1, self.rebuild.1, self.rebuilding.1) = (true, true, true);
                    }
                }
                Verdict::Stuck if !*stuck => {
                    *stuck = true;
                    told = true;
                }
                Verdict::Fine if *stuck && self.watch.as_ref().is_some_and(|w| w[side].proven()) => {
                    *stuck = false;
                    told = true;
                }
                _ => {}
            }
        }
        told
    }

    /// Takes a finished reopen, if one is ready, and posts the next one. Never opens a device
    /// itself: that work is already running, or is posted here and finishes on a later tick.
    pub(crate) fn service(&mut self, audio: &mut Audio, controls: &Controls, ticks: u64) -> Result<(), (CallFailure, String)> {
        loop {
            let note = match self.desk.notes.try_recv() {
                Ok(note) => note,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err((CallFailure::Engine, "The call engine could not start. Try again.".to_owned())),
            };
            self.desk.busy = false;
            match note {
                Note::Fresh { fresh, rebuild } => {
                    let sides = fresh.sides();
                    let retired = audio.install(fresh);
                    let _ = self.desk.orders.send(Order::Retire(retired));
                    self.rewatch(Instant::now(), audio.counts(), sides, rebuild);
                    announce(audio);
                }
                Note::Same => announce(audio),
                Note::Quiet => {}
                Note::Failed { err, fatal, backoff } => {
                    audio.backoff = backoff;
                    if fatal {
                        return Err(failure(err));
                    }
                    // What was asked for would not open and nothing stood in: the devices in
                    // use carry on, and the one asked for is tried again on a later check.
                    let (side, words) = match err {
                        AudioError::NoMicrophone | AudioError::Microphone(_) => (0, format!("The microphone you chose didn't start. Still using {}.", audio.input.name)),
                        AudioError::NoSpeaker | AudioError::Speaker(_) => (1, format!("The speaker you chose didn't start. Still using {}.", audio.output.name)),
                    };
                    tracing::warn!("mediad: {words}");
                    if side == 0 {
                        audio.input.notice = Some(words);
                    } else {
                        audio.output.notice = Some(words);
                    }
                    announce(audio);
                }
            }
        }
        if self.desk.busy {
            return Ok(());
        }
        let chosen = controls.devices.lock().unwrap().take();
        if let Some((input, output)) = chosen {
            // Picking the same device again is how a stuck one is tried once more.
            self.reopen_wanted.0 |= input != audio.want_input || audio.stuck.0;
            self.reopen_wanted.1 |= output != audio.want_output || audio.stuck.1;
            audio.want_input = input;
            audio.want_output = output;
        }
        let broken = audio.device_broken();
        self.reopen_wanted.0 |= broken.0;
        self.reopen_wanted.1 |= broken.1;
        if audio.on_devices() && self.self_check(audio) {
            announce(audio);
        }
        let poll_route = ticks.is_multiple_of(SECOND) && audio.on_devices();
        if self.reopen_wanted.0 || self.reopen_wanted.1 || poll_route {
            let ask = OpenAsk {
                want_input: audio.want_input.clone(),
                want_output: audio.want_output.clone(),
                current_input: audio.input.id.clone(),
                current_output: audio.output.id.clone(),
                sides: audio.sides(),
                broken,
                force: self.reopen_wanted,
                rebuild: self.rebuild,
                mic_running: audio.mic_running(),
                simulated: !audio.on_devices(),
                pause: Duration::ZERO,
                backoff: audio.backoff.clone(),
                health: audio.health(),
            };
            self.reopen_wanted = (false, false);
            self.rebuild = (false, false);
            let _ = self.desk.orders.send(Order::Open(ask));
            self.desk.busy = true;
        }
        Ok(())
    }
}

/// What the call thread asks the device thread to do.
enum Order {
    Open(OpenAsk),
    Retire(Retired),
    Stop,
}

struct OpenAsk {
    want_input: Option<String>,
    want_output: Option<String>,
    /// The ids of the devices open now.
    current_input: String,
    current_output: String,
    sides: (bool, bool),
    broken: (bool, bool),
    /// Sides to open whatever the route says: the user picked a device, the callback reported a
    /// failure, the stream never ran, or permission was just granted.
    force: (bool, bool),
    /// Of those, the sides reopened because they never ran.
    rebuild: (bool, bool),
    mic_running: bool,
    simulated: bool,
    /// Test-only wait before the open, so the caller can show the tick is not inside it.
    pause: Duration,
    backoff: Backoff,
    /// The call's counters, for the devices opened to count into.
    health: Arc<Health>,
}

enum Note {
    Fresh {
        fresh: Fresh,
        rebuild: (bool, bool),
    },
    /// Stand-in devices: nothing was opened, and the call should say which names it is using.
    Same,
    /// The route was checked and it has not moved.
    Quiet,
    /// Nothing would open. Fatal when it was the speaker and the one in use is broken: the call
    /// has nothing left to play on. A call without a microphone goes on ([`open_call`]).
    Failed {
        err: AudioError,
        fatal: bool,
        backoff: Backoff,
    },
}

fn satisfy(ask: OpenAsk) -> Note {
    if !ask.pause.is_zero() {
        thread::sleep(ask.pause);
    }
    let forced = ask.force.0 || ask.force.1;
    if ask.simulated {
        return if forced { Note::Same } else { Note::Quiet };
    }
    let moved = route_moved(
        (ask.want_input.as_deref(), ask.want_output.as_deref()),
        (&ask.current_input, &ask.current_output),
        ask.sides,
        &ask.backoff,
    );
    let which = (
        ask.sides.0 && (ask.force.0 || ask.broken.0 || moved.0),
        ask.sides.1 && (ask.force.1 || ask.broken.1 || moved.1),
    );
    if !which.0 && !which.1 {
        return Note::Quiet;
    }
    let mut backoff = ask.backoff;
    // A microphone that broke, or was never there, leaves the call up without one. One that
    // works stays in use when what was asked for instead will not open.
    let open = if ask.broken.0 || ask.current_input.is_empty() { open_call } else { open_fresh };
    match open(ask.want_input.as_deref(), ask.want_output.as_deref(), which, ask.mic_running, &mut backoff, &ask.health) {
        Ok(fresh) => {
            let keep = (ask.force.0 || ask.broken.0, ask.force.1 || ask.broken.1);
            let fresh = fresh.unless_same((&ask.current_input, &ask.current_output), keep);
            Note::Fresh { fresh, rebuild: ask.rebuild }
        }
        Err(err) => {
            let fatal = speaker_lost(&err, which, ask.broken);
            Note::Failed { err, fatal, backoff }
        }
    }
}

/// Whether a device that would not open ends the call: only a speaker, and only when the one in
/// use broke. Losing the microphone never does.
fn speaker_lost(err: &AudioError, which: (bool, bool), broken: (bool, bool)) -> bool {
    matches!(err, AudioError::NoSpeaker | AudioError::Speaker(_)) && which.1 && broken.1
}

struct DeviceDesk {
    orders: mpsc::Sender<Order>,
    notes: mpsc::Receiver<Note>,
    busy: bool,
    join: Option<thread::JoinHandle<()>>,
}

impl DeviceDesk {
    fn start() -> DeviceDesk {
        let (orders_tx, orders_rx) = mpsc::channel();
        let (notes_tx, notes_rx) = mpsc::channel();
        let join = crate::out::spawn(thread::Builder::new()
            .name("mediad-devices".to_owned()),
            move || {
                while let Ok(order) = orders_rx.recv() {
                    match order {
                        Order::Open(ask) => {
                            let _ = notes_tx.send(satisfy(ask));
                        }
                        Order::Retire(retired) => drop(retired),
                        Order::Stop => break,
                    }
                }
            })
            .expect("the device thread starts");
        DeviceDesk {
            orders: orders_tx,
            notes: notes_rx,
            busy: false,
            join: Some(join),
        }
    }

    fn stop(&mut self) {
        let _ = self.orders.send(Order::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for DeviceDesk {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;
    use crate::pick::GRACE;

    #[test]
    fn reopening_a_device_does_not_block_the_caller() {
        let desk = DeviceDesk::start();
        let started = Instant::now();
        desk.orders
            .send(Order::Open(OpenAsk {
                want_input: None,
                want_output: None,
                current_input: String::new(),
                current_output: String::new(),
                sides: (true, true),
                broken: (false, false),
                force: (true, true),
                rebuild: (false, false),
                mic_running: true,
                simulated: true,
                pause: Duration::from_millis(200),
                backoff: Backoff::default(),
                health: Arc::default(),
            }))
            .unwrap();
        assert!(desk.notes.try_recv().is_err(), "the open has not finished");
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "posting the open stalled the caller"
        );
        let note = desk
            .notes
            .recv_timeout(Duration::from_millis(500))
            .expect("the open finished");
        assert!(matches!(note, Note::Same));
        drop(desk);
    }

    #[test]
    fn a_speaker_that_opens_and_never_plays_is_rebuilt_once_then_reported() {
        let mut devices = Devices::start(false);
        let start = Instant::now();
        // The microphone delivers; the speaker's callback never runs.
        assert_eq!(devices.judge(start, (0, 0), (true, true)), [Verdict::Fine; 2]);
        let late = start + GRACE;
        assert_eq!(devices.judge(late, (960, 0), (true, true)), [Verdict::Fine, Verdict::Rebuild]);
        // The rebuild is asked for; while it is under way the speaker is not judged again.
        devices.rebuilding.1 = true;
        assert_eq!(devices.judge(late + GRACE, (1920, 0), (true, true)), [Verdict::Fine; 2]);
        // Installed, watched afresh as a rebuild: silent again means stuck, not another rebuild.
        let rebuilt = late + GRACE;
        devices.rewatch(rebuilt, (1920, 0), (false, true), (false, true));
        assert!(!devices.rebuilding.1);
        assert_eq!(devices.judge(rebuilt + GRACE, (2880, 0), (true, true)), [Verdict::Fine, Verdict::Stuck]);
        // It starts after all.
        assert_eq!(devices.judge(rebuilt + GRACE * 2, (3840, 4), (true, true)), [Verdict::Fine; 2]);
    }

    /// Before, a microphone that broke with nothing to stand in for it was fatal: the headset
    /// dropped for two seconds and the call ended with "Plug in a microphone".
    #[test]
    fn losing_the_microphone_never_ends_the_call_and_losing_the_speaker_in_use_does() {
        let (mic, speaker) = (AudioError::NoMicrophone, AudioError::NoSpeaker);
        assert!(!speaker_lost(&mic, (true, false), (true, false)), "the only microphone broke");
        assert!(!speaker_lost(&AudioError::Microphone("gone".to_owned()), (true, true), (true, true)));
        assert!(speaker_lost(&speaker, (false, true), (false, true)), "nothing is left to play on");
        assert!(speaker_lost(&AudioError::Speaker("gone".to_owned()), (true, true), (true, true)));
        // A speaker that was only asked for, while the one in use still plays, is not the end.
        assert!(!speaker_lost(&speaker, (true, true), (true, false)));
    }

    /// The whole of it on virtual devices: talking, the microphone goes, the ring goes out and
    /// nothing more is sent; it comes back and the next word is sent, with no rejoin.
    #[test]
    fn a_microphone_lost_in_mid_word_stops_the_sending_and_one_that_returns_resumes_it() {
        let (mut audio, mut mic, _speaker) = Audio::virtual_devices((48_000, 2), (48_000, 2));
        let controls = Controls::default();
        let (mut processor, mut gate) = (Processor::new(), Gate::default());
        let mut frame = [0i16; FRAME_SAMPLES];
        let loud: Vec<f32> = (0..FRAME_SAMPLES).map(|n| ((n / CHANNELS) as f32 * 440. * std::f32::consts::TAU / 48_000.).sin() * 0.5).collect();
        let mut said = None;
        for _ in 0..10 {
            mic.deliver(&loud);
            let (step, _) = listen(&controls, &mut audio, &mut processor, &mut gate, &mut frame);
            said = step.speaking.or(said);
        }
        assert_eq!(said, Some(true), "the tone opens the gate");
        // The headset drops, and nothing stands in for it.
        mic.unplug();
        assert_eq!(audio.device_broken(), (true, false));
        drop(audio.install(Fresh::without_microphone()));
        let (step, heard) = listen(&controls, &mut audio, &mut processor, &mut gate, &mut frame);
        assert_eq!((step.speaking, heard), (Some(false), 0), "the ring stayed lit with no microphone");
        let mut sent = 0;
        for _ in 0..50 {
            let (step, _) = listen(&controls, &mut audio, &mut processor, &mut gate, &mut frame);
            assert_eq!(step.speaking, None);
            sent += usize::from(matches!(step.send, Some(Send::Frame(_))));
        }
        assert_eq!(sent, 0, "a call with no microphone sent audio");
        assert!(!gate.talking());
        // A second of that is not a stream to rebuild or report: there is none.
        let mut devices = Devices::start(false);
        let start = Instant::now();
        let running = (audio.sides().0 && audio.mic_running(), audio.sides().1);
        devices.judge(start, audio.counts(), running);
        assert_eq!(devices.judge(start + GRACE * 4, (0, 500), running), [Verdict::Fine; 2]);
        // It is plugged back in: the reopen swaps it in, and the next word goes out.
        let (fresh, mut mic) = Fresh::virtual_mic((48_000, 2), &audio.health());
        drop(audio.install(fresh));
        let mut said = None;
        let mut sent = 0;
        for _ in 0..10 {
            mic.deliver(&loud);
            let (step, _) = listen(&controls, &mut audio, &mut processor, &mut gate, &mut frame);
            said = step.speaking.or(said);
            sent += usize::from(matches!(step.send, Some(Send::Frame(_))));
        }
        assert_eq!(said, Some(true), "the microphone that came back is not heard");
        assert!(sent >= 5, "{sent} frames sent after it came back");
    }

    #[test]
    fn a_muted_microphone_is_never_counted_as_broken() {
        let mut devices = Devices::start(false);
        let start = Instant::now();
        devices.judge(start, (0, 0), (false, true));
        // Muted for a long time: paused on purpose, so no frames, and no verdict.
        assert_eq!(devices.judge(start + GRACE * 10, (0, 500), (false, true)), [Verdict::Fine; 2]);
        // Unmuted: it has its grace time from now.
        let unmuted = start + GRACE * 10;
        assert_eq!(devices.judge(unmuted + GRACE / 2, (0, 600), (true, true)), [Verdict::Fine; 2]);
        assert_eq!(devices.judge(unmuted + GRACE * 2, (0, 700), (true, true))[0], Verdict::Rebuild);
    }
}

/// Loss on the way in, through the real receive path: the jitter buffer, FEC from the next
/// packet, and concealment.
#[cfg(test)]
mod loss_tests {
    use super::*;
    use crate::opus::Encoder;

    /// Two seconds and more of a voiced vowel: 140 Hz and ten harmonics, swelling and fading
    /// three times a second, as a Tiny call's encoder sends it.
    fn vowel_packets(frames: usize) -> Vec<Vec<u8>> {
        let mut encoder = Encoder::voice().unwrap();
        (0..frames)
            .map(|f| {
                let mut pcm = [0i16; FRAME_SAMPLES];
                for (n, pair) in pcm.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
                    let t = (f * FRAME + n) as f64 / 48_000.;
                    let swell = 0.6 + 0.4 * (t * 3. * std::f64::consts::TAU).sin();
                    let voice: f64 = (1..=10).map(|k| (t * 140. * k as f64 * std::f64::consts::TAU).sin() / k as f64).sum();
                    pair.fill((voice * swell * 4_000.) as i16);
                }
                let mut packet = [0u8; 1275];
                let len = encoder.encode(&pcm, &mut packet).unwrap();
                packet[..len].to_vec()
            })
            .collect()
    }

    /// Plays `packets` through a receive stream, losing those `lost` says, one tick each.
    fn heard(packets: &[Vec<u8>], lost: impl Fn(usize) -> bool) -> (Vec<i16>, Stream) {
        let mut stream = Stream::new().unwrap();
        let mut out = Vec::new();
        for (n, packet) in packets.iter().enumerate() {
            if !lost(n) {
                stream.jitter.insert(n as u16, packet.clone());
            }
            if let Some(frame) = stream.pull(&"test") {
                out.extend(frame.iter().step_by(CHANNELS));
            }
        }
        (out, stream)
    }

    /// What a live call's log showed on a clean link: nothing lost, eight packets in mid-word
    /// held up 160 ms and then here together. The voice stalls once. Before: every frame on
    /// the way into the stall was played a pitch period longer and every frame after it one
    /// shorter, twenty and more in a row, which is a voice at up to twice its speed with a
    /// splice in each frame: the "robot". Now a frame that was stretched or squeezed is
    /// followed by frames played as they are, and the delay still comes back.
    #[test]
    fn a_stall_in_mid_speech_is_given_back_a_little_at_a_time() {
        let packets = vowel_packets(160);
        let mut stream = Stream::clocked().unwrap();
        let (mut run, mut longest_run, mut last) = (0u32, 0u32, (0u64, 0u64));
        for tick in 0..packets.len() {
            // 40 to 47 come with 48; the rest on time.
            let arriving = match tick {
                40..=47 => 0..0,
                48 => 40..49,
                _ => tick..tick + 1,
            };
            for n in arriving {
                stream.jitter.insert(n as u16, packets[n].clone());
            }
            stream.pull(&"stall");
            let now = (stream.playout.played, stream.playout.stretched + stream.playout.squeezed);
            let (frames, changed) = (now.0 - last.0, now.1 - last.1);
            last = now;
            assert!(changed <= 1, "{changed} frames changed in one tick, at {tick}");
            run = if frames > 0 && changed == frames { run + 1 } else if frames > 0 { 0 } else { run };
            longest_run = longest_run.max(run);
        }
        assert_eq!(stream.jitter.lost, 0, "nothing was lost");
        assert!(stream.playout.waited >= 3, "the stall was heard as one: {:?}", stream.playout);
        assert!(stream.playout.squeezed > 0, "the delay is given back in speech: {:?}", stream.playout);
        assert!(longest_run <= 1, "{longest_run} ticks in a row played nothing as it came: {:?}", stream.playout);
        assert!(stream.jitter.held() <= stream.jitter.target() + 1, "{} still queued against {}", stream.jitter.held(), stream.jitter.target());
    }

    /// The biggest step from one sample to the next, past the codec's start.
    fn steepest(pcm: &[i16]) -> i32 {
        pcm[FRAME * 4..].windows(2).map(|w| (i32::from(w[1]) - i32::from(w[0])).abs()).max().unwrap()
    }

    /// Steps over `limit`, counted once per 10 ms.
    fn clicks(pcm: &[i16], limit: i32) -> usize {
        let mut last = None;
        let mut count = 0;
        for (n, w) in pcm[FRAME * 4..].windows(2).enumerate() {
            if (i32::from(w[1]) - i32::from(w[0])).abs() > limit && last.is_none_or(|at| n - at > 480) {
                count += 1;
                last = Some(n);
            }
        }
        count
    }

    /// Before: concealment repeated the whole last frame at half level, and the decoder went on
    /// from the speech before the gap; this test counted 21 clicks in ten seconds, the worst a
    /// step of 4007 where the voice itself steps 690 at most.
    #[test]
    fn five_percent_random_loss_and_fifty_ms_bursts_are_covered_without_clicks() {
        let packets = vowel_packets(500);
        let (clean, _) = heard(&packets, |_| false);
        let natural = steepest(&clean);
        let mut seed = 7u32;
        let random: Vec<bool> = (0..packets.len())
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed >> 8 < (1 << 24) / 20
            })
            .collect();
        // 5 % at random, and 60 ms gone at once every second.
        let (lossy, stream) = heard(&packets, |n| random[n] || (n % 50 >= 30 && n % 50 < 33));
        let lost = stream.jitter.lost;
        println!(
            "natural step {natural}, worst with loss {}, clicks {}, lost {lost}, rebuilt {}",
            steepest(&lossy),
            clicks(&lossy, natural * 3 / 2),
            stream.rebuilt
        );
        // Counted when a hole is skipped, not during the two ticks a reorder may still fill it.
        assert!(lost > 25, "{lost} lost");
        assert!(stream.rebuilt > 10, "FEC rebuilt {}", stream.rebuilt);
        assert_eq!(lossy.len(), clean.len(), "every tick had a frame");
        assert_eq!(clicks(&lossy, natural * 3 / 2), 0, "steps past the voice's own {natural}");
    }

    /// The Discord playback test: one second of a 440 Hz tone, packet 20 lost, and sequences
    /// 29 and 30 swapped. The loudest 400 ms has to stay the tone. Waiting out that reorder
    /// used to conceal, the decoder then forgot its state, and the frames after the real loss
    /// came back near 7800.
    #[test]
    fn a_lost_tone_and_a_swapped_pair_stay_at_the_tones_level() {
        let mut encoder = Encoder::new().unwrap();
        let frames: Vec<Vec<u8>> = (0..50)
            .map(|frame| {
                let mut pcm = [0i16; FRAME_SAMPLES];
                for (n, pair) in pcm.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
                    let t = (frame * FRAME + n) as f64 / 48_000.;
                    pair.fill(((t * 440. * std::f64::consts::TAU).sin() * 8000.) as i16);
                }
                let mut packet = [0u8; 1275];
                let len = encoder.encode(&pcm, &mut packet).unwrap();
                packet[..len].to_vec()
            })
            .collect();
        let (clean, _) = heard(&frames, |_| false);
        let mut order: Vec<usize> = (0..50).filter(|&n| n != 20).collect();
        order.swap(28, 29);
        let mut stream = Stream::new().unwrap();
        let mut played = Vec::new();
        for n in order {
            stream.jitter.insert(n as u16, frames[n].clone());
            if let Some(frame) = stream.pull(&"test") {
                played.extend(frame);
            }
        }
        for _ in 0..8 {
            if let Some(frame) = stream.pull(&"test") {
                played.extend(frame);
            } else {
                break;
            }
        }
        let rms = |pcm: &[i16]| (pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt();
        let loudest = |pcm: &[i16]| pcm.chunks(FRAME_SAMPLES * 20).map(rms).fold(0., f64::max);
        let (clean_level, played_level) = (loudest(&clean), loudest(&played));
        assert!((4000. ..7000.).contains(&clean_level), "clean level {clean_level}");
        assert!((4000. ..7000.).contains(&played_level), "played level {played_level}");
        assert!(played_level < clean_level * 1.15, "played {played_level} against clean {clean_level}");
        assert_eq!(stream.jitter.lost, 1, "one skipped packet");
        assert_eq!(stream.jitter.late, 0, "the swapped packet arrived inside the wait");
        assert_eq!(clicks(&played, steepest(&clean) * 3 / 2), 0, "a reorder wait must not click");
    }
}

#[cfg(test)]
mod receive_tests {
    use super::*;

    /// Two voices' running counts were summed, so one leaving took the sum backwards: nothing
    /// looked received, and the next lost packet read as 1000 per mille.
    #[test]
    fn a_voice_that_leaves_does_not_read_as_a_loss_spike() {
        let mut meter = Meter::new(CallPath::Udp);
        let (mut ada, mut bea) = (Stream::new().unwrap(), Stream::new().unwrap());
        ada.jitter.received = 1000;
        bea.jitter.received = 1000;
        meter.second([ada.counts(), bea.counts()].into_iter());
        // Bea leaves. Ada goes on, and one of her next 50 packets is lost.
        ada.jitter.received += 49;
        ada.jitter.lost += 1;
        assert_eq!(meter.second([ada.counts()].into_iter()).loss_permille, 20);
    }

    #[test]
    fn the_end_of_a_spurt_ramps_to_silence_instead_of_stepping() {
        let mut stream = Stream::new().expect("decoder");
        let mut encoder = Encoder::new().expect("encoder");
        let mut packet = [0u8; 1500];
        let mut pcm = [0i16; FRAME_SAMPLES];
        for n in 0..8u16 {
            for (i, sample) in pcm.chunks_mut(CHANNELS).enumerate() {
                let t = (n as usize * FRAME + i) as f32;
                let tone = (t / 48_000. * 440. * std::f32::consts::TAU).sin();
                sample.fill((tone * 8_000.) as i16);
            }
            let len = encoder.encode(&pcm, &mut packet).unwrap();
            stream.jitter.insert(n, packet[..len].to_vec());
        }
        let mut last = None;
        for _ in 0..8 {
            last = stream.pull(&format_args!("speaker"));
        }
        let last = last.expect("the spurt plays");
        let tail = stream.pull(&format_args!("speaker")).expect("the ending frame");
        let from = last[last.len() - CHANNELS];
        assert!(
            (i32::from(tail[0]) - i32::from(from)).abs() < 1_500,
            "the tail steps from {from} to {}",
            tail[0]
        );
        let ended = (0..2).map(|_| stream.pull(&format_args!("speaker")).expect("the ending frame")).last().unwrap();
        assert_eq!(ended[ended.len() - 1], 0, "the tail ends at silence");
        assert_eq!((stream.jitter.lost, stream.gaps), (0, 0), "nothing was lost, and nothing came back");
    }

    /// A tone, encoded as a Tiny call does.
    fn tone_packets(frames: usize) -> Vec<Vec<u8>> {
        let mut encoder = Encoder::voice().unwrap();
        (0..frames)
            .map(|f| {
                let mut pcm = [0i16; FRAME_SAMPLES];
                for (n, pair) in pcm.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
                    let t = (f * FRAME + n) as f64 / 48_000.;
                    pair.fill(((t * 440. * std::f64::consts::TAU).sin() * 8000.) as i16);
                }
                let mut packet = [0u8; 1275];
                let len = encoder.encode(&pcm, &mut packet).unwrap();
                packet[..len].to_vec()
            })
            .collect()
    }

    /// A pause through the whole receive path, as the gate leaves it: speech, the trail of
    /// silence, 200 ms of nothing with the RTP clock running, and speech again.
    #[test]
    fn the_words_after_a_pause_play_at_once_and_the_pause_loses_nothing() {
        let mut encoder = Encoder::voice().unwrap();
        let tone = |at: usize| {
            let mut pcm = [0i16; FRAME_SAMPLES];
            for (n, pair) in pcm.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
                let t = (at * FRAME + n) as f64 / 48_000.;
                pair.fill(((t * 440. * std::f64::consts::TAU).sin() * 8000.) as i16);
            }
            pcm
        };
        let mut encode = |pcm: &[i16; FRAME_SAMPLES]| {
            let mut packet = [0u8; 1275];
            let len = encoder.encode(pcm, &mut packet).unwrap();
            packet[..len].to_vec()
        };
        let mut sent: Vec<(u16, Vec<u8>)> = (0..20).map(|n| (n as u16, encode(&tone(n)))).collect();
        sent.extend((20..25).map(|n| (n as u16, encode(&[0; FRAME_SAMPLES]))));
        let resumed = 35usize;
        sent.extend((resumed..resumed + 20).map(|n| (n as u16, encode(&tone(n)))));
        let mut stream = Stream::clocked().unwrap();
        let mut heard_at = None;
        for tick in 0..resumed + 20 {
            if let Some((sequence, packet)) = sent.iter().find(|(sequence, _)| usize::from(*sequence) == tick) {
                stream.jitter.insert(*sequence, packet.clone());
            }
            let loud = stream.pull(&"pause").is_some_and(|frame| level(&frame) >= VOICE);
            if tick >= resumed && loud && heard_at.is_none() {
                heard_at = Some(tick);
            }
        }
        let late = heard_at.expect("the words play") - resumed;
        assert!(late <= 5, "the words came {late} ticks after they arrived");
        assert_eq!(stream.jitter.lost, 0, "a pause is not a loss");
        assert_eq!(stream.gaps, 0);
    }

    #[test]
    fn a_wifi_call_is_carried_on_by_libopus_and_paced_through_a_stall() {
        eprintln!("{:?}", wifi_call());
        prove_voice_playout().unwrap();
    }

    /// Speech and pauses, with 60 ms too many queued, as a stall leaves it. The speech plays a
    /// pitch period faster now and then until the extra is gone, and not one frame of it is
    /// dropped; the quiet frames after it go whole.
    #[test]
    fn a_backlog_drains_through_speech_played_faster_and_never_dropped() {
        let mut encoder = Encoder::voice().unwrap();
        let (speech, mut spare) = (tone_packets(60), Vec::new());
        for _ in 0..60 {
            let mut packet = [0u8; 1275];
            let len = encoder.encode(&[0i16; FRAME_SAMPLES], &mut packet).unwrap();
            spare.push(packet[..len].to_vec());
        }
        let mut stream = Stream::new().unwrap();
        let (sequences, mut played) = (speech.iter().chain(&spare).enumerate(), 0);
        let mut sequences = sequences.map(|(n, packet)| (n as u16, packet.clone()));
        for (n, packet) in sequences.by_ref().take(6) {
            stream.jitter.insert(n, packet);
        }
        for (n, packet) in sequences {
            stream.jitter.insert(n, packet);
            played += usize::from(stream.pull(&"backlog").is_some());
            if n == 59 {
                assert_eq!(stream.playout.shed, 0, "speech is never dropped");
                assert!(stream.playout.squeezed > 0, "speech is played faster");
                assert!(stream.jitter.held() < 6, "{} still queued at the end of the speech", stream.jitter.held());
            }
        }
        assert_eq!(played, 114);
        assert!(stream.jitter.held() <= stream.jitter.target(), "{} still queued", stream.jitter.held());
    }

    /// A packet 60 ms late in the middle of a word: the voice fades and comes back. Nothing is
    /// lost, the decoder goes on as if nothing happened, and the stall is counted.
    #[test]
    fn a_late_packet_is_a_stall_and_not_the_end_of_a_sentence() {
        let packets = tone_packets(30);
        let mut stream = Stream::new().unwrap();
        for (n, packet) in packets[..12].iter().enumerate() {
            stream.jitter.insert(n as u16, packet.clone());
        }
        for _ in 0..12 {
            stream.pull(&"late").unwrap();
        }
        let concealed: Vec<_> = (0..3).map(|_| stream.pull(&"late").unwrap()).collect();
        assert!(level(&concealed[0]) >= VOICE, "the voice carries on through the first tick");
        for (n, packet) in packets[12..].iter().enumerate() {
            stream.jitter.insert((n + 12) as u16, packet.clone());
            stream.pull(&"late").unwrap();
        }
        let counts = stream.counts();
        assert_eq!((counts.lost, counts.gaps), (0, 1), "{counts:?}");
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn muting_fades_the_last_frame_out_before_the_silence_trail() {
        let mut gate = Gate::default();
        let loud = [8_000i16; FRAME_SAMPLES];
        let opened = gate.step(&loud, true, false, true, 13);
        assert!(matches!(opened.send, Some(Send::Frame(_))));
        let closing = gate.step(&loud, true, true, false, 13);
        let Some(Send::Frame(frame)) = closing.send else {
            panic!("the last frame should fade out, not cut");
        };
        assert!(frame[0] > 7_000, "the fade starts at the voice, at {}", frame[0]);
        assert_eq!(frame[FRAME_SAMPLES - 1], 0, "the fade ends at silence");
        let trail = gate.step(&loud, true, true, false, 13);
        assert!(matches!(trail.send, Some(Send::Silence)));
    }

    #[test]
    fn a_missed_microphone_frame_does_not_count_as_quiet() {
        let mut gate = Gate::default();
        let loud = [8_000i16; FRAME_SAMPLES];
        assert!(matches!(gate.step(&loud, true, false, true, 13).send, Some(Send::Frame(_))));
        let missed = gate.step(&loud, false, false, false, 13);
        assert!(missed.send.is_none(), "a hole is not a silence packet");
        assert!(gate.talking(), "a missed frame closed the gate");
        for _ in 0..12 {
            gate.step(&[0; FRAME_SAMPLES], true, false, false, 13);
        }
        assert!(gate.talking(), "one missed frame counted toward the hangover");
        let closing = gate.step(&[0; FRAME_SAMPLES], true, false, false, 13);
        assert!(!gate.talking(), "a real quiet spell still closes the gate");
        assert!(matches!(closing.send, Some(Send::Frame(_))));
    }
}
