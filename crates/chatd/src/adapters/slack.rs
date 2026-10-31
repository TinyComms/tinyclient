//! Slack, with the session the web client uses: its API for the sidebar, history and sending, and
//! its websocket for everything live. One adapter is one workspace, with a socket of its own.
//!
//! Slack documents its public API; several calls here are the web client's own and are marked
//! "unverified" where no primary source confirmed their shape. The integrator should check those
//! first against a real workspace.
//!
//! What stays in memory is small and bounded: the people recently seen, the muted channels, who
//! the user has direct conversations with, and frames waiting for the socket. The sidebar, people
//! and messages go to the store as they are parsed.

mod emoji;
mod info;
mod message;
mod people;
mod send;
mod text;
mod tree;

use super::net::{address, browser, redact, Socket};
use super::{keep_connected, serve_commands, About, Command, Context, Download, Failure, Refusal, Signed};
use people::{Roster, User};
use protocol::{Link, Message, Presence, Reaction, ReplyTo, ThreadSummary, THREAD_FACES};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tree::Counted;
use ureq::tls::{TlsConfig, TlsProvider};

const HISTORY: &str = "50";
/// Messages asked for on each side of one the user jumped to.
const AROUND: &str = "25";
/// Replies fetched per call, and how many calls one opening of a long thread may make.
const REPLIES: &str = "100";
const REPLY_PAGES: usize = 5;
/// The pause after a history fetch. The real limit is kept by [`Slack::pace`], per method.
const PACE: Duration = Duration::from_millis(300);
/// Calls of any kind are kept this far apart, so nothing this adapter does is a burst.
const GAP: Duration = Duration::from_millis(200);
/// The longest `Retry-After` that is waited out before the one retry. A longer one is told to
/// the user at once and nothing is retried: the wait is never cut short.
const PATIENCE: u64 = 60;
/// A history this adapter fetched itself this recently is not fetched again when opened.
const FRESH: Duration = Duration::from_secs(60);
/// The wait before each conversation warmed after connecting: more than twice the 1.2 s that
/// tier 3 (50 a minute) allows, plus up to half as much again so the calls are not in step.
const WARM_GAP: Duration = Duration::from_millis(3000);
/// What marks a `Command::Warm` this adapter queued for itself, so it is told from an opening.
const WARM: char = '~';
/// Greater than any timestamp: the end of a stretch of history the socket is keeping current.
const LIVE: &str = "~";
/// Stretches of history remembered as whole, per conversation, and for how many conversations.
const STRETCHES: usize = 4;
const COVERED: usize = 32;
/// Channels and people Slack would not describe, remembered for the session.
const UNKNOWN: usize = 1_000;
/// People searches whose results are already in the store, remembered for the session.
const SEARCHED: usize = 256;
/// The least time between two people searches. The server debounces by the same.
const SEARCH_EVERY: Duration = Duration::from_millis(400);
/// Unverified: the web client's own ping interval was not confirmed; the socket stays up with
/// this one, and Slack closes a socket that is silent for much longer.
const PING: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(75);
/// How long the socket waits for a frame before looking at what there is to send.
const TICK: Duration = Duration::from_secs(2);
/// Others are told the user is typing at most this often per conversation (account safety,
/// rule 8). Slack's own client signals more often; less is never an anomaly.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// How long "is typing" stays without another signal.
const TYPING_SHOWN: i64 = 6;
/// A sidebar the shell opens again within this time is not fetched again; the socket keeps it
/// current in between.
const TREE_FRESH: Duration = Duration::from_secs(600);
/// Frames kept for a socket that is down.
const OUTBOX: usize = 64;
/// Errors that mean this workspace's session is over. The other workspaces are untouched.
const ENDED: &[&str] = &["invalid_auth", "not_authed", "account_inactive", "token_revoked", "token_expired"];
/// Errors that mean Slack wants something done in its own app before it answers again, and the
/// words for it. Unverified: these are the documented names; what a real workspace sends when
/// it demands a second factor or a fresh organization sign-in was not seen.
const DEMANDS: &[(&str, &str)] = &[
    ("two_factor_setup_required", "Slack wants two-factor authentication set up. Finish that in the Slack app."),
    ("org_login_required", "Your organization wants you to sign in again. Do that in the Slack app."),
    ("team_access_not_granted", "This workspace has not let you in yet. Finish joining in the Slack app."),
    ("password_change_required", "Slack wants your password changed. Do that in the Slack app."),
];
/// Addresses Slack itself serves. Nothing else in a message is ever registered for fetching.
const HOSTS: &[&str] = &["slack.com", "slack-edge.com", "slack-imgs.com", "slack-files.com"];

/// One workspace's part of the sign-in: what `accounts` hands to `start`.
#[derive(Deserialize)]
struct Session {
    token: String,
    /// The `d` cookie, as the browser stored it. One for every workspace of the sign-in.
    cookie: String,
    /// The workspace's own address, `https://example.slack.com/`.
    url: String,
    #[serde(default)]
    team_id: String,
    /// The User-Agent of the window the user signed in with. Slack reports a session whose
    /// agent changes as an anomaly, so every request and the socket carry this one.
    #[serde(default)]
    user_agent: String,
}

/// Why a call to Slack failed. Deliberately not `Display`: it becomes a [`Failure`] or words
/// for the user, never a raw string.
enum Fault {
    /// The session is over.
    Auth(String),
    /// Slack answered `ok: false` with this error.
    Slack(String),
    /// Rate limited, for this many seconds.
    Limited(u64),
    Net(String),
}

impl From<Fault> for Failure {
    fn from(fault: Fault) -> Failure {
        match fault {
            Fault::Auth(why) => Failure::SignedOut(why),
            Fault::Slack(error) => Failure::Dropped(error),
            Fault::Limited(seconds) => Failure::Dropped(format!("rate limited for {seconds} s")),
            Fault::Net(why) => Failure::Dropped(why),
        }
    }
}

/// What reactions a Slack conversation takes. Slack takes a reaction by name, so the standard
/// emoji are the ones chatd can name (`emoji::nameable`); a reaction already on a message is
/// named there and can always be added to. The workspace's custom emoji are all usable. Slack's
/// help centre puts the limits at 23 reactions a person and 50 different emoji a message
/// (`too_many_reactions`, `too_many_emoji`); unverified against a real workspace. An archived
/// channel, which takes none, is never listed (`exclude_archived`).
pub(crate) fn reaction_rules() -> protocol::ReactionRules {
    protocol::ReactionRules {
        set: protocol::ReactionSet::Only,
        only: emoji::nameable(),
        // Slack names a tone (`+1::skin-tone-4`).
        tones: true,
        custom: protocol::EmojiScope::All,
        mine: 23,
        distinct: 50,
        ..protocol::ReactionRules::default()
    }
}

/// A fault in words the user can read.
fn reason(fault: Fault) -> String {
    let words = match fault {
        Fault::Limited(seconds) => return format!("Slack is rate limiting you. Try again in {seconds} s."),
        Fault::Auth(_) => "Slack signed you out of this workspace. Sign in again.",
        Fault::Net(_) => "Slack could not be reached.",
        Fault::Slack(error) => match error.as_str() {
            "cant_update_message" | "edit_window_closed" => "This message can no longer be edited.",
            "cant_delete_message" => "Slack does not let you delete this message.",
            "message_not_found" => "That message is gone.",
            "too_many_reactions" => "You have added as many reactions to this message as Slack allows.",
            "too_many_emoji" => "This message has as many different reactions as Slack allows.",
            "invalid_name" => "Slack does not know that emoji.",
            "no_emoji_name" => "Slack has no name for that emoji, so it cannot be a reaction here.",
            "no_dnd_end" => "Slack's Do Not Disturb needs an end time. Set it in the Slack app.",
            "profile_set_failed" | "not_allowed" => "This workspace does not let you change that.",
            demand if DEMANDS.iter().any(|(name, _)| *name == demand) => {
                return DEMANDS.iter().find(|(name, _)| *name == demand).map_or_else(String::new, |(_, words)| (*words).to_owned());
            }
            "not_in_channel" => "You are not in this channel.",
            "msg_too_long" => "The message is too long for Slack.",
            "is_archived" => "This channel is archived.",
            "channel_not_found" => "Slack cannot find this conversation.",
            "no_text" => "Slack does not take an empty message.",
            "thread_not_found" | "cannot_reply_to_message" => "That message can no longer be replied to.",
            "user_not_found" | "user_disabled" | "cannot_dm_bot" => "Slack cannot open a conversation with them.",
            error if error.starts_with("restricted_action") => "This workspace does not let you post here.",
            error => return format!("Slack refused ({error})."),
        },
    };
    words.to_owned()
}

#[derive(Default)]
struct State {
    /// Frames for the socket thread to send: typing, presence subscriptions, pings.
    outbox: Vec<String>,
    /// The id Slack's socket wants on each frame we send.
    sent: u64,
    users: HashMap<String, User>,
    muted: HashSet<String>,
    mutes_read: bool,
    /// Who the user has a direct conversation with, and which conversation it is.
    partners: HashMap<String, String>,
    /// Our own handle, to leave out of a group DM's title.
    handle: String,
    /// Set by any call that finds the session over, so the socket thread ends it.
    expired: Option<String>,
    /// Slack demanded something only its app can do (the error's name). Nothing is asked of
    /// Slack while this is set, but the sign-in check at the next connection.
    halted: Option<String>,
    /// When each conversation was last told the user is typing.
    typed: HashMap<String, Instant>,
    /// Whose presence the socket is subscribed to.
    watched: Vec<String>,
    /// When the sidebar was last loaded.
    tree: Option<Instant>,
    /// The socket has just connected and the unread state may have moved.
    resync: bool,
    /// Conversations were warmed once this run. Never again, whatever reconnects.
    warmed: bool,
    /// The unread state of conversations waiting to be warmed that had no row yet.
    warm: HashMap<String, Counted>,
    /// When this adapter last fetched a conversation's newest page on its own.
    fetched: HashMap<String, Instant>,
    /// Stretches of each channel's history the cache holds whole, as `(oldest, newest)`.
    covered: HashMap<String, Vec<(String, String)>>,
    /// Channels and people Slack would not describe. Not asked about again this session.
    unknown: HashSet<String>,
    /// The member list being read, with where Slack's paging stands.
    roster: Option<Roster>,
    /// Searches already made this session, in lower case, and when the last one went out.
    searched: HashSet<String>,
    search_at: Option<Instant>,
    groups_listed: bool,
    partners_read: bool,
    /// When the followed threads were last asked for.
    threads_at: Option<Instant>,
    /// Rows on screen waiting for their newest message, whether a thread is fetching them, and
    /// when the last one was fetched.
    previews: VecDeque<String>,
    previewing: bool,
    previewed: Option<Instant>,
    /// Sends with files waiting for the socket to show the message that shares them, by the
    /// first file's id.
    shares: HashMap<String, Sender<Message>>,
    /// When `users.info` was last asked about each person: once an hour each ([`INFO_EVERY`]).
    info_asked: HashMap<String, Instant>,
    /// The card last read for each person, answered again inside that hour.
    abouts: HashMap<String, About>,
}

/// `users.info` is asked about one person at most this often.
const INFO_EVERY: Duration = Duration::from_secs(3600);

/// When Slack was last asked anything, and what each method has left of its allowance.
#[derive(Default)]
struct Pacer {
    last: Option<Instant>,
    /// Calls a method may still make now, and when that was reckoned.
    left: HashMap<String, (f64, Instant)>,
}

/// Calls a minute Slack documents for a method (its tiers 2, 3 and 4). The web client's own
/// calls have no documented tier and are held to tier 3. Unverified for this kind of session:
/// Slack does not say whether the web client's token is counted by the same tiers.
fn per_minute(method: &str) -> f64 {
    match method {
        "users.setPresence" | "reactions.remove" | "emoji.list" | "usergroups.list" | "dnd.setSnooze" => 20.,
        "conversations.members" | "users.info" | "users.profile.get" => 100.,
        // One message a second per channel, and the sign-in check is in a tier of its own.
        "chat.postMessage" | "auth.test" => 60.,
        _ => 50.,
    }
}

/// Adds a stretch of history the cache now holds whole, joining it to any it touches.
fn cover(stretches: &mut Vec<(String, String)>, oldest: &str, newest: &str) {
    stretches.push((oldest.to_owned(), newest.to_owned()));
    stretches.sort();
    let mut joined: Vec<(String, String)> = Vec::with_capacity(stretches.len());
    for (oldest, newest) in stretches.drain(..) {
        match joined.last_mut() {
            Some(last) if last.1 >= oldest => last.1 = std::mem::take(&mut last.1).max(newest),
            _ => joined.push((oldest, newest)),
        }
    }
    // The newest stretches are the ones being read.
    let extra = joined.len().saturating_sub(STRETCHES);
    joined.drain(..extra);
    *stretches = joined;
}

/// The stretch that holds a message, if the cache is known to be whole around it.
fn covering<'a>(stretches: &'a [(String, String)], ts: &str) -> Option<&'a (String, String)> {
    stretches.iter().find(|(oldest, newest)| oldest.as_str() <= ts && ts <= newest.as_str())
}

/// Which page of a channel is asked for.
#[derive(Clone, Copy)]
enum Bounds<'a> {
    Newest,
    /// Older than this message.
    Before(&'a str),
    /// Newer than this message, oldest first.
    After(&'a str),
    /// This message and the ones before it.
    Through(&'a str),
}

/// Slack's three escapes for text a user typed. A mention the composer wrote in Slack's own
/// form (`<@U…>`, `<#C…>`, `<!here>`, `<!subteam^S…>`) is left whole, or it would arrive as
/// those characters and notify nobody.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['&', '<', '>']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(len) = mention_len(tail) {
            out.push_str(&tail[..len]);
            rest = &tail[len..];
            continue;
        }
        out.push_str(match tail.as_bytes()[0] {
            b'&' => "&amp;",
            b'<' => "&lt;",
            _ => "&gt;",
        });
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// The length of the Slack mention `text` starts with, if it starts with one.
fn mention_len(text: &str) -> Option<usize> {
    let end = text.find('>')?;
    let inner = text.strip_prefix('<')?.get(..end - 1)?;
    let id = |rest: &str| !rest.is_empty() && rest.len() <= 24 && rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let ok = match inner.as_bytes().first()? {
        b'@' => id(&inner[1..]),
        b'#' => id(&inner[1..]),
        b'!' => matches!(&inner[1..], "here" | "channel" | "everyone") || inner[1..].strip_prefix("subteam^").is_some_and(id),
        _ => false,
    };
    ok.then_some(end + 1)
}

/// A part of a second that differs from call to call, for gaps that must not be in step.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.subsec_nanos());
    f64::from(nanos % 1_000_000) / 1_000_000.
}

impl State {
    fn queue(&mut self, mut frame: Value) {
        self.sent += 1;
        frame["id"] = self.sent.into();
        if self.outbox.len() >= OUTBOX {
            self.outbox.remove(0);
        }
        self.outbox.push(frame.to_string());
    }
}

struct Slack {
    ctx: Context,
    agent: ureq::Agent,
    session: Session,
    /// `d=…`, sent with every request and the socket. Replaced when Slack reissues it.
    cookie: Mutex<String>,
    /// Where the API is: the workspace's own address unless a test moved it.
    api: String,
    /// Where the web client's cache service is.
    edge: String,
    /// A test moved the API, and its files with it.
    moved: bool,
    me: OnceLock<String>,
    state: Mutex<State>,
    pacer: Mutex<Pacer>,
}

/// The workspace's host, for a session too old to say which team it is.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

fn text_of<'a>(value: &'a Value, names: &[&str]) -> &'a str {
    names.iter().filter_map(|name| value[*name].as_str()).find(|text| !text.is_empty()).unwrap_or("")
}

/// The workspaces of one sign-in: `teams` in what the helper prints now, or the single team of a
/// session stored before it printed them all.
fn teams(session: &Value) -> Vec<&Value> {
    match session["teams"].as_array() {
        Some(teams) => teams.iter().collect(),
        None => vec![session],
    }
}

fn team_id(team: &Value) -> &str {
    match text_of(team, &["id", "team_id"]) {
        "" => host(text_of(team, &["url"])),
        id => id,
    }
}

/// One account per workspace. They share the sign-in's cookie and User-Agent and nothing else.
///
/// Enterprise Grid is not treated specially: each workspace of an org is its own account, so
/// org-wide channels and direct messages appear under each of them. Making an org one account
/// needs the org-level token, which this has not been run against.
pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let parsed: Value = serde_json::from_str(session).map_err(|err| err.to_string())?;
    let cookie = parsed["cookie"].as_str().ok_or("the Slack session has no cookie")?;
    let mut found: Vec<Signed> = Vec::new();
    for team in teams(&parsed) {
        let (token, url, id) = (text_of(team, &["token"]), text_of(team, &["url"]), team_id(team));
        if token.is_empty() || url.is_empty() || found.iter().any(|signed| signed.account == id) {
            continue;
        }
        let name = match text_of(team, &["name", "team"]) {
            "" => host(url).split('.').next().unwrap_or(id),
            name => name,
        };
        let session = json!({
            "token": token, "cookie": cookie, "url": url, "team_id": id,
            "user_agent": text_of(&parsed, &["user_agent"]),
        });
        found.push(Signed {
            account: id.to_owned(),
            name: name.to_owned(),
            session: session.to_string(),
        });
    }
    if found.is_empty() {
        return Err("the Slack session has no workspace".to_owned());
    }
    Ok(found)
}

/// The session without one workspace, or `None` when it was the last.
pub fn without(session: &str, account: &str) -> Option<String> {
    let mut parsed: Value = serde_json::from_str(session).ok()?;
    let teams = parsed["teams"].as_array_mut()?;
    teams.retain(|team| team_id(team) != account);
    if teams.is_empty() {
        return None;
    }
    Some(parsed.to_string())
}

/// The stored session with its `d` cookie replaced, when it still holds the one this adapter
/// was started with. Another workspace of the same sign-in may have saved a newer one already;
/// then it is left alone. Nothing else in the session is touched.
fn recookied(stored: &str, old: &str, fresh: &str) -> Option<String> {
    let mut parsed: Value = serde_json::from_str(stored).ok()?;
    if parsed["cookie"].as_str() != Some(old) {
        return None;
    }
    parsed["cookie"] = fresh.into();
    Some(parsed.to_string())
}

/// Parses the session and returns at once: everything that could be slow (finding out which
/// Safari is installed for a session that did not record its browser, building the HTTP agent,
/// every call to Slack) happens on the adapter's own threads.
pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let mut session: Session = serde_json::from_str(session).map_err(|err| err.to_string())?;
    if !session.url.ends_with('/') {
        session.url.push('/');
    }
    if session.team_id.is_empty() {
        session.team_id = ctx.account.account.clone();
    }
    let mut api = address("TINYCLIENT_SLACK_API", &session.url);
    if !api.ends_with('/') {
        api.push('/');
    }
    let spawned = std::thread::Builder::new().name("slack".into()).spawn(move || {
        if session.user_agent.is_empty() {
            // A session stored before the helper recorded its browser: the same Safari.
            session.user_agent = browser().to_owned();
        }
        let moved = api != session.url;
        let agent = ureq::Agent::config_builder()
            .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent(&session.user_agent)
            .build()
            .into();
        let slack = Arc::new(Slack {
            ctx,
            agent,
            cookie: Mutex::new(format!("d={}", session.cookie)),
            edge: if moved { format!("{api}cache/") } else { "https://edgeapi.slack.com/cache/".to_owned() },
            api,
            moved,
            session,
            me: OnceLock::new(),
            state: Mutex::new(State::default()),
            pacer: Mutex::new(Pacer::default()),
        });
        let socket = slack.clone();
        std::thread::spawn(move || keep_connected(&socket.ctx, || socket.connect()));
        // A send with files uploads on a thread of its own, and previews are fetched on one,
        // paced: the commands behind either (a cancel among them) are not held up.
        let other = |command| match command {
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                let sending = slack.clone();
                let (conversation, client) = (conversation_id.clone(), client_id.clone());
                crate::blobs::upload::spawn(&slack.ctx, &conversation_id, &client_id, &with.files.clone(), move |up| {
                    sending.send_with(&conversation, &client, &text, reply_to.as_deref(), &with, up)
                });
            }
            Command::Previews(ids) => slack.previews(ids),
            command => slack.serve(command),
        };
        serve_commands(&slack.ctx, inbox, PACE, |id| slack.report(slack.fetch(id)), other)
    });
    spawned.map(drop).map_err(|err| err.to_string())
}

/// Seconds from a Slack timestamp such as `1700000000.000200`.
fn seconds(ts: &str) -> i64 {
    ts.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// A thread is a conversation of its own: its channel and the timestamp of its first message.
fn thread_id(channel: &str, root: &str) -> String {
    format!("{channel}:{root}")
}

fn thread_of(conversation: &str) -> Option<(&str, &str)> {
    conversation.split_once(':')
}

/// A stable 64-bit name for an address, for media ids. FNV-1a, so it never changes between
/// builds the way the standard hasher may.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The shell's `client_id` (digits) as the `client_msg_id` Slack's own client would send: a
/// version 4 UUID in form, with the digits in its low end so the echo can be matched back.
/// Unverified: that Slack echoes a `client_msg_id` given to `chat.postMessage` on the socket.
/// When it does not, the pending row is still replaced by the call's own answer.
fn client_msg_id(client_id: &str) -> String {
    let digits: String = client_id.chars().filter(char::is_ascii_digit).take(30).collect();
    let hex = format!("{digits:f>30}");
    format!("{}-{}-4{}-8{}-{}", &hex[..8], &hex[8..12], &hex[12..15], &hex[15..18], &hex[18..])
}

/// The `client_id` inside a `client_msg_id` of ours. `None` for any other client's.
fn client_id(client_msg_id: &str) -> Option<String> {
    let parts: Vec<&str> = client_msg_id.split('-').collect();
    let [a, b, c, d, e] = parts[..] else {
        return None;
    };
    let shaped = [a.len(), b.len(), c.len(), d.len(), e.len()] == [8, 4, 4, 4, 12];
    if !shaped || !c.starts_with('4') || !d.starts_with('8') {
        return None;
    }
    let hex = format!("{a}{b}{}{}{e}", &c[1..], &d[1..]);
    let digits = hex.trim_start_matches('f');
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.to_owned())
}

/// The host of an `https` address, or `None` for anything that is not plainly one.
fn https_host(url: &str) -> Option<&str> {
    let authority = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    authority.split(':').next()
}

fn within(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.'))
}

impl Slack {
    fn state(&self) -> MutexGuard<'_, State> {
        crate::hub::lock(&self.state)
    }

    /// Whether `users.info` may be asked about `person` now, and if so, notes that it was. Once
    /// per person per [`INFO_EVERY`], however often a card is opened or a name is unknown.
    fn info_due(&self, person: &str) -> bool {
        let mut state = self.state();
        if state.info_asked.get(person).is_some_and(|at| at.elapsed() < INFO_EVERY) {
            return false;
        }
        if state.info_asked.len() >= 1024 {
            state.info_asked.retain(|_, at| at.elapsed() < INFO_EVERY);
            let State { info_asked, abouts, .. } = &mut *state;
            abouts.retain(|person, _| info_asked.contains_key(person));
        }
        state.info_asked.insert(person.to_owned(), Instant::now());
        true
    }

    fn report(&self, result: Result<(), Failure>) {
        if let Err(Failure::SignedOut(why) | Failure::Dropped(why)) = result {
            tracing::warn!("chatd: Slack: {}", redact(&why));
        }
    }

    fn cookie(&self) -> String {
        crate::hub::lock(&self.cookie).clone()
    }

    /// True for an address Slack serves: its files, avatars, emoji and image proxy.
    fn hosted(&self, url: &str) -> bool {
        (self.moved && url.starts_with(&self.api))
            || https_host(url).is_some_and(|host| HOSTS.iter().any(|domain| within(host, domain)))
    }

    /// True for an address that answers only to the session: uploads, on `files.slack.com` or
    /// the workspace's own host. Avatars, emoji and proxied images are public.
    fn private(&self, url: &str) -> bool {
        (self.moved && url.starts_with(&self.api)) || https_host(url).is_some_and(|host| within(host, "slack.com"))
    }

    /// How the media cache fetches a registered file: with the session where Slack asks for it,
    /// and always as the same browser.
    ///
    /// CONTRACT: the cache applies these headers only on a host it trusts with the session
    /// (`files.slack.com`). A public file (an avatar, an emoji) is fetched with the cache's own
    /// agent, `net::browser()`, which is this session's browser only while the Safari installed
    /// is the one the user signed in with.
    fn download(&self, url: String) -> Download {
        let headers = download_headers(&self.session.user_agent, &self.session.token, &self.cookie(), self.private(&url));
        Download { url, headers }
    }

    /// Waits for the method's turn: [`GAP`] after the last call of any kind, and never more
    /// often than half what Slack documents for the method, a few calls in a row allowed (a
    /// tenth of a minute's worth) so that what the user clicks is not held back. True if the
    /// adapter was stopped while waiting.
    fn pace(&self, method: &str) -> bool {
        loop {
            let wait = {
                let mut pacer = crate::hub::lock(&self.pacer);
                let now = Instant::now();
                let gap = pacer.last.map_or(Duration::ZERO, |last| GAP.saturating_sub(now.duration_since(last)));
                let (a_second, most) = (per_minute(method) / 2. / 60., (per_minute(method) / 10.).max(2.));
                let (left, at) = pacer.left.entry(method.to_owned()).or_insert((most, now));
                *left = (*left + now.duration_since(*at).as_secs_f64() * a_second).min(most);
                *at = now;
                if *left >= 1. && gap.is_zero() {
                    *left -= 1.;
                    pacer.last = Some(now);
                    return false;
                }
                let refill = if *left < 1. { Duration::from_secs_f64((1. - *left) / a_second) } else { Duration::ZERO };
                refill.max(gap)
            };
            if self.ctx.pause(wait) {
                return true;
            }
        }
    }

    /// Reads Slack's `ok` envelope. A session Slack has ended is remembered, so the socket
    /// thread reports this workspace expired whichever thread found out. A demand only Slack's
    /// own app can meet stops every further request and is shown on the account.
    fn checked(&self, method: &str, body: Value) -> Result<Value, Fault> {
        if body["ok"] == true {
            return Ok(body);
        }
        let error = body["error"].as_str().unwrap_or("unknown error");
        if ENDED.contains(&error) {
            let why = format!("{method}: {error}");
            self.state().expired = Some(why.clone());
            return Err(Fault::Auth(why));
        }
        if let Some((name, words)) = DEMANDS.iter().find(|(name, _)| *name == error) {
            self.state().halted = Some((*name).to_owned());
            self.ctx.attention(words);
        }
        Err(Fault::Slack(error.to_owned()))
    }

    /// Slack reissues the `d` cookie now and then (`Set-Cookie` on any answer). The new one is
    /// used from the next request on and saved, so the stored session never goes stale.
    fn reissued(&self, headers: &ureq::http::HeaderMap) {
        for set in headers.get_all("set-cookie") {
            let Some(fresh) = set.to_str().ok().and_then(|set| set.strip_prefix("d=")) else {
                continue;
            };
            let fresh = fresh.split(';').next().unwrap_or("").trim();
            // An emptied cookie is Slack signing the browser out; the next answer says so.
            if fresh.is_empty() || fresh == "deleted" {
                continue;
            }
            let old = std::mem::replace(&mut *crate::hub::lock(&self.cookie), format!("d={fresh}"));
            let old = old.trim_start_matches("d=").to_owned();
            if old != fresh {
                self.ctx.save_session(|stored| recookied(stored, &old, fresh));
            }
        }
    }

    /// Refuses a call while Slack is waiting for the user to do something in its own app.
    fn halted(&self, method: &str) -> Result<(), Fault> {
        match self.state().halted.clone() {
            Some(demand) if method != "auth.test" => Err(Fault::Slack(demand)),
            _ => Ok(()),
        }
    }

    /// One call to Slack's web API. The token goes in the form, the cookie beside it.
    ///
    /// A 429 gets two attempts in all: the full `Retry-After` is waited out, the call is made
    /// once more, and if that is limited too the caller is told (a command's answer, a send's
    /// failure) and nothing more is tried.
    // CONTRACT: a fetch nobody is waiting on an answer for (a history page, the sidebar) has
    // nowhere to say it was rate limited; `Context::attention` is for what only Slack's app can
    // do. It is logged, and the user sees the page not arriving.
    fn api(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, Fault> {
        self.halted(method)?;
        let url = format!("{}api/{method}", self.api);
        let mut form = vec![("token", self.session.token.as_str())];
        form.extend(params.iter().filter(|(_, value)| !value.is_empty()));
        let net = |err: ureq::Error| Fault::Net(format!("{method}: {err}"));
        let stopped = || Fault::Net(format!("{method}: stopped"));
        let mut wait = 0;
        for attempt in 0..2 {
            if self.pace(method) {
                return Err(stopped());
            }
            let sent = self.agent.post(&url).header("Cookie", self.cookie()).send_form(form.iter().copied());
            let mut response = sent.map_err(net)?;
            self.reissued(response.headers());
            let retry = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
            let limited = response.status().as_u16() == 429;
            let body: Value = if limited { Value::Null } else { response.body_mut().read_json().map_err(net)? };
            if !limited && body["error"] != "ratelimited" {
                return self.checked(method, body);
            }
            wait = retry.unwrap_or(5);
            if attempt > 0 || wait > PATIENCE {
                break;
            }
            if self.ctx.pause(Duration::from_secs(wait)) {
                return Err(stopped());
            }
        }
        Err(Fault::Limited(wait))
    }

    // The socket.

    /// Where the websocket is. A session of this kind does not call `rtm.connect` (that is for
    /// bots and apps): the web client derives the address from the token and the workspace,
    /// every time it connects. Unverified against a real workspace; the form is the one
    /// wee-slack uses for the same kind of token, `gateway_server=<team>-1` included. On
    /// Enterprise Grid the web client is believed to name the org there; that was not confirmed
    /// either, so the workspace's own id is kept.
    fn socket_url(&self) -> String {
        let base = address("TINYCLIENT_SLACK_SOCKET", "wss://wss-primary.slack.com/");
        format!(
            "{base}?token={}&gateway_server={}-1&slack_client=desktop&batch_presence_aware=1",
            self.session.token, self.session.team_id
        )
    }

    /// One connection, from sign-in check to the socket dropping.
    fn connect(&self) -> Failure {
        match self.run() {
            Ok(()) => Failure::Dropped("stopped".to_owned()),
            Err(failure) => failure,
        }
    }

    fn run(&self) -> Result<(), Failure> {
        let auth = match self.api("auth.test", &[]) {
            Ok(auth) => auth,
            Err(fault) => {
                // Slack wants something done in its own app. Asking again soon is how a flag
                // becomes a ban, so this workspace is left alone for an hour.
                if self.state().halted.is_some() {
                    self.ctx.pause(Duration::from_secs(3600));
                }
                return Err(fault.into());
            }
        };
        if self.state().halted.take().is_some() {
            self.ctx.attention("");
        }
        let me = auth["user_id"].as_str().unwrap_or_default();
        let _ = self.me.set(me.to_owned());
        let name = auth["team"].as_str().unwrap_or_default();
        self.ctx.describe(name, me)?;
        self.ctx.capabilities(capabilities());
        self.space(name)?;
        let cookie = self.cookie();
        let headers = [
            ("Cookie", cookie.as_str()),
            ("Origin", "https://app.slack.com"),
            ("User-Agent", self.session.user_agent.as_str()),
        ];
        let mut socket = match Socket::connect(&self.socket_url(), &headers, TICK) {
            Ok(socket) => socket,
            // The address above is derived, not confirmed. The one Slack hands out through
            // `rtm.connect` is known to work for this kind of session on a real workspace.
            Err(first) => match self.api("rtm.connect", &[]).ok().and_then(|answer| answer["url"].as_str().map(str::to_owned)) {
                Some(url) => Socket::connect(&url, &headers, TICK)?,
                None => return Err(first.into()),
            },
        };
        self.ctx.link(Link::Connected);
        {
            let mut state = self.state();
            state.handle = auth["user"].as_str().unwrap_or_default().to_owned();
            state.outbox.clear();
            state.watched.clear();
            state.expired = None;
            state.resync = true;
            // What was whole may have a hole now: the socket was down.
            state.covered.clear();
        }
        // The unread state is the command thread's to fetch, paced, so the socket is never kept
        // waiting on it.
        let _ = self.ctx.commands.send(Command::OpenSpace(self.ctx.account.account.clone()));
        self.watch();

        let (mut heard, mut pinged) = (Instant::now(), Instant::now());
        while !self.ctx.stopped() {
            let (expired, outbox) = {
                let mut state = self.state();
                (state.expired.clone(), std::mem::take(&mut state.outbox))
            };
            if let Some(why) = expired {
                return Err(Failure::SignedOut(why));
            }
            for frame in outbox {
                socket.send(frame)?;
            }
            if let Some(frame) = socket.read()? {
                heard = Instant::now();
                if let Ok(event) = serde_json::from_str::<Value>(&frame) {
                    self.event(&event)?;
                }
            }
            if heard.elapsed() > SILENCE {
                return Err(Failure::Dropped("socket went quiet".to_owned()));
            }
            if pinged.elapsed() > PING {
                self.state().queue(json!({"type": "ping"}));
                pinged = Instant::now();
                // The shell's watch lapses without telling anyone.
                self.watch();
            }
        }
        Ok(())
    }

    fn event(&self, event: &Value) -> Result<(), Failure> {
        match event["type"].as_str().unwrap_or("") {
            "message" => self.heard(event),
            "reaction_added" | "reaction_removed" => self.reacted(event),
            "channel_marked" | "im_marked" | "group_marked" | "mpim_marked" => self.marked(event),
            "thread_marked" => self.thread_marked(event),
            "user_typing" => {
                let (Some(channel), Some(user)) = (event["channel"].as_str(), event["user"].as_str()) else {
                    return Ok(());
                };
                if self.me.get().is_some_and(|me| me == user) {
                    return Ok(());
                }
                let place = match event["thread_ts"].as_str() {
                    Some(root) => thread_id(channel, root),
                    None => channel.to_owned(),
                };
                self.ctx.typing(&place, user, &self.user(user).name, TYPING_SHOWN);
                Ok(())
            }
            "presence_change" => {
                self.presence_changed(event);
                Ok(())
            }
            // A channel someone else created is not in the sidebar until joined, which is its
            // own event.
            "channel_created" => Ok(()),
            "channel_joined" | "group_joined" | "im_open" | "mpim_open" | "group_open" | "channel_unarchive" => {
                self.joined(event)
            }
            "channel_left" | "group_left" | "channel_deleted" | "group_deleted" | "channel_archive"
            | "group_archive" | "im_close" | "mpim_close" | "group_close" => self.left(event),
            "channel_rename" | "group_rename" => self.renamed(event),
            // A huddle joined or left, a status set: the user object comes whole with each.
            // Unverified: `user_huddle_changed` and `user_status_changed` carry it as
            // `user_change` does (the community's reading).
            "user_change" | "user_huddle_changed" | "user_status_changed" => self.user_changed(&event["user"]),
            "member_joined_channel" | "member_left_channel" => {
                self.member_moved(event);
                Ok(())
            }
            "manual_presence_change" => {
                self.own_presence(event["presence"].as_str().unwrap_or(""));
                Ok(())
            }
            "pref_change" => self.muted(event["name"].as_str().unwrap_or(""), &event["value"]),
            "emoji_changed" => {
                self.emoji_changed(event);
                Ok(())
            }
            "goodbye" => Err(Failure::Dropped("Slack asked for a new connection".to_owned())),
            "error" => {
                let error = &event["error"];
                let why = error["msg"].as_str().unwrap_or("socket error").to_owned();
                if ENDED.contains(&why.as_str()) {
                    return Err(Failure::SignedOut(why));
                }
                Err(Failure::Dropped(why))
            }
            _ => Ok(()),
        }
    }

    // Messages.

    /// A `message` event: a new message, or news about an old one.
    fn heard(&self, event: &Value) -> Result<(), Failure> {
        let Some(channel) = event["channel"].as_str() else {
            return Ok(());
        };
        match event["subtype"].as_str() {
            Some("message_changed") => self.changed(channel, &event["message"]),
            Some("message_deleted") => {
                let Some(ts) = event["deleted_ts"].as_str() else {
                    return Ok(());
                };
                self.ctx.deleted(channel, ts)?;
                let root = event["previous_message"]["thread_ts"].as_str().unwrap_or(ts);
                self.ctx.deleted(&thread_id(channel, root), ts)?;
                Ok(())
            }
            Some("message_replied") => self.replied(channel, &event["message"]),
            Some("channel_topic" | "group_topic") => {
                self.topic_changed(channel, event["topic"].as_str().unwrap_or(""))?;
                self.arrived(channel, event)
            }
            _ => self.arrived(channel, event),
        }
    }

    /// The first message of a thread as the one line a reply shown in the channel points at.
    fn quote(&self, channel: &str, root: &str) -> Option<ReplyTo> {
        let stored = self.ctx.stored_message(channel, root).ok().flatten();
        Some(match stored {
            Some(stored) => ReplyTo {
                deleted: stored.deleted,
                person: Some(&stored.sender.id).filter(|id| !id.is_empty()).map(|id| self.ctx.key(id)),
                id: stored.id,
                sender: stored.sender.name,
                snippet: if stored.deleted { String::new() } else { stored.text.chars().take(120).collect() },
            },
            None => ReplyTo {
                deleted: false,
                person: None,
                id: root.to_owned(),
                sender: String::new(),
                snippet: String::new(),
            },
        })
    }

    fn arrived(&self, channel: &str, event: &Value) -> Result<(), Failure> {
        // A conversation Slack will not describe to this workspace is not this workspace's.
        if !self.known(channel)? {
            return Ok(());
        }
        let Some(mut message) = self.message(channel, event) else {
            return Ok(());
        };
        let root = event["thread_ts"].as_str().filter(|root| *root != message.id);
        // The files of a send of ours, shared: that send records it as its own.
        if event["files"].is_array() && self.shared_here(event, &message) {
            return Ok(());
        }
        let Some(root) = root else {
            if message.thread.is_some() {
                self.thread_known(channel, &message.id)?;
            }
            self.ctx.message(channel, &message)?;
            return Ok(());
        };
        // A reply lives in its thread, not in the channel's own pages.
        let thread = thread_id(channel, root);
        self.thread_known(channel, root)?;
        if message.mentions_me {
            // A mention is the one thing in a thread that needs me whether or not I follow it.
            self.ctx.message(&thread, &message)?;
        } else {
            // Slack counts a thread's replies as unread only for its followers, and does not
            // say on the socket who they are. Opening the thread reads its real state.
            self.ctx.history(&thread, std::slice::from_ref(&message))?;
            self.ctx.typing_stopped(&thread, &message.sender.id);
        }
        self.bump(channel, root, &message)?;
        if event["subtype"] == "thread_broadcast" || event["reply_broadcast"] == true {
            message.reply_to = self.quote(channel, root);
            self.ctx.message(channel, &message)?;
        }
        Ok(())
    }

    /// A reply arrived: the thread's first message, where the channel shows it, counts one more.
    /// `message_replied` carries Slack's own count when it comes; a reply no newer than the
    /// count already shown is taken to be included in it.
    fn bump(&self, channel: &str, root: &str, reply: &Message) -> Result<(), Failure> {
        let Some(mut first) = self.ctx.stored_message(channel, root)? else {
            return Ok(());
        };
        let summary = first.thread.get_or_insert(ThreadSummary {
            participants: Vec::new(),
            unread: 0,
            id: thread_id(channel, root),
            replies: 0,
            last: 0,
        });
        if reply.sent > summary.last {
            summary.replies += 1;
            summary.last = reply.sent;
            if !reply.sender.id.is_empty() {
                let replier = self.ctx.key(&reply.sender.id);
                summary.participants.retain(|person| *person != replier);
                summary.participants.insert(0, replier);
                summary.participants.truncate(THREAD_FACES);
            }
            self.ctx.history(channel, &[first])?;
        }
        Ok(())
    }

    /// `message_replied`: the first message of a thread with its count as Slack has it.
    fn replied(&self, channel: &str, first: &Value) -> Result<(), Failure> {
        let (Some(ts), Some(mapped)) = (first["ts"].as_str(), self.message(channel, first)) else {
            return Ok(());
        };
        if let Some(mut stored) = self.ctx.stored_message(channel, ts)? {
            // The event counts the replies; how many of them are unread it does not say.
            let unread = stored.thread.as_ref().map_or(0, |summary| summary.unread);
            stored.thread = mapped.thread.map(|summary| ThreadSummary { unread, ..summary });
            self.thread_known(channel, ts)?;
            self.ctx.history(channel, &[stored])?;
        }
        Ok(())
    }

    /// `message_changed`: an edit, or a link preview arriving. Only what is stored is updated;
    /// an old message outside the cached pages is fetched in its new form when scrolled to.
    fn changed(&self, channel: &str, inner: &Value) -> Result<(), Failure> {
        let Some(message) = self.message(channel, inner) else {
            return Ok(());
        };
        let root = inner["thread_ts"].as_str().unwrap_or(&message.id);
        let reply = root != message.id;
        let in_channel = !reply || inner["subtype"] == "thread_broadcast";
        let places = [Some(thread_id(channel, root)), in_channel.then(|| channel.to_owned())];
        for place in places.into_iter().flatten() {
            let Some(stored) = self.ctx.stored_message(&place, &message.id)? else {
                continue;
            };
            let mut message = message.clone();
            message.reply_to = stored.reply_to;
            message.client_id = stored.client_id;
            // An answer to `chat.update` carries the text and not what hangs off the message.
            if inner["reply_count"].is_null() {
                message.thread = stored.thread;
            }
            if inner["reactions"].is_null() && !message.deleted {
                message.reactions = stored.reactions;
            }
            self.ctx.message(&place, &message)?;
        }
        Ok(())
    }

    fn reacted(&self, event: &Value) -> Result<(), Failure> {
        let item = &event["item"];
        let (Some(channel), Some(ts), Some(name)) = (item["channel"].as_str(), item["ts"].as_str(), event["reaction"].as_str()) else {
            return Ok(());
        };
        let (emoji, file) = self.reaction(name);
        let reaction = Reaction {
            emoji,
            file,
            count: 1,
            mine: self.me.get().is_some_and(|me| event["user"] == me.as_str()),
        };
        let added = event["type"] == "reaction_added";
        // The event does not say which thread a reply is in. The message is found in the
        // channel, in the thread it starts, or in the thread the user has open.
        let mut places = vec![channel.to_owned(), thread_id(channel, ts)];
        let watching = self.ctx.watching();
        let open = watching.conversation.as_ref().filter(|open| open.account() == self.ctx.account);
        if let Some(open) = open.filter(|open| thread_of(&open.id).is_some_and(|(parent, _)| parent == channel)) {
            if !places.contains(&open.id) {
                places.push(open.id.clone());
            }
        }
        for place in places {
            self.ctx.reacted(&place, ts, &reaction, added)?;
        }
        Ok(())
    }

    /// Everyone a page of messages was written by, looked up together before the page is read.
    fn authors(&self, entries: &[Value]) {
        let ids: Vec<String> = entries.iter().filter_map(|entry| entry["user"].as_str()).map(str::to_owned).collect();
        self.users(&ids);
    }

    /// What `serve_commands` hands over: a conversation the user opened, or one this adapter
    /// queued for itself after connecting (marked with [`WARM`]).
    fn fetch(&self, id: &str) -> Result<(), Failure> {
        let warm = id.strip_prefix(WARM);
        let conversation = warm.unwrap_or(id);
        if warm.is_some() {
            // Spaced out, and not in step: these are the only fetches nobody clicked for.
            if self.ctx.pause(WARM_GAP.mul_f64(1. + jitter() / 2.)) {
                return Ok(());
            }
            let counted = self.state().warm.remove(conversation);
            if !self.known(conversation)? {
                return Ok(());
            }
            if let Some(counted) = counted {
                self.count(conversation, &counted)?;
            }
        }
        // Warmed a moment ago and opened now, or the other way round: one fetch serves both.
        if self.state().fetched.get(conversation).is_some_and(|at| at.elapsed() < FRESH) {
            return Ok(());
        }
        self.backfill(conversation)?;
        let mut state = self.state();
        state.fetched.retain(|_, at| at.elapsed() < FRESH);
        state.fetched.insert(conversation.to_owned(), Instant::now());
        Ok(())
    }

    fn backfill(&self, conversation: &str) -> Result<(), Failure> {
        match thread_of(conversation) {
            Some((channel, root)) => self.replies(channel, root),
            None => self.page(conversation, Bounds::Newest),
        }
    }

    /// A page of a channel's own messages. What the page shows to be gone from the cache is
    /// removed, and the stretch it covers is remembered as whole.
    ///
    /// Unverified: with `oldest` and no `latest`, Slack is taken to answer the messages just
    /// after `oldest` (not the newest of the channel), with `has_more` saying there are newer.
    fn page(&self, channel: &str, bounds: Bounds) -> Result<(), Failure> {
        let (latest, oldest, inclusive, limit) = match bounds {
            Bounds::Newest => ("", "", "", HISTORY),
            Bounds::Before(ts) => (ts, "", "", HISTORY),
            Bounds::After(ts) => ("", ts, "", HISTORY),
            Bounds::Through(ts) => (ts, "", "true", AROUND),
        };
        let history = self.api(
            "conversations.history",
            &[("channel", channel), ("limit", limit), ("latest", latest), ("oldest", oldest), ("inclusive", inclusive)],
        )?;
        let entries = history["messages"].as_array().map_or(&[][..], Vec::as_slice);
        if !self.known(channel)? {
            return Ok(());
        }
        self.authors(entries);
        let mut messages: Vec<Message> = entries.iter().filter_map(|entry| self.message(channel, entry)).collect();
        for first in messages.iter_mut().filter(|message| message.thread.is_some()) {
            self.thread_known(channel, &first.id)?;
            // A page says how many replies a thread has, not how many are unread: what the
            // thread itself said when it was last opened stands.
            let unread = self.ctx.stored_message(channel, &first.id)?.and_then(|stored| stored.thread).map_or(0, |summary| summary.unread);
            if let Some(summary) = first.thread.as_mut().filter(|summary| summary.unread == 0) {
                summary.unread = unread;
            }
        }
        self.ctx.history(channel, &messages)?;

        let listed: Vec<&str> = entries.iter().filter_map(|entry| entry["ts"].as_str()).collect();
        let (first, last) = (listed.iter().min().copied(), listed.iter().max().copied());
        if let (Some(first), Some(last)) = (first, last) {
            // Deleted while nobody was listening. A message that has only just arrived may be
            // newer than the page, and a reply shown in the channel is the thread's to keep.
            let settled = crate::hub::now() - 10;
            for id in self.ctx.stored_ids(channel, first, last, 200)? {
                if listed.contains(&id.as_str()) || seconds(&id) >= settled {
                    continue;
                }
                let kept = self.ctx.stored_message(channel, &id)?.is_some_and(|stored| stored.reply_to.is_some());
                if !kept {
                    self.ctx.deleted(channel, &id)?;
                }
            }
        }
        let more = history["has_more"] == true;
        let start = first.filter(|_| more).unwrap_or("0");
        let (oldest, newest) = match bounds {
            Bounds::Newest => (start, LIVE),
            Bounds::Before(ts) | Bounds::Through(ts) => (start, ts),
            Bounds::After(ts) => (ts, last.filter(|_| more).unwrap_or(LIVE)),
        };
        let mut state = self.state();
        if state.covered.len() >= COVERED && !state.covered.contains_key(channel) {
            state.covered.clear();
        }
        cover(state.covered.entry(channel.to_owned()).or_default(), oldest, newest);
        Ok(())
    }

    /// The page after a message, when the cache is not known to hold it: reading forward from
    /// an old message the user jumped to. Inside a stretch already fetched nothing is asked.
    fn after(&self, channel: &str, after: &str) -> Result<(), Failure> {
        let held = self.state().covered.get(channel).and_then(|stretches| covering(stretches, after)).cloned();
        if let Some((_, newest)) = held {
            if newest == LIVE || self.ctx.stored_ids(channel, after, &newest, 2)?.len() > 1 {
                return Ok(());
            }
        }
        self.page(channel, Bounds::After(after))
    }

    /// The messages around one the user jumped to: it and those before it, then those after.
    fn around(&self, channel: &str, ts: &str) -> Result<(), Failure> {
        let held = self.state().covered.get(channel).and_then(|stretches| covering(stretches, ts)).cloned();
        let inside = held.is_some_and(|(oldest, newest)| oldest.as_str() < ts && ts < newest.as_str());
        if inside && self.ctx.stored_message(channel, ts)?.is_some() {
            return Ok(());
        }
        self.page(channel, Bounds::Through(ts))?;
        self.page(channel, Bounds::After(ts))
    }

    /// A thread, first message included, from the start. Its first message says whether the
    /// user follows it and how far they have read (documented for `conversations.replies`;
    /// unverified that this kind of session is given those fields).
    fn replies(&self, channel: &str, root: &str) -> Result<(), Failure> {
        let thread = thread_id(channel, root);
        if !self.known(channel)? {
            return Ok(());
        }
        self.thread_known(channel, root)?;
        let mut cursor = String::new();
        for _ in 0..REPLY_PAGES {
            let page = self.api(
                "conversations.replies",
                &[("channel", channel), ("ts", root), ("limit", REPLIES), ("cursor", &cursor)],
            )?;
            let entries = page["messages"].as_array().map_or(&[][..], Vec::as_slice);
            self.authors(entries);
            let messages: Vec<Message> = entries.iter().filter_map(|entry| self.message(channel, entry)).collect();
            self.ctx.history(&thread, &messages)?;
            if let Some(first) = entries.iter().find(|entry| entry["ts"] == root) {
                // The channel's copy of the first message carries the count.
                if let Some(mut stored) = self.ctx.stored_message(channel, root)? {
                    stored.thread = messages.iter().find(|message| message.id == root).and_then(|m| m.thread.clone());
                    self.ctx.history(channel, &[stored])?;
                }
                if first["subscribed"] == true {
                    let unread = first["unread_count"].as_u64().unwrap_or(0) as u32;
                    let latest = seconds(first["latest_reply"].as_str().unwrap_or(root));
                    self.ctx.counts(&thread, unread, 0, first["last_read"].as_str(), latest)?;
                }
            }
            cursor = page["response_metadata"]["next_cursor"].as_str().unwrap_or("").to_owned();
            if cursor.is_empty() {
                break;
            }
        }
        Ok(())
    }

    /// How many replies of a thread are unread, where the channel shows its first message.
    pub(super) fn thread_unread(&self, channel: &str, root: &str, unread: u32) -> Result<(), Failure> {
        let Some(mut first) = self.ctx.stored_message(channel, root)? else {
            return Ok(());
        };
        if let Some(summary) = first.thread.as_mut().filter(|summary| summary.unread != unread) {
            summary.unread = unread;
            self.ctx.history(channel, &[first])?;
        }
        Ok(())
    }

    // Commands.

    fn serve(&self, command: Command) {
        /// A command's answer: Slack's refusal goes back in words.
        fn answer<T>(reply: super::Reply<T>, outcome: Result<T, Fault>) {
            let _ = reply.send(outcome.map_err(|fault| Refusal::Refused(reason(fault))));
        }
        match command {
            // A thread is fetched whole from its start when opened, so only a channel has
            // pages before, after or around a message to ask for.
            Command::History { conversation_id, before } => {
                if thread_of(&conversation_id).is_none() {
                    self.report(self.page(&conversation_id, Bounds::Before(&before)));
                }
            }
            Command::HistoryAfter { conversation_id, after } => {
                if thread_of(&conversation_id).is_none() {
                    self.report(self.after(&conversation_id, &after));
                }
            }
            Command::HistoryAround { conversation_id, around } => {
                if thread_of(&conversation_id).is_none() {
                    self.report(self.around(&conversation_id, &around));
                }
            }
            Command::OpenSpace(_) => self.report(self.open_space()),
            Command::MarkRead { conversation_id, message_id } => self.report(self.mark_read(&conversation_id, &message_id)),
            Command::Send { conversation_id, client_id, text, reply_to } => {
                self.send(&conversation_id, &client_id, &text, reply_to.as_deref(), false)
            }
            Command::CancelSend { client_id, .. } => crate::blobs::upload::cancel(&self.ctx.account, &client_id),
            Command::ConversationInfo { conversation_id, reply } => answer(reply, self.conversation_info(&conversation_id)),
            Command::SetMute { conversation_id, notify, reply } => answer(reply, self.set_mute(&conversation_id, notify)),
            Command::Pinned { conversation_id, reply } => answer(reply, self.pinned(&conversation_id)),
            Command::SearchShared { conversation_id, kind, before, limit, reply } => {
                match self.search_shared(&conversation_id, kind, before.as_deref(), limit) {
                    Some(found) => answer(reply, found),
                    None => drop(reply.send(Err(Refusal::Unsupported))),
                }
            }
            // Only the workspace's emoji: Slack has no GIFs or stickers of its own (see
            // `capabilities`).
            Command::Catalog { kind: protocol::CatalogKind::Emoji, query, cursor, reply, .. } => {
                answer(reply, self.emoji_catalog(&query, cursor.as_deref()))
            }
            Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
            Command::OpenThreads(_) => self.report(self.open_threads()),
            Command::React { conversation_id, message_id, emoji, on, reply } => {
                answer(reply, self.react(&conversation_id, &message_id, &emoji, on));
            }
            Command::Edit { conversation_id, message_id, text, reply } => {
                answer(reply, self.edit(&conversation_id, &message_id, &text));
            }
            Command::Delete { conversation_id, message_id, reply } => {
                answer(reply, self.delete(&conversation_id, &message_id));
            }
            Command::SetPresence { presence, status, reply } => answer(reply, self.set_presence(presence, status)),
            Command::Profile { person_id, reply } => answer(reply, self.profile(&person_id)),
            Command::Typing(conversation) => self.typing(&conversation),
            Command::Watch(_) => self.watch(),
            Command::SearchPeople(query) => self.search(&query),
            Command::Members { conversation_id, offset, limit, reply } => {
                answer(reply, self.members(&conversation_id, offset, limit));
            }
            Command::StartDirect { person_id, reply } => answer(reply, self.start_direct(&person_id)),
            Command::Download { url, reply, .. } => {
                let _ = reply.send(Ok(self.download(url)));
            }
            Command::Handoff { conversation_id, reply } => {
                let _ = reply.send(Ok(self.huddle_link(&conversation_id)));
            }
            // Answered at once: there is nothing to end on Slack. The web session and its `d`
            // cookie belong to the browser the user signed in with and are shared with their
            // other workspaces, so ending it would sign those out too. Signing out here
            // forgets this workspace's token.
            Command::SignOut(reply) => {
                let _ = reply.send(Ok(()));
            }
            // Slack has no friends to accept or block (`Relate`), and no calls tinyclient carries.
            command => command.unsupported(&self.ctx),
        }
    }

    /// The socket has just connected, or the shell opened the Space.
    ///
    /// Connecting reads the unread state (`client.counts`) and the mutes, and nothing else:
    /// the rows are in the cache. Only a workspace never listed here has its sidebar read
    /// (`users.conversations`, every conversation ever), because without it there is nothing to
    /// show. Opening the Space again later reads `client.counts` alone: the socket has kept the
    /// rows current, and paging every DM again on each visit is the burst to avoid.
    fn open_space(&self) -> Result<(), Failure> {
        let (connected, loaded) = {
            let mut state = self.state();
            (std::mem::take(&mut state.resync), state.tree)
        };
        if connected {
            self.partners();
            self.tell_partners();
            return match self.ctx.note("tree") {
                Some(_) => self.resync(),
                None => self.load_tree(),
            };
        }
        match loaded {
            Some(at) if at.elapsed() < TREE_FRESH => Ok(()),
            Some(_) => self.recount(),
            None if self.ctx.note("tree").is_some() => self.recount(),
            None => self.load_tree(),
        }
    }

    fn mark_read(&self, conversation: &str, ts: &str) -> Result<(), Failure> {
        // A message of ours that Slack has not named yet has no timestamp to mark.
        if !ts.contains('.') {
            return Ok(());
        }
        match thread_of(conversation) {
            // Unverified: the web client's own call for a thread's read marker.
            Some((channel, root)) => {
                self.api(
                    "subscriptions.thread.mark",
                    &[("channel", channel), ("thread_ts", root), ("ts", ts), ("read", "1")],
                )?;
                self.thread_unread(channel, root, 0)?;
            }
            None => {
                self.api("conversations.mark", &[("channel", conversation), ("ts", ts)])?;
            }
        };
        Ok(())
    }

    /// Tells the conversation the user is typing, at most once per [`TYPING_EVERY`] for each
    /// conversation. Unverified: the web client's socket takes `user_typing`; the older
    /// documented name for the same frame is `typing`.
    fn typing(&self, conversation: &str) {
        let (channel, root) = match thread_of(conversation) {
            Some((channel, root)) => (channel, Some(root)),
            None => (conversation, None),
        };
        let mut state = self.state();
        state.typed.retain(|_, at| at.elapsed() < TYPING_EVERY);
        if state.typed.contains_key(conversation) {
            return;
        }
        state.typed.insert(conversation.to_owned(), Instant::now());
        let mut frame = json!({"type": "user_typing", "channel": channel});
        if let Some(root) = root {
            frame["thread_ts"] = root.into();
        }
        state.queue(frame);
    }

    /// Posts a message and ends the send. In a thread's conversation it is a reply to that
    /// thread. Slack has no reply outside threads, so a reply to a message in a channel is a
    /// reply in its thread that is also shown in the channel.
    fn send(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, suppress: bool) {
        let outcome = self.post(conversation, client_id, text, reply_to, suppress);
        self.ctx.sent(conversation, client_id, outcome);
    }

    /// The post itself, answering what Slack accepted.
    fn post(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, suppress: bool) -> Result<Message, String> {
        let (channel, thread) = match thread_of(conversation) {
            Some((channel, root)) => (channel, Some(root)),
            None => (conversation, None),
        };
        let nonce = client_msg_id(client_id);
        let escaped = escape(text);
        let mut params = vec![("channel", channel), ("text", escaped.as_str()), ("client_msg_id", nonce.as_str())];
        let shown = reply_to.filter(|_| thread.is_none());
        if let Some(root) = thread.or(shown) {
            params.push(("thread_ts", root));
        }
        if shown.is_some() {
            params.push(("reply_broadcast", "true"));
        }
        // The preview chip dismissed: Slack is asked not to unfurl the links.
        if suppress {
            params.extend([("unfurl_links", "false"), ("unfurl_media", "false")]);
        }
        let posted = self.api("chat.postMessage", &params).map_err(reason);
        let outcome = posted.and_then(|posted| {
            let mut event = posted["message"].clone();
            if event["ts"].is_null() {
                event["ts"] = posted["ts"].clone();
            }
            let accepted = self.message(channel, &event);
            accepted.ok_or_else(|| "Slack accepted it but sent nothing back.".to_owned())
        });
        outcome.map(|mut message| {
            if let Some(root) = thread {
                self.report(self.bump(channel, root, &message));
            }
            if let Some(root) = shown {
                message.reply_to = self.quote(channel, root);
            }
            message
        })
    }

    /// Slack's name for a reaction: the shell sends the emoji itself, a custom one as `:name:`,
    /// or the id of a custom emoji's registered file.
    fn reaction_name(&self, emoji: &str) -> Option<String> {
        if let Some(file) = emoji.strip_prefix("emoji:") {
            return file.rsplit_once(':').map(|(name, _)| name.to_owned());
        }
        let named = emoji.strip_prefix(':').and_then(|rest| rest.strip_suffix(':'));
        match named.filter(|name| !name.is_empty()) {
            Some(name) => Some(name.to_owned()),
            // The table holds the emoji people actually use, not all of Unicode: one outside
            // it has no name here and is refused in words.
            None => emoji::name(emoji),
        }
    }

    /// Adds or takes back our reaction. The store already shows it; Slack's own event for it
    /// arrives on the socket and counts once.
    fn react(&self, conversation: &str, ts: &str, emoji: &str, on: bool) -> Result<(), Fault> {
        let channel = thread_of(conversation).map_or(conversation, |(channel, _)| channel);
        let name = self.reaction_name(emoji).ok_or_else(|| Fault::Slack("no_emoji_name".to_owned()))?;
        let method = if on { "reactions.add" } else { "reactions.remove" };
        match self.api(method, &[("channel", channel), ("timestamp", ts), ("name", &name)]) {
            // Already as asked, from another device.
            Err(Fault::Slack(error)) if error == "already_reacted" || error == "no_reaction" => Ok(()),
            other => other.map(drop),
        }
    }

    /// Replaces the text of one of our messages and reports it as Slack now has it.
    fn edit(&self, conversation: &str, ts: &str, text: &str) -> Result<(), Fault> {
        let (channel, thread) = match thread_of(conversation) {
            Some((channel, root)) => (channel, Some(root)),
            None => (conversation, None),
        };
        let updated = self.api("chat.update", &[("channel", channel), ("ts", ts), ("text", &escape(text))])?;
        let mut inner = match &updated["message"] {
            message @ Value::Object(_) => message.clone(),
            _ => json!({"type": "message", "text": updated["text"].as_str().unwrap_or(&escape(text))}),
        };
        inner["ts"] = ts.into();
        if inner["user"].is_null() {
            inner["user"] = self.me.get().map_or("", String::as_str).into();
        }
        if inner["edited"].is_null() {
            inner["edited"] = json!({"ts": ts});
        }
        if let (Some(root), true) = (thread, inner["thread_ts"].is_null()) {
            inner["thread_ts"] = root.into();
        }
        self.changed(channel, &inner).map_err(kept)
    }

    /// Deletes a message for everyone, here as on the socket: from the channel and its thread.
    fn delete(&self, conversation: &str, ts: &str) -> Result<(), Fault> {
        let (channel, root) = match thread_of(conversation) {
            Some((channel, root)) => (channel, root),
            None => (conversation, ts),
        };
        self.api("chat.delete", &[("channel", channel), ("ts", ts)])?;
        let gone = self.ctx.deleted(channel, ts).and_then(|()| self.ctx.deleted(&thread_id(channel, root), ts));
        gone.map_err(|err| Fault::Net(err.to_string()))
    }

    /// Our own presence, and the status text when one is given (empty clears it).
    fn set_presence(&self, presence: Presence, status: Option<String>) -> Result<(), Fault> {
        let word = match presence {
            Presence::Online => Some("auto"),
            // Slack knows two words. Away is all it has for not being here.
            Presence::Idle | Presence::Offline => Some("away"),
            // Not built: Slack's Do Not Disturb is a snooze that must be given its length
            // (`dnd.setSnooze`, `num_minutes`), and the request carries none. Refused in words
            // rather than shown as set when it is not.
            Presence::Dnd | Presence::Busy => return Err(Fault::Slack("no_dnd_end".to_owned())),
            Presence::Unknown => None,
        };
        if let Some(word) = word {
            self.api("users.setPresence", &[("presence", word)])?;
        }
        let me = self.me.get().cloned().unwrap_or_default();
        if let Some(text) = &status {
            // An emptied emoji with a text makes Slack show its default, the speech balloon.
            let profile = json!({"status_text": text, "status_emoji": "", "status_expiration": 0}).to_string();
            let set = self.api("users.profile.set", &[("profile", &profile)])?;
            if set["profile"].is_object() {
                let handle = self.state().handle.clone();
                self.learn(&[json!({"id": me, "name": handle, "profile": set["profile"]})]);
            }
        }
        let shown = match (self.cached(&me), status) {
            (Some(user), _) => user.status,
            (None, status) => status.unwrap_or_default(),
        };
        self.ctx.presence(&me, presence, &shown, true);
        Ok(())
    }

    /// One person's card. `users.info` alone carries all of it (the title in the profile, the
    /// time zone and its offset beside it), so the one request the contract allows is enough
    /// and `users.profile.get` is not called as well.
    fn profile(&self, person: &str) -> Result<About, Fault> {
        if !self.info_due(person) {
            // Asked within the hour: the card read then, or what the row knows.
            return Ok(self.state().abouts.get(person).cloned().unwrap_or_default());
        }
        let info = self.api("users.info", &[("user", person)])?;
        self.learn(std::slice::from_ref(&info["user"]));
        Ok(self.keep_about(person, &info["user"]))
    }

    /// The card in a `users.info` answer, kept for the hour no other is asked for.
    fn keep_about(&self, person: &str, user: &Value) -> About {
        let about = about_of(user);
        self.state().abouts.insert(person.to_owned(), about.clone());
        about
    }
}

/// A person's card from their `users.info` answer.
fn about_of(user: &Value) -> About {
    let said = |key: &str| user["profile"][key].as_str().unwrap_or("").trim().to_owned();
    let contact = |kind, value: String| (!value.is_empty()).then_some(protocol::Contact { kind, value, masked: kind == protocol::ContactKind::Phone });
    let contacts = [contact(protocol::ContactKind::Email, said("email")), contact(protocol::ContactKind::Phone, said("phone"))];
    // Slack's status expiry, 0 for none.
    let until = user["profile"]["status_expiration"].as_i64().filter(|until| *until > 0);
    About {
        // Slack has no bio: the title is a title (06, story 3).
        title: said("title"),
        pronouns: said("pronouns"),
        contacts: contacts.into_iter().flatten().collect(),
        // Every workspace of the sign-in is its own account; what they share is not asked.
        mutual_spaces: Vec::new(),
        time_zone: user["tz"].as_str().unwrap_or("").to_owned(),
        utc_offset: user["tz_offset"].as_i64().map(|offset| offset as i32),
        status_until: until,
        ..About::default()
    }
}

/// What a Slack account may send. The same on every plan (unverified: a workspace may lower
/// the file limit or turn uploads off, which Slack then refuses in words). No GIF picker:
/// Slack has no GIF search of its own. Its GIFs come from the Giphy app a workspace may
/// install, run as `/giphy`, which a client can only reach through the web client's
/// undocumented `chat.command` (an imitation the account rules rule out, ruling 09); any other
/// search would be a third-party service with a key of ours. A GIF pasted as a link still
/// unfurls, and one sent as a file plays.
fn capabilities() -> protocol::Capabilities {
    protocol::Capabilities {
        max_file_bytes: send::LARGEST,
        max_files: send::FILES as u32,
        custom_emoji: protocol::EmojiScope::All,
        schedule: true,
        suppress_embeds: true,
        ..protocol::Capabilities::default()
    }
}

/// A store error on the way out of a command, as a fault. Slack had already said yes.
fn kept(failure: Failure) -> Fault {
    match failure {
        Failure::SignedOut(why) => Fault::Auth(why),
        Failure::Dropped(why) => Fault::Net(why),
    }
}

/// The headers a file is fetched with: always the session's browser, and the session itself
/// only for an address that answers to nobody else.
fn download_headers(agent: &str, token: &str, cookie: &str, private: bool) -> Vec<(String, String)> {
    let mut headers = vec![("User-Agent".to_owned(), agent.to_owned())];
    if private {
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
        headers.push(("Cookie".to_owned(), cookie.to_owned()));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_mention_survives_the_escapes_and_nothing_else_does() {
        assert_eq!(super::escape("hi <@U07ABC> in <#C12> <!here> <!subteam^S9>"), "hi <@U07ABC> in <#C12> <!here> <!subteam^S9>");
        assert_eq!(super::escape("a < b && c > d <b> <@lower> <!nope> <@>"), "a &lt; b &amp;&amp; c &gt; d &lt;b&gt; &lt;@lower&gt; &lt;!nope&gt; &lt;@&gt;");
        assert_eq!(super::escape("é<@U1>é"), "é<@U1>é");
    }

    #[test]
    fn one_sign_in_is_one_account_per_workspace_and_each_can_be_removed() {
        let session = json!({
            "cookie": "xoxd-abc", "user_agent": "Safari/1",
            "teams": [
                {"id": "T1", "name": "Acme", "url": "https://acme.slack.com/", "token": "xoxc-1"},
                {"id": "T2", "name": "", "url": "https://side-project.slack.com/", "token": "xoxc-2"},
                {"id": "T3", "name": "No token", "url": "https://gone.slack.com/", "token": ""},
            ],
        })
        .to_string();
        let found = accounts(&session).unwrap();
        let listed: Vec<(&str, &str)> = found.iter().map(|signed| (signed.account.as_str(), signed.name.as_str())).collect();
        assert_eq!(listed, [("T1", "Acme"), ("T2", "side-project")]);
        let second: Session = serde_json::from_str(&found[1].session).unwrap();
        assert_eq!((second.token.as_str(), second.cookie.as_str()), ("xoxc-2", "xoxd-abc"));
        assert_eq!((second.team_id.as_str(), second.user_agent.as_str()), ("T2", "Safari/1"));

        let left = without(&session, "T1").unwrap();
        let ids: Vec<String> = accounts(&left).unwrap().into_iter().map(|signed| signed.account).collect();
        assert_eq!(ids, ["T2"]);
        assert_eq!(without(&without(&left, "T3").unwrap(), "T2"), None);
    }

    #[test]
    fn a_session_stored_before_workspaces_were_listed_still_signs_in() {
        let old = r#"{"token":"xoxc-9","team_id":"T9","team":"Glimmr","url":"https://glimmr.slack.com/","cookie":"xoxd-z"}"#;
        let found = accounts(old).unwrap();
        assert_eq!((found[0].account.as_str(), found[0].name.as_str()), ("T9", "Glimmr"));
        let session: Session = serde_json::from_str(&found[0].session).unwrap();
        assert_eq!((session.token.as_str(), session.user_agent.as_str()), ("xoxc-9", ""));
        // Its one workspace is all there is, so removing it leaves nothing.
        assert_eq!(without(old, "T9"), None);
    }

    #[test]
    fn a_client_id_survives_the_trip_through_slacks_nonce() {
        for id in ["1", "1760400000123456", "9999999999999999999999999"] {
            let nonce = client_msg_id(id);
            let shape: Vec<usize> = nonce.split('-').map(str::len).collect();
            assert_eq!(shape, [8, 4, 4, 4, 12], "{nonce}");
            assert!(nonce.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit()));
            assert_eq!(client_id(&nonce).as_deref(), Some(id));
        }
        // Another client's nonce is nobody's pending message.
        assert_eq!(client_id("6d0c1f3e-2a4b-4c5d-8e6f-0a1b2c3d4e5f"), None);
        assert_eq!(client_id("not-a-uuid"), None);
    }

    #[test]
    fn only_slacks_own_addresses_count_as_slack() {
        assert_eq!(https_host("https://files.slack.com/files-pri/T1-F1/a.png"), Some("files.slack.com"));
        assert_eq!(https_host("http://files.slack.com/a.png"), None);
        assert_eq!(https_host("https://files.slack.com:pw@evil.example/a.png"), None);
        assert!(within("avatars.slack-edge.com", "slack-edge.com"));
        assert!(!within("notslack-edge.com", "slack-edge.com"));
        assert!(!within("slack.com.evil.example", "slack.com"));
    }

    #[test]
    fn stretches_of_history_join_where_they_touch_and_a_message_is_found_in_its_own() {
        let mut held = Vec::new();
        cover(&mut held, "1760000090.000090", LIVE);
        cover(&mut held, "1759000400.000400", "1759000500.000500");
        cover(&mut held, "1759000500.000500", "1759990200.000200");
        assert_eq!(held.len(), 2, "{held:?}");
        assert_eq!(covering(&held, "1759990100.000100"), Some(&("1759000400.000400".to_owned(), "1759990200.000200".to_owned())));
        assert_eq!(covering(&held, "1760000300.000300").map(|(_, newest)| newest.as_str()), Some(LIVE));
        assert_eq!(covering(&held, "1759995000.000000"), None, "the gap between the two is not held");
        // An older page that reaches the newest stretch makes it one; only a few are kept.
        cover(&mut held, "1759990200.000200", "1760000090.000090");
        assert_eq!(held, [("1759000400.000400".to_owned(), LIVE.to_owned())]);
        for at in 0..10 {
            cover(&mut held, &format!("17000000{at}0.000000"), &format!("17000000{at}5.000000"));
        }
        assert_eq!(held.len(), STRETCHES);
        assert_eq!(held.last().map(|(_, newest)| newest.as_str()), Some(LIVE), "the newest are the ones kept");
    }

    #[test]
    fn a_reissued_cookie_replaces_only_the_one_this_sign_in_was_started_with() {
        let stored = json!({"cookie": "xoxd-old", "user_agent": "Safari/1", "teams": [{"id": "T1", "token": "xoxc-1", "url": "https://a.slack.com/"}]}).to_string();
        let fresh: Value = serde_json::from_str(&recookied(&stored, "xoxd-old", "xoxd-new").unwrap()).unwrap();
        assert_eq!((fresh["cookie"].as_str(), fresh["teams"][0]["token"].as_str(), fresh["user_agent"].as_str()), (Some("xoxd-new"), Some("xoxc-1"), Some("Safari/1")));
        // Another workspace of the sign-in saved a newer one first: it stands.
        assert_eq!(recookied(&fresh.to_string(), "xoxd-old", "xoxd-other"), None);
        assert_eq!(recookied("not json", "xoxd-old", "xoxd-new"), None);
    }

    #[test]
    fn a_file_is_fetched_as_the_sessions_browser_and_only_a_private_one_with_the_session() {
        let open = download_headers("Safari/1", "xoxc-1", "d=xoxd-abc", false);
        assert_eq!(open, [("User-Agent".to_owned(), "Safari/1".to_owned())]);
        let private = download_headers("Safari/1", "xoxc-1", "d=xoxd-abc", true);
        let named: Vec<(&str, &str)> = private.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        assert_eq!(named, [("User-Agent", "Safari/1"), ("Authorization", "Bearer xoxc-1"), ("Cookie", "d=xoxd-abc")]);
    }

    #[test]
    fn every_method_is_held_to_half_its_documented_tier() {
        // Tier 2 is 20 a minute, tier 3 is 50, tier 4 is 100; what Slack does not document is
        // treated as tier 3.
        assert_eq!((per_minute("users.setPresence"), per_minute("conversations.history"), per_minute("conversations.members")), (20., 50., 100.));
        assert_eq!((per_minute("client.counts"), per_minute("edge:users/info")), (50., 50.));
        assert!(WARM_GAP.as_secs_f64() >= 2. * 60. / per_minute("conversations.history"));
        assert!((0. ..1.).contains(&jitter()));
    }

    #[test]
    fn a_refused_send_is_told_in_words() {
        assert_eq!(
            reason(Fault::Slack("two_factor_setup_required".into())),
            "Slack wants two-factor authentication set up. Finish that in the Slack app."
        );
        assert_eq!(reason(Fault::Slack("not_in_channel".into())), "You are not in this channel.");
        assert_eq!(
            reason(Fault::Slack("restricted_action_read_only_channel".into())),
            "This workspace does not let you post here."
        );
        assert_eq!(reason(Fault::Limited(7)), "Slack is rate limiting you. Try again in 7 s.");
        assert_eq!(reason(Fault::Slack("fatal_error".into())), "Slack refused (fatal_error).");
    }
}
