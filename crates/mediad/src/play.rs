//! `mediad --play`: one voice message or audio file, from chatd's `Open` until the end, or until
//! chatd closes our stdin (pause, another file, stop). Then the process exits, so nothing of the
//! file, the decoder or the speaker outlives playing it.
//!
//! The loop decodes only as far ahead as the speaker needs ([`AHEAD`]); the rest of the time it
//! waits on chatd's next word or on the speaker draining.

use crate::audio::Speaker;
use crate::decode::Source;
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
    let (path, at, rate, output) = match told.recv() {
        Ok(Some(PlayerRequest::Open { path, at, rate, output })) => (path, at, rate, output),
        Ok(Some(other)) => {
            tracing::warn!("mediad: {other:?} before Open");
            fail("Playing could not start.")
        }
        _ => return Ok(()),
    };
    let mut source = Source::open(Path::new(&path)).unwrap_or_else(|detail| fail(detail));
    let duration = source.duration();
    let mut speaker = Speaker::open(output.as_deref()).unwrap_or_else(|err| {
        tracing::warn!("mediad: speaker: {err:?}");
        fail("The speaker could not be opened.")
    });
    let mut stretch = Stretch::new(rate_of(rate));
    let mut clock = Clock { from: 0, read: 0 };
    if at > 0 && (duration == 0 || at < duration) {
        clock.from = source.seek(at).unwrap_or_else(|detail| fail(detail));
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
                clock = Clock { from: source.seek(at).unwrap_or_else(|detail| fail(detail)), read: 0 };
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
                clock = Clock { from: source.seek(at).unwrap_or_else(|detail| fail(detail)), read: 0 };
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
                send(&PlayerEvent::Ended { duration: duration.max(clock.heard(&stretch, &speaker)) });
                return Ok(());
            }
            continue;
        }
        if speaker.queued() >= ahead {
            continue;
        }
        piece.clear();
        let more = source.read(&mut piece).unwrap_or_else(|detail| fail(detail));
        clock.read += (piece.len() / CHANNELS) as u64;
        made.clear();
        stretch.push(&piece, &mut made);
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
