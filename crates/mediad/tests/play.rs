//! `mediad --play` as chatd runs it: frames in on stdin, events out on stdout, the speaker
//! replaced by a file (`TINYCLIENT_MEDIAD_SINK`) that drains at a device's pace.

use protocol::{read_frame, write_frame, PlayerEvent, PlayerRequest};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name).display().to_string()
}

/// A silent WAV of `seconds`. The fixtures are about a second long, which a busy machine can
/// play out before the thread reading stdin is scheduled, so a pause test needs a longer one.
fn silence(path: &Path, seconds: u32) {
    let rate = 48_000u32;
    let channels = 2u16;
    let data = rate * seconds * u32::from(channels) * 2;
    let mut file = std::fs::File::create(path).unwrap();
    let mut head = Vec::with_capacity(44);
    head.extend_from_slice(b"RIFF");
    head.extend_from_slice(&(36 + data).to_le_bytes());
    head.extend_from_slice(b"WAVEfmt ");
    head.extend_from_slice(&16u32.to_le_bytes());
    head.extend_from_slice(&1u16.to_le_bytes());
    head.extend_from_slice(&channels.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    head.extend_from_slice(&(channels * 2).to_le_bytes());
    head.extend_from_slice(&16u16.to_le_bytes());
    head.extend_from_slice(b"data");
    head.extend_from_slice(&data.to_le_bytes());
    file.write_all(&head).unwrap();
    let piece = vec![0u8; 48_000];
    let mut left = data;
    while left > 0 {
        let n = (left as usize).min(piece.len());
        file.write_all(&piece[..n]).unwrap();
        left -= n as u32;
    }
}

struct Player {
    child: Child,
    out: BufReader<ChildStdout>,
    sink: PathBuf,
}

impl Player {
    fn open(name: &str, path: &str, at: u32, rate: u16) -> Player {
        let sink = std::env::temp_dir().join(format!("tinyclient-play-{}-{name}.pcm", std::process::id()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_mediad"))
            .arg("--play")
            .env("TINYCLIENT_MEDIAD_AUDIO", "null")
            .env("TINYCLIENT_MEDIAD_SINK", &sink)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut player = Player { child, out, sink };
        player.ask(&PlayerRequest::Open { path: path.to_owned(), at, rate, output: None });
        player
    }

    fn ask(&mut self, request: &PlayerRequest) {
        write_frame(self.child.stdin.as_mut().unwrap(), request).unwrap();
    }

    fn next(&mut self) -> PlayerEvent {
        read_frame(&mut self.out).unwrap()
    }

    /// Seconds of sound that reached the speaker.
    fn played(&self) -> f32 {
        std::fs::metadata(&self.sink).map_or(0., |meta| meta.len() as f32 / 4. / 48_000.)
    }

    fn exits_cleanly(mut self) {
        let status = self.child.wait().unwrap();
        assert!(status.success(), "{status}");
        let _ = std::fs::remove_file(&self.sink);
    }
}

#[test]
fn a_voice_message_plays_to_its_end_and_the_process_goes() {
    let started = Instant::now();
    let mut player = Player::open("end", &fixture("tone.opus"), 0, 100);
    let PlayerEvent::Playing { at: 0, duration, rate: 100 } = player.next() else { panic!("not playing") };
    let playing = Instant::now();
    assert!((980..=1010).contains(&duration), "{duration} ms");
    let PlayerEvent::Ended { duration: ended } = player.next() else { panic!("did not end") };
    assert!(ended >= duration);
    // It took as long as the message is: it was played, not just read. The bound above is from
    // when it played: starting the process is slow on a busy machine.
    let (took, from_playing) = (started.elapsed(), playing.elapsed());
    assert!(took > Duration::from_millis(900) && from_playing < Duration::from_secs(3), "{took:?}, {from_playing:?} from playing");
    let played = player.played();
    assert!((0.98..1.02).contains(&played), "{played} s played");
    player.exits_cleanly();
}

#[test]
fn closing_stdin_pauses_and_says_where() {
    let wav = std::env::temp_dir().join(format!("tinyclient-play-{}-pause.wav", std::process::id()));
    silence(&wav, 30);
    let mut player = Player::open("pause", &wav.display().to_string(), 0, 100);
    let PlayerEvent::Playing { duration, .. } = player.next() else { panic!("not playing") };
    assert!(duration >= 30_000, "{duration} ms");
    let playing = Instant::now();
    std::thread::sleep(Duration::from_millis(400));
    // Measured, not assumed: a busy machine stretches the sleep, and the sound plays on meanwhile.
    let played = playing.elapsed().as_millis() as u32;
    drop(player.child.stdin.take());
    let event = player.next();
    let PlayerEvent::Stopped { at } = event else { panic!("did not stop: {event:?}") };
    assert!((played.saturating_sub(100)..=played + 100).contains(&at), "stopped at {at} ms after {played} ms of playing");
    player.exits_cleanly();
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn twice_as_fast_takes_half_as_long_and_a_seek_jumps() {
    // Timed from when it plays: starting a process is no part of it, and is slow on a busy machine.
    let mut player = Player::open("fast", &fixture("tone.opus"), 0, 200);
    assert!(matches!(player.next(), PlayerEvent::Playing { rate: 200, .. }));
    let started = Instant::now();
    assert!(matches!(player.next(), PlayerEvent::Ended { .. }));
    let took = started.elapsed();
    assert!(took < Duration::from_millis(900), "{took:?} at 2x");
    player.exits_cleanly();

    let mut player = Player::open("seek", &fixture("tone.opus"), 0, 100);
    assert!(matches!(player.next(), PlayerEvent::Playing { .. }));
    player.ask(&PlayerRequest::Seek { at: 800 });
    assert!(matches!(player.next(), PlayerEvent::Playing { at: 800, .. }));
    assert!(matches!(player.next(), PlayerEvent::Ended { .. }));
    player.exits_cleanly();

    // Opened part of the way in, as a paused message resumes.
    let mut player = Player::open("resume", &fixture("tone.m4a"), 700, 100);
    assert!(matches!(player.next(), PlayerEvent::Playing { at: 700, .. }));
    let started = Instant::now();
    assert!(matches!(player.next(), PlayerEvent::Ended { .. }));
    assert!(started.elapsed() < Duration::from_millis(800));
    player.exits_cleanly();
}

#[test]
fn a_file_that_is_gone_or_not_audio_fails_in_words() {
    for (name, path, words) in [
        ("gone", "/nonexistent/voice.ogg".to_owned(), "The file is no longer on this computer."),
        ("text", fixture("../../Cargo.toml"), "This file can't be played."),
    ] {
        let mut player = Player::open(name, &path, 0, 100);
        let PlayerEvent::Failed { detail } = player.next() else { panic!("{name} did not fail") };
        assert_eq!(detail, words);
        assert_eq!(player.child.wait().unwrap().code(), Some(2));
        let _ = std::fs::remove_file(&player.sink);
    }
}
