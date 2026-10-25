//! The live socket: one per signed-in server, at `GET /v1/live`. Its frames, close codes and
//! reconnect rules are tinyserver's `docs/calls.md`. It holds the link, who is online, and who
//! is in each room's call, and it tells the page when people, roles, invites or rooms change, so
//! the page never polls.
//!
//! Sync tungstenite, as Discord's gateway: the thread sleeps in `poll` on the socket and on a
//! wake socket, so it wakes for a frame, for a call request from the command thread, or for the
//! next ping, and for nothing else. It does not use `keep_connected`: the server says by its
//! close code whether to resume, start over, sign in again or stop, and asks for a fixed backoff.
//!
//! Joining a room only takes a seat. There is no sound yet, and nothing here touches `mediad`.
//! A server whose `/health` says its audio is up wants an offer with every join, which this
//! client cannot make, so no join is sent to it.

use super::super::discord::gateway::{wait_sockets, Pollable, WakePair, POLL_IN, POLL_OUT};
use super::super::{net, Context, Refusal, Reply, ReconnectWait, PANIC_PAUSE, RECONNECTING};
use crate::hub::lock;
use super::{members_of, Saved};
use protocol::{join_host, Link, Occupant, Presence};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::HandshakeError;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// How long each part of the connection may take, and how the pauses between them grow. Tests
/// shorten them; the server's own values are [`SERVER`].
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    /// A ping this often. The server closes a socket that says nothing for 60 s.
    pub ping: Duration,
    /// The longest a connect, the handshake, a REST read or the answer to a call request takes.
    pub answer: Duration,
    /// The first pause after a drop, doubled after each one up to `cap`.
    pub first: Duration,
    pub cap: Duration,
    /// A connection that held this long starts the pauses from `first` again.
    pub held: Duration,
    /// A server with no live socket (an older tinyserver) is asked again after this.
    pub absent: Duration,
}

/// `docs/calls.md`: ping every 25 s; reconnect after 0.5, 1, 2 and 4 s, capped at 10.
pub(crate) const SERVER: Timing = Timing {
    ping: Duration::from_secs(25),
    answer: Duration::from_secs(10),
    first: Duration::from_millis(500),
    cap: Duration::from_secs(10),
    held: Duration::from_secs(60),
    absent: Duration::from_secs(300),
};

/// The server sends at most 64 KiB a frame today. `ready` lists everyone online, so it is
/// allowed more.
const FRAME: usize = 4 << 20;
/// Names kept for the people in calls. The server's call frames carry only user ids.
const NAMES: usize = 2_000;
/// Rooms with someone in them, and people in one room. The server caps them at 12 and 32.
const ROOMS: usize = 256;
const SEATS: usize = 64;
/// Call requests waiting for the socket. A user clicks; nothing queues many.
const QUEUED: usize = 16;
/// How often a held socket looks up, to notice it was stopped.
const HELD: Duration = Duration::from_secs(30);

const OFFLINE: &str = "Tiny server isn't connected. Try again in a moment.";
const LOST: &str = "The connection to the server dropped. Try again.";
const NO_ANSWER: &str = "The server didn't answer. Try again.";
const BUSY: &str = "Wait for the last request to finish.";
const AUDIO_SOON: &str = "This server has audio, and tinyclient can't join with audio yet.";
const AUDIO_DOWN: &str = "This server's audio is down. Try again later.";
pub(crate) const ENDED: &str = "Tiny server signed you out. Sign in again.";
pub(crate) const REMOVED: &str = "You were removed from this server.";
pub(crate) const REPLACED: &str = "This account connected on another device, so this one stopped. Open the server to connect again.";

/// What the command thread hands the socket thread.
pub enum Ask {
    Join { room_id: String, reply: Reply<()> },
    Leave { reply: Reply<()> },
    Kick { room_id: String, user_id: String, reply: Reply<()> },
    /// The page was opened. A socket another device replaced tries again.
    Look,
}

impl Ask {
    fn into_reply(self) -> Option<Reply<()>> {
        match self {
            Ask::Join { reply, .. } | Ask::Leave { reply } | Ask::Kick { reply, .. } => Some(reply),
            Ask::Look => None,
        }
    }

    fn refuse(self, words: &str) {
        if let Some(reply) = self.into_reply() {
            let _ = reply.send(Err(Refusal::Refused(words.to_owned())));
        }
    }

    pub fn unsupported(self) {
        if let Some(reply) = self.into_reply() {
            let _ = reply.send(Err(Refusal::Unsupported));
        }
    }
}

/// Shared by the socket thread and the command thread.
pub struct Live {
    wake: WakePair,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    queued: VecDeque<Ask>,
    /// `ready` was heard on the socket that is up now.
    connected: bool,
    /// Closed with 4008: nothing is tried until the user opens the server.
    held: bool,
    /// `/health`'s `media`, read at each fresh `ready`. Empty or `off` takes a seat without audio.
    media: String,
}

impl Live {
    pub fn new() -> io::Result<Live> {
        Ok(Live { wake: WakePair::new()?, inner: Mutex::new(Inner::default()) })
    }

    /// From the command thread. A request while the socket is down is refused at once.
    pub fn ask(&self, ask: Ask) {
        let mut inner = lock(&self.inner);
        match ask {
            Ask::Look => {
                if std::mem::take(&mut inner.held) {
                    drop(inner);
                    self.wake.signal();
                }
            }
            ask if !inner.connected => ask.refuse(OFFLINE),
            ask @ Ask::Join { .. } if inner.media == "up" => ask.refuse(AUDIO_SOON),
            ask @ Ask::Join { .. } if inner.media == "down" => ask.refuse(AUDIO_DOWN),
            ask if inner.queued.len() >= QUEUED => ask.refuse(BUSY),
            ask => {
                inner.queued.push_back(ask);
                drop(inner);
                self.wake.signal();
            }
        }
    }

    /// Wakes the socket thread, to notice it was stopped.
    pub fn wake(&self) {
        self.wake.signal();
    }

    fn connected(&self, connected: bool) {
        let refused: Vec<Ask> = {
            let mut inner = lock(&self.inner);
            inner.connected = connected;
            if connected { Vec::new() } else { inner.queued.drain(..).collect() }
        };
        refused.into_iter().for_each(|ask| ask.refuse(LOST));
    }

    fn take(&self) -> Vec<Ask> {
        lock(&self.inner).queued.drain(..).collect()
    }

    fn set_media(&self, media: String) {
        lock(&self.inner).media = media;
    }

    /// A join without an offer can work: the server runs without audio.
    fn seats_only(&self) -> bool {
        matches!(lock(&self.inner).media.as_str(), "" | "off")
    }

    fn hold(&self, held: bool) {
        lock(&self.inner).held = held;
    }

    fn held(&self) -> bool {
        lock(&self.inner).held
    }
}

/// What is known across connections. Ephemeral: gone at restart, and `ready` replaces it.
struct Seat {
    since: Option<i64>,
    me: String,
    role: String,
    names: HashMap<String, String>,
    /// Who is in each room's call, in the order they joined.
    calls: BTreeMap<String, Vec<Sitting>>,
    /// The room the user joined and has not left. A socket that drops (a server restart) does
    /// not hear the seat go, so `ready` without us in it means take it again.
    wanted: Option<String>,
    described: bool,
}

/// Someone in a call, and the mute and deafen they set.
#[derive(Clone)]
struct Sitting {
    user: String,
    muted: bool,
    deafened: bool,
}

impl Sitting {
    fn new(user: &str) -> Sitting {
        Sitting { user: user.to_owned(), muted: false, deafened: false }
    }
}

/// How one connection ended, and so what happens next.
#[derive(Debug, PartialEq)]
enum End {
    Stopped,
    /// 401 at the handshake, or 4001.
    SignedOut,
    /// 4003.
    Removed,
    /// 4008.
    Replaced,
    /// `resync`, or 4002: start over without `since`.
    Resync,
    /// 4029.
    SlowDown,
    /// No live socket at this address: a 404 to the upgrade.
    Absent,
    /// Anything else, 1011, 1012 and 4000 among it: resume with `since`.
    Dropped(String),
}

fn closed(code: u16, reason: &str) -> End {
    match code {
        4001 => End::SignedOut,
        4002 => End::Resync,
        4003 => End::Removed,
        4008 => End::Replaced,
        4029 => End::SlowDown,
        _ => End::Dropped(format!("closed {code} {reason}").trim().to_owned()),
    }
}

/// What a refused call request says.
fn words(kick: bool, code: &str) -> &'static str {
    match code {
        "full" => "That room is full.",
        "server_full" => "Every seat on this server is taken. Try again later.",
        "not_found" if kick => "They're not in that room's call.",
        "not_found" => "That room is gone.",
        "forbidden" => "You can't do that.",
        "slow_down" => "Too many tries. Wait a moment, then try again.",
        "unavailable" => "The server had a problem. Try again in a moment.",
        "unknown_type" => "This server doesn't take calls yet.",
        "bad_frame" => "The server couldn't read that request.",
        "bad_sdp" => "The server couldn't use tinyclient's audio setup.",
        "media_unavailable" => "This server's audio isn't available. Try again later.",
        _ => "That didn't work. Try again.",
    }
}

/// `http://host:8080` is `ws://host:8080/v1/live`, `https` is `wss`.
fn live_url(base: &str, since: Option<i64>) -> Option<String> {
    let rest = base.strip_prefix("https://").map(|rest| ("wss://", rest)).or_else(|| base.strip_prefix("http://").map(|rest| ("ws://", rest)));
    let (scheme, rest) = rest?;
    let rest = rest.trim_end_matches('/');
    Some(match since {
        Some(since) => format!("{scheme}{rest}/v1/live?since={since}"),
        None => format!("{scheme}{rest}/v1/live"),
    })
}

/// Keeps the socket up until the adapter stops, the session ends, or the account is removed.
pub(super) fn keep_live(ctx: &Context, saved: &Saved, live: &Live, timing: &Timing) {
    let mut seat = Seat {
        since: None,
        me: saved.user_id.clone(),
        role: saved.role.clone(),
        names: HashMap::new(),
        calls: BTreeMap::new(),
        wanted: None,
        described: false,
    };
    let mut pause = timing.first;
    while !ctx.stopped() {
        let generation = ctx.stop.recovery.load(Ordering::Relaxed);
        let began = Instant::now();
        let end = match catch_unwind(AssertUnwindSafe(|| connect(ctx, saved, live, &mut seat, timing))) {
            Ok(end) => end,
            Err(_) => {
                live.connected(false);
                if !ctx.panicked("connection") || ctx.pause(PANIC_PAUSE) {
                    return;
                }
                End::Dropped("tinyclient failed on this connection".to_owned())
            }
        };
        live.connected(false);
        if ctx.stopped() || end == End::Stopped {
            return;
        }
        // Nothing heard while down is true any more; `ready` brings it all back.
        seat.calls.clear();
        ctx.hub.forget_live(&ctx.account);
        if began.elapsed() >= timing.held {
            pause = timing.first;
        }
        // Resuming and starting over climb the server's ladder; the others have their own pause.
        let climbs = matches!(end, End::Resync | End::Dropped(_));
        let absent_here = end == End::Absent;
        let wait = match end {
            End::Stopped => return,
            End::SignedOut => {
                eprintln!("chatd: {}: the session ended", ctx.account.network.label());
                ctx.hub.set_link(&ctx.account, Link::Expired, ENDED);
                return;
            }
            End::Removed => {
                eprintln!("chatd: {}: removed from the server", ctx.account.network.label());
                ctx.hub.set_link(&ctx.account, Link::Expired, REMOVED);
                return;
            }
            End::Replaced => {
                // Two devices that each reconnect at once would close each other forever.
                eprintln!("chatd: {}: replaced by another device; waiting for the user", ctx.account.network.label());
                ctx.hub.set_offline(&ctx.account, false);
                ctx.attention(REPLACED);
                if hold(ctx, live) {
                    return;
                }
                ctx.attention("");
                pause = timing.first;
                continue;
            }
            End::Resync => {
                seat.since = None;
                pause
            }
            End::SlowDown => timing.cap,
            End::Absent => {
                // An older server: what REST can do still works, and the socket is asked for
                // now and then.
                if !absent(ctx, saved, &mut seat) {
                    return;
                }
                timing.absent
            }
            End::Dropped(why) => {
                eprintln!("chatd: {}: live socket: {}", ctx.account.network.label(), net::redact(&why));
                pause
            }
        };
        if climbs {
            pause = (pause * 2).min(timing.cap);
        }
        if !absent_here || ctx.hub.link(&ctx.account) != Link::Connected {
            ctx.hub.set_offline(&ctx.account, wait <= RECONNECTING);
        }
        match ctx.reconnect_pause(wait, generation) {
            ReconnectWait::Stopped => return,
            ReconnectWait::Elapsed => {}
            ReconnectWait::Recovered => {
                pause = timing.first;
                if ctx.pause(timing.first.saturating_sub(began.elapsed())) {
                    return;
                }
            }
        }
    }
}

/// Waits for the user to open the server again. True if the adapter was stopped meanwhile.
fn hold(ctx: &Context, live: &Live) -> bool {
    live.hold(true);
    let wake = live.wake.poll_read();
    while live.held() {
        if ctx.stopped() {
            return true;
        }
        wait_sockets(Instant::now() + HELD, &[(wake, POLL_IN)]);
        live.wake.drain();
    }
    ctx.stopped()
}

/// A server with no live socket: connected if its session still works. False once it does not.
fn absent(ctx: &Context, saved: &Saved, seat: &mut Seat) -> bool {
    match super::get_me(saved) {
        Ok(me) => {
            if !seat.described {
                let _ = ctx.describe(&join_host(&saved.base_url), &me.email);
                seat.described = true;
            }
            ctx.link(Link::Connected);
            true
        }
        Err(super::Fault::Unauthorized) => {
            ctx.hub.set_link(&ctx.account, Link::Expired, ENDED);
            false
        }
        Err(_) => true,
    }
}

/// One request the server has not answered yet. A rejoin has nobody waiting on it.
struct Pending {
    reference: String,
    asked: Asked,
    until: Instant,
    reply: Option<Reply<()>>,
}

#[derive(Clone)]
enum Asked {
    Join(String),
    Leave,
    Kick,
}

fn connect(ctx: &Context, saved: &Saved, live: &Live, seat: &mut Seat, timing: &Timing) -> End {
    let Some(url) = live_url(&saved.base_url, seat.since) else {
        return End::Dropped("the server's address is not http or https".to_owned());
    };
    let mut wire = match Wire::open(&url, &saved.token, timing.answer) {
        Ok(wire) => wire,
        Err(Opened::Status(401)) => return End::SignedOut,
        Err(Opened::Status(404)) => return End::Absent,
        Err(Opened::Status(status)) => return End::Dropped(format!("the server answered {status}")),
        Err(Opened::Failed(why)) => return End::Dropped(why),
    };
    let mut pending = Vec::new();
    let end = serve(ctx, saved, live, seat, timing, &mut wire, &mut pending);
    for pending in pending {
        if let Some(reply) = pending.reply {
            let _ = reply.send(Err(Refusal::Refused(LOST.to_owned())));
        }
    }
    if end == End::Stopped {
        wire.close();
    }
    end
}

#[allow(clippy::too_many_arguments)]
fn serve(ctx: &Context, saved: &Saved, live: &Live, seat: &mut Seat, timing: &Timing, wire: &mut Wire, pending: &mut Vec<Pending>) -> End {
    let wake = live.wake.poll_read();
    let mut next_ping = Instant::now() + timing.ping;
    // Something arrived since the last ping. A server that answers nothing is gone.
    let mut heard = true;
    let mut ready = false;
    let mut refs = 0u64;
    loop {
        if ctx.stopped() {
            return End::Stopped;
        }
        loop {
            match wire.read() {
                Ok(Some(text)) => {
                    heard = true;
                    let frame: Value = match serde_json::from_str(&text) {
                        Ok(frame) => frame,
                        Err(_) => continue,
                    };
                    if let Some(end) = heard_frame(ctx, saved, live, seat, pending, &mut ready, &frame) {
                        return end;
                    }
                    if ready && frame.get("type").and_then(Value::as_str) == Some("ready") && live.seats_only() {
                        if let Some(room) = rejoin(seat) {
                            refs += 1;
                            if let Err(err) = ask_join(wire, pending, refs, room, None, timing) {
                                return End::Dropped(err.to_string());
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(Lost::Closed(code, reason)) => return closed(code, &reason),
                Err(Lost::Broken(why)) => return End::Dropped(why),
            }
        }
        if ready {
            for ask in live.take() {
                refs += 1;
                let sent = match ask {
                    Ask::Join { room_id, reply } => ask_join(wire, pending, refs, room_id, Some(reply), timing),
                    Ask::Leave { reply } => {
                        let reference = refs.to_string();
                        pending.push(Pending { reference: reference.clone(), asked: Asked::Leave, until: Instant::now() + timing.answer, reply: Some(reply) });
                        wire.send(json!({"type": "call.leave", "ref": reference}).to_string())
                    }
                    Ask::Kick { room_id, user_id, reply } => {
                        let reference = refs.to_string();
                        pending.push(Pending { reference: reference.clone(), asked: Asked::Kick, until: Instant::now() + timing.answer, reply: Some(reply) });
                        wire.send(json!({"type": "call.kick", "ref": reference, "room_id": room_id, "user_id": user_id}).to_string())
                    }
                    Ask::Look => Ok(()),
                };
                if let Err(err) = sent {
                    return End::Dropped(err.to_string());
                }
            }
        }
        let now = Instant::now();
        let (late, waiting): (Vec<Pending>, Vec<Pending>) = std::mem::take(pending).into_iter().partition(|pending| pending.until <= now);
        *pending = waiting;
        for late in late {
            if let Some(reply) = late.reply {
                let _ = reply.send(Err(Refusal::Refused(NO_ANSWER.to_owned())));
            }
        }
        if now >= next_ping {
            if !heard {
                return End::Dropped("the server stopped answering".to_owned());
            }
            if let Err(err) = wire.send(json!({"type": "ping"}).to_string()) {
                return End::Dropped(err.to_string());
            }
            heard = false;
            next_ping = now + timing.ping;
        }
        let until = pending.iter().map(|pending| pending.until).fold(next_ping, Instant::min);
        wire.wait(wake, until);
        live.wake.drain();
    }
}

fn ask_join(wire: &mut Wire, pending: &mut Vec<Pending>, refs: u64, room_id: String, reply: Option<Reply<()>>, timing: &Timing) -> io::Result<()> {
    let reference = refs.to_string();
    let frame = json!({"type": "call.join", "ref": reference, "room_id": room_id}).to_string();
    pending.push(Pending { reference, asked: Asked::Join(room_id), until: Instant::now() + timing.answer, reply });
    wire.send(frame)
}

/// The room to take again after `ready`, when the user's seat went with the old socket.
fn rejoin(seat: &Seat) -> Option<String> {
    let room = seat.wanted.clone()?;
    let seated = seat.calls.values().any(|people| people.iter().any(|who| who.user == seat.me));
    (!seated).then_some(room)
}

#[derive(Deserialize)]
struct ReadyFrame {
    seq: i64,
    me: MeFrame,
    #[serde(default)]
    online: Vec<String>,
    #[serde(default)]
    calls: Vec<CallFrame>,
}

#[derive(Deserialize)]
struct MeFrame {
    role: String,
}

#[derive(Deserialize)]
struct CallFrame {
    room_id: String,
    #[serde(default)]
    participants: Vec<ParticipantFrame>,
}

#[derive(Deserialize)]
struct ParticipantFrame {
    user_id: String,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    deafened: bool,
}

fn text<'a>(frame: &'a Value, field: &str) -> Option<&'a str> {
    frame.get(field).and_then(Value::as_str)
}

/// One frame from the server. `Some` ends the connection.
fn heard_frame(ctx: &Context, saved: &Saved, live: &Live, seat: &mut Seat, pending: &mut Vec<Pending>, ready: &mut bool, frame: &Value) -> Option<End> {
    let kind = text(frame, "type").unwrap_or("");
    match kind {
        "ready" => {
            let Ok(heard) = serde_json::from_value::<ReadyFrame>(frame.clone()) else {
                return Some(End::Dropped("the server's ready could not be read".to_owned()));
            };
            ready_heard(ctx, saved, live, seat, heard);
            *ready = true;
            live.connected(true);
            ctx.link(Link::Connected);
        }
        "resync" => return Some(End::Resync),
        "pong" => {}
        "presence" => {
            if let (Some(user), Some(online)) = (text(frame, "user_id"), frame.get("online").and_then(Value::as_bool)) {
                ctx.presence(user, if online { Presence::Online } else { Presence::Offline }, "", true);
            }
        }
        "call.joined" => {
            if let (Some(room), Some(user)) = (text(frame, "room_id"), text(frame, "user_id")) {
                seated(ctx, seat, room, user);
            }
        }
        // Whatever the reason, `media_timeout` and `media_lost` among them.
        "call.left" => {
            if let (Some(room), Some(user)) = (text(frame, "room_id"), text(frame, "user_id")) {
                unseated(ctx, seat, room, user);
            }
        }
        "call.state" => {
            if let (Some(room), Some(user)) = (text(frame, "room_id"), text(frame, "user_id")) {
                let flag = |field| frame.get(field).and_then(Value::as_bool).unwrap_or(false);
                if let Some(who) = seat.calls.get_mut(room).and_then(|people| people.iter_mut().find(|who| who.user == user)) {
                    (who.muted, who.deafened) = (flag("muted"), flag("deafened"));
                    tell(ctx, seat, room);
                }
            }
        }
        "ok" | "error" => answered(seat, pending, frame, kind == "ok"),
        _ if frame.get("seq").is_some() => durable(ctx, seat, kind, frame),
        // A type this client does not know yet, from a newer server.
        _ => {}
    }
    None
}

fn ready_heard(ctx: &Context, saved: &Saved, live: &Live, seat: &mut Seat, heard: ReadyFrame) {
    // Without `since` nothing was replayed: what REST says is the truth now.
    let fresh = seat.since.is_none();
    if fresh {
        live.set_media(super::media_of(&saved.base_url));
    }
    seat.since = Some(heard.seq);
    if heard.me.role != seat.role && matches!(heard.me.role.as_str(), "owner" | "admin" | "member") {
        seat.role = heard.me.role;
        save_role(ctx, &seat.me, &seat.role);
    }
    if fresh || !seat.described {
        if let Ok(members) = members_of(saved) {
            seat.names = members.iter().take(NAMES).map(|member| (member.user_id.clone(), member.display_name.clone())).collect();
            if !seat.described {
                let email = members.iter().find(|member| member.user_id == seat.me).map_or(saved.email.as_str(), |member| member.email.as_str());
                let _ = ctx.describe(&join_host(&saved.base_url), email);
                seat.described = true;
            }
        }
    }
    for user in heard.online.iter().take(NAMES) {
        ctx.presence(user, Presence::Online, "", true);
    }
    let before: Vec<String> = seat.calls.keys().cloned().collect();
    seat.calls = heard
        .calls
        .into_iter()
        .take(ROOMS)
        .map(|call| {
            let people = call.participants.into_iter().take(SEATS).map(|who| Sitting { user: who.user_id, muted: who.muted, deafened: who.deafened });
            (call.room_id, people.collect::<Vec<_>>())
        })
        .filter(|(_, people)| !people.is_empty())
        .collect();
    for room in before.iter().filter(|room| !seat.calls.contains_key(*room)) {
        ctx.occupants(room, Vec::new(), 0);
    }
    for room in seat.calls.keys() {
        tell(ctx, seat, room);
    }
    // Who went offline while the socket was down is never told; the page reads it all again.
    ctx.community_changed();
}

/// Who is in one room's call, with their names, as a voice channel's occupants.
fn tell(ctx: &Context, seat: &Seat, room: &str) {
    let people = seat.calls.get(room).map(Vec::as_slice).unwrap_or_default();
    let occupants = people
        .iter()
        .map(|who| Occupant {
            person: ctx.key(&who.user),
            name: seat.names.get(&who.user).filter(|name| !name.is_empty()).cloned().unwrap_or_else(|| "Someone".to_owned()),
            muted: who.muted,
            deafened: who.deafened,
            streaming: false,
        })
        .collect();
    ctx.occupants(room, occupants, people.len() as u32);
}

/// One seat a person: joining a room leaves any other.
fn seated(ctx: &Context, seat: &mut Seat, room: &str, user: &str) {
    let left: Vec<String> = seat.calls.iter().filter(|(other, people)| other.as_str() != room && people.iter().any(|who| who.user == user)).map(|(other, _)| other.clone()).collect();
    for other in &left {
        unseat(seat, other, user);
        tell(ctx, seat, other);
    }
    if !seat.calls.contains_key(room) && seat.calls.len() >= ROOMS {
        return;
    }
    let people = seat.calls.entry(room.to_owned()).or_default();
    if !people.iter().any(|who| who.user == user) && people.len() < SEATS {
        people.push(Sitting::new(user));
    }
    if user == seat.me {
        seat.wanted = Some(room.to_owned());
    }
    tell(ctx, seat, room);
}

fn unseated(ctx: &Context, seat: &mut Seat, room: &str, user: &str) {
    unseat(seat, room, user);
    // Whatever the reason, the seat is gone and is not taken again on its own.
    if user == seat.me && seat.wanted.as_deref() == Some(room) {
        seat.wanted = None;
    }
    tell(ctx, seat, room);
}

fn unseat(seat: &mut Seat, room: &str, user: &str) {
    if let Some(people) = seat.calls.get_mut(room) {
        people.retain(|who| who.user != user);
        if people.is_empty() {
            seat.calls.remove(room);
        }
    }
}

fn answered(seat: &mut Seat, pending: &mut Vec<Pending>, frame: &Value, ok: bool) {
    let reference = match frame.get("ref") {
        Some(Value::String(reference)) => reference.clone(),
        Some(Value::Number(reference)) => reference.to_string(),
        _ => {
            if !ok {
                eprintln!("chatd: Tiny server: live socket error {}", text(frame, "code").unwrap_or("?"));
            }
            return;
        }
    };
    let Some(at) = pending.iter().position(|pending| pending.reference == reference) else {
        return;
    };
    let pending = pending.remove(at);
    let answer = if ok {
        match &pending.asked {
            Asked::Join(room) => seat.wanted = Some(room.clone()),
            Asked::Leave => seat.wanted = None,
            Asked::Kick => {}
        }
        Ok(())
    } else {
        // A rejoin nobody asked for that fails is not tried again.
        if pending.reply.is_none() {
            seat.wanted = None;
        }
        let code = text(frame, "code").unwrap_or("");
        Err(Refusal::Refused(words(matches!(pending.asked, Asked::Kick), code).to_owned()))
    };
    if let Some(reply) = pending.reply {
        let _ = reply.send(answer);
    }
}

/// Room, member and invite events, in order. One already heard (a replay overlapping what
/// arrived before the drop) is skipped.
fn durable(ctx: &Context, seat: &mut Seat, kind: &str, frame: &Value) {
    let seq = frame.get("seq").and_then(Value::as_i64);
    if let (Some(seq), Some(since)) = (seq, seat.since) {
        if seq <= since {
            return;
        }
    }
    if let Some(seq) = seq {
        seat.since = Some(seq);
    }
    match kind {
        "member.joined" | "member.renamed" | "member.role" => {
            let Some(member) = frame.get("member") else {
                return;
            };
            let (Some(user), name, role) = (text(member, "user_id"), text(member, "display_name"), text(member, "role")) else {
                return;
            };
            if let Some(name) = name {
                if seat.names.len() < NAMES || seat.names.contains_key(user) {
                    seat.names.insert(user.to_owned(), name.to_owned());
                }
            }
            if let Some(role) = role.filter(|role| user == seat.me && *role != seat.role && matches!(*role, "owner" | "admin" | "member")) {
                seat.role = role.to_owned();
                save_role(ctx, &seat.me, &seat.role);
            }
            let rooms: Vec<String> = seat.calls.iter().filter(|(_, people)| people.iter().any(|who| who.user == user)).map(|(room, _)| room.clone()).collect();
            for room in rooms {
                tell(ctx, seat, &room);
            }
        }
        "member.removed" => {
            if let Some(user) = text(frame, "user_id") {
                seat.names.remove(user);
            }
        }
        // The room's `call.left` frames may come before or after this; either way it is empty.
        "room.deleted" => {
            if let Some(room) = text(frame, "room_id") {
                if seat.calls.remove(room).is_some() {
                    ctx.occupants(room, Vec::new(), 0);
                }
                if seat.wanted.as_deref() == Some(room) {
                    seat.wanted = None;
                }
            }
        }
        _ => {}
    }
    if kind.starts_with("room.") || kind.starts_with("member.") || kind.starts_with("invite.") {
        ctx.community_changed();
    }
}

/// The role the session holds decides what the page offers. It changed on the server.
fn save_role(ctx: &Context, me: &str, role: &str) {
    ctx.save_session(|stored| super::with_role(stored, me, role));
    ctx.community_changed();
}

/// How the socket was lost.
enum Lost {
    Closed(u16, String),
    Broken(String),
}

/// Why the handshake failed: the server's HTTP status, or the connection itself.
enum Opened {
    Status(u16),
    Failed(String),
}

struct Wire {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    poll: Pollable,
}

impl Wire {
    /// The bearer goes in the header only, so no address that is logged holds it. Every step
    /// has `limit`: a host that does not answer is not waited on for the system's minute.
    fn open(url: &str, token: &str, limit: Duration) -> Result<Wire, Opened> {
        let failed = |err: &dyn std::fmt::Display| Opened::Failed(net::redact(&err.to_string()));
        let mut request = url.into_client_request().map_err(|err| failed(&err))?;
        let bearer = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| failed(&err))?;
        request.headers_mut().insert("Authorization", bearer);
        request.headers_mut().insert("User-Agent", HeaderValue::from_static("tinyclient"));
        let uri = request.uri();
        let host = uri.host().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_owned();
        let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
        let addrs = (host.as_str(), port).to_socket_addrs().map_err(|err| failed(&err))?;
        let mut tcp = Err(io::Error::new(ErrorKind::NotFound, "the server's name did not resolve"));
        for addr in addrs {
            tcp = TcpStream::connect_timeout(&addr, limit);
            if tcp.is_ok() {
                break;
            }
        }
        let tcp = tcp.map_err(|err| failed(&err))?;
        let _ = tcp.set_nodelay(true);
        tcp.set_read_timeout(Some(limit)).map_err(|err| failed(&err))?;
        tcp.set_write_timeout(Some(limit)).map_err(|err| failed(&err))?;
        let config = WebSocketConfig::default().max_frame_size(Some(FRAME)).max_message_size(Some(FRAME));
        let socket = match tungstenite::client_tls_with_config(request, tcp, Some(config), None) {
            Ok((socket, _)) => socket,
            Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => return Err(Opened::Status(response.status().as_u16())),
            Err(HandshakeError::Failure(err)) => return Err(failed(&err)),
            Err(HandshakeError::Interrupted(_)) => return Err(Opened::Failed("the server did not finish the handshake".to_owned())),
        };
        let tcp = match socket.get_ref() {
            MaybeTlsStream::NativeTls(tls) => tls.get_ref(),
            MaybeTlsStream::Plain(tcp) => tcp,
            _ => return Err(Opened::Failed("unexpected stream".to_owned())),
        };
        tcp.set_nonblocking(true).map_err(|err| failed(&err))?;
        let poll = Pollable::from_tcp(tcp);
        Ok(Wire { socket, poll })
    }

    /// The next text frame already here, or `None` when there is nothing more to read now.
    fn read(&mut self) -> Result<Option<String>, Lost> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(Some(text.as_str().to_owned())),
                Ok(Message::Close(frame)) => {
                    let (code, reason) = frame.map_or((0, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
                    return Err(Lost::Closed(code, reason));
                }
                // WebSocket pings are answered by tungstenite on the next read or write.
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(Lost::Broken("the server closed the connection".to_owned()))
                }
                Err(err) => return Err(Lost::Broken(net::redact(&err.to_string()))),
            }
        }
    }

    fn send(&mut self, text: String) -> io::Result<()> {
        let mut sent = self.socket.send(Message::text(text));
        // A full socket buffer keeps the frame queued inside tungstenite; flush until it is out.
        for _ in 0..50 {
            match sent {
                Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                    wait_sockets(Instant::now() + Duration::from_millis(200), &[(self.poll, POLL_OUT)]);
                    sent = self.socket.flush();
                }
                other => return other.map_err(io::Error::other),
            }
        }
        Err(io::Error::new(ErrorKind::TimedOut, "the server is not taking what is sent"))
    }

    /// Sleeps until the socket has something, the wake socket is written to, or `until`.
    fn wait(&self, wake: Pollable, until: Instant) {
        wait_sockets(until, &[(self.poll, POLL_IN), (wake, POLL_IN)]);
    }

    /// Says goodbye. Best effort: the adapter is going either way.
    fn close(&mut self) {
        let _ = self.socket.close(None);
        let _ = self.socket.flush();
    }
}

#[cfg(test)]
mod tests;
