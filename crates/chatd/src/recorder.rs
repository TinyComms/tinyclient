//! The recording of a voice message, across every network: what the shell is told
//! (`Change::Recording`) and the `mediad --rec` that records it. chatd writes no audio here; it
//! starts mediad for a file when the user presses the mic and closes its stdin on stop or forget,
//! so a recording gone holds no device at all.
//!
//! The file is chatd's to name: a `voice-<millis>.opus` beside the account's cache files, so a
//! recording is sent from inside the media cache like any file. One recording at a time, across
//! every network: starting another ends the one before it, whose file is dropped once its mediad
//! has let go of it.

use crate::blobs::dir_under;
use crate::hub::{lock, Hub};
use crate::media::mediad_path;
use protocol::{read_frame, Change, Key, RecordRequest, Recording, RecordState, RecEvent, WAVEFORM_LIMIT};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The word for a recording chatd itself could not start. mediad's own failures name themselves.
pub const NO_RECORDER: &str = "Recording did not start. Try again.";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// One recording mediad: the stdin to close for a stop, the recording as it was started, the file
/// it is making, and whether that file is still wanted.
struct Active {
    stdin: Option<Box<dyn io::Write + Send>>,
    /// False once the recording was replaced or forgotten: the file goes once mediad is done,
    /// unless its facts arrived — a finished note's file stays for the send it was named by.
    keep: Arc<AtomicBool>,
    /// True once mediad said the file is made (or failed): anything of the send is told by it.
    said: Arc<AtomicBool>,
    recording: Recording,
    path: PathBuf,
}

/// What mediad's word makes of `shown`. False when the word is stale: a recording replaced or
/// forgotten before its mediad answered is not told its facts.
fn heard(shown: &mut Recording, active: &Active, event: &RecEvent) -> bool {
    if *shown != active.recording {
        return false;
    }
    *shown = match event {
        RecEvent::Done { duration, waveform } => Recording {
            conversation: active.recording.conversation.clone(),
            since: active.recording.since,
            state: RecordState::Recorded {
                path: active.path.display().to_string(),
                duration: *duration,
                waveform: waveform.iter().copied().take(WAVEFORM_LIMIT).collect(),
            },
        },
        RecEvent::Failed { detail } => Recording {
            conversation: active.recording.conversation.clone(),
            since: active.recording.since,
            state: RecordState::Failed { detail: detail.clone() },
        },
    };
    true
}

#[derive(Default)]
pub struct Recorder {
    /// What the shell is shown, until the file is sent or the recording forgotten.
    shown: Mutex<Option<Recording>>,
    /// The mediad running, while there is one.
    active: Mutex<Option<Active>>,
}

impl Recorder {
    /// The one recording, wherever the user has gone since.
    pub fn shown(&self) -> Option<Recording> {
        lock(&self.shown).clone()
    }

    fn tell(&self, hub: &Hub) {
        hub.changed(Change::Recording { recording: self.shown() });
    }

    /// The shell's request. Every answer is `Ok`: what happened is a `Change::Recording`.
    pub fn request(&self, hub: &Arc<Hub>, media: &Path, request: RecordRequest) {
        match request {
            RecordRequest::Start { conversation } => self.start(hub, media, conversation),
            RecordRequest::Stop => self.stop(hub),
            RecordRequest::Forget => self.forget(hub),
        }
    }

    /// Starts a mediad. Whatever was recording ends first: its facts never arrive any more.
    fn start(&self, hub: &Arc<Hub>, media: &Path, conversation: Key) {
        let since = now_ms();
        let path = dir_under(media, &conversation.account()).join(format!("voice-{since}.opus"));
        self.replace(hub, None);
        let recording = Recording { conversation, since, state: RecordState::Active };
        let _ = std::fs::create_dir_all(path.parent().expect("the cache has a parent"));
        self.spawn_recording(hub, &recording, &path);
        self.tell(hub);
    }

    /// Runs `mediad --rec` for the recording's file, and reads its one event off its stdout.
    fn spawn_recording(&self, hub: &Arc<Hub>, recording: &Recording, path: &Path) {
        let error = match mediad_path() {
            Err(err) => Some(err),
            Ok(mediad) => {
                let mut command = Command::new(mediad);
                command.arg("--rec").arg(path);
                protocol::launch::no_console(&mut command);
                match protocol::media_host::spawn(&mut command) {
                    Ok(spawned) => {
                        self.running(hub, recording, path, spawned);
                        None
                    }
                    Err(err) => Some(err),
                }
            }
        };
        if let Some(err) = error {
            tracing::warn!("chatd: the recorder did not start: {err}");
            *lock(&self.active) = None;
            *lock(&self.shown) = Some(Recording {
                conversation: recording.conversation.clone(),
                since: recording.since,
                state: RecordState::Failed { detail: NO_RECORDER.to_owned() },
            });
        }
    }

    /// The mediad is up: the recording is on show, and its events are read until it exits.
    fn running(&self, hub: &Arc<Hub>, recording: &Recording, path: &Path, spawned: protocol::media_host::Spawned) {
        let protocol::media_host::Spawned { mut child, stdin, stdout } = spawned;
        let mut stdout = BufReader::new(stdout.expect("stdout is piped"));
        let active = Active {
            stdin,
            keep: Arc::new(AtomicBool::new(true)),
            said: Arc::new(AtomicBool::new(false)),
            recording: recording.clone(),
            path: path.to_owned(),
        };
        let (keep, said) = (active.keep.clone(), active.said.clone());
        *lock(&self.active) = Some(active);
        // Ends when mediad exits, which it does when chatd closes our stdin.
        let hub = hub.clone();
        let recording = recording.clone();
        let path = path.to_owned();
        let _ = std::thread::Builder::new().name("recorder".into()).stack_size(128 * 1024).spawn(move || {
            while let Ok(event) = read_frame::<RecEvent>(&mut stdout) {
                hub.recorder.reported(&hub, &recording, &path, event);
            }
            let _ = child.wait();
            if !keep.load(Ordering::Relaxed) && !said.load(Ordering::Relaxed) {
                // Best effort: a recording nobody kept (replaced or forgotten while running)
                // leaves no file at all. A finished note's file stays: a send may still hold it.
                let _ = std::fs::remove_file(&path);
            }
        });
    }

    /// Ends the recording. mediad finishes the file, says the facts, and exits.
    fn stop(&self, hub: &Hub) {
        if let Some(active) = lock(&self.active).as_mut() {
            active.stdin.take();
        }
        self.tell(hub);
    }

    /// The composer is not holding a recording: the facts and the file go.
    fn forget(&self, hub: &Hub) {
        self.replace(hub, None);
    }

    /// Ends a recording so another can be. A replaced recording's facts never arrive: the running
    /// one alone answers for itself.
    fn replace(&self, hub: &Hub, kept: Option<Recording>) {
        if let Some(mut past) = lock(&self.active).take() {
            past.keep.store(false, Ordering::Relaxed);
            drop(past.stdin.take());
        }
        *lock(&self.shown) = kept;
        self.tell(hub);
    }

    /// A word from the mediad of a recording. A word about one that no longer runs is not
    /// shown; whatever was reported goes on record, for the file's fate and the shell's shown.
    fn reported(&self, hub: &Arc<Hub>, recording: &Recording, path: &Path, event: RecEvent) {
        let hearing = {
            let mut shown = lock(&self.shown);
            let active = lock(&self.active);
            match (active.as_ref(), shown.as_mut()) {
                (Some(active), Some(shown)) if active.recording == *recording => {
                    active.said.store(true, Ordering::Relaxed);
                    heard(shown, active, &event)
                }
                _ => {
                    let _ = path;
                    false
                }
            }
        };
        if hearing {
            self.tell(hub);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Network;

    fn recording(since: u64) -> Recording {
        Recording { conversation: Key { network: Network::Telegram, account: "10".into(), id: "7002".into() }, since, state: RecordState::Active }
    }

    fn active(recording: Recording, path: &str) -> Active {
        Active {
            stdin: None,
            keep: Arc::new(AtomicBool::new(true)),
            said: Arc::new(AtomicBool::new(false)),
            recording: recording.clone(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn the_facts_of_a_finished_recording_are_shown_with_the_file() {
        let recording = recording(5_000);
        let running = active(recording.clone(), "media/telegram/10/voice-1000.opus");
        let mut shown = recording.clone();
        assert!(heard(&mut shown, &running, &RecEvent::Done { duration: 12_000, waveform: vec![31, 0] }));
        assert_eq!(
            shown.state.clone(),
            RecordState::Recorded { path: "media/telegram/10/voice-1000.opus".into(), duration: 12_000, waveform: vec![31, 0] }
        );
        assert_eq!(shown.conversation, recording.conversation);
        assert_eq!(shown.since, 5_000);
    }

    /// The bars are at most the contract's, whatever mediad heard.
    #[test]
    fn a_waveform_longer_than_the_contract_is_cut_to_it() {
        let recording = recording(5_000);
        let running = active(recording.clone(), "voice-0.opus");
        let mut shown = recording;
        let loud = vec![8u8; WAVEFORM_LIMIT + 10];
        assert!(heard(&mut shown, &running, &RecEvent::Done { duration: 60_000, waveform: loud }));
        if let RecordState::Recorded { waveform, .. } = shown.state.clone() {
            assert_eq!(waveform.len(), WAVEFORM_LIMIT);
        } else {
            unreachable!("the recording was done");
        }
    }

    #[test]
    fn a_failed_mic_is_a_failed_recording_in_mediads_words() {
        let recording = recording(5_000);
        let running = active(recording.clone(), "voice-0.opus");
        let mut shown = recording.clone();
        assert!(heard(&mut shown, &running, &RecEvent::Failed { detail: "No microphone.".into() }));
        assert_eq!(shown.state.clone(), RecordState::Failed { detail: "No microphone.".into() });
        assert_eq!(shown.conversation, recording.conversation);
        assert_eq!(shown.since, 5_000);
    }

    /// A recording replaced or forgotten before its mediad answered keeps its facts to itself.
    #[test]
    fn a_replaced_recording_hears_no_facts_any_more() {
        let old = recording(5_000);
        let running = active(old, "voice-0.opus");
        let mut shown = recording(9_000);
        assert!(!heard(&mut shown, &running, &RecEvent::Done { duration: 1_000, waveform: vec![] }));
        assert_eq!(shown.state.clone(), RecordState::Active);
        assert_eq!(shown.since, 9_000);
    }

    /// What is shown names the file gone with the recording it replaced: the path it kept is
    /// still told, but only by the running one's word.
    #[test]
    fn the_facts_name_the_path_the_recording_was_making() {
        let recording = recording(5_000);
        let running = active(recording.clone(), "media/voice-42.opus");
        let mut shown = recording;
        assert!(heard(&mut shown, &running, &RecEvent::Done { duration: 960, waveform: vec![0] }));
        if let RecordState::Recorded { path, .. } = shown.state.clone() {
            assert!(path.ends_with("voice-42.opus"), "{path}");
        } else {
            unreachable!("the recording was done");
        }
    }
}
