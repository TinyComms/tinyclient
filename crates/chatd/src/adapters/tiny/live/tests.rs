//! The live socket against a local fake: a WebSocket at `/v1/live` that each test scripts, and
//! the REST routes the socket reads beside it. Timings are shortened through `Timing`.

use super::*;
use crate::adapters::{local, Adapter};
use crate::hub::Hub;
use crate::store::Store;
use protocol::{AccountKey, Change, Key, Network};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;

const ME: &str = "11111111-1111-1111-1111-111111111111";
const BEA: &str = "22222222-2222-2222-2222-222222222222";
const ROOM: &str = "33333333-3333-3333-3333-333333333333";
const OTHER_ROOM: &str = "44444444-4444-4444-4444-444444444444";

/// Short enough that a test is quick, long enough that a loaded machine keeps the order.
const QUICK: Timing = Timing {
    ping: Duration::from_secs(30),
    answer: Duration::from_secs(2),
    first: Duration::from_millis(40),
    cap: Duration::from_millis(160),
    held: Duration::from_secs(60),
    absent: Duration::from_millis(200),
};

/// What the fake saw.
#[derive(Debug)]
enum Seen {
    Live { since: Option<i64>, bearer: String, at: Instant },
    Rest(String),
    Ping(Instant),
}

/// The server's end of one live socket.
struct Conn {
    ws: WebSocket<TcpStream>,
    seen: Sender<Seen>,
    /// Pings are answered unless a test wants a silent server.
    pong: bool,
}

impl Conn {
    fn send(&mut self, frame: Value) {
        let mut sent = self.ws.send(Message::text(frame.to_string()));
        for _ in 0..200 {
            match sent {
                Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    sent = self.ws.flush();
                }
                _ => return,
            }
        }
    }

    fn close(mut self, code: u16) {
        let _ = self.ws.close(Some(CloseFrame { code: CloseCode::from(code), reason: "".into() }));
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            match self.ws.read() {
                Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    /// The client's next frame that is not a ping, answering pings on the way. `None` once
    /// `wait` passes or the client went.
    fn next(&mut self, wait: Duration) -> Option<Value> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(text.as_str()).expect("the client sends JSON");
                    if frame["type"] == "ping" {
                        let _ = self.seen.send(Seen::Ping(Instant::now()));
                        if self.pong {
                            self.send(json!({"type": "pong"}));
                        }
                        continue;
                    }
                    return Some(frame);
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => return None,
            }
        }
        None
    }

    /// Holds the socket open for `wait`, answering pings. False if the client went first.
    fn idle(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            if self.next(left).is_none() && Instant::now() < deadline {
                return false;
            }
        }
    }
}

struct Fake {
    base: String,
    seen: Receiver<Seen>,
    /// What `connected` read past on its way to a connection.
    kept: std::cell::RefCell<Vec<Seen>>,
}

impl Fake {
    /// The next live connection the fake accepted.
    fn connected(&self, within: Duration) -> Option<(Option<i64>, String, Instant)> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.seen.recv_timeout(left) {
                Ok(Seen::Live { since, bearer, at }) => return Some((since, bearer, at)),
                Ok(other) => self.kept.borrow_mut().push(other),
                Err(_) => return None,
            }
        }
    }

    /// REST requests and pings seen so far, without waiting.
    fn drained(&self) -> Vec<Seen> {
        let mut seen = std::mem::take(&mut *self.kept.borrow_mut());
        seen.extend(self.seen.try_iter());
        seen
    }
}

fn members() -> String {
    format!(
        r#"[{{"user_id":"{ME}","email":"ada@example.com","display_name":"Ada","role":"owner","created_at":"2026-01-01T00:00:00Z"}},{{"user_id":"{BEA}","email":"bea@example.com","display_name":"Bea","role":"member","created_at":"2026-01-01T00:00:00Z"}}]"#
    )
}

/// A fake server. `refuse` answers the live connection with that number with an HTTP status
/// instead of upgrading it. `script` runs each live connection that is upgraded, numbered from 0.
fn fake(refuse: &'static [(usize, u16)], script: impl Fn(usize, Conn) + Send + Sync + 'static) -> Fake {
    fake_with("off", refuse, script)
}

/// The same, with `/health` saying `media` of the server's audio.
fn fake_with(media: &'static str, refuse: &'static [(usize, u16)], script: impl Fn(usize, Conn) + Send + Sync + 'static) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let (seen, heard) = channel();
    let script = Arc::new(script);
    let count = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let (seen, script, count) = (seen.clone(), script.clone(), count.clone());
            std::thread::spawn(move || {
                let mut first = [0u8; 16];
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                if stream.peek(&mut first).is_err() {
                    return;
                }
                if first.starts_with(b"GET /v1/live") {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    upgrade(n, stream, refuse, &seen, &*script);
                } else {
                    rest(stream, &seen, media);
                }
            });
        }
    });
    Fake { base, seen: heard, kept: Default::default() }
}

fn upgrade(n: usize, stream: TcpStream, refuse: &[(usize, u16)], seen: &Sender<Seen>, script: &(dyn Fn(usize, Conn) + Send + Sync)) {
    let status = refuse.iter().find(|(at, _)| *at == n).map(|(_, status)| *status);
    let mut asked = (None, String::new());
    let accepted = tungstenite::accept_hdr(stream.try_clone().expect("clone"), |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let query = request.uri().query().unwrap_or("");
        asked.0 = query.strip_prefix("since=").and_then(|since| since.parse().ok());
        asked.1 = request.headers().get("Authorization").and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
        match status {
            Some(status) => Err(tungstenite::http::Response::builder().status(status).body(Some(r#"{"error":"no"}"#.to_owned())).expect("a response")),
            None => Ok(response),
        }
    })
    .ok();
    let _ = seen.send(Seen::Live { since: asked.0, bearer: asked.1, at: Instant::now() });
    let Some(ws) = accepted else { return };
    // Not a read timeout: on Windows a socket whose read timed out is not to be used again.
    let _ = stream.set_nonblocking(true);
    script(n, Conn { ws, seen: seen.clone(), pong: true });
}

fn rest(mut stream: TcpStream, seen: &Sender<Seen>, media: &str) {
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while !got.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(read) => got.extend_from_slice(&buf[..read]),
        }
    }
    let line = String::from_utf8_lossy(&got).lines().next().unwrap_or("").to_owned();
    let _ = seen.send(Seen::Rest(line.clone()));
    let (status, body) = if line.starts_with("GET /v1/members ") {
        ("200 OK", members())
    } else if line.starts_with("GET /health ") {
        ("200 OK", format!(r#"{{"ok":true,"database":"ok","media":"{media}"}}"#))
    } else if line.starts_with("GET /v1/me ") {
        ("200 OK", format!(r#"{{"user_id":"{ME}","email":"ada@example.com","display_name":"Ada","role":"owner"}}"#))
    } else {
        ("404 Not Found", "{}".to_owned())
    };
    let answer = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(answer.as_bytes());
}

fn ready(seq: i64, online: &[&str], calls: Value) -> Value {
    json!({"type": "ready", "seq": seq, "me": {"user_id": ME, "role": "owner"}, "online": online, "calls": calls})
}

/// The socket thread, against a fake, with a hub to look at.
struct Running {
    hub: Arc<Hub>,
    account: AccountKey,
    live: Arc<Live>,
    adapter: Option<Adapter>,
    thread: Option<JoinHandle<()>>,
}

fn run(base: &str, timing: Timing) -> Running {
    let hub = Arc::new(Hub::new(Store::open(std::path::Path::new(":memory:")).expect("a store in memory")));
    let account = AccountKey { network: Network::Tiny, account: ME.to_owned() };
    let (adapter, ctx, _inbox) = local(account.clone(), hub.clone());
    ctx.link(Link::Offline);
    let saved = Saved {
        base_url: base.to_owned(),
        token: "t".to_owned(),
        user_id: ME.to_owned(),
        email: "ada@example.com".to_owned(),
        display_name: "Ada".to_owned(),
        role: "owner".to_owned(),
    };
    let live = Arc::new(Live::new().expect("a wake pair"));
    let socket_live = live.clone();
    let thread = std::thread::spawn(move || keep_live(&ctx, &saved, &socket_live, &timing));
    Running { hub, account, live, adapter: Some(adapter), thread: Some(thread) }
}

impl Running {
    fn link(&self) -> (Link, String) {
        self.hub.links().into_iter().find(|linked| linked.account == self.account).map_or((Link::SignedOut, String::new()), |linked| (linked.link, linked.detail))
    }

    fn key(&self, id: &str) -> Key {
        Key::new(&self.account, id)
    }

    /// Who in a room's call muted or deafened themselves, by name.
    fn quiet_in(&self, room: &str) -> Vec<(String, bool, bool)> {
        self.hub.occupants(&self.key(room)).map(|inside| inside.people.into_iter().map(|who| (who.name, who.muted, who.deafened)).collect()).unwrap_or_default()
    }

    /// Who is in a room's call, by name.
    fn in_room(&self, room: &str) -> Vec<String> {
        self.hub.occupants(&self.key(room)).map(|inside| inside.people.into_iter().map(|who| who.name).collect()).unwrap_or_default()
    }

    fn told_tiny(&self, since: u64) -> bool {
        let (_, _, changes) = self.hub.wait(since, Duration::ZERO);
        changes.iter().any(|change| matches!(change, Change::Tiny { account } if *account == self.account))
    }

    /// Asks the socket thread, as the command thread would, and waits for the answer.
    fn ask(&self, ask: impl FnOnce(Reply<()>) -> Ask) -> Result<(), String> {
        self.ask_within(Duration::from_secs(5), ask)
    }

    /// As `ask`, for a join with audio, which starts mediad first: a loaded machine is slow at it.
    fn join_with_audio(&self, room: &str) -> Result<(), String> {
        self.ask_within(Duration::from_secs(30), |reply| Ask::Join { room_id: room.into(), reply })
    }

    fn ask_within(&self, wait: Duration, ask: impl FnOnce(Reply<()>) -> Ask) -> Result<(), String> {
        let (reply, answer) = channel();
        self.live.ask(ask(reply));
        match answer.recv_timeout(wait).expect("an answer") {
            Ok(()) => Ok(()),
            Err(Refusal::Refused(words)) => Err(words),
            Err(Refusal::Unsupported) => Err("unsupported".to_owned()),
        }
    }

    fn done(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| thread.is_finished())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.adapter.take());
        self.live.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn rest_hits(seen: &[Seen], route: &str) -> usize {
    seen.iter().filter(|seen| matches!(seen, Seen::Rest(line) if line.starts_with(route))).count()
}

#[test]
fn ready_fills_presence_and_rooms_and_the_frames_after_it_move_them() {
    let fake = fake(&[], |_, mut conn| {
        conn.send(ready(5, &[ME, BEA], json!([{"room_id": ROOM, "participants": [{"user_id": BEA, "muted": true, "deafened": false}]}])));
        conn.idle(Duration::from_millis(300));
        conn.send(json!({"type": "call.state", "room_id": ROOM, "user_id": BEA, "muted": true, "deafened": true}));
        conn.send(json!({"type": "call.speaking", "room_id": ROOM, "user_ids": [BEA]}));
        conn.idle(Duration::from_millis(300));
        conn.send(json!({"type": "presence", "user_id": BEA, "online": false}));
        conn.send(json!({"type": "call.joined", "room_id": OTHER_ROOM, "user_id": BEA}));
        conn.idle(Duration::from_millis(300));
        conn.send(json!({"type": "call.left", "room_id": OTHER_ROOM, "user_id": BEA, "reason": "left"}));
        conn.send(json!({"type": "something.new", "x": 1}));
        conn.idle(Duration::from_secs(5));
    });
    let running = run(&fake.base, QUICK);
    let since = running.hub.seq();
    let (asked, bearer, _) = fake.connected(Duration::from_secs(5)).expect("a live connection");
    assert_eq!(asked, None, "a first connection has nothing to resume");
    assert_eq!(bearer, "Bearer t", "the session goes in the header, never the address");
    until("the link", || running.link().0 == Link::Connected);
    assert_eq!(running.hub.presence(&running.key(BEA)), Presence::Online);
    assert_eq!(running.hub.presence(&running.key(ME)), Presence::Online);
    assert_eq!(running.in_room(ROOM), ["Bea"], "names come from the member list");
    assert!(running.told_tiny(since), "the page reads the server again after a fresh ready");
    assert_eq!(running.quiet_in(ROOM), [("Bea".to_owned(), true, false)], "ready carries each seat's mute");
    until("Bea deafened", || running.quiet_in(ROOM) == [("Bea".to_owned(), true, true)]);
    let seen = fake.drained();
    assert_eq!(rest_hits(&seen, "GET /v1/members "), 1);
    assert_eq!(rest_hits(&seen, "GET /health "), 1, "a fresh ready asks whether the server has audio");

    until("Bea going offline", || running.hub.presence(&running.key(BEA)) == Presence::Offline);
    until("Bea moving rooms", || running.in_room(OTHER_ROOM) == ["Bea"]);
    assert!(running.hub.occupants(&running.key(ROOM)).is_none(), "one seat a person: the first room emptied");
    until("Bea leaving", || running.hub.occupants(&running.key(OTHER_ROOM)).is_none());
    assert_eq!(running.link().0, Link::Connected, "an unknown frame is ignored");
}

#[test]
fn events_apply_in_order_a_replay_is_skipped_and_a_drop_resumes_with_since() {
    let fake = fake(&[], |n, mut conn| match n {
        0 => {
            conn.send(ready(5, &[ME], json!([{"room_id": ROOM, "participants": [{"user_id": BEA}]}])));
            conn.send(json!({"type": "member.renamed", "seq": 6, "member": {"user_id": BEA, "display_name": "Bea Two", "role": "member"}}));
            conn.send(json!({"type": "member.renamed", "seq": 7, "member": {"user_id": BEA, "display_name": "Bea Three", "role": "member"}}));
            // The same event again, as an overlapping replay would send it.
            conn.send(json!({"type": "member.renamed", "seq": 6, "member": {"user_id": BEA, "display_name": "Stale", "role": "member"}}));
            conn.idle(Duration::from_millis(400));
            conn.close(1012);
        }
        _ => {
            conn.send(ready(7, &[ME], json!([{"room_id": ROOM, "participants": [{"user_id": BEA}]}])));
            conn.idle(Duration::from_secs(5));
        }
    });
    let running = run(&fake.base, QUICK);
    fake.connected(Duration::from_secs(5)).expect("the first connection");
    until("the renames, in order", || running.in_room(ROOM) == ["Bea Three"]);
    let (since, bearer, _) = fake.connected(Duration::from_secs(5)).expect("a reconnect after 1012");
    assert_eq!(since, Some(7), "the last seq heard, not the stale replay");
    assert_eq!(bearer, "Bearer t");
    until("the link again", || running.link().0 == Link::Connected && running.in_room(ROOM) == ["Bea Three"]);
    assert_eq!(rest_hits(&fake.drained(), "GET /v1/members "), 1, "a resume reads nothing over REST");
}

#[test]
fn resync_starts_over_without_since_and_reads_rest_again() {
    let fake = fake(&[], |n, mut conn| match n {
        0 => {
            conn.send(ready(3, &[ME], json!([])));
            conn.idle(Duration::from_millis(200));
            conn.send(json!({"type": "resync"}));
            conn.close(4002);
        }
        1 => {
            conn.send(ready(90, &[ME], json!([])));
            conn.idle(Duration::from_millis(200));
            // A 4002 without the resync frame means the same.
            conn.close(4002);
        }
        _ => {
            conn.send(ready(91, &[ME], json!([])));
            conn.idle(Duration::from_secs(5));
        }
    });
    let running = run(&fake.base, QUICK);
    assert_eq!(fake.connected(Duration::from_secs(5)).expect("first").0, None);
    until("the first ready", || running.link().0 == Link::Connected);
    let before = running.hub.seq();
    assert_eq!(fake.connected(Duration::from_secs(5)).expect("after resync").0, None, "a resync starts over");
    assert_eq!(fake.connected(Duration::from_secs(5)).expect("after 4002").0, None, "so does 4002");
    until("the third ready", || running.link().0 == Link::Connected && running.told_tiny(before));
    // The first ready read the members, and each fresh ready after a resync reads them again.
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while rest_hits(&seen, "GET /v1/members ") < 3 && Instant::now() < deadline {
        seen.extend(fake.drained());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rest_hits(&seen, "GET /v1/members "), 3, "{seen:?}");
}

#[test]
fn a_drop_backs_off_doubling_to_the_cap_and_each_reconnect_resumes() {
    let timing = Timing { first: Duration::from_millis(200), cap: Duration::from_millis(400), ..QUICK };
    let fake = fake(&[(4, 503)], |n, mut conn| {
        conn.send(ready(10 + n as i64, &[ME], json!([])));
        conn.idle(Duration::from_millis(20));
        match n {
            0 => conn.close(1012),
            1 => conn.close(1011),
            2 => conn.close(4000),
            3 => conn.close(1012),
            _ => {
                conn.idle(Duration::from_secs(5));
            }
        }
    });
    let running = run(&fake.base, timing);
    let mut at = Vec::new();
    let mut since = Vec::new();
    for _ in 0..6 {
        let (asked, _, when) = fake.connected(Duration::from_secs(5)).expect("a connection");
        at.push(when);
        since.push(asked);
    }
    // 1012, 1011, 4000 and 1012 resume; the 503 before the last is retried the same way.
    assert_eq!(since, [None, Some(10), Some(11), Some(12), Some(13), Some(13)]);
    let expected = [200, 400, 400, 400, 400];
    for (ix, wanted) in expected.iter().enumerate() {
        let gap = at[ix + 1].duration_since(at[ix]);
        let wanted = Duration::from_millis(*wanted);
        assert!(gap >= wanted, "pause {ix} was {gap:?}, under {wanted:?}");
        assert!(gap < wanted + Duration::from_millis(390), "pause {ix} was {gap:?}: past the cap");
    }
    until("connected at the end", || running.link().0 == Link::Connected);
}

#[test]
fn a_401_at_the_door_or_a_4001_close_is_a_sign_in_and_nothing_more_is_tried() {
    let refused = fake(&[(0, 401)], |_, _| {});
    let running = run(&refused.base, QUICK);
    refused.connected(Duration::from_secs(5)).expect("one try");
    until("expired", || running.link().0 == Link::Expired);
    assert_eq!(running.link().1, ENDED);
    until("the thread ends", || running.done());
    assert!(refused.connected(Duration::from_millis(300)).is_none(), "an ended session is never retried");

    let ended = fake(&[], |_, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        conn.idle(Duration::from_millis(100));
        conn.close(4001);
    });
    let running = run(&ended.base, QUICK);
    ended.connected(Duration::from_secs(5)).expect("one connection");
    until("expired", || running.link().0 == Link::Expired);
    assert_eq!(running.link().1, ENDED);
    until("the thread ends", || running.done());
    assert!(ended.connected(Duration::from_millis(300)).is_none());
}

#[test]
fn a_removed_account_stops_and_says_why() {
    let fake = fake(&[], |_, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        conn.send(json!({"type": "member.removed", "seq": 2, "user_id": ME}));
        conn.close(4003);
    });
    let running = run(&fake.base, QUICK);
    fake.connected(Duration::from_secs(5)).expect("one connection");
    until("expired", || running.link().0 == Link::Expired);
    assert_eq!(running.link().1, REMOVED);
    until("the thread ends", || running.done());
    assert!(fake.connected(Duration::from_millis(300)).is_none(), "a removed account is not tried again");
}

#[test]
fn a_replaced_socket_waits_for_the_user_and_a_slow_down_waits_the_cap() {
    let fake = fake(&[], |n, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        conn.idle(Duration::from_millis(50));
        match n {
            0 => conn.close(4008),
            1 => conn.close(4029),
            _ => {
                conn.idle(Duration::from_secs(5));
            }
        }
    });
    let running = run(&fake.base, QUICK);
    fake.connected(Duration::from_secs(5)).expect("the first connection");
    until("offline and held", || running.link().0 == Link::Offline && running.live.held());
    assert!(fake.connected(Duration::from_millis(500)).is_none(), "4008 is not answered by reconnecting");
    assert!(!running.done());
    let asked = Instant::now();
    running.live.ask(Ask::Look);
    let (since, _, again) = fake.connected(Duration::from_secs(5)).expect("the user opened the server");
    assert!(again.duration_since(asked) < Duration::from_secs(2));
    assert_eq!(since, Some(1));
    let (_, _, after_slow) = fake.connected(Duration::from_secs(5)).expect("a reconnect after 4029");
    assert!(after_slow.duration_since(again) >= QUICK.cap, "4029 waits the longest pause");
    until("connected", || running.link().0 == Link::Connected);
}

#[test]
fn pings_go_on_time_and_a_silent_server_is_dropped() {
    let timing = Timing { ping: Duration::from_millis(80), ..QUICK };
    let fake = fake(&[], |n, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        if n == 0 {
            conn.idle(Duration::from_millis(600));
            // Stops answering, as a server that hung would.
            conn.pong = false;
            conn.idle(Duration::from_secs(3));
        } else {
            conn.idle(Duration::from_secs(5));
        }
    });
    let running = run(&fake.base, timing);
    let (_, _, first) = fake.connected(Duration::from_secs(5)).expect("a connection");
    let (since, _, second) = fake.connected(Duration::from_secs(5)).expect("a reconnect once pings went unanswered");
    assert_eq!(since, Some(1));
    let pings: Vec<Instant> = fake.drained().into_iter().filter_map(|seen| match seen { Seen::Ping(at) => Some(at), _ => None }).filter(|at| *at < second).collect();
    assert!(pings.len() >= 6, "{} pings", pings.len());
    // The fake reads on its own clock, so one ping seen late makes the next gap look short.
    // The cadence is the average, and no two are close together.
    let average = pings[pings.len() - 1].duration_since(pings[0]) / (pings.len() as u32 - 1);
    assert!(average >= Duration::from_millis(75), "pings {average:?} apart on average");
    for pair in pings.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(gap >= Duration::from_millis(40), "pings {gap:?} apart");
    }
    assert!(pings[0].duration_since(first) >= Duration::from_millis(70), "no ping before the first interval");
    // Answered until 600 ms, then one unanswered ping, then the next one finds it silent.
    assert!(second.duration_since(first) < Duration::from_millis(600 + 3 * 80 + 400));
    until("connected again", || running.link().0 == Link::Connected);
}

/// The server's side of call requests: a full room, a kick that is refused, and one that is
/// never answered.
fn calls_script(_: usize, mut conn: Conn) {
    conn.send(ready(1, &[ME, BEA], json!([{"room_id": OTHER_ROOM, "participants": [{"user_id": BEA}]}])));
    while let Some(frame) = conn.next(Duration::from_secs(10)) {
        let reference = frame["ref"].clone();
        match (frame["type"].as_str().unwrap_or(""), frame["room_id"].as_str().unwrap_or(""), frame["user_id"].as_str().unwrap_or("")) {
            ("call.join", "full", _) => conn.send(json!({"type": "error", "ref": reference, "code": "full"})),
            ("call.join", "gone", _) => conn.send(json!({"type": "error", "ref": reference, "code": "not_found"})),
            ("call.join", "quiet", _) => {}
            ("call.join", room, _) => {
                conn.send(json!({"type": "ok", "ref": reference}));
                conn.send(json!({"type": "call.joined", "room_id": room, "user_id": ME}));
            }
            ("call.leave", _, _) => {
                conn.send(json!({"type": "ok", "ref": reference}));
                conn.send(json!({"type": "call.left", "room_id": ROOM, "user_id": ME, "reason": "left"}));
            }
            ("call.kick", _, "owner") => conn.send(json!({"type": "error", "ref": reference, "code": "forbidden"})),
            ("call.kick", _, "nobody") => conn.send(json!({"type": "error", "ref": reference, "code": "not_found"})),
            ("call.kick", room, user) => {
                conn.send(json!({"type": "ok", "ref": reference}));
                conn.send(json!({"type": "call.left", "room_id": room, "user_id": user, "reason": "kicked"}));
            }
            _ => conn.send(json!({"type": "error", "ref": reference, "code": "unknown_type"})),
        }
    }
}

#[test]
fn joining_leaving_and_kicking_get_the_servers_answer_in_words() {
    let fake = fake(&[], calls_script);
    let timing = Timing { answer: Duration::from_millis(400), ..QUICK };
    let running = run(&fake.base, timing);
    assert_eq!(running.ask(|reply| Ask::Join { room_id: ROOM.into(), reply }), Err(OFFLINE.to_owned()), "nothing is queued while down");
    until("connected", || running.link().0 == Link::Connected);

    assert_eq!(running.ask(|reply| Ask::Join { room_id: ROOM.into(), reply }), Ok(()));
    until("my seat", || running.in_room(ROOM) == ["Ada"]);
    assert_eq!(running.ask(|reply| Ask::Join { room_id: "full".into(), reply }), Err("That room is full.".to_owned()));
    assert_eq!(running.ask(|reply| Ask::Join { room_id: "gone".into(), reply }), Err("That room is gone.".to_owned()));
    assert_eq!(running.ask(|reply| Ask::Join { room_id: "quiet".into(), reply }), Err(NO_ANSWER.to_owned()));
    assert_eq!(running.in_room(ROOM), ["Ada"], "a refused join keeps the seat");

    assert_eq!(running.ask(|reply| Ask::Kick { room_id: OTHER_ROOM.into(), user_id: "owner".into(), reply }), Err("You can't do that.".to_owned()));
    assert_eq!(running.ask(|reply| Ask::Kick { room_id: OTHER_ROOM.into(), user_id: "nobody".into(), reply }), Err("They're not in that room's call.".to_owned()));
    assert_eq!(running.ask(|reply| Ask::Kick { room_id: OTHER_ROOM.into(), user_id: BEA.into(), reply }), Ok(()));
    until("Bea removed from the call", || running.hub.occupants(&running.key(OTHER_ROOM)).is_none());

    assert_eq!(running.ask(|reply| Ask::Leave { reply }), Ok(()));
    until("my seat gone", || running.hub.occupants(&running.key(ROOM)).is_none());
}

#[test]
fn a_server_with_audio_down_is_sent_no_join() {
    let fake = fake_with("down", &[], |_, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        let asked = conn.next(Duration::from_secs(2));
        assert!(asked.is_none(), "no join reaches a server whose audio is down: {asked:?}");
        conn.idle(Duration::from_secs(5));
    });
    let running = run(&fake.base, QUICK);
    until("connected", || running.link().0 == Link::Connected);
    until("the health read", || !running.live.seats_only());
    assert_eq!(running.ask(|reply| Ask::Join { room_id: ROOM.into(), reply }), Err(AUDIO_DOWN.to_owned()));
}

/// The real mediad, for the joins with audio. It hears a tone, so it opens no device.
fn with_mediad() {
    static BUILT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let built = std::process::Command::new(cargo).args(["build", "-p", "mediad", "--bin", "mediad"]).status().expect("cargo runs");
        assert!(built.success(), "mediad builds");
        // target/debug/deps/chatd-… is beside target/debug/mediad.
        let deps = std::env::current_exe().expect("the test's path");
        let mediad = deps.parent().and_then(|deps| deps.parent()).expect("target/debug").join(format!("mediad{}", std::env::consts::EXE_SUFFIX));
        std::env::set_var("TINYCLIENT_MEDIAD", mediad);
        std::env::set_var("TINYCLIENT_MEDIAD_AUDIO", "tone");
    });
}

/// An answer to mediad's real offer, worded as an ICE-lite SFU would, for an address nothing
/// listens on: mediad takes it, and waits to connect.
fn answer_for(offer: &str) -> String {
    let mut lines = vec![];
    let mut candidate = true;
    for line in offer.lines() {
        if ["a=candidate:", "a=ice-options", "a=ssrc:", "a=msid:"].iter().any(|skip| line.starts_with(skip)) {
            continue;
        }
        let line = match line {
            line if line.starts_with("a=ice-ufrag:") => "a=ice-ufrag:fakeufrag",
            line if line.starts_with("a=ice-pwd:") => "a=ice-pwd:fakepasswordfakepassword12",
            "a=setup:actpass" => "a=setup:passive",
            "a=sendonly" => "a=recvonly",
            "a=recvonly" => "a=sendonly",
            line => line,
        };
        lines.push(line.to_owned());
        if line == "t=0 0" {
            lines.push("a=ice-lite".to_owned());
        }
        if line.starts_with("c=") && candidate {
            lines.push("a=candidate:1 1 udp 2130706431 127.0.0.1 9 typ host".to_owned());
            candidate = false;
        }
    }
    lines.join("\r\n") + "\r\n"
}

/// The mids of an offer's receive slots, in order.
fn slot_mids(offer: &str) -> Vec<String> {
    offer.split("m=audio").skip(1).filter(|section| section.contains("a=recvonly")).filter_map(|section| section.lines().find_map(|line| line.strip_prefix("a=mid:")).map(str::to_owned)).collect()
}

fn the_call(running: &Running) -> Option<protocol::Call> {
    running.hub.calls.describe().0.filter(|call| call.conversation == running.key(ROOM))
}

/// The server's side of a join with audio: the answer, the seat, the slots and who speaks.
fn answer_join(conn: &mut Conn, call_id: &str) -> Value {
    let join = conn.next(Duration::from_secs(30)).expect("a join");
    assert_eq!(join["type"], "call.join");
    let sdp = join["sdp"].as_str().expect("a join to a server with audio carries an offer");
    conn.send(json!({"type": "call.answer", "ref": join["ref"], "call_id": call_id, "sdp": answer_for(sdp)}));
    conn.send(json!({"type": "call.joined", "room_id": ROOM, "user_id": ME}));
    conn.send(json!({"type": "call.slots", "call_id": "c-stale", "audio": [], "video": null}));
    conn.send(json!({"type": "call.slots", "call_id": call_id, "audio": [{"mid": slot_mids(sdp)[0], "user_id": BEA}], "video": null}));
    join
}

#[test]
fn a_join_with_audio_carries_mediads_offer_out_and_the_answer_back() {
    with_mediad();
    let (said, heard) = channel();
    let fake = fake_with("up", &[], move |_, mut conn| {
        conn.send(ready(1, &[ME, BEA], json!([{"room_id": ROOM, "participants": [{"user_id": BEA}]}])));
        let join = answer_join(&mut conn, "c-1");
        let _ = said.send(join["sdp"].as_str().unwrap_or("").to_owned());
        conn.send(json!({"type": "call.speaking", "room_id": ROOM, "user_ids": [BEA]}));
        // Muting is told to the server.
        let state = conn.next(Duration::from_secs(5)).expect("call.state");
        assert_eq!((state["type"].as_str(), state["muted"].as_bool(), state["deafened"].as_bool()), (Some("call.state"), Some(true), Some(false)));
        conn.send(json!({"type": "ok", "ref": state["ref"]}));
        // Hanging up in chatd leaves the room.
        let leave = conn.next(Duration::from_secs(5)).expect("call.leave");
        assert_eq!(leave["type"], "call.leave");
        conn.send(json!({"type": "ok", "ref": leave["ref"]}));
        conn.send(json!({"type": "call.left", "room_id": ROOM, "user_id": ME, "reason": "left"}));
        // Joined again, and the server loses the audio.
        answer_join(&mut conn, "c-2");
        conn.idle(Duration::from_millis(300));
        conn.send(json!({"type": "call.left", "room_id": ROOM, "user_id": ME, "reason": "media_lost"}));
        // chatd hangs up after a failure; we are out of the room already, so nothing is sent.
        let after = conn.next(Duration::from_millis(800));
        assert!(after.is_none(), "nothing to leave: {after:?}");
        conn.idle(Duration::from_secs(5));
    });
    let running = run(&fake.base, QUICK);
    until("connected", || running.link().0 == Link::Connected);
    until("the health read", || !running.live.seats_only());

    assert_eq!(running.join_with_audio(ROOM), Ok(()));
    let offer = heard.recv_timeout(Duration::from_secs(5)).expect("the offer");
    assert_eq!(offer.matches("m=audio").count(), ROOM_CAP as usize, "a microphone and a slot for each other seat");
    assert_eq!(offer.matches("a=sendonly").count(), 1);
    let call = the_call(&running).expect("chatd holds the call");
    assert!(call.engine_linked);
    assert_eq!(call.state, protocol::CallState::Connecting, "connecting until media flows");
    until("Bea speaking in the room", || running.hub.occupants(&running.key(ROOM)).is_some_and(|inside| inside.people.iter().any(|who| who.name == "Bea" && who.speaking)));

    running.live.ask(Ask::State { muted: true, deafened: false });
    // What chatd does on CallEnd: its call goes, and the adapter is told to hang up.
    assert!(running.hub.calls.end(&running.hub).is_some());
    running.live.ask(Ask::Hangup { room_id: ROOM.into() });
    until("my seat gone", || running.hub.occupants(&running.key(ROOM)).is_some_and(|inside| inside.people.iter().all(|who| who.name != "Ada")));

    assert_eq!(running.join_with_audio(ROOM), Ok(()));
    until("the call failed in words", || the_call(&running).is_some_and(|call| call.state == protocol::CallState::Failed { reason: CallFailure::Lost, detail: MEDIA_LOST.to_owned() }));
    running.live.ask(Ask::Hangup { room_id: ROOM.into() });
    std::thread::sleep(Duration::from_millis(1000));
}

#[test]
fn a_join_with_audio_the_server_refuses_says_why_and_stops_mediad() {
    with_mediad();
    let fake = fake_with("up", &[], |_, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        for code in ["bad_sdp", "media_unavailable"] {
            let join = conn.next(Duration::from_secs(30)).expect("a join");
            assert!(join["sdp"].is_string());
            conn.send(json!({"type": "error", "ref": join["ref"], "code": code}));
        }
        conn.idle(Duration::from_secs(5));
    });
    let running = run(&fake.base, QUICK);
    until("connected", || running.link().0 == Link::Connected);
    until("the health read", || !running.live.seats_only());
    for words in ["The server couldn't use tinyclient's audio setup.", "This server's audio isn't available. Try again later."] {
        assert_eq!(running.join_with_audio(ROOM), Err(words.to_owned()));
        until("no call left behind", || running.hub.calls.describe().0.is_none());
    }
}

#[test]
fn after_a_restart_a_call_with_audio_joins_again_with_a_fresh_offer() {
    with_mediad();
    let (said, heard) = channel();
    let fake = fake_with("up", &[], move |n, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        match n {
            0 => {
                let join = answer_join(&mut conn, "c-1");
                let _ = said.send(join["sdp"].as_str().unwrap_or("").to_owned());
                conn.idle(Duration::from_millis(200));
                conn.close(1012);
            }
            _ => {
                let join = answer_join(&mut conn, "c-2");
                let _ = said.send(join["sdp"].as_str().unwrap_or("").to_owned());
                conn.idle(Duration::from_secs(5));
            }
        }
    });
    let running = run(&fake.base, QUICK);
    until("connected", || running.link().0 == Link::Connected);
    until("the health read", || !running.live.seats_only());
    assert_eq!(running.join_with_audio(ROOM), Ok(()));
    let first = heard.recv_timeout(Duration::from_secs(5)).expect("the first offer");
    let again = heard.recv_timeout(Duration::from_secs(30)).expect("the offer after the restart");
    let ufrag = |sdp: &str| sdp.lines().find(|line| line.starts_with("a=ice-ufrag:")).map(str::to_owned);
    assert_ne!(ufrag(&first), ufrag(&again), "a new media session");
    until("the seat again", || running.in_room(ROOM) == ["Ada"]);
    let call = the_call(&running).expect("the same call goes on");
    assert!(!matches!(call.state, protocol::CallState::Failed { .. }), "{:?}", call.state);
}

#[test]
fn a_seat_lost_with_the_socket_is_taken_again_and_one_left_on_purpose_is_not() {
    let fake = fake(&[], |n, mut conn| {
        conn.send(ready(1, &[ME], json!([])));
        match n {
            0 => {
                let join = conn.next(Duration::from_secs(5)).expect("a join");
                conn.send(json!({"type": "ok", "ref": join["ref"]}));
                conn.send(json!({"type": "call.joined", "room_id": ROOM, "user_id": ME}));
                conn.idle(Duration::from_millis(100));
                // A restart: no call.left, the close is the signal.
                conn.close(1012);
            }
            1 => {
                let join = conn.next(Duration::from_secs(5)).expect("the seat taken again");
                assert_eq!(join["type"], "call.join");
                assert_eq!(join["room_id"], ROOM);
                conn.send(json!({"type": "ok", "ref": join["ref"]}));
                conn.send(json!({"type": "call.joined", "room_id": ROOM, "user_id": ME}));
                conn.idle(Duration::from_millis(600));
                conn.send(json!({"type": "call.left", "room_id": ROOM, "user_id": ME, "reason": "media_lost"}));
                conn.idle(Duration::from_millis(100));
                conn.close(1012);
            }
            _ => {
                let unasked = conn.next(Duration::from_millis(500));
                assert!(unasked.is_none(), "a seat taken away is not taken back: {unasked:?}");
                conn.idle(Duration::from_secs(5));
            }
        }
    });
    let running = run(&fake.base, QUICK);
    until("connected", || running.link().0 == Link::Connected);
    assert_eq!(running.join_with_audio(ROOM), Ok(()));
    for _ in 0..2 {
        fake.connected(Duration::from_secs(5)).expect("a connection");
    }
    until("in the room after the restart", || running.in_room(ROOM) == ["Ada"]);
    until("the seat lost", || running.hub.occupants(&running.key(ROOM)).is_none());
    fake.connected(Duration::from_secs(5)).expect("the third connection");
    std::thread::sleep(Duration::from_millis(700));
    assert!(running.hub.occupants(&running.key(ROOM)).is_none());
}

#[test]
fn every_refusal_has_words_and_every_address_has_a_socket() {
    assert_eq!(words(false, "full"), "That room is full.");
    assert_eq!(words(false, "server_full"), "Every seat on this server is taken. Try again later.");
    assert_eq!(words(false, "not_found"), "That room is gone.");
    assert_eq!(words(true, "not_found"), "They're not in that room's call.");
    assert_eq!(words(true, "forbidden"), "You can't do that.");
    assert_eq!(words(false, "slow_down"), "Too many tries. Wait a moment, then try again.");
    assert_eq!(words(false, "unavailable"), "The server had a problem. Try again in a moment.");
    assert_eq!(words(false, "unknown_type"), "This server doesn't take calls yet.");
    assert_eq!(words(false, "bad_frame"), "The server couldn't read that request.");
    assert_eq!(words(false, "bad_sdp"), "The server couldn't use tinyclient's audio setup.");
    assert_eq!(words(false, "media_unavailable"), "This server's audio isn't available. Try again later.");
    assert_eq!(words(false, "share_taken"), "That didn't work. Try again.");

    assert_eq!(live_url("http://127.0.0.1:8080", None).as_deref(), Some("ws://127.0.0.1:8080/v1/live"));
    assert_eq!(live_url("https://chat.example/", Some(42)).as_deref(), Some("wss://chat.example/v1/live?since=42"));
    assert_eq!(live_url("ftp://chat.example", None), None);

    for (code, end) in [(4001, End::SignedOut), (4002, End::Resync), (4003, End::Removed), (4008, End::Replaced), (4029, End::SlowDown)] {
        assert_eq!(closed(code, ""), end);
    }
    for code in [1011, 1012, 4000, 1009, 1006] {
        assert!(matches!(closed(code, "x"), End::Dropped(_)), "{code}");
    }
}

/// Against a tinyserver that is already serving, as the owner. Not part of `cargo test`: it
/// needs `TINY_LIVE_BASE`, `TINY_LIVE_EMAIL`, and `TINY_LIVE_PASSWORD`. It makes one room, sits
/// in it, leaves, and deletes the room again.
#[test]
#[ignore]
fn live_socket_round_trip() {
    let base = std::env::var("TINY_LIVE_BASE").expect("TINY_LIVE_BASE");
    let email = std::env::var("TINY_LIVE_EMAIL").expect("TINY_LIVE_EMAIL");
    let password = std::env::var("TINY_LIVE_PASSWORD").expect("TINY_LIVE_PASSWORD");
    let saved = match super::super::post_session(&base, &email, &password) {
        Ok(saved) => saved,
        Err(_) => panic!("open failed"),
    };
    assert!(matches!(saved.role.as_str(), "owner" | "admin"), "the room is made by an owner or an admin");
    let hub = Arc::new(Hub::new(Store::open(std::path::Path::new(":memory:")).expect("a store in memory")));
    let account = AccountKey { network: Network::Tiny, account: saved.user_id.clone() };
    let (adapter, ctx, _inbox) = local(account.clone(), hub.clone());
    ctx.link(Link::Offline);
    let live = Arc::new(Live::new().expect("a wake pair"));
    let (socket, socket_live) = (saved.clone(), live.clone());
    let thread = std::thread::spawn(move || keep_live(&ctx, &socket, &socket_live, &SERVER));
    let running = Running { hub, account, live, adapter: Some(adapter), thread: Some(thread) };
    until("the live socket", || running.link().0 == Link::Connected);
    assert_eq!(running.hub.presence(&running.key(&saved.user_id)), Presence::Online, "ready lists me online");

    let made = super::super::call("POST", &format!("{base}/v1/rooms"), Some(&saved.token), Some(json!({"name": "tinyclient live test"})));
    let room = match made {
        Ok((201, body)) => body["id"].as_str().expect("a room id").to_owned(),
        _ => panic!("the room was not made"),
    };
    let since = running.hub.seq();
    until("room.created reaches the page", || running.told_tiny(since));
    let joined = running.ask(|reply| Ask::Join { room_id: room.clone(), reply });
    let left = joined.as_ref().ok().map(|()| {
        until("my seat", || running.in_room(&room).len() == 1);
        running.ask(|reply| Ask::Leave { reply })
    });
    let gone = super::super::call("DELETE", &format!("{base}/v1/rooms/{room}"), Some(&saved.token), None);
    assert!(matches!(gone, Ok((204, _))), "the room was put back");
    assert_eq!(joined, Ok(()));
    assert_eq!(left, Some(Ok(())));
    until("the seat gone", || running.hub.occupants(&running.key(&room)).is_none());
    drop(running);
    super::super::delete_session(&saved);
}

/// One side of a call played by hand, as a second tinyclient would: its own live socket and its
/// own mediad, with the offer and answer carried between them here.
struct Hand {
    ws: WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    mediad: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    frames: Receiver<protocol::MediaResponse>,
}

impl Hand {
    fn join(saved: &Saved, room: &str, ice: &str, sink: &std::path::Path) -> Hand {
        let url = live_url(&saved.base_url, None).expect("a live address");
        let mut request = url.into_client_request().expect("a request");
        request.headers_mut().insert("Authorization", HeaderValue::from_str(&format!("Bearer {}", saved.token)).expect("a header"));
        let (ws, _) = tungstenite::connect(request).expect("the second live socket");
        if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = ws.get_ref() {
            tcp.set_read_timeout(Some(Duration::from_millis(200))).expect("a read timeout");
        }
        let mut mediad = std::process::Command::new(std::env::var_os("TINYCLIENT_MEDIAD").expect("mediad"))
            .arg("1")
            .env("TINYCLIENT_MEDIAD_AUDIO", "tone")
            .env("TINYCLIENT_MEDIAD_SINK", sink)
            .env("TINYCLIENT_MEDIAD_ICE", ice)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the second mediad");
        let stdin = mediad.stdin.take();
        let mut stdout = mediad.stdout.take().expect("stdout");
        let (found, frames) = channel();
        std::thread::spawn(move || {
            while let Ok(frame) = protocol::read_frame::<protocol::MediaResponse>(&mut stdout) {
                if found.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut hand = Hand { ws, mediad, stdin, frames };
        assert!(matches!(hand.frames.recv_timeout(Duration::from_secs(10)), Ok(protocol::MediaResponse::Ready { .. })));
        let host = tungstenite::http::Uri::try_from(saved.base_url.as_str()).expect("an address").host().unwrap_or("").to_owned();
        let offer = TinyOffer { user_id: saved.user_id.clone(), host, slots: ROOM_CAP - 1, muted: false, deafened: false };
        let protocol::MediaResponse::Offer { sdp } = hand.ask(protocol::MediaRequest::TinyOffer(offer)) else {
            panic!("no offer");
        };
        hand.send(json!({"type": "call.join", "ref": "join", "room_id": room, "sdp": sdp}));
        let answer = hand.until("call.answer", |frame| frame["type"] == "call.answer");
        let call_id = answer["call_id"].as_str().expect("a call id").to_owned();
        assert_eq!(hand.ask(protocol::MediaRequest::TinyAnswer { sdp: answer["sdp"].as_str().expect("an answer").to_owned() }), protocol::MediaResponse::Done);
        let slots = hand.until("call.slots", |frame| frame["type"] == "call.slots" && frame["call_id"] == call_id.as_str());
        let audio = slots["audio"].as_array().into_iter().flatten().map(|slot| TinySlot { mid: slot["mid"].as_str().unwrap_or("").into(), user_id: slot["user_id"].as_str().unwrap_or("").into() });
        assert_eq!(hand.ask(protocol::MediaRequest::TinySlots { audio: audio.collect() }), protocol::MediaResponse::Done);
        hand
    }

    fn send(&mut self, frame: Value) {
        self.ws.send(Message::text(frame.to_string())).expect("sent");
    }

    fn ask(&mut self, request: protocol::MediaRequest) -> protocol::MediaResponse {
        protocol::write_frame(self.stdin.as_mut().expect("stdin"), &request).expect("written");
        loop {
            let frame = self.frames.recv_timeout(Duration::from_secs(10)).expect("mediad answered");
            if frame.is_reply() {
                return frame;
            }
        }
    }

    /// The first frame from the server that matches, pinging meanwhile.
    fn until(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < deadline, "no {what}");
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(text.as_str()).expect("JSON");
                    if matches(&frame) {
                        return frame;
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(err) => panic!("the second live socket: {err}"),
            }
        }
    }

    fn leave(mut self) {
        self.send(json!({"type": "call.leave", "ref": "leave"}));
        self.until("its leave answered", |frame| frame["ref"] == "leave");
        self.stdin.take();
        let _ = self.mediad.wait();
        let _ = self.ws.close(None);
    }
}

/// The level of the last half second a mediad played into `sink`. The tone is about 5657.
fn played_level(sink: &std::path::Path) -> f64 {
    let bytes = std::fs::read(sink).unwrap_or_default();
    let pcm: Vec<i16> = bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
    let tail = &pcm[pcm.len().saturating_sub(48_000)..];
    (tail.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / tail.len().max(1) as f64).sqrt()
}

/// A call with audio against a real tinyserver with media on, over UDP and then over ICE-TCP
/// alone. Not part of `cargo test`: it needs the server running. From tinyserver's checkout:
///
/// ```sh
/// TINYSERVER_PUBLIC_IP=127.0.0.1 cargo run -- serve
/// ```
///
/// and from this one, with the owner's sign-in:
///
/// ```sh
/// TINY_LIVE_BASE=http://127.0.0.1:8080 TINY_LIVE_EMAIL=… TINY_LIVE_PASSWORD=… \
///   cargo test -p chatd --lib tiny_server_call -- --ignored --nocapture
/// ```
///
/// Ada is the owner, through chatd's own live socket and call, as the app runs it. Bea is a
/// member it invites for the test, played by hand. Each must hear the other's tone. The room
/// and Bea are removed afterwards.
#[test]
#[ignore]
fn tiny_server_call() {
    with_mediad();
    let base = std::env::var("TINY_LIVE_BASE").expect("TINY_LIVE_BASE");
    let email = std::env::var("TINY_LIVE_EMAIL").expect("TINY_LIVE_EMAIL");
    let password = std::env::var("TINY_LIVE_PASSWORD").expect("TINY_LIVE_PASSWORD");
    let Ok(owner) = super::super::post_session(&base, &email, &password) else {
        panic!("the owner's sign-in failed");
    };
    let made = super::super::call("POST", &format!("{base}/v1/rooms"), Some(&owner.token), Some(json!({"name": "tinyclient audio test"})));
    let Ok((201, room)) = made else { panic!("the room was not made") };
    let room = room["id"].as_str().expect("a room id").to_owned();
    let invited = super::super::call("POST", &format!("{base}/v1/invites"), Some(&owner.token), Some(json!({"role": "member"})));
    let Ok((201, invite)) = invited else { panic!("the invite was not made") };
    let bea_email = format!("tinyclient-audio-{}@example.invalid", std::process::id());
    let Ok(bea) = super::super::post_accept(&base, invite["token"].as_str().expect("a token"), &bea_email, "tinyclient-audio-test", "Bea") else {
        panic!("Bea could not join");
    };

    let outcome = std::panic::catch_unwind(|| {
        for ice in ["udp", "tcp"] {
            let (ada_sink, bea_sink) = (std::env::temp_dir().join(format!("tc-call-{ice}-ada.pcm")), std::env::temp_dir().join(format!("tc-call-{ice}-bea.pcm")));
            std::env::set_var("TINYCLIENT_MEDIAD_ICE", ice);
            std::env::set_var("TINYCLIENT_MEDIAD_SINK", &ada_sink);
            let hub = Arc::new(Hub::new(Store::open(std::path::Path::new(":memory:")).expect("a store in memory")));
            let account = AccountKey { network: Network::Tiny, account: owner.user_id.clone() };
            let (adapter, ctx, _inbox) = local(account.clone(), hub.clone());
            ctx.link(Link::Offline);
            let live = Arc::new(Live::new().expect("a wake pair"));
            let (socket, socket_live) = (owner.clone(), live.clone());
            let thread = std::thread::spawn(move || keep_live(&ctx, &socket, &socket_live, &SERVER));
            let ada = Running { hub, account, live, adapter: Some(adapter), thread: Some(thread) };
            until("the live socket", || ada.link().0 == Link::Connected);
            until("the server's audio is up", || !ada.live.seats_only());

            assert_eq!(ada.join_with_audio(&room), Ok(()));
            let bea_hand = Hand::join(&bea, &room, ice, &bea_sink);
            let joined = Instant::now();
            let call = || ada.hub.calls.describe().0.filter(|call| call.conversation == ada.key(&room));
            while call().is_none_or(|call| call.state != protocol::CallState::Joined) {
                assert!(joined.elapsed() < Duration::from_secs(15), "Ada's call is up over {ice}: {:?}", call());
                std::thread::sleep(Duration::from_millis(50));
            }
            while !((3000. ..7000.).contains(&played_level(&ada_sink)) && (3000. ..7000.).contains(&played_level(&bea_sink))) {
                assert!(joined.elapsed() < Duration::from_secs(20), "each hears the other over {ice}: {} {}", played_level(&ada_sink), played_level(&bea_sink));
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("over {ice}: Ada hears {:.0}, Bea hears {:.0}", played_level(&ada_sink), played_level(&bea_sink));
            assert!(call().is_some_and(|call| call.participants.iter().any(|who| who.id == bea.user_id)), "Bea is on Ada's roster");
            bea_hand.leave();
            assert_eq!(ada.ask(|reply| Ask::Leave { reply }), Ok(()));
            until("Ada's call over", || call().is_none());
            drop(ada);
            let _ = std::fs::remove_file(&ada_sink);
            let _ = std::fs::remove_file(&bea_sink);
        }
    });
    std::env::remove_var("TINYCLIENT_MEDIAD_ICE");
    std::env::remove_var("TINYCLIENT_MEDIAD_SINK");
    let gone = super::super::call("DELETE", &format!("{base}/v1/rooms/{room}"), Some(&owner.token), None);
    let removed = super::super::call("DELETE", &format!("{base}/v1/members/{}", bea.user_id), Some(&owner.token), None);
    super::super::delete_session(&owner);
    assert!(matches!(gone, Ok((204, _))), "the room was put back");
    assert!(matches!(removed, Ok((204, _))), "Bea was removed");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
