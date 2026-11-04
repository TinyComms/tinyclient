//! The microphone's processing (V10 to V12): echo cancellation, noise suppression and automatic
//! gain, through `sonora`, WebRTC's audio processing module ported to Rust (AEC3, its noise
//! suppressor and AGC2, with the high-pass filter in front). Enhanced noise, the default,
//! isolates the near voice with `hush-vani` (weights compiled in) after echo cancellation and
//! before automatic gain, so another voice or a cry is turned down instead of being raised.
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
    prev: f32,
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
            prev: 0.,
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
            self.band.up(&clean, &mut done);
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

    /// 16 kHz mono in, 48 kHz mono out. Linear, one sample late, which is under a tenth of a
    /// millisecond on top of the model's own 10 ms.
    fn up(&mut self, input: &[f32; LOW * 2], out: &mut [f32; FRAME]) {
        let mut last = self.prev;
        for (i, &sample) in input.iter().enumerate() {
            let at = i * 3;
            out[at] = last;
            out[at + 1] = last + (sample - last) / 3.;
            out[at + 2] = last + (sample - last) * 2. / 3.;
            last = sample;
        }
        self.prev = last;
    }
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

    /// Enhanced (voice isolation) brings steady hiss down hard. The 16 kHz bridge alone is only
    /// a few dB on white noise, so the bar sits well above that.
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
        assert!(
            reduced > 20.,
            "enhanced hiss is only {reduced:.1} dB quieter"
        );
    }

    /// A low tone survives the 48 kHz to 16 kHz bridge and back at nearly the same level.
    #[test]
    fn the_rate_bridge_keeps_a_low_tone() {
        let mut band = Band::default();
        let mut phase = 0f32;
        let (mut heard, mut sent) = (0f64, 0f64);
        let mut count = 0f64;
        for n in 0..100 {
            let mut input = [0f32; FRAME];
            for sample in &mut input {
                phase += 200. / RATE as f32;
                *sample = (phase * std::f32::consts::TAU).sin() * 0.4;
            }
            let mut low = [0f32; LOW * 2];
            let mut out = [0f32; FRAME];
            band.down(&input, &mut low);
            band.up(&low, &mut out);
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
}
