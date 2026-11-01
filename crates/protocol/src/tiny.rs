//! A Tiny server's members, invites, audit, rooms, text channels, and direct messages. A room is a voice channel:
//! joining takes a seat in its call, with audio when the server's audio is up.
//!
//! The server and this client ship apart, so everything read from it is read tolerantly: a field
//! that is missing or `null` takes its default, a field this client does not know is ignored, and
//! a row that cannot be read at all is left out of its list ([`tiny_rows`]) rather than failing
//! the page. Only a row's own id is required.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyMember {
    pub user_id: String,
    /// Empty when the server has none for them.
    #[serde(default, deserialize_with = "text_or_null")]
    pub email: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub display_name: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub role: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub created_at: String,
    /// The rest is from a server with roles; an older one sends none of it.
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "crate::is_false")]
    pub owner: bool,
    /// Role ids, @everyone not among them.
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    /// Timed out until then: no calls. A time already past is no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_until: Option<String>,
    /// `invite` or `groups` (the sign-in provider's groups let them in). `None` when not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_via: Option<String>,
    /// SHA-256 of their picture, when they have set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Where that picture is on disk. chatd fills this; the server does not send it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub avatar_path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyInvite {
    pub id: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub role: String,
    /// Who made it. Empty for the server itself: the owner invite printed at setup has no
    /// creator, and the page says "Server setup".
    #[serde(default, deserialize_with = "text_or_null")]
    pub created_by: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_by: Option<String>,
    /// The rest is from a server with invite options; an older one sends none of it.
    /// The creator's name; empty for the server itself.
    #[serde(default, deserialize_with = "text_or_null", skip_serializing_if = "String::is_empty")]
    pub created_by_name: String,
    #[serde(default, deserialize_with = "text_or_null", skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// `None` is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "is_zero")]
    pub uses: u32,
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "Vec::is_empty")]
    pub role_ids: Vec<String>,
}

/// The token is only in the answer to creating an invite.
/// `origin` is the signed-in server, filled in by chatd. The server does not send it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyCode {
    pub id: String,
    pub token: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub role: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub expires_at: String,
    #[serde(default)]
    pub origin: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyAudit {
    pub id: i64,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default, deserialize_with = "text_or_null")]
    pub action: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub subject: String,
    /// The server's detail object, as JSON text. Empty when the server sent none.
    #[serde(default, deserialize_with = "text_or_null")]
    pub detail: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub created_at: String,
    /// The rest is from a newer server. Names are empty when it did not say; no `actor_id` and
    /// no name is the server itself.
    #[serde(default, deserialize_with = "text_or_null", skip_serializing_if = "String::is_empty")]
    pub actor_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_user_id: Option<String>,
    #[serde(default, deserialize_with = "text_or_null", skip_serializing_if = "String::is_empty")]
    pub subject_name: String,
    #[serde(default, deserialize_with = "text_or_null", skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// One room, and who is in its call now, in the order they joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyRoom {
    pub id: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub name: String,
    /// Filled in by chatd from the live socket; the server's room list has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<TinySeat>,
    /// The rest is from a server with categories and channel settings. An older one sends none
    /// of it, and every room then reads as uncategorized, with no limit of its own, at the
    /// default bitrate, with voice activation and video allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "is_zero_i32")]
    pub position: i32,
    #[serde(default, deserialize_with = "text_or_null", skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// 0 is the server's default limit.
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "is_zero")]
    pub user_limit: u32,
    /// 0 when the server did not say: encode at [`TinyRoom::bitrate`].
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "is_zero")]
    pub bitrate_kbps: u32,
    /// Push-to-talk only, whatever voice activation allows.
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "crate::is_false")]
    pub ptt_only: bool,
    #[serde(default = "yes", deserialize_with = "true_or_null", skip_serializing_if = "is_true")]
    pub video_enabled: bool,
    /// Only apps that encrypt end to end may join this room's call.
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "crate::is_false")]
    pub e2ee_required: bool,
    #[serde(default, deserialize_with = "or_default", skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<TinyOverride>,
    /// What this account may do in this room. `None` from an older server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_permissions: Option<u64>,
}

impl TinyRoom {
    /// A room as an older server describes it: a name and nothing else.
    pub fn new(id: &str, name: &str) -> TinyRoom {
        TinyRoom {
            id: id.to_owned(),
            name: name.to_owned(),
            participants: Vec::new(),
            category_id: None,
            position: 0,
            topic: String::new(),
            user_limit: 0,
            bitrate_kbps: 0,
            ptt_only: false,
            e2ee_required: false,
            video_enabled: true,
            overrides: Vec::new(),
            my_permissions: None,
        }
    }

    /// The bitrate to encode at, in kb/s: the room's, held to what the server allows, or the
    /// default.
    pub fn bitrate(&self) -> u32 {
        match self.bitrate_kbps {
            0 => TINY_BITRATE_DEFAULT,
            kbps => kbps.clamp(TINY_BITRATE_MIN, TINY_BITRATE_MAX),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

/// A switch that is on unless said otherwise, which the server may send as `null`.
fn true_or_null<'de, D: serde::Deserializer<'de>>(from: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(from)?.unwrap_or(true))
}

/// Someone in a room's call, and whether they muted or deafened themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinySeat {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deafened: bool,
    /// Heard right now. Only known while we are in the same call.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub speaking: bool,
    /// A moderator muted or deafened them in this room (`call.moderate`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_muted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_deafened: bool,
    /// They may not speak in this room, so nothing of theirs is heard.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
    /// Holding priority speech now: listeners duck everyone else while they talk.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub priority: bool,
    /// Their camera is on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub camera: bool,
    /// They hold the room's share.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sharing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyCommunity {
    pub role: String,
    pub members: Vec<TinyMember>,
    pub invites: Vec<TinyInvite>,
    pub audit: Vec<TinyAudit>,
    /// The signed-in server, filled in by chatd. Empty when an older answer had none.
    #[serde(default)]
    pub origin: String,
    /// The server's rooms, in its order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rooms: Vec<TinyRoom>,
    /// Members online now, as the live socket last said. Empty while it is not connected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub online: Vec<String>,
    /// The server's audio, as its `/health` says: `off`, `up` or `down`. Empty when it did not
    /// say. With `up` a join carries audio; with `off` or empty it takes a seat alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub media: String,
    /// What the server can do. No features: an older server, and the page offers only what it
    /// always did.
    #[serde(default, skip_serializing_if = "TinyCapabilities::is_empty")]
    pub capabilities: TinyCapabilities,
    /// What this account may do, from a server with roles. `None`: the legacy `role` decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<TinyAccess>,
    /// This account's user id on the server.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub me: String,
    /// Name, icon and settings, from a server with the `server` feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<TinyServerInfo>,
    /// Highest position first. Empty from an older server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<TinyRole>,
    /// Top first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<TinyCategory>,
    /// Only for an account that may ban.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bans: Vec<TinyBan>,
    /// The sign-in provider's name, as the server's `/v1/auth/config` gives it, filled in by
    /// chatd. Empty when the server did not say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
}

impl TinyCommunity {
    /// Whether this account may do `id` ([`tiny_allowed`]).
    pub fn allowed(&self, id: &str) -> bool {
        tiny_allowed(self.access.as_ref(), &self.role, id)
    }

    /// Whether the server lists `feature`.
    pub fn has(&self, feature: &str) -> bool {
        self.capabilities.has(feature)
    }

    pub fn member(&self, user_id: &str) -> Option<&TinyMember> {
        self.members.iter().find(|member| member.user_id == user_id)
    }

    /// Whether this account may act on `target`: kick, ban, timeout, roles, server mute or
    /// deafen, move, disconnect. On an older server as the page always allowed: an owner on
    /// anyone else who is not an owner, an admin on a member.
    pub fn outranks(&self, target: &TinyMember) -> bool {
        match &self.access {
            Some(access) => access.outranks(&self.me, target, &self.roles),
            None => target.user_id != self.me && target.role != "owner" && (self.role == "owner" || (self.role == "admin" && target.role == "member")),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Server management (tinyserver's `feat/server-management`). Every field here is additive: an
// older server sends none of it, and `TinyCommunity::access` is then `None`, which is how the
// page knows to offer only what it offered before.

/// What the server says it can do, from `GET /v1/capabilities`. A server without that route is
/// an older one: no features, and the page behaves as it always did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyCapabilities {
    #[serde(default, deserialize_with = "text_or_null")]
    pub version: String,
    #[serde(default, deserialize_with = "or_default")]
    pub features: Vec<String>,
}

/// The features a server may list. Each gates its part of the page and its requests.
pub const TINY_FEATURE_SERVER: &str = "server";
pub const TINY_FEATURE_ROLES: &str = "roles";
pub const TINY_FEATURE_CATEGORIES: &str = "categories";
pub const TINY_FEATURE_BANS: &str = "bans";
pub const TINY_FEATURE_TIMEOUTS: &str = "timeouts";
pub const TINY_FEATURE_INVITE_OPTIONS: &str = "invite_options";
pub const TINY_FEATURE_AUDIT_QUERY: &str = "audit_query";
pub const TINY_FEATURE_VOICE_MODERATION: &str = "voice_moderation";
/// Calls can be end-to-end encrypted: MLS between the apps, which the server only relays.
pub const TINY_FEATURE_E2EE_CALLS: &str = "e2ee_calls";
/// The domains Incognito hosts communities under. A community there promises calls that are end
/// to end encrypted, so tinyclient never carries one on unencrypted, whatever the server says.
pub const INCOGNITO_DOMAINS: [&str; 2] = ["incognito.tinyclient.io", "tinycloud.chat"];

/// `host` is one of Incognito's, or a name under one: `c-7f3k2x9q.tinycloud.chat`. A host, not a
/// URL: no scheme, port or path. Case and a trailing dot do not matter; a name that only ends in
/// the same letters (`eviltinycloud.chat`) is not one.
pub fn incognito_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    INCOGNITO_DOMAINS.iter().any(|domain| host == *domain || host.strip_suffix(domain).is_some_and(|label| label.ends_with('.') && label.len() > 1))
}

/// An admin can set how large an upload may be.
pub const TINY_FEATURE_UPLOAD_LIMITS: &str = "upload_limits";
/// Uploads are scanned, and archives stay off until an owner allows them.
pub const TINY_FEATURE_FILE_SCAN: &str = "file_scan";
/// Images and GIFs, until an admin changes it.
pub const TINY_IMAGE_MAX_DEFAULT: i64 = 8 * 1024 * 1024;
/// Video and every other file, until an admin changes it.
pub const TINY_FILE_MAX_DEFAULT: i64 = 25 * 1024 * 1024;
/// The most either cap may be.
pub const TINY_UPLOAD_MAX: i64 = 100 * 1024 * 1024;

impl TinyCapabilities {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|known| known == feature)
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty() && self.version.is_empty()
    }
}

// Permission bits, as the server numbers them. Owners have every bit everywhere.
pub const TINY_VIEW_CHANNEL: u64 = 1 << 0;
pub const TINY_CONNECT: u64 = 1 << 1;
pub const TINY_SPEAK: u64 = 1 << 2;
pub const TINY_VIDEO: u64 = 1 << 3;
pub const TINY_SHARE_SCREEN: u64 = 1 << 4;
/// Without it, push-to-talk only.
pub const TINY_VOICE_ACTIVITY: u64 = 1 << 5;
pub const TINY_PRIORITY_SPEAKER: u64 = 1 << 6;
pub const TINY_CREATE_INVITES: u64 = 1 << 7;
pub const TINY_MANAGE_INVITES: u64 = 1 << 8;
pub const TINY_MANAGE_CHANNELS: u64 = 1 << 9;
pub const TINY_MANAGE_ROLES: u64 = 1 << 10;
pub const TINY_KICK_MEMBERS: u64 = 1 << 11;
pub const TINY_BAN_MEMBERS: u64 = 1 << 12;
pub const TINY_TIMEOUT_MEMBERS: u64 = 1 << 13;
pub const TINY_MUTE_MEMBERS: u64 = 1 << 14;
pub const TINY_DEAFEN_MEMBERS: u64 = 1 << 15;
pub const TINY_MOVE_MEMBERS: u64 = 1 << 16;
pub const TINY_DISCONNECT_MEMBERS: u64 = 1 << 17;
pub const TINY_VIEW_AUDIT_LOG: u64 = 1 << 18;
pub const TINY_MANAGE_SERVER: u64 = 1 << 19;
/// Every bit there is.
pub const TINY_ALL: u64 = (1 << 20) - 1;
/// The bits a room's override may change.
pub const TINY_CHANNEL_SCOPED: u64 = TINY_VIEW_CHANNEL
    | TINY_CONNECT
    | TINY_SPEAK
    | TINY_VIDEO
    | TINY_SHARE_SCREEN
    | TINY_VOICE_ACTIVITY
    | TINY_PRIORITY_SPEAKER
    | TINY_MANAGE_CHANNELS
    | TINY_MUTE_MEMBERS
    | TINY_DEAFEN_MEMBERS
    | TINY_MOVE_MEMBERS
    | TINY_DISCONNECT_MEMBERS;
/// What @everyone has on a server that was never changed.
pub const TINY_EVERYONE_DEFAULT: u64 = TINY_VIEW_CHANNEL | TINY_CONNECT | TINY_SPEAK | TINY_VIDEO | TINY_SHARE_SCREEN | TINY_VOICE_ACTIVITY;

/// Every permission, in the server's order, with its wire name: for the settings page's
/// toggles. Bit `n` is `1 << n`.
pub const TINY_PERMISSIONS: [(u64, &str); 20] = [
    (TINY_VIEW_CHANNEL, "view_channel"),
    (TINY_CONNECT, "connect"),
    (TINY_SPEAK, "speak"),
    (TINY_VIDEO, "video"),
    (TINY_SHARE_SCREEN, "share_screen"),
    (TINY_VOICE_ACTIVITY, "voice_activity"),
    (TINY_PRIORITY_SPEAKER, "priority_speaker"),
    (TINY_CREATE_INVITES, "create_invites"),
    (TINY_MANAGE_INVITES, "manage_invites"),
    (TINY_MANAGE_CHANNELS, "manage_channels"),
    (TINY_MANAGE_ROLES, "manage_roles"),
    (TINY_KICK_MEMBERS, "kick_members"),
    (TINY_BAN_MEMBERS, "ban_members"),
    (TINY_TIMEOUT_MEMBERS, "timeout_members"),
    (TINY_MUTE_MEMBERS, "mute_members"),
    (TINY_DEAFEN_MEMBERS, "deafen_members"),
    (TINY_MOVE_MEMBERS, "move_members"),
    (TINY_DISCONNECT_MEMBERS, "disconnect_members"),
    (TINY_VIEW_AUDIT_LOG, "view_audit_log"),
    (TINY_MANAGE_SERVER, "manage_server"),
];

/// The server's identity and settings, from `GET /v1/server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyServerInfo {
    #[serde(default, deserialize_with = "text_or_null")]
    pub name: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub description: String,
    /// Shown on the server's home.
    #[serde(default, deserialize_with = "text_or_null")]
    pub welcome: String,
    /// The icon's SHA-256, hex. `None` when the server has no icon.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub default_room_id: Option<String>,
    #[serde(default)]
    pub afk_room_id: Option<String>,
    /// 0 is off.
    #[serde(default, deserialize_with = "or_default")]
    pub afk_timeout_secs: i32,
    /// Images and GIFs, in bytes. Missing on an older server: 8 MiB.
    #[serde(default = "image_max_default", deserialize_with = "image_max_bytes")]
    pub image_max_bytes: i64,
    /// Video and every other file, in bytes. Missing on an older server: 25 MiB.
    #[serde(default = "file_max_default", deserialize_with = "file_max_bytes")]
    pub file_max_bytes: i64,
    /// Zip and the other archives may be uploaded. Missing on an older server: false.
    #[serde(default)]
    pub allow_archives: bool,
    #[serde(default, deserialize_with = "text_or_null")]
    pub created_at: String,
    /// The icon as a file on this computer, filled in by chatd. Empty while there is none or it
    /// could not be fetched.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon_path: String,
}

impl Default for TinyServerInfo {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            welcome: String::new(),
            icon: None,
            default_room_id: None,
            afk_room_id: None,
            afk_timeout_secs: 0,
            image_max_bytes: TINY_IMAGE_MAX_DEFAULT,
            file_max_bytes: TINY_FILE_MAX_DEFAULT,
            allow_archives: false,
            created_at: String::new(),
            icon_path: String::new(),
        }
    }
}

fn image_max_default() -> i64 {
    TINY_IMAGE_MAX_DEFAULT
}

fn file_max_default() -> i64 {
    TINY_FILE_MAX_DEFAULT
}

fn image_max_bytes<'de, D: serde::Deserializer<'de>>(from: D) -> Result<i64, D::Error> {
    Ok(Option::<i64>::deserialize(from)?.filter(|bytes| *bytes > 0).unwrap_or(TINY_IMAGE_MAX_DEFAULT))
}

fn file_max_bytes<'de, D: serde::Deserializer<'de>>(from: D) -> Result<i64, D::Error> {
    Ok(Option::<i64>::deserialize(from)?.filter(|bytes| *bytes > 0).unwrap_or(TINY_FILE_MAX_DEFAULT))
}

/// Longest server name, description and welcome text the server keeps.
pub const TINY_SERVER_NAME_MAX: usize = 80;
pub const TINY_DESCRIPTION_MAX: usize = 500;
pub const TINY_WELCOME_MAX: usize = 2000;
/// The largest icon the server takes, in bytes: PNG, JPEG or WebP.
pub const TINY_ICON_MAX: usize = 256 * 1024;

/// One role. @everyone is the one with `default`, at position 0.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyRole {
    pub id: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub name: String,
    /// `#RRGGBB`, or `None` for no colour.
    #[serde(default)]
    pub color: Option<String>,
    /// Higher is more senior.
    #[serde(default, deserialize_with = "or_default")]
    pub position: i32,
    #[serde(default, deserialize_with = "or_default")]
    pub permissions: u64,
    /// Shown as its own group in the member list.
    #[serde(default, deserialize_with = "or_default")]
    pub hoist: bool,
    /// @everyone.
    #[serde(default, deserialize_with = "or_default")]
    pub default: bool,
    #[serde(default, deserialize_with = "or_default")]
    pub member_count: u32,
}

/// A folding group of rooms. Ascending `position` is top first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyCategory {
    pub id: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub name: String,
    #[serde(default, deserialize_with = "or_default")]
    pub position: i32,
}

/// One role's allow and deny bits in one room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyOverride {
    pub role_id: String,
    #[serde(default, deserialize_with = "or_default")]
    pub allow: u64,
    #[serde(default, deserialize_with = "or_default")]
    pub deny: u64,
}

/// Someone banned from the server, from `GET /v1/bans`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyBan {
    pub user_id: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub display_name: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub email: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub reason: String,
    #[serde(default)]
    pub banned_by: Option<String>,
    #[serde(default, deserialize_with = "text_or_null")]
    pub banned_by_name: String,
    #[serde(default, deserialize_with = "text_or_null")]
    pub created_at: String,
    /// `None` is for good.
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// What this account may do on the server, from `GET /v1/me`. `None` on a server from before
/// roles: the legacy role then decides, as [`tiny_can`] always has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyAccess {
    #[serde(default, deserialize_with = "or_default")]
    pub owner: bool,
    #[serde(default, deserialize_with = "or_default")]
    pub roles: Vec<String>,
    /// Server-wide bits: the OR of this account's roles, @everyone among them. A room's own
    /// are its `my_permissions`.
    #[serde(default, deserialize_with = "or_default")]
    pub permissions: u64,
    /// The highest position among this account's roles; owners rank above every role.
    #[serde(default, deserialize_with = "or_default")]
    pub rank: i32,
}

impl TinyAccess {
    /// Every bit for an owner; the server-wide bits for anyone else.
    pub fn bits(&self) -> u64 {
        if self.owner {
            TINY_ALL
        } else {
            self.permissions
        }
    }

    /// Whether this account has all of `bits` server-wide.
    pub fn has(&self, bits: u64) -> bool {
        self.bits() & bits == bits
    }

    /// Whether this account has all of `bits` in `room`, by the room's `my_permissions` where
    /// the server sent them, else by the server-wide bits.
    pub fn has_in(&self, room: &TinyRoom, bits: u64) -> bool {
        if self.owner {
            return true;
        }
        room.my_permissions.unwrap_or(self.permissions) & bits == bits
    }

    /// Whether this account may act on someone (kick, ban, timeout, roles, server mute or
    /// deafen, move, disconnect): only on people ranked below it, and only an owner on an owner's
    /// peers. Nobody acts on themselves this way.
    pub fn outranks(&self, me: &str, target: &TinyMember, roles: &[TinyRole]) -> bool {
        if target.user_id == me {
            return false;
        }
        if target.owner {
            return false;
        }
        self.owner || tiny_rank(roles, &target.roles, false) < self.rank
    }

    /// Whether this account may give, take or edit `role`: only one below its own rank, and
    /// never @everyone's place.
    pub fn may_assign(&self, role: &TinyRole) -> bool {
        self.owner || role.position < self.rank
    }
}

/// A member's rank: the highest position among their roles (@everyone is 0), and above every
/// role for an owner.
pub fn tiny_rank(roles: &[TinyRole], member_roles: &[String], owner: bool) -> i32 {
    if owner {
        return i32::MAX;
    }
    roles.iter().filter(|role| member_roles.contains(&role.id)).map(|role| role.position).fold(0, i32::max)
}

/// The bits someone has in one room, worked out as the server does: the OR of @everyone and
/// their roles, then @everyone's override (deny, then allow), then the OR of their other roles'
/// overrides (deny, then allow). Owners have every bit. The server sends this for the caller as
/// `my_permissions`; this is for "view as role" and for anyone else.
pub fn tiny_room_permissions(roles: &[TinyRole], member_roles: &[String], owner: bool, overrides: &[TinyOverride]) -> u64 {
    if owner {
        return TINY_ALL;
    }
    let everyone = roles.iter().find(|role| role.default);
    let mine = |role: &&TinyRole| role.default || member_roles.contains(&role.id);
    let mut bits = roles.iter().filter(mine).fold(0, |bits, role| bits | role.permissions);
    if let Some(over) = everyone.and_then(|everyone| overrides.iter().find(|over| over.role_id == everyone.id)) {
        bits = (bits & !over.deny) | over.allow;
    }
    let theirs = overrides.iter().filter(|over| everyone.is_none_or(|everyone| over.role_id != everyone.id) && member_roles.contains(&over.role_id));
    let (deny, allow) = theirs.fold((0, 0), |(deny, allow), over| (deny | over.deny, allow | over.allow));
    (bits & !deny) | allow
}

/// The bits a server-wide action `id` needs, as `tiny_can`'s ids name them. `Some(0)` needs
/// nothing beyond membership; `None` is an id this build does not know.
fn tiny_bits(id: &str) -> Option<u64> {
    Some(match id {
        "community.members.read" | "account.name.edit" | "account.sessions.end" => 0,
        "community.invites.read" | "community.invites.create" => TINY_CREATE_INVITES,
        "community.invites.revoke" => TINY_MANAGE_INVITES,
        "community.invites.roles" => TINY_CREATE_INVITES | TINY_MANAGE_ROLES,
        "community.members.role" | "community.roles.manage" => TINY_MANAGE_ROLES,
        "community.members.remove" => TINY_KICK_MEMBERS,
        "community.members.ban" | "community.bans.read" => TINY_BAN_MEMBERS,
        "community.members.timeout" => TINY_TIMEOUT_MEMBERS,
        "community.audit.read" => TINY_VIEW_AUDIT_LOG,
        "community.server.edit" => TINY_MANAGE_SERVER,
        "community.rooms.create" | "community.rooms.rename" | "community.rooms.delete" | "community.rooms.edit" | "community.categories.manage" => {
            TINY_MANAGE_CHANNELS
        }
        "community.rooms.overrides" => TINY_MANAGE_CHANNELS | TINY_MANAGE_ROLES,
        "community.calls.join" => TINY_CONNECT,
        "community.calls.kick" => TINY_DISCONNECT_MEMBERS,
        "community.calls.mute" => TINY_MUTE_MEMBERS,
        "community.calls.deafen" => TINY_DEAFEN_MEMBERS,
        "community.calls.move" => TINY_MOVE_MEMBERS,
        "community.calls.priority" => TINY_PRIORITY_SPEAKER,
        // Only owners make or unmake owners.
        "community.members.owner" => return Some(u64::MAX),
        _ => return None,
    })
}

/// Whether this account may do `id`. With `access` (a server with roles) the permission bits
/// decide; without it (an older server) the legacy role does, exactly as [`tiny_can`]. Owners
/// may do everything this build knows. An unknown id is refused.
///
/// A manager of invites may revoke any; one who may only create them may revoke their own, which
/// the page checks against `created_by`.
pub fn tiny_allowed(access: Option<&TinyAccess>, role: &str, id: &str) -> bool {
    let Some(access) = access else {
        return tiny_can(role, id);
    };
    let Some(bits) = tiny_bits(id) else {
        return false;
    };
    if access.owner {
        return true;
    }
    match id {
        "community.members.owner" => false,
        "community.invites.read" => access.bits() & (TINY_CREATE_INVITES | TINY_MANAGE_INVITES) != 0,
        "community.invites.revoke" => access.bits() & (TINY_CREATE_INVITES | TINY_MANAGE_INVITES) != 0,
        _ => access.has(bits),
    }
}

/// A change to the server's settings (`PATCH /v1/server`). Only what is `Some` is sent. For the
/// two rooms, `Some("")` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyServerEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afk_room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afk_timeout_secs: Option<i32>,
    /// Images and GIFs, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_max_bytes: Option<i64>,
    /// Video and every other file, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_max_bytes: Option<i64>,
    /// Whether archives may be uploaded. `None` leaves the server's flag as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_archives: Option<bool>,
}

/// A role to make, or a change to one. Only what is `Some` is sent; `color: Some("")` clears
/// the colour.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyRoleEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hoist: Option<bool>,
}

/// A room to make, or a change to one. Only what is `Some` is sent; `category_id: Some("")`
/// moves it out of its category.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyRoomEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// 0 is the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_limit: Option<u32>,
    /// 16 to 128.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ptt_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2ee_required: Option<bool>,
}

/// The lowest and highest bitrate a room may have, and what one has unless set.
pub const TINY_BITRATE_MIN: u32 = 16;
pub const TINY_BITRATE_MAX: u32 = 128;
pub const TINY_BITRATE_DEFAULT: u32 = 64;

/// An invite with the choices a newer server takes. On a server without `invite_options` only
/// the legacy role invite (`Request::TinyInvite`) exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyInviteOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub role_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// 1 to 1000; `None` is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    /// 60 s to 30 days. `None` never expires, which only owners and server managers may ask
    /// for; `default_expiry` leaves it to the server (7 days).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub default_expiry: bool,
}

/// One group of the channel list's order: a category, or `None` for the rooms outside any
/// (always shown first), with its rooms top first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyLayoutGroup {
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub room_ids: Vec<String>,
}

/// One page of the audit, newest first. Page on by passing the last row's id as `before`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TinyAuditFilter {
    /// 1 to 200; 0 asks for 50.
    #[serde(default)]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    /// Only what this user did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Only actions starting with this, as `member.` or `role.create`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
}

/// The most characters in a display name, after trimming.
pub const TINY_NAME_MAX: usize = 80;

/// A string the server may send as `null`: someone whose provider gave no email has none.
pub fn text_or_null<'de, D: serde::Deserializer<'de>>(from: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(from)?.unwrap_or_default())
}

/// Any value the server may send as `null` or leave out: it takes its default.
pub fn or_default<'de, D: serde::Deserializer<'de>, T: Deserialize<'de> + Default>(from: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(from)?.unwrap_or_default())
}

/// A list the server sent, with each row that cannot be read left out, and how many were. Not a
/// list at all is `None`: the answer as a whole is unreadable.
pub fn tiny_rows<T: DeserializeOwned>(list: Value) -> Option<(Vec<T>, usize)> {
    let Value::Array(rows) = list else {
        return None;
    };
    let mut skipped = 0;
    let read = rows
        .into_iter()
        .filter_map(|row| match serde_json::from_value(row) {
            Ok(row) => Some(row),
            Err(_) => {
                skipped += 1;
                None
            }
        })
        .collect();
    Some((read, skipped))
}

/// The name the server would keep, or `None` when it would refuse it.
/// Unix seconds as an RFC 3339 time in UTC, the form the server takes (a timeout's end).
pub fn tiny_time(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", secs / 3_600, secs % 3_600 / 60, secs % 60)
}

/// An RFC 3339 time as unix seconds, offset applied. `None` when it does not read as one.
pub fn tiny_unix(stamp: &str) -> Option<i64> {
    let bytes = stamp.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &stamp[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        rest = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        zone if zone.len() == 6 && matches!(zone.as_bytes()[0], b'+' | b'-') && zone.as_bytes()[3] == b':' => {
            let hours: i64 = zone[1..3].parse().ok()?;
            let minutes: i64 = zone[4..6].parse().ok()?;
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            sign * (hours * 3_600 + minutes * 60)
        }
        _ => return None,
    };
    // Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

pub fn tiny_name(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= TINY_NAME_MAX).then_some(name)
}

/// One paste for the other tinyclient: `tc1.` then the origin in hex, then the server's token.
/// The server never sees this string. Signing in still sends only `token`, as the invite.
pub fn pack_join(origin: &str, token: &str) -> Option<String> {
    if origin.is_empty() || token.is_empty() {
        return None;
    }
    if origin.chars().any(char::is_whitespace) || token.chars().any(|c| c.is_whitespace() || c == '.') {
        return None;
    }
    let hex: String = origin.bytes().map(|byte| format!("{byte:02x}")).collect();
    Some(format!("tc1.{hex}.{token}"))
}

/// The origin and the server's token inside a pasted code. Whitespace is ignored.
/// `None` when it is not this code: a raw token, a typed address, or a damaged paste.
pub fn unpack_join(pasted: &str) -> Option<(String, String)> {
    let compact: String = pasted.chars().filter(|c| !c.is_whitespace()).collect();
    let rest = compact.strip_prefix("tc1.")?;
    let (hex, token) = rest.rsplit_once('.')?;
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.bytes().all(|byte| from_hex(byte).is_some()) {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let hex = hex.as_bytes();
    let mut at = 0;
    while at < hex.len() {
        let high = from_hex(hex[at])?;
        let low = from_hex(hex[at + 1])?;
        bytes.push((high << 4) | low);
        at += 2;
    }
    let origin = String::from_utf8(bytes).ok()?;
    if origin.is_empty() || origin.chars().any(char::is_whitespace) {
        return None;
    }
    Some((origin, token.to_owned()))
}

/// The maximum length of an invite payload. Invite links are bearer credentials.
pub const INVITE_PAYLOAD_MAX: usize = 2048;
const INVITE_ORIGIN_MAX: usize = 512;
const INVITE_TOKEN_MAX: usize = 256;
const INVITE_SITE_PREFIX: &str = "https://tinyclient.io/invite#";
const INVITE_APP_PREFIX: &str = "tinyclient://join#";

/// Normalize an HTTP(S) server origin without accepting credentials or URL components.
fn invite_origin(origin: &str) -> Option<String> {
    if origin.len() > INVITE_ORIGIN_MAX || origin.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
        return None;
    }
    let (_, authority) = origin.split_once("://")?;
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) || authority.ends_with(':') || authority.ends_with('.') {
        return None;
    }
    let parsed = url::Url::parse(origin).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none_or(|host| host.ends_with('.'))
        || parsed.port() == Some(0)
    {
        return None;
    }
    Some(parsed.origin().ascii_serialization())
}

/// Create a shareable link. Its fragment contains a secret; don't log the returned value.
pub fn invite_url(origin: &str, token: &str) -> Option<String> {
    let origin = invite_origin(origin)?;
    if token.is_empty() || token.len() > INVITE_TOKEN_MAX || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    let payload = pack_join(&origin, token)?;
    (payload.len() <= INVITE_PAYLOAD_MAX).then(|| format!("{INVITE_SITE_PREFIX}{payload}"))
}

/// Read a canonical site link, app link or legacy code and validate its destination.
/// Returned HTTP origins require a user-visible warning before connecting.
pub fn parse_invite_link(input: &str) -> Option<(String, String)> {
    if input.len() > INVITE_PAYLOAD_MAX + INVITE_SITE_PREFIX.len() {
        return None;
    }
    let payload = input.strip_prefix(INVITE_SITE_PREFIX)
        .or_else(|| input.strip_prefix(INVITE_APP_PREFIX))
        .unwrap_or(input);
    if payload.len() > INVITE_PAYLOAD_MAX || payload.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let (origin, token) = unpack_join(payload)?;
    if token.len() > INVITE_TOKEN_MAX {
        return None;
    }
    Some((invite_origin(&origin)?, token))
}

/// The host, and a port when it is not the usual one. `https://private.tinyclient.io` is
/// `private.tinyclient.io`. `http://127.0.0.1:8080` keeps the port.
pub fn join_host(origin: &str) -> String {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return origin.to_owned();
    };
    if rest.is_empty() {
        return origin.to_owned();
    }
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        match v6.split_once(']') {
            Some((addr, after)) => (format!("[{addr}]"), after.strip_prefix(':').unwrap_or("")),
            None => return rest.to_owned(),
        }
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) => (host.to_owned(), port),
            _ => (rest.to_owned(), ""),
        }
    };
    let usual = (scheme == "https" && port == "443") || (scheme == "http" && port == "80");
    if port.is_empty() || usual {
        host
    } else {
        format!("{host}:{port}")
    }
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether `role` may do `id`. An unknown id is refused.
pub fn tiny_can(role: &str, id: &str) -> bool {
    match id {
        "community.members.read" => matches!(role, "owner" | "admin" | "member"),
        "community.invites.read"
        | "community.invites.create"
        | "community.members.role"
        | "community.members.remove"
        | "community.invites.revoke"
        | "community.audit.read" => matches!(role, "owner" | "admin"),
        "account.name.edit" | "account.sessions.end" | "community.calls.join" => {
            matches!(role, "owner" | "admin" | "member")
        }
        "community.rooms.create" | "community.rooms.rename" | "community.rooms.delete" | "community.calls.kick" => {
            matches!(role, "owner" | "admin")
        }
        _ => false,
    }
}

/// How far what is sent to a Tiny server can be read on the way, from the address alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// `https`.
    Secure,
    /// `http` to this computer: nothing leaves it.
    Loopback,
    /// `http` to a machine that is, by its address or its name, on a network of the person's own
    /// (a private or link-local address, a Tailscale address, a bare machine name, `.local`,
    /// `.lan`, `.home.arpa`, `.internal`, `.ts.net`). Anyone on that network can read it.
    PrivateNetwork,
    /// `http` to anything else: the open internet, or a name this cannot place.
    Open,
}

/// What an address reaches. A typed host with no scheme is `https`, except a machine on this
/// computer, which is `http`, as the sign-in sheet reads it. A scheme other than `http` and
/// `https` is [`Reach::Open`]: nothing here sends over it.
pub fn reach(raw: &str) -> Reach {
    let raw = raw.trim();
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (Some(scheme.to_ascii_lowercase()), rest),
        None => (None, raw),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => match authority.rsplit_once(':') {
            Some((host, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => host,
            _ => authority,
        },
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match scheme.as_deref() {
        Some("https") => Reach::Secure,
        Some("http") => host_reach(&host),
        None if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0") => Reach::Loopback,
        None => Reach::Secure,
        Some(_) => Reach::Open,
    }
}

fn host_reach(host: &str) -> Reach {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    fn v4(ip: Ipv4Addr) -> Reach {
        let [a, b, ..] = ip.octets();
        if ip.is_loopback() || ip.is_unspecified() {
            Reach::Loopback
        } else if ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b)) {
            Reach::PrivateNetwork
        } else {
            Reach::Open
        }
    }
    fn v6(ip: Ipv6Addr) -> Reach {
        if let Some(mapped) = ip.to_ipv4_mapped() {
            return v4(mapped);
        }
        let first = ip.segments()[0];
        if ip.is_loopback() || ip.is_unspecified() {
            Reach::Loopback
        } else if first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80 {
            Reach::PrivateNetwork
        } else {
            Reach::Open
        }
    }
    if host.is_empty() {
        return Reach::Open;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => return v4(ip),
        Ok(IpAddr::V6(ip)) => return v6(ip),
        Err(_) => {}
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return Reach::Loopback;
    }
    let named_for_a_private_network = [".local", ".lan", ".home.arpa", ".internal", ".ts.net"].iter().any(|suffix| host.ends_with(suffix));
    // A bare machine name, but not a bare number: some resolvers read one as an address.
    let bare = !host.contains('.') && !host.bytes().all(|byte| byte.is_ascii_digit());
    if named_for_a_private_network || bare {
        Reach::PrivateNetwork
    } else {
        Reach::Open
    }
}

/// What to tell a person about an `http` address, or nothing for one that needs no telling.
pub fn plain_http_words(reach: Reach) -> Option<&'static str> {
    match reach {
        Reach::Secure | Reach::Loopback => None,
        Reach::PrivateNetwork => Some("This address uses http, not https. Anyone on your network between you and the server can read what you send, your sign-in included."),
        Reach::Open => Some(
            "This address uses http, not https, and the server is not on your own network. Anyone between you and it can read what you send, your sign-in included, and take over your account. Use https if the server has it.",
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_time_goes_to_rfc_3339_and_back() {
        assert_eq!(super::tiny_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::tiny_time(1_793_210_400), "2026-10-28T18:00:00Z");
        assert_eq!(super::tiny_unix("2026-10-28T18:00:00Z"), Some(1_793_210_400));
        assert_eq!(super::tiny_unix("2026-10-28T20:00:00.123+02:00"), Some(1_793_210_400));
        assert_eq!(super::tiny_unix("2024-02-29T00:00:00Z").map(super::tiny_time).as_deref(), Some("2024-02-29T00:00:00Z"));
        assert_eq!(super::tiny_unix("soon"), None);
        assert_eq!(super::tiny_unix("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn incognito_hosts_are_its_domains_and_the_names_under_them() {
        for host in ["incognito.tinyclient.io", "c-7f3k2x9q.tinycloud.chat", "tinycloud.chat", "C-7F3K2.TinyCloud.Chat", "c-1.tinycloud.chat."] {
            assert!(incognito_host(host), "{host}");
        }
        for host in ["eviltinycloud.chat", "tinycloud.chat.evil.example", "tinyclient.io", "chat.example.org", ".tinycloud.chat", "", "192.168.1.120", "localhost"] {
            assert!(!incognito_host(host), "{host}");
        }
    }

    use super::*;

    #[test]
    fn invite_links_round_trip_site_app_and_legacy_payload() {
        let url = invite_url("https://Chat.Example:8443", "tok-en_1").unwrap();
        assert!(url.starts_with("https://tinyclient.io/invite#tc1."));
        let payload = url.split_once('#').unwrap().1;
        let expected = Some(("https://chat.example:8443".to_owned(), "tok-en_1".to_owned()));
        assert_eq!(parse_invite_link(&url), expected);
        assert_eq!(parse_invite_link(&format!("tinyclient://join#{payload}")), expected);
        assert_eq!(parse_invite_link(payload), expected);
        let local = invite_url("http://[::1]:8080", "a").unwrap();
        assert_eq!(parse_invite_link(&local), Some(("http://[::1]:8080".into(), "a".into())));
    }

    #[test]
    fn invite_links_reject_unsafe_origins_and_untrusted_wrappers() {
        for origin in ["ftp://host", "https://user@host", "https://host/path", "https://host?x=1", "https://host#x", "https://host:0", "https://host:99999", "https://[bad]:80", "https://host\\evil", "https://host:", "https://host."] {
            assert!(invite_url(origin, "token").is_none(), "{origin}");
        }
        assert!(invite_url("https://host", "has.dot").is_none());
        assert!(invite_url("https://host", &"a".repeat(257)).is_none());
        let code = pack_join("https://host", "token").unwrap();
        assert!(parse_invite_link(&format!("https://evil.example/invite#{code}")).is_none());
        assert!(parse_invite_link(&format!("https://tinyclient.io/invite?x=1#{code}")).is_none());
        assert!(parse_invite_link(&"a".repeat(2049)).is_none());
        assert!(parse_invite_link("tc1.zz.token").is_none());
    }

    #[test]
    fn tiny_can_matrix() {
        assert!(tiny_can("member", "community.members.read"));
        assert!(tiny_can("admin", "community.members.read"));
        assert!(tiny_can("owner", "community.members.read"));
        assert!(!tiny_can("guest", "community.members.read"));
        for id in [
            "community.invites.read",
            "community.invites.create",
            "community.invites.revoke",
            "community.members.role",
            "community.members.remove",
            "community.audit.read",
            "community.rooms.create",
            "community.rooms.rename",
            "community.rooms.delete",
            "community.calls.kick",
        ] {
            assert!(!tiny_can("member", id), "{id}");
            assert!(tiny_can("admin", id), "{id}");
            assert!(tiny_can("owner", id), "{id}");
            assert!(!tiny_can("someone", id), "{id}");
        }
        assert!(!tiny_can("owner", "community.nope"));
        assert!(!tiny_can("admin", ""));
        assert!(!tiny_can("member", "community.invites.create"));
        for id in ["account.name.edit", "account.sessions.end", "community.calls.join"] {
            for role in ["owner", "admin", "member"] {
                assert!(tiny_can(role, id), "{role} {id}");
            }
            assert!(!tiny_can("guest", id), "{id}");
        }
    }

    #[test]
    fn a_name_is_trimmed_and_an_email_may_be_null() {
        assert_eq!(tiny_name("  Ada  "), Some("Ada"));
        assert_eq!(tiny_name("   "), None);
        assert!(tiny_name(&"é".repeat(80)).is_some());
        assert_eq!(tiny_name(&"a".repeat(81)), None);
        let member: TinyMember = serde_json::from_str(
            r#"{"user_id":"u","email":null,"display_name":"Ada","role":"member","created_at":"t"}"#,
        )
        .unwrap();
        assert_eq!(member.email, "");
        let member: TinyMember = serde_json::from_str(r#"{"user_id":"u","display_name":"Ada","role":"member","created_at":"t"}"#).unwrap();
        assert_eq!(member.email, "");
    }

    #[test]
    fn a_join_code_carries_the_server_and_an_older_answer_still_reads() {
        let origin = "https://private.tinyclient.io";
        let token = "abcdefghijklmnopqrstuvwxyz0123456789-_ABC";
        let packed = pack_join(origin, token).unwrap();
        assert!(packed.starts_with("tc1."));
        assert!(!packed.contains(origin));
        assert_eq!(unpack_join(&packed), Some((origin.to_owned(), token.to_owned())));
        let wrapped = format!("  {}\n{}", &packed[..20], &packed[20..]);
        assert_eq!(unpack_join(&wrapped), Some((origin.to_owned(), token.to_owned())));
        assert_eq!(unpack_join(token), None);
        assert_eq!(unpack_join("https://private.tinyclient.io"), None);
        assert_eq!(pack_join("", token), None);
        assert_eq!(pack_join(origin, "has.dot"), None);
        assert!(pack_join("http://127.0.0.1:8080", token).is_some());
        assert_eq!(join_host(origin), "private.tinyclient.io");
        assert_eq!(join_host("https://private.tinyclient.io:443"), "private.tinyclient.io");
        assert_eq!(join_host("http://127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(join_host("http://127.0.0.1:80"), "127.0.0.1");
        assert_eq!(join_host("http://[::1]:8080"), "[::1]:8080");
        assert_eq!(join_host("https://chat.example:8443"), "chat.example:8443");
        let old: TinyCode = serde_json::from_str(r#"{"id":"1","token":"t","role":"member","expires_at":"t"}"#).unwrap();
        assert_eq!(old.origin, "");
        let community: TinyCommunity = serde_json::from_str(r#"{"role":"owner","members":[],"invites":[],"audit":[]}"#).unwrap();
        assert_eq!(community.origin, "");
        assert!(community.rooms.is_empty() && community.online.is_empty() && community.media.is_empty());
        // Nothing new is written while it is empty, so an older shell reads the same frame.
        let json = serde_json::to_string(&community).unwrap();
        assert!(!json.contains("rooms") && !json.contains("online") && !json.contains("media"), "{json}");
        let room: TinyRoom = serde_json::from_str(r#"{"id":"r","name":"Lobby"}"#).unwrap();
        assert!(room.participants.is_empty());
        assert!(!serde_json::to_string(&room).unwrap().contains("participants"));
    }

    /// The owner invite the server prints at setup has no creator. It once failed the whole
    /// list, and with it the page.
    #[test]
    fn an_invite_the_server_made_itself_reads_and_one_bad_row_is_left_out() {
        let setup: TinyInvite = serde_json::from_str(r#"{"id":"1","role":"owner","created_by":null,"expires_at":"t"}"#).unwrap();
        assert_eq!(setup.created_by, "");
        let newer: TinyInvite = serde_json::from_str(
            r#"{"id":"2","role":null,"created_by":"a","created_by_name":"Ada","label":"for Bea","uses":0,"max_uses":null,"expires_at":"t","shiny":{"x":1}}"#,
        )
        .unwrap();
        assert_eq!((newer.role.as_str(), newer.created_by.as_str()), ("", "a"));
        let list = serde_json::json!([
            {"id":"1","role":"owner","created_by":null,"expires_at":"t"},
            {"role":"member","created_by":"a","expires_at":"t"},
            {"id":"3","role":"member","created_by":"a","expires_at":"t","extra":true},
            "not a row"
        ]);
        let (rows, skipped) = tiny_rows::<TinyInvite>(list).unwrap();
        assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["1", "3"]);
        assert_eq!(skipped, 2);
        assert!(tiny_rows::<TinyInvite>(serde_json::json!({"error": "no"})).is_none());
        let member: TinyMember = serde_json::from_str(r#"{"user_id":"u","display_name":null,"role":null,"created_at":null,"owner":true}"#).unwrap();
        assert_eq!((member.display_name.as_str(), member.role.as_str()), ("", ""));
        let audit: TinyAudit = serde_json::from_str(r#"{"id":4,"actor_id":null,"action":"server.bootstrap","subject":null,"detail":null,"created_at":"t","actor_name":null}"#).unwrap();
        assert_eq!((audit.subject.as_str(), audit.detail.as_str()), ("", ""));
    }

    fn role(id: &str, position: i32, permissions: u64) -> TinyRole {
        TinyRole { id: id.into(), name: id.into(), position, permissions, default: id == "everyone", ..TinyRole::default() }
    }

    #[test]
    fn room_permissions_apply_everyones_override_then_the_members_roles() {
        let roles = [role("everyone", 0, TINY_EVERYONE_DEFAULT), role("mod", 2, TINY_MUTE_MEMBERS), role("dj", 1, 0)];
        let mine = vec!["mod".to_owned(), "dj".to_owned()];
        assert_eq!(tiny_room_permissions(&roles, &mine, false, &[]), TINY_EVERYONE_DEFAULT | TINY_MUTE_MEMBERS);
        // @everyone may not speak here; the DJ role may; the moderator role may not move.
        let overrides = [
            TinyOverride { role_id: "everyone".into(), allow: 0, deny: TINY_SPEAK | TINY_VIEW_CHANNEL },
            TinyOverride { role_id: "dj".into(), allow: TINY_SPEAK | TINY_PRIORITY_SPEAKER, deny: 0 },
            TinyOverride { role_id: "mod".into(), allow: 0, deny: TINY_SPEAK },
        ];
        let bits = tiny_room_permissions(&roles, &mine, false, &overrides);
        assert_eq!(bits & TINY_SPEAK, TINY_SPEAK, "a role's allow wins over another role's deny");
        assert_eq!(bits & TINY_VIEW_CHANNEL, 0, "@everyone's deny stands where no role allows it");
        assert_eq!(bits & TINY_PRIORITY_SPEAKER, TINY_PRIORITY_SPEAKER);
        let plain = tiny_room_permissions(&roles, &[], false, &overrides);
        assert_eq!(plain & (TINY_SPEAK | TINY_VIEW_CHANNEL), 0);
        assert_eq!(tiny_room_permissions(&roles, &[], true, &overrides), TINY_ALL, "owners have every bit");
        // An override for a role the member does not have changes nothing.
        let other = [TinyOverride { role_id: "dj".into(), allow: TINY_MANAGE_CHANNELS, deny: 0 }];
        assert_eq!(tiny_room_permissions(&roles, &["mod".into()], false, &other), TINY_EVERYONE_DEFAULT | TINY_MUTE_MEMBERS);
    }

    #[test]
    fn rank_decides_who_may_act_on_whom() {
        let roles = [role("everyone", 0, TINY_EVERYONE_DEFAULT), role("admin", 5, TINY_KICK_MEMBERS), role("mod", 2, TINY_KICK_MEMBERS)];
        let member = |id: &str, roles: &[&str], owner: bool| TinyMember {
            user_id: id.into(),
            owner,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            ..TinyMember::default()
        };
        assert_eq!(tiny_rank(&roles, &["mod".into(), "admin".into()], false), 5);
        assert_eq!(tiny_rank(&roles, &[], false), 0);
        assert_eq!(tiny_rank(&roles, &[], true), i32::MAX);
        let moderator = TinyAccess { owner: false, roles: vec!["mod".into()], permissions: TINY_KICK_MEMBERS, rank: 2 };
        assert!(moderator.outranks("me", &member("a", &[], false), &roles));
        assert!(!moderator.outranks("me", &member("b", &["mod"], false), &roles), "not a peer");
        assert!(!moderator.outranks("me", &member("c", &["admin"], false), &roles), "not a senior");
        assert!(!moderator.outranks("me", &member("me", &[], false), &roles), "not themselves");
        assert!(!moderator.outranks("me", &member("d", &[], true), &roles), "never an owner");
        assert!(moderator.may_assign(&roles[0]) && !moderator.may_assign(&roles[2]));
        let owner = TinyAccess { owner: true, ..TinyAccess::default() };
        assert!(owner.outranks("me", &member("c", &["admin"], false), &roles));
        assert!(!owner.outranks("me", &member("d", &[], true), &roles));
    }

    #[test]
    fn permissions_decide_on_a_newer_server_and_the_role_on_an_older_one() {
        for role in ["owner", "admin", "member", "guest"] {
            for id in ["community.invites.create", "community.calls.kick", "community.rooms.create", "community.calls.join", "community.server.edit"] {
                assert_eq!(tiny_allowed(None, role, id), tiny_can(role, id), "{role} {id}");
            }
        }
        let kicker = TinyAccess { permissions: TINY_EVERYONE_DEFAULT | TINY_KICK_MEMBERS, rank: 1, ..TinyAccess::default() };
        assert!(tiny_allowed(Some(&kicker), "member", "community.members.remove"), "a custom role, whatever the legacy role says");
        assert!(tiny_allowed(Some(&kicker), "member", "community.calls.join"));
        assert!(!tiny_allowed(Some(&kicker), "admin", "community.rooms.create"), "the bits decide, not the legacy role");
        assert!(!tiny_allowed(Some(&kicker), "member", "community.members.owner"));
        assert!(!tiny_allowed(Some(&kicker), "member", "community.nope"));
        let creator = TinyAccess { permissions: TINY_CREATE_INVITES, ..TinyAccess::default() };
        assert!(tiny_allowed(Some(&creator), "member", "community.invites.read"));
        assert!(tiny_allowed(Some(&creator), "member", "community.invites.revoke"), "their own; the page checks which");
        assert!(!tiny_allowed(Some(&creator), "member", "community.invites.roles"));
        let owner = TinyAccess { owner: true, ..TinyAccess::default() };
        assert!(tiny_allowed(Some(&owner), "member", "community.members.owner"));
        assert!(tiny_allowed(Some(&owner), "member", "community.server.edit"));
        assert!(!tiny_allowed(Some(&owner), "owner", "community.nope"));
        let room = TinyRoom { my_permissions: Some(TINY_VIEW_CHANNEL), ..TinyRoom::new("r", "Lobby") };
        assert!(!kicker.has_in(&room, TINY_CONNECT));
        assert!(kicker.has_in(&TinyRoom::new("r", "Lobby"), TINY_CONNECT), "an older room: the server-wide bits");
        assert!(owner.has_in(&room, TINY_CONNECT));
    }

    #[test]
    fn a_newer_server_reads_in_full_and_an_older_one_reads_as_before() {
        let room: TinyRoom = serde_json::from_str(
            r#"{"id":"r","name":"Lounge","category_id":"c","position":2,"topic":null,"user_limit":12,"bitrate_kbps":200,"ptt_only":true,"video_enabled":false,"overrides":[{"role_id":"e","allow":4,"deny":1}],"my_permissions":3}"#,
        )
        .unwrap();
        assert_eq!((room.user_limit, room.bitrate(), room.ptt_only, room.video_enabled), (12, TINY_BITRATE_MAX, true, false));
        assert_eq!(room.overrides, [TinyOverride { role_id: "e".into(), allow: 4, deny: 1 }]);
        let old = TinyRoom::new("r", "Lobby");
        assert_eq!(serde_json::from_str::<TinyRoom>(r#"{"id":"r","name":"Lobby","video_enabled":null}"#).unwrap(), old);
        assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"id":"r","name":"Lobby"}"#, "an older shell reads the same frame");
        assert_eq!(old.bitrate(), TINY_BITRATE_DEFAULT);
        let seat: TinySeat = serde_json::from_str(r#"{"user_id":"u","server_muted":true,"priority":true,"suppressed":false}"#).unwrap();
        assert!(seat.server_muted && seat.priority && !seat.server_deafened);
        let community: TinyCommunity = serde_json::from_str(r#"{"role":"member","members":[],"invites":[],"audit":[]}"#).unwrap();
        assert!(community.access.is_none() && community.server.is_none() && community.capabilities.is_empty());
        let json = serde_json::to_string(&community).unwrap();
        assert!(!json.contains("access") && !json.contains("capabilities") && !json.contains("roles"), "{json}");
        let info: TinyServerInfo = serde_json::from_str(r#"{"name":"AtomicStudio","description":null,"icon":null,"afk_timeout_secs":null,"new":1}"#).unwrap();
        assert_eq!((info.name.as_str(), info.icon, info.afk_timeout_secs), ("AtomicStudio", None, 0));
        assert_eq!((info.image_max_bytes, info.file_max_bytes), (TINY_IMAGE_MAX_DEFAULT, TINY_FILE_MAX_DEFAULT));
        assert!(!info.allow_archives, "a missing flag is off");
        let archives: TinyServerInfo = serde_json::from_str(r#"{"name":"AtomicStudio","allow_archives":true}"#).unwrap();
        assert!(archives.allow_archives);
        let roles: Vec<TinyRole> = serde_json::from_str(r##"[{"id":"e","name":"@everyone","color":null,"position":0,"permissions":63,"hoist":false,"default":true,"member_count":4}]"##).unwrap();
        assert!(roles[0].default && roles[0].permissions == TINY_EVERYONE_DEFAULT);
        let capabilities: TinyCapabilities = serde_json::from_str(r#"{"version":"0.3.0","features":["server","bans"]}"#).unwrap();
        assert!(capabilities.has(TINY_FEATURE_BANS) && !capabilities.has(TINY_FEATURE_ROLES));
        let options = TinyInviteOptions { max_uses: Some(5), ..TinyInviteOptions::default() };
        assert_eq!(serde_json::to_string(&options).unwrap(), r#"{"max_uses":5}"#);
        let names: Vec<&str> = TINY_PERMISSIONS.iter().map(|(_, name)| *name).collect();
        assert_eq!(names.len(), 20);
        assert!(TINY_PERMISSIONS.iter().enumerate().all(|(n, (bit, _))| *bit == 1 << n));
    }

    #[test]
    fn an_invite_omits_an_acceptance_it_does_not_have() {
        let invite = TinyInvite { id: "1".into(), role: "member".into(), created_by: "a".into(), expires_at: "t".into(), ..TinyInvite::default() };
        let json = serde_json::to_string(&invite).unwrap();
        assert!(!json.contains("accepted"));
        let back: TinyInvite = serde_json::from_str(r#"{"id":"1","role":"member","created_by":"a","expires_at":"t"}"#).unwrap();
        assert_eq!(back, invite);
    }

    #[test]
    fn an_address_reaches_as_far_as_its_scheme_and_host_say() {
        use super::Reach::*;
        let cases = [
            ("https://chat.example", Secure),
            ("HTTPS://chat.example:8443/x", Secure),
            ("chat.example", Secure),
            ("chat.example:8080", Secure),
            ("http://localhost:8080", Loopback),
            ("http://127.0.0.1:8080", Loopback),
            ("http://127.9.9.9", Loopback),
            ("http://[::1]:8080", Loopback),
            ("http://0.0.0.0:8080", Loopback),
            ("http://dev.localhost", Loopback),
            ("localhost", Loopback),
            ("127.0.0.1:8080", Loopback),
            ("[::1]:8080", Loopback),
            ("http://192.168.1.120", PrivateNetwork),
            ("http://10.0.0.5:8080", PrivateNetwork),
            ("http://172.16.0.1", PrivateNetwork),
            ("http://172.31.255.254", PrivateNetwork),
            ("http://169.254.10.10", PrivateNetwork),
            ("http://100.73.171.76", PrivateNetwork),
            ("http://100.127.101.103", PrivateNetwork),
            ("http://[fd12:3456::1]", PrivateNetwork),
            ("http://[fe80::1]", PrivateNetwork),
            ("http://[::ffff:192.168.1.5]", PrivateNetwork),
            ("http://atomicstudio", PrivateNetwork),
            ("http://atomicstudio:8080", PrivateNetwork),
            ("http://printer.local", PrivateNetwork),
            ("http://nas.lan", PrivateNetwork),
            ("http://box.home.arpa", PrivateNetwork),
            ("http://svc.internal", PrivateNetwork),
            ("http://atomicstudio.tail1234.ts.net", PrivateNetwork),
            ("http://172.32.0.1", Open),
            ("http://100.128.0.1", Open),
            ("http://100.63.0.1", Open),
            ("http://8.8.8.8", Open),
            ("http://chat.example", Open),
            ("http://chat.example:80", Open),
            ("http://localhost.example.com", Open),
            ("http://192.168.1.1.example.com", Open),
            ("http://[2001:db8::1]", Open),
            ("http://2130706433", Open),
            ("http://127.1", Open),
            ("http://", Open),
            ("ftp://chat.example", Open),
        ];
        for (address, expected) in cases {
            assert_eq!(super::reach(address), expected, "{address}");
        }
        // A user name does not move the host: what is before the `@` is not the server.
        assert_eq!(super::reach("http://192.168.1.1@chat.example"), Open);
        assert_eq!(super::reach("http://chat.example@192.168.1.1"), PrivateNetwork);
    }

    #[test]
    fn only_a_plain_http_address_off_this_computer_is_warned_about_and_the_open_one_says_so_plainly() {
        use super::Reach::*;
        assert_eq!(super::plain_http_words(Secure), None);
        assert_eq!(super::plain_http_words(Loopback), None);
        let own = super::plain_http_words(PrivateNetwork).unwrap();
        assert!(own.contains("http, not https") && own.contains("your network"));
        let open = super::plain_http_words(Open).unwrap();
        assert!(open.contains("http, not https") && open.contains("not on your own network") && open.contains("take over your account"));
    }
}
