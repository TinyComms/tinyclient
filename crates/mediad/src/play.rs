//! `mediad --play`: one voice message, audio file, or round video message, from chatd's `Open`
//! until the end, or until chatd closes our stdin (pause, another file, stop). Then the process
//! exits, so nothing of the file, the decoder or the speaker outlives playing it.
//!
//! The loop decodes only as far ahead as the speaker needs ([`AHEAD`]); the rest of the time it
//! waits on chatd's next word or on the speaker draining. A round video message also demuxes its
//! H.264 picture ([`crate::reel`]) and sends frames. Its sound is silence while it is muted, and
//! when the file has no audio track. A track that will not decode is still a failure.

use crate::audio::Speaker;
use crate::decode::Source;
use crate::reel::Reel;
use crate::stretch::Stretch;
use crate::{CHANNELS, RATE};
use protocol::{read_frame, PlayerEvent, PlayerRequest, RATES};
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

/// Audio held ahead of the speaker: enough to ride out a busy moment, short enough that a
/// seek or a new speed is heard at once.
const AHEAD: Duration = Duration::from_millis(250);
/// How often the speaker is looked at while it holds enough.
const NAP: Duration = Duration::from_millis(20);
/// Silence for a video with no audio track, one voice frame at a time, so the picture keeps up.
const SILENT_MS: u32 = 20;

/// What the speaker is fed. A file with an audio track, or silence the length of a picture
/// whose container has none.
enum Sound {
    File(Source),
    Silent { duration: u32, cursor: u32 },
}

impl Sound {
    fn duration(&self) -> u32 {
        match self {
            Sound::File(source) => source.duration(),
            Sound::Silent { duration, .. } => *duration,
        }
    }

    fn seek(&mut self, at: u32) -> Result<u32, String> {
        match self {
            Sound::File(source) => source.seek(at),
            Sound::Silent { duration, cursor } => {
                *cursor = at.min(*duration);
                Ok(at)
            }
        }
    }

    /// Appends the next piece. False once, with nothing added, at the end.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        match self {
            Sound::File(source) => source.read(out),
            Sound::Silent { duration, cursor } => {
                if *cursor >= *duration {
                    return Ok(false);
                }
                let step = SILENT_MS.min(*duration - *cursor);
                let frames = (u64::from(step) * u64::from(RATE) / 1000) as usize;
                out.resize(out.len() + frames * CHANNELS, 0.);
                *cursor += step;
                Ok(true)
            }
        }
    }
}

/// One event to chatd. A closed pipe is chatd gone, and with it whoever wanted to hear this:
/// `out::write` exits.
fn send(event: &PlayerEvent) {
    crate::out::write(event);
}

fn fail(detail: impl Into<String>) -> ! {
    send(&PlayerEvent::Failed { detail: detail.into() });
    crate::out::exit(2);
}

/// chatd's requests, read on their own thread so the loop never blocks on stdin. `None` is
/// stdin closing.
fn requests() -> Receiver<Option<PlayerRequest>> {
    let (tell, told) = channel();
    let _ = crate::out::spawn(std::thread::Builder::new().name("requests".to_owned()), move || {
        let mut stdin = crate::out::stdin();
        loop {
            match read_frame::<PlayerRequest>(&mut stdin) {
                Ok(request) => {
                    if tell.send(Some(request)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    if err.kind() != ErrorKind::UnexpectedEof {
                        tracing::warn!("mediad: {err}");
                    }
                    let _ = tell.send(None);
                    return;
                }
            }
        }
    });
    told
}

fn rate_of(rate: u16) -> u16 {
    if RATES.contains(&rate) {
        rate
    } else {
        100
    }
}

/// Where the listener is: what was read, less what is still on its way to the speaker.
struct Clock {
    /// Milliseconds into the file where reading last started.
    from: u32,
    /// 48 kHz frames read since then.
    read: u64,
}

impl Clock {
    fn heard(&self, stretch: &Stretch, speaker: &Speaker) -> u32 {
        // What the speaker holds was made at `rate`, so it stands for that much more input.
        let queued = speaker.queued() as u64 * u64::from(stretch.rate()) / 100;
        let behind = stretch.held() as u64 + queued;
        let frames = self.read.saturating_sub(behind);
        self.from + (frames * 1000 / u64::from(RATE)) as u32
    }
}

pub fn run() -> io::Result<()> {
    let told = requests();
    let (path, at, rate, output, round, muted, looped) = match told.recv() {
        Ok(Some(PlayerRequest::Open { path, at, rate, output, round, muted, looped })) => (path, at, rate, output, round, muted, looped),
        Ok(Some(other)) => {
            tracing::warn!("mediad: {other:?} before Open");
            fail("Playing could not start.")
        }
        _ => return Ok(()),
    };
    let file = Path::new(&path);
    let opened = Source::open(file);
    // `NO_AUDIO` is a video container with no audio track. Anything else, including a track
    // this build cannot decode, still fails before the picture is opened.
    let no_audio = opened.as_ref().is_err_and(|detail| detail == crate::decode::NO_AUDIO);
    let mut reel = if round && (opened.is_ok() || no_audio) {
        Some(Reel::open(file).unwrap_or_else(|detail| fail(detail)))
    } else {
        None
    };
    let mut sound = match opened {
        Ok(source) => Sound::File(source),
        Err(_) if no_audio => match reel.as_ref() {
            Some(picture) => Sound::Silent { duration: picture.duration(), cursor: 0 },
            None => fail(crate::decode::NO_AUDIO.to_owned()),
        },
        Err(detail) => fail(detail),
    };
    let duration = sound.duration();
    let mut speaker = Speaker::open(output.as_deref()).unwrap_or_else(|err| {
        tracing::warn!("mediad: speaker: {err:?}");
        fail("The speaker could not be opened.")
    });
    let mut stretch = Stretch::new(rate_of(rate));
    let mut clock = Clock { from: 0, read: 0 };
    if at > 0 && (duration == 0 || at < duration) {
        clock.from = sound.seek(at).unwrap_or_else(|detail| fail(detail));
        if let Some(reel) = &mut reel {
            reel.seek(at).unwrap_or_else(|detail| fail(detail));
        }
    }
    send(&PlayerEvent::Playing { at: clock.from, duration, rate: stretch.rate() });

    let ahead = (AHEAD.as_secs_f64() * f64::from(RATE)) as usize;
    let (mut piece, mut made) = (Vec::new(), Vec::new());
    let mut ended = false;
    loop {
        // chatd first: a seek or a new speed is heard before anything more is read.
        let wait = if ended || speaker.queued() >= ahead { NAP } else { Duration::ZERO };
        match told.recv_timeout(wait) {
            Ok(Some(PlayerRequest::Seek { at })) => {
                speaker.clear();
                let rate = stretch.rate();
                stretch.reset(rate);
                clock = Clock { from: sound.seek(at).unwrap_or_else(|detail| fail(detail)), read: 0 };
                if let Some(reel) = &mut reel {
                    reel.seek(clock.from).unwrap_or_else(|detail| fail(detail));
                }
                ended = false;
                send(&PlayerEvent::Playing { at: clock.from, duration, rate });
                continue;
            }
            Ok(Some(PlayerRequest::Rate { rate })) => {
                let at = clock.heard(&stretch, &speaker);
                speaker.clear();
                // What was held is fed again at the new speed, so nothing is skipped; what the
                // speaker dropped is read again from `at`.
                stretch.reset(rate_of(rate));
                clock = Clock { from: sound.seek(at).unwrap_or_else(|detail| fail(detail)), read: 0 };
                if let Some(reel) = &mut reel {
                    reel.seek(clock.from).unwrap_or_else(|detail| fail(detail));
                }
                ended = false;
                send(&PlayerEvent::Playing { at: clock.from, duration, rate: stretch.rate() });
                continue;
            }
            Ok(Some(PlayerRequest::Open { .. })) => tracing::warn!("mediad: a second Open is ignored"),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                send(&PlayerEvent::Stopped { at: clock.heard(&stretch, &speaker) });
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if speaker.broken() {
            fail("The speaker stopped.");
        }
        if ended {
            if speaker.queued() == 0 {
                if looped {
                    speaker.clear();
                    let rate = stretch.rate();
                    stretch.reset(rate);
                    clock = Clock { from: sound.seek(0).unwrap_or_else(|detail| fail(detail)), read: 0 };
                    if let Some(reel) = &mut reel {
                        reel.seek(0).unwrap_or_else(|detail| fail(detail));
                    }
                    ended = false;
                    send(&PlayerEvent::Playing { at: 0, duration, rate });
                    continue;
                }
                send(&PlayerEvent::Ended { duration: duration.max(clock.heard(&stretch, &speaker)) });
                return Ok(());
            }
            continue;
        }
        if speaker.queued() >= ahead {
            continue;
        }
        piece.clear();
        let more = sound.read(&mut piece).unwrap_or_else(|detail| fail(detail));
        clock.read += (piece.len() / CHANNELS) as u64;
        made.clear();
        stretch.push(&piece, &mut made);
        if muted {
            made.fill(0.);
        }
        if let Some(reel) = &mut reel {
            if let Some(frame) = reel.catch_up(clock.heard(&stretch, &speaker)).unwrap_or_else(|detail| fail(detail)) {
                send(&frame);
            }
        }
        if !more {
            // The stretcher's last windows: what it holds is played through at normal speed.
            let left = stretch.reset(100);
            made.extend_from_slice(&left);
            ended = true;
        }
        speaker.push(&made);
        if ended {
            speaker.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_lasts_as_long_as_the_picture_and_then_ends() {
        let mut sound = Sound::Silent { duration: 50, cursor: 0 };
        assert_eq!(sound.duration(), 50);
        let mut piece = Vec::new();
        let mut reads = 0;
        while sound.read(&mut piece).unwrap() {
            reads += 1;
            assert!(reads < 8, "did not end");
        }
        assert_eq!(reads, 3, "20 ms, 20 ms, then the last 10 ms");
        assert!(piece.iter().all(|sample| *sample == 0.));
        let ms = (piece.len() / CHANNELS) as u32 * 1000 / RATE;
        assert_eq!(ms, 50);
        let kept = piece.len();
        assert!(!sound.read(&mut piece).unwrap());
        assert_eq!(piece.len(), kept, "the end adds nothing");
        assert_eq!(sound.seek(30).unwrap(), 30);
        piece.clear();
        assert!(sound.read(&mut piece).unwrap());
        assert_eq!((piece.len() / CHANNELS) as u32 * 1000 / RATE, 20);
        assert!(!sound.read(&mut Vec::new()).unwrap());
    }
}
