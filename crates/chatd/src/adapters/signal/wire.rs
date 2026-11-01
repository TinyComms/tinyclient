//! The seam between the adapter and the Signal library. Everything above it speaks the contract;
//! everything below it is Signal: keys, sessions, sealed sender, the socket. `real` is presage
//! (built with the `signal` feature); `fake` is a script, so the adapter can be run through
//! chatd's socket with no Signal behind it and no Signal library linked.
//!
//! Signal names a message by who wrote it and the millisecond they sent it, and has no list of
//! conversations or history on its servers: what arrives is all there is.

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
}

/// The earlier message a message answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quote {
    pub sent: u64,
    pub author: String,
    pub text: String,
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
    /// What it carries that is not text and is not drawn here ("Photo", "Sticker"), in words.
    pub unsupported: Option<String>,
}

/// Something Signal sent unasked, or the end of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The contacts the phone shared, whole.
    Contacts(Vec<Contact>),
    /// The groups the account is in, whole.
    Groups(Vec<GroupInfo>),
    /// A message from someone else, or one we sent from another device (`outgoing`).
    Message { peer: Peer, text: Text, outgoing: bool },
    /// A message was edited. `target` is when it was first sent.
    Edited { peer: Peer, author: String, target: u64, text: String },
    Deleted { peer: Peer, author: String, target: u64 },
    /// `by` reacted to `author`'s message sent at `target`.
    Reacted { peer: Peer, by: String, author: String, target: u64, emoji: String, removed: bool },
    /// The user read these messages (author, sent) on another device.
    ReadElsewhere(Vec<(String, u64)>),
    /// `by`'s phone received our messages sent at these times, or `by` read them (`read`).
    Receipt { by: String, sent: Vec<u64>, read: bool },
    Typing { peer: Peer, by: String, name: String, started: bool },
    /// The user's own settings, as the phone shares them.
    Settings { read_receipts: bool, typing: bool },
    /// Everything that waited while we were away has arrived.
    Caught,
    /// The connection is gone; a new wire is needed.
    Closed(String),
    /// The phone removed this device, or the account is gone. Signing in again links anew.
    Ended(String),
}

/// What is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Text { text: String, quote: Option<Quote> },
    React { author: String, target: u64, emoji: String, remove: bool },
    Edit { target: u64, text: String },
    Delete { target: u64 },
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
    fn typing(&self, to: &Peer, started: bool) -> Result<(), Fault>;
    /// The user read these (author, sent): the user's other devices are told, and with `receipts`
    /// the authors too.
    fn read(&self, read: &[(String, u64)], receipts: bool) -> Result<(), Fault>;
    /// Removes this device from the account, and what it kept here with it.
    fn unlink(&self) -> Result<(), Fault>;
    /// A person's profile picture or a group's, opened: `None` when they have none, or we hold
    /// no key to their profile.
    fn picture(&self, of: &Peer) -> Result<Option<Vec<u8>>, Fault>;
    /// Ends the connection: the events end, and every later request fails.
    fn close(&self);
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
}
