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
    /// Our own camera.
    Camera,
}

/// What the one received picture is, as `call.slots` `video.kind` says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PictureKind {
    #[default]
    Camera,
    Share,
}

/// How much of the screen a share spends: its size, its frame rate and its bitrate. The sharer
/// picks it; a watcher gets the one layer the sharer sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareQuality {
    /// The screen as it is, up to 2560×1440, at its own refresh rate up to 60 frames a second.
    #[default]
    Best,
    /// 1080p at 30 frames a second.
    Balanced,
    /// 720p at 15 frames a second: what every share was before there was a choice.
    Low,
}

/// The picture on screen: a JPEG written by mediad, and removed once the next one is written.
/// The window reads it into an image of its own (`gui/tiny/feed.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallPicture {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
    pub user_id: String,
    pub kind: PictureKind,
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
    /// Their camera is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub camera: bool,
    /// They hold the room's share.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sharing: bool,
}

/// The most screens and windows `mediad --screens` lists.
pub const SCREENS_LIMIT: usize = 24;

/// One screen or window a share can send, as `mediad --screens` lists it for the share picker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSource {
    /// What [`MediaRequest::ShareSend`] names it by: `display:<device>` or `window:<handle>`.
    pub id: String,
    /// "Screen 1", or the window's title.
    pub name: String,
    /// A window, not a whole screen.
    #[serde(default, skip_serializing_if = "is_false")]
    pub window: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub primary: bool,
    /// A small picture of it, as a file, when one could be taken.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thumbnail: String,
}

/// One camera the system lists, for Settings › Audio & video.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraSource {
    /// The system's id for it, which `settings::CAMERA` stores with the name.
    pub id: String,
    pub name: String,
    /// A small picture from it, as a file, when one could be taken.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thumbnail: String,
}

/// One microphone or speaker the system lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// The system's id for it: a WASAPI endpoint id, a CoreAudio UID, an ALSA name. What a
    /// choice (`settings::DeviceChoice`) finds it by first.
    pub id: String,
    /// As the system names it, which on Windows includes the driver in brackets.
    pub name: String,
    /// Software, not hardware (Krisp, VB-Cable, Voicemeeter, NVIDIA Broadcast, ...): never
    /// used unless picked.
    #[serde(default, rename = "virtual", skip_serializing_if = "is_false")]
    pub is_virtual: bool,
    /// The system default now.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default: bool,
    /// What a call uses when nothing was chosen (`DeviceChoice::Auto`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto: bool,
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
    /// The system's ids for them (`AudioDevice::id`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_id: String,
    /// In words, why a device in use is not the one chosen, or that one is not working: shown
    /// until it is put right. `None` while everything is as chosen and running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_notice: Option<String>,
    /// A device stays silent after it was rebuilt: the notice asks the user to switch device.
    #[serde(default, skip_serializing_if = "is_false")]
    pub device_stuck: bool,
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
    /// The level of what our speaker plays, 0 to 100, on the same terms as `level`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_level: Option<u8>,
    /// Every microphone and speaker the system lists, at most [`DEVICES_LIMIT`] of each. Empty
    /// until mediad says.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<AudioDevice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<AudioDevice>,
    /// How the call's media is protected, for the lock in the call's header. `None` for a call
    /// that says nothing about it (every network but a Tiny server today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<CallSecurity>,
    /// The one picture being watched. `None` while nothing is on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<CallPicture>,
    /// My own camera as it is sent, for my tile. `None` while it is off. Its `user_id` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_view: Option<CallPicture>,
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
    /// This call is never carried on unencrypted: the person chose so, the server is an Incognito
    /// host, or the room only takes end-to-end encrypted calls. A [`SecurityMode::Blocked`] call
    /// then offers no way on but leaving.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub required: bool,
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
    /// The server said this call is to be transport only (`call.e2ee` `off`). That frame is the
    /// server's word alone, so it is never obeyed silently: nothing is sent or played until the
    /// person chooses to carry on unencrypted ([`MediaRequest::TinyTransport`]), which a
    /// [`CallSecurity::required`] call does not allow. It is not [`SecurityMode::Transport`]:
    /// that is a call carrying on unencrypted.
    Blocked,
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
    /// The call is never carried on unencrypted: if the server turns it to transport only, it is
    /// blocked ([`SecurityMode::Blocked`]) and the person cannot choose to carry on. Set by
    /// chatd for the person's setting, an Incognito host, or a room that requires it.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub required: bool,
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
    /// A Go Live connection, not the call's own voice. Its loop sends pictures and no audio.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub stream: bool,
    /// This Go Live connection only receives. It sends no picture of its own.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub watch: bool,
    /// The MLS group for a stream, the stream server id minus one. Empty on the voice
    /// connection, which uses the channel id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group_id: String,
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

/// Who the one video slot carries, as `call.slots` `video` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyPicture {
    pub mid: String,
    pub user_id: String,
    #[serde(default)]
    pub kind: PictureKind,
}

/// chatd to mediad, over mediad's stdin. Closing stdin ends the call. Each request gets exactly
/// one reply (`Streams`, `Done` or `Refused`), in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "media", rename_all = "snake_case")]
pub enum MediaRequest {
    Streams,
    /// Send our screen, or stop sending it.
    ShareSend {
        on: bool,
        /// Which screen or window, as `mediad --screens` lists it ([`ScreenSource::id`]).
        /// `None` is the primary screen, as older shells asked for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
        /// How much to spend on it. `None` is [`ShareQuality::Best`], as older shells asked for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<ShareQuality>,
    },
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
    /// A Go Live connection beside the voice call. Answered `Done`. The leg returns on its
    /// own and does not end the process.
    Stream(VoiceJoin),
    /// Stop one Go Live leg. `watch` is the receive-only connection. Answered `Done`.
    StreamStop { watch: bool },
    /// The device menu opened or closed. While on, `Levels` are wanted and `DeviceList` is sent
    /// once at once and again whenever the system's devices change.
    Meter { on: bool },
    /// Muted: capture nothing and send nothing. Deafened: play nothing.
    Audio { muted: bool, deafened: bool },
    /// The microphone and speaker to use, as V1 and V2 store them (`settings::DeviceChoice`):
    /// `None` is the automatic choice. Sent at start and whenever either setting changes. The
    /// camera the same way (V22); `None` is the first the system lists.
    Devices {
        input: Option<String>,
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        camera: Option<String>,
    },
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
    /// The answer to a renegotiation, or an empty SDP when the server refused it (`share_taken`).
    TinyRenegotiation { sdp: String },
    /// Our camera, on or off. A Tiny room and a Discord call.
    Camera { on: bool },
    /// Who the one video slot carries now. `None` when the server sends `video: null`.
    TinyPicture { picture: Option<TinyPicture> },
    /// One `call.e2ee` frame from the server, as its JSON text, in the order it arrived.
    TinyE2ee { frame: String },
    /// Everyone in the room's call, us included, whenever it changes: whom the MLS group may
    /// hold.
    TinyRoster { user_ids: Vec<String> },
    /// The person chose to carry on without end-to-end encryption after the server turned it
    /// off ([`SecurityMode::Blocked`]). Answered `Done`, or `Refused` in words when the call is
    /// not blocked or may not carry on unencrypted ([`CallSecurity::required`]).
    TinyTransport,

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
    /// How clearly to encode within that bitrate (Tiny calls).
    Quality(VoiceQuality),
    /// How the connection is doing. Answered `Stats`, or `Refused` before media flows.
    Stats,
    /// Settings' microphone test plays the microphone back on the speaker, through the same
    /// processing a call uses. Only in `mediad test`; a call refuses it.
    Loopback { on: bool },
    /// Write mediad's debug ring. Answered `LogsSaved`.
    SaveLogs,
    /// The log ceiling changed in chatd's settings (Verbose logging): `error`, `warn`, `info`,
    /// `debug` or `trace`, as `settings::LOG_LEVEL` holds it. Answered `Done`; an unknown word
    /// changes nothing.
    LogLevel { level: String },
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

/// How noise is taken out of the microphone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    /// Nothing: fans, typing, traffic and other voices go through as captured.
    Off,
    /// WebRTC noise suppression at a strong everyday level. Other voices stay.
    Standard,
    /// WebRTC noise suppression at its strongest. Other voices stay.
    High,
    /// Voice isolation: other voices and cries are turned down, and so is steady noise.
    /// This is the default. The voice is band-limited to wideband (16 kHz processing).
    #[default]
    Enhanced,
}

impl NoiseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NoiseMode::Off => "off",
            NoiseMode::Standard => "standard",
            NoiseMode::High => "high",
            NoiseMode::Enhanced => "enhanced",
        }
    }

    pub fn parse(value: &str) -> Option<NoiseMode> {
        Some(match value {
            "off" => NoiseMode::Off,
            "standard" => NoiseMode::Standard,
            "high" => NoiseMode::High,
            // Older builds stored a switch: "on" meant noise suppression was enabled.
            // That choice now follows Enhanced, the mode that turns other voices down.
            "on" | "enhanced" => NoiseMode::Enhanced,
            _ => return None,
        })
    }
}

/// How clearly our voice is encoded, within a Tiny room's bitrate ceiling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceQuality {
    /// Save bandwidth: about 32 kb/s.
    DataSaver,
    /// The room's bitrate as set (default).
    #[default]
    Standard,
    /// Prefer the top of the room's range; clearer Opus Audio mode.
    High,
}

impl VoiceQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            VoiceQuality::DataSaver => "data_saver",
            VoiceQuality::Standard => "standard",
            VoiceQuality::High => "high",
        }
    }

    pub fn parse(value: &str) -> Option<VoiceQuality> {
        Some(match value {
            "data_saver" => VoiceQuality::DataSaver,
            "standard" => VoiceQuality::Standard,
            "high" => VoiceQuality::High,
            _ => return None,
        })
    }

    /// The encode target in kb/s for a Tiny room whose ceiling is `room_kbps` (16 to 128).
    /// Data saver caps at 32; High uses the room's full rate (admins set 96–128 for clarity).
    pub fn apply_room(self, room_kbps: u32) -> u32 {
        let room = room_kbps.clamp(16, 128);
        match self {
            VoiceQuality::DataSaver => room.min(32),
            VoiceQuality::Standard | VoiceQuality::High => room,
        }
    }

    /// Whether the encoder should prefer Opus's Audio application (clearer than Voip).
    pub fn prefer_audio_application(self) -> bool {
        matches!(self, VoiceQuality::High)
    }
}

#[cfg(test)]
mod quality_tests {
    use super::*;

    #[test]
    fn voice_quality_caps_data_saver_and_keeps_room_for_high() {
        assert_eq!(VoiceQuality::DataSaver.apply_room(128), 32);
        assert_eq!(VoiceQuality::DataSaver.apply_room(24), 24);
        assert_eq!(VoiceQuality::Standard.apply_room(64), 64);
        assert_eq!(VoiceQuality::High.apply_room(128), 128);
        assert!(VoiceQuality::High.prefer_audio_application());
        assert!(!VoiceQuality::Standard.prefer_audio_application());
    }

    #[test]
    fn noise_mode_reads_legacy_on_off() {
        assert_eq!(NoiseMode::parse("on"), Some(NoiseMode::Enhanced));
        assert_eq!(NoiseMode::parse("off"), Some(NoiseMode::Off));
        assert_eq!(NoiseMode::parse("standard"), Some(NoiseMode::Standard));
        assert_eq!(NoiseMode::parse("enhanced"), Some(NoiseMode::Enhanced));
    }
}

/// The microphone's processing. Echo and gain are on unless turned off; noise has modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioProcessing {
    pub echo_cancellation: bool,
    pub auto_gain: bool,
    #[serde(default)]
    pub noise: NoiseMode,
}

impl Default for AudioProcessing {
    fn default() -> AudioProcessing {
        AudioProcessing { echo_cancellation: true, auto_gain: true, noise: NoiseMode::Enhanced }
    }
}

impl AudioProcessing {
    /// Whether any noise suppression runs.
    pub fn noise_suppression(self) -> bool {
        self.noise != NoiseMode::Off
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
    /// Video transport output, received access units and completed render images, over the last
    /// interval. No capture-to-display latency is inferred from these counters or RTT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoStats>,
    /// Round trip, in milliseconds. 0 until measured.
    #[serde(default)]
    pub rtt_ms: u32,
    /// How unevenly packets travel. On a Tiny call, our own path's.
    #[serde(default)]
    pub jitter_ms: u32,
    /// Of every thousand packets, those lost and not repaired. On a Tiny call, on our own path.
    #[serde(default)]
    pub loss_permille: u16,
    /// Something flowed in the last second, so `jitter_ms` and `loss_permille` mean something.
    /// While it did not they read 0, which says nothing about the link.
    #[serde(default)]
    pub link_measured: bool,
    /// Of every thousand frames expected from the others, those that did not come, on anyone's
    /// path. The jitter buffers count these, so a sender's own trouble is in here.
    #[serde(default)]
    pub voice_loss_permille: u16,
    #[serde(default)]
    pub send_kbps: u32,
    #[serde(default)]
    pub receive_kbps: u32,
    #[serde(default)]
    pub path: CallPath,
    /// As `opus`. Empty when not known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub codec: String,
    /// The call's audio health, counted over the whole call rather than the last second, so the
    /// UI can tell a clean call from one that glitched. Glitches the system reported on the
    /// microphone or the speaker (a buffer underrun or overrun on the device).
    #[serde(default)]
    pub device_glitches: u32,
    /// Microphone audio dropped because the call fell too far behind it.
    #[serde(default)]
    pub capture_overruns: u32,
    /// 20 ms ticks that found no microphone audio: a hole in what was sent.
    #[serde(default)]
    pub capture_gaps: u32,
    /// The speaker ran dry and had to refill: a gap in what was heard.
    #[serde(default)]
    pub playout_underruns: u32,
    /// Lost frames from others that were made up (packet loss concealment).
    #[serde(default)]
    pub concealed_frames: u32,
    /// Lost frames from others rebuilt from the copy in the next packet (Opus in-band FEC).
    #[serde(default)]
    pub fec_recovered: u32,
    /// Packets from others that came after their turn to play had gone.
    #[serde(default)]
    pub late_packets: u32,
    /// Times someone's voice stalled in mid-speech and went on when its packet came.
    #[serde(default)]
    pub playout_gaps: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoStats {
    /// A remote video slot is selected and being watched.
    pub receive_expected: bool,
    /// Time this receive slot has been watched, so startup is not called a stall.
    pub watching_ms: u32,
    /// Video RTP bytes accepted by the transport after pacing, including headers and RTX.
    /// TCP may still buffer them. Padding is excluded.
    pub send_kbps: u32,
    pub receive_kbps: u32,
    /// Original frame end markers emitted after pacing, combined across all video tracks.
    pub send_fps: u32,
    pub receive_fps: u32,
    /// Images completed for the GUI, rather than refreshes of the physical display.
    pub rendered_fps: u32,
    pub width: u32,
    pub height: u32,
    /// Time since the last completed image. None until an image exists.
    pub frame_age_ms: Option<u32>,
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
    /// Our microphone's soft gain before processing. 100 is as captured; 0 is silence.
    #[serde(default = "tuning_input_default")]
    pub input: u8,
    /// Someone joins or leaves (S7).
    pub joins: bool,
    /// I mute, unmute, deafen or undeafen (S8).
    pub mine: bool,
    /// The connection is lost or back (S9).
    pub connection: bool,
}

fn tuning_input_default() -> u8 {
    100
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning { voice: 100, gate: 25, cues: 50, input: 100, joins: true, mine: true, connection: true }
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
    /// The bundle mediad just wrote, or empty when export is off.
    LogsSaved { path: String },

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
    /// The devices in use, at start and whenever they change: a pick, a device coming or going,
    /// the system default moving, a device that broke and was opened again.
    Devices {
        input: String,
        output: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        input_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output_id: String,
        /// In words: a device in use that stands in for the one chosen, or one that is not
        /// working. `None` when all is as chosen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notice: Option<String>,
        /// A device stayed silent after it was rebuilt: the user should switch device.
        #[serde(default, skip_serializing_if = "is_false")]
        stuck: bool,
    },
    /// Every device the system lists. Sent at start, when `Meter` turns on, and whenever one is
    /// plugged in or removed, or the default moves.
    DeviceList { inputs: Vec<AudioDevice>, outputs: Vec<AudioDevice> },
    /// Media stopped and mediad is reconnecting on its own.
    Interrupted,
    /// The call cannot go on. mediad exits after sending this.
    Failed { reason: CallFailure, detail: String },
    /// A `call.e2ee` frame for the server, as its JSON text.
    TinyE2ee { frame: String },
    /// How the Tiny call is protected now. Every member's `trust` is `Unverified`: chatd knows
    /// whom the user verified.
    TinySecurity(CallSecurity),
    /// A renegotiation offer for `call.offer`. Not a reply: it arrives while the call is up.
    TinyRenegotiate { sdp: String },
    /// The picture to draw, or an empty path when it should come off the screen.
    Picture {
        path: String,
        width: u32,
        height: u32,
        generation: u64,
        user_id: String,
        kind: PictureKind,
    },
    /// What our camera and share are actually sending, after an answer or a refusal.
    Sending { share: bool, camera: bool },
    /// Our own camera as it is being sent, a few times a second, so the person sees what
    /// everyone else sees; an empty path when the camera went off.
    SelfView { path: String, width: u32, height: u32, generation: u64 },
    /// A picture that could not be captured, in words. The voice call stays.
    Notice { detail: String },
}

impl MediaResponse {
    /// The enum name only. Logs use this so a frame's SDP and paths stay out of the line.
    pub fn name(&self) -> &'static str {
        match self {
            MediaResponse::Ready { .. } => "Ready",
            MediaResponse::Streams { .. } => "Streams",
            MediaResponse::Done => "Done",
            MediaResponse::Offer { .. } => "Offer",
            MediaResponse::Stats(_) => "Stats",
            MediaResponse::Refused { .. } => "Refused",
            MediaResponse::LogsSaved { .. } => "LogsSaved",
            MediaResponse::Connected { .. } => "Connected",
            MediaResponse::Source { .. } => "Source",
            MediaResponse::Speaking { .. } => "Speaking",
            MediaResponse::Gone { .. } => "Gone",
            MediaResponse::Levels { .. } => "Levels",
            MediaResponse::Devices { .. } => "Devices",
            MediaResponse::DeviceList { .. } => "DeviceList",
            MediaResponse::Interrupted => "Interrupted",
            MediaResponse::Failed { .. } => "Failed",
            MediaResponse::TinyE2ee { .. } => "TinyE2ee",
            MediaResponse::TinySecurity(_) => "TinySecurity",
            MediaResponse::TinyRenegotiate { .. } => "TinyRenegotiate",
            MediaResponse::Picture { .. } => "Picture",
            MediaResponse::Sending { .. } => "Sending",
            MediaResponse::SelfView { .. } => "SelfView",
            MediaResponse::Notice { .. } => "Notice",
        }
    }

    /// True for the frames that answer a request.
    pub fn is_reply(&self) -> bool {
        matches!(
            self,
            MediaResponse::Streams { .. }
                | MediaResponse::Done
                | MediaResponse::Offer { .. }
                | MediaResponse::Stats(_)
                | MediaResponse::Refused { .. }
                | MediaResponse::LogsSaved { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requirement_is_sent_only_when_it_is_made_and_read_as_off_when_absent() {
        // An older chatd or mediad does not send it: that is "not required", never an error.
        let older: TinyE2ee = serde_json::from_str(r#"{"room_id":"r","identity":"/keys/ada"}"#).unwrap();
        assert!(!older.required);
        // Not required stays off the wire, so a frame is the one an older build already reads.
        let off = TinyE2ee { room_id: "r".into(), identity: "/keys/ada".into(), required: false };
        assert_eq!(serde_json::to_string(&off).unwrap(), r#"{"room_id":"r","identity":"/keys/ada"}"#);
        let on = TinyE2ee { required: true, ..off };
        let json = serde_json::to_string(&on).unwrap();
        assert_eq!(json, r#"{"room_id":"r","identity":"/keys/ada","required":true}"#);
        assert_eq!(serde_json::from_str::<TinyE2ee>(&json).unwrap(), on);
    }

    #[test]
    fn a_share_without_a_screen_is_the_frame_an_older_build_sends() {
        let older: MediaRequest = serde_json::from_str(r#"{"media":"share_send","on":true}"#).unwrap();
        assert_eq!(older, MediaRequest::ShareSend { on: true, screen: None, quality: None });
        assert_eq!(serde_json::to_string(&older).unwrap(), r#"{"media":"share_send","on":true}"#);
        let chosen = MediaRequest::ShareSend { on: true, screen: Some("display:\\\\.\\DISPLAY2".into()), quality: Some(ShareQuality::Balanced) };
        let json = serde_json::to_string(&chosen).unwrap();
        assert!(json.contains(r#""quality":"balanced""#));
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), chosen);
    }

    #[test]
    fn a_blocked_call_round_trips_and_is_not_transport() {
        let security = CallSecurity { mode: SecurityMode::Blocked, unsupported: vec!["bea".into()], ..CallSecurity::default() };
        let json = serde_json::to_string(&security).unwrap();
        assert!(json.contains(r#""mode":"blocked""#), "{json}");
        assert_eq!(serde_json::from_str::<CallSecurity>(&json).unwrap(), security);
        assert_ne!(SecurityMode::Blocked, SecurityMode::Transport);
        // The default is still the one that says nothing more than holds.
        assert_eq!(SecurityMode::default(), SecurityMode::Transport);
    }
}
