//! Faster playback that keeps the voice's pitch: WSOLA (waveform-similarity overlap-add). The
//! input is cut into overlapping 21 ms windows taken `rate` times further apart than they are
//! laid down; each is nudged by up to 5 ms to where it best continues the one before, so the
//! seams fall in step and no warble or click is heard. Speeding up by resampling instead would
//! raise every voice by a fifth or an octave.

use crate::CHANNELS;

/// Frames in one window. A hop is half of it.
const WINDOW: usize = 1024;
const HOP: usize = WINDOW / 2;
/// How far a window may move, in frames, to line up with the last.
const SEEK: usize = 240;

pub struct Stretch {
    /// Percent of normal speed. 100 passes everything through untouched.
    rate: u16,
    /// Interleaved input not yet done with.
    input: Vec<f32>,
    /// Where the next window would start with no nudging, in frames from the start of `input`.
    nominal: f64,
    /// Where the input carries on naturally from the last window (half a window into it), in
    /// frames from the start of `input`: what the next window should sound like.
    natural: Option<usize>,
    /// The second half of the last window, already weighted, waiting for the next to add to it.
    tail: Vec<f32>,
    weights: Vec<f32>,
}

impl Stretch {
    pub fn new(rate: u16) -> Stretch {
        // A periodic Hann window: two of them half a window apart add up to exactly one.
        let weights = (0..WINDOW)
            .map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / WINDOW as f32).cos())
            .collect();
        Stretch { rate, input: Vec::new(), nominal: 0., natural: None, tail: vec![0.; HOP * CHANNELS], weights }
    }

    pub fn rate(&self) -> u16 {
        self.rate
    }

    /// Input frames taken in and not yet played out: what a position has to subtract.
    pub fn held(&self) -> usize {
        if self.rate == 100 {
            return 0;
        }
        (self.input.len() / CHANNELS).saturating_sub(self.nominal as usize)
    }

    /// Starts again at another speed, or after a seek. Gives back the input not yet used, to
    /// be fed again, so nothing is skipped.
    pub fn reset(&mut self, rate: u16) -> Vec<f32> {
        let from = (self.nominal as usize * CHANNELS).min(self.input.len());
        let left = self.input.split_off(from);
        *self = Stretch::new(rate);
        left
    }

    /// Takes interleaved 48 kHz stereo and adds what it makes of it to `output`.
    pub fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.rate == 100 {
            output.extend_from_slice(input);
            return;
        }
        self.input.extend_from_slice(input);
        let step = HOP as f64 * f64::from(self.rate) / 100.;
        loop {
            let nominal = self.nominal as usize;
            let frames = self.input.len() / CHANNELS;
            // The window may land anywhere up to SEEK past where it is due, and the last
            // window's natural continuation must be there to compare with.
            let natural = self.natural.map_or(0, |natural| natural + HOP);
            if nominal + SEEK + WINDOW > frames || natural > frames {
                break;
            }
            let start = match self.natural {
                None => nominal,
                Some(natural) => self.best(natural, nominal),
            };
            let window = &self.input[start * CHANNELS..(start + WINDOW) * CHANNELS];
            for (n, (frame, weight)) in window.chunks_exact(CHANNELS).zip(&self.weights).enumerate() {
                for (channel, sample) in frame.iter().enumerate() {
                    if n < HOP {
                        output.push(self.tail[n * CHANNELS + channel] + sample * weight);
                    } else {
                        self.tail[(n - HOP) * CHANNELS + channel] = sample * weight;
                    }
                }
            }
            self.nominal += step;
            // What no later window can reach, nor be compared with, goes.
            let done = (self.nominal as usize).saturating_sub(SEEK).min(start + HOP);
            self.natural = Some(start + HOP - done);
            if done > 0 {
                self.input.drain(..done * CHANNELS);
                self.nominal -= done as f64;
            }
        }
    }

    /// Where near `nominal` a window starts that sounds most like the input at `natural`: the
    /// largest correlation of the two, over half a window, on every other frame of the mix.
    fn best(&self, natural: usize, nominal: usize) -> usize {
        let mono = |at: usize| self.input[at * CHANNELS] + self.input[at * CHANNELS + 1];
        let low = nominal.saturating_sub(SEEK);
        let mut best = (f32::MIN, nominal);
        for start in (low..=nominal + SEEK).step_by(2) {
            let mut sum = 0.;
            for n in (0..HOP).step_by(2) {
                sum += mono(natural + n) * mono(start + n);
            }
            if sum > best.0 {
                best = (sum, start);
            }
        }
        best.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hertz: f32, frames: usize) -> Vec<f32> {
        (0..frames).flat_map(|n| [(n as f32 * hertz * std::f32::consts::TAU / 48_000.).sin() * 0.5; 2]).collect()
    }

    /// Rising zero crossings of the left channel a second: the pitch.
    fn pitch(pcm: &[f32]) -> f32 {
        let left: Vec<f32> = pcm.chunks_exact(2).map(|frame| frame[0]).collect();
        let rising = left.windows(2).filter(|pair| pair[0] < 0. && pair[1] >= 0.).count();
        rising as f32 * 48_000. / left.len() as f32
    }

    fn stretched(rate: u16, input: &[f32]) -> Vec<f32> {
        let mut stretch = Stretch::new(rate);
        let mut output = Vec::new();
        for piece in input.chunks(2 * 960) {
            stretch.push(piece, &mut output);
        }
        output
    }

    #[test]
    fn faster_is_shorter_at_the_same_pitch() {
        let input = tone(440., 48_000 * 2);
        for (rate, seconds) in [(150, 2. / 1.5), (200, 1.)] {
            let output = stretched(rate, &input);
            let length = output.len() as f32 / 2. / 48_000.;
            // Less the windows still held at the end.
            assert!((seconds - 0.05..=seconds).contains(&length), "{rate}%: {length} s");
            let heard = pitch(&output[2 * WINDOW..]);
            assert!((430. ..450.).contains(&heard), "{rate}%: {heard} Hz");
        }
    }

    #[test]
    fn the_seams_neither_click_nor_dip() {
        let output = stretched(150, &tone(440., 48_000));
        let left: Vec<f32> = output[2 * WINDOW..].chunks_exact(2).map(|frame| frame[0]).collect();
        // A 440 Hz tone at 0.5 moves at most 0.029 a sample; a seam out of step jumps further.
        let jump = left.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0., f32::max);
        assert!(jump < 0.05, "a jump of {jump}");
        // And the level holds through every seam.
        let peaks: Vec<f32> = left.chunks(HOP).map(|part| part.iter().fold(0., |max: f32, s| max.max(s.abs()))).collect();
        assert!(peaks[..peaks.len() - 1].iter().all(|peak| (0.45..0.55).contains(peak)), "{peaks:?}");
    }

    #[test]
    fn normal_speed_is_untouched_and_a_reset_loses_nothing() {
        let input = tone(440., 4_800);
        assert_eq!(stretched(100, &input), input);
        let mut stretch = Stretch::new(200);
        let mut output = Vec::new();
        stretch.push(&input, &mut output);
        let held = stretch.held();
        let left = stretch.reset(100);
        assert_eq!(left.len() / 2, held, "what is given back is what was held");
        assert_eq!(stretch.rate(), 100);
    }
}
