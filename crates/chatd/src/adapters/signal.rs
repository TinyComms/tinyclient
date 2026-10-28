//! Signal: text first. This computer is linked to the user's account as one more device, the way
//! Signal Desktop is; it never registers a phone number, which would take the account from the
//! phone. Calls and voice stay out of chatd.
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
#[cfg(feature = "signal")]
mod real;
pub mod signin;
#[cfg(feature = "signal")]
mod vault;
mod wire;

use super::{keep_connected, serve_commands, Command, Context, Failure, Refusal, Signed};
use crate::session;
use crate::store::{ConversationRow, PersonRow};
use protocol::{ConversationKind, Link, Message, MessageKind, Reaction, Relation, ReplyTo};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wire::{Body, Event, Fault, Peer, Quote, Text, Wire};

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
/// Our sent messages remembered, for the receipt that says where one was read.
const SENT_KEPT: usize = 1_024;
/// Reactions remembered, so a new one from the same person replaces their old one.
const REACTIONS_KEPT: usize = 4_096;
/// What a person with no name we know is called.
const NO_NAME: &str = "Signal user";
/// Most pictures asked of Signal for one change to what is on screen.
const PICTURES: usize = 25;

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
    reactions: HashMap<(String, String), String>,
    reacted_order: VecDeque<(String, String)>,
    typed: HashMap<String, Instant>,
    /// The user's own Signal settings. Off until the phone says.
    read_receipts: bool,
    typing: bool,
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

    /// Records a person's reaction, answering the one it replaced.
    fn react(&mut self, message: &str, by: &str, emoji: Option<&str>) -> Option<String> {
        let key = (message.to_owned(), by.to_owned());
        let before = match emoji {
            Some(emoji) => {
                if !self.reactions.contains_key(&key) {
                    if self.reacted_order.len() >= REACTIONS_KEPT {
                        if let Some(old) = self.reacted_order.pop_front() {
                            self.reactions.remove(&old);
                        }
                    }
                    self.reacted_order.push_back(key.clone());
                }
                self.reactions.insert(key, emoji.to_owned())
            }
            None => self.reactions.remove(&key),
        };
        before.filter(|before| Some(before.as_str()) != emoji)
    }
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
    // What is learned (names, settings, unread) outlives one connection.
    let live: Arc<Mutex<Live>> = Arc::default();
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
        Err(err) => eprintln!("chatd: Signal: {err}"),
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
    let words = match (&text.unsupported, text.text.is_empty()) {
        (Some(what), true) => what.clone(),
        (Some(what), false) => format!("{what}: {}", text.text),
        (None, _) => text.text.clone(),
    };
    let sent = i64::try_from(text.sent / 1_000).unwrap_or(0);
    let mut message = Message::plain(wire::message_id(text.sent, author), name, words, sent, outgoing);
    message.sender.id = author.to_owned();
    message.mentions_me = text.mentions_me && !outgoing;
    if text.unsupported.is_some() {
        message.kind = MessageKind::Unsupported { link: String::new() };
    }
    message.reply_to = text.quote.as_ref().map(|quote| reply_to(account, quote));
    message
}

fn reply_to(account: &Account, quote: &Quote) -> ReplyTo {
    let sender = if quote.author == account.me { account.name.clone() } else { account.live().name(&quote.author) };
    ReplyTo {
        id: wire::message_id(quote.sent, &quote.author),
        sender,
        snippet: quote.text.chars().take(200).collect(),
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
                row.avatar = ctx.avatar_of(&row.id);
                let _ = ctx.conversation(&row);
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
            let message = message(account, &text, outgoing);
            if outgoing {
                account.live().sent(text.sent, &conversation);
            } else {
                account.live().unread(&conversation, &text.author, text.sent);
            }
            let _ = ctx.message(&conversation, &message);
        }
        Event::Edited { peer, author, target, text } => {
            let conversation = peer.conversation();
            if let Ok(Some(mut message)) = ctx.stored_message(&conversation, &wire::message_id(target, &author)) {
                message.text = text;
                message.edited = true;
                let _ = ctx.message(&conversation, &message);
            }
        }
        Event::Deleted { peer, author, target } => {
            let _ = ctx.deleted(&peer.conversation(), &wire::message_id(target, &author));
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

/// Answers a command while there is no connection.
fn offline(ctx: &Context, command: Command) {
    let down = || Refusal::Refused("Signal is offline.".to_owned());
    match command {
        Command::Send { conversation_id, client_id, .. } => ctx.sent(&conversation_id, &client_id, Err("Signal is offline.".to_owned())),
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

fn serve(account: &Arc<Account>, command: Command) {
    let ctx = &account.ctx;
    match command {
        Command::MarkRead { conversation_id, message_id } => mark_read(account, &conversation_id, &message_id),
        Command::Send { conversation_id, client_id, text, reply_to } => {
            let outcome = send_text(account, &conversation_id, &text, reply_to.as_deref());
            ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
        }
        Command::React { conversation_id, message_id, emoji, on, reply } => {
            let _ = reply.send(react(account, &conversation_id, &message_id, &emoji, on).map_err(refused));
        }
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        Command::Edit { conversation_id, message_id, text, reply } => {
            let _ = reply.send(edit(account, &conversation_id, &message_id, &text).map_err(refused));
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
        // Signal has no directory to search: the people known are the contacts already stored.
        Command::SearchPeople(query) => ctx.searched(&query),
        Command::Watch(watching) => pictures(account, &watching),
        Command::SignOut(reply) => {
            if let Err(fault) = account.wire.unlink() {
                eprintln!("chatd: Signal: unlinking: {}", super::net::redact(&fault.to_string()));
            }
            let _ = reply.send(Ok(()));
        }
        other => other.unsupported(ctx),
    }
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
                    eprintln!("chatd: Signal: picture: {}", super::net::redact(&fault.to_string()));
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
        eprintln!("chatd: Signal: {err}");
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

fn send_text(account: &Account, conversation: &str, text: &str, reply_to: Option<&str>) -> Result<Message, Fault> {
    let to = peer(conversation)?;
    let quote = match reply_to.map(target).transpose()? {
        Some((sent, author)) => {
            let text = account.ctx.stored_message(conversation, &wire::message_id(sent, &author)).ok().flatten().map_or_else(String::new, |m| m.text);
            Some(Quote { sent, author, text })
        }
        None => None,
    };
    let sent = account.wire.send(&to, Body::Text { text: text.to_owned(), quote: quote.clone() })?;
    account.live().sent(sent, conversation);
    let words = Text { author: account.me.clone(), name: account.name.clone(), sent, text: text.to_owned(), quote, ..Text::default() };
    Ok(message(account, &words, true))
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
    account.wire.send(&to, Body::Edit { target: sent, text: text.to_owned() })?;
    if let Ok(Some(mut message)) = account.ctx.stored_message(conversation, message_id) {
        message.text = text.to_owned();
        message.edited = true;
        let _ = account.ctx.message(conversation, &message);
    }
    Ok(())
}

fn delete(account: &Account, conversation: &str, message_id: &str) -> Result<(), Fault> {
    let to = peer(conversation)?;
    let sent = own(account, message_id)?;
    account.wire.send(&to, Body::Delete { target: sent })?;
    let _ = account.ctx.deleted(conversation, message_id);
    Ok(())
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
        eprintln!("chatd: Signal: read: {}", super::net::redact(&fault.to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
