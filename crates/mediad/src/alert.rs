//! `mediad --sound <word> <percent> [speaker]`: one notification sound, once, for chatd. The
//! sound is rendered from `cue::sound` at the device rate, the same code the bundled files are
//! rendered from, so nothing is read from disk. The process exits when it has played, or when
//! chatd kills it (a ring that stopped).
//!
//! Windows toasts from an app that is not packaged can play only the system's own sounds, and
//! the freedesktop sound hint names a theme's, so on both chatd posts the notification silent
//! and plays the chosen sound through this.

use crate::audio::Speaker;
use crate::cue::{sound, Sound};
use crate::{CHANNELS, RATE};
use std::time::{Duration, Instant};

/// The sound as 48 kHz stereo at `percent` of its level.
pub fn render(which: Sound, percent: u8) -> Vec<f32> {
    let gain = f32::from(percent.min(100)) / 100.;
    sound(which, RATE).into_iter().flat_map(|s| [s * gain; CHANNELS]).collect()
}

pub fn run(args: &[String]) -> Result<(), String> {
    let which = args.first().and_then(|word| Sound::from_word(word)).ok_or("no such sound")?;
    let percent = args.get(1).and_then(|n| n.parse::<u8>().ok()).unwrap_or(70);
    let output = args.get(2).filter(|name| !name.is_empty());
    let mut speaker = Speaker::open(output.map(String::as_str)).map_err(|err| format!("speaker: {err:?}"))?;
    let pcm = render(which, percent);
    let length = Duration::from_secs_f64(pcm.len() as f64 / CHANNELS as f64 / f64::from(RATE));
    speaker.push(&pcm);
    speaker.finish();
    // The device drains it; a second past its length is as long as anything waits.
    let began = Instant::now();
    while speaker.queued() > 0 && !speaker.broken() && began.elapsed() < length + Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(20));
    }
    // The last buffer the device took is still sounding.
    std::thread::sleep(Duration::from_millis(60));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_is_rendered_in_stereo_at_the_volume_asked() {
        let full = render(Sound::Pebble, 100);
        let half = render(Sound::Pebble, 50);
        assert_eq!(full.len(), sound(Sound::Pebble, RATE).len() * CHANNELS);
        let peak = |pcm: &[f32]| pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak(&half) * 2. - peak(&full)).abs() < 1e-4);
        assert!(render(Sound::Tap, 0).iter().all(|s| *s == 0.));
        assert_eq!(Sound::from_word("classic_ring"), Some(Sound::ClassicRing));
        assert_eq!(Sound::from_word("none"), None);
    }

    #[test]
    fn every_sound_a_picker_offers_is_one_mediad_plays() {
        use protocol::settings::{MENTION_SOUND_CHOICES, RING_CHOICES, SOUND_CHOICES};
        let offered = SOUND_CHOICES.iter().chain(&MENTION_SOUND_CHOICES).chain(&RING_CHOICES);
        for word in offered.filter(|word| !matches!(**word, "none" | "same")) {
            assert!(Sound::from_word(word).is_some(), "{word}");
        }
        assert!(RING_CHOICES.iter().filter_map(|word| Sound::from_word(word)).all(Sound::ring));
    }
}
