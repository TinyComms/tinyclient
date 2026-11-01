//! What each voice is played at (V9, priority speech): one person's volume for me from 0 to
//! 200 % or muted for me alone (`MediaRequest::Gain`), and everyone else
//! [`PRIORITY_DUCK_DB`] quieter while someone holding priority speech is speaking
//! (`MediaRequest::Priority`). Who is speaking is judged on the voice as it arrived, before any
//! of this, so a person I muted still lights up when they talk.
//!
//! A gain that changes goes from the old to the new across one frame, so nothing clicks. The
//! sum goes through a [`Limiter`] on its way to the speaker, so a loud room is never clipped.

use crate::FRAME_SAMPLES;
use protocol::{GAIN_MAX, PRIORITY_DUCK_DB};
use std::collections::HashMap;

/// One person's choice: percent, and muted for me.
pub type Gain = (u16, bool);

/// [`PRIORITY_DUCK_DB`] as a factor: about a quarter.
pub fn duck() -> f32 {
    10f32.powf(-f32::from(PRIORITY_DUCK_DB) / 20.)
}

/// The choices of one tick.
#[derive(Debug, Clone, Default)]
pub struct Ear {
    pub gains: HashMap<String, Gain>,
    pub priority: Vec<String>,
}

impl Ear {
    /// Whether anyone holding priority is speaking now. `speakers` is each voice's user id (empty
    /// while unknown) and whether it is speaking.
    pub fn ducking<'a>(&self, mut speakers: impl Iterator<Item = (&'a str, bool)>) -> bool {
        !self.priority.is_empty() && speakers.any(|(user, speaking)| speaking && !user.is_empty() && self.priority.iter().any(|p| p == user))
    }

    /// The factor `user`'s voice is played at.
    pub fn factor(&self, user: &str, ducked: bool) -> f32 {
        let (percent, muted) = self.gains.get(user).copied().unwrap_or((100, false));
        if muted {
            return 0.;
        }
        let own = f32::from(percent.min(GAIN_MAX)) / 100.;
        let held = !user.is_empty() && self.priority.iter().any(|p| p == user);
        if ducked && !held {
            own * duck()
        } else {
            own
        }
    }
}

/// Adds `frame` to the mix, its gain going from `from` at its first sample to `to` at its last.
pub fn add(mix: &mut [i32; FRAME_SAMPLES], frame: &[i16], from: f32, to: f32) {
    let pairs = (frame.len() / crate::CHANNELS).max(1) as f32;
    for (n, (sums, samples)) in mix.as_chunks_mut::<{ crate::CHANNELS }>().0.iter_mut().zip(frame.as_chunks::<{ crate::CHANNELS }>().0).enumerate() {
        let gain = if from == to { to } else { from + (to - from) * (n + 1) as f32 / pairs };
        for (sum, &sample) in sums.iter_mut().zip(samples) {
            *sum += (f32::from(sample) * gain).round() as i32;
        }
    }
}

/// Peaks are held to this, about -1 dBFS: the mix is never clipped.
const CEILING: f32 = 29_200.;
/// Frames the limiter looks ahead (1 ms), so its gain is already down when a peak arrives.
const LOOKAHEAD: usize = 48;
/// How fast the gain comes back up after a peak: about 80 ms.
const RELEASE: f32 = 1. / (0.08 * crate::RATE as f32);

/// The mix's last stage: everyone's voices added up can go past full scale, and a clamp there
/// is the crackle of clipping. A look-ahead peak limiter turns the whole mix down smoothly for
/// a moment instead, and a soft knee above [`CEILING`] catches what is left. Below the ceiling
/// it passes the mix untouched, 1 ms late.
#[derive(Debug)]
pub struct Limiter {
    /// The last [`LOOKAHEAD`] frames, waiting to go out.
    delay: [[f32; crate::CHANNELS]; LOOKAHEAD],
    at: usize,
    /// Gain each of those frames needs on its own.
    needs: [f32; LOOKAHEAD],
    gain: f32,
}

impl Default for Limiter {
    fn default() -> Limiter {
        Limiter { delay: [[0.; crate::CHANNELS]; LOOKAHEAD], at: 0, needs: [1.; LOOKAHEAD], gain: 1. }
    }
}

impl Limiter {
    /// The mix of one tick, limited, as samples.
    pub fn run(&mut self, mix: &[i32; FRAME_SAMPLES], out: &mut [i16; FRAME_SAMPLES]) {
        for (pair, sums) in out.as_chunks_mut::<{ crate::CHANNELS }>().0.iter_mut().zip(mix.as_chunks::<{ crate::CHANNELS }>().0) {
            let peak = sums.iter().fold(0f32, |peak, &s| peak.max((s as f32).abs()));
            let need = if peak > CEILING { CEILING / peak } else { 1. };
            let leaving = self.delay[self.at];
            self.delay[self.at] = sums.map(|s| s as f32);
            self.needs[self.at] = need;
            self.at = (self.at + 1) % LOOKAHEAD;
            // The lowest any frame in sight needs: down toward it within the look-ahead, back up
            // slowly once it has passed.
            let target = self.needs.iter().fold(1f32, |low, &n| low.min(n));
            if target < self.gain {
                self.gain += (target - self.gain) * (4. / LOOKAHEAD as f32);
            } else {
                self.gain += (target - self.gain) * RELEASE;
            }
            for (out, sample) in pair.iter_mut().zip(leaving) {
                *out = knee(sample * self.gain) as i16;
            }
        }
    }
}

/// Above the ceiling, a curve that bends toward full scale and never reaches it.
fn knee(sample: f32) -> f32 {
    let size = sample.abs();
    if size <= CEILING {
        return sample;
    }
    let room = 32_767. - CEILING;
    sample.signum() * (CEILING + room * ((size - CEILING) / room).tanh())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ear(gains: &[(&str, u16, bool)], priority: &[&str]) -> Ear {
        Ear {
            gains: gains.iter().map(|(user, percent, muted)| ((*user).to_owned(), (*percent, *muted))).collect(),
            priority: priority.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[test]
    fn a_person_is_played_at_their_own_volume_or_not_at_all() {
        let ear = ear(&[("ada", 50, false), ("bea", 200, false), ("cy", 150, true), ("dee", 900, false)], &[]);
        assert_eq!(ear.factor("ada", false), 0.5);
        assert_eq!(ear.factor("bea", false), 2.);
        assert_eq!(ear.factor("cy", false), 0., "muted for me, whatever the volume");
        assert_eq!(ear.factor("dee", false), 2., "held to 200 %");
        assert_eq!(ear.factor("eve", false), 1., "never named is as decoded");
        assert_eq!(ear.factor("", false), 1.);
    }

    #[test]
    fn while_a_priority_speaker_talks_everyone_else_is_twelve_db_quieter() {
        let ear = ear(&[("ada", 50, false)], &["boss"]);
        assert!(!ear.ducking([("boss", false), ("ada", true)].into_iter()), "only while they speak");
        assert!(ear.ducking([("boss", true), ("ada", true)].into_iter()));
        assert!(!self::ear(&[], &[]).ducking([("boss", true)].into_iter()));
        let quarter = ear.factor("eve", true);
        assert!((20. * quarter.log10() + 12.).abs() < 0.01, "{quarter}");
        assert!((ear.factor("ada", true) - 0.5 * quarter).abs() < 1e-6, "on top of their own volume");
        assert_eq!(ear.factor("boss", true), 1., "the priority speaker is not ducked");
    }

    /// Three people shouting at full scale: the sum is three times too loud for the speaker.
    /// Nothing is clipped (no sample at full scale, no flat tops) and no step is bigger than
    /// the voices themselves make.
    #[test]
    fn a_loud_room_is_limited_not_clipped() {
        let mut limiter = Limiter::default();
        let (mut peak, mut flat, mut steepest) = (0i32, 0, 0i32);
        let mut last = 0i16;
        for tick in 0..100 {
            let mut mix = [0i32; FRAME_SAMPLES];
            for (n, pair) in mix.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let t = (tick * 960 + n) as f32 / 48_000.;
                let loud = if tick < 50 { 3. } else { 0.3 };
                let voice = |hz: f32| (t * hz * std::f32::consts::TAU).sin() * 32_000. * loud / 3.;
                pair.fill((voice(220.) + voice(330.) + voice(495.)) as i32);
            }
            let mut out = [0i16; FRAME_SAMPLES];
            limiter.run(&mix, &mut out);
            for &sample in out.iter().step_by(2) {
                peak = peak.max(i32::from(sample).abs());
                flat += i32::from(sample == last && sample.unsigned_abs() > 20_000);
                steepest = steepest.max((i32::from(sample) - i32::from(last)).abs());
                last = sample;
            }
        }
        assert!(peak < 32_767, "clipped: {peak}");
        assert!(peak > 25_000, "and still loud: {peak}");
        assert!(flat < 10, "{flat} flat-topped samples");
        assert!(steepest < 3_500, "a step of {steepest}");
    }

    #[test]
    fn under_the_ceiling_the_mix_passes_untouched_a_millisecond_late() {
        let mut limiter = Limiter::default();
        let mut mix = [0i32; FRAME_SAMPLES];
        for (n, sample) in mix.iter_mut().enumerate() {
            *sample = (n as i32 % 200) * 100 - 10_000;
        }
        let mut out = [0i16; FRAME_SAMPLES];
        limiter.run(&mix, &mut out);
        assert!(out[..2 * LOOKAHEAD].iter().all(|&s| s == 0));
        for (n, &sample) in out[2 * LOOKAHEAD..].iter().enumerate() {
            assert_eq!(i32::from(sample), mix[n]);
        }
    }

    #[test]
    fn a_change_of_gain_ramps_across_the_frame() {
        let frame = [1000i16; FRAME_SAMPLES];
        let mut mix = [0i32; FRAME_SAMPLES];
        add(&mut mix, &frame, 1., 1.);
        assert!(mix.iter().all(|&s| s == 1000));
        let mut mix = [0i32; FRAME_SAMPLES];
        add(&mut mix, &frame, 1., 0.);
        assert!(mix[0] > 990 && mix[0] < 1000, "starts where it was: {}", mix[0]);
        assert_eq!(mix[FRAME_SAMPLES - 1], 0, "ends where it goes");
        assert!(mix.windows(2).all(|w| w[0] >= w[1]), "and falls all the way");
    }
}
