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

/// What an account may send, as the session says (the plan, the workspace's settings). A
/// control the account lacks is absent in the shell, never disabled. Nothing is allowed until
/// the adapter reports it (`Context::capabilities`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Largest file, in bytes. 0: files cannot be sent.
    #[serde(default, skip_serializing_if = "crate::is_zero64")]
    pub max_file_bytes: u64,
    /// Most files in one message.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_files: u32,
    /// Longest caption under files, in characters. 0: the text limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub caption_limit: u32,
    /// Per-file options.
    #[serde(default, skip_serializing_if = "is_false")]
    pub spoiler: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub as_file: bool,
    /// The network has a GIF search tinyclient can ask (`Catalog` with `Gifs`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub gifs: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stickers: bool,
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
    /// Seconds after sending that an edit is still allowed. 0: no limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edit_window: u32,
}

impl Capabilities {
    pub fn is_none(&self) -> bool {
        *self == Capabilities::default()
    }
}

/// One file the user attached: the shell names a path on disk and chatd streams it from there.
/// Bytes never cross the socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Unix seconds: the network sends it then (Slack, Telegram). tinyclient never runs a timer
    /// that sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<i64>,
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
