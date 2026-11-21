//! The seam between the adapter and the Signal library. Everything above it speaks the contract;
//! everything below it is Signal: keys, sessions, sealed sender, the socket. `real` is presage
//! (built with the `signal` feature); `fake` is a script, so the adapter can be run through
//! chatd's socket with no Signal behind it and no Signal library linked.
//!
//! Signal names a message by who wrote it and the millisecond they sent it, and has no list of
//! conversations or history on its servers: what arrives is all there is.

use super::style::Range;
use protocol::Span;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// Where a message goes or came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Peer {
    /// A person, by their account id (ACI).
    Direct(String),
    /// A group, by its public id in hex. The group's master key never leaves the wire.
    Group(String),
}

const GROUP: &str = "g.";

impl Peer {
    /// The conversation id the contract knows it by.
    pub fn conversation(&self) -> String {
        match self {
            Peer::Direct(aci) => aci.clone(),
            Peer::Group(id) => format!("{GROUP}{id}"),
        }
    }

    pub fn parse(conversation: &str) -> Option<Peer> {
        match conversation.strip_prefix(GROUP) {
            Some(id) if !id.is_empty() && id.bytes().all(|c| c.is_ascii_hexdigit()) => Some(Peer::Group(id.to_owned())),
            Some(_) => None,
            None if is_aci(conversation) => Some(Peer::Direct(conversation.to_owned())),
            None => None,
        }
    }
}

/// An account id as Signal writes one: a UUID in lower case.
pub fn is_aci(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, c)| if matches!(at, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() && !c.is_ascii_uppercase() })
}

/// The id of a message in the contract: when it was sent, in milliseconds, and who wrote it.
pub fn message_id(sent: u64, author: &str) -> String {
    format!("{sent}-{author}")
}

/// The other way: `None` for an id this adapter did not make.
pub fn parse_message_id(id: &str) -> Option<(u64, String)> {
    let (sent, author) = id.split_once('-')?;
    let sent = sent.parse().ok()?;
    is_aci(author).then(|| (sent, author.to_owned()))
}

/// Someone in the user's contacts, as the phone shares them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contact {
    pub id: String,
    pub name: String,
    /// In international form; empty when the phone did not share it.
    pub phone: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupInfo {
    /// Public id, hex.
    pub id: String,
    pub title: String,
    pub description: String,
    pub members: u32,
    /// Whether this account is currently a member of the group.
    pub member: bool,
    /// Account ids of the members, including us. Empty when the wire has not said who.
    pub member_ids: Vec<String>,
    /// Account ids of group administrators.
    pub admins: Vec<String>,
    /// Disappearing-message timer in seconds. 0 means off.
    pub timer_secs: u32,
}

/// Disappearing-message timer carried on a conversation or outgoing message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timer {
    pub seconds: u32,
    pub version: u32,
}

/// Per-conversation sync state from Signal's storage service (contact or group record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatState {
    pub peer: Peer,
    /// Milliseconds since 1970 until which the chat is muted. 0 = not muted; >= i64::MAX as u64 = forever.
    pub muted_until: u64,
    pub dont_notify_for_mentions_if_muted: bool,
    pub archived: bool,
    pub marked_unread: bool,
    pub blocked: bool,
    pub whitelisted: bool,
}

/// One Signal chat folder from the storage service (`ChatFolderRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatFolder {
    pub id: String,
    pub name: String,
    pub position: u32,
    pub show_only_unread: bool,
    pub show_muted: bool,
    pub include_all_individual: bool,
    pub include_all_groups: bool,
    pub included: Vec<Peer>,
    pub excluded: Vec<Peer>,
}

/// Response to a Signal message request (`SyncMessage.MessageRequestResponse.Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRequestAction {
    Accept,
    Delete,
    Block,
    BlockAndDelete,
    Spam,
    BlockAndSpam,
}

/// The earlier message a message answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quote {
    pub sent: u64,
    pub author: String,
    pub text: String,
    /// Styles of `text` after any mention in the quote was substituted.
    pub spans: Vec<Span>,
}

/// One message's words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub author: String,
    /// The author's name as the user knows them: their contact name, else their profile's.
    pub name: String,
    /// Milliseconds since 1970: Signal's own id for the message.
    pub sent: u64,
    /// Mentions already read as `@Name`.
    pub text: String,
    pub quote: Option<Quote>,
    pub mentions_me: bool,
    /// Styles of `text`, byte offsets, after mention substitution.
    pub spans: Vec<Span>,
    /// What it carries that is not text and is not drawn here ("Photo", "Sticker"), in words.
    pub unsupported: Option<String>,
    /// Disappearing-message timer in seconds carried on this message. 0 means off.
    pub expire_timer: u32,
    /// Timer version carried on this message. 0 when unset.
    pub expire_timer_version: u32,
    /// Shared contact cards attached to this message.
    pub contacts: Vec<protocol::SharedContact>,
}

/// Longest payment note shown in its label: what a sender typed joins the words after an em dash.
/// The reply snippet in `signal.rs` keeps the same room.
const NOTE_LIMIT: usize = 200;

/// What a payment says (#733): where it is acted on, and the note the sender wrote, when one came.
/// The amount itself rides inside an encrypted receipt only a wallet can read, so nothing of it is
/// shown. The real wire and the fake one say the same words, so a scripted test exercises what a
/// person is told.
pub fn payment_label(note: Option<&str>) -> String {
    let mut label = "Payment · open Signal on your phone".to_owned();
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        label.push_str(" — ");
        label.push_str(&note.chars().take(NOTE_LIMIT).collect::<String>());
    }
    label
}

/// What a gift badge says (#733): redeeming it presents a receipt credential to Signal's server,
/// and that is the phone's work.
pub fn gift_badge_label() -> &'static str {
    "Gift badge · open Signal on your phone to redeem"
}

/// What a view-once photo or video says while its media cannot be opened from here (#748): what
/// it is, and where it opens. The flag goes with an attachment, and the attachment is not a
/// "Photo" that opens to nothing. The real wire and the fake one say the same words, so a
/// scripted test exercises what a person is told.
pub fn view_once_label(content_type: Option<&str>) -> &'static str {
    match content_type.and_then(|kind| kind.split('/').next()) {
        Some("image") => "View-once photo · open it on your phone",
        Some("video") => "View-once video · open it on your phone",
        _ => "View-once media · open it on your phone",
    }
}

/// The note under a long text's body until attachments can be fetched: what is here is its
/// first part, the rest on the phone.
pub const CONTINUES: &str = "\n\n(message continues on your phone)";

/// Something Signal sent unasked, or the end of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The contacts the phone shared, whole.
    Contacts(Vec<Contact>),
    /// The groups the account is in, whole. This is a snapshot: it does not say what changed.
    Groups(Vec<GroupInfo>),
    /// One group changed after that snapshot. `changes` is one system line each.
    GroupChanged {
        peer: Peer,
        /// Who did it, when the change names them. Empty when it does not.
        by: String,
        changes: Vec<String>,
        /// Set when the name changed, so the row can take it without reading the line.
        title: Option<String>,
        /// Set when the description changed.
        description: Option<String>,
        /// This account is no longer a member.
        left: bool,
        /// This account was added back to the group.
        rejoined: bool,
    },
    /// A message from someone else, or one we sent from another device (`outgoing`).
    Message { peer: Peer, text: Text, outgoing: bool },
    /// A message was edited. `target` is when it was first sent, `at` when the edit was made.
    Edited { peer: Peer, author: String, target: u64, at: u64, text: String, spans: Vec<Span> },
    Deleted { peer: Peer, author: String, target: u64 },
    /// Removed on another device for this account only. `target` is when it was first sent.
    DeletedForMe { peer: Peer, author: String, target: u64 },
    /// The whole conversation was removed for this account on another device.
    Forgot(Peer),
    /// `by` reacted to `author`'s message sent at `target`.
    Reacted { peer: Peer, by: String, author: String, target: u64, emoji: String, removed: bool },
    /// A poll was started. `options` are the choices, in order.
    Poll {
        peer: Peer,
        author: String,
        name: String,
        sent: u64,
        question: String,
        options: Vec<String>,
        multiple: bool,
        outgoing: bool,
    },
    /// `by` voted on `author`'s poll sent at `target`. An empty `indexes` takes the vote back.
    /// `serial` is Signal's voteCount; `0` means this vote is newer than the one we have.
    PollVote { peer: Peer, by: String, author: String, target: u64, indexes: Vec<u32>, serial: u32 },
    /// `author` ended their poll sent at `target`.
    PollEnded { peer: Peer, author: String, target: u64 },
    /// The user read these messages (author, sent) on another device.
    ReadElsewhere(Vec<(String, u64)>),
    /// `by`'s phone received our messages sent at these times, or `by` read them (`read`).
    /// `viewed` is a voice note or a video played: it counts as read, and says when.
    /// `at` is when this receipt was sent, in milliseconds.
    Receipt { by: String, sent: Vec<u64>, read: bool, viewed: bool, at: u64 },
    Typing { peer: Peer, by: String, name: String, started: bool },
    /// Someone is calling: an offer for the person (`by`) in a one-to-one conversation. A
    /// group's ring update is a blob this library cannot open, so a group call stays with the
    /// phone. `name` is the caller's as the user knows them, `sent` when the offer was sent.
    Call { peer: Peer, by: String, name: String, video: bool, id: u64, sent: u64 },
    /// A call we heard of is over: its hangup, or the other side telling us it was busy.
    /// `answered` says someone took it on another device; nobody did when it is false.
    CallEnded { id: u64, answered: bool },
    /// The user's own settings, as the phone shares them.
    Settings { read_receipts: bool, typing: bool },
    /// A conversation's disappearing-messages timer changed (0 turns it off).
    TimerChanged { peer: Peer, seconds: u32, version: u32 },
    /// Per-conversation mute, archive, pin, and mark-unread state from the phone / storage service.
    ChatStates {
        chats: Vec<ChatState>,
        /// Ordered list of pinned conversations, when present in the account record.
        pinned: Option<Vec<Peer>>,
        /// Account setting: keep muted chats archived when new messages arrive.
        keep_muted_archived: Option<bool>,
    },
    /// Chat folders synced from the phone's storage service.
    Folders(Vec<ChatFolder>),
    /// Block list synced from the phone (`SyncMessage.blocked`): blocked ACIs and group public ids.
    Blocked { people: Vec<String>, groups: Vec<String> },
    /// A message request was answered on another device (`SyncMessage.messageRequestResponse`).
    MessageRequestResponse { peer: Peer, action: MessageRequestAction },
    /// Everything that waited while we were away has arrived.
    Caught,
    /// The connection is gone; a new wire is needed.
    Closed(String),
    /// The phone removed this device, or the account is gone. Signing in again links anew.
    Ended(String),
}

/// One file on a message we send. The bytes are what Signal is given: a still picture has already
/// had its metadata removed, unless the person asked for the file as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// A small JPEG a conversation can show while the file arrives. Empty when there is none.
    pub thumb: Vec<u8>,
    pub blurhash: Option<String>,
}

/// What is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Text { text: String, quote: Option<Quote>, ranges: Vec<Range>, timer: Timer },
    /// Files, with the caption in `text` (empty when there is none).
    Files { text: String, quote: Option<Quote>, files: Vec<File>, timer: Timer },
    React { author: String, target: u64, emoji: String, remove: bool },
    Edit { target: u64, text: String, ranges: Vec<Range>, timer: Timer },
    /// Delete for everyone.
    Delete { target: u64 },
    /// Delete for me: the user's other devices drop this message, and nobody else is told.
    DeleteForMe { author: String, target: u64 },
    /// Start a poll. `options` has 2 to 10 choices.
    Poll { question: String, options: Vec<String>, multiple: bool, timer: Timer },
    /// Vote, or take a vote back when `indexes` is empty. `serial` increments each time we vote.
    PollVote { author: String, target: u64, indexes: Vec<u32>, serial: u32 },
    /// End our own poll.
    PollEnd { target: u64 },
    /// Share a contact card.
    Contact { contact: protocol::SharedContact, timer: Timer },
}

/// Why something failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The session is over: this device was unlinked. Only the real wire can tell.
    #[cfg_attr(not(feature = "signal"), allow(dead_code))]
    Ended(String),
    /// Signal said no, in words the user can read.
    Refused(String),
    /// No answer.
    Net(String),
}

impl Fault {
    pub fn words(&self) -> String {
        match self {
            Fault::Ended(_) => "This computer is no longer linked to Signal. Sign in again.".to_owned(),
            Fault::Refused(why) => why.clone(),
            Fault::Net(_) => "Signal did not answer.".to_owned(),
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Ended(why) | Fault::Refused(why) | Fault::Net(why) => f.write_str(why),
        }
    }
}

/// One connection to Signal under one linked device.
pub trait Wire: Send + Sync {
    /// The events, once: whoever takes them is the only reader.
    fn events(&self) -> Option<Receiver<Event>>;
    /// Our account id and our own name.
    fn me(&self) -> (String, String);
    /// Sends, and answers the time it went with: the message's id from now on.
    fn send(&self, to: &Peer, body: Body) -> Result<u64, Fault>;
    /// People the last group send did not reach. Empty when the wire cannot say who: a hard
    /// error fails the whole send instead.
    fn missed(&self) -> Vec<String> {
        Vec::new()
    }
    fn typing(&self, to: &Peer, started: bool) -> Result<(), Fault>;
    /// The user read these (author, sent): the user's other devices are told, and with `receipts`
    /// the authors too.
    fn read(&self, read: &[(String, u64)], receipts: bool) -> Result<(), Fault>;
    /// Removes this device from the account, and what it kept here with it.
    fn unlink(&self) -> Result<(), Fault>;
    /// A person's profile picture or a group's, opened: `None` when they have none, or we hold
    /// no key to their profile.
    fn picture(&self, of: &Peer) -> Result<Option<Vec<u8>>, Fault>;
    /// Our own card: name, about, about emoji, username, phone, and a signal.me link when one
    /// is known. A username the library cannot read is empty, and so is the link.
    fn card(&self) -> Result<OwnCard, Fault>;
    /// Writes the name, about, and about emoji. `None` leaves that part. An empty about or
    /// emoji clears it. Succeeds only once the written profile was read back and the other
    /// devices were told.
    fn set_card(&self, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Fault>;
    /// The account id behind a username (`name.42`), or none when Signal has no such username.
    /// A phone number is not looked up here.
    fn lookup(&self, username: &str) -> Result<Option<String>, Fault>;
    /// Sets a conversation's mute expiry in milliseconds since 1970 (0 unmutes; i64::MAX mutes forever).
    fn set_mute(&self, to: &Peer, until_ms: u64) -> Result<(), Fault>;
    /// Archives or unarchives a conversation.
    fn set_archived(&self, to: &Peer, archived: bool) -> Result<(), Fault>;
    /// Pins or unpins a conversation.
    fn set_pinned(&self, to: &Peer, pinned: bool) -> Result<(), Fault>;
    /// Marks a conversation as unread (or clears the manual unread flag).
    fn set_marked_unread(&self, to: &Peer, unread: bool) -> Result<(), Fault>;
    /// Blocks or unblocks a person or group, syncing the updated block list to other devices.
    fn set_blocked(&self, peer: &Peer, blocked: bool) -> Result<(), Fault>;
    /// Responds to a message request (Accept, Delete, Block, Spam, etc.) and syncs to other devices.
    fn respond_message_request(&self, peer: &Peer, action: MessageRequestAction) -> Result<(), Fault>;
    /// Creates a new Signal Groups v2 group with `title` and `members` (ACIs) and returns its info.
    fn create_group(&self, title: &str, members: &[String]) -> Result<GroupInfo, Fault>;
    /// Leaves a Signal Groups v2 group by its public hex id.
    fn leave_group(&self, group_id: &str) -> Result<(), Fault>;
    /// Ends the connection: the events end, and every later request fails.
    fn close(&self);
}

/// Our own profile, as the wire can read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnCard {
    pub name: String,
    pub about: String,
    pub emoji: String,
    /// Empty when this build cannot read one.
    pub username: String,
    /// International form. Empty when the registration did not keep one.
    pub phone: String,
    /// A signal.me link when a username is known. Empty otherwise. The shell draws the QR.
    pub link: String,
}

/// What a finished link produced.
pub struct Linked {
    /// What the keychain keeps (`super::Saved`, as JSON).
    pub session: String,
}

/// Links this computer to an account as a new device: shows the link to scan, waits for the
/// phone, and answers what to keep. Blocks for as long as that takes. A link the user cancels
/// (`cancelled`) is undone, never left on the account.
pub trait Linker: Send {
    fn link(self: Box<Self>, device: &str, show: &dyn Fn(String), cancelled: Arc<AtomicBool>) -> Result<Linked, Fault>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_go_both_ways_and_nothing_else_reads_as_one() {
        let aci = "0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b";
        assert_eq!(Peer::parse(aci), Some(Peer::Direct(aci.to_owned())));
        assert_eq!(Peer::parse("g.00ff"), Some(Peer::Group("00ff".into())));
        assert_eq!(Peer::Group("00ff".into()).conversation(), "g.00ff");
        assert_eq!(Peer::parse("g."), None);
        assert_eq!(Peer::parse("0F6C2A3E-1B4D-4E5F-8A9B-0C1D2E3F4A5B"), None);
        assert_eq!(parse_message_id(&message_id(1_791_892_800_123, aci)), Some((1_791_892_800_123, aci.to_owned())));
        assert_eq!(parse_message_id("17-not-an-aci"), None);
    }

    #[test]
    fn a_payment_and_a_gift_badge_say_where_they_are_acting() {
        assert_eq!(payment_label(None), "Payment · open Signal on your phone");
        assert_eq!(payment_label(Some("for coffee")), "Payment · open Signal on your phone — for coffee");
        // An empty or blank note is no note.
        assert_eq!(payment_label(Some("")), "Payment · open Signal on your phone");
        assert_eq!(payment_label(Some("   ")), "Payment · open Signal on your phone");
        // A note is a peer's words: capped, never unbounded.
        let long = "x".repeat(NOTE_LIMIT + 50);
        let said = payment_label(Some(long.as_str()));
        assert_eq!(said.chars().count(), "Payment · open Signal on your phone — ".chars().count() + NOTE_LIMIT);
        assert_eq!(gift_badge_label(), "Gift badge · open Signal on your phone to redeem");
    }

    #[test]
    fn a_view_once_message_says_where_it_opens() {
        assert_eq!(view_once_label(Some("image/jpeg")), "View-once photo · open it on your phone");
        assert_eq!(view_once_label(Some("video/mp4")), "View-once video · open it on your phone");
        assert_eq!(view_once_label(Some("application/pdf")), "View-once media · open it on your phone");
        assert_eq!(view_once_label(None), "View-once media · open it on your phone");
    }
}
