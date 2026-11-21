//! What the polish round added beside the rows: how a network files its conversations (folders,
//! pins, archive), what a person is doing, the typed card and info panes, what an account may
//! send, the pickers' catalogs, and what chatd is doing with the keychain.
//!
//! Every field here is optional on the wire and every list has a cap, as in the rest of the
//! contract.

use crate::{is_false, is_zero, Conversation, Key, MediaFile, Network, Space};
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
pub const PROFILE_CONTACTS: usize = 6;
/// Most files one `Send` carries. A network that takes fewer says so in `Capabilities`.
pub const ATTACHMENTS_LIMIT: usize = 10;
/// Most items one catalog answer carries.
pub const CATALOG_LIMIT: usize = 200;
/// Most pinned messages one `Pinned` answer carries.
pub const PINNED_LIMIT: usize = 50;
/// Most people each section of `Around` lists.
pub const AROUND_LIMIT: usize = 100;

fn is_true(value: &bool) -> bool {
    *value
}

fn yes() -> bool {
    true
}

/// Which account privacy setting to read or update (`Request::GetPrivacy`, `Request::SetPrivacy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyKey {
    PhoneCall,
    PhoneP2p,
    VoiceMessages,
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
}

impl Activity {
    pub fn bound(&mut self) {
        crate::clip(&mut self.name, 128);
        crate::clip(&mut self.place, 128);
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

/// The one heads-up line on a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    Scam,
    Fake,
    /// The network says impersonation or spam about this account.
    Spam,
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
        }
    }
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
    pub profile_nickname: bool,
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

/// What kind of event an [`ActivityItem`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityItemKind {
    Mention,
    ThreadReply,
    Reaction,
    App,
    Invite,
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

/// Content-type filter for network message search (`SearchFilters::has`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchHas {
    Files,
    Links,
    Images,
}

/// Optional filters on `Request::SearchNetwork`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has: Option<SearchHas>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_me: bool,
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

    /// True if any own-profile field (name, about, pronouns, avatar) can be edited on this network.
    pub fn can_edit_profile(&self) -> bool {
        self.profile_name || self.profile_about || self.profile_pronouns || self.profile_avatar
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
            spans: Vec::new(),
            poll: None,
            importance: Importance::Normal,
            broadcast: false,
            effect: None,
            contact: None,
            send_as: None,
            snippet_type: None,
            subject: None,
        }
    }
}

impl Outgoing {
    /// Nothing but text: what every adapter could already send.
    pub fn is_plain(&self) -> bool {
        *self == Outgoing::default()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountKey, Key, Network, Request};

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
                photo: false,
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
}
