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
//! On a server whose `/health` says its audio is up, a join carries audio: chatd's call starts
//! mediad, mediad makes the offer, and it goes out as `call.join` `sdp`. The `call.answer` and
//! every `call.slots` go back to mediad (`Context::tiny_answer`, `tiny_slots`); str0m and every
//! packet stay inside mediad. Mute and deafen go out as `call.state`, and hanging up as
//! `call.leave`. After a server restart the seat is taken again with a fresh offer. On a server
//! without audio a join only takes a seat, and nothing here touches mediad.

use super::super::discord::gateway::{wait_sockets, Pollable, WakePair, POLL_IN, POLL_OUT};
use super::super::{net, Context, Refusal, Reply, ReconnectWait, PANIC_PAUSE, RECONNECTING};
use crate::hub::lock;
use super::{members_of, Saved};
use protocol::{join_host, CallFailure, Cue, Link, MediaRequest, Occupant, Participant, Presence, TinyE2ee, TinyOffer, TinySeat, TinySlot};
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
/// People in one room's call, the server's default cap (`TINYSERVER_CALL_ROOM_MAX`). The offer
/// has a receive slot for each of the others.
const ROOM_CAP: u16 = 12;

const OFFLINE: &str = "Tiny server isn't connected. Try again in a moment.";
const LOST: &str = "The connection to the server dropped. Try again.";
const NO_ANSWER: &str = "The server didn't answer. Try again.";
const BUSY: &str = "Wait for the last request to finish.";
const AUDIO_DOWN: &str = "This server's audio is down. Try again later.";
const MEDIA_TIMEOUT: &str = "Call audio couldn't connect to the server. A VPN or firewall may be blocking it.";
const MEDIA_LOST: &str = "The server stopped hearing this call's audio, so you've left the call.";
const KICKED: &str = "You were removed from the call.";
const ROOM_GONE: &str = "The room was deleted, so the call ended.";
const ELSEWHERE: &str = "You joined a call on another device.";
const BANNED: &str = "You were banned from this server.";
const TIMED_OUT: &str = "You were timed out on this server, so you've left the call.";
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
    /// The call in this room ended in chatd (hung up, or failed): leave it on the server if
    /// we are still in it there.
    Hangup { room_id: String },
    /// Our mute and deafen changed in a call with audio.
    State { muted: bool, deafened: bool },
    /// Server-mute or server-deafen someone in a room's call, or undo it. `None` leaves it.
    Moderate { room_id: String, user_id: String, server_muted: Option<bool>, server_deafened: Option<bool>, reply: Reply<()> },
    /// Move someone in a call to another room.
    Move { user_id: String, room_id: String, reply: Reply<()> },
    /// The priority key is held, or let go: told to the server with our mute and deafen.
    Priority { on: bool },
    /// A `call.e2ee` frame from mediad, for the server as it is.
    E2ee { frame: String },
}

impl Ask {
    fn into_reply(self) -> Option<Reply<()>> {
        match self {
            Ask::Join { reply, .. } | Ask::Leave { reply } | Ask::Kick { reply, .. } | Ask::Moderate { reply, .. } | Ask::Move { reply, .. } => Some(reply),
            Ask::Look | Ask::Hangup { .. } | Ask::State { .. } | Ask::Priority { .. } | Ask::E2ee { .. } => None,
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
    /// What the socket thread chose to wait for, so tests read the choices, not a clock.
    #[cfg(test)]
    did: Mutex<Vec<Did>>,
}

/// One timing choice of the socket thread.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Did {
    /// A connection is up, and its first ping is due a ping interval from `at`.
    Opened { at: Instant },
    /// A ping went out at `sent`, due at `due`.
    Pinged { due: Instant, sent: Instant },
    /// The pause asked for before the next connection.
    Paused(Duration),
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
        Ok(Live {
            wake: WakePair::new()?,
            inner: Mutex::new(Inner::default()),
            #[cfg(test)]
            did: Mutex::default(),
        })
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
            // A hang-up while down needs nothing sent: the seat is not taken again without the
            // call (`rejoin`), and the server lets it go after its grace.
            ask if !inner.connected => ask.refuse(OFFLINE),
            ask @ Ask::Join { .. } if inner.media == "down" => ask.refuse(AUDIO_DOWN),
            ask if inner.queued.len() >= QUEUED => ask.refuse(BUSY),
            ask => {
                inner.queued.push_back(ask);
                drop(inner);
                self.wake.signal();
            }
        }
    }

    #[cfg(test)]
    fn note(&self, did: Did) {
        lock(&self.did).push(did);
    }

    #[cfg(not(test))]
    fn note(&self, _: Did) {}

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
    /// The server and user, for what the page reads of each seat (`manage::Known::seats`).
    saved: Saved,
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
    /// The media session of our seat, from `call.answer`. `call.slots` for any other is stale.
    call_id: Option<String>,
    /// Who is speaking in our room's call, from `call.speaking`.
    speaking: Vec<String>,
    /// The room our seat was last taken in. A move carries the call from there.
    last: Option<String>,
    /// The server moved us here (`call.moved`): join it once the frame is read.
    move_to: Option<String>,
    /// The server stopped hearing our media (`media_lost`) on a socket that is still up: take
    /// the seat again with a fresh offer once the frame is read, as after a restart.
    recover: Option<String>,
    /// When the call last came back that way, so a link that keeps failing is let go.
    recovered: Vec<Instant>,
    /// Our own mute, deafen and priority, as last told to the server.
    muted: bool,
    deafened: bool,
    priority: bool,
    /// Who holds priority speech in our room's call, as mediad was last told.
    ducking: Vec<String>,
    /// The server encrypts calls end to end (`e2ee_calls`), read at each fresh `ready`.
    e2ee: bool,
    /// Everyone in our room's call, as mediad was last told: whom its MLS group may hold.
    rostered: Vec<String>,
}

/// Someone in a call: the mute and deafen they set, and what the server says of them.
#[derive(Clone, Default, PartialEq)]
struct Sitting {
    user: String,
    muted: bool,
    deafened: bool,
    server_muted: bool,
    server_deafened: bool,
    suppressed: bool,
    priority: bool,
}

impl Sitting {
    fn new(user: &str) -> Sitting {
        Sitting { user: user.to_owned(), ..Sitting::default() }
    }

    /// The flags a participant object carries, on the frame itself or in its `participant`.
    /// An older server sends none: all off.
    fn heard(&mut self, frame: &Value) {
        let from = frame.get("participant").filter(|inner| inner.is_object()).unwrap_or(frame);
        let flag = |field| from.get(field).and_then(Value::as_bool).unwrap_or(false);
        (self.muted, self.deafened) = (flag("muted"), flag("deafened"));
        (self.server_muted, self.server_deafened, self.suppressed, self.priority) = (flag("server_muted"), flag("server_deafened"), flag("suppressed"), flag("priority"));
    }

    /// What the server enforces changed: the page reads it again.
    fn moderated(&self, before: &Sitting) -> bool {
        (self.server_muted, self.server_deafened, self.suppressed) != (before.server_muted, before.server_deafened, before.suppressed)
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

/// What a refused call request says. `someone`: the request was about another person.
fn words(someone: bool, code: &str) -> &'static str {
    match code {
        "full" | "room_full" => "That room is full.",
        "server_full" => "Every seat on this server is taken. Try again later.",
        "banned" => "You're banned from this server.",
        "timed_out" => "You're timed out on this server, so you can't join calls for now.",
        "hierarchy" => super::HIERARCHY,
        "not_found" if someone => "They're not in that room's call.",
        "not_found" => "That room is gone.",
        "forbidden" => "You can't do that.",
        "slow_down" => "Too many tries. Wait a moment, then try again.",
        "unavailable" => "The server had a problem. Try again in a moment.",
        "unknown_type" => "This server doesn't take calls yet.",
        "bad_frame" => "The server couldn't read that request.",
        "bad_sdp" => "The server couldn't use tinyclient's audio setup.",
        "media_unavailable" => "This server's audio isn't available. Try again later.",
        "e2ee_required" => "This room only takes calls encrypted end to end, and this one couldn't be. Try again.",
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
        saved: saved.clone(),
        since: None,
        me: saved.user_id.clone(),
        role: saved.role.clone(),
        names: HashMap::new(),
        calls: BTreeMap::new(),
        wanted: None,
        described: false,
        call_id: None,
        speaking: Vec::new(),
        last: None,
        move_to: None,
        recover: None,
        recovered: Vec::new(),
        muted: false,
        deafened: false,
        priority: false,
        ducking: Vec::new(),
        e2ee: false,
        rostered: Vec::new(),
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
        seat.speaking.clear();
        seat.move_to = None;
        ctx.hub.forget_live(&ctx.account);
        super::manage::remember(saved, |known| known.seats.clear());
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
        live.note(Did::Paused(wait));
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
    /// A join, and whether it carries audio.
    Join(String, bool),
    Leave,
    Kick,
    State,
    /// Server mute or deafen, or a move: about someone else.
    Moderate,
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
            // A join with audio that never got its answer has a mediad waiting for nothing.
            if let Asked::Join(room, true) = &pending.asked {
                ctx.call_over(room);
            }
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
    let opened = Instant::now();
    live.note(Did::Opened { at: opened });
    let mut next_ping = opened + timing.ping;
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
                    if ready && frame.get("type").and_then(Value::as_str) == Some("ready") {
                        if let Some(room) = rejoin(seat) {
                            refs += 1;
                            if let Err(err) = rejoin_with(ctx, saved, live, seat, wire, pending, refs, room, timing) {
                                return End::Dropped(err.to_string());
                            }
                        }
                    }
                    if let Some(room) = seat.move_to.take() {
                        refs += 1;
                        if let Err(err) = move_with(ctx, saved, live, seat, wire, pending, refs, room, timing) {
                            return End::Dropped(err.to_string());
                        }
                    }
                    if let Some(room) = seat.recover.take() {
                        refs += 1;
                        eprintln!("chatd: Tiny server: the call's media was lost; joining again");
                        if let Err(err) = rejoin_with(ctx, saved, live, seat, wire, pending, refs, room, timing) {
                            return End::Dropped(err.to_string());
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
                    Ask::Join { room_id, reply } if live.seats_only() => ask_join(wire, pending, refs, room_id, None, Some(reply), timing),
                    Ask::Join { room_id, reply } => match offer(ctx, saved, seat, &room_id) {
                        Ok(sdp) => ask_join(wire, pending, refs, room_id, Some(sdp), Some(reply), timing),
                        Err(words) => {
                            let _ = reply.send(Err(Refusal::Refused(words)));
                            Ok(())
                        }
                    },
                    Ask::Leave { reply } => {
                        // Leaving from the page ends the call's audio here too.
                        if let Some(room) = seat.wanted.clone() {
                            ctx.call_over(&room);
                        }
                        let reference = refs.to_string();
                        pending.push(Pending { reference: reference.clone(), asked: Asked::Leave, until: Instant::now() + timing.answer, reply: Some(reply) });
                        wire.send(json!({"type": "call.leave", "ref": reference}).to_string())
                    }
                    Ask::Hangup { room_id } => {
                        let joining = pending.iter().any(|waiting| matches!(&waiting.asked, Asked::Join(room, _) if *room == room_id));
                        if seat.wanted.as_deref() == Some(room_id.as_str()) {
                            seat.wanted = None;
                        }
                        if seated_in(seat, &room_id) || joining {
                            let reference = refs.to_string();
                            pending.push(Pending { reference: reference.clone(), asked: Asked::Leave, until: Instant::now() + timing.answer, reply: None });
                            wire.send(json!({"type": "call.leave", "ref": reference}).to_string())
                        } else {
                            Ok(())
                        }
                    }
                    Ask::State { muted, deafened } => {
                        (seat.muted, seat.deafened) = (muted, deafened);
                        state(wire, pending, refs, seat, timing)
                    }
                    Ask::Priority { on } if on != seat.priority => {
                        seat.priority = on;
                        state(wire, pending, refs, seat, timing)
                    }
                    Ask::Priority { .. } => Ok(()),
                    // Only for the seat that has media; one from a call that has gone is stale.
                    Ask::E2ee { frame } if seat.call_id.is_some() => wire.send(frame),
                    Ask::E2ee { .. } => Ok(()),
                    Ask::Moderate { room_id, user_id, server_muted, server_deafened, reply } => {
                        let reference = refs.to_string();
                        pending.push(Pending { reference: reference.clone(), asked: Asked::Moderate, until: Instant::now() + timing.answer, reply: Some(reply) });
                        let mut frame = json!({"type": "call.moderate", "ref": reference, "room_id": room_id, "user_id": user_id});
                        if let Some(muted) = server_muted {
                            frame["server_muted"] = json!(muted);
                        }
                        if let Some(deafened) = server_deafened {
                            frame["server_deafened"] = json!(deafened);
                        }
                        wire.send(frame.to_string())
                    }
                    Ask::Move { user_id, room_id, reply } => {
                        let reference = refs.to_string();
                        pending.push(Pending { reference: reference.clone(), asked: Asked::Moderate, until: Instant::now() + timing.answer, reply: Some(reply) });
                        wire.send(json!({"type": "call.move", "ref": reference, "user_id": user_id, "room_id": room_id}).to_string())
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
            if let Asked::Join(room, true) = &late.asked {
                if late.reply.is_some() {
                    ctx.call_over(room);
                } else {
                    ctx.call_failed(room, CallFailure::Lost, NO_ANSWER);
                }
            }
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
            live.note(Did::Pinged { due: next_ping, sent: now });
            heard = false;
            next_ping = now + timing.ping;
        }
        let until = pending.iter().map(|pending| pending.until).fold(next_ping, Instant::min);
        wire.wait(wake, until);
        live.wake.drain();
    }
}

/// Our mute, deafen and priority, in a call with audio. `priority` goes only while held, so an
/// older server never sees the field.
fn state(wire: &mut Wire, pending: &mut Vec<Pending>, refs: u64, seat: &Seat, timing: &Timing) -> io::Result<()> {
    if seat.call_id.is_none() {
        return Ok(());
    }
    let reference = refs.to_string();
    pending.push(Pending { reference: reference.clone(), asked: Asked::State, until: Instant::now() + timing.answer, reply: None });
    let mut frame = json!({"type": "call.state", "ref": reference, "muted": seat.muted, "deafened": seat.deafened});
    if seat.priority {
        frame["priority"] = json!(true);
    }
    wire.send(frame.to_string())
}

/// The server moved us to `room` (`call.moved`): join it with no confirmation. A call with
/// audio goes along, with its mediad, devices, mute and deafen, and a fresh offer.
#[allow(clippy::too_many_arguments)]
fn move_with(ctx: &Context, saved: &Saved, live: &Live, seat: &mut Seat, wire: &mut Wire, pending: &mut Vec<Pending>, refs: u64, room: String, timing: &Timing) -> io::Result<()> {
    let from = seat.last.clone().or_else(|| seat.wanted.clone());
    seat.call_id = None;
    seat.speaking.clear();
    if live.seats_only() {
        seat.wanted = Some(room.clone());
        return ask_join(wire, pending, refs, room, None, None, timing);
    }
    let carried = from.as_deref().is_some_and(|from| from == room || ctx.tiny_moved(from, &room));
    if !carried {
        // No call of ours was in that room here: there is nothing to carry.
        seat.wanted = None;
        return Ok(());
    }
    seat.wanted = Some(room.clone());
    match offer(ctx, saved, seat, &room) {
        Ok(sdp) => ask_join(wire, pending, refs, room, Some(sdp), None, timing),
        Err(words) => {
            seat.wanted = None;
            ctx.call_failed(&room, CallFailure::Lost, &words);
            Ok(())
        }
    }
}

/// The offer, and whether it comes with end-to-end encryption.
type Offered = (String, bool);

/// The version of the server's `call.e2ee` mediad speaks.
const E2EE_VERSION: u16 = 1;

#[allow(clippy::too_many_arguments)]
fn ask_join(wire: &mut Wire, pending: &mut Vec<Pending>, refs: u64, room_id: String, sdp: Option<Offered>, reply: Option<Reply<()>>, timing: &Timing) -> io::Result<()> {
    let reference = refs.to_string();
    let mut frame = json!({"type": "call.join", "ref": reference, "room_id": room_id});
    let media = sdp.is_some();
    if let Some((sdp, e2ee)) = sdp {
        frame["sdp"] = json!(sdp);
        // An older server ignores the field; a newer one says after its answer what the call is.
        if e2ee {
            frame["e2ee"] = json!(E2EE_VERSION);
        }
    }
    pending.push(Pending { reference, asked: Asked::Join(room_id, media), until: Instant::now() + timing.answer, reply });
    wire.send(frame.to_string())
}

/// mediad's offer for a room, with mediad started for it, then what the room asks of the call.
/// On a server that encrypts calls end to end, the offer says so, with the room that names the
/// group and where this account keeps its identity key; mediad then hears who is in the call.
fn offer(ctx: &Context, saved: &Saved, seat: &mut Seat, room: &str) -> Result<Offered, String> {
    let host = tungstenite::http::Uri::try_from(saved.base_url.as_str()).ok().and_then(|uri| uri.host().map(|host| host.trim_matches(['[', ']']).to_owned()));
    let identity = || crate::verified::identity_path(&ctx.account).to_string_lossy().into_owned();
    let e2ee = seat.e2ee.then(|| TinyE2ee { room_id: room.to_owned(), identity: identity() });
    let offered = e2ee.is_some();
    let offer = TinyOffer { user_id: seat.me.clone(), host: host.unwrap_or_default(), slots: ROOM_CAP - 1, muted: false, deafened: false, e2ee };
    let sdp = ctx.tiny_offer(room, offer)?;
    let (kbps, push_to_talk) = super::manage::room_audio(saved, room);
    ctx.tiny_room(room, kbps, push_to_talk);
    seat.rostered.clear();
    roster(ctx, seat, room);
    Ok((sdp, offered))
}

/// The room to take again after `ready`, when the user's seat went with the old socket.
fn rejoin(seat: &Seat) -> Option<String> {
    let room = seat.wanted.clone()?;
    let seated = seat.calls.values().any(|people| people.iter().any(|who| who.user == seat.me));
    (!seated).then_some(room)
}

/// Takes the seat again. With audio, only while chatd still holds the call, and with a fresh
/// offer: the old media session went with the server.
#[allow(clippy::too_many_arguments)]
fn rejoin_with(ctx: &Context, saved: &Saved, live: &Live, seat: &mut Seat, wire: &mut Wire, pending: &mut Vec<Pending>, refs: u64, room: String, timing: &Timing) -> io::Result<()> {
    if live.seats_only() {
        return ask_join(wire, pending, refs, room, None, None, timing);
    }
    if !ctx.in_call(&room) {
        seat.wanted = None;
        return Ok(());
    }
    match offer(ctx, saved, seat, &room) {
        Ok(sdp) => ask_join(wire, pending, refs, room, Some(sdp), None, timing),
        Err(words) => {
            seat.wanted = None;
            ctx.call_failed(&room, CallFailure::Lost, &words);
            Ok(())
        }
    }
}

fn seated_in(seat: &Seat, room: &str) -> bool {
    seat.calls.get(room).is_some_and(|people| people.iter().any(|who| who.user == seat.me))
}

#[derive(Deserialize)]
struct ReadyFrame {
    seq: i64,
    #[serde(default)]
    me: MeFrame,
    #[serde(default, deserialize_with = "protocol::or_default")]
    online: Vec<String>,
    /// Read row by row (`calls_of`): one call this client cannot read leaves the others.
    #[serde(default)]
    calls: Value,
}

#[derive(Deserialize, Default)]
struct MeFrame {
    #[serde(default, deserialize_with = "protocol::text_or_null")]
    role: String,
}

#[derive(Deserialize)]
struct CallFrame {
    room_id: String,
    #[serde(default)]
    participants: Value,
}

#[derive(Deserialize)]
struct ParticipantFrame {
    user_id: String,
    #[serde(default, deserialize_with = "protocol::or_default")]
    muted: bool,
    #[serde(default, deserialize_with = "protocol::or_default")]
    deafened: bool,
    #[serde(default, deserialize_with = "protocol::or_default")]
    server_muted: bool,
    #[serde(default, deserialize_with = "protocol::or_default")]
    server_deafened: bool,
    #[serde(default, deserialize_with = "protocol::or_default")]
    suppressed: bool,
    #[serde(default, deserialize_with = "protocol::or_default")]
    priority: bool,
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
                seated(ctx, seat, room, user, frame);
                duck(ctx, seat, room);
            }
        }
        // Whatever the reason, `media_timeout` and `media_lost` among them.
        "call.left" => {
            if let (Some(room), Some(user)) = (text(frame, "room_id"), text(frame, "user_id")) {
                unseated(ctx, seat, room, user);
                if user == seat.me {
                    left(ctx, seat, room, text(frame, "reason").unwrap_or(""));
                }
                duck(ctx, seat, room);
            }
        }
        // A moderator moved us. Joining the new room needs the socket, so it happens once this
        // frame is read (`move_with`).
        "call.moved" => {
            if let Some(room) = text(frame, "room_id").filter(|room| !room.is_empty()) {
                seat.move_to = Some(room.to_owned());
            }
        }
        "call.answer" => answered(ctx, seat, pending, frame, true),
        // The room's end-to-end encryption, for mediad alone: str0m's keys and the group's
        // never leave it. Only for the seat that has media, in the order it came.
        "call.e2ee" => {
            if let (true, Some(room)) = (seat.call_id.is_some(), seat.wanted.clone()) {
                ctx.tell_media(&room, MediaRequest::TinyE2ee { frame: frame.to_string() });
            }
        }
        "call.slots" => {
            let current = seat.call_id.is_some() && text(frame, "call_id") == seat.call_id.as_deref();
            if let (true, Some(room)) = (current, seat.wanted.clone()) {
                let audio = frame.get("audio").and_then(Value::as_array).into_iter().flatten().take(SEATS);
                let audio = audio.filter_map(|slot| Some(TinySlot { mid: text(slot, "mid")?.to_owned(), user_id: text(slot, "user_id")?.to_owned() }));
                ctx.tiny_slots(&room, audio.collect());
            }
        }
        "call.speaking" => {
            if let Some(room) = text(frame, "room_id") {
                let users = frame.get("user_ids").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).take(SEATS);
                seat.speaking = users.map(str::to_owned).collect();
                tell(ctx, seat, room);
            }
        }
        "call.state" => {
            if let (Some(room), Some(user)) = (text(frame, "room_id"), text(frame, "user_id")) {
                if let Some(who) = seat.calls.get_mut(room).and_then(|people| people.iter_mut().find(|who| who.user == user)) {
                    let before = who.clone();
                    who.heard(frame);
                    let moderated = who.moderated(&before);
                    tell(ctx, seat, room);
                    duck(ctx, seat, room);
                    if moderated {
                        ctx.community_changed();
                    }
                }
            }
        }
        "ok" | "error" => answered(ctx, seat, pending, frame, kind == "ok"),
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
        seat.e2ee = super::manage::capabilities_of(saved).has(protocol::TINY_FEATURE_E2EE_CALLS);
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
                let email = members.iter().find(|member| member.user_id == seat.me).map_or(saved.email.as_deref().unwrap_or(""), |member| member.email.as_str());
                let _ = ctx.describe(&join_host(&saved.base_url), email);
                seat.described = true;
            }
        }
    }
    for user in heard.online.iter().take(NAMES) {
        ctx.presence(user, Presence::Online, "", true);
    }
    let before: Vec<String> = seat.calls.keys().cloned().collect();
    let calls: Vec<CallFrame> = super::rows(heard.calls, "calls").unwrap_or_default();
    seat.calls = calls
        .into_iter()
        .take(ROOMS)
        .map(|call| {
            let people: Vec<ParticipantFrame> = super::rows(call.participants, "call seats").unwrap_or_default();
            let people = people.into_iter().take(SEATS).map(|who| Sitting {
                user: who.user_id,
                muted: who.muted,
                deafened: who.deafened,
                server_muted: who.server_muted,
                server_deafened: who.server_deafened,
                suppressed: who.suppressed,
                priority: who.priority,
            });
            (call.room_id, people.collect::<Vec<_>>())
        })
        .filter(|(_, people)| !people.is_empty())
        .collect();
    for room in before.iter().filter(|room| !seat.calls.contains_key(*room)) {
        ctx.occupants(room, Vec::new(), 0);
    }
    super::manage::remember(saved, |known| known.seats.clear());
    for room in seat.calls.keys() {
        tell(ctx, seat, room);
    }
    let ours: Vec<String> = seat.calls.keys().filter(|room| ctx.in_call(room)).cloned().collect();
    for room in ours {
        duck(ctx, seat, &room);
    }
    // Who went offline while the socket was down is never told; the page reads it all again.
    ctx.community_changed();
}

/// Who is in one room's call, with their names, as a voice channel's occupants. A call chatd
/// holds in that room gets everyone but us as its roster.
fn tell(ctx: &Context, seat: &Seat, room: &str) {
    let people = seat.calls.get(room).map(Vec::as_slice).unwrap_or_default();
    let name = |user: &str| seat.names.get(user).filter(|name| !name.is_empty()).cloned().unwrap_or_else(|| "Someone".to_owned());
    if ctx.in_call(room) {
        let others = people.iter().filter(|who| who.user != seat.me);
        let roster = others.map(|who| Participant {
            id: who.user.clone(),
            name: name(&who.user),
            speaking: false,
            muted: who.muted,
            deafened: who.deafened,
            server_muted: who.server_muted,
            server_deafened: who.server_deafened,
            suppressed: who.suppressed,
            priority: who.priority,
        });
        ctx.participants(room, roster.collect());
    }
    // The page reads each seat's moderation from here; the hub's occupants carry only mute.
    let flagged: Vec<TinySeat> = people
        .iter()
        .map(|who| TinySeat {
            user_id: who.user.clone(),
            muted: who.muted,
            deafened: who.deafened,
            speaking: seat.speaking.contains(&who.user),
            server_muted: who.server_muted,
            server_deafened: who.server_deafened,
            suppressed: who.suppressed,
            priority: who.priority,
        })
        .collect();
    super::manage::remember(&seat.saved, |known| {
        if flagged.is_empty() {
            known.seats.remove(room);
        } else {
            known.seats.insert(room.to_owned(), flagged);
        }
    });
    let occupants = people
        .iter()
        .map(|who| Occupant {
            person: ctx.key(&who.user),
            name: name(&who.user),
            muted: who.muted,
            deafened: who.deafened,
            streaming: false,
            speaking: seat.speaking.contains(&who.user),
        })
        .collect();
    ctx.occupants(room, occupants, people.len() as u32);
}

/// Everyone in our room's call, us included, told to mediad when it changes: its MLS group
/// takes in only people the call has, and names its members by them.
fn roster(ctx: &Context, seat: &mut Seat, room: &str) {
    if !seat.e2ee || !ctx.in_call(room) {
        return;
    }
    let people: Vec<String> = seat.calls.get(room).into_iter().flatten().map(|who| who.user.clone()).collect();
    if people != seat.rostered {
        seat.rostered = people.clone();
        ctx.tell_media(room, MediaRequest::TinyRoster { user_ids: people });
    }
}

/// Who in our room's call holds priority speech, told to mediad when it changes, so it ducks
/// everyone else while they talk.
fn duck(ctx: &Context, seat: &mut Seat, room: &str) {
    roster(ctx, seat, room);
    if !ctx.in_call(room) {
        return;
    }
    let holders: Vec<String> = seat.calls.get(room).into_iter().flatten().filter(|who| who.priority && who.user != seat.me).map(|who| who.user.clone()).collect();
    if holders != seat.ducking {
        seat.ducking = holders.clone();
        ctx.tell_media(room, MediaRequest::Priority { user_ids: holders });
    }
}

/// One seat a person: joining a room leaves any other.
fn seated(ctx: &Context, seat: &mut Seat, room: &str, user: &str, frame: &Value) {
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
        let mut who = Sitting::new(user);
        who.heard(frame);
        people.push(who);
    }
    if user == seat.me {
        seat.wanted = Some(room.to_owned());
        seat.last = Some(room.to_owned());
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

/// We left a room's call on the server. If chatd's call is in it, it ends: in words when
/// something took it from us, quietly when we left it ourselves somewhere.
fn left(ctx: &Context, seat: &mut Seat, room: &str, reason: &str) {
    // Moved by a moderator: `call.moved` carries the call on to the new room, before this
    // frame or after it.
    if reason == "moved" {
        return;
    }
    if seat.wanted.is_none() || seat.wanted.as_deref() == Some(room) {
        seat.call_id = None;
        seat.speaking.clear();
    }
    if !ctx.in_call(room) {
        return;
    }
    // A network blip the server noticed before we did: the call joins again by itself, a few
    // times, before it is given up in words.
    if reason == "media_lost" && seat.wanted.as_deref() == Some(room) && recovering(&mut seat.recovered, Instant::now()) {
        seat.recover = Some(room.to_owned());
        return;
    }
    let words = match reason {
        "media_timeout" => Some((CallFailure::Lost, MEDIA_TIMEOUT)),
        "media_lost" => Some((CallFailure::Lost, MEDIA_LOST)),
        "kicked" => Some((CallFailure::Refused, KICKED)),
        "room_deleted" => Some((CallFailure::Refused, ROOM_GONE)),
        "joined_elsewhere" => Some((CallFailure::Refused, ELSEWHERE)),
        "banned" => Some((CallFailure::Refused, BANNED)),
        "timed_out" => Some((CallFailure::Refused, TIMED_OUT)),
        _ => None,
    };
    // Put out by someone: the cue that says so plays out as the call ends.
    if matches!(reason, "kicked" | "banned" | "timed_out" | "room_deleted") {
        ctx.tell_media(room, MediaRequest::Cue { cue: Cue::Disconnected });
    }
    match words {
        // chatd then has the adapter hang up, which sends nothing: we are not in the room.
        Some((reason, detail)) => ctx.call_failed(room, reason, detail),
        None => drop(ctx.call_over(room)),
    }
}

/// Times a call whose media the server lost joins again by itself within [`RECOVER_WINDOW`].
const RECOVERIES: usize = 3;
const RECOVER_WINDOW: Duration = Duration::from_secs(120);

/// Whether a call that lost its media may join again now, counting this time if so.
fn recovering(recovered: &mut Vec<Instant>, now: Instant) -> bool {
    recovered.retain(|at| now.saturating_duration_since(*at) < RECOVER_WINDOW);
    if recovered.len() >= RECOVERIES {
        return false;
    }
    recovered.push(now);
    true
}

fn answered(ctx: &Context, seat: &mut Seat, pending: &mut Vec<Pending>, frame: &Value, ok: bool) {
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
            Asked::Join(room, media) => {
                seat.wanted = Some(room.clone());
                seat.last = Some(room.clone());
                // With audio the answer is `call.answer`, and it goes to mediad first.
                if let (true, Some(sdp)) = (*media, text(frame, "sdp")) {
                    seat.call_id = text(frame, "call_id").map(str::to_owned);
                    ctx.tiny_answer(room, sdp.to_owned());
                }
            }
            Asked::Leave => {
                seat.wanted = None;
                seat.call_id = None;
            }
            Asked::Kick | Asked::State | Asked::Moderate => {}
        }
        Ok(())
    } else {
        let code = text(frame, "code").unwrap_or("");
        let said = words(matches!(pending.asked, Asked::Kick | Asked::Moderate), code);
        if let Asked::Join(room, true) = &pending.asked {
            // A join with audio the server refused: its mediad goes. A rejoin that fails is
            // shown as the end of the call; the user asked for the first join, and is told.
            if pending.reply.is_none() {
                ctx.call_failed(room, CallFailure::Refused, said);
            } else {
                ctx.call_over(room);
            }
        }
        if matches!(pending.asked, Asked::State) {
            eprintln!("chatd: Tiny server: call.state refused ({code})");
        }
        // A rejoin nobody asked for that fails is not tried again.
        if pending.reply.is_none() {
            seat.wanted = None;
        }
        Err(Refusal::Refused(said.to_owned()))
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
        "member.joined" | "member.renamed" | "member.role" | "member.roles" => {
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
        "member.removed" | "member.banned" => {
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
    // Anything the page shows: the server, its roles, categories and order, rooms, members
    // and invites. `role.*` and `member.roles` can also change what this account may do; the
    // page's next read learns it.
    let shown = ["server.", "role.", "roles.", "category.", "layout.", "room.", "member.", "invite."];
    if shown.iter().any(|prefix| kind.starts_with(prefix)) {
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
