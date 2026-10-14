//! Playing a voice message or an audio file: what the shell asks, what it is told, and the
//! frames between chatd and the `mediad --play` that does it.
//!
//! One file plays at a time, across every network. chatd decodes nothing: it starts a mediad in
//! its playback mode for the file, and that process exits when playing stops, so a paused or
//! finished message holds no decoder, buffer or device.

use crate::{AccountKey, Key};
use serde::{Deserialize, Serialize};

/// Speeds a voice message can play at, in percent.
pub const RATES: [u16; 3] = [100, 150, 200];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlayState {
    /// mediad is starting, opening the file and the speaker.
    Loading,
    Playing,
    Paused,
    /// It played to the end. `at` is the end.
    Ended,
    /// It cannot play, in plain words.
    Failed { detail: String },
}

/// The file that is playing, or was last played. Its position moves with the clock while
/// `Playing`: [`Playback::position`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playback {
    pub account: AccountKey,
    /// The attachment's `MediaFile::id`.
    pub file: String,
    /// Where it was posted and who posted it, for the now-playing control.
    pub conversation: Key,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(flatten)]
    pub state: PlayState,
    /// Milliseconds into the file at `since`.
    pub at: u32,
    /// Unix milliseconds when `at` was true.
    pub since: u64,
    /// Milliseconds. 0 until mediad has read the file.
    #[serde(default)]
    pub duration: u32,
    /// Percent of normal speed: one of [`RATES`].
    pub rate: u16,
}

impl Playback {
    /// Milliseconds into the file at unix milliseconds `now`.
    pub fn position(&self, now: u64) -> u32 {
        if self.state != PlayState::Playing {
            return self.at;
        }
        let gone = now.saturating_sub(self.since) * u64::from(self.rate) / 100;
        let at = u64::from(self.at) + gone;
        let end = if self.duration > 0 { u64::from(self.duration) } else { u64::MAX };
        at.min(end) as u32
    }

    pub fn is(&self, account: &AccountKey, file: &str) -> bool {
        &self.account == account && self.file == file
    }
}

/// The shell to chatd: `Request::Play`. Each is answered `Ok` at once; what happens is told as
/// `Change::Playback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "play", rename_all = "snake_case")]
pub enum PlayRequest {
    /// Plays a file that is on disk (`path`, inside chatd's media cache), from `at`
    /// milliseconds, or from where it was paused. Whatever else was playing stops.
    Start {
        account: AccountKey,
        file: String,
        path: String,
        conversation: Key,
        message: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<u32>,
        rate: u16,
        /// The speaker picked in the call menu. `None` follows the system default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    Pause,
    /// Moves to `at` milliseconds, playing or paused.
    Seek { at: u32 },
    Rate { rate: u16 },
    /// Stops and forgets it: the now-playing control goes.
    Stop,
}

/// chatd to `mediad --play`, over its stdin. Closing stdin stops playing: mediad answers
/// `Stopped` with where it got to, and exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "player", rename_all = "snake_case")]
pub enum PlayerRequest {
    Open {
        path: String,
        at: u32,
        rate: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    Seek { at: u32 },
    Rate { rate: u16 },
}

/// `mediad --play` to chatd, over its stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "player", rename_all = "snake_case")]
pub enum PlayerEvent {
    /// Sound is leaving the speaker from `at` milliseconds, at `rate`: after opening, a seek and
    /// a change of speed.
    Playing { at: u32, duration: u32, rate: u16 },
    /// The end was played. mediad exits.
    Ended { duration: u32 },
    /// stdin closed. Where it got to; mediad exits.
    Stopped { at: u32 },
    /// The file or the speaker could not be used. mediad exits.
    Failed { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Network;

    fn playback(state: PlayState) -> Playback {
        Playback {
            account: AccountKey { network: Network::Discord, account: "me".into() },
            file: "f".into(),
            conversation: Key { network: Network::Discord, account: "me".into(), id: "c".into() },
            message: "m".into(),
            title: String::new(),
            state,
            at: 1_000,
            since: 10_000,
            duration: 5_000,
            rate: 150,
        }
    }

    #[test]
    fn the_position_runs_with_the_clock_only_while_playing() {
        let playing = playback(PlayState::Playing);
        assert_eq!(playing.position(10_000), 1_000);
        // Two seconds at 1.5x is three seconds of the file.
        assert_eq!(playing.position(12_000), 4_000);
        assert_eq!(playing.position(60_000), 5_000, "never past the end");
        assert_eq!(playback(PlayState::Paused).position(60_000), 1_000);
    }

    #[test]
    fn frames_read_back_as_written() {
        let start = PlayRequest::Start {
            account: AccountKey { network: Network::Telegram, account: "1".into() },
            file: "voice".into(),
            path: "/tmp/v.ogg".into(),
            conversation: Key { network: Network::Telegram, account: "1".into(), id: "2".into() },
            message: "3".into(),
            title: String::new(),
            at: None,
            rate: 100,
            output: None,
        };
        let json = serde_json::to_string(&start).unwrap();
        assert!(json.contains(r#""play":"start""#), "{json}");
        assert_eq!(serde_json::from_str::<PlayRequest>(&json).unwrap(), start);
        let shown = playback(PlayState::Failed { detail: "No speaker.".into() });
        let json = serde_json::to_string(&shown).unwrap();
        assert!(json.contains(r#""state":"failed""#), "{json}");
        assert_eq!(serde_json::from_str::<Playback>(&json).unwrap(), shown);
    }
}
