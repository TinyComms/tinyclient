//! `TINYCLIENT_CALL_FIXTURE` stands in for the microphone, camera, and screen.

use mediad::audio::{self, Audio};
use mediad::picture::{self, Pictures};
use mediad::FRAME_SAMPLES;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call")
}

struct Env {
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn hold_env() -> Env {
    Env { _lock: ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }
}

impl Drop for Env {
    fn drop(&mut self) {
        std::env::remove_var("TINYCLIENT_CALL_FIXTURE");
        std::env::remove_var("TINYCLIENT_MEDIAD_AUDIO");
        std::env::remove_var("TINYCLIENT_MEDIAD_SINK");
    }
}

fn read_pcm(path: &Path) -> Vec<i16> {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    assert_eq!(bytes.len() % 2, 0, "speech.pcm is not 16-bit");
    bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

/// 16-bit PCM WAV to 48 kHz stereo little-endian, one linear step. ffmpeg is not on this machine.
fn resample_wav_to_pcm(wav: &[u8]) -> Vec<u8> {
    let (rate, channels, samples) = pcm_wav(wav);
    let frames = samples.len() / channels as usize;
    let out_frames = (frames as u64 * 48_000) / u64::from(rate);
    let mut pcm = Vec::with_capacity(out_frames as usize * 4);
    for n in 0..out_frames {
        let pos = n as f64 * f64::from(rate) / 48_000.0;
        let index = pos.floor() as usize;
        let frac = pos - index as f64;
        let mut mixed = 0.0;
        for channel in 0..channels as usize {
            let at = |frame: usize| {
                let frame = frame.min(frames.saturating_sub(1));
                samples[frame * channels as usize + channel] as f64
            };
            mixed += at(index) + (at(index + 1) - at(index)) * frac;
        }
        let sample = (mixed / f64::from(channels)).round().clamp(-32768.0, 32767.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    pcm
}

fn pcm_wav(wav: &[u8]) -> (u32, u16, Vec<i16>) {
    assert!(wav.starts_with(b"RIFF") && wav.get(8..12) == Some(b"WAVE".as_slice()), "not a wav");
    let mut at = 12usize;
    let mut rate = 0u32;
    let mut channels = 0u16;
    let mut bits = 0u16;
    let mut data: &[u8] = &[];
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let size = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
        let end = at + 8 + size;
        let body = &wav[at + 8..end];
        if id == b"fmt " {
            let format = u16::from_le_bytes(body[0..2].try_into().unwrap());
            assert_eq!(format, 1, "wav is not pcm");
            channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
        } else if id == b"data" {
            data = body;
        }
        at = end + (size % 2);
    }
    assert_eq!(bits, 16, "wav is not 16-bit");
    assert!(channels > 0 && rate > 0 && !data.is_empty(), "wav has no pcm");
    let samples = data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    (rate, channels, samples)
}

fn annex_frames(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut at = 0;
    while let Some(found) = stream.get(at..).and_then(|rest| rest.windows(3).position(|window| window == [0, 0, 1])) {
        starts.push(at + found + 3);
        at += found + 3;
    }
    let units: Vec<&[u8]> = starts
        .iter()
        .enumerate()
        .filter_map(|(n, &start)| {
            let end = match starts.get(n + 1) {
                Some(&next) => {
                    let end = next - 3;
                    if end > start && stream.get(end - 1) == Some(&0) { end - 1 } else { end }
                }
                None => stream.len(),
            };
            stream.get(start..end).filter(|unit| !unit.is_empty())
        })
        .collect();
    let mut frames = Vec::new();
    let mut frame = Vec::new();
    for unit in units {
        frame.extend_from_slice(&[0, 0, 0, 1]);
        frame.extend_from_slice(unit);
        if matches!(unit.first().map(|header| header & 0x1f), Some(1 | 5)) {
            frames.push(std::mem::take(&mut frame));
        }
    }
    frames
}

fn tone_frame() -> [i16; FRAME_SAMPLES] {
    let mut phase = 0.0f64;
    let step = 440.0 * std::f64::consts::TAU / 48_000.0;
    let mut frame = [0i16; FRAME_SAMPLES];
    for pair in frame.as_chunks_mut::<2>().0 {
        let sample = (phase.sin() * 8000.0) as i16;
        pair[0] = sample;
        pair[1] = sample;
        phase = (phase + step) % std::f64::consts::TAU;
    }
    frame
}

#[test]
fn speech_pcm_is_the_spoken_wav_at_48k_stereo() {
    let wav = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/speech.wav")).expect("speech.wav");
    let decoded = resample_wav_to_pcm(&wav);
    let committed = std::fs::read(fixtures().join("speech.pcm")).expect("speech.pcm");
    assert_eq!(committed, decoded);
    let samples = read_pcm(&fixtures().join("speech.pcm"));
    let mean = samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64;
    assert!(mean.sqrt() > 500.0, "the sentence is silent: rms {:.1}", mean.sqrt());
    assert!(samples.len() > FRAME_SAMPLES * 2, "shorter than two frames");
    assert_eq!(samples.len() % 2, 0, "not stereo");
}

#[test]
fn a_fixture_directory_lists_the_fixture_microphone_as_the_default() {
    let _env = hold_env();
    std::env::set_var("TINYCLIENT_CALL_FIXTURE", fixtures());
    let listing = audio::listing();
    let fixture = listing.inputs.iter().find(|device| device.id == "fixture").expect("no fixture input");
    assert_eq!(fixture.name, "fixture");
    assert!(fixture.default, "the fixture input is not the default");
    assert_eq!(listing.default_input, "fixture");
}

#[test]
fn capture_reads_speech_one_frame_at_a_time_and_loops() {
    let _env = hold_env();
    std::env::set_var("TINYCLIENT_CALL_FIXTURE", fixtures());
    let speech = read_pcm(&fixtures().join("speech.pcm"));
    let mut audio = Audio::open(None, None).expect("fixture microphone opens without a device");
    assert_eq!((audio.input.id.as_str(), audio.input.name.as_str()), ("fixture", "fixture"));
    let mut at = 0usize;
    let frames = speech.len() / FRAME_SAMPLES + 2;
    for _ in 0..frames {
        let mut frame = [0i16; FRAME_SAMPLES];
        assert!(audio.capture(&mut frame), "a fixture frame is ready every tick");
        for sample in frame {
            assert_eq!(sample, speech[at % speech.len()], "sample {at}");
            at += 1;
        }
    }
    assert!(at > speech.len(), "the clip did not loop");
}

#[test]
fn played_fixture_audio_is_written_to_the_sink() {
    let _env = hold_env();
    let sink = std::env::temp_dir().join("tinyclient-call-fixture-sink.pcm");
    let _ = std::fs::remove_file(&sink);
    std::env::set_var("TINYCLIENT_CALL_FIXTURE", fixtures());
    std::env::set_var("TINYCLIENT_MEDIAD_SINK", &sink);
    let mut audio = Audio::open(None, None).expect("fixture opens");
    let frame = [7i16; FRAME_SAMPLES];
    audio.play(&frame);
    drop(audio);
    let written = std::fs::read(&sink).expect("sink file");
    let samples: Vec<i16> = written.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    assert_eq!(samples, frame);
}

#[test]
fn the_tone_stand_in_wins_when_both_variables_are_set() {
    let _env = hold_env();
    std::env::set_var("TINYCLIENT_CALL_FIXTURE", fixtures());
    std::env::set_var("TINYCLIENT_MEDIAD_AUDIO", "tone");
    let listing = audio::listing();
    assert_eq!(listing.default_input, "tone");
    assert!(listing.inputs.iter().all(|device| device.id != "fixture"));
    let mut audio = Audio::open(None, None).expect("tone opens");
    let mut frame = [0i16; FRAME_SAMPLES];
    assert!(audio.capture(&mut frame));
    assert_eq!(frame, tone_frame());
}

#[test]
fn without_the_variable_the_listing_has_no_fixture_microphone() {
    let _env = hold_env();
    let listing = audio::listing();
    assert!(listing.inputs.iter().all(|device| device.id != "fixture" && device.name != "fixture"));
    assert_ne!(listing.default_input, "fixture");
}

#[test]
fn probe_camera_and_probe_screen_succeed_for_a_fixture() {
    let _env = hold_env();
    std::env::set_var("TINYCLIENT_CALL_FIXTURE", fixtures());
    picture::probe_camera().expect("probe camera");
    picture::probe_screen().expect("probe screen");
}

#[test]
fn camera_and_share_return_the_next_clip_frame_and_then_loop() {
    let _env = hold_env();
    std::env::set_var("TINYCLIENT_CALL_FIXTURE", fixtures());
    let high = annex_frames(&std::fs::read(fixtures().join("camera-h.h264")).unwrap());
    let low = annex_frames(&std::fs::read(fixtures().join("camera-l.h264")).unwrap());
    let share = annex_frames(&std::fs::read(fixtures().join("share.h264")).unwrap());
    assert!(!high.is_empty() && !low.is_empty() && !share.is_empty());
    let mut pictures = Pictures::default();
    let rounds = high.len().max(low.len()).max(share.len()) + 1;
    for n in 0..rounds {
        let (got_high, got_low) = pictures.camera(n == 0).expect("camera clip");
        assert_eq!(got_high, high[n % high.len()], "high frame {n}");
        assert_eq!(got_low, low[n % low.len()], "low frame {n}");
        let got_share = pictures.share(n == 0).expect("share clip");
        assert_eq!(got_share, share[n % share.len()], "share frame {n}");
    }
}
