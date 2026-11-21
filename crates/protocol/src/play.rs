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

/// One decoded frame of a round video message: a JPEG mediad wrote, the way a call picture is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayPicture {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
}

/// Where a round video message is, for the click and the muted autoplay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundPhase {
    /// Not the file playing.
    Idle,
    /// On screen, looping, with no sound.
    Muted,
    /// Playing with sound.
    Sound,
    Paused,
    Ended,
    /// mediad could not decode it. The system player is the fallback.
    Failed,
}

/// What a click on a round video message does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundClick {
    Pause,
    /// Play with sound. `Some(0)` starts over (a muted loop, or the end); `None` resumes.
    Sound { at: Option<u32> },
    /// Hand the file to the system's player.
    System,
}

/// A click pauses sound, resumes a pause, and starts a muted loop or the end over with sound.
/// A file mediad could not decode goes to the system player.
pub fn round_click(phase: RoundPhase) -> RoundClick {
    match phase {
        RoundPhase::Failed => RoundClick::System,
        RoundPhase::Sound => RoundClick::Pause,
        RoundPhase::Paused => RoundClick::Sound { at: None },
        RoundPhase::Muted | RoundPhase::Idle | RoundPhase::Ended => RoundClick::Sound { at: Some(0) },
    }
}

/// Muted autoplay follows "Play GIFs and animated stickers": always, only under the pointer,
/// or never. Reduce Motion and a window that is not key hold it. A file the user paused, or
/// that is already playing, is left as they left it.
pub fn round_should_autoplay(always: bool, hover_setting: bool, hovered: bool, reduce_motion: bool, window_active: bool, phase: RoundPhase) -> bool {
    if reduce_motion || !window_active {
        return false;
    }
    let allowed = always || (hover_setting && hovered);
    allowed && matches!(phase, RoundPhase::Idle | RoundPhase::Ended)
}

/// AVCDecoderConfigurationRecord (`avcC`): the NAL length size, and the SPS and PPS as Annex B.
pub struct AvcConfig {
    pub nal_length: usize,
    pub parameter_sets: Vec<u8>,
}

/// Reads an `avcC` payload. `None` when it is not one this player can turn into Annex B.
pub fn avc_config(record: &[u8]) -> Option<AvcConfig> {
    if record.len() < 7 || record[0] != 1 {
        return None;
    }
    let nal_length = (record[4] & 0x03) as usize + 1;
    if !matches!(nal_length, 1 | 2 | 4) {
        return None;
    }
    let mut sets = Vec::new();
    let mut at = 5;
    let sps_count = (*record.get(at)? & 0x1f) as usize;
    at += 1;
    for _ in 0..sps_count {
        push_nal(record, &mut at, &mut sets)?;
    }
    let pps_count = *record.get(at)? as usize;
    at += 1;
    for _ in 0..pps_count {
        push_nal(record, &mut at, &mut sets)?;
    }
    (!sets.is_empty()).then_some(AvcConfig { nal_length, parameter_sets: sets })
}

fn push_nal(record: &[u8], at: &mut usize, sets: &mut Vec<u8>) -> Option<()> {
    let len = u16::from_be_bytes([*record.get(*at)?, *record.get(*at + 1)?]) as usize;
    *at += 2;
    let nal = record.get(*at..*at + len)?;
    if nal.is_empty() {
        return None;
    }
    sets.extend_from_slice(&[0, 0, 0, 1]);
    sets.extend_from_slice(nal);
    *at += len;
    Some(())
}

/// Length-prefixed MP4 NALs (AVCC) as Annex B. `with_sets` puts the SPS and PPS in front,
/// which a seek and a keyframe need. A buffer that is already Annex B is kept.
pub fn annex_b_from_avcc(packet: &[u8], nal_length: usize, parameter_sets: &[u8], with_sets: bool) -> Option<Vec<u8>> {
    if packet.starts_with(&[0, 0, 0, 1]) || packet.starts_with(&[0, 0, 1]) {
        let mut out = Vec::new();
        if with_sets {
            out.extend_from_slice(parameter_sets);
        }
        out.extend_from_slice(packet);
        return Some(out);
    }
    if !matches!(nal_length, 1 | 2 | 4) || packet.is_empty() {
        return None;
    }
    let mut nals = Vec::new();
    let mut at = 0;
    let mut idr = false;
    while at + nal_length <= packet.len() {
        let mut len = 0usize;
        for byte in &packet[at..at + nal_length] {
            len = (len << 8) | *byte as usize;
        }
        at += nal_length;
        if len == 0 || at + len > packet.len() {
            return None;
        }
        let nal = &packet[at..at + len];
        if nal.first().is_some_and(|byte| byte & 0x1f == 5) {
            idr = true;
        }
        nals.extend_from_slice(&[0, 0, 0, 1]);
        nals.extend_from_slice(nal);
        at += len;
    }
    if nals.is_empty() || packet[at..].iter().any(|byte| *byte != 0) {
        return None;
    }
    let mut out = Vec::new();
    if with_sets || idr {
        out.extend_from_slice(parameter_sets);
    }
    out.extend_from_slice(&nals);
    Some(out)
}

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
    /// Sound is held back: a round video looping on screen before a click.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub muted: bool,
    /// The newest frame of a round video message. Absent for a voice message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<PlayPicture>,
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
        /// A round video message: demux the picture as well as the sound. Anything else is audio.
        #[serde(default, skip_serializing_if = "crate::is_false")]
        round: bool,
        /// Play the picture and hold the sound. A click starts it again with sound.
        #[serde(default, skip_serializing_if = "crate::is_false")]
        muted: bool,
        /// At the end, start again from the beginning. Round notes loop.
        #[serde(default, skip_serializing_if = "crate::is_false")]
        looped: bool,
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
        /// Decode the H.264 picture of a round video message and send it as [`PlayerEvent::Frame`].
        #[serde(default, skip_serializing_if = "crate::is_false")]
        round: bool,
        /// The picture plays and the speaker is fed silence.
        #[serde(default, skip_serializing_if = "crate::is_false")]
        muted: bool,
        /// Seek back to the start when the file ends, instead of [`PlayerEvent::Ended`].
        #[serde(default, skip_serializing_if = "crate::is_false")]
        looped: bool,
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
    /// One frame of a round video message, as a JPEG. The shell draws `generation` inside the circle.
    Frame { path: String, width: u32, height: u32, generation: u64 },
}

/// What the lab's round-video driver checks, with no file and no decoder: a length-prefixed
/// picture becomes Annex B with its parameter sets, a click pauses sound and falls back when
/// the picture cannot be decoded, and a frame tells the shell its JPEG.
pub fn prove_round_video() -> Result<(), String> {
    let sps = [0x67, 0x42, 0x00, 0x0a];
    let pps = [0x68, 0xce, 0x38, 0x80];
    let mut record = vec![1, 0x42, 0x00, 0x0a, 0xff];
    record.push(0xe1);
    record.extend_from_slice(&4u16.to_be_bytes());
    record.extend_from_slice(&sps);
    record.push(1);
    record.extend_from_slice(&4u16.to_be_bytes());
    record.extend_from_slice(&pps);
    let config = avc_config(&record).ok_or("avcC did not read")?;
    if config.nal_length != 4 {
        return Err(format!("nal length {}", config.nal_length));
    }
    let mut packet = Vec::new();
    packet.extend_from_slice(&4u32.to_be_bytes());
    packet.extend_from_slice(&[0x65, 0x88, 0x84, 0x00]);
    let annex = annex_b_from_avcc(&packet, config.nal_length, &config.parameter_sets, false).ok_or("AVCC did not become Annex B")?;
    if !annex.windows(4).any(|code| code == [0, 0, 0, 1]) {
        return Err("Annex B has no start code".to_owned());
    }
    if &annex[..4] != [0, 0, 0, 1] || annex.get(4) != Some(&0x67) {
        return Err("a keyframe was not given its parameter sets".to_owned());
    }
    if round_click(RoundPhase::Sound) != RoundClick::Pause {
        return Err("a click did not pause".to_owned());
    }
    if round_click(RoundPhase::Failed) != RoundClick::System {
        return Err("a file that cannot be decoded did not fall back".to_owned());
    }
    if round_click(RoundPhase::Muted) != (RoundClick::Sound { at: Some(0) }) {
        return Err("a muted loop did not start over with sound".to_owned());
    }
    if round_click(RoundPhase::Paused) != (RoundClick::Sound { at: None }) {
        return Err("a pause did not resume".to_owned());
    }
    if !round_should_autoplay(true, false, false, false, true, RoundPhase::Idle) {
        return Err("autoplay did not start a round note".to_owned());
    }
    if round_should_autoplay(true, false, false, true, true, RoundPhase::Idle) {
        return Err("Reduce Motion did not hold autoplay".to_owned());
    }
    if round_should_autoplay(false, true, false, false, true, RoundPhase::Idle) {
        return Err("hover autoplay started with the pointer away".to_owned());
    }
    let event = PlayerEvent::Frame { path: "round.jpg".into(), width: 384, height: 384, generation: 2 };
    let json = serde_json::to_string(&event).map_err(|err| err.to_string())?;
    if !json.contains(r#""player":"frame""#) {
        return Err(format!("frame was not a player event: {json}"));
    }
    if serde_json::from_str::<PlayerEvent>(&json).map_err(|err| err.to_string())? != event {
        return Err("a frame did not read back".to_owned());
    }
    Ok(())
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
            muted: false,
            picture: None,
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
            round: false,
            muted: false,
            looped: false,
        };
        let json = serde_json::to_string(&start).unwrap();
        assert!(json.contains(r#""play":"start""#), "{json}");
        assert_eq!(serde_json::from_str::<PlayRequest>(&json).unwrap(), start);
        let shown = playback(PlayState::Failed { detail: "No speaker.".into() });
        let json = serde_json::to_string(&shown).unwrap();
        assert!(json.contains(r#""state":"failed""#), "{json}");
        assert_eq!(serde_json::from_str::<Playback>(&json).unwrap(), shown);
    }

    #[test]
    fn a_round_note_clicks_pauses_and_falls_back() {
        prove_round_video().unwrap();
    }
}
