//! `mediad --rec <path>`: one voice note recorded off the microphone, into one Ogg Opus file
//! as Telegram and Discord take them (mono, the source's 48 kHz, Opus), until chatd closes our
//! stdin (stop), the ceiling passes, or the device fails. Then the facts chatd needs to label
//! the file — how long it was, how loud it was where — go over in the one event it reads, and
//! the process leaves nothing behind but the file.
//!
//! The loudness bars come from the same 20 ms frames the encoder encodes: nothing is opened or
//! decoded twice.

use crate::audio::{self, Audio};
use crate::ogg::Writer;
use crate::opus::Encoder;
use crate::{media, out, FRAME_SAMPLES};
use protocol::{read_frame, RecEvent};
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// However the recorder is stopped, longer than this is not recorded: a voice note of more is
/// typed instead.
const LIMIT: Duration = Duration::from_secs(600);
/// The bars of loudness the facts carry, one byte each, over the whole recording.
const BARS: usize = 64;
/// The tick, the same 20 ms the calls run at.
const TICK: Duration = Duration::from_millis(20);

/// Nothing is asked of us yet; our stdin closing is the stop, as it is for a call.
fn stopper() -> Receiver<()> {
    let (tell, told) = channel();
    let _ = out::spawn(std::thread::Builder::new().name("stopper".to_owned()), move || {
        let mut stdin = out::stdin();
        loop {
            match read_frame::<protocol::PlayerRequest>(&mut stdin) {
                Ok(_) => continue,
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => break,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    break;
                }
            }
        }
        let _ = tell.send(());
    });
    told
}

fn fail(detail: impl Into<String>) -> ! {
    out::write(&RecEvent::Failed { detail: detail.into() });
    out::exit(2)
}

/// The recording's loudness over time, one bar a frame, drawn together into [`BARS`] bars
/// afterwards so a short note and a long one draw alike. The scale is the contract's own: 0 is
/// quiet, 255 is a wall of sound.
struct Loudness {
    loudest: u8,
    bars: Vec<u8>,
}

impl Loudness {
    fn new() -> Loudness {
        Loudness { loudest: 0, bars: Vec::new() }
    }

    fn frame(&mut self, level: u8) {
        self.loudest = self.loudest.max(level);
    }

    /// Closes one frame's bar. A silent frame is kept as a bar of its own, as a pause is.
    fn bar(&mut self) {
        self.bars.push(self.loudest);
        self.loudest = 0;
    }

    /// The loudest of each slice of the recording, `bars.len()` bars over [`BARS`], one byte a
    /// bar, 0 to 255. Empty before any frame came.
    fn over(self) -> Vec<u8> {
        let mut drawn = vec![0u8; self.bars.len().min(BARS)];
        for (at, &bar) in self.bars.iter().enumerate() {
            let spot = at * drawn.len() / self.bars.len().max(1);
            drawn[spot] = drawn[spot].max((u16::from(bar) * 255 / 100) as u8);
        }
        // The bars are drawn for the composer and sent on whole by chatd.
        drawn
    }
}

pub fn run(path: PathBuf) -> io::Result<()> {
    run_with(path, stopper())
}

/// The recording itself, as `run` and a test drive it: `told` is the stop.
fn run_with(path: PathBuf, told: Receiver<()>) -> io::Result<()> {
    // The microphone of a call: the same capture stage, and no speaker, so what we hear never
    // comes back out through our own.
    let mut audio = Audio::open_sides(None, None, (true, false)).unwrap_or_else(|err| {
        let (reason, detail) = media::failure(err);
        out::fail(reason, detail)
    });
    let mut encoder = match Encoder::voice() {
        Some(encoder) => encoder,
        None => {
            out::write(&RecEvent::Failed { detail: "The microphone is not readable.".into() });
            out::exit(2)
        }
    };
    let out_file = match std::fs::File::create(&path) {
        Ok(out_file) => out_file,
        Err(err) => {
            tracing::warn!("mediad: {}: {err}", path.display());
            fail("The recording could not be written.")
        }
    };
    let mut file = match Writer::create(out_file, 4_242, 312) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!("mediad: {err}");
            fail("The recording could not be written.")
        }
    };
    let mut loudness = Loudness::new();
    let mut packet = [0u8; 1600];
    let mut frames = 0u64;
    let since = Instant::now();
    loop {
        // A stop is waited for in whole ticks, and the tick is the pacing: the recorder runs
        // at the microphone's rate, as a call's loop does.
        match told.recv_timeout(TICK) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let mut frame = [0i16; FRAME_SAMPLES];
        if audio.capture(&mut frame) {
            let bytes = match encoder.encode(&frame, &mut packet) {
                Some(bytes) => bytes,
                // One frame that would not encode is a moment of quiet, not the end.
                None => continue,
            };
            if file.packet(&packet[..bytes]).is_err() {
                drop(file);
                fail("The recording could not be written.");
            }
            loudness.frame(audio::level(&frame));
            loudness.bar();
            frames += 1;
        }
        if since.elapsed() >= LIMIT {
            break;
        }
    }
    let duration = u32::try_from(frames * 20).unwrap_or(u32::MAX);
    let waveform = loudness.over();
    let _ = file.end(u64::from(frames) * crate::FRAME as u64);
    out::write(&RecEvent::Done { duration, waveform });
    Ok(())
}




#[cfg(test)]
mod tests {
    use super::*;

    /// A recording off the stand-in microphone (the tone) leaves a file the player's own
    /// reader opens: as long as the recording ran, and carrying the tone that was heard. This
    /// is the whole trip a voice note needs, with no microphone required.
    #[test]
    fn a_tone_recorded_leaves_a_file_the_player_can_open() {
        let dir = std::env::temp_dir().join(format!("tinyclient-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.opus");
        std::env::set_var("TINYCLIENT_MEDIAD_AUDIO", "tone");
        let (tell, told) = channel();
        // One second of tone: the encoder is fed whole frames the whole time.
        let _ = std::thread::Builder::new().name("stop".into()).spawn(move || {
            std::thread::sleep(Duration::from_millis(1000));
            let _ = tell.send(());
        });
        run_with(path.clone(), told).unwrap();
        std::env::remove_var("TINYCLIENT_MEDIAD_AUDIO");

        let mut source = crate::decode::Source::open(&path).unwrap();
        let seconds = source.duration() as f64 / 1000.;
        // A debug build's encoder may not keep up with the microphone; whatever it kept, the
        // file is a voice note as long as the recording got to, around the second it ran.
        assert!((0.4..1.3).contains(&seconds), "{seconds} s");
        let mut heard = Vec::new();
        while source.read(&mut heard).unwrap() {}
        let seconds = heard.len() as f64 / 2. / crate::RATE as f64;
        assert!((0.4..1.3).contains(&seconds), "{seconds} s heard");
        // The reader hands stereo; the test hears one ear to a mono line.
        let mono: Vec<f32> = heard.chunks_exact(2).map(|pair| (pair[0] + pair[1]) / 2.).collect();
        eprintln!("mono: {} first {:?} loudest {:?}", mono.len(), mono[..3.min(mono.len())].to_vec(), mono.iter().fold(0f32, |a, b| a.max(*b)));
        // Rising zero crossings of what was heard a second: the 440 Hz the stand-in makes.
        // A crossing between two samples is half-seen by a pair window, so twice as many go
        // by as are counted here.
        let rising = mono.chunks_exact(2).filter(|pair| pair[0] < 0. && pair[1] >= 0.).count();
        let pitch = rising as f64 * 2. * crate::RATE as f64 / (mono.len() as f64).max(1.);
        assert!((380. ..500.).contains(&pitch), "{pitch} Hz");
        // Mono is the same in both ears, as a voice note is.
        assert!(heard.chunks_exact(2).all(|pair| (pair[0] - pair[1]).abs() < 0.001));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The loudness bars of a recording spread over the bars the facts carry, and nothing is
    /// louder than the loudest moment.
    #[test]
    fn the_bars_carry_the_loudness_over_whatever_ran() {
        let mut loudness = Loudness::new();
        for frame in [100u8, 10, 0, 100, 10, 0] {
            loudness.frame(frame);
            loudness.bar();
        }
        let over = loudness.over();
        assert_eq!(over.len(), 6, "that short needs no spreading");
        assert_eq!(over.iter().max().copied(), Some(255u8), "a loud frame stays loud");
        // Gaps stay quiet, even with a loud moment spread over their neighbourhood.
        let mut loudness = Loudness::new();
        loudness.frame(100);
        for _ in 0..63 {
            loudness.frame(0);
            loudness.bar();
        }
        let over = loudness.over();
        assert_eq!(over.len(), 63);
        assert!(over.iter().filter(|&&bar| bar > 0).count() <= 2, "{over:?}");
    }
}




