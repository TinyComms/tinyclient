//! One adapter per account. Each speaks its network's own protocol and nothing else.
//!
//! This file is the whole surface between an adapter and the rest of chatd:
//!
//! - [`Command`] is everything chatd asks of an adapter. An adapter serves the ones it can and
//!   hands the rest to [`Command::unsupported`], so a new command never breaks an adapter.
//! - [`Context`] is everything an adapter tells chatd. Each call writes the store and announces
//!   the right change; an adapter never touches the store's tables or the change list itself.
//! - An adapter's module exports `start`, `accounts` and `without` (see the functions of those
//!   names at the bottom), and reads its addresses through [`net::address`] so a test can point
//!   it at a local fake.
//!
//! Adapters hold bounded state: trees, people and messages go to the store as they arrive.
//!
//! Nothing here acts for the user unasked. A friend request is answered, a presence set, a
//! message edited, deleted or reacted to only by the command the shell's request became.

mod discord;

/// `TINYCLIENT_PROVE=discord-nitro-length`. Nitro's 4,000-character limit, shared by every send path.
pub fn prove_discord_nitro_length() -> Result<(), String> {
    discord::prove_nitro_length()
}

/// `TINYCLIENT_PROVE=discord-voice-bitrate`. A voice channel is encoded at its own bitrate.
pub fn prove_discord_voice_bitrate() -> Result<(), String> {
    discord::prove_voice_bitrate()
}

/// `TINYCLIENT_PROVE=discord-group-members`. A group DM's list is its other people plus us.
pub fn prove_discord_group_dm() -> Result<(), String> {
    discord::prove_group_dm()
}

/// `TINYCLIENT_PROVE=discord-mark-unread`. A deliberate unread ack is manual, and an older
/// ack only moves the marker when it says so.
pub fn prove_discord_mark_unread() -> Result<(), String> {
    discord::prove_mark_unread()
}

/// `TINYCLIENT_PROVE=discord-forward-report`. A forward is a type-1 reference, and a report
/// uses one captured message-menu path.
pub fn prove_discord_forward_report() -> Result<(), String> {
    discord::prove_forward_report()
}

/// `TINYCLIENT_PROVE=discord-voice-move`. A move to another voice channel in the same server is followed.
pub fn prove_discord_voice_move() -> Result<(), String> {
    discord::prove_voice_move()
}
mod recovery;
pub mod net;
mod script;
pub mod messages;
pub mod signal;
pub(crate) mod slack;
pub(crate) mod teams;
pub mod telegram;
pub mod tiny;
pub mod tiny_bots;

use crate::hub::{lock, now, Hub};
use crate::session;
use crate::store::{
    ConversationRow, Filing, FolderRow, PersonDetails, PersonRow, PreviewLine, SpaceRow, Stored,
    KEEP_MESSAGES,
};
use protocol::{
    AccountKey, Activity, CallFailure, CallState, Capabilities, CatalogItem, CatalogKind, Change,
    Contact, Group, Key, Link, MediaFile, MemberSection, Message, MessageHit, Network, Notify,
    Occupant, Occupants, Outgoing, Participant, Person, Presence, Reaction, RelateAction, Relation,
    SendState, SharedItem, SharedKind, Compose, Translation, Typist, VoiceJoin, Warning, Watching,
};
use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Why an adapter did not do what a command asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This adapter does not do this (yet).
    Unsupported,
    /// The network said no, in words the user can read: "Slow mode: 8 s".
    Refused(String),
}

/// Where the answer to a command goes. Send exactly one.
pub type Reply<T> = Sender<Result<T, Refusal>>;

/// How to fetch a file: the address to ask now, and the headers the session adds.
#[derive(Debug, Clone)]
pub struct Download {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The file is a sealed (end-to-end encrypted) blob: what opens it. The cache opens it after
    /// the download and keeps only the opened file.
    pub sealed: Option<SealedFile>,
}

/// How to open a sealed attachment (`e2ee::attachment`), from the message that named it. Not
/// `Debug`: it holds the file's key.
#[derive(Clone)]
pub struct SealedFile {
    pub key: [u8; 32],
    /// SHA-256 of the sealed blob, checked before anything is decrypted.
    pub sha256: [u8; 32],
    /// The plaintext's length, as the message says.
    pub plain_len: u64,
    /// The first chunk's associated data besides its header.
    pub context: Vec<u8>,
    /// What the sender says it is. Checked against the opened bytes, never trusted.
    pub mime: String,
    pub name: String,
}

impl std::fmt::Debug for SealedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedFile").field("plain_len", &self.plain_len).finish_non_exhaustive()
    }
}

/// What the search's ask names: a Space's own search, one conversation's, or the whole account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    Space(String),
    Conversation(String),
    Account,
}

/// One page of recent calls across all chats on an account (`Command::CallHistory`).
#[derive(Debug, Clone, Default)]
pub struct CallHistoryPage {
    pub entries: Vec<protocol::CallLogEntry>,
    pub next: Option<String>,
    pub more: bool,
}

/// One window of a member list.
#[derive(Debug, Clone)]
pub struct Members {
    pub pending: bool,
    pub sections: Vec<MemberSection>,
    pub members: Vec<Person>,
}

/// What a person's card shows beyond their row, as the network tells it.
#[derive(Debug, Clone, Default)]
pub struct About {
    pub bio: String,
    /// Titles of the Spaces we share.
    pub mutual_spaces: Vec<String>,
    /// As the network names it: `Europe/Lisbon`.
    pub time_zone: String,
    /// Seconds east of UTC now.
    pub utc_offset: Option<i32>,
    /// Whatever else the network says of them, label and value, in its own order: Phone,
    /// Username, Email, Title, Pronouns. At most twelve are kept.
    pub fields: Vec<(String, String)>,
    /// Unix seconds they joined the network.
    pub since: Option<i64>,
    pub mutual_friends: u32,
    /// Their banner's colour, `0xRRGGBB`, and its picture as a registered file.
    pub accent: Option<u32>,
    pub banner: Option<protocol::MediaFile>,
    // Typed. Fill these rather than `fields`: chatd moves a `fields` line labelled Pronouns,
    // Title, Department, Email, Phone or Roles into its place, and keeps the rest as leftovers.
    /// Their nickname in the Space asked about (`Command::ProfileIn`).
    pub nickname: String,
    pub pronouns: String,
    pub title: String,
    pub department: String,
    pub office: String,
    pub manager: Option<protocol::ProfilePerson>,
    pub reports: Vec<protocol::ProfilePerson>,
    /// Role names in that Space, colours left out.
    pub roles: Vec<String>,
    /// Email and phone; chatd masks phone numbers.
    pub contacts: Vec<Contact>,
    /// Mutual Spaces in all, when more than `mutual_spaces` lists.
    pub mutual_total: u32,
    /// Unix seconds: joined that Space, became friends, last seen.
    pub joined: Option<i64>,
    pub friends_since: Option<i64>,
    pub last_seen: Option<i64>,
    pub status_until: Option<i64>,
    /// Out of office, in the network's words, and until when.
    pub away: String,
    pub away_until: Option<i64>,
    pub note: String,
    pub warning: Option<Warning>,
}

/// What a conversation's info pane says beyond its row, as the network tells it.
#[derive(Debug, Clone, Default)]
pub struct ConversationAbout {
    pub description: String,
    pub members: Option<u32>,
    pub online: Option<u32>,
    pub created: Option<i64>,
    pub creator: String,
    pub notify: Notify,
    pub pinned: Option<u32>,
    pub external: String,
    pub link: String,
    pub disappearing: Option<u32>,
    pub bookmarks: Vec<protocol::Bookmark>,
    pub pending_requests: u32,
}

/// What a Space's info says, as the network tells it.
#[derive(Debug, Clone, Default)]
pub struct SpaceAbout {
    pub description: String,
    pub members: Option<u32>,
    pub online: Option<u32>,
    pub nickname: String,
    pub roles: Vec<String>,
    pub notify: Notify,
    pub tags: Vec<protocol::TeamTag>,
}

/// A page of what was shared, from the network's own search.
#[derive(Debug, Clone, Default)]
pub struct SharedPage {
    pub items: Vec<SharedItem>,
    /// The network has more below the last one.
    pub more: bool,
    /// The exact total, where the network counted.
    pub total: Option<u32>,
}

/// One page of workspace/account-wide files (`Command::AccountFiles`).
#[derive(Debug, Clone, Default)]
pub struct AccountFilesPage {
    pub items: Vec<SharedItem>,
    pub cursor: Option<String>,
    pub more: bool,
}

/// A page of a picker's catalog, and where the next one starts.
#[derive(Debug, Clone, Default)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub cursor: Option<String>,
}

/// A page of an account's activity feed, and where the next one starts.
#[derive(Debug, Clone, Default)]
pub struct ActivityPage {
    pub items: Vec<protocol::ActivityItem>,
    pub cursor: Option<String>,
}

/// A page of public channels from `BrowseChannels`.
#[derive(Debug, Clone, Default)]
pub struct BrowsedPage {
    pub channels: Vec<protocol::BrowsedChannel>,
    pub cursor: Option<String>,
}

/// A page of people who reacted to a message (`Command::Reactors`).
#[derive(Debug, Clone, Default)]
pub struct ReactorsPage {
    pub reactors: Vec<protocol::Reactor>,
    pub more: bool,
}

/// One page of network message search results (`Command::Search`).
#[derive(Debug, Clone, Default)]
pub struct SearchPage {
    pub hits: Vec<MessageHit>,
    pub cursor: Option<String>,
    pub total: Option<u32>,
}

impl From<Vec<MessageHit>> for SearchPage {
    fn from(hits: Vec<MessageHit>) -> Self {
        Self { hits, cursor: None, total: None }
    }
}

/// A picture on the call the user is in. Discord is the network that acts on it.
pub enum Picture {
    /// Our camera, on or off.
    Camera(bool),
    /// Our screen, on or off.
    Share(bool),
    /// Whose share to watch. `None` stops watching.
    Watch(Option<String>),
}

/// A Telegram link opened inside the app. `None` when the chat is not one this account can show.
#[derive(Debug, Clone)]
pub struct OpenedLink {
    pub conversation_id: String,
    pub message_id: Option<String>,
    pub title: String,
}

/// What chatd asks of an adapter. Ids are the network's own; the account is the adapter's.
pub enum Command {
    /// Fetch the recent history of a conversation the user opened, and correct what is cached
    /// (edits and deletes made while offline).
    Backfill(String),
    /// The same fetch for a conversation nobody is waiting on. Runs after everything else.
    Warm(String),
    /// The user scrolled past the oldest cached message: fetch the page before it.
    History { conversation_id: String, before: String },
    /// The user is reading forward from `after` through history that is not the newest page:
    /// fetch the page after it if the cache has a gap there. Sent for every such page, so check
    /// what is stored first (`Context::stored_ids`) and ask the network only for what is missing.
    HistoryAfter { conversation_id: String, after: String },
    /// The user jumped to a message (a reply's original, a search hit, a link): fetch the page
    /// around it. Sent when the cache does not hold the message or runs out on either side.
    HistoryAround { conversation_id: String, around: String },
    /// The user opened a Space: bring its tree up to date if the network needs asking.
    OpenSpace(String),
    /// The user collapsed or expanded a Space group (Discord category sync).
    Collapse { space_id: String, group_id: String, collapsed: bool },
    /// Tell the network the conversation has been read up to this message.
    MarkRead { conversation_id: String, message_id: String },
    /// Tell the network the marker moved back. `message_id` is the last message that still
    /// counts as read (possibly empty). `mentions` is how many unread messages name the user.
    MarkUnread { conversation_id: String, message_id: String, mentions: u32 },
    /// Tell the network the user played/viewed a voice message or story (`Viewed` receipt on Signal).
    Viewed { conversation_id: String, message_id: String },
    /// Tell Discord several conversations are read, in one request. Other networks ignore it.
    AckBulk { reads: Vec<(String, String)> },
    /// Send, with `client_id` as the network's nonce. Always answer through [`Context::sent`].
    Send {
        conversation_id: String,
        client_id: String,
        text: String,
        reply_to: Option<String>,
    },
    /// The same with files or options (`Outgoing`): stream each file from its path in parts,
    /// never the whole file in memory, telling [`Context::upload`] as it goes. Always answer
    /// through [`Context::sent`]; a failure names the file.
    SendWith {
        conversation_id: String,
        client_id: String,
        text: String,
        reply_to: Option<String>,
        with: Box<Outgoing>,
    },
    /// Stop uploading a `SendWith` and answer it through [`Context::sent`] as failed
    /// ("Cancelled"). Nothing, if it already went.
    CancelSend { conversation_id: String, client_id: String },
    /// Add (`on`) or take back our reaction. `emoji` is the emoji itself, or the id of a custom
    /// emoji's registered file. The store already shows it; answer `Err` and it is taken back.
    /// The network's own event for it is reported as usual and counts once.
    React {
        conversation_id: String,
        message_id: String,
        emoji: String,
        on: bool,
        burst: bool,
        reply: Reply<()>,
    },
    /// What reactions a conversation takes from this account, from what the network says of
    /// it: the chat's own set, our permissions and plan. One request at most, because the user
    /// opened it; chatd keeps the answer five minutes. `Unsupported` leaves every reaction to the
    /// network to judge.
    ReactionRules { conversation_id: String, reply: Reply<protocol::ReactionRules> },
    /// Current Telegram Stars balance (`payments.getStarsStatus`).
    StarsBalance { reply: Reply<(i64, i32)> },
    /// Send a confirmed paid Star reaction (`messages.sendPaidReaction`).
    SendPaidReaction {
        conversation_id: String,
        message_id: String,
        count: u32,
        anonymous: bool,
        reply: Reply<()>,
    },
    /// Toggle privacy of our paid Star reaction (`messages.togglePaidReactionPrivacy`).
    TogglePaidReactionPrivacy {
        conversation_id: String,
        message_id: String,
        anonymous: bool,
        reply: Reply<()>,
    },
    /// Change the account's default quick reaction (`messages.setDefaultReaction`).
    SetDefaultReaction {
        emoji: String,
        reply: Reply<()>,
    },
    /// Per-conversation send permissions, slow mode, timed mute, and paid-message star price.
    SendRules { conversation_id: String, reply: Reply<protocol::SendRules> },
    /// Who reacted to a message with `emoji`, paged.
    Reactors {
        conversation_id: String,
        message_id: String,
        emoji: String,
        burst: bool,
        after: Option<String>,
        limit: u32,
        reply: Reply<ReactorsPage>,
    },
    /// Replace the text of one of our messages. Report the edited message through
    /// [`Context::message`] before answering: nothing was changed locally. `spans` are the
    /// styled ranges of `text`; an adapter that cannot send them leaves them unused.
    Edit {
        conversation_id: String,
        message_id: String,
        text: String,
        spans: Vec<protocol::Span>,
        reply: Reply<()>,
    },
    /// Replace the media (photo/file) on one of our messages, optionally updating its caption.
    /// Report the edited message through [`Context::message`] before answering.
    EditMedia {
        conversation_id: String,
        message_id: String,
        file: protocol::OutgoingFile,
        text: String,
        spans: Vec<protocol::Span>,
        invert_media: bool,
        reply: Reply<()>,
    },
    /// Fetch the raw source text of a message for lossless editing.
    Source {
        conversation_id: String,
        message_id: String,
        reply: Reply<String>,
    },
    /// Delete a message (`Everyone` or `Me`). Report it through [`Context::deleted`] before answering.
    Delete {
        conversation_id: String,
        message_id: String,
        scope: protocol::DeleteScope,
        reply: Reply<()>,
    },
    /// Delete several messages in one conversation in one batch (`messages.deleteMessages` / `channels.deleteMessages`).
    DeleteMany {
        conversation_id: String,
        message_ids: Vec<String>,
        scope: protocol::DeleteScope,
        reply: Reply<()>,
    },
    /// Delete all messages from one participant in a supergroup/channel (`channels.deleteParticipantHistory`).
    DeleteParticipantHistory {
        conversation_id: String,
        person_id: String,
        reply: Reply<()>,
    },
    /// Report specific messages from one participant in a supergroup/channel as spam (`channels.reportSpam`).
    ReportSpamMessages {
        conversation_id: String,
        person_id: String,
        message_ids: Vec<String>,
        reply: Reply<()>,
    },
    /// Vote on a poll, retract a vote (`Some(vec![])`), or end a poll (`None`). Report the
    /// updated message through [`Context::message`] before answering.
    Vote {
        conversation_id: String,
        message_id: String,
        votes: Option<Vec<u32>>,
        reply: Reply<()>,
    },
    /// The user's draft is changing. Pace it: at most one typing signal per 8 s per
    /// conversation. `activity` is what they are doing; an adapter that only has typing ignores it.
    Typing { conversation_id: String, activity: Compose },
    /// The user followed a link out of this message (the shell is opening it). Telegram's terms
    /// want a sponsored message's follow told; nothing waits on it.
    FollowLink { conversation_id: String, message_id: String },
    /// The user started playing a voice or round video message; tell the network its contents
    /// were listened to (Telegram `readMessageContents`).
    MarkPlayed { conversation_id: String, message_id: String },
    /// A conversation's cloud draft here changed to `text` (empty clears it). The shell has
    /// already waited out the pauses between the keys, so this is one settled draft and there
    /// is nothing to answer.
    Draft { conversation_id: String, text: String },
    /// What the shell has on screen changed. Subscribe to presence, typing and member ranges for
    /// exactly this, and drop what is no longer in it.
    Watch(Arc<Watching>),
    /// Search the network's directory. Results go to [`Context::people`]; at most 25.
    SearchPeople(String),
    /// Resolve a public `@username` on click (cached on the adapter; never joins a chat).
    /// Answers `Some((conversation_id, title))` when found.
    ResolveUsername { username: String, reply: Reply<Option<(String, String)>> },
    Members {
        conversation_id: String,
        offset: u32,
        limit: u32,
        reply: Reply<Members>,
    },
    /// Add or invite people to a group or channel.
    MembersAdd {
        conversation_id: String,
        people: Vec<String>,
        reply: Reply<()>,
    },
    /// Remove one person from a group or channel.
    MembersRemove {
        conversation_id: String,
        person: String,
        reply: Reply<()>,
    },
    /// Rename a conversation or update its topic or description.
    ConversationUpdate {
        conversation_id: String,
        title: Option<String>,
        topic: Option<String>,
        description: Option<String>,
        reply: Reply<()>,
    },
    /// Set or clear a conversation's picture from a local file path.
    ConversationPicture {
        conversation_id: String,
        path: String,
        reply: Reply<()>,
    },
    /// Kick, ban, unban, or restrict a member of a group or channel.
    ModerateMember {
        conversation_id: String,
        person: String,
        action: protocol::MemberModeration,
        reply: Reply<()>,
    },
    /// Promote or demote an admin with specific rights and optional custom title.
    SetAdmin {
        conversation_id: String,
        person: String,
        rights: protocol::AdminRights,
        reply: Reply<()>,
    },
    /// Transfer group/channel ownership using the account's 2FA password.
    TransferOwnership {
        conversation_id: String,
        person: String,
        password: String,
        reply: Reply<()>,
    },
    /// Update group/channel default send permissions, slow mode, and feature toggles.
    UpdateChatSettings {
        conversation_id: String,
        settings: protocol::ChatSettings,
        reply: Reply<()>,
    },
    /// Read recent admin log events for a supergroup or channel.
    AdminLog {
        conversation_id: String,
        limit: u32,
        reply: Reply<Vec<protocol::AdminLogEntry>>,
    },
    /// List selectable send-as peers for a conversation and the current default peer id.
    SendAsOptions {
        conversation_id: String,
        reply: Reply<(Vec<protocol::SendAs>, Option<String>)>,
    },
    /// Save the default send-as peer for a conversation.
    SetDefaultSendAs {
        conversation_id: String,
        peer: String,
        reply: Reply<()>,
    },
    /// The id of the direct conversation with a person, created if there is none.
    StartDirect { person_id: String, reply: Reply<String> },
    /// Tiny call setup can create encrypted text history without enrolling a text device.
    StartDirectCall { person_id: String, reply: Reply<String> },
    /// Start a group conversation with several people; record it and answer with its id.
    StartGroup {
        people: Vec<String>,
        title: Option<String>,
        reply: Reply<String>,
    },
    /// Create a channel in a Space; record it and answer with its id.
    CreateChannel {
        space_id: String,
        name: String,
        kind: protocol::ConversationKind,
        private: bool,
        group: Option<String>,
        description: Option<String>,
        username: Option<String>,
        reply: Reply<String>,
    },
    /// Create a group conversation with two or more members (person ids or phone numbers) and an optional name.
    CreateGroup {
        members: Vec<String>,
        name: Option<String>,
        reply: Reply<String>,
    },
    /// The id of the thread a channel message starts. Discord only: create the thread channel,
    /// record it, and answer with its id. `name` is already one line, at most 100 characters.
    StartThread {
        conversation_id: String,
        message_id: String,
        name: String,
        reply: Reply<String>,
    },
    /// One person's card. One request to the network, and only because the user opened it.
    Profile { person_id: String, reply: Reply<About> },
    /// The same, opened from inside a Space: add the nickname, roles and join date there. An
    /// adapter that answers `Unsupported` is asked `Profile` instead.
    ProfileIn { person_id: String, space_id: String, reply: Reply<About> },
    /// A conversation's info pane: one request, because the user opened it.
    ConversationInfo { conversation_id: String, reply: Reply<ConversationAbout> },
    /// Paged activity feed (mentions, thread replies, reactions, apps) for this account.
    ActivityFeed {
        kind: Option<protocol::ActivityItemKind>,
        cursor: Option<String>,
        limit: u32,
        reply: Reply<ActivityPage>,
    },
    /// Mark one activity feed item (`Some(id)`) or all items (`None`) read.
    ActivityMarkRead {
        item_id: Option<String>,
        reply: Reply<()>,
    },
    /// Members who have read a given message (`Request::ReadBy`).
    ReadBy {
        conversation_id: String,
        message_id: String,
        reply: Reply<Vec<protocol::ReadBy>>,
    },
    /// Add a bookmark to a channel.
    BookmarkAdd {
        conversation_id: String,
        title: String,
        link: String,
        emoji: String,
        reply: Reply<protocol::Bookmark>,
    },
    /// Edit an existing channel bookmark.
    BookmarkEdit {
        conversation_id: String,
        bookmark_id: String,
        title: Option<String>,
        link: Option<String>,
        emoji: Option<String>,
        reply: Reply<protocol::Bookmark>,
    },
    /// Remove a channel bookmark.
    BookmarkRemove {
        conversation_id: String,
        bookmark_id: String,
        reply: Reply<()>,
    },
    /// Browse public channels in a Space (paged, never fetched whole).
    BrowseChannels {
        space_id: String,
        query: String,
        cursor: Option<String>,
        limit: u32,
        reply: Reply<BrowsedPage>,
    },
    /// Preview an unjoined public channel (stores as `joined: false`, `read_only: true`, outside the sidebar).
    PreviewChannel {
        conversation_id: String,
        reply: Reply<()>,
    },
    /// Join a public channel (`conversations.join`).
    Join {
        conversation_id: String,
        reply: Reply<()>,
    },
    /// Paged workspace/account-wide file list (Slack `files.list` without a channel).
    AccountFiles {
        query: String,
        kind: Option<String>,
        cursor: Option<String>,
        limit: u32,
        reply: Reply<AccountFilesPage>,
    },
    /// Delete a file on the network (Slack `files.delete`).
    DeleteFile {
        file_id: String,
        reply: Reply<()>,
    },
    /// A Space's info, opened from its name.
    SpaceInfo { space_id: String, reply: Reply<SpaceAbout> },
    /// Preview a Space invite link or code.
    InvitePreview {
        code: String,
        reply: Reply<protocol::InvitePreview>,
    },
    /// Join a Space from an invite link or code; returns `(space_id, channel_id)`.
    InviteJoin {
        code: String,
        /// `(space_id, conversation_id, pending_approval)`.
        reply: Reply<(Option<String>, Option<String>, bool)>,
    },
    /// Lists exported invite links for a group or channel.
    ChatInvites {
        conversation_id: String,
        revoked: bool,
        reply: Reply<Vec<protocol::ChatInviteLink>>,
    },
    /// Creates a new invite link for a group or channel.
    CreateInvite {
        conversation_id: String,
        title: Option<String>,
        expire_date: Option<i64>,
        usage_limit: Option<u32>,
        request_needed: bool,
        reply: Reply<protocol::ChatInviteLink>,
    },
    /// Revokes an exported invite link.
    RevokeInvite {
        conversation_id: String,
        link: String,
        reply: Reply<protocol::ChatInviteLink>,
    },
    /// Lists pending join requests for a group or channel.
    JoinRequests {
        conversation_id: String,
        link: Option<String>,
        limit: u32,
        reply: Reply<Vec<protocol::JoinRequest>>,
    },
    /// Approves or declines one (`user_id: Some`) or all (`user_id: None`) pending join requests.
    HandleJoinRequest {
        conversation_id: String,
        user_id: Option<String>,
        approved: bool,
        link: Option<String>,
        reply: Reply<()>,
    },
    /// Moderate a member in a Space (kick, ban, timeout, untimeout, nickname).
    Moderate {
        space_id: String,
        person_id: String,
        action: protocol::ModerateAction,
        reason: String,
        reply: Reply<()>,
    },
    /// Lift a member ban in a Space.
    Unban {
        space_id: String,
        person_id: String,
        reason: String,
        reply: Reply<()>,
    },
    /// Set the network's notification setting. `until` is when a mute ends, as Unix seconds.
    /// Absent means muted until it is turned off, or not muted. An adapter with no timed mute
    /// ignores it. Report the new mute through [`Context::muted`] before answering.
    SetMute {
        conversation_id: String,
        notify: Notify,
        until: Option<i64>,
        reply: Reply<()>,
    },
    /// Set or clear a conversation's disappearing-messages timer (`seconds == 0` turns it off).
    /// Report the new timer through [`Context::disappearing`] before answering.
    SetDisappearing {
        conversation_id: String,
        seconds: u32,
        reply: Reply<()>,
    },
    /// Read the account's default disappearing-messages timer for new chats (seconds, 0 = off).
    GetDefaultDisappearing {
        reply: Reply<u32>,
    },
    /// Set the account's default disappearing-messages timer for new chats (seconds, 0 = off).
    SetDefaultDisappearing {
        seconds: u32,
        reply: Reply<()>,
    },
    /// Create or edit a chat folder on the network (Telegram `messages.updateDialogFilter`).
    FolderSave {
        folder: protocol::FolderSpec,
        reply: Reply<protocol::Folder>,
    },
    /// Delete a chat folder on the network.
    FolderDelete {
        folder_id: String,
        reply: Reply<()>,
    },
    /// Reorder chat folders on the network.
    FoldersReorder {
        order: Vec<String>,
        reply: Reply<()>,
    },
    /// Fetch additional or searched topics for a forum (Telegram `messages.getForumTopics`).
    /// Answers with the network's total topic count for the query/forum.
    LoadTopics {
        conversation_id: String,
        query: Option<String>,
        offset: u32,
        limit: u32,
        reply: Reply<u32>,
    },
    /// Create a topic in a forum; answers with the new topic's conversation id.
    TopicCreate {
        conversation_id: String,
        title: String,
        icon_color: Option<i32>,
        icon_emoji_id: Option<String>,
        reply: Reply<String>,
    },
    /// Edit a forum topic (rename, icon, close/reopen, hide, pin/unpin).
    TopicUpdate {
        conversation_id: String,
        title: Option<String>,
        icon_emoji_id: Option<String>,
        closed: Option<bool>,
        hidden: Option<bool>,
        pinned: Option<bool>,
        reply: Reply<()>,
    },
    /// Delete a forum topic and its history.
    TopicDelete {
        conversation_id: String,
        reply: Reply<()>,
    },
    /// Fetch default forum topic icons.
    TopicIcons {
        reply: Reply<CatalogPage>,
    },
    /// Fetch network-suggested chat folders.
    SuggestedFolders {
        reply: Reply<Vec<protocol::SuggestedFolder>>,
    },
    /// Star or unstar a conversation on the network. Report the flag through [`Context::starred`]
    /// before answering, and only after the network has it. Answer `Err` to leave the local flag
    /// as it was. Networks that do not serve this never receive it: their star stays local.
    Star { conversation_id: String, starred: bool, reply: Reply<()> },
    /// File a conversation on the network: pin/unpin, archive/unarchive, or add/remove a folder.
    /// Report the new filing through [`Context::filing`] before answering.
    File {
        conversation_id: String,
        pinned: Option<bool>,
        archived: Option<bool>,
        folder_add: Option<String>,
        folder_remove: Option<String>,
        reply: Reply<()>,
    },
    /// Star or unstar one message on the network (Google Messages, Slack Later).
    StarMessage {
        conversation_id: String,
        message_id: String,
        starred: bool,
        reply: Reply<()>,
    },
    /// Saved / starred messages across the account (or one conversation when set), newest first.
    StarredMessages {
        conversation_id: Option<String>,
        limit: u32,
        reply: Reply<Vec<Message>>,
    },
    /// Set or cancel a network reminder on one message (Slack `reminders.add` / `reminders.delete`).
    Remind {
        conversation_id: String,
        message_id: String,
        remind_at: Option<i64>,
        reply: Reply<()>,
    },
    /// Lists pending scheduled messages for one conversation (Slack `chat.scheduledMessages.list`).
    ScheduledList {
        conversation_id: String,
        reply: Reply<Vec<protocol::ScheduledMessage>>,
    },
    /// Cancels a pending scheduled message (Slack `chat.deleteScheduledMessage` / Telegram `messages.deleteScheduledMessages`).
    CancelScheduled {
        conversation_id: String,
        scheduled_id: String,
        reply: Reply<()>,
    },
    /// Sends a pending scheduled message immediately (Telegram `messages.sendScheduledMessages`).
    SendScheduledNow {
        conversation_id: String,
        scheduled_id: String,
        reply: Reply<()>,
    },
    /// Edits/reschedules a pending scheduled message (Slack delete + `chat.scheduleMessage`).
    Reschedule {
        conversation_id: String,
        scheduled_id: String,
        text: Option<String>,
        post_at: i64,
        reply: Reply<protocol::ScheduledMessage>,
    },
    /// Follow (`on: true`) or unfollow (`on: false`) a thread (Slack `subscriptions.thread.add` / `remove`).
    FollowThread {
        conversation_id: String,
        on: bool,
        reply: Reply<()>,
    },
    /// Accept or decline/ignore a message request (Discord).
    AnswerMessageRequest { conversation_id: String, accept: bool, reply: Reply<()> },
    /// Leave a group conversation. Report it gone through [`Context::conversation_gone`].
    Leave { conversation_id: String, reply: Reply<()> },
    /// Leave a Space (e.g. Discord server `DELETE /users/@me/guilds/{id}`).
    LeaveSpace { space_id: String, reply: Reply<()> },
    /// Close a direct conversation as the network's own × does. Report it through
    /// [`Context::filing`] (hidden) or [`Context::conversation_gone`].
    CloseDirect { conversation_id: String, reply: Reply<()> },
    ClearHistory { conversation_id: String, delete_chat: bool, revoke: bool, reply: Reply<()> },
    /// The messages pinned in a conversation, newest first, at most 50.
    Pinned { conversation_id: String, reply: Reply<Vec<Message>> },
    /// Pins or unpins a message on the network.
    Pin {
        conversation_id: String,
        message_id: String,
        pinned: bool,
        notify: bool,
        me_only: bool,
        reply: Reply<()>,
    },
    /// Executes an interactive Block Kit control on a message (Slack `blocks.actions`).
    MessageAction {
        conversation_id: String,
        message_id: String,
        block_id: String,
        action_id: String,
        element_type: String,
        value: String,
        values: Vec<String>,
        reply: Reply<()>,
    },
    /// Presses an inline callback button under a bot message (Telegram `messages.getBotCallbackAnswer`).
    PressButton {
        conversation_id: String,
        message_id: String,
        data: String,
        reply: Reply<protocol::ButtonAnswer>,
    },
    /// Reads account-wide notification defaults and per-chat exceptions.
    GetNotifyDefaults {
        reply: Reply<protocol::NotifyDefaults>,
    },
    /// Updates an account-wide notification default for private chats, groups, or channels.
    SetNotifyDefault {
        scope: protocol::NotifyDefaultScope,
        muted: bool,
        reply: Reply<protocol::NotifyDefaults>,
    },
    /// Resets all account-wide notification defaults and exceptions.
    ResetNotifyDefaults {
        reply: Reply<protocol::NotifyDefaults>,
    },
    /// Lists Saved Messages grouped by source peer plus reaction tags (Telegram).
    SavedDialogs {
        reply: Reply<(Vec<protocol::SavedDialog>, Vec<protocol::SavedTag>)>,
    },
    /// Fetches Saved Messages filtered by source peer and/or reaction tag (Telegram).
    SavedHistory {
        peer_id: Option<String>,
        tag: Option<String>,
        limit: u32,
        reply: Reply<Vec<protocol::Message>>,
    },
    /// Lists active signed-in devices/sessions for this account.
    ActiveSessions {
        reply: Reply<Vec<protocol::ActiveSession>>,
    },
    /// Terminates another active session by id.
    EndSession {
        session_id: String,
        reply: Reply<()>,
    },
    /// Fetches unread mention or reaction message IDs in a conversation.
    UnreadItems {
        conversation_id: String,
        kind: protocol::UnreadItemKind,
        limit: u32,
        reply: Reply<Vec<String>>,
    },
    /// Marks unread mentions or reactions read in a conversation.
    MarkUnreadItemsRead {
        conversation_id: String,
        kind: protocol::UnreadItemKind,
        message_ids: Vec<String>,
        reply: Reply<()>,
    },
    /// Forwards one or more messages to target conversations on the same account.
    /// Answers with `(target_conversation_id, Ok(()) | Err(reason))` per target.
    Forward {
        from_conversation_id: String,
        message_ids: Vec<String>,
        to_conversation_ids: Vec<String>,
        note: String,
        reply: Reply<Vec<(String, Result<(), String>)>>,
    },
    /// Save or remove a GIF in the user's saved GIFs collection (`messages.saveGif`).
    SaveGif {
        id: String,
        unsave: bool,
        reply: Reply<()>,
    },
    /// Install or remove a sticker pack, or restack the installed packs.
    StickerSet {
        set: String,
        install: Option<bool>,
        order: Vec<i64>,
        reply: Reply<()>,
    },
    /// Star or unstar one sticker (`messages.faveSticker`).
    FaveSticker {
        id: String,
        unfave: bool,
        reply: Reply<()>,
    },
    /// What was shared in a conversation, from the network's own search, because the user
    /// clicked "Search all". `before` continues below an item already listed.
    SearchShared {
        conversation_id: String,
        kind: SharedKind,
        before: Option<String>,
        limit: u32,
        reply: Reply<SharedPage>,
    },
    /// Recent calls across all chats on an account, newest first.
    CallHistory {
        missed: bool,
        before: Option<String>,
        limit: u32,
        reply: Reply<CallHistoryPage>,
    },
    /// Delete one or more call log entries by message id (`revoke` for both sides).
    DeleteCallEntries {
        message_ids: Vec<String>,
        revoke: bool,
        reply: Reply<()>,
    },
    /// Read an account's network privacy policy for `key`.
    GetPrivacy {
        key: protocol::PrivacyKey,
        reply: Reply<protocol::PrivacyPolicy>,
    },
    /// Write an account's network privacy policy for `key`.
    SetPrivacy {
        key: protocol::PrivacyKey,
        policy: protocol::PrivacyPolicy,
        reply: Reply<protocol::PrivacyPolicy>,
    },
    /// Read an account's out-of-office / automatic replies settings.
    GetOutOfOffice {
        reply: Reply<protocol::OutOfOffice>,
    },
    /// Write an account's out-of-office / automatic replies settings.
    SetOutOfOffice {
        ooo: protocol::OutOfOffice,
        reply: Reply<protocol::OutOfOffice>,
    },
    /// List mentionable tags in a Space (e.g. Teams team tags).
    SpaceTags {
        space_id: String,
        reply: Reply<Vec<protocol::TeamTag>>,
    },
    /// A picker's catalog: custom emoji (of one Space when `space_id` is set), stickers, or GIFs
    /// for `query`. Register every preview through [`Context::media`]. Only on a key press:
    /// never ahead.
    Catalog {
        kind: CatalogKind,
        space_id: Option<String>,
        query: String,
        cursor: Option<String>,
        reply: Reply<CatalogPage>,
    },
    /// These rows are on screen with no preview line. Fetch each one's newest message, at most
    /// one request a second, and record it ([`Context::history`] or [`Context::preview`]).
    /// Never sent to Discord, where a fetch per DM is the burst that flags accounts.
    Previews(Vec<String>),
    /// The user opened the network's home list (`Request::Home`): bring the conversations that
    /// belong to no Space up to date if the network needs asking.
    OpenHome,
    /// The user opened a Space's Threads list: fetch the network's list of followed threads
    /// once, and record them with `Filing::followed`.
    OpenThreads(String),
    /// The user opened a forum: fetch its topics once and answer, so the landing topic the
    /// shell then reads from the store is named on what the network says now.
    OpenForum { conversation_id: String, reply: Reply<()> },
    /// The user answered, withdrew or ended a friendship, or blocked someone. Report the new
    /// relationship through [`Context::relation`] before answering.
    Relate {
        person_id: String,
        action: RelateAction,
        reply: Reply<()>,
    },
    /// Send a friend request by username (e.g. Discord `POST /users/@me/relationships`).
    AddFriend {
        username: String,
        reply: Reply<()>,
    },
    /// Report a message, person, or conversation to the network.
    Report {
        target: protocol::ReportTarget,
        reason: protocol::ReportReason,
        note: String,
        block: bool,
        reply: Reply<()>,
    },
    /// Ask the network's supported report reasons for `target`.
    ReportReasons {
        target: protocol::ReportTarget,
        reply: Reply<Vec<protocol::ReportReason>>,
    },
    /// The user set their own presence, and their status text unless it is `None` (empty clears
    /// it). The hub already shows it; answer `Err` and it is put back.
    SetPresence {
        presence: Presence,
        status: Option<String>,
        status_emoji: Option<String>,
        clear_at: Option<i64>,
        reply: Reply<()>,
    },
    /// Edit our own profile fields on the network. `None` leaves a field unchanged.
    SetProfile {
        name: Option<String>,
        about: Option<String>,
        pronouns: Option<String>,
        username: Option<String>,
        emoji: Option<String>,
        birthday: Option<String>,
        reply: Reply<()>,
    },
    /// Set (or clear, when `path` is empty) our own profile picture from a local file path.
    SetAvatar {
        path: String,
        reply: Reply<()>,
    },
    /// Set (or clear, when `None` or empty) our per-space nickname.
    SetNickname {
        space_id: String,
        nickname: Option<String>,
        reply: Reply<()>,
    },
    /// Set (or clear, when `None` or empty) a private nickname for a friend.
    SetFriendNickname {
        person_id: String,
        nickname: Option<String>,
        reply: Reply<()>,
    },
    /// A registered file is wanted: answer with where and how to fetch it now.
    Download { id: String, url: String, reply: Reply<Download> },
    /// The pane's search asked the network itself: a Space's own search, or one conversation's.
    /// Answer with the hits, mapped like the cache's, or why not. `at` is when the shell
    /// asked: one queued past the shell's patience does not ask the network at all.
    Search {
        query: String,
        scope: SearchScope,
        filters: protocol::SearchFilters,
        cursor: Option<String>,
        at: Instant,
        reply: Reply<SearchPage>,
    },
    /// Jump to the message closest to `date` (Unix seconds) in a conversation; answers with the message id.
    JumpToDate {
        conversation_id: String,
        date: i64,
        reply: Reply<Option<String>>,
    },
    /// Calendar days with matching messages in a conversation (Telegram `messages.getSearchResultsCalendar`).
    SearchCalendar {
        conversation_id: String,
        kind: Option<SharedKind>,
        offset_date: i64,
        reply: Reply<Vec<protocol::SearchCalendarDay>>,
    },
    /// The shell asked for these files and the media cache knows none of them, or knows them
    /// only by Telegram's name for a file it kept, since gone from disk. An adapter that fetches
    /// files over its own session (Telegram, Messages) fetches those that are its own, such as
    /// the original a picture's viewer opened, and records each as the cache would. Telegram
    /// also takes it as what is on screen: a picture a message shows goes ahead of those that
    /// are not, even one no page has named yet. Others ignore it.
    Wanted(Vec<String>),
    /// The link that opens this conversation's call in the official app.
    Handoff { conversation_id: String, reply: Reply<String> },
    /// mediad is up: join or ring on the network, then report through [`Context::voice`].
    CallJoin(String),
    /// Leave, cancel or decline on the network. Also sent when the call failed here (mediad
    /// died, a deadline passed), so the network stops showing us in the channel or ringing the
    /// other side. May arrive for a call the adapter already left: then it is nothing.
    CallLeave(String),
    /// Our mute and deafen changed; the network shows them to others.
    CallAudio { muted: bool, deafened: bool },
    /// A camera, a screen share, or whose share to watch. Nothing answers: the call's own
    /// events say what the network did.
    CallPicture(Picture),
    /// Rings everyone (`None`) or specific people (`Some`) in the current direct/group call.
    CallRing { conversation_id: String, people: Option<Vec<String>>, reply: Reply<()> },
    /// Stops ringing everyone (`None`) or specific people (`Some`) in the current direct/group call.
    CallStopRinging { conversation_id: String, people: Option<Vec<String>>, reply: Reply<()> },
    /// Adds friends to the current direct or group call, returning the resulting conversation id.
    CallAddPeople { conversation_id: String, people: Vec<String>, reply: Reply<String> },
    /// A Tiny server's live socket: take or leave a seat in a room's call, remove someone from
    /// one, or hear that the user opened the server.
    Tiny(tiny::live::Ask),
    /// The user signed out: end the session on the network, best effort, then answer. chatd
    /// waits five seconds for the answer and forgets the account either way.
    SignOut(Reply<()>),
    /// The adapter is being dropped. `serve_commands` returns on it; an adapter never sees it.
    Stop,
    /// Open a `t.me` or `telegram.me` link as a chat this account already has, or a preview of a
    /// public username. Never joins. Other adapters refuse it.
    OpenLink { url: String, reply: Reply<Option<OpenedLink>> },
    /// Translate these messages. The adapter returns the words; it does not replace what is stored.
    Translate {
        conversation_id: String,
        message_ids: Vec<String>,
        to: String,
        reply: Reply<Vec<Translation>>,
    },
}

impl Command {
    /// Answers a command this adapter does not serve, so nothing waits for it.
    pub fn unsupported(self, ctx: &Context) {
        match self {
            Command::Members { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ResolveUsername { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MembersAdd { reply, .. }
            | Command::MembersRemove { reply, .. }
            | Command::ConversationUpdate { reply, .. }
            | Command::ConversationPicture { reply, .. }
            | Command::ModerateMember { reply, .. }
            | Command::SetAdmin { reply, .. }
            | Command::TransferOwnership { reply, .. }
            | Command::UpdateChatSettings { reply, .. }
            | Command::SetDefaultSendAs { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AdminLog { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendAsOptions { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StartDirect { reply, .. }
            | Command::StartDirectCall { reply, .. }
            | Command::StartGroup { reply, .. }
            | Command::CreateChannel { reply, .. }
            | Command::CreateGroup { reply, .. }
            | Command::StartThread { reply, .. }
            | Command::Handoff { reply, .. }
            | Command::CallAddPeople { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::CallRing { reply, .. } | Command::CallStopRinging { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::Profile { reply, .. } | Command::ProfileIn { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ConversationInfo { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceInfo { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::InvitePreview { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::InviteJoin { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ChatInvites { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CreateInvite { reply, .. }
            | Command::RevokeInvite { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::JoinRequests { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::HandleJoinRequest { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetMute { reply, .. }
            | Command::Moderate { reply, .. }
            | Command::Unban { reply, .. }
            | Command::SetDisappearing { reply, .. }
            | Command::SetDefaultDisappearing { reply, .. }
            | Command::FolderDelete { reply, .. }
            | Command::FoldersReorder { reply, .. }
            | Command::TopicUpdate { reply, .. }
            | Command::TopicDelete { reply, .. }
            | Command::Leave { reply, .. }
            | Command::LeaveSpace { reply, .. }
            | Command::CloseDirect { reply, .. }
            | Command::ClearHistory { reply, .. }
            | Command::Star { reply, .. }
            | Command::File { reply, .. }
            | Command::StarMessage { reply, .. }
            | Command::Remind { reply, .. }
            | Command::CancelScheduled { reply, .. }
            | Command::SendScheduledNow { reply, .. }
            | Command::FollowThread { reply, .. } => {
                drop(reply.send(Err(Refusal::Unsupported)))
            }
            Command::GetDefaultDisappearing { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::FolderSave { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::LoadTopics { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TopicCreate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::TopicIcons { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SuggestedFolders { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ScheduledList { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Reschedule { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StarredMessages { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Pinned { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Pin { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MessageAction { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::PressButton { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetNotifyDefaults { reply }
            | Command::SetNotifyDefault { reply, .. }
            | Command::ResetNotifyDefaults { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SavedDialogs { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SavedHistory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ActiveSessions { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::EndSession { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::UnreadItems { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::MarkUnreadItemsRead { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Forward { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SaveGif { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StickerSet { reply, .. } | Command::FaveSticker { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SearchShared { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::CallHistory { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::DeleteCallEntries { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetPrivacy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetPrivacy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::GetOutOfOffice { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SetOutOfOffice { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SpaceTags { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Catalog { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ActivityFeed { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ActivityMarkRead { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReadBy { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::BookmarkAdd { reply, .. }
            | Command::BookmarkEdit { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::BookmarkRemove { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::BrowseChannels { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::PreviewChannel { reply, .. }
            | Command::Join { reply, .. }
            | Command::DeleteFile { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::AccountFiles { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReactionRules { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::StarsBalance { reply } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendPaidReaction { reply, .. }
            | Command::TogglePaidReactionPrivacy { reply, .. }
            | Command::SetDefaultReaction { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendRules { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Reactors { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SendWith { conversation_id, client_id, .. } => {
                let why = format!("Sending files and options is not built for {} yet.", ctx.account.network.label());
                ctx.sent(&conversation_id, &client_id, Err(why));
            }
            Command::React { reply, .. }
            | Command::Edit { reply, .. }
            | Command::EditMedia { reply, .. }
            | Command::Delete { reply, .. }
            | Command::DeleteMany { reply, .. }
            | Command::DeleteParticipantHistory { reply, .. }
            | Command::ReportSpamMessages { reply, .. }
            | Command::Vote { reply, .. }
            | Command::Relate { reply, .. }
            | Command::AddFriend { reply, .. }
            | Command::Report { reply, .. }
            | Command::SetPresence { reply, .. }
            | Command::SetProfile { reply, .. }
            | Command::SetAvatar { reply, .. }
            | Command::SetNickname { reply, .. }
            | Command::SetFriendNickname { reply, .. }
            | Command::AnswerMessageRequest { reply, .. }
            | Command::OpenForum { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Source { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::OpenLink { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Translate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::ReportReasons { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            // Nothing to end on the network: the session is simply forgotten.
            Command::SignOut(reply) => drop(reply.send(Ok(()))),
            Command::Download { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Search { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::JumpToDate { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::SearchCalendar { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Send { conversation_id, client_id, .. } => {
                ctx.sent(&conversation_id, &client_id, Err("Sending is not built for this network.".to_owned()));
            }
            Command::Tiny(ask) => ask.unsupported(),
            Command::CallJoin(conversation_id) => {
                let detail = format!("{} calls are not built yet.", ctx.account.network.label());
                ctx.call_failed(&conversation_id, CallFailure::Refused, &detail);
            }
            _ => {}
        }
    }
}

/// Why a connection ended.
pub enum Failure {
    /// The network rejected the session. Retrying cannot help; the user has to sign in again.
    SignedOut(String),
    /// Anything else. Reconnect after a pause.
    Dropped(String),
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(err: E) -> Failure {
        Failure::Dropped(err.to_string())
    }
}

/// Tells an adapter's threads to stop, and wakes the ones that are waiting.
#[derive(Default)]
struct Halt {
    stopped: AtomicBool,
    recovery: AtomicU64,
    /// Panics caught on this adapter's threads. The first is recovered from; the second stops
    /// the adapter and the account says why.
    panics: AtomicU32,
    waiting: Mutex<()>,
    wake: Condvar,
}

/// What every adapter thread holds, and every call an adapter makes back into chatd.
#[derive(Clone)]
pub struct Context {
    pub account: AccountKey,
    hub: Arc<Hub>,
    /// The adapter's own inbox, so its socket thread can queue work for its command thread.
    pub commands: Sender<Command>,
    stop: Arc<Halt>,
}

type Stores = rusqlite::Result<()>;

impl Context {
    pub fn stopped(&self) -> bool {
        self.stop.stopped.load(Ordering::Relaxed)
    }

    /// Waits for `time`, or less if the adapter is stopped meanwhile. True if it was stopped.
    /// One wakeup however long the wait: use this, not a loop of short sleeps.
    pub fn pause(&self, time: Duration) -> bool {
        let waiting = lock(&self.stop.waiting);
        drop(self.stop.wake.wait_timeout_while(waiting, time, |_| !self.stopped()));
        self.stopped()
    }

    fn reconnect_pause(&self, time: Duration, generation: u64) -> ReconnectWait {
        let waiting = lock(&self.stop.waiting);
        drop(self.stop.wake.wait_timeout_while(waiting, time, |_| {
            !self.stopped() && self.stop.recovery.load(Ordering::Relaxed) == generation
        }));
        if self.stopped() { ReconnectWait::Stopped }
        else if self.stop.recovery.load(Ordering::Relaxed) != generation { ReconnectWait::Recovered }
        else { ReconnectWait::Elapsed }
    }

    pub fn key(&self, id: &str) -> Key {
        Key::new(&self.account, id)
    }

    /// Whether a conversation may appear where the user asked for what is listed: hidden
    /// conversations and message requests stay out of it, as they stay out of the cache's own
    /// search. A conversation the store does not know is not listed.
    pub fn listable(&self, conversation_id: &str) -> bool {
        self.hub.store().conversation_listed(&self.key(conversation_id)).unwrap_or_default()
    }

    // The account.

    /// The socket is up (`Connected`) or down (`Offline`).
    pub fn link(&self, link: Link) {
        if link != Link::Connected {
            self.hub.forget_live(&self.account);
        }
        self.hub.set_link(&self.account, link, "");
    }

    /// Something only the network's own app can do stands in the way: a captcha, a phone or
    /// e-mail verification. Say what in words the user can act on; empty once it is done. Shown
    /// on the account whatever its link is.
    pub fn attention(&self, required: &str) {
        self.hub.set_required(&self.account, required);
    }

    /// The network reissued part of the session (Slack's rotated `d` cookie). `rewrite` is given
    /// the network's whole stored session, which may hold other accounts, and answers it with
    /// the new part in place; `None` leaves it. True once the keychain holds it.
    pub fn save_session(&self, rewrite: impl FnOnce(&str) -> Option<String>) -> bool {
        match session::rewrite(self.account.network, rewrite) {
            Ok(saved) => saved,
            Err(why) => {
                tracing::warn!("chatd: {}: the session was not saved: {}", self.account.network.label(), net::redact(&why));
                false
            }
        }
    }

    /// The account's name (a workspace's, or the user's own) and our person id on it.
    pub fn describe(&self, name: &str, me: &str) -> Stores {
        self.hub.store().set_account(&self.account, name, me)?;
        self.hub.changed(Change::Status);
        Ok(())
    }

    // Spaces, trees and rows.

    /// Every Space of the account, as the network lists them at connect.
    pub fn spaces(&self, spaces: &[SpaceRow]) -> Stores {
        self.hub.store().replace_spaces(&self.account, spaces)?;
        self.hub.changed(Change::Spaces { account: self.account.clone() });
        Ok(())
    }

    /// Sets whether the user can create channels in `space_id`.
    pub fn set_space_can_create_channels(&self, space_id: &str, can: bool) -> Stores {
        let space = self.key(space_id);
        if self.hub.store().set_space_can_create_channels(&space, can)? {
            self.hub.changed(Change::Spaces { account: self.account.clone() });
        }
        Ok(())
    }

    /// One Space's whole tree: only what the user may see, in the network's order. Rows go
    /// straight to the store; keep none of them.
    pub fn tree(&self, space_id: &str, groups: &[Group], conversations: &[ConversationRow]) -> Stores {
        let space = self.key(space_id);
        self.hub.store().replace_tree(&space, groups, conversations)?;
        self.hub.changed(Change::Tree { space });
        Ok(())
    }

    /// One group of a Space created, renamed or moved, without sending the whole tree again.
    pub fn group(&self, space_id: &str, group: &Group) -> Stores {
        let space = self.key(space_id);
        self.hub.store().upsert_group(&space, group)?;
        self.hub.changed(Change::Tree { space });
        Ok(())
    }

    /// Updates a group's collapsed state from the network (e.g. Discord category settings).
    pub fn set_collapsed(&self, space_id: &str, group_id: &str, collapsed: bool) -> Stores {
        let space = self.key(space_id);
        if self.hub.store().set_collapsed(&space, group_id, collapsed)? {
            self.hub.changed(Change::Tree { space });
        }
        Ok(())
    }

    /// A group was removed. Its conversations stay, under no group, until the network moves them.
    pub fn group_gone(&self, space_id: &str, group_id: &str) -> Stores {
        let space = self.key(space_id);
        if self.hub.store().remove_group(&space, group_id)? {
            self.hub.changed(Change::Tree { space });
        }
        Ok(())
    }

    /// Everyone in a voice channel, as the network lists them, us included when we are in it,
    /// and how many there are in all. Report it for every voice channel of a Space on screen
    /// whenever it changes; an empty list when the channel empties. Never stored, and the tree
    /// is not announced: the row is told alone.
    pub fn occupants(&self, conversation_id: &str, people: Vec<Occupant>, total: u32) {
        self.hub.set_occupants(self.key(conversation_id), Occupants { people, total });
    }

    /// One conversation: a direct message, or a channel created, renamed or moved.
    pub fn conversation(&self, row: &ConversationRow) -> Stores {
        self.hub.store().upsert_conversation(&self.account, row)?;
        self.row_changed(&row.id, row.space.as_deref());
        if row.filing.is_some() {
            self.hub.changed(Change::Folders { account: self.account.clone() });
        }
        Ok(())
    }

    pub fn conversation_gone(&self, conversation_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        let space = self.hub.store().conversation(&conversation)?.and_then(|row| row.space);
        self.hub.store().remove_conversation(&conversation)?;
        self.row_changed(conversation_id, space.as_deref());
        self.hub.changed(Change::Status);
        Ok(())
    }

    /// The unread state as the network has it: at connect, and whenever another device reads.
    /// `unread` for a channel may be just 1 for "some". `updated` is the newest message's time.
    pub fn counts(
        &self,
        conversation_id: &str,
        unread: u32,
        mentions: u32,
        last_read: Option<&str>,
        updated: i64,
    ) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_counts(&conversation, unread, mentions, last_read, updated)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    pub fn unread_reactions(&self, conversation_id: &str, count: u32) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_unread_reactions(&conversation, count)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    pub fn mentions_count(&self, conversation_id: &str, count: u32) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_mentions_count(&conversation, count)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// Another device deliberately moved the read marker, including backwards. [`Self::counts`]
    /// refuses that move, so a manual acknowledgement uses this instead.
    pub fn manual_ack(&self, conversation_id: &str, last_read: &str, mentions: Option<u32>) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().apply_manual_ack(&conversation, last_read, mentions)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// The user marked the conversation unread. The badge becomes at least one; a higher count
    /// stays. The read marker is left where it is.
    pub fn mark_unread(&self, conversation_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().mark_unread_badge(&conversation)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// A conversation this client already has was upgraded to end-to-end encryption. Only the
    /// label changes; the row is not rebuilt.
    pub fn mark_encryption(
        &self,
        conversation_id: &str,
        encryption: protocol::Encryption,
    ) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_encryption(&conversation, encryption)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    /// How far this user has read one conversation here, as far as the store says. It stays what
    /// they did, and a network count older than it is a lost tell of it, not the truth.
    pub fn read_up_to(&self, conversation_id: &str) -> Option<String> {
        self.hub.store().conversation(&self.key(conversation_id)).ok().flatten().and_then(|row| row.last_read)
    }

    /// The network's own mute setting, after its rules (overrides, suppress flags) are applied.
    pub fn muted(&self, conversation_id: &str, muted: bool) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_muted(&conversation, muted)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// The network's star on one conversation. On a network that serves stars, this is that
    /// star; a row that is not on the network is left as it was.
    pub fn starred(&self, conversation_id: &str, starred: bool) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_starred(&conversation, starred)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    /// Per-conversation moderation and management rights for this account.
    pub fn rights(&self, conversation_id: &str, rights: protocol::ConversationRights) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_rights(&conversation, rights)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    /// The network's disappearing-messages timer for a conversation (seconds; `None` or `Some(0)` means off).
    pub fn disappearing(&self, conversation_id: &str, seconds: Option<u32>) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_disappearing(&conversation, seconds)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    /// Where the network files one conversation: its folders, archive, pin, hidden, followed.
    /// One call for one change; never a re-read of every row.
    pub fn filing(&self, conversation_id: &str, filing: &Filing) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_filing(&conversation, filing)? {
            let space = self.hub.store().conversation(&conversation)?.and_then(|row| row.space);
            self.row_changed(conversation_id, space.as_deref());
            self.hub.changed(Change::Status);
            self.hub.changed(Change::Folders { account: self.account.clone() });
        }
        Ok(())
    }

    /// Every folder of the account, in the network's order: Telegram's chat folders, Discord's
    /// server folders. Membership of chats is on their rows (`Filing::folders`).
    pub fn folders(&self, folders: &[FolderRow]) -> Stores {
        self.hub.store().replace_folders(&self.account, folders)?;
        self.hub.changed(Change::Folders { account: self.account.clone() });
        self.hub.changed(Change::Spaces { account: self.account.clone() });
        Ok(())
    }

    /// A Space's own settings: muted as a whole, and the user's "hide muted channels".
    pub fn space_flags(&self, space_id: &str, muted: bool, hide_muted: bool) -> Stores {
        let space = self.key(space_id);
        if self.hub.store().set_space_flags(&space, muted, hide_muted)? {
            self.hub.changed(Change::Spaces { account: self.account.clone() });
            self.hub.changed(Change::Tree { space });
        }
        Ok(())
    }

    /// The newest message as a line, where the network's list gives only that (Teams'
    /// `lastMessage`). Where a whole message is at hand, record it with [`Context::history`].
    pub fn preview(&self, conversation_id: &str, line: &PreviewLine) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_preview(&conversation, line)? {
            self.hub.changed(Change::Conversation { conversation });
            if line.expires_at.is_some() {
                self.hub.expiring();
            }
        }
        Ok(())
    }

    /// The other side has read my messages up to this one (Telegram's ✓✓, Teams' "Seen").
    pub fn seen(&self, conversation_id: &str, message_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_seen(&conversation, message_id)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    /// A cloud draft the network gives one conversation, replacing the row's: the text another
    /// device left, or nothing, for one the other side cleared. Written only where it differs,
    /// so the echo of a draft this client told costs nothing.
    pub fn draft(&self, conversation_id: &str, text: &str) -> Stores {
        let conversation = self.key(conversation_id);
        let stored = self.hub.store().conversation(&conversation).ok().flatten();
        let newer = stored.as_ref().is_some_and(|row| row.draft != text).then_some(text);
        match newer {
            Some(text) => self.hub.store().set_draft(&conversation, text)?,
            None => return Ok(()),
        };
        self.hub.changed(Change::Conversation { conversation: conversation.clone() });
        // A draft holds an otherwise idle row in the Inbox, as typing nothing does.
        self.hub.changed(Change::Status);
        Ok(())
    }

    /// My messages up to this one reached the other side's device (Signal's and Google
    /// Messages' second tick).
    pub fn delivered(&self, conversation_id: &str, message_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_delivered(&conversation, message_id)? {
            self.hub.changed(Change::Conversation { conversation });
        }
        Ok(())
    }

    fn row_changed(&self, conversation_id: &str, space: Option<&str>) {
        self.hub.changed(Change::Conversation { conversation: self.key(conversation_id) });
        if let Some(space) = space {
            self.hub.changed(Change::Tree { space: self.key(space) });
        }
    }

    // Messages.

    /// Announces what recording messages did. `live` ones may also have changed the totals.
    fn stored(&self, conversation: &Key, stored: &[Stored], live: bool) {
        if stored.iter().any(|stored| *stored != Stored::Same) {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
        }
        if stored.contains(&Stored::New) {
            // A new message moves the row and changes its preview.
            self.hub.changed(Change::Conversation { conversation: conversation.clone() });
            if live {
                self.hub.changed(Change::Status);
            }
        }
    }

    /// A message heard on the socket: new, or the newer form of a stored one (an edit, a link
    /// preview). Its conversation must have been recorded. A new incoming one counts as unread;
    /// set `mentions_me` by the network's own rules, mute and suppress settings included. If it
    /// carries the `client_id` of a message we are sending, it replaces that one.
    pub fn message(&self, conversation_id: &str, message: &Message) -> Stores {
        self.live_message(conversation_id, message, true)
    }

    /// A message heard on the socket that the network's own unread count already includes,
    /// because its counts were read after the message arrived. It is stored without counting
    /// again, and announced as `message` announces a new one.
    pub fn counted_message(&self, conversation_id: &str, message: &Message) -> Stores {
        self.live_message(conversation_id, message, false)
    }

    fn live_message(&self, conversation_id: &str, message: &Message, count: bool) -> Stores {
        let conversation = self.key(conversation_id);
        let stored = self.hub.store().ingest(&conversation, message, count)?;
        if stored == Stored::New && !message.outgoing {
            self.typing_stopped(conversation_id, &message.sender.id);
            // After the row counted it, so a notification's read check sees it unread.
            self.hub.notify(crate::notify::Event::Message { conversation: conversation.clone(), message: Box::new(message.clone()) });
        }
        self.stored(&conversation, &[stored], true);
        self.carries_timer(message);
        Ok(())
    }

    /// A direct-call line. It is not a message: the list preview stays, and nothing is
    /// announced as a new conversation. `bump_unread` is a missed call we did not place.
    pub fn quiet_line(
        &self,
        conversation_id: &str,
        message: &Message,
        bump_unread: bool,
    ) -> Stores {
        let conversation = self.key(conversation_id);
        if self
            .hub
            .store()
            .insert_line(&conversation, message, bump_unread)?
        {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(())
    }

    /// A page of history, in any order. Does not count as unread: the network already said how
    /// much is. Messages already stored and unchanged cost nothing.
    pub fn history(&self, conversation_id: &str, messages: &[Message]) -> Stores {
        let conversation = self.key(conversation_id);
        let mut stored = Vec::with_capacity(messages.len());
        for message in messages {
            stored.push(self.hub.store().ingest(&conversation, message, false)?);
        }
        self.stored(&conversation, &stored, false);
        if let Some(message) = messages.iter().find(|message| message.expires_at.is_some()) {
            self.carries_timer(message);
        }
        Ok(())
    }

    /// A message with its own timer joins the retention thread's wait, so it leaves the store
    /// at its moment rather than up to a minute late.
    fn carries_timer(&self, message: &Message) {
        if message.expires_at.is_some() {
            self.hub.expiring();
        }
    }

    /// The message as stored, for networks whose updates are partial and must be merged.
    pub fn stored_message(&self, conversation_id: &str, message_id: &str) -> rusqlite::Result<Option<Message>> {
        self.hub.store().message(&self.key(conversation_id), message_id)
    }

    /// The conversation a stored message is in. `None` when this account has no such message,
    /// so a receipt for it is not filed onto some other chat.
    pub fn conversation_of_message(&self, message_id: &str) -> rusqlite::Result<Option<String>> {
        self.hub.store().conversation_of_message(&self.account, message_id)
    }

    /// The ids stored between two message ids, both included, oldest first, at most `limit`: a
    /// page from the network is compared against this to find what was deleted while away.
    pub fn stored_ids(&self, conversation_id: &str, oldest: &str, newest: &str, limit: u32) -> rusqlite::Result<Vec<String>> {
        self.hub.store().ids_between(&self.key(conversation_id), oldest, newest, limit.min(KEEP_MESSAGES))
    }

    /// A message was pinned or unpinned on the network.
    pub fn pinned(&self, conversation_id: &str, message_id: &str, pinned: bool) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().set_pinned(&conversation, message_id, pinned)? {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(())
    }

    /// Clears `media_unread` on a stored message after it is listened to / played.
    /// Returns true when the flag changed from true to false.
    pub fn mark_media_played(&self, conversation_id: &str, message_id: &str) -> rusqlite::Result<bool> {
        let conversation = self.key(conversation_id);
        let changed = self.hub.store().clear_media_unread(&conversation, message_id)?;
        if changed {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(changed)
    }

    /// Consumes one unread mention in a conversation when a message's contents are marked read.
    pub fn consume_mention(&self, conversation_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().consume_mention(&conversation)? {
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// A message is gone. One with a thread hanging off it stays as a tombstone, and replies to
    /// it say their original was deleted.
    pub fn deleted(&self, conversation_id: &str, message_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().delete_message(&conversation, message_id)? {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(())
    }

    /// A moderator removed another member's message. Kept as a tombstone with `moderated = true`
    /// and its text, attachments, and reactions cleared.
    pub fn deleted_by_moderator(&self, conversation_id: &str, message_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().delete_message_moderated(&conversation, message_id)? {
            self.hub.changed(Change::Messages { conversation });
        }
        Ok(())
    }

    /// One person added or removed one reaction. Set `reaction.mine` when it was us.
    pub fn reacted(&self, conversation_id: &str, message_id: &str, reaction: &Reaction, added: bool) -> Stores {
        self.reacted_by(conversation_id, message_id, reaction, added, None)
    }

    /// Like [`reacted`], with the reactor's display name when the network reports who reacted.
    pub fn reacted_by(
        &self,
        conversation_id: &str,
        message_id: &str,
        reaction: &Reaction,
        added: bool,
        reactor: Option<&str>,
    ) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().react(&conversation, message_id, reaction, added)? {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
            if added && !reaction.mine {
                if let Ok(Some(target)) = self.hub.store().message(&conversation, message_id) {
                    if target.outgoing {
                        let snippet = crate::store::preview_text(&target);
                        self.hub.notify(crate::notify::Event::Reaction {
                            conversation,
                            emoji: reaction.emoji.clone(),
                            reactor: reactor.map(str::to_owned),
                            snippet,
                            outgoing_target: true,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// The outcome of a `Send`: the message as the network accepted it, or why it did not go.
    pub fn sent(&self, conversation_id: &str, client_id: &str, outcome: Result<Message, String>) {
        let conversation = self.key(conversation_id);
        // A message with its own timer joins the retention thread's wait, so it leaves the
        // store at its moment rather than up to a minute late.
        let timer = matches!(&outcome, Ok(message) if message.expires_at.is_some());
        let stored = match outcome {
            Ok(mut message) => {
                message.client_id = Some(client_id.to_owned());
                self.hub.store().ingest(&conversation, &message, false)
            }
            Err(reason) => {
                let pending = self.hub.store().message(&conversation, client_id);
                match pending {
                    Ok(Some(mut message)) => {
                        message.state = SendState::Failed { reason };
                        self.hub.store().ingest(&conversation, &message, false)
                    }
                    // Discarded while it was on its way.
                    Ok(None) => Ok(Stored::Same),
                    Err(err) => Err(err),
                }
            }
        };
        match stored {
            Ok(stored) => self.stored(&conversation, &[stored], true),
            Err(err) => tracing::warn!("chatd: {}: {}", self.account.network.label(), net::redact(&err.to_string())),
        }
        if timer {
            self.hub.expiring();
        }
    }

    /// The id of the newest message stored for a conversation.
    pub fn newest(&self, conversation_id: &str) -> rusqlite::Result<Option<String>> {
        self.hub.store().newest(&self.key(conversation_id))
    }

    /// The row as stored, or `None` for a conversation never recorded.
    pub fn known(&self, conversation_id: &str) -> rusqlite::Result<Option<protocol::Conversation>> {
        self.hub.store().conversation(&self.key(conversation_id))
    }

    pub(crate) fn unread_telegram_channels(&self, after: &str, limit: u32) -> rusqlite::Result<Vec<String>> {
        self.hub.store().unread_telegram_channels(&self.account, after, limit)
    }

    /// Remembers the newest message id the network named, without a preview. "Mark as read"
    /// before history has loaded acks this id.
    pub fn note_newest(&self, conversation_id: &str, message_id: &str) -> Stores {
        self.hub.store().note_newest(&self.key(conversation_id), message_id)?;
        Ok(())
    }

    /// Drops cached messages sent by `sender_id` in `conversation_id` (and its child forum topics).
    pub fn clear_sender_messages(&self, conversation_id: &str, sender_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        let affected = self.hub.store().clear_sender_messages(&conversation, sender_id)?;
        for conv_id in affected {
            let key = self.key(&conv_id);
            self.hub.changed(Change::Messages { conversation: key.clone() });
            self.hub.changed(Change::Conversation { conversation: key });
        }
        Ok(())
    }

    /// Cached pinned topics of a forum conversation.
    pub fn pinned_topics(&self, forum_id: &str) -> rusqlite::Result<Vec<protocol::Conversation>> {
        let (rows, _) = self.hub.store().topics(&self.key(forum_id), 0, 200)?;
        Ok(rows.into_iter().filter(|c| c.pin.is_some()).collect())
    }

    /// Drops the cached messages of a conversation while keeping its row.
    pub fn clear_messages(&self, conversation_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().clear_messages(&conversation)? {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    /// Drops the cached messages and hides a direct or group conversation. The network is not
    /// told. A newer live message brings the row back.
    pub fn forget_conversation(&self, conversation_id: &str) -> Stores {
        let conversation = self.key(conversation_id);
        if self.hub.store().forget_conversation(&conversation)? {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
            self.hub.changed(Change::Conversation { conversation });
            self.hub.changed(Change::Status);
        }
        Ok(())
    }

    // People.

    /// People seen: friends, direct partners, authors, search results. Never a whole directory.
    pub fn people(&self, people: &[PersonRow]) -> Stores {
        self.hub.store().upsert_people(&self.account, people)?;
        self.hub.changed(Change::People { account: self.account.clone() });
        Ok(())
    }

    /// The whole list the user owns, by person id, as sent at connect. Record the people first.
    pub fn relations(&self, relations: &[(String, Relation)]) -> Stores {
        self.hub.store().replace_relations(&self.account, relations)?;
        self.hub.changed(Change::People { account: self.account.clone() });
        Ok(())
    }

    pub fn relation(&self, person_id: &str, relation: Relation) -> Stores {
        if self.hub.store().set_relation(&self.key(person_id), relation)? {
            self.hub.changed(Change::People { account: self.account.clone() });
        }
        Ok(())
    }

    /// What people's rows say beyond `PersonRow`: title, time zone, tag. Record the people
    /// first.
    pub fn details(&self, people: &[(String, PersonDetails)]) -> Stores {
        self.hub.store().set_details(&self.account, people)?;
        self.hub.changed(Change::People { account: self.account.clone() });
        Ok(())
    }

    /// Their status emoji and what they are doing (playing, in voice, in a meeting), kept like
    /// presence: never stored, and only for owned lists and people the shell watches.
    pub fn activity(&self, person_id: &str, emoji: &str, activity: Option<Activity>, owned: bool) {
        self.hub.set_activity(self.key(person_id), emoji, activity, owned);
    }

    /// What this account may send: file limits, pickers, options. Say it once the session
    /// tells (the plan, the workspace's settings); until then nothing is offered.
    pub fn capabilities(&self, can: Capabilities) {
        self.hub.set_capabilities(&self.account, can);
    }

    /// Selectable sending identities or lines on this account (e.g. dual-SIM cards).
    pub fn send_as(&self, options: Vec<protocol::SendAs>) {
        self.hub.set_send_as(&self.account, options);
    }

    /// How far a `SendWith`'s files have gone, in bytes of all of them. Call as often as is
    /// convenient: the shell is told at most four times a second.
    pub fn upload(&self, conversation_id: &str, client_id: &str, sent: u64, total: u64) {
        self.hub.set_upload(self.key(conversation_id), client_id, sent, total);
    }

    /// The network finished a `SearchPeople`, whether or not it found anyone. Record what it
    /// found through [`Context::people`] first.
    pub fn searched(&self, query: &str) {
        self.hub.changed(Change::Searched {
            account: self.account.clone(),
            query: query.to_owned(),
        });
    }

    /// A Tiny server's people, roles, invites or rooms changed: the shell reads them again.
    pub fn community_changed(&self) {
        self.hub.changed(Change::Tiny { account: self.account.clone() });
    }

    /// Rows of a conversation's member list were added, removed or moved.
    pub fn members_changed(&self, conversation_id: &str) {
        self.hub.changed(Change::Members { conversation: self.key(conversation_id) });
    }

    /// The info pane's cached count is no longer true.
    pub fn info_stale(&self, conversation_id: &str) {
        self.hub.infos.forget(&self.key(conversation_id));
    }

    /// What is held about a person's presence now, for a member row built here.
    pub fn presence_of(&self, person_id: &str) -> Presence {
        self.hub.presence(&self.key(person_id))
    }

    /// Never stored. `owned` marks someone on a list the user owns; anyone else is kept only
    /// while the shell is watching them, so pass on whatever the network sends. Our own presence
    /// is reported the same way, under our own id and `owned`.
    pub fn presence(&self, person_id: &str, presence: Presence, status: &str, owned: bool) {
        self.hub.set_presence(self.key(person_id), presence, status, owned);
    }

    /// Someone is typing, for `seconds` unless more is heard (the network's own timeout).
    pub fn typing(&self, conversation_id: &str, person_id: &str, name: &str, seconds: i64) {
        self.typing_as(conversation_id, person_id, name, seconds, Compose::Typing);
    }

    /// The same, naming what they are doing. [`Compose::Typing`] is the only kind most networks have.
    pub fn typing_as(&self, conversation_id: &str, person_id: &str, name: &str, seconds: i64, activity: Compose) {
        let typist = Typist {
            id: person_id.to_owned(),
            name: name.to_owned(),
            until: now() + seconds,
            activity,
        };
        self.hub.set_typing(self.key(conversation_id), typist, true);
    }

    pub fn typing_stopped(&self, conversation_id: &str, person_id: &str) {
        let typist = Typist {
            id: person_id.to_owned(),
            name: String::new(),
            until: 0,
            activity: Compose::Typing,
        };
        self.hub.set_typing(self.key(conversation_id), typist, false);
    }

    /// What the shell has on screen, across every account. Also arrives as `Command::Watch`.
    pub fn watching(&self) -> Arc<Watching> {
        self.hub.watching()
    }

    // Media.

    /// Files the messages, rows and people recorded here refer to, each with the address it is
    /// fetched from. Register before recording what refers to them. Nothing is fetched until the
    /// shell asks.
    pub fn media(&self, files: &[(MediaFile, String)]) -> Stores {
        self.hub.store().register_media(&self.account, files)
    }

    /// The same picture at other sizes: the longest side in pixels, and where that size is
    /// fetched from. Register the file first, with the address and the `width` and `height` of
    /// the size a row normally shows; then the sizes the network can also serve, at most six
    /// (for a 2x screen at least twice the normal one). `Fetch` names the size the shell draws
    /// at and chatd fetches the smallest that is large enough.
    pub fn media_variants(&self, id: &str, variants: &[(u32, String)]) -> Stores {
        self.hub.store().register_variants(&self.account, id, variants)
    }

    /// A registered file's address ran out and was exchanged for this one (while serving
    /// `Command::Download`): keep it, so the next fetch need not ask again.
    pub fn media_refreshed(&self, id: &str, url: &str) -> Stores {
        self.hub.store().media_refreshed(&self.account, id, url)
    }

    /// A file the adapter fetched over its own session, where the cache could not (Signal's
    /// and Messages' pictures), kept as the cache keeps what it fetched: written under the
    /// account's directory, registered, recorded as landed, and announced. `address` only names
    /// where it came from; nothing fetches it. Answers the file with its path.
    pub fn landed(&self, file: &MediaFile, address: &str, bytes: &[u8]) -> Option<MediaFile> {
        let media = protocol::support_dir().join("media");
        let path = crate::blobs::path_of(&media, &self.account, &file.id);
        let part = path.with_extension("part");
        std::fs::create_dir_all(crate::blobs::dir_under(&media, &self.account)).ok()?;
        std::fs::write(&part, bytes).ok()?;
        if std::fs::rename(&part, &path).is_err() {
            let _ = std::fs::remove_file(&part);
            return None;
        }
        let landed = {
            let mut store = self.hub.store();
            store.register_media(&self.account, &[(file.clone(), address.to_owned())]).ok()?;
            store.media_landed(&self.account, &file.id, &path.to_string_lossy(), bytes.len() as u64, now()).ok()??
        };
        self.hub.changed(Change::Media { account: self.account.clone(), file: landed.clone() });
        Some(landed)
    }

    /// Commits a file the adapter already streamed and decrypted onto disk at `part` (never
    /// holding large payloads in memory), moving it into the account's media directory,
    /// recording it as landed in the store, and announcing `Change::Media`.
    pub fn landed_from_part(&self, file: &MediaFile, address: &str, part: &std::path::Path, bytes: u64) -> Option<MediaFile> {
        let media = protocol::support_dir().join("media");
        let path = crate::blobs::path_of(&media, &self.account, &file.id);
        std::fs::create_dir_all(crate::blobs::dir_under(&media, &self.account)).ok()?;
        if std::fs::rename(part, &path).is_err() {
            if std::fs::copy(part, &path).is_err() {
                let _ = std::fs::remove_file(part);
                return None;
            }
            let _ = std::fs::remove_file(part);
        }
        let landed = {
            let mut store = self.hub.store();
            store.register_media(&self.account, &[(file.clone(), address.to_owned())]).ok()?;
            store.media_landed(&self.account, &file.id, &path.to_string_lossy(), bytes, now()).ok()??
        };
        self.hub.changed(Change::Media { account: self.account.clone(), file: landed.clone() });
        Some(landed)
    }

    /// Records a permanent fetch failure for an adapter-managed file (such as an expired Signal
    /// CDN attachment) and announces `Change::Media` so the shell shows the reason.
    pub fn media_failed(&self, id: &str, reason: &str) {
        let failed = self.hub.store().media_failed(&self.account, id, reason).ok().flatten();
        if let Some(file) = failed {
            self.hub.changed(Change::Media { account: self.account.clone(), file });
        }
    }

    /// A picture kept with [`Context::landed`] is the avatar of the row and of the person this
    /// id names, whichever are recorded. Rows and people the adapter records later should name
    /// it too, or they take it away again.
    pub fn avatar(&self, id: &str, avatar: &str) -> Stores {
        let (row, person) = self.hub.store().set_avatar(&self.account, id, avatar)?;
        if row {
            self.row_changed(id, None);
        }
        if person {
            self.hub.changed(Change::People { account: self.account.clone() });
        }
        Ok(())
    }

    /// The avatar the row or the person of this id names now, so a row recorded again keeps it.
    pub fn avatar_of(&self, id: &str) -> Option<String> {
        let store = self.hub.store();
        let row = store.conversation(&self.key(id)).ok().flatten().and_then(|row| row.avatar);
        row.or_else(|| store.person(&self.key(id)).ok().flatten().and_then(|person| person.avatar)).map(|file| file.id)
    }

    // Calls. Signalling only; mediad carries the media.

    /// Someone is calling us in a conversation.
    pub fn ring(&self, conversation_id: &str, from: &str) {
        self.hub.calls.ring(&self.hub, self.key(conversation_id), from);
    }

    /// The caller gave up, or another device answered.
    pub fn ring_stopped(&self, conversation_id: &str) {
        self.hub.calls.ring_stopped(&self.hub, &self.key(conversation_id));
    }

    /// Someone is calling on a network whose calls are answered on the phone (a Signal call):
    /// one notification with one action, no call here. The same silence rules as other rings.
    pub fn phone_ring(&self, conversation_id: &str, from: &str) {
        self.hub.notify(crate::notify::Event::PhoneRing { conversation: self.key(conversation_id), from: from.to_owned() });
    }

    /// That call is over; its notification and its sound go.
    pub fn phone_ring_stopped(&self, conversation_id: &str) {
        self.hub.notify(crate::notify::Event::PhoneRingStopped(self.key(conversation_id)));
    }

    /// We are ringing the other side of a direct call and nobody has answered yet.
    pub fn ringing_out(&self, conversation_id: &str) {
        self.hub.calls.set_state(&self.hub, &self.key(conversation_id), CallState::RingingOut);
    }

    /// The other side of a call we rang picked up. Until this, a ring we placed is never shown
    /// as a call, whatever our own media is doing.
    pub fn answered(&self, conversation_id: &str) {
        self.hub.calls.answered(&self.hub, &self.key(conversation_id));
    }

    /// The network handed out the voice session for the call the user started, or a new one for
    /// a call that is already up (the voice server moved): mediad is told to join, or to rejoin.
    pub fn voice(&self, conversation_id: &str, voice: VoiceJoin) {
        self.hub.calls.join(&self.hub, &self.key(conversation_id), voice);
    }

    /// Everyone in the call but us, with their mute and deafen as the network shows them.
    pub fn participants(&self, conversation_id: &str, participants: Vec<Participant>) {
        self.hub.calls.set_participants(&self.hub, &self.key(conversation_id), participants);
    }

    /// Server-side moderation on our own voice state (server mute, server deafen, suppressed).
    pub fn self_moderation(&self, conversation_id: &str, server_muted: bool, server_deafened: bool, suppressed: bool) {
        self.hub.calls.set_self_moderation(&self.hub, &self.key(conversation_id), server_muted, server_deafened, suppressed);
    }

    /// A Tiny server call: mediad's offer for this room, with mediad started for it first. A
    /// rejoin of the room the call is in keeps the call.
    pub fn tiny_offer(&self, conversation_id: &str, offer: protocol::TinyOffer) -> Result<String, String> {
        self.hub.calls.tiny_offer(&self.hub, &self.key(conversation_id), offer)
    }

    pub fn tiny_direct_offer(&self, conversation_id: &str, offer: protocol::TinyOffer, intent: u64) -> Result<String, String> {
        self.hub.calls.tiny_direct_offer(&self.hub, &self.key(conversation_id), offer, intent)
    }

    /// The server's answer to that offer: mediad connects with it.
    pub fn tiny_answer(&self, conversation_id: &str, sdp: String) {
        self.hub.calls.tiny_answer(&self.hub, &self.key(conversation_id), sdp);
    }

    /// Whose voice each receive slot carries.
    pub fn tiny_slots(&self, conversation_id: &str, audio: Vec<protocol::TinySlot>) {
        self.hub.calls.tiny_slots(&self.hub, &self.key(conversation_id), audio);
    }

    /// The call in this room is over and the network already knows. A joined call plays the
    /// leave chime, then mediad goes.
    pub fn call_over(&self, conversation_id: &str) -> bool {
        self.hub.calls.end_if(&self.hub, &self.key(conversation_id))
    }

    /// A Tiny server moved us from one room to another: the call there, its mediad, devices,
    /// mute and deafen, now belong to the new room. False when no call was in `from`.
    pub fn tiny_moved(&self, from: &str, to: &str) -> bool {
        self.hub.calls.retarget(&self.hub, &self.key(from), self.key(to))
    }

    /// A Tiny room's bitrate and whether it requires push-to-talk, for the call in it.
    pub fn tiny_room(&self, conversation_id: &str, kbps: u32, push_to_talk: bool) {
        if self.in_call(conversation_id) {
            self.hub.calls.tiny_room(&self.hub, &self.key(conversation_id), kbps, push_to_talk);
        }
    }

    /// Tells the call's mediad, if there is one: who holds priority speech.
    pub fn tell_media(&self, conversation_id: &str, request: protocol::MediaRequest) {
        if self.in_call(conversation_id) {
            if let protocol::MediaRequest::TinyPicture { ref picture } = request {
                let source = picture.as_ref().map(|p| (p.user_id.clone(), p.kind));
                self.hub.calls.set_picture_source(&self.hub, &self.key(conversation_id), source);
            }
            let _ = self.hub.calls.request(&self.hub, &request);
        }
    }

    /// The call is up for this conversation, with its mediad.
    pub fn in_call(&self, conversation_id: &str) -> bool {
        let (call, running) = self.hub.calls.describe();
        running && call.is_some_and(|call| call.conversation == self.key(conversation_id))
    }

    pub fn call_failed(&self, conversation_id: &str, reason: CallFailure, detail: &str) {
        let failed = CallState::Failed {
            reason,
            detail: detail.to_owned(),
        };
        self.hub.calls.set_state(&self.hub, &self.key(conversation_id), failed);
    }

    // Notes.

    /// Something small the adapter looked up once and should not ask the network for again.
    pub fn note(&self, key: &str) -> Option<String> {
        self.hub.store().name(&self.account, key).ok().flatten()
    }

    pub fn set_note(&self, key: &str, value: &str) {
        let _ = self.hub.store().set_name(&self.account, key, value);
    }

    /// Marks a cached media file as evicted so the next fetch downloads a fresh copy (e.g. a
    /// Teams profile picture whose URL never changes when the photo is updated).
    pub fn media_evict(&self, id: &str) {
        let _ = self.hub.store().media_evicted(&self.account, id);
    }
}

/// A running adapter. Dropping it disconnects.
pub struct Adapter {
    commands: Sender<Command>,
    stop: Arc<Halt>,
}

impl Adapter {
    pub fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The adapter's inbox, for a thread that asks on its own (the media cache).
    pub fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.stop.stopped.store(true, Ordering::Relaxed);
        drop(self.stop.waiting.lock());
        self.stop.wake.notify_all();
        let _ = self.commands.send(Command::Stop);
    }
}

/// What the account says when tinyclient itself failed on it, and stopped trying.
pub const BROKEN: &str = "Something went wrong in tinyclient with this account. Quit and reopen tinyclient to try again.";
/// What the account says while it recovers from tinyclient's own failure.
pub const RECOVERING: &str = "Something went wrong in tinyclient with this account. Trying again.";
/// The pause before an adapter thread that panicked runs again.
const PANIC_PAUSE: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(30) };
const PANIC_PAUSE_COMMANDS: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(5) };

impl Context {
    /// A panic was caught on one of this adapter's threads. Answers true when it is the first
    /// and the thread should run again after its pause; the second stops the adapter, and the
    /// account is offline with [`BROKEN`] until the user restarts tinyclient.
    fn panicked(&self, thread: &str) -> bool {
        let count = self.stop.panics.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::warn!("chatd: {}: the {thread} thread panicked ({count})", self.account.network.label());
        self.hub.forget_live(&self.account);
        if count > 1 {
            self.stop.stopped.store(true, Ordering::Relaxed);
            drop(self.stop.waiting.lock());
            self.stop.wake.notify_all();
            let _ = self.commands.send(Command::Stop);
            self.hub.set_link(&self.account, Link::Offline, BROKEN);
            return false;
        }
        true
    }
}

/// Counts recent drops of connections that had held, so a link that drops every minute backs
/// off as one that never held does, rather than signing on again every minute for ever. The
/// count halves every [`DROPS_HALF_LIFE`].
#[derive(Debug, Default)]
struct Drops {
    count: f64,
    last: Option<Instant>,
}

const DROPS_HALF_LIFE: Duration = Duration::from_secs(600);

impl Drops {
    /// One more drop at `now`: the pause before the next attempt. `held` says the connection
    /// was up longer than a minute; `pause` is the last pause, which a connection that never
    /// held doubles.
    fn next(&mut self, now: Instant, held: bool, pause: Duration) -> Duration {
        let since = self.last.map_or(Duration::MAX, |last| now.saturating_duration_since(last));
        let decay = 0.5f64.powf(since.as_secs_f64() / DROPS_HALF_LIFE.as_secs_f64());
        self.count = self.count * decay + 1.;
        self.last = Some(now);
        if !held {
            return (pause * 2).min(RETRY_CEILING);
        }
        // The first drop in a while retries in a second; each recent one doubles it.
        let doublings = (self.count.floor() as u32).saturating_sub(1).min(16);
        Duration::from_secs(1u64 << doublings).min(RETRY_CEILING)
    }
}

/// Keeps one connection up: reconnects with a growing pause, and gives up only when the network
/// says the session is no longer valid. Then the account is `Expired`: its session and cache
/// stay, so what was read stays readable and signing in again replaces the session.
///
/// A panic inside `connect` is caught here, at the thread's edge: the account goes offline
/// with [`RECOVERING`] and connects again after [`PANIC_PAUSE`]; a second panic on this
/// adapter stops it with [`BROKEN`].
fn keep_connected(ctx: &Context, connect: impl FnMut() -> Failure) {
    keep_connected_with_backoff(ctx, connect, Duration::from_secs(1));
}

fn keep_connected_with_backoff(ctx: &Context, mut connect: impl FnMut() -> Failure, mut pause: Duration) {
    let mut drops = Drops::default();
    let mut attempt = 0u64;
    while !ctx.stopped() {
        let generation = ctx.stop.recovery.load(Ordering::Relaxed);
        let began = Instant::now();
        let failure = match catch_unwind(AssertUnwindSafe(&mut connect)) {
            Ok(failure) => failure,
            Err(_) => {
                if !ctx.panicked("connection") {
                    return;
                }
                ctx.hub.set_link(&ctx.account, Link::Offline, RECOVERING);
                if ctx.pause(PANIC_PAUSE) {
                    return;
                }
                continue;
            }
        };
        if ctx.stopped() {
            return;
        }
        match failure {
            Failure::SignedOut(why) => {
                tracing::warn!("chatd: {}: signed out: {}", ctx.account.network.label(), net::redact(&why));
                ctx.hub.forget_all_live(&ctx.account);
                ctx.hub.set_link(&ctx.account, Link::Expired, &why);
                return;
            }
            Failure::Dropped(why) => {
                tracing::warn!("chatd: {}: {}", ctx.account.network.label(), net::redact(&why));
            }
        }
        // A connection that held for a while earns a quick retry, unless it has been dropping
        // often. One that never holds backs off to a quarter of an hour: unattended, that is
        // under a hundred sign-ons a day, not 1,400.
        pause = drops.next(Instant::now(), began.elapsed() > Duration::from_secs(60), pause);
        attempt = attempt.saturating_add(1);
        tracing::info!(
            network = ctx.account.network.label(),
            attempt,
            pause_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
            "reconnect scheduled"
        );
        ctx.hub.forget_live(&ctx.account);
        ctx.hub.set_offline(&ctx.account, pause <= RECONNECTING);
        // Not on the second: many clients retrying in step is what a network watches for.
        let jitter = pause.mul_f64(0.25 * f64::from(now().unsigned_abs() as u32 % 1000) / 1000.);
        match ctx.reconnect_pause(pause + jitter, generation) {
            ReconnectWait::Stopped => return,
            ReconnectWait::Elapsed => {},
            ReconnectWait::Recovered => {
                // A changed route or resume is a reason to try, not proof that
                // the remote service is reachable. Failures still back off.
                drops = Drops::default();
                pause = Duration::from_secs(1);
                ctx.hub.set_offline(&ctx.account, true);
                tracing::warn!("chatd: {}: network changed or system resumed; retrying connection", ctx.account.network.label());
                // Consume bursts on the next iteration and never attempt more
                // often than once per second, even if hints keep arriving.
                if ctx.pause(Duration::from_secs(1).saturating_sub(began.elapsed())) { return; }
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReconnectWait { Elapsed, Recovered, Stopped }

/// A retry this close is "reconnecting"; one further off is plain offline.
const RECONNECTING: Duration = Duration::from_secs(30);

/// The longest pause between attempts to connect.
const RETRY_CEILING: Duration = Duration::from_secs(900);

/// How long a fetched history is trusted. The socket keeps it current in between.
const FRESH: Duration = Duration::from_secs(60);

/// Runs an adapter's commands until it is stopped. What the user is waiting on goes first.
/// `pace` is the pause after a history fetch, to stay inside the network's rate limit.
///
/// With nothing queued this waits on the inbox and nothing else: no timer, so an idle account
/// costs no wakeups. Dropping the [`Adapter`] sends `Command::Stop`, which ends it.
///
/// A panic while serving one command is caught: that command's reply is dropped (its asker
/// hears it failed), and the thread serves on after a short pause. A second panic on this
/// adapter stops it, and the account says why ([`Context::panicked`]).
pub(crate) fn serve_commands(
    ctx: &Context,
    inbox: Receiver<Command>,
    pace: Duration,
    mut backfill: impl FnMut(&str),
    mut other: impl FnMut(Command),
) {
    let mut urgent = VecDeque::new();
    let mut warm = VecDeque::new();
    let mut fetched: HashMap<String, Instant> = HashMap::new();
    while !ctx.stopped() {
        if urgent.is_empty() && warm.is_empty() {
            match inbox.recv() {
                Ok(Command::Stop) | Err(_) => return,
                Ok(Command::Warm(id)) => warm.push_back(id),
                Ok(command) => urgent.push_back(command),
            }
        }
        for command in inbox.try_iter() {
            match command {
                Command::Stop => return,
                Command::Warm(id) => warm.push_back(id),
                command => urgent.push_back(command),
            }
        }
        let id = match urgent.pop_front() {
            Some(Command::Backfill(id)) => id,
            Some(command) => {
                if catch_unwind(AssertUnwindSafe(|| other(command))).is_err() && (!ctx.panicked("command") || ctx.pause(PANIC_PAUSE_COMMANDS)) {
                    return;
                }
                continue;
            }
            None => match warm.pop_front() {
                Some(id) => id,
                None => continue,
            },
        };
        if fetched.get(&id).is_some_and(|at| at.elapsed() < FRESH) {
            continue;
        }
        if catch_unwind(AssertUnwindSafe(|| backfill(&id))).is_err() && (!ctx.panicked("command") || ctx.pause(PANIC_PAUSE_COMMANDS)) {
            return;
        }
        fetched.insert(id, Instant::now());
        if ctx.pause(pace) {
            return;
        }
    }
}

/// One account found in what a sign-in produced.
pub struct Signed {
    /// Stable across restarts and safe as a file name: letters, digits, `.`, `_` and `-`.
    pub account: String,
    /// Shown until the adapter connects and calls [`Context::describe`].
    pub name: String,
    /// What `start` is given for this account.
    pub session: String,
}

/// True for the networks `start` can run.
pub fn supported(network: Network) -> bool {
    match network {
        Network::Slack | Network::Discord | Network::Teams => true,
        Network::Telegram => telegram::supported(),
        Network::Signal => signal::supported(),
        Network::Messages => messages::supported(),
        Network::Tiny => tiny::supported(),
    }
}

/// An account id names a directory of the media cache, so it must be one plain file name:
/// letters, digits, `.`, `_` and `-`, and neither of the two names that mean a directory.
fn safe(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// The accounts inside one network's stored session. One Slack sign-in holds a workspace each.
pub fn accounts(network: Network, session: &str) -> Result<Vec<Signed>, String> {
    let found = match network {
        Network::Slack => slack::accounts(session)?,
        Network::Discord => discord::accounts(session)?,
        Network::Telegram => telegram::accounts(session)?,
        Network::Teams => teams::accounts(session)?,
        Network::Signal => signal::accounts(session)?,
        Network::Messages => messages::accounts(session)?,
        Network::Tiny => tiny::accounts(session)?,
    };
    match found.iter().find(|signed| !safe(&signed.account)) {
        Some(bad) => Err(format!("{}: unusable account id {:?}", network.label(), bad.account)),
        None => Ok(found),
    }
}

/// The stored session with one account taken out, for signing out of that account alone. `None`
/// when nothing would be left, and the keychain item goes.
pub fn without(network: Network, session: &str, account: &str) -> Option<String> {
    match network {
        Network::Slack => slack::without(session, account),
        Network::Discord => discord::without(session, account),
        Network::Telegram => telegram::without(session, account),
        Network::Teams => teams::without(session, account),
        Network::Signal => signal::without(session, account),
        Network::Messages => messages::without(session, account),
        Network::Tiny => tiny::without(session, account),
    }
}

/// Starts the adapter for an account from its part of the session.
pub fn start(account: AccountKey, session: &str, hub: Arc<Hub>) -> Result<Adapter, String> {
    let stop = Arc::new(Halt::default());
    recovery::watch(&stop);
    let (commands, inbox): (Sender<Command>, Receiver<Command>) = channel();
    let ctx = Context {
        account: account.clone(),
        hub,
        commands: commands.clone(),
        stop: stop.clone(),
    };
    // Offline until the adapter says more, and said before its threads start: one that connects
    // at once (the scripted wires do) would otherwise have its Connected overwritten for good.
    ctx.link(Link::Offline);
    let started = match account.network {
        Network::Slack => slack::start(ctx.clone(), session, inbox),
        Network::Discord => discord::start(ctx.clone(), session, inbox),
        Network::Telegram => telegram::start(ctx.clone(), session, inbox),
        Network::Teams => teams::start(ctx.clone(), session, inbox),
        Network::Signal => signal::start(ctx.clone(), session, inbox),
        Network::Messages => messages::start(ctx.clone(), session, inbox),
        Network::Tiny => tiny::start(ctx.clone(), session, inbox),
    };
    if let Err(why) = started {
        // Stay Offline with the reason. SignedOut made shows() false, so messages() answered
        // an empty page and the open pane cleared even though the cache was still on disk.
        ctx.hub.set_link(&ctx.account, Link::Offline, &why);
        return Err(why);
    }
    Ok(Adapter { commands, stop })
}

/// The lab's teams-markdown driver. See `teams::prove_teams_markdown`.
pub fn prove_teams_markdown() -> Result<(), String> {
    teams::prove_teams_markdown()
}

/// The lab's slack-custom-emoji driver. A custom emoji is a picture, and renaming it keeps
/// that picture under the new name.
pub fn prove_slack_custom_emoji() -> Result<(), String> {
    slack::prove_custom_emoji()
}

/// The lab's slack-profile-fields driver. A labelled profile keeps both names, the department,
/// two custom fields and a manager link.
pub fn prove_slack_profile_fields() -> Result<(), String> {
    slack::prove_profile_fields()
}

/// The lab's slack-star-channel driver. Starring a Slack conversation is `stars.add` on the
/// channel, a `star_removed` event clears it, and a message star is not the channel's.
pub fn prove_slack_channel_star() -> Result<(), String> {
    slack::prove_channel_star()
}

/// The lab's slack-call-cards driver. A Zoom, Meet, Teams or Webex call block is a card
/// with a Join link, and an old Slack call says that it happened.
pub fn prove_slack_call_cards() -> Result<(), String> {
    slack::prove_call_cards()
}

/// The lab's discord-thread-chip driver. A thread chip counts new replies and keeps the
/// newest faces. A build without this returns to the server and stays up.
pub fn prove_discord_thread_chip() -> Result<(), String> {
    discord::prove_thread_chip()
}

/// The lab's discord-code-colors driver. A fenced block is coloured from its language.
/// A build without this returns to the server and stays up.
pub fn prove_discord_code_colors() -> Result<(), String> {
    discord::prove_code_colors()
}

/// The lab's teams-chain-paging driver. See `teams::prove_chain_paging`.
pub fn prove_chain_paging() -> Result<(), String> {
    teams::prove_chain_paging()
}

/// The lab's teams-copy-link driver. See `teams::prove_share_links`.
pub fn prove_share_links() -> Result<(), String> {
    teams::prove_share_links()
}

/// The lab's failed-start-offline driver. A start that fails leaves the account offline,
/// so messages() still answers from the cache instead of an empty signed-out page.
pub fn prove_failed_start_offline() -> Result<(), String> {
    let hub = Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = AccountKey {
        network: Network::Tiny,
        account: "11111111-1111-1111-1111-111111111111".into(),
    };
    if start(account.clone(), "not-json", hub.clone()).is_ok() {
        return Err("a broken session started".to_owned());
    }
    match hub.link(&account) {
        Link::Offline => Ok(()),
        other => Err(format!("link was {other:?}, not Offline")),
    }
}

/// The lab's teams-reactions driver. See `teams::prove_reactions`.
pub fn prove_teams_reactions() -> Result<(), String> {
    teams::prove_reactions()
}

/// The lab's reaction-reactors driver. A reaction keeps who the network named with it: events
/// fold into the list one person at a time, and a taking-back lets that person go. Telegram's
/// half is in `telegram::prove_telegram_reactors`.
pub fn prove_reactors() -> Result<(), String> {
    let mut store = crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?;
    let account = AccountKey { network: Network::Slack, account: "T1".to_owned() };
    let conversation = Key::new(&account, "c1");
    let said = Message::plain("m1", "Ada", "hello", 1_700_000_000, false);
    store.ingest(&conversation, &said, false).map_err(|err| err.to_string())?;
    let by = |id: &str| protocol::Reaction { emoji: "👍".into(), file: None, count: 1, mine: false, who: vec![id.to_owned()] };
    store.react(&conversation, "m1", &by("u-ana"), true).map_err(|err| err.to_string())?;
    store.react(&conversation, "m1", &by("u-bea"), true).map_err(|err| err.to_string())?;
    let who = |store: &crate::store::Store| -> Result<Vec<String>, String> {
        Ok(store
            .message(&conversation, "m1")
            .map_err(|err| err.to_string())?
            .ok_or("the message is gone")?
            .reactions
            .into_iter()
            .next()
            .ok_or("the reaction is gone")?
            .who)
    };
    if who(&store)? != ["u-ana", "u-bea"] {
        return Err(format!("the reactions the events named were not kept: {:?}", who(&store)?));
    }
    store.react(&conversation, "m1", &by("u-ana"), false).map_err(|err| err.to_string())?;
    if who(&store)? != ["u-bea"] {
        return Err(format!("taking a reaction back did not let its person go: {:?}", who(&store)?));
    }
    telegram::prove_telegram_reactors()
}

/// The lab's teams-file-download driver. See `teams::prove_shared_file`.
pub fn prove_teams_shared_file() -> Result<(), String> {
    teams::prove_shared_file()
}

/// The lab's teams-loop-component driver. See `teams::prove_loop_component`.
pub fn prove_teams_loop_component() -> Result<(), String> {
    teams::prove_loop_component()
}

/// The lab's slack-connect-org driver. See `slack::prove_connect_names`.
pub fn prove_slack_connect_org() -> Result<(), String> {
    slack::prove_connect_names()
}

/// The lab's slack-rich-text driver. See `slack::prove_rich_text`.
pub fn prove_slack_rich_text() -> Result<(), String> {
    slack::prove_rich_text()
}

/// The lab's teams-team-info driver. See `teams::prove_team_info`.
pub fn prove_teams_team_info() -> Result<(), String> {
    teams::prove_team_info()
}

/// An adapter with no network behind it, for whatever serves an account inside chatd (the
/// demo). The caller runs `serve_commands` on the inbox with the context.
pub(crate) fn local(account: AccountKey, hub: Arc<Hub>) -> (Adapter, Context, Receiver<Command>) {
    let stop = Arc::new(Halt::default());
    let (commands, inbox) = channel();
    let ctx = Context {
        account,
        hub,
        commands: commands.clone(),
        stop: stop.clone(),
    };
    (Adapter { commands, stop }, ctx, inbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn recovery_interrupts_a_fifteen_minute_reconnect_wait_without_rapid_attempts() {
        let hub = hub();
        let account = AccountKey { network: Network::Signal, account: "recovery".into() };
        let (_adapter, ctx, _inbox) = local(account.clone(), hub.clone());
        ctx.link(Link::Connected);
        let (attempted, attempts) = channel();
        let thread_ctx = ctx.clone();
        let thread = std::thread::spawn(move || {
            let mut count = 0;
            keep_connected_with_backoff(&thread_ctx, || {
                count += 1;
                attempted.send(Instant::now()).unwrap();
                if count == 1 { Failure::Dropped("offline".into()) }
                else { Failure::SignedOut("test finished".into()) }
            }, Duration::from_secs(900));
        });
        let first = attempts.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while linked(&hub, &account).0 != Link::Offline {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..100 { recovery::hint(&ctx.stop); }
        let next = attempts.recv_timeout(Duration::from_secs(2)).expect("recovery must not wait out the 900 s delay");
        assert!(next.duration_since(first) >= Duration::from_secs(1), "hints cannot create a rapid retry loop");
        thread.join().unwrap();
        assert_eq!(linked(&hub, &account).0, Link::Expired);
        recovery::hint(&ctx.stop);
        assert!(attempts.try_recv().is_err(), "an expired session is never retried by a hint");
    }

    #[test]
    fn recovery_during_a_failed_connect_is_not_lost_before_the_wait() {
        let (_adapter, ctx, _inbox) = local(AccountKey { network: Network::Signal, account: "during".into() }, hub());
        let generation = ctx.stop.recovery.load(Ordering::Relaxed);
        recovery::hint(&ctx.stop);
        assert_eq!(ctx.reconnect_pause(Duration::from_secs(900), generation), ReconnectWait::Recovered);
        let generation = ctx.stop.recovery.load(Ordering::Relaxed);
        assert_eq!(ctx.reconnect_pause(Duration::ZERO, generation), ReconnectWait::Elapsed, "a consumed hint does not wake every later wait");
    }

    #[test]
    fn stopping_an_adapter_interrupts_its_reconnect_wait() {
        let (adapter, ctx, _inbox) = local(AccountKey { network: Network::Signal, account: "stop".into() }, hub());
        let thread = std::thread::spawn(move || ctx.reconnect_pause(Duration::from_secs(900), 0));
        drop(adapter);
        assert_eq!(thread.join().unwrap(), ReconnectWait::Stopped);
    }

    #[test]
    fn recovery_hints_do_not_bypass_http_or_command_pacing() {
        let (_adapter, ctx, _inbox) = local(AccountKey { network: Network::Signal, account: "pacing".into() }, hub());
        let thread_ctx = ctx.clone();
        let (started, waiting) = channel();
        let thread = std::thread::spawn(move || {
            let start = Instant::now();
            started.send(()).unwrap();
            assert!(!thread_ctx.pause(Duration::from_millis(150)));
            start.elapsed()
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        recovery::hint(&ctx.stop);
        // Timed inside the pause, so a slow runner cannot make it look early.
        assert!(thread.join().unwrap() >= Duration::from_millis(150), "a hint does not cut a pacing pause short");
    }

    fn hub() -> Arc<Hub> {
        Arc::new(Hub::new(Store::open(std::path::Path::new(":memory:")).expect("a store in memory")))
    }

    fn linked(hub: &Hub, account: &AccountKey) -> (Link, String) {
        let links = hub.links();
        let linked = links.iter().find(|linked| linked.account == *account).expect("the account is linked");
        (linked.link, linked.detail.clone())
    }

    #[test]
    fn a_failed_start_keeps_the_account_offline_so_cached_messages_still_show() {
        let hub = hub();
        let account = AccountKey {
            network: Network::Tiny,
            account: "11111111-1111-1111-1111-111111111111".into(),
        };
        let err = super::start(account.clone(), "not-json", hub.clone());
        assert!(err.is_err(), "a broken session must not start");
        let (link, detail) = linked(&hub, &account);
        assert_eq!(link, Link::Offline, "SignedOut would hide the cache and clear the pane");
        assert!(!detail.is_empty(), "the start error is kept on the account");
        assert_eq!(super::prove_failed_start_offline(), Ok(()));
    }

    #[test]
    fn a_link_that_drops_every_minute_backs_off_and_one_drop_after_a_quiet_hour_does_not() {
        let t0 = Instant::now();
        let minute = Duration::from_secs(60);
        let mut drops = Drops::default();
        let mut pause = Duration::from_secs(1);
        let mut pauses = Vec::new();
        for n in 0..12 {
            pause = drops.next(t0 + minute * n, true, pause);
            pauses.push(pause.as_secs());
        }
        assert_eq!(pauses[..2], [1, 1], "the first drops retry at once");
        assert!(pauses.windows(2).all(|pair| pair[1] >= pair[0]), "{pauses:?}");
        assert!(pauses[11] >= 120, "after twelve drops in twelve minutes it waits minutes: {pauses:?}");
        // An hour of quiet: the count has decayed, and a drop retries in a second again.
        let quiet = drops.next(t0 + minute * 80, true, pause);
        assert!(quiet <= Duration::from_secs(2), "{quiet:?}");
        // One that never held doubles, up to the ceiling, whatever the count.
        assert_eq!(drops.next(t0 + minute * 81, false, Duration::from_secs(8)), Duration::from_secs(16));
        assert_eq!(drops.next(t0 + minute * 82, false, RETRY_CEILING), RETRY_CEILING);
    }

    #[test]
    fn a_panicking_command_is_recovered_from_once_and_the_second_stops_the_account_with_words() {
        let hub = hub();
        let account = AccountKey { network: Network::Slack, account: "T1".into() };
        let (adapter, ctx, inbox) = local(account.clone(), hub.clone());
        ctx.link(Link::Connected);
        let served = Arc::new(Mutex::new(Vec::new()));
        let seen = served.clone();
        let thread_ctx = ctx.clone();
        let thread = std::thread::spawn(move || {
            serve_commands(&thread_ctx, inbox, Duration::ZERO, |id| {
                if id.starts_with("bad") {
                    panic!("a bug in the adapter");
                }
                lock(&seen).push(id.to_owned());
            }, |_| {})
        });
        adapter.command(Command::Backfill("bad1".into()));
        adapter.command(Command::Backfill("good".into()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&served).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*lock(&served), ["good"], "the thread served on after the first panic");
        assert_eq!(linked(&hub, &account), (Link::Connected, String::new()), "one recovered panic leaves the link alone");
        adapter.command(Command::Backfill("bad2".into()));
        thread.join().expect("the panic was caught, not carried out of the thread");
        assert!(ctx.stopped());
        assert_eq!(linked(&hub, &account), (Link::Offline, BROKEN.to_owned()));
    }

    #[test]
    fn a_panicking_connection_says_why_and_is_connected_again_once() {
        let hub = hub();
        let account = AccountKey { network: Network::Discord, account: "1".into() };
        let (_adapter, ctx, _inbox) = local(account.clone(), hub.clone());
        let mut attempts = 0;
        let mut said = None;
        let watch = (hub.clone(), account.clone());
        keep_connected(&ctx, || {
            attempts += 1;
            if attempts == 1 {
                panic!("a bug in the socket thread");
            }
            said = Some(linked(&watch.0, &watch.1));
            Failure::SignedOut("done".into())
        });
        assert_eq!(attempts, 2, "restarted once");
        assert_eq!(said, Some((Link::Offline, RECOVERING.to_owned())), "while it recovered the account said why");
        assert_eq!(linked(&hub, &account).0, Link::Expired);
    }
}
