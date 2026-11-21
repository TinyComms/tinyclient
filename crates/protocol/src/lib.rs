//! The contract between the processes: the shell and chatd over the chat socket, chatd and mediad
//! over mediad's stdin and stdout. Length-prefixed JSON, local only, at most 1 MiB a frame.
//!
//! Identity. Everything belongs to an account, and an account is `(network, account)`: one Slack
//! sign-in yields one account per workspace. Conversations, Spaces and people are named by a
//! [`Key`], which adds the network's own id. A Space is a server, workspace or team.
//!
//! Every list request is bounded. Nothing answers with "everything".

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;

pub mod browser;
mod call;
pub mod crash;
pub mod device_approval;
mod e2ee;
pub mod highlight;
pub mod hotkey;
pub mod log_policy;
pub mod media_host;
pub mod private;
mod info;
pub mod ipc;
pub mod launch;
pub mod link;
mod message;
pub mod otp;
mod play;
mod record;
pub mod quarantine;
mod react;
pub mod settings;
mod signin;
pub mod telegram_call_emoji;
mod tiny;
pub mod web_sign_in;

pub use call::*;
pub use crash::redact;
pub use e2ee::*;
pub use info::*;
pub use ipc::{ChatListener, ChatStream};
pub use message::*;
pub use play::*;
pub use react::*;
pub use record::*;
pub use signin::*;
pub use telegram_call_emoji::*;
pub use tiny::*;

pub const CHAT_SOCK_NAME: &str = "chat.sock";

/// Most rows one answer carries, whatever the request asked for.
pub const PAGE_LIMIT: u32 = 200;
/// Most people the shell may watch at once (presence).
pub const WATCH_LIMIT: usize = 500;
/// Most people a voice channel's row names. `Occupants::total` says how many there are.
pub const OCCUPANTS_LIMIT: usize = 25;
/// Most Spaces a [`Profile`] lists as shared.
pub const MUTUAL_SPACES: usize = 20;
/// Most labelled lines a [`Profile`] carries.
pub const PROFILE_FIELDS: usize = 12;
/// Fewest characters of a people search that reach a network.
pub const SEARCH_MIN_CHARS: usize = 2;
/// Most characters of a draft chatd keeps.
pub const DRAFT_LIMIT: usize = 4_000;

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn is_true(value: &bool) -> bool {
    *value
}

pub(crate) fn yes() -> bool {
    true
}

pub(crate) fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(crate) fn is_zero64(value: &u64) -> bool {
    *value == 0
}

pub(crate) fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Telegram,
    Discord,
    Slack,
    Teams,
    Signal,
    /// Android Messages: Google Messages on the phone, paired to this computer.
    Messages,
    /// A Tiny server the owner already runs. Optional, and separate from the others.
    Tiny,
}

impl Network {
    pub const ALL: [Network; 7] = [
        Network::Telegram,
        Network::Discord,
        Network::Slack,
        Network::Teams,
        Network::Signal,
        Network::Messages,
        Network::Tiny,
    ];

    /// The label the shell shows on a row. The network is a label, never a different layout.
    pub fn label(self) -> &'static str {
        match self {
            Network::Telegram => "Telegram",
            Network::Discord => "Discord",
            Network::Slack => "Slack",
            Network::Teams => "Teams",
            Network::Signal => "Signal",
            Network::Messages => "Messages",
            Network::Tiny => "Tiny server",
        }
    }
}

/// One signed-in identity. `account` is chosen by the adapter and is stable across restarts: a
/// Slack team id, a Discord user id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountKey {
    pub network: Network,
    pub account: String,
}

/// Names a conversation, a Space or a person: the account it belongs to and the network's own id.
/// Which of the three it is follows from the field it is in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub network: Network,
    pub account: String,
    pub id: String,
}

impl Key {
    pub fn new(account: &AccountKey, id: impl Into<String>) -> Key {
        Key {
            network: account.network,
            account: account.account.clone(),
            id: id.into(),
        }
    }

    pub fn account(&self) -> AccountKey {
        AccountKey {
            network: self.network,
            account: self.account.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    /// No session. This is the default until the user signs in.
    SignedOut,
    /// The network ended the session. What is cached stays readable; sending is refused until the
    /// user signs in again.
    Expired,
    /// Signed in, but the socket is down or still connecting. `AccountStatus::reconnecting` says
    /// whether chatd is trying right now.
    Offline,
    Connected,
}

/// A network as a whole: the best link of its accounts, `SignedOut` when it has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub network: Network,
    pub link: Link,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub key: AccountKey,
    /// The workspace's name, a Tiny server's host, or the user's own name where an account is a person.
    pub name: String,
    pub link: Link,
    /// Why the link is what it is, in words, when the network said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Our own person id on this account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub me: String,
    /// The sum of the numbers on this account's rows (see [`Conversation::badge`]).
    pub attention: u32,
    /// Something unmuted is unread that has no number.
    pub unread: bool,
    /// With `Offline`: the connection dropped and chatd is trying again within seconds. False
    /// while `Offline` means it has not connected yet, or has backed off to trying rarely.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reconnecting: bool,
    /// Something only the network's own app can do stands in the way (a captcha, a
    /// verification), in words: "Discord wants you to verify your account in its app." Empty
    /// when nothing does. The link may be anything; `Connected` with this set still fails
    /// most requests.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub required: String,
    /// Our own picture on this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    /// Our own presence and status text as the network shows them to others.
    #[serde(default)]
    pub presence: Presence,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// What this account may send: file limits, pickers and options. All off until the adapter
    /// reports them.
    #[serde(default, skip_serializing_if = "Capabilities::is_none")]
    pub can: Capabilities,
    /// Selectable sending identities or lines on this account (e.g. dual-SIM cards on Messages).
    /// Empty when the account has only one line or does not offer a chooser.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub send_as: Vec<SendAs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Media {
    /// mediad is not running. Calls and screen share are impossible in this state.
    Stopped,
    Running,
}

/// A server, workspace or team. Every account also has its direct conversations, which belong to
/// no Space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub key: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<MediaFile>,
    /// The network's own order.
    pub position: i32,
    pub attention: u32,
    pub unread: bool,
    /// The folder it is filed in (Discord's server folders), by `Folder::key` id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Muted as a whole: every row in it is drawn dimmed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// The user's own "hide muted channels" for this Space: muted rows with nothing for me are
    /// not drawn in its tree.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hide_muted: bool,
    /// The user has permission to create channels in this Space (`Request::CreateChannel`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_create_channels: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    /// A category or sidebar section of channels.
    #[default]
    Section,
    /// The direct and group conversations of a Space whose network keeps them inside it (a
    /// Slack workspace). Its rows are `Direct` and `Group` conversations with `space` set.
    Directs,
}

impl GroupKind {
    fn is_section(&self) -> bool {
        *self == GroupKind::Section
    }
}

/// A collapsible header inside a Space: a category, a sidebar section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub position: i32,
    /// Remembered by chatd, never sent to the network.
    #[serde(default, skip_serializing_if = "is_false")]
    pub collapsed: bool,
    #[serde(default, skip_serializing_if = "GroupKind::is_section")]
    pub kind: GroupKind,
    /// How many conversations sit under it, whatever page of them an answer carries. Filled by
    /// chatd on `Tree`; an adapter leaves it 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub count: u32,
    /// Muted on the network (a muted category): it starts collapsed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// The user's own "unread only" for this section (Slack): rows with nothing unread are not
    /// drawn, except the open one and starred ones.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unread_only: bool,
}

impl Group {
    pub fn new(id: impl Into<String>, name: impl Into<String>, position: i32) -> Group {
        Group {
            id: id.into(),
            name: name.into(),
            position,
            collapsed: false,
            kind: GroupKind::Section,
            count: 0,
            muted: false,
            unread_only: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Direct,
    /// A group direct message.
    Group,
    Channel,
    Announcement,
    Voice,
    Forum,
    Thread,
}

impl ConversationKind {
    /// Every unread message in these needs me; in the rest only mentions do.
    pub fn is_direct(self) -> bool {
        matches!(self, ConversationKind::Direct | ConversationKind::Group)
    }
}

/// What pressing the call button of a conversation does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallSupport {
    #[default]
    None,
    /// tinyclient carries the call itself.
    Native,
    /// The official app does: ask `Handoff` for the link and open it.
    Handoff,
    /// The other person's privacy settings restrict calls from this account.
    Restricted,
}

/// Someone in a voice channel, as its row in the tree shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occupant {
    pub person: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deafened: bool,
    /// A moderator muted or deafened them on the server.
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_deafened: bool,
    /// They may not speak in this voice channel.
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppressed: bool,
    /// Sharing a screen.
    #[serde(default, skip_serializing_if = "is_false")]
    pub streaming: bool,
    /// Camera is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub camera: bool,
    /// Heard right now, where the network says so to someone in the call (a Tiny server's
    /// `call.speaking`), or on stage: a stage channel's platform holds whoever Discord does
    /// not suppress.
    #[serde(default, skip_serializing_if = "is_false")]
    pub speaking: bool,
}

/// Who is in a voice channel, whether or not we are. Never stored: it arrives as
/// `Change::Occupants` and rides on the row when the row is read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occupants {
    /// At most [`OCCUPANTS_LIMIT`], in the network's order. We are in it when we are in the
    /// channel.
    pub people: Vec<Occupant>,
    /// How many are in the channel in all.
    pub total: u32,
}

/// What a row shows beside its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    None,
    Dot,
    Count(u32),
}

/// One row of the Inbox or of a Space's tree. No message bodies beyond the preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub key: Key,
    pub kind: ConversationKind,
    pub title: String,
    /// The Space it is in. Direct conversations have none, except on a network that keeps them
    /// inside a Space (see [`GroupKind::Directs`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// That Space's name, so a row can say where it is without a second request.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space_title: String,
    /// The [`Group`] it sits under in its Space's tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// That Group's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group_title: String,
    /// What the conversation is for, as its owners wrote it. One line, cut to 300 characters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// For a `Thread`: the conversation it hangs off, and the message there that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// The network's own order inside its Space.
    #[serde(default)]
    pub position: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    /// The other person of a `Direct` conversation, for presence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// Unread messages. For a channel the network may only say "some", which is 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unread: u32,
    /// Unread messages that mention me.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mentions: u32,
    /// Muted on the network. Drawn dimmed, never a dot, still a quiet mention count.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Pinned to the top of the Inbox. Local; the network never hears of it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub starred: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
    /// Unix seconds of the newest message.
    #[serde(default)]
    pub updated: i64,
    /// The newest message read, for the "New" divider. `Messages` answers with the value from
    /// before the shell marks anything read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<String>,
    #[serde(default)]
    pub call: CallSupport,
    /// The other side supports video calls (Telegram's `video_calls_available`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub call_video: bool,
    /// Who is in it, for a voice channel someone is in. `None` when it is empty or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupants: Option<Occupants>,
    /// How many may be in a voice channel at once, 0 when it has no limit. Filled by the
    /// network's adapter from the channel; the shell draws `4/12` on an occupied voice row.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub limit: u32,
    /// I may read it but not post in it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// Only invited people see it: drawn with a lock.
    #[serde(default, skip_serializing_if = "is_false")]
    pub private: bool,
    /// End-to-end encrypted or not, where the network says (a Tiny server). A plaintext
    /// conversation can be upgraded once; messages from before that stay readable by the server.
    #[serde(default, skip_serializing_if = "Encryption::is_unknown")]
    pub encryption: Encryption,
    /// What the user had typed and not sent, kept by chatd across restarts (`SetDraft`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft: String,

    // How the network files it. Shown only in that network's own home, never in the Inbox.
    /// The folders it is in (Telegram's chat folders), by `Folder::key` id. A chat in two
    /// folders is in both. At most [`FOLDERS_PER_ROW`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// In the network's archive: out of the Inbox and every count unless it mentions me.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
    /// Pinned on the network, and where among the pins (lowest first). Separate from `starred`,
    /// which is tinyclient's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<i32>,
    /// Hidden on the network (a Teams chat the user hid): never listed. The adapter unhides it
    /// when a new message arrives, as the network does.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// A followed thread (one the user joined or replied in): the Threads list holds these.
    #[serde(default, skip_serializing_if = "is_false")]
    pub followed: bool,

    // What it is.
    /// The other side is a bot or an app. Never among people: the Inbox folds these into one
    /// Apps row, and their plain unread raises no number anywhere.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bot: bool,
    /// A meeting's chat (Teams). The shell moves one quiet for 14 days (`updated`) into a
    /// collapsed Meetings group.
    #[serde(default, skip_serializing_if = "is_false")]
    pub meeting: bool,
    #[serde(default, skip_serializing_if = "Special::is_none")]
    pub special: Special,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
    /// Telegram's own topic color for a `Thread` row that hangs off a forum (its palette
    /// index), the tile the row draws its icon with. `None` on every other row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_icon: Option<i32>,
    /// The user did not opt in to it (Discord's "show all channels" off): drawn only under the
    /// tree's "Browse n more channels".
    #[serde(default, skip_serializing_if = "is_false")]
    pub opted_out: bool,
    /// Filled by chatd on reading: nothing said in 30 days, nothing unread, not starred and no
    /// draft. Slack folds these into "n quiet channels".
    #[serde(default, skip_serializing_if = "is_false")]
    pub quiet: bool,

    // The preview line's parts.
    /// Who said the newest message, in a group or channel. Empty when it was me (`preview_mine`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview_from: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub preview_mine: bool,
    /// My newest message has been read by the other side (Telegram's ✓✓, Teams' "Seen").
    #[serde(default, skip_serializing_if = "is_false")]
    pub seen: bool,
    /// My newest message has reached the other side's device (Signal's and Google Messages'
    /// second tick). `seen` implies it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub delivered: bool,
    /// My newest message is still going.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sending: bool,
    /// Per-conversation upload limit override, in bytes (e.g. a boosted Discord server). 0: use account's `Capabilities::max_file_bytes`.
    #[serde(default, skip_serializing_if = "is_zero64")]
    pub max_file_bytes: u64,
    /// Why this conversation is read-only, in words (e.g. a Discord server timeout). Empty uses the default sentence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub read_only_reason: String,
    /// Flagged as spam by the network (e.g. Discord spam message request).
    #[serde(default, skip_serializing_if = "is_false")]
    pub spam: bool,
    /// Per-conversation moderation and management rights for this account.
    #[serde(default, skip_serializing_if = "ConversationRights::is_none")]
    pub rights: ConversationRights,
    /// Disappearing-messages timer in seconds (`None` or `Some(0)` means off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing: Option<u32>,
    /// This account may delete other members' messages in this conversation (Tiny `manage_messages`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub moderate: bool,
    /// This account may pin and unpin messages in this conversation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_pin: bool,
    /// Whether this account has joined the conversation (`false` when previewing an unjoined public channel).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub joined: bool,
}

/// What became of my newest message, as its mark says it (`design/06-live-signals.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receipt {
    Sending,
    Sent,
    Delivered,
    Read,
}

impl Conversation {
    /// A row with nothing said about it but what it is: for tests, and for the shell's own
    /// stand-in while the real row loads.
    pub fn new(key: Key, kind: ConversationKind, title: impl Into<String>) -> Conversation {
        Conversation {
            key,
            kind,
            title: title.into(),
            space: None,
            space_title: String::new(),
            group: None,
            group_title: String::new(),
            topic: String::new(),
            parent: None,
            root: None,
            position: 0,
            avatar: None,
            peer: None,
            unread: 0,
            mentions: 0,
            muted: false,
            starred: false,
            preview: String::new(),
            updated: 0,
            last_read: None,
            call: CallSupport::None,
            call_video: false,
            occupants: None,
            limit: 0,
            read_only: false,
            private: false,
            encryption: Encryption::Unknown,
            draft: String::new(),
            folders: Vec::new(),
            archived: false,
            pin: None,
            hidden: false,
            followed: false,
            bot: false,
            meeting: false,
            special: Special::None,
            mark: None,
            topic_icon: None,
            opted_out: false,
            quiet: false,
            preview_from: String::new(),
            preview_mine: false,
            seen: false,
            delivered: false,
            sending: false,
            max_file_bytes: 0,
            read_only_reason: String::new(),
            spam: false,
            rights: ConversationRights::default(),
            disappearing: None,
            moderate: false,
            can_pin: false,
            joined: true,
        }
    }

    /// Whether this account may post messages in this conversation.
    pub fn can_post(&self) -> bool {
        !self.read_only
    }

    /// The mark on my newest message. `None` when the newest is not mine.
    pub fn receipt(&self) -> Option<Receipt> {
        if !self.preview_mine {
            None
        } else if self.sending {
            Some(Receipt::Sending)
        } else if self.seen {
            Some(Receipt::Read)
        } else if self.delivered {
            Some(Receipt::Delivered)
        } else {
            Some(Receipt::Sent)
        }
    }

    /// The line under the title in the header: `Space > group`, then the topic where known.
    /// Empty for a direct conversation outside any Space.
    pub fn subtitle(&self) -> String {
        let place = [self.space_title.as_str(), self.group_title.as_str()];
        let place: Vec<&str> = place.into_iter().filter(|part| !part.is_empty()).collect();
        let mut line = place.join(" › ");
        if !self.topic.is_empty() {
            if !line.is_empty() {
                line.push_str(" · ");
            }
            line.push_str(&self.topic);
        }
        line
    }

    /// The one rule for numbers and dots. A number means someone needs me: every unread message
    /// of a direct conversation with a person, each mention anywhere. A dot is an unmuted row
    /// with unread messages and no mention. A bot, an archived chat and a message request raise
    /// a number only for a mention.
    pub fn badge(&self) -> Badge {
        let quiet = self.bot || self.archived || self.special == Special::Request;
        let needs_me = if self.kind.is_direct() && !self.muted && !quiet {
            self.unread.max(self.mentions)
        } else {
            self.mentions
        };
        match (needs_me, self.unread, self.muted) {
            (0, 0, _) | (0, _, true) => Badge::None,
            (0, _, false) => Badge::Dot,
            (count, _, _) => Badge::Count(count),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// Seen somewhere: a message author, a search result.
    #[default]
    None,
    /// Someone I have a direct conversation with, on networks that have no friends.
    Contact,
    Friend,
    /// They asked to be my friend.
    Incoming,
    /// I asked.
    Outgoing,
    Blocked,
    /// Ignored without blocking (Discord): messages and notifications are suppressed without telling them.
    Ignored,
}

/// Never stored. Known only for people the shell is watching and for lists the user owns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// Not offline: drawn as no dot at all.
    #[default]
    Unknown,
    Online,
    Idle,
    Dnd,
    /// In a meeting, a call or a huddle (Teams' Busy): a solid red disc, distinct from
    /// do-not-disturb's bar.
    Busy,
    Offline,
}

impl Presence {
    /// Online, idle, do-not-disturb or busy: around.
    pub fn is_around(self) -> bool {
        matches!(self, Presence::Online | Presence::Idle | Presence::Dnd | Presence::Busy)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub key: Key,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bot: bool,
    #[serde(default)]
    pub relation: Relation,
    #[serde(default)]
    pub presence: Presence,
    /// Custom status text, as last seen.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// Opens this person in the network's own app, where the network has such a link.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
    /// Their status's emoji, apart from its words (`status`). Live, like presence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub emoji: String,
    /// What they are doing now: playing, listening, in voice, in a meeting. Live. Boxed: few
    /// people in a list are doing anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Box<Activity>>,
    /// Their job title (Slack, Teams). Stored.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Seconds east of UTC now, for "4:12 PM · 3 h behind you". Stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
}

impl Person {
    /// Someone known by name only.
    pub fn new(key: Key, name: impl Into<String>) -> Person {
        Person {
            key,
            name: name.into(),
            handle: String::new(),
            avatar: None,
            bot: false,
            relation: Relation::None,
            presence: Presence::Unknown,
            status: String::new(),
            link: String::new(),
            emoji: String::new(),
            activity: None,
            title: String::new(),
            utc_offset: None,
            mark: None,
        }
    }
}

/// What a person's card shows beyond the row: asked for one person at a time, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub person: Person,
    /// Their own words about themselves, cut to 1,000 characters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bio: String,
    /// The titles of the Spaces we share, at most [`MUTUAL_SPACES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutual_spaces: Vec<String>,
    /// Their time zone as the network names it (`Europe/Lisbon`), and its offset from UTC in
    /// seconds now, so the shell can show their local time without a zone database.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub time_zone: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset: Option<i32>,
    /// What else the network says of them that has no typed place below, as label and value in
    /// its own words. At most [`PROFILE_FIELDS`]. Leftovers only: the shell draws them last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    /// Unix seconds they joined the network ("Member since").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    /// Friends we have in common, where the network counts them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mutual_friends: u32,
    /// The colour of their profile's banner, `0xRRGGBB`, and its picture where they set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<MediaFile>,

    // Typed, in the card's fixed order. Each is drawn only when it has a value.
    /// Their nickname in the Space the card was opened from (`Profile.space`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nickname: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pronouns: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub department: String,
    /// Their roles in that Space, names only (colours are left out), at most [`PROFILE_ROLES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Email and phone, each copied on click; phone numbers masked until clicked. At most
    /// [`PROFILE_CONTACTS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// How many Spaces we share in all, when that is more than `mutual_spaces` lists.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mutual_total: u32,
    /// Unix seconds: joined that Space, became friends, last seen online.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friends_since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<i64>,
    /// When their status clears, unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_until: Option<i64>,
    /// Out of office, in the network's words, and until when.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub away: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away_until: Option<i64>,
    /// The user's own private note on them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<Warning>,
}

impl Profile {
    /// A card that says nothing beyond the person's row.
    pub fn of(person: Person) -> Profile {
        Profile {
            person,
            bio: String::new(),
            mutual_spaces: Vec::new(),
            time_zone: String::new(),
            utc_offset: None,
            fields: Vec::new(),
            since: None,
            mutual_friends: 0,
            accent: None,
            banner: None,
            nickname: String::new(),
            pronouns: String::new(),
            title: String::new(),
            department: String::new(),
            roles: Vec::new(),
            contacts: Vec::new(),
            mutual_total: 0,
            joined: None,
            friends_since: None,
            last_seen: None,
            status_until: None,
            away: String::new(),
            away_until: None,
            note: String::new(),
            warning: None,
        }
    }
}

/// Which of the things said in a conversation `Shared` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedKind {
    /// Pictures, videos and GIFs.
    Media,
    /// Every other attachment: documents, archives, audio.
    Files,
    Links,
}

/// One thing shared in a conversation, and the message it was shared in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedItem {
    pub message_id: String,
    /// Unix seconds.
    pub sent: i64,
    pub sender: String,
    /// For `Media` and `Files`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<Attachment>,
    /// For `Links`: where it goes, and the page's title when the network made a preview.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

/// What the user does about a person. Never automated: only on a shell request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelateAction {
    /// Send a friend request.
    Add,
    /// Accept their friend request.
    Accept,
    /// Dismiss their friend request without telling them.
    Ignore,
    /// Withdraw the request I sent.
    Cancel,
    /// End the friendship.
    Remove,
    Block,
    /// Lift a block.
    Unblock,
    /// Ignore someone without blocking them (Discord).
    IgnoreUser,
    /// Stop ignoring someone (Discord).
    UnignoreUser,
}

/// What is being reported to a network (`Request::Report`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportTarget {
    Message { conversation: Key, message_id: String },
    Person(Key),
    Conversation(Key),
}

impl ReportTarget {
    pub fn account(&self) -> AccountKey {
        match self {
            ReportTarget::Message { conversation, .. } => conversation.account(),
            ReportTarget::Person(key) | ReportTarget::Conversation(key) => key.account(),
        }
    }
}

/// Shared report reason categories (`Request::Report`, `Request::ReportReasons`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    #[default]
    Spam,
    Harassment,
    Violence,
    Illegal,
    Impersonation,
    Other,
}

impl ReportReason {
    pub const ALL: [ReportReason; 6] = [
        ReportReason::Spam,
        ReportReason::Harassment,
        ReportReason::Violence,
        ReportReason::Illegal,
        ReportReason::Impersonation,
        ReportReason::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReportReason::Spam => "Spam",
            ReportReason::Harassment => "Harassment",
            ReportReason::Violence => "Violence or threats",
            ReportReason::Illegal => "Illegal content",
            ReportReason::Impersonation => "Impersonation",
            ReportReason::Other => "Other",
        }
    }
}

/// What someone is doing in a composer. Networks that only have typing leave this as [`Typing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compose {
    #[default]
    Typing,
    RecordingVoice,
    RecordingVideo,
    UploadingPhoto,
    UploadingVideo,
    UploadingAudio,
    UploadingFile,
    ChoosingSticker,
    PlayingGame,
    /// Speaking in a group call or video chat (`speakingInGroupCallAction`).
    Speaking,
    /// Stopped typing, recording, or uploading (`sendMessageCancelAction`).
    Cancel,
}

impl Compose {
    pub(crate) fn is_typing(&self) -> bool {
        matches!(self, Compose::Typing)
    }
}

/// Someone typing in a conversation right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typist {
    pub id: String,
    pub name: String,
    /// Unix seconds at which the line goes away if nothing more is heard. The shell times this
    /// out itself; chatd sends no change for it.
    pub until: i64,
    /// What they are doing. Absent on an older frame, which means typing.
    #[serde(default, skip_serializing_if = "Compose::is_typing")]
    pub activity: Compose,
}

/// One section of a member list, with how many people it holds on the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSection {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The account's socket is down, so nothing was sent or stored.
    Offline,
    NotFound,
    CallActive,
    NoCall,
    Media,
    /// Sign-in was cancelled (the window was closed) or failed.
    SignIn,
    /// The network's sign-in page could not be loaded.
    SignInUnreachable,
    /// The user's organization does not allow this kind of sign-in.
    SignInBlocked,
    /// This network has no adapter, or its adapter does not do this yet.
    Unsupported,
    /// The network said no. `detail` is its reason, in words the user can read.
    Refused,
    /// The answer would not fit in one frame. Ask for a smaller page.
    TooLarge,
    Internal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxFilter {
    #[default]
    All,
    /// Anything unread or mentioning me that is not muted: what I have not seen.
    Unread,
    /// Only what mentions me, muted or not: who is waiting on me.
    Mentions,
    /// Reactions to my messages, replies in followed threads, and mentions.
    Activity,
    /// Direct and group conversations only.
    Directs,
}

/// How many rows each Inbox filter holds, for the same `network` as the request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxCounts {
    pub all: u32,
    pub unread: u32,
    pub mentions: u32,
    /// Bot and app rows with something unread: the Apps row's dot, never a number.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub apps: u32,
    /// Unmuted rows with reactions to me, unread followed threads, or mentions.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub activity: u32,
}

/// Which messages of a conversation. Always oldest first in the answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Page {
    #[default]
    Newest,
    /// Older than this message. When the cache runs out chatd asks the network and reports the
    /// arrival as a `Messages` change.
    Before { id: String },
    After { id: String },
    /// Centred on this message, which is included.
    Around { id: String },
}

/// What the shell has on screen. Replaces the previous `Watch` as a whole. chatd asks the network
/// for presence, typing and members of exactly this and drops the rest. It lapses when the shell
/// stops asking `Wait`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watching {
    /// The open conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Key>,
    /// People whose presence is drawn, at most [`WATCH_LIMIT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<Key>,
    /// The rows of the open conversation's member list that are visible: first row and count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<(u32, u32)>,
    /// The Space whose tree is on screen, whether or not a conversation of it is open. An
    /// adapter keeps a Space's live subscription only while it is named here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<Key>,
    /// The conversation rows on screen in any list, at most [`ROWS_WATCHED`]. chatd fills in
    /// the preview of those that have none, paced, on every network but Discord.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Key>,
    /// The window is the one the user is typing in (key on macOS, foreground on Windows).
    /// With `conversation`, what arrives there is being read: chatd posts no notification for
    /// it, and plays no sound for any other. Lapses with the rest.
    #[serde(default, skip_serializing_if = "is_false")]
    pub focused: bool,
}

/// One hit of a message search: where the message is, and enough of it to choose by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHit {
    pub conversation: Key,
    /// The message's id, to land on it with `Page::Around`.
    pub message_id: String,
    /// Unix seconds.
    pub sent: i64,
    /// Who sent it, as the message's own row says.
    pub sender: String,
    /// The text around the match, on one line, at most 200 characters.
    pub snippet: String,
    /// The conversation's title, so a result row says where it is without a second request.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The title of the Space the conversation sits in, when it sits in one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space_title: String,
    /// Mine, so the row can say "You".
    #[serde(default, skip_serializing_if = "is_false")]
    pub outgoing: bool,
}

/// A reminder the user set on one message ("Remind me…"). Local to this device: no network
/// hears of it, and it lives until it fires or the user cancels it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReminder {
    /// The message the reminder is on.
    pub message_id: String,
    /// Unix seconds it is due at.
    pub due: i64,
}

/// What cached data `Request::Export` writes to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ExportScope {
    /// One conversation.
    Conversation { conversation: Key },
    /// Every cached conversation of one account.
    Account { account: AccountKey },
    /// Every cached conversation on this device, organized per account.
    All,
}

/// Most targets a single `Request::Forward` may name (official apps cap around 5).
pub const FORWARD_LIMIT: usize = 5;

/// Per-target outcome of a `Request::Forward`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardOutcome {
    pub conversation: Key,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Who a `Request::Delete` removes the message for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteScope {
    /// Remove the message for everyone in the conversation (the default).
    #[default]
    Everyone,
    /// Remove the message on this user's side only (Telegram, Signal, Google Messages).
    Me,
}

impl DeleteScope {
    pub fn is_everyone(&self) -> bool {
        *self == DeleteScope::Everyone
    }
}

fn default_starred_limit() -> u32 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    /// Answers with `Changes` once anything has changed since `since`, or after a while with none.
    /// This is how the shell hears about everything without polling. Start from `Status.seq`.
    Wait {
        since: u64,
    },

    /// The conversations I am in that I would answer, starred first, then newest first; for
    /// `Unread` and `Mentions`, mentions first, then newest. In it: direct and group
    /// conversations, starred rows, rows the adapter lists, and channels that mention me (for
    /// 24 h after the mention, read or not). Never: hidden rows, message requests, rows with no
    /// message at all, archived rows that do not mention me, and anything quiet for 30 days
    /// that is not starred, unread, drafted or holding a failed send. Bot rows are in it with
    /// `bot` set: the shell folds them into one Apps row. Day groups are the shell's, from
    /// `updated`.
    Inbox {
        network: Option<Network>,
        #[serde(default)]
        filter: InboxFilter,
        offset: u32,
        limit: u32,
    },
    /// Every Space of a network, or of all of them, in the network's order. Small by nature.
    Spaces {
        network: Option<Network>,
    },
    /// The groups of one Space and a page of its conversations in tree order. Asking is what makes
    /// chatd bring a Space up to date; a Space nobody opened costs nothing.
    Tree {
        space: Key,
        offset: u32,
        limit: u32,
        /// Read what is stored and bring nothing up to date. The quick switcher reads many Spaces
        /// at once, and that must not subscribe to every one of them on the network.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        peek: bool,
    },
    /// One row, for after a `Conversation` change.
    Conversation {
        conversation: Key,
    },
    Collapse {
        space: Key,
        group: String,
        collapsed: bool,
    },
    Star {
        conversation: Key,
        starred: bool,
    },
    /// Files a conversation on the network: pins or unpins it, archives or unarchives it,
    /// or adds or removes it from a folder. Unset fields are left alone. The new filing is
    /// reported back through the row's `Conversation` change and `Folders` change.
    File {
        conversation: Key,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pinned: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archived: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_add: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_remove: Option<String>,
    },
    /// Star or unstar one message on the network (Google Messages).
    StarMessage {
        conversation: Key,
        message_id: String,
        starred: bool,
    },
    /// Starred messages across an account (or one conversation when set), newest first.
    /// Answers `Response::Pinned { messages }`.
    StarredMessages {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<Key>,
        #[serde(default = "default_starred_limit")]
        limit: u32,
    },
    /// Puts a reminder on one message, to be told about it when the time comes, or takes it
    /// back with `remind_at: None`. Local: nothing goes to any network. A time already past is
    /// refused, and a message that is not in the cache has nothing to remind about.
    Remind {
        conversation: Key,
        message_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remind_at: Option<i64>,
    },
    /// The reminders still waiting in one conversation, due first.
    Reminders {
        conversation: Key,
    },
    /// Lists pending scheduled messages for one conversation (Slack `chat.scheduledMessages.list`).
    ScheduledMessages {
        conversation: Key,
    },
    /// Cancels a pending scheduled message (Slack `chat.deleteScheduledMessage`).
    CancelScheduled {
        conversation: Key,
        scheduled_id: String,
    },
    /// Edits and/or reschedules a pending scheduled message (Slack delete + `chat.scheduleMessage`).
    RescheduleMessage {
        conversation: Key,
        scheduled_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        post_at: i64,
    },
    /// Conversations whose title contains `query`, across every Space, most recent first. From
    /// the cache alone: a channel of a Space never opened is not in it.
    Find {
        network: Option<Network>,
        query: String,
        limit: u32,
    },
    /// Cached messages whose text contains `query`, newest first. `conversation` narrows to one
    /// conversation, `network` to one network, `space` to one Space's conversations. Hidden
    /// conversations and message requests stay out of it, as they stay out of everything that
    /// counts. From the cache alone: what a network itself finds lands as it answers, and is a
    /// later step.
    SearchMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<Key>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<Key>,
        query: String,
        limit: u32,
    },
    /// Ask the network itself for messages, as the pane's Enter does: a server's own search
    /// when a Space is the scope, the channel's when one conversation is, one account/network, or
    /// everywhere across every connected account that supports global search.
    SearchNetwork {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<Key>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<Key>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<AccountKey>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        query: String,
        #[serde(default, skip_serializing_if = "SearchFilters::is_empty")]
        filters: SearchFilters,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// Keeps what the user typed and did not send. Empty text forgets it. Local: no network
    /// hears of it and no change is announced.
    SetDraft {
        conversation: Key,
        text: String,
    },
    /// A network's own list of the conversations that belong to no Space (Discord's DMs, every
    /// Telegram chat, Teams' chats), as its home shows them: the network's pins in their order,
    /// then newest first. Hidden rows never; the archive and message requests only on their
    /// own `shelf`. With `folder`, the chats in that folder whatever their archive, the
    /// folder's own pins first. `counts` in the answer are this list's. Asking with `offset` 0
    /// is what makes chatd bring it up to date.
    Home {
        network: Network,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<AccountKey>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
        #[serde(default, skip_serializing_if = "Shelf::is_main")]
        shelf: Shelf,
        offset: u32,
        limit: u32,
    },
    /// The folders of a network's accounts in their order: Telegram's chat folders with their
    /// unread counts, Discord's server folders with their Spaces.
    Folders {
        network: Network,
    },
    /// The threads the user follows in a Space (a Slack workspace's Threads view, a Discord
    /// server's joined threads), newest first. Asking with `offset` 0 is what makes chatd fetch
    /// the network's own list, once; it is kept fresh by the socket after that.
    Threads {
        space: Key,
        offset: u32,
        limit: u32,
    },
    /// Follow (`on: true`) or unfollow (`on: false`) a thread for reply notifications
    /// (Slack `subscriptions.thread.add` / `subscriptions.thread.remove`).
    FollowThread {
        conversation: Key,
        on: bool,
    },
    /// The topics of a forum (a Telegram forum group, a Discord forum), the network's pins
    /// first, then newest first. From the cache; opening a topic is opening a conversation.
    /// With `landing`, not a page but the one topic opening the forum lands on: the newest
    /// unread, else General. Asking for the landing is also what makes chatd read the
    /// topics from the network first, so the answer names the landing on Telegram's word now.
    Topics {
        forum: Key,
        offset: u32,
        limit: u32,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        landing: bool,
    },
    /// Voice channels and calls with a friend in them now, across Spaces: the Discord home's
    /// "In voice". From what chatd holds live; at most 50.
    InVoice {
        network: Option<Network>,
    },
    /// Who could I talk to now: pending requests counted, then people in a call, online, and
    /// talked to in the last 14 days, each A to Z, at most [`AROUND_LIMIT`]. Bots never.
    Around {
        network: Option<Network>,
    },

    /// The lists the user owns (friends, contacts, direct partners), A to Z. With `query`, cached
    /// matches at once and the network's own search behind them as a `People` change. chatd
    /// decides when a network hears of it: a query of [`SEARCH_MIN_CHARS`] or more, the newest
    /// one only, once the typing has paused, to the connected accounts of `network`.
    People {
        network: Option<Network>,
        relation: Option<Relation>,
        query: Option<String>,
        offset: u32,
        limit: u32,
    },
    /// The visible rows of a conversation's member list. Never the whole list.
    Members {
        conversation: Key,
        offset: u32,
        limit: u32,
    },
    /// Adds or invites people (by person id) to a group or channel.
    MembersAdd {
        conversation: Key,
        people: Vec<String>,
    },
    /// Removes one person (by person id) from a group or channel.
    MembersRemove {
        conversation: Key,
        person: String,
    },
    /// Renames a conversation or updates its topic or description. Unset fields are left alone.
    ConversationUpdate {
        conversation: Key,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Sets a conversation's picture from a local file path (empty clears it).
    ConversationPicture {
        conversation: Key,
        path: String,
    },
    /// The direct conversation with a person, created if there is none. Nothing reaches the other
    /// person until a message is sent.
    StartDirect {
        person: Key,
    },
    /// Opens a Tiny direct for a call. Text enrollment is not required to ring; creating the
    /// conversation keeps the server's text encryption policy and sending text stays guarded.
    StartDirectCall {
        person: Key,
    },
    /// Start a group conversation with several people on one account. Answers with the new
    /// conversation.
    StartGroup {
        account: AccountKey,
        people: Vec<Key>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Create a channel inside a Space where the user has permission to do so. Answers with the
    /// new conversation.
    CreateChannel {
        space: Key,
        name: String,
        kind: ConversationKind,
        #[serde(default, skip_serializing_if = "is_false")]
        private: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
    },
    /// Starts a new group conversation with two or more members (by person id or phone number)
    /// and an optional group name. Answers `Response::Conversation`.
    CreateGroup {
        account: AccountKey,
        members: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// The thread a channel message starts. Discord only: its thread is a new channel, so this
    /// creates it and answers with that conversation. Slack and Teams name a thread from the
    /// message, and opening or sending there is enough.
    StartThread {
        conversation: Key,
        message: String,
    },
    /// One person's card, asked of the network when the card opens and kept five minutes.
    /// `space` is where it was opened from, for the nickname, roles and join date there.
    Profile {
        person: Key,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<Key>,
    },
    /// What the info pane says of a conversation beyond its row. Asked of the network when the
    /// pane opens and kept five minutes.
    ConversationInfo {
        conversation: Key,
    },
    /// A Space's own info, opened from its name.
    SpaceInfo {
        space: Key,
    },
    /// The network's notification setting for a conversation. The row changes when the network
    /// says so. `until` is when a mute ends, as Unix seconds. Absent means muted until it is
    /// turned off, or not muted when `notify` asks for everything.
    SetMute {
        conversation: Key,
        notify: Notify,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<i64>,
    },
    /// Sets or clears a conversation's disappearing-messages timer (`seconds == 0` turns it off).
    /// Separate from local cache retention (`keep_days`).
    SetDisappearing {
        conversation: Key,
        seconds: u32,
    },
    /// Leaves a group conversation on the network. The shell asks once to confirm first.
    Leave {
        conversation: Key,
    },
    /// Closes a direct conversation, as the network's own ×: it leaves the list until a new
    /// message brings it back. Nothing is deleted.
    CloseDirect {
        conversation: Key,
    },
    /// Clears a conversation's history on the network, or deletes the chat when `delete_chat` is
    /// true. `revoke` also removes the other side's copy where the network allows it.
    ClearHistory {
        conversation: Key,
        #[serde(default, skip_serializing_if = "is_false")]
        delete_chat: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        revoke: bool,
    },
    /// Accept (`accept == true`) or decline/ignore (`accept == false`) a message request (Discord).
    AnswerMessageRequest {
        conversation: Key,
        accept: bool,
    },
    /// The messages pinned in a conversation, asked of the network, newest first, at most
    /// [`PINNED_LIMIT`].
    Pinned {
        conversation: Key,
    },
    /// Pins or unpins a message on the network, waiting for the network's answer.
    /// `notify` and `me_only` are used on networks that support them (Telegram).
    Pin {
        conversation: Key,
        message_id: String,
        pinned: bool,
        #[serde(default)]
        notify: bool,
        #[serde(default)]
        me_only: bool,
    },
    /// The pictures, files or links said in a conversation, newest first, from what is cached:
    /// what the info pane lists under a person or a channel. `before` continues below a
    /// message already listed. With `search`, the network is asked instead, because the user
    /// clicked "Search all"; never otherwise.
    Shared {
        conversation: Key,
        kind: SharedKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        search: bool,
    },
    /// Recent calls across all chats on an account, newest first. `missed` filters to missed
    /// incoming calls only; `before` continues below a message id already listed.
    CallHistory {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "is_false")]
        missed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        limit: u32,
    },
    /// Delete one or more entries from an account's recent calls list. `revoke` removes for both
    /// sides when the network supports it.
    DeleteCallEntries {
        account: AccountKey,
        message_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        revoke: bool,
    },
    /// Read an account's network privacy policy for `key` (e.g. Telegram Calls, P2P, VoiceMessages).
    GetPrivacy {
        account: AccountKey,
        key: PrivacyKey,
    },
    /// Write an account's network privacy policy for `key`. Callers must read the current policy
    /// first and send the full updated policy so existing exceptions are never wiped.
    SetPrivacy {
        account: AccountKey,
        key: PrivacyKey,
        policy: PrivacyPolicy,
    },
    /// Answer, withdraw or end a friendship, or block. The list changes when the network says so
    /// (a `People` change).
    Relate {
        person: Key,
        action: RelateAction,
    },
    /// Send a friend request by typing a username (`username` or legacy `username#1234`).
    /// The outgoing request arrives via `Change::People` once the network accepts it.
    AddFriend {
        account: AccountKey,
        username: String,
    },
    /// Report a message, person, or conversation to the network. Never sent without a confirm sheet.
    Report {
        target: ReportTarget,
        reason: ReportReason,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        note: String,
        #[serde(default, skip_serializing_if = "is_false")]
        block: bool,
    },
    /// Asks what report reasons the network offers for `target`; defaults to [`ReportReason::ALL`].
    ReportReasons {
        target: ReportTarget,
    },
    /// Our own presence on one account, and the status text beside it: `None` leaves the text
    /// as it is, empty clears it. `status_emoji`, when given, sets the status emoji (empty
    /// clears it; `None` preserves the current emoji when updating text). `clear_at`, when given,
    /// is a Unix timestamp (seconds) when the status expires.
    SetPresence {
        account: AccountKey,
        presence: Presence,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_emoji: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clear_at: Option<i64>,
    },
    /// Our own profile on one account. `None` leaves that part. An empty about or emoji clears
    /// it. A username, or `photo`, is refused: those are still changed on the phone. Signal is
    /// the account that accepts a name, about, and about emoji.
    SetProfile {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        about: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pronouns: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emoji: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        photo: bool,
    },
    /// Sets our own profile picture on a network from a local file path (empty clears it).
    /// Maps Tiny's `TinyAvatar` onto the shared request.
    SetAvatar {
        account: AccountKey,
        path: String,
    },
    /// Sets (or clears, when `None` or empty) our per-space nickname (e.g. Discord guild nickname).
    SetNickname {
        space: Key,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nickname: Option<String>,
    },
    /// Sets (or clears, when `None` or empty) a private nickname for a friend (Discord).
    SetFriendNickname {
        person: Key,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nickname: Option<String>,
    },
    /// Leaves a Space (e.g. a Discord server). Never sent without a confirm dialog.
    LeaveSpace {
        space: Key,
    },
    /// Preview a Space invite link or code before joining (e.g. Discord `GET /invites/{code}`).
    InvitePreview {
        account: AccountKey,
        code: String,
    },
    /// Join a Space from an invite link or code (e.g. Discord `POST /invites/{code}`).
    InviteJoin {
        account: AccountKey,
        code: String,
    },
    Watch(Watching),

    /// A page of one conversation, oldest first. Reading is not marking read.
    Messages {
        conversation: Key,
        #[serde(default)]
        page: Page,
        limit: u32,
    },
    /// Earlier wordings of one message, oldest first. Empty when this network does not keep
    /// them (`Capabilities::revisions`) or when nothing earlier was stored. A live edit does
    /// not fill this by itself.
    Revisions {
        conversation: Key,
        message_id: String,
    },
    /// Raw source text of a message as the network stores it (for lossless editing of markdown,
    /// code blocks, quotes, lists, masked links, and custom emoji).
    Source {
        conversation: Key,
        message_id: String,
    },
    /// Resolves a network message or channel URL against signed-in accounts' cached conversations.
    /// Never triggers a join.
    Resolve {
        url: String,
    },
    /// The user saw the conversation up to this message: the window is active, the conversation is
    /// open and the message is on screen. Read here and on the network. An empty `message_id`
    /// means the newest message chatd knows, for "mark as read" before any message is on screen.
    MarkRead {
        conversation: Key,
        message_id: String,
    },
    /// The user marked `message_id` unread: it is the first message that counts, and the marker
    /// moves to the one before it. An empty id means the newest message chatd knows. This is the
    /// user's own marker, so it is told to the network even when read receipts are off.
    MarkUnread {
        conversation: Key,
        message_id: String,
    },
    /// Every conversation with a badge is read, here and on the network. `network` limits it to
    /// one; `space` to one Space's conversations; both `None` is the whole Inbox. Message
    /// requests are read here only.
    MarkAllRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<Network>,
        /// The Space to read out, the account it belongs to and the network's own id. An older
        /// peer that does not know the field ignores it and hears the wider request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<Key>,
    },
    /// Drops this conversation's cached messages and its badge. A direct or group conversation
    /// leaves the Inbox until a newer message arrives. The network is not asked to delete
    /// anything.
    Forget {
        conversation: Key,
    },
    /// Answers at once with the message as `Pending`. `client_id` is made by the shell and comes
    /// back on the message; when the network accepts it the same row turns `Sent` under its real
    /// id, or `Failed` with the reason. A text send while the account is `Offline` is stored
    /// `Pending` with `waiting` and goes when the link returns. `SignedOut`, `Expired`, and a
    /// send that carries files are still refused.
    ///
    /// `with` carries files (streamed by chatd from their paths, with `Change::Upload` as they
    /// go) and options; the pending message lists the files at once. Refused when it asks
    /// for more than the account's `Capabilities` allow.
    Send {
        conversation: Key,
        client_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        #[serde(default, skip_serializing_if = "Outgoing::is_plain")]
        with: Outgoing,
    },
    /// Stops a send whose files are still uploading. It ends `Failed` ("Cancelled"), so Retry
    /// and Discard work on it as on any other.
    CancelSend {
        conversation: Key,
        client_id: String,
    },
    /// Sends a `Failed` message again.
    Retry {
        conversation: Key,
        client_id: String,
    },
    /// Removes a `Failed` or `Pending` message that never left.
    Discard {
        conversation: Key,
        client_id: String,
    },
    /// The user's draft is changing. chatd decides how often the network hears of it.
    /// `activity` is what they are doing. Absent means typing.
    Typing {
        conversation: Key,
        #[serde(default, skip_serializing_if = "Compose::is_typing")]
        activity: Compose,
    },
    /// The user followed a link out of this message (the shell is opening it). Telegram's terms
    /// want a sponsored message's follow told to it; other networks have nothing to do.
    FollowLink {
        conversation: Key,
        message_id: String,
    },
    /// Adds (`on`) or takes back our reaction. `emoji` is the emoji itself, or a custom one's
    /// `:name:` as `Reaction::emoji` spells it. Shown at once; what the network then says is
    /// what stays.
    React {
        conversation: Key,
        message_id: String,
        emoji: String,
        on: bool,
        /// Discord Nitro super / burst reaction.
        #[serde(default, skip_serializing_if = "is_false")]
        burst: bool,
    },
    /// What reactions the conversation takes from this account: asked when it opens, kept
    /// five minutes. chatd refuses a `React` these rules refuse, before the network hears of it.
    Reactions {
        conversation: Key,
    },
    /// Who reacted to a message with `emoji`, paged. Fetched only on demand (clicking or
    /// opening the reactors list), never prefetched.
    Reactors {
        conversation: Key,
        message_id: String,
        emoji: String,
        #[serde(default, skip_serializing_if = "is_false")]
        burst: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        #[serde(default)]
        limit: u32,
    },
    /// Replaces the text of one of our own messages. Nothing changes on screen until the
    /// network accepts it; a refusal is the answer. `spans` are the styled ranges of `text`;
    /// empty keeps the edit as plain text. An older client omits them.
    Edit {
        conversation: Key,
        message_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        spans: Vec<Span>,
    },
    /// Deletes a message. `scope` defaults to `Everyone` (so older shells keep working);
    /// `Me` removes it on this user's side only (Telegram, Signal, Google Messages).
    Delete {
        conversation: Key,
        message_id: String,
        #[serde(default, skip_serializing_if = "DeleteScope::is_everyone")]
        scope: DeleteScope,
    },
    /// Forwards one or more messages to up to [`FORWARD_LIMIT`] conversations on the same account.
    /// Cross-account targets are refused; callers use a separate re-send for "Copy to…".
    Forward {
        from: Key,
        message_ids: Vec<String>,
        to: Vec<Key>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        note: String,
    },
    /// Vote on a poll, take a vote back, or end one. `votes` of `None` ends it (only the person
    /// who started it), `Some` replaces our vote, and an empty list takes it back. Signal only.
    /// The shell's composer menu that starts a poll is separate: that is a `Send` with `poll`.
    Poll {
        conversation: Key,
        message_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        votes: Option<Vec<u32>>,
    },

    /// Custom emoji, stickers or GIFs for the picker, asked when its tab opens and never before,
    /// kept five minutes. `space` narrows custom emoji to one Space's; `query` searches;
    /// `cursor` continues a page. At most [`CATALOG_LIMIT`] items.
    Catalog {
        account: AccountKey,
        kind: CatalogKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<Key>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },

    /// Saves a GIF to the account's saved GIFs, or removes it when `unsave` is true (Telegram).
    SaveGif {
        account: AccountKey,
        id: String,
        #[serde(default, skip_serializing_if = "is_false")]
        unsave: bool,
    },

    /// Asks for these files to be on disk. Answers with what is known now; each file that lands
    /// later is a `Media` change. Ask only for what is on screen.
    Fetch {
        account: AccountKey,
        ids: Vec<String>,
        /// The longest side of the whole picture at the scale it is drawn, in device pixels
        /// (points times the screen's scale; for a cropped tile, the uncropped picture). Where
        /// the adapter registered size variants, chatd fetches the smallest one at least this
        /// large, and fetches again when the file on disk is smaller. Such a file answers
        /// with no `path` until the larger one lands.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u32>,
        /// The user asked again for files that failed. Without this a failed file is answered
        /// with its `failed` reason and nothing is fetched.
        #[serde(default, skip_serializing_if = "is_false")]
        retry: bool,
    },

    /// Opens the network's sign-in and keeps the session. Answers when the user is done.
    SignIn {
        network: Network,
        /// The main window's top left corner in screen points, so the sign-in window opens
        /// over it and not on another display.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<(i32, i32)>,
    },
    /// Reads again the sessions the keychain refused (`Keychain::denied`): the user's "Try
    /// again". Answers `Status` at once; the read goes on behind it.
    ReadSessions,
    /// Forgets the account's session and everything stored for it. Waits up to 5 s for the
    /// network to end the session first. A Tiny server's encrypted history stays on this device
    /// (sealed, its key in the keychain) for a later sign-in there, unless `erase` is set.
    SignOut {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
    /// Begins, or reads again, a sign-in the shell draws itself (`signin.rs`). Answers with the
    /// step to draw. For a network that signs in through a web page this is `Unsupported`.
    SignInBegin {
        network: Network,
    },
    /// What the user did about the step on show. Answers with the step to draw next.
    SignInAnswer {
        network: Network,
        input: SignInInput,
    },
    /// The user closed the sheet. Nothing is kept.
    SignInCancel {
        network: Network,
    },

    /// Joins or rings. Starts mediad. One call at a time. Refused with `Unsupported`, and no
    /// mediad, unless the conversation's `call` is `Native`.
    CallStart {
        conversation: Key,
    },
    /// Answers the incoming ring. Starts mediad.
    CallAccept,
    /// Stops the incoming ring for both sides. mediad never starts.
    CallDecline,
    /// Leaves. Stops mediad.
    CallEnd,
    CallSet {
        muted: bool,
        deafened: bool,
    },
    /// Rings people in the current direct/group call (`None` rings everyone in the conversation).
    /// Single explicit user action; never retried.
    CallRing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        people: Option<Vec<String>>,
    },
    /// Stops ringing specific people in the current direct/group call (`None` stops ringing all).
    CallStopRinging {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        people: Option<Vec<String>>,
    },
    /// Adds friends to the current direct or group call. In a 1:1 DM call, creates a group DM
    /// with the peer and added friends and moves the call there; in a group DM call, adds them
    /// and rings them.
    CallAddPeople {
        people: Vec<String>,
    },
    /// Where the official app takes this conversation's call.
    Handoff {
        conversation: Key,
    },
    Media(MediaRequest),
    /// A voice message or audio file: play, pause, seek, speed, stop. Answers `Ok`; the rest is
    /// `Change::Playback`.
    Play(PlayRequest),
    /// One recorded voice message: start, stop, forget. Answers `Ok`; the rest is
    /// `Change::Recording`.
    Record(RecordRequest),

    /// Every stored setting (`settings`). Missing ones are at their defaults.
    Settings,
    /// Stores one setting, or with `None` returns it to its default. Answered `Ok`, then
    /// `Change::Settings`. Refused unless `settings::valid` (or, for `None`, `settings::known`).
    Set {
        scope: String,
        key: String,
        #[serde(default)]
        value: Option<String>,
    },
    /// Reset all settings: every stored row goes, so every setting is at its default.
    ResetSettings,
    /// Stops chatd: ⌘Q, and closing the window while Keep receiving is off. Answered `Ok`
    /// first; a call is left on the network before the process exits.
    Shutdown,
    /// What tinyclient keeps on this Mac, in bytes (Settings › Storage).
    Storage,
    /// Deletes every cached message and file. Sessions, drafts and settings stay.
    Clear,
    /// Exports cached messages (and optionally on-disk media) to an unencrypted folder at
    /// `destination`. Never fetches history from any network and never exports member lists.
    Export {
        scope: ExportScope,
        destination: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<i64>,
        #[serde(default)]
        include_files: bool,
    },
    /// How much a retention of `days` would delete now.
    Prunable {
        days: u32,
    },
    /// The microphone test: on, the level arrives as `Change::Meter` ten times a second until
    /// off, 30 s, or a call starts. In a call it is the call's own meter.
    TestMicrophone {
        on: bool,
        /// Also play the microphone back on the speaker, processed as a call would send it.
        /// Outside a call only.
        #[serde(default, skip_serializing_if = "is_false")]
        loopback: bool,
    },
    /// Plays the test sound on the call speaker (V2): in the call's mediad, or one started for it.
    TestSound,
    /// N13: posts one real notification, "This is how messages will look and sound", with the
    /// preview and sound chosen. Answers `NotifyTested` with what the system said.
    NotifyTest,
    /// Saved Tiny server origins, read locally without contacting servers.
    TinyLocalServers,
    /// S11: plays one notification sound once, by its settings word (`pebble`, `soft_ring`), at
    /// the alert volume, as a notification would. Plays even when sounds are off.
    PlaySound {
        sound: String,
    },
    /// Who is on this Tiny server. An owner or an admin also gets the invites and the audit.
    TinyCommunity {
        account: AccountKey,
    },
    /// Creates an invite for `role` (`admin` or `member`). The token is in the answer, once.
    TinyInvite {
        account: AccountKey,
        role: String,
    },
    /// Changes one member's role (`owner`, `admin`, or `member`).
    TinyRole {
        account: AccountKey,
        user_id: String,
        role: String,
    },
    /// Removes one member. The server ends their sessions.
    TinyRemove {
        account: AccountKey,
        user_id: String,
    },
    /// Revokes one invite nobody has accepted.
    TinyRevoke {
        account: AccountKey,
        invite_id: String,
    },
    /// Changes this account's own display name on the server. The account stays named by its host.
    TinyRename {
        account: AccountKey,
        display_name: String,
    },
    /// Ends every session of this account on the server, this one too, then forgets it here as
    /// `SignOut` does. Answers `Status`.
    TinySignOutEverywhere {
        account: AccountKey,
    },
    /// Permanently deletes this account on the Tiny server (`DELETE /v1/me` with confirmation),
    /// then wipes this install's local session, keys, and cache for that server. Answers `Status`.
    TinyDeleteAccount {
        account: AccountKey,
    },
    /// Files an abuse report on a Tiny server (`POST /v1/reports`). Answers `TinyReported`.
    TinyReport {
        account: AccountKey,
        kind: String,
        target_id: String,
        category: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
    },
    /// Lists a Tiny server's custom emoji and stickers (`GET /v1/emoji`). Answers `TinyEmojiList`.
    TinyEmojiList {
        account: AccountKey,
    },
    /// Uploads a new custom emoji or sticker (`POST /v1/emoji`). Answers `Ok`.
    TinyEmojiCreate {
        account: AccountKey,
        name: String,
        kind: String,
        path: String,
    },
    /// Renames a custom emoji or sticker (`PATCH /v1/emoji/{id}`). Answers `Ok`.
    TinyEmojiRename {
        account: AccountKey,
        emoji_id: String,
        name: String,
    },
    /// Deletes a custom emoji or sticker (`DELETE /v1/emoji/{id}`). Answers `Ok`.
    TinyEmojiDelete {
        account: AccountKey,
        emoji_id: String,
    },
    /// Makes a room, for an owner or an admin. Its call is empty until someone joins.
    TinyRoomCreate {
        account: AccountKey,
        name: String,
    },
    /// Renames one room, for an owner or an admin.
    TinyRoomRename {
        account: AccountKey,
        room_id: String,
        name: String,
    },
    /// Deletes one room, for an owner or an admin. Anyone in its call leaves it.
    TinyRoomDelete {
        account: AccountKey,
        room_id: String,
    },
    /// Takes a seat in a room's call, leaving any other first. On a server whose audio is up
    /// it starts mediad and joins with audio, as `CallStart` does; on one without audio the
    /// seat is all it is. Answers `Ok` once the server took it.
    TinyCallJoin {
        account: AccountKey,
        room_id: String,
    },
    /// Leaves the call this account is in. Not being in one is not an error.
    TinyCallLeave {
        account: AccountKey,
    },
    /// Removes someone from a room's call, for an owner or an admin.
    TinyCallKick {
        account: AccountKey,
        room_id: String,
        user_id: String,
    },
    /// Marks someone's identity key verified on this account (the user compared the safety
    /// number), or takes that back. Remembered per server and person; a later key that is not
    /// `key` shows as `Trust::Changed`. Answers `Ok`.
    TinyVerify {
        account: AccountKey,
        user_id: String,
        key: String,
        verified: bool,
    },

    // Server management. Each needs the server to list the feature in its capabilities
    // (`TinyCommunity::capabilities`); an older server answers that it can't. Each answers `Ok`
    // unless it says otherwise; the server then tells every page over its live socket, which is
    // a `Change::Tiny`.
    /// Changes the server's name, description, welcome text, default room, AFK room and
    /// timeout, or upload caps (`server`, manage_server; `upload_limits` for the caps).
    TinyServerUpdate {
        account: AccountKey,
        edit: TinyServerEdit,
    },
    /// Sets the server's icon from a picture on this computer (PNG, JPEG or WebP, at most
    /// [`TINY_ICON_MAX`] bytes), or with no `path` removes it (`server`, manage_server).
    TinyServerIcon {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Sets this account's picture from a file on this computer (PNG, JPEG or WebP, at most
    /// [`TINY_ICON_MAX`] bytes), or with no `path` removes it. Any member may set their own.
    TinyAvatar {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Makes a role just below this account's own rank (`roles`, manage_roles).
    TinyRoleCreate {
        account: AccountKey,
        role: TinyRoleEdit,
    },
    TinyRoleUpdate {
        account: AccountKey,
        role_id: String,
        role: TinyRoleEdit,
    },
    /// @everyone can't be deleted.
    TinyRoleDelete {
        account: AccountKey,
        role_id: String,
    },
    /// Every role but @everyone, most senior first.
    TinyRoleOrder {
        account: AccountKey,
        role_ids: Vec<String>,
    },
    /// The roles one member has, all of them (`roles`, manage_roles).
    TinyMemberRoles {
        account: AccountKey,
        user_id: String,
        role_ids: Vec<String>,
    },
    /// Makes someone an owner or takes it away. Owners only; the last owner stays.
    TinyMemberOwner {
        account: AccountKey,
        user_id: String,
        owner: bool,
    },
    /// Times someone out until `until` (RFC 3339, at most 28 days away), or with `None` ends
    /// it. A timed-out member can't be in calls (`timeouts`, timeout_members).
    TinyTimeout {
        account: AccountKey,
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
    /// Removes one member, as `TinyRemove`, with a reason for the audit.
    TinyKick {
        account: AccountKey,
        user_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
    /// Bans someone: they leave, their sessions end, and they can't come back by invite, by
    /// sign-in groups or by signing in again. `None` is for good (`bans`, ban_members).
    TinyBan {
        account: AccountKey,
        user_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
    },
    TinyUnban {
        account: AccountKey,
        user_id: String,
    },
    /// Categories (`categories`, manage_channels). Deleting one leaves its rooms uncategorized.
    TinyCategoryCreate {
        account: AccountKey,
        name: String,
    },
    TinyCategoryRename {
        account: AccountKey,
        category_id: String,
        name: String,
    },
    TinyCategoryDelete {
        account: AccountKey,
        category_id: String,
    },
    /// The whole channel list's order: categories in the order given (the uncategorized group,
    /// `category_id: None`, is always first), and each one's rooms top first.
    TinyLayout {
        account: AccountKey,
        groups: Vec<TinyLayoutGroup>,
    },
    /// Makes a room with its settings, as `TinyRoomCreate` with more.
    TinyRoomCreateWith {
        account: AccountKey,
        room: TinyRoomEdit,
    },
    /// Makes a text channel on a Tiny server. Answers `TinyChannelCreated`.
    TinyChannelCreate {
        account: AccountKey,
        channel: TinyChannelEdit,
    },
    /// Changes a text channel's name, topic, category or position. Never sends `e2ee`.
    TinyChannelUpdate {
        account: AccountKey,
        channel_id: String,
        channel: TinyChannelEdit,
    },
    /// Deletes a text channel and its messages for everyone.
    TinyChannelDelete {
        account: AccountKey,
        channel_id: String,
    },
    /// Changes a room's name, category, place, topic, limit, bitrate, push-to-talk or video.
    TinyRoomUpdate {
        account: AccountKey,
        room_id: String,
        room: TinyRoomEdit,
    },
    /// One role's allow and deny bits in one room (manage_roles and manage_channels). Only
    /// [`TINY_CHANNEL_SCOPED`] bits mean anything here.
    TinyOverrideSet {
        account: AccountKey,
        room_id: String,
        role_id: String,
        allow: u64,
        deny: u64,
    },
    TinyOverrideClear {
        account: AccountKey,
        room_id: String,
        role_id: String,
    },
    /// One role's allow and deny bits in one text channel (manage_roles and manage_channels).
    TinyChannelOverrideSet {
        account: AccountKey,
        channel_id: String,
        role_id: String,
        allow: u64,
        deny: u64,
    },
    TinyChannelOverrideClear {
        account: AccountKey,
        channel_id: String,
        role_id: String,
    },
    /// An invite with roles, a label, a use limit and an expiry (`invite_options`). Answers
    /// `TinyCode`, as `TinyInvite` does.
    TinyInviteWith {
        account: AccountKey,
        options: TinyInviteOptions,
    },
    /// End-to-end encrypted text: this device, this person's devices, other people's safety
    /// numbers, approving a device, the recovery key (`E2eeAsk` says what each answers).
    TinyE2ee {
        account: AccountKey,
        ask: E2eeAsk,
    },
    /// One page of the audit. Answers `TinyAudit`.
    TinyAudit {
        account: AccountKey,
        #[serde(default)]
        filter: TinyAuditFilter,
    },
    /// One page of the server's abuse reports (`GET /v1/reports`). Answers `TinyReports`.
    TinyReportsList {
        account: AccountKey,
        #[serde(default)]
        filter: TinyReportFilter,
    },
    /// One abuse report with reporter note, unverified evidence, and moderator notes (`GET /v1/reports/{id}`).
    /// Kept in memory only; never cached or logged. Answers `TinyReport`.
    TinyReportRead {
        account: AccountKey,
        report_id: String,
    },
    /// Update an abuse report (`PATCH /v1/reports/{id}`): status, moderator note, linked audit id,
    /// or legal hold (`preserve`, owners only). Answers `TinyReport`.
    TinyReportWork {
        account: AccountKey,
        report_id: String,
        update: TinyReportUpdate,
    },
    /// What the owner is asked about the community's privacy (`GET /v1/setup`). Answers
    /// `TinySetup`; refused in words unless the server has `first_run_setup` and this account is
    /// an owner.
    TinySetupRead {
        account: AccountKey,
    },
    /// The owner's answer (`PUT /v1/setup`; `apply` empty keeps what the server has and finishes
    /// setup). Answers `TinySetupApplied`, or `TinySetupRefused` when the server says what to do
    /// first. The server then tells every page over its live socket.
    TinySetupApply {
        account: AccountKey,
        apply: TinySetupApply,
    },
    /// Asks the server for a link that adds a passkey for this account and opens it in the system
    /// browser (`POST /v1/me/passkeys/link`). Answers `Ok`. The server wants a sign-in within the
    /// last ten minutes, and chatd says so in words when it is not.
    TinyPasskeyLink {
        account: AccountKey,
    },
    /// The owner asks for a link per chosen member to add a passkey
    /// (`POST /v1/setup/claim-links`). Answers `TinyClaimLinks`. A member is refused.
    TinySetupClaimLinks {
        account: AccountKey,
        user_ids: Vec<String>,
    },
    /// This account's own passkeys (`GET /v1/me/passkeys`). Answers `TinyPasskeys`.
    TinyPasskeysRead {
        account: AccountKey,
    },
    /// Renames one of this account's passkeys. Answers `Ok`.
    TinyPasskeyRename {
        account: AccountKey,
        id: String,
        label: String,
    },
    /// Removes one of this account's passkeys. Answers `Ok`; the server refuses to remove the
    /// last way to sign in.
    TinyPasskeyRemove {
        account: AccountKey,
        id: String,
    },
    /// Makes a fresh set of recovery codes for this account, which replaces the old ones. Answers
    /// `TinyRecoveryCodes`. The codes are shown once.
    TinyRecoveryCodes {
        account: AccountKey,
    },
    /// The server's application bots. Answers `TinyBots`. A member is refused.
    TinyBots {
        account: AccountKey,
    },
    /// Creates a bot and answers `TinyBotCreated`. `scopes` are `rooms.read`, `rooms.write`,
    /// and `members.read`. The token is in that answer once.
    TinyBotCreate {
        account: AccountKey,
        name: String,
        scopes: Vec<String>,
    },
    /// Revokes one bot. Answers `Ok`.
    TinyBotRevoke {
        account: AccountKey,
        bot_id: String,
    },
    /// Server-mutes or server-deafens someone in a room's call, or undoes it; `None` leaves
    /// that one as it is (`voice_moderation`, mute_members / deafen_members).
    TinyCallModerate {
        account: AccountKey,
        room_id: String,
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_muted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_deafened: Option<bool>,
    },
    /// Moves someone in a call to another room (`voice_moderation`, move_members in both).
    TinyCallMove {
        account: AccountKey,
        user_id: String,
        room_id: String,
    },

    /// The push-to-talk key went down or up, from the shell's global hotkey. `priority` is the
    /// priority key: on a Tiny server it also asks to be heard over everyone else, which the
    /// server honours only with priority_speaker. Refused with no call.
    CallTalk {
        held: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        priority: bool,
    },
    /// How the call's connection is doing now. Answers `CallStats`.
    CallStats,
    /// A call may be asked for soon (a place that can call is open, or a call just ended):
    /// chatd starts its call engine now so that joining finds it ready. Answers `Ok` at once;
    /// it starts nothing that opens a microphone, camera or connection, and does nothing when
    /// the engine is already ready or is part of the app itself.
    CallWarm,
    /// The person chose to carry on the Tiny call without end-to-end encryption after the server
    /// turned it off (`SecurityMode::Blocked`). Answers `Ok`, or `Refused` in words when the call
    /// is not blocked or must stay end to end (`CallSecurity::required`).
    CallCarryOnUnencrypted,
    /// One person's volume for me alone, 0 to [`GAIN_MAX`] percent, and whether I muted them
    /// (V9). Kept per account (`settings::gain_key`), so it holds in every call on that
    /// account, and applied by the call's mediad at once. Answers `Ok`, with or without a
    /// call.
    CallGain {
        account: AccountKey,
        user_id: String,
        percent: u16,
        #[serde(default, skip_serializing_if = "is_false")]
        muted: bool,
    },
    /// Write this process's debug ring to disk. The answer names files, never their text.
    SaveLogs,
    /// Paged activity feed (mentions, thread replies, reactions, app notifications) for an account.
    ActivityFeed {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ActivityItemKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default)]
        limit: u32,
    },
    /// Mark one activity feed item (`Some(id)`) or all items (`None`) read on the network.
    ActivityMarkRead {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
    /// Add a link bookmark to a channel (Slack `bookmarks.add`).
    BookmarkAdd {
        conversation: Key,
        title: String,
        link: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        emoji: String,
    },
    /// Edit a channel bookmark (Slack `bookmarks.edit`).
    BookmarkEdit {
        conversation: Key,
        bookmark_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emoji: Option<String>,
    },
    /// Remove a channel bookmark (Slack `bookmarks.remove`).
    BookmarkRemove {
        conversation: Key,
        bookmark_id: String,
    },
    /// Browse public channels in a Space (paged, optionally filtered by `query`).
    BrowseChannels {
        space: Key,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default)]
        limit: u32,
    },
    /// Open an unjoined public channel for read-only preview outside the sidebar.
    PreviewChannel {
        conversation: Key,
    },
    /// Join a public channel and add it to the sidebar tree.
    Join {
        conversation: Key,
    },
    /// Paged workspace/account-wide file list (Slack `files.list` without a channel).
    /// Answers `Response::AccountFiles`.
    AccountFiles {
        account: AccountKey,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default)]
        limit: u32,
    },
    /// Delete a file on the network (Slack `files.delete`, own files).
    DeleteFile {
        account: AccountKey,
        file_id: String,
    },
    /// Execute an interactive control on a message (Slack Block Kit `blocks.actions`).
    /// Answers `Response::Ok` when accepted; message updates arrive asynchronously.
    MessageAction {
        conversation: Key,
        message_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        block_id: String,
        action_id: String,
        element_type: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        value: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        values: Vec<String>,
    },
}

/// One thing that changed. The shell asks again for exactly that; the ephemeral kinds carry their
/// whole new value and never cause a list to be read again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum Change {
    /// Links, accounts or their totals: ask `Status`.
    Status,
    /// A row's title, preview, order or unread state: ask `Conversation`, or the list it is in.
    Conversation { conversation: Key },
    /// Messages arrived, were edited, deleted, reacted to or changed send state: ask `Messages`.
    Messages { conversation: Key },
    /// Spaces were added, removed or reordered: ask `Spaces`.
    Spaces { account: AccountKey },
    /// A Space's groups or conversations: ask `Tree`.
    Tree { space: Key },
    /// The owned lists or a search result: ask `People`.
    People { account: AccountKey },
    /// Rows of a conversation's member list were added, removed or moved: ask `Members` again
    /// for the rows on screen.
    Members { conversation: Key },
    /// A requested file is on disk (`path`), or could not be fetched (`failed` says why).
    Media { account: AccountKey, file: MediaFile },
    /// Ephemeral. Who is in a voice channel now; no people and a total of 0 when it emptied.
    Occupants { conversation: Key, occupants: Occupants },
    /// Ephemeral. A network finished the search for `query`, whether or not it found anyone.
    Searched { account: AccountKey, query: String },
    /// Ephemeral. Everyone typing in the conversation now; empty when they stopped.
    Typing { conversation: Key, typists: Vec<Typist> },
    /// A network's folders, or their counts: ask `Folders`.
    Folders { account: AccountKey },
    /// Ephemeral. Presence, status and what they are doing, all of it.
    Presence {
        person: Key,
        presence: Presence,
        status: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        emoji: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity: Option<Activity>,
    },
    /// Ephemeral. How far the files of a pending message have gone, in bytes, at most four
    /// times a second. Its outcome is the message's own change.
    Upload { conversation: Key, client_id: String, sent: u64, total: u64 },
    /// Ephemeral. The call as it is now: who is speaking, mute, state. `None` when it is over.
    /// `ring` is a second incoming call waiting while this one is up. It has no mediad.
    Call {
        call: Option<Call>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ring: Option<Call>,
    },
    /// Ephemeral. A sign-in the shell is drawing moved on by itself: the code was scanned, or
    /// ran out and was replaced.
    SignIn { network: Network, step: SignInStep },
    /// Ephemeral. The voice message or audio file playing, paused or just ended; `None` once
    /// stopped.
    Playback { playback: Option<Playback> },
    /// Ephemeral. The one recording: started, recorded (with its facts) or failed; `None` once
    /// forgotten.
    Recording { recording: Option<Recording> },
    /// A setting was stored or reset: ask `Settings`.
    Settings,
    /// Ephemeral. A notification was clicked: bring the window forward on this conversation.
    Open { conversation: Key },
    /// Ephemeral. The microphone test's level, 0 to 100; `None` once the test is over.
    Meter { level: Option<u8> },
    /// A Tiny server's people, roles, invites or rooms changed: ask `TinyCommunity`. Who is in
    /// a room's call is `Occupants`, and who is online `Presence`.
    Tiny { account: AccountKey },
}

impl Change {
    /// Two changes to the same thing: the later one replaces the earlier.
    pub fn same_scope(&self, other: &Change) -> bool {
        match (self, other) {
            (Change::Status, Change::Status)
            | (Change::Call { .. }, Change::Call { .. })
            | (Change::Playback { .. }, Change::Playback { .. })
            | (Change::Recording { .. }, Change::Recording { .. })
            | (Change::Settings, Change::Settings)
            | (Change::Open { .. }, Change::Open { .. })
            | (Change::Meter { .. }, Change::Meter { .. }) => true,
            (Change::SignIn { network: a, .. }, Change::SignIn { network: b, .. }) => a == b,
            (Change::Conversation { conversation: a }, Change::Conversation { conversation: b })
            | (Change::Messages { conversation: a }, Change::Messages { conversation: b })
            | (Change::Typing { conversation: a, .. }, Change::Typing { conversation: b, .. })
            | (Change::Members { conversation: a }, Change::Members { conversation: b })
            | (Change::Occupants { conversation: a, .. }, Change::Occupants { conversation: b, .. })
            | (Change::Tree { space: a }, Change::Tree { space: b })
            | (Change::Presence { person: a, .. }, Change::Presence { person: b, .. }) => a == b,
            (Change::Upload { conversation: a, client_id: x, .. }, Change::Upload { conversation: b, client_id: y, .. }) => {
                a == b && x == y
            }
            (Change::Spaces { account: a }, Change::Spaces { account: b })
            | (Change::Folders { account: a }, Change::Folders { account: b })
            | (Change::Searched { account: a, .. }, Change::Searched { account: b, .. })
            | (Change::People { account: a }, Change::People { account: b })
            | (Change::Tiny { account: a }, Change::Tiny { account: b }) => a == b,
            (Change::Media { account: a, file: x }, Change::Media { account: b, file: y }) => {
                a == b && x.id == y.id
            }
            _ => false,
        }
    }

    /// Typing, presence, occupants, a finished search and the call: the change is the whole
    /// new value, so nothing needs reading again.
    pub fn is_ephemeral(&self) -> bool {
        matches!(
            self,
            Change::Typing { .. }
                | Change::Presence { .. }
                | Change::Call { .. }
                | Change::Occupants { .. }
                | Change::Searched { .. }
                | Change::SignIn { .. }
                | Change::Upload { .. }
                | Change::Playback { .. }
                | Change::Recording { .. }
                | Change::Meter { .. }
                | Change::Open { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Response {
    Pong,
    /// Done, with nothing to say.
    Ok,
    /// The report reasons a network offers for a target (`Request::ReportReasons`).
    ReportReasons {
        reasons: Vec<ReportReason>,
    },
    /// `NotifyTest`: whether the system took the notification, and its words when it did not
    /// (notifications turned off for tinyclient, no notification service).
    NotifyTested {
        posted: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        detail: String,
    },
    Status {
        networks: Vec<NetworkStatus>,
        accounts: Vec<AccountStatus>,
        media: Media,
        call: Option<Call>,
        /// A ring waiting beside `call`. Empty when nobody else is calling.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ring: Option<Call>,
        /// Where `Wait` starts from.
        seq: u64,
        /// What chatd is doing with the keychain: reading, waiting on macOS, refused.
        #[serde(default, skip_serializing_if = "Keychain::is_settled")]
        keychain: Keychain,
        /// When this chatd started, unix milliseconds. A new value means it restarted: read
        /// everything on screen again.
        #[serde(default)]
        started: u64,
        /// Shared by the window, chatd, and mediad for this run. Empty on an older chatd.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        session: String,
    },
    Changes {
        /// Pass this to the next `Wait`.
        seq: u64,
        /// chatd no longer knows what changed since `since` (it restarted, or the shell fell too
        /// far behind): read everything on screen again.
        reset: bool,
        changes: Vec<Change>,
    },
    Conversations {
        conversations: Vec<Conversation>,
        /// How many rows the request matches in all.
        total: u32,
        /// For `Inbox`: what each filter would hold. Zero for `Find`.
        #[serde(default)]
        counts: InboxCounts,
    },
    Conversation {
        conversation: Conversation,
    },
    Spaces {
        spaces: Vec<Space>,
    },
    Tree {
        space: Key,
        groups: Vec<Group>,
        conversations: Vec<Conversation>,
        total: u32,
    },
    People {
        people: Vec<Person>,
        total: u32,
        /// Connected accounts that already searched this query, so no `Searched` follows for
        /// them. The shell takes their "Asking…" down. Empty when the query is new, too short,
        /// or a later page.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        settled: Vec<AccountKey>,
    },
    Members {
        /// Discord cache miss: rows will arrive through the active viewport subscription.
        #[serde(default, skip_serializing_if = "is_false")]
        pending: bool,
        conversation: Key,
        sections: Vec<MemberSection>,
        offset: u32,
        members: Vec<Person>,
    },
    /// What is known now about what the shell watches.
    Watching {
        people: Vec<Person>,
        typists: Vec<Typist>,
    },
    Messages {
        conversation: Key,
        messages: Vec<Message>,
        /// Where the user had read to when asked.
        last_read: Option<String>,
        /// The cache holds messages older than the first one here.
        older: bool,
        /// The cache holds messages newer than the last one here.
        newer: bool,
    },
    /// Answers `Revisions`: earlier wordings, oldest first.
    Revisions {
        conversation: Key,
        message_id: String,
        revisions: Vec<Revision>,
    },
    /// Answers `Source`: the raw source text of the message as the network holds it.
    Source {
        text: String,
    },
    /// Answers `Resolve`: the matching cached conversation and optional message id on a signed-in account.
    Resolved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<Key>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        title: String,
    },
    /// Answers `Export`: where the unencrypted export was written and what cached range it covered.
    Exported {
        path: String,
        conversations: u32,
        messages: u64,
        files: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        earliest: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latest: Option<i64>,
    },
    /// Answers `Forward`: one outcome per target conversation.
    Forwarded {
        results: Vec<ForwardOutcome>,
    },
    /// Answers `SearchMessages`: the newest hits first, at most the limit asked for.
    MessageHits {
        hits: Vec<MessageHit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_searched: Vec<Network>,
    },
    /// Answers `Reminders`: what is still waiting in the conversation, due first.
    Reminders {
        conversation: Key,
        reminders: Vec<MessageReminder>,
    },
    Sent {
        message: Message,
    },
    Media {
        files: Vec<MediaFile>,
    },
    Link {
        url: String,
    },
    Profile {
        profile: Box<Profile>,
    },
    Shared {
        conversation: Key,
        items: Vec<SharedItem>,
        /// The cache holds older messages that were not looked at yet, or the network has more.
        more: bool,
        /// The list is the network's own and whole (a search was asked, or the network keeps the
        /// list). False: it is only what is in the messages loaded here, and must say so.
        #[serde(default, skip_serializing_if = "is_false")]
        complete: bool,
        /// The exact total, only where the network counted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u32>,
    },
    CallHistory {
        entries: Vec<CallLogEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        more: bool,
    },
    Privacy {
        key: PrivacyKey,
        policy: PrivacyPolicy,
    },
    Folders {
        folders: Vec<Folder>,
    },
    Around {
        /// Incoming requests waiting on me (Discord's Pending), counted only.
        requests: u32,
        in_call: Vec<Person>,
        online: Vec<Person>,
        recent: Vec<Person>,
    },
    /// Boxed, as `Profile` is: few answers are one, and every answer would carry its size.
    ConversationInfo {
        info: Box<ConversationInfo>,
    },
    SpaceInfo {
        info: Box<SpaceInfo>,
    },
    /// Answers `InvitePreview`: metadata and member counts for an invite code.
    InvitePreview {
        preview: InvitePreview,
    },
    /// Answers `InviteJoin`: the joined Space and landing conversation, when reported.
    InviteJoined {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<Key>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<Key>,
    },
    Pinned {
        conversation: Key,
        messages: Vec<Message>,
    },
    Reactions {
        conversation: Key,
        rules: ReactionRules,
    },
    /// Answers `Reactors`: one page of people who reacted with `emoji`.
    Reactors {
        conversation: Key,
        message_id: String,
        emoji: String,
        reactors: Vec<Reactor>,
        #[serde(default, skip_serializing_if = "is_false")]
        more: bool,
    },
    Catalog {
        items: Vec<CatalogItem>,
        /// Pass back for the next page; `None` at the end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// Where a sign-in the shell draws stands.
    SignIn {
        network: Network,
        step: SignInStep,
    },
    /// Answers `MessageAction` when the network returned structured details, an alert, or a link.
    ActionResult {
        result: ActionResult,
    },
    Error {
        code: ErrorCode,
        detail: String,
    },
    Settings {
        values: Vec<settings::Setting>,
    },
    /// Bytes on this Mac: the message database and the cached images and files.
    Storage {
        messages: u64,
        files: u64,
    },
    /// What a shorter retention would delete: messages, and about how many bytes of them.
    Prunable {
        messages: u64,
        bytes: u64,
    },
    /// Saved Tiny account keys and their server origins; no credentials.
    TinyLocalServers {
        servers: Vec<(AccountKey, String)>,
    },
    /// Who is on this Tiny server, plus the invites and the audit when this account may see them.
    TinyCommunity {
        community: TinyCommunity,
    },
    /// An invite that was just created. `code.token` is not kept.
    TinyCode {
        code: TinyCode,
    },
    /// A text channel that was just created, so the UI can open it.
    TinyChannelCreated {
        id: String,
        #[serde(default)]
        name: String,
    },
    /// One page of a Tiny server's audit, newest first. Fewer rows than the limit: the end.
    TinyAudit {
        audit: Vec<TinyAudit>,
    },
    /// One page of a Tiny server's abuse reports, newest first.
    TinyReports {
        reports: Vec<TinyReportSummary>,
    },
    /// One full abuse report (note, evidence, moderator notes). Kept in memory only.
    TinyReport {
        report: Box<TinyReportDetail>,
    },
    /// What `TinySetupRead` asked.
    TinySetup {
        setup: TinySetup,
    },
    /// The choices went in. `applied.settings` is what the server keeps now.
    TinySetupApplied {
        applied: TinySetupApplied,
    },
    /// The server would not apply the choices, and said what the owner can do. Nothing changed.
    TinySetupRefused {
        refusal: TinySetupRefusal,
    },
    /// One link per member asked for, each to add a passkey.
    TinyClaimLinks {
        links: Vec<TinyClaimLink>,
    },
    /// This account's own passkeys.
    TinyPasskeys {
        passkeys: TinyPasskeys,
    },
    /// A fresh set of recovery codes, shown once.
    TinyRecoveryCodes {
        codes: Vec<String>,
    },
    /// The server's application bots. None of them carries a token.
    TinyBots {
        bots: Vec<TinyBot>,
    },
    /// A bot just created. `token` is shown once and is not kept.
    TinyBotCreated {
        bot: TinyBot,
        token: String,
    },
    /// An abuse report was accepted (`POST /v1/reports`). `duplicate` is true on `200` when the
    /// same reporter already has an open or reviewing report on this target.
    TinyReported {
        duplicate: bool,
    },
    /// A Tiny server's community custom emoji and stickers (`GET /v1/emoji`).
    TinyEmojiList {
        items: Vec<TinyEmoji>,
    },
    /// What `E2eeAsk::Status` asked.
    TinyE2ee {
        status: E2eeStatus,
    },
    /// What `E2eeAsk::Person` asked.
    TinyE2eePerson {
        person: E2eePerson,
    },
    /// A recovery key just made, shown once: 24 characters in six groups.
    TinyRecoveryKey {
        key: String,
    },
    /// What an encrypted backup file holds (`ExportBackup`) or added here (`ImportBackup`).
    TinyBackup {
        messages: u64,
    },
    /// What `E2eeAsk::Link` asked: the device link in progress on this device, if any.
    TinyE2eeLink {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<E2eeLink>,
    },
    CallStats {
        stats: CallStats,
    },
    /// Paths of debug bundles just written. Empty when export is off.
    LogsSaved {
        paths: Vec<String>,
    },
    /// One page of an account's activity feed (`Request::ActivityFeed`).
    ActivityFeed {
        items: Vec<ActivityItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// A created or edited channel bookmark (`BookmarkAdd` / `BookmarkEdit`).
    Bookmark {
        bookmark: Bookmark,
    },
    /// One page of public channels (`Request::BrowseChannels`).
    BrowsedChannels {
        channels: Vec<BrowsedChannel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// One page of workspace/account-wide files (`Request::AccountFiles`).
    AccountFiles {
        items: Vec<SharedItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        more: bool,
    },
    /// Pending scheduled messages for a conversation (`Request::ScheduledMessages`).
    ScheduledMessages {
        messages: Vec<ScheduledMessage>,
    },
    /// One updated/rescheduled message (`Request::RescheduleMessage`).
    ScheduledMessage {
        message: ScheduledMessage,
    },
}

/// The phone's app-private data directory, set once by the mobile entry (`set_support_dir`).
static APP_SUPPORT_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Names the app's private data directory on a phone. The Android shell calls it before the window
/// starts, because Android has no `HOME` and the fallback below would land in `/tmp`. Not an
/// environment variable: `TINYCLIENT_DIR` marks a scratch run, which keeps no sign-ins.
pub fn set_support_dir(dir: PathBuf) {
    let _ = APP_SUPPORT_DIR.set(dir);
}

/// `TINYCLIENT_DIR` moves the socket and database, for tests and the demo seed.
pub fn support_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TINYCLIENT_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = APP_SUPPORT_DIR.get() {
        return dir.clone();
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        return home.join("Library/Application Support/tinyclient");
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("tinyclient");
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile).join("AppData/Roaming/tinyclient");
        }
        return PathBuf::from(r"C:\ProgramData\tinyclient");
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data_home).join("tinyclient");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".local/share/tinyclient")
}

pub fn chat_sock_path() -> PathBuf {
    chat_sock_path_in(&support_dir())
}

/// The bytes a Unix socket path may hold, NUL included (`sun_path`): 104 on Apple systems, 108
/// elsewhere. A longer path fails at bind or connect, so [`ipc`] checks it first.
pub const SUN_PATH_MAX: usize = if cfg!(target_vendor = "apple") { 104 } else { 108 };

/// chatd's socket for a support directory. On iOS the directory is not used: an app's container
/// path alone is about 76 bytes, and the socket under the support directory would run past
/// [`SUN_PATH_MAX`]. The socket goes to `tmp/cs` under the container instead.
pub fn chat_sock_path_in(dir: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "ios") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let short = short_sock_path(&home);
        if short.as_os_str().len() < SUN_PATH_MAX {
            return short;
        }
        // The simulator's container lives under the Mac's ~/Library/Developer/CoreSimulator, about
        // 150 bytes deep, and a simulated app may use the Mac's /tmp: one directory per container.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        home.as_os_str().hash(&mut hasher);
        return PathBuf::from(format!("/tmp/tinyclient-{:016x}", hasher.finish())).join(CHAT_SOCK_NAME);
    }
    dir.join(CHAT_SOCK_NAME)
}

/// The socket under an app container: `<container>/tmp/cs/chat.sock`.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn short_sock_path(container: &std::path::Path) -> PathBuf {
    container.join("tmp").join("cs").join(CHAT_SOCK_NAME)
}

/// The largest frame either side writes or reads.
pub const FRAME_LIMIT: usize = 1024 * 1024;

/// Writes one frame. A value larger than [`FRAME_LIMIT`] is refused with `InvalidData` and
/// nothing is written, so the other side never reads a frame it would reject.
pub fn write_frame(mut w: impl Write, value: &impl Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    if bytes.len() > FRAME_LIMIT {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame larger than 1 MiB"));
    }
    let len = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// One frame's bytes, unparsed: for a reader that answers a frame it cannot parse.
pub fn read_frame_bytes(mut r: impl Read) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > FRAME_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame larger than 1 MiB",
        ));
    }
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub fn read_frame<T: for<'de> Deserialize<'de>>(r: impl Read) -> std::io::Result<T> {
    let bytes = read_frame_bytes(r)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

/// mediad exists only while a call or share is up.
pub fn media_process_needed(active_calls: usize) -> bool {
    active_calls > 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn members_pending_is_explicit_and_old_complete_responses_still_decode() {
        let mut value = serde_json::json!({"op": "members", "conversation": {
            "network": "discord", "account": "account", "id": "channel"
        }, "offset": 0, "sections": [], "members": []});
        let complete: super::Response = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(complete, super::Response::Members { pending: false, .. }));
        assert!(serde_json::to_value(complete).unwrap().get("pending").is_none());
        value["pending"] = serde_json::json!(true);
        let pending: super::Response = serde_json::from_value(value).unwrap();
        assert!(matches!(pending, super::Response::Members { pending: true, .. }));
        assert_eq!(serde_json::to_value(pending).unwrap()["pending"], true);
    }

    use super::*;

    fn row(kind: ConversationKind, unread: u32, mentions: u32, muted: bool) -> Conversation {
        let key = Key {
            network: Network::Slack,
            account: "T1".into(),
            id: "C1".into(),
        };
        Conversation {
            unread,
            mentions,
            muted,
            ..Conversation::new(key, kind, "#platform")
        }
    }

    #[test]
    fn my_newest_message_says_sending_sent_delivered_or_read_and_an_old_row_still_decodes() {
        let mut c = row(ConversationKind::Direct, 0, 0, false);
        assert_eq!(c.receipt(), None);
        c.preview_mine = true;
        assert_eq!(c.receipt(), Some(Receipt::Sent));
        c.delivered = true;
        assert_eq!(c.receipt(), Some(Receipt::Delivered));
        c.seen = true;
        assert_eq!(c.receipt(), Some(Receipt::Read));
        c.sending = true;
        assert_eq!(c.receipt(), Some(Receipt::Sending));
        // A chatd from before the second tick says neither.
        let mut old = serde_json::to_value(row(ConversationKind::Direct, 0, 0, false)).unwrap();
        old["preview_mine"] = serde_json::json!(true);
        assert!(old.get("delivered").is_none() && old.get("sending").is_none());
        let old: Conversation = serde_json::from_value(old).unwrap();
        assert_eq!(old.receipt(), Some(Receipt::Sent));
    }

    #[test]
    fn idle_does_not_need_mediad() {
        assert!(!media_process_needed(0));
        assert!(media_process_needed(1));
    }

    #[test]
    fn a_number_means_someone_needs_me_and_a_dot_means_only_unread() {
        use ConversationKind::{Channel, Direct};
        assert_eq!(row(Direct, 3, 0, false).badge(), Badge::Count(3));
        assert_eq!(row(Channel, 3, 0, false).badge(), Badge::Dot);
        assert_eq!(row(Channel, 3, 2, false).badge(), Badge::Count(2));
        assert_eq!(row(Channel, 0, 0, false).badge(), Badge::None);
        // Muted: no dot, no number for plain unread, but a mention still counts.
        assert_eq!(row(Channel, 3, 0, true).badge(), Badge::None);
        assert_eq!(row(Direct, 3, 0, true).badge(), Badge::None);
        assert_eq!(row(Channel, 3, 1, true).badge(), Badge::Count(1));
        // A bot, an archived chat and a request raise a number only for a mention.
        let app = Conversation { bot: true, ..row(Direct, 4, 0, false) };
        assert_eq!(app.badge(), Badge::Dot);
        assert_eq!(Conversation { mentions: 1, ..app }.badge(), Badge::Count(1));
        let archived = Conversation { archived: true, ..row(Direct, 4, 0, false) };
        assert_eq!(archived.badge(), Badge::Dot);
        // Teams direct chats show their unread count; channels show a dot unless mentioned.
        let mut teams = row(Direct, 7, 0, false);
        teams.key.network = Network::Teams;
        assert_eq!(teams.badge(), Badge::Count(7));
        let mut teams_chan = row(Channel, 3, 0, false);
        teams_chan.key.network = Network::Teams;
        assert_eq!(teams_chan.badge(), Badge::Dot);
        teams_chan.mentions = 2;
        assert_eq!(teams_chan.badge(), Badge::Count(2));
    }

    #[test]
    fn a_plain_send_and_a_status_from_before_the_polish_round_read_the_same() {
        let send: Request = serde_json::from_str(
            r#"{"op":"send","conversation":{"network":"slack","account":"T1","id":"C1"},"client_id":"1","text":"hi"}"#,
        )
        .unwrap();
        let Request::Send { with, .. } = &send else { panic!("not a send") };
        assert!(with.is_plain() && with.ping, "a reply pings unless asked not to");
        assert!(!serde_json::to_string(&send).unwrap().contains("with"));
        let status: Response = serde_json::from_str(r#"{"op":"status","networks":[],"accounts":[],"media":"stopped","call":null,"seq":3}"#).unwrap();
        assert!(matches!(status, Response::Status { started: 0, ref keychain, .. } if keychain.is_settled()));
        let quiet = Outgoing { ping: false, ..Outgoing::default() };
        let json = serde_json::to_string(&quiet).unwrap();
        assert_eq!(json, r#"{"ping":false}"#);
        assert_eq!(serde_json::from_str::<Outgoing>(&json).unwrap(), quiet);
        let old: Request = serde_json::from_str(
            r#"{"op":"edit","conversation":{"network":"slack","account":"T1","id":"C1"},"message_id":"1","text":"hi"}"#,
        )
        .unwrap();
        let Request::Edit { spans, text, .. } = &old else { panic!("not an edit") };
        assert!(spans.is_empty() && text == "hi");
        assert!(!serde_json::to_string(&old).unwrap().contains("spans"));
        let styled = Outgoing { spans: vec![Span { start: 0, end: 2, style: Style::Bold }], ..Outgoing::default() };
        assert!(!styled.is_plain());
        assert!(serde_json::to_string(&styled).unwrap().contains("bold"));
    }

    #[test]
    fn a_message_search_and_its_hits_read_off_the_wire() {
        let ask: Request = serde_json::from_str(r#"{"op":"search_messages","query":"lichen","limit":20}"#).unwrap();
        let Request::SearchMessages { conversation, network, space, query, limit } = &ask else {
            panic!("not a search")
        };
        assert!(conversation.is_none() && network.is_none() && space.is_none() && query == "lichen" && *limit == 20);
        let with_filters = Request::SearchMessages {
            conversation: Some(Key { network: Network::Slack, account: "T1".into(), id: "C1".into() }),
            network: Some(Network::Slack),
            space: Some(Key { network: Network::Slack, account: "T1".into(), id: "S1".into() }),
            query: "lichen".into(),
            limit: 20,
        };
        assert_eq!(
            serde_json::to_string(&with_filters).unwrap(),
            r#"{"op":"search_messages","conversation":{"network":"slack","account":"T1","id":"C1"},"network":"slack","space":{"network":"slack","account":"T1","id":"S1"},"query":"lichen","limit":20}"#
        );
        let hit = MessageHit {
            conversation: Key { network: Network::Discord, account: "1".into(), id: "C2".into() },
            message_id: "9".into(),
            sent: 20,
            sender: "mara".into(),
            snippet: "hello world".into(),
            title: String::new(),
            space_title: String::new(),
            outgoing: false,
        };
        assert_eq!(
            serde_json::to_string(&Response::MessageHits { hits: vec![hit], cursor: None, not_searched: Vec::new() }).unwrap(),
            r#"{"op":"message_hits","hits":[{"conversation":{"network":"discord","account":"1","id":"C2"},"message_id":"9","sent":20,"sender":"mara","snippet":"hello world"}]}"#
        );
        // The network's own ask: the scope is a conversation, Space, account, network, or everywhere.
        let ask: Request = serde_json::from_str(r#"{"op":"search_network","query":"lichen"}"#).unwrap();
        let Request::SearchNetwork { conversation, space, query, filters, cursor, .. } = &ask else {
            panic!("not a network search")
        };
        assert!(conversation.is_none() && space.is_none() && query == "lichen" && filters.is_empty() && cursor.is_none());
        let asked = Request::SearchNetwork {
            conversation: Some(Key { network: Network::Discord, account: "1".into(), id: "C2".into() }),
            space: None,
            account: None,
            network: None,
            query: "lichen".into(),
            filters: SearchFilters::default(),
            cursor: None,
        };
        assert_eq!(
            serde_json::to_string(&asked).unwrap(),
            r#"{"op":"search_network","conversation":{"network":"discord","account":"1","id":"C2"},"query":"lichen"}"#
        );
    }

    #[test]
    fn a_frame_too_large_to_read_is_never_written() {
        let mut out = Vec::new();
        let huge = "x".repeat(FRAME_LIMIT + 1);
        let err = write_frame(&mut out, &huge).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(out.is_empty());
    }

    #[test]
    fn a_later_change_replaces_an_earlier_one_to_the_same_thing_only() {
        let key = |id: &str| Key {
            network: Network::Discord,
            account: "1".into(),
            id: id.into(),
        };
        let typing = |id: &str, names: &[&str]| Change::Typing {
            conversation: key(id),
            typists: names
                .iter()
                .map(|name| Typist { id: (*name).into(), name: (*name).into(), until: 0, activity: Compose::Typing })
                .collect(),
        };
        assert!(typing("a", &["ana"]).same_scope(&typing("a", &[])));
        assert!(!typing("a", &["ana"]).same_scope(&typing("b", &["ana"])));
        assert!(!typing("a", &[]).same_scope(&Change::Messages { conversation: key("a") }));
        assert!(!Change::Messages { conversation: key("a") }
            .same_scope(&Change::Conversation { conversation: key("a") }));
    }

    #[test]
    fn frames_written_before_these_fields_existed_still_read() {
        let request: Request = serde_json::from_str(r#"{"op":"fetch","account":{"network":"slack","account":"T1"},"ids":["a"]}"#).unwrap();
        assert!(matches!(request, Request::Fetch { size: None, retry: false, .. }));
        let request: Request = serde_json::from_str(r#"{"op":"sign_in","network":"slack"}"#).unwrap();
        assert!(matches!(request, Request::SignIn { at: None, .. }));
        let thread: ThreadSummary = serde_json::from_str(r#"{"id":"t","replies":2,"last":5}"#).unwrap();
        assert_eq!((thread.participants.len(), thread.unread), (0, 0));
        let call = r#"{"conversation":{"network":"discord","account":"1","id":"c"},"state":"joined",
            "participants":[],"muted":false,"deafened":false,"joined":1,"engine_linked":true,"video":[]}"#;
        let call: Call = serde_json::from_str(call).unwrap();
        assert_eq!((call.speaking, call.level, call.inputs.len()), (false, None, 0));
        let message: Message = serde_json::from_str(
            r#"{"id":"1","sender":{"name":"mara"},"sent":1,"text":"hi"}"#,
        )
        .unwrap();
        assert_eq!((message.edited, message.edited_at, message.prior_text.as_deref()), (false, None, None));
    }

    #[test]
    fn a_message_is_cut_to_its_limits_and_the_header_line_names_the_place() {
        let mut message = Message::plain("1", "bot", "", 1, false);
        message.card = Some(Box::new(Card {
            title: "é".repeat(300),
            fields: (0..40).map(|n| (n.to_string(), "v".repeat(2_000))).collect(),
            ..Card::default()
        }));
        message.bound();
        let card = message.card.as_ref().unwrap();
        assert_eq!((card.title.chars().count(), card.fields.len(), card.fields[0].1.len()), (256, CARD_FIELDS, 1_000));

        let mut place = row(ConversationKind::Channel, 0, 0, false);
        assert_eq!(place.subtitle(), "");
        place.space_title = "Moss & Lichen".into();
        place.group_title = "Trail".into();
        assert_eq!(place.subtitle(), "Moss & Lichen › Trail");
        place.topic = "Where we walk".into();
        assert_eq!(place.subtitle(), "Moss & Lichen › Trail · Where we walk");
    }

    #[test]
    fn empty_fields_stay_off_the_wire() {
        let message = Message::plain("1", "mara", "hi", 1_700_000_000, false);
        let json = serde_json::to_string(&message).unwrap();
        for absent in ["spans", "attachments", "reactions", "reply_to", "thread", "client_id", "card", "forwarded", "deleted", "edited", "edited_at", "prior_text"] {
            assert!(!json.contains(absent), "{absent} in {json}");
        }
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), message);
    }

    #[test]
    fn file_request_and_capabilities_round_trip() {
        let key = Key::new(&AccountKey { network: Network::Telegram, account: "a".into() }, "u:1");
        let req = Request::File {
            conversation: key.clone(),
            pinned: Some(true),
            archived: Some(false),
            folder_add: Some("2".into()),
            folder_remove: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("folder_remove"), "{json}");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        // Minimal request with only archived set leaves other fields None.
        let minimal: Request = serde_json::from_str(&format!(
            r#"{{"op":"file","conversation":{},"archived":true}}"#,
            serde_json::to_string(&key).unwrap()
        ))
        .unwrap();
        assert_eq!(
            minimal,
            Request::File { conversation: key, pinned: None, archived: Some(true), folder_add: None, folder_remove: None }
        );

        let can = Capabilities { pin_chats: 5, archive: true, folders_edit: true, ..Capabilities::default() };
        let cjson = serde_json::to_string(&can).unwrap();
        assert_eq!(serde_json::from_str::<Capabilities>(&cjson).unwrap(), can);
        // Older capabilities JSON without these fields defaults to 0 / false.
        let old: Capabilities = serde_json::from_str("{}").unwrap();
        assert_eq!((old.pin_chats, old.archive, old.folders_edit), (0, false, false));
    }

    #[test]
    fn delete_scope_and_conversation_rights_round_trip() {
        let key = Key::new(&AccountKey { network: Network::Telegram, account: "a".into() }, "u:1");
        // Default scope stays off the wire for backward compatibility.
        let everyone = Request::Delete { conversation: key.clone(), message_id: "9".into(), scope: DeleteScope::Everyone };
        let json = serde_json::to_string(&everyone).unwrap();
        assert!(!json.contains("scope"), "{json}");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), everyone);

        // Explicit `Me` scope round-trips.
        let mine = Request::Delete { conversation: key.clone(), message_id: "9".into(), scope: DeleteScope::Me };
        let me_json = serde_json::to_string(&mine).unwrap();
        assert!(me_json.contains(r#""scope":"me""#), "{me_json}");
        assert_eq!(serde_json::from_str::<Request>(&me_json).unwrap(), mine);

        // Older shell request without `scope` decodes as `Everyone`.
        let legacy: Request = serde_json::from_str(&format!(
            r#"{{"op":"delete","conversation":{},"message_id":"9"}}"#,
            serde_json::to_string(&key).unwrap()
        ))
        .unwrap();
        assert_eq!(legacy, everyone);

        // ConversationRights bitmask + serde round-trip.
        let rights = ConversationRights { delete_others: true, ..ConversationRights::default() };
        assert_eq!(ConversationRights::from_bits(rights.to_bits()), rights);
        assert!(ConversationRights::default().is_empty());
        assert!(!rights.is_empty());
    }

    #[test]
    fn disappearing_timer_request_and_presets_round_trip() {
        let key = Key::new(&AccountKey { network: Network::Signal, account: "sig".into() }, "u:1");
        let req = Request::SetDisappearing { conversation: key.clone(), seconds: 30 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        assert_eq!(format_disappearing(0), "Off");
        assert_eq!(format_disappearing(30), "30s");
        assert_eq!(format_disappearing(300), "5m");
        assert_eq!(format_disappearing(3_600), "1h");
        assert_eq!(format_disappearing(86_400), "1d");
        assert_eq!(format_disappearing(604_800), "1w");
        assert!(disappearing_presets(Network::Signal).contains(&30));
        assert!(disappearing_presets(Network::Telegram).contains(&86_400));
    }

    #[test]
    fn conversation_management_requests_and_rights_round_trip() {
        let key = Key::new(&AccountKey { network: Network::Slack, account: "w1".into() }, "C1");
        let add = Request::MembersAdd { conversation: key.clone(), people: vec!["U1".into(), "U2".into()] };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&add).unwrap()).unwrap(), add);

        let remove = Request::MembersRemove { conversation: key.clone(), person: "U2".into() };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&remove).unwrap()).unwrap(), remove);

        let update = Request::ConversationUpdate {
            conversation: key.clone(),
            title: Some("ops-new".into()),
            topic: Some("deployments".into()),
            description: None,
        };
        let ujson = serde_json::to_string(&update).unwrap();
        assert!(!ujson.contains("description"), "{ujson}");
        assert_eq!(serde_json::from_str::<Request>(&ujson).unwrap(), update);

        let pic = Request::ConversationPicture { conversation: key, path: "/tmp/icon.png".into() };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&pic).unwrap()).unwrap(), pic);

        let all = ConversationRights {
            pin: true,
            delete_others: true,
            manage_members: true,
            add_members: true,
            remove_members: true,
            rename: true,
            edit_topic: true,
            edit_picture: true,
        };
        assert_eq!(ConversationRights::from_bits(all.to_bits()), all);
    }
}

#[cfg(test)]
mod sock_path_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn ios_socket_path_fits_sun_path_in_a_device_container() {
        // A device container: iOS gives every app a root of this shape, its UUID 36 bytes long.
        let container = Path::new("/var/mobile/Containers/Data/Application/0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9");
        let path = short_sock_path(container);
        assert!(path.as_os_str().len() < 104, "{} bytes: {}", path.as_os_str().len(), path.display());
    }

    #[test]
    fn android_socket_path_fits_sun_path_in_the_app_data_dir() {
        let dir = Path::new("/data/user/0/dev.tinyclient.app/files");
        let path = chat_sock_path_in(dir);
        assert!(path.as_os_str().len() < 104, "{} bytes: {}", path.as_os_str().len(), path.display());
    }

    #[test]
    fn desktop_socket_stays_in_the_support_dir() {
        if !cfg!(target_os = "ios") {
            assert_eq!(chat_sock_path_in(Path::new("/x/tinyclient")), Path::new("/x/tinyclient/chat.sock"));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn an_over_long_socket_path_is_refused_by_name() {
        let long = Path::new("/tmp").join("x".repeat(200)).join("chat.sock");
        let err = ChatStream::connect(&long).err().expect("a path past sun_path cannot connect");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("bytes"), "{err}");
    }

    #[test]
    fn scheduled_message_requests_and_responses_round_trip() {
        let conv = Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, "C1");
        let reqs = [
            Request::ScheduledMessages { conversation: conv.clone() },
            Request::CancelScheduled { conversation: conv.clone(), scheduled_id: "Q1".into() },
            Request::RescheduleMessage {
                conversation: conv.clone(),
                scheduled_id: "Q1".into(),
                text: Some("Later".into()),
                post_at: 1_900_000_000,
            },
        ];
        for req in reqs {
            let encoded = serde_json::to_string(&req).unwrap();
            let decoded: Request = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, req);
        }
        let msg = ScheduledMessage {
            id: "Q2".into(),
            conversation: conv,
            text: "Later".into(),
            post_at: 1_900_000_000,
            created: 1_899_999_000,
        };
        let resp = Response::ScheduledMessages { messages: vec![msg.clone()] };
        assert_eq!(serde_json::from_str::<Response>(&serde_json::to_string(&resp).unwrap()).unwrap(), resp);
        let one = Response::ScheduledMessage { message: msg };
        assert_eq!(serde_json::from_str::<Response>(&serde_json::to_string(&one).unwrap()).unwrap(), one);
    }

    #[test]
    fn follow_thread_request_and_capability_round_trip() {
        let conv = Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, "C1:1760000200.000200");
        let req = Request::FollowThread { conversation: conv, on: true };
        let encoded = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), req);

        let can = Capabilities { follow_threads: true, dnd_snooze: true, ..Capabilities::default() };
        let cjson = serde_json::to_string(&can).unwrap();
        assert_eq!(serde_json::from_str::<Capabilities>(&cjson).unwrap(), can);
        let old: Capabilities = serde_json::from_str("{}").unwrap();
        assert!(!old.follow_threads);
        assert!(!old.dnd_snooze);
    }

    #[test]
    fn compose_activities_round_trip_and_default_to_typing() {
        let conv = Key::new(&AccountKey { network: Network::Telegram, account: "1".into() }, "u7002");
        for activity in [
            Compose::Typing,
            Compose::RecordingVoice,
            Compose::RecordingVideo,
            Compose::UploadingPhoto,
            Compose::UploadingVideo,
            Compose::UploadingAudio,
            Compose::UploadingFile,
            Compose::ChoosingSticker,
            Compose::PlayingGame,
            Compose::Speaking,
            Compose::Cancel,
        ] {
            let req = Request::Typing { conversation: conv.clone(), activity };
            let encoded = serde_json::to_string(&req).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), req);
        }
        // Older Typist JSON without `activity` defaults to `Compose::Typing`.
        let legacy: Typist = serde_json::from_str(r#"{"id":"u1","name":"Ada","until":100}"#).unwrap();
        assert_eq!(legacy.activity, Compose::Typing);
    }
}
