//! Telegram: the one network whose terms welcome a third-party client. Chat first; calls hand
//! off to Telegram's own app for now.
//!
//! # The library: grammers, in this process (decided 2026-10-13, by measurement on this Mac)
//!
//! | | TDLib (C++, Telegram's own) | grammers 0.10 (pure Rust MTProto) |
//! | --- | --- | --- |
//! | Builds here | No: needs CMake, which Command Line Tools lack. Usable only prebuilt. | Yes, 27 s cold, once `glass_pumpkin` is pinned (see `Cargo.toml`). |
//! | Prebuilt | None from Telegram. npm `@prebuilt-tdlib/darwin-arm64` (one maintainer's CI, ad-hoc signed, static OpenSSL, needs macOS 14) is the only shippable one; PyPI `tdjson` bundles end-of-life OpenSSL 1.1.1; Homebrew's bottle is TDLib 1.8.0 from 2021 and links Homebrew's OpenSSL. | Not needed. |
//! | On disk | 31.9 MB dylib (11.7 MB compressed). | Linked in; see the README's size table. |
//! | Idle memory | +2.8 MB footprint loaded with no parameters set: a floor. With its database and a connection it could not be measured without an app id; the docs' estimate is 20 to 50 MB. | +0.2 MB footprint, +1.2 MB RSS over an empty process, measured connected to a datacenter with the key exchange done. |
//! | Second cache | Its own SQLite beside ours: every message stored twice. | None: messages go to chatd's store as they arrive. |
//! | Maintained | By Telegram. | One maintainer plus contributors; archived on GitHub 2026-02-10 and moved to Codeberg, where it is active: 0.10.0 on 2026-07-02, schema layer bumped 2026-10-10, commits the day this was written. |
//! | Covers | Everything, secret chats and call signalling included. | Phone login and 2FA as methods; QR login, folders, topics, reactions, typing, read state and files through its raw request call, which reaches the whole schema. No secret chats, no calls. |
//!
//! grammers wins on the two things this app is about, weight and one cache, and it is the only
//! one of the two that can be built on the machine tinyclient is developed on. What it costs: a
//! schema change at Telegram waits on one maintainer, secret chats are out, and calls (a later
//! stage, tgcalls either way) will need their signalling written here. If grammers is ever
//! abandoned, everything above `wire::Wire` speaks Telegram's schema, not grammers, so the
//! transport can be replaced without touching the mapping.
//!
//! chatd stays inside its budget, so Telegram is not a process of its own. grammers is async:
//! one current-thread tokio runtime, on the adapter's own thread (`real`), built when an account
//! exists or a sign-in begins and at no other time.
//!
//! # The app id
//!
//! Telegram wants one registered `api_id` per app, shipped in the binary and kept out of the
//! repository. It is read at build time from `TINYCLIENT_TELEGRAM_API_ID` and
//! `TINYCLIENT_TELEGRAM_API_HASH`. Built without them, Telegram is `Unsupported` and says why.
//!
//! # The session
//!
//! One keychain item, like every network: the keys Telegram issued (256 bytes a datacenter),
//! the home datacenter and whose they are, as JSON (`Saved`). Nothing of it is on disk.
//!
//! # What the terms bind (`docs/product/08-sign-in-accounts.md`)
//!
//! Reading is told to Telegram exactly when the shell says a message was seen, typing while the
//! user types, and being here when the user acts; no setting hides any of the three. Nothing is
//! automated. A flood wait is slept in full or told to the user, never cut short. A channel's
//! sponsored message is asked when the channel is opened (at most every five minutes) and told
//! as viewed once it is on screen.
//!
//! # Unproven
//!
//! Nobody has signed in through this yet: there was no app id and no account when it was
//! written. The mapping, the choice of requests and the sign-in steps are tested through chatd's
//! socket against a scripted wire (`fake`); `real` has only been run as far as a datacenter's
//! refusal of a missing app id.

mod catalog;
mod content;
mod fake;
mod files;
mod filing;
mod info;
mod live;
mod peers;
pub(crate) mod react;
mod real;
mod send;
pub mod signin;
mod sync;
mod wire;

use super::{keep_connected, serve_commands, Command, Context, Failure, Members, Refusal, Signed};
use crate::session;
use grammers_tl_types as tl;
use peers::{Directory, Who};
use protocol::{Link, MemberSection, Person, Presence, Relation};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wire::{Event, Fault, Wire};

/// The pause after a history fetch.
const PACE: Duration = Duration::from_millis(300);
/// Messages asked for in one page of history.
const PAGE: i32 = 50;
/// Others are told the user is typing at most this often per conversation (budget rule 8).
/// Telegram shows it for six seconds and its own apps renew it every five; less is never an
/// anomaly.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// How long "is typing" stays without another signal.
const TYPING_SHOWN: i64 = 6;
/// Being here is told at most this often: when the connection opens, on a cadence while it
/// lives, and when the user acts. Telegram lets it lapse by itself, so going away is never sent.
const HERE_EVERY: Duration = Duration::from_secs(240);
/// Messages whose conversation is remembered, for a deletion that names only the message.
const PLACED: usize = 4_096;

/// The registered app id, when this build was given one.
pub fn credentials() -> Option<(i32, &'static str)> {
    let id = option_env!("TINYCLIENT_TELEGRAM_API_ID")?.trim().parse().ok()?;
    let hash = option_env!("TINYCLIENT_TELEGRAM_API_HASH")?.trim();
    (id > 0 && !hash.is_empty()).then_some((id, hash))
}

/// A test in a scratch directory asked for the scripted wire.
fn scripted() -> bool {
    !session::enabled() && std::env::var_os("TINYCLIENT_TELEGRAM_FAKE").is_some()
}

pub fn supported() -> bool {
    credentials().is_some() || scripted()
}

/// Why Telegram cannot be used in this build, for the sign-in sheet.
pub fn unsupported_reason() -> String {
    "This build of tinyclient was made without a Telegram app id, so it cannot sign in to Telegram.".to_owned()
}

/// What the keychain holds for Telegram. Not `Debug`: it is the account's keys.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    pub user: i64,
    #[serde(default)]
    pub name: String,
    pub home: i32,
    #[serde(default)]
    pub dcs: Vec<SavedDc>,
}

/// One datacenter's key, in hex, and where the datacenter is.
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedDc {
    pub id: i32,
    pub v4: String,
    pub v6: String,
    pub key: String,
}

fn parse(session: &str) -> Result<Saved, String> {
    serde_json::from_str(session).map_err(|_| "the Telegram session is unreadable".to_owned())
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let saved = parse(session)?;
    Ok(vec![Signed {
        account: saved.user.to_string(),
        name: if saved.name.is_empty() { "Telegram".to_owned() } else { saved.name },
        session: session.to_owned(),
    }])
}

/// One sign-in is one account, so nothing is left once it goes.
pub fn without(_session: &str, _account: &str) -> Option<String> {
    None
}

/// A wire for this session, or for a sign-in when there is none yet.
fn open(saved: Option<&Saved>) -> Result<Box<dyn Wire>, String> {
    if scripted() {
        return Ok(Box::new(fake::Fake::open(saved.is_some())));
    }
    let (api_id, _) = credentials().ok_or_else(unsupported_reason)?;
    Ok(Box::new(real::Real::open(api_id, saved)?))
}

/// Small things remembered while connected. All bounded.
#[derive(Default)]
struct Live {
    /// Where recent messages are, for the updates that name a message and not its place: a
    /// deletion outside a channel, anything inside a forum.
    placed: VecDeque<((Who, i32), String)>,
    /// Messages we sent and the id the shell knows each by, until the echo has passed.
    sent: VecDeque<(i32, String)>,
    random: VecDeque<(i64, String)>,
    typed: HashMap<String, Instant>,
    here: Option<Instant>,
    /// Whether people, groups and channels are muted when a chat has no setting of its own.
    muted: [bool; 3],
    /// A default mute changed, which only reading the chats again shows. Pins and folders
    /// never need it: they are told row by row (`filing`).
    stale: bool,
    /// Conversations a `HistoryAfter` was served for, so reading on does not ask twice.
    after: VecDeque<(String, String, Instant)>,
    /// How the chats are filed: folders, pins, archive, and where each list continues.
    filed: filing::Filed,
    /// Pictures already queued this connection, so a row on screen is fetched once.
    pictured: HashSet<String>,
    /// Channels whose sponsored messages were asked, and when (asked again after 5 minutes).
    ads: Vec<(String, Instant)>,
    /// Sponsored messages recorded: the channel, the ad's id, and whether it was told as viewed.
    shown: Vec<(String, Vec<u8>, bool)>,
    /// What the picker's items send.
    picks: VecDeque<(String, catalog::Pick)>,
    /// The installed sticker packs, from the first page of the Stickers tab.
    sticker_sets: Option<Vec<(i64, i64, String)>>,
    /// Scheduled sends shown until Telegram sends them: the chat, the text, the placeholder.
    scheduled: VecDeque<(String, String, String)>,
    /// Telegram's reaction emoji, and whether each needs Premium, once asked (`react`).
    reactions: Option<Vec<(String, bool)>>,
}

/// Telegram's own send options, which Telegram carries out.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Options {
    pub silent: bool,
    pub no_webpage: bool,
    /// Unix seconds: Telegram sends it then.
    pub schedule: Option<i32>,
}

/// One connection's worth of an account: what every thread of the adapter shares.
pub(super) struct Account {
    ctx: Context,
    wire: Box<dyn Wire>,
    me: i64,
    name: String,
    dir: Mutex<Directory>,
    live: Mutex<Live>,
    files: files::Files,
}

impl Account {
    fn dir(&self) -> MutexGuard<'_, Directory> {
        self.dir.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One request, with Telegram's flood waits honored.
    fn ask<R: tl::RemoteCall>(&self, request: &R) -> Result<R::Return, Fault> {
        wire::patient(self.wire.as_ref(), &|time| self.ctx.pause(time), request)
    }

    /// Takes in the people and chats an answer or update named: the directory, the hashes kept
    /// for the next run, and the people's rows.
    fn learn(&self, users: &[tl::enums::User], chats: &[tl::enums::Chat]) {
        if users.is_empty() && chats.is_empty() {
            return;
        }
        let learned = self.dir().learn(users, chats);
        for (key, note) in &learned.notes {
            self.ctx.set_note(key, note);
        }
        if !learned.people.is_empty() {
            let _ = self.ctx.people(&learned.people);
            let _ = self.ctx.details(&learned.details);
        }
        for who in learned.chats {
            sync::refresh_row(self, who);
        }
    }

    /// How a peer is addressed. One seen in an earlier run is recalled from its note.
    fn input(&self, who: Who) -> Result<tl::enums::InputPeer, Fault> {
        let mut dir = self.dir();
        if dir.input_peer(who).is_none() {
            if let Some(note) = self.ctx.note(&peers::note_key(who)) {
                dir.recall(who, &note);
            }
        }
        dir.input_peer(who).ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })
    }

    fn input_channel(&self, who: Who) -> Result<tl::enums::InputChannel, Fault> {
        self.input(who)?;
        self.dir().input_channel(who).ok_or_else(|| Fault::Rpc { code: 400, name: "CHANNEL_INVALID".to_owned(), value: None })
    }

    /// Remembers where a message is.
    fn place(&self, who: Who, message: i32, conversation: &str) {
        // A channel that is not a forum says where its messages are by itself.
        if matches!(who, Who::Channel(_)) && !conversation.contains('_') {
            return;
        }
        let mut live = self.live();
        if live.placed.len() >= PLACED {
            live.placed.pop_front();
        }
        live.placed.push_back(((who, message), conversation.to_owned()));
    }

    fn placed(&self, who: Option<Who>, message: i32) -> Option<String> {
        let live = self.live();
        let found = live.placed.iter().rev().find(|((peer, id), _)| *id == message && match who {
            Some(who) => *peer == who,
            None => !matches!(peer, Who::Channel(_)),
        });
        found.map(|(_, conversation)| conversation.clone())
    }

    /// Writes the session out again when a key was made for another datacenter.
    fn keep_session(&self) {
        if self.wire.unsaved() {
            let fresh = self.wire.session(self.me, &self.name);
            self.ctx.save_session(|_| Some(fresh));
        }
    }

    /// The user did something only a person at the keyboard does: they are here. Telegram's own
    /// apps say so while their window is in use, and the terms rule out hiding it.
    fn here(&self) {
        {
            let mut live = self.live();
            if live.here.is_some_and(|told| told.elapsed() < HERE_EVERY) {
                return;
            }
            live.here = Some(Instant::now());
        }
        let _ = wire::call(self.wire.as_ref(), &tl::functions::account::UpdateStatus { offline: false });
    }
}

fn refused(fault: Fault) -> Refusal {
    Refusal::Refused(fault.words())
}

/// A user's last-seen as the contract's presence. Where Telegram hides the time ("recently",
/// "within a week") the person is not known to be offline, so nothing is claimed.
pub(super) fn presence(status: Option<&tl::enums::UserStatus>) -> Presence {
    match status {
        Some(tl::enums::UserStatus::Online(_)) => Presence::Online,
        Some(tl::enums::UserStatus::Offline(_)) => Presence::Offline,
        _ => Presence::Unknown,
    }
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.user.to_string() != ctx.account.account {
        return Err("the Telegram session is for another account".to_owned());
    }
    if !supported() {
        return Err(unsupported_reason());
    }
    // The command thread serves whichever connection is current; with none, commands that need
    // the network are answered as offline.
    let current: Arc<Mutex<Option<Arc<Account>>>> = Arc::default();
    let (socket_ctx, held) = (ctx.clone(), current.clone());
    std::thread::Builder::new()
        .name("telegram-events".into())
        .spawn(move || {
            keep_connected(&socket_ctx, || connect(&socket_ctx, &saved, &held));
            held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        })
        .map_err(|err| err.to_string())?;
    std::thread::Builder::new()
        .name("telegram-commands".into())
        .spawn(move || {
            let account = || current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            serve_commands(
                &ctx,
                inbox,
                PACE,
                |id| match account() {
                    Some(account) => sync::backfill(&account, id),
                    // The link is down. The store holds its answer already and the ask waits
                    // for the next connect, where it fills what was opened.
                    None => held_back(id.to_owned()),
                },
                |command| match account() {
                    Some(account) => serve(&account, command),
                    None => offline(&ctx, command),
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
fn connect(ctx: &Context, saved: &Saved, current: &Mutex<Option<Arc<Account>>>) -> Failure {
    let wire = match open(Some(saved)) {
        Ok(wire) => wire,
        Err(why) => return Failure::Dropped(why),
    };
    let Some(events) = wire.events() else {
        return Failure::Dropped("the wire has no events".to_owned());
    };
    let account = Arc::new(Account {
        files: files::Files::start(ctx),
        ctx: ctx.clone(),
        wire,
        me: saved.user,
        name: saved.name.clone(),
        dir: Mutex::new(Directory::new(saved.user)),
        live: Mutex::default(),
    });
    // A long flood wait at sign-on is told on the account and waited out here, in full.
    let ready = loop {
        match sync::sign_on(&account) {
            Err(fault) => match fault.flood() {
                Some(seconds) => {
                    ctx.attention(&wire::wait_words(seconds));
                    if ctx.pause(Duration::from_secs(seconds)) {
                        return Failure::Dropped("stopped".to_owned());
                    }
                    ctx.attention("");
                }
                None => break Err(fault),
            },
            Ok(()) => break Ok(()),
        }
    };
    if let Err(fault) = ready {
        return match fault.signed_out() {
            true => Failure::SignedOut("Telegram ended this session.".to_owned()),
            false => Failure::Dropped(fault.to_string()),
        };
    }
    account.files.attach(&account);
    *current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(account.clone());
    // Dropped while signing on: the command thread has gone and will not close this wire.
    if ctx.stopped() {
        return Failure::Dropped("stopped".to_owned());
    }
    ctx.link(Link::Connected);
    account.keep_session();
    // The asks the link was down for go before anything else: the user's open stands, and
    // the chats the sign-on read have the rows the ask refers to.
    for id in held_take() {
        sync::backfill(&account, &id);
    }
    // Telegram fades an account to offline by itself, so being here is said on opening the
    // connection and then on a cadence while it lives; `here` keeps it to one call per
    // HERE_EVERY however often it is asked. The event loop is otherwise idle, so its wait is
    // that cadence: a tick with nothing to read sends it. Nothing outlives the connection.
    account.here();
    let mut said = Instant::now();
    let ended = loop {
        let wait = HERE_EVERY.saturating_sub(said.elapsed());
        match events.recv_timeout(wait) {
            Ok(Event::Update { update, users, chats }) => {
                account.learn(&users, &chats);
                live::apply(&account, &update);
            }
            Ok(Event::Closed(why)) => break why,
            Err(RecvTimeoutError::Timeout) => {
                account.here();
                said = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => break "the connection closed".to_owned(),
        }
        if ctx.stopped() {
            break "stopped".to_owned();
        }
    };
    current.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    // Whether the session is over is asked of the wire that just failed: a revoked session
    // closes the connection first and says why only when asked.
    match wire::call(account.wire.as_ref(), &tl::functions::updates::GetState {}) {
        Err(fault) if fault.signed_out() => Failure::SignedOut("Telegram ended this session.".to_owned()),
        _ => Failure::Dropped(ended),
    }
}

#[derive(Default)]
struct Held {
    /// Ids of conversations to fill, newest last; one read twice is still one look.
    backs: VecDeque<String>,
}

/// Asks the shell made while the link was down, that the network hears at the next
/// connect: the conversation the user opened, whose store answer remains until then. One
/// ask once, at most this many, and gone the moment the session ends (a sign-out), so
/// nothing held outlives the account it was asked for.
static HELD: Mutex<Held> = Mutex::new(Held { backs: VecDeque::new() });

const HELD_BACKS: usize = 32;

fn held_back(id: String) {
    let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    held.backs.retain(|held| *held != id);
    held.backs.push_back(id);
    while held.backs.len() > HELD_BACKS {
        held.backs.pop_front();
    }
}

fn held_take() -> VecDeque<String> {
    std::mem::take(&mut HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).backs)
}

/// Answers a command while there is no connection.
fn offline(ctx: &Context, command: Command) {
    let down = || Refusal::Refused("Telegram is offline.".to_owned());
    match command {
        Command::Send { conversation_id, client_id, .. } => ctx.sent(&conversation_id, &client_id, Err("Telegram is offline.".to_owned())),
        Command::React { reply, .. } | Command::Edit { reply, .. } | Command::Delete { reply, .. } => drop(reply.send(Err(down()))),
        Command::Members { reply, .. } => drop(reply.send(Err(down()))),
        Command::Profile { reply, .. } | Command::ProfileIn { reply, .. } => drop(reply.send(Err(down()))),
        Command::ConversationInfo { reply, .. } => drop(reply.send(Err(down()))),
        Command::SetMute { reply, .. } | Command::Leave { reply, .. } => drop(reply.send(Err(down()))),
        Command::Pinned { reply, .. } => drop(reply.send(Err(down()))),
        Command::SearchShared { reply, .. } => drop(reply.send(Err(down()))),
        Command::Search { reply, .. } => drop(reply.send(Err(down()))),
        Command::Catalog { reply, .. } => drop(reply.send(Err(down()))),
        Command::SendWith { conversation_id, client_id, .. } => ctx.sent(&conversation_id, &client_id, Err("Telegram is offline.".to_owned())),
        Command::StartDirect { reply, .. } | Command::Handoff { reply, .. } => drop(reply.send(Err(down()))),
        Command::SearchPeople(query) => ctx.searched(&query),
        other => other.unsupported(ctx),
    }
}

fn serve(account: &Arc<Account>, command: Command) {
    let ctx = &account.ctx;
    match command {
        Command::History { conversation_id, before } => drop(sync::history(account, &conversation_id, sync::Around::Before(&before))),
        Command::HistoryAfter { conversation_id, after } => drop(sync::history(account, &conversation_id, sync::Around::After(&after))),
        Command::HistoryAround { conversation_id, around } => drop(sync::history(account, &conversation_id, sync::Around::At(&around))),
        // The chat list is current from updates; only a default mute changing needs it read
        // again, and then only when the user opens it.
        Command::OpenHome => {
            if std::mem::take(&mut account.live().stale) {
                if let Err(fault) = sync::dialogs(account) {
                    tracing::warn!("chatd: Telegram: chats: {fault}");
                }
            }
        }
        Command::Watch(watching) => sync::on_screen(account, &watching),
        Command::Wanted(ids) => drop(account.files.asked(&ids)),
        Command::Previews(ids) => sync::previews(account, &ids),
        Command::MarkRead { conversation_id, message_id } => mark_read(account, &conversation_id, &message_id),
        Command::Send { conversation_id, client_id, text, reply_to } => {
            account.here();
            let outcome = send_text(account, &conversation_id, &client_id, &text, reply_to.as_deref(), Options::default());
            ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
        }
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            send::send_with(account, &conversation_id, &client_id, &text, reply_to.as_deref(), *with);
        }
        Command::CancelSend { client_id, .. } => send::cancel(account, &client_id),
        Command::React { conversation_id, message_id, emoji, on, reply } => {
            let _ = reply.send(react::react(account, &conversation_id, &message_id, &emoji, on).map_err(refused));
        }
        Command::ReactionRules { conversation_id, reply } => {
            let _ = reply.send(react::rules(account, &conversation_id).map_err(refused));
        }
        Command::Edit { conversation_id, message_id, text, reply } => {
            let _ = reply.send(edit(account, &conversation_id, &message_id, &text).map_err(refused));
        }
        Command::Delete { conversation_id, message_id, reply } => {
            let _ = reply.send(delete(account, &conversation_id, &message_id).map_err(refused));
        }
        Command::Typing(conversation_id) => typing(account, &conversation_id),
        Command::SearchPeople(query) => {
            let request = tl::functions::contacts::Search { broadcasts: false, bots: false, q: query.clone(), limit: 25 };
            if let Ok(tl::enums::contacts::Found::Found(found)) = account.ask(&request) {
                account.learn(&found.users, &[]);
            }
            // Resolving is tightly flood-limited: only an explicit, complete @username asks
            // for it, after ranked search. Never hold queued sends up waiting out a flood.
            if let Some(username) = query.trim().strip_prefix('@').filter(|name| {
                (5..=32).contains(&name.len()) && name.as_bytes()[0].is_ascii_alphabetic()
                    && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            }) {
                let request = tl::functions::contacts::ResolveUsername { username: username.to_owned(), referer: None };
                if let Ok(tl::enums::contacts::ResolvedPeer::Peer(found)) = wire::call(account.wire.as_ref(), &request) {
                    account.learn(&found.users, &[]);
                }
            }
            ctx.searched(&query);
        }
        Command::Members { conversation_id, offset, limit, reply } => {
            let _ = reply.send(members(account, &conversation_id, offset, limit).map_err(refused));
        }
        Command::StartDirect { person_id, reply } => {
            // Nothing reaches the other person: the chat exists on Telegram once a message does.
            let started = match Who::parse(&person_id) {
                Some((who @ Who::User(_), None)) => sync::ensure_row(account, who, None).then_some(person_id),
                _ => None,
            };
            let _ = reply.send(started.ok_or_else(|| Refusal::Refused("That person can't be messaged from here.".to_owned())));
        }
        Command::Profile { person_id, reply } => {
            let _ = reply.send(info::profile(account, &person_id).map_err(refused));
        }
        Command::ProfileIn { person_id, space_id, reply } => {
            let _ = reply.send(info::profile_in(account, &person_id, &space_id).map_err(refused));
        }
        Command::ConversationInfo { conversation_id, reply } => {
            let _ = reply.send(info::conversation(account, &conversation_id).map_err(refused));
        }
        Command::SetMute { conversation_id, notify, reply } => {
            let _ = reply.send(info::set_mute(account, &conversation_id, notify).map_err(refused));
        }
        Command::Leave { conversation_id, reply } => {
            let _ = reply.send(info::leave(account, &conversation_id).map_err(refused));
        }
        Command::Pinned { conversation_id, reply } => {
            let _ = reply.send(info::pinned(account, &conversation_id).map_err(refused));
        }
        Command::Search { query, scope, reply, .. } => {
            let result = match scope {
                super::SearchScope::Conversation(id) => info::search_messages(account, &id, &query).map_err(refused),
                super::SearchScope::Space(_) => Err(Refusal::Unsupported),
            };
            let _ = reply.send(result);
        }
        Command::SearchShared { conversation_id, kind, before, limit, reply } => {
            let _ = reply.send(info::shared(account, &conversation_id, kind, before.as_deref(), limit).map_err(refused));
        }
        Command::Catalog { kind, query, cursor, reply, .. } => {
            let _ = reply.send(catalog::catalog(account, kind, &query, cursor.as_deref()).map_err(refused));
        }
        Command::Handoff { conversation_id, reply } => {
            let link = Who::parse(&conversation_id).map(|(who, _)| {
                let handle = account.dir().get(who).map(|known| known.handle.clone()).unwrap_or_default();
                match (who, handle.as_str()) {
                    (_, handle) if !handle.is_empty() => format!("tg://resolve?domain={handle}"),
                    (Who::User(id), _) => format!("tg://user?id={id}"),
                    (Who::Group(id), _) => format!("tg://openmessage?chat_id={id}"),
                    (Who::Channel(id), _) => format!("tg://privatepost?channel={id}&post=1"),
                }
            });
            let _ = reply.send(link.ok_or(Refusal::Unsupported));
        }
        Command::SignOut(reply) => {
            // Ends the session on Telegram, so the device list no longer shows it. What the
            // link was down for belongs to this session and not the next.
            held_take();
            let _ = account.ask(&tl::functions::auth::LogOut {});
            let _ = reply.send(Ok(()));
        }
        // CONTRACT: the media cache fetches over HTTPS from hosts it trusts and has none for
        // Telegram, whose files come over the session itself; it never asks this adapter for one.
        // What a row shows is fetched here when its conversation is opened (`files`); an original
        // (a full photo, a file, a video) cannot be fetched until the cache hands Telegram's
        // `Fetch`es to the adapter.
        other => other.unsupported(ctx),
    }
}

fn mark_read(account: &Account, conversation: &str, message: &str) {
    let (Some((who, topic)), Ok(max_id)) = (Who::parse(conversation), message.parse::<i32>()) else {
        return;
    };
    account.here();
    // Told exactly as Telegram's own apps tell it: reading is never withheld (the terms).
    let told = match (who, topic) {
        (who, Some(topic)) => account.input(who).and_then(|peer| {
            account.ask(&tl::functions::messages::ReadDiscussion { peer, msg_id: topic, read_max_id: max_id }).map(drop)
        }),
        (who @ Who::Channel(_), None) => account
            .input_channel(who)
            .and_then(|channel| account.ask(&tl::functions::channels::ReadHistory { channel, max_id }).map(drop)),
        (who, None) => {
            account.input(who).and_then(|peer| account.ask(&tl::functions::messages::ReadHistory { peer, max_id }).map(drop))
        }
    };
    if let Err(fault) = told {
        tracing::warn!("chatd: Telegram: read: {fault}");
    }
}

/// The nonce Telegram dedupes a send by, from the shell's client id.
fn random_id(client_id: &str) -> i64 {
    client_id.parse().unwrap_or_else(|_| {
        client_id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)) as i64
    })
}

/// How a send names what it answers. In a forum a post names its topic the way a reply names
/// its message; General needs none.
fn reply_header(reply: Option<i32>, topic: Option<i32>) -> Option<tl::enums::InputReplyTo> {
    let reply_to = match (reply, topic.filter(|topic| *topic != 1)) {
        (None, None) => None,
        (Some(reply), top) => Some((reply, top)),
        (None, Some(topic)) => Some((topic, None)),
    };
    reply_to.map(|(reply_to_msg_id, top_msg_id)| {
        tl::types::InputReplyToMessage {
            reply_to_msg_id,
            top_msg_id,
            reply_to_peer_id: None,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            monoforum_peer_id: None,
            todo_item_id: None,
            poll_option: None,
        }
        .into()
    })
}

/// Remembers which pending message a nonce belongs to, so the echo replaces it.
fn remember_random(account: &Account, random: i64, client_id: &str) {
    let mut live = account.live();
    if live.random.len() >= 64 {
        live.random.pop_front();
    }
    live.random.push_back((random, client_id.to_owned()));
}

fn send_text(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, opts: Options) -> Result<protocol::Message, Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None };
    let (who, topic) = Who::parse(conversation).ok_or_else(invalid)?;
    let peer = account.input(who)?;
    let reply = reply_to.and_then(|id| id.parse::<i32>().ok());
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    let request = tl::functions::messages::SendMessage {
        no_webpage: opts.no_webpage,
        silent: opts.silent,
        background: false,
        clear_draft: true,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer,
        reply_to: reply_header(reply, topic),
        message: text.to_owned(),
        random_id: random,
        reply_markup: None,
        entities: None,
        schedule_date: opts.schedule,
        schedule_repeat_period: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        allow_paid_stars: None,
        suggested_post: None,
        rich_message: None,
    };
    let answer = account.ask(&request)?;
    live::answered_scheduled(account, conversation, text, opts, &answer)
        .or_else(|| live::sent(account, conversation, who, text, reply, &answer))
        .ok_or_else(|| Fault::Net("Telegram accepted the message but did not name it".to_owned()))
}

fn edit(account: &Account, conversation: &str, message: &str, text: &str) -> Result<(), Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None };
    let ((who, _), id) = (Who::parse(conversation).ok_or_else(invalid)?, message.parse::<i32>().map_err(|_| invalid())?);
    let request = tl::functions::messages::EditMessage {
        no_webpage: false,
        invert_media: false,
        peer: account.input(who)?,
        id,
        message: Some(text.to_owned()),
        media: None,
        reply_markup: None,
        entities: None,
        schedule_date: None,
        schedule_repeat_period: None,
        quick_reply_shortcut_id: None,
        rich_message: None,
    };
    let answer = account.ask(&request)?;
    // Reported before the answer, as the contract asks: nothing was changed locally.
    live::answered(account, &answer);
    Ok(())
}

fn delete(account: &Account, conversation: &str, message: &str) -> Result<(), Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None };
    let ((who, _), id) = (Who::parse(conversation).ok_or_else(invalid)?, message.parse::<i32>().map_err(|_| invalid())?);
    match who {
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            account.ask(&tl::functions::channels::DeleteMessages { channel, id: vec![id] })?;
        }
        // For everyone, which is what `Delete` means.
        _ => {
            account.ask(&tl::functions::messages::DeleteMessages { revoke: true, id: vec![id] })?;
        }
    }
    let _ = account.ctx.deleted(conversation, message);
    Ok(())
}

fn typing(account: &Account, conversation: &str) {
    let Some((who, topic)) = Who::parse(conversation) else {
        return;
    };
    {
        let mut live = account.live();
        if live.typed.get(conversation).is_some_and(|told| told.elapsed() < TYPING_EVERY) {
            return;
        }
        live.typed.retain(|_, told| told.elapsed() < TYPING_EVERY);
        live.typed.insert(conversation.to_owned(), Instant::now());
    }
    account.here();
    if let Ok(peer) = account.input(who) {
        let request = tl::functions::messages::SetTyping {
            peer,
            top_msg_id: topic,
            action: tl::enums::SendMessageAction::SendMessageTypingAction,
        };
        // Not `ask`: a typing signal is not worth waiting out a flood for.
        let _ = wire::call(account.wire.as_ref(), &request);
    }
}

fn person(account: &Account, who: Who, relation: Relation) -> Person {
    let dir = account.dir();
    let row = peers::person_row(who, &dir.get(who).cloned().unwrap_or_default());
    Person {
        key: account.ctx.key(&row.id),
        presence: account.ctx.presence_of(&row.id),
        name: row.name,
        handle: row.handle,
        avatar: row.avatar.map(|id| protocol::MediaFile::new(id, protocol::MediaKind::Avatar)),
        bot: row.bot,
        relation,
        status: String::new(),
        link: row.link,
        ..Person::new(account.ctx.key(&row.id), "")
    }
}

fn members(account: &Account, conversation: &str, offset: u32, limit: u32) -> Result<Members, Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None };
    let (who, _) = Who::parse(conversation).ok_or_else(invalid)?;
    let relation = |account: &Account, id: i64| match account.dir().get(Who::User(id)).is_some_and(|known| known.contact) {
        true => Relation::Contact,
        false => Relation::None,
    };
    let (total, ids): (u32, Vec<i64>) = match who {
        Who::User(id) => (2, [account.me, id].into_iter().skip(offset as usize).take(limit as usize).collect()),
        Who::Group(chat_id) => {
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
            account.learn(&full.users, &full.chats);
            let listed = match &full.full_chat {
                tl::enums::ChatFull::Full(chat) => match &chat.participants {
                    tl::enums::ChatParticipants::Participants(list) => list.participants.iter().map(|one| match one {
                        tl::enums::ChatParticipant::Participant(one) => one.user_id,
                        tl::enums::ChatParticipant::Creator(one) => one.user_id,
                        tl::enums::ChatParticipant::Admin(one) => one.user_id,
                    }).collect(),
                    tl::enums::ChatParticipants::Forbidden(_) => Vec::new(),
                },
                tl::enums::ChatFull::ChannelFull(_) => Vec::new(),
            };
            let listed: Vec<i64> = listed;
            (listed.len() as u32, listed.into_iter().skip(offset as usize).take(limit as usize).collect())
        }
        Who::Channel(_) => {
            let request = tl::functions::channels::GetParticipants {
                channel: account.input_channel(who)?,
                filter: tl::enums::ChannelParticipantsFilter::ChannelParticipantsRecent,
                offset: offset as i32,
                limit: limit as i32,
                hash: 0,
            };
            match account.ask(&request)? {
                tl::enums::channels::ChannelParticipants::Participants(page) => {
                    account.learn(&page.users, &page.chats);
                    let ids = page.participants.iter().filter_map(|one| match one {
                        tl::enums::ChannelParticipant::Participant(one) => Some(one.user_id),
                        tl::enums::ChannelParticipant::ParticipantSelf(one) => Some(one.user_id),
                        tl::enums::ChannelParticipant::Creator(one) => Some(one.user_id),
                        tl::enums::ChannelParticipant::Admin(one) => Some(one.user_id),
                        _ => None,
                    });
                    (page.count.max(0) as u32, ids.collect())
                }
                tl::enums::channels::ChannelParticipants::NotModified => (0, Vec::new()),
            }
        }
    };
    Ok(Members {
        pending: false,
        sections: vec![MemberSection { name: "Members".to_owned(), count: total }],
        members: ids.into_iter().map(|id| person(account, Who::User(id), relation(account, id))).collect(),
    })
}
