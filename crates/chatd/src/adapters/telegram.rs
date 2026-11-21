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
//! | Covers | Everything, secret chats and call signalling included. | Phone login and 2FA as methods; QR login, folders, topics, reactions, typing, read state and files through its raw request call, which reaches the whole schema. A secret chat is a row that says it stays in Telegram's phone apps. No calls. |
//!
//! grammers wins on the two things this app is about, weight and one cache, and it is the only
//! one of the two that can be built on the machine tinyclient is developed on. What it costs: a
//! schema change at Telegram waits on one maintainer, secret-chat messages are not carried, and calls (a later
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
//! One keychain item, like every network. It holds every Telegram account signed in here:
//! one `Saved`, or a list of them. The keys Telegram issued (256 bytes a datacenter), the
//! home datacenter and whose they are. Nothing of it is on disk. Removing one leaves the rest.
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
mod members;
mod peers;
mod premium;
pub(crate) mod react;
mod real;
mod secret;
mod send;
pub mod signin;
mod sync;
mod wire;

use super::{keep_connected, serve_commands, Command, Context, Failure, Refusal, Signed};
use crate::session;
use grammers_tl_types as tl;
use peers::{Directory, Who};
use protocol::{Link, Person, Presence, RelateAction, Relation};
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

fn unreadable() -> String {
    "the Telegram session is unreadable".to_owned()
}

/// One stored sign-in, or a list of them. A session from before several accounts were kept
/// is the one object.
fn rows(session: &str) -> Result<Vec<Saved>, String> {
    if session.trim_start().starts_with('[') {
        let saved: Vec<Saved> = serde_json::from_str(session).map_err(|_| unreadable())?;
        if saved.is_empty() {
            return Err(unreadable());
        }
        return Ok(saved);
    }
    Ok(vec![parse(session)?])
}

/// One account stays one object. Several are a list.
fn pack(saved: &[Saved]) -> Result<String, String> {
    match saved {
        [] => Err(unreadable()),
        [one] => serde_json::to_string(one).map_err(|_| unreadable()),
        many => serde_json::to_string(many).map_err(|_| unreadable()),
    }
}

fn signed_for(saved: &Saved) -> Result<Signed, String> {
    Ok(Signed {
        account: saved.user.to_string(),
        name: if saved.name.is_empty() { "Telegram".to_owned() } else { saved.name.clone() },
        session: serde_json::to_string(saved).map_err(|_| unreadable())?,
    })
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let mut found = Vec::new();
    for saved in rows(session)? {
        if found.iter().any(|signed: &Signed| signed.account == saved.user.to_string()) {
            continue;
        }
        found.push(signed_for(&saved)?);
    }
    if found.is_empty() {
        return Err(unreadable());
    }
    Ok(found)
}

/// The session with one account taken out. `None` only when that was the last one. An
/// account that is not in it leaves the original string untouched.
pub fn without(session: &str, account: &str) -> Option<String> {
    let mut saved = rows(session).ok()?;
    let before = saved.len();
    saved.retain(|one| one.user.to_string() != account);
    if saved.len() == before {
        return Some(session.to_owned());
    }
    match saved.len() {
        0 => None,
        _ => pack(&saved).ok(),
    }
}

/// A finished sign-in joins the accounts already stored. The same user replaces their own keys.
pub(super) fn combine(existing: &str, fresh: &str) -> Result<String, String> {
    let fresh_saved = parse(fresh)?;
    let mut kept = rows(existing)?;
    kept.retain(|saved| saved.user != fresh_saved.user);
    kept.push(fresh_saved);
    pack(&kept)
}

/// A wire for this session, or for a sign-in when there is none yet.
fn open(saved: Option<&Saved>) -> Result<Box<dyn Wire>, String> {
    if scripted() {
        return Ok(Box::new(fake::Fake::open(saved)));
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
    /// Users blocked on this connection. A second Block of one of them unblocks them.
    blocked: HashSet<i64>,
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
    /// The discussion group a comments thread writes through and the echo of its post
    /// there (`discussion`): what the open learned, for its sends and its mapping back.
    discussion: VecDeque<(String, Who, i32)>,
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
    /// The installed custom-emoji packs, kept apart from stickers so one tab does not blank the other.
    emoji_sets: Option<Vec<(i64, i64, String)>>,
    /// `:alt:` labels the emoji tab offered, newest last, so a send can name the document.
    emoji_labels: VecDeque<content::EmojiLabel>,
    /// Scheduled sends shown until Telegram sends them: the chat, the text, the placeholder.
    scheduled: VecDeque<(String, String, String)>,
    /// The next slice of cached unread channels to check after sign-on.
    unread_check: Option<Instant>,
    /// Unread snapshots from recovered dialogs already include messages up to these IDs.
    recovered_through: VecDeque<(Who, i32)>,
    /// Telegram's reaction emoji, and whether each needs Premium, once asked (`react`).
    reactions: Option<Vec<(String, bool)>>,
    /// File, caption and reaction limits from `help.getAppConfig`, or the documented defaults.
    limits: premium::Limits,
    /// One conversation's member list, so the next page does not ask Telegram again.
    roster: Option<members::Held>,
    /// Photo albums still arriving or open, so a later item, edit, reaction or deletion
    /// finds the row they share.
    albums: VecDeque<live::HeldAlbum>,
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
    pending: Arc<Mutex<live::PendingChats>>,
    /// Scrolling and recovery may advance a dialog list from different threads.
    dialog_pages: Mutex<()>,
    files: files::Files,
}

impl Account {
    fn dir(&self) -> MutexGuard<'_, Directory> {
        self.dir.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn pending(&self) -> MutexGuard<'_, live::PendingChats> {
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        for chat in chats {
            match chat {
                tl::enums::Chat::Channel(channel) if !channel.min => self.wire.channel_membership(channel.id, !channel.left),
                tl::enums::Chat::ChannelForbidden(channel) => self.wire.channel_membership(channel.id, false),
                _ => {}
            }
        }
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

    /// Remembers where a message is, in this connection and in a note that outlives it.
    fn place(&self, who: Who, message: i32, conversation: &str) {
        // A channel that is not a forum says where its messages are by itself.
        if matches!(who, Who::Channel(_)) && !conversation.contains('_') {
            return;
        }
        let channel = matches!(who, Who::Channel(_)).then(|| who.text());
        self.ctx.set_note(&message_note(channel.as_deref(), message), conversation);
        let mut live = self.live();
        if live.placed.len() >= PLACED {
            live.placed.pop_front();
        }
        live.placed.push_back(((who, message), conversation.to_owned()));
    }

    fn placed(&self, who: Option<Who>, message: i32) -> Option<String> {
        let memory = {
            let live = self.live();
            live.placed.iter().rev().find(|((peer, id), _)| *id == message && match who {
                Some(who) => *peer == who,
                None => !matches!(peer, Who::Channel(_)),
            }).map(|(_, conversation)| conversation.clone())
        };
        let channel = match who {
            Some(Who::Channel(_)) => who.map(|who| who.text()),
            _ => None,
        };
        remembered(memory.as_deref(), self.ctx.note(&message_note(channel.as_deref(), message)).as_deref())
    }

    /// The id was deleted, so a later message must not inherit its chat.
    fn forget_placed(&self, who: Option<Who>, message: i32) {
        let channel = match who {
            Some(Who::Channel(_)) => who.map(|who| who.text()),
            _ => None,
        };
        self.ctx.set_note(&message_note(channel.as_deref(), message), "");
        let mut live = self.live();
        live.placed.retain(|((peer, id), _)| {
            !(*id == message && match who {
                Some(who) => *peer == who,
                None => !matches!(peer, Who::Channel(_)),
            })
        });
    }

    fn user_blocked(&self, id: i64) -> bool {
        self.live().blocked.contains(&id)
    }

    fn set_user_blocked(&self, id: i64, blocked: bool) {
        let mut live = self.live();
        if blocked {
            live.blocked.insert(id);
        } else {
            live.blocked.remove(&id);
        }
    }

    /// Writes the session out again when a key was made for another datacenter. The stored
    /// item may hold other accounts; only this one is replaced.
    fn keep_session(&self) {
        if self.wire.unsaved() {
            let fresh = self.wire.session(self.me, &self.name);
            self.ctx.save_session(|stored| match combine(stored, &fresh) {
                Ok(joined) if joined != stored => Some(joined),
                _ => None,
            });
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

/// The note a message's conversation is kept under. A private chat and a basic group share
/// one id space, so the id is enough. A channel's ids are its own.
pub(super) fn message_note(who: Option<&str>, message: i32) -> String {
    match who {
        Some(who) => format!("tg-msg:{who}:{message}"),
        None => format!("tg-msg:{message}"),
    }
}

/// The conversation a message was seen in. This connection wins. An empty note was cleared.
pub(super) fn remembered(memory: Option<&str>, note: Option<&str>) -> Option<String> {
    let kept = |place: Option<&str>| place.filter(|place| !place.is_empty()).map(str::to_owned);
    kept(memory).or_else(|| kept(note))
}

/// The lab's telegram-delete-placed driver. A build without this stays up and the driver fails.
pub fn prove_telegram_delete_placed() -> Result<(), String> {
    if message_note(None, 13) != "tg-msg:13" {
        return Err("a private message is remembered by its id".to_owned());
    }
    if message_note(Some("c9002"), 41) != "tg-msg:c9002:41" {
        return Err("a channel message is remembered with its channel".to_owned());
    }
    if remembered(None, Some("u7002")).as_deref() != Some("u7002") {
        return Err("a note finds the chat after a reconnect".to_owned());
    }
    if remembered(Some("u7002"), Some("")).as_deref() != Some("u7002") {
        return Err("this connection's place wins over a cleared note".to_owned());
    }
    if remembered(None, Some("")).is_some() {
        return Err("a cleared note is not a chat".to_owned());
    }
    Ok(())
}

/// The lab's telegram-view-once driver. A build without this stays up and the driver fails.
pub fn prove_telegram_view_once() -> Result<(), String> {
    content::prove_view_once()
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
            let pending = Arc::default();
            keep_connected(&socket_ctx, || connect(&socket_ctx, &saved, &held, &pending));
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
                    None => held_back(&ctx.account.account, id.to_owned()),
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
            held_drop(&ctx.account.account);
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// One connection, from opening the wire to losing it.
fn connect(
    ctx: &Context,
    saved: &Saved,
    current: &Mutex<Option<Arc<Account>>>,
    pending: &Arc<Mutex<live::PendingChats>>,
) -> Failure {
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
        pending: pending.clone(),
        dialog_pages: Mutex::default(),
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
    for id in held_take(&ctx.account.account) {
        sync::backfill(&account, &id);
    }
    // Telegram fades an account to offline by itself, so being here is said on opening the
    // connection and then on a cadence while it lives; `here` keeps it to one call per
    // HERE_EVERY however often it is asked. The event loop is otherwise idle, so its wait is
    // that cadence: a tick with nothing to read sends it. Nothing outlives the connection.
    account.here();
    let mut said = Instant::now();
    let ended = loop {
        if said.elapsed() >= HERE_EVERY {
            account.here();
            said = Instant::now();
        }
        live::retry_pending(&account);
        sync::retry_unread_channels(&account);
        let wait = [
            Some(said + HERE_EVERY),
            live::pending_deadline(&account),
            account.live().unread_check,
        ].into_iter().flatten().min().unwrap_or(said + HERE_EVERY).saturating_duration_since(Instant::now());
        match events.recv_timeout(wait) {
            Ok(Event::Update { update, users, chats }) => {
                account.learn(&users, &chats);
                live::apply(&account, &update);
            }
            Ok(Event::Closed(why)) => break why,
            Err(RecvTimeoutError::Timeout) => {}
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

struct Held {
    /// Conversation ids to fill, newest last, each with the account that asked. One read
    /// twice is still one look. Two accounts do not share a list.
    backs: VecDeque<(String, String)>,
}

/// Asks the shell made while the link was down, that the network hears at the next
/// connect: the conversation the user opened, whose store answer remains until then. One
/// ask once, at most this many, and gone the moment the session ends (a sign-out), so
/// nothing held outlives the account it was asked for.
static HELD: Mutex<Held> = Mutex::new(Held { backs: VecDeque::new() });

const HELD_BACKS: usize = 32;

fn held_lock() -> MutexGuard<'static, Held> {
    HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn held_back(account: &str, id: String) {
    let mut held = held_lock();
    held.backs.retain(|(who, held_id)| who != account || held_id != &id);
    held.backs.push_back((account.to_owned(), id));
    while held.backs.iter().filter(|(who, _)| who == account).count() > HELD_BACKS {
        if let Some(at) = held.backs.iter().position(|(who, _)| who == account) {
            held.backs.remove(at);
        }
    }
}

fn held_take(account: &str) -> VecDeque<String> {
    let mut held = held_lock();
    let mut mine = VecDeque::new();
    let mut keep = VecDeque::new();
    for (who, id) in held.backs.drain(..) {
        if who == account {
            mine.push_back(id);
        } else {
            keep.push_back((who, id));
        }
    }
    held.backs = keep;
    mine
}

fn held_drop(account: &str) {
    held_lock().backs.retain(|(who, _)| who != account);
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
        Command::SetMute { reply, .. } | Command::Leave { reply, .. } | Command::ClearHistory { reply, .. } => drop(reply.send(Err(down()))),
        Command::Pinned { reply, .. } => drop(reply.send(Err(down()))),
        Command::Pin { reply, .. } => drop(reply.send(Err(down()))),
        Command::Forward { reply, .. } => drop(reply.send(Err(down()))),
        Command::SearchShared { reply, .. } => drop(reply.send(Err(down()))),
        Command::Search { reply, .. } => drop(reply.send(Err(down()))),
        Command::Catalog { reply, .. } => drop(reply.send(Err(down()))),
        Command::SendWith { conversation_id, client_id, .. } => ctx.sent(&conversation_id, &client_id, Err("Telegram is offline.".to_owned())),
        Command::StartDirect { reply, .. } | Command::Handoff { reply, .. } => drop(reply.send(Err(down()))),
        Command::OpenForum { reply, .. } => drop(reply.send(Err(down()))),
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
        // Opening a forum lists its topics, once per open; the landing the shell then reads
        // from the store is named by what this read left.
        Command::OpenForum { conversation_id, reply } => {
            sync::backfill(account, &conversation_id);
            let _ = reply.send(Ok(()));
        }
        Command::Wanted(ids) => drop(account.files.asked(&ids)),
        Command::Previews(ids) => sync::previews(account, &ids),
        Command::MarkRead { conversation_id, message_id } => mark_read(account, &conversation_id, &message_id),
        Command::MarkUnread { conversation_id, .. } => mark_dialog_unread(account, &conversation_id, true),
        Command::Send { conversation_id, client_id, text, reply_to } => {
            account.here();
            let outcome = send_text(account, &conversation_id, &client_id, &text, reply_to.as_deref(), Options::default(), &[]);
            ctx.sent(&conversation_id, &client_id, outcome.map_err(|fault| fault.words()));
        }
        Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
            send::send_with(account, &conversation_id, &client_id, &text, reply_to.as_deref(), *with);
        }
        Command::CancelSend { conversation_id, client_id } => send::cancel(account, &conversation_id, &client_id),
        Command::React { conversation_id, message_id, emoji, on, reply } => {
            let _ = reply.send(react::react(account, &conversation_id, &message_id, &emoji, on).map_err(refused));
        }
        Command::ReactionRules { conversation_id, reply } => {
            let _ = reply.send(react::rules(account, &conversation_id).map_err(refused));
        }
        Command::Edit { conversation_id, message_id, text, spans, reply } => {
            let _ = reply.send(edit(account, &conversation_id, &message_id, &text, &spans).map_err(refused));
        }
        Command::Delete { conversation_id, message_id, scope, reply } => {
            let _ = reply.send(delete(account, &conversation_id, &message_id, scope).map_err(refused));
        }
        Command::Typing { conversation_id, activity } => typing(account, &conversation_id, activity),
        Command::FollowLink { conversation_id, message_id } => sync::followed(account, &conversation_id, &message_id),
        Command::Draft { conversation_id, text } => save_draft(account, &conversation_id, &text),
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
            // A channel on screen is searched too. The contact search above already stands.
            members::search_open(account, &query);
            ctx.searched(&query);
        }
        Command::Members { conversation_id, offset, limit, reply } => {
            let _ = reply.send(members::list(account, &conversation_id, offset, limit).map_err(refused));
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
        Command::SetMute { conversation_id, notify, until, reply } => {
            let _ = reply.send(info::set_mute(account, &conversation_id, notify, until).map_err(refused));
        }
        Command::SetDisappearing { conversation_id, seconds, reply } => {
            let _ = reply.send(info::set_disappearing(account, &conversation_id, seconds).map_err(refused));
        }
        Command::Leave { conversation_id, reply } => {
            let _ = reply.send(info::leave(account, &conversation_id).map_err(refused));
        }
        Command::ClearHistory { conversation_id, delete_chat, revoke, reply } => {
            let _ = reply.send(info::clear_history(account, &conversation_id, delete_chat, revoke).map_err(refused));
        }
        Command::File { conversation_id, pinned, archived, folder_add, folder_remove, reply } => {
            let _ = reply.send(
                filing::file(account, &conversation_id, pinned, archived, folder_add.as_deref(), folder_remove.as_deref())
                    .map_err(refused),
            );
        }
        Command::Pinned { conversation_id, reply } => {
            let _ = reply.send(info::pinned(account, &conversation_id).map_err(refused));
        }
        Command::Pin { conversation_id, message_id, pinned, notify, me_only, reply } => {
            let _ = reply.send(info::pin(account, &conversation_id, &message_id, pinned, notify, me_only).map_err(refused));
        }
        Command::Forward { from_conversation_id, message_ids, to_conversation_ids, note, reply } => {
            let _ = reply.send(info::forward(account, &from_conversation_id, &message_ids, &to_conversation_ids, &note).map_err(refused));
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
        Command::Catalog { kind, space_id, query, cursor, reply } => {
            let _ = reply.send(catalog::catalog(account, kind, space_id.as_deref(), &query, cursor.as_deref()).map_err(refused));
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
        Command::Relate { person_id, action, reply } => {
            let answer = match action {
                RelateAction::Accept | RelateAction::Ignore | RelateAction::Cancel => {
                    Err(Refusal::Refused("Telegram has no friend request.".to_owned()))
                }
                RelateAction::IgnoreUser | RelateAction::UnignoreUser => {
                    Err(Refusal::Refused("Telegram does not support ignoring users.".to_owned()))
                }
                action => relate(account, &person_id, action).map_err(refused),
            };
            let _ = reply.send(answer);
        }
        Command::SignOut(reply) => {
            // Ends the session on Telegram, so the device list no longer shows it. What the
            // link was down for belongs to this session and not the next.
            held_take(&account.ctx.account.account);
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
    let read = told.is_ok();
    if let Err(fault) = told {
        tracing::warn!("chatd: Telegram: read: {fault}");
    }
    // Reading does not clear a manual unread mark. Telegram's own apps send both.
    if topic.is_none() {
        mark_dialog_unread(account, conversation, false);
    }
    // The comments were read: the badge on the post they hang off goes with them.
    if let (true, who @ Who::Channel(_), Some(root)) = (read, who, topic) {
        if account.dir().get(who).is_some_and(|known| known.broadcast) {
            sync::comment_unread(account, who, root, 0);
        }
    }
}

/// `messages.markDialogUnread`. A topic has no dialog mark of its own; the forum's does.
fn mark_dialog_unread(account: &Account, conversation: &str, unread: bool) {
    let Some((who, None)) = Who::parse(conversation) else {
        return;
    };
    let Ok(peer) = account.input(who) else {
        return;
    };
    account.here();
    let request = tl::functions::messages::MarkDialogUnread {
        unread,
        parent_peer: None,
        peer: tl::enums::InputDialogPeer::Peer(tl::types::InputDialogPeer { peer }),
    };
    if let Err(fault) = account.ask(&request) {
        tracing::warn!("chatd: Telegram: unread mark: {fault}");
    }
    filing::facts_changed(account, who, |facts| facts.read = !unread);
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

fn send_text(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, opts: Options, spans: &[protocol::Span]) -> Result<protocol::Message, Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None };
    let (who, topic) = Who::parse(conversation).ok_or_else(invalid)?;
    // The comments of a channel post are held in the discussion group behind it: the send
    // names that, under the echo of the post itself, where Telegram's own apps write them.
    let (who, topic) = sync::comment_target(account, conversation)?.map(|(group, echo)| (group, Some(echo))).unwrap_or((who, topic));
    let peer = account.input(who)?;
    let reply = reply_to.and_then(|id| id.parse::<i32>().ok());
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    let (plain, entities) = prepared_with_spans(account, conversation, text, spans);
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
        message: plain.clone(),
        random_id: random,
        reply_markup: None,
        entities,
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
    live::answered_scheduled(account, conversation, &plain, opts, &answer)
        .or_else(|| live::sent(account, conversation, who, &plain, reply, &answer))
        .ok_or_else(|| Fault::Net("Telegram accepted the message but did not name it".to_owned()))
}

fn edit(account: &Account, conversation: &str, message: &str, text: &str, spans: &[protocol::Span]) -> Result<(), Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None };
    let ((who, _), id) = (Who::parse(conversation).ok_or_else(invalid)?, message.parse::<i32>().map_err(|_| invalid())?);
    let (plain, mut entities) = content::outgoing(text);
    if entities.is_none() {
        if let Ok(Some(stored)) = account.ctx.stored_message(conversation, message) {
            entities = content::with_stored(&plain, &stored.spans);
        }
    }
    let (plain, entities) = if spans.is_empty() {
        dress(account, conversation, plain, entities)
    } else {
        let dir = account.dir();
        let next = content::apply_mention_spans(&plain, entities.unwrap_or_default(), spans, |uid| {
            dir.get(Who::User(uid)).and_then(|known| known.hash)
        });
        (plain, (!next.is_empty()).then_some(next))
    };
    let request = tl::functions::messages::EditMessage {
        no_webpage: false,
        invert_media: false,
        peer: account.input(who)?,
        id,
        message: Some(plain),
        media: None,
        reply_markup: None,
        entities,
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

fn delete(account: &Account, conversation: &str, message: &str, scope: protocol::DeleteScope) -> Result<(), Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None };
    let ((who, _), id) = (Who::parse(conversation).ok_or_else(invalid)?, message.parse::<i32>().map_err(|_| invalid())?);
    match who {
        Who::Channel(_) => {
            if scope == protocol::DeleteScope::Me {
                return Err(Fault::Rpc {
                    code: 400,
                    name: "CHANNEL_DELETE_FOR_EVERYONE_ONLY".to_owned(),
                    value: None,
                });
            }
            let channel = account.input_channel(who)?;
            account.ask(&tl::functions::channels::DeleteMessages { channel, id: vec![id] })?;
        }
        _ => {
            let revoke = scope == protocol::DeleteScope::Everyone;
            account.ask(&tl::functions::messages::DeleteMessages { revoke, id: vec![id] })?;
        }
    }
    let _ = account.ctx.deleted(conversation, message);
    let noted = match who {
        Who::Channel(_) => Some(who),
        _ => None,
    };
    account.forget_placed(noted, id);
    Ok(())
}

fn typing(account: &Account, conversation: &str, activity: protocol::Compose) {
    // Keystrokes are paced. A recording or an upload is not: the other person should see it now.
    deliver(account, conversation, activity_action(activity, 0), activity == protocol::Compose::Typing);
}

/// An upload's progress, 0 to 100. Not paced: each new percent is one signal.
pub(super) fn show_upload(account: &Account, conversation: &str, activity: protocol::Compose, progress: i32) {
    deliver(account, conversation, activity_action(activity, progress), false);
}

/// The other person should see that an upload was abandoned.
pub(super) fn deliver_cancel(account: &Account, conversation: &str) {
    deliver(account, conversation, tl::enums::SendMessageAction::SendMessageCancelAction, false);
}

fn deliver(account: &Account, conversation: &str, action: tl::enums::SendMessageAction, paced: bool) {
    let Some((who, topic)) = Who::parse(conversation) else {
        return;
    };
    if paced {
        let mut live = account.live();
        if live.typed.get(conversation).is_some_and(|told| told.elapsed() < TYPING_EVERY) {
            return;
        }
        live.typed.retain(|_, told| told.elapsed() < TYPING_EVERY);
        live.typed.insert(conversation.to_owned(), Instant::now());
    }
    account.here();
    if let Ok(peer) = account.input(who) {
        let request = tl::functions::messages::SetTyping { peer, top_msg_id: topic, action };
        // Not `ask`: a typing signal is not worth waiting out a flood for.
        let _ = wire::call(account.wire.as_ref(), &request);
    }
}

fn activity_action(activity: protocol::Compose, progress: i32) -> tl::enums::SendMessageAction {
    let progress = progress.clamp(0, 100);
    use tl::enums::SendMessageAction::*;
    match activity {
        protocol::Compose::Typing => SendMessageTypingAction,
        protocol::Compose::RecordingVoice => SendMessageRecordAudioAction,
        protocol::Compose::RecordingVideo => SendMessageRecordVideoAction,
        protocol::Compose::UploadingPhoto => SendMessageUploadPhotoAction(tl::types::SendMessageUploadPhotoAction { progress }),
        protocol::Compose::UploadingVideo => SendMessageUploadVideoAction(tl::types::SendMessageUploadVideoAction { progress }),
        protocol::Compose::UploadingAudio => SendMessageUploadAudioAction(tl::types::SendMessageUploadAudioAction { progress }),
        protocol::Compose::UploadingFile => SendMessageUploadDocumentAction(tl::types::SendMessageUploadDocumentAction { progress }),
        protocol::Compose::ChoosingSticker => SendMessageChooseStickerAction,
        protocol::Compose::PlayingGame => SendMessageGamePlayAction,
    }
}

/// Keeps the cloud draft of this conversation on Telegram: the text as it stands here, or an
/// empty one to clear it on the phone too. Telegram's own apps save a draft as their user
/// types; the same draft arriving back is written nowhere, being what the store already says.
/// Not `ask` for the plain paths: a draft is not worth waiting out a flood for. A comment's
/// thread may have to `ask` where its discussion fell out of the held few — rare, and the
/// row stays read-only until the thread was open, which is what fills the held few.
fn save_draft(account: &Account, conversation: &str, text: &str) {
    let Some((mut who, mut topic)) = Who::parse(conversation) else {
        return;
    };
    // A comments thread's draft is kept where its sends go: in the discussion group behind
    // the post, under the echo of it — not against the channel post itself.
    match sync::comment_target(account, conversation) {
        Ok(Some((group, echo))) => {
            who = group;
            topic = Some(echo);
        }
        Ok(None) => {}
        Err(fault) => {
            tracing::warn!("chatd: Telegram: draft: {fault}");
            return;
        }
    }
    let Ok(peer) = account.input(who) else {
        return;
    };
    let (plain, entities) = prepared(account, conversation, text);
    let request = tl::functions::messages::SaveDraft {
        no_webpage: false,
        invert_media: false,
        // In a forum the draft belongs to the topic it was typed in.
        reply_to: reply_header(None, topic),
        peer,
        message: plain,
        entities,
        media: None,
        effect: None,
        suggested_post: None,
        rich_message: None,
    };
    if let Err(fault) = wire::call(account.wire.as_ref(), &request) {
        tracing::warn!("chatd: Telegram: draft: {fault}");
    }
}

fn person(account: &Account, who: Who, relation: Relation, rank: &str) -> Person {
    let known = {
        let dir = account.dir();
        dir.get(who).cloned().unwrap_or_default()
    };
    let row = peers::shown_person(account.me, who, &known);
    Person {
        key: account.ctx.key(&row.id),
        presence: account.ctx.presence_of(&row.id),
        name: row.name,
        handle: row.handle,
        avatar: row.avatar.map(|id| protocol::MediaFile::new(id, protocol::MediaKind::Avatar)),
        bot: row.bot,
        relation,
        status: row.status,
        link: row.link,
        // The title Telegram gave them in this chat. Status stays the Premium badge.
        title: rank.to_owned(),
        ..Person::new(account.ctx.key(&row.id), "")
    }
}

/// Composer text as Telegram should receive it: markers become entities, a custom emoji
/// the catalog offered (`:alt:`) becomes that emoji's fallback glyph, and `@Display Name`
/// becomes a username or a text mention.
fn prepared(account: &Account, conversation: &str, text: &str) -> (String, Option<Vec<tl::enums::MessageEntity>>) {
    let (plain, entities) = content::outgoing(text);
    dress(account, conversation, plain, entities)
}

fn dress(
    account: &Account,
    conversation: &str,
    plain: String,
    entities: Option<Vec<tl::enums::MessageEntity>>,
) -> (String, Option<Vec<tl::enums::MessageEntity>>) {
    let (plain, entities) = if plain.contains(':') {
        let labels: Vec<content::EmojiLabel> = account.live().emoji_labels.iter().cloned().collect();
        if labels.is_empty() {
            (plain, entities)
        } else {
            let allow = custom_allowed(account, conversation);
            let (plain, entities) = content::apply_emoji(&plain, entities.unwrap_or_default(), &labels, allow);
            (plain, (!entities.is_empty()).then_some(entities))
        }
    } else {
        (plain, entities)
    };
    named(account, plain, entities)
}

fn prepared_with_spans(
    account: &Account,
    conversation: &str,
    text: &str,
    spans: &[protocol::Span],
) -> (String, Option<Vec<tl::enums::MessageEntity>>) {
    if spans.is_empty() {
        return prepared(account, conversation, text);
    }
    let (plain, entities) = content::outgoing(text);
    let dir = account.dir();
    let next = content::apply_mention_spans(&plain, entities.unwrap_or_default(), spans, |uid| {
        dir.get(Who::User(uid)).and_then(|known| known.hash)
    });
    (plain, (!next.is_empty()).then_some(next))
}

/// Premium may send a custom emoji in any chat. Anyone may send one in Saved Messages.
/// Everywhere else only the fallback glyph goes out.
fn custom_allowed(account: &Account, conversation: &str) -> bool {
    if account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium) {
        return true;
    }
    matches!(Who::parse(conversation), Some((Who::User(id), _)) if id == account.me)
}

fn named(account: &Account, plain: String, entities: Option<Vec<tl::enums::MessageEntity>>) -> (String, Option<Vec<tl::enums::MessageEntity>>) {
    if !plain.contains('@') {
        return (plain, entities);
    }
    let people = mention_people(account);
    if people.is_empty() {
        return (plain, entities);
    }
    let (plain, entities) = content::apply_mentions(&plain, entities.unwrap_or_default(), &people);
    (plain, (!entities.is_empty()).then_some(entities))
}

/// People a typed `@Full Name` can notify. The same name twice is skipped: a send cannot know which.
fn mention_people(account: &Account) -> Vec<content::MentionTarget> {
    let dir = account.dir();
    let mut people = Vec::new();
    for (id, known) in dir.users() {
        if id == account.me || known.deleted {
            continue;
        }
        let Some(access_hash) = known.hash else {
            continue;
        };
        let name = known.name.trim();
        if name.is_empty() || name == "Unknown" || name == "Deleted Account" {
            continue;
        }
        let first = name.split_once(' ').map(|(first, _)| first).unwrap_or(name);
        if first.is_empty() {
            continue;
        }
        people.push(content::MentionTarget {
            name: name.to_owned(),
            first: first.to_owned(),
            handle: known.handle.clone(),
            user_id: id,
            access_hash,
        });
    }
    let names: Vec<String> = people.iter().map(|person| person.name.clone()).collect();
    people.retain(|person| names.iter().filter(|name| *name == &person.name).count() == 1);
    people
}

/// The lab's telegram-markers driver. A build without this returns to the server below and stays up.
pub fn prove_telegram_markers() -> Result<(), String> {
    let text = "né 🇵🇹 🦀 code";
    let entities = vec![
        tl::types::MessageEntityBold { offset: 0, length: 2 }.into(),
        tl::types::MessageEntityItalic { offset: 3, length: 4 }.into(),
        tl::types::MessageEntityTextUrl { offset: 8, length: 2, url: "https://crab.example".into() }.into(),
    ];
    let once = content::spans(text, &entities);
    let twice = content::spans(text, &content::entities_of(text, &once));
    if twice != once {
        return Err("styled ranges did not survive a trip through Telegram entities".into());
    }
    let (plain, styled) = content::from_markers("**hi** _there_");
    if plain != "hi there" {
        return Err(format!("markers were left in the text: {plain}"));
    }
    let bold = styled.iter().any(|span| matches!(span.style, protocol::Style::Bold) && span.start == 0 && span.end == 2);
    let italic = styled.iter().any(|span| matches!(span.style, protocol::Style::Italic) && span.start == 3 && span.end == 8);
    if !bold || !italic {
        return Err("bold and italic did not land on the words".into());
    }
    let kept = content::with_stored(&plain, &styled).ok_or("an edit with no markers dropped the bold")?;
    let still = content::spans(&plain, &kept);
    if !still.iter().any(|span| matches!(span.style, protocol::Style::Bold) && span.start == 0 && span.end == 2) {
        return Err("the bold run was not kept on edit".into());
    }
    Ok(())
}

/// The lab's reaction-reactors driver, Telegram's half. The list sent with the message names
/// each reactor where Telegram shows it; a hidden list (`can_see_list` unset) leaves the chip
/// a number.
pub fn prove_telegram_reactors() -> Result<(), String> {
    let emoji = |emoticon: &str| tl::enums::Reaction::Emoji(tl::types::ReactionEmoji { emoticon: emoticon.to_owned() }.into());
    let reacted = |user: i64, emoticon: &str| tl::types::MessagePeerReaction {
        big: false,
        unread: false,
        my: false,
        peer_id: tl::enums::Peer::User(tl::types::PeerUser { user_id: user }),
        date: 0,
        reaction: emoji(emoticon),
    };
    let results = |count: i32, emoticon: &str| tl::enums::ReactionCount::Count(tl::types::ReactionCount {
        chosen_order: None,
        reaction: emoji(emoticon),
        count,
    });
    let mut shown = tl::types::MessageReactions {
        min: false,
        can_see_list: true,
        reactions_as_tags: false,
        results: vec![results(2, "👍")],
        recent_reactions: Some(vec![reacted(42, "👍").into(), reacted(43, "👍").into()]),
        top_reactors: None,
    };
    let read = content::reactions_of(&shown.clone().into());
    if read.first().map(|one| one.who.as_slice()) != Some(&["u42".to_owned(), "u43".to_owned()][..]) {
        return Err(format!("the reactors the message carried were dropped: {:?}", read.first().map(|one| &one.who)));
    }
    shown.can_see_list = false;
    if content::reactions_of(&shown.into()).iter().any(|one| !one.who.is_empty()) {
        return Err("a list Telegram hides was shown anyway".into());
    }
    Ok(())
}

/// The lab's telegram-custom-emoji driver. A build without this stays up and the driver fails.
pub fn prove_telegram_custom_emoji() -> Result<(), String> {
    let labels = vec![content::EmojiLabel {
        label: ":\u{2B50}:".to_owned(),
        document_id: 6_402,
        fallback: "\u{2B50}".to_owned(),
        ambiguous: false,
    }];
    let (plain, entities) = content::outgoing("**go** :\u{2B50}:");
    let (plain, entities) = content::apply_emoji(&plain, entities.unwrap_or_default(), &labels, true);
    if plain != "go \u{2B50}" {
        return Err(format!("the fallback did not replace the label: {plain}"));
    }
    let bold = entities.iter().any(|entity| matches!(entity, tl::enums::MessageEntity::Bold(bold) if bold.offset == 0 && bold.length == 2));
    let emoji = entities.iter().any(|entity| {
        matches!(entity, tl::enums::MessageEntity::CustomEmoji(emoji) if emoji.document_id == 6_402 && emoji.offset == 3 && emoji.length == 1)
    });
    if !bold || !emoji {
        return Err("the custom emoji was not named on the send".into());
    }
    let (plain, entities) = content::apply_emoji("go :\u{2B50}:", Vec::new(), &labels, false);
    if plain != "go \u{2B50}" {
        return Err(format!("a chat without Premium did not keep the fallback: {plain}"));
    }
    if entities.iter().any(|entity| matches!(entity, tl::enums::MessageEntity::CustomEmoji(_))) {
        return Err("a chat without Premium kept the custom emoji".into());
    }
    Ok(())
}

/// The lab's telegram-service-lines driver. A build without this stays up and the driver fails.
pub fn prove_telegram_service_lines() -> Result<(), String> {
    content::prove_service_lines()
}

/// The lab's telegram-edited driver. A build without this stays up and the driver fails.
pub fn prove_telegram_edited() -> Result<(), String> {
    content::prove_edited()
}

/// The lab's telegram-premium driver. A build without this stays up and the driver fails.
pub fn prove_telegram_premium() -> Result<(), String> {
    premium::prove()
}

/// The lab's telegram-accounts driver. A build without this stays up and the driver fails.
pub fn prove_telegram_accounts() -> Result<(), String> {
    let mira = r#"{"user":7001,"name":"Mira Kessler","home":2,"dcs":[]}"#;
    let lea = r#"{"user":7101,"name":"Lea Hart","home":2,"dcs":[]}"#;
    let both = format!("[{mira},{lea}]");
    let signed = accounts(&both)?;
    if signed.len() != 2 || signed[0].account == signed[1].account {
        return Err(format!("two accounts were not keyed apart: {}", signed.len()));
    }
    let left = without(&both, &signed[1].account).ok_or("removing one dropped the session")?;
    let still = accounts(&left)?;
    if still.len() != 1 || still[0].account != signed[0].account {
        return Err("the account that stayed was not the first".into());
    }
    let again = combine(mira, lea)?;
    if accounts(&again)?.len() != 2 {
        return Err("a second sign-in replaced the first".into());
    }
    if accounts(&combine(&again, lea)?)?.len() != 2 {
        return Err("signing in again duplicated an account".into());
    }
    if accounts(mira)?.len() != 1 {
        return Err("an older session was not one account".into());
    }
    Ok(())
}

/// The lab's telegram-invoices driver. A build without this stays up and the driver fails.
pub fn prove_telegram_invoices() -> Result<(), String> {
    content::prove_invoices()
}

/// The lab's telegram-location driver. A build without this stays up and the driver fails.
pub fn prove_telegram_location() -> Result<(), String> {
    content::prove_location()?;
    match send::pasted_place("37.7749, -122.4194") {
        Some(send::PastedPlace::Point { lat, long }) if (lat - 37.7749).abs() < 1e-6 && (long + 122.4194).abs() < 1e-6 => {}
        _ => return Err("pasted coordinates are not a location".into()),
    }
    let venue = send::pasted_place("venue: Ferry Building | 1 The Embarcadero | 37.7955, -122.3937").ok_or("a venue line is not a venue")?;
    match send::pasted_media(&venue) {
        tl::enums::InputMedia::Venue(media) if media.title == "Ferry Building" && media.address == "1 The Embarcadero" => {}
        _ => return Err("a venue line is not sent as a venue".into()),
    }
    if send::pasted_place("see you at 37.7749, -122.4194").is_some() {
        return Err("words around coordinates were sent as a place".into());
    }
    Ok(())
}

/// The lab's telegram-polls driver. A build without this stays up and the driver fails.
pub fn prove_telegram_polls() -> Result<(), String> {
    content::prove_polls()
}

/// The lab's telegram-profile driver. A build without this stays up and the driver fails.
pub fn prove_telegram_profile() -> Result<(), String> {
    let recently = tl::enums::UserStatus::Recently(tl::types::UserStatusRecently { by_me: false });
    let week = tl::enums::UserStatus::LastWeek(tl::types::UserStatusLastWeek { by_me: false });
    let month = tl::enums::UserStatus::LastMonth(tl::types::UserStatusLastMonth { by_me: false });
    let empty = tl::enums::UserStatus::Empty;
    if peers::rough_seen(Some(&recently)) != "last seen recently" {
        return Err("recently did not read as last seen recently".into());
    }
    if peers::rough_seen(Some(&week)) != "last seen within a week" || peers::rough_seen(Some(&month)) != "last seen within a month" {
        return Err("a week or a month was not named".into());
    }
    if !peers::rough_seen(Some(&empty)).is_empty() || presence(Some(&recently)) != Presence::Unknown {
        return Err("a hidden or empty last seen was treated as offline".into());
    }
    if !peers::row_status(true, true, "last seen recently").is_empty() {
        return Err("our own row carried a last seen".into());
    }
    if info::birthday_line(14, 3, None).as_deref() != Some("14 March") || info::birthday_line(14, 3, Some(1990)).as_deref() != Some("14 March 1990") {
        return Err("a birthday was not named".into());
    }
    Ok(())
}

/// The lab's telegram-inline-bots driver. A build without this stays up and the driver fails.
pub fn prove_telegram_inline_bots() -> Result<(), String> {
    content::prove_inline_bots()?;
    match catalog::bot_query("@wiki rust") {
        Some(("wiki", "rust")) => {}
        other => return Err(format!("@wiki rust was not that bot's query, got {other:?}")),
    }
    if catalog::bot_query("hike").is_some() {
        return Err("a GIF search was treated as a bot".into());
    }
    Ok(())
}

/// The lab's telegram-live-location driver. A build without this stays up and the driver fails.
pub fn prove_telegram_live_location() -> Result<(), String> {
    content::prove_live_location()
}

/// The lab's telegram-mark-unread driver. A build without this stays up and the driver fails.
pub fn prove_telegram_mark_unread() -> Result<(), String> {
    if sync::unread_of(0, true) != 1 || sync::unread_of(4, true) != 4 {
        return Err("a manual unread mark does not leave a badge of at least one".into());
    }
    if sync::unread_of(0, false) != 0 || sync::unread_of(3, false) != 3 {
        return Err("clearing the mark did not keep the dialog's own unread count".into());
    }
    Ok(())
}

/// The lab's telegram-member-sections driver. A build without this stays up and the driver fails.
pub fn prove_telegram_member_sections() -> Result<(), String> {
    members::prove()
}

/// The lab's telegram-quote driver. A build without this stays up and the driver fails.
pub fn prove_telegram_quote() -> Result<(), String> {
    content::prove_quote()
}

/// The lab's telegram-album driver. A build without this stays up and the driver fails.
pub fn prove_telegram_album() -> Result<(), String> {
    content::prove_album()
}

/// The lab's telegram-stickers driver. A build without this stays up and the driver fails.
pub fn prove_telegram_stickers() -> Result<(), String> {
    content::prove_stickers()
}

/// The lab's telegram-mute-until driver. A build without this stays up and the driver fails.
pub fn prove_telegram_mute_until() -> Result<(), String> {
    if !filing::mute_holds(i32::MAX) {
        return Err("a mute until it is turned off does not stay muted".into());
    }
    if filing::mute_holds(1) {
        return Err("a mute that already ended still silences the chat".into());
    }
    let soon = (crate::hub::now() + 3_600).clamp(0, i64::from(i32::MAX)) as i32;
    if !filing::mute_holds(soon) {
        return Err("a mute until a later time is not muted".into());
    }
    Ok(())
}

/// The lab's telegram-mention-name driver. A build without this stays up and the driver fails.
pub fn prove_telegram_mention_name() -> Result<(), String> {
    let nameless = content::MentionTarget {
        name: "Ada Stone".to_owned(),
        first: "Ada".to_owned(),
        handle: String::new(),
        user_id: 7_003,
        access_hash: 7,
    };
    let named = content::MentionTarget {
        name: "Bea Moss".to_owned(),
        first: "Bea".to_owned(),
        handle: "bea_m".to_owned(),
        user_id: 7_002,
        access_hash: 8,
    };
    let (plain, entities) = content::apply_mentions("see @Ada Stone", Vec::new(), std::slice::from_ref(&nameless));
    if plain != "see Ada" {
        return Err(format!("a person with no username was not reduced to their first name: {plain}"));
    }
    let told = entities.iter().any(|entity| {
        matches!(entity, tl::enums::MessageEntity::InputMessageEntityMentionName(mention)
            if mention.offset == 4 && mention.length == 3 && matches!(&mention.user_id, tl::enums::InputUser::User(user) if user.user_id == 7_003))
    });
    if !told {
        return Err("the text mention was not named on the send".into());
    }
    let (plain, entities) = content::apply_mentions("see @Ada Stone and @Bea Moss", Vec::new(), &[nameless, named]);
    if plain != "see Ada and @bea_m" {
        return Err(format!("a username was not written as the handle: {plain}"));
    }
    if entities.iter().filter(|entity| matches!(entity, tl::enums::MessageEntity::InputMessageEntityMentionName(_))).count() != 1 {
        return Err("a person with a username was also given a text mention".into());
    }
    Ok(())
}

/// The lab's telegram-secret-chat driver. A build without this stays up and the driver fails.
pub fn prove_telegram_secret_chat() -> Result<(), String> {
    secret::prove()
}

/// The lab's telegram-large-files driver. A build without this stays up and the driver fails.
pub fn prove_telegram_large_files() -> Result<(), String> {
    files::prove_large_files()
}

/// The lab's telegram-link-preview driver. A build without this stays up and the driver fails.
pub fn prove_telegram_link_preview() -> Result<(), String> {
    content::prove_link_preview()
}

/// The lab's telegram-typing-activity driver. A build without this stays up and the driver fails.
pub fn prove_telegram_typing_activity() -> Result<(), String> {
    use tl::enums::SendMessageAction::*;
    if live::noted(&SendMessageRecordAudioAction) != Some(protocol::Compose::RecordingVoice) {
        return Err("a voice recording was read as typing".into());
    }
    if live::noted(&SendMessageCancelAction).is_some() {
        return Err("a cancelled action was still shown".into());
    }
    let photo = SendMessageUploadPhotoAction(tl::types::SendMessageUploadPhotoAction { progress: 40 });
    if live::noted(&photo) != Some(protocol::Compose::UploadingPhoto) {
        return Err("sending a photo was read as typing".into());
    }
    if live::noted(&SendMessageChooseStickerAction) != Some(protocol::Compose::ChoosingSticker) {
        return Err("choosing a sticker was read as typing".into());
    }
    if live::action_word(&activity_action(protocol::Compose::RecordingVoice, 0)) != "record-audio" {
        return Err("recording a voice note was not told as a recording".into());
    }
    Ok(())
}

/// The lab's telegram-dice driver. A build without this stays up and the driver fails.
pub fn prove_telegram_dice() -> Result<(), String> {
    content::prove_dice_roll()?;
    if send::dice_emoticon("\u{1F3B2}") != Some("\u{1F3B2}") {
        return Err("a lone dice emoji is sent as a roll".into());
    }
    if send::dice_emoticon("\u{1F3B2} please").is_some() {
        return Err("words around a dice emoji stay ordinary text".into());
    }
    Ok(())
}

/// Whether `Block` blocks or, when they are already blocked, unblocks, and the relation that leaves.
fn block_choice(already: bool, contact: bool) -> (bool, Relation) {
    if already {
        (true, if contact { Relation::Contact } else { Relation::None })
    } else {
        (false, Relation::Blocked)
    }
}

fn split_name(name: &str) -> (String, String) {
    match name.split_once(' ') {
        Some((first, last)) => (first.to_owned(), last.to_owned()),
        None => (name.to_owned(), String::new()),
    }
}

fn learn_updates(account: &Account, answer: &tl::enums::Updates) {
    match answer {
        tl::enums::Updates::Updates(updates) => account.learn(&updates.users, &updates.chats),
        tl::enums::Updates::Combined(updates) => account.learn(&updates.users, &updates.chats),
        _ => {}
    }
}

fn relate(account: &Account, person_id: &str, action: RelateAction) -> Result<(), Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "USER_ID_INVALID".to_owned(), value: None };
    let (who @ Who::User(user_id), None) = Who::parse(person_id).ok_or_else(invalid)? else {
        return Err(invalid());
    };
    match action {
        RelateAction::Block => {
            let peer = account.input(who)?;
            let contact = account.dir().get(who).is_some_and(|known| known.contact);
            let (unblock, relation) = block_choice(account.user_blocked(user_id), contact);
            if unblock {
                account.ask(&tl::functions::contacts::Unblock { my_stories_from: false, id: peer })?;
            } else {
                account.ask(&tl::functions::contacts::Block { my_stories_from: false, id: peer })?;
            }
            account.set_user_blocked(user_id, !unblock);
            let _ = account.ctx.relation(person_id, relation);
        }
        RelateAction::Add => {
            let id = account.dir().input_user(who).ok_or_else(invalid)?;
            let known = account.dir().get(who).cloned().unwrap_or_default();
            let (first_name, last_name) = split_name(&known.name);
            let answer = account.ask(&tl::functions::contacts::AddContact {
                add_phone_privacy_exception: false,
                id,
                first_name,
                last_name,
                phone: String::new(),
                note: None,
            })?;
            learn_updates(account, &answer);
            let _ = account.ctx.relation(person_id, Relation::Contact);
        }
        RelateAction::Remove => {
            let id = account.dir().input_user(who).ok_or_else(invalid)?;
            let answer = account.ask(&tl::functions::contacts::DeleteContacts { id: vec![id] })?;
            learn_updates(account, &answer);
            if !account.user_blocked(user_id) {
                let _ = account.ctx.relation(person_id, Relation::None);
            }
        }
        RelateAction::Unblock => {
            let peer = account.input(who)?;
            let contact = account.dir().get(who).is_some_and(|known| known.contact);
            account.ask(&tl::functions::contacts::Unblock { my_stories_from: false, id: peer })?;
            account.set_user_blocked(user_id, false);
            let _ = account.ctx.relation(person_id, if contact { Relation::Contact } else { Relation::None });
        }
        RelateAction::Accept | RelateAction::Ignore | RelateAction::Cancel | RelateAction::IgnoreUser | RelateAction::UnignoreUser => {}
    }
    Ok(())
}

/// The lab's telegram-block-contact driver. A build without this returns to the server below and stays up.
pub fn prove_telegram_block_contact() -> Result<(), String> {
    let (unblock, relation) = block_choice(false, true);
    if unblock || relation != Relation::Blocked {
        return Err("block did not mark the person blocked".into());
    }
    let (unblock, relation) = block_choice(true, true);
    if !unblock || relation != Relation::Contact {
        return Err("blocking someone already blocked did not unblock a contact".into());
    }
    let (unblock, relation) = block_choice(true, false);
    if !unblock || relation != Relation::None {
        return Err("unblocking a stranger did not clear the block".into());
    }
    let (first, last) = split_name("Wren Halloran");
    if first != "Wren" || last != "Halloran" {
        return Err(format!("contact name split left {first} {last}"));
    }
    if !split_name("Wren").1.is_empty() {
        return Err("a one-word name invented a last name".into());
    }
    Ok(())
}

/// The lab's telegram-checklist driver. A build without this stays up and the driver fails.
pub fn prove_telegram_checklist() -> Result<(), String> {
    content::prove_checklist()
}
