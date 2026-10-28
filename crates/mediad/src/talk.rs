//! How the microphone opens (V6, V8): by voice activation at the user's threshold, or only while
//! a push-to-talk key is held and a moment after (`MediaRequest::Input`, `Talk`). The speech gate
//! (`media::Gate`) does the rest: what goes out ahead of a word, and the silence after it.

use crate::media::HOLD;
use protocol::InputMode;
use std::time::{Duration, Instant};

/// The push-to-talk keys as the shell last said: each held or not, and when the last of them
/// was let go.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub talk: bool,
    pub priority: bool,
    pub released: Option<Instant>,
}

impl Keys {
    /// One key went down or up (`MediaRequest::Talk`).
    pub fn press(&mut self, held: bool, priority: bool, now: Instant) {
        let was = self.talk || self.priority;
        if priority {
            self.priority = held;
        } else {
            self.talk = held;
        }
        if was && !(self.talk || self.priority) {
            self.released = Some(now);
        }
    }

    fn held(&self) -> bool {
        self.talk || self.priority
    }
}

/// Whether the microphone is to be open this tick, and how many quiet ticks it then stays open
/// for. `heard` is the frame's level (`audio::level`, after processing). Voice activation opens
/// at the threshold and holds through the pauses between words; push-to-talk opens while a key
/// is held and for `release_ms` after, and shuts the moment that is over. The priority key opens
/// it in either mode.
pub fn opening(mode: InputMode, keys: &Keys, heard: u8, now: Instant) -> (bool, u32) {
    match mode {
        InputMode::Voice { threshold } => (heard >= threshold || keys.priority, HOLD),
        InputMode::PushToTalk { release_ms } => {
            let lingering = keys.released.is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(u64::from(release_ms)));
            (keys.held() || lingering, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PTT: InputMode = InputMode::PushToTalk { release_ms: 200 };

    #[test]
    fn voice_activation_opens_at_the_threshold_and_ignores_the_plain_key() {
        let now = Instant::now();
        let voice = InputMode::Voice { threshold: 40 };
        let mut keys = Keys::default();
        assert_eq!(opening(voice, &keys, 39, now), (false, HOLD));
        assert_eq!(opening(voice, &keys, 40, now), (true, HOLD));
        keys.press(true, false, now);
        assert!(!opening(voice, &keys, 10, now).0, "the push-to-talk key does nothing here");
        keys.press(true, true, now);
        assert!(opening(voice, &keys, 10, now).0, "the priority key opens it in either mode");
    }

    #[test]
    fn push_to_talk_opens_while_held_and_for_the_release_delay_after() {
        let start = Instant::now();
        let mut keys = Keys::default();
        assert!(!opening(PTT, &keys, 100, start).0, "however loud, not without the key");
        keys.press(true, false, start);
        assert_eq!(opening(PTT, &keys, 0, start), (true, 0), "held, even in silence");
        keys.press(false, false, start + Duration::from_millis(500));
        let after = |ms: u64| start + Duration::from_millis(500 + ms);
        assert!(opening(PTT, &keys, 0, after(199)).0);
        assert!(!opening(PTT, &keys, 0, after(200)).0);
        let none = InputMode::PushToTalk { release_ms: 0 };
        assert!(!opening(none, &keys, 0, after(0)).0, "no delay shuts at once");
    }

    #[test]
    fn letting_go_of_one_key_while_the_other_is_held_keeps_it_open() {
        let now = Instant::now();
        let mut keys = Keys::default();
        keys.press(true, false, now);
        keys.press(true, true, now);
        keys.press(false, false, now);
        assert_eq!(keys.released, None);
        assert!(opening(PTT, &keys, 0, now + Duration::from_secs(5)).0);
        keys.press(false, true, now + Duration::from_secs(5));
        assert_eq!(keys.released, Some(now + Duration::from_secs(5)));
    }
}
