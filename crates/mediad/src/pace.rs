//! A call's voice played a little slower or faster, a frame at a time and at its own pitch, so
//! the jitter buffer can be kept where it should be without running dry or holding on to delay.
//! A frame gets one pitch period longer ([`expand`]) or shorter ([`accelerate`]): two periods
//! that sound alike are crossfaded into three, or into one. Voiced speech repeats itself period
//! by period, so neither is heard; nothing waits for more audio than the frame, so neither adds
//! delay. This is what WebRTC's NetEq calls preemptive expand and accelerate.

use crate::CHANNELS;

/// The shortest and longest period looked for: 2.5 and 10 ms (400 down to 100 Hz). Two of the
/// longest fit in a 20 ms frame.
const SHORTEST: usize = 120;
const LONGEST: usize = 480;
/// How alike two periods must be, as a normalized correlation, for one to stand in for the
/// other. Below this the frame is not periodic enough, and it plays as it is.
const ALIKE: f32 = 0.8;
/// Below this level (mean square of the mono mix) a frame is quiet enough that any period will
/// do: nothing in it is heard to repeat.
const QUIET: f32 = 300. * 300.;

/// The period at which the start of `frame` (interleaved stereo) best repeats, in frames, and
/// whether it is alike enough to stretch or squeeze by.
fn period(frame: &[i16]) -> Option<usize> {
    let mono: Vec<f32> = frame.chunks_exact(CHANNELS).map(|pair| (f32::from(pair[0]) + f32::from(pair[1])) / 2.).collect();
    let longest = LONGEST.min(mono.len() / 2);
    if longest < SHORTEST {
        return None;
    }
    let power = mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32;
    if power < QUIET {
        return Some(longest);
    }
    let mut best = (ALIKE, None);
    for lag in SHORTEST..=longest {
        let (a, b) = (&mono[..lag], &mono[lag..2 * lag]);
        let (mut cross, mut ea, mut eb) = (0f32, 0f32, 0f32);
        for (x, y) in a.iter().zip(b) {
            cross += x * y;
            ea += x * x;
            eb += y * y;
        }
        let alike = cross / (ea * eb).sqrt().max(1.);
        if alike > best.0 {
            best = (alike, Some(lag));
        }
    }
    best.1
}

fn blend(from: i16, to: i16, toward: f32) -> i16 {
    (f32::from(from) * (1. - toward) + f32::from(to) * toward).round() as i16
}

/// `frame` one pitch period shorter: the first two periods become one that fades from the first
/// into the second, and the rest follows on from where the second ended. `None` when it does
/// not repeat well enough to.
pub fn accelerate(frame: &[i16]) -> Option<Vec<i16>> {
    let lag = period(frame)?;
    let mut out = Vec::with_capacity(frame.len() - lag * CHANNELS);
    for n in 0..lag {
        let toward = (n as f32 + 0.5) / lag as f32;
        for channel in 0..CHANNELS {
            out.push(blend(frame[n * CHANNELS + channel], frame[(n + lag) * CHANNELS + channel], toward));
        }
    }
    out.extend_from_slice(&frame[2 * lag * CHANNELS..]);
    Some(out)
}

/// `frame` one pitch period longer: after the first period comes the second fading back into
/// the first, and then the second again and the rest. `None` when it does not repeat well
/// enough to.
pub fn expand(frame: &[i16]) -> Option<Vec<i16>> {
    let lag = period(frame)?;
    let mut out = Vec::with_capacity(frame.len() + lag * CHANNELS);
    out.extend_from_slice(&frame[..lag * CHANNELS]);
    for n in 0..lag {
        let toward = (n as f32 + 0.5) / lag as f32;
        for channel in 0..CHANNELS {
            out.push(blend(frame[(n + lag) * CHANNELS + channel], frame[n * CHANNELS + channel], toward));
        }
    }
    out.extend_from_slice(&frame[lag * CHANNELS..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME;

    /// A voiced sound: a 200 Hz fundamental with two harmonics, as stereo.
    fn voiced(frames: usize, offset: usize) -> Vec<i16> {
        (offset..offset + frames)
            .flat_map(|n| {
                let t = n as f32 * std::f32::consts::TAU * 200. / 48_000.;
                let sample = (t.sin() * 6000. + (2. * t).sin() * 2500. + (3. * t).sin() * 1200.) as i16;
                [sample; 2]
            })
            .collect()
    }

    fn largest_step(pcm: &[i16]) -> i32 {
        pcm.chunks_exact(CHANNELS).collect::<Vec<_>>().windows(2).map(|pair| (i32::from(pair[1][0]) - i32::from(pair[0][0])).abs()).max().unwrap()
    }

    #[test]
    fn a_voiced_frame_grows_and_shrinks_by_whole_periods_without_a_step() {
        let frame = voiced(FRAME, 0);
        // The steepest the sound itself moves in a sample, and a little room for rounding.
        let natural = largest_step(&frame) + 40;
        let longer = expand(&frame).expect("periodic");
        let shorter = accelerate(&frame).expect("periodic");
        // 200 Hz is 240 frames a period, and its multiples repeat too.
        assert_eq!((longer.len() / CHANNELS - FRAME) % 240, 0, "{}", longer.len() / CHANNELS);
        assert_eq!((FRAME - shorter.len() / CHANNELS) % 240, 0, "{}", shorter.len() / CHANNELS);
        assert!(largest_step(&longer) <= natural, "expand steps {} against {natural}", largest_step(&longer));
        assert!(largest_step(&shorter) <= natural, "accelerate steps {} against {natural}", largest_step(&shorter));
        // And the frame still ends where it did, so the next one follows on.
        assert_eq!(longer[longer.len() - CHANNELS..], frame[frame.len() - CHANNELS..]);
        assert_eq!(shorter[shorter.len() - CHANNELS..], frame[frame.len() - CHANNELS..]);
    }

    #[test]
    fn noise_is_left_alone_and_quiet_goes_either_way() {
        let mut seed = 7u32;
        let noise: Vec<i16> = (0..FRAME * CHANNELS)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 16) as i16
            })
            .collect();
        assert_eq!(expand(&noise), None);
        assert_eq!(accelerate(&noise), None);
        let quiet = vec![3i16; FRAME * CHANNELS];
        assert_eq!(expand(&quiet).map(|out| out.len() / CHANNELS), Some(FRAME + LONGEST));
        assert_eq!(accelerate(&quiet).map(|out| out.len() / CHANNELS), Some(FRAME - LONGEST));
    }
}
