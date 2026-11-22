//! The mic: press to record, press again to stop, and the finished note in the tray until it is
//! sent — or, with push-to-talk on, hold to record and let go to stop. Anything of sound is
//! chatd's and mediad's to do: the composer asks (`Request::Record`) and reads what came back
//! (`Change::Recording`).

use protocol::{Capabilities, Key, OutgoingFile, RecordRequest, RecordState, Recording, VoiceNote};
use super::{super::playing, Tone};

/// Where the composer has a mic: any account that supports voice notes, including networks like
/// Teams that upload voice notes to a dedicated media store even when general file uploads are off.
pub(in crate::gui) fn available(can: &Capabilities) -> bool {
    can.voice_notes
}

/// Whether voice and round video messages are allowed in a conversation given account capabilities
/// and any per-conversation restriction reason (`Conversation::voice_notes_forbidden`).
#[allow(dead_code)]
pub(in crate::gui) fn allowed_in(can: &Capabilities, voice_notes_forbidden: &str) -> bool {
    available(can) && voice_notes_forbidden.is_empty()
}

/// Returns the per-conversation reason voice and round video messages are disabled, if any.
pub(in crate::gui) fn blocked_reason(voice_notes_forbidden: &str) -> Option<&str> {
    (!voice_notes_forbidden.is_empty()).then_some(voice_notes_forbidden)
}

/// What a press on the mic is: the first on a conversation records, the next stops. A press
/// while another recording holds starts over instead: one recording at a time, and the pushed
/// one goes (chatd ends it and says so).
pub(in crate::gui) fn on_press(recording: &Option<Recording>, target: &Key, input: Option<String>, video: bool) -> RecordRequest {
    let recording = recording.as_ref().map(|recording| (recording.conversation == *target, recording));
    match recording {
        Some((true, recording)) if matches!(recording.state, RecordState::Active) => RecordRequest::Stop,
        _ => RecordRequest::Start { conversation: target.clone(), input, video },
    }
}

/// What a held mic is: the down records. Every down is a start, like a press where nothing of
/// this conversation's runs — chatd ends whatever ran, and what it kept goes with it.
pub(in crate::gui) fn on_hold_down(target: &Key, input: Option<String>, video: bool) -> RecordRequest {
    RecordRequest::Start { conversation: target.clone(), input, video }
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

/// What a word from chatd means to a stop that is still owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::gui) enum Owed {
    /// The recording the hold began runs now: stop it, and the debt is paid.
    Stop,
    /// It ended by itself (a note kept, or a failure): nothing is owed any more.
    Paid,
    /// Not a word about it yet: the debt stands.
    Wait,
}

/// The mic was let go before chatd said its recording runs: a quick tap, a release grace of
/// none, or a recorder slow to start. `on_hold_up` had nothing to stop then, so the stop is owed
/// to the conversation, and each `Change::Recording` after it is asked here. Without this the
/// recording would start a moment later with nobody left to end it.
pub(in crate::gui) fn on_owed_stop(recording: &Option<Recording>, owed: &Key) -> Owed {
    match recording.as_ref().filter(|recording| recording.conversation == *owed) {
        Some(recording) if matches!(recording.state, RecordState::Active) => Owed::Stop,
        Some(_) => Owed::Paid,
        // Nothing on show, or another conversation's: the start is still on its way.
        None => Owed::Wait,
    }
}

/// The lab's push-to-talk-release driver: the whole of a quick tap, as the composer walks it.
/// The press records; the release finds nothing running yet and owes the stop; chatd's words
/// arrive, and the one that says the recording runs is answered with the stop.
pub(in crate::gui) fn prove_push_to_talk_release() -> Result<(), &'static str> {
    let key = Key { network: protocol::Network::Telegram, account: "1".into(), id: "7002".into() };
    let running = Recording {
        conversation: key.clone(),
        since: 10_000,
        device: None,
        notice: None,
        video: false,
        side: 0,
        frame: None,
        state: RecordState::Active,
    };
    if !matches!(on_hold_down(&key, None, false), RecordRequest::Start { .. }) {
        return Err("holding the mic did not start a recording");
    }
    if on_hold_up(&Some(running.clone()), &key) != Some(RecordRequest::Stop) {
        return Err("letting go did not stop the recording that ran");
    }
    // The quick tap: let go before the recording is said to run.
    if on_hold_up(&None, &key).is_some() {
        return Err("letting go stopped a recording that was not there");
    }
    if on_owed_stop(&None, &key) != Owed::Wait {
        return Err("the owed stop was dropped before the recording began");
    }
    if on_owed_stop(&Some(running.clone()), &key) != Owed::Stop {
        return Err("a quick tap left the microphone recording");
    }
    let failed = Recording { state: RecordState::Failed { detail: "No microphone.".into() }, ..running };
    if on_owed_stop(&Some(failed), &key) != Owed::Paid {
        return Err("a recording that failed was still owed a stop");
    }
    Ok(())
}

/// The composer's foot on a recording, for the conversation it belongs to. `now` is unix
/// milliseconds, as `since` is; `push` names the way the mic works, for what ends it.
pub(in crate::gui) fn foot(recording: &Option<Recording>, target: &Key, now: u64, push: bool) -> Option<(String, Tone)> {
    let recording = recording.as_ref().filter(|recording| recording.conversation == *target)?;
    Some(match &recording.state {
        RecordState::Active => {
            let elapsed = clock(now.saturating_sub(recording.since));
            let kind = if recording.video { "Recording a video message" } else { "Recording" };
            if let Some(notice) = recording.notice.as_deref().filter(|s| !s.is_empty()) {
                (format!("{kind} · {elapsed} — {notice}"), Tone::Warning)
            } else {
                let how = if push { "release the mic to stop" } else { "press the mic again to stop" };
                match recording.device.as_deref().filter(|s| !s.is_empty()) {
                    Some(device) => (format!("{kind} · {elapsed} on {device} — {how}"), Tone::Note),
                    None => (format!("{kind} · {elapsed} — {how}"), Tone::Note),
                }
            }
        }
        RecordState::Recorded { duration, .. } if recording.video => (
            format!(
                "Video message · {} kept. The whole take goes with your message, at most a minute.",
                clock(u64::from(*duration))
            ),
            Tone::Note,
        ),
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
    if recording.video {
        return None;
    }
    match &recording.state {
        RecordState::Recorded { duration, waveform, path } if *path == file.path => {
            Some(VoiceNote { duration: *duration, waveform: waveform.clone() })
        }
        _ => None,
    }
}

/// The round video a send carries. Picture only: this build writes no AAC track.
pub(in crate::gui) fn sent_video(recording: &Option<Recording>, files: &[OutgoingFile]) -> Option<protocol::VideoNote> {
    if files.len() != 1 {
        return None;
    }
    let (recording, file) = (recording.as_ref()?, files.first()?);
    if !recording.video {
        return None;
    }
    match &recording.state {
        RecordState::Recorded { duration, path, .. } if *path == file.path => {
            let (side, duration) = protocol::clamp_round(recording.side, *duration);
            Some(protocol::VideoNote { duration, side, sound: false })
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

/// Only the finished note held by this chip is previewable; ordinary attachments stay files.
pub(super) fn preview(recording: &Option<Recording>, chip: &super::attach::Staged) -> Option<playing::Play> {
    if chip.refused.is_some() || !holds_the_note(recording, chip) {
        return None;
    }
    let recording = recording.as_ref()?;
    let RecordState::Recorded { path, .. } = &recording.state else { return None };
    let video = recording.video;
    Some(playing::Play {
        account: recording.conversation.account(),
        file: path.clone(),
        path: path.clone(),
        conversation: recording.conversation.clone(),
        message: String::new(),
        title: if video { "Video message preview".to_owned() } else { "Voice message preview".to_owned() },
        at: None,
        round: video,
        muted: false,
        looped: video,
    })
}

pub(super) fn preview_caption(duration: u32, playback: Option<&protocol::Playback>, now: u64, video: bool) -> String {
    let Some(playback) = playback else {
        let kind = if video { "Video message" } else { "Voice note" };
        return format!("{kind} · {}", clock(u64::from(duration)));
    };
    let label = match &playback.state {
        protocol::PlayState::Failed { detail } => return detail.clone(),
        protocol::PlayState::Playing => "Playing",
        protocol::PlayState::Loading => "Loading",
        protocol::PlayState::Paused => "Paused",
        _ => "Voice note",
    };
    format!("{label} · {} / {}", clock(u64::from(playback.position(now))), clock(u64::from(duration)))
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
        Recording {
            conversation: target(),
            since: 10_000,
            device: None,
            notice: None,
            video: false,
            side: 0,
            frame: None,
            state: RecordState::Active,
        }
    }

    fn kept(path: &str) -> Recording {
        Recording {
            conversation: target(),
            since: 10_000,
            device: None,
            notice: None,
            video: false,
            side: 0,
            frame: None,
            state: RecordState::Recorded { path: path.into(), duration: 12_000, waveform: vec![31, 0] },
        }
    }

    #[test]
    fn only_the_staged_recording_has_a_preview_and_preview_keeps_send_metadata() {
        let path = std::path::PathBuf::from("voice.opus");
        let mut chip = super::super::attach::Staged::new(1, path, super::super::attach::Found::File(1000),
            &Capabilities { max_file_bytes: 1_000_000, ..Capabilities::default() }, Network::Telegram);
        let note = Some(kept("voice.opus"));
        let play = preview(&note, &chip).unwrap();
        assert_eq!(play.path, "voice.opus");
        assert_eq!(play.conversation, target());
        assert!(play.message.is_empty(), "a draft has no sent message");
        assert_eq!(sent_as(&note, &[chip.outgoing().unwrap()]).unwrap().duration, 12_000);
        assert!(preview(&Some(active()), &chip).is_none());
        assert!(preview(&Some(kept("other.opus")), &chip).is_none());
        chip.refused = Some("Not on disk anymore".into());
        assert!(preview(&note, &chip).is_none());
    }

    #[test]
    fn a_preview_shows_its_position_and_keeps_the_recorded_duration_while_loading() {
        let mut playback = protocol::Playback {
            account: target().account(), file: "voice.opus".into(), conversation: target(),
            message: String::new(), title: "Voice message preview".into(),
            state: protocol::PlayState::Loading, at: 0, since: 1000, duration: 0, rate: 100, has_next: false, has_previous: false, muted: false, picture: None,
        };
        assert_eq!(preview_caption(12_000, None, 1000, false), "Voice note · 0:12");
        assert_eq!(preview_caption(12_000, Some(&playback), 1000, false), "Loading · 0:00 / 0:12");
        playback.state = protocol::PlayState::Paused;
        playback.at = 3000;
        playback.duration = 12_000;
        assert_eq!(preview_caption(12_000, Some(&playback), 9000, false), "Paused · 0:03 / 0:12");
        playback.state = protocol::PlayState::Playing;
        assert_eq!(preview_caption(12_000, Some(&playback), 3000, false), "Playing · 0:05 / 0:12");
    }

    #[test]
    fn a_press_starts_a_recording_and_the_next_one_stops_it() {
        let key = target();
        assert_eq!(
            on_press(&None, &key, Some("Blue Yeti\tusb-mic".into()), false),
            RecordRequest::Start { conversation: key.clone(), input: Some("Blue Yeti\tusb-mic".into()), video: false },
            "the first press records with the chosen microphone"
        );
        assert!(matches!(on_press(&Some(active()), &key, None, false), RecordRequest::Stop), "the second press stops");
    }

    /// A press in the conversation already recording stops it; one elsewhere starts over.
    #[test]
    fn the_press_names_the_recording_of_this_conversation() {
        let key = target();
        let elsewhere = Recording { conversation: Key { account: key.account.clone(), network: key.network, id: "9001".into() }, ..active() };
        assert!(matches!(on_press(&Some(elsewhere), &key, None, true), RecordRequest::Start { video: true, .. }), "another's recording is no stop");
        // A finished note is not running either: the press records a new one.
        assert!(matches!(on_press(&Some(kept("voice-1.opus")), &key, None, false), RecordRequest::Start { .. }));
    }

    #[test]
    fn the_foot_shows_the_clock_while_it_records_and_the_note_when_it_is_kept() {
        let key = target();
        let (words, tone) = foot(&Some(active()), &key, 79_000, false).unwrap();
        assert_eq!(words, "Recording · 1:09 — press the mic again to stop");
        assert_eq!(tone, Tone::Note);
        let (words, _tone) = foot(&Some(active()), &key, 79_000, true).unwrap();
        assert_eq!(words, "Recording · 1:09 — release the mic to stop");
        let named = Recording { device: Some("Blue Yeti".into()), ..active() };
        let (words, tone) = foot(&Some(named), &key, 79_000, false).unwrap();
        assert_eq!(words, "Recording · 1:09 on Blue Yeti — press the mic again to stop");
        assert_eq!(tone, Tone::Note);
        let fallback = Recording {
            device: Some("Built-in Mic".into()),
            notice: Some("Blue Yeti isn't connected. Using Built-in Mic until it's back.".into()),
            ..active()
        };
        let (words, tone) = foot(&Some(fallback), &key, 79_000, false).unwrap();
        assert_eq!(words, "Recording · 1:09 — Blue Yeti isn't connected. Using Built-in Mic until it's back.");
        assert_eq!(tone, Tone::Warning);
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
        assert_eq!(
            on_hold_down(&key, Some("Headset Mic".into()), false),
            RecordRequest::Start { conversation: key.clone(), input: Some("Headset Mic".into()), video: false },
            "every down records with the chosen microphone"
        );
        assert_eq!(on_hold_up(&Some(active()), &key), Some(RecordRequest::Stop), "the release stops the run");
    }

    /// A tap let go before chatd says the recording runs: the stop is owed, waits out the words
    /// that are not about it, and is paid by the one that says it runs.
    #[test]
    fn a_tap_let_go_before_the_recording_runs_still_stops_it() {
        let key = target();
        assert_eq!(on_hold_up(&None, &key), None, "nothing runs yet at the release");
        // `start` clears what was on show first, so an empty word comes before the running one.
        assert_eq!(on_owed_stop(&None, &key), Owed::Wait);
        let elsewhere = Recording { conversation: Key { account: key.account.clone(), network: key.network, id: "9001".into() }, ..active() };
        assert_eq!(on_owed_stop(&Some(elsewhere), &key), Owed::Wait, "another's recording is not the one owed");
        assert_eq!(on_owed_stop(&Some(active()), &key), Owed::Stop, "the mic does not stay hot");
    }

    /// A recording that ended by itself owes no stop: asking would only end the next one.
    #[test]
    fn an_owed_stop_is_paid_by_a_recording_that_ended_by_itself() {
        let key = target();
        assert_eq!(on_owed_stop(&Some(kept("voice-1.opus")), &key), Owed::Paid);
        let failed = Recording { state: RecordState::Failed { detail: "No microphone.".into() }, ..active() };
        assert_eq!(on_owed_stop(&Some(failed), &key), Owed::Paid);
    }

    #[test]
    fn the_lab_proof_of_a_quick_tap_holds() {
        assert_eq!(prove_push_to_talk_release(), Ok(()));
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
        let file = |path: &str| OutgoingFile { path: path.into(), name: String::new(), spoiler: false, as_file: false, original: false, view_once: false, media: None };
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

    #[test]
    fn voice_and_round_video_gate_honours_per_conversation_restriction() {
        let can = Capabilities { voice_notes: true, max_file_bytes: 1_000_000, ..Capabilities::default() };
        assert!(allowed_in(&can, ""));
        assert_eq!(blocked_reason(""), None);
        let why = "Odalys Brandt doesn't accept voice and video messages.";
        assert!(!allowed_in(&can, why));
        assert_eq!(blocked_reason(why), Some(why));
        let no_voice = Capabilities { voice_notes: false, max_file_bytes: 1_000_000, ..Capabilities::default() };
        assert!(!allowed_in(&no_voice, ""));
    }

    #[test]
    fn voice_notes_available_even_when_general_file_uploads_are_disabled() {
        let teams = Capabilities { voice_notes: true, max_file_bytes: 0, ..Capabilities::default() };
        assert!(available(&teams));
        let disabled = Capabilities { voice_notes: false, max_file_bytes: 10_000, ..Capabilities::default() };
        assert!(!available(&disabled));
    }
}
