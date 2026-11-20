//! Signal: text, and the photos, videos and files a person attaches. This computer is linked
//! to the user's account as one more device, the way Signal Desktop is; it never registers a
//! phone number, which would take the account from the phone. Calls and voice stay out of chatd.
//!
//! # The library: presage, in this process (decided 2026-10-14, by building it on this Mac)
//!
//! presage (whisperfish, on libsignal-service-rs and Signal's own libsignal) is the maintained
//! Rust client; the alternatives are signal-cli, a JVM daemon of a few hundred megabytes resident,
//! and Signal Desktop's own code, which is Electron. presage builds here once `protoc` is on the
//! path (Signal's post-quantum ratchet compiles its protobufs at build time); with its `cdsi`
//! feature off nothing needs CMake or BoringSSL. The `signal` cargo feature is on by default.
//! presage, libsignal-service-rs and libsignal are AGPL-3.0, so a chatd that links them is a
//! combined work under the AGPL. libsignal-service speaks HTTPS and websockets through reqwest
//! and rustls (with ring), pinned to Signal's own certificate authority; the rest of chatd uses
//! the system's TLS. presage and libsignal-service are git dependencies at fixed commits.
//!
//! Like Telegram's grammers it is async: one current-thread tokio runtime, on the adapter's own
//! thread (`real`), built when an account exists or a link begins and at no other time.
//!
//! # The session
//!
//! The keychain item holds who the account is and the key to its state (`Saved`). The state
//! itself, which Signal's ratchets change with every message (identity and prekeys, sessions,
//! group keys, contacts), is one file per account under `signal/` in the support directory, each
//! value sealed with that key (`vault`). No message is kept there: messages go to chatd's store
//! as they arrive, and Signal keeps no history on its servers to fetch again.
//!
//! # What the user did, and only that
//!
//! Read receipts go out when the shell says a message was read and the user's own Signal settings
//! send them; typing likewise. Both stay off until the phone has said what those settings are.
//! Nothing else is sent unasked: no reaction, edit, delete or profile change.

mod fake;
mod files;
#[cfg(feature = "signal")]
mod real;
mod style;
pub mod signin;
#[cfg(feature = "signal")]
mod vault;
mod wire;

use super::{keep_connected, serve_commands, About, Command, Context, Failure, Refusal, Signed};
use crate::blobs::upload::{self, Upload};
use crate::session;
use crate::store::{ConversationRow, PersonRow};
use protocol::{Attachment, Contact, ContactKind, ConversationKind, Link, MediaFile, MediaKind, Message, MessageKind, Outgoing, Poll, PollBallot, PollDraft, Reaction, Relation, ReplyTo, Span};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wire::{view_once_label, Body, Event, Fault, Peer, Quote, Text, Wire};

/// Signal has no history to fetch, so nothing is paced.
const PACE: Duration = Duration::ZERO;
/// Others are told the user is typing at most this often per conversation. Signal shows it for
/// fifteen seconds and its own apps renew it every ten.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// How long "is typing" stays without another signal.
const TYPING_SHOWN: i64 = 15;
/// Unread messages remembered per conversation, for the receipts a read sends.
const UNREAD_KEPT: usize = 500;
/// Conversations with unread messages remembered.
const CONVERSATIONS_KEPT: usize = 1_024;
/// An offer older than this is catch-up from a call that is long over: a line to record when
/// it ends, not news to ring.
const CALL_STALE: i64 = 120;
/// Our sent messages remembered, for the receipt that says where one was read.
const SENT_KEPT: usize = 1_024;
/// Reactions remembered, so a new one from the same person replaces their old one.
const REACTIONS_KEPT: usize = 4_096;
/// Largest reaction record file read on startup. `REACTIONS_KEPT` rows sit well below this;
/// anything larger is skipped so an oversized or corrupt file is not parsed under the lock.
const REACTIONS_FILE_LIMIT: u64 = 2 * 1024 * 1024;
/// Ballots remembered on one poll. Older ones drop off the list; the choice counts stay.
const BALLOTS_KEPT: usize = 1_024;
/// Signal's poll limits, from its desktop schema: a question and each choice are at most this
/// many characters, and a poll has at most this many choices.
const POLL_CHARS: usize = 100;
const POLL_MOST: usize = 10;
/// A vote or an end, packed into an edit so the command channel already exists. A real edit
/// never starts with this mark.
const POLL_VOTE: &str = "\u{1}signal-poll-vote\u{1}";
const POLL_END: &str = "\u{1}signal-poll-end\u{1}";
/// What a person with no name we know is called.
const NO_NAME: &str = "Signal user";

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// One member, as a group change compares them. `admin` is Signal's administrator role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSnap {
    pub id: String,
    pub admin: bool,
}

/// The parts of a group a change can show. Built from the stored group, never from an unsigned change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupSnap {
    pub title: String,
    pub description: String,
    pub timer_secs: u32,
    pub avatar: String,
    pub members: Vec<MemberSnap>,
    pub pending: Vec<String>,
    pub requesting: Vec<String>,
    /// Anyone with the link may join.
    pub link: bool,
}

/// What changed between two snapshots of one group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupDelta {
    pub lines: Vec<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub left: bool,
    pub rejoined: bool,
}

/// Tracks the last revision applied for each group. The first snapshot is recorded silently;
/// stale or duplicate revisions are skipped; a newer revision yields its prior snapshot once.
#[derive(Debug, Default)]
pub struct GroupRevisions {
    known: HashMap<String, (u32, GroupSnap)>,
}

impl GroupRevisions {
    pub fn contains(&self, id: &str) -> bool {
        self.known.contains_key(id)
    }

    /// Records the initial snapshot for a group without announcing a change.
    /// Does not overwrite an already-tracked group.
    pub fn seed(&mut self, id: String, revision: u32, snap: GroupSnap) {
        self.known.entry(id).or_insert((revision, snap));
    }

    /// True only when `id` is tracked and `revision` is strictly newer than the last applied one.
    pub fn should_check(&self, id: &str, revision: u32) -> bool {
        self.known.get(id).is_some_and(|(known_rev, _)| revision > *known_rev)
    }

    /// Advances `id` to `stored_revision` and returns the prior snapshot if `stored_revision`
    /// is strictly newer than the last applied one.
    pub fn advance(&mut self, id: &str, stored_revision: u32, after: GroupSnap) -> Option<GroupSnap> {
        let known_rev = self.known.get(id)?.0;
        if stored_revision <= known_rev {
            return None;
        }
        let (_, before) = self.known.insert(id.to_owned(), (stored_revision, after))?;
        Some(before)
    }
}

fn person(id: &str, me: &str, name: &impl Fn(&str) -> String) -> String {
    if id == me {
        "You".to_owned()
    } else {
        let named = name(id);
        if named.is_empty() { "Someone".to_owned() } else { named }
    }
}

fn duration(secs: u32) -> String {
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        let days = secs / 86_400;
        format!("{days} day{}", if days == 1 { "" } else { "s" })
    } else if secs >= 3_600 && secs.is_multiple_of(3_600) {
        let hours = secs / 3_600;
        format!("{hours} hour{}", if hours == 1 { "" } else { "s" })
    } else if secs >= 60 && secs.is_multiple_of(60) {
        let minutes = secs / 60;
        format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
    } else {
        format!("{secs} seconds")
    }
}

/// The lines and the row fields for one group revision. The first snapshot of a connection is not
/// passed here: a connect has no "before", so it shows the current name and no system lines.
pub fn group_delta(before: &GroupSnap, after: &GroupSnap, me: &str, name: impl Fn(&str) -> String) -> GroupDelta {
    let mut lines = Vec::new();
    let title = (before.title != after.title).then(|| {
        lines.push(format!("The group name is now {}", after.title));
        after.title.clone()
    });
    let description = (before.description != after.description).then(|| {
        if after.description.is_empty() {
            lines.push("The group description was cleared".to_owned());
        } else {
            lines.push(format!("The group description is now {}", after.description));
        }
        after.description.clone()
    });
    if before.avatar != after.avatar {
        lines.push("The group photo changed".to_owned());
    }
    if before.timer_secs != after.timer_secs {
        if after.timer_secs == 0 {
            lines.push("Disappearing messages are off".to_owned());
        } else {
            lines.push(format!("Disappearing messages are set to {}", duration(after.timer_secs)));
        }
    }
    if before.link != after.link {
        lines.push(if after.link { "The group link is on".to_owned() } else { "The group link is off".to_owned() });
    }
    let was: HashMap<&str, bool> = before.members.iter().map(|member| (member.id.as_str(), member.admin)).collect();
    let now: HashMap<&str, bool> = after.members.iter().map(|member| (member.id.as_str(), member.admin)).collect();
    let mut joined: Vec<&str> = now.keys().copied().filter(|id| !was.contains_key(id)).collect();
    joined.sort_unstable();
    let mut rejoined = false;
    for id in joined {
        if id == me {
            rejoined = true;
        }
        lines.push(format!("{} joined the group", person(id, me, &name)));
    }
    let mut stayed: Vec<&str> = now.keys().copied().filter(|id| was.get(id).is_some_and(|admin| *admin != now[id])).collect();
    stayed.sort_unstable();
    for id in stayed {
        let admin = now[id];
        let line = if id == me {
            if admin { "You are now an admin".to_owned() } else { "You are no longer an admin".to_owned() }
        } else if admin {
            format!("{} is now an admin", person(id, me, &name))
        } else {
            format!("{} is no longer an admin", person(id, me, &name))
        };
        lines.push(line);
    }
    let mut gone: Vec<&str> = was.keys().copied().filter(|id| !now.contains_key(id)).collect();
    gone.sort_unstable();
    let mut left = false;
    for id in gone {
        if id == me {
            left = true;
            lines.push("You are no longer in this group".to_owned());
        } else {
            lines.push(format!("{} is no longer in the group", person(id, me, &name)));
        }
    }
    let pending: HashSet<&str> = before.pending.iter().map(String::as_str).collect();
    let mut invited: Vec<&str> = after.pending.iter().map(String::as_str).filter(|id| !pending.contains(id) && !now.contains_key(id)).collect();
    invited.sort_unstable();
    for id in invited {
        lines.push(format!("{} was invited", person(id, me, &name)));
    }
    let asking: HashSet<&str> = before.requesting.iter().map(String::as_str).collect();
    let mut requests: Vec<&str> = after.requesting.iter().map(String::as_str).filter(|id| !asking.contains(id)).collect();
    requests.sort_unstable();
    for id in requests {
        lines.push(format!("{} asked to join", person(id, me, &name)));
    }
    GroupDelta { lines, title, description, left, rejoined }
}

/// The lab driver for `signal-group-rename`. A build that drops a rename fails.
pub fn prove_signal_group_rename() -> Result<(), String> {
    let before = GroupSnap { title: "Bridge club".into(), ..GroupSnap::default() };
    let after = GroupSnap { title: "Wednesday club".into(), ..GroupSnap::default() };
    let delta = group_delta(&before, &after, "me", |_| String::new());
    if delta.title.as_deref() != Some("Wednesday club") || delta.lines != ["The group name is now Wednesday club"] || delta.left {
        return Err("a renamed group must take the new name and one line".into());
    }
    let member = GroupSnap { members: vec![MemberSnap { id: "me".into(), admin: false }], ..GroupSnap::default() };
    let left = group_delta(&member, &GroupSnap::default(), "me", |_| String::new());
    if !left.left || left.lines != ["You are no longer in this group"] {
        return Err("leaving the group must say so".into());
    }
    Ok(())
}

/// Most pictures asked of Signal for one change to what is on screen.
const PICTURES: usize = 25;
/// What the official apps keep in the message itself of a long text; more travels as Signal's
/// own plain-text attachment, whose first part the body repeats. Sending that way is still to
/// come. The composer counts in characters where Signal cuts in bytes on a character boundary.
const TEXT_LIMIT: u32 = 2_048;
/// Marks a `SetPresence` status that is really an edit of our own profile. A status a person
/// typed never starts with this, and the server sends it only for Signal, without changing
/// presence.
const PROFILE_MARK: char = '\u{1}';
const PROFILE_PREFIX: &str = "\u{1}signal-profile\u{1}";
/// A username still needs a reservation this library does not make.
pub const USERNAME_ON_PHONE: &str = "A Signal username is still changed on your phone.";
/// Uploading an avatar needs a cipher stream this library leaves unimplemented.
pub const PHOTO_ON_PHONE: &str = "A Signal profile photo is still changed on your phone.";

/// Packs a profile edit into the status a `SetPresence` already carries. `None` leaves that
/// part. Lengths are Signal's own, cut here so a long paste cannot become the whole command.
pub fn profile_status(name: Option<&str>, about: Option<&str>, emoji: Option<&str>) -> String {
    format!("{PROFILE_PREFIX}{}{PROFILE_MARK}{}{PROFILE_MARK}{}", profile_field(name, 64), profile_field(about, 350), profile_field(emoji, 16))
}

fn profile_field(value: Option<&str>, limit: usize) -> String {
    match value {
        None => "0".to_owned(),
        Some(text) => {
            let text: String = text.chars().filter(|c| *c != PROFILE_MARK).take(limit).collect();
            format!("1{text}")
        }
    }
}

/// The name, about, and emoji in a status [`profile_status`] packed. `None` when it is a
/// real status, or a field that was left alone.
pub fn profile_edit(status: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let rest = status.strip_prefix(PROFILE_PREFIX)?;
    let mut parts = rest.split(PROFILE_MARK);
    Some((profile_part(parts.next()?)?, profile_part(parts.next()?)?, profile_part(parts.next()?)?))
}

fn profile_part(part: &str) -> Option<Option<String>> {
    let mut chars = part.chars();
    match chars.next() {
        Some('0') => Some(None),
        Some('1') => Some(Some(chars.collect())),
        _ => None,
    }
}

/// The lab's signal-long-text driver. A build without the Signal library has no driver, so it stays up and the lab fails.
pub fn prove_signal_long_text() -> Result<(), String> {
    #[cfg(feature = "signal")]
    {
        real::prove_long_text()
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// The lab's signal-own-profile driver. A build without this stays up, so the driver fails.
pub fn prove_signal_own_profile() -> Result<(), String> {
    let packed = profile_status(Some("Ada"), Some("New about"), None);
    let (name, about, emoji) = profile_edit(&packed).ok_or("a profile edit was not recognised")?;
    if name.as_deref() != Some("Ada") || about.as_deref() != Some("New about") || emoji.is_some() {
        return Err(format!("the profile edit did not round-trip: {name:?} {about:?} {emoji:?}"));
    }
    if profile_edit("Lunch").is_some() {
        return Err("a status was read as a profile edit".into());
    }
    if !USERNAME_ON_PHONE.to_lowercase().contains("phone") || !PHOTO_ON_PHONE.to_lowercase().contains("phone") {
        return Err("username and photo are still changed on the phone".into());
    }
    Ok(())
}

/// The lab's signal-payment-labels driver. A build without this stays up, so the driver fails.
pub fn prove_signal_payment_labels() -> Result<(), String> {
    #[cfg(feature = "signal")]
    {
        real::prove_payment_labels()
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// The lab's signal-view-once driver. A build without this stays up, so the driver fails.
pub fn prove_signal_view_once() -> Result<(), String> {
    let wants = [(Some("image/jpeg"), "View-once photo · open it on your phone"), (Some("video/mp4"), "View-once video · open it on your phone"), (None, "View-once media · open it on your phone")];
    for (kind, want) in wants {
        let said = view_once_label(kind);
        if said != want {
            return Err(format!("a view-once message is labelled {said:?}, not {want:?}"));
        }
    }
    Ok(())
}

/// The lab's signal-privacy-local driver. Signal takes a local switch for typing and read
/// receipts: stored in its account scope, on until chosen, off stops it here. Nothing stored is
/// the account's own setting, which stays off until the phone says otherwise.
pub fn prove_signal_privacy_local() -> Result<(), String> {
    let account = protocol::AccountKey { network: protocol::Network::Signal, account: "prove".to_owned() };
    let scope = protocol::settings::account_scope(&account);
    if !protocol::settings::typing_choice(protocol::Network::Signal) {
        return Err("Signal has no local typing switch".to_owned());
    }
    if !protocol::settings::valid(&scope, protocol::settings::TYPING, "off") || !protocol::settings::valid(&scope, protocol::settings::RECEIPTS, "off") {
        return Err("Signal's account scope takes no local off for typing or receipts".to_owned());
    }
    let mut values = protocol::settings::Values::default();
    if !values.typing(&account) || !values.receipts(&account) {
        return Err("nothing stored must leave both on".to_owned());
    }
    values.set(&scope, protocol::settings::TYPING, Some("off".into()));
    values.set(&scope, protocol::settings::RECEIPTS, Some("off".into()));
    if values.typing(&account) || values.receipts(&account) {
        return Err("off here did not stop it".to_owned());
    }
    Ok(())
}

/// One day, in milliseconds. Signal Desktop still sends an edit or a delete for everyone for this
/// long (`DAY` in its source) and then does not.
const SEND_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// Two days. An incoming edit or delete for everyone older than this is ignored (`DAY * 2`).
const RECEIVE_WINDOW_MS: u64 = 2 * SEND_WINDOW_MS;

/// A test in a scratch directory asked for the scripted wire.
fn scripted() -> bool {
    !session::enabled() && std::env::var_os("TINYCLIENT_SIGNAL_FAKE").is_some()
}

pub fn supported() -> bool {
    cfg!(feature = "signal") || scripted()
}

/// Why Signal cannot be used in this build, for the sign-in sheet.
pub fn unsupported_reason() -> String {
    "This build of tinyclient was made without Signal, so it cannot sign in to Signal.".to_owned()
}

/// What the keychain holds for Signal. Not `Debug`: `vault` opens the account's keys.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    /// The account id (ACI).
    pub aci: String,
    #[serde(default)]
    pub name: String,
    /// The key the account's state file is sealed with, hex.
    #[serde(default)]
    pub vault: String,
}

fn parse(session: &str) -> Result<Saved, String> {
    let saved: Saved = serde_json::from_str(session).map_err(|_| "the Signal session is unreadable".to_owned())?;
    if !wire::is_aci(&saved.aci) {
        return Err("the Signal session names no account".to_owned());
    }
    Ok(saved)
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let saved = parse(session)?;
    Ok(vec![Signed {
        account: saved.aci.clone(),
        name: if saved.name.is_empty() { "Signal".to_owned() } else { saved.name },
        session: session.to_owned(),
    }])
}

/// One link is one account, so nothing is left once it goes.
pub fn without(_session: &str, _account: &str) -> Option<String> {
    None
}

/// A wire for this account.
fn open(saved: &Saved) -> Result<Box<dyn Wire>, String> {
    if scripted() {
        return Ok(Box::new(fake::Fake::open(saved)));
    }
    #[cfg(feature = "signal")]
    {
        Ok(Box::new(real::Real::open(saved)?))
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// What links a new device, for the sign-in.
fn linker() -> Result<Box<dyn wire::Linker>, String> {
    if scripted() {
        return Ok(Box::new(fake::FakeLinker));
    }
    #[cfg(feature = "signal")]
    {
        Ok(Box::new(real::RealLinker))
    }
    #[cfg(not(feature = "signal"))]
    {
        Err(unsupported_reason())
    }
}

/// A thread that runs presage: the wire's, or a link's. Unoptimized, presage and libsignal keep far
/// more on the stack than Rust's default 2 MiB, and a debug build overflows it, which aborts chatd.
/// Only the pages a thread touches use memory.
fn presage_thread(name: &str) -> std::thread::Builder {
    let thread = std::thread::Builder::new().name(name.to_owned());
    if cfg!(debug_assertions) {
        thread.stack_size(64 * 1024 * 1024)
    } else {
        thread
    }
}

/// What this device kept of an account, gone: after unlinking, or when signing out offline.
fn forget(saved: &Saved) {
    #[cfg(feature = "signal")]
    if !scripted() {
        real::forget(saved);
    }
    let _ = saved;
}

/// Small things remembered while running. All bounded.
#[derive(Default)]
struct Live {
    /// Names as the user knows people: contact names, else what their messages said.
    names: HashMap<String, String>,
    /// Unread incoming messages per conversation, (author, sent), oldest first.
    unread: HashMap<String, VecDeque<(String, u64)>>,
    /// Our sent messages and their conversation, for receipts that name only the time.
    sent: VecDeque<(u64, String)>,
    /// Each person's reaction on a message: (message id, person) → emoji.
    /// Bounded to `REACTIONS_KEPT` in memory and on disk at `reactions_path`.
    reactions: HashMap<(String, String), String>,
    reacted_order: VecDeque<(String, String)>,
    /// `{support}/signal/{aci}.reactions`. Empty when nothing is kept on disk.
    reactions_path: PathBuf,
    reactions_loaded: bool,
    typed: HashMap<String, Instant>,
    /// The one incoming call we ring for: its id, conversation, caller, kind, and when the
    /// offer was sent. The newest offer replaces an older one, so this stays one entry, and
    /// only the offer sitting here ends its own ring.
    ring: Option<Ring>,
    /// The user's own Signal settings. Off until the phone says.
    read_receipts: bool,
    typing: bool,
    /// File sends, by the time Signal gave them, so the phone's copy does not replace the file.
    echoed: VecDeque<u64>,
}

/// What [`Event::Call`] told of, for [`Event::CallEnded`] to end it.
struct Ring {
    id: u64,
    conversation: String,
    author: String,
    video: bool,
    sent: u64,
}

impl Live {
    fn name(&self, id: &str) -> String {
        self.names.get(id).cloned().unwrap_or_else(|| NO_NAME.to_owned())
    }

    fn unread(&mut self, conversation: &str, author: &str, sent: u64) {
        if !self.unread.contains_key(conversation) && self.unread.len() >= CONVERSATIONS_KEPT {
            let oldest = self.unread.iter().min_by_key(|(_, held)| held.back().map_or(0, |(_, sent)| *sent)).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.unread.remove(&oldest);
            }
        }
        let held = self.unread.entry(conversation.to_owned()).or_default();
        if held.len() >= UNREAD_KEPT {
            held.pop_front();
        }
        held.push_back((author.to_owned(), sent));
    }

    /// Takes the unread messages of a conversation up to and including `upto`.
    fn read_upto(&mut self, conversation: &str, upto: u64) -> Vec<(String, u64)> {
        let Some(held) = self.unread.get_mut(conversation) else {
            return Vec::new();
        };
        let read: Vec<_> = held.iter().filter(|(_, sent)| *sent <= upto).cloned().collect();
        held.retain(|(_, sent)| *sent > upto);
        if held.is_empty() {
            self.unread.remove(conversation);
        }
        read
    }

    fn sent(&mut self, sent: u64, conversation: &str) {
        if self.sent.len() >= SENT_KEPT {
            self.sent.pop_front();
        }
        self.sent.push_back((sent, conversation.to_owned()));
    }

    /// Loads the reaction record from disk once, keeping at most `REACTIONS_KEPT` newest rows.
    fn load_reactions(&mut self) {
        if self.reactions_loaded {
            return;
        }
        self.reactions_loaded = true;
        let rows = read_reactions(&self.reactions_path);
        let skip = rows.len().saturating_sub(REACTIONS_KEPT);
        for row in rows.into_iter().skip(skip) {
            let key = (row.message, row.person);
            if !self.reactions.contains_key(&key) {
                self.reacted_order.push_back(key.clone());
            }
            self.reactions.insert(key, row.emoji);
        }
    }

    /// This send carried a file. The phone's later copy of the same send is ignored.
    fn echoed(&mut self, sent: u64) {
        if self.echoed.len() >= SENT_KEPT {
            self.echoed.pop_front();
        }
        self.echoed.push_back(sent);
    }

    fn echoed_file(&self, sent: u64) -> bool {
        self.echoed.contains(&sent)
    }

    /// Records a person's reaction, answering the one it replaced. Both memory and the file on
    /// disk are capped at `REACTIONS_KEPT`, oldest evicted first.
    fn react(&mut self, message: &str, by: &str, emoji: Option<&str>) -> Option<String> {
        self.load_reactions();
        let key = (message.to_owned(), by.to_owned());
        let mut changed = false;
        let before = match emoji {
            Some(emoji) => {
                if self.reactions.get(&key).map(String::as_str) == Some(emoji) {
                    Some(emoji.to_owned())
                } else {
                    changed = true;
                    if let Some(pos) = self.reacted_order.iter().position(|k| k == &key) {
                        self.reacted_order.remove(pos);
                    } else {
                        while self.reacted_order.len() >= REACTIONS_KEPT {
                            if let Some(old) = self.reacted_order.pop_front() {
                                self.reactions.remove(&old);
                            }
                        }
                    }
                    self.reacted_order.push_back(key.clone());
                    self.reactions.insert(key, emoji.to_owned())
                }
            }
            None => {
                let removed = self.reactions.remove(&key);
                if removed.is_some() {
                    changed = true;
                    self.reacted_order.retain(|k| k != &key);
                }
                removed
            }
        };
        if changed {
            write_reactions(&self.reactions_path, &self.reacted_order, &self.reactions);
        }
        before.filter(|before| Some(before.as_str()) != emoji)
    }
}

/// One kept reaction. The file is a list of these in oldest-to-newest order, owned by this
/// adapter, not the message store.
#[derive(Serialize, Deserialize)]
struct KeptReaction {
    message: String,
    person: String,
    emoji: String,
}

fn reactions_path(aci: &str) -> PathBuf {
    protocol::support_dir().join("signal").join(format!("{aci}.reactions"))
}

fn read_reactions(path: &Path) -> Vec<KeptReaction> {
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() > REACTIONS_FILE_LIMIT {
        tracing::warn!("chatd: Signal: skipping oversized reaction record ({} bytes)", meta.len());
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<KeptReaction>>(&bytes).unwrap_or_default()
}

fn write_reactions(path: &Path, order: &VecDeque<(String, String)>, book: &HashMap<(String, String), String>) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            tracing::warn!("chatd: Signal: the reaction record directory could not be created");
            return;
        }
    }
    let rows: Vec<KeptReaction> = order
        .iter()
        .filter_map(|key| {
            let emoji = book.get(key)?;
            Some(KeptReaction { message: key.0.clone(), person: key.1.clone(), emoji: emoji.clone() })
        })
        .collect();
    let Ok(bytes) = serde_json::to_vec(&rows) else {
        return;
    };
    let temp = path.with_extension("tmp");
    if std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path)).is_err() {
        let _ = std::fs::remove_file(&temp);
        tracing::warn!("chatd: Signal: the reaction record could not be kept");
    }
}

/// The lab's signal-reaction-restart driver. A build without this keeps running.
pub fn prove_signal_reaction_restart() -> Result<(), String> {
    if std::env::var_os("TINYCLIENT_DIR").is_none() {
        return Err("a scratch directory is required".into());
    }
    let path = protocol::support_dir().join("signal").join("prove-reaction-restart.reactions");
    let _ = std::fs::remove_file(&path);
    let message = "1791892801000-b2b2b2b2-0000-4000-8000-000000000002";
    let person = "b2b2b2b2-0000-4000-8000-000000000002";
    let mut live = Live { reactions_path: path.clone(), ..Live::default() };
    if live.react(message, person, Some("👍")).is_some() {
        return Err("a first reaction replaced one that was not there".into());
    }
    // What a restart is: the memory is new, the file is the same.
    let mut live = Live { reactions_path: path, ..Live::default() };
    match live.react(message, person, Some("❤️")) {
        Some(old) if old == "👍" => {}
        other => return Err(format!("a restart forgot the reaction, got {other:?}")),
    }
    let held = read_reactions(&live.reactions_path)
        .into_iter()
        .find(|row| row.message == message && row.person == person)
        .map(|row| row.emoji);
    if held.as_deref() != Some("❤️") {
        return Err(format!("the kept reaction is {held:?}"));
    }
    Ok(())
}

/// One connection's worth of an account: what both threads of the adapter share.
struct Account {
    ctx: Context,
    wire: Box<dyn Wire>,
    me: String,
    name: String,
    live: Arc<Mutex<Live>>,
    /// Conversations whose picture has been asked of Signal on this connection.
    pictured: Mutex<HashSet<String>>,
}

impl Account {
    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.aci != ctx.account.account {
        return Err("the Signal session is for another account".to_owned());
    }
    if !supported() {
        return Err(unsupported_reason());
    }
    // What is learned (names, settings, unread) outlives one connection. Reactions outlive the
    // process: the file is what a later reaction replaces, once this memory is gone.
    let live = Arc::new(Mutex::new(Live { reactions_path: reactions_path(&saved.aci), ..Live::default() }));
    let current: Arc<Mutex<Option<Arc<Account>>>> = Arc::default();
    let (socket_ctx, held, socket_saved, socket_live) = (ctx.clone(), current.clone(), saved.clone(), live.clone());
    std::thread::Builder::new()
        .name("signal-events".into())
        .spawn(move || {
            keep_connected(&socket_ctx, || connect(&socket_ctx, &socket_saved, &socket_live, &held));
            held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        })
        .map_err(|err| err.to_string())?;
    std::thread::Builder::new()
        .name("signal-commands".into())
        .spawn(move || {
            let account = || current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            serve_commands(
                &ctx,
                inbox,
                PACE,
                // Signal keeps no history on its servers: what is stored is all there is.
                |_| {},
                |command| match (account(), command) {
                    (Some(account), command) => serve(&account, command),
                    (None, Command::SignOut(reply)) => {
                        forget(&saved);
                        let _ = reply.send(Ok(()));
                    }
                    (None, command) => offline(&ctx, command),
                },
            );
            // The adapter was dropped (a sign-out, a new sign-in): the connection goes with it,
            // which is also what lets the thread that follows its events end.
            if let Some(account) = account() {
                account.wire.close();
            }
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// One connection, from opening the wire to losing it.
fn connect(ctx: &Context, saved: &Saved, live: &Arc<Mutex<Live>>, current: &Mutex<Option<Arc<Account>>>) -> Failure {
    let wire = match open(saved) {
        Ok(wire) => wire,
        Err(why) => return Failure::Dropped(why),
    };
    let Some(events) = wire.events() else {
        return Failure::Dropped("the wire has no events".to_owned());
    };
    let (me, name) = wire.me();
    let name = if name.is_empty() { saved.name.clone() } else { name };
    let account = Arc::new(Account { ctx: ctx.clone(), wire, me, name, live: live.clone(), pictured: Mutex::default() });
    let _ = ctx.describe(if account.name.is_empty() { "Signal" } else { &account.name }, &account.me);
    show_self(&account);
    // Nothing is counted until an adapter says so; Signal counts its own body limit.
    ctx.capabilities(capabilities());
    *current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(account.clone());
    // Dropped while connecting: the command thread has gone and will not close this wire.
    if ctx.stopped() {
        account.wire.close();
        return Failure::Dropped("stopped".to_owned());
    }
    // Older builds may have cached our own conversation under the contact fallback.
    // Repair it on reconnect without creating an empty self conversation.
    if matches!(ctx.known(&account.me), Ok(Some(_))) {
        ensure_row(&account, &Peer::Direct(account.me.clone()), "");
    }
    ctx.link(Link::Connected);
    let ended = loop {
        match events.recv() {
            Ok(Event::Closed(why)) => break Failure::Dropped(why),
            Ok(Event::Ended(why)) => break Failure::SignedOut(why),
            Ok(event) => apply(&account, event),
            Err(_) => break Failure::Dropped("the connection closed".to_owned()),
        }
        if ctx.stopped() {
            break Failure::Dropped("stopped".to_owned());
        }
    };
    current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    account.wire.close();
    ended
}

/// The row of a conversation, recorded the first time it is seen.
fn ensure_row(account: &Account, peer: &Peer, title: &str) {
    // The phone need not include ourselves in its contact list.
    let title = if matches!(peer, Peer::Direct(aci) if aci == &account.me) { "Note to Self" } else { title };
    let id = peer.conversation();
    match account.ctx.known(&id) {
        Ok(Some(known)) if title.is_empty() || known.title == title => {}
        // A picture fetched earlier stays the row's.
        Ok(Some(_)) | Ok(None) => drop(account.ctx.conversation(&ConversationRow { avatar: account.ctx.avatar_of(&id), ..row(peer, title) })),
        Err(err) => tracing::warn!("chatd: Signal: {err}"),
    }
}

fn row(peer: &Peer, title: &str) -> ConversationRow {
    match peer {
        Peer::Direct(aci) => {
            let title = if title.is_empty() { NO_NAME } else { title };
            let mut row = ConversationRow::new(aci.clone(), ConversationKind::Direct, title);
            row.peer = Some(aci.clone());
            row.listed = true;
            row.private = true;
            row
        }
        Peer::Group(_) => {
            let title = if title.is_empty() { "Signal group" } else { title };
            let mut row = ConversationRow::new(peer.conversation(), ConversationKind::Group, title);
            row.listed = true;
            row.private = true;
            row
        }
    }
}

/// A message as the contract has it.
fn message(account: &Account, text: &Text, outgoing: bool) -> Message {
    let author = if outgoing { account.me.as_str() } else { text.author.as_str() };
    let name = if outgoing {
        account.name.clone()
    } else if text.name.is_empty() {
        account.live().name(author)
    } else {
        text.name.clone()
    };
    let (words, shift) = match (&text.unsupported, text.text.is_empty()) {
        (Some(what), true) => (what.clone(), None),
        (Some(what), false) => (format!("{what}: {}", text.text), Some(format!("{what}: ").len() as u32)),
        (None, _) => (text.text.clone(), Some(0)),
    };
    let sent = i64::try_from(text.sent / 1_000).unwrap_or(0);
    let mut message = Message::plain(wire::message_id(text.sent, author), name, words, sent, outgoing);
    message.sender.id = author.to_owned();
    message.mentions_me = text.mentions_me && !outgoing;
    if let Some(shift) = shift {
        message.spans = text.spans.iter().map(|span| Span { start: span.start + shift, end: span.end + shift, style: span.style.clone() }).collect();
    }
    if text.unsupported.is_some() {
        message.kind = MessageKind::Unsupported { link: String::new() };
    }
    message.reply_to = text.quote.as_ref().map(|quote| reply_to(account, quote));
    message
}

fn reply_to(account: &Account, quote: &Quote) -> ReplyTo {
    let sender = if quote.author == account.me { account.name.clone() } else { account.live().name(&quote.author) };
    let snippet = crate::store::without_spoilers(&quote.text, &quote.spans);
    ReplyTo {
        id: wire::message_id(quote.sent, &quote.author),
        sender,
        snippet: snippet.chars().take(200).collect(),
        person: Some(account.ctx.key(&quote.author)),
        deleted: false,
    }
}

fn apply(account: &Account, event: Event) {
    let ctx = &account.ctx;
    match event {
        Event::Contacts(contacts) => {
            let people: Vec<PersonRow> = contacts
                .iter()
                .map(|contact| PersonRow {
                    id: contact.id.clone(),
                    name: if contact.name.is_empty() { NO_NAME.to_owned() } else { contact.name.clone() },
                    handle: String::new(),
                    avatar: ctx.avatar_of(&contact.id),
                    bot: false,
                    status: String::new(),
                    link: String::new(),
                })
                .collect();
            {
                let mut live = account.live();
                for contact in contacts.iter().filter(|contact| !contact.name.is_empty()) {
                    live.names.insert(contact.id.clone(), contact.name.clone());
                }
            }
            let _ = ctx.people(&people);
            let owned: Vec<(String, Relation)> = contacts.iter().map(|contact| (contact.id.clone(), Relation::Contact)).collect();
            let _ = ctx.relations(&owned);
            // A row named before the contact was known takes the contact's name.
            for contact in contacts.iter().filter(|contact| !contact.name.is_empty()) {
                if let Ok(Some(_)) = ctx.known(&contact.id) {
                    ensure_row(account, &Peer::Direct(contact.id.clone()), &contact.name);
                }
            }
        }
        Event::Groups(groups) => {
            for group in groups {
                let peer = Peer::Group(group.id.clone());
                let mut row = row(&peer, &group.title);
                row.topic = group.description.chars().take(300).collect();
                row.read_only = !group.member;
                row.avatar = ctx.avatar_of(&row.id);
                let _ = ctx.conversation(&row);
            }
        }
        Event::GroupChanged { peer, changes, title, description, left, rejoined, .. } => {
            let id = peer.conversation();
            let known = ctx.known(&id).ok().flatten();
            if title.is_some() || description.is_some() || left || rejoined {
                let title = title.unwrap_or_else(|| known.as_ref().map(|row| row.title.clone()).unwrap_or_default());
                let mut row = row(&peer, &title);
                row.topic = description.unwrap_or_else(|| known.as_ref().map(|row| row.topic.clone()).unwrap_or_default()).chars().take(300).collect();
                // A later rename does not put a removed member back, while re-adding clears read-only.
                row.read_only = if left {
                    true
                } else if rejoined {
                    false
                } else {
                    known.as_ref().is_some_and(|row| row.read_only)
                };
                row.avatar = ctx.avatar_of(&row.id);
                let _ = ctx.conversation(&row);
            }
            let sent_ms = now_ms();
            for (at, line) in changes.iter().enumerate() {
                let mut message = Message::plain(format!("signal-system-{sent_ms}-{at}"), String::new(), line.clone(), (sent_ms / 1_000) as i64, false);
                message.kind = MessageKind::System;
                let _ = ctx.message(&id, &message);
            }
        }
        Event::Message { peer, text, outgoing } => {
            let conversation = peer.conversation();
            if !outgoing && !text.name.is_empty() {
                let mut live = account.live();
                if !live.names.contains_key(&text.author) {
                    live.names.insert(text.author.clone(), text.name.clone());
                }
            }
            match &peer {
                // A person's row takes the name the user knows them by, once there is one.
                Peer::Direct(aci) => {
                    let title = account.live().names.get(aci).cloned().unwrap_or_default();
                    ensure_row(account, &peer, &title);
                }
                Peer::Group(_) => {
                    if let Ok(None) = ctx.known(&conversation) {
                        let _ = ctx.conversation(&row(&peer, ""));
                    }
                }
            }
            // A file we sent is already stored with that file. The phone's copy of the same
            // send is text, and would replace the row.
            let had_file = outgoing
                && ctx
                    .stored_message(&conversation, &wire::message_id(text.sent, &account.me))
                    .ok()
                    .flatten()
                    .is_some_and(|stored| !stored.attachments.is_empty());
            if outgoing && (account.live().echoed_file(text.sent) || files::replace_outgoing_sync(had_file)) {
                return;
            }
            let message = message(account, &text, outgoing);
            if outgoing {
                account.live().sent(text.sent, &conversation);
            } else {
                account.live().unread(&conversation, &text.author, text.sent);
            }
            let _ = ctx.message(&conversation, &message);
        }
        Event::Edited { peer, author, target, text, spans } => {
            // A late edit is ignored by every other client. Keeping it here would show words
            // nobody else has.
            if !accept_remote(target, now_ms(), note_to_self(account, &peer)) {
                return;
            }
            let conversation = peer.conversation();
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &wire::message_id(target, &author)) {
                message.text = text;
                message.spans = spans;
                message.edited = true;
                let _ = ctx.message(&conversation, &message);
            }
        }
        Event::Deleted { peer, author, target } => {
            if !accept_remote(target, now_ms(), note_to_self(account, &peer)) {
                return;
            }
            let _ = ctx.deleted(&peer.conversation(), &wire::message_id(target, &author));
        }
        // Delete for me has no time limit: the phone already decided, and only this account is told.
        Event::DeletedForMe { peer, author, target } => {
            let _ = ctx.deleted(&peer.conversation(), &wire::message_id(target, &author));
        }
        Event::Forgot(peer) => {
            let _ = ctx.forget_conversation(&peer.conversation());
        }
        Event::Poll { peer, author, name, sent, question, options, multiple, outgoing } => {
            if !outgoing && !name.is_empty() {
                let mut live = account.live();
                if !live.names.contains_key(&author) {
                    live.names.insert(author.clone(), name.clone());
                }
            }
            let conversation = peer.conversation();
            match &peer {
                Peer::Direct(aci) => {
                    let title = account.live().names.get(aci).cloned().unwrap_or_default();
                    ensure_row(account, &peer, &title);
                }
                Peer::Group(_) => {
                    if let Ok(None) = ctx.known(&conversation) {
                        let _ = ctx.conversation(&row(&peer, ""));
                    }
                }
            }
            let message = with_poll(account, &author, &name, sent, outgoing, Poll::open(question, options, multiple));
            if outgoing {
                account.live().sent(sent, &conversation);
            } else {
                account.live().unread(&conversation, &author, sent);
            }
            let _ = ctx.message(&conversation, &message);
        }
        Event::PollVote { peer, by, author, target, indexes, serial } => {
            let conversation = peer.conversation();
            let id = wire::message_id(target, &author);
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &id) {
                let changed = message.poll.as_mut().is_some_and(|poll| apply_ballot(poll, &by, indexes, serial, &account.me));
                if changed {
                    let _ = ctx.message(&conversation, &message);
                }
            }
        }
        Event::PollEnded { peer, author, target } => {
            let conversation = peer.conversation();
            let id = wire::message_id(target, &author);
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &id) {
                if let Some(poll) = message.poll.as_mut().filter(|poll| !poll.closed) {
                    poll.closed = true;
                    let _ = ctx.message(&conversation, &message);
                }
            }
        }
        Event::Reacted { peer, by, author, target, emoji, removed } => {
            let conversation = peer.conversation();
            let id = wire::message_id(target, &author);
            let mine = by == account.me;
            let replaced = account.live().react(&id, &by, (!removed).then_some(emoji.as_str()));
            if let Some(old) = replaced {
                let _ = ctx.reacted(&conversation, &id, &Reaction { emoji: old, file: None, count: 1, mine }, false);
            }
            let _ = ctx.reacted(&conversation, &id, &Reaction { emoji, file: None, count: 1, mine }, !removed);
        }
        Event::ReadElsewhere(read) => read_elsewhere(account, &read),
        Event::Receipt { by, sent, read } => {
            let Some(newest) = sent.iter().max().copied() else {
                return;
            };
            let conversation = {
                let live = account.live();
                live.sent.iter().rev().find(|(at, _)| *at == newest).map(|(_, conversation)| conversation.clone())
            };
            let conversation = conversation.unwrap_or_else(|| Peer::Direct(by).conversation());
            if let Ok(Some(_)) = ctx.known(&conversation) {
                let id = wire::message_id(newest, &account.me);
                let _ = if read { ctx.seen(&conversation, &id) } else { ctx.delivered(&conversation, &id) };
            }
        }
        Event::Typing { peer, by, name, started } => {
            let conversation = peer.conversation();
            if started {
                let name = if name.is_empty() { account.live().name(&by) } else { name };
                ctx.typing(&conversation, &by, &name, TYPING_SHOWN);
            } else {
                ctx.typing_stopped(&conversation, &by);
            }
        }
        Event::Call { peer, by, name, video, id, sent } => incoming_call(account, &peer, &by, &name, video, id, sent),
        Event::CallEnded { id, answered } => ended_call(account, id, answered),
        Event::Settings { read_receipts, typing } => {
            let mut live = account.live();
            live.read_receipts = read_receipts;
            live.typing = typing;
        }
        Event::Caught | Event::Closed(_) | Event::Ended(_) => {}
    }
}

/// The user read messages on another device: the rows they were in are read up to there.
fn read_elsewhere(account: &Account, read: &[(String, u64)]) {
    let mut touched: Vec<(String, u64, String)> = Vec::new();
    {
        let mut live = account.live();
        let held: Vec<(String, u64, String)> = live
            .unread
            .iter()
            .flat_map(|(conversation, held)| {
                read.iter()
                    .filter(|(author, sent)| held.iter().any(|(a, s)| a == author && s == sent))
                    .map(move |(author, sent)| (conversation.clone(), *sent, author.clone()))
            })
            .collect();
        for (conversation, sent, author) in held {
            match touched.iter_mut().find(|(c, _, _)| *c == conversation) {
                Some(seen) if seen.1 < sent => *seen = (conversation, sent, author),
                Some(_) => {}
                None => touched.push((conversation, sent, author)),
            }
        }
        for (conversation, upto, _) in &touched {
            live.read_upto(conversation, *upto);
        }
    }
    for (conversation, upto, author) in touched {
        let left = account.live().unread.get(&conversation).map_or(0, VecDeque::len) as u32;
        let updated = account.ctx.known(&conversation).ok().flatten().map_or(0, |row| row.updated);
        let _ = account.ctx.counts(&conversation, left, 0, Some(&wire::message_id(upto, &author)), updated);
    }
}

/// Whether an offer sent `sent` (ms) is recent enough to ring for. One from before we
/// connected is a call that is long over.
fn fresh_call(sent: u64) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    sent <= now && now - sent <= (CALL_STALE as u64) * 1_000
}

/// Someone is calling: their row is recorded (a ring names a conversation the list may not
/// have yet), and the ring is one notification with one action, "Answer on your phone" —
/// a Signal call is answered on the phone, never here. An offer that waited while we were
/// away is remembered for its end, but not replayed as a ring. Only a person calls here: a
/// group's ring update is a blob the wire cannot open, so it never names a conversation.
fn incoming_call(account: &Account, peer: &Peer, by: &str, name: &str, video: bool, id: u64, sent: u64) {
    if by == account.me || !matches!(peer, Peer::Direct(_)) {
        return;
    }
    let conversation = peer.conversation();
    let caller = if name.is_empty() { account.live().name(by) } else { name.to_owned() };
    ensure_row(account, peer, &caller);
    // The newest offer takes the one ring slot. An older or replayed one leaves both the slot
    // and the ring alone: a displaced call's hangup matches nothing here, and a losing offer
    // must not ring for a note its own hangup could not end.
    if account.live().ring.as_ref().is_none_or(|held| held.sent < sent) {
        account.live().ring = Some(Ring { id, conversation: conversation.clone(), author: by.to_owned(), video, sent });
        if fresh_call(sent) {
            account.ctx.phone_ring(&conversation, &caller);
        }
    }
}

/// The call ended: the ring goes, and a call nobody answered here leaves the line the other
/// networks leave. One answered on the phone leaves none, because it was not missed.
fn ended_call(account: &Account, id: u64, answered: bool) {
    if account.live().ring.as_ref().is_none_or(|ring| ring.id != id) {
        return;
    }
    let Some(ring) = account.live().ring.take() else {
        return;
    };
    account.ctx.phone_ring_stopped(&ring.conversation);
    if answered {
        return;
    }
    let mut line = Message::plain(
        wire::message_id(ring.sent, &ring.author),
        account.live().name(&ring.author),
        if ring.video { "Missed video call" } else { "Missed voice call" },
        i64::try_from(ring.sent / 1_000).unwrap_or(0),
        false,
    );
    line.kind = MessageKind::System;
    line.sender.id = ring.author;
    let _ = account.ctx.quiet_line(&ring.conversation, &line, true);
}

/// A Signal username as libsignal's `Username::new` accepts one: `nickname.42`, optionally with
/// one leading `@`. A plain name, a phone number, and a `signal.me` link are not usernames, so
/// they are not sent to Signal.
fn username_of(query: &str) -> Option<&str> {
    let username = query.trim().strip_prefix('@').unwrap_or(query.trim());
    let (nickname, discriminator) = username.rsplit_once('.')?;
    (nickname_ok(nickname) && discriminator_ok(discriminator)).then_some(username)
}

/// Nickname: 1 to 48 of `[_0-9A-Za-z]`, and not starting with a digit. libsignal's hard limit.
fn nickname_ok(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_digit() || !nickname_char(first) {
        return false;
    }
    let mut count = 1usize;
    for ch in chars {
        if !nickname_char(ch) {
            return false;
        }
        count += 1;
        if count > 48 {
            return false;
        }
    }
    true
}

fn nickname_char(ch: char) -> bool {
    matches!(ch, '_' | 'a'..='z' | 'A'..='Z' | '0'..='9')
}

/// Discriminator: at least two digits, not zero, and no leading zero once it is longer than two.
/// `01` is a username; `1` and `042` are not.
fn discriminator_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() == 1 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if bytes.len() > 2 && bytes[0] == b'0' {
        return false;
    }
    text.parse::<u64>().ok().is_some_and(|value| value != 0)
}

/// Looks up a username and stores the account it names. Anything else only finishes the search.
fn search_people(account: &Account, query: &str) {
    if let Some(username) = username_of(query) {
        match account.wire.lookup(username) {
            Ok(Some(aci)) if wire::is_aci(&aci) => {
                let name = account.live().names.entry(aci.clone()).or_insert_with(|| username.to_owned()).clone();
                let _ = account.ctx.people(&[PersonRow {
                    avatar: account.ctx.avatar_of(&aci),
                    id: aci,
                    name,
                    handle: username.to_owned(),
                    ..PersonRow::default()
                }]);
            }
            Ok(_) => {}
            Err(fault) => tracing::warn!("chatd: Signal: username: {}", super::net::redact(&fault.to_string())),
        }
    }
    account.ctx.searched(query);
}

/// The lab checks this without a Signal account: which typed queries are usernames.
pub fn prove_signal_username() -> Result<(), String> {
    for username in ["ada.42", "@ada.42", "Ada.42", "@Ada.42", "He110.01", "usr.999999999", "_identifier.42"] {
        if username_of(username).is_none() {
            return Err(format!("{username} should be a username"));
        }
    }
    if username_of("@Ada.42") != Some("Ada.42") {
        return Err("a leading @ is not part of the username".to_owned());
    }
    for query in ["Grace", "+15550100002", "https://signal.me/#eu/abc", "ada.1", "ada.042", "1ada.42", "ada.00"] {
        if username_of(query).is_some() {
            return Err(format!("{query} should not be looked up"));
        }
    }
    Ok(())
}

/// Answers a command while there is no connection.
fn offline(ctx: &Context, command: Command) {
    let down = || Refusal::Refused("Signal is offline.".to_owned());
    match command {
        Command::Send { conversation_id, client_id, .. } | Command::SendWith { conversation_id, client_id, .. } => {
            ctx.sent(&conversation_id, &client_id, Err("Signal is offline.".to_owned()));
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&ctx.account, &client_id),
        Command::React { reply, .. } | Command::Edit { reply, .. } | Command::Delete { reply, .. } => drop(reply.send(Err(down()))),
        Command::StartDirect { reply, .. } => drop(reply.send(Err(down()))),
        Command::SearchPeople(query) => ctx.searched(&query),
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        other => other.unsupported(ctx),
    }
}

/// What reactions a Signal conversation takes: any one emoji, and one reaction a person on a
/// message, a new one replacing it (which Signal's own apps do, and its protocol carries one
/// emoji per person). No custom emoji, and no cap on how many different ones a message holds.
pub fn reaction_rules() -> protocol::ReactionRules {
    protocol::ReactionRules { mine: 1, replaces: true, ..protocol::ReactionRules::default() }
}

/// What this account may send: text up to the body other clients keep in the message itself,
/// files up to Signal's limits, and a one-day edit window (how long Signal Desktop still sends an
/// edit; a late delete is delete-for-me instead, decided in `delete`).
fn capabilities() -> protocol::Capabilities {
    let files = files::capabilities();
    protocol::Capabilities {
        text_limit: TEXT_LIMIT,
        edit_window: (SEND_WINDOW_MS / 1_000) as u32,
        max_file_bytes: files.max_file_bytes,
        max_files: files.max_files,
        ..protocol::Capabilities::default()
    }
}

fn serve(account: &Arc<Account>, command: Command) {
    let ctx = &account.ctx;
    match command {
        Command::MarkRead { conversation_id, message_id } => mark_read(account, &conversation_id, &message_id),
        Command::Send { conversation_id, client_id, text, reply_to } => {
            let outcome = send_text(account, &conversation_id, &text, reply_to.as_deref());
            ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
        }
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            if let Some(draft) = with.poll.clone() {
                if !with.files.is_empty() {
                    ctx.sent(&conversation_id, &client_id, Err("A poll can't carry a file.".to_owned()));
                } else {
                    let outcome = send_poll(account, &conversation_id, &draft, reply_to.as_deref());
                    ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
                }
            } else {
                send_with(account, &conversation_id, &client_id, &text, reply_to.as_deref(), *with);
            }
        }
        Command::CancelSend { client_id, .. } => upload::cancel(&account.ctx.account, &client_id),
        Command::React { conversation_id, message_id, emoji, on, reply } => {
            let _ = reply.send(react(account, &conversation_id, &message_id, &emoji, on).map_err(refused));
        }
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        Command::Edit { conversation_id, message_id, text, reply } => {
            let answered = match poll_action(&text) {
                Some(act) => poll_act(account, &conversation_id, &message_id, act),
                None => edit(account, &conversation_id, &message_id, &text),
            };
            let _ = reply.send(answered.map_err(refused));
        }
        Command::Delete { conversation_id, message_id, reply } => {
            let _ = reply.send(delete(account, &conversation_id, &message_id).map_err(refused));
        }
        Command::Typing(conversation_id) => typing(account, &conversation_id),
        Command::StartDirect { person_id, reply } => {
            // Nothing reaches the other person: the chat exists on Signal once a message does.
            let started = wire::is_aci(&person_id).then(|| {
                let name = account.live().names.get(&person_id).cloned().unwrap_or_default();
                ensure_row(account, &Peer::Direct(person_id.clone()), &name);
                person_id
            });
            let _ = reply.send(started.ok_or_else(|| Refusal::Refused("That person can't be messaged from here.".to_owned())));
        }
        Command::SearchPeople(query) => search_people(account, &query),
        Command::Watch(watching) => pictures(account, &watching),
        Command::SignOut(reply) => {
            if let Err(fault) = account.wire.unlink() {
                tracing::warn!("chatd: Signal: unlinking: {}", super::net::redact(&fault.to_string()));
            }
            let _ = reply.send(Ok(()));
        }
        Command::Profile { person_id, reply } if person_id == account.me => {
            let _ = reply.send(own_about(account));
        }
        Command::SetPresence { status, reply, .. } => {
            let Some((name, about, emoji)) = status.as_deref().and_then(profile_edit) else {
                let _ = reply.send(Err(Refusal::Unsupported));
                return;
            };
            let _ = reply.send(set_own(account, name, about, emoji));
        }
        other => other.unsupported(ctx),
    }
}

/// Records us as a person, so the account card can be opened. A card the wire cannot read
/// still leaves the name connect already stored.
fn show_self(account: &Account) {
    let card = account.wire.card().unwrap_or_default();
    let name = if card.name.is_empty() { account.name.clone() } else { card.name };
    if !name.is_empty() {
        let _ = account.ctx.describe(&name, &account.me);
    }
    let _ = account.ctx.people(&[PersonRow {
        id: account.me.clone(),
        name,
        handle: card.username,
        avatar: account.ctx.avatar_of(&account.me),
        link: card.link,
        ..PersonRow::default()
    }]);
}

fn own_about(account: &Account) -> Result<About, Refusal> {
    let card = account.wire.card().map_err(refused)?;
    let mut about = About { bio: card.about, ..About::default() };
    if !card.phone.is_empty() {
        about.contacts.push(Contact { kind: ContactKind::Phone, value: card.phone, masked: true });
    }
    if !card.emoji.is_empty() {
        about.fields.push(("About emoji".into(), card.emoji));
    }
    if !card.username.is_empty() {
        about.fields.push(("Username".into(), card.username));
    }
    Ok(about)
}

fn set_own(account: &Account, name: Option<String>, about: Option<String>, emoji: Option<String>) -> Result<(), Refusal> {
    if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
        return Err(Refusal::Refused("A name is needed.".into()));
    }
    account.wire.set_card(name, about, emoji).map_err(refused)?;
    show_self(account);
    Ok(())
}

/// The pictures of the people and groups on screen (rows, people, the open conversation): each
/// asked of Signal once a connection, one after another, off the command thread since each is a
/// trip to Signal's CDN. What lands is the row's and the person's avatar.
fn pictures(account: &Arc<Account>, watching: &protocol::Watching) {
    let on_screen = watching.rows.iter().chain(&watching.people).chain(&watching.conversation);
    let wanted: Vec<Peer> = {
        let mut pictured = account.pictured.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        on_screen
            .filter(|key| key.account() == account.ctx.account)
            .filter_map(|key| Peer::parse(&key.id))
            .filter(|peer| pictured.insert(peer.conversation()))
            .take(PICTURES)
            .collect()
    };
    if wanted.is_empty() {
        return;
    }
    let account = account.clone();
    let spawned = std::thread::Builder::new().name("signal-pictures".into()).spawn(move || {
        for peer in wanted {
            let bytes = match account.wire.picture(&peer) {
                Ok(Some(bytes)) if crate::blobs::is_picture(&bytes) => bytes,
                Ok(_) => continue,
                // Asked again on the next connection.
                Err(fault) => {
                    tracing::warn!("chatd: Signal: picture: {}", super::net::redact(&fault.to_string()));
                    continue;
                }
            };
            let conversation = peer.conversation();
            // A new picture is a new id, so the shell never draws the old one in its place.
            let id = format!("signal:avatar:{conversation}:{:016x}", crate::blobs::fnv_bytes(&bytes));
            let file = protocol::MediaFile::new(id.clone(), protocol::MediaKind::Avatar);
            if account.ctx.landed(&file, &format!("signal://avatar/{conversation}"), &bytes).is_some() {
                let _ = account.ctx.avatar(&conversation, &id);
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("chatd: Signal: {err}");
    }
}

fn refused(fault: Fault) -> Refusal {
    Refusal::Refused(fault.words())
}

fn peer(conversation: &str) -> Result<Peer, Fault> {
    Peer::parse(conversation).ok_or_else(|| Fault::Refused("That conversation isn't one Signal knows.".to_owned()))
}

/// A message id this adapter made: when it was sent, and by whom.
fn target(message_id: &str) -> Result<(u64, String), Fault> {
    wire::parse_message_id(message_id).ok_or_else(|| Fault::Refused("That message is no longer there.".to_owned()))
}

/// The words a quote quotes of a stored message: without the note that the rest is on the
/// phone, which is this device's invention for drawing the message and never travels.
fn quoted_words(text: &str) -> String {
    text.strip_suffix(wire::CONTINUES).map_or_else(|| text.to_owned(), str::to_owned)
}

fn quote_of(account: &Account, conversation: &str, reply_to: Option<&str>) -> Result<Option<Quote>, Fault> {
    Ok(match reply_to.map(target).transpose()? {
        Some((sent, author)) => {
            let stored = account.ctx.stored_message(conversation, &wire::message_id(sent, &author)).ok().flatten();
            let (text, spans) = stored.map_or_else(|| (String::new(), Vec::new()), |message| (quoted_words(&message.text), message.spans));
            Some(Quote { sent, author, text, spans })
        }
        None => None,
    })
}

fn send_text(account: &Account, conversation: &str, text: &str, reply_to: Option<&str>) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let (plain, ranges) = style::parse(text);
    let spans = style::spans(&plain, &ranges);
    let quote = quote_of(account, conversation, reply_to)?;
    let sent = account.wire.send(&to, Body::Text { text: plain.clone(), quote: quote.clone(), ranges })?;
    account.live().sent(sent, conversation);
    let words = Text { author: account.me.clone(), name: account.name.clone(), sent, text: plain, spans, quote, ..Text::default() };
    Ok(message(account, &words, true))
}

/// Files go on the upload thread: each is read in parts, then handed to Signal as one message.
fn send_with(account: &Arc<Account>, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: Outgoing) {
    if let Some(why) = files::refused(&with) {
        account.ctx.sent(conversation, client_id, Err(why));
        return;
    }
    if with.files.is_empty() {
        let outcome = send_text(account, conversation, text, reply_to).map_err(|fault| fault.words());
        account.ctx.sent(conversation, client_id, outcome);
        return;
    }
    let account = Arc::clone(account);
    let conversation = conversation.to_owned();
    let text = text.to_owned();
    let reply_to = reply_to.map(str::to_owned);
    let client_id_owned = client_id.to_owned();
    let ctx = account.ctx.clone();
    let conversation_id = conversation.clone();
    upload::spawn(&ctx, &conversation_id, client_id, &with.files, move |upload| {
        send_files(&account, &conversation, &client_id_owned, &text, reply_to.as_deref(), upload)
    });
}

fn send_files(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, upload: &mut Upload) -> Result<Message, String> {
    if upload.files().len() > files::MAX_FILES as usize {
        return Err(format!("Signal takes at most {} files in one message.", files::MAX_FILES));
    }
    let count = upload.files().len();
    let mut prepared = Vec::with_capacity(count);
    for at in 0..count {
        let name = upload.files()[at].name.clone();
        let mime = upload.files()[at].mime.to_owned();
        let keep = upload.files()[at].as_file || upload.files()[at].original;
        let len = upload.files()[at].bytes;
        if let Some(why) = files::too_big(&name, len) {
            return Err(why);
        }
        let mut bytes = Vec::new();
        upload.parts(at, 1024 * 1024, 0, |part| {
            bytes.extend_from_slice(part.bytes);
            Ok(())
        })?;
        prepared.push(files::prepare(&name, &mime, bytes, keep));
    }
    upload.check()?;
    let to = peer(conversation).map_err(|fault| fault.words())?;
    let quote = quote_of(account, conversation, reply_to).map_err(|fault| fault.words())?;
    // Write each file to disk first, then move that one copy to Signal. Cloning `prepared`
    // would keep every payload in memory a second time for the whole upload.
    let attachments: Vec<_> = prepared.iter().enumerate().map(|(index, file)| show_file(account, client_id, index, file)).collect();
    let sent = match account.wire.send(&to, Body::Files { text: text.to_owned(), quote: quote.clone(), files: prepared }) {
        Ok(sent) => sent,
        Err(fault) => {
            forget_landed(&attachments);
            return Err(fault.words());
        }
    };
    account.live().echoed(sent);
    account.live().sent(sent, conversation);
    let words = Text { author: account.me.clone(), name: account.name.clone(), sent, text: text.to_owned(), quote, ..Text::default() };
    let mut message = message(account, &words, true);
    message.attachments = attachments;
    Ok(message)
}

/// The file as the row shows it, kept here so it stays after the staged copy is removed.
/// The id is the shell's client id, known before Signal names the message, so the bytes can
/// be written once and then moved to the wire.
fn show_file(account: &Account, client_id: &str, index: usize, file: &wire::File) -> Attachment {
    let mut media = MediaFile::new(format!("signal:{client_id}:{index}"), files::kind(&file.mime));
    media.width = file.width;
    media.height = file.height;
    media.bytes = file.bytes.len() as u64;
    if let Some(landed) = account.ctx.landed(&media, &format!("signal://file/{client_id}/{index}"), &file.bytes) {
        media = landed;
    }
    let mut attachment = Attachment::new(file.name.clone(), media);
    attachment.mime = file.mime.clone();
    if !file.thumb.is_empty() {
        let thumb = MediaFile::new(format!("signal:{client_id}:{index}:thumb"), MediaKind::Image);
        if let Some(landed) = account.ctx.landed(&thumb, &format!("signal://file/{client_id}/{index}/thumb"), &file.thumb) {
            attachment.thumb = Some(landed);
        }
    }
    attachment
}

/// A send that did not go must not leave its files on disk.
fn forget_landed(attachments: &[Attachment]) {
    for attachment in attachments {
        if let Some(path) = &attachment.file.path {
            let _ = std::fs::remove_file(path);
        }
        if let Some(thumb) = &attachment.thumb {
            if let Some(path) = &thumb.path {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn react(account: &Account, conversation: &str, message_id: &str, emoji: &str, on: bool) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let (sent, author) = target(message_id)?;
    // One reaction a person, on Signal: taking back sends the one being taken back.
    account.wire.send(&to, Body::React { author, target: sent, emoji: emoji.to_owned(), remove: !on })?;
    let replaced = account.live().react(message_id, &account.me, on.then_some(emoji));
    if let Some(old) = replaced.filter(|_| on) {
        let _ = account.ctx.reacted(conversation, message_id, &Reaction { emoji: old, file: None, count: 1, mine: true }, false);
    }
    Ok(())
}

fn own(account: &Account, message_id: &str) -> Result<u64, Fault> {
    let (sent, author) = target(message_id)?;
    if author != account.me {
        return Err(Fault::Refused("Only your own messages can be changed.".to_owned()));
    }
    Ok(sent)
}

fn edit(account: &Account, conversation: &str, message_id: &str, text: &str) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let sent = own(account, message_id)?;
    if !can(sent, now_ms(), true, conversation == account.me).edit {
        return Err(Fault::Refused("This message can no longer be edited.".to_owned()));
    }
    let (plain, ranges) = style::parse(text);
    let spans = style::spans(&plain, &ranges);
    account.wire.send(&to, Body::Edit { target: sent, text: plain.clone(), ranges })?;
    if let Ok(Some(mut message)) = account.ctx.stored_message(conversation, message_id) {
        message.text = plain;
        message.spans = spans;
        message.edited = true;
        let _ = account.ctx.message(conversation, &message);
    }
    Ok(())
}

/// The lab's signal-text-styles driver. A build without this keeps running.
pub fn prove_signal_styles() -> Result<(), String> {
    style::prove()
}

fn delete(account: &Account, conversation: &str, message_id: &str) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let (sent, author) = target(message_id)?;
    let offer = can(sent, now_ms(), author == account.me, conversation == account.me);
    // Past the window, other clients ignore a delete for everyone. Tell only this account's
    // devices, and remove it here either way.
    let body = if offer.delete_for_everyone { Body::Delete { target: sent } } else { Body::DeleteForMe { author, target: sent } };
    account.wire.send(&to, body)?;
    let _ = account.ctx.deleted(conversation, message_id);
    Ok(())
}

fn note_to_self(account: &Account, peer: &Peer) -> bool {
    matches!(peer, Peer::Direct(aci) if aci == &account.me)
}

/// The user read up to a message: the user's other devices are told, and the authors too when
/// the user's Signal settings send read receipts.
fn mark_read(account: &Account, conversation: &str, message_id: &str) {
    let Ok((upto, author)) = target(message_id) else {
        return;
    };
    let (mut read, receipts) = {
        let mut live = account.live();
        (live.read_upto(conversation, upto), live.read_receipts)
    };
    // Unread from before chatd started is not remembered: the message read stands for it.
    if read.is_empty() && author != account.me {
        read.push((author, upto));
    }
    if read.is_empty() {
        return;
    }
    if let Err(fault) = account.wire.read(&read, receipts) {
        tracing::warn!("chatd: Signal: read: {}", super::net::redact(&fault.to_string()));
    }
}

/// What a `Request::Poll` asked for, once the sentinel is read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollAct {
    /// Replace our vote. Empty takes it back.
    Vote(Vec<u32>),
    /// End the poll. Only its author may.
    End,
}

/// Packs a poll request into the edit text the Signal command thread already understands.
pub fn poll_command(votes: Option<&[u32]>) -> String {
    match votes {
        None => POLL_END.to_owned(),
        Some(votes) => {
            let list = votes.iter().take(POLL_MOST).map(|index| index.to_string()).collect::<Vec<_>>().join(",");
            format!("{POLL_VOTE}{list}")
        }
    }
}

/// The other way. `None` is an ordinary edit.
pub fn poll_action(text: &str) -> Option<PollAct> {
    if text == POLL_END {
        return Some(PollAct::End);
    }
    let rest = text.strip_prefix(POLL_VOTE)?;
    if rest.is_empty() {
        return Some(PollAct::Vote(Vec::new()));
    }
    let mut votes = Vec::new();
    for part in rest.split(',') {
        votes.push(part.parse().ok()?);
    }
    Some(PollAct::Vote(votes))
}

/// What `TINYCLIENT_PROVE=signal-poll` checks, with no account and no network: a vote, a
/// retraction and an end pack and unpack, and ordinary text does not.
pub fn prove_signal_poll() -> Result<(), String> {
    match poll_action(&poll_command(Some(&[0, 2]))) {
        Some(PollAct::Vote(indexes)) if indexes == [0, 2] => {}
        _ => return Err("a vote did not round-trip".into()),
    }
    match poll_action(&poll_command(Some(&[]))) {
        Some(PollAct::Vote(indexes)) if indexes.is_empty() => {}
        _ => return Err("taking a vote back did not round-trip".into()),
    }
    match poll_action(&poll_command(None)) {
        Some(PollAct::End) => {}
        _ => return Err("ending a poll did not round-trip".into()),
    }
    if poll_action("Dinner?").is_some() {
        return Err("plain text was read as a poll".into());
    }
    Ok(())
}

fn checked_draft(draft: &PollDraft) -> Result<(String, Vec<String>, bool), Fault> {
    let question = draft.question.trim();
    if question.is_empty() {
        return Err(Fault::Refused("A poll needs a question.".into()));
    }
    if question.chars().count() > POLL_CHARS {
        return Err(Fault::Refused("A poll's question can be at most 100 characters.".into()));
    }
    let options: Vec<String> = draft.options.iter().map(|option| option.trim().to_owned()).filter(|option| !option.is_empty()).collect();
    if options.len() < 2 {
        return Err(Fault::Refused("A poll needs at least two choices.".into()));
    }
    if options.len() > POLL_MOST {
        return Err(Fault::Refused("A poll can have at most 10 choices.".into()));
    }
    if options.iter().any(|option| option.chars().count() > POLL_CHARS) {
        return Err(Fault::Refused("A poll's choice can be at most 100 characters.".into()));
    }
    Ok((question.to_owned(), options, draft.multiple))
}

fn with_poll(account: &Account, author: &str, name: &str, sent: u64, outgoing: bool, mut poll: Poll) -> Message {
    protocol::clip(&mut poll.question, POLL_CHARS);
    let words = Text { author: author.to_owned(), name: name.to_owned(), sent, text: format!("Poll: {}", poll.question), ..Text::default() };
    let mut message = message(account, &words, outgoing);
    message.poll = Some(Box::new(poll));
    message
}

/// Records one person's vote, replacing their earlier one when `serial` is newer. `serial` of 0
/// means this vote follows the one we already have.
fn apply_ballot(poll: &mut Poll, person: &str, mut choices: Vec<u32>, serial: u32, me: &str) -> bool {
    if poll.closed {
        return false;
    }
    choices.retain(|index| (*index as usize) < poll.choices.len());
    choices.sort_unstable();
    choices.dedup();
    if !poll.multiple {
        choices.truncate(1);
    }
    let serial = if serial == 0 { poll.ballots.iter().find(|ballot| ballot.person == person).map(|ballot| ballot.serial.saturating_add(1)).unwrap_or(1) } else { serial };
    if let Some(existing) = poll.ballots.iter().find(|ballot| ballot.person == person) {
        if serial <= existing.serial {
            return false;
        }
    }
    if poll.ballots.len() >= BALLOTS_KEPT && !poll.ballots.iter().any(|ballot| ballot.person == person) {
        poll.ballots.remove(0);
    }
    poll.ballots.retain(|ballot| ballot.person != person);
    poll.ballots.push(PollBallot { person: person.to_owned(), choices, serial });
    recount(poll, me);
    true
}

fn recount(poll: &mut Poll, me: &str) {
    let ballots = poll.ballots.clone();
    for choice in &mut poll.choices {
        choice.voters.clear();
        choice.count = 0;
    }
    for ballot in &ballots {
        for index in &ballot.choices {
            if let Some(choice) = poll.choices.get_mut(*index as usize) {
                choice.count = choice.count.saturating_add(1);
                if choice.voters.len() < protocol::POLL_VOTERS {
                    choice.voters.push(ballot.person.clone());
                }
            }
        }
    }
    poll.mine = ballots.into_iter().find(|ballot| ballot.person == me).map(|ballot| ballot.choices).unwrap_or_default();
}

fn chosen(poll: &Poll, indexes: &[u32]) -> Result<Vec<u32>, Fault> {
    if indexes.is_empty() {
        return Ok(Vec::new());
    }
    if indexes.iter().any(|index| *index as usize >= poll.choices.len()) {
        return Err(Fault::Refused("That choice is not on this poll.".into()));
    }
    let mut indexes = indexes.to_vec();
    indexes.sort_unstable();
    indexes.dedup();
    if !poll.multiple && indexes.len() > 1 {
        return Err(Fault::Refused("This poll takes one choice.".into()));
    }
    Ok(indexes)
}

fn send_poll(account: &Account, conversation: &str, draft: &PollDraft, _reply_to: Option<&str>) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let (question, options, multiple) = checked_draft(draft)?;
    let sent = account.wire.send(&to, Body::Poll { question: question.clone(), options: options.clone(), multiple })?;
    account.live().sent(sent, conversation);
    Ok(with_poll(account, &account.me, &account.name, sent, true, Poll::open(question, options, multiple)))
}

fn poll_act(account: &Account, conversation: &str, message_id: &str, act: PollAct) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let (sent, author) = target(message_id)?;
    let Some(mut message) = account.ctx.stored_message(conversation, message_id).ok().flatten() else {
        return Err(Fault::Refused("That message is no longer there.".into()));
    };
    let Some(poll) = message.poll.as_ref() else {
        return Err(Fault::Refused("That is not a poll.".into()));
    };
    match act {
        PollAct::End => {
            if author != account.me {
                return Err(Fault::Refused("Only the person who started this poll can end it.".into()));
            }
            if poll.closed {
                return Ok(());
            }
            account.wire.send(&to, Body::PollEnd { target: sent })?;
            if let Some(poll) = message.poll.as_mut() {
                poll.closed = true;
            }
            let _ = account.ctx.message(conversation, &message);
            Ok(())
        }
        PollAct::Vote(indexes) => {
            if poll.closed {
                return Err(Fault::Refused("This poll has ended.".into()));
            }
            let choices = chosen(poll, &indexes)?;
            let serial = poll.ballots.iter().find(|ballot| ballot.person == account.me).map(|ballot| ballot.serial.saturating_add(1)).unwrap_or(1);
            account.wire.send(&to, Body::PollVote { author, target: sent, indexes: choices.clone(), serial })?;
            if let Some(poll) = message.poll.as_mut() {
                apply_ballot(poll, &account.me, choices, serial, &account.me);
            }
            let _ = account.ctx.message(conversation, &message);
            Ok(())
        }
    }
}

fn typing(account: &Account, conversation: &str) {
    let Some(to) = Peer::parse(conversation) else {
        return;
    };
    {
        let mut live = account.live();
        if !live.typing || live.typed.get(conversation).is_some_and(|told| told.elapsed() < TYPING_EVERY) {
            return;
        }
        live.typed.retain(|_, told| told.elapsed() < TYPING_EVERY);
        live.typed.insert(conversation.to_owned(), Instant::now());
    }
    let _ = account.wire.typing(&to, true);
}

/// What the message menu may offer. `sent_ms` and `now_ms` are milliseconds. `own` is a message
/// this account sent. `note_to_self` is the chat with oneself, which has no delete-for-everyone limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub edit: bool,
    /// Delete for everyone. Note to Self has no time limit; every other chat has one day.
    pub delete_for_everyone: bool,
    /// Always. This removes the message on this account's devices only.
    pub delete_for_me: bool,
}

/// Signal Desktop sends an edit or a delete for everyone for one day. Delete for me is always offered.
pub fn can(sent_ms: u64, now_ms: u64, own: bool, note_to_self: bool) -> Offer {
    let within_day = now_ms.saturating_sub(sent_ms) <= SEND_WINDOW_MS;
    Offer { edit: own && within_day, delete_for_everyone: own && (note_to_self || within_day), delete_for_me: true }
}

/// An incoming edit or delete for everyone is kept for two days, and without a limit in Note to Self.
pub fn accept_remote(sent_ms: u64, now_ms: u64, note_to_self: bool) -> bool {
    note_to_self || now_ms.saturating_sub(sent_ms) <= RECEIVE_WINDOW_MS
}

/// The lab driver for `signal-delete-for-me`. A build that offers a late delete for everyone fails.
pub fn prove_signal_delete_for_me() -> Result<(), String> {
    let hour = 60 * 60 * 1000;
    let now = 50 * hour;
    let recent = can(now - hour, now, true, false);
    if !recent.edit || !recent.delete_for_everyone || !recent.delete_for_me {
        return Err("a recent message must offer an edit and both deletes".into());
    }
    let late = can(now - 25 * hour, now, true, false);
    if late.edit || late.delete_for_everyone || !late.delete_for_me {
        return Err("a day-old message must offer only delete for me".into());
    }
    let note = can(now - 25 * hour, now, true, true);
    if !note.delete_for_everyone || note.edit {
        return Err("note to self keeps delete for everyone and still closes the edit window".into());
    }
    let theirs = can(now - hour, now, false, false);
    if theirs.edit || theirs.delete_for_everyone || !theirs.delete_for_me {
        return Err("someone else's message offers only delete for me".into());
    }
    if !accept_remote(now - 47 * hour, now, false) || accept_remote(now - 49 * hour, now, false) {
        return Err("an incoming delete for everyone is kept for two days".into());
    }
    if !accept_remote(now - 49 * hour, now, true) {
        return Err("note to self keeps an old delete for everyone".into());
    }
    Ok(())
}

/// The lab's signal-send-file driver. A build without the hook stays up, so the driver fails there.
pub fn prove_signal_send_file() -> Result<(), String> {
    let can = files::capabilities();
    if can.max_file_bytes != files::MAX_FILE_BYTES || can.max_files != files::MAX_FILES || can.caption_limit != 0 {
        return Err("files are not offered".into());
    }
    if files::blurhash(&[(255, 0, 0)], 1, 1, 1, 1) != "00TI:j" {
        return Err("a solid red picture did not hash".into());
    }
    if !files::replace_outgoing_sync(true) || files::replace_outgoing_sync(false) {
        return Err("a phone copy would replace the file".into());
    }
    let line = files::call_line("b2b2b2b2-0000-4000-8000-000000000002", &["card.png".into()], "the hand", None);
    if line != "send b2b2b2b2-0000-4000-8000-000000000002 files card.png the hand" {
        return Err("the send was not recorded as a file".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_vote_is_not_an_edit() {
        prove_signal_poll().unwrap();
    }

    #[test]
    fn a_session_names_one_account_and_says_nothing_of_its_key() {
        let session = r#"{"aci":"0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b","name":"Ada","vault":"00ff"}"#;
        let found = accounts(session).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].account.as_str(), found[0].name.as_str()), ("0f6c2a3e-1b4d-4e5f-8a9b-0c1d2e3f4a5b", "Ada"));
        assert!(accounts(r#"{"aci":"../etc"}"#).is_err());
        assert_eq!(without(session, &found[0].account), None);
        assert!(!protocol::redact(session).contains("00ff"));
    }

    #[test]
    fn a_new_reaction_from_the_same_person_replaces_theirs() {
        let mut live = Live::default();
        assert_eq!(live.react("m", "p", Some("👍")), None);
        assert_eq!(live.react("m", "p", Some("❤️")), Some("👍".to_owned()));
        assert_eq!(live.react("m", "p", Some("❤️")), None);
        assert_eq!(live.react("m", "p", None), Some("❤️".to_owned()));
    }

    #[test]
    fn reaction_record_bounds_size_and_skips_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acct.reactions");

        // Pre-populate more than REACTIONS_KEPT rows; loading + writing prunes to REACTIONS_KEPT newest.
        let rows: Vec<KeptReaction> = (0..=REACTIONS_KEPT)
            .map(|i| KeptReaction { message: format!("m{i}"), person: "p".into(), emoji: "👍".into() })
            .collect();
        std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();

        let mut live = Live { reactions_path: path.clone(), ..Live::default() };
        assert_eq!(live.react("m0", "p", Some("❤️")), None, "oldest row beyond cap was evicted");
        let kept = read_reactions(&path);
        assert_eq!(kept.len(), REACTIONS_KEPT);
        assert_eq!(kept.last().map(|r| (r.message.as_str(), r.emoji.as_str())), Some(("m0", "❤️")));
        assert!(!path.with_extension("tmp").exists(), "temp file was renamed away");

        // An oversized file is skipped on load rather than read into memory.
        let oversized = vec![b' '; (REACTIONS_FILE_LIMIT as usize) + 1];
        std::fs::write(&path, oversized).unwrap();
        assert!(read_reactions(&path).is_empty());
    }

    #[test]
    fn a_username_shape_is_proven() {
        prove_signal_username().unwrap();
    }

    #[test]
    fn reading_takes_what_is_older_and_leaves_what_is_newer() {
        let mut live = Live::default();
        for sent in [1, 2, 3] {
            live.unread("c", "a", sent);
        }
        assert_eq!(live.read_upto("c", 2), vec![("a".to_owned(), 1), ("a".to_owned(), 2)]);
        assert_eq!(live.unread["c"].len(), 1);
        assert_eq!(live.read_upto("c", 3).len(), 1);
        assert!(!live.unread.contains_key("c"));
    }

    #[test]
    fn a_profile_edit_is_not_a_status() {
        let packed = profile_status(Some("Ada"), Some("New about"), Some(""));
        let (name, about, emoji) = profile_edit(&packed).unwrap();
        assert_eq!((name.as_deref(), about.as_deref(), emoji.as_deref()), (Some("Ada"), Some("New about"), Some("")));
        assert!(profile_edit("Lunch").is_none());
        assert!(prove_signal_own_profile().is_ok());
    }

    #[test]
    fn a_quote_quotes_the_words_without_the_note() {
        let body = "y".repeat(2_048);
        assert_eq!(quoted_words(&format!("{body}{}", wire::CONTINUES)), body);
        assert_eq!(quoted_words("Tuesday, seven sharp."), "Tuesday, seven sharp.");
    }

    #[test]
    fn group_revisions_gate_connect_stale_duplicate_and_newer() {
        let mut revisions = GroupRevisions::default();
        let initial = GroupSnap { title: "Bridge club".into(), ..GroupSnap::default() };
        assert!(!revisions.contains("g1"));
        assert!(!revisions.should_check("g1", 1));
        assert_eq!(revisions.advance("g1", 1, initial.clone()), None);

        // First snapshot is seeded silently and not overwritten by a second seed.
        revisions.seed("g1".into(), 5, initial.clone());
        assert!(revisions.contains("g1"));
        let renamed = GroupSnap { title: "Wednesday club".into(), ..GroupSnap::default() };
        revisions.seed("g1".into(), 6, renamed.clone());

        // Stale and duplicate revisions are skipped.
        assert!(!revisions.should_check("g1", 4));
        assert!(!revisions.should_check("g1", 5));
        assert_eq!(revisions.advance("g1", 4, renamed.clone()), None);
        assert_eq!(revisions.advance("g1", 5, renamed.clone()), None);

        // A newer revision is announced once, then skipped on repeat.
        assert!(revisions.should_check("g1", 6));
        let before = revisions.advance("g1", 6, renamed.clone()).expect("newer revision");
        assert_eq!(before, initial);
        assert!(!revisions.should_check("g1", 6));
        assert_eq!(revisions.advance("g1", 6, renamed), None);
    }

    #[test]
    fn rejoining_a_group_sets_rejoined_and_clears_left() {
        let out = GroupSnap::default();
        let inside = GroupSnap { members: vec![MemberSnap { id: "me".into(), admin: false }], ..GroupSnap::default() };
        let left = group_delta(&inside, &out, "me", |_| String::new());
        assert!(left.left && !left.rejoined);
        let back = group_delta(&out, &inside, "me", |_| String::new());
        assert!(!back.left && back.rejoined);
        assert_eq!(back.lines, ["You joined the group"]);
    }
}
