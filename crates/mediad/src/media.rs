//! The 20 ms loop of a joined call. One thread owns the UDP socket and the codecs: between ticks
//! it takes packets off the socket into each sender's jitter buffer; on the tick it sends one
//! frame of the microphone and plays one frame of everyone mixed. Opening or replacing an audio
//! device happens on another thread. The tick only swaps in a device that is already open.

use crate::audio::{level, open_fresh, route_moved, Audio, AudioError, Fresh, Retired};
use crate::cue::Playing;
use crate::hear::{self, Ear, Limiter};
use crate::process::Processor;
use crate::stats::{Arrivals, Counts, Meter};
use crate::dave::{Dave, Outgoing};
use crate::jitter::{Jitter, Pop};
use crate::opus::{Decoder, Encoder, MAX_FRAME};
use crate::permission::{microphone, Microphone};
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
    /// The gain the last frame ended at (`hear`), so the next one starts from it.
    gain: Option<f32>,
    pub(crate) arrivals: Arrivals,
}

impl Stream {
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
            gain: None,
            arrivals: Arrivals::default(),
        })
    }

    /// Adds this tick's frame to the mix at `to`, from where the last frame ended.
    pub(crate) fn mix_into(&mut self, mix: &mut [i32; FRAME_SAMPLES], frame: &[i16], to: f32) {
        hear::add(mix, frame, self.gain.unwrap_or(to), to);
        self.gain = Some(to);
    }

    /// What the connection meter needs of this voice.
    pub(crate) fn counts(&self) -> Counts {
        Counts { received: self.jitter.received, lost: self.jitter.lost, jitter_ms: self.arrivals.ms() }
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
        while self.pcm.len() < FRAME_SAMPLES {
            let decoded = match self.jitter.pop() {
                Pop::Packet(packet) => self.decoder.decode(Some(&packet), 0, &mut pcm),
                Pop::Lost => {
                    // The packet after the hole may carry a copy of what was lost.
                    let rebuilt = self.jitter.next().and_then(|next| self.decoder.recover(next, &mut pcm));
                    self.rebuilt += u64::from(rebuilt.is_some());
                    rebuilt.or_else(|| self.decoder.decode(None, FRAME, &mut pcm))
                }
                Pop::Empty => break,
            };
            match decoded {
                Some(samples) => self.pcm.extend(&pcm[..samples * CHANNELS]),
                None => {
                    self.undecodable += 1;
                    if self.undecodable == 1 {
                        eprintln!("mediad: a frame from {who} did not decode");
                    }
                }
            }
        }
        (self.pcm.len() >= FRAME_SAMPLES).then(|| self.pcm.drain(..FRAME_SAMPLES).collect())
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
            self.quiet += 1;
            if muted || self.quiet >= hold.max(1) {
                self.talking = false;
                self.trail = TRAIL;
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
            self.preroll.clear();
            step.send = Some(Send::Frame(*frame));
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
        processor.capture(frame);
    }
    let heard = if captured { level(frame) } else { 0 };
    let (open, hold) = crate::talk::opening(controls.input(), &controls.keys(), heard, Instant::now());
    (gate.step(frame, captured, muted, open, hold), heard)
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
    eprintln!("mediad: audio: {err:?}");
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
}

impl Loop {
    fn receive(&mut self, datagram: &[u8]) {
        self.meter.received(datagram.len());
        if let Ok(echo) = <[u8; 8]>::try_from(datagram) {
            // Our keepalive, back from the voice server: the round trip.
            self.meter.echoed(u32::from_be_bytes([echo[0], echo[1], echo[2], echo[3]]));
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
        let stats = self.meter.second(self.streams.values().map(Stream::counts));
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
        while !self.setup.stop.load(Ordering::Relaxed) {
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
        finish_cue(&self.setup.controls, &mut self.cue, &mut self.setup.audio);
        for (ssrc, stream) in &self.streams {
            let (jitter, rebuilt) = (&stream.jitter, stream.rebuilt);
            eprintln!(
                "mediad: ssrc {ssrc}: {} lost ({rebuilt} rebuilt), {} late",
                jitter.lost, jitter.late
            );
        }
    }
}

/// Tells chatd which devices the call is using.
pub(crate) fn announce(audio: &Audio) {
    out::send(&MediaResponse::Devices { input: audio.input.clone(), output: audio.output.clone() });
}

/// Runs until `setup.stop` is set. Gives the devices back, still open, for the connection that
/// follows a move to another voice server.
pub fn run(setup: Setup) -> Audio {
    let Some(encoder) = Encoder::new() else {
        eprintln!("mediad: the Opus encoder could not be created");
        let fatal = FromMedia::Fatal(
            CallFailure::Engine,
            "The call engine could not start. Try again.".to_owned(),
        );
        let _ = setup.report.send(fatal);
        return setup.audio;
    };
    let devices = Devices::start(setup.microphone_unasked);
    let mut state = Loop {
        setup,
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
/// another, when one breaks or the system default moves, and when the microphone is allowed.
pub(crate) struct Devices {
    desk: DeviceDesk,
    /// Housekeeping asked for a reopen. The tick posts it; it does not open anything itself.
    reopen_wanted: bool,
    /// The system has not yet been asked about the microphone: watch for the answer.
    microphone_unasked: bool,
}

impl Devices {
    pub(crate) fn start(microphone_unasked: bool) -> Devices {
        Devices { desk: DeviceDesk::start(), reopen_wanted: false, microphone_unasked }
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
                eprintln!("mediad: microphone access was granted");
                self.reopen_wanted = true;
                Ok(())
            }
        }
    }

    /// Takes a finished reopen, if one is ready, and posts the next one. Never opens a device
    /// itself: that work is already running, or is posted here and finishes on a later tick.
    pub(crate) fn service(&mut self, audio: &mut Audio, controls: &Controls, ticks: u64) -> Result<(), (CallFailure, String)> {
        let desk = &mut self.desk;
        loop {
            let note = match desk.notes.try_recv() {
                Ok(note) => note,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err((CallFailure::Engine, "The call engine could not start. Try again.".to_owned())),
            };
            desk.busy = false;
            match note {
                Note::Fresh { fresh, fell_back } => {
                    if fell_back {
                        audio.want_input = None;
                        audio.want_output = None;
                    }
                    let retired = audio.install(fresh);
                    let _ = desk.orders.send(Order::Retire(retired));
                    announce(audio);
                }
                Note::Same => announce(audio),
                Note::Quiet => {}
                Note::Failed(err) => return Err(failure(err)),
            }
            if let Some(ask) = desk.queued.take() {
                let _ = desk.orders.send(Order::Open(ask));
                desk.busy = true;
            }
        }
        if desk.busy {
            return Ok(());
        }
        let chosen = controls.devices.lock().unwrap().take();
        if let Some((input, output)) = chosen {
            audio.want_input = input;
            audio.want_output = output;
            self.reopen_wanted = true;
        }
        if audio.device_broken() {
            self.reopen_wanted = true;
        }
        let poll_route = ticks.is_multiple_of(SECOND) && audio.on_devices();
        if self.reopen_wanted || poll_route {
            let ask = OpenAsk {
                want_input: audio.want_input.clone(),
                want_output: audio.want_output.clone(),
                current_input: audio.input.clone(),
                current_output: audio.output.clone(),
                sides: audio.sides(),
                broken: audio.device_broken(),
                force: self.reopen_wanted,
                mic_running: audio.mic_running(),
                simulated: !audio.on_devices(),
                pause: Duration::ZERO,
            };
            self.reopen_wanted = false;
            let _ = desk.orders.send(Order::Open(ask));
            desk.busy = true;
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
    current_input: String,
    current_output: String,
    sides: (bool, bool),
    broken: bool,
    /// The user picked a device, the callback reported a failure, or permission was just granted.
    force: bool,
    mic_running: bool,
    simulated: bool,
    /// Test-only wait before the open, so the caller can show the tick is not inside it.
    pause: Duration,
}

enum Note {
    Fresh {
        fresh: Fresh,
        fell_back: bool,
    },
    /// Stand-in devices: nothing was opened, and the call should say which names it is using.
    Same,
    /// The route was checked and it has not moved.
    Quiet,
    Failed(AudioError),
}

fn satisfy(ask: OpenAsk) -> Note {
    if !ask.pause.is_zero() {
        thread::sleep(ask.pause);
    }
    if ask.simulated {
        return if ask.force { Note::Same } else { Note::Quiet };
    }
    let moved = ask.broken
        || route_moved(
            &ask.want_input,
            &ask.current_input,
            &ask.want_output,
            &ask.current_output,
            ask.sides,
        );
    if !ask.force && !moved {
        return Note::Quiet;
    }
    match open_fresh(
        ask.want_input.as_deref(),
        ask.want_output.as_deref(),
        ask.sides,
        ask.mic_running,
    ) {
        Ok(fresh) => Note::Fresh {
            fresh,
            fell_back: false,
        },
        Err(err) => {
            eprintln!("mediad: {err:?}; falling back to the system default devices");
            match open_fresh(None, None, ask.sides, ask.mic_running) {
                Ok(fresh) => Note::Fresh {
                    fresh,
                    fell_back: true,
                },
                Err(err) => Note::Failed(err),
            }
        }
    }
}

struct DeviceDesk {
    orders: mpsc::Sender<Order>,
    notes: mpsc::Receiver<Note>,
    busy: bool,
    queued: Option<OpenAsk>,
    join: Option<thread::JoinHandle<()>>,
}

impl DeviceDesk {
    fn start() -> DeviceDesk {
        let (orders_tx, orders_rx) = mpsc::channel();
        let (notes_tx, notes_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("mediad-devices".to_owned())
            .spawn(move || {
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
            queued: None,
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
                broken: false,
                force: true,
                mic_running: true,
                simulated: true,
                pause: Duration::from_millis(200),
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
}
