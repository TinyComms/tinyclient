//! The microphone's processing (V10 to V12): echo cancellation, noise suppression and automatic
//! gain, through `sonora`, WebRTC's audio processing module ported to Rust (AEC3, its noise
//! suppressor and AGC2, with the high-pass filter in front).
//!
//! The echo canceller needs what the speaker plays: the call's mix goes in as the far end
//! ([`Processor::render`]) every tick, cues included, and each microphone frame comes out with
//! that echo taken away ([`Processor::capture`]). It finds the delay between the two itself.
//! Processing is mono at 48 kHz in 10 ms halves of our 20 ms frame; a stereo frame is folded
//! to mono going in and the result written to both channels coming out.
//!
//! Until chatd says what the user chose (`MediaRequest::Processing`), nothing is processed, and
//! with all three off the processor is not even built: the microphone is as captured.

use crate::{CHANNELS, FRAME, FRAME_SAMPLES, RATE};
use protocol::AudioProcessing;
use sonora::config::{AdaptiveDigital, EchoCanceller, GainController2, HighPassFilter, NoiseSuppression};
use sonora::{Config, StreamConfig};

/// Samples in one 10 ms block, the module's unit.
const BLOCK: usize = FRAME / 2;

pub struct Processor {
    apm: Option<sonora::AudioProcessing>,
    /// What is in force, so a repeat of the same choice changes nothing.
    chosen: Option<AudioProcessing>,
}

impl Default for Processor {
    fn default() -> Processor {
        Processor::new()
    }
}

/// The module's settings for the user's three switches.
fn config(chosen: AudioProcessing) -> Config {
    Config {
        high_pass_filter: Some(HighPassFilter::default()),
        echo_canceller: chosen.echo_cancellation.then(EchoCanceller::default),
        noise_suppression: chosen.noise_suppression.then(NoiseSuppression::default),
        // Adaptive digital gain only: it brings a quiet microphone up to speech level and never
        // touches the system's input volume.
        gain_controller2: chosen.auto_gain.then(|| GainController2 { adaptive_digital: Some(AdaptiveDigital::default()), ..GainController2::default() }),
        ..Config::default()
    }
}

impl Processor {
    /// Off: frames pass untouched.
    pub fn new() -> Processor {
        Processor { apm: None, chosen: None }
    }

    /// Takes the user's choice. All off, or `None`, turns processing off.
    pub fn set(&mut self, chosen: Option<AudioProcessing>) {
        if chosen == self.chosen {
            return;
        }
        self.chosen = chosen;
        let wanted = chosen.filter(|c| c.echo_cancellation || c.noise_suppression || c.auto_gain);
        self.apm = wanted.map(|chosen| {
            let stream = StreamConfig::new(RATE, 1);
            sonora::AudioProcessing::builder().config(config(chosen)).capture_config(stream).render_config(stream).build()
        });
    }

    pub fn active(&self) -> bool {
        self.apm.is_some()
    }

    /// What the speaker is about to play: the echo canceller's reference.
    pub fn render(&mut self, frame: &[i16; FRAME_SAMPLES]) {
        let Some(apm) = &mut self.apm else {
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
        let Some(apm) = &mut self.apm else {
            return;
        };
        let mono = fold(frame);
        let mut done = [0f32; FRAME];
        for (block, out) in mono.as_chunks::<BLOCK>().0.iter().zip(done.as_chunks_mut::<BLOCK>().0) {
            if apm.process_capture_f32(&[&block[..]], &mut [&mut out[..]]).is_err() {
                out.copy_from_slice(block);
            }
        }
        for (pair, sample) in frame.as_chunks_mut::<CHANNELS>().0.iter_mut().zip(done) {
            pair.fill((sample.clamp(-1., 1.) * 32767.) as i16);
        }
    }

    /// How much echo the canceller is taking away now, in dB, once it knows.
    pub fn echo_reduction_db(&self) -> Option<f64> {
        self.apm.as_ref()?.statistics().echo_return_loss_enhancement
    }
}

/// A stereo frame as mono floats.
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
        (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
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
                let syllables = (n as f32 / RATE as f32 * 3. * std::f32::consts::TAU).sin().abs() + 0.2;
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

    fn on(echo_cancellation: bool, noise_suppression: bool, auto_gain: bool) -> Option<AudioProcessing> {
        Some(AudioProcessing { echo_cancellation, noise_suppression, auto_gain })
    }

    /// The speaker's sound reaches the microphone 60 ms later at 40 %: what was played comes
    /// back, and the canceller takes it out. Measured over the last four seconds of ten, once
    /// it has converged.
    #[test]
    fn the_echo_canceller_takes_away_what_the_speaker_played() {
        let far = far_end(10);
        let delay = RATE as usize * 60 / 1000;
        let mut processor = Processor::new();
        processor.set(on(true, false, false));
        let (mut heard, mut sent) = (Vec::new(), Vec::new());
        for (n, played) in far.as_chunks::<FRAME>().0.iter().enumerate() {
            processor.render(&stereo(played));
            let echo: Vec<i16> = (n * FRAME..(n + 1) * FRAME).map(|i| if i >= delay { (f64::from(far[i - delay]) * 0.4) as i16 } else { 0 }).collect();
            let mut frame = stereo(&echo);
            processor.capture(&mut frame);
            if n * FRAME >= RATE as usize * 6 {
                heard.extend_from_slice(&echo);
                sent.extend(frame.iter().step_by(CHANNELS));
            }
        }
        let reduced = db(rms(&heard) / rms(&sent).max(1.));
        assert!(reduced > 20., "the echo is only {reduced:.1} dB quieter");
        assert!(processor.echo_reduction_db().is_some_and(|db| db > 10.), "{:?}", processor.echo_reduction_db());
    }

    /// With nothing played, speech goes through the canceller untouched in level.
    #[test]
    fn speech_with_no_echo_goes_through() {
        let near = far_end(4);
        let mut processor = Processor::new();
        processor.set(on(true, false, false));
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
        processor.set(on(false, true, false));
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

    /// Automatic gain brings a quiet voice up.
    #[test]
    fn auto_gain_brings_a_quiet_voice_up() {
        let quiet: Vec<i16> = far_end(6).into_iter().map(|s| s / 20).collect();
        let mut processor = Processor::new();
        processor.set(on(false, false, true));
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
        for chosen in [None, on(false, false, false)] {
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
}
