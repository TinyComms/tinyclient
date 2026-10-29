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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A Tiny server's moderators muted or deafened them in this room.
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_deafened: bool,
    /// They may not speak in this room: nothing of theirs is heard.
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppressed: bool,
    /// Holding priority speech: while they talk, everyone else is quieter.
    #[serde(default, skip_serializing_if = "is_false")]
    pub priority: bool,
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
    /// How the call's media is protected, for the lock in the call's header. `None` for a call
    /// that says nothing about it (every network but a Tiny server today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<CallSecurity>,
}

/// A Tiny server call's protection (tinyserver's `docs/calls.md`, "End-to-end encryption").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSecurity {
    pub mode: SecurityMode,
    /// The call code: 30 digits from the group's epoch authenticator, the same for everyone in
    /// the call, new whenever someone joins or leaves. Empty until the group has keys.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// The group's epoch. It moves on every join and leave.
    #[serde(default)]
    pub epoch: u64,
    /// Everyone else in the encrypted group, as its members' credentials say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<SecureMember>,
    /// People in the call whose app cannot encrypt end to end: why the call is
    /// [`SecurityMode::Transport`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<String>,
    /// Only with [`SecurityMode::Transport`]: the server itself has no end-to-end encryption.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub server_lacks: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityMode {
    /// Encrypted between each app and the server (DTLS-SRTP) only. The server could hear it.
    #[default]
    Transport,
    /// End-to-end encryption is being set up. Our microphone is held back until it is.
    Pending,
    /// Only the people in the call hold the keys. The server forwards what it cannot read.
    EndToEnd,
}

/// One other person in the encrypted group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecureMember {
    pub user_id: String,
    /// Their identity key (hex), what "Mark as verified" remembers.
    pub key: String,
    /// The safety number both of us see for the pair of our keys: 45 digits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub safety: String,
    #[serde(default)]
    pub trust: Trust,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    #[default]
    Unverified,
    /// Marked verified, and they still hold that key.
    Verified,
    /// Marked verified once, and the key they hold now is not that key.
    Changed,
}

/// End-to-end encryption for a Tiny call: the room (whose id names the MLS group) and the file
/// that holds this account's identity key, made there on first use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyE2ee {
    pub room_id: String,
    pub identity: String,
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

/// What mediad needs to make an offer to a Tiny server's SFU (tinyserver's `docs/calls.md`).
/// chatd carries the offer to the server's live socket and the answer and slots back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyOffer {
    /// Our user id on the server. Our own speaking is reported under it.
    pub user_id: String,
    /// The server's host, without a scheme or port. The local address that reaches it is the
    /// one offered.
    pub host: String,
    /// Receive slots: one for each other person the room can hold.
    pub slots: u16,
    pub muted: bool,
    pub deafened: bool,
    /// The server can encrypt calls end to end (`e2ee_calls`): offer it. `None` on an older
    /// server, and the call is encrypted to the server only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2ee: Option<TinyE2ee>,
}

/// Whose voice one receive slot carries, as the server's `call.slots` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinySlot {
    pub mid: String,
    pub user_id: String,
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
    /// Gains, the speech gate and which cues play (Settings › Sounds and Audio & video). Sent
    /// at start and whenever one changes.
    Tune(Tuning),
    /// Plays one cue now, on the call's speaker, under the voices. Outside a call (a test
    /// mediad) it opens the speaker for the cue alone.
    Cue { cue: Cue },
    /// A Tiny server call: make an offer. Answered `Offer`. Sent again for a call that is
    /// already up (the server restarted), it is a fresh offer, and the call carries on with it
    /// once its answer arrives.
    TinyOffer(TinyOffer),
    /// The server's answer to the last offer. Answered `Done`; then `Connected` once media
    /// flows.
    TinyAnswer { sdp: String },
    /// The server's whole map of receive slots, whenever it changes.
    TinySlots { audio: Vec<TinySlot> },
    /// One `call.e2ee` frame from the server, as its JSON text, in the order it arrived.
    TinyE2ee { frame: String },
    /// Everyone in the room's call, us included, whenever it changes: whom the MLS group may
    /// hold.
    TinyRoster { user_ids: Vec<String> },

    // What the shell and the room decide about the sound, for mediad to apply. Each is answered
    // `Done`, and each holds until sent again.
    /// One participant's voice for us alone: `percent` from 0 to [`GAIN_MAX`] (100 is as
    /// decoded), and `muted` drops it entirely. Sent for each participant whose choice is not
    /// the default; a participant never named is at 100 and heard.
    Gain { user_id: String, percent: u16, muted: bool },
    /// How our microphone opens: by voice activation, or only while push-to-talk is held.
    Input(InputMode),
    /// The push-to-talk key (`priority` false) or the priority key (`priority` true) is down
    /// or up; each is tracked on its own, so letting go of one while the other is held keeps
    /// the microphone open. In voice activation the plain key does nothing; the priority key
    /// opens the microphone in either mode, since speaking over everyone is its point.
    Talk { held: bool, priority: bool },
    /// Who in the call holds priority speech now. While any of them is speaking, everyone else
    /// is played [`PRIORITY_DUCK_DB`] quieter.
    Priority { user_ids: Vec<String> },
    /// Echo cancellation, noise suppression and automatic gain on the microphone.
    Processing(AudioProcessing),
    /// What our voice is encoded at, in kb/s (a Tiny room's bitrate, 16 to 128).
    Bitrate { kbps: u32 },
    /// How the connection is doing. Answered `Stats`, or `Refused` before media flows.
    Stats,
    /// Settings' microphone test plays the microphone back on the speaker, through the same
    /// processing a call uses. Only in `mediad test`; a call refuses it.
    Loopback { on: bool },
}

/// The loudest one participant may be set, in percent.
pub const GAIN_MAX: u16 = 200;
/// How much quieter everyone else is while a priority speaker talks.
pub const PRIORITY_DUCK_DB: u8 = 12;

/// How the microphone opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum InputMode {
    /// Open while our level is at `threshold` (0 to 100, as `Tuning::gate` and the meter) or
    /// above.
    Voice { threshold: u8 },
    /// Open only while the key is held, and `release_ms` after it is let go, so the last word
    /// is not cut.
    PushToTalk { release_ms: u16 },
}

impl Default for InputMode {
    fn default() -> InputMode {
        InputMode::Voice { threshold: Tuning::default().gate }
    }
}

/// The microphone's processing. All on unless the user turned one off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioProcessing {
    pub echo_cancellation: bool,
    pub noise_suppression: bool,
    pub auto_gain: bool,
}

impl Default for AudioProcessing {
    fn default() -> AudioProcessing {
        AudioProcessing { echo_cancellation: true, noise_suppression: true, auto_gain: true }
    }
}

/// How the media gets to the server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPath {
    #[default]
    Unknown,
    /// Straight over UDP: the best there is.
    Udp,
    /// Over TCP (ICE-TCP), when UDP is blocked.
    Tcp,
    /// Through a relay.
    Relay,
}

/// The connection as it is now, over the last few seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStats {
    /// Round trip, in milliseconds. 0 until measured.
    #[serde(default)]
    pub rtt_ms: u32,
    #[serde(default)]
    pub jitter_ms: u32,
    /// Packets lost of every thousand received.
    #[serde(default)]
    pub loss_permille: u16,
    #[serde(default)]
    pub send_kbps: u32,
    #[serde(default)]
    pub receive_kbps: u32,
    #[serde(default)]
    pub path: CallPath,
    /// As `opus`. Empty when not known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub codec: String,
}

/// What the user chose for a call's sound. Percentages, 0 to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuning {
    /// Everyone's voice in the mix. 100 is the level as decoded.
    pub voice: u8,
    /// Our microphone counts as speech from this level (`audio::level`) up.
    pub gate: u8,
    /// The cues' volume. At 100 a cue peaks at -20 dBFS.
    pub cues: u8,
    /// Someone joins or leaves (S7).
    pub joins: bool,
    /// I mute, unmute, deafen or undeafen (S8).
    pub mine: bool,
    /// The connection is lost or back (S9).
    pub connection: bool,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning { voice: 100, gate: 25, cues: 50, joins: true, mine: true, connection: true }
    }
}

/// The short sounds mediad makes inside a call (`docs/design/09-settings.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cue {
    Joined,
    Left,
    Muted,
    Unmuted,
    Deafened,
    Undeafened,
    Lost,
    Back,
    /// A moderator moved us to another room (S7, with joins and leaves).
    Moved,
    /// We were put out of the call: kicked, banned, timed out, or the room went (S9).
    Disconnected,
    /// "Play test sound": the join cue, whatever the switches say.
    Test,
}

/// mediad to chatd, over mediad's stdout. `Ready` is sent once at start. `Streams`, `Done` and
/// `Refused` answer a request; everything else is an event and may arrive at any time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "media", rename_all = "snake_case")]
pub enum MediaResponse {
    Ready { engine_linked: bool },
    Streams { video: Vec<VideoStream> },
    Done,
    /// The SDP offer for a Tiny server call, in answer to `TinyOffer`.
    Offer { sdp: String },
    /// The connection as it is now, in answer to `Stats`.
    Stats(CallStats),
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
    /// A `call.e2ee` frame for the server, as its JSON text.
    TinyE2ee { frame: String },
    /// How the Tiny call is protected now. Every member's `trust` is `Unverified`: chatd knows
    /// whom the user verified.
    TinySecurity(CallSecurity),
}

impl MediaResponse {
    /// True for the frames that answer a request.
    pub fn is_reply(&self) -> bool {
        matches!(
            self,
            MediaResponse::Streams { .. }
                | MediaResponse::Done
                | MediaResponse::Offer { .. }
                | MediaResponse::Stats(_)
                | MediaResponse::Refused { .. }
        )
    }
}
