//! Every sound tinyclient makes, from code (`docs/design/09-settings.md` §8): sine and
//! two-operator FM voices with simple envelopes, a 4-pole 300 Hz high-pass and 6 kHz low-pass,
//! all deterministic. No samples, presets or sound fonts, so the licence is the repository's.
//!
//! The in-call cues are rendered here when they play, mixed under the voices. The seven
//! notification sounds are rendered by `examples/sounds.rs` into files the app bundle carries;
//! `tests` hold them to their length, loudness and band.

use protocol::Cue;
use std::f64::consts::TAU;

/// The bundled notification sounds, by the names the pickers show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Pebble,
    Drop,
    Tap,
    Chime,
    Glass,
    SoftRing,
    ClassicRing,
}

impl Sound {
    pub const ALL: [Sound; 7] = [Sound::Pebble, Sound::Drop, Sound::Tap, Sound::Chime, Sound::Glass, Sound::SoftRing, Sound::ClassicRing];

    /// The file stem: `tinyclient-<name>.caf`, prefixed so it cannot meet a system sound.
    pub fn file(self) -> &'static str {
        match self {
            Sound::Pebble => "tinyclient-pebble",
            Sound::Drop => "tinyclient-drop",
            Sound::Tap => "tinyclient-tap",
            Sound::Chime => "tinyclient-chime",
            Sound::Glass => "tinyclient-glass",
            Sound::SoftRing => "tinyclient-soft-ring",
            Sound::ClassicRing => "tinyclient-classic-ring",
        }
    }

    pub fn ring(self) -> bool {
        matches!(self, Sound::SoftRing | Sound::ClassicRing)
    }

    /// Length in seconds and peak in dBFS.
    pub fn shape(self) -> (f64, f64) {
        match self {
            Sound::Pebble => (0.35, -14.),
            Sound::Drop => (0.25, -14.),
            Sound::Tap => (0.12, -14.),
            Sound::Chime => (0.5, -14.),
            Sound::Glass => (0.45, -14.),
            Sound::SoftRing | Sound::ClassicRing => (4.0, -10.),
        }
    }
}

/// The peak of a cue at 100 % of its volume: under normal speech.
pub const CUE_PEAK_DB: f64 = -20.;

/// An exponential decay that reaches -60 dB `over` seconds after `from`.
fn decay(t: f64, from: f64, over: f64) -> f64 {
    if t < from {
        return 1.;
    }
    (0.001f64.ln() * (t - from) / over).exp()
}

/// A linear rise over `attack` seconds.
fn rise(t: f64, attack: f64) -> f64 {
    if attack <= 0. {
        1.
    } else {
        (t / attack).clamp(0., 1.)
    }
}

/// A raised-cosine fade in and out of `fade` seconds over a note of `length`, so nothing clicks.
fn edges(t: f64, length: f64, fade: f64) -> f64 {
    if t < 0. || t > length {
        return 0.;
    }
    let ramp = |x: f64| 0.5 - 0.5 * (std::f64::consts::PI * (x / fade).min(1.)).cos();
    ramp(t) * ramp(length - t)
}

/// A sine whose frequency moves linearly from `from` to `to` over `glide` seconds, phase kept
/// continuous by summing it sample by sample.
struct Voice {
    phase: f64,
}

impl Voice {
    fn new() -> Voice {
        Voice { phase: 0. }
    }

    fn next(&mut self, hertz: f64, rate: f64) -> f64 {
        let value = self.phase.sin();
        self.phase = (self.phase + TAU * hertz / rate) % TAU;
        value
    }
}

/// A frequency gliding from `from` to `to` over the first `glide` seconds, then holding.
fn glide(t: f64, from: f64, to: f64, glide: f64) -> f64 {
    from + (to - from) * (t / glide).clamp(0., 1.)
}

/// A 2-pole (biquad) Butterworth filter, the RBJ cookbook's.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(hertz: f64, rate: f64, high: bool) -> Biquad {
        let w = TAU * hertz / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2. * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1. + alpha;
        let b = if high { [(1. + cos) / 2., -(1. + cos), (1. + cos) / 2.] } else { [(1. - cos) / 2., 1. - cos, (1. - cos) / 2.] };
        Biquad { b: b.map(|b| b / a0), a: [-2. * cos / a0, (1. - alpha) / a0], x: [0.; 2], y: [0.; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Keeps a laptop speaker's band: nothing under 300 Hz it cannot play, nothing shrill over 6 kHz.
/// Each edge is two 2-pole sections (4-pole, 24 dB an octave): one section lets an FM bell's
/// upper sidebands through at a few percent.
fn band(samples: &mut [f64], rate: f64) {
    let mut filters = [
        Biquad::new(300., rate, true),
        Biquad::new(300., rate, true),
        Biquad::new(6_000., rate, false),
        Biquad::new(6_000., rate, false),
    ];
    for sample in samples.iter_mut() {
        *sample = filters.iter_mut().fold(*sample, |x, filter| filter.run(x));
    }
}

/// Scales so the loudest sample is at `db` dBFS.
fn normalise(samples: &mut [f64], db: f64) {
    let peak = samples.iter().fold(0f64, |peak, s| peak.max(s.abs()));
    if peak > 0. {
        let gain = 10f64.powf(db / 20.) / peak;
        samples.iter_mut().for_each(|s| *s *= gain);
    }
}

/// One FM note: carrier `hertz`, modulator at `ratio` times it, the index moving from `index.0`
/// to `index.1` over `sweep` seconds.
fn fm(t: f64, hertz: f64, ratio: f64, index: (f64, f64), sweep: f64) -> f64 {
    let depth = index.0 + (index.1 - index.0) * (t / sweep).clamp(0., 1.);
    (TAU * hertz * t + depth * (TAU * hertz * ratio * t).sin()).sin()
}

/// A notification sound at `rate`, mono, at its peak.
pub fn sound(sound: Sound, rate: u32) -> Vec<f32> {
    let (length, peak) = sound.shape();
    let r = f64::from(rate);
    let count = (length * r).round() as usize;
    let mut samples = vec![0f64; count];
    match sound {
        Sound::Pebble => {
            let (mut root, mut octave) = (Voice::new(), Voice::new());
            for (n, out) in samples.iter_mut().enumerate() {
                let t = n as f64 / r;
                // The pitch falls 3 % over the sound: a drop into water, not a beep.
                let hertz = 659.25 * (1. - 0.03 * t / length);
                let tone = root.next(hertz, r) + 0.25 * octave.next(2. * hertz, r);
                *out = tone * rise(t, 0.005) * decay(t, 0.005, 0.3);
            }
        }
        Sound::Drop => {
            let mut voice = Voice::new();
            for (n, out) in samples.iter_mut().enumerate() {
                let t = n as f64 / r;
                *out = voice.next(glide(t, 1_200., 620., 0.06), r) * rise(t, 0.003) * decay(t, 0.06, 0.16);
            }
        }
        Sound::Tap => {
            for (n, out) in samples.iter_mut().enumerate() {
                let t = n as f64 / r;
                *out = fm(t, 1_000., 1.41, (2., 0.), 0.03) * rise(t, 0.002) * decay(t, 0.002, 0.09);
            }
        }
        Sound::Chime => {
            for (start, hertz) in [(0., 783.99), (0.09, 1_046.5)] {
                for (n, out) in samples.iter_mut().enumerate() {
                    let t = n as f64 / r - start;
                    if t >= 0. {
                        *out += fm(t, hertz, 3.5, (1.5, 0.2), 0.35) * rise(t, 0.003) * decay(t, 0.003, 0.35);
                    }
                }
            }
        }
        Sound::Glass => {
            let (mut root, mut partial) = (Voice::new(), Voice::new());
            for (n, out) in samples.iter_mut().enumerate() {
                let t = n as f64 / r;
                // 2.76 times the root: a struck glass's first partial, inharmonic, 10 dB down.
                let tone = root.next(1_760., r) + 0.316 * partial.next(1_760. * 2.76, r);
                *out = tone * rise(t, 0.003) * decay(t, 0.003, 0.4);
            }
        }
        Sound::SoftRing => {
            for rise_at in [0., 2.] {
                for (step, hertz) in [523.25, 659.25, 783.99].into_iter().enumerate() {
                    let start = rise_at + 0.12 * step as f64;
                    for (n, out) in samples.iter_mut().enumerate() {
                        let t = n as f64 / r - start;
                        if t >= 0. && t < 1.5 {
                            *out += (TAU * hertz * t).sin() * rise(t, 0.01) * decay(t, 0.01, 0.6);
                        }
                    }
                }
            }
        }
        Sound::ClassicRing => {
            for (n, out) in samples.iter_mut().enumerate() {
                let t = n as f64 / r;
                let burst = edges(t, 1., 0.03) + edges(t - 2., 1., 0.03);
                // A trill at 20 Hz, 60 % deep: a phone's ring, not a buzz.
                let trill = 1. - 0.6 * (0.5 + 0.5 * (TAU * 20. * t).cos());
                *out = ((TAU * 440. * t).sin() + (TAU * 480. * t).sin()) * trill * burst;
            }
        }
    }
    band(&mut samples, r);
    normalise(&mut samples, peak);
    // The filters ring a little past the end: the last 5 ms fade so the file ends at zero.
    let tail = ((0.005 * r) as usize).min(count);
    for (k, sample) in samples[count - tail..].iter_mut().enumerate() {
        *sample *= 1. - (k + 1) as f64 / tail as f64;
    }
    samples.into_iter().map(|s| s as f32).collect()
}

/// Notes of a cue: (start, length, hertz). A tick is a note that decays instead of holding.
fn notes(cue: Cue) -> (&'static [(f64, f64, f64)], bool) {
    const E5: f64 = 659.25;
    const A5: f64 = 880.;
    const A4: f64 = 440.;
    const E4: f64 = 329.63;
    match cue {
        Cue::Joined | Cue::Test => (&[(0., 0.09, E5), (0.09, 0.09, A5)], false),
        Cue::Left => (&[(0., 0.09, A5), (0.09, 0.09, E5)], false),
        Cue::Muted => (&[(0., 0.06, A4)], true),
        Cue::Unmuted => (&[(0., 0.06, A5)], true),
        Cue::Deafened => (&[(0., 0.06, A4), (0.06, 0.06, A4)], true),
        Cue::Undeafened => (&[(0., 0.06, A5), (0.06, 0.06, A5)], true),
        Cue::Lost => (&[(0., 0.125, A4), (0.125, 0.125, E4)], false),
        Cue::Back => (&[(0., 0.125, E4), (0.125, 0.125, A4)], false),
    }
}

/// A cue at `rate`, mono, peaking at [`CUE_PEAK_DB`]. A few thousand samples, made when played.
pub fn cue(cue: Cue, rate: u32) -> Vec<f32> {
    let r = f64::from(rate);
    let (notes, tick) = notes(cue);
    let length = notes.iter().map(|(start, length, _)| start + length).fold(0., f64::max);
    let count = (length * r).round() as usize;
    let mut samples = vec![0f64; count];
    for &(start, note, hertz) in notes {
        for (n, out) in samples.iter_mut().enumerate() {
            let t = n as f64 / r - start;
            if !(0. ..=note).contains(&t) {
                continue;
            }
            let shape = if tick { rise(t, 0.002) * decay(t, 0.002, note) * edges(t, note, 0.002) } else { edges(t, note, 0.01) };
            *out += (TAU * hertz * t).sin() * shape;
        }
    }
    normalise(&mut samples, CUE_PEAK_DB);
    samples.into_iter().map(|s| s as f32).collect()
}

/// A cue being played into 20 ms frames, at `gain` (S10, 0 to 1).
pub struct Playing {
    samples: Vec<f32>,
    at: usize,
    gain: f32,
}

impl Playing {
    pub fn new(cue: Cue, gain: f32) -> Playing {
        Playing { samples: self::cue(cue, crate::RATE), at: 0, gain }
    }

    /// Adds the next frame of the cue to an interleaved stereo frame. False once it is over.
    pub fn mix(&mut self, frame: &mut [i16]) -> bool {
        for pair in frame.chunks_exact_mut(crate::CHANNELS) {
            let Some(&sample) = self.samples.get(self.at) else {
                return false;
            };
            self.at += 1;
            let add = (sample * self.gain * 32767.) as i32;
            for out in pair {
                *out = (i32::from(*out) + add).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            }
        }
        self.at < self.samples.len()
    }
}

/// A mono WAV of 16-bit samples: what `examples/sounds.rs` writes for `afconvert`.
pub fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&((sample.clamp(-1., 1.) * 32767.).round() as i16).to_le_bytes());
    }
    out
}

/// The rate the bundled files are rendered at: nothing in them is above 6 kHz.
pub const FILE_RATE: u32 = 22_050;

#[cfg(test)]
mod tests {
    use super::*;

    fn db(x: f64) -> f64 {
        20. * x.log10()
    }

    fn peak(samples: &[f32]) -> f64 {
        samples.iter().fold(0f64, |peak, s| peak.max(f64::from(s.abs())))
    }

    /// The share of the energy in each bin, by a plain radix-2 FFT over the zero-padded signal.
    fn spectrum(samples: &[f32]) -> Vec<f64> {
        let size = samples.len().next_power_of_two();
        let mut re: Vec<f64> = samples.iter().map(|&s| f64::from(s)).chain(std::iter::repeat(0.)).take(size).collect();
        let mut im = vec![0f64; size];
        let mut j = 0;
        for i in 1..size {
            let mut bit = size >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= size {
            let (sin, cos) = (-TAU / len as f64).sin_cos();
            for start in (0..size).step_by(len) {
                let (mut wr, mut wi) = (1f64, 0f64);
                for k in 0..len / 2 {
                    let (a, b) = (start + k, start + k + len / 2);
                    let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                    (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                    re[a] += tr;
                    im[a] += ti;
                    (wr, wi) = (wr * cos - wi * sin, wr * sin + wi * cos);
                }
            }
            len <<= 1;
        }
        (0..=size / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
    }

    /// The share of the energy below `low` Hz or above `high` Hz.
    fn outside(samples: &[f32], rate: u32, low: f64, high: f64) -> f64 {
        let bins = spectrum(samples);
        let hertz = |k: usize| k as f64 * f64::from(rate) / (2. * (bins.len() - 1) as f64);
        let total: f64 = bins.iter().sum();
        let out: f64 = bins.iter().enumerate().filter(|(k, _)| hertz(*k) < low || hertz(*k) > high).map(|(_, e)| e).sum();
        out / total
    }

    #[test]
    fn every_bundled_sound_keeps_its_length_its_peak_and_its_band() {
        for sound in Sound::ALL {
            let samples = self::sound(sound, FILE_RATE);
            let (length, target) = sound.shape();
            let seconds = samples.len() as f64 / f64::from(FILE_RATE);
            assert!((seconds - length).abs() < 0.001, "{sound:?} lasts {seconds}");
            assert!(seconds <= if sound.ring() { 4. } else { 0.5 }, "{sound:?} is too long");
            let peak = db(peak(&samples));
            assert!((peak - target).abs() < 0.1, "{sound:?} peaks at {peak} dBFS");
            let stray = outside(&samples, FILE_RATE, 300., 6_000.);
            assert!(stray < 0.01, "{sound:?} has {:.2} % of its energy outside 300 Hz to 6 kHz", stray * 100.);
            assert!(samples.last().is_some_and(|s| s.abs() < 1e-3), "{sound:?} ends on a click");
        }
    }

    #[test]
    fn the_sounds_are_the_same_every_time_they_are_rendered() {
        for sound in Sound::ALL {
            assert_eq!(self::sound(sound, FILE_RATE), self::sound(sound, FILE_RATE), "{sound:?}");
        }
    }

    #[test]
    fn a_cue_sits_under_speech_and_is_as_long_as_designed() {
        let lengths = [
            (Cue::Joined, 0.18),
            (Cue::Left, 0.18),
            (Cue::Muted, 0.06),
            (Cue::Unmuted, 0.06),
            (Cue::Deafened, 0.12),
            (Cue::Undeafened, 0.12),
            (Cue::Lost, 0.25),
            (Cue::Back, 0.25),
            (Cue::Test, 0.18),
        ];
        for (cue, length) in lengths {
            let samples = self::cue(cue, crate::RATE);
            let seconds = samples.len() as f64 / f64::from(crate::RATE);
            assert!((seconds - length).abs() < 0.001, "{cue:?} lasts {seconds}");
            let peak = db(peak(&samples));
            assert!((peak - CUE_PEAK_DB).abs() < 0.1, "{cue:?} peaks at {peak}");
            // It starts and ends at silence: nothing clicks.
            assert!(samples[0].abs() < 1e-3 && samples[samples.len() - 1].abs() < 2e-3, "{cue:?} clicks");
        }
        // Joining rises and leaving falls: the first half of each is the other's second.
        let (joined, left) = (self::cue(Cue::Joined, crate::RATE), self::cue(Cue::Left, crate::RATE));
        let half = joined.len() / 2;
        assert_eq!(joined[..half], left[half..]);
    }

    #[test]
    fn a_cue_is_mixed_at_its_volume_and_none_at_zero() {
        let mut silent = Playing::new(Cue::Test, 0.);
        let mut frame = [0i16; crate::FRAME_SAMPLES];
        while silent.mix(&mut frame) {}
        assert!(frame.iter().all(|&s| s == 0));

        let mut full = Playing::new(Cue::Test, 1.);
        let mut loudest = 0i16;
        loop {
            let mut frame = [0i16; crate::FRAME_SAMPLES];
            let more = full.mix(&mut frame);
            loudest = loudest.max(frame.iter().map(|s| s.saturating_abs()).max().unwrap());
            if !more {
                break;
            }
        }
        let peak = db(f64::from(loudest) / 32767.);
        assert!((peak - CUE_PEAK_DB).abs() < 1., "at 100 % a cue peaks at {peak} dBFS");
    }

    #[test]
    fn a_wav_has_the_header_afconvert_reads() {
        let bytes = wav(&[0., 0.5, -0.5], 22_050);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 22_050);
        assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), 16_384);
    }
}
