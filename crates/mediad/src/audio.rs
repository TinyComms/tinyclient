//! The microphone and the speaker. Everything above this file sees 48 kHz stereo `i16`; a device
//! that runs at another rate or channel count is converted here, without touching its settings.
//!
//! `TINYCLIENT_MEDIAD_AUDIO` replaces the devices for tests: `null` hears silence, `tone` hears a
//! 440 Hz tone, `onset` hears that tone once a second, each time led in by 40 ms too quiet to
//! count as speech, and none of them opens a device. `TINYCLIENT_MEDIAD_SINK` names a file that then takes
//! what would have been played, as raw 48 kHz stereo `i16`.

use crate::pick::{self, Backoff, Endpoint, Host, Pick};
use crate::{CHANNELS, FRAME_SAMPLES, RATE};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use protocol::settings::DeviceChoice;
use protocol::AudioDevice;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Device-rate stereo samples the callbacks and the media loop pass each other.
type Ring = Arc<Mutex<VecDeque<f32>>>;

/// What is waiting to be played, and whether playing has begun.
#[derive(Default)]
struct Playout {
    ring: VecDeque<f32>,
    /// Enough is held to play. False at the start and after the ring ran dry: the speaker is
    /// given silence until [`PREBUFFER`] is held again, instead of a frame, a gap and a click.
    playing: bool,
    /// Samples still to be faded in after playing (re)started.
    fade: usize,
    underruns: u64,
    /// Play what is held even when it is less than the cushion: a file's last moments.
    flush: bool,
}

impl Playout {
    /// One stereo frame for the device, or silence while filling.
    fn next(&mut self, fill: usize, fade: usize) -> (f32, f32) {
        if !self.playing {
            if self.ring.len() < fill && !(self.flush && !self.ring.is_empty()) {
                return (0., 0.);
            }
            self.playing = true;
            self.fade = fade;
        }
        let frames_left = self.ring.len() / CHANNELS;
        let (Some(left), Some(right)) = (self.ring.pop_front(), self.ring.pop_front()) else {
            // Ran dry: we fell behind the device. Fill again before playing on.
            self.playing = false;
            self.underruns += 1;
            return (0., 0.);
        };
        // Fade in at the start, and fade the last `fade` frames down to silence. A step from a
        // loud sample to zero is the click both ends hear when the cushion runs out.
        let mut gain = 1.;
        if self.fade > 0 && fade > 0 {
            gain *= 1. - self.fade as f32 / fade as f32;
            self.fade -= 1;
        }
        if fade > 0 && frames_left <= fade {
            gain *= frames_left.saturating_sub(1) as f32 / fade as f32;
        }
        (left * gain, right * gain)
    }

    /// Drops samples off the front until `keep` remain. The samples that were about to play are
    /// blended into the new head, so the clock-drift skip is a ramp and not a splice.
    fn trim_front(&mut self, keep: usize, fade_frames: usize) {
        let len = self.ring.len();
        if len <= keep {
            return;
        }
        let mut drop = len - keep;
        drop -= drop % CHANNELS;
        if drop == 0 {
            return;
        }
        let fade = fade_frames
            .min(drop / CHANNELS)
            .min((len - drop) / CHANNELS);
        for i in 0..fade {
            let toward_new = (i + 1) as f32 / (fade as f32 + 1.);
            for channel in 0..CHANNELS {
                let at = i * CHANNELS + channel;
                let kept = self.ring[at];
                let ahead = self.ring[drop + at];
                self.ring[drop + at] = kept * (1. - toward_new) + ahead * toward_new;
            }
        }
        self.ring.drain(..drop);
    }
}

/// Seconds of audio either ring may hold before the oldest is dropped: the device clock and ours
/// drift, and a backlog is delay.
const BACKLOG: f32 = 0.2;
/// Seconds held before the speaker is given anything. The media loop hands over 20 ms at a time
/// and not exactly on time; without a cushion the device runs dry between frames and clicks.
const PREBUFFER: f32 = 0.06;
/// Seconds over which playing fades in when it starts, and out before the cushion runs dry or
/// the ring skips ahead.
const FADE: f32 = 0.005;

#[derive(Debug)]
pub enum AudioError {
    NoMicrophone,
    NoSpeaker,
    /// The microphone exists and would not open. The system's own words, for the log only.
    Microphone(String),
    Speaker(String),
}

/// Every device, and the system's defaults.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Listing {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
    pub default_input: String,
    pub default_output: String,
}

/// The system's devices, through cpal: WASAPI, CoreAudio, ALSA.
pub(crate) struct System {
    host: cpal::Host,
}

impl System {
    pub(crate) fn new() -> System {
        System { host: cpal::default_host() }
    }

    fn devices(&self, input: bool) -> Vec<cpal::Device> {
        let devices = if input {
            self.host.input_devices().map(Iterator::collect)
        } else {
            self.host.output_devices().map(Iterator::collect)
        };
        devices.unwrap_or_default()
    }

    /// The device listed under `id`, to open.
    fn device(&self, id: &str, input: bool) -> Option<cpal::Device> {
        self.devices(input).into_iter().find(|device| endpoint(device).is_some_and(|e| e.id == id))
    }
}

/// What a device is, for choosing it. `None` for one that went while it was being read.
fn endpoint(device: &cpal::Device) -> Option<Endpoint> {
    use cpal::{DeviceType, InterfaceType};
    let description = device.description().ok()?;
    let name = description.name().to_owned();
    // Without an id (a backend that has none) the name stands in, as it did before ids.
    let id = device.id().map(|id| id.to_string()).unwrap_or_else(|_| name.clone());
    let is_virtual = description.device_type() == DeviceType::Virtual
        || pick::is_virtual(&name)
        || description.driver().is_some_and(pick::is_virtual);
    let rank = match (description.device_type(), description.interface_type()) {
        // A monitor's or receiver's digital input: often nothing is plugged into it.
        (_, InterfaceType::Hdmi | InterfaceType::DisplayPort | InterfaceType::Spdif) => 8,
        (DeviceType::Headset, _) => 0,
        (DeviceType::Headphones | DeviceType::Handset | DeviceType::Earpiece, _) => 1,
        (DeviceType::Speaker | DeviceType::Microphone, _) => 2,
        _ => 5,
    };
    Some(Endpoint { id, name, is_virtual, rank })
}

impl Host for System {
    fn endpoints(&self, input: bool) -> Vec<Endpoint> {
        let mut endpoints: Vec<Endpoint> = Vec::new();
        for endpoint in self.devices(input).iter().filter_map(endpoint) {
            if !endpoints.iter().any(|e| e.id == endpoint.id) {
                endpoints.push(endpoint);
            }
        }
        endpoints
    }

    fn default_id(&self, input: bool) -> Option<String> {
        let device = if input { self.host.default_input_device() } else { self.host.default_output_device() };
        device.as_ref().and_then(endpoint).map(|e| e.id)
    }
}

/// One kind of device for the menu: what is listed, which is the default, which a call left to
/// choose by itself would use.
fn devices_of(system: &dyn Host, input: bool) -> (Vec<AudioDevice>, String) {
    let endpoints = system.endpoints(input);
    let default = system.default_id(input);
    let auto = pick::automatic(&endpoints, default.as_deref(), &[]).map(|e| e.id.clone());
    let default_name = endpoints.iter().find(|e| Some(&e.id) == default.as_ref()).map(|e| e.name.clone()).unwrap_or_default();
    let listed = endpoints
        .into_iter()
        .map(|e| AudioDevice {
            default: Some(&e.id) == default.as_ref(),
            auto: Some(&e.id) == auto.as_ref(),
            id: e.id,
            name: e.name,
            is_virtual: e.is_virtual,
        })
        .collect();
    (listed, default_name)
}

/// Asks the system what exists. Opens nothing.
pub fn listing() -> Listing {
    if let Ok(kind) = std::env::var("TINYCLIENT_MEDIAD_AUDIO") {
        let stand_in = |name: &str| AudioDevice { id: name.to_owned(), name: name.to_owned(), default: true, auto: true, ..AudioDevice::default() };
        return Listing {
            inputs: vec![stand_in(&kind)],
            outputs: vec![stand_in("null")],
            default_input: kind,
            default_output: "null".to_owned(),
        };
    }
    let system = System::new();
    let (inputs, default_input) = devices_of(&system, true);
    let (outputs, default_output) = devices_of(&system, false);
    Listing { inputs, outputs, default_input, default_output }
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
/// device at 48 kHz bypasses it. Output sample `n` is the input at time `n * step`; it becomes
/// available `TAPS - CENTRE` input frames later (under half a millisecond).
pub(crate) struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// Where the next output falls, in frames from the start of `held`.
    position: f64,
    /// Input not yet done with, interleaved. Starts with `CENTRE` frames of silence.
    held: Vec<f32>,
    /// Empty when the rates are equal.
    table: Vec<[f32; TAPS]>,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Resampler {
        let step = f64::from(from) / f64::from(to);
        if from == to {
            return Resampler {
                step,
                position: 0.,
                held: Vec::new(),
                table: Vec::new(),
            };
        }
        // Going down, everything above the new Nyquist frequency has to go first.
        let cutoff = 0.95 * (f64::from(to) / f64::from(from)).min(1.);
        Resampler {
            step,
            position: 0.,
            held: vec![0.; CENTRE * CHANNELS],
            table: table(cutoff),
        }
    }

    pub(crate) fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.table.is_empty() {
            output.extend_from_slice(input);
            return;
        }
        self.held
            .extend_from_slice(&input[..input.len() - input.len() % CHANNELS]);
        let frames = self.held.len() / CHANNELS;
        while self.position as usize + TAPS <= frames {
            let first = self.position as usize;
            let between = (self.position - first as f64) * PHASES as f64;
            let phase = between as usize;
            let blend = (between - phase as f64) as f32;
            let (near, far) = (&self.table[phase], &self.table[phase + 1]);
            let mut sums = [0f32; CHANNELS];
            let taps = self.held[first * CHANNELS..(first + TAPS) * CHANNELS]
                .as_chunks::<CHANNELS>()
                .0;
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

/// What a stream's callbacks tell the call: that the device broke, and that it is running.
#[derive(Clone, Default)]
pub(crate) struct Health {
    /// The device went away or the stream must be rebuilt. A glitch (an underrun) is not this.
    broken: Arc<AtomicBool>,
    /// Callbacks run (a speaker) or frames delivered (a microphone), so a stream that opened
    /// and never started can be told from one that works.
    count: Arc<AtomicU64>,
    /// Glitches: counted and logged, never a reason to rebuild.
    xruns: Arc<AtomicU64>,
}

impl Health {
    /// The error callback. A device that is gone or a stream that is invalid is rebuilt; an
    /// underrun or overrun is a click, and rebuilding for it is what kept a call silent: a
    /// virtual microphone such as Krisp reports a discontinuity many times a second, and each
    /// one used to reopen the speaker too, so it never got to play.
    fn report(&self, side: &str, err: &cpal::Error) {
        use cpal::ErrorKind;
        match err.kind() {
            ErrorKind::Xrun => {
                let xruns = self.xruns.fetch_add(1, Ordering::Relaxed);
                if xruns.is_power_of_two() || xruns == 0 {
                    eprintln!("mediad: {side}: {err} ({} so far)", xruns + 1);
                }
            }
            // Rerouted by the system, still playing; or no real-time priority, still playing.
            ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => eprintln!("mediad: {side}: {err}"),
            _ => {
                eprintln!("mediad: {side}: {err}; opening it again");
                self.broken.store(true, Ordering::Relaxed);
            }
        }
    }
}

struct Input {
    stream: cpal::Stream,
    ring: Ring,
    resampler: Resampler,
    running: bool,
    health: Health,
}

struct Output {
    _stream: cpal::Stream,
    playout: Arc<Mutex<Playout>>,
    resampler: Resampler,
    /// Samples the ring may hold, and how many it is cut back to when it is over.
    room: usize,
    fill: usize,
    /// Frames of the ramp used when the ring skips ahead.
    fade: usize,
    health: Health,
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

/// The device in use on one side, as the menu and the dock are told.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InUse {
    pub name: String,
    pub id: String,
    /// Why it is not the device chosen, in words. `None` when it is.
    pub notice: Option<String>,
}

impl InUse {
    fn of(pick: &Pick) -> InUse {
        InUse { name: pick.endpoint.name.clone(), id: pick.endpoint.id.clone(), notice: pick::notice(pick) }
    }
}

pub struct Audio {
    source: Source,
    sink: Sink,
    /// 48 kHz samples captured and not yet taken as a frame.
    captured: Vec<f32>,
    scratch: Vec<f32>,
    /// The devices asked for, as V1 and V2 store them (`DeviceChoice`). `None` is automatic.
    pub want_input: Option<String>,
    pub want_output: Option<String>,
    pub input: InUse,
    pub output: InUse,
    /// A side that stayed silent after it was rebuilt: the user is asked to switch device.
    pub stuck: (bool, bool),
    /// Devices that would not open, and when each is tried again.
    pub(crate) backoff: Backoff,
    /// Which of the two are opened. A call opens both; a test opens only what it tries.
    sides: (bool, bool),
}

/// Devices opened away from the call thread, ready to take over. A side not reopened is `None`.
pub struct Fresh {
    source: Option<Source>,
    sink: Option<Sink>,
    input: Option<InUse>,
    output: Option<InUse>,
    pub(crate) backoff: Backoff,
}

impl Fresh {
    /// Which sides this replaces.
    pub(crate) fn sides(&self) -> (bool, bool) {
        (self.source.is_some(), self.sink.is_some())
    }

    /// Drops a stream that is the device already open, unless `keep` says it must be replaced
    /// (it broke, never ran, or was asked for): a retry of a device that failed again lands on
    /// the same stand-in, and swapping it for itself would only click. What is said about the
    /// side is kept.
    pub(crate) fn unless_same(mut self, open: (&str, &str), keep: (bool, bool)) -> Fresh {
        if !keep.0 && self.input.as_ref().is_some_and(|input| input.id == open.0) {
            self.source = None;
        }
        if !keep.1 && self.output.as_ref().is_some_and(|output| output.id == open.1) {
            self.sink = None;
        }
        self
    }
}

/// Streams a call has stopped using. Drop this away from the 20 ms thread: stopping a device
/// is the slow half of a reopen. The fields exist so that drop runs here, not on the tick.
#[allow(dead_code)]
pub struct Retired {
    source: Option<Source>,
    sink: Option<Sink>,
}

/// Which sides should be opened again because the device a choice means is no longer the one
/// open: the chosen device came back, the system default moved, or a device went away.
pub(crate) fn route_moved(
    want: (Option<&str>, Option<&str>),
    open: (&str, &str),
    sides: (bool, bool),
    backoff: &Backoff,
) -> (bool, bool) {
    let system = System::new();
    let avoid = backoff.avoid(Instant::now());
    let moved = |want: Option<&str>, open: &str, input: bool| {
        let pick = pick::resolve_on(&system, &DeviceChoice::parse(want), input, &avoid);
        pick.is_some_and(|pick| pick.endpoint.id != open)
    };
    (sides.0 && moved(want.0, open.0, true), sides.1 && moved(want.1, open.1, false))
}

/// Opens the device `choice` means on one side, standing in for it when it is missing or will
/// not open. A device that fails is noted in `backoff` and the next best is tried.
fn open_side<T>(
    system: &System,
    choice: &DeviceChoice,
    input: bool,
    backoff: &mut Backoff,
    open: impl Fn(&cpal::Device) -> Result<T, AudioError>,
) -> Result<(T, Pick), AudioError> {
    let now = Instant::now();
    let mut avoid = backoff.avoid(now);
    let mut last = None;
    // A few tries: the chosen device, then stand-ins, each failure crossed off.
    for _ in 0..4 {
        let Some(pick) = pick::resolve_on(system, choice, input, &avoid) else {
            break;
        };
        let id = pick.endpoint.id.clone();
        let opened = match system.device(&id, input) {
            Some(device) => open(&device),
            None => Err(if input { AudioError::NoMicrophone } else { AudioError::NoSpeaker }),
        };
        match opened {
            Ok(stream) => {
                backoff.opened(&id);
                return Ok((stream, pick));
            }
            Err(err) => {
                eprintln!("mediad: {:?} would not open: {err:?}", pick.endpoint.name);
                backoff.failed(&id, now);
                avoid.push(id);
                last = Some(err);
            }
        }
    }
    Err(last.unwrap_or(if input { AudioError::NoMicrophone } else { AudioError::NoSpeaker }))
}

/// Opens the wanted devices without touching a call that is already playing. `which` are the
/// sides to open; the others stay as they are.
pub(crate) fn open_fresh(
    want_input: Option<&str>,
    want_output: Option<&str>,
    which: (bool, bool),
    mic_running: bool,
    backoff: &mut Backoff,
) -> Result<Fresh, AudioError> {
    let system = System::new();
    let mut fresh = Fresh { source: None, sink: None, input: None, output: None, backoff: Backoff::default() };
    if which.0 {
        let choice = DeviceChoice::parse(want_input);
        let (mut input, pick) = open_side(&system, &choice, true, backoff, open_input)?;
        if !mic_running {
            let _ = input.stream.pause();
            input.running = false;
        }
        fresh.input = Some(InUse::of(&pick));
        fresh.source = Some(Source::Device(Some(input)));
    }
    if which.1 {
        let choice = DeviceChoice::parse(want_output);
        let (output, pick) = open_side(&system, &choice, false, backoff, open_output)?;
        fresh.output = Some(InUse::of(&pick));
        fresh.sink = Some(Sink::Device(Some(output)));
    }
    fresh.backoff = backoff.clone();
    Ok(fresh)
}

fn open_input(device: &cpal::Device) -> Result<Input, AudioError> {
    let supported = device
        .default_input_config()
        .map_err(|e| AudioError::Microphone(e.to_string()))?;
    let config = supported.config();
    let (channels, rate) = (usize::from(config.channels), config.sample_rate);
    let ring: Ring = Arc::default();
    let room = (rate as f32 * BACKLOG) as usize * CHANNELS;
    let health = Health::default();
    let (filled, delivered, told) = (ring.clone(), health.count.clone(), health.clone());
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut ring = filled.lock().unwrap();
                for frame in data.chunks_exact(channels) {
                    // Mono is heard in both ears; of more than two channels the first two count.
                    ring.push_back(frame[0]);
                    ring.push_back(frame[if channels > 1 { 1 } else { 0 }]);
                }
                let over = ring.len().saturating_sub(room);
                ring.drain(..over);
                delivered.fetch_add((data.len() / channels.max(1)) as u64, Ordering::Relaxed);
            },
            move |err| told.report("microphone", &err),
            None,
        )
        .map_err(|e| AudioError::Microphone(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Microphone(e.to_string()))?;
    Ok(Input {
        stream,
        ring,
        resampler: Resampler::new(rate, RATE),
        running: true,
        health,
    })
}

fn open_output(device: &cpal::Device) -> Result<Output, AudioError> {
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Speaker(e.to_string()))?;
    let config = supported.config();
    let (channels, rate) = (usize::from(config.channels), config.sample_rate);
    let playout: Arc<Mutex<Playout>> = Arc::default();
    let fill = (rate as f32 * PREBUFFER) as usize * CHANNELS;
    let fade = ((rate as f32 * FADE) as usize).max(1);
    let health = Health::default();
    let (drained, ran, told) = (playout.clone(), health.count.clone(), health.clone());
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut playout = drained.lock().unwrap();
                for frame in data.chunks_exact_mut(channels) {
                    let (left, right) = playout.next(fill, fade);
                    frame.fill(0.);
                    if channels > 1 {
                        frame[0] = left;
                        frame[1] = right;
                    } else {
                        frame[0] = (left + right) / 2.;
                    }
                }
                ran.fetch_add(1, Ordering::Relaxed);
            },
            move |err| told.report("speaker", &err),
            None,
        )
        .map_err(|e| AudioError::Speaker(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Speaker(e.to_string()))?;
    let room = (rate as f32 * BACKLOG) as usize * CHANNELS;
    Ok(Output {
        _stream: stream,
        playout,
        resampler: Resampler::new(RATE, rate),
        room,
        fill,
        fade,
        health,
    })
}

impl Audio {
    /// True when the environment replaced the devices, so no permission applies.
    pub fn simulated() -> bool {
        std::env::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some()
    }

    /// Opens the microphone and speaker, or their stand-ins.
    pub fn open(
        want_input: Option<String>,
        want_output: Option<String>,
    ) -> Result<Audio, AudioError> {
        Audio::open_sides(want_input, want_output, (true, true))
    }

    /// Opens only the microphone (`sides.0`) or only the speaker (`sides.1`), for a test.
    pub fn open_sides(
        want_input: Option<String>,
        want_output: Option<String>,
        sides: (bool, bool),
    ) -> Result<Audio, AudioError> {
        let mut audio = Audio {
            source: Source::Device(None),
            sink: Sink::Device(None),
            captured: Vec::new(),
            scratch: Vec::new(),
            want_input,
            want_output,
            input: InUse::default(),
            output: InUse::default(),
            stuck: (false, false),
            backoff: Backoff::default(),
            sides,
        };
        if let Ok(kind) = std::env::var("TINYCLIENT_MEDIAD_AUDIO") {
            audio.source = match kind.as_str() {
                "tone" => Source::Tone { phase: 0. },
                "onset" => Source::Onset { at: 0 },
                _ => Source::Silence,
            };
            audio.sink = match std::env::var_os("TINYCLIENT_MEDIAD_SINK") {
                Some(path) => {
                    Sink::File(File::create(path).map_err(|e| AudioError::Speaker(e.to_string()))?)
                }
                None => Sink::Null,
            };
            audio.input = InUse { name: kind.clone(), id: kind, notice: None };
            audio.output = InUse { name: "null".to_owned(), id: "null".to_owned(), notice: None };
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

    fn health(&self) -> (Option<&Health>, Option<&Health>) {
        let input = match &self.source {
            Source::Device(Some(input)) => Some(&input.health),
            _ => None,
        };
        let output = match &self.sink {
            Sink::Device(Some(output)) => Some(&output.health),
            _ => None,
        };
        (input, output)
    }

    /// Which side's device stopped or went away.
    pub(crate) fn device_broken(&self) -> (bool, bool) {
        let broken = |health: Option<&Health>| health.is_some_and(|h| h.broken.load(Ordering::Relaxed));
        let (input, output) = self.health();
        (broken(input), broken(output))
    }

    /// Frames the microphone delivered and callbacks the speaker ran, since each was opened.
    pub(crate) fn counts(&self) -> (u64, u64) {
        let count = |health: Option<&Health>| health.map_or(0, |h| h.count.load(Ordering::Relaxed));
        let (input, output) = self.health();
        (count(input), count(output))
    }

    pub(crate) fn sides(&self) -> (bool, bool) {
        self.sides
    }

    /// Opens the wanted devices again: at start, before the 20 ms loop is running.
    pub fn reopen(&mut self) -> Result<(), AudioError> {
        if !self.on_devices() {
            return Ok(());
        }
        let fresh = open_fresh(
            self.want_input.as_deref(),
            self.want_output.as_deref(),
            self.sides,
            self.mic_running(),
            &mut self.backoff,
        )?;
        drop(self.install(fresh));
        Ok(())
    }

    /// Takes devices that were opened off the call thread. The returned streams are the ones
    /// just replaced; drop them off the 20 ms thread.
    pub(crate) fn install(&mut self, fresh: Fresh) -> Retired {
        let mut retired = Retired { source: None, sink: None };
        if let Some(source) = fresh.source {
            retired.source = Some(std::mem::replace(&mut self.source, source));
            self.captured.clear();
            self.stuck.0 = false;
        }
        if let Some(sink) = fresh.sink {
            retired.sink = Some(std::mem::replace(&mut self.sink, sink));
            self.stuck.1 = false;
        }
        if let Some(input) = fresh.input {
            self.input = input;
        }
        if let Some(output) = fresh.output {
            self.output = output;
        }
        self.backoff = fresh.backoff;
        self.scratch.clear();
        retired
    }

    /// The words for what is not as chosen, and whether a device is stuck silent.
    pub fn notice(&self) -> (Option<String>, bool) {
        let mut words: Vec<String> = Vec::new();
        words.extend(self.input.notice.clone());
        words.extend(self.output.notice.clone());
        if self.stuck.0 {
            words.push(format!("{} isn't sending any sound. Switch device to fix it.", self.input.name));
        }
        if self.stuck.1 {
            words.push(format!("{} isn't playing any sound. Switch device to fix it.", self.output.name));
        }
        let stuck = self.stuck.0 || self.stuck.1;
        ((!words.is_empty()).then(|| words.join(" ")), stuck)
    }

    /// Stops or starts the microphone itself, so a muted call shows no recording indicator.
    pub fn listen(&mut self, on: bool) {
        if let Source::Device(Some(input)) = &mut self.source {
            if input.running != on {
                let changed = if on {
                    input.stream.play()
                } else {
                    input.stream.pause()
                };
                if let Err(err) = changed {
                    eprintln!("mediad: microphone: {err}");
                }
                input.running = on;
                input.ring.lock().unwrap().clear();
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
                    *phase = (*phase + 440. * std::f64::consts::TAU / f64::from(RATE))
                        % std::f64::consts::TAU;
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
                self.scratch.clear();
                self.scratch.extend(input.ring.lock().unwrap().drain(..));
                input.resampler.push(&self.scratch, &mut self.captured);
                // Keep at most a few frames: what is older is delay nobody wants.
                let over = self.captured.len().saturating_sub(FRAME_SAMPLES * 5);
                self.captured.drain(..over - over % FRAME_SAMPLES);
                if self.captured.len() < FRAME_SAMPLES {
                    return false;
                }
                for (out, sample) in frame.iter_mut().zip(self.captured.drain(..FRAME_SAMPLES)) {
                    *out = (sample.clamp(-1., 1.) * 32767.) as i16;
                }
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
                self.scratch.clear();
                self.scratch
                    .extend(frame.iter().map(|&s| f32::from(s) / 32768.));
                let mut resampled = Vec::with_capacity(FRAME_SAMPLES * 2);
                output.resampler.push(&self.scratch, &mut resampled);
                let mut playout = output.playout.lock().unwrap();
                playout.ring.extend(resampled);
                if playout.ring.len() > output.room {
                    // Our clock runs ahead of the device's. Skip to the cushion: one short
                    // ramp now rather than a delay that grows.
                    playout.trim_front(output.fill, output.fade);
                }
            }
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
    pub name: String,
}

impl Speaker {
    /// The speaker `want` chooses (V2, as `DeviceChoice` reads it), or what stands in for it as
    /// for a call. `TINYCLIENT_MEDIAD_AUDIO` replaces it with nothing, or with
    /// `TINYCLIENT_MEDIAD_SINK`'s file, as for a call.
    pub fn open(want: Option<&str>) -> Result<Speaker, AudioError> {
        let mut speaker = Speaker {
            sink: Sink::Null,
            began: Instant::now(),
            given: 0,
            scratch: Vec::new(),
            name: "null".to_owned(),
        };
        if Audio::simulated() {
            if let Some(path) = std::env::var_os("TINYCLIENT_MEDIAD_SINK") {
                speaker.sink =
                    Sink::File(File::create(path).map_err(|e| AudioError::Speaker(e.to_string()))?);
            }
            return Ok(speaker);
        }
        let mut backoff = Backoff::default();
        let (output, pick) = open_side(&System::new(), &DeviceChoice::parse(want), false, &mut backoff, open_output)?;
        speaker.sink = Sink::Device(Some(output));
        speaker.name = pick.endpoint.name;
        Ok(speaker)
    }

    /// 48 kHz frames given and not yet played.
    pub fn queued(&self) -> usize {
        match &self.sink {
            Sink::Device(Some(output)) => {
                let held = output.playout.lock().unwrap().ring.len() / CHANNELS;
                // The ring is at the device's rate; the player counts at ours.
                (held as f64 * output.resampler.step) as usize
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
                let bytes: Vec<u8> = pcm
                    .iter()
                    .flat_map(|s| ((s.clamp(-1., 1.) * 32767.) as i16).to_le_bytes())
                    .collect();
                let _ = file.write_all(&bytes);
            }
            Sink::Device(Some(output)) => {
                self.scratch.clear();
                output.resampler.push(pcm, &mut self.scratch);
                let mut playout = output.playout.lock().unwrap();
                playout.ring.extend(self.scratch.iter());
                playout.flush = false;
            }
        }
    }

    /// Nothing more is coming: what is held plays even if it is shorter than the cushion.
    pub fn finish(&mut self) {
        if let Sink::Device(Some(output)) = &mut self.sink {
            output.playout.lock().unwrap().flush = true;
        }
    }

    /// Drops what is queued, so a seek or a new speed is heard at once.
    pub fn clear(&mut self) {
        if let Sink::Device(Some(output)) = &mut self.sink {
            let mut playout = output.playout.lock().unwrap();
            playout.ring.clear();
            playout.playing = false;
        }
        self.given = 0;
    }

    /// The device stopped or went away.
    pub fn broken(&self) -> bool {
        match &self.sink {
            Sink::Device(Some(output)) => output.health.broken.load(Ordering::Relaxed),
            _ => false,
        }
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

    #[test]
    fn the_speaker_is_given_nothing_until_the_cushion_is_held_and_refills_after_running_dry() {
        let (fill, fade) = (8, 2);
        let mut playout = Playout::default();
        playout.ring.extend([0.5; 6]);
        assert_eq!(
            playout.next(fill, fade),
            (0., 0.),
            "three frames held, four wanted"
        );
        assert_eq!(playout.ring.len(), 6, "and nothing was taken");
        playout.ring.extend([0.5; 2]);
        // Playing starts faded in, so even a loud first sample does not click.
        assert_eq!(playout.next(fill, fade), (0., 0.));
        assert_eq!(playout.next(fill, fade), (0.25, 0.25));
        // The last two frames ramp down, so the underrun that follows is already silence.
        assert_eq!(playout.next(fill, fade), (0.25, 0.25));
        assert_eq!(playout.next(fill, fade), (0., 0.));
        // Dry: silence, and the cushion is built again before anything plays.
        assert_eq!(playout.next(fill, fade), (0., 0.));
        assert_eq!(playout.underruns, 1);
        playout.ring.extend([0.5; 4]);
        assert_eq!(playout.next(fill, fade), (0., 0.));
        assert_eq!(playout.ring.len(), 4);
    }

    /// The biggest jump between one played sample and the next.
    fn steepest(played: &[(f32, f32)]) -> f32 {
        played.windows(2).fold(0f32, |steepest, pair| {
            let step = (pair[1].0 - pair[0].0)
                .abs()
                .max((pair[1].1 - pair[0].1).abs());
            steepest.max(step)
        })
    }

    #[test]
    fn running_dry_ramps_to_silence_instead_of_stepping() {
        let (fill, fade) = (4, 8);
        let mut playout = Playout::default();
        playout.ring.extend([1.; 40]);
        let mut played = Vec::new();
        for _ in 0..24 {
            played.push(playout.next(fill, fade));
        }
        assert_eq!(playout.underruns, 1);
        assert!(played.last() == Some(&(0., 0.)));
        let step = steepest(&played);
        assert!(step <= 1. / fade as f32 + 1e-4, "a step of {step}");
    }

    #[test]
    fn skipping_ahead_crossfades_the_splice() {
        let mut playout = Playout::default();
        playout.ring.extend([0.; 20]);
        playout.ring.extend([1.; 8]);
        playout.trim_front(8, 4);
        assert_eq!(playout.ring.len(), 8);
        let samples: Vec<f32> = playout.ring.iter().copied().collect();
        let played: Vec<(f32, f32)> = samples
            .chunks(2)
            .map(|frame| (frame[0], frame[1]))
            .collect();
        let step = steepest(&played);
        assert!(step < 0.25, "a splice of {step}: {played:?}");
        assert!(played[0].0 < 0.3 && played[3].0 > 0.7, "{played:?}");
    }

    /// On a real machine: the automatic choice opens hardware, and both streams are seen
    /// running within the grace time. Plays silence. `cargo test -p mediad -- --ignored`.
    #[test]
    #[ignore = "opens this machine's microphone and speaker"]
    fn the_real_devices_open_and_are_seen_running() {
        if Audio::simulated() {
            return;
        }
        let audio = Audio::open(None, None).expect("the devices open");
        assert!(!audio.input.id.is_empty() && !audio.output.id.is_empty());
        eprintln!("in: {:?}\nout: {:?}", audio.input, audio.output);
        let started = Instant::now();
        while audio.counts().0 == 0 || audio.counts().1 == 0 {
            assert!(started.elapsed() < crate::pick::GRACE, "not running: {:?}", audio.counts());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(audio.device_broken(), (false, false));
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
