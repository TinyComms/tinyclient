//! Recording a voice message: what the shell asks, what it is told, and the frames between
//! chatd and the `mediad --rec` that does it.
//!
//! One recording at a time, across every network. chatd decodes and encodes nothing: it starts
//! a mediad in its recording mode for a file in the media cache, and the process stops when
//! chatd closes its stdin, so a finished or abandoned recording holds no device at all.

use crate::Key;
use serde::{Deserialize, Serialize};

/// What the shell shows of the recording in the composer's foot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// Where the message will go, so a recording is the composer's to show and send.
    pub conversation: Key,
    /// Unix milliseconds the recording started at, for the clock in the composer.
    pub since: u64,
    #[serde(flatten)]
    pub state: RecordState,
}

/// What the shell knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecordState {
    /// mediad is starting, or the microphone is recording.
    Active,
    /// Done: the file is in the media cache for the shell to send as a voice note.
    Recorded {
        /// The path among the cache, named in `OutgoingFile::path`.
        path: String,
        /// Milliseconds of sound, as the recorder finished the file.
        duration: u32,
        /// A voice message's loudness over time, one byte a bar, at most
        /// [`crate::WAVEFORM_LIMIT`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waveform: Vec<u8>,
    },
    /// It cannot record, in plain words.
    Failed { detail: String },
}

/// The shell to chatd: `Request::Record`. Each is answered `Ok` at once; what happens is told
/// as `Change::Recording`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum RecordRequest {
    /// Starts one recording for the conversation: chatd names the file, mediad hears the
    /// microphone. Whatever was recording is stopped.
    Start { conversation: Key },
    /// Ends the recording: the facts arrive as `Recorded`, and stay until `Forget`.
    Stop,
    /// The composer is not showing a recording: the facts and the file go.
    Forget,
}

/// `mediad --rec` to chatd, over its stdout. One event, and the process exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "recorder", rename_all = "snake_case")]
pub enum RecEvent {
    /// The file is complete. mediad exits.
    Done { duration: u32, waveform: Vec<u8> },
    /// The microphone could not be opened, or the file could not be written. mediad exits.
    Failed { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_back_as_written() {
        let shown = Recording {
            conversation: Key { network: crate::Network::Telegram, account: "1".into(), id: "7002".into() },
            since: 10_000,
            state: RecordState::Recorded { path: "/media/telegram/1/voice.opus".into(), duration: 12_000, waveform: vec![31, 0] },
        };
        let json = serde_json::to_string(&shown).unwrap();
        assert!(json.contains(r#""state":"recorded""#), "{json}");
        assert_eq!(serde_json::from_str::<Recording>(&json).unwrap(), shown);
        let failed = Recording { conversation: shown.conversation.clone(), since: 10_000, state: RecordState::Failed { detail: "No microphone.".into() } };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains(r#""state":"failed""#), "{json}");
        assert_eq!(serde_json::from_str::<Recording>(&json).unwrap(), failed);
    }
}
