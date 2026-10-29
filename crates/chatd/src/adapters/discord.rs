//! Discord, signed in as the user: the gateway websocket for everything live, the HTTP API for
//! history and for what the user does.
//!
//! The socket thread (`gateway.rs`) holds the gateway, turns its events into
//! `Context` calls and is the only one that writes to the socket; the command thread serves
//! HTTP commands and hands gateway sends to the socket thread's outbox. A separate
//! viewport thread serves cache reads and Watch updates without waiting for HTTP/backfill.
//!
//! What stays in memory is bounded and listed on [`State`]: roles and ids per server, the lists
//! the user owns, one visible member window, the voice states of the servers. Trees, people and
//! messages go to the store as READY and the events arrive.

mod content;
mod events;
/// Its socket waits are shared with the Tiny server's live socket.
pub(super) mod gateway;
mod info;
mod members;
mod model;
mod picker;
mod proto;
mod ready;
mod rest;
mod send;
mod voice;

use super::{serve_commands, keep_connected, Command, Context, Download, Failure, Refusal, Reply, Signed};
use crate::blobs::upload;
use crate::store::ConversationRow;
use base64::Engine;
use content::{Files, Names, Place};
use model::{number, Posted, User};
use protocol::{Conversation, ConversationKind, Message, MessageKind, Presence, RelateAction, Relation, SendState, ThreadSummary};
use rest::{Refused, Rest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The pause after a history fetch.
const PACE: Duration = Duration::from_millis(700);
/// A search asked and not answered within this does not reach Discord at all: the shell has
/// already been told the network did not answer.
const SEARCH_PATIENCE: Duration = Duration::from_secs(25);
/// Unread conversations fetched ahead of being opened, once per run.
const WARM: usize = 5;
/// A page of history, as the web client asks for it.
const PAGE: usize = 50;
/// Discord's own limit on a message.
const LONGEST: usize = 2000;
/// At most one typing signal per conversation in this long.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// Most addresses Discord renews in one request.
const RENEWED_TOGETHER: usize = 50;
/// Conversations whose fetched stretches of history are remembered, and stretches of each.
const COVERED: usize = 64;
const STRETCHES: usize = 8;

#[derive(Deserialize)]
struct Session {
    token: String,
}

/// A server's settings as the user made them in Discord. Times are unix seconds; `i64::MAX` is
/// "until I turn it back on".
#[derive(Default)]
struct Quiet {
    muted_until: Option<i64>,
    suppress_everyone: bool,
    suppress_roles: bool,
    channels: HashMap<String, i64>,
    /// "Hide muted channels", as the user set it for the server.
    hide_muted: bool,
    /// "Show all channels" is off: only what is opted in shows.
    opt_in: bool,
    /// Channels and categories opted in.
    opted_in: HashSet<String>,
    /// The server's notification level (0 all, 1 mentions, 2 nothing), and the channels and
    /// categories that set their own.
    level: u8,
    levels: HashMap<String, u8>,
}

/// One of a server's custom emoji or stickers, kept for the picker: about 40 bytes each.
struct Pickable {
    id: String,
    name: String,
    animated: bool,
    /// A sticker's format; 0 for an emoji.
    format: u8,
    /// The roles an emoji is limited to; empty for everyone.
    roles: Box<[u64]>,
}

struct RoleInfo {
    name: String,
    permissions: u64,
}

/// What is kept of a server once its tree is in the store.
struct GuildInfo {
    name: String,
    icon: Option<String>,
    owner: bool,
    /// Where it sits in the user's own order.
    position: i32,
    /// Unix seconds. A channel never read is unread only for what came after this.
    joined: i64,
    roles: HashMap<u64, RoleInfo>,
    my_roles: Vec<u64>,
    /// Category id to its position and name, to place a channel created later and to write
    /// its header when the first channel we may see appears under it.
    categories: HashMap<String, (i32, String)>,
    /// Ids only (channel, parent), to apply a changed mute setting to every row. The rows
    /// themselves are in the store.
    channels: Vec<(u64, u64)>,
    quiet: Quiet,
    /// Our own nickname there.
    nick: String,
    emojis: Vec<Pickable>,
    stickers: Vec<Pickable>,
}

/// The gateway session, to resume it after a dropped socket.
#[derive(Clone)]
struct Resumable {
    id: String,
    url: String,
}

/// Everything the adapter remembers. All of it is bounded by what the user owns or has on screen.
#[derive(Default)]
struct State {
    me: String,
    session: Option<Resumable>,
    guilds: HashMap<String, GuildInfo>,
    /// Friends, pending requests and direct partners by id: the name shown for them.
    users: HashMap<String, String>,
    relations: HashMap<String, Relation>,
    /// The People rows of those relationships, to store a friend's status line when it changes.
    rows: HashMap<String, crate::store::PersonRow>,
    /// Person id to the direct conversation with them.
    directs: HashMap<String, String>,
    /// Every direct and group conversation's id, to apply a changed mute setting to.
    privates: HashSet<String>,
    quiet_directs: Quiet,
    /// Friends' presence, replayed after a resume because chatd forgets it when the link drops.
    presence: HashMap<String, (Presence, String)>,
    /// When the earliest timed mute runs out.
    next_unmute: Option<i64>,
    members: members::Watched,
    search: members::Search,
    voice: voice::Voice,
    typed: HashMap<String, Instant>,
    warmed: bool,
    warming: HashSet<String>,
    /// What the last acknowledgement answered with; the web client sends it back on the next.
    ack_token: Option<String>,
    /// Our own presence and status line as the settings have them, and those settings as
    /// Discord sent them (`proto::status_settings`), to change one part and send back the rest.
    own: (Presence, String),
    status_settings: Vec<u8>,
    /// Per conversation, the stretches of history fetched whole in this session, as message
    /// ids from and to: reading on inside one asks Discord nothing.
    covered: HashMap<String, Vec<(u64, u64)>>,
    /// Files whose addresses ran out, waiting to be renewed together.
    renewing: Vec<(String, String, Reply<Download>)>,
    /// The last addresses renewed, old to fresh.
    renewed: HashMap<String, String>,
    /// Our plan (`premium_type`), which says what may be sent.
    premium: u8,
    /// Friends' status emoji and what they are doing, as their presence last said, so a voice
    /// state can be told beside it.
    doing: HashMap<String, (String, Option<protocol::Activity>)>,
    /// When the last profile was asked for: the route is watched, so one per 3 s at most.
    profiled: Option<Instant>,
    /// How many profiles have been asked for: one still waiting its turn that is not the newest
    /// was superseded, and is dropped rather than sent (`info.rs`).
    profile_asked: u64,
    /// Per channel, the read acknowledgement last sent and the newest one held back since
    /// (`mark_read`): one per channel per [`ACK_EVERY`].
    acks: HashMap<String, Ack>,
}

/// One channel's read acknowledgements.
#[derive(Default)]
struct Ack {
    sent: Option<Instant>,
    /// The newest message read since `sent`, sent when the 3 s are up.
    held: Option<String>,
}

/// Read acknowledgements to one channel are at most this often; the newest goes.
const ACK_EVERY: Duration = Duration::from_secs(3);

/// Frames the command thread wants sent, and a local socket pair that wakes the socket thread.
struct Outbox {
    queue: Mutex<gateway::SendQueue>,
    wake: gateway::WakePair,
}

struct Discord {
    ctx: Context,
    rest: Rest,
    token: String,
    state: Mutex<State>,
    outbox: Outbox,
    /// The last sequence number heard, for heartbeats and for resuming.
    sequence: AtomicU64,
    /// How many sessions in a row Discord refused to start, and what it said last.
    refused_sessions: Mutex<(u32, String)>,
    /// No session is asked for until the user does something (`gateway.rs::hold`).
    held: AtomicBool,
    /// This account's message searches: one in flight at a time, paced from when the last
    /// finished, with recent answers kept in memory.
    message_search: Mutex<info::MessageSearch>,
}

/// The user id a token starts with (its first part is the id in base64), so the account is named
/// by who it is.
fn user_of(token: &str) -> Option<String> {
    let first = token.split('.').next()?;
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(first.trim_end_matches('=')).ok()?;
    let id = String::from_utf8(bytes).ok()?;
    (!id.is_empty() && id.len() <= 20 && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

/// One account per sign-in, named by the user's id when the token shows it.
pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let parsed: Session = serde_json::from_str(session).map_err(|err| err.to_string())?;
    Ok(vec![Signed {
        account: user_of(&parsed.token).unwrap_or_else(|| "main".to_owned()),
        name: "Discord".to_owned(),
        session: session.to_owned(),
    }])
}

pub fn without(_session: &str, _account: &str) -> Option<String> {
    None
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let session: Session = serde_json::from_str(session).map_err(|err| err.to_string())?;
    let wake = gateway::WakePair::new().map_err(|err| err.to_string())?;
    // Nothing here touches the network or runs a program: who the session says it is (the
    // browser's version, the web client's build) is worked out on the adapter's own threads,
    // so chatd is serving before any of it.
    let discord = Arc::new(Discord {
        rest: Rest::new(session.token.clone(), ctx.clone()),
        ctx,
        token: session.token,
        state: Mutex::new(State::default()),
        outbox: Outbox {
            queue: Mutex::new(gateway::SendQueue::default()),
            wake,
        },
        sequence: AtomicU64::new(0),
        refused_sessions: Mutex::new((0, String::new())),
        held: AtomicBool::new(false),
        message_search: Mutex::new(info::MessageSearch::default()),
    });
    let socket = discord.clone();
    std::thread::spawn(move || keep_connected(&socket.ctx, || socket.connect()));
    let (commands, network_inbox) = channel();
    let viewport = discord.clone();
    std::thread::spawn(move || {
        // Cache reads and desired viewport updates must not sit behind history
        // backfill sleeps or an HTTP request. Preserve their order on one thread.
        while let Ok(command) = inbox.recv() {
            match command {
                command @ (Command::Members { .. } | Command::Watch(_) | Command::OpenSpace(_) | Command::SearchPeople(_)) => viewport.command(command),
                command => {
                    let stop = matches!(&command, Command::Stop);
                    if commands.send(command).is_err() || stop { break; }
                }
            }
        }
    });
    std::thread::spawn(move || {
        // A send with files uploads on a thread of its own, so the commands behind it (its own
        // cancel among them) are not held up for as long as the upload takes.
        let other = |command| match command {
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                discord.asked();
                let sending = discord.clone();
                let (channel, client) = (conversation_id.clone(), client_id.clone());
                upload::spawn(&discord.ctx, &conversation_id, &client_id, &with.files.clone(), move |up| {
                    sending.send_with(&channel, &client, &text, reply_to.as_deref(), &with, up)
                });
            }
            // A search can wait on Discord's tight search limits: it runs on a thread of its
            // own, so the sends and reactions behind it are not held up, and one queued past
            // the shell's patience does not ask at all.
            Command::Search { query, scope, at, reply } => {
                discord.asked();
                if at.elapsed() < SEARCH_PATIENCE {
                    Discord::search_queued(&discord, query, scope, at, reply);
                } else {
                    drop(reply);
                }
            }
            // A profile waits its turn (one per 3 s) on a thread of its own, so a send behind it
            // is never held up, and one superseded while it waits is dropped.
            Command::Profile { person_id, reply } => Discord::profile_queued(&discord, person_id, None, reply),
            Command::ProfileIn { person_id, space_id, reply } => Discord::profile_queued(&discord, person_id, Some(space_id), reply),
            command => discord.command(command),
        };
        serve_commands(&discord.ctx, network_inbox, PACE, |id| discord.backfill(id), other)
    });
    Ok(())
}

/// A pause a person might make: `from` to `to` milliseconds.
fn jitter(from: u64, to: u64) -> Duration {
    Duration::from_millis(from + gateway::random() % (to - from).max(1))
}

/// Names for the ids inside one message.
struct Lookup<'a> {
    discord: &'a Discord,
    guild: Option<&'a str>,
    mentions: &'a [User],
}

impl Names for Lookup<'_> {
    fn user(&self, id: &str) -> Option<String> {
        if let Some(user) = self.mentions.iter().find(|user| user.id == id) {
            let nick = user.member.as_ref().and_then(|member| member.nick.clone());
            return Some(nick.filter(|nick| !nick.is_empty()).unwrap_or_else(|| user.name().to_owned()));
        }
        crate::hub::lock(&self.discord.state).users.get(id).cloned()
    }

    fn role(&self, id: &str) -> Option<String> {
        let state = crate::hub::lock(&self.discord.state);
        let guild = state.guilds.get(self.guild?)?;
        guild.roles.get(&number(id)).map(|role| role.name.clone())
    }

    fn channel(&self, id: &str) -> Option<String> {
        self.discord.ctx.known(id).ok().flatten().map(|row| row.title)
    }

    fn space(&self, id: &str) -> Option<String> {
        crate::hub::lock(&self.discord.state).guilds.get(id).map(|guild| guild.name.clone())
    }
}

/// An emoji as a path names it: every byte that is not plainly safe in a path, escaped.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Which messages a page of history is.
enum Stretch<'a> {
    Newest,
    Before(&'a str),
    After(&'a str),
    Around(&'a str),
}

fn refusal(refused: Refused) -> Refusal {
    Refusal::Refused(refused.reason())
}

impl Discord {
    fn log(&self, what: &str, why: impl std::fmt::Display) {
        eprintln!("chatd: Discord: {what}: {why}");
    }

    fn me(&self) -> String {
        crate::hub::lock(&self.state).me.clone()
    }

    /// A request, with the socket thread woken when it shows the session is over.
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, Refused> {
        let answer = self.rest.call(method, path, body);
        if matches!(answer, Err(Refused::Expired)) {
            self.wake();
        }
        answer
    }

    /// A message object into the model. Its files are returned for registering.
    fn convert(&self, posted: &Posted, guild: Option<&str>, kind: ConversationKind) -> (Message, Files) {
        let me = self.me();
        let lookup = Lookup {
            discord: self,
            guild,
            mentions: &posted.mentions,
        };
        // Outside a server a friend goes by the nickname the user gave them.
        let author = posted.author.as_ref().filter(|_| guild.is_none() && posted.webhook_id.is_none());
        let author = author.and_then(|author| crate::hub::lock(&self.state).users.get(&author.id).cloned());
        let place = Place {
            account: &self.ctx.account,
            me: &me,
            guild,
            names: &lookup,
            author,
        };
        let (mut message, files) = content::message(posted, &place);
        message.mentions_me = self.mentions_me(posted, guild, kind, &me);
        (message, files)
    }

    /// Discord's own rule for a mention, with the suppress settings the user made there.
    fn mentions_me(&self, posted: &Posted, guild: Option<&str>, kind: ConversationKind, me: &str) -> bool {
        if posted.author.as_ref().is_some_and(|author| author.id == me) {
            return false;
        }
        let named = posted.mentions.iter().any(|user| user.id == me);
        // Every message of a direct conversation already counts; only a group's can single me out.
        if kind == ConversationKind::Direct {
            return false;
        }
        let state = crate::hub::lock(&self.state);
        let Some(info) = guild.and_then(|guild| state.guilds.get(guild)) else {
            return named || posted.mention_everyone;
        };
        let role = posted.mention_roles.iter().any(|role| info.my_roles.contains(&number(role)));
        named || (posted.mention_everyone && !info.quiet.suppress_everyone) || (role && !info.quiet.suppress_roles)
    }

    /// Records the thread a message started, so the shell can open it like any conversation.
    fn thread_of(&self, posted: &Posted, parent: &Conversation) -> rusqlite::Result<()> {
        let Some(thread) = &posted.thread else {
            return Ok(());
        };
        if self.ctx.known(&thread.id)?.is_some() {
            return Ok(());
        }
        // Outside the tree: a thread we are not in is not listed there. It names its parent,
        // so it goes when the channel goes.
        let title = thread.name.clone().unwrap_or_else(|| "Thread".to_owned());
        if let Some(guild) = self.guild_of(parent) {
            self.ctx.set_note(&format!("guild:{}", thread.id), &guild);
        }
        self.ctx.conversation(&ConversationRow {
            parent: Some(parent.key.id.clone()),
            // A thread started from a message has that message's id.
            root: Some(thread.id.clone()),
            ..ConversationRow::new(&thread.id, ConversationKind::Thread, title)
        })
    }

    /// Remembers that the messages `from..=to` of a conversation are stored whole.
    fn cover(&self, channel: &str, from: u64, to: u64) {
        let mut state = crate::hub::lock(&self.state);
        if state.covered.len() >= COVERED && !state.covered.contains_key(channel) {
            state.covered.clear();
        }
        let stretches = state.covered.entry(channel.to_owned()).or_default();
        stretches.push((from, to));
        stretches.sort_unstable();
        let mut joined: Vec<(u64, u64)> = Vec::with_capacity(stretches.len());
        for stretch in stretches.drain(..) {
            match joined.last_mut() {
                Some(last) if stretch.0 <= last.1 => last.1 = last.1.max(stretch.1),
                _ => joined.push(stretch),
            }
        }
        // The newest stretches are the ones being read.
        let extra = joined.len().saturating_sub(STRETCHES);
        *stretches = joined.split_off(extra);
    }

    /// True when what follows message `after` is already stored: it lies inside a stretch
    /// fetched in this session, short of its end.
    fn covered_after(&self, channel: &str, after: u64) -> bool {
        let state = crate::hub::lock(&self.state);
        let stretches = state.covered.get(channel);
        stretches.is_some_and(|stretches| stretches.iter().any(|(from, to)| *from <= after && after < *to))
    }

    /// One page of a conversation's history into the store.
    fn page(&self, conversation: &Conversation, stretch: Stretch) -> Result<(), Failure> {
        let channel = &conversation.key.id;
        let query = match stretch {
            Stretch::Newest => format!("limit={PAGE}"),
            Stretch::Before(id) => format!("before={id}&limit={PAGE}"),
            Stretch::After(id) => format!("after={id}&limit={PAGE}"),
            Stretch::Around(id) => format!("around={id}&limit={PAGE}"),
        };
        let text = self.call("GET", &format!("/channels/{channel}/messages?{query}"), None)?;
        let posted: Vec<Posted> = serde_json::from_str(&text)?;
        drop(text);
        let guild = self.guild_of(conversation);
        let mut messages = Vec::with_capacity(posted.len());
        let mut files = Files::default();
        for posted in &posted {
            let (message, referred) = self.convert(posted, guild.as_deref(), conversation.kind);
            files.merge(referred);
            self.thread_of(posted, conversation)?;
            messages.push(message);
        }
        let full = messages.len() >= PAGE;
        if matches!(stretch, Stretch::Newest) && full {
            self.close_gap(channel, &messages)?;
        }
        files.register(&self.ctx)?;
        self.ctx.history(channel, &messages)?;
        let ids = messages.iter().map(|message| number(&message.id));
        let (Some(oldest), Some(newest)) = (ids.clone().min(), ids.max()) else {
            return Ok(());
        };
        self.forget_deleted(channel, &messages, oldest, newest)?;
        // What the page proves is stored: itself, and up to whatever it was asked from.
        let (from, to) = match stretch {
            Stretch::Newest => (oldest, u64::MAX),
            Stretch::Before(id) => (oldest, number(id)),
            Stretch::After(id) => (number(id), if full { newest } else { u64::MAX }),
            Stretch::Around(_) => (oldest, newest),
        };
        self.cover(channel, from, to);
        Ok(())
    }

    /// What is stored between the ends of a page Discord just sent, and is not in it, was
    /// deleted while we were away.
    fn forget_deleted(&self, channel: &str, page: &[Message], oldest: u64, newest: u64) -> rusqlite::Result<()> {
        let stored = self.ctx.stored_ids(channel, &oldest.to_string(), &newest.to_string(), crate::store::KEEP_MESSAGES)?;
        for id in stored.iter().filter(|id| !page.iter().any(|message| message.id == **id)) {
            // Only what Discord named: a message of ours still on its way has no id of its.
            let sent = self.ctx.stored_message(channel, id)?.is_some_and(|message| message.state == SendState::Sent);
            if sent && (oldest..=newest).contains(&number(id)) {
                self.ctx.deleted(channel, id)?;
            }
        }
        Ok(())
    }

    /// The newest page does not reach what is stored: everything between was never fetched.
    /// Stored messages older than the page go, so the cache has no silent hole and scrolling up
    /// asks Discord again.
    fn close_gap(&self, channel: &str, page: &[Message]) -> rusqlite::Result<()> {
        let Some(oldest) = page.iter().map(|message| number(&message.id)).min() else {
            return Ok(());
        };
        for _ in 0..crate::store::KEEP_MESSAGES {
            match self.ctx.newest(channel)? {
                Some(stored) if number(&stored) < oldest => self.ctx.deleted(channel, &stored)?,
                _ => break,
            }
        }
        Ok(())
    }

    /// The server a conversation is in. A thread recorded outside a tree has it in a note.
    fn guild_of(&self, conversation: &Conversation) -> Option<String> {
        match (&conversation.space, conversation.kind) {
            (Some(space), _) => Some(space.clone()),
            (None, ConversationKind::Thread) => self.ctx.note(&format!("guild:{}", conversation.key.id)),
            _ => None,
        }
    }

    fn backfill(&self, channel: &str) {
        // Warming is nobody's click, so it does not run at a machine's pace.
        if crate::hub::lock(&self.state).warming.remove(channel) {
            std::thread::sleep(jitter(400, 2500));
        } else {
            self.asked();
        }
        let conversation = match self.ctx.known(channel) {
            Ok(Some(conversation)) => conversation,
            _ => return,
        };
        // A forum's posts are its threads; it has no messages of its own.
        if conversation.kind == ConversationKind::Forum {
            return;
        }
        self.fetch(&conversation, Stretch::Newest);
    }

    fn fetch(&self, conversation: &Conversation, stretch: Stretch) {
        if let Err(Failure::SignedOut(why) | Failure::Dropped(why)) = self.page(conversation, stretch) {
            self.log("history", why);
        }
    }

    /// Adds or takes back our reaction. `emoji` is the emoji itself, or a custom one: as the
    /// message spells it (`:name:`) when it is already there, or as the id of its registered
    /// file (`emoji:<id>`). Discord wants a custom one as `name:id`.
    ///
    /// The query is what the web client adds; it is from the community documentation and
    /// unconfirmed against a real account. So is sending a placeholder name with an id whose
    /// name is not known here (Discord is said to go by the id).
    fn react(&self, channel: &str, message: &str, emoji: &str, on: bool) -> Result<(), Refusal> {
        let spelled = emoji.len() > 2 && emoji.starts_with(':') && emoji.ends_with(':');
        let named = if spelled || emoji.starts_with("emoji:") {
            let stored = self.ctx.stored_message(channel, message).ok().flatten();
            let reactions = stored.map(|stored| stored.reactions).unwrap_or_default();
            let known = reactions.into_iter().find(|reaction| match &reaction.file {
                Some(file) => reaction.emoji == emoji || file.id == emoji,
                None => false,
            });
            let id = known.as_ref().and_then(|reaction| reaction.file.as_ref()).map(|file| file.id.clone());
            let id = id.or_else(|| emoji.starts_with("emoji:").then(|| emoji.to_owned()));
            // CONTRACT: `:name:` says which custom emoji only while it is on the message. One
            // that is not there has to come as its file's id.
            let Some(id) = id.as_deref().and_then(|id| id.strip_prefix("emoji:")) else {
                return Err(Refusal::Refused("That emoji is not on the message, so tinyclient cannot tell Discord which one it is.".to_owned()));
            };
            let name = known.map_or_else(|| "emoji".to_owned(), |reaction| reaction.emoji.trim_matches(':').to_owned());
            format!("{name}:{id}")
        } else {
            emoji.to_owned()
        };
        let reaction = format!("/channels/{channel}/messages/{message}/reactions/{}", escaped(&named));
        let asked = match on {
            true => self.call("PUT", &format!("{reaction}/%40me?location=Message%20Reaction%20Picker&type=0"), None),
            false => self.call("DELETE", &format!("{reaction}/0/%40me?location=Message%20Inline%20Button&burst=false"), None),
        };
        asked.map(drop).map_err(refusal)
    }

    /// Replaces the text of one of our messages. The store changes with Discord's answer.
    fn edit(&self, channel: &str, message: &str, text: &str) -> Result<(), Refusal> {
        if text.chars().count() > LONGEST {
            return Err(Refusal::Refused(format!("Message too long: Discord allows {LONGEST} characters.")));
        }
        let path = format!("/channels/{channel}/messages/{message}");
        let edited = self.call("PATCH", &path, Some(json!({"content": text}))).map_err(refusal)?;
        let unreadable = |err: String| Refusal::Refused(format!("Discord's answer was unreadable: {err}"));
        let mut posted: Posted = serde_json::from_str(&edited).map_err(|err| unreadable(err.to_string()))?;
        if posted.channel_id.is_empty() {
            posted.channel_id = channel.to_owned();
        }
        self.updated(posted).map_err(|failed| match failed {
            Failure::SignedOut(why) | Failure::Dropped(why) => unreadable(why),
        })
    }

    fn delete(&self, channel: &str, message: &str) -> Result<(), Refusal> {
        self.call("DELETE", &format!("/channels/{channel}/messages/{message}"), None).map_err(refusal)?;
        self.ctx.deleted(channel, message).map_err(|err| Refusal::Refused(err.to_string()))
    }

    /// Answers, withdraws or ends a friendship, or blocks. One request, and only ever here:
    /// managing friends is what Discord's anti-abuse system watches most closely.
    ///
    /// The bodies and the context header are the web client's as the community documents them,
    /// unconfirmed against a real account.
    fn relate(&self, person: &str, action: RelateAction) -> Result<(), Refusal> {
        let path = format!("/users/@me/relationships/{person}");
        let context = |location: &str| base64::engine::general_purpose::STANDARD.encode(json!({"location": location}).to_string());
        let (method, body, location, becomes) = match action {
            RelateAction::Accept => ("PUT", Some(json!({})), "Friends", Relation::Friend),
            RelateAction::Block => ("PUT", Some(json!({"type": 2})), "ContextMenu", Relation::Blocked),
            RelateAction::Ignore | RelateAction::Cancel | RelateAction::Remove => ("DELETE", None, "Friends", Relation::None),
        };
        let context = context(location);
        self.rest.call_with(method, &path, body, &[("X-Context-Properties", &context)]).map_err(refusal)?;
        {
            let mut state = crate::hub::lock(&self.state);
            match becomes {
                Relation::None => {
                    state.relations.remove(person);
                    state.presence.remove(person);
                }
                becomes => drop(state.relations.insert(person.to_owned(), becomes)),
            }
        }
        self.ctx.relation(person, becomes).map_err(|err| Refusal::Refused(err.to_string()))
    }

    /// Our own presence, and the status line unless it is `None`. Both are settings: the web
    /// client saves them (protobuf, `settings-proto/1`) and tells the gateway (op 3), and so
    /// does this.
    ///
    /// Unverified against a real account: the field numbers of the status settings are the
    /// community's (`proto.rs`), and the whole status block is sent back so nothing in it is
    /// lost if Discord replaces rather than merges it.
    fn set_presence(&self, presence: Presence, status: Option<String>) -> Result<(), Refusal> {
        let word = match presence {
            Presence::Idle => "idle",
            Presence::Dnd => "dnd",
            Presence::Offline => "invisible",
            Presence::Online | Presence::Unknown | Presence::Busy => "online",
        };
        let (kept, text) = {
            let state = crate::hub::lock(&self.state);
            (state.status_settings.clone(), status.clone().unwrap_or_else(|| state.own.1.clone()))
        };
        let settings = proto::with_status(&kept, word, status.as_deref());
        let body = json!({"settings": proto::settings(&settings)});
        self.call("PATCH", "/users/@me/settings-proto/1", Some(body)).map_err(refusal)?;
        let custom = match text.is_empty() {
            true => json!([]),
            false => json!([{"name": "Custom Status", "type": 4, "state": text, "emoji": null}]),
        };
        self.tell(3, json!({"status": word, "since": 0, "activities": custom, "afk": false}));
        {
            let mut state = crate::hub::lock(&self.state);
            state.status_settings = settings;
            state.own = (presence, text.clone());
        }
        self.ctx.presence(&self.me(), presence, &text, true);
        Ok(())
    }

    /// Acknowledges reading up to `message`. At most one per channel per [`ACK_EVERY`]: a read
    /// inside that time is held, and the newest held one is sent when the time is up, once.
    fn mark_read(&self, channel: &str, message: &str) {
        // Opening a message request reads nothing on Discord: its sender is not told.
        let request = self.ctx.known(channel).ok().flatten().is_some_and(|row| row.special == protocol::Special::Request);
        if request {
            return;
        }
        let message = {
            let mut state = crate::hub::lock(&self.state);
            // Only channels inside their 3 s are worth keeping.
            if state.acks.len() > 256 {
                state.acks.retain(|_, ack| ack.held.is_some() || ack.sent.is_some_and(|at| at.elapsed() < ACK_EVERY));
            }
            let ack = state.acks.entry(channel.to_owned()).or_default();
            let first = ack.held.is_none();
            let newest = match ack.held.take() {
                Some(held) if number(&held) > number(message) => held,
                _ => message.to_owned(),
            };
            if let Some(left) = ack.sent.map(|at| ACK_EVERY.saturating_sub(at.elapsed())).filter(|left| !left.is_zero()) {
                // The first read held in this window sends the newest when the window ends.
                ack.held = Some(newest.clone());
                if first {
                    let (ctx, channel) = (self.ctx.clone(), channel.to_owned());
                    let _ = std::thread::Builder::new().name("discord-ack".into()).stack_size(64 * 1024).spawn(move || {
                        if !ctx.pause(left) {
                            let _ = ctx.commands.send(Command::MarkRead { conversation_id: channel, message_id: newest });
                        }
                    });
                }
                return;
            }
            ack.sent = Some(Instant::now());
            newest
        };
        let message = message.as_str();
        let token = crate::hub::lock(&self.state).ack_token.clone();
        // `last_viewed` is the day, counted from Discord's epoch, as the web client sends it.
        // Unconfirmed against a real account.
        let day = (crate::hub::now() - 1_420_070_400) / 86_400;
        let body = json!({"token": token, "last_viewed": day});
        match self.call("POST", &format!("/channels/{channel}/messages/{message}/ack"), Some(body)) {
            Ok(text) => {
                let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                crate::hub::lock(&self.state).ack_token = answer["token"].as_str().map(str::to_owned);
            }
            Err(refused) => self.log("mark read", refused),
        }
    }

    fn send(&self, channel: &str, client_id: &str, text: &str, reply_to: Option<&str>) -> Result<Message, String> {
        if text.chars().count() > LONGEST {
            return Err(format!("Message too long: Discord allows {LONGEST} characters."));
        }
        let conversation = self.ctx.known(channel).map_err(|err| err.to_string())?;
        let conversation = conversation.ok_or_else(|| "This conversation is gone.".to_owned())?;
        let guild = self.guild_of(&conversation);
        let mut body = json!({
            "content": text,
            "nonce": client_id,
            "tts": false,
            "flags": 0,
            "mobile_network_type": "unknown",
        });
        if let Some(reply_to) = reply_to {
            let mut reference = json!({"channel_id": channel, "message_id": reply_to});
            if let Some(guild) = &guild {
                reference["guild_id"] = json!(guild);
            }
            body["message_reference"] = reference;
            body["allowed_mentions"] = json!({"parse": ["users", "roles", "everyone"], "replied_user": true});
        }
        let accepted = self
            .call("POST", &format!("/channels/{channel}/messages"), Some(body))
            .map_err(|refused| refused.reason())?;
        let posted: Posted = serde_json::from_str(&accepted).map_err(|err| format!("Discord's answer was unreadable: {err}"))?;
        let (message, files) = self.convert(&posted, guild.as_deref(), conversation.kind);
        let _ = files.register(&self.ctx);
        Ok(message)
    }

    fn typing(&self, channel: &str) {
        {
            let mut state = crate::hub::lock(&self.state);
            if state.typed.get(channel).is_some_and(|at| at.elapsed() < TYPING_EVERY) {
                return;
            }
            state.typed.retain(|_, at| at.elapsed() < TYPING_EVERY);
            state.typed.insert(channel.to_owned(), Instant::now());
        }
        if let Err(refused) = self.call("POST", &format!("/channels/{channel}/typing"), None) {
            self.log("typing", refused);
        }
    }

    /// The direct conversation with a person. One request, and only when the user asked: opening
    /// conversations is something Discord watches.
    fn start_direct(&self, person: &str) -> Result<String, Refusal> {
        if let Some(known) = crate::hub::lock(&self.state).directs.get(person) {
            return Ok(known.clone());
        }
        let refuse = |why: String| Refusal::Refused(why);
        let created = self
            .call("POST", "/users/@me/channels", Some(json!({"recipients": [person]})))
            .map_err(|refused| refuse(refused.reason()))?;
        let channel: model::Channel = serde_json::from_str(&created).map_err(|err| refuse(err.to_string()))?;
        self.direct(&channel, &HashMap::new()).map_err(|err| refuse(err.to_string()))?;
        Ok(channel.id)
    }

    /// The thread a channel message starts. One request, and only when the user asked: Discord
    /// watches thread creation. A message that already has a thread answers with that channel.
    fn start_thread(&self, channel: &str, message_id: &str, name: &str) -> Result<String, Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let parent = self
            .ctx
            .known(channel)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This conversation is gone.".to_owned()))?;
        if !matches!(parent.kind, ConversationKind::Channel | ConversationKind::Announcement) {
            return Err(refuse("A thread starts from a channel message.".to_owned()));
        }
        let stored = self
            .ctx
            .stored_message(channel, message_id)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("That message is not here.".to_owned()))?;
        if stored.deleted || matches!(stored.kind, MessageKind::System) {
            return Err(refuse("That message cannot start a thread.".to_owned()));
        }
        let name = thread_name(name);
        if let Some(existing) = stored.thread.as_ref() {
            self.record_thread(&parent, &existing.id, message_id, &name)?;
            return Ok(existing.id.clone());
        }
        let created = self
            .call(
                "POST",
                &format!("/channels/{channel}/messages/{message_id}/threads"),
                Some(json!({ "name": name })),
            )
            .map_err(refusal)?;
        let thread: model::Channel = serde_json::from_str(&created)
            .map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
        let title = thread.name.clone().filter(|title| !title.trim().is_empty()).unwrap_or_else(|| name.clone());
        self.record_thread(&parent, &thread.id, message_id, &title)?;
        let mut message = stored;
        message.thread = Some(ThreadSummary {
            id: thread.id.clone(),
            replies: thread.message_count.unwrap_or(0),
            last: 0,
            participants: Vec::new(),
            unread: 0,
        });
        self.ctx.history(channel, &[message]).map_err(|err| refuse(err.to_string()))?;
        Ok(thread.id)
    }

    /// The thread's conversation, when it is not recorded yet. Its root is the message it hangs
    /// off. A thread kept outside the tree still needs its server, which `guild_of` reads back.
    fn record_thread(&self, parent: &Conversation, thread_id: &str, root: &str, title: &str) -> Result<(), Refusal> {
        let refuse = |err: rusqlite::Error| Refusal::Refused(err.to_string());
        if self.ctx.known(thread_id).map_err(refuse)?.is_some() {
            return Ok(());
        }
        if let Some(guild) = self.guild_of(parent) {
            self.ctx.set_note(&format!("guild:{thread_id}"), &guild);
        }
        let mut row = ConversationRow::new(thread_id, ConversationKind::Thread, title);
        row.parent = Some(parent.key.id.clone());
        row.root = Some(root.to_owned());
        row.private = parent.private;
        row.read_only = parent.read_only;
        self.ctx.conversation(&row).map_err(refuse)
    }

    /// Where a registered file is fetched now. Attachment addresses are signed and run out
    /// (`ex` is the hex time); one that has is put aside to be exchanged for a fresh one.
    ///
    /// CONTRACT: `Command::Download` names one file, but Discord renews up to fifty addresses
    /// in one request and the media cache asks about every waiting file in a row. So an
    /// expired one is not answered at once: it waits in `renewing` until a `Download` with no
    /// id, which this sends to its own inbox behind whatever is already queued, renews them
    /// all together.
    fn download(&self, id: String, url: String, reply: Reply<Download>) {
        if id.is_empty() && url.is_empty() {
            return self.renew();
        }
        let expires = url.split(['?', '&']).find_map(|part| i64::from_str_radix(part.strip_prefix("ex=")?, 16).ok());
        if expires.is_none_or(|expires| expires >= crate::hub::now() + 60) {
            return answer(reply, Ok(self.from(url)));
        }
        // Asked about twice (two fetches renewing at once each name the other's file): the
        // answer Discord already gave is given again.
        let renewed = crate::hub::lock(&self.state).renewed.get(&url).cloned();
        if let Some(fresh) = renewed {
            return answer(reply, Ok(self.from(fresh)));
        }
        let first = {
            let mut state = crate::hub::lock(&self.state);
            state.renewing.push((id, url, reply));
            state.renewing.len() == 1
        };
        if first {
            let (nobody, _) = std::sync::mpsc::channel();
            let marker = Command::Download {
                id: String::new(),
                url: String::new(),
                reply: nobody,
            };
            let _ = self.ctx.commands.send(marker);
        }
    }

    fn from(&self, url: String) -> Download {
        Download {
            url,
            headers: vec![("User-Agent".to_owned(), self.rest.identity().user_agent.clone())],
        }
    }

    /// Exchanges every address put aside for a fresh one, fifty to a request, records each
    /// (`Context::media_refreshed`) so it is used from now on, and answers whoever asked.
    ///
    /// A resized picture's address is the attachment's with our own size appended
    /// (`content::resized`): Discord is asked about the attachment's, and the size goes back on.
    fn renew(&self) {
        let waiting = std::mem::take(&mut crate::hub::lock(&self.state).renewing);
        let plain = |url: &str| -> (String, String) {
            let cut = url.find("&format=webp&width=").or_else(|| url.find("?format=webp&width="));
            cut.map_or_else(|| (url.to_owned(), String::new()), |cut| (url[..cut].to_owned(), url[cut + 1..].to_owned()))
        };
        for batch in waiting.chunks(RENEWED_TOGETHER) {
            let mut urls: Vec<String> = batch.iter().map(|(_, url, _)| plain(url).0).collect();
            urls.sort();
            urls.dedup();
            let asked = self.call("POST", "/attachments/refresh-urls", Some(json!({"attachment_urls": urls})));
            let renewed: Vec<(String, String)> = match &asked {
                Ok(text) => {
                    let answer: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                    let pairs = answer["refreshed_urls"].as_array().map_or(&[][..], Vec::as_slice).iter();
                    let pairs = pairs.filter_map(|pair| Some((pair["original"].as_str()?.to_owned(), pair["refreshed"].as_str()?.to_owned())));
                    pairs.collect()
                }
                Err(_) => Vec::new(),
            };
            for (id, url, reply) in batch {
                let (attachment, size) = plain(url);
                let fresh = renewed.iter().find(|(original, _)| *original == attachment).map(|(_, fresh)| match size.is_empty() {
                    true => fresh.clone(),
                    false => format!("{fresh}{}{size}", if fresh.contains('?') { '&' } else { '?' }),
                });
                let outcome = match (fresh, &asked) {
                    (Some(fresh), _) => {
                        if let Err(err) = self.ctx.media_refreshed(id, &fresh) {
                            self.log("media", err);
                        }
                        let mut state = crate::hub::lock(&self.state);
                        if state.renewed.len() >= 4 * RENEWED_TOGETHER {
                            state.renewed.clear();
                        }
                        state.renewed.insert(url.clone(), fresh.clone());
                        drop(state);
                        Ok(self.from(fresh))
                    }
                    // Not renewed: the cache is told where the file was, and finds it gone.
                    (None, Ok(_)) => Ok(self.from(url.clone())),
                    (None, Err(refused)) => Err(Refusal::Refused(refused.reason())),
                };
                let _ = reply.send(outcome);
            }
        }
    }

    fn handoff(&self, channel: &str) -> Result<String, Refusal> {
        let conversation = self.ctx.known(channel).ok().flatten();
        let conversation = conversation.ok_or_else(|| Refusal::Refused("This conversation is gone.".to_owned()))?;
        let guild = self.guild_of(&conversation);
        Ok(voice::handoff_link(guild.as_deref(), channel))
    }

    fn command(&self, command: Command) {
        // Everything but these is the user doing something, which is what an adapter that
        // stopped asking for a session was waiting for. The screen moving counts when it moved
        // to something of this account; a watch that lapsed names nothing.
        let ours = |key: &Option<protocol::Key>| key.as_ref().is_some_and(|key| key.account() == self.ctx.account);
        let unasked = match &command {
            Command::Watch(watching) => !ours(&watching.conversation) && !ours(&watching.space),
            Command::Warm(_)
            | Command::Members { .. }
            | Command::Download { .. }
            | Command::Handoff { .. }
            | Command::CallLeave(_)
            | Command::CallAudio { .. }
            | Command::SignOut(_) => true,
            _ => false,
        };
        if !unasked {
            self.asked();
        }
        match command {
            // `serve_commands` keeps these two for itself.
            Command::Backfill(id) | Command::Warm(id) => self.backfill(&id),
            Command::History { conversation_id, before } => {
                if let Ok(Some(conversation)) = self.ctx.known(&conversation_id) {
                    self.fetch(&conversation, Stretch::Before(&before));
                }
            }
            // Sent for every page read forward: Discord is asked only when what follows is not
            // already stored from this session.
            Command::HistoryAfter { conversation_id, after } => {
                if self.covered_after(&conversation_id, number(&after)) {
                    return;
                }
                if let Ok(Some(conversation)) = self.ctx.known(&conversation_id) {
                    self.fetch(&conversation, Stretch::After(&after));
                }
            }
            Command::HistoryAround { conversation_id, around } => {
                let at = number(&around);
                let inside = {
                    let state = crate::hub::lock(&self.state);
                    let stretches = state.covered.get(&conversation_id);
                    stretches.is_some_and(|stretches| stretches.iter().any(|(from, to)| (*from..=*to).contains(&at)))
                };
                if inside && self.ctx.stored_message(&conversation_id, &around).ok().flatten().is_some() {
                    return;
                }
                if let Ok(Some(conversation)) = self.ctx.known(&conversation_id) {
                    self.fetch(&conversation, Stretch::Around(&around));
                }
            }
            Command::OpenSpace(space) => self.open_space(&space),
            Command::MarkRead { conversation_id, message_id } => self.mark_read(&conversation_id, &message_id),
            Command::Send { conversation_id, client_id, text, reply_to } => {
                let outcome = self.send(&conversation_id, &client_id, &text, reply_to.as_deref());
                self.ctx.sent(&conversation_id, &client_id, outcome);
            }
            Command::React { conversation_id, message_id, emoji, on, reply } => {
                answer(reply, self.react(&conversation_id, &message_id, &emoji, on))
            }
            Command::Edit { conversation_id, message_id, text, reply } => {
                answer(reply, self.edit(&conversation_id, &message_id, &text))
            }
            Command::Delete { conversation_id, message_id, reply } => answer(reply, self.delete(&conversation_id, &message_id)),
            Command::Relate { person_id, action, reply } => answer(reply, self.relate(&person_id, action)),
            Command::SetPresence { presence, status, reply } => answer(reply, self.set_presence(presence, status)),
            Command::Profile { person_id, reply } => answer(reply, self.profile(&person_id, None)),
            Command::ProfileIn { person_id, space_id, reply } => answer(reply, self.profile(&person_id, Some(&space_id))),
            Command::ConversationInfo { conversation_id, reply } => answer(reply, self.conversation_info(&conversation_id)),
            Command::SpaceInfo { space_id, reply } => answer(reply, self.space_info(&space_id)),
            Command::SetMute { conversation_id, notify, reply } => answer(reply, self.set_mute(&conversation_id, notify)),
            Command::Leave { conversation_id, reply } => answer(reply, self.leave(&conversation_id, false)),
            Command::CloseDirect { conversation_id, reply } => answer(reply, self.leave(&conversation_id, true)),
            Command::Pinned { conversation_id, reply } => answer(reply, self.pinned(&conversation_id)),
            Command::SearchShared { conversation_id, kind, before, limit, reply } => {
                answer(reply, self.search_shared(&conversation_id, kind, before.as_deref(), limit))
            }
            Command::Search { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Catalog { kind, space_id, query, cursor, reply } => {
                answer(reply, self.catalog(kind, space_id.as_deref(), &query, cursor.as_deref()))
            }
            Command::ReactionRules { conversation_id, reply } => drop(reply.send(Ok(self.reaction_rules(&conversation_id)))),
            Command::CancelSend { client_id, .. } => upload::cancel(&self.ctx.account, &client_id),
            // Never: a fetch per DM is the burst that flags accounts. chatd does not send it,
            // and it is refused here too should it ever arrive.
            Command::Previews(_) => {}
            Command::Typing(conversation_id) => self.typing(&conversation_id),
            Command::Watch(watching) => {
                self.watch(&watching);
                self.voice_watch(&watching);
            }
            Command::SearchPeople(query) => self.search_people(&query),
            Command::Members { conversation_id, offset, limit, reply } => {
                self.members(conversation_id, offset, limit, reply)
            }
            Command::StartDirect { person_id, reply } => drop(reply.send(self.start_direct(&person_id))),
            Command::StartThread { conversation_id, message_id, name, reply } => {
                drop(reply.send(self.start_thread(&conversation_id, &message_id, &name)))
            }
            Command::Download { id, url, reply } => self.download(id, url, reply),
            Command::Handoff { conversation_id, reply } => drop(reply.send(self.handoff(&conversation_id))),
            Command::CallJoin(conversation_id) => self.call_join(&conversation_id),
            Command::CallLeave(conversation_id) => self.call_leave(&conversation_id),
            Command::CallAudio { muted, deafened } => self.call_audio(muted, deafened),
            // The user asked to sign out, so the token should stop working, not just be
            // forgotten: the answer goes once Discord has ended the session, or refused to.
            Command::SignOut(done) => {
                let body = json!({"provider": null, "voip_provider": null});
                if let Err(refused) = self.call("POST", "/auth/logout", Some(body)) {
                    self.log("sign out", refused);
                }
                let _ = done.send(Ok(()));
            }
            command => command.unsupported(&self.ctx),
        }
    }
}

/// Discord's thread name: one line, at most 100 characters. Empty becomes "Thread".
fn thread_name(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = flat.chars().take(100).collect();
    let name = name.trim().to_owned();
    if name.is_empty() { "Thread".to_owned() } else { name }
}

/// Sends the answer to a command the shell is waiting on.
fn answer<T>(reply: Reply<T>, outcome: Result<T, Refusal>) {
    let _ = reply.send(outcome);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_is_named_by_the_user_id_in_the_token() {
        // "MTAwMA" is base64 for "1000".
        let found = accounts(r#"{"token":"MTAwMA.Gabcde.signature"}"#).unwrap();
        assert_eq!(found[0].account, "1000");
        // A token that does not start with an id still signs in.
        assert_eq!(accounts(r#"{"token":"opaque"}"#).unwrap()[0].account, "main");
    }
}
