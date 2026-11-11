//! The microphone's processing (V10 to V12): echo cancellation, noise suppression and automatic
//! gain, through `sonora`, WebRTC's audio processing module ported to Rust (AEC3, its noise
//! suppressor and AGC2, with the high-pass filter in front). Enhanced noise, the default,
//! isolates the near voice with `hush-vani` (weights compiled in) after echo cancellation and
//! before automatic gain, so another voice or a cry is turned down instead of being raised.
//! Voice isolation works on a 16 kHz band; everything above its 7.2 kHz cut rejoins the
//! reconstruction from the original, delayed by the path's lag and scaled by the isolation's
//! own gain, so a fullband encode keeps a voice's sparkle while whatever the model silences
//! below the cut is silenced above it too, like the fallback's fullband suppression.
//!
//! The echo canceller needs what the speaker plays: the call's mix goes in as the far end
//! ([`Processor::render`]) every tick, cues included, and each microphone frame comes out with
//! that echo taken away ([`Processor::capture`]). It finds the delay between the two itself.
//! Processing is mono at 48 kHz in 10 ms halves of our 20 ms frame; a stereo frame is folded
//! to mono going in and the result written to both channels coming out.
//!
//! Until chatd says what the user chose (`MediaRequest::Processing`), nothing is processed, and
//! with everything off the processor is not even built: the microphone is as captured.

use std::sync::{Arc, OnceLock};

use crate::{CHANNELS, FRAME, FRAME_SAMPLES, RATE};
use hush_vani::{Hush, HushStream};
use protocol::{AudioProcessing, NoiseMode};
use sonora::config::{
    AdaptiveDigital, EchoCanceller, GainController2, HighPassFilter, NoiseSuppression,
    NoiseSuppressionLevel,
};
use sonora::{Config, StreamConfig};

/// Samples in one 10 ms block, the module's unit.
const BLOCK: usize = FRAME / 2;
/// 10 ms at 16 kHz, one voice-isolation frame. A 20 ms mic frame is two of these.
const LOW: usize = 160;
/// Low-pass before throwing away two of every three samples. An odd length keeps the delay
/// an integer number of samples.
const TAPS: usize = 47;
/// The 16 kHz reconstruction lags the original by this many 48 kHz samples, derived from the
/// chain hop by hop rather than measured:
/// - the decimation low-pass and the reconstruction low-pass are the same linear-phase FIR,
///   [`TAPS`] taps, each centered (TAPS − 1) / 2 = 23 samples late: 46 together;
/// - voice isolation buffers exactly one hop. Its 50 % overlap STFT rebuilds every input
///   sample from the two analysis windows that cover it, and the second of those windows is
///   complete one hop after the sample, which is the crate's `Hush::LATENCY_SAMPLES`: 160 at
///   16 kHz, 480 at ours. For any gain the model applies per frame this lag is exact, the
///   gain rides on the frame and not on the sample; only its deep filter mixes older frames
///   in, and on speech the current frame dominates that mix.
///
/// 23 + 480 + 23 = 526. The enhanced lag test below holds a measurement against this on
/// speech the model passes. Measuring on noise, as the first cut did, is what produced a
/// fake 173: the model attenuates noise by tens of decibels, so the correlation peak sits
/// on residue and wanders with the run length (557 over 120 frames).
const BRIDGE_DELAY: usize = TAPS / 2 + TAPS / 2;
const HUSH_DELAY: usize = Hush::LATENCY_SAMPLES * (RATE / Hush::SAMPLE_RATE) as usize;
const RECON_DELAY: usize = BRIDGE_DELAY + HUSH_DELAY;
/// How far back the original's samples are kept for the high band. The high-pass filter is
/// centered one filter-half later than the low-pass, so the delayed original sits that much
/// earlier and the bands land on the same sample again.
const DELAY: usize = RECON_DELAY - TAPS / 2;
/// The ring that supplies [`DELAY`] is one frame long, so the delay must fit inside a frame.
const _: () = assert!(DELAY < FRAME);
/// Below this RMS, about −80 dBFS, the original low band carries nothing to judge, so the
/// high band's gain holds instead of chasing a ratio of silences.
const HIGH_BAND_FLOOR: f32 = 1e-4;
/// How far the high band's gain moves toward the newest hop's ratio, per hop (two per
/// frame): a syllable moves it within tens of milliseconds, a hop of model flutter does not.
const HIGH_BAND_RATE: f32 = 0.25;

pub struct Processor {
    /// High-pass, echo cancellation, and WebRTC noise suppression when that is the mode.
    /// Automatic gain is not here for Enhanced: it runs after voice isolation.
    front: Option<sonora::AudioProcessing>,
    /// Automatic gain after voice isolation, so a cry is not turned up first.
    gain: Option<sonora::AudioProcessing>,
    hush: Option<HushStream>,
    band: Band,
    /// What is in force, so a repeat of the same choice changes nothing.
    chosen: Option<AudioProcessing>,
}

/// The 48 kHz ⟷ 16 kHz bridge for voice isolation, and the last samples it still needs.
struct Band {
    hist: [f32; TAPS - 1],
    /// The high-rate samples the reconstruction filter has not finished with.
    up_hist: [f32; TAPS - 1],
    /// The high-pass's own history over the original.
    hp_hist: [f32; TAPS - 1],
    /// The high band's past, as a delay line of `DELAY`: written at `i`, read at `i - DELAY`.
    /// One frame long, so `DELAY` must stay under [`FRAME`].
    hp_ring: [f32; FRAME],
    /// The high band's gain, following the isolation's own, smoothed over hops. Starts
    /// neutral, so a quiet start hands the band through instead of chasing a silence.
    high_gain: f32,
}

impl Default for Processor {
    fn default() -> Processor {
        Processor::new()
    }
}

impl Default for Band {
    fn default() -> Band {
        Band {
            hist: [0.; TAPS - 1],
            up_hist: [0.; TAPS - 1],
            hp_hist: [0.; TAPS - 1],
            hp_ring: [0.; FRAME],
            high_gain: 1.,
        }
    }
}

fn ns_level(mode: NoiseMode) -> Option<NoiseSuppression> {
    let level = match mode {
        NoiseMode::Off | NoiseMode::Enhanced => return None,
        NoiseMode::Standard => NoiseSuppressionLevel::High,
        NoiseMode::High => NoiseSuppressionLevel::VeryHigh,
    };
    Some(NoiseSuppression {
        level,
        ..NoiseSuppression::default()
    })
}

fn agc() -> GainController2 {
    // Adaptive digital gain only: it brings a quiet microphone up to speech level and never
    // touches the system's input volume.
    GainController2 {
        adaptive_digital: Some(AdaptiveDigital::default()),
        ..GainController2::default()
    }
}

/// WebRTC settings for everything except Enhanced's automatic gain, which is a second pass.
fn front_config(chosen: AudioProcessing, isolated: bool) -> Config {
    let enhanced = chosen.noise == NoiseMode::Enhanced && isolated;
    Config {
        high_pass_filter: Some(HighPassFilter::default()),
        echo_canceller: chosen.echo_cancellation.then(EchoCanceller::default),
        noise_suppression: if enhanced {
            None
        } else {
            // If voice isolation did not start, Enhanced falls back to the strongest WebRTC filter.
            let mode = if chosen.noise == NoiseMode::Enhanced { NoiseMode::High } else { chosen.noise };
            ns_level(mode)
        },
        gain_controller2: (!enhanced && chosen.auto_gain).then(agc),
        ..Config::default()
    }
}

fn gain_config() -> Config {
    Config {
        gain_controller2: Some(agc()),
        ..Config::default()
    }
}

fn wants_front(chosen: AudioProcessing) -> bool {
    chosen.echo_cancellation || chosen.auto_gain || chosen.noise != NoiseMode::Off
}

fn apm(config: Config) -> sonora::AudioProcessing {
    let stream = StreamConfig::new(RATE, 1);
    sonora::AudioProcessing::builder()
        .config(config)
        .capture_config(stream)
        .render_config(stream)
        .build()
}

/// Hamming-windowed low-pass at 7.2 kHz, unity gain at dc, so decimation by 3 does not alias.
fn lowpass() -> &'static [f32; TAPS] {
    static COEFFS: OnceLock<[f32; TAPS]> = OnceLock::new();
    COEFFS.get_or_init(|| {
        const FC: f32 = 7200. / 48000.;
        let mid = (TAPS / 2) as f32;
        let mut coeff = [0f32; TAPS];
        let mut sum = 0f32;
        for (i, tap) in coeff.iter_mut().enumerate() {
            let n = i as f32 - mid;
            let window =
                0.54 - 0.46 * (std::f32::consts::TAU * i as f32 / (TAPS as f32 - 1.)).cos();
            let sinc = if n == 0. {
                2. * FC
            } else {
                (n * FC * std::f32::consts::TAU).sin() / (n * std::f32::consts::PI)
            };
            *tap = sinc * window;
            sum += *tap;
        }
        for tap in &mut coeff {
            *tap /= sum;
        }
        coeff
    })
}

/// Hamming-windowed high-pass at 7.2 kHz, built as an impulse minus the low-pass: passband
/// gain stays at one (delta minus the same gain) and the stopband takes the low-pass's
/// rejection, on the same filter center as [`lowpass`].
fn highpass() -> &'static [f32; TAPS] {
    static COEFFS: OnceLock<[f32; TAPS]> = OnceLock::new();
    COEFFS.get_or_init(|| {
        let mut coeff = *lowpass();
        coeff[TAPS / 2] -= 1.;
        for tap in &mut coeff {
            *tap = -*tap;
        }
        coeff
    })
}

impl Processor {
    /// Off: frames pass untouched.
    pub fn new() -> Processor {
        Processor {
            front: None,
            gain: None,
            hush: None,
            band: Band::default(),
            chosen: None,
        }
    }

    /// Takes the user's choice. All off, or `None`, turns processing off.
    pub fn set(&mut self, chosen: Option<AudioProcessing>) {
        if chosen == self.chosen {
            return;
        }
        self.chosen = chosen;
        self.band = Band::default();
        let Some(chosen) = chosen else {
            self.front = None;
            self.gain = None;
            self.hush = None;
            return;
        };
        let hush = (chosen.noise == NoiseMode::Enhanced).then(|| match Hush::new() {
            Ok(model) => Some(HushStream::new(Arc::new(model))),
            Err(err) => {
                tracing::warn!("voice isolation did not start: {err}");
                None
            }
        });
        let isolated = hush.as_ref().is_some_and(Option::is_some);
        self.hush = hush.flatten();
        self.front = wants_front(chosen).then(|| apm(front_config(chosen, isolated)));
        // Gain after isolation only. Other modes keep it inside `front`, where WebRTC runs it
        // after its own suppressor.
        self.gain = (isolated && chosen.auto_gain).then(|| apm(gain_config()));
    }

    pub fn active(&self) -> bool {
        self.front.is_some() || self.hush.is_some() || self.gain.is_some()
    }

    /// What the speaker is about to play: the echo canceller's reference.
    pub fn render(&mut self, frame: &[i16; FRAME_SAMPLES]) {
        let Some(apm) = &mut self.front else {
            return;
        };
        if self.chosen.is_some_and(|c| !c.echo_cancellation) {
            return;
        }
        let mono = fold(frame);
        let mut out = [0f32; BLOCK];
        for block in mono.as_chunks::<BLOCK>().0 {
            // A frame the module refuses only goes unheard by the canceller.
            let _ = apm.process_render_f32(&[&block[..]], &mut [&mut out]);
        }
    }

    /// One microphone frame, processed in place.
    pub fn capture(&mut self, frame: &mut [i16; FRAME_SAMPLES]) {
        if !self.active() {
            return;
        }
        let mono = fold(frame);
        let mut done = [0f32; FRAME];
        if let Some(apm) = &mut self.front {
            run_blocks(apm, &mono, &mut done);
        } else {
            done = mono;
        }
        if self.hush.is_some() {
            let mut low = [0f32; LOW * 2];
            let mut clean = [0f32; LOW * 2];
            self.band.down(&done, &mut low);
            if let Some(hush) = &mut self.hush {
                for (input, out) in low
                    .as_chunks::<LOW>()
                    .0
                    .iter()
                    .zip(clean.as_chunks_mut::<LOW>().0)
                {
                    hush.process_frame(input, out);
                }
            }
            let original = done;
            self.band.up(&clean, &low, &original, &mut done);
        }
        if let Some(apm) = &mut self.gain {
            let source = done;
            run_blocks(apm, &source, &mut done);
        }
        for (pair, sample) in frame.as_chunks_mut::<CHANNELS>().0.iter_mut().zip(done) {
            pair.fill((sample.clamp(-1., 1.) * 32767.) as i16);
        }
    }

    /// How much echo the canceller is taking away now, in dB, once it knows.
    pub fn echo_reduction_db(&self) -> Option<f64> {
        self.front
            .as_ref()?
            .statistics()
            .echo_return_loss_enhancement
    }
}

/// Root mean square, the level a hop's high-band gain ratio is judged at.
fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn run_blocks(apm: &mut sonora::AudioProcessing, input: &[f32; FRAME], output: &mut [f32; FRAME]) {
    for (block, out) in input
        .as_chunks::<BLOCK>()
        .0
        .iter()
        .zip(output.as_chunks_mut::<BLOCK>().0)
    {
        if apm
            .process_capture_f32(&[&block[..]], &mut [&mut out[..]])
            .is_err()
        {
            out.copy_from_slice(block);
        }
    }
}

impl Band {
    /// 48 kHz mono in, 16 kHz mono out, three samples to one.
    fn down(&mut self, input: &[f32; FRAME], out: &mut [f32; LOW * 2]) {
        let coeff = lowpass();
        let mut window = [0f32; FRAME + TAPS];
        let keep = TAPS - 1;
        window[..keep].copy_from_slice(&self.hist);
        window[keep..keep + FRAME].copy_from_slice(input);
        for (i, sample) in out.iter_mut().enumerate() {
            let at = keep + i * 3;
            let mut acc = 0f32;
            for (k, tap) in coeff.iter().enumerate() {
                acc += tap * window[at - k];
            }
            *sample = acc;
        }
        let end = keep + FRAME;
        self.hist.copy_from_slice(&window[end - keep..end]);
    }

    /// 16 kHz mono in, 48 kHz mono out, with the original at hand and the low band the
    /// model saw. Zero-stuffed and low-passed, so the copies a linear stretch would paint
    /// above 8 kHz are not there for fullband Opus to encode. The original's part above the
    /// 7.2 kHz cut rejoins from `original` at the path's lag, scaled by the isolation's own
    /// gain, so whatever the model silences below the cut is silenced above it too, the way
    /// the WebRTC fallback suppresses fullband.
    fn up(&mut self, isolated: &[f32; LOW * 2], noisy: &[f32; LOW * 2],
        original: &[f32; FRAME], out: &mut [f32; FRAME]) {
        const KEEP: usize = TAPS - 1;
        let coeff = lowpass();
        let mut window = [0f32; FRAME + TAPS];
        window[..KEEP].copy_from_slice(&self.up_hist);
        for (i, &sample) in isolated.iter().enumerate() {
            window[KEEP + i * 3] = sample * 3.;
        }
        for (i, sample) in out.iter_mut().enumerate() {
            let at = KEEP + i;
            let mut acc = 0f32;
            for (k, tap) in coeff.iter().enumerate() {
                acc += tap * window[at - k];
            }
            *sample = acc;
        }
        let end = KEEP + FRAME;
        self.up_hist.copy_from_slice(&window[end - KEEP..end]);
        // The high band's gain: per hop, the ratio of isolated to original low-band energy,
        // smoothed and clamped to muting. The model never has reason to lift the sparkle
        // above the original, only to let a voice's through. Under the floor it holds:
        // silence judges nothing. With the model passing the band (`isolated == noisy`)
        // the ratio is exactly one and the gain stays exactly neutral, which is what the
        // bridge tests rely on.
        let mut gain = self.high_gain;
        for (kept, heard) in isolated
            .as_chunks::<LOW>()
            .0
            .iter()
            .zip(noisy.as_chunks::<LOW>().0)
        {
            let noisy_rms = rms(heard);
            if noisy_rms >= HIGH_BAND_FLOOR {
                let ratio = (rms(kept) / noisy_rms).clamp(0., 1.);
                gain += HIGH_BAND_RATE * (ratio - gain);
            }
        }
        self.high_gain = gain;
        // The original's high band rejoins: the same causal filter shape as the low-pass,
        // centered one filter-half earlier in the delayed original, so its samples land on
        // the reconstruction's lag without a phase step between the bands. The ring is the
        // rest of the lag: sample `i` reads what the high-pass produced at `i - DELAY`,
        // earlier this frame once `i` passes the delay, the last frame's before that. It
        // must read one slot while writing another: a ring read and written at the same
        // slot hands every sample the whole previous frame back, whatever the offset.
        let mut hwin = [0f32; KEEP + FRAME];
        hwin[..KEEP].copy_from_slice(&self.hp_hist);
        hwin[KEEP..].copy_from_slice(original);
        let hp = highpass();
        let offset = FRAME - DELAY;
        for i in 0..FRAME {
            let at = KEEP + i;
            let mut acc = 0f32;
            for (k, tap) in hp.iter().enumerate() {
                acc += tap * hwin[at - k];
            }
            let joined = self.hp_ring[(i + offset) % FRAME];
            self.hp_ring[i] = acc;
            out[i] += gain * joined;
        }
        self.hp_hist.copy_from_slice(&hwin[end - KEEP..end]);
    }
}

/// A 6 kHz tone through the 16 kHz bridge must not come back with a 10 kHz image.
pub fn prove_bridge_image() -> Result<(), &'static str> {
    let mut band = Band::default();
    let mut phase = 0f64;
    let mut heard = Vec::new();
    for _ in 0..20 {
        let mut input = [0f32; FRAME];
        for sample in &mut input {
            phase += 6000. / RATE as f64;
            *sample = (phase * std::f64::consts::TAU).sin() as f32 * 0.4;
        }
        let mut low = [0f32; LOW * 2];
        let mut out = [0f32; FRAME];
        band.down(&input, &mut low);
        band.up(&low, &low, &input, &mut out);
        heard.extend_from_slice(&out);
    }
    let settled = &heard[FRAME * 4..];
    let (tone, image) = (bin_level(settled, 6000.), bin_level(settled, 10000.));
    if tone <= 0.02 {
        return Err("the tone did not survive");
    }
    if image * 10. >= tone {
        return Err("a high tone came back with an image above 8 kHz");
    }
    Ok(())
}

/// The lab's high-band driver: with Enhanced chosen, the voice's sparkle above the 7.2 kHz
/// cut reaches the send, and at the path's lag. Before the rejoin it did not reach the send
/// at all, and with the ring reading the slot it wrote it came back a whole frame late,
/// which smears every sibilant across the crossover.
pub fn prove_high_band() -> Result<(), &'static str> {
    // The lag, to the sample: an impulse's high band is the high-pass's response, and the
    // ring is all that stands between it and the reconstruction, so its peak belongs at
    // `impulse + RECON_DELAY`. The low band is silent, so the gain holds at neutral.
    let mut band = Band::default();
    let at = FRAME / 2;
    let mut original = [0f32; FRAME];
    original[at] = 1.;
    let quiet = [0f32; LOW * 2];
    let mut heard = Vec::new();
    for _ in 0..2 {
        let mut out = [0f32; FRAME];
        band.up(&quiet, &quiet, &original, &mut out);
        heard.extend_from_slice(&out);
        original = [0f32; FRAME];
    }
    let (mut peak, mut loud) = (0, 0f32);
    for (n, &sample) in heard.iter().enumerate() {
        if sample.abs() > loud {
            (peak, loud) = (n, sample.abs());
        }
    }
    if peak.abs_diff(at + RECON_DELAY) > 2 {
        return Err("the high band rejoined a frame late, not at the path's lag");
    }
    // And a voice keeps its sibilants: harmonics the model passes, so the isolation's gain
    // stays near one and the tone above the cut rides with them to the send. A tone alone
    // is not voice and is rightly muted now; the point here is what a voice keeps.
    let mut processor = Processor::new();
    processor.set(Some(AudioProcessing {
        echo_cancellation: false,
        auto_gain: false,
        noise: NoiseMode::Enhanced,
    }));
    let mut heard = Vec::new();
    for n in 0..80 {
        let mut frame = [0f32; FRAME];
        for (s, sample) in frame.iter_mut().enumerate() {
            let t = (n * FRAME + s) as f64 / f64::from(RATE);
            let f0 = 140. + 30. * (t * std::f64::consts::TAU * 3.).sin();
            let gate = if (t * 4.) as u32 % 3 == 2 { 0. } else { 1. };
            let mut voice = 0f64;
            for k in 1..=8 {
                voice += (t * std::f64::consts::TAU * f0 * k as f64).sin() / k as f64;
            }
            let sparkle = (t * std::f64::consts::TAU * 10_000.).sin();
            *sample = (0.3 * gate * voice + 0.5 * sparkle) as f32;
        }
        let mut stereo_frame = [0i16; FRAME_SAMPLES];
        for (pair, &s) in stereo_frame.as_chunks_mut::<CHANNELS>().0.iter_mut().zip(&frame) {
            pair.fill((s * 8000.) as i16);
        }
        processor.capture(&mut stereo_frame);
        if n > 20 {
            heard.extend(
                stereo_frame
                    .iter()
                    .step_by(CHANNELS)
                    .map(|&s| f32::from(s) / 32768.),
            );
        }
    }
    let sparkle = bin_level(&heard, 10_000.);
    if sparkle <= 0.02 {
        return Err("the sparkle above the cut did not ride with the voice through Enhanced");
    }
    Ok(())
}

fn bin_level(samples: &[f32], hz: f32) -> f64 {
    // The reference phase runs in f64: at 10 kHz over tens of thousands of samples an f32
    // accumulator drifts enough to smear the bin by tens of percent.
    let step = hz as f64 / RATE as f64;
    let mut phase = 0f64;
    let (mut i, mut q) = (0f64, 0f64);
    for &sample in samples {
        let angle = phase * std::f64::consts::TAU;
        i += f64::from(sample) * angle.cos();
        q += f64::from(sample) * angle.sin();
        phase = (phase + step) % 1.;
    }
    (i * i + q * q).sqrt() / samples.len() as f64
}

/// Soft mic gain from Settings (0 to 100, 100 = unity), applied in place before processing.
pub fn apply_input_gain(frame: &mut [i16; FRAME_SAMPLES], percent: u8) {
    if percent >= 100 {
        return;
    }
    if percent == 0 {
        frame.fill(0);
        return;
    }
    let gain = f32::from(percent) / 100.;
    for sample in frame.iter_mut() {
        *sample = (f32::from(*sample) * gain).round().clamp(-32768., 32767.) as i16;
    }
}

/// A stereo frame as mono floats in −1…1.
fn fold(frame: &[i16; FRAME_SAMPLES]) -> [f32; FRAME] {
    let mut mono = [0f32; FRAME];
    for (out, pair) in mono.iter_mut().zip(frame.as_chunks::<CHANNELS>().0) {
        *out = (f32::from(pair[0]) + f32::from(pair[1])) / 2. / 32768.;
    }
    mono
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[i16]) -> f64 {
        (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len().max(1) as f64)
            .sqrt()
    }

    fn db(ratio: f64) -> f64 {
        20. * ratio.log10()
    }

    /// Speech-shaped noise: low-passed, rising and falling three times a second, louder and
    /// softer in half-second turns.
    fn far_end(seconds: usize) -> Vec<i16> {
        let mut seed: u32 = 1;
        let mut low = 0f32;
        (0..RATE as usize * seconds)
            .map(|n| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2. - 1.;
                low = 0.7 * low + 0.3 * noise;
                let turn = if (n / 24_000) % 2 == 0 { 1. } else { 0.3 };
                let syllables = (n as f32 / RATE as f32 * 3. * std::f32::consts::TAU)
                    .sin()
                    .abs()
                    + 0.2;
                (low * 0.3 * turn * syllables * 32767.) as i16
            })
            .collect()
    }

    fn stereo(mono: &[i16]) -> [i16; FRAME_SAMPLES] {
        let mut frame = [0i16; FRAME_SAMPLES];
        for (pair, &s) in frame.as_chunks_mut::<CHANNELS>().0.iter_mut().zip(mono) {
            pair.fill(s);
        }
        frame
    }

    fn on(echo_cancellation: bool, noise: NoiseMode, auto_gain: bool) -> Option<AudioProcessing> {
        Some(AudioProcessing {
            echo_cancellation,
            auto_gain,
            noise,
        })
    }

    /// The speaker's sound reaches the microphone 60 ms later at 40 %: what was played comes
    /// back, and the canceller takes it out. Measured over the last four seconds of ten, once
    /// it has converged.
    #[test]
    fn the_echo_canceller_takes_away_what_the_speaker_played() {
        let far = far_end(10);
        let delay = RATE as usize * 60 / 1000;
        let mut processor = Processor::new();
        processor.set(on(true, NoiseMode::Off, false));
        let (mut heard, mut sent) = (Vec::new(), Vec::new());
        for (n, played) in far.as_chunks::<FRAME>().0.iter().enumerate() {
            processor.render(&stereo(played));
            let echo: Vec<i16> = (n * FRAME..(n + 1) * FRAME)
                .map(|i| {
                    if i >= delay {
                        (f64::from(far[i - delay]) * 0.4) as i16
                    } else {
                        0
                    }
                })
                .collect();
            let mut frame = stereo(&echo);
            processor.capture(&mut frame);
            if n * FRAME >= RATE as usize * 6 {
                heard.extend_from_slice(&echo);
                sent.extend(frame.iter().step_by(CHANNELS));
            }
        }
        let reduced = db(rms(&heard) / rms(&sent).max(1.));
        assert!(reduced > 20., "the echo is only {reduced:.1} dB quieter");
        assert!(
            processor.echo_reduction_db().is_some_and(|db| db > 10.),
            "{:?}",
            processor.echo_reduction_db()
        );
    }

    /// With nothing played, speech goes through the canceller untouched in level.
    #[test]
    fn speech_with_no_echo_goes_through() {
        let near = far_end(4);
        let mut processor = Processor::new();
        processor.set(on(true, NoiseMode::Off, false));
        let (mut heard, mut sent) = (Vec::new(), Vec::new());
        for (n, said) in near.as_chunks::<FRAME>().0.iter().enumerate() {
            processor.render(&[0; FRAME_SAMPLES]);
            let mut frame = stereo(said);
            processor.capture(&mut frame);
            if n > 50 {
                heard.extend_from_slice(said);
                sent.extend(frame.iter().step_by(CHANNELS));
            }
        }
        let change = db(rms(&sent) / rms(&heard));
        assert!(change.abs() < 3., "speech moved by {change:.1} dB");
    }

    /// Steady hiss under the voice is brought down by the noise suppressor.
    #[test]
    fn noise_suppression_brings_steady_hiss_down() {
        let mut seed: u32 = 7;
        let hiss: Vec<i16> = (0..RATE as usize * 3)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1 << 24) as f32 * 2. - 1.) as f64 * 1500.
            })
            .map(|s| s as i16)
            .collect();
        let mut processor = Processor::new();
        processor.set(on(false, NoiseMode::Standard, false));
        let (mut heard, mut sent) = (Vec::new(), Vec::new());
        for (n, noise) in hiss.as_chunks::<FRAME>().0.iter().enumerate() {
            let mut frame = stereo(noise);
            processor.capture(&mut frame);
            if n > 50 {
                heard.extend_from_slice(noise);
                sent.extend(frame.iter().step_by(CHANNELS));
            }
        }
        let reduced = db(rms(&heard) / rms(&sent).max(1.));
        assert!(reduced > 6., "the hiss is only {reduced:.1} dB quieter");
    }

    /// Enhanced (voice isolation) brings steady hiss down across the band. The model
    /// silences the band it owns, and the high band above its 7.2 kHz cut follows the
    /// isolation's gain, so what reaches the send is quiet fullband, the way the WebRTC
    /// fallback suppresses. Measured over the whole band on purpose: the high band used to
    /// slip through untouched, which a below-the-cut bin would not have seen.
    #[test]
    fn enhanced_noise_brings_steady_hiss_down() {
        let mut seed: u32 = 7;
        let hiss: Vec<i16> = (0..RATE as usize * 3)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1 << 24) as f32 * 2. - 1.) as f64 * 1500.
            })
            .map(|s| s as i16)
            .collect();
        let mut processor = Processor::new();
        processor.set(on(false, NoiseMode::Enhanced, false));
        let (mut heard, mut sent) = (Vec::new(), Vec::new());
        // The model settles over the first second; measure after that.
        for (n, noise) in hiss.as_chunks::<FRAME>().0.iter().enumerate() {
            let mut frame = stereo(noise);
            processor.capture(&mut frame);
            if n > 50 {
                heard.extend_from_slice(noise);
                sent.extend(frame.iter().step_by(CHANNELS));
            }
        }
        let reduced = db(rms(&heard) / rms(&sent).max(1.));
        assert!(reduced > 15., "enhanced hiss is only {reduced:.1} dB quieter");
    }

    /// A 10 kHz tone is above the 7.2 kHz cut the bridge works at, and used to come back as
    /// silence. The original's high band rejoins the reconstruction, so what a fullband
    /// encoder sends keeps the sparkle isolation used to cut.
    #[test]
    fn the_bridge_keeps_a_tone_above_the_cut() {
        let mut band = Band::default();
        let mut phase = 0f64;
        let mut heard = Vec::new();
        for _ in 0..30 {
            let mut input = [0f32; FRAME];
            for sample in &mut input {
                phase += 10_000. / RATE as f64;
                *sample = (phase * std::f64::consts::TAU).sin() as f32 * 0.4;
            }
            let mut low = [0f32; LOW * 2];
            let mut out = [0f32; FRAME];
            band.down(&input, &mut low);
            band.up(&low, &low, &input, &mut out);
            heard.extend_from_slice(&out);
        }
        let settled = &heard[FRAME * 8..];
        let tone = bin_level(settled, 10_000.);
        assert!(
            tone > 0.02,
            "the 10 kHz tone did not survive the bridge: {tone}"
        );
    }

    /// The high band follows the isolation's gain: the same original through the same
    /// bridge, with the model's low band muted to a quarter, comes back with the band above
    /// the cut at a quarter too. So what the model silences below the cut is silenced above
    /// it, instead of every hiss harmonic over 7.2 kHz riding through untouched. The
    /// smoothed gain settles a step above the quarter it chases, hence the window.
    #[test]
    fn the_high_band_follows_the_isolation_gain() {
        fn run(mute: f32) -> Vec<f32> {
            let mut band = Band::default();
            let mut phase = 0f64;
            let mut heard = Vec::new();
            for n in 0..30 {
                let mut input = [0f32; FRAME];
                for sample in &mut input {
                    phase += 10_000. / RATE as f64;
                    *sample = (phase * std::f64::consts::TAU).sin() as f32 * 0.4;
                }
                let mut low = [0f32; LOW * 2];
                band.down(&input, &mut low);
                let mut kept = low;
                for sample in kept.iter_mut() {
                    *sample *= mute;
                }
                let mut out = [0f32; FRAME];
                band.up(&kept, &low, &input, &mut out);
                if n >= 15 {
                    heard.extend_from_slice(&out);
                }
            }
            heard
        }
        let ratio = bin_level(&run(0.25), 10_000.) / bin_level(&run(1.), 10_000.);
        assert!(
            (ratio - 0.25).abs() < 0.01,
            "the high band is at {ratio} of its unmuted level, not the isolation's quarter"
        );
    }

    /// The reconstruction lags the original by exactly the filters' delay. A wrong one would
    /// comb-filter every tone near the cut, so the lag is measured, not assumed. The high
    /// band is fed zeros, so only the reconstruction is measured.
    #[test]
    fn the_reconstruction_lags_the_original_by_the_filters_delay() {
        let mut seed: u32 = 11;
        let mut band = Band::default();
        let mut original = Vec::new();
        let mut out = Vec::new();
        for _ in 0..60 {
            let mut frame = [0f32; FRAME];
            for sample in &mut frame {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *sample = (seed >> 8) as f32 / (1 << 24) as f32 * 2. - 1.;
            }
            let mut low = [0f32; LOW * 2];
            let mut done = [0f32; FRAME];
            band.down(&frame, &mut low);
            band.up(&low, &low, &[0f32; FRAME], &mut done);
            original.extend_from_slice(&frame);
            out.extend_from_slice(&done);
        }
        let lag = lag_of(&original, &out, BRIDGE_DELAY, FRAME * 6);
        assert_eq!(lag, BRIDGE_DELAY, "the bridge's lag is {lag}");
    }

    /// The whole Enhanced path, voice isolation included, lags by the path delay the
    /// constants derive: two FIR centers plus the model's documented one-hop buffering. The
    /// input is harmonics with a wobbling pitch, gated like syllables: speech the model
    /// passes, so the correlation peak rides the signal and not the residue a silenced
    /// signal leaves, which is what used to wander the peak by tens of samples. The
    /// derivation is exact to the sample, and ±4 is under a percent of a cycle at 7.2 kHz,
    /// tight enough to guard the crossover's phase.
    #[test]
    fn the_enhanced_path_lags_the_original_by_the_path_delay() {
        let mut processor = Processor::new();
        processor.set(on(false, NoiseMode::Enhanced, false));
        let mut original = Vec::new();
        let mut out = Vec::new();
        for n in 0..200 {
            let mut frame = [0f32; FRAME];
            for (s, sample) in frame.iter_mut().enumerate() {
                let t = (n * FRAME + s) as f64 / f64::from(RATE);
                let f0 = 140. + 30. * (t * std::f64::consts::TAU * 3.).sin();
                let gate = if (t * 4.) as u32 % 3 == 2 { 0. } else { 1. };
                let mut voice = 0f64;
                for k in 1..=8 {
                    voice += (t * std::f64::consts::TAU * f0 * k as f64).sin() / k as f64;
                }
                *sample = (0.3 * gate * voice) as f32;
            }
            let mut stereo_frame = [0i16; FRAME_SAMPLES];
            for (pair, &s) in stereo_frame
                .as_chunks_mut::<CHANNELS>()
                .0
                .iter_mut()
                .zip(&frame)
            {
                pair.fill((s * 8000.) as i16);
            }
            processor.capture(&mut stereo_frame);
            original.extend_from_slice(&frame);
            out.extend(
                stereo_frame
                    .iter()
                    .step_by(CHANNELS)
                    .map(|&s| f32::from(s) / 32768.),
            );
        }
        // The model's normalisations take about a second to settle; correlate past them.
        let lag = lag_of(&original, &out, RECON_DELAY, FRAME * 75);
        assert!(
            lag.abs_diff(RECON_DELAY) <= 4,
            "the enhanced path's lag is {lag}, not within 4 of {RECON_DELAY}"
        );
    }

    /// The ring that delays the high band hands over exactly `DELAY` samples: an impulse's
    /// high-passed response lands with its peak at the path lag, where the reconstruction of
    /// its low band already sits. A ring read at the slot it writes hands every sample the
    /// whole previous frame back instead, which smears the high band 457 samples past the
    /// low band it joins. The lab's `high-band` driver proves the same.
    #[test]
    fn the_high_band_joins_at_the_path_delay() {
        assert_eq!(prove_high_band(), Ok(()));
    }

    /// Where `out` correlates best with `original`, over a window around `around`, after
    /// `settle` samples of warm-up.
    fn lag_of(original: &[f32], out: &[f32], around: usize, settle: usize) -> usize {
        let mut best = (0f64, 0usize);
        for lag in around - 40..=around + 40 {
            let (mut num, mut den) = (0f64, 0f64);
            for n in settle..out.len() {
                num += f64::from(out[n]) * f64::from(original[n - lag]);
                den += f64::from(original[n - lag]).powi(2);
            }
            let score = num.abs().sqrt() / den.sqrt().max(1e-9);
            if score > best.0 {
                best = (score, lag);
            }
        }
        best.1
    }

    /// A low tone survives the 48 kHz to 16 kHz bridge and back at nearly the same level.
    #[test]
    fn the_rate_bridge_keeps_a_low_tone() {
        let mut band = Band::default();
        let mut phase = 0f64;
        let (mut heard, mut sent) = (0f64, 0f64);
        let mut count = 0f64;
        for n in 0..100 {
            let mut input = [0f32; FRAME];
            for sample in &mut input {
                phase += 200. / RATE as f64;
                *sample = (phase * std::f64::consts::TAU).sin() as f32 * 0.4;
            }
            let mut low = [0f32; LOW * 2];
            let mut out = [0f32; FRAME];
            band.down(&input, &mut low);
            band.up(&low, &low, &input, &mut out);
            if n > 10 {
                heard += input.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
                sent += out.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
                count += input.len() as f64;
            }
        }
        let reduced = db((heard / count).sqrt() / (sent / count).sqrt().max(1e-9));
        assert!(
            reduced.abs() < 3.,
            "the bridge moved a tone by {reduced:.1} dB"
        );
    }

    /// A 6 kHz tone is inside the 16 kHz bridge. Linear interpolation paints a 10 kHz image
    /// of it, and fullband Opus then encodes that image.
    #[test]
    fn the_rate_bridge_does_not_image_a_high_tone() {
        let mut band = Band::default();
        let mut phase = 0f64;
        let mut heard = Vec::new();
        for _ in 0..20 {
            let mut input = [0f32; FRAME];
            for sample in &mut input {
                phase += 6000. / RATE as f64;
                *sample = (phase * std::f64::consts::TAU).sin() as f32 * 0.4;
            }
            let mut low = [0f32; LOW * 2];
            let mut out = [0f32; FRAME];
            band.down(&input, &mut low);
            band.up(&low, &low, &input, &mut out);
            heard.extend_from_slice(&out);
        }
        let settled = &heard[FRAME * 4..];
        let tone = bin_level(settled, 6000.);
        let image = bin_level(settled, 10000.);
        assert!(tone > 0.02, "the tone did not survive: {tone}");
        assert!(image * 10. < tone, "a 10 kHz image of {image} sits beside the tone {tone}");
    }

    fn bin_level(samples: &[f32], hz: f32) -> f64 {
        let step = hz as f64 / RATE as f64;
        let mut phase = 0f64;
        let (mut i, mut q) = (0f64, 0f64);
        for &sample in samples {
            let angle = phase * std::f64::consts::TAU;
            i += f64::from(sample) * angle.cos();
            q += f64::from(sample) * angle.sin();
            phase = (phase + step) % 1.;
        }
        (i * i + q * q).sqrt() / samples.len() as f64
    }

    /// Automatic gain brings a quiet voice up.
    #[test]
    fn auto_gain_brings_a_quiet_voice_up() {
        let quiet: Vec<i16> = far_end(6).into_iter().map(|s| s / 20).collect();
        let mut processor = Processor::new();
        processor.set(on(false, NoiseMode::Off, true));
        let (mut heard, mut sent) = (Vec::new(), Vec::new());
        for (n, said) in quiet.as_chunks::<FRAME>().0.iter().enumerate() {
            let mut frame = stereo(said);
            processor.capture(&mut frame);
            if n > 150 {
                heard.extend_from_slice(said);
                sent.extend(frame.iter().step_by(CHANNELS));
            }
        }
        let raised = db(rms(&sent) / rms(&heard));
        assert!(raised > 6., "a quiet voice is only {raised:.1} dB louder");
    }

    #[test]
    fn off_or_untold_leaves_the_microphone_as_captured() {
        let said = far_end(1);
        for chosen in [None, on(false, NoiseMode::Off, false)] {
            let mut processor = Processor::new();
            processor.set(chosen);
            assert!(!processor.active());
            let mut frame = stereo(&said[..FRAME]);
            let before = frame;
            processor.render(&before);
            processor.capture(&mut frame);
            assert_eq!(frame, before);
        }
    }

    #[test]
    fn input_gain_scales_and_zero_silences() {
        let mut frame = [1000i16; FRAME_SAMPLES];
        apply_input_gain(&mut frame, 50);
        assert_eq!(frame[0], 500);
        apply_input_gain(&mut frame, 0);
        assert_eq!(frame[0], 0);
        let mut full = [1000i16; FRAME_SAMPLES];
        apply_input_gain(&mut full, 100);
        assert_eq!(full[0], 1000);
    }

    /// The microphone test's stand-in is a 440 Hz tone at the same peak the device
    /// stand-in uses. Standard leaves it in the meter's band. Enhanced takes it down.
    #[test]
    fn standard_keeps_the_test_tone_and_enhanced_takes_it_down() {
        fn tone() -> Vec<[i16; FRAME_SAMPLES]> {
            let mut phase = 0f64;
            let step = 440. * std::f64::consts::TAU / f64::from(RATE);
            (0..25)
                .map(|_| {
                    let mut frame = [0i16; FRAME_SAMPLES];
                    for pair in frame.as_chunks_mut::<CHANNELS>().0 {
                        pair.fill((phase.sin() * 8000.) as i16);
                        phase = (phase + step) % std::f64::consts::TAU;
                    }
                    frame
                })
                .collect()
        }
        // The probe reports the loudest frame of each tenth of a second, and it does
        // not feed the speaker back while the meter runs without loopback.
        fn meter(noise: NoiseMode, frames: &[[i16; FRAME_SAMPLES]]) -> Vec<u8> {
            let mut processor = Processor::new();
            processor.set(on(true, noise, true));
            let mut levels = Vec::new();
            let mut loudest = 0u8;
            for (n, frame) in frames.iter().enumerate() {
                let mut frame = *frame;
                processor.capture(&mut frame);
                loudest = loudest.max(crate::audio::level(&frame));
                if (n + 1).is_multiple_of(5) {
                    levels.push(loudest);
                    loudest = 0;
                }
            }
            levels
        }
        let frames = tone();
        let standard = meter(NoiseMode::Standard, &frames);
        let enhanced = meter(NoiseMode::Enhanced, &frames);
        assert!(standard.iter().all(|level| (60..90).contains(level)), "standard: {standard:?}");
        assert!(enhanced.iter().any(|level| *level < 50), "enhanced left the tone up: {enhanced:?}");
    }
}
