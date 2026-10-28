//! What each voice is played at (V9, priority speech): one person's volume for me from 0 to
//! 200 % or muted for me alone (`MediaRequest::Gain`), and everyone else
//! [`PRIORITY_DUCK_DB`] quieter while someone holding priority speech is speaking
//! (`MediaRequest::Priority`). Who is speaking is judged on the voice as it arrived, before any
//! of this, so a person I muted still lights up when they talk.
//!
//! A gain that changes goes from the old to the new across one frame, so nothing clicks.

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
