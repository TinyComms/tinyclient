//! Calls: what the shell is told about one, and the frames between chatd and mediad.
//!
//! chatd does signalling only (the network's own socket). mediad carries every byte of media and
//! exists from `Connecting` until the call is over, never during a ring.

use crate::{is_false, Key, Network};
use serde::{Deserialize, Serialize};

/// The settings Discord returns when a voice channel editor is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelVoice {
    pub bitrate: u32,
    pub user_limit: u32,
    pub rtc_region: Option<String>,
    pub video_quality_mode: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceRegion {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordVoiceSettings {
    pub channel: Option<DiscordChannelVoice>,
    pub max_bitrate: u32,
    pub regions: Vec<DiscordVoiceRegion>,
    pub current_region: Option<String>,
}

/// One entry in a network's recent calls list (`Request::CallHistory`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallLogEntry {
    /// The conversation this call belongs to (to open at `message_id` or call back).
    pub conversation: Key,
    /// The other person's id on this account, if 1:1.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub person_id: String,
    /// Display name of the person or group.
    pub name: String,
    /// The message id of the call service message (for jumping to it or deleting it).
    pub message_id: String,
    /// True when we placed the call.
    #[serde(default, skip_serializing_if = "is_false")]
    pub outgoing: bool,
    /// Structured call metadata (outcome, caller_id, seconds, video) shared with call rows.
    pub note: crate::CallNote,
    /// Unix seconds when the call happened.
    pub time: i64,
}

/// A voice message in the signed-in account's mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voicemail {
    pub id: String,
    pub caller: String,
    pub time: i64,
    pub transcript: String,
    pub read: bool,
    pub audio: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoicemailAction {
    MarkRead,
    Delete,
}

/// The network asked for a quality rating after a call ended (Telegram `phoneCallDiscarded.need_rating`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRatingPrompt {
    /// The conversation the call belonged to.
    pub conversation: Key,
    /// Network-specific call identifier passed back to `Request::CallRate`.
    pub call_id: String,
    /// True when the call carried video.
    #[serde(default, skip_serializing_if = "is_false")]
    pub video: bool,
    /// Duration of the call in seconds, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u32>,
}

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// How much of the camera a call spends: its top resolution, frame rate, and bitrate ceiling.
/// `Auto` picks HD (1280×720 at 30 fps) when a hardware encoder is available and 640×360 at 15 fps
/// on software; `DataSaver` caps at 640×360 at 15 fps / 350 kb/s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraQuality {
    #[default]
    Auto,
    Hd,
    Standard,
    DataSaver,
}

impl CameraQuality {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "auto" => Some(Self::Auto),
            "hd" => Some(Self::Hd),
            "standard" => Some(Self::Standard),
            "data_saver" => Some(Self::DataSaver),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hd => "hd",
            Self::Standard => "standard",
            Self::DataSaver => "data_saver",
        }
    }
}

/// Camera capture, quality, and self-view preferences applied in mediad (`MediaRequest::CameraSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraSettings {
    #[serde(default)]
    pub quality: CameraQuality,
    /// Mirror the local self-view horizontally. Never alters what remote peers receive.
    #[serde(default = "crate::yes")]
    pub mirror: bool,
    /// Lightweight on-device background blur before encoding.
    #[serde(default)]
    pub blur: bool,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self { quality: CameraQuality::Auto, mirror: true, blur: false }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<crate::MediaFile>,
    #[serde(default)]
    pub speaking: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
    /// A moderator muted or deafened them in this channel or room.
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_deafened: bool,
    /// They may not speak in this channel or room: nothing of theirs is heard.
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
    /// Being rung for this call and not yet joined (Discord DM / group DM calls, Signal group calls).
    #[serde(default, skip_serializing_if = "is_false")]
    pub ringing: bool,
    /// A short preview of their live stream, as a local file. Empty until one was fetched.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
    /// Raised hand in a moderated group video chat (`groupCallParticipant.raise_hand_rating`) or group call (e.g., Signal).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hand_raised: bool,
    /// Ordering rating for raised hands in Telegram group video chats (`raise_hand_rating`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_hand_rating: Option<i64>,
    /// Active transient emoji reaction in a group call (e.g., Signal group calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<String>,
    /// Short bio/about text returned on the participant (`groupCallParticipant.about`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
    /// Locally muted by you on the server (`groupCallParticipant.muted_by_you`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted_by_you: bool,
    /// True when this participant joined as a channel or group rather than a user account.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_channel: bool,
    /// Server-reported per-participant volume in percent (0..=200), mapped from Telegram's 1..=20000 scale (#468).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u16>,
    /// True while their camera video track is paused (`groupCallParticipantVideo.paused`) (#467).
    #[serde(default, skip_serializing_if = "is_false")]
    pub video_paused: bool,
    /// True while their screen-share presentation is paused (`groupCallParticipantVideo.paused`) (#467).
    #[serde(default, skip_serializing_if = "is_false")]
    pub presentation_paused: bool,
    /// Their camera is paused (e.g., Telegram 1:1 `videoState: paused`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub camera_paused: bool,
    /// Their device reported low battery (e.g., Telegram 1:1 `isBatteryLow`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub low_battery: bool,
}

/// Converts Telegram's `groupCallParticipant.volume` (`1..=20000`, where `10000` = 100%)
/// to tinyclient's gain percentage (`0..=200`) (#468).
pub fn telegram_volume_to_percent(volume: i32) -> u16 {
    let clamped = volume.clamp(0, 20_000) as u32;
    ((clamped + 50) / 100).min(u32::from(GAIN_MAX)) as u16
}

/// Converts tinyclient's gain percentage (`0..=200`) to Telegram's `volume` scale (`1..=20000`) (#468).
pub fn percent_to_telegram_volume(percent: u16) -> i32 {
    (i32::from(percent.min(GAIN_MAX)) * 100).clamp(1, 20_000)
}

/// Recording mode for Telegram admin `phone.toggleGroupCallRecord` (#463).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupCallRecordMode {
    Audio,
    PortraitVideo,
    LandscapeVideo,
}

/// Playback status of a Telegram channel RTMP live stream (#454).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RtmpStreamState {
    Waiting,
    Live,
    Paused,
    Ended,
}

/// Server URL and stream key returned by `phone.getGroupCallStreamRtmpUrl` (#454).
/// The stream key is redacted from `Debug` output so it is never written to logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallRtmpUrl {
    pub url: String,
    pub key: String,
}

impl std::fmt::Debug for GroupCallRtmpUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupCallRtmpUrl")
            .field("url", &self.url)
            .field("key", &"[redacted]")
            .finish()
    }
}

/// One eligible identity returned by `phone.getGroupCallJoinAs` (#460).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallJoinPeer {
    /// Peer id (`u<id>`, `c<id>`, `g<id>`).
    pub id: String,
    pub name: String,
    /// True when this peer is the user's own personal account ("Me").
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_self: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<crate::MediaFile>,
}

/// Allowed join-as peers and the chat's remembered choice (`groupcall_default_join_as`) (#460).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallJoinAs {
    pub peers: Vec<GroupCallJoinPeer>,
    /// Remembered peer id for this chat, when present on the full chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_peer: Option<String>,
    /// True when the full chat could not be read; the UI should ask rather than defaulting to Me.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default_unknown: bool,
}

/// Per-person refusal returned when inviting people into a Telegram group video chat (#459).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallInviteRefusal {
    pub user_id: String,
    pub reason: String,
}

/// Result of `Request::GroupCallInvite` (#459).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCallInviteResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invited: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<GroupCallInviteRefusal>,
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
    /// A moderator muted or deafened us in this channel or room.
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_deafened: bool,
    /// We may not speak in this channel or room (missing Speak permission or stage audience).
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppressed: bool,
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
    /// Every camera the system lists (names and ids only), at most [`DEVICES_LIMIT`]. Filled
    /// while the device menu or camera preview is open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<CameraSource>,
    /// The system's id for the chosen camera, when one is chosen or active.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub camera_id: String,
    /// How the call's media is protected, for the lock in the call's header. `None` for a call
    /// that says nothing about it (every network but a Tiny server today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<CallSecurity>,
    /// The one picture being watched. `None` while nothing is on screen. Mirrors the primary
    /// entry of [`Call::pictures`] for single-stream callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<CallPicture>,
    /// All active received pictures being decoded and painted (multi-stream video).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<CallPicture>,
    /// Sources refused by mediad's decode budget (too many concurrent videos).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_refused: Vec<(String, PictureKind)>,
    /// My own camera as it is sent, for my tile. `None` while it is off. Its `user_id` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_view: Option<CallPicture>,
    /// My own share as it is sent, for the stage while I am sharing. `None` while it is off. Its
    /// `user_id` is empty. Apart from `self_view`: a camera and a share can be on together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_view: Option<CallPicture>,
    /// Who the server's video slot currently carries (`call.slots` `video`), even before a frame
    /// has been decoded and painted. `None` when the slot is empty or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_source: Option<(String, PictureKind)>,
    /// Whether this call allows camera and screen share (a room's `video_enabled`, or `true` for
    /// direct calls and Discord). Defaults to `true` when omitted by older builds.
    #[serde(default = "video_allowed_default", skip_serializing_if = "is_true")]
    pub video_allowed: bool,
    /// People rung for this call who are not in it yet. Empty on a call that rings no one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ringing: Vec<CallRing>,
    /// The direct conversation this call moved out of. Set while the move is still settling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<Key>,
    /// One sentence about that move, or why it came back. The shell shows it once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_notice: Option<String>,
    /// An incoming ring is a video call (Telegram `phoneCallRequested.video`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub incoming_video: bool,
    /// Title of the live group video chat / live stream, when set (`groupCall.title`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// People watching our own share, named from the ids the network sent. Not inferred from
    /// who is merely in the voice channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub viewers: Vec<Participant>,
    /// The stream we are watching has stopped sending. The tile says so.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream_paused: bool,
    /// A Go Live leg's own MLS group. It does not replace [`Call::security`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_security: Option<CallSecurity>,
    /// Whether our own hand is raised in the active group call / video chat.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hand_raised: bool,
    /// Our own active transient emoji reaction in the active group call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<String>,
    /// Shareable call invite link (`https://t.me/call/<slug>`) for an active Telegram encrypted conference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_link: Option<String>,
    /// Telegram `groupCall.join_muted`: whether new participants join muted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub join_muted: bool,
    /// Telegram `groupCall.can_change_join_muted`: whether we may toggle `join_muted`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_change_join_muted: bool,
    /// Total participant count reported by the server (`groupCall.participants_count`), which may
    /// exceed `participants.len() + 1` in large video chats.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub participants_count: u32,
    /// Peer id we joined this Telegram group call as (`u<id>` / `c<id>` / `g<id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_as: Option<String>,
    /// Unix seconds when server-side recording started (`groupCall.record_start_date`) (#463).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_start_date: Option<i64>,
    /// True when video is also being recorded (`groupCall.record_video_active`) (#463).
    #[serde(default, skip_serializing_if = "is_false")]
    pub record_video_active: bool,
    /// Telegram `groupCall.can_start_video`: whether we may start camera/screen video (#467).
    #[serde(default = "video_allowed_default", skip_serializing_if = "is_true")]
    pub can_start_video: bool,
    /// Active video/presentation streams currently unmuted (`groupCall.unmuted_video_count`) (#467).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub unmuted_video_count: u32,
    /// Maximum simultaneous video/presentation streams allowed (`groupCall.unmuted_video_limit`) (#467).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub unmuted_video_limit: u32,
    /// True when this Telegram call is an RTMP live stream (`groupCall.rtmp_stream`) (#454).
    #[serde(default, skip_serializing_if = "is_false")]
    pub rtmp_stream: bool,
    /// Playback status of a joined Telegram RTMP live stream (#454).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtmp_state: Option<RtmpStreamState>,
    /// Currently selected RTMP stream quality tier (0 = Low, 1 = Medium, 2 = High) (#454).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtmp_quality: Option<u8>,
    /// Available RTMP stream quality tiers reported for this stream (#454).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rtmp_qualities: Vec<u8>,
}

impl Call {
    /// Returns a clear user-facing reason when starting camera or screen share is blocked by the
    /// server's video limit or live-stream mode (#467, #454). Pass `already_sending = true` when
    /// toggling off an already active local track (stopping is always allowed).
    pub fn video_limit_reason(&self, already_sending: bool) -> Option<String> {
        if already_sending {
            return None;
        }
        if self.conversation.network != crate::Network::Telegram {
            return None;
        }
        if self.rtmp_stream {
            return Some("Viewers can't turn on camera or share screen in a live stream.".to_owned());
        }
        if !self.can_start_video {
            return Some(if self.unmuted_video_limit > 0 {
                format!(
                    "Video limit reached ({} of {} active video streams).",
                    self.unmuted_video_count, self.unmuted_video_limit
                )
            } else {
                "Video isn't available in this video chat right now.".to_owned()
            });
        }
        if self.unmuted_video_limit > 0 && self.unmuted_video_count >= self.unmuted_video_limit {
            return Some(format!(
                "Video limit reached ({} of {} active video streams).",
                self.unmuted_video_count, self.unmuted_video_limit
            ));
        }
        None
    }
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Preview and metadata for a Telegram encrypted conference call (`t.me/call/<slug>`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConferenceCallInfo {
    pub call_id: String,
    #[serde(default)]
    pub access_hash: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slug: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invite_link: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default)]
    pub participants_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<crate::Occupant>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub video: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub e2ee: bool,
}

/// Someone rung into a group call, and what the server last said of that ring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRing {
    pub id: String,
    pub name: String,
    /// `ringing`, `declined`, `missed`, `busy`, or `unreachable`.
    pub state: String,
}

fn video_allowed_default() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

/// A Tiny server call's protection (tinyserver's `docs/calls.md`, "End-to-end encryption").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSecurity {
    pub mode: SecurityMode,
    /// The call code: 30 digits from the group's epoch authenticator, the same for everyone in
    /// the call, new whenever someone joins or leaves. Empty until the group has keys.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// Explicit verification emoji for a 1:1 end-to-end encrypted call (Telegram's 4 emoji from
    /// `SHA-256(key || g_a)` over the 333-emoji table). Empty before `phone.confirmCall` succeeds
    /// and the fingerprint matches, and empty for group video chats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emojis: Vec<String>,
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
    /// This report is the Go Live leg, not the voice call. chatd keeps it on [`Call::stream_security`].
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub stream: bool,
    /// Our DAVE identity public key (hex). Empty on a Tiny call. chatd publishes it once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub identity: String,
    /// This call is never carried on unencrypted: the person chose so, the server is an Incognito
    /// host, or the room only takes end-to-end encrypted calls. A [`SecurityMode::Blocked`] call
    /// then offers no way on but leaving.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub required: bool,
}

impl CallSecurity {
    /// Builds [`CallSecurity`] for a confirmed Telegram 1:1 call whose key fingerprint matched.
    /// Returns `None` before `phone.confirmCall` succeeds or when the fingerprint did not match.
    pub fn telegram_verified(key: &[u8], g_a: &[u8], confirmed: bool, fingerprint_matched: bool) -> Option<Self> {
        if !confirmed || !fingerprint_matched {
            return None;
        }
        let emojis = crate::telegram_call_emojis(key, g_a)
            .into_iter()
            .map(str::to_owned)
            .collect();
        Some(CallSecurity {
            mode: SecurityMode::EndToEnd,
            emojis,
            ..CallSecurity::default()
        })
    }

    /// Builds [`CallSecurity`] for a verified Telegram E2EE conference call.
    /// The four emoji are derived from the verified chain state hash and change on every membership update.
    pub fn telegram_conference_verified(state_hash: &[u8; 32], epoch: u64, members: Vec<SecureMember>) -> Self {
        let emojis = crate::telegram_call_emoji::telegram_conference_emojis(state_hash)
            .into_iter()
            .map(str::to_owned)
            .collect();
        CallSecurity {
            mode: SecurityMode::EndToEnd,
            emojis,
            epoch,
            members,
            required: true,
            ..CallSecurity::default()
        }
    }

    /// Fail-closed security state for a Telegram E2EE conference whose chain verification failed.
    /// `required: true` forbids carrying on unencrypted.
    pub fn telegram_conference_blocked() -> Self {
        CallSecurity {
            mode: SecurityMode::Blocked,
            required: true,
            ..CallSecurity::default()
        }
    }
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
    /// A Go Live connection, not the call's own voice. Its loop sends the screen, and that
    /// screen's sound when the share asked for it.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub stream: bool,
    /// This Go Live connection only receives. It sends no picture of its own.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub watch: bool,
    /// The user whose Go Live stream this receive-only leg is watching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_owner: Option<String>,
    /// The MLS group for a stream, the stream server id minus one. Empty on the voice
    /// connection, which uses the channel id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group_id: String,
}

/// Gain key for a Discord Go Live stream. Voice gains use the bare user id.
pub fn discord_stream_gain_key(user_id: &str) -> String {
    format!("stream:{user_id}")
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
    /// Number of receive-only video m-lines to offer (0 or 1 = legacy single video slot).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub video_slots: u8,
}

/// Desired display size for a watched video stream (`Large` requests `"high"`, `Small` requests `"low"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchSize {
    #[default]
    Large,
    Small,
}

/// One video stream the shell wants decoded and painted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WatchSource {
    pub user_id: String,
    #[serde(default)]
    pub kind: PictureKind,
    #[serde(default)]
    pub size: WatchSize,
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

/// One Telegram 1:1 call endpoint (`phoneConnection` reflector or `phoneConnectionWebrtc` STUN/TURN).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramEndpoint {
    pub id: i64,
    pub ip: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ipv6: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "is_false")]
    pub webrtc: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tcp: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stun: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub turn: bool,
    /// Base64 of the 16-byte reflector `peer_tag` (for `phoneConnection`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub peer_tag: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
}

impl std::fmt::Debug for TelegramEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramEndpoint")
            .field("id", &self.id)
            .field("ip", &self.ip)
            .field("port", &self.port)
            .field("webrtc", &self.webrtc)
            .field("tcp", &self.tcp)
            .field("stun", &self.stun)
            .field("turn", &self.turn)
            .finish_non_exhaustive()
    }
}

/// Everything mediad needs to carry a confirmed Telegram 1:1 voice or video call.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramCallJoin {
    pub user_id: String,
    pub peer_id: String,
    pub outgoing: bool,
    /// Base64 of the 256-byte Diffie-Hellman shared key. Redacted in Debug.
    pub key: String,
    pub connections: Vec<TelegramEndpoint>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub p2p_allowed: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub config_json: String,
    pub muted: bool,
    pub deafened: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub video: bool,
}

impl std::fmt::Debug for TelegramCallJoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramCallJoin")
            .field("user_id", &self.user_id)
            .field("peer_id", &self.peer_id)
            .field("outgoing", &self.outgoing)
            .field("connections", &self.connections.len())
            .field("p2p_allowed", &self.p2p_allowed)
            .field("muted", &self.muted)
            .field("deafened", &self.deafened)
            .field("video", &self.video)
            .finish_non_exhaustive()
    }
}

/// What mediad needs to build Telegram's `phone.joinGroupCall` `params` JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramGroupOffer {
    pub user_id: String,
    pub muted: bool,
    pub deafened: bool,
    /// True for an encrypted conference call (`tde2e`-compatible chain verification + per-frame E2EE).
    #[serde(default, skip_serializing_if = "is_false")]
    pub conference: bool,
}

/// Maps a Telegram group call audio SSRC (`source`) to its participant id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramGroupSource {
    pub ssrc: u32,
    pub user_id: String,
}

/// Maps a remote Telegram group call camera or screen-share endpoint & SSRC group to its owner (#467).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramGroupVideoSource {
    pub user_id: String,
    pub kind: PictureKind,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssrcs: Vec<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

/// One TURN relay server handed out by Signal's calling relay endpoint (`/v1/accounts/turn`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRelay {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
}

/// What mediad needs to build a WebRTC offer for a Signal 1:1 or group call (pure-Rust `str0m`
/// Opus + H.264 over DTLS-SRTP).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalOffer {
    pub user_id: String,
    pub peer_id: String,
    /// Host or IP used to choose the local route when direct candidates are allowed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub video: bool,
    /// When true, gather only TURN relay candidates (no host IP exposed to the peer).
    #[serde(default, skip_serializing_if = "is_false")]
    pub relay_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<SignalRelay>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
}

/// What mediad needs to accept an incoming Signal call offer and produce an SDP answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalAccept {
    pub user_id: String,
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    pub offer_sdp: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub video: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub relay_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<SignalRelay>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
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
        /// The sharer's pointer drawn into the frames. `None` is on, as viewers follow what is
        /// pointed at and the Wayland portal already embedded it; `Some(false)` hides it where
        /// the capture can.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        show_cursor: Option<bool>,
        /// The sound of the shared screen or window, on the Go Live audio SSRC. `None` is off,
        /// which is what an older shell asked for, and what a phone sends: it has no loopback.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sound: Option<bool>,
        /// Send Discord a small preview of the share, outside the encrypted stream. `None` is
        /// off: an older shell, or a phone, which has no switch for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<bool>,
    },
    /// Watch one participant's share. Replaces any share already being watched.
    ShareWatch { source: Option<String> },
    /// Watch zero or more video streams at once (multi-stream video). Replaces the watch list.
    Watch { sources: Vec<WatchSource> },
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
    /// Stop one Go Live leg. `watch` is the receive-only connection. When `stream_owner` is
    /// `Some`, only that user's watched stream leg is stopped; `None` stops all watch legs (or
    /// the outgoing share leg when `watch` is false). Answered `Done`.
    StreamStop {
        watch: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_owner: Option<String>,
    },
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
    /// Local camera preview before turning the camera on: captures for `SelfView` without sending
    /// video to peers.
    CameraPreview { on: bool },
    /// Camera quality, self-view mirroring, and on-device background blur (V23–V25).
    CameraSettings(CameraSettings),
    /// Who the one video slot carries now. `None` when the server sends `video: null`.
    TinyPicture { picture: Option<TinyPicture> },
    /// Who each active video receive slot carries now, from `call.slots.videos`.
    TinyPictures { pictures: Vec<TinyPicture> },
    /// One `call.e2ee` frame from the server, as its JSON text, in the order it arrived.
    TinyE2ee { frame: String },
    /// Everyone in the room's call, us included, whenever it changes: whom the MLS group may
    /// hold.
    TinyRoster { user_ids: Vec<String> },
    /// The person chose to carry on without end-to-end encryption after the server turned it
    /// off ([`SecurityMode::Blocked`]). Answered `Done`, or `Refused` in words when the call is
    /// not blocked or may not carry on unencrypted ([`CallSecurity::required`]).
    TinyTransport,
    /// A confirmed Telegram 1:1 voice call: connect media over the negotiated endpoints using the
    /// DH shared key. Answered `Done`; then `Connected` once media flows.
    TelegramCall(TelegramCallJoin),
    /// In-call signalling payload received via Telegram `updatePhoneCallSignalingData` (base64).
    TelegramSignal { data: String },
    /// A Telegram group video chat: build local ICE/DTLS join JSON and audio SSRC.
    /// Answered `TelegramGroupJoin`.
    TelegramGroupOffer(TelegramGroupOffer),
    /// Telegram's server transport JSON from `updateGroupCallConnection` for a group video chat.
    /// Answered `Done`; then `Connected` once media flows.
    TelegramGroupAnswer { params: String },
    /// Current SSRC -> participant id mapping for a Telegram group video chat.
    TelegramGroupSources { sources: Vec<TelegramGroupSource> },
    /// Build a presentation (screen share) WebRTC offer for `phone.joinGroupCallPresentation` (#467).
    /// Answered `TelegramGroupPresentationJoin`.
    TelegramGroupPresentationOffer,
    /// Server transport JSON from `updateGroupCallConnection(presentation=true)` (#467).
    TelegramGroupPresentationAnswer { params: String },
    /// Tear down the local Telegram presentation transport (`phone.leaveGroupCallPresentation`) (#467).
    TelegramGroupPresentationLeave,
    /// Remote camera & presentation video endpoints/SSRCs for a Telegram group video chat (#467).
    TelegramGroupVideoSources { sources: Vec<TelegramGroupVideoSource> },
    /// Start receive-only RTMP live stream playback in mediad (no microphone) (#454).
    TelegramRtmpStart { channel_id: String },
    /// Deliver one fetched `inputGroupCallStream` segment (base64 payload) to mediad (#454).
    TelegramRtmpSegment {
        time_ms: i64,
        scale: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_channel: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_quality: Option<u8>,
        data: String,
    },
    /// Report RTMP stream state change (Live, Paused, Ended, Waiting) to mediad (#454).
    TelegramRtmpState { state: RtmpStreamState },
    /// Verified or incoming E2EE chain blocks (`phone.getGroupCallChainBlocks` /
    /// `updateGroupCallChainBlocks`) for an encrypted Telegram conference call (base64).
    /// Fails closed if any block fails verification.
    TelegramConferenceBlocks { sub_chain_id: i32, blocks: Vec<String> },
    /// A Signal 1:1 or group call: build a pure-Rust `str0m` WebRTC SDP offer. Answered `Offer`.
    SignalOffer(SignalOffer),
    /// Accept an incoming Signal call SDP offer and start media. Answered `Answer`.
    SignalAccept(SignalAccept),
    /// Apply the remote peer's SDP answer to our pending Signal offer and start media. Answered `Done`.
    SignalAnswer { sdp: String },
    /// Feed a remote ICE candidate (`candidate:...`) into the active or pending Signal WebRTC session.
    SignalIce { candidate: String },

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

/// How long mediad waits with no STUN answer before saying a Tiny link is lost, in milliseconds.
/// Short enough to stay ahead of the server's 15 s consent drop; long enough that a brief Wi-Fi
/// blip does not flash Reconnecting while the seat is still up.
pub const TINY_STUN_SILENCE_MS: u64 = 10_000;

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
    /// Distinct encoders and decoders that produced video in the sampled interval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub encoder_backends: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decoder_backends: Vec<String>,
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
    /// Someone started sharing their screen in this call (S7, with joins and leaves).
    ShareStarted,
    /// Server-side recording started while joined to a video chat (#463).
    RecordingStarted,
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
    /// The SDP offer for a Tiny or Signal call, in answer to `TinyOffer` or `SignalOffer`.
    Offer { sdp: String },
    /// The join JSON and local audio SSRC for a Telegram group video chat, in answer to `TelegramGroupOffer`.
    TelegramGroupJoin {
        params: String,
        ssrc: u32,
        /// Base64 of our 32-byte conference E2EE public key when `conference == true`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        public_key: Option<String>,
    },
    /// The presentation join JSON and video SSRC in answer to `TelegramGroupPresentationOffer` (#467).
    TelegramGroupPresentationJoin { params: String, ssrc: u32 },
    /// The SDP answer for an incoming Signal call, in answer to `SignalAccept`.
    Answer { sdp: String },
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
    DeviceList {
        inputs: Vec<AudioDevice>,
        outputs: Vec<AudioDevice>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cameras: Vec<CameraSource>,
    },
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
    /// Our own camera, or with `share` our own share, as it is being sent, a few times a second,
    /// so the person sees what everyone else sees; an empty path when it went off.
    SelfView {
        path: String,
        width: u32,
        height: u32,
        generation: u64,
        #[serde(default)]
        share: bool,
    },
    /// A picture that could not be captured, in words. The voice call stays.
    Notice { detail: String },
    /// Sources dropped because the requested watch list exceeded this device's decode budget.
    WatchRefused { refused: Vec<(String, PictureKind)> },
    /// In-call signalling bytes for Telegram `phone.sendSignalingData` (base64).
    TelegramSignal { data: String },
    /// Remote peer camera/share/battery state reported over Telegram 1:1 in-call signalling.
    TelegramPeerState {
        user_id: String,
        camera: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        camera_paused: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        low_battery: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        sharing: bool,
    },
    /// A local ICE candidate gathered for an active Signal call, for chatd to send in `IceUpdate`.
    SignalIce { candidate: String },
}

impl MediaResponse {
    /// The enum name only. Logs use this so a frame's SDP and paths stay out of the line.
    pub fn name(&self) -> &'static str {
        match self {
            MediaResponse::Ready { .. } => "Ready",
            MediaResponse::Streams { .. } => "Streams",
            MediaResponse::Done => "Done",
            MediaResponse::Offer { .. } => "Offer",
            MediaResponse::TelegramGroupJoin { .. } => "TelegramGroupJoin",
            MediaResponse::TelegramGroupPresentationJoin { .. } => "TelegramGroupPresentationJoin",
            MediaResponse::Answer { .. } => "Answer",
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
            MediaResponse::WatchRefused { .. } => "WatchRefused",
            MediaResponse::TelegramSignal { .. } => "TelegramSignal",
            MediaResponse::TelegramPeerState { .. } => "TelegramPeerState",
            MediaResponse::SignalIce { .. } => "SignalIce",
        }
    }

    /// True for the frames that answer a request.
    pub fn is_reply(&self) -> bool {
        matches!(
            self,
            MediaResponse::Streams { .. }
                | MediaResponse::Done
                | MediaResponse::Offer { .. }
                | MediaResponse::TelegramGroupJoin { .. }
                | MediaResponse::TelegramGroupPresentationJoin { .. }
                | MediaResponse::Answer { .. }
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
    fn stream_stop_supports_per_owner_and_legacy_all_watch_legs() {
        let legacy: MediaRequest = serde_json::from_str(r#"{"media":"stream_stop","watch":true}"#).unwrap();
        assert_eq!(legacy, MediaRequest::StreamStop { watch: true, stream_owner: None });
        let one = MediaRequest::StreamStop { watch: true, stream_owner: Some("2002".into()) };
        let round: MediaRequest = serde_json::from_str(&serde_json::to_string(&one).unwrap()).unwrap();
        assert_eq!(round, one);
    }

    #[test]
    fn a_watched_go_live_leg_can_name_its_owner_without_changing_older_joins() {
        let old = serde_json::json!({
            "network": "discord", "endpoint": "localhost", "token": "t", "session_id": "s",
            "user_id": "111", "server_id": "900", "channel_id": "901",
            "dave": {"max_version": 1, "required": true}, "muted": false, "deafened": false,
            "stream": true, "watch": true, "group_id": "899"
        });
        let mut join: VoiceJoin = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(join.stream_owner, None);
        assert_eq!(serde_json::to_value(&join).unwrap(), old);

        join.stream_owner = Some("222".into());
        let wire = serde_json::to_value(&join).unwrap();
        assert_eq!(wire["stream_owner"], "222");
        assert_eq!(serde_json::from_value::<VoiceJoin>(wire).unwrap(), join);

        let stream = discord_stream_gain_key("222");
        let account = crate::settings::account_scope(&crate::AccountKey { network: Network::Discord, account: "111".into() });
        assert_ne!(stream, "222");
        assert!(crate::settings::valid(&account, &crate::settings::gain_key(&stream), "50 muted"));
    }

    #[test]
    fn a_self_view_is_my_camera_unless_it_says_it_is_my_share() {
        // A mediad from before shares had a preview sends none: that is my camera, never an error.
        let older: MediaResponse = serde_json::from_str(r#"{"media":"self_view","path":"a.jpg","width":640,"height":360,"generation":3}"#).unwrap();
        assert_eq!(older, MediaResponse::SelfView { path: "a.jpg".into(), width: 640, height: 360, generation: 3, share: false });
        let mine = MediaResponse::SelfView { path: "b.jpg".into(), width: 640, height: 360, generation: 4, share: true };
        let back: MediaResponse = serde_json::from_str(&serde_json::to_string(&mine).unwrap()).unwrap();
        assert_eq!(back, mine);
    }

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
        assert_eq!(older, MediaRequest::ShareSend { on: true, screen: None, quality: None, show_cursor: None, sound: None, preview: None });
        assert_eq!(serde_json::to_string(&older).unwrap(), r#"{"media":"share_send","on":true}"#);
        // The pointer is on when nothing is said, so an older shell gets it like everyone.
        let chosen = MediaRequest::ShareSend { on: true, screen: Some("display:\\\\.\\DISPLAY2".into()), quality: Some(ShareQuality::Balanced), show_cursor: Some(false), sound: None, preview: None };
        let json = serde_json::to_string(&chosen).unwrap();
        assert!(json.contains(r#""quality":"balanced""#) && json.contains(r#""show_cursor":false"#));
        assert!(!json.contains("sound"), "{json}");
        assert!(!json.contains("preview"), "{json}");
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), chosen);
        let shown = MediaRequest::ShareSend { on: true, screen: None, quality: None, show_cursor: Some(true), sound: None, preview: None };
        assert_eq!(serde_json::to_string(&shown).unwrap(), r#"{"media":"share_send","on":true,"show_cursor":true}"#);
        let heard = MediaRequest::ShareSend { on: true, screen: None, quality: None, show_cursor: None, sound: Some(true), preview: None };
        let json = serde_json::to_string(&heard).unwrap();
        assert_eq!(json, r#"{"media":"share_send","on":true,"sound":true}"#);
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), heard);
        let previewed = MediaRequest::ShareSend { on: true, screen: None, quality: None, show_cursor: None, sound: None, preview: Some(true) };
        let json = serde_json::to_string(&previewed).unwrap();
        assert_eq!(json, r#"{"media":"share_send","on":true,"preview":true}"#);
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), previewed);
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

    #[test]
    fn call_video_allowed_defaults_true_and_omits_when_true() {
        let old: Call = serde_json::from_value(serde_json::json!({
            "conversation": { "network": "tiny", "account": "srv", "id": "room" },
            "state": "joined", "participants": [], "muted": false, "deafened": false,
            "joined": 1, "engine_linked": true, "video": []
        }))
        .unwrap();
        assert!(old.video_allowed);
        let wire = serde_json::to_string(&old).unwrap();
        assert!(!wire.contains("video_allowed"), "default true stays off the wire: {wire}");
        let mut disabled = old;
        disabled.video_allowed = false;
        let wire = serde_json::to_string(&disabled).unwrap();
        assert!(wire.contains(r#""video_allowed":false"#), "{wire}");
        assert!(!serde_json::from_str::<Call>(&wire).unwrap().video_allowed);
    }

    #[test]
    fn camera_preview_and_device_list_cameras_round_trip_with_old_wire_compat() {
        let preview = MediaRequest::CameraPreview { on: true };
        let json = serde_json::to_string(&preview).unwrap();
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), preview);

        // Old mediad sends DeviceList without `cameras`: still parses cleanly as empty.
        let old: MediaResponse = serde_json::from_str(r#"{"media":"device_list","inputs":[],"outputs":[]}"#).unwrap();
        assert_eq!(old, MediaResponse::DeviceList { inputs: Vec::new(), outputs: Vec::new(), cameras: Vec::new() });

        let with_cams = MediaResponse::DeviceList {
            inputs: Vec::new(),
            outputs: Vec::new(),
            cameras: vec![CameraSource { id: "c1".into(), name: "Desk Cam".into(), thumbnail: String::new() }],
        };
        let wire = serde_json::to_string(&with_cams).unwrap();
        assert!(wire.contains(r#""cameras""#), "{wire}");
        assert_eq!(serde_json::from_str::<MediaResponse>(&wire).unwrap(), with_cams);
    }

    #[test]
    fn multi_stream_watch_tiny_pictures_and_watch_refused_round_trip() {
        let watch = MediaRequest::Watch {
            sources: vec![
                WatchSource { user_id: "ada".into(), kind: PictureKind::Share, size: WatchSize::Large },
                WatchSource { user_id: "bea".into(), kind: PictureKind::Camera, size: WatchSize::Small },
            ],
        };
        let wire = serde_json::to_string(&watch).unwrap();
        assert_eq!(serde_json::from_str::<MediaRequest>(&wire).unwrap(), watch);

        let pics = MediaRequest::TinyPictures {
            pictures: vec![
                TinyPicture { mid: "2".into(), user_id: "ada".into(), kind: PictureKind::Share },
                TinyPicture { mid: "3".into(), user_id: "bea".into(), kind: PictureKind::Camera },
            ],
        };
        let wire = serde_json::to_string(&pics).unwrap();
        assert_eq!(serde_json::from_str::<MediaRequest>(&wire).unwrap(), pics);

        let refused = MediaResponse::WatchRefused { refused: vec![("cy".into(), PictureKind::Camera)] };
        assert_eq!(refused.name(), "WatchRefused");
        let wire = serde_json::to_string(&refused).unwrap();
        assert_eq!(serde_json::from_str::<MediaResponse>(&wire).unwrap(), refused);
    }

    #[test]
    fn camera_settings_round_trip_and_default_mirror_on_blur_off() {
        let minimal: CameraSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(minimal, CameraSettings { quality: CameraQuality::Auto, mirror: true, blur: false });
        let req = MediaRequest::CameraSettings(CameraSettings {
            quality: CameraQuality::Hd,
            mirror: false,
            blur: true,
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""camera_settings""#) && json.contains(r#""hd""#), "{json}");
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), req);
        for q in [CameraQuality::Auto, CameraQuality::Hd, CameraQuality::Standard, CameraQuality::DataSaver] {
            assert_eq!(CameraQuality::parse(q.word()), Some(q));
        }
    }

    #[test]
    fn telegram_call_and_group_media_frames_round_trip_and_redact_secrets() {
        let endpoint = TelegramEndpoint {
            id: 7,
            ip: "127.0.0.1".into(),
            ipv6: String::new(),
            port: 1400,
            webrtc: false,
            tcp: false,
            stun: false,
            turn: false,
            peer_tag: "AQIDBA==".into(),
            username: "u".into(),
            password: "super-secret-password".into(),
        };
        let join = TelegramCallJoin {
            user_id: "u7001".into(),
            peer_id: "u7002".into(),
            outgoing: true,
            key: "SECRET_BASE64_KEY".into(),
            connections: vec![endpoint],
            p2p_allowed: true,
            config_json: r#"{"audio_max_bitrate":64000}"#.into(),
            muted: false,
            deafened: false,
            video: true,
        };
        let debug = format!("{join:?}");
        assert!(!debug.contains("SECRET_BASE64_KEY"), "DH key must not leak in Debug: {debug}");
        assert!(!debug.contains("super-secret-password"), "TURN password must not leak in Debug: {debug}");
        let req = MediaRequest::TelegramCall(join);
        let wire = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<MediaRequest>(&wire).unwrap(), req);

        let sig = MediaRequest::TelegramSignal { data: "aGVsbG8=".into() };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&sig).unwrap()).unwrap(), sig);
        let offer = MediaRequest::TelegramGroupOffer(TelegramGroupOffer { user_id: "u7001".into(), muted: true, deafened: false, conference: true });
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&offer).unwrap()).unwrap(), offer);
        let ans = MediaRequest::TelegramGroupAnswer { params: r#"{"transport":{}}"#.into() };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&ans).unwrap()).unwrap(), ans);
        let srcs = MediaRequest::TelegramGroupSources { sources: vec![TelegramGroupSource { ssrc: 42, user_id: "u7002".into() }] };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&srcs).unwrap()).unwrap(), srcs);
        let blocks = MediaRequest::TelegramConferenceBlocks { sub_chain_id: 0, blocks: vec!["AQID".into()] };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&blocks).unwrap()).unwrap(), blocks);

        let join_resp = MediaResponse::TelegramGroupJoin { params: r#"{"ufrag":"a"}"#.into(), ssrc: 99, public_key: Some("AAAA".into()) };
        assert!(join_resp.is_reply());
        assert_eq!(join_resp.name(), "TelegramGroupJoin");
        assert_eq!(serde_json::from_str::<MediaResponse>(&serde_json::to_string(&join_resp).unwrap()).unwrap(), join_resp);

        let peer_state = MediaResponse::TelegramPeerState {
            user_id: "u7002".into(),
            camera: true,
            camera_paused: true,
            low_battery: true,
            sharing: true,
        };
        assert!(!peer_state.is_reply());
        assert_eq!(peer_state.name(), "TelegramPeerState");
        assert_eq!(serde_json::from_str::<MediaResponse>(&serde_json::to_string(&peer_state).unwrap()).unwrap(), peer_state);

        let sig_resp = MediaResponse::TelegramSignal { data: "d29ybGQ=".into() };
        assert!(!sig_resp.is_reply());
        assert_eq!(sig_resp.name(), "TelegramSignal");
        assert_eq!(serde_json::from_str::<MediaResponse>(&serde_json::to_string(&sig_resp).unwrap()).unwrap(), sig_resp);
    }

    #[test]
    fn occupants_live_and_scheduled_round_trip_and_manage_call_bit_persists() {
        let sched = crate::Occupants {
            people: Vec::new(),
            total: 0,
            title: "Weekly standup".into(),
            scheduled_at: Some(1_800_000_000),
            subscribed: true,
            live: false,
            ..crate::Occupants::default()
        };
        assert!(sched.active());
        assert!(sched.is_scheduled());
        let json = serde_json::to_string(&sched).unwrap();
        assert_eq!(serde_json::from_str::<crate::Occupants>(&json).unwrap(), sched);
        assert!(!crate::Occupants::default().active());

        let rights = crate::ConversationRights { manage_call: true, ..crate::ConversationRights::default() };
        assert!(crate::ConversationRights::from_bits(rights.to_bits()).manage_call);
    }

    #[test]
    fn wave16b1_group_call_types_and_additive_fields_round_trip() {
        let p = Participant {
            id: "c9001".into(),
            name: "Field Notes".into(),
            muted: true,
            suppressed: true,
            hand_raised: true,
            raise_hand_rating: Some(42),
            about: "Trail dispatch".into(),
            muted_by_you: true,
            is_channel: true,
            ..Participant::default()
        };
        let p_json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Participant>(&p_json).unwrap(), p);
        // Legacy JSON without new fields deserializes with defaults.
        let legacy: Participant = serde_json::from_str(r#"{"id":"u1","name":"Ada"}"#).unwrap();
        assert!(!legacy.hand_raised && legacy.raise_hand_rating.is_none() && legacy.about.is_empty() && !legacy.is_channel);

        let join_as = GroupCallJoinAs {
            peers: vec![
                GroupCallJoinPeer { id: "u1".into(), name: "Me".into(), is_self: true, avatar: None },
                GroupCallJoinPeer { id: "c9001".into(), name: "Field Notes".into(), is_self: false, avatar: None },
            ],
            default_peer: Some("c9001".into()),
            default_unknown: false,
        };
        assert_eq!(
            serde_json::from_str::<GroupCallJoinAs>(&serde_json::to_string(&join_as).unwrap()).unwrap(),
            join_as
        );

        let invited = GroupCallInviteResult {
            invited: vec!["u2".into()],
            refused: vec![GroupCallInviteRefusal { user_id: "u3".into(), reason: "Privacy settings prevent inviting this user.".into() }],
        };
        assert_eq!(
            serde_json::from_str::<GroupCallInviteResult>(&serde_json::to_string(&invited).unwrap()).unwrap(),
            invited
        );
    }

    #[test]
    fn wave16b2_recording_volume_video_limit_and_rtmp_types_round_trip() {
        // Telegram volume scale 1..=20000 <-> 0..=200%.
        assert_eq!(telegram_volume_to_percent(5_000), 50);
        assert_eq!(telegram_volume_to_percent(10_000), 100);
        assert_eq!(telegram_volume_to_percent(20_000), 200);
        assert_eq!(telegram_volume_to_percent(0), 0);
        assert_eq!(percent_to_telegram_volume(50), 5_000);
        assert_eq!(percent_to_telegram_volume(100), 10_000);
        assert_eq!(percent_to_telegram_volume(200), 20_000);
        assert_eq!(percent_to_telegram_volume(0), 1);

        // Stream key is redacted from Debug output so it is never logged.
        let rtmp = GroupCallRtmpUrl {
            url: "rtmps://dc2-1.rtmp.t.me/s/".into(),
            key: "super-secret-stream-key-999".into(),
        };
        let dbg = format!("{rtmp:?}");
        assert!(dbg.contains("rtmps://dc2-1.rtmp.t.me/s/"));
        assert!(!dbg.contains("super-secret-stream-key-999"));
        assert_eq!(serde_json::from_str::<GroupCallRtmpUrl>(&serde_json::to_string(&rtmp).unwrap()).unwrap(), rtmp);

        // Recording & RTMP fields on Occupants and Participant round-trip cleanly.
        let occ = crate::Occupants {
            live: true,
            total: 42,
            record_start_date: Some(1_700_000_123),
            record_video_active: true,
            rtmp_stream: true,
            ..crate::Occupants::default()
        };
        assert_eq!(serde_json::from_str::<crate::Occupants>(&serde_json::to_string(&occ).unwrap()).unwrap(), occ);

        let p = Participant {
            id: "c9001".into(),
            name: "Field Notes".into(),
            volume: Some(50),
            camera: true,
            video_paused: true,
            sharing: true,
            presentation_paused: false,
            ..Participant::default()
        };
        assert_eq!(serde_json::from_str::<Participant>(&serde_json::to_string(&p).unwrap()).unwrap(), p);

        // Call::video_limit_reason enforces can_start_video, unmuted_video_limit, and rtmp_stream.
        let mut call: Call = serde_json::from_str(
            r#"{"conversation":{"network":"telegram","account":"a","id":"g8001"},"state":"joined","participants":[],"muted":true,"deafened":false,"joined":10,"engine_linked":true,"video":[]}"#,
        )
        .unwrap();
        assert!(call.can_start_video);
        assert!(call.video_limit_reason(false).is_none());
        call.unmuted_video_limit = 5;
        call.unmuted_video_count = 5;
        assert!(call.video_limit_reason(false).unwrap().contains("5 of 5"));
        assert!(call.video_limit_reason(true).is_none(), "stopping own video is always allowed");
        call.unmuted_video_count = 2;
        call.can_start_video = false;
        assert!(call.video_limit_reason(false).is_some());
        call.can_start_video = true;
        call.rtmp_stream = true;
        assert!(call.video_limit_reason(false).unwrap().contains("live stream"));

        // MediaRequest / MediaResponse additions round-trip and is_reply works.
        let pres_join = MediaResponse::TelegramGroupPresentationJoin { params: r#"{"ufrag":"p"}"#.into(), ssrc: 77 };
        assert!(pres_join.is_reply());
        assert_eq!(pres_join.name(), "TelegramGroupPresentationJoin");
        let vsrc = MediaRequest::TelegramGroupVideoSources {
            sources: vec![TelegramGroupVideoSource {
                user_id: "u7002".into(),
                kind: PictureKind::Camera,
                endpoint: "unified_1".into(),
                ssrcs: vec![2001, 2002],
                paused: false,
            }],
        };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&vsrc).unwrap()).unwrap(), vsrc);
        let seg = MediaRequest::TelegramRtmpSegment {
            time_ms: 1_700_000_000_000,
            scale: 0,
            video_channel: Some(1),
            video_quality: Some(2),
            data: "AAAA".into(),
        };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&seg).unwrap()).unwrap(), seg);
        assert_eq!(
            serde_json::from_str::<Cue>(&serde_json::to_string(&Cue::RecordingStarted).unwrap()).unwrap(),
            Cue::RecordingStarted
        );
        assert!(crate::call_pictures(crate::Network::Telegram));
    }

    #[test]
    fn signal_call_media_frames_round_trip() {
        let offer = MediaRequest::SignalOffer(SignalOffer {
            user_id: "me-aci".into(),
            peer_id: "grace-aci".into(),
            host: "127.0.0.1".into(),
            video: true,
            relay_only: true,
            relays: vec![SignalRelay {
                urls: vec!["turn:turn1.whispersystems.org:443?transport=tcp".into()],
                username: "u".into(),
                password: "p".into(),
            }],
            muted: false,
            deafened: false,
        });
        let json = serde_json::to_string(&offer).unwrap();
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), offer);

        let accept = MediaRequest::SignalAccept(SignalAccept {
            user_id: "me-aci".into(),
            peer_id: "grace-aci".into(),
            host: "127.0.0.1".into(),
            offer_sdp: "v=0\r\n".into(),
            video: true,
            relay_only: false,
            relays: Vec::new(),
            muted: false,
            deafened: false,
        });
        let json = serde_json::to_string(&accept).unwrap();
        assert_eq!(serde_json::from_str::<MediaRequest>(&json).unwrap(), accept);

        let ans_req = MediaRequest::SignalAnswer { sdp: "v=0\r\n".into() };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&ans_req).unwrap()).unwrap(), ans_req);

        let ice_req = MediaRequest::SignalIce { candidate: "candidate:1 1 UDP 2130706431 127.0.0.1 9 typ host".into() };
        assert_eq!(serde_json::from_str::<MediaRequest>(&serde_json::to_string(&ice_req).unwrap()).unwrap(), ice_req);

        let ans_resp = MediaResponse::Answer { sdp: "v=0\r\n".into() };
        assert!(ans_resp.is_reply());
        assert_eq!(ans_resp.name(), "Answer");
        assert_eq!(serde_json::from_str::<MediaResponse>(&serde_json::to_string(&ans_resp).unwrap()).unwrap(), ans_resp);

        let ice_resp = MediaResponse::SignalIce { candidate: "candidate:1 1 UDP 2130706431 127.0.0.1 9 typ host".into() };
        assert!(!ice_resp.is_reply());
        assert_eq!(ice_resp.name(), "SignalIce");
        assert_eq!(serde_json::from_str::<MediaResponse>(&serde_json::to_string(&ice_resp).unwrap()).unwrap(), ice_resp);

        // Group call raise-hand and emoji reaction fields default cleanly for older builds and round-trip when set.
        let p = Participant {
            id: "grace".into(),
            name: "Grace".into(),
            hand_raised: true,
            reaction: Some("👏".into()),
            ..Participant::default()
        };
        let wire = serde_json::to_string(&p).unwrap();
        assert!(wire.contains(r#""hand_raised":true"#) && wire.contains(r#""reaction":"👏""#), "{wire}");
        assert_eq!(serde_json::from_str::<Participant>(&wire).unwrap(), p);
    }
}

#[cfg(test)]
#[test]
fn video_codec_backends_are_optional_on_the_wire() {
    let stats = VideoStats::default();
    let old = serde_json::to_value(&stats).unwrap();
    assert!(old.get("encoder_backends").is_none());
    let mut new = old.clone();
    new["encoder_backends"] = serde_json::json!(["VideoToolbox", "OpenH264"]);
    new["decoder_backends"] = serde_json::json!(["VideoToolbox"]);
    let round_trip = serde_json::to_value(serde_json::from_value::<VideoStats>(new.clone()).unwrap()).unwrap();
    assert_eq!(round_trip, new);
    assert_eq!(serde_json::from_value::<VideoStats>(old).unwrap(), stats);
}
