//! `mediad --play` as chatd runs it: frames in on stdin, events out on stdout, the speaker
//! replaced by a file (`TINYCLIENT_MEDIAD_SINK`) that drains at a device's pace.

use protocol::{read_frame, write_frame, PlayerEvent, PlayerRequest};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name).display().to_string()
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
    assert!((980..=1010).contains(&duration), "{duration} ms");
    let PlayerEvent::Ended { duration: ended } = player.next() else { panic!("did not end") };
    assert!(ended >= duration);
    // It took as long as the message is: it was played, not just read.
    let took = started.elapsed();
    assert!(took > Duration::from_millis(900) && took < Duration::from_secs(3), "{took:?}");
    let played = player.played();
    assert!((0.98..1.02).contains(&played), "{played} s played");
    player.exits_cleanly();
}

#[test]
fn closing_stdin_pauses_and_says_where() {
    let mut player = Player::open("pause", &fixture("tone.opus"), 0, 100);
    assert!(matches!(player.next(), PlayerEvent::Playing { .. }));
    std::thread::sleep(Duration::from_millis(400));
    drop(player.child.stdin.take());
    let PlayerEvent::Stopped { at } = player.next() else { panic!("did not stop") };
    assert!((300..=500).contains(&at), "stopped at {at} ms");
    player.exits_cleanly();
}

#[test]
fn twice_as_fast_takes_half_as_long_and_a_seek_jumps() {
    let started = Instant::now();
    let mut player = Player::open("fast", &fixture("tone.opus"), 0, 200);
    assert!(matches!(player.next(), PlayerEvent::Playing { rate: 200, .. }));
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
    let started = Instant::now();
    let mut player = Player::open("resume", &fixture("tone.m4a"), 700, 100);
    assert!(matches!(player.next(), PlayerEvent::Playing { at: 700, .. }));
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
