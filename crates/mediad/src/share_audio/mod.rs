//! Loopback of a shared screen or window, so a Go Live leg can send its sound.
//!
//! The capture lives here, not in the Discord module, so a Tiny room share can use the same
//! open later. What this process just played is subtracted: a share must not send the call back.

use crate::{FRAME, FRAME_SAMPLES, RATE};
use cpal::traits::{DeviceTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Set to `tone` to send a 440 Hz sine instead of opening a device. The voice tests use it.
const ENV: &str = "TINYCLIENT_SHARE_AUDIO";
/// How many played frames are kept for the subtraction. Half a second covers a device's delay.
const KEPT_FRAMES: usize = 25;
/// A match weaker than this is not our playback, and the frame is left as it was captured.
const STRONG: f64 = 0.35;

/// One 20 ms stereo frame, or nothing when the device has not filled one yet.
pub trait Pull: Send {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]>;
}

/// Desktop loopback exists. A phone has no system mix to share. The tone harness counts too,
/// so a test can ask for sound without a device.
pub fn supported() -> bool {
    std::env::var(ENV).is_ok() || cfg!(any(windows, target_os = "macos", all(target_os = "linux", not(target_env = "ohos"))))
}

/// The loopback for `screen` (`None` is the primary screen). The tone harness skips the device.
pub fn open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    match std::env::var(ENV).ok().as_deref() {
        Some("tone") => return Ok(Box::new(Tone::default())),
        Some("null") => return Ok(Box::new(Silence)),
        _ => {}
    }
    platform_open(screen).map(|inner| Box::new(Clean { inner }) as Box<dyn Pull>)
}

#[cfg(windows)]
fn platform_open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    win::open(screen)
}

#[cfg(target_os = "macos")]
fn platform_open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    mac::open(screen)
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn platform_open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    linux::open(screen)
}

#[cfg(not(any(windows, target_os = "macos", all(target_os = "linux", not(target_env = "ohos")))))]
fn platform_open(_screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    Err("Sharing sound is not available on this device.".to_owned())
}

#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod linux;

/// What the call just played, oldest first. The share thread reads a copy.
struct Played {
    samples: VecDeque<i16>,
}

static PLAYED: Mutex<Played> = Mutex::new(Played { samples: VecDeque::new() });

/// Remember one frame the speaker was given, including a harness that plays to nowhere.
pub fn note_played(frame: &[i16; FRAME_SAMPLES]) {
    let mut played = PLAYED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    played.samples.extend(frame);
    let cap = KEPT_FRAMES * FRAME_SAMPLES;
    while played.samples.len() > cap {
        played.samples.pop_front();
    }
}

/// Take our own playback out of a loopback frame, when the frame actually contains it.
pub fn subtract(frame: &mut [i16; FRAME_SAMPLES]) {
    let history: Vec<i16> = {
        let played = PLAYED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        played.samples.iter().copied().collect()
    };
    subtract_from(frame, &history);
}

fn subtract_from(frame: &mut [i16; FRAME_SAMPLES], history: &[i16]) {
    if history.len() < FRAME_SAMPLES {
        return;
    }
    let last = history.len() - FRAME_SAMPLES;
    let mut best_lag = 0usize;
    let mut best_dot = i64::MIN;
    let mut lag = 0;
    while lag <= last {
        let dot = dot_at(frame, &history[lag..lag + FRAME_SAMPLES], 96);
        if dot > best_dot {
            best_dot = dot;
            best_lag = lag;
        }
        lag += 96;
    }
    let from = best_lag.saturating_sub(96);
    let to = (best_lag + 96).min(last);
    lag = from;
    while lag <= to {
        let dot = dot_at(frame, &history[lag..lag + FRAME_SAMPLES], 8);
        if dot > best_dot {
            best_dot = dot;
            best_lag = lag;
        }
        lag += 8;
    }
    let from = best_lag.saturating_sub(8);
    let to = (best_lag + 8).min(last);
    for lag in from..=to {
        let dot = dot_at(frame, &history[lag..lag + FRAME_SAMPLES], 1);
        if dot > best_dot {
            best_dot = dot;
            best_lag = lag;
        }
    }
    let played = &history[best_lag..best_lag + FRAME_SAMPLES];
    let mut captured_energy = 0i64;
    let mut played_energy = 0i64;
    let mut dot = 0i64;
    for i in 0..FRAME_SAMPLES {
        let captured = i64::from(frame[i]);
        let sample = i64::from(played[i]);
        captured_energy += captured * captured;
        played_energy += sample * sample;
        dot += captured * sample;
    }
    // Quiet playback, or a frame that does not resemble it, is left alone.
    if played_energy < 1_000_000 || captured_energy < 1 || dot <= 0 {
        return;
    }
    let corr = dot as f64 / ((captured_energy as f64) * (played_energy as f64)).sqrt();
    if corr < STRONG {
        return;
    }
    let gain = dot as f64 / played_energy as f64;
    for i in 0..FRAME_SAMPLES {
        let removed = f64::from(frame[i]) - gain * f64::from(played[i]);
        frame[i] = removed.clamp(-32768.0, 32767.0) as i16;
    }
}

fn dot_at(frame: &[i16; FRAME_SAMPLES], played: &[i16], step: usize) -> i64 {
    let mut dot = 0i64;
    let mut i = 0;
    while i < FRAME_SAMPLES {
        dot += i64::from(frame[i]) * i64::from(played[i]);
        i += step;
    }
    dot
}

struct Clean {
    inner: Box<dyn Pull>,
}

impl Pull for Clean {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        let mut frame = self.inner.pull()?;
        subtract(&mut frame);
        Some(frame)
    }
}

#[derive(Default)]
struct Tone {
    at: u64,
}

impl Pull for Tone {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        let mut frame = [0i16; FRAME_SAMPLES];
        for n in 0..FRAME {
            let t = (self.at + n as u64) as f64 / f64::from(RATE);
            let sample = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            frame[n * 2] = sample;
            frame[n * 2 + 1] = sample;
        }
        self.at += FRAME as u64;
        Some(frame)
    }
}

struct Silence;

impl Pull for Silence {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        None
    }
}

/// Samples a device callback has queued, resampled to 48 kHz stereo on the way out.
pub(super) struct Pcm {
    incoming: Arc<Mutex<VecDeque<i16>>>,
    channels: usize,
    rate: u32,
    held: Vec<i16>,
    pos: f64,
}

impl Pcm {
    pub(super) fn new(channels: usize, rate: u32) -> (Pcm, Arc<Mutex<VecDeque<i16>>>) {
        let incoming = Arc::new(Mutex::new(VecDeque::new()));
        (Pcm { incoming: incoming.clone(), channels: channels.max(1), rate: rate.max(1), held: Vec::new(), pos: 0.0 }, incoming)
    }

    pub(super) fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        {
            let mut incoming = self.incoming.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.held.extend(incoming.drain(..));
        }
        let step = f64::from(self.rate) / f64::from(RATE);
        let need = (self.pos + step * FRAME as f64).ceil() as usize + 1;
        if self.held.len() < need.saturating_mul(self.channels) {
            return None;
        }
        let mut out = [0i16; FRAME_SAMPLES];
        for frame in 0..FRAME {
            let index = self.pos.floor() as usize;
            let frac = self.pos - index as f64;
            let base = index * self.channels;
            for channel in 0..2 {
                let src = if self.channels == 1 { 0 } else { channel.min(self.channels - 1) };
                let left = self.held[base + src];
                let right = self.held.get(base + self.channels + src).copied().unwrap_or(left);
                let mixed = f64::from(left) + (f64::from(right) - f64::from(left)) * frac;
                out[frame * 2 + channel] = mixed.clamp(-32768.0, 32767.0) as i16;
            }
            self.pos += step;
        }
        let drop_frames = self.pos.floor() as usize;
        let drop = drop_frames * self.channels;
        if drop > 0 && drop <= self.held.len() {
            self.held.drain(..drop);
            self.pos -= drop_frames as f64;
        }
        Some(out)
    }
}

pub(super) fn push_f32(data: &[f32], channels: usize, incoming: &Mutex<VecDeque<i16>>) {
    if channels == 0 {
        return;
    }
    let mut guard = incoming.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for frame in data.chunks(channels) {
        for sample in frame {
            guard.push_back((sample.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
    }
    cap(&mut guard, channels);
}

pub(super) fn push_i16(data: &[i16], channels: usize, incoming: &Mutex<VecDeque<i16>>) {
    if channels == 0 {
        return;
    }
    let mut guard = incoming.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.extend(data);
    cap(&mut guard, channels);
}

fn cap(samples: &mut VecDeque<i16>, channels: usize) {
    let most = RATE as usize * channels;
    while samples.len() > most {
        samples.pop_front();
    }
}

/// A cpal capture kept alive for as long as the pull is.
pub(super) struct Device {
    _stream: cpal::Stream,
    pcm: Pcm,
}

impl Pull for Device {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        self.pcm.pull()
    }
}

pub(super) fn cpal_input(device: &cpal::Device, supported: cpal::SupportedStreamConfig) -> Result<Device, String> {
    let format = supported.sample_format();
    let config = supported.config();
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let (pcm, incoming) = Pcm::new(channels, rate);
    let buffer = incoming.clone();
    let stream = match format {
        cpal::SampleFormat::F32 => device
            .build_input_stream(config, move |data: &[f32], _| push_f32(data, channels, &buffer), on_glitch, None)
            .map_err(|err| err.to_string())?,
        cpal::SampleFormat::I16 => device
            .build_input_stream(config, move |data: &[i16], _| push_i16(data, channels, &buffer), on_glitch, None)
            .map_err(|err| err.to_string())?,
        other => return Err(format!("This device's sample format ({other:?}) cannot be shared.")),
    };
    stream.play().map_err(|err| err.to_string())?;
    Ok(Device { _stream: stream, pcm })
}

fn on_glitch(err: cpal::Error) {
    tracing::warn!("mediad: share audio: {err}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, at: usize) -> [i16; FRAME_SAMPLES] {
        let mut frame = [0i16; FRAME_SAMPLES];
        for n in 0..FRAME {
            let t = (at + n) as f64 / f64::from(RATE);
            let sample = ((t * hz * std::f64::consts::TAU).sin() * 8000.0) as i16;
            frame[n * 2] = sample;
            frame[n * 2 + 1] = sample;
        }
        frame
    }

    fn rms(frame: &[i16]) -> f64 {
        (frame.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / frame.len().max(1) as f64).sqrt()
    }

    #[test]
    fn playback_that_is_the_whole_frame_is_taken_out() {
        let frame = sine(440.0, 0);
        let mut heard = frame;
        subtract_from(&mut heard, &frame);
        assert!(heard.iter().all(|sample| sample.abs() < 2), "left {:?}", rms(&heard));
    }

    #[test]
    fn a_tone_on_top_of_playback_is_what_remains() {
        let played = sine(440.0, 0);
        let other = sine(1000.0, 0);
        let mut heard = [0i16; FRAME_SAMPLES];
        for i in 0..FRAME_SAMPLES {
            heard[i] = played[i].saturating_add(other[i]);
        }
        subtract_from(&mut heard, &played);
        let left = rms(&heard);
        assert!((4000.0..7000.0).contains(&left), "the other tone was {left}");
    }

    #[test]
    fn an_unrelated_frame_is_left_alone() {
        let played = sine(440.0, 0);
        let mut heard = [1000i16; FRAME_SAMPLES];
        let before = heard;
        subtract_from(&mut heard, &played);
        assert_eq!(heard, before);
    }

    #[test]
    fn the_tone_harness_is_the_440_hz_a_voice_test_decodes() {
        let mut tone = Tone::default();
        let frame = tone.pull().unwrap();
        let level = rms(&frame);
        assert!((5000.0..6000.0).contains(&level), "{level}");
    }
}
