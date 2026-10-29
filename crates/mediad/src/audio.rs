//! The microphone and the speaker. Everything above this file sees 48 kHz stereo `i16`; a device
//! that runs at another rate or channel count is converted here, without touching its settings.
//!
//! The device callbacks run on the system's audio threads ([`crate::rt`] gives them real-time
//! scheduling on Windows) and do nothing that can wait: each moves samples through a lock-free
//! ring ([`crate::ring`]) and counts what went wrong. Everything else (resampling, processing,
//! codecs) is the call's tick's, which reads the microphone's ring and fills the speaker's once
//! every 20 ms. The devices' clocks are not ours: each side resamples by a hair more or less
//! than its nominal ratio ([`Drift`]) to keep its ring at a steady level, so neither ever runs
//! dry or over and nothing is spliced.
//!
//! A device glitch the system reports (WASAPI's "buffer underrun or overrun") is counted, not
//! acted on: the stream plays on. Only a device that stopped is reopened.
//!
//! `TINYCLIENT_MEDIAD_AUDIO` replaces the devices for tests: `null` hears silence, `tone` hears a
//! 440 Hz tone, `onset` hears that tone once a second, each time led in by 40 ms too quiet to
//! count as speech, and none of them opens a device. `TINYCLIENT_MEDIAD_SINK` names a file that then takes
//! what would have been played, as raw 48 kHz stereo `i16`.

use crate::ring::{ring, Consumer, Producer};
use crate::{rt, CHANNELS, FRAME_SAMPLES, RATE};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Seconds either ring holds at most. Far more than the tick's lateness on a busy machine.
const RING: f32 = 0.5;
/// Seconds held before the speaker is given anything, when a call starts and after it ran dry.
const PREBUFFER: f32 = 0.05;
/// Seconds over which playing fades in when it starts, and out before the cushion runs dry or
/// a backlog is skipped.
const FADE: f32 = 0.005;
/// The speaker's ring, in seconds, past which the backlog is skipped at once (with a fade out
/// and in) rather than worked off slowly: what a stalled tick leaves behind.
const PLAYOUT_MAX: f32 = 0.25;
/// The microphone's, at 48 kHz, past which the oldest is dropped (with a crossfade).
const CAPTURE_MAX: usize = FRAME_SAMPLES * 10;
/// What each side's drift control keeps at the lowest point of each second, in ms: the
/// microphone's before a frame is taken (a frame and 10 ms), the speaker's before one is added.
const CAPTURE_LOW_MS: f64 = 30.;
const PLAYOUT_LOW_MS: f64 = 20.;

/// What went wrong on a pair of devices, counted by their callbacks.
#[derive(Debug, Default)]
pub struct Health {
    /// The system said the microphone or the speaker glitched (WASAPI's data discontinuity).
    pub mic_glitches: AtomicU64,
    pub speaker_glitches: AtomicU64,
    /// The microphone's ring was full: the tick fell half a second behind.
    pub overruns: AtomicU64,
    /// The speaker's ring ran dry while playing.
    pub underruns: AtomicU64,
}

/// A call's audio troubles so far, over every device it has had.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Troubles {
    pub mic_glitches: u64,
    pub speaker_glitches: u64,
    pub overruns: u64,
    pub underruns: u64,
    /// Ticks after the microphone started that found no frame from it.
    pub gaps: u64,
    /// A backlog skipped at once, on either side.
    pub skips: u64,
}

impl Troubles {
    fn add(&mut self, health: &Health) {
        self.mic_glitches += health.mic_glitches.load(Ordering::Relaxed);
        self.speaker_glitches += health.speaker_glitches.load(Ordering::Relaxed);
        self.overruns += health.overruns.load(Ordering::Relaxed);
        self.underruns += health.underruns.load(Ordering::Relaxed);
    }
}

/// Keeps a ring at a steady level against a device clock that is not ours. Once a second it
/// looks at the lowest the ring was at a tick, smoothed, and sets how much faster or slower than
/// nominal to resample: 0.01 % for every ms off, at most 0.5 %, which no one hears as pitch.
#[derive(Debug)]
pub(crate) struct Drift {
    target_ms: f64,
    low: f64,
    seen: u32,
    smooth: Option<f64>,
    /// Resample by `1 + adjust` times the nominal step.
    pub(crate) adjust: f64,
}

const DRIFT_GAIN: f64 = 1e-4;
const DRIFT_MAX: f64 = 0.005;

impl Drift {
    pub(crate) fn new(target_ms: f64) -> Drift {
        Drift { target_ms, low: f64::MAX, seen: 0, smooth: None, adjust: 0. }
    }

    /// The ring held `ms` at a tick. Gives a new adjustment once a second.
    pub(crate) fn observe(&mut self, ms: f64) -> Option<f64> {
        self.low = self.low.min(ms);
        self.seen += 1;
        if self.seen < 50 {
            return None;
        }
        let low = std::mem::replace(&mut self.low, f64::MAX);
        self.seen = 0;
        let smooth = self.smooth.map_or(low, |smooth| smooth * 0.7 + low * 0.3);
        self.smooth = Some(smooth);
        self.adjust = ((smooth - self.target_ms) * DRIFT_GAIN).clamp(-DRIFT_MAX, DRIFT_MAX);
        Some(self.adjust)
    }

    /// The device clock against ours, in parts per million, as the control has found it.
    pub(crate) fn ppm(&self) -> i32 {
        (self.adjust * 1e6).round() as i32
    }

    /// Keeps 10 ms more than before, up to 80 ms.
    fn deepen(&mut self) {
        self.target_ms = (self.target_ms + 10.).min(80.);
    }

    fn restart(&mut self) {
        *self = Drift::new(self.target_ms);
    }
}

/// Requests the tick makes of the speaker's callback.
#[derive(Debug, Default)]
struct Control {
    /// Drop everything held (a seek in a file).
    clear: AtomicBool,
    /// Play what is held even when it is less than the cushion: a file's last moments.
    flush: AtomicBool,
    /// Skip ahead until this many samples are held. 0 asks nothing.
    trim_to: AtomicUsize,
    /// Samples held before playing (re)starts. Raised each time the speaker runs dry, so a
    /// machine that keeps the tick waiting gets a deeper cushion. 0 until set.
    fill: AtomicUsize,
}

/// Frames the microphone's callback converts at a time, without allocating.
const CHUNK: usize = 1024;

/// The microphone's callback: the device's frames, as stereo, onto the ring.
pub(crate) struct CaptureEnd {
    ring: Producer,
    channels: usize,
    health: Arc<Health>,
    stereo: [f32; CHUNK * CHANNELS],
}

impl CaptureEnd {
    fn new(ring: Producer, channels: usize, health: Arc<Health>) -> CaptureEnd {
        CaptureEnd { ring, channels: channels.max(1), health, stereo: [0.; CHUNK * CHANNELS] }
    }

    fn on_data(&mut self, data: &[f32]) {
        rt::device_thread();
        let channels = self.channels;
        for piece in data.chunks(CHUNK * channels) {
            let frames = piece.len() / channels;
            for (pair, frame) in self.stereo.as_chunks_mut::<CHANNELS>().0.iter_mut().zip(piece.chunks_exact(channels)) {
                // Mono is heard in both ears; of more than two channels the first two count.
                *pair = [frame[0], frame[if channels > 1 { 1 } else { 0 }]];
            }
            let wanted = frames * CHANNELS;
            if self.ring.push(&self.stereo[..wanted]) < wanted {
                self.health.overruns.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// The speaker's callback: what is held, faded in when it starts and out before it runs dry,
/// or silence while filling.
pub(crate) struct PlayEnd {
    ring: Consumer,
    control: Arc<Control>,
    health: Arc<Health>,
    channels: usize,
    /// Samples held before playing (re)starts, and frames each fade lasts.
    fill: usize,
    fade: usize,
    playing: bool,
    /// Frames still to fade in.
    fade_in: usize,
    /// The gain held while the ring is nearly dry: it follows the ring down at once and comes
    /// back up a fade's length later, so a refill just in time is a dip, not a step.
    nearly: f32,
    /// A skip asked for: frames still to fade out before it, and samples to skip then.
    dip: Option<(usize, usize)>,
}

impl PlayEnd {
    fn new(ring: Consumer, control: Arc<Control>, health: Arc<Health>, channels: usize, fill: usize, fade: usize) -> PlayEnd {
        control.fill.store(fill, Ordering::Relaxed);
        PlayEnd { ring, control, health, channels: channels.max(1), fill, fade: fade.max(1), playing: false, fade_in: 0, nearly: 1., dip: None }
    }

    fn on_data(&mut self, data: &mut [f32]) {
        rt::device_thread();
        if self.control.clear.swap(false, Ordering::Relaxed) {
            self.ring.skip(usize::MAX);
            self.playing = false;
            self.dip = None;
        }
        let trim = self.control.trim_to.swap(0, Ordering::Relaxed);
        if trim > 0 && self.dip.is_none() && self.playing {
            let over = self.ring.len().saturating_sub(trim) / CHANNELS;
            if over > 2 * self.fade {
                self.dip = Some((self.fade, (over - self.fade) * CHANNELS));
            }
        }
        let flush = self.control.flush.load(Ordering::Relaxed);
        self.fill = self.control.fill.load(Ordering::Relaxed).max(CHANNELS);
        let channels = self.channels;
        for frame in data.chunks_exact_mut(channels) {
            let (left, right) = self.next(flush);
            frame.fill(0.);
            if channels > 1 {
                frame[0] = left;
                frame[1] = right;
            } else {
                frame[0] = (left + right) / 2.;
            }
        }
    }

    /// One stereo frame for the device.
    fn next(&mut self, flush: bool) -> (f32, f32) {
        if !self.playing {
            let held = self.ring.len();
            if held < self.fill && !(flush && held > 0) {
                return (0., 0.);
            }
            self.playing = true;
            self.fade_in = self.fade;
            self.nearly = 1.;
        }
        let mut pair = [0f32; CHANNELS];
        if self.ring.pop(&mut pair) < CHANNELS {
            // Ran dry: we fell behind the device. Fill again before playing on. A file's end
            // is not a fault.
            self.playing = false;
            if !flush {
                self.health.underruns.fetch_add(1, Ordering::Relaxed);
            }
            return (0., 0.);
        }
        let fade = self.fade as f32;
        let mut gain = 1.;
        if self.fade_in > 0 {
            gain *= 1. - self.fade_in as f32 / fade;
            self.fade_in -= 1;
        }
        // Fade the last frames before running dry down to silence: a step from a loud sample to
        // zero is the click both ends hear when the cushion runs out.
        let near = (self.ring.len() / CHANNELS) as f32 / fade;
        self.nearly = if near < self.nearly { near } else { (self.nearly + 1. / fade).min(near).min(1.) };
        gain *= self.nearly;
        if let Some((out, skip)) = &mut self.dip {
            *out -= 1;
            gain *= *out as f32 / fade;
            if *out == 0 {
                self.ring.skip(*skip);
                self.fade_in = self.fade;
                self.dip = None;
            }
        }
        (pair[0] * gain, pair[1] * gain)
    }
}

/// A device reported trouble. A glitch is counted and the stream plays on (WASAPI reports a
/// data discontinuity and carries on); only a device that stopped is reopened. Runs on the
/// device's thread: a glitch is one atomic add.
fn on_error(side: &str, err: &cpal::Error, glitches: &AtomicU64, broken: &AtomicBool) {
    match err.kind() {
        cpal::ErrorKind::Xrun => {
            glitches.fetch_add(1, Ordering::Relaxed);
        }
        // Rerouted by the system and still playing, or a scheduling refusal: neither stops it.
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => eprintln!("mediad: {side}: {err}"),
        _ => {
            eprintln!("mediad: {side}: {err}");
            broken.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
pub enum AudioError {
    NoMicrophone,
    NoSpeaker,
    /// The microphone exists and would not open. The system's own words, for the log only.
    Microphone(String),
    Speaker(String),
}

/// Every device by name, and the system's defaults.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Listing {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: String,
    pub default_output: String,
}

fn name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

/// Asks CoreAudio what exists. Opens nothing.
pub fn listing() -> Listing {
    if let Ok(kind) = std::env::var("TINYCLIENT_MEDIAD_AUDIO") {
        return Listing {
            inputs: vec![kind.clone()],
            outputs: vec!["null".to_owned()],
            default_input: kind,
            default_output: "null".to_owned(),
        };
    }
    let host = cpal::default_host();
    let names = |devices: Option<Vec<cpal::Device>>| -> Vec<String> {
        let mut names: Vec<String> = devices
            .unwrap_or_default()
            .iter()
            .filter_map(name)
            .collect();
        names.dedup();
        names
    };
    Listing {
        inputs: names(host.input_devices().ok().map(Iterator::collect)),
        outputs: names(host.output_devices().ok().map(Iterator::collect)),
        default_input: host
            .default_input_device()
            .as_ref()
            .and_then(name)
            .unwrap_or_default(),
        default_output: host
            .default_output_device()
            .as_ref()
            .and_then(name)
            .unwrap_or_default(),
    }
}

/// Input frames each output sample is weighed from.
const TAPS: usize = 32;
/// Positions between two input frames the filter is tabulated at; between those it is
/// interpolated.
const PHASES: usize = 128;
/// The filter's centre, in frames from its first tap.
const CENTRE: usize = TAPS / 2 - 1;

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut n) = (1., 1., 1.);
    while term > sum * 1e-12 {
        term *= (x / (2. * n)).powi(2);
        sum += term;
        n += 1.;
    }
    sum
}

/// A windowed-sinc low-pass (Kaiser, about 80 dB down) as a polyphase table: row `p` is the
/// filter for an output that falls `p / PHASES` of the way from one input frame to the next.
/// `cutoff` is a fraction of the input's Nyquist frequency.
fn table(cutoff: f64) -> Vec<[f32; TAPS]> {
    const BETA: f64 = 8.;
    let half = TAPS as f64 / 2.;
    (0..=PHASES)
        .map(|phase| {
            let offset = phase as f64 / PHASES as f64;
            let mut row = [0f64; TAPS];
            for (tap, weight) in row.iter_mut().enumerate() {
                let x = tap as f64 - CENTRE as f64 - offset;
                let sinc = if x.abs() < 1e-9 {
                    1.
                } else {
                    (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * cutoff * x)
                };
                let across = (x / half).clamp(-1., 1.);
                let window = bessel_i0(BETA * (1. - across * across).sqrt()) / bessel_i0(BETA);
                *weight = cutoff * sinc * window;
            }
            // Unity gain at every phase, so a steady level does not ripple.
            let sum: f64 = row.iter().sum();
            row.map(|weight| (weight / sum) as f32)
        })
        .collect()
}

/// Band-limited conversion between two rates, one stereo stream: a polyphase windowed sinc. A
/// device at 48 kHz bypasses it unless it is [`Resampler::adaptive`], which follows a device
/// clock that is a hair off ours. Output sample `n` is the input at time `n * step`; it becomes
/// available `TAPS - CENTRE` input frames later (under half a millisecond).
pub(crate) struct Resampler {
    /// Input frames per output frame, nominal and as adjusted for drift.
    nominal: f64,
    step: f64,
    /// Where the next output falls, in frames from the start of `held`.
    position: f64,
    /// Input not yet done with, interleaved. Starts with `CENTRE` frames of silence.
    held: Vec<f32>,
    /// Empty when the rates are equal and the ratio fixed.
    table: Vec<[f32; TAPS]>,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Resampler {
        if from == to {
            let step = 1.;
            return Resampler { nominal: step, step, position: 0., held: Vec::new(), table: Vec::new() };
        }
        Resampler::adaptive(from, to)
    }

    /// Always filtered, so its ratio can be moved by [`Resampler::adjust`].
    pub(crate) fn adaptive(from: u32, to: u32) -> Resampler {
        let step = f64::from(from) / f64::from(to);
        // Going down, everything above the new Nyquist frequency has to go first.
        let cutoff = 0.95 * (f64::from(to) / f64::from(from)).min(1.);
        Resampler { nominal: step, step, position: 0., held: vec![0.; CENTRE * CHANNELS], table: table(cutoff) }
    }

    /// Takes `1 + adjust` input frames for every nominal one: positive drains the input faster.
    pub(crate) fn adjust(&mut self, adjust: f64) {
        if !self.table.is_empty() {
            self.step = self.nominal * (1. + adjust);
        }
    }

    pub(crate) fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.table.is_empty() {
            output.extend_from_slice(input);
            return;
        }
        self.held.extend_from_slice(&input[..input.len() - input.len() % CHANNELS]);
        let frames = self.held.len() / CHANNELS;
        while self.position as usize + TAPS <= frames {
            let first = self.position as usize;
            let between = (self.position - first as f64) * PHASES as f64;
            let phase = between as usize;
            let blend = (between - phase as f64) as f32;
            let (near, far) = (&self.table[phase], &self.table[phase + 1]);
            let mut sums = [0f32; CHANNELS];
            let taps = self.held[first * CHANNELS..(first + TAPS) * CHANNELS].as_chunks::<CHANNELS>().0;
            for (frame, (near, far)) in taps.iter().zip(near.iter().zip(far)) {
                let weight = near + (far - near) * blend;
                for (sum, sample) in sums.iter_mut().zip(frame) {
                    *sum += sample * weight;
                }
            }
            output.extend_from_slice(&sums);
            self.position += self.step;
        }
        let done = (self.position as usize).min(frames);
        self.held.drain(..done * CHANNELS);
        self.position -= done as f64;
    }

    /// Input frames per output frame, nominally.
    pub(crate) fn nominal(&self) -> f64 {
        self.nominal
    }
}

/// The level of a frame, 0 to 100, on a scale where speech sits in the middle. 0 is under
/// -60 dBFS.
pub fn level(pcm: &[i16]) -> u8 {
    if pcm.is_empty() {
        return 0;
    }
    let mean = pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len() as f64;
    let db = 10. * (mean / (32768f64 * 32768.)).max(1e-12).log10();
    ((db + 60.) / 60. * 100.).clamp(0., 100.) as u8
}

struct Input {
    /// `None` for a harness's virtual device, driven by a test instead of the system.
    stream: Option<cpal::Stream>,
    ring: Consumer,
    resampler: Resampler,
    drift: Drift,
    running: bool,
    /// A frame has come out since the microphone (re)started: a tick without one is a gap.
    started: bool,
    /// 48 kHz frames still to fade in after (re)starting, so the first word does not click.
    fade_in: usize,
}

struct Output {
    _stream: Option<cpal::Stream>,
    ring: Producer,
    control: Arc<Control>,
    resampler: Resampler,
    drift: Drift,
    /// The device's rate, and the samples held past which the backlog is skipped.
    rate: u32,
    most: usize,
    fill: usize,
    resampled: Vec<f32>,
    /// Underruns already answered with a deeper cushion.
    answered: u64,
}

impl Output {
    /// What is held, in ms of the device's time.
    fn held_ms(&self) -> f64 {
        (self.ring.len() / CHANNELS) as f64 * 1000. / f64::from(self.rate)
    }
}

enum Source {
    Silence,
    Tone {
        phase: f64,
    },
    /// A word a second: half a second of nothing, 40 ms of a faint tone, then the tone.
    Onset {
        at: u32,
    },
    Device(Option<Input>),
}

enum Sink {
    Null,
    File(File),
    Device(Option<Output>),
}

pub struct Audio {
    source: Source,
    sink: Sink,
    /// 48 kHz samples captured and not yet taken as a frame.
    captured: Vec<f32>,
    scratch: Vec<f32>,
    /// Set by a device callback when its device went away or changed under it.
    broken: Arc<AtomicBool>,
    /// What the devices' callbacks count, and what earlier devices of this call counted.
    health: Arc<Health>,
    past: Troubles,
    gaps: u64,
    skips: u64,
    /// The microphone glitches already logged, and when; the log gets a line a minute at most.
    logged: (u64, Option<Instant>),
    /// The devices asked for by name. `None` follows the system default.
    pub want_input: Option<String>,
    pub want_output: Option<String>,
    pub input: String,
    pub output: String,
    /// Which of the two are opened. A call opens both; a test opens only what it tries.
    sides: (bool, bool),
}

/// Devices opened away from the call thread, ready to take over.
pub struct Fresh {
    source: Source,
    sink: Sink,
    broken: Arc<AtomicBool>,
    health: Arc<Health>,
    input: String,
    output: String,
}

/// Streams a call has stopped using. Drop this away from the 20 ms thread: stopping a device
/// is the slow half of a reopen. The fields exist so that drop runs here, not on the tick.
#[allow(dead_code)]
pub struct Retired {
    source: Source,
    sink: Sink,
}

/// True when the system default we follow is no longer the device we have open.
pub(crate) fn route_moved(
    want_input: &Option<String>,
    input: &str,
    want_output: &Option<String>,
    output: &str,
    sides: (bool, bool),
) -> bool {
    let host = cpal::default_host();
    let moved = |wanted: &Option<String>, current: &str, device: Option<cpal::Device>| {
        wanted.is_none()
            && device
                .as_ref()
                .and_then(name)
                .is_some_and(|name| name != current)
    };
    (sides.0 && moved(want_input, input, host.default_input_device()))
        || (sides.1 && moved(want_output, output, host.default_output_device()))
}

/// Opens the wanted devices without touching a call that is already playing.
pub(crate) fn open_fresh(
    want_input: Option<&str>,
    want_output: Option<&str>,
    sides: (bool, bool),
    mic_running: bool,
) -> Result<Fresh, AudioError> {
    let host = cpal::default_host();
    let broken = Arc::new(AtomicBool::new(false));
    let health: Arc<Health> = Arc::default();
    let mut source = Source::Device(None);
    let mut sink = Sink::Device(None);
    let mut input_name = String::new();
    let mut output_name = String::new();
    if sides.0 {
        let microphone = find(&host, want_input, true).ok_or(AudioError::NoMicrophone)?;
        let mut input = open_input(&microphone, &broken, &health)?;
        if !mic_running {
            if let Some(stream) = &input.stream {
                let _ = stream.pause();
            }
            input.running = false;
        }
        input_name = name(&microphone).unwrap_or_default();
        source = Source::Device(Some(input));
    }
    if sides.1 {
        let speaker = find(&host, want_output, false).ok_or(AudioError::NoSpeaker)?;
        sink = Sink::Device(Some(open_output(&speaker, &broken, &health, true)?));
        output_name = name(&speaker).unwrap_or_default();
    }
    Ok(Fresh {
        source,
        sink,
        broken,
        health,
        input: input_name,
        output: output_name,
    })
}

fn find(host: &cpal::Host, wanted: Option<&str>, input: bool) -> Option<cpal::Device> {
    if let Some(wanted) = wanted {
        let devices: Option<Vec<cpal::Device>> = if input {
            host.input_devices().ok().map(Iterator::collect)
        } else {
            host.output_devices().ok().map(Iterator::collect)
        };
        if let Some(device) = devices
            .unwrap_or_default()
            .into_iter()
            .find(|d| name(d).as_deref() == Some(wanted))
        {
            return Some(device);
        }
        eprintln!("mediad: no device named {wanted:?}, using the system default");
    }
    if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

/// The microphone's side of a device, at its rate: the callback's end and the tick's.
fn input_path(rate: u32, channels: usize, health: &Arc<Health>) -> (CaptureEnd, Input) {
    let (producer, consumer) = ring((rate as f32 * RING) as usize * CHANNELS);
    let end = CaptureEnd::new(producer, channels, health.clone());
    let input = Input {
        stream: None,
        ring: consumer,
        resampler: Resampler::adaptive(rate, RATE),
        drift: Drift::new(CAPTURE_LOW_MS),
        running: true,
        started: false,
        fade_in: fade_frames(RATE),
    };
    (end, input)
}

/// The speaker's side of a device, at its rate. `adaptive` follows its clock (a call); a file's
/// player paces itself by what is queued instead.
fn output_path(rate: u32, channels: usize, health: &Arc<Health>, adaptive: bool) -> (PlayEnd, Output) {
    let (producer, consumer) = ring((rate as f32 * RING * if adaptive { 1. } else { 4. }) as usize * CHANNELS);
    let control: Arc<Control> = Arc::default();
    let fill = (rate as f32 * PREBUFFER) as usize * CHANNELS;
    let end = PlayEnd::new(consumer, control.clone(), health.clone(), channels, fill, fade_frames(rate));
    let output = Output {
        _stream: None,
        ring: producer,
        control,
        resampler: if adaptive { Resampler::adaptive(RATE, rate) } else { Resampler::new(RATE, rate) },
        drift: Drift::new(PLAYOUT_LOW_MS),
        rate,
        most: (rate as f32 * PLAYOUT_MAX) as usize * CHANNELS,
        fill,
        resampled: Vec::with_capacity(FRAME_SAMPLES * 4),
        answered: 0,
    };
    (end, output)
}

fn fade_frames(rate: u32) -> usize {
    ((rate as f32 * FADE) as usize).max(1)
}

fn open_input(device: &cpal::Device, broken: &Arc<AtomicBool>, health: &Arc<Health>) -> Result<Input, AudioError> {
    let supported = device.default_input_config().map_err(|e| AudioError::Microphone(e.to_string()))?;
    let config = supported.config();
    let (channels, rate) = (usize::from(config.channels), config.sample_rate);
    let (mut end, mut input) = input_path(rate, channels, health);
    let (flag, counts) = (broken.clone(), health.clone());
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| end.on_data(data),
            move |err| on_error("microphone", &err, &counts.mic_glitches, &flag),
            None,
        )
        .map_err(|e| AudioError::Microphone(e.to_string()))?;
    stream.play().map_err(|e| AudioError::Microphone(e.to_string()))?;
    input.stream = Some(stream);
    Ok(input)
}

fn open_output(device: &cpal::Device, broken: &Arc<AtomicBool>, health: &Arc<Health>, adaptive: bool) -> Result<Output, AudioError> {
    let supported = device.default_output_config().map_err(|e| AudioError::Speaker(e.to_string()))?;
    let config = supported.config();
    let (channels, rate) = (usize::from(config.channels), config.sample_rate);
    let (mut end, mut output) = output_path(rate, channels, health, adaptive);
    let (flag, counts) = (broken.clone(), health.clone());
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| end.on_data(data),
            move |err| on_error("speaker", &err, &counts.speaker_glitches, &flag),
            None,
        )
        .map_err(|e| AudioError::Speaker(e.to_string()))?;
    stream.play().map_err(|e| AudioError::Speaker(e.to_string()))?;
    output._stream = Some(stream);
    Ok(output)
}

/// Drops `samples.len() - keep` samples off the front, crossfading what was about to come into
/// what comes after the cut so it is a ramp and not a splice.
fn trim_front(samples: &mut Vec<f32>, keep: usize, fade: usize) {
    let len = samples.len();
    if len <= keep {
        return;
    }
    let mut drop = len - keep;
    drop -= drop % CHANNELS;
    let fade = fade.min(drop / CHANNELS).min((len - drop) / CHANNELS);
    for i in 0..fade {
        let toward = (i + 1) as f32 / (fade as f32 + 1.);
        for channel in 0..CHANNELS {
            let at = i * CHANNELS + channel;
            samples[drop + at] = samples[at] * (1. - toward) + samples[drop + at] * toward;
        }
    }
    samples.drain(..drop);
}

impl Audio {
    /// True when the environment replaced the devices, so no permission applies.
    pub fn simulated() -> bool {
        std::env::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some()
    }

    /// Opens the microphone and speaker, or their stand-ins.
    pub fn open(want_input: Option<String>, want_output: Option<String>) -> Result<Audio, AudioError> {
        Audio::open_sides(want_input, want_output, (true, true))
    }

    fn empty(want_input: Option<String>, want_output: Option<String>, sides: (bool, bool)) -> Audio {
        Audio {
            source: Source::Device(None),
            sink: Sink::Device(None),
            captured: Vec::with_capacity(CAPTURE_MAX + FRAME_SAMPLES * 4),
            scratch: vec![0.; 4096],
            broken: Arc::default(),
            health: Arc::default(),
            past: Troubles::default(),
            gaps: 0,
            skips: 0,
            logged: (0, None),
            want_input,
            want_output,
            input: String::new(),
            output: String::new(),
            sides,
        }
    }

    /// Opens only the microphone (`sides.0`) or only the speaker (`sides.1`), for a test.
    pub fn open_sides(want_input: Option<String>, want_output: Option<String>, sides: (bool, bool)) -> Result<Audio, AudioError> {
        let mut audio = Audio::empty(want_input, want_output, sides);
        if let Ok(kind) = std::env::var("TINYCLIENT_MEDIAD_AUDIO") {
            audio.source = match kind.as_str() {
                "tone" => Source::Tone { phase: 0. },
                "onset" => Source::Onset { at: 0 },
                _ => Source::Silence,
            };
            audio.sink = match std::env::var_os("TINYCLIENT_MEDIAD_SINK") {
                Some(path) => Sink::File(File::create(path).map_err(|e| AudioError::Speaker(e.to_string()))?),
                None => Sink::Null,
            };
            audio.input = kind;
            audio.output = "null".to_owned();
            return Ok(audio);
        }
        audio.reopen()?;
        Ok(audio)
    }

    /// Real devices, not the stand-ins a test asks for.
    pub(crate) fn on_devices(&self) -> bool {
        matches!(self.source, Source::Device(_))
    }

    pub(crate) fn mic_running(&self) -> bool {
        match &self.source {
            Source::Device(Some(input)) => input.running,
            _ => true,
        }
    }

    pub(crate) fn device_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    pub(crate) fn sides(&self) -> (bool, bool) {
        self.sides
    }

    /// Opens the wanted devices again: at start, before the 20 ms loop is running.
    pub fn reopen(&mut self) -> Result<(), AudioError> {
        if !self.on_devices() {
            return Ok(());
        }
        let fresh = open_fresh(self.want_input.as_deref(), self.want_output.as_deref(), self.sides, self.mic_running())?;
        drop(self.install(fresh));
        Ok(())
    }

    /// Takes devices that were opened off the call thread. The returned streams are the ones
    /// just replaced; drop them off the 20 ms thread.
    pub(crate) fn install(&mut self, fresh: Fresh) -> Retired {
        let retired = Retired {
            source: std::mem::replace(&mut self.source, fresh.source),
            sink: std::mem::replace(&mut self.sink, fresh.sink),
        };
        self.past.add(&self.health);
        self.health = fresh.health;
        self.broken = fresh.broken;
        self.input = fresh.input;
        self.output = fresh.output;
        self.captured.clear();
        retired
    }

    /// True when a device stopped, or the system default we follow moved to another device.
    pub fn moved(&self) -> bool {
        if !self.on_devices() {
            return false;
        }
        self.device_broken() || route_moved(&self.want_input, &self.input, &self.want_output, &self.output, self.sides)
    }

    /// Stops or starts the microphone itself, so a muted call shows no recording indicator.
    pub fn listen(&mut self, on: bool) {
        if let Source::Device(Some(input)) = &mut self.source {
            if input.running != on {
                if let Some(stream) = &input.stream {
                    let changed = if on { stream.play() } else { stream.pause() };
                    if let Err(err) = changed {
                        eprintln!("mediad: microphone: {err}");
                    }
                }
                input.running = on;
                input.ring.skip(usize::MAX);
                input.started = false;
                input.fade_in = fade_frames(RATE);
                input.drift.restart();
                self.captured.clear();
            }
        }
    }

    /// The next 20 ms from the microphone, if that much has arrived. Called once a tick.
    pub fn capture(&mut self, frame: &mut [i16; FRAME_SAMPLES]) -> bool {
        match &mut self.source {
            Source::Silence => {
                frame.fill(0);
                true
            }
            Source::Tone { phase } => {
                for pair in frame.as_chunks_mut::<CHANNELS>().0 {
                    pair.fill((phase.sin() * 8000.) as i16);
                    *phase = (*phase + 440. * std::f64::consts::TAU / f64::from(RATE)) % std::f64::consts::TAU;
                }
                true
            }
            Source::Onset { at } => {
                for pair in frame.as_chunks_mut::<CHANNELS>().0 {
                    let peak = match *at % RATE {
                        0..24_000 => 0.,
                        24_000..25_920 => 100.,
                        _ => 8000.,
                    };
                    let angle = f64::from(*at) * 440. * std::f64::consts::TAU / f64::from(RATE);
                    pair.fill((angle.sin() * peak) as i16);
                    *at = at.wrapping_add(1);
                }
                true
            }
            Source::Device(None) => false,
            Source::Device(Some(input)) => {
                if !input.running {
                    return false;
                }
                loop {
                    let n = input.ring.pop(&mut self.scratch);
                    input.resampler.push(&self.scratch[..n], &mut self.captured);
                    if n < self.scratch.len() {
                        break;
                    }
                }
                if input.started {
                    let held = (self.captured.len() / CHANNELS) as f64 * 1000. / f64::from(RATE);
                    if let Some(adjust) = input.drift.observe(held) {
                        input.resampler.adjust(adjust);
                    }
                }
                if self.captured.len() > CAPTURE_MAX {
                    // The tick stalled for a long while: what is older is delay nobody wants.
                    trim_front(&mut self.captured, FRAME_SAMPLES * 2, fade_frames(RATE));
                    self.skips += 1;
                }
                if self.captured.len() < FRAME_SAMPLES {
                    self.gaps += u64::from(input.started);
                    return false;
                }
                input.started = true;
                // The first frame after (re)starting fades in.
                let fade = input.fade_in.max(1) as f32;
                for (n, (pair, samples)) in frame.as_chunks_mut::<CHANNELS>().0.iter_mut().zip(self.captured[..FRAME_SAMPLES].as_chunks::<CHANNELS>().0).enumerate() {
                    let gain = if input.fade_in > 0 { (n as f32 / fade).min(1.) } else { 1. };
                    for (out, &sample) in pair.iter_mut().zip(samples) {
                        *out = ((sample * gain).clamp(-1., 1.) * 32767.) as i16;
                    }
                }
                input.fade_in = 0;
                self.captured.drain(..FRAME_SAMPLES);
                true
            }
        }
    }

    /// 20 ms of the mix, to the speaker.
    pub fn play(&mut self, frame: &[i16; FRAME_SAMPLES]) {
        match &mut self.sink {
            Sink::Null | Sink::Device(None) => {}
            Sink::File(file) => {
                let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
                let _ = file.write_all(&bytes);
            }
            Sink::Device(Some(output)) => {
                let underruns = self.health.underruns.load(Ordering::Relaxed);
                if underruns > output.answered {
                    // The tick came later than the cushion could cover: keep more from now on.
                    output.answered = underruns;
                    output.drift.deepen();
                    output.fill = (output.fill + output.rate as usize / 100 * CHANNELS).min(output.most / 2);
                    output.control.fill.store(output.fill, Ordering::Relaxed);
                }
                let held = output.held_ms();
                if let Some(adjust) = output.drift.observe(held) {
                    // Held more than wanted: the device plays slower than we make. Make fewer.
                    output.resampler.adjust(adjust);
                }
                let mut pcm = [0f32; FRAME_SAMPLES];
                for (out, &sample) in pcm.iter_mut().zip(frame) {
                    *out = f32::from(sample) / 32768.;
                }
                output.resampled.clear();
                output.resampler.push(&pcm, &mut output.resampled);
                output.ring.push(&output.resampled);
                if output.ring.len() > output.most {
                    // The tick stalled and caught up: skip to the cushion, with a fade, rather
                    // than keep the delay.
                    output.control.trim_to.store(output.fill, Ordering::Relaxed);
                    self.skips += 1;
                }
            }
        }
    }

    /// The call's troubles so far, over every device it has had.
    pub fn troubles(&self) -> Troubles {
        let mut troubles = self.past;
        troubles.add(&self.health);
        troubles.gaps = self.gaps;
        troubles.skips = self.skips;
        troubles
    }

    /// How far each device's clock is from ours as the drift control has found it, in ppm.
    pub fn drift_ppm(&self) -> (i32, i32) {
        let mic = match &self.source {
            Source::Device(Some(input)) => input.drift.ppm(),
            _ => 0,
        };
        let speaker = match &self.sink {
            Sink::Device(Some(output)) => -output.drift.ppm(),
            _ => 0,
        };
        (mic, speaker)
    }

    /// Once a second: a line in the log for device glitches, at most one a minute, instead of a
    /// line for each.
    pub fn log_glitches(&mut self) {
        let troubles = self.troubles();
        let glitches = troubles.mic_glitches + troubles.speaker_glitches;
        let (logged, at) = self.logged;
        if glitches > logged && at.is_none_or(|at| at.elapsed() >= Duration::from_secs(60)) {
            eprintln!(
                "mediad: {} device glitches (buffer underrun or overrun) since {} (microphone {}, speaker {} in all)",
                glitches - logged,
                if at.is_some() { "the last report" } else { "the call began" },
                troubles.mic_glitches,
                troubles.speaker_glitches
            );
            self.logged = (glitches, Some(Instant::now()));
        }
    }
}

/// The speaker alone, for playing a file (`mediad --play`): no microphone is opened, so no
/// permission is asked and no recording indicator shows. Unlike a call it never drops what it
/// is given; the player waits on [`Speaker::queued`] instead.
pub struct Speaker {
    sink: Sink,
    /// For a stand-in sink, which has no device to drain it: when it began draining and how
    /// much it was given since, so it drains at the pace a device would.
    began: Instant,
    given: u64,
    scratch: Vec<f32>,
    broken: Arc<AtomicBool>,
    pub name: String,
}

impl Speaker {
    /// The device named `want`, else the system default. `TINYCLIENT_MEDIAD_AUDIO` replaces it
    /// with nothing, or with `TINYCLIENT_MEDIAD_SINK`'s file, as for a call.
    pub fn open(want: Option<&str>) -> Result<Speaker, AudioError> {
        let mut speaker = Speaker { sink: Sink::Null, began: Instant::now(), given: 0, scratch: Vec::new(), broken: Arc::default(), name: "null".to_owned() };
        if Audio::simulated() {
            if let Some(path) = std::env::var_os("TINYCLIENT_MEDIAD_SINK") {
                speaker.sink = Sink::File(File::create(path).map_err(|e| AudioError::Speaker(e.to_string()))?);
            }
            return Ok(speaker);
        }
        let host = cpal::default_host();
        let device = find(&host, want, false).ok_or(AudioError::NoSpeaker)?;
        speaker.sink = Sink::Device(Some(open_output(&device, &speaker.broken, &Arc::default(), false)?));
        speaker.name = name(&device).unwrap_or_default();
        Ok(speaker)
    }

    /// 48 kHz frames given and not yet played.
    pub fn queued(&self) -> usize {
        match &self.sink {
            Sink::Device(Some(output)) => {
                let held = output.ring.len() / CHANNELS;
                // The ring is at the device's rate; the player counts at ours.
                (held as f64 / output.resampler.nominal()) as usize
            }
            _ => {
                let drained = (self.began.elapsed().as_secs_f64() * f64::from(RATE)) as u64;
                self.given.saturating_sub(drained) as usize
            }
        }
    }

    /// Interleaved 48 kHz stereo, played after what is already queued.
    pub fn push(&mut self, pcm: &[f32]) {
        if self.queued() == 0 {
            // A stand-in starts draining when it is given something, as a device would.
            self.began = Instant::now();
            self.given = 0;
        }
        self.given += (pcm.len() / CHANNELS) as u64;
        match &mut self.sink {
            Sink::Null | Sink::Device(None) => {}
            Sink::File(file) => {
                let bytes: Vec<u8> = pcm.iter().flat_map(|s| ((s.clamp(-1., 1.) * 32767.) as i16).to_le_bytes()).collect();
                let _ = file.write_all(&bytes);
            }
            Sink::Device(Some(output)) => {
                self.scratch.clear();
                output.resampler.push(pcm, &mut self.scratch);
                output.control.flush.store(false, Ordering::Relaxed);
                // Never dropped: wait for the device to make room, as a player waits anyway.
                let mut rest = &self.scratch[..];
                let until = Instant::now() + Duration::from_secs(2);
                while !rest.is_empty() && Instant::now() < until {
                    let n = output.ring.push(rest);
                    rest = &rest[n..];
                    if !rest.is_empty() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
    }

    /// Nothing more is coming: what is held plays even if it is shorter than the cushion.
    pub fn finish(&mut self) {
        if let Sink::Device(Some(output)) = &mut self.sink {
            output.control.flush.store(true, Ordering::Relaxed);
        }
    }

    /// Drops what is queued, so a seek or a new speed is heard at once.
    pub fn clear(&mut self) {
        if let Sink::Device(Some(output)) = &mut self.sink {
            output.control.clear.store(true, Ordering::Relaxed);
            // The callback empties the ring on its next call; until then nothing new goes in
            // behind what it is about to drop.
            let until = Instant::now() + Duration::from_millis(200);
            while output.control.clear.load(Ordering::Relaxed) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.given = 0;
    }

    /// The device stopped or went away.
    pub fn broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }
}

/// A harness's stand-ins for the system's devices: the same callbacks a device stream runs,
/// driven by a test at a device's rate and period, with its clock off by as much as it likes.
#[cfg(test)]
pub(crate) struct VirtualMic {
    end: CaptureEnd,
    health: Arc<Health>,
    broken: Arc<AtomicBool>,
}

#[cfg(test)]
impl VirtualMic {
    /// One device period of interleaved samples, as the system's callback hands them over.
    pub(crate) fn deliver(&mut self, data: &[f32]) {
        self.end.on_data(data);
    }

    /// The system flagged a glitch (WASAPI's data discontinuity).
    pub(crate) fn xrun(&mut self) {
        on_error("microphone", &cpal::Error::new(cpal::ErrorKind::Xrun), &self.health.mic_glitches, &self.broken);
    }
}

#[cfg(test)]
pub(crate) struct VirtualSpeaker {
    end: PlayEnd,
    health: Arc<Health>,
}

#[cfg(test)]
impl VirtualSpeaker {
    pub(crate) fn render(&mut self, data: &mut [f32]) {
        self.end.on_data(data);
    }

    pub(crate) fn underruns(&self) -> u64 {
        self.health.underruns.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
impl Audio {
    /// A call's audio on virtual devices: `(rate, channels)` of the microphone and the speaker.
    pub(crate) fn virtual_devices(mic: (u32, usize), speaker: (u32, usize)) -> (Audio, VirtualMic, VirtualSpeaker) {
        let mut audio = Audio::empty(None, None, (true, true));
        let health = audio.health.clone();
        let (capture, input) = input_path(mic.0, mic.1, &health);
        let (play, output) = output_path(speaker.0, speaker.1, &health, true);
        audio.source = Source::Device(Some(input));
        audio.sink = Sink::Device(Some(output));
        audio.input = "virtual".to_owned();
        audio.output = "virtual".to_owned();
        let broken = audio.broken.clone();
        (audio, VirtualMic { end: capture, health: health.clone(), broken }, VirtualSpeaker { end: play, health })
    }

    /// Frames held now between the microphone and the tick (at 48 kHz), and between the tick and
    /// the speaker (at its rate).
    pub(crate) fn backlog(&self) -> (usize, usize) {
        let capture = match &self.source {
            Source::Device(Some(input)) => self.captured.len() / CHANNELS + ((input.ring.len() / CHANNELS) as f64 / input.resampler.nominal()) as usize,
            _ => 0,
        };
        let playout = match &self.sink {
            Sink::Device(Some(output)) => output.ring.len() / CHANNELS,
            _ => 0,
        };
        (capture, playout)
    }

    /// Takes the device's report of trouble, as a reopen would.
    pub(crate) fn take_broken(&mut self) -> bool {
        self.broken.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hertz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| [(n as f32 * hertz * std::f32::consts::TAU / rate as f32).sin(); 2])
            .collect()
    }

    /// Resamples in uneven pieces, as a device delivers them.
    fn resampled(from: u32, to: u32, input: &[f32]) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to);
        let mut output = Vec::new();
        for piece in input.chunks(2 * 441 + 2) {
            resampler.push(piece, &mut output);
        }
        output
    }

    /// The largest distance between `output` and a tone of `hertz` at `rate`, past the start.
    fn worst(output: &[f32], rate: u32, hertz: f32) -> f32 {
        let frames = output.len() / 2;
        (200..frames)
            .map(|n| {
                (output[2 * n] - (n as f32 * hertz * std::f32::consts::TAU / rate as f32).sin())
                    .abs()
            })
            .fold(0f32, f32::max)
    }

    #[test]
    fn resampling_keeps_the_duration_and_the_shape() {
        // 100 ms of a 440 Hz tone at 44.1 kHz becomes 100 ms at 48 kHz, less the filter's reach.
        let output = resampled(44_100, 48_000, &tone(44_100, 440., 4410));
        let frames = output.len() / 2;
        assert!((4775..=4800).contains(&frames), "{frames} frames");
        // Each output sample is the tone at its own time: no delay to account for.
        let off = worst(&output, 48_000, 440.);
        assert!(off < 0.002, "off by {off}");
    }

    #[test]
    fn resampling_keeps_high_frequencies_that_a_straight_line_would_bend() {
        // 9 kHz is where consonants live; linear interpolation is off by a quarter there.
        for (from, to) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (16_000, 48_000),
            (96_000, 48_000),
        ] {
            let hertz = if from == 16_000 { 5_000. } else { 9_000. };
            let output = resampled(from, to, &tone(from, hertz, from as usize / 10));
            let off = worst(&output, to, hertz);
            assert!(off < 0.01, "{from} to {to}: off by {off}");
        }
    }

    #[test]
    fn going_down_removes_what_the_new_rate_cannot_carry() {
        // 30 kHz at 96 kHz would fold to 18 kHz at 48 kHz. It has to be gone instead.
        let output = resampled(96_000, 48_000, &tone(96_000, 30_000., 9_600));
        let loudest = output[400..].iter().fold(0f32, |max, s| max.max(s.abs()));
        assert!(loudest < 0.003, "an alias at {loudest}");
    }

    #[test]
    fn a_steady_level_stays_steady_at_every_phase() {
        let output = resampled(44_100, 48_000, &vec![0.5; 2 * 4410]);
        let (low, high) = output[200..]
            .iter()
            .fold((1f32, 0f32), |(low, high), &s| (low.min(s), high.max(s)));
        assert!(low > 0.4999 && high < 0.5001, "{low} to {high}");
    }

    /// A speaker's end on a ring of its own: `fill` samples before playing, fades of `fade`.
    fn speaker(fill: usize, fade: usize) -> (Producer, PlayEnd, Arc<Control>, Arc<Health>) {
        let (producer, consumer) = ring(256);
        let (control, health): (Arc<Control>, Arc<Health>) = Default::default();
        (producer, PlayEnd::new(consumer, control.clone(), health.clone(), 2, fill, fade), control, health)
    }

    #[test]
    fn the_speaker_is_given_nothing_until_the_cushion_is_held_and_refills_after_running_dry() {
        let (mut ring, mut end, _, health) = speaker(8, 2);
        ring.push(&[0.5; 6]);
        assert_eq!(end.next(false), (0., 0.), "three frames held, four wanted");
        assert_eq!(end.ring.len(), 6, "and nothing was taken");
        ring.push(&[0.5; 2]);
        // Playing starts faded in, so even a loud first sample does not click.
        assert_eq!(end.next(false), (0., 0.));
        assert_eq!(end.next(false), (0.25, 0.25));
        // The last two frames ramp down, so the underrun that follows is already silence.
        assert_eq!(end.next(false), (0.25, 0.25));
        assert_eq!(end.next(false), (0., 0.));
        // Dry: silence, and the cushion is built again before anything plays.
        assert_eq!(end.next(false), (0., 0.));
        assert_eq!(health.underruns.load(Ordering::Relaxed), 1);
        ring.push(&[0.5; 4]);
        assert_eq!(end.next(false), (0., 0.));
        assert_eq!(end.ring.len(), 4);
    }

    /// The biggest jump between one played sample and the next.
    fn steepest(played: &[(f32, f32)]) -> f32 {
        played.windows(2).fold(0f32, |steepest, pair| {
            let step = (pair[1].0 - pair[0].0).abs().max((pair[1].1 - pair[0].1).abs());
            steepest.max(step)
        })
    }

    #[test]
    fn running_dry_ramps_to_silence_instead_of_stepping() {
        let (fill, fade) = (4, 8);
        let (mut ring, mut end, _, health) = speaker(fill, fade);
        ring.push(&[1.; 40]);
        let played: Vec<(f32, f32)> = (0..24).map(|_| end.next(false)).collect();
        assert_eq!(health.underruns.load(Ordering::Relaxed), 1);
        assert!(played.last() == Some(&(0., 0.)));
        let step = steepest(&played);
        assert!(step <= 1. / fade as f32 + 1e-4, "a step of {step}");
    }

    #[test]
    fn the_end_of_a_file_plays_out_and_is_not_an_underrun() {
        let (mut ring, mut end, control, health) = speaker(100, 2);
        ring.push(&[0.5; 6]);
        assert_eq!(end.next(false), (0., 0.), "short of the cushion");
        control.flush.store(true, Ordering::Relaxed);
        let played: Vec<(f32, f32)> = (0..5).map(|_| end.next(true)).collect();
        assert!(played.iter().any(|&(left, _)| left > 0.), "{played:?}");
        assert_eq!(health.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn skipping_a_backlog_fades_out_and_back_in() {
        let (mut ring, mut end, control, _) = speaker(4, 4);
        ring.push(&[1.; 200]);
        let mut played = vec![end.next(false), end.next(false), end.next(false), end.next(false)];
        control.trim_to.store(20, Ordering::Relaxed);
        let mut data = [0f32; 2 * 30];
        end.on_data(&mut data);
        played.extend(data.as_chunks::<2>().0.iter().map(|pair| (pair[0], pair[1])));
        assert!(end.ring.len() < 40, "skipped to about the cushion: {}", end.ring.len());
        let step = steepest(&played);
        assert!(step <= 0.25 + 1e-4, "a step of {step}: {played:?}");
    }

    #[test]
    fn a_dropped_backlog_is_crossfaded() {
        let mut samples = vec![0.; 20];
        samples.extend([1.; 8]);
        trim_front(&mut samples, 8, 4);
        assert_eq!(samples.len(), 8);
        let played: Vec<(f32, f32)> = samples.chunks(2).map(|frame| (frame[0], frame[1])).collect();
        let step = steepest(&played);
        assert!(step < 0.25, "a splice of {step}: {played:?}");
        assert!(played[0].0 < 0.3 && played[3].0 > 0.7, "{played:?}");
    }

    /// The user's call logged 1361 glitches, and before this each one reopened both devices.
    #[test]
    fn a_glitch_is_counted_and_only_a_lost_device_is_reopened() {
        let (glitches, broken) = (AtomicU64::new(0), AtomicBool::new(false));
        for _ in 0..1361 {
            on_error("microphone", &cpal::Error::new(cpal::ErrorKind::Xrun), &glitches, &broken);
        }
        on_error("microphone", &cpal::Error::new(cpal::ErrorKind::DeviceChanged), &glitches, &broken);
        assert_eq!(glitches.load(Ordering::Relaxed), 1361);
        assert!(!broken.load(Ordering::Relaxed), "the stream plays on");
        on_error("microphone", &cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable), &glitches, &broken);
        assert!(broken.load(Ordering::Relaxed));
    }

    #[test]
    fn drift_control_leans_the_way_the_ring_is_off_and_no_further_than_half_a_percent() {
        let mut drift = Drift::new(20.);
        let mut last = None;
        for _ in 0..50 {
            last = drift.observe(30.);
        }
        assert!(last.is_some_and(|adjust| (adjust - 0.001).abs() < 1e-9), "{last:?}");
        for _ in 0..500 {
            drift.observe(10_000.);
        }
        assert_eq!(drift.adjust, DRIFT_MAX);
        let mut drift = Drift::new(20.);
        for _ in 0..50 {
            drift.observe(5.);
        }
        assert!(drift.adjust < 0.);
    }

    #[test]
    fn a_microphone_frame_fades_in_after_starting() {
        let (mut audio, mut mic, _) = Audio::virtual_devices((48_000, 1), (48_000, 2));
        mic.deliver(&[0.5; 2400]);
        let mut frame = [0i16; FRAME_SAMPLES];
        assert!(audio.capture(&mut frame));
        assert!(frame[2].abs() < 100 && frame[2] < frame[200] && frame[200] < frame[600], "{:?}", &frame[..8]);
        assert!((16_000..16_500).contains(&frame[FRAME_SAMPLES - 1]), "{}", frame[FRAME_SAMPLES - 1]);
        assert!(audio.capture(&mut frame));
        assert!(frame[0] > 16_000, "only the first: {}", frame[0]);
    }

    #[test]
    fn level_is_zero_for_silence_and_rises_with_loudness() {
        assert_eq!(level(&[0; 1920]), 0);
        let quiet = level(&[33; 1920]);
        let speech = level(&[3000; 1920]);
        let full = level(&[32767; 1920]);
        assert!(quiet < 5, "{quiet}");
        assert!((55..75).contains(&speech), "{speech}");
        assert_eq!(full, 99);
    }
}
