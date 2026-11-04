//! The mic: press to record, press again to stop, and the finished note in the tray until it is
//! sent — or, with push-to-talk on, hold to record and let go to stop. Anything of sound is
//! chatd's and mediad's to do: the composer asks (`Request::Record`) and reads what came back
//! (`Change::Recording`).

use protocol::{Capabilities, Key, OutgoingFile, RecordRequest, RecordState, Recording, VoiceNote};
use super::{super::playing, Tone};

/// Where the composer has a mic: the account takes files and names one a voice note with. A
/// recording is a file send, so a network without files has no voice notes either.
pub(in crate::gui) fn available(can: &Capabilities) -> bool {
    can.voice_notes && can.max_file_bytes > 0
}

/// What a press on the mic is: the first on a conversation records, the next stops. A press
/// while another recording holds starts over instead: one recording at a time, and the pushed
/// one goes (chatd ends it and says so).
pub(in crate::gui) fn on_press(recording: &Option<Recording>, target: &Key) -> RecordRequest {
    let recording = recording.as_ref().map(|recording| (recording.conversation == *target, recording));
    match recording {
        Some((true, recording)) if matches!(recording.state, RecordState::Active) => RecordRequest::Stop,
        _ => RecordRequest::Start { conversation: target.clone() },
    }
}

/// What a held mic is: the down records. Every down is a start, like a press where nothing of
/// this conversation's runs — chatd ends whatever ran, and what it kept goes with it.
pub(in crate::gui) fn on_hold_down(target: &Key) -> RecordRequest {
    RecordRequest::Start { conversation: target.clone() }
}

/// What letting the mic go asks, once the release grace has run: the stop, while this
/// conversation's recording still runs. One that never started, failed, or finished is none to
/// stop, and asking would only end another's.
pub(in crate::gui) fn on_hold_up(recording: &Option<Recording>, target: &Key) -> Option<RecordRequest> {
    let runs = recording
        .as_ref()
        .is_some_and(|recording| recording.conversation == *target && matches!(recording.state, RecordState::Active));
    runs.then_some(RecordRequest::Stop)
}

/// The composer's foot on a recording, for the conversation it belongs to. `now` is unix
/// milliseconds, as `since` is; `push` names the way the mic works, for what ends it.
pub(in crate::gui) fn foot(recording: &Option<Recording>, target: &Key, now: u64, push: bool) -> Option<(String, Tone)> {
    let recording = recording.as_ref().filter(|recording| recording.conversation == *target)?;
    Some(match &recording.state {
        RecordState::Active => {
            let how = if push { "release the mic to stop" } else { "press the mic again to stop" };
            (format!("Recording · {} — {}", clock(now.saturating_sub(recording.since)), how), Tone::Note)
        }
        RecordState::Recorded { duration, .. } => {
            (format!("Voice note · {} kept. It goes with your message.", clock(u64::from(*duration))), Tone::Note)
        }
        RecordState::Failed { detail } => (detail.clone(), Tone::Refused),
    })
}

/// The note a send carries its facts with: the file going out is the one recorded.
pub(in crate::gui) fn sent_as(recording: &Option<Recording>, files: &[OutgoingFile]) -> Option<VoiceNote> {
    if files.len() != 1 {
        return None;
    }
    let (recording, file) = (recording.as_ref()?, files.first()?);
    match &recording.state {
        RecordState::Recorded { duration, waveform, path } if *path == file.path => {
            Some(VoiceNote { duration: *duration, waveform: waveform.clone() })
        }
        _ => None,
    }
}

/// The chip is the one the facts name: taking it back gives the recording in (`Forget`).
pub(in crate::gui) fn holds_the_note(recording: &Option<Recording>, chip: &super::attach::Staged) -> bool {
    let Some(recording) = recording.as_ref() else { return false };
    match &recording.state {
        RecordState::Recorded { path, .. } => chip.path.to_string_lossy() == *path,
        _ => false,
    }
}

/// `0:07`, from milliseconds, in the same shape the players show.
fn clock(ms: u64) -> String {
    playing::clock(ms.min(u32::MAX as u64) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Network;

    fn target() -> Key {
        Key { network: Network::Telegram, account: "1".into(), id: "7002".into() }
    }

    fn active() -> Recording {
        Recording { conversation: target(), since: 10_000, state: RecordState::Active }
    }

    fn kept(path: &str) -> Recording {
        Recording {
            conversation: target(),
            since: 10_000,
            state: RecordState::Recorded { path: path.into(), duration: 12_000, waveform: vec![31, 0] },
        }
    }

    #[test]
    fn a_press_starts_a_recording_and_the_next_one_stops_it() {
        let key = target();
        assert!(matches!(on_press(&None, &key), RecordRequest::Start { .. }), "the first press records");
        assert!(matches!(on_press(&Some(active()), &key), RecordRequest::Stop), "the second press stops");
    }

    /// A press in the conversation already recording stops it; one elsewhere starts over.
    #[test]
    fn the_press_names_the_recording_of_this_conversation() {
        let key = target();
        let elsewhere = Recording { conversation: Key { account: key.account.clone(), network: key.network, id: "9001".into() }, ..active() };
        assert!(matches!(on_press(&Some(elsewhere), &key), RecordRequest::Start { .. }), "another's recording is no stop");
        // A finished note is not running either: the press records a new one.
        assert!(matches!(on_press(&Some(kept("voice-1.opus")), &key), RecordRequest::Start { .. }));
    }

    #[test]
    fn the_foot_shows_the_clock_while_it_records_and_the_note_when_it_is_kept() {
        let key = target();
        let (words, tone) = foot(&Some(active()), &key, 79_000, false).unwrap();
        assert_eq!(words, "Recording · 1:09 — press the mic again to stop");
        assert_eq!(tone, Tone::Note);
        let (words, tone) = foot(&Some(active()), &key, 79_000, true).unwrap();
        assert_eq!(words, "Recording · 1:09 — release the mic to stop");
        let (words, tone) = foot(&Some(kept("voice-1.opus")), &key, 80_000, false).unwrap();
        assert_eq!(words, "Voice note · 0:12 kept. It goes with your message.");
        assert_eq!(tone, Tone::Note);
        let (words, tone) = foot(&Some(Recording { since: 10_000, state: RecordState::Failed { detail: "No microphone.".into() }, ..active() }), &key, 80_000, false).unwrap();
        assert_eq!(words, "No microphone.");
        assert_eq!(tone, Tone::Refused);
        // Another conversation's recording is not this foot's to show.
        assert!(foot(&Some(active()), &Key { id: "9001".into(), ..key }, 80_000, false).is_none());
    }

    #[test]
    fn a_hold_down_records_and_its_release_stops_what_still_runs() {
        let key = target();
        assert!(matches!(on_hold_down(&key), RecordRequest::Start { .. }), "every down records");
        assert_eq!(on_hold_up(&Some(active()), &key), Some(RecordRequest::Stop), "the release stops the run");
    }

    /// A release stops only a recording that still runs here: a note kept, a failure, another
    /// conversation's recording, and nothing at all are none of its business.
    #[test]
    fn the_release_stops_nothing_else() {
        let key = target();
        assert_eq!(on_hold_up(&None, &key), None);
        let elsewhere = Recording { conversation: Key { account: key.account.clone(), network: key.network, id: "9001".into() }, ..active() };
        assert_eq!(on_hold_up(&Some(elsewhere), &key), None);
        assert_eq!(on_hold_up(&Some(kept("voice-1.opus")), &key), None, "a finished note is no run");
        let failed = Recording { state: RecordState::Failed { detail: "No microphone.".into() }, ..active() };
        assert_eq!(on_hold_up(&Some(failed), &key), None);
    }

    #[test]
    fn the_send_of_the_recorded_file_carries_its_facts() {
        let file = |path: &str| OutgoingFile { path: path.into(), name: String::new(), spoiler: false, as_file: false, original: false };
        let files = vec![file("voice-1.opus")];
        assert_eq!(sent_as(&Some(kept("voice-1.opus")), &files).unwrap(), VoiceNote { duration: 12_000, waveform: vec![31, 0] });
        // A send of anything else carries no facts.
        assert!(sent_as(&Some(kept("voice-1.opus")), &vec![file("trail.jpg")]).is_none());
        // A recording still running has no facts to carry.
        assert!(sent_as(&Some(active()), &files).is_none());
        assert!(sent_as(&None, &files).is_none());
        // Two files are not one voice note (`Outgoing::voice` names the last of `files`).
        let two = vec![file("voice-1.opus"), file("trail.jpg")];
        assert!(sent_as(&Some(kept("voice-1.opus")), &two).is_none());
    }
}
