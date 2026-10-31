//! `mediad test`: Settings' microphone test (V3) and test sound (S12) when there is no call.
//!
//! chatd sends the same frames as to a call (`Tune`, `Devices`, `Processing`, `Meter`, `Cue`),
//! each answered `Done`. The meter is of the microphone after the call's processing, so the
//! threshold drawn over it is the one a call would use. `Loopback` also plays the microphone
//! back on the speaker as others would hear it, with the echo canceller hearing what is played,
//! so a test on speakers does not howl. The microphone is open only while the meter is on, the
//! speaker only while a cue or the loopback plays, and the process exits once neither has
//! happened for [`IDLE`], when chatd closes our stdin, or [`METER_LIMIT`] after the meter was
//! turned on.

use crate::audio::{level, Audio};
use crate::cue::Playing;
use crate::permission::{microphone as microphone_access, Microphone};
use crate::process::Processor;
use crate::session::Controls;
use crate::{media, out, FRAME_SAMPLES};
use protocol::{read_frame, MediaRequest, MediaResponse};
use std::io::ErrorKind;
use std::sync::mpsc::{channel, TryRecvError};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(20);
/// Idle this long, the test is over. Well inside the 3 s the product allows.
const IDLE: Duration = Duration::from_millis(1500);
/// A meter left on stops by itself.
pub const METER_LIMIT: Duration = Duration::from_secs(30);
/// How long the speaker stays open after a cue, for the device to play what it holds.
const DRAIN: Duration = Duration::from_millis(300);

/// Ends the process when the device would not open: chatd sees us gone and the meter stop.
fn give_up(err: crate::audio::AudioError) -> ! {
    let (reason, detail) = media::failure(err);
    out::fail(reason, detail)
}

pub fn run() {
    let (requests, asked) = channel();
    let _ = std::thread::Builder::new().name("stdin".to_owned()).spawn(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            match read_frame::<MediaRequest>(&mut stdin) {
                Ok(request) => {
                    if requests.send(request).is_err() {
                        return;
                    }
                }
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => return,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    return;
                }
            }
        }
    });
    let controls = Controls::default();
    let (mut want_input, mut want_output) = (None, None);
    let mut metering: Option<Instant> = None;
    let (mut microphone, mut speaker): (Option<Audio>, Option<Audio>) = (None, None);
    let mut playing: Option<Playing> = None;
    let mut processor = Processor::new();
    let mut loopback = false;
    let mut drained = Instant::now();
    let mut busy = Instant::now();
    let (mut ticks, mut loudest) = (0u64, 0u8);
    let mut next = Instant::now();
    loop {
        loop {
            match asked.try_recv() {
                Ok(request) => {
                    match request {
                        MediaRequest::Tune(tuning) => controls.tune(tuning),
                        MediaRequest::Devices { input, output } => {
                            // A choice made while a device is open takes effect on the next open.
                            (want_input, want_output) = (input, output);
                        }
                        MediaRequest::Meter { on } => {
                            metering = on.then(Instant::now);
                            loopback &= on;
                        }
                        MediaRequest::Loopback { on } => loopback = on,
                        MediaRequest::Processing(processing) => controls.set_processing(processing),
                        MediaRequest::Input(mode) => controls.set_input(mode),
                        MediaRequest::Cue { cue } => controls.cue(cue),
                        MediaRequest::SaveLogs => {
                            let path = protocol::log_policy::current()
                                .and_then(|guard| guard.export_bundle().ok().flatten())
                                .map(|path| path.display().to_string())
                                .unwrap_or_default();
                            out::send(&MediaResponse::LogsSaved { path });
                            continue;
                        }
                        _ => {
                            out::send(&MediaResponse::Refused { detail: "There is no call.".to_owned() });
                            continue;
                        }
                    }
                    out::send(&MediaResponse::Done);
                }
                Err(TryRecvError::Empty) => break,
                // chatd closed our stdin: the test is over whatever it was doing.
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let now = Instant::now();
        if metering.is_some_and(|since| now - since > METER_LIMIT) {
            tracing::warn!("mediad: the microphone test ran 30 s; stopping it");
            metering = None;
        }

        // What the speaker plays this tick: the microphone, for the loopback, and a cue on top.
        let mut played = [0i16; FRAME_SAMPLES];
        let mut play = false;
        processor.set(controls.processing());
        if metering.is_some() {
            if microphone.is_none() && !Audio::simulated() && microphone_access() == Some(Microphone::Denied) {
                out::fail(protocol::CallFailure::Audio, media::MICROPHONE_DENIED);
            }
            let audio = match &mut microphone {
                Some(audio) => audio,
                None => microphone.insert(Audio::open_sides(want_input.clone(), None, (true, false)).unwrap_or_else(|err| give_up(err))),
            };
            let mut frame = [0i16; FRAME_SAMPLES];
            if audio.capture(&mut frame) {
                processor.capture(&mut frame);
                loudest = loudest.max(level(&frame));
                if loopback {
                    played = frame;
                    play = true;
                }
            }
            ticks += 1;
            // Ten a second, as in a call.
            if ticks.is_multiple_of(5) {
                out::send(&MediaResponse::Levels { input: loudest, output: 0 });
                loudest = 0;
            }
        } else if microphone.take().is_some() {
            // The recording indicator goes off with the meter.
            ticks = 0;
        }

        if let Some(cue) = controls.take_cue() {
            playing = Some(Playing::new(cue, f32::from(controls.tuning().cues.min(100)) / 100.));
        }
        if let Some(cue) = &mut playing {
            play = true;
            if !cue.mix(&mut played) {
                playing = None;
                drained = now + DRAIN;
            }
        }
        if play {
            let audio = match &mut speaker {
                Some(audio) => audio,
                None => speaker.insert(Audio::open_sides(None, want_output.clone(), (false, true)).unwrap_or_else(|err| give_up(err))),
            };
            audio.play(&played);
            processor.render(&played);
            if loopback {
                drained = now + DRAIN;
            }
        } else if speaker.is_some() && now > drained {
            speaker = None;
        }

        if metering.is_some() || playing.is_some() || speaker.is_some() {
            busy = now;
        } else if now - busy > IDLE {
            return;
        }
        next += TICK;
        let wait = next.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            next = Instant::now();
        }
        std::thread::sleep(wait);
    }
}
