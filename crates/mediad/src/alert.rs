//! `mediad --sound <word> <percent> [speaker] [--repeat <gap ms>] [--for <seconds>]`: one
//! notification sound, once or ringing, for chatd. A bundled sound is rendered from
//! `cue::sound` at the device rate, the same code the bundled files are rendered from, so
//! nothing is read from disk. A custom sound (`custom:<id>`) is decoded from the user's sounds
//! directory. The process exits when it has played, when chatd kills it (a ring that stopped),
//! or at the limit a repeating sound is given.
//!
//! Windows toasts from an app that is not packaged can play only the system's own sounds, and
//! the freedesktop sound hint names a theme's, so on both chatd posts the notification silent
//! and plays the chosen sound through this. On macOS a bundled sound rides the notification
//! itself, and a custom one — which the system cannot name — comes through here too.
//!
//! `mediad --import-sound <source> <id>` checks a file the user chose, brings it to one level,
//! and writes it beside the database as `<id>.wav`, mono 16-bit. The shell then lists it.

use crate::audio::Speaker;
use crate::cue::{sound, wav, Sound};
use crate::decode::Source;
use crate::{CHANNELS, RATE};
use protocol::settings;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The longest sound a picker may add, in seconds.
pub const LONGEST: f64 = 30.;
/// The peak an imported file is brought to: the same ceiling a message sound has.
const IMPORT_PEAK_DB: f64 = -14.;
/// How much of a repeat is kept playing ahead: enough that a busy moment cannot leave a hole.
const AHEAD: Duration = Duration::from_millis(500);

/// The sound as 48 kHz stereo at `percent` of its level.
pub fn render(which: Sound, percent: u8) -> Vec<f32> {
    let gain = f32::from(percent.min(100)) / 100.;
    sound(which, RATE).into_iter().flat_map(|s| [s * gain; CHANNELS]).collect()
}

/// What a word names: a bundled sound, or a file the user added.
enum Wanted {
    Bundled(Sound),
    Custom(PathBuf),
}

fn resolve(word: &str) -> Result<Wanted, String> {
    if let Some(which) = Sound::from_word(word) {
        return Ok(Wanted::Bundled(which));
    }
    let id = word.strip_prefix("custom:").ok_or("no such sound")?;
    if !settings::sound_id(id) {
        return Err("no such sound".to_owned());
    }
    let path = settings::sound_path(&protocol::support_dir(), id);
    if !path.is_file() {
        return Err("that sound's file is no longer on this computer".to_owned());
    }
    Ok(Wanted::Custom(path))
}

/// The samples of one play, at 48 kHz stereo, already at `percent`.
fn samples(wanted: &Wanted, percent: u8) -> Result<Vec<f32>, String> {
    let gain = f32::from(percent.min(100)) / 100.;
    match wanted {
        Wanted::Bundled(which) => Ok(render(*which, percent)),
        Wanted::Custom(path) => {
            let mut source = Source::open(path)?;
            let mut pcm = Vec::new();
            while pcm.len() < (LONGEST as usize + 2) * RATE as usize * CHANNELS {
                if !source.read(&mut pcm)? {
                    break;
                }
            }
            pcm.truncate((LONGEST as usize) * RATE as usize * CHANNELS);
            if pcm.is_empty() {
                return Err("that sound has nothing in it".to_owned());
            }
            for sample in &mut pcm {
                *sample *= gain;
            }
            Ok(pcm)
        }
    }
}

/// What the command line asked for: `<word> <percent> [speaker]` and the flags, in any order
/// after the word.
struct Options {
    word: String,
    percent: u8,
    output: Option<String>,
    /// Milliseconds of silence between repeats; `None` plays once.
    gap: Option<u64>,
    /// Seconds before a repeating sound stops by itself.
    limit: u64,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut parsed = Options { word: String::new(), percent: 70, output: None, gap: None, limit: 90 };
    let mut position = 0;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--repeat" || arg == "--for" {
            let value = rest.next().ok_or_else(|| format!("{arg} needs a number"))?;
            let number = value.parse::<u64>().map_err(|_| format!("{arg} needs a number"))?;
            if arg == "--repeat" {
                parsed.gap = Some(number.min(10_000));
            } else {
                parsed.limit = number.clamp(1, 300);
            }
            continue;
        }
        if arg.starts_with("--") {
            return Err(format!("no such option: {arg}"));
        }
        match position {
            0 => parsed.word = arg.clone(),
            1 => parsed.percent = arg.parse::<u8>().map_err(|_| "the volume is a number from 0 to 100".to_owned())?.min(100),
            2 => parsed.output = Some(arg.clone()),
            _ => return Err("too many arguments".to_owned()),
        }
        position += 1;
    }
    Ok(parsed)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    let wanted = resolve(&options.word)?;
    let percent = options.percent;
    let output = options.output.as_deref().filter(|name| !name.is_empty());
    let mut speaker = Speaker::open(output).map_err(|err| format!("speaker: {err:?}"))?;
    let pcm = samples(&wanted, percent)?;
    // A repeating sound carries its silence with it, so the same buffer is pushed each time.
    let silence = options.gap.map(|ms| vec![0f32; (ms as usize * RATE as usize / 1000) * CHANNELS]);
    let began = Instant::now();
    let limit = Duration::from_secs(options.limit);
    loop {
        speaker.push(&pcm);
        if let Some(silence) = &silence {
            speaker.push(silence);
        }
        speaker.finish();
        if silence.is_none() {
            break;
        }
        // Wait until what is queued is the tail of this play, then add the next.
        let ahead = AHEAD.as_millis() as usize * RATE as usize / 1000;
        while speaker.queued() > ahead && !speaker.broken() {
            if began.elapsed() >= limit {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if began.elapsed() >= limit || speaker.broken() {
            return Ok(());
        }
    }
    // The device drains it; a second past its length is as long as anything waits.
    let length = Duration::from_secs_f64(pcm.len() as f64 / CHANNELS as f64 / f64::from(RATE));
    while speaker.queued() > 0 && !speaker.broken() && began.elapsed() < length + Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(20));
    }
    // The last buffer the device took is still sounding.
    std::thread::sleep(Duration::from_millis(60));
    Ok(())
}

/// S13: reads `source`, checks it, brings it to one level and writes `<id>.wav` in `dir`.
/// Errors are words the Settings window can show.
pub fn import(source: &str, id: &str, dir: &Path) -> Result<(), String> {
    if !settings::sound_id(id) {
        return Err("that sound has no name".to_owned());
    }
    let path = Path::new(source);
    let size = std::fs::metadata(path).map_err(|_| "That file is no longer there.".to_owned())?.len();
    if size > 40 * 1024 * 1024 {
        return Err("That file is too big for a notification sound. Try one under 40 MB.".to_owned());
    }
    let mut decoded = Source::open(path)?;
    let mut pcm = Vec::new();
    loop {
        if !decoded.read(&mut pcm)? {
            break;
        }
        if pcm.len() > (LONGEST as usize + 1) * RATE as usize * CHANNELS {
            break;
        }
    }
    let frames = pcm.len() / CHANNELS;
    if frames < RATE as usize / 20 {
        return Err("That file is too short to hear. Try one longer than a tenth of a second.".to_owned());
    }
    if frames >= (LONGEST as usize + 1) * RATE as usize {
        return Err(format!("That sound is longer than {LONGEST:.0} seconds. Try a shorter one."));
    }
    // Both ears into one: a notification sound is heard in the middle of the head.
    let mut mono: Vec<f64> = pcm.chunks_exact(CHANNELS).map(|frame| (f64::from(frame[0]) + f64::from(frame[1])) / 2.).collect();
    let peak = mono.iter().fold(0f64, |peak, s| peak.max(s.abs()));
    if peak < 1e-4 {
        return Err("That file is silent.".to_owned());
    }
    let gain = 10f64.powf(IMPORT_PEAK_DB / 20.) / peak;
    mono.iter_mut().for_each(|s| *s *= gain);
    // Ends at silence in both directions, so a finished sound never clicks.
    let fade = (RATE as usize / 200).min(mono.len());
    for (k, sample) in mono[..fade].iter_mut().enumerate() {
        *sample *= (k + 1) as f64 / fade as f64;
    }
    let length = mono.len();
    for (k, sample) in mono[length - fade..].iter_mut().enumerate() {
        *sample *= 1. - (k + 1) as f64 / fade as f64;
    }
    let float: Vec<f32> = mono.into_iter().map(|s| s as f32).collect();
    let sounds = dir.join("sounds");
    std::fs::create_dir_all(&sounds).map_err(|err| format!("That sound could not be saved: {err}"))?;
    std::fs::write(settings::sound_path(dir, id), wav(&float, RATE))
        .map_err(|err| format!("That sound could not be saved: {err}"))
}

/// The sound files on disk that no row names, so a reset can take them with it.
pub fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let sounds = dir.join("sounds");
    let Ok(entries) = std::fs::read_dir(sounds) else { return Vec::new() };
    entries.filter_map(|entry| Some(entry.ok()?.path())).filter(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))).collect()
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

    #[test]
    fn a_custom_word_names_a_file_and_nothing_else_reaches_a_path() {
        assert!(resolve("custom:not-an-id").is_err());
        assert!(resolve("custom:../../secret").is_err());
        assert!(resolve("pebble").is_ok());
        assert!(resolve("nonsense").is_err());
    }

    #[test]
    fn a_repeat_takes_a_gap_and_a_limit_and_nothing_else() {
        let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let parsed = options(&args(&["pebble", "70", "--repeat", "1800", "--for", "45"])).expect("flags");
        assert_eq!((parsed.gap, parsed.limit, parsed.output), (Some(1800), 45, None));
        // A speaker comes after the volume and before the flags.
        let parsed = options(&args(&["pebble", "70", "AirPods Pro", "--repeat", "1800"])).expect("speaker");
        assert_eq!((parsed.gap, parsed.output.as_deref()), (Some(1800), Some("AirPods Pro")));
        assert_eq!(options(&args(&["pebble", "70"])).expect("none").gap, None);
        assert!(options(&args(&["pebble", "70", "--repeat"])).is_err());
        assert!(options(&args(&["pebble", "70", "--nope", "1"])).is_err());
        // A pathological gap cannot hold the process forever.
        assert_eq!(options(&args(&["pebble", "70", "--repeat", "999999"])).expect("clamped").gap, Some(10_000));
    }

    #[test]
    fn an_imported_file_is_one_level_and_ends_in_silence() {
        // A tone at 0.9 peak and a second of silence is measured, not read from the tag.
        let dir = std::env::temp_dir().join(format!("tinyclient-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("loud.wav");
        let samples: Vec<f32> = (0..RATE as usize / 2).map(|n| 0.9 * (std::f64::consts::TAU * 440. * n as f64 / f64::from(RATE)).sin() as f32).collect();
        std::fs::write(&source, wav(&samples, RATE)).unwrap();
        let id = "0123456789abcdef";
        import(source.to_str().unwrap(), id, &dir).expect("imported");
        let written = settings::sound_path(&dir, id);
        assert!(written.is_file());
        let bytes = std::fs::read(&written).unwrap();
        let data = bytes.windows(4).position(|w| w == b"data").expect("a data chunk") + 8;
        let pcm: Vec<f32> = bytes[data..].chunks_exact(2).map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.).collect();
        let peak = pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
        let target = 10f64.powf(IMPORT_PEAK_DB / 20.) as f32;
        assert!((peak - target).abs() < 0.01, "peaks at {peak}, wanted {target}");
        assert!(pcm.first().is_some_and(|s| s.abs() < 0.01) && pcm.last().is_some_and(|s| s.abs() < 0.01), "ends in a fade");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_too_short_or_silent_is_refused_in_words() {
        let dir = std::env::temp_dir().join(format!("tinyclient-import-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let silent = dir.join("silent.wav");
        std::fs::write(&silent, wav(&vec![0f32; RATE as usize / 4], RATE)).unwrap();
        let refused = import(silent.to_str().unwrap(), "0123456789abcdef", &dir).unwrap_err();
        assert!(refused.contains("silent"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

