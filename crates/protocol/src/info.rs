//! What the polish round added beside the rows: how a network files its conversations (folders,
//! pins, archive), what a person is doing, the typed card and info panes, what an account may
//! send, the pickers' catalogs, and what chatd is doing with the keychain.
//!
//! Every field here is optional on the wire and every list has a cap, as in the rest of the
//! contract.

use crate::{is_false, is_zero, is_zero64, Conversation, Key, MediaFile, Message, Network, Space};
use serde::{Deserialize, Serialize};

/// Most folders an account lists. Telegram allows 30 with Premium; Discord's server folders are
/// the user's own and rarely more than a few dozen.
pub const FOLDERS_LIMIT: usize = 64;
/// Most folders one conversation names.
pub const FOLDERS_PER_ROW: usize = 32;
/// Most Spaces, or pinned conversations, one folder names.
pub const FOLDER_MEMBERS: usize = 200;
/// Most rows the shell may name as on screen in `Watching::rows`.
pub const ROWS_WATCHED: usize = 100;
/// Most roles, or contact lines, a profile carries.
pub const PROFILE_ROLES: usize = 12;
/// Discord permission bits the Roles page labels. Values are Discord's; keep this table when
/// Discord adds a bit the page should name. Bits absent here still round-trip as a number.
pub const DISCORD_ROLE_PERMISSIONS: &[(u64, &str)] = &[
    (1 << 0, "Create invite"),
    (1 << 1, "Kick members"),
    (1 << 2, "Ban members"),
    (1 << 3, "Administrator"),
    (1 << 4, "Manage channels"),
    (1 << 5, "Manage server"),
    (1 << 6, "Add reactions"),
    (1 << 10, "View channels"),
    (1 << 11, "Send messages"),
    (1 << 13, "Manage messages"),
    (1 << 14, "Embed links"),
    (1 << 15, "Attach files"),
    (1 << 16, "Read message history"),
    (1 << 17, "Mention @everyone"),
    (1 << 20, "Connect"),
    (1 << 21, "Speak"),
    (1 << 24, "Move members"),
    (1 << 26, "Change nickname"),
    (1 << 27, "Manage nicknames"),
    (1 << 28, "Manage roles"),
    (1 << 29, "Manage webhooks"),
    (1 << 31, "Use application commands"),
    (1 << 33, "Manage events"),
    (1 << 34, "Manage threads"),
    (1 << 40, "Timeout members"),
    (1 << 51, "Pin messages"),
];
pub const PROFILE_CONTACTS: usize = 6;
/// Most badges or connected accounts a profile carries.
pub const PROFILE_BADGES: usize = 24;
pub const PROFILE_CONNECTIONS: usize = 16;
/// Most files one `Send` carries. A network that takes fewer says so in `Capabilities`.
pub const ATTACHMENTS_LIMIT: usize = 10;
/// Most items one catalog answer carries.
pub const CATALOG_LIMIT: usize = 200;
/// Most pinned messages one `Pinned` answer carries.
pub const PINNED_LIMIT: usize = 50;
/// Most pinned messages an answer carries from a network that pages its pins (Discord keeps
/// 250 a channel, 50 a page).
pub const PINNED_PAGED_LIMIT: usize = 250;
/// Most people each section of `Around` lists.
pub const AROUND_LIMIT: usize = 100;
/// Most channel or chat tabs listed in [`ConversationInfo::tabs`].
pub const TABS_LIMIT: usize = 32;
/// Most meetings one `Meetings` answer carries. Today's list, soonest first.
pub const MEETINGS_LIMIT: usize = 32;

fn is_true(value: &bool) -> bool {
    *value
}

fn yes() -> bool {
    true
}

/// A lightweight person reference on a profile (e.g. a Teams manager or direct report).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePerson {
    pub key: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

/// One mentionable tag in a Space (e.g. a Teams team tag such as `@On-Call`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamTag {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub member_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<ProfilePerson>,
    /// True when the signed-in user belongs to this tag.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mine: bool,
}

/// Automatic replies / out-of-office settings for an account (`Request::GetOutOfOffice`, `Request::SetOutOfOffice`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutOfOffice {
    /// True when automatic replies are turned on (always or scheduled).
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    /// Internal reply message (shown to colleagues in Teams and Outlook).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Optional scheduled start (Unix seconds, UTC). Both `start` and `end` set schedules the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// Optional scheduled end (Unix seconds, UTC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    /// Whether automatic replies are also sent to people outside the organization.
    #[serde(default, skip_serializing_if = "is_false")]
    pub send_external: bool,
    /// Reply message sent to external senders when `send_external` is true (defaults to `message` when empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub external_message: String,
}

/// Which account privacy setting to read or update (`Request::GetPrivacy`, `Request::SetPrivacy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyKey {
    PhoneCall,
    PhoneP2p,
    VoiceMessages,
    /// Who can see when this account was last online (`inputPrivacyKeyStatusTimestamp`).
    LastSeen,
    /// Who can see this account's phone number (`inputPrivacyKeyPhoneNumber`).
    PhoneNumber,
    /// Who can see this account's profile photo (`inputPrivacyKeyProfilePhoto`).
    ProfilePhoto,
    /// Who can add this account to groups and channels (`inputPrivacyKeyChatInvite`).
    ChatInvite,
}

/// Base audience for a network privacy rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyAudience {
    #[default]
    Everybody,
    Contacts,
    Nobody,
}

/// One person or chat exception in a privacy rule list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyException {
    /// Peer id as the adapter spells it (`u<id>`, `g<id>`, `c<id>`).
    pub id: String,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// True for a group/channel chat exception; false for a user exception.
    #[serde(default, skip_serializing_if = "is_false")]
    pub chat: bool,
}

/// Account-level privacy & safety settings (Discord Privacy & Safety / Content & Social).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPrivacy {
    /// Anyone on the network may send a friend request.
    #[serde(default = "yes")]
    pub friend_requests_everyone: bool,
    /// Friends of friends may send a friend request.
    #[serde(default = "yes")]
    pub friend_requests_mutual_friends: bool,
    /// Server members may send a friend request.
    #[serde(default = "yes")]
    pub friend_requests_mutual_spaces: bool,
    /// Allow direct messages from server members by default.
    #[serde(default = "yes")]
    pub allow_dms_from_spaces: bool,
    /// Allow message requests from non-friends in servers by default.
    #[serde(default = "yes")]
    pub allow_message_requests: bool,
    /// Explicit-content filter level (0 = off, 1 = non-friends, 2 = everyone).
    #[serde(default)]
    pub explicit_content_filter: u8,
    /// DM spam filter level (0 = off, 1 = non-friends, 2 = everyone).
    #[serde(default)]
    pub dm_spam_filter: u8,
    /// Server ids where DMs from members are restricted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restricted_spaces: Vec<String>,
    /// Server ids where message requests from members are restricted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_restricted_spaces: Vec<String>,
}

impl Default for NetworkPrivacy {
    fn default() -> NetworkPrivacy {
        NetworkPrivacy {
            friend_requests_everyone: true,
            friend_requests_mutual_friends: true,
            friend_requests_mutual_spaces: true,
            allow_dms_from_spaces: true,
            allow_message_requests: true,
            explicit_content_filter: 0,
            dm_spam_filter: 0,
            restricted_spaces: Vec::new(),
            request_restricted_spaces: Vec::new(),
        }
    }
}

/// Per-space privacy switches (Discord per-server Privacy Settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpacePrivacy {
    #[serde(default = "yes")]
    pub allow_dms: bool,
    #[serde(default = "yes")]
    pub allow_requests: bool,
}

impl Default for SpacePrivacy {
    fn default() -> SpacePrivacy {
        SpacePrivacy { allow_dms: true, allow_requests: true }
    }
}

/// Current privacy policy for one [`PrivacyKey`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyPolicy {
    pub audience: PrivacyAudience,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_allow: Vec<PrivacyException>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub never_allow: Vec<PrivacyException>,
    /// True when changing this key requires a paid tier (e.g. Telegram Premium for VoiceMessages).
    #[serde(default, skip_serializing_if = "is_false")]
    pub premium_only: bool,
}

fn disabled_preset() -> bool {
    true
}

/// One connection class of Telegram's `autoDownloadSettings` (roaming, mobile, or Wi-Fi).
/// A missing preset is off, so an older frame never starts fetching full files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDownloadPreset {
    #[serde(default = "disabled_preset")]
    pub disabled: bool,
    #[serde(default)]
    pub photo_size_max: i64,
    #[serde(default)]
    pub video_size_max: i64,
    #[serde(default)]
    pub file_size_max: i64,
}

impl Default for AutoDownloadPreset {
    fn default() -> Self {
        Self { disabled: true, photo_size_max: 0, video_size_max: 0, file_size_max: 0 }
    }
}

/// Telegram's three auto-download presets, plus the local chat-kind gates this app keeps.
/// Size limits are Telegram's (`account.getAutoDownloadSettings`). Which chats may use them
/// is local and stays off until the account turns a kind on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDownload {
    #[serde(default)]
    pub roaming: AutoDownloadPreset,
    #[serde(default)]
    pub mobile: AutoDownloadPreset,
    #[serde(default)]
    pub wifi: AutoDownloadPreset,
    /// Fetch a photo's full size when the active preset's photo cap allows it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fetch_full_photos: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub private_chats: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub groups: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub channels: bool,
}

impl Default for AutoDownload {
    fn default() -> Self {
        Self {
            roaming: AutoDownloadPreset::default(),
            mobile: AutoDownloadPreset::default(),
            wifi: AutoDownloadPreset::default(),
            fetch_full_photos: false,
            private_chats: false,
            groups: false,
            channels: false,
        }
    }
}

/// A channel's boost level, from `premium.getBoostsStatus`.
/// `slots` are the free boosts this Premium account can still give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelBoosts {
    #[serde(default)]
    pub level: i32,
    #[serde(default)]
    pub boosts: i32,
    #[serde(default)]
    pub current_level_boosts: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_level_boosts: Option<i32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub my_boost: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub boost_url: String,
    /// The signed-in account is Telegram Premium, so it may give a boost.
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_boost: bool,
}

/// One command a bot lists for `/` completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCommand {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A bot's description, commands, and menu button, from `users.getFullUser`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotMenu {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<BotCommand>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub menu_label: String,
    /// An `http` or `https` menu button. Anything else is left empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub menu_url: String,
}

/// Cached files one conversation is using, when those files can be attributed to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCache {
    pub conversation: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub bytes: u64,
}

/// Which list of a network's home `Home` reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shelf {
    /// The chats: neither archived nor requests.
    #[default]
    Main,
    /// The network's archive (Telegram).
    Archive,
    /// Message requests from strangers (Discord): never counted, opening one reads nothing.
    Requests,
}

impl Shelf {
    pub fn is_main(&self) -> bool {
        *self == Shelf::Main
    }
}

/// What a conversation is besides its kind. Each changes what the row may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Special {
    #[default]
    None,
    /// The user's own notes: Telegram's Saved Messages, Teams' Notes, a Slack DM with oneself.
    /// No presence, no call.
    Saved,
    /// The network's own account: Telegram's 777000 (login codes), Discord's system DMs.
    /// Read-only, never a call, never muted by tinyclient.
    Service,
    /// A message request from a stranger. Listed only behind the Requests row, never counted,
    /// and opening it reads nothing on the network.
    Request,
}

impl Special {
    pub fn is_none(&self) -> bool {
        *self == Special::None
    }
}

/// The one tag after a name. Scam and Fake are drawn in `danger` wherever the name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Verified,
    Scam,
    Fake,
    /// A guest of this workspace or tenant.
    Guest,
    /// From another organization (Slack Connect, a Teams federated user, a shared channel).
    External,
    /// The account was deactivated: drawn greyed, never in People.
    Deactivated,
}

/// What someone is doing now. Live, like presence: never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Playing,
    Listening,
    Watching,
    Streaming,
    Competing,
    /// In a voice channel. `name` is the channel, `place` its Space.
    InVoice,
    /// In a direct call.
    InCall,
    InMeeting,
    Presenting,
    /// In a Slack huddle.
    Huddle,
}

impl ActivityKind {
    /// Talking to someone right now: People's "In a call" section.
    pub fn in_call(self) -> bool {
        matches!(
            self,
            ActivityKind::InVoice | ActivityKind::InCall | ActivityKind::InMeeting | ActivityKind::Presenting | ActivityKind::Huddle
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub kind: ActivityKind,
    /// The game, the song's service, the channel: "Hades", "Spotify", "#trail". At most 128
    /// characters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Where, when it is somewhere: the Space of a voice channel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub place: String,
    /// The conversation to open for it (the voice channel), where there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Key>,
    /// Extra detail (e.g. game zone or song title) shown on profiles, never in list rows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub details: String,
    /// Secondary detail (e.g. artist or party info) shown on profiles, never in list rows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// Unix seconds when the activity started, for elapsed time on profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<i64>,
    /// Validated `http://` or `https://` URL for streaming activities.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Album art or activity thumbnail (e.g. Spotify cover), shown on profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art: Option<MediaFile>,
}

impl Activity {
    pub fn new(kind: ActivityKind, name: impl Into<String>) -> Activity {
        Activity {
            kind,
            name: name.into(),
            place: String::new(),
            conversation: None,
            details: String::new(),
            state: String::new(),
            started: None,
            url: String::new(),
            art: None,
        }
    }

    pub fn bound(&mut self) {
        crate::clip(&mut self.name, 128);
        crate::clip(&mut self.place, 128);
        crate::clip(&mut self.details, 128);
        crate::clip(&mut self.state, 128);
        crate::clip(&mut self.url, 512);
    }
}

/// How much of a conversation notifies the user. The network's own setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    #[default]
    All,
    Mentions,
    /// Muted.
    Nothing,
}

/// One folder of an account. On Telegram a folder is a filter over the chat list: a chat is in
/// every folder that names it (`Conversation::folders`). On Discord a server folder groups
/// Spaces (`spaces`). `unread` and `mentions` are filled by chatd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// The account and the network's id of the folder.
    pub key: Key,
    /// May be empty: an unnamed Discord folder is shown as its first three servers' names.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The network's order of folders.
    pub position: i32,
    /// `0xRRGGBB`, where the network gives folders a colour (Discord).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// The Spaces it holds, in the user's order (Discord), at most [`FOLDER_MEMBERS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<String>,
    /// Conversations in it with unread messages that are not muted: what Telegram counts on a
    /// folder chip. For a folder of Spaces, the conversations of those Spaces.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unread: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mentions: u32,
}

/// A Space we share with the person on a profile, with their server nickname there if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutualSpace {
    pub key: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nickname: String,
}
/// Specification for creating or editing a chat folder (Telegram `dialogFilter`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSpec {
    /// Existing folder id when editing; `None` creates a new folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Folder title (max 12 characters on Telegram).
    pub name: String,
    /// Optional icon / emoticon label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoticon: Option<String>,
    /// Explicit chat IDs always included in the folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Explicit chat IDs excluded from the folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Pinned chat IDs in this folder, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub contacts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub non_contacts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub groups: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub broadcasts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bots: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exclude_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exclude_read: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exclude_archived: bool,
}

/// A suggested chat folder offered by the network (Telegram `messages.getSuggestedDialogFilters`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedFolder {
    pub spec: FolderSpec,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// How a phone number or address on a profile is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactKind {
    Email,
    Phone,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub kind: ContactKind,
    pub value: String,
    /// Drawn masked ("+44 •••• ••42 · Show") until clicked, each time. Phone numbers always.
    #[serde(default, skip_serializing_if = "is_false")]
    pub masked: bool,
}

/// One badge on a person's profile (Nitro, HypeSquad, Server Booster, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileBadge {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<MediaFile>,
}

/// One external account linked on a person's profile (GitHub, X, Spotify, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConnection {
    pub service: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub verified: bool,
}

/// The one heads-up line on a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    Scam,
    Fake,
    /// The network says impersonation or spam about this account.
    Spam,
}

/// How a forum's posts are ordered. Activity is Discord's `last_message_time`; Created is
/// `creation_time`. Both are asked newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TopicSort {
    #[default]
    Activity,
    Created,
}

/// One tag defined on a forum or media channel (`ForumInfo::tags`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumTag {
    pub id: String,
    pub name: String,
    /// Unicode emoji or custom emoji name/id when the tag carries one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub emoji: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub moderated: bool,
}

/// Guidelines, available tags, and posting rules of a forum or media channel (`Request::ForumInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumInfo {
    pub forum: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guidelines: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<ForumTag>,
    /// True when every new post must carry at least one tag (Discord channel flags bit 4).
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_tag: bool,
    /// True for a media channel (Discord channel type 16), where posts require an attachment.
    #[serde(default, skip_serializing_if = "is_false")]
    pub media: bool,
    /// Default per-post slowmode in seconds, when set on the forum.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub default_slowmode: u32,
}

/// A conversation's info pane: what the row does not carry, asked of the network once when the
/// pane opens and kept five minutes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationInfo {
    pub conversation: Conversation,
    /// The topic or about text in full, at most 4,000 characters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// How many are in it, and how many of them are online, where the network counts. The
    /// `@everyone` / `@channel` confirm uses `members`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<u32>,
    /// Unix seconds, and who made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creator: String,
    #[serde(default)]
    pub notify: Notify,
    /// How many messages are pinned, where the network says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<u32>,
    /// The other organization of a shared channel (Slack Connect, a Teams shared channel).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub external: String,
    /// The public link (`t.me/name`), where there is one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
    /// Disappearing-messages timer in seconds (`None` or `Some(0)` means off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<u32>,
    /// Channel bookmarks (links, canvases, folders; Slack `bookmarks.list`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<Bookmark>,
    /// Pending join requests awaiting admin approval (Telegram groups/channels).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pending_requests: u32,
    /// Pinned channel/chat tabs and apps (Teams: Files, Notes, Planner, websites, apps).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<ConversationTab>,
    /// The meeting this chat is for, when Teams gave a start time. Join is a link until calls
    /// exist here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting: Option<MeetingEvent>,
}

/// One tab or pinned app on a channel or chat (`ConversationInfo::tabs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTab {
    pub id: String,
    pub name: String,
    /// App display name when distinct from `name` (e.g. "Planner", "OneNote", "Website").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    /// Normalized kind: `"files"`, `"notes"`, `"planner"`, `"website"`, or `"app"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Direct web URL (`https://` or `http://`) to open the tab externally, or Teams deep link.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub web_url: String,
    /// Underlying content URL when provided by the tab configuration.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_url: String,
}

/// How I answered a calendar event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingResponse {
    #[default]
    None,
    Accepted,
    Tentative,
    Declined,
    Organizer,
}

impl MeetingResponse {
    /// The word Details and Upcoming show beside the time.
    pub fn label(self) -> &'static str {
        match self {
            MeetingResponse::None => "",
            MeetingResponse::Accepted => "Accepted",
            MeetingResponse::Tentative => "Tentative",
            MeetingResponse::Declined => "Declined",
            MeetingResponse::Organizer => "Organizer",
        }
    }
}

/// One calendar event: today's meetings, and the meeting a chat is for.
///
/// `start` and `end` are Unix seconds. `join_url` is `https://` or `http://` only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingEvent {
    pub id: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organizer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub join_url: String,
    #[serde(default)]
    pub response: MeetingResponse,
    /// Teams thread id when the meeting has a chat (`19:meeting_…`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conversation: String,
}

/// Kind of a channel bookmark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkKind {
    #[default]
    Link,
    Canvas,
    Folder,
}

/// One bookmark pinned in a channel's bookmarks bar (`ConversationInfo::bookmarks`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub emoji: String,
    #[serde(default)]
    pub kind: BookmarkKind,
}

/// One channel inside a Space that overrides the Space's notification level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceChannelOverride {
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub notify: Notify,
}

/// A Space's info: opened from its name, never from a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub space: Space,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<u32>,
    /// Our own nickname and roles there, role colours left out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nickname: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default)]
    pub notify: Notify,
    /// Suppress `@everyone` and `@here` mentions in this Space.
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppress_everyone: bool,
    /// Suppress all role `@mentions` in this Space.
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppress_roles: bool,
    /// Highlights notifications enabled (Discord `notify_highlights != 1`).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub highlights: bool,
    /// New scheduled event notifications enabled (`!mute_scheduled_events`).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub events: bool,
    /// Discord mobile app push notifications (affects Discord's own phone app only).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub mobile_push: bool,
    /// Channels whose notification setting overrides the Space's default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_overrides: Vec<SpaceChannelOverride>,
    /// Per-space privacy switches (Discord: allow DMs and message requests from members).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<SpacePrivacy>,
    /// Whether this account may change its own nickname in this Space.
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_change_nickname: bool,
    /// Mentionable team tags in this Space (Teams).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TeamTag>,
    /// Server banner image, where the Space has one (e.g. boosted Discord server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<MediaFile>,
    /// Boost level / tier (0..=3 on Discord).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub boost_level: u8,
    /// Number of active server boosts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub boosts: u32,
    /// Verified or partnered server mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
    /// System messages channel id, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_channel_id: Option<String>,
    /// AFK voice channel id, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afk_channel_id: Option<String>,
    /// AFK timeout in seconds (0 when unknown).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub afk_timeout: u32,
    /// Server-wide default notification level (`All` or `Mentions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_notify: Option<Notify>,
    /// Vanity invite code, if the server has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vanity_url: Option<String>,
    /// Whether invites are currently paused (`INVITES_DISABLED` feature).
    #[serde(default, skip_serializing_if = "is_false")]
    pub invites_disabled: bool,
    /// Optional `https://` hand-off link to open or manage this Space in the network's app.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
    /// Workspace subdomain (e.g. `acme` for `acme.slack.com`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    /// Canonical HTTPS workspace URL (e.g. `https://acme.slack.com`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

/// Fields to update on a Space's overview (`Request::EditSpace`).
/// `icon` / `banner`: `None` = unchanged, `Some("")` = clear, `Some(path)` = upload image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    /// `Some("")` clears the system channel; `Some(id)` sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_channel_id: Option<String>,
    /// `Some("")` clears the AFK channel; `Some(id)` sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afk_channel_id: Option<String>,
    /// AFK timeout in seconds (Discord accepts 60, 300, 900, 1800, 3600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afk_timeout: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_notify: Option<Notify>,
}

/// Fields to create or edit a Space's scheduled event (`Request::CreateEvent` / `EditEvent`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceEventEdit {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Start time in Unix seconds.
    pub start: i64,
    /// End time in Unix seconds (required by Discord for external events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    #[serde(default)]
    pub place_kind: EventPlaceKind,
    /// Voice or stage channel id (for Voice/Stage events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// External location string (for External events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Local path to cover image (`Some("")` removes cover on edit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// One active invite link in a Space (`Request::SpaceInvites`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInvite {
    pub code: String,
    /// Full shareable invite URL (e.g. `https://discord.gg/<code>`).
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub inviter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uses: u32,
    /// 0 means unlimited uses.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_uses: u32,
    /// Unix seconds when this invite expires (`None` = never expires).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// True for the server's read-only vanity URL row.
    #[serde(default, skip_serializing_if = "is_false")]
    pub vanity: bool,
}

/// One role in a Space (`Request::Roles`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRole {
    pub id: String,
    pub name: String,
    /// `0xRRGGBB`. Zero means the role has no colour.
    #[serde(default)]
    pub color: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hoist: bool,
    /// Discord's order. Zero is the everyone role, so it is kept on the wire.
    #[serde(default)]
    pub position: i32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentionable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub managed: bool,
    /// A validated picture URL on Discord's CDN, or a unicode emoji. Empty when the role has neither.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "is_zero64")]
    pub permissions: u64,
    /// The signed-in user may edit this role (below their highest role, and not managed).
    #[serde(default, skip_serializing_if = "is_false")]
    pub editable: bool,
    /// The signed-in user may give or take this role. The everyone role is never assignable.
    #[serde(default, skip_serializing_if = "is_false")]
    pub assignable: bool,
}

/// Fields to create or edit a role (`Request::CreateRole` / `EditRole`).
/// `icon`: `None` unchanged, `Some("")` clear, `Some(path)` a picture on this computer.
/// `unicode_emoji`: `None` unchanged, `Some("")` clear.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hoist: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentionable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unicode_emoji: Option<String>,
    /// Required when this edit newly grants Administrator.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm_admin: bool,
}

/// One role's new place in the list (`Request::MoveRoles`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleMove {
    pub id: String,
    #[serde(default)]
    pub position: i32,
}

/// Lifecycle status of a Space's scheduled event (Discord `guild_scheduled_events`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    #[default]
    Scheduled,
    Active,
    Completed,
    Canceled,
}

/// Where a Space's scheduled event takes place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPlaceKind {
    #[default]
    Voice,
    Stage,
    External,
}

/// One scheduled event in a Space (`Request::Events`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceEvent {
    /// Event key (account + event id).
    pub key: Key,
    /// The Space hosting this event.
    pub space: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Scheduled start time, Unix seconds.
    pub start: i64,
    /// Scheduled end time, Unix seconds, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    /// Voice channel, stage channel, or external location.
    #[serde(default)]
    pub place_kind: EventPlaceKind,
    /// Channel name or external location string.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub place: String,
    /// Voice or stage conversation key, when held inside a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Key>,
    #[serde(default)]
    pub status: EventStatus,
    /// Whether the signed-in user marked themselves interested.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interested: bool,
    /// Number of interested users reported by the network.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub count: u32,
    /// Event cover image, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<MediaFile>,
}

/// One workspace user group (e.g. Slack `@oncall`, `@design`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserGroup {
    pub id: String,
    pub name: String,
    pub handle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub member_count: u32,
}

/// One slash command exposed by a Slack app (shown in its About tab).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSlashCommand {
    pub command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub usage: String,
}

/// One validated external link on a Slack app's About tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppLink {
    pub label: String,
    pub url: String,
}

/// A Slack app's About metadata, fetched on demand when its conversation opens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppProfile {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<AppSlashCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<AppLink>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub installed_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub open_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub directory_url: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub home_enabled: bool,
}

/// A Slack app's published Home tab (Block Kit view), fetched on demand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppHome {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub view_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<crate::Span>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<crate::Card>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<crate::MessageControl>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub empty_reason: String,
}

/// Combined About + optional Home view for a Slack app conversation (`Request::AppView`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppView {
    pub profile: AppProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<AppHome>,
}

/// One slash command available in a workspace (`Request::SlashCommands`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    pub command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub usage: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub builtin: bool,
}

/// One conversation's unread messages in the All Unreads view (`Request::AllUnreads`, #606).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadGroup {
    pub conversation: Conversation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Exact unread count known after fetching messages past `last_read`.
    #[serde(default)]
    pub total_unread: u32,
}

/// Which of a custom emoji set this account may send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmojiScope {
    /// None at all (Telegram without Premium).
    #[default]
    None,
    /// Only the Space's own, where the message is said (Discord without Nitro).
    Space,
    /// Every custom emoji the account can see (Slack's workspace set, Discord with Nitro).
    All,
}

impl EmojiScope {
    pub fn is_none(&self) -> bool {
        *self == EmojiScope::None
    }
}

/// How an account encodes inline mentions in outgoing wire text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionFormat {
    /// Mentions stay as their plain display label (Telegram, Messages, Tiny).
    #[default]
    Plain,
    /// `<@id>`, `<@&id>`, `<#id>`, `<:name:id>` / `<a:name:id>`.
    Discord,
    /// `<@id>`, `<!subteam^id>`, `<#id>`; custom emoji stay `:name:`.
    Slack,
    /// `<@id|label>`, `<@&id|label>`, `<#id|label>`; custom emoji stay `:name:`.
    Teams,
    /// `<@aci>` for person mentions; adapter replaces with U+FFFC and UTF-16 `BodyRange(MentionAci)`.
    Signal,
}

impl MentionFormat {
    pub fn is_plain(&self) -> bool {
        *self == MentionFormat::Plain
    }
}

/// What this account may do in one conversation beyond its own messages.
/// Posting follows `Conversation::read_only` (`Conversation::can_post()`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRights {
    /// May pin or unpin messages in this conversation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pin: bool,
    /// May delete other people's messages in this conversation (Discord
    /// `MANAGE_MESSAGES`, Slack admin, Telegram `delete_messages`, Signal group admin,
    /// Teams owner).
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete_others: bool,
    /// May add, remove, or manage members in this conversation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_members: bool,
    /// May add/invite people to this conversation (`Request::MembersAdd`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub add_members: bool,
    /// May remove people from this conversation (`Request::MembersRemove`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub remove_members: bool,
    /// May rename this conversation (`Request::ConversationUpdate`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub rename: bool,
    /// May edit this conversation's topic or description (`Request::ConversationUpdate`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub edit_topic: bool,
    /// May change this conversation's picture (`Request::ConversationPicture`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub edit_picture: bool,
    /// May create public threads in this channel (Discord `CREATE_PUBLIC_THREADS`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub create_public_threads: bool,
    /// May create private threads in this channel (Discord `CREATE_PRIVATE_THREADS`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub create_private_threads: bool,
    /// May send messages in threads of this channel (Discord `SEND_MESSAGES_IN_THREADS`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub send_in_threads: bool,
    /// May rename, archive, lock, or delete threads here (Discord `MANAGE_THREADS`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_threads: bool,
    /// Exempt from channel/thread slowmode (Discord `MANAGE_MESSAGES` / `MANAGE_CHANNELS` / `MANAGE_THREADS`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub slowmode_exempt: bool,
    /// May create an invite link for this conversation (`Request::CreateInvite`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub create_invite: bool,
    /// May manage webhooks in this channel (Discord `MANAGE_WEBHOOKS`, required to follow an announcement channel here).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_webhooks: bool,
    /// May delete this channel (`Request::DeleteChannel`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete_channel: bool,
    /// May edit this channel's permission overwrites (`Request::SetOverwrite`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_permissions: bool,
    /// May create, edit, close, pin, or delete topics in a forum (`Request::TopicCreate`, `Request::TopicUpdate`, `Request::TopicDelete`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_topics: bool,
    /// May ban, unban, or restrict members (`Request::ModerateMember`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub ban_members: bool,
    /// May promote or demote admins and set admin custom titles (`Request::SetAdmin`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub promote_admins: bool,
    /// May edit group/channel settings: default permissions, slow mode, join requests, forum,
    /// pre-history visibility, signatures, protected content (`Request::UpdateChatSettings`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_settings: bool,
    /// May read the recent-actions admin log (`Request::AdminLog`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub view_admin_log: bool,
    /// Owner/creator: may transfer ownership with a 2FA password (`Request::TransferOwnership`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub transfer_owner: bool,
    /// Admin posts anonymously as the group/channel (`anonymous` admin right).
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
}

impl ConversationRights {
    const PIN: i64 = 1 << 0;
    const DELETE_OTHERS: i64 = 1 << 1;
    const MANAGE_MEMBERS: i64 = 1 << 2;
    const ADD_MEMBERS: i64 = 1 << 3;
    const REMOVE_MEMBERS: i64 = 1 << 4;
    const RENAME: i64 = 1 << 5;
    const EDIT_TOPIC: i64 = 1 << 6;
    const EDIT_PICTURE: i64 = 1 << 7;
    const CREATE_PUBLIC_THREADS: i64 = 1 << 8;
    const CREATE_PRIVATE_THREADS: i64 = 1 << 9;
    const SEND_IN_THREADS: i64 = 1 << 10;
    const MANAGE_THREADS: i64 = 1 << 11;
    const SLOWMODE_EXEMPT: i64 = 1 << 12;
    // Bits 8..=11 are thread rights and bit 12 is slowmode exemption on the Discord
    // thread lane, so an invite right cannot share them. The review asked for bit 12.
    const CREATE_INVITE: i64 = 1 << 13;
    /// Bit 14. Bits 8–12 are already conversation rights on other Discord branches.
    const MANAGE_WEBHOOKS: i64 = 1 << 14;
    const DELETE_CHANNEL: i64 = 1 << 15;
    const MANAGE_PERMISSIONS: i64 = 1 << 16;
    const BAN_MEMBERS: i64 = 1 << 17;
    const PROMOTE_ADMINS: i64 = 1 << 18;
    const MANAGE_SETTINGS: i64 = 1 << 19;
    const VIEW_ADMIN_LOG: i64 = 1 << 20;
    const TRANSFER_OWNER: i64 = 1 << 21;
    const ANONYMOUS: i64 = 1 << 22;
    const MANAGE_TOPICS: i64 = 1 << 23;

    pub fn is_none(&self) -> bool {
        *self == ConversationRights::default()
    }

    pub fn is_empty(&self) -> bool {
        self.is_none()
    }

    pub fn to_bits(self) -> i64 {
        let mut bits = 0i64;
        if self.pin {
            bits |= Self::PIN;
        }
        if self.delete_others {
            bits |= Self::DELETE_OTHERS;
        }
        if self.manage_members {
            bits |= Self::MANAGE_MEMBERS;
        }
        if self.add_members {
            bits |= Self::ADD_MEMBERS;
        }
        if self.remove_members {
            bits |= Self::REMOVE_MEMBERS;
        }
        if self.rename {
            bits |= Self::RENAME;
        }
        if self.edit_topic {
            bits |= Self::EDIT_TOPIC;
        }
        if self.edit_picture {
            bits |= Self::EDIT_PICTURE;
        }
        if self.create_public_threads {
            bits |= Self::CREATE_PUBLIC_THREADS;
        }
        if self.create_private_threads {
            bits |= Self::CREATE_PRIVATE_THREADS;
        }
        if self.send_in_threads {
            bits |= Self::SEND_IN_THREADS;
        }
        if self.manage_threads {
            bits |= Self::MANAGE_THREADS;
        }
        if self.slowmode_exempt {
            bits |= Self::SLOWMODE_EXEMPT;
        }
        if self.create_invite {
            bits |= Self::CREATE_INVITE;
        }
        if self.manage_webhooks {
            bits |= Self::MANAGE_WEBHOOKS;
        }
        if self.delete_channel {
            bits |= Self::DELETE_CHANNEL;
        }
        if self.manage_permissions {
            bits |= Self::MANAGE_PERMISSIONS;
        }
        if self.manage_topics {
            bits |= Self::MANAGE_TOPICS;
        }
        if self.ban_members {
            bits |= Self::BAN_MEMBERS;
        }
        if self.promote_admins {
            bits |= Self::PROMOTE_ADMINS;
        }
        if self.manage_settings {
            bits |= Self::MANAGE_SETTINGS;
        }
        if self.view_admin_log {
            bits |= Self::VIEW_ADMIN_LOG;
        }
        if self.transfer_owner {
            bits |= Self::TRANSFER_OWNER;
        }
        if self.anonymous {
            bits |= Self::ANONYMOUS;
        }
        bits
    }

    pub fn from_bits(bits: i64) -> Self {
        Self {
            pin: (bits & Self::PIN) != 0,
            delete_others: (bits & Self::DELETE_OTHERS) != 0,
            manage_members: (bits & Self::MANAGE_MEMBERS) != 0,
            add_members: (bits & Self::ADD_MEMBERS) != 0,
            remove_members: (bits & Self::REMOVE_MEMBERS) != 0,
            rename: (bits & Self::RENAME) != 0,
            edit_topic: (bits & Self::EDIT_TOPIC) != 0,
            edit_picture: (bits & Self::EDIT_PICTURE) != 0,
            create_public_threads: (bits & Self::CREATE_PUBLIC_THREADS) != 0,
            create_private_threads: (bits & Self::CREATE_PRIVATE_THREADS) != 0,
            send_in_threads: (bits & Self::SEND_IN_THREADS) != 0,
            manage_threads: (bits & Self::MANAGE_THREADS) != 0,
            slowmode_exempt: (bits & Self::SLOWMODE_EXEMPT) != 0,
            create_invite: (bits & Self::CREATE_INVITE) != 0,
            manage_webhooks: (bits & Self::MANAGE_WEBHOOKS) != 0,
            delete_channel: (bits & Self::DELETE_CHANNEL) != 0,
            manage_permissions: (bits & Self::MANAGE_PERMISSIONS) != 0,
            manage_topics: (bits & Self::MANAGE_TOPICS) != 0,
            ban_members: (bits & Self::BAN_MEMBERS) != 0,
            promote_admins: (bits & Self::PROMOTE_ADMINS) != 0,
            manage_settings: (bits & Self::MANAGE_SETTINGS) != 0,
            view_admin_log: (bits & Self::VIEW_ADMIN_LOG) != 0,
            transfer_owner: (bits & Self::TRANSFER_OWNER) != 0,
            anonymous: (bits & Self::ANONYMOUS) != 0,
        }
    }
}

/// One channel or category's target position in a bulk reorder (`Request::MoveChannels`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPosition {
    pub id: String,
    pub position: i32,
    /// `Some(Some(cat))` moves under that category; `Some(None)` uncategorises; `None` leaves parent unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Option<String>>,
    /// When moving under a category, sync overwrites with the new category's permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_permissions: Option<bool>,
}
/// Moderation action against one member of a group or channel (`Request::ModerateMember`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MemberModeration {
    /// Remove from the group/channel without permanently banning.
    Kick,
    /// Ban from the group/channel (`until`: Unix seconds, `None` = permanent).
    Ban {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<i64>,
    },
    /// Lift an existing ban or restriction.
    Unban,
    /// Restrict specific send permissions (`until`: Unix seconds, `None` = permanent).
    Restrict {
        rules: SendRules,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<i64>,
    },
}

/// Admin permissions and custom title assigned via `Request::SetAdmin`. All booleans false demotes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRights {
    #[serde(default, skip_serializing_if = "is_false")]
    pub change_info: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub post_messages: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub edit_messages: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete_messages: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ban_users: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub invite_users: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pin_messages: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub add_admins: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_call: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_topics: bool,
    /// Custom admin title / rank (e.g. "Moderator"). Empty leaves unset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rank: String,
}

/// Partial update to group or channel settings (`Request::UpdateChatSettings`). Unset fields stay.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSettings {
    /// Group-wide default member send permissions (`messages.editChatDefaultBannedRights`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_send_rules: Option<SendRules>,
    /// Slow-mode interval in seconds (`0` disables slow mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowmode_seconds: Option<u32>,
    /// Require admin approval for join requests (`channels.toggleJoinRequest`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_request: Option<bool>,
    /// Enable or disable forum topics (`channels.toggleForum`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forum: Option<bool>,
    /// Hide prior chat history for new members (`channels.togglePreHistoryHidden`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_history_hidden: Option<bool>,
    /// Sign channel posts with the posting admin's name (`channels.toggleSignatures`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures: Option<bool>,
    /// Protected content: forbid forwarding and saving media (`messages.toggleNoForwards`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_forwards: Option<bool>,
}

/// One read-only entry in a channel/supergroup's recent-actions log (`Request::AdminLog`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLogEntry {
    pub id: String,
    /// Unix seconds when the action occurred.
    pub date: i64,
    /// Display name of the admin or user who performed the action.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub actor: String,
    /// Plain-text summary of what changed.
    pub summary: String,
}

/// What an account may send and do on messages, as the session says (the plan, the workspace's
/// settings). A control the account lacks is absent in the shell, never disabled. Nothing is
/// allowed until the adapter reports it (`Context::capabilities`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// This Tiny account can ring a member before a direct conversation exists.
    #[serde(default, skip_serializing_if = "is_false")]
    pub direct_calls: bool,
    /// Largest file, in bytes. 0: files cannot be sent.
    #[serde(default, skip_serializing_if = "crate::is_zero64")]
    pub max_file_bytes: u64,
    /// Most files in one message.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_files: u32,
    /// Only image files (e.g. pasted screenshots) may be attached; other documents are refused.
    #[serde(default, skip_serializing_if = "is_false")]
    pub images_only: bool,
    /// Longest caption under files, in characters. 0: the text limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub caption_limit: u32,
    /// Longest message, in characters. 0: the composer does not count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text_limit: u32,
    /// Per-file options.
    #[serde(default, skip_serializing_if = "is_false")]
    pub spoiler: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub as_file: bool,
    /// The network has a GIF search tinyclient can ask (`Catalog` with `Gifs`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub gifs: bool,
    /// The GIF button picks a `.gif` file through the attachment path when `max_file_bytes > 0` (Tiny).
    #[serde(default, skip_serializing_if = "is_false")]
    pub gif_file: bool,
    /// This Tiny server supports setting presence state (online, idle, dnd, invisible) and status text.
    #[serde(default, skip_serializing_if = "is_false")]
    pub presence_status: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stickers: bool,
    /// The network searches its own messages (`SearchNetwork`), which the search pane's Enter
    /// asks. Everywhere stays the cache alone unless `search_global` is also set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub search: bool,
    /// The network supports account-wide search (Slack `search.messages`, Telegram
    /// `messages.searchGlobal`), so Search Everywhere fans out to this account on Enter.
    #[serde(default, skip_serializing_if = "is_false")]
    pub search_global: bool,
    #[serde(default)]
    pub custom_emoji: EmojiScope,
    /// `Outgoing::silent`, `Outgoing::schedule`, `Outgoing::suppress_embeds` and
    /// `Outgoing::ping` are carried out by the network.
    #[serde(default, skip_serializing_if = "is_false")]
    pub silent: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub schedule: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppress_embeds: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reply_ping: bool,
    /// This network takes a file sent as a recorded voice note (`Outgoing::voice`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub voice_notes: bool,
    /// This network takes a round video message (`Outgoing::video`). Telegram only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub video_notes: bool,
    /// Seconds after sending that an edit is still allowed. 0: no limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edit_window: u32,
    /// This Tiny server scans uploads before anyone can fetch them. False on every other
    /// network and on an older Tiny server, which keeps today's upload.
    #[serde(default, skip_serializing_if = "is_false")]
    pub file_scan: bool,
    /// This Tiny server takes archives. Ignored unless `file_scan` is set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_archives: bool,
    /// The network takes a description on a picture (`OutgoingFile::alt`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub alt_text: bool,
    /// This Tiny server accepts abuse reports (`POST /v1/reports`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub abuse_reports: bool,
    /// This network keeps earlier wordings of an edited message (`Request::Revisions`).
    /// Off everywhere until that network's adapter says so. The shell does not ask otherwise,
    /// and chatd does not keep words a sender replaced on a network that has no history.
    #[serde(default, skip_serializing_if = "is_false")]
    pub revisions: bool,
    /// This account supports closing a direct conversation (`Request::CloseDirect`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub close_direct: bool,
    /// This account supports clearing a chat's history or deleting a chat on the network
    /// (`Request::ClearHistory`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_history: bool,
    /// This account supports inline replies on messages.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reply: bool,
    /// This account supports editing own messages.
    #[serde(default, skip_serializing_if = "is_false")]
    pub edit: bool,
    /// This account supports replacing the photo or file of an already-sent message (`Request::EditMedia`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub edit_media: bool,
    /// This account supports deleting messages.
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
    /// This account supports starting threads on channel / announcement messages.
    #[serde(default, skip_serializing_if = "is_false")]
    pub threads: bool,
    /// This account also supports starting threads in direct / group conversations (e.g. Slack).
    #[serde(default, skip_serializing_if = "is_false")]
    pub direct_threads: bool,
    /// This account supports pinning and unpinning messages.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pins: bool,
    /// This account supports forwarding messages natively.
    #[serde(default, skip_serializing_if = "is_false")]
    pub forward: bool,
    /// This account supports creating and voting in polls, with its limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polls: Option<PollCapabilities>,
    /// This account supports message reactions.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reactions: bool,
    /// This account supports fetching who reacted to a message (`Request::Reactors`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub reactors: bool,
    /// How inline mentions are formatted on the wire for this account.
    #[serde(default, skip_serializing_if = "MentionFormat::is_plain")]
    pub mention_format: MentionFormat,
    /// Whether this account carries structured mention spans out-of-band on outgoing messages.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mention_spans: bool,
    /// Per-field own-profile edit capabilities (`Request::SetProfile`, `Request::SetAvatar`,
    /// `Request::SetNickname`). The shell shows only the fields the network accepts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_name: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_about: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_pronouns: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_avatar: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_username: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_birthday: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_nickname: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_accent: bool,
    /// Own profile banner image (Nitro-only on Discord).
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_banner: bool,
    /// Per-server profile overrides (Nitro-only on Discord).
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_server: bool,
    /// Most chats this account may pin on the network (`Request::File`). 0: cannot pin.
    /// Telegram allows 5 without Premium and 10 with Premium.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pin_chats: u32,
    /// This account supports archiving and unarchiving conversations on the network (`Request::File`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub archive: bool,
    /// This account supports adding and removing conversations from folders (`Request::File`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub folders_edit: bool,
    /// This account supports setting a conversation's disappearing-messages timer (`Request::SetDisappearing`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub disappearing: bool,
    /// Maximum members in a group chat this account can create (`Request::StartGroup`). 0: unsupported.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub groups: u32,
    /// This account supports marking messages as Important or Urgent (`Outgoing::importance`, Teams).
    #[serde(default, skip_serializing_if = "is_false")]
    pub importance: bool,
    /// This account supports broadcasting a thread reply to the channel (`Outgoing::broadcast`, Slack).
    #[serde(default, skip_serializing_if = "is_false")]
    pub thread_broadcast: bool,
    /// This account supports Telegram-style message effects (`Outgoing::effect`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub effects: bool,
    /// This account supports view-once media attachments (`OutgoingFile::view_once`, Signal/Telegram).
    #[serde(default, skip_serializing_if = "is_false")]
    pub view_once: bool,
    /// This account supports super / burst reactions (`Request::React { burst: true }`, Discord Nitro).
    #[serde(default, skip_serializing_if = "is_false")]
    pub burst_reactions: bool,
    /// This account supports blocking and unblocking people (`Request::Relate` with Block/Unblock).
    #[serde(default, skip_serializing_if = "is_false")]
    pub blocks: bool,
    /// This account supports sending a friend request by typing a username (`Request::AddFriend`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub add_friend_by_username: bool,
    /// This account supports setting a private nickname on a friend (`Request::SetFriendNickname`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub friend_nickname: bool,
    /// This account serves an activity feed (`Request::ActivityFeed`: mentions, thread replies, reactions).
    #[serde(default, skip_serializing_if = "is_false")]
    pub activity_feed: bool,
    /// This account supports per-message read-receipt lookup (`Request::ReadBy`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_by: bool,
    /// This account supports channel bookmarks (`bookmarks.list` / `add` / `edit` / `remove`, Slack).
    #[serde(default, skip_serializing_if = "is_false")]
    pub bookmarks: bool,
    /// This account supports browsing unjoined public channels, previewing, and joining them (`BrowseChannels`, `PreviewChannel`, `Join`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub browse_channels: bool,
    /// This account supports an optional subject line on new channel posts (Teams).
    #[serde(default, skip_serializing_if = "is_false")]
    pub channel_subject: bool,
    /// This account supports saving/starring messages for Later and setting network reminders (`StarMessage`, `StarredMessages`, `Remind`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub save_messages: bool,
    /// This account supports setting a status with an emoji and a clear-after expiration (`SetPresence` with `status_emoji` and `clear_at`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub rich_status: bool,
    /// This account supports workspace-wide file listing and deleting own files (`AccountFiles`, `DeleteFile`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_files: bool,
    /// This account supports previewing and joining a Space from an invite link (`InvitePreview`, `InviteJoin`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub join_space_by_invite: bool,
    /// This account supports leaving a Space (`Request::LeaveSpace`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub leave_space: bool,
    /// This account supports creating a new Space (`Request::CreateSpace`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub create_space: bool,
    /// This account supports sharing contact cards (`Outgoing::contact`, Telegram/Signal).
    #[serde(default, skip_serializing_if = "is_false")]
    pub share_contact: bool,
    /// This account supports saving and removing GIFs in the saved GIFs collection (`Request::SaveGif`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub save_gif: bool,
    /// This account supports pausing notifications for a duration (`SetPresence { presence: Dnd, clear_at: Some(..) }` -> `dnd.setSnooze` / `dnd.endSnooze`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dnd_snooze: bool,
    /// This account supports following and unfollowing threads (`Request::FollowThread`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_threads: bool,
    /// The network marks this account as not permitted to enter age-restricted channels (`nsfw_allowed == false` on Discord).
    #[serde(default, skip_serializing_if = "is_false")]
    pub age_restricted_blocked: bool,
    /// This account supports following announcement channels (`Request::Follow`) and publishing messages in them (`Request::Publish`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub announcement_follow: bool,
    /// This account supports creating a friend-invite link (`Request::CreateInvite` with `account` only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub create_friend_invite: bool,
    /// This account supports reading and writing private notes on people (`Request::SetNote`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub person_notes: bool,
    /// This account supports on-demand server transcription of voice and round video messages (`Request::Transcribe`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub transcribe: bool,
    /// This account supports on-demand in-place message translation (`Request::Translate`, Teams).
    #[serde(default, skip_serializing_if = "is_false")]
    pub translate: bool,
    /// This account supports reading and setting out-of-office / automatic replies (`Request::GetOutOfOffice`, `Request::SetOutOfOffice`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub out_of_office: bool,
    /// This account can fetch recent mentions from before tinyclient was open (`Request::Mentions`, Discord).
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_history: bool,
    /// This account supports reordering Spaces and creating/renaming/colouring Space folders (`MoveSpace`, `SetFolder`, Discord).
    #[serde(default, skip_serializing_if = "is_false")]
    pub manage_space_folders: bool,
    /// This account supports creating, renaming, deleting, reordering, and moving channels
    /// between custom sidebar sections (Slack paid plans).
    #[serde(default, skip_serializing_if = "is_false")]
    pub sidebar_sections: bool,
    /// This account supports muting a conversation for a chosen duration (`SetMute` with `until`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_mute: bool,
    /// This account supports reading/updating account-wide notification defaults and exceptions (Telegram).
    #[serde(default, skip_serializing_if = "is_false")]
    pub notify_defaults: bool,
    /// This account supports browsing Saved Messages grouped by source chat and reaction tags (Telegram).
    #[serde(default, skip_serializing_if = "is_false")]
    pub saved_subdialogs: bool,
    /// This account supports listing active signed-in devices/sessions and ending another session (Telegram).
    #[serde(default, skip_serializing_if = "is_false")]
    pub active_sessions: bool,
    /// This account supports jumping to unread mentions and reactions per conversation (Telegram).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unread_jumps: bool,
}

/// One recent mention fetched from the network (`Request::Mentions`).
/// Never written to the channel's message history store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionHit {
    pub conversation: Key,
    pub message_id: String,
    pub sent: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_avatar: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space_title: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_everyone: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_role: bool,
}

/// Scope of an account-wide notification default (Telegram `InputNotifyUsers` / `Chats` / `Broadcasts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyDefaultScope {
    Users,
    Groups,
    Channels,
}

/// One conversation whose notification setting overrides its account-wide scope default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyException {
    pub conversation: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
}

/// Account-wide notification defaults and per-chat exceptions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyDefaults {
    #[serde(default, skip_serializing_if = "is_false")]
    pub users_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub groups_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub channels_muted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<NotifyException>,
}

/// One sub-view of Saved Messages grouped by source peer (`messages.getSavedDialogs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDialog {
    /// Peer id (e.g. `"u7002"`, `"c9001"`, or `"u7001"` for My Notes).
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
    #[serde(default)]
    pub updated: i64,
}

/// One reaction tag applied to Saved Messages (`messages.getSavedReactionTags`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTag {
    pub emoji: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub count: u32,
}

/// One active signed-in device/session on an account (`account.getAuthorizations`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSession {
    /// Opaque session identifier (`"0"` for the current session on Telegram).
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub platform: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub location: String,
    #[serde(default)]
    pub last_active: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub current: bool,
}

/// Which unread markers to list or clear in a conversation (`UnreadItems` / `MarkUnreadItemsRead`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreadItemKind {
    Mentions,
    Reactions,
}

/// What a jump tells the network it just opened.
/// A mention is that message. A reaction names the one just opened until the last,
/// which clears the chat (`readReactions` takes no ids).
pub fn jump_mark_ids(kind: UnreadItemKind, id: &str, last: bool) -> Vec<String> {
    match kind {
        UnreadItemKind::Mentions => vec![id.to_owned()],
        UnreadItemKind::Reactions if last => Vec::new(),
        UnreadItemKind::Reactions => vec![id.to_owned()],
    }
}

/// The badge left after that mark. An empty mark clears the count.
pub fn badge_after_jump(count: u32, marked: u32) -> u32 {
    if marked == 0 { 0 } else { count.saturating_sub(marked) }
}

/// One public server from Discord Discovery (`Request::Discover`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredGuild {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Discord's icon hash. Never a URL.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub members: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub online: u32,
}

/// One question on a server's rules form (`Request::SpaceGate`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateField {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Discord's `field_type`: TERMS, TEXT_INPUT, PARAGRAPH, MULTIPLE_CHOICE, VERIFICATION.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

/// One answer a server's onboarding offers. The id is a snowflake, never a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOption {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// One onboarding question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatePrompt {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<GateOption>,
}

/// What a server asks before a member can talk (`Request::SpaceGate`).
/// `external` is only ever `https://discord.com/app`, and only when phone or email
/// verification has to be finished in Discord. Rules text is not a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpaceGate {
    #[serde(default, skip_serializing_if = "is_false")]
    pub pending: bool,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub verification: u8,
    #[serde(default, skip_serializing_if = "is_false")]
    pub manual: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub onboarding: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<GateField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<GatePrompt>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub external: String,
}

/// Preview of a Space invite (`Request::InvitePreview`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitePreview {
    pub code: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// True when this account is already a member of `space_id`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub joined: bool,
    /// Optional scheduled event attached to this invite (`?event=<id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<crate::ScheduledEvent>,
    /// True when joining via this invite submits a join request for admin approval (Telegram).
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_needed: bool,
}

/// One exported invite link for a group or channel (Telegram `ChatInviteExported`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatInviteLink {
    pub link: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub permanent: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_needed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_date: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit: Option<u32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub usage: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pending_requests: u32,
}

/// One person who requested to join a chat via an approval-gated invite link (Telegram `ChatInviteImporter`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub user_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
    #[serde(default)]
    pub date: i64,
}

/// One public channel returned by [`crate::Request::BrowseChannels`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsedChannel {
    pub key: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub members: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub joined: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub private: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
}

/// Telegram sentinel `schedule_date` value meaning "send when the recipient comes online".
pub const SCHEDULE_WHEN_ONLINE: i64 = 0x7FFF_FFFE;

/// One pending scheduled message returned by [`crate::Request::ScheduledMessages`] or
/// [`crate::Request::RescheduleMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMessage {
    pub id: String,
    pub conversation: Key,
    #[serde(default)]
    pub text: String,
    pub post_at: i64,
    #[serde(default)]
    pub created: i64,
}

/// What kind of event an [`ActivityItem`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityItemKind {
    Mention,
    ThreadReply,
    Reaction,
    MissedCall,
    App,
    Invite,
    #[serde(other)]
    Other,
}

/// One row of an account's activity feed (mentions, thread replies, reactions to my messages, apps).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
    /// Stable identifier from the network (used for mark-read and keying).
    pub id: String,
    pub kind: ActivityItemKind,
    /// Conversation where the activity occurred.
    pub conversation: Key,
    /// Target message id to jump to when clicked.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    /// Thread conversation id when the activity is inside a thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Display name of the person or app who triggered the activity.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub actor: String,
    /// Network user/app id of the actor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub actor_id: String,
    /// Emoji for a reaction item (Unicode or `:name:`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub emoji: String,
    /// Short preview text of the message or notification.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Unix seconds when the activity happened.
    #[serde(default)]
    pub time: i64,
    /// Whether the network considers this item unread.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unread: bool,
}

/// One member who has read up to a given message (`Request::ReadBy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadBy {
    pub person: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    /// Unix seconds when they read it, if reported by the network.
    #[serde(default)]
    pub at: i64,
}

/// Allowed disappearing-messages timer presets (in seconds, `0` = off) for a network.
pub fn disappearing_presets(network: Network) -> &'static [u32] {
    match network {
        Network::Signal => &[0, 30, 300, 3_600, 28_800, 86_400, 604_800, 2_419_200],
        Network::Telegram => &[0, 86_400, 604_800, 2_678_400],
        _ => &[0, 30, 300, 3_600, 86_400, 604_800],
    }
}

/// Short human-readable label for a disappearing-messages timer in seconds.
pub fn format_disappearing(seconds: u32) -> String {
    if seconds == 0 {
        return "Off".to_owned();
    }
    if seconds % 604_800 == 0 {
        format!("{}w", seconds / 604_800)
    } else if seconds % 86_400 == 0 {
        format!("{}d", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}h", seconds / 3_600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

/// How much prior chat history a newly added member receives (Teams group chats).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum HistoryShare {
    #[default]
    None,
    Days { days: u32 },
    All,
}

/// Content-type filter for network message search (`SearchFilters::has`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchHas {
    Files,
    Links,
    Images,
    Videos,
    Embeds,
    Sounds,
    Stickers,
    Polls,
}

/// Sort order for network search (`SearchFilters::sort`, `Request::SearchFiles`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    #[default]
    Newest,
    Oldest,
    Relevance,
}

impl SearchSort {
    pub fn is_newest(&self) -> bool {
        *self == SearchSort::Newest
    }
}

/// Optional filters on `Request::SearchNetwork`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentions: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_channel: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has: Option<SearchHas>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_me: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_type: Option<String>,
    #[serde(default, skip_serializing_if = "SearchSort::is_newest")]
    pub sort: SearchSort,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: u32,
}

/// One calendar day with matching messages (`Request::SearchCalendar` / Telegram `messages.getSearchResultsCalendar`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCalendarDay {
    /// Unix timestamp for the day.
    pub date: i64,
    pub min_message_id: String,
    pub max_message_id: String,
    pub count: u32,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        *self == SearchFilters::default()
    }
}

/// Message urgency level (Teams: Normal, Important, Urgent).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    #[default]
    Normal,
    Important,
    Urgent,
}

impl Importance {
    pub fn is_normal(&self) -> bool {
        *self == Importance::Normal
    }
}

/// What a network allows when creating a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PollCapabilities {
    /// Maximum number of options (e.g. 10).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub max_options: u8,
    /// Longest duration in seconds. 0 means no duration setting / network default.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_duration_secs: u32,
    /// Whether multi-select polls are supported.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiple: bool,
    /// Whether anonymous polls are supported.
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

impl Capabilities {
    pub fn is_none(&self) -> bool {
        *self == Capabilities::default()
    }

    /// True if any own-profile field can be edited on this network.
    pub fn can_edit_profile(&self) -> bool {
        self.profile_name
            || self.profile_about
            || self.profile_pronouns
            || self.profile_avatar
            || self.profile_accent
            || self.profile_banner
            || self.profile_server
            || self.profile_username
            || self.profile_birthday
            || self.rich_status
    }
}

/// Optional media facts staged by the shell for video or audio uploads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMedia {
    /// Duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// Video width in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Video height in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Path to a JPEG poster image on disk (validated by chatd like any upload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    /// Audio track title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Audio performer / artist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performer: Option<String>,
}

/// One file the user attached: the shell names a path on disk and chatd streams it from there.
/// Bytes never cross the socket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingFile {
    pub path: String,
    /// The name the network shows. Empty: the path's file name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub spoiler: bool,
    /// Telegram: as a document, uncompressed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub as_file: bool,
    /// Send the file as it is (a HEIC rather than the JPEG made at staging).
    #[serde(default, skip_serializing_if = "is_false")]
    pub original: bool,
    /// View-once media (Signal, Telegram): hidden until opened and cannot be kept.
    #[serde(default, skip_serializing_if = "is_false")]
    pub view_once: bool,
    /// A description of the picture. Empty: none. chatd clips it to [`crate::ALT_LIMIT`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alt: String,
    /// Optional video/audio metadata and poster thumbnail path staged by the shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<OutgoingMedia>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind {
    /// The account's custom emoji (Unicode emoji are the shell's own list).
    Emoji,
    Stickers,
    Gifs,
}

/// A sticker or GIF chosen in the picker, sent as itself rather than uploaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    pub kind: CatalogKind,
    /// `CatalogItem::id`, which only the adapter understands.
    pub id: String,
}

/// A recorded voice message: the facts the recorder read while making the file, sent with it
/// so the network labels it a voice note without listening to it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceNote {
    /// Milliseconds of sound, as the recorder finished the file.
    pub duration: u32,
    /// A voice message's loudness over time, one byte a bar, at most
    /// [`crate::WAVEFORM_LIMIT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waveform: Vec<u8>,
}

/// A round video message: a square picture, at most a minute. `sound` is set only when the
/// file has an AAC track. This build's recorder writes video only, so `sound` stays off and
/// the upload says `nosound`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoNote {
    /// Milliseconds of picture, as the recorder finished the file.
    pub duration: u32,
    /// Square side in pixels.
    pub side: u32,
    /// The file has an AAC sound track. False: picture only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sound: bool,
}

/// A poll the user is starting. Signal: a question of 1 to 100 characters and 2 to 10 choices,
/// each at most 100 characters. `multiple` lets a person pick more than one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollDraft {
    pub question: String,
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiple: bool,
    /// Optional duration in seconds before the poll closes automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u32>,
    /// Whether votes are anonymous.
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
}

/// One selectable sending identity or line on an account (e.g. a SIM card on a dual-SIM phone,
/// or a channel send-as alias).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendAs {
    /// Adapter-opaque id carried back in `Outgoing::send_as`.
    pub id: String,
    /// Short display label shown beside the composer (e.g. carrier name or "SIM 2").
    pub label: String,
    /// Optional accent colour (`0xRRGGBB`), e.g. SIM colour from the phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// Optional secondary detail (e.g. "RCS" / "SMS" or masked number).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// What this account may send in one conversation right now: per-chat media restrictions,
/// slow mode, timed mute expiry, and paid-message star price (`Request::SendRules`).
///
/// Every permission defaults to `true`, so an empty JSON object or an adapter that does not
/// report per-chat rules leaves sending up to account [`Capabilities`] and the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRules {
    /// May send messages at all (`false` when muted/banned or read-only).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_messages: bool,
    /// May send plain text messages.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_plain: bool,
    /// May send media/attachments in general.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_media: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_photos: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_videos: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_round_videos: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_audios: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_voices: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_files: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_stickers: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_gifs: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub embed_links: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub send_polls: bool,
    /// Slow-mode interval in seconds (`0` means slow mode is off).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub slowmode_seconds: u32,
    /// Unix seconds when the next message may be sent under slow mode (`None` when ready now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowmode_next_send: Option<i64>,
    /// Unix seconds when a timed mute / send ban expires (`None` if permanent or not muted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ban_until: Option<i64>,
    /// Telegram Stars charged per message (`send_paid_messages_stars`). `None` or `Some(0)` is free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_stars: Option<u64>,
}

impl Default for SendRules {
    fn default() -> SendRules {
        SendRules {
            send_messages: true,
            send_plain: true,
            send_media: true,
            send_photos: true,
            send_videos: true,
            send_round_videos: true,
            send_audios: true,
            send_voices: true,
            send_files: true,
            send_stickers: true,
            send_gifs: true,
            embed_links: true,
            send_polls: true,
            slowmode_seconds: 0,
            slowmode_next_send: None,
            ban_until: None,
            paid_stars: None,
        }
    }
}

impl SendRules {
    /// True when at least one file/media attachment category is allowed.
    pub fn allows_any_attachment(&self) -> bool {
        self.send_media && (self.send_photos || self.send_videos || self.send_audios || self.send_files)
    }

    /// Whole seconds remaining before slow mode allows another send at unix timestamp `now`.
    pub fn slowmode_remaining(&self, now: i64) -> u32 {
        self.slowmode_next_send
            .map_or(0, |until| until.saturating_sub(now).max(0).min(i64::from(u32::MAX)) as u32)
    }

    pub fn is_default(&self) -> bool {
        *self == SendRules::default()
    }
}

/// What a `Send` carries besides its text. All of it is off by default, and a plain text
/// message leaves it off the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outgoing {
    /// At most [`ATTACHMENTS_LIMIT`], and at most the account's `Capabilities::max_files`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<OutgoingFile>,
    /// Deliver without a notification (Discord's `@silent`, Telegram's silent).
    #[serde(default, skip_serializing_if = "is_false")]
    pub silent: bool,
    /// Ask the network not to unfurl the links in it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppress_embeds: bool,
    /// With `reply_to`: notify its author. On by default.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub ping: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<Pick>,
    /// The last file is a voice message this client recorded (`files` and `voice` go together;
    /// `files` of one). The facts name it what it is without a second look at the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceNote>,
    /// The last file is a round video message (`files` of one). Not a voice note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoNote>,
    /// Unix seconds: the network sends it then (Slack, Telegram). tinyclient never runs a timer
    /// that sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<i64>,
    /// Styled ranges of the send's text, the same model a stored message uses. Empty is plain
    /// text and stays off the wire, so an older client still reads the send.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<crate::Span>,
    /// Start a poll instead of a text message. A plain send leaves it off, so `is_plain` stays
    /// true for text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<PollDraft>,
    /// Message importance (Teams: Normal / Important / Urgent).
    #[serde(default, skip_serializing_if = "Importance::is_normal")]
    pub importance: Importance,
    /// Also send a thread reply to the parent channel (Slack).
    #[serde(default, skip_serializing_if = "is_false")]
    pub broadcast: bool,
    /// Optional message effect identifier from the account's catalog (Telegram).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// Optional contact card to share (Signal, Telegram).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<crate::SharedContact>,
    /// Optional sending identity or line id from `AccountStatus::send_as` (e.g. a SIM card on a
    /// dual-SIM phone). `None` keeps the conversation's default line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_as: Option<String>,
    /// Optional Slack snippet syntax type (e.g. `"rust"`, `"python"`, `"text"`) passed to
    /// `files.getUploadURLExternal` when uploading a code/text snippet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_type: Option<String>,
    /// Optional one-line subject for a new channel post (Teams).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Explicit consent to pay up to this many Telegram Stars for a paid message. Never set
    /// without the user confirming the price first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_paid_stars: Option<u64>,
}

impl Default for Outgoing {
    fn default() -> Outgoing {
        Outgoing {
            files: Vec::new(),
            silent: false,
            suppress_embeds: false,
            ping: true,
            pick: None,
            schedule: None,
            voice: None,
            video: None,
            spans: Vec::new(),
            poll: None,
            importance: Importance::Normal,
            broadcast: false,
            effect: None,
            contact: None,
            send_as: None,
            snippet_type: None,
            subject: None,
            allow_paid_stars: None,
        }
    }
}

impl Outgoing {
    /// Nothing but text: what every adapter could already send.
    pub fn is_plain(&self) -> bool {
        *self == Outgoing::default()
    }
}

/// One changed field inside a server audit log entry (`#394`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuditChange {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
}

/// One entry in a server's audit log (`Request::AuditLog`, `#394`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    #[serde(default)]
    pub action_type: u32,
    #[serde(default)]
    pub action_label: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub actor_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default)]
    pub target_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default)]
    pub time: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<AuditChange>,
}

/// One custom emoji, sticker or GIF in a picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItem {
    /// What `Pick::id` or a reaction names. For a custom emoji, the id of its registered file.
    pub id: String,
    /// `:name:` for an emoji, a sticker's emoji or a GIF's title.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its still preview, a registered file the shell asks `Fetch` for when it is on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<MediaFile>,
    /// The pack or Space it belongs to, for the picker's sections.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pack: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub animated: bool,
    /// `id:access_hash` or a short name when this sticker's pack is not installed yet.
    /// Empty for an installed pack, a locked Premium pack, a GIF, or an emoji. A locked pack
    /// leaves this empty so the picker does not install it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set: String,
    /// A Premium sticker this account cannot send.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

/// What chatd is doing with the keychain. Sessions are read after the socket is answering, one
/// network at a time; macOS may put up a prompt during a read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keychain {
    /// The sessions are still being read; accounts appear as each network's is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reading: bool,
    /// A read has gone on for over a second: macOS is asking the user. Shown as "Waiting for
    /// permission to read your sign-in from the keychain".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Network>,
    /// Networks whose session could not be read: the prompt was denied or cancelled, or the
    /// keychain is locked. Their accounts and cache are kept; `ReadSessions` asks again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied: Vec<Network>,
}

impl Keychain {
    pub fn is_settled(&self) -> bool {
        *self == Keychain::default()
    }
}

/// A server's safety setup (`Request::AutomodRules`, `#395`).
///
/// `verification_level` is Discord's 0–4 (none through highest). `explicit_filter` is
/// `explicit_content_filter`, 0–2. The pause fields are unix seconds from incident actions;
/// `0` means that pause is off.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SpaceSafety {
    #[serde(default)]
    pub verification_level: u8,
    #[serde(default)]
    pub explicit_filter: u8,
    #[serde(default)]
    pub invites_paused_until: i64,
    #[serde(default)]
    pub dms_paused_until: i64,
}

/// What an AutoMod rule watches. Unknown triggers stay `Other` and are saved back untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomodTrigger {
    Keyword {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keywords: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regex: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<String>,
    },
    Spam,
    Preset {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        presets: Vec<u8>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<String>,
    },
    Mention {
        #[serde(default)]
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        raid_protection: bool,
    },
    Profile {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keywords: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regex: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<String>,
    },
    /// A trigger this build does not edit. `metadata` is the original JSON object.
    Other {
        #[serde(default)]
        code: u8,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        metadata: String,
    },
}

// `#[default]` only takes a unit variant, so the empty keyword rule is spelled out.
impl Default for AutomodTrigger {
    fn default() -> Self {
        Self::Keyword { keywords: Vec::new(), regex: Vec::new(), allow: Vec::new() }
    }
}

/// What an AutoMod rule does when it matches. The block message is display text only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomodAction {
    Block {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
    Alert {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        channel_id: String,
    },
    Timeout {
        #[serde(default)]
        seconds: u32,
    },
    /// Discord `BLOCK_MEMBER_INTERACTION`.
    Quarantine,
    /// An action this build does not edit. `metadata` is the original JSON object.
    Other {
        #[serde(default)]
        code: u8,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        metadata: String,
    },
}

impl Default for AutomodAction {
    fn default() -> Self {
        Self::Block { message: String::new() }
    }
}

/// One AutoMod rule (`#395`). An empty `id` creates a rule. `read_only` rules are shown and,
/// if saved, written back from `raw` without tinyclient interpreting their trigger or actions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AutomodRule {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    #[serde(default)]
    pub event_type: u8,
    #[serde(default)]
    pub trigger: AutomodTrigger,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AutomodAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exempt_roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exempt_channels: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// Original Discord rule object when the trigger or an action is not one this build edits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub raw: String,
}
/// Kind of webhook in a Space (e.g. Discord incoming, channel follower, or application webhook).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookKind {
    #[default]
    Incoming,
    ChannelFollower,
    Application,
}

/// One webhook in a Space (incoming webhook, followed announcement channel, or app webhook).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceWebhook {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    #[serde(default)]
    pub kind: WebhookKind,
    /// Full webhook URL (including secret token for incoming webhooks). Never logged; only shown
    /// or copied on explicit user action.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creator: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_guild: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_channel: String,
}

/// One installed bot, app, or third-party integration in a Space.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceIntegration {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Integration type (e.g. `"discord"`, `"twitch"`, `"youtube"`, `"guild_subscription"`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default = "is_true_val", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub application_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
}

fn is_true_val() -> bool {
    true
}

/// A Space's webhooks, followed channels, and installed bots/integrations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceIntegrations {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhooks: Vec<SpaceWebhook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followers: Vec<SpaceWebhook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub integrations: Vec<SpaceIntegration>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_manage_webhooks: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_manage_guild: bool,
    /// External URL to add a bot/app in Discord's App Directory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub add_bot_url: String,
    /// External URL to edit per-command integration permissions in Discord.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permissions_url: String,
}

/// One custom emoji in a Space (e.g. Discord guild emoji).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceEmoji {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub animated: bool,
    #[serde(default = "is_true_val", skip_serializing_if = "is_true")]
    pub available: bool,
    /// Role IDs allowed to use this emoji; empty means everyone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creator: String,
}

/// One custom sticker in a Space (e.g. Discord guild sticker).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceSticker {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Related Unicode emoji or autocomplete tag.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tags: String,
    /// Discord sticker format: 1 PNG, 2 APNG, 3 Lottie, 4 GIF.
    #[serde(default)]
    pub format: u8,
    #[serde(default = "is_true_val", skip_serializing_if = "is_true")]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creator: String,
}

/// A role that can be selected when restricting custom emoji access.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRoleChoice {
    pub id: String,
    pub name: String,
}

/// A Space's custom emoji, stickers, slot limits (from boost tier), and available roles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceExpressions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emojis: Vec<SpaceEmoji>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stickers: Vec<SpaceSticker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<SpaceRoleChoice>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub static_emoji_limit: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub animated_emoji_limit: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sticker_limit: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_manage: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_create: bool,
    #[serde(default)]
    pub premium_tier: u8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountKey, Key, Network, Request};

    #[test]
    fn jump_mark_ids_mentions_one_message_and_reactions_clear_on_the_last() {
        assert_eq!(jump_mark_ids(UnreadItemKind::Mentions, "21", true), vec!["21".to_owned()]);
        assert_eq!(jump_mark_ids(UnreadItemKind::Reactions, "24", false), vec!["24".to_owned()]);
        assert!(jump_mark_ids(UnreadItemKind::Reactions, "25", true).is_empty());
        assert_eq!(badge_after_jump(3, 1), 2);
        assert_eq!(badge_after_jump(3, 0), 0);
    }

    #[test]
    fn profile_edit_capabilities_and_requests_round_trip() {
        let mut can = Capabilities::default();
        assert!(!can.can_edit_profile());
        can.profile_about = true;
        assert!(can.can_edit_profile());
        let json = serde_json::to_string(&can).unwrap();
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, can);

        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let reqs = [
            Request::SetProfile {
                account: account.clone(),
                name: Some("Ada".into()),
                about: Some("Builder".into()),
                pronouns: Some("she/her".into()),
                emoji: None,
                username: None,
                birthday: None,
                photo: false,
                accent: Some(0x5865F2),
                banner: None,
                space: None,
            },
            Request::SetAvatar { account: account.clone(), path: "/tmp/avatar.png".into() },
            Request::SetNickname { space: Key::new(&account, "guild-1"), nickname: Some("Ada!".into()) },
        ];
        for req in reqs {
            let s = serde_json::to_string(&req).unwrap();
            let got: Request = serde_json::from_str(&s).unwrap();
            assert_eq!(got, req);
        }
    }

    #[test]
    fn report_requests_and_reasons_round_trip() {
        use crate::{ReportReason, ReportTarget, Response};
        let account = AccountKey { network: Network::Telegram, account: "1".into() };
        let conv = Key::new(&account, "c1");
        let target = ReportTarget::Message { conversation: conv.clone(), message_id: "m42".into() };
        assert_eq!(target.account(), account);
        let req = Request::Report {
            target: target.clone(),
            reason: ReportReason::Spam,
            note: "unsolicited links".into(),
            block: true,
        };
        let got: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(got, req);

        let reasons_req = Request::ReportReasons { target };
        let got_r: Request = serde_json::from_str(&serde_json::to_string(&reasons_req).unwrap()).unwrap();
        assert_eq!(got_r, reasons_req);

        let resp = Response::ReportReasons { reasons: ReportReason::ALL.to_vec() };
        let got_resp: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(got_resp, resp);
        assert_eq!(ReportReason::ALL.len(), 6);
    }

    #[test]
    fn send_as_round_trips_and_stays_off_wire_when_unset() {
        let plain = Outgoing::default();
        assert!(plain.is_plain());
        assert!(!serde_json::to_string(&plain).unwrap().contains("send_as"));

        let chosen = Outgoing {
            send_as: Some("sim-2".into()),
            ..Outgoing::default()
        };
        assert!(!chosen.is_plain());
        let wire = serde_json::to_string(&chosen).unwrap();
        let back: Outgoing = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, chosen);

        let opt = SendAs {
            id: "sim-2".into(),
            label: "Work".into(),
            color: Some(0x34A853),
            detail: "SMS · +1 •••• ••99".into(),
        };
        let back_opt: SendAs = serde_json::from_str(&serde_json::to_string(&opt).unwrap()).unwrap();
        assert_eq!(back_opt, opt);
    }

    #[test]
    fn automod_rules_default_when_a_field_is_missing() {
        let bare: AutomodRule = serde_json::from_str(r#"{"name":"spam"}"#).unwrap();
        assert_eq!(bare.name, "spam");
        assert!(!bare.enabled);
        assert!(bare.id.is_empty());
        assert_eq!(bare.trigger, AutomodTrigger::default());
        assert!(bare.actions.is_empty());
        assert!(!bare.read_only);
        let round: AutomodRule = serde_json::from_str(&serde_json::to_string(&AutomodRule::default()).unwrap()).unwrap();
        assert_eq!(round, AutomodRule::default());

        let safety: SpaceSafety = serde_json::from_str("{}").unwrap();
        assert_eq!(safety, SpaceSafety::default());

        let space = Key::new(&AccountKey { network: Network::Discord, account: "1".into() }, "4000");
        let bare_req = Request::SaveAutomodRule { space: space.clone(), rule: AutomodRule::default() };
        let mut value = serde_json::to_value(&bare_req).unwrap();
        value.as_object_mut().unwrap().remove("rule");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(back, bare_req);

        let listed = Request::AutomodRules { space };
        let again: Request = serde_json::from_str(&serde_json::to_string(&listed).unwrap()).unwrap();
        assert_eq!(again, listed);
    }

    #[test]
    fn space_info_banner_boosts_and_mark_round_trip_and_default_cleanly() {
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let space = Space {
            key: Key::new(&account, "4000"),
            name: "Rustaceans".into(),
            icon: None,
            position: 0,
            attention: 0,
            unread: false,
            folder: None,
            muted: false,
            hide_muted: false,
            can_manage_webhooks: false,
            can_manage_guild: false,
            can_manage_expressions: false,
            can_create_channels: false,
            can_create_invites: false,
            opt_in_channels: false,
            rights: crate::SpaceRights::default(),
        };
        let legacy = serde_json::json!({
            "space": space,
            "description": "Crabs",
            "members": 42,
            "online": 7
        });
        let parsed: SpaceInfo = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.banner, None);
        assert_eq!(parsed.boost_level, 0);
        assert_eq!(parsed.boosts, 0);
        assert_eq!(parsed.mark, None);

        let full = SpaceInfo {
            banner: Some(MediaFile::new("banner:4000:abc", crate::MediaKind::Image)),
            boost_level: 2,
            boosts: 14,
            mark: Some(Mark::Verified),
            ..parsed
        };
        let wire = serde_json::to_string(&full).unwrap();
        let back: SpaceInfo = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn create_space_request_response_and_capability_round_trip() {
        use crate::Response;
        let mut can = Capabilities::default();
        assert!(!can.create_space);
        can.create_space = true;
        let back_can: Capabilities = serde_json::from_str(&serde_json::to_string(&can).unwrap()).unwrap();
        assert!(back_can.create_space);

        let account = AccountKey { network: Network::Discord, account: "1000".into() };
        let req = Request::CreateSpace {
            account: account.clone(),
            name: "Book Club".into(),
            icon: Some("/tmp/icon.png".into()),
            template: Some("friends".into()),
        };
        let back_req: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back_req, req);

        let resp = Response::SpaceCreated {
            space: Key::new(&account, "8800"),
            conversation: Some(Key::new(&account, "8801")),
        };
        let back_resp: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back_resp, resp);
    }

    #[test]
    fn space_events_requests_responses_and_change_round_trip() {
        use crate::{Change, Response};
        let account = AccountKey { network: Network::Discord, account: "1000".into() };
        let space = Key::new(&account, "4000");
        let ev_key = Key::new(&account, "9001");
        let ev = SpaceEvent {
            key: ev_key.clone(),
            space: space.clone(),
            name: "Community Q&A".into(),
            description: "Ask anything".into(),
            start: 1_760_000_000,
            end: Some(1_760_003_600),
            place_kind: EventPlaceKind::Voice,
            place: "General Voice".into(),
            conversation: Some(Key::new(&account, "4201")),
            status: EventStatus::Active,
            interested: true,
            count: 17,
            cover: Some(MediaFile::new("event_cover:9001:abc", crate::MediaKind::Image)),
        };
        let reqs = [
            Request::Events { space: space.clone() },
            Request::EventInterest { space: space.clone(), event: ev_key, on: true },
        ];
        for req in reqs {
            let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
            assert_eq!(back, req);
        }
        let resp = Response::Events { space: space.clone(), events: vec![ev] };
        let back_resp: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back_resp, resp);

        let change = Change::Events { space: space.clone() };
        assert!(change.same_scope(&Change::Events { space }));
        assert!(!change.is_ephemeral());
    }

    #[test]
    fn space_edit_rights_and_event_admin_round_trip() {
        use crate::SpaceRights;
        let rights = SpaceRights {
            manage_space: true,
            manage_roles: false,
            manage_events: true,
            owner: true,
            ..SpaceRights::default()
        };
        assert_eq!(SpaceRights::from_bits(rights.to_bits()), rights);
        assert!(rights.any_settings());
        assert!(SpaceRights::default().is_empty());

        let account = AccountKey { network: Network::Discord, account: "1000".into() };
        let space = Key::new(&account, "4000");
        let edit = SpaceEdit {
            name: Some("New Name".into()),
            description: Some("Fresh description".into()),
            afk_timeout: Some(900),
            default_notify: Some(Notify::Mentions),
            ..SpaceEdit::default()
        };
        let ev_edit = SpaceEventEdit {
            name: "Town Hall".into(),
            description: "Monthly sync".into(),
            start: 1_760_000_000,
            end: Some(1_760_003_600),
            place_kind: EventPlaceKind::External,
            channel_id: None,
            location: Some("https://example.com/hall".into()),
            image: None,
        };
        let reqs = [
            Request::EditSpace { space: space.clone(), edit},
            Request::DeleteSpace { space: space.clone(), mfa_code: Some("123456".into())},
            Request::TransferSpace {
                space: space.clone(),
                new_owner: Key::new(&account, "2001"),
                mfa_code: None,
            },
            Request::CreateEvent { space: space.clone(), event: ev_edit.clone() },
            Request::EditEvent { space: space.clone(), event_id: "9001".into(), event: ev_edit },
            Request::DeleteEvent { space, event_id: "9001".into() },
        ];
        for req in reqs {
            let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
            assert_eq!(back, req);
        }
    }

    #[test]
    fn space_invites_requests_and_response_round_trip() {
        use crate::Response;
        let account = AccountKey { network: Network::Discord, account: "1000".into() };
        let space = Key::new(&account, "4000");
        let reqs = [
            Request::SpaceInvites { space: space.clone() },
            Request::RevokeSpaceInvite { space: space.clone(), code: "abc123".into() },
            Request::SetInvitesPaused { space: space.clone(), paused: true },
        ];
        for req in reqs {
            let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
            assert_eq!(back, req);
        }
        let resp = Response::SpaceInvites {
            space,
            paused: true,
            invites: vec![
                SpaceInvite {
                    code: "rust".into(),
                    url: "https://discord.gg/rust".into(),
                    inviter: String::new(),
                    channel_id: None,
                    channel_name: String::new(),
                    uses: 0,
                    max_uses: 0,
                    expires_at: None,
                    vanity: true,
                },
                SpaceInvite {
                    code: "abc123".into(),
                    url: "https://discord.gg/abc123".into(),
                    inviter: "Justin".into(),
                    channel_id: Some("4101".into()),
                    channel_name: "general".into(),
                    uses: 4,
                    max_uses: 10,
                    expires_at: Some(1_760_100_000),
                    vanity: false,
                },
            ],
        };
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn role_requests_round_trip_and_omit_defaults() {
        use crate::Response;
        let account = AccountKey { network: Network::Discord, account: "1000".into() };
        let space = Key::new(&account, "4777");
        let person = Key::new(&account, "2006");
        let edit = RoleEdit {
            name: Some("Helper".into()),
            color: Some(0xFF_00_00),
            hoist: Some(true),
            mentionable: Some(false),
            permissions: Some(1 << 28),
            confirm_admin: true,
            ..RoleEdit::default()
        };
        let role = SpaceRole {
            id: "4701".into(),
            name: "Newbie".into(),
            color: 0xFF_00_00,
            hoist: true,
            position: 1,
            mentionable: true,
            managed: false,
            icon: "https://cdn.discordapp.com/role-icons/4701/a1b2c3d4.png".into(),
            permissions: 0,
            editable: true,
            assignable: true,
        };
        let reqs = [
            Request::Roles { space: space.clone() },
            Request::CreateRole { space: space.clone(), edit: edit.clone() },
            Request::EditRole { space: space.clone(), role_id: "4701".into(), edit: edit.clone() },
            Request::MoveRoles {
                space: space.clone(),
                roles: vec![RoleMove { id: "4701".into(), position: 2 }],
            },
            Request::DeleteRole { space: space.clone(), role_id: "4701".into() },
            Request::SetMemberRoles { space: space.clone(), person, roles: vec!["4701".into()] },
        ];
        for req in reqs {
            let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
            assert_eq!(back, req);
        }
        let bare = Request::CreateRole { space: space.clone(), edit: RoleEdit::default() };
        let mut value = serde_json::to_value(&bare).unwrap();
        value.as_object_mut().unwrap().remove("edit");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(back, bare);
        let omitted: SpaceRole = serde_json::from_value(serde_json::json!({"id": "1", "name": "Helper"})).unwrap();
        assert_eq!(omitted.color, 0);
        assert!(!omitted.hoist && !omitted.mentionable && !omitted.managed);
        assert_eq!(omitted.position, 0);
        assert!(omitted.icon.is_empty());
        assert_eq!(omitted.permissions, 0);
        assert!(!omitted.editable && !omitted.assignable);
        let resp = Response::Roles { space, roles: vec![role.clone()] };
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
        let one = Response::Role { role };
        let back: Response = serde_json::from_str(&serde_json::to_string(&one).unwrap()).unwrap();
        assert_eq!(back, one);
    }

    #[test]
    fn space_integrations_and_webhook_requests_round_trip() {
        use crate::{AccountKey, Request, Response};
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let space = Key::new(&account, "4000");
        let conv = Key::new(&account, "4101");
        let reqs = [
            Request::SpaceIntegrations { space: space.clone() },
            Request::CreateWebhook { conversation: conv.clone(), name: "Deployments".into() },
            Request::EditWebhook {
                space: space.clone(),
                webhook_id: "9001".into(),
                name: Some("CI".into()),
                channel_id: Some("4102".into()),
            },
            Request::DeleteWebhook { space: space.clone(), webhook_id: "9001".into() },
            Request::RemoveIntegration { space: space.clone(), integration_id: "8001".into() },
        ];
        for req in reqs {
            let wire = serde_json::to_string(&req).unwrap();
            let back: Request = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, req);
        }
        let hook = SpaceWebhook {
            id: "9001".into(),
            name: "CI".into(),
            channel_id: "4101".into(),
            channel_name: "general".into(),
            kind: WebhookKind::Incoming,
            url: "https://discord.com/api/webhooks/9001/secret".into(),
            creator: "Justin".into(),
            ..SpaceWebhook::default()
        };
        let page = SpaceIntegrations {
            webhooks: vec![hook.clone()],
            followers: vec![SpaceWebhook {
                id: "9002".into(),
                name: "Rust News".into(),
                channel_id: "4101".into(),
                kind: WebhookKind::ChannelFollower,
                source_guild: "Rust".into(),
                source_channel: "announcements".into(),
                ..SpaceWebhook::default()
            }],
            integrations: vec![SpaceIntegration {
                id: "8001".into(),
                name: "HelperBot".into(),
                kind: "discord".into(),
                enabled: true,
                application_id: "7001".into(),
                bot_id: "7002".into(),
                bot_name: "HelperBot".into(),
                ..SpaceIntegration::default()
            }],
            can_manage_webhooks: true,
            can_manage_guild: true,
            add_bot_url: "https://discord.com/discovery/applications".into(),
            permissions_url: "https://discord.com/channels/4000".into(),
        };
        let resp = Response::SpaceIntegrations { space, integrations: Box::new(page) };
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
        let hook_resp = Response::SpaceWebhook { webhook: Box::new(hook) };
        let back_hook: Response = serde_json::from_str(&serde_json::to_string(&hook_resp).unwrap()).unwrap();
        assert_eq!(back_hook, hook_resp);
    }

    #[test]
    fn space_expressions_requests_and_responses_round_trip() {
        use crate::{AccountKey, Request, Response};
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let space = Key::new(&account, "4000");
        let reqs = [
            Request::SpaceExpressions { space: space.clone() },
            Request::CreateEmoji { space: space.clone(), name: "ferris".into(), path: "/tmp/ferris.png".into() },
            Request::EditEmoji {
                space: space.clone(),
                emoji_id: "7001".into(),
                name: Some("ferris_happy".into()),
                roles: Some(vec!["4010".into()]),
            },
            Request::DeleteEmoji { space: space.clone(), emoji_id: "7001".into() },
            Request::CreateSticker {
                space: space.clone(),
                name: "Wave".into(),
                tags: "wave".into(),
                description: "Waving mascot".into(),
                path: "/tmp/wave.png".into(),
            },
            Request::EditSticker {
                space: space.clone(),
                sticker_id: "8001".into(),
                name: Some("Big Wave".into()),
                tags: Some("hand".into()),
                description: Some("Updated".into()),
            },
            Request::DeleteSticker { space: space.clone(), sticker_id: "8001".into() },
        ];
        for req in reqs {
            let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
            assert_eq!(back, req);
        }
        let emoji = SpaceEmoji {
            id: "7001".into(),
            name: "ferris".into(),
            roles: vec!["4010".into()],
            ..SpaceEmoji::default()
        };
        let sticker = SpaceSticker {
            id: "8001".into(),
            name: "Wave".into(),
            tags: "wave".into(),
            format: 1,
            ..SpaceSticker::default()
        };
        let expr = SpaceExpressions {
            emojis: vec![emoji.clone()],
            stickers: vec![sticker.clone()],
            roles: vec![SpaceRoleChoice { id: "4010".into(), name: "mods".into() }],
            static_emoji_limit: 50,
            animated_emoji_limit: 50,
            sticker_limit: 5,
            can_manage: true,
            can_create: true,
            premium_tier: 0,
        };
        let resp = Response::SpaceExpressions { space, expressions: Box::new(expr) };
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
        let e_resp = Response::SpaceEmoji { emoji: Box::new(emoji) };
        assert_eq!(serde_json::from_str::<Response>(&serde_json::to_string(&e_resp).unwrap()).unwrap(), e_resp);
        let s_resp = Response::SpaceSticker { sticker: Box::new(sticker) };
        assert_eq!(serde_json::from_str::<Response>(&serde_json::to_string(&s_resp).unwrap()).unwrap(), s_resp);
    }

    #[test]
    fn discover_requests_round_trip_and_default_the_new_fields() {
        use crate::Response;
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let listed = Request::Discover { account: account.clone(), query: String::new(), category: 0, offset: 0, limit: 0 };
        let wire = serde_json::to_string(&listed).unwrap();
        assert!(!wire.contains("\"query\"") && !wire.contains("category"), "{wire}");
        assert_eq!(serde_json::from_str::<Request>(&wire).unwrap(), listed);
        let account_json = serde_json::to_string(&account).unwrap();
        let bare = format!(r#"{{"op":"discover","account":{account_json}}}"#);
        assert_eq!(serde_json::from_str::<Request>(&bare).unwrap(), listed);
        let searched = Request::Discover { account: account.clone(), query: "lichen".into(), category: 1, offset: 24, limit: 24 };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&searched).unwrap()).unwrap(), searched);
        let join = Request::JoinDiscovered { account, guild_id: "5000".into() };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&join).unwrap()).unwrap(), join);
        let guild = DiscoveredGuild { id: "5000".into(), name: "Lichen".into(), description: String::new(), icon: String::new(), members: 12, online: 3 };
        let resp = Response::Discovered { guilds: vec![guild], total: 1 };
        assert_eq!(serde_json::from_str::<Response>(&serde_json::to_string(&resp).unwrap()).unwrap(), resp);
    }

    #[test]
    fn space_gate_requests_round_trip_and_default_the_new_fields() {
        use crate::Response;
        let space = Key { network: Network::Discord, account: "1".into(), id: "4000".into() };
        let bare = Request::SpaceGate { space: space.clone() };
        let wire = serde_json::to_string(&bare).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&wire).unwrap(), bare);
        let accept = Request::AcceptRules { space: space.clone(), answers: vec!["true".into()] };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&accept).unwrap()).unwrap(), accept);
        let empty = format!(r#"{{"op":"accept_rules","space":{}}}"#, serde_json::to_string(&space).unwrap());
        assert!(matches!(serde_json::from_str::<Request>(&empty).unwrap(), Request::AcceptRules { answers, .. } if answers.is_empty()));
        let submit = Request::SubmitOnboarding { space: space.clone(), options: vec!["6101".into()] };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&submit).unwrap()).unwrap(), submit);
        let gate = SpaceGate {
            pending: true,
            description: "Be kind".into(),
            reason: "Accept the rules to talk here".into(),
            external: "https://discord.com/app".into(),
            fields: vec![GateField { label: "Rules".into(), kind: "TERMS".into(), values: vec!["Be kind".into()], required: true, choices: Vec::new() }],
            ..SpaceGate::default()
        };
        let resp = Response::SpaceGate { space, gate };
        assert_eq!(serde_json::from_str::<Response>(&serde_json::to_string(&resp).unwrap()).unwrap(), resp);
        let omitted = r#"{"op":"space_gate","space":{"network":"discord","account":"1","id":"4000"},"gate":{}}"#;
        let back: Response = serde_json::from_str(omitted).unwrap();
        match back {
            Response::SpaceGate { gate, .. } => {
                assert!(!gate.pending && gate.fields.is_empty() && gate.external.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn set_activity_round_trips_and_an_omitted_name_is_empty() {
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let set = Request::SetActivity { account: account.clone(), name: "Celeste".into() };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&set).unwrap()).unwrap(), set);
        let bare = format!(r#"{{"op":"set_activity","account":{}}}"#, serde_json::to_string(&account).unwrap());
        assert!(matches!(serde_json::from_str::<Request>(&bare).unwrap(), Request::SetActivity { name, .. } if name.is_empty()));
    }

    #[test]
    fn send_rules_defaults_to_all_allowed_and_round_trips() {
        let default: SendRules = serde_json::from_str("{}").unwrap();
        assert_eq!(default, SendRules::default());
        assert!(default.is_default());
        assert!(default.allows_any_attachment());
        assert_eq!(default.slowmode_remaining(1_000), 0);
        assert_eq!(serde_json::to_string(&default).unwrap(), "{}");

        let restricted = SendRules {
            send_media: false,
            send_stickers: false,
            send_gifs: false,
            slowmode_seconds: 30,
            slowmode_next_send: Some(1_025),
            ban_until: Some(2_000),
            paid_stars: Some(50),
            ..SendRules::default()
        };
        assert!(!restricted.allows_any_attachment());
        assert_eq!(restricted.slowmode_remaining(1_000), 25);
        assert_eq!(restricted.slowmode_remaining(1_030), 0);
        let wire = serde_json::to_string(&restricted).unwrap();
        let back: SendRules = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, restricted);

        let paid = Outgoing { allow_paid_stars: Some(50), ..Outgoing::default() };
        assert!(!paid.is_plain());
        let back_paid: Outgoing = serde_json::from_str(&serde_json::to_string(&paid).unwrap()).unwrap();
        assert_eq!(back_paid, paid);
    }

    #[test]
    fn activity_item_kind_missed_call_round_trips() {
        let wire = serde_json::to_string(&ActivityItemKind::MissedCall).unwrap();
        assert_eq!(wire, "\"missed_call\"");
        let back: ActivityItemKind = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, ActivityItemKind::MissedCall);
        let unknown: ActivityItemKind = serde_json::from_str("\"future_activity_kind\"").unwrap();
        assert_eq!(unknown, ActivityItemKind::Other);
    }

    #[test]
    fn history_share_round_trips_and_stays_backward_compatible() {
        for mode in [HistoryShare::None, HistoryShare::Days { days: 7 }, HistoryShare::All] {
            let wire = serde_json::to_string(&mode).unwrap();
            let back: HistoryShare = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, mode);
        }
        let legacy = json_members_add_without_share();
        let req: Request = serde_json::from_str(&legacy).unwrap();
        assert!(matches!(req, Request::MembersAdd { share_history: None, .. }));
    }

    fn json_members_add_without_share() -> String {
        r#"{"op":"members_add","conversation":{"network":"teams","account":"t1","id":"19:g@thread.v2"},"people":["8:orgid:a"]}"#.to_owned()
    }

    #[test]
    fn space_info_link_round_trips_and_defaults_empty() {
        let space = Space {
            key: Key { network: Network::Teams, account: "t1".into(), id: "19:g@thread.tacv2".into() },
            name: "Platform".into(),
            icon: None,
            position: 0,
            attention: 0,
            unread: false,
            folder: None,
            muted: false,
            hide_muted: false,
            can_create_channels: true,
            can_manage_webhooks: false,
            can_manage_guild: false,
            can_manage_expressions: false,
            can_create_invites: false,
            opt_in_channels: false,
            rights: crate::SpaceRights::default(),
        };
        let info = SpaceInfo {
            space,
            description: "Core".into(),
            members: Some(4),
            online: None,
            nickname: String::new(),
            roles: vec!["Owner".into()],
            notify: Notify::All,
            tags: Vec::new(),
            suppress_everyone: false,
            suppress_roles: false,
            highlights: true,
            events: true,
            mobile_push: true,
            channel_overrides: Vec::new(),
            privacy: None,
            can_change_nickname: false,
            banner: None,
            boost_level: 0,
            boosts: 0,
            mark: None,
            system_channel_id: None,
            afk_channel_id: None,
            afk_timeout: 0,
            default_notify: None,
            vanity_url: None,
            invites_disabled: false,
            domain: String::new(),
            url: String::new(),
            link: "https://teams.microsoft.com/l/team/19%3Ag/conversations?groupId=g1&tenantId=t1".into(),
        };
        let wire = serde_json::to_string(&info).unwrap();
        let back: SpaceInfo = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, info);

        let without_link = wire.replace(&format!(r#","link":"{}""#, info.link), "");
        let legacy: SpaceInfo = serde_json::from_str(&without_link).unwrap();
        assert!(legacy.link.is_empty());
    }
}
