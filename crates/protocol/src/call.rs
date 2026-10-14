//! Calls: what the shell is told about one, and the frames between chatd and mediad.
//!
//! chatd does signalling only (the network's own socket). mediad carries every byte of media and
//! exists from `Connecting` until the call is over, never during a ring.

use crate::{is_false, Key, Network};
use serde::{Deserialize, Serialize};

/// Most device names of one kind a call lists.
pub const DEVICES_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Encode,
    Decode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Content {
    /// The one screen share being sent or watched.
    Share,
    /// The active speaker's camera.
    Speaker,
}

/// A video stream mediad holds a codec session for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoStream {
    pub direction: Direction,
    pub content: Content,
    /// Who the stream belongs to. Empty for our own encode.
    pub source: String,
}

/// Why a call is not happening, for the one line the dock shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallFailure {
    /// Nobody answered, or they declined.
    NoAnswer,
    /// The network refused the voice connection.
    Refused,
    /// The network requires a newer encryption than this build speaks (Discord close code 4017).
    Encryption,
    /// No reply in time, or the packets stopped and did not come back.
    Lost,
    /// The microphone or speaker could not be opened, or permission was denied.
    Audio,
    /// mediad could not start, or died.
    Engine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CallState {
    /// We are calling and nobody has answered. It stays this, whatever our own media is doing,
    /// until the adapter says the other side picked up.
    RingingOut,
    /// Someone is calling us. mediad is not running.
    RingingIn { from: String },
    /// mediad is up and joining. Not yet a call: nobody can hear anybody.
    Connecting,
    /// The network confirmed the join and media flows. The only state drawn as "in a call".
    Joined,
    /// Media stopped; mediad is trying to get it back.
    Reconnecting,
    Failed { reason: CallFailure, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// The person's id on the call's account.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub speaking: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub conversation: Key,
    #[serde(flatten)]
    pub state: CallState,
    /// Everyone in the call but us.
    pub participants: Vec<Participant>,
    pub muted: bool,
    pub deafened: bool,
    /// Unix seconds the call was joined. 0 before that.
    pub joined: i64,
    /// The microphone and speaker in use, as the system names them. Empty until mediad says.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// False until mediad links a real media engine. The lifecycle runs either way.
    pub engine_linked: bool,
    pub video: Vec<VideoStream>,
    /// We are heard right now: our microphone is open and carrying speech.
    #[serde(default, skip_serializing_if = "is_false")]
    pub speaking: bool,
    /// Our microphone's level, 0 to 100. Only while the shell asked for it with
    /// `MediaRequest::Meter` (the device menu is open); `None` the rest of the time, so a call
    /// nobody is adjusting sends no changes for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Every microphone and speaker the system lists, at most [`DEVICES_LIMIT`] of each. Empty
    /// until mediad says.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
}

/// What Discord's end-to-end encryption needs from mediad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dave {
    /// The highest protocol version to offer in the voice identify. 0 offers none.
    pub max_version: u16,
    /// The call cannot proceed unencrypted: fail with `Encryption` rather than fall back.
    pub required: bool,
}

/// Everything mediad needs to join one Discord voice connection. The adapter gathers it from the
/// gateway (`VOICE_STATE_UPDATE` gives the session id, `VOICE_SERVER_UPDATE` the rest) and chatd
/// passes it on untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceJoin {
    pub network: Network,
    /// Host and port of the voice gateway, without a scheme.
    pub endpoint: String,
    pub token: String,
    pub session_id: String,
    pub user_id: String,
    /// The voice gateway's `server_id`: the guild, or the channel for a direct call.
    pub server_id: String,
    pub channel_id: String,
    pub dave: Dave,
    pub muted: bool,
    pub deafened: bool,
}

/// chatd to mediad, over mediad's stdin. Closing stdin ends the call. Each request gets exactly
/// one reply (`Streams`, `Done` or `Refused`), in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "media", rename_all = "snake_case")]
pub enum MediaRequest {
    Streams,
    /// Send our screen, or stop sending it.
    ShareSend { on: bool },
    /// Watch one participant's share. Replaces any share already being watched.
    ShareWatch { source: Option<String> },
    /// The active speaker changed. Replaces the previous speaker's decode.
    Speaker { source: Option<String> },
    /// Connect the voice session. Progress and the outcome arrive as events.
    Join(VoiceJoin),
    /// The network moved a call that is already joined (a new voice server, a new session):
    /// drop the old connection and connect this one, keeping devices, mute and the call itself.
    /// Answered `Done`; then `Interrupted` and `Connected` as for any reconnect.
    Rejoin(VoiceJoin),
    /// The device menu opened or closed. While on, `Levels` are wanted and `DeviceList` is sent
    /// once at once and again whenever the system's devices change.
    Meter { on: bool },
    /// Muted: capture nothing and send nothing. Deafened: play nothing.
    Audio { muted: bool, deafened: bool },
    /// Device names as the system lists them. `None` follows the system default.
    Devices { input: Option<String>, output: Option<String> },
}

/// mediad to chatd, over mediad's stdout. `Ready` is sent once at start. `Streams`, `Done` and
/// `Refused` answer a request; everything else is an event and may arrive at any time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "media", rename_all = "snake_case")]
pub enum MediaResponse {
    Ready { engine_linked: bool },
    Streams { video: Vec<VideoStream> },
    Done,
    /// This mediad cannot do what was asked.
    Refused { detail: String },

    /// Voice is up both ways: the network accepted us and the transport is encrypted.
    Connected {
        /// Our own audio SSRC.
        ssrc: u32,
        /// The end-to-end protocol version in use. 0 is none.
        dave_version: u16,
    },
    /// Whose audio an SSRC carries, as the voice gateway announced it.
    Source { user_id: String, ssrc: u32 },
    /// Someone is heard, or stopped. Our own `user_id` (the one in `VoiceJoin`) reports us.
    Speaking { user_id: String, speaking: bool },
    /// The voice gateway said this user left the connection.
    Gone { user_id: String },
    /// Our microphone's level and the mix being played, 0 to 100. At most ten a second, only
    /// while they change, and only while `Meter` is on.
    Levels { input: u8, output: u8 },
    /// The devices in use, at start and whenever the system moves them.
    Devices { input: String, output: String },
    /// Every device the system lists, by the names `MediaRequest::Devices` takes. Sent at start,
    /// when `Meter` turns on, and whenever one is plugged in or removed.
    DeviceList { inputs: Vec<String>, outputs: Vec<String> },
    /// Media stopped and mediad is reconnecting on its own.
    Interrupted,
    /// The call cannot go on. mediad exits after sending this.
    Failed { reason: CallFailure, detail: String },
}

impl MediaResponse {
    /// True for the frames that answer a request.
    pub fn is_reply(&self) -> bool {
        matches!(
            self,
            MediaResponse::Streams { .. } | MediaResponse::Done | MediaResponse::Refused { .. }
        )
    }
}
