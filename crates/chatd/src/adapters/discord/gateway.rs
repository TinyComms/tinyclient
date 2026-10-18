//! The gateway socket: identify as the web client, heartbeat, resume after a drop, and hand each
//! event to its handler.
//!
//! The socket thread sleeps in `poll` on the socket and on a local wake socket, so it wakes for a
//! frame, for something the command thread wants sent, or for the next heartbeat, and for nothing
//! else.

use super::super::net::address;
use super::super::Failure;
use super::ready::Ready;
use super::{jitter, Discord};
use protocol::Link;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, Utf8Bytes, WebSocket};

#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, RawSocket};

/// The web client's gateway version. Compression is left off: it would need a zlib or zstd
/// stream decoder, and the only large frame is READY, once.
const GATEWAY: &str = "wss://gateway.discord.gg/?encoding=json&v=9";
/// What the session can take, as the current web client declares it (deduplicated users, a
/// split READY, versioned read states and settings, protobuf settings, passive updates,
/// debounced reactions). The exact value is from the community documentation and unconfirmed
/// against a real session.
const CAPABILITIES: u64 = 1_734_653;
/// How often the loop looks up when nothing happens, to notice it was stopped.
const IDLE: Duration = Duration::from_secs(5);
/// Queued frames a minute. Discord allows 120; the rest is kept for heartbeats and the handshake,
/// which go straight to the socket.
const SENDS_A_MINUTE: usize = 100;
/// Member subscriptions go only while the minute holds fewer frames than this, so scrolling never
/// leaves a voice join or a presence change waiting for the minute to roll over.
const SUBSCRIPTIONS_A_MINUTE: usize = 80;
/// This many subscription frames a minute go the moment they are asked for. Past them each waits
/// a spacing that grows as the minute's allowance runs down, so a long scroll settles near half
/// of it and the last position is never more than a second or two behind.
const SUBSCRIPTION_BURST: usize = 8;
const MINUTE: Duration = Duration::from_secs(60);
/// This many sessions refused in a row and no more are asked for until the user does something.
const IDENTIFIES: u32 = 3;
/// How often a held adapter looks up, to notice it was stopped.
const HELD: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct SendQueue {
    commands: VecDeque<String>,
    subscriptions: serde_json::Map<String, Value>,
    /// Subscription changes written since the last heartbeat went, and those written before the
    /// heartbeat still unanswered: what a socket that died may have lost on the way.
    written: serde_json::Map<String, Value>,
    unanswered: serde_json::Map<String, Value>,
    /// When each frame of the last minute went, and whether it was a subscription.
    sent: VecDeque<(Instant, bool)>,
    /// When each guild's subscription last went, for the spacing.
    subscribed: HashMap<String, Instant>,
}

impl SendQueue {
    fn push(&mut self, op: u8, data: Value) {
        if op == 37 {
            if let Some(guilds) = data.get("subscriptions").and_then(Value::as_object) {
                self.subscriptions.extend(guilds.iter().map(|(guild, state)| (guild.clone(), state.clone())));
            }
        } else { self.commands.push_back(json!({"op": op, "d": data}).to_string()); }
    }

    /// The next frame the allowance lets go at `now`.
    fn pop(&mut self, now: Instant) -> Option<String> {
        if self.due(now)? > now { return None; }
        let subscription = self.commands.is_empty();
        self.sent.push_back((now, subscription));
        if !subscription { return self.commands.pop_front(); }
        for guild in self.subscriptions.keys() { self.subscribed.insert(guild.clone(), now); }
        let frame = json!({"op": 37, "d": {"subscriptions": &self.subscriptions}}).to_string();
        self.written.extend(std::mem::take(&mut self.subscriptions));
        Some(frame)
    }

    /// When the next waiting frame may go, or `None` with nothing waiting. Commands go first and
    /// may take the whole allowance; subscriptions are spaced and capped below it.
    fn due(&mut self, now: Instant) -> Option<Instant> {
        while self.sent.front().is_some_and(|(at, _)| now.saturating_duration_since(*at) >= MINUTE) { self.sent.pop_front(); }
        self.subscribed.retain(|_, at| now.saturating_duration_since(*at) < MINUTE);
        let free = match self.sent.front() {
            Some((at, _)) if self.sent.len() >= SENDS_A_MINUTE => *at + MINUTE,
            _ => now,
        };
        if !self.commands.is_empty() { return Some(free); }
        if self.subscriptions.is_empty() { return None; }
        let used = self.sent.len();
        let spaced = if used >= SUBSCRIPTIONS_A_MINUTE {
            self.sent[used - SUBSCRIPTIONS_A_MINUTE].0 + MINUTE
        } else if self.sent.iter().filter(|(_, subscription)| *subscription).count() < SUBSCRIPTION_BURST {
            now
        } else {
            // A guild not subscribed this minute, a server just opened, goes at once.
            let gap = MINUTE / (SUBSCRIPTIONS_A_MINUTE - used) as u32;
            self.subscriptions.keys().map(|guild| self.subscribed.get(guild).map_or(now, |at| *at + gap)).min().unwrap_or(now)
        };
        Some(spaced.max(free).max(now))
    }

    /// A heartbeat went. Discord reads in order, so its answer means it heard what was written
    /// before it.
    fn beat(&mut self) { self.unanswered.extend(std::mem::take(&mut self.written)); }

    fn answered(&mut self) { self.unanswered.clear(); }

    /// A new connection, with a minute of its own: Discord's allowance is per connection. Commands
    /// were for the old one. A resumed session still needs every subscription change Discord may
    /// not have heard, written or not, as one frame; a new session starts with none.
    fn reconnected(&mut self, resuming: bool) {
        let mut kept = serde_json::Map::new();
        if resuming {
            for changes in [&mut self.unanswered, &mut self.written, &mut self.subscriptions] { kept.extend(std::mem::take(changes)); }
        }
        *self = Self { subscriptions: kept, ..Self::default() };
    }
}

/// A socket handle for `poll` / `WSAPoll`.
#[derive(Clone, Copy)]
pub struct Pollable {
    #[cfg(unix)]
    fd: RawFd,
    #[cfg(windows)]
    socket: RawSocket,
}

impl Pollable {
    fn from_tcp(tcp: &TcpStream) -> Pollable {
        #[cfg(unix)]
        {
            Pollable { fd: tcp.as_raw_fd() }
        }
        #[cfg(windows)]
        {
            Pollable { socket: tcp.as_raw_socket() }
        }
    }
}

#[cfg(unix)]
const POLL_IN: libc::c_short = libc::POLLIN;
#[cfg(unix)]
const POLL_OUT: libc::c_short = libc::POLLOUT;

#[cfg(windows)]
const POLL_IN: i16 = windows_sys::Win32::Networking::WinSock::POLLRDNORM as i16;
#[cfg(windows)]
const POLL_OUT: i16 = windows_sys::Win32::Networking::WinSock::POLLWRNORM as i16;

fn wait_sockets(until: Instant, sockets: &[(Pollable, i16)]) {
    let ms = until.saturating_duration_since(Instant::now()).as_millis().min(60_000) as i32;
    if ms <= 0 {
        return;
    }
    #[cfg(unix)]
    {
        let mut fds: Vec<libc::pollfd> = sockets
            .iter()
            .map(|(socket, events)| libc::pollfd {
                fd: socket.fd,
                events: *events,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is valid for the length of the call.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms as libc::c_int) };
    }
    #[cfg(windows)]
    {
        let mut fds: Vec<windows_sys::Win32::Networking::WinSock::WSAPOLLFD> = sockets
            .iter()
            .map(|(socket, events)| windows_sys::Win32::Networking::WinSock::WSAPOLLFD {
                fd: socket.socket as _,
                events: *events,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is valid for the length of the call.
        unsafe {
            windows_sys::Win32::Networking::WinSock::WSAPoll(
                fds.as_mut_ptr(),
                fds.len() as u32,
                ms,
            );
        }
    }
}

fn set_cloexec(stream: &TcpStream) {
    #[cfg(unix)]
    unsafe {
        libc::fcntl(stream.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }
    #[cfg(windows)]
    let _ = stream;
}

fn set_nonblocking(stream: &TcpStream) -> io::Result<()> {
    stream.set_nonblocking(true)
}

/// Both ends of a loopback TCP connection, non-blocking, used only to wake the socket thread.
pub struct WakePair {
    read: Mutex<TcpStream>,
    write: Mutex<TcpStream>,
    read_poll: Pollable,
}

impl WakePair {
    pub fn new() -> io::Result<WakePair> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let client = TcpStream::connect(addr)?;
        let (server, _) = loop {
            match listener.accept() {
                Ok(pair) => break pair,
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => return Err(err),
            }
        };
        set_nonblocking(&server)?;
        set_nonblocking(&client)?;
        set_cloexec(&server);
        set_cloexec(&client);
        let read_poll = Pollable::from_tcp(&server);
        Ok(WakePair {
            read: Mutex::new(server),
            write: Mutex::new(client),
            read_poll,
        })
    }

    pub fn poll_read(&self) -> Pollable {
        self.read_poll
    }

    pub fn signal(&self) {
        if let Ok(mut write) = self.write.lock() {
            let _ = write.write_all(&[1u8]);
        }
    }

    pub fn drain(&self) {
        let Ok(mut read) = self.read.lock() else {
            return;
        };
        let mut buf = [0u8; 64];
        loop {
            match read.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }
}

/// Not for secrets: jitter only.
pub fn random() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(Instant::now().elapsed().as_nanos());
    hasher.finish()
}

/// How a connection ended.
enum Lost {
    /// The other side closed it, with its code.
    Closed(u16, String),
    Broken(String),
}

struct Wire {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    poll: Pollable,
}

impl Wire {
    fn connect(url: &str, user_agent: &str) -> io::Result<Wire> {
        let mut request = url.into_client_request().map_err(io::Error::other)?;
        let headers = [("User-Agent", user_agent), ("Origin", "https://discord.com")];
        for (name, value) in headers {
            request.headers_mut().insert(name, HeaderValue::from_str(value).map_err(io::Error::other)?);
        }
        // READY for an account in many servers is larger than the default frame limit.
        let config = WebSocketConfig::default().max_frame_size(Some(64 << 20)).max_message_size(Some(128 << 20));
        let (socket, _) =
            tungstenite::client::connect_with_config(request, Some(config), 3).map_err(io::Error::other)?;
        let tcp = match socket.get_ref() {
            MaybeTlsStream::NativeTls(tls) => tls.get_ref(),
            MaybeTlsStream::Plain(tcp) => tcp,
            _ => return Err(io::Error::other("unexpected stream")),
        };
        tcp.set_nonblocking(true)?;
        let poll = Pollable::from_tcp(tcp);
        Ok(Wire { socket, poll })
    }

    /// The next text frame already here, or `None` when there is nothing more to read now. The
    /// frame is handed on as it was read: READY is megabytes, and a copy would double its peak.
    fn read(&mut self) -> Result<Option<Utf8Bytes>, Lost> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(Some(text)),
                Ok(Message::Close(frame)) => {
                    let (code, reason) = frame.map_or((0, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
                    return Err(Lost::Closed(code, reason));
                }
                // Pings are answered by tungstenite on the next read or write.
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(err) => return Err(Lost::Broken(err.to_string())),
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
        Err(io::Error::new(ErrorKind::TimedOut, "the gateway is not taking what is sent"))
    }

    /// Sleeps until the socket has something, the wake socket is written to, or `limit` passes.
    fn wait(&self, wake: Pollable, limit: Duration) {
        wait_sockets(Instant::now() + limit, &[(self.poll, POLL_IN), (wake, POLL_IN)]);
    }
}

fn wait_for_wake(wake: Pollable, limit: Duration) {
    wait_sockets(Instant::now() + limit, &[(wake, POLL_IN)]);
}

/// One gateway frame. `d` is left unparsed so only the events used are ever decoded.
#[derive(Deserialize)]
struct Frame<'a> {
    op: u8,
    s: Option<u64>,
    t: Option<&'a str>,
    #[serde(borrow)]
    d: Option<&'a RawValue>,
}

struct Beat {
    every: Duration,
    next: Instant,
    /// The last heartbeat was answered. A silent gateway is a dead socket.
    acked: bool,
    /// READY or RESUMED was heard, so queued sends may go.
    ready: bool,
    /// This connection asked for a new session (identify), not for the one it had.
    identified: bool,
}

impl Discord {
    pub(super) fn wake(&self) {
        self.outbox.wake.signal();
    }

    /// Queues a gateway frame for the socket thread.
    pub(super) fn tell(&self, op: u8, d: Value) {
        self.outbox.queue.lock().unwrap().push(op, d);
        self.wake();
    }

    /// A session was refused: the reason is kept for the user, and counted.
    fn identify_failed(&self, why: &str) {
        self.forget_session();
        let mut refused = self.refused_sessions.lock().unwrap();
        refused.0 += 1;
        refused.1 = why.to_owned();
    }

    /// The user did something that needs the network: an adapter that stopped asking for a
    /// session tries again.
    pub(super) fn asked(&self) {
        if self.held.swap(false, Ordering::Relaxed) {
            self.wake();
        }
    }

    /// After [`IDENTIFIES`] refused sessions in a row nothing more is tried until the user
    /// asks: identifying over and over is what Discord's anti-abuse system looks for, and the
    /// growing pause between attempts only makes it slower. True if the adapter was stopped
    /// meanwhile.
    fn hold(&self) -> bool {
        let why = {
            let refused = self.refused_sessions.lock().unwrap();
            if refused.0 < IDENTIFIES {
                return false;
            }
            refused.1.clone()
        };
        self.ctx.attention(&format!(
            "Discord would not start a session ({why}). Check your account in Discord; tinyclient tries again when you open a conversation."
        ));
        self.held.store(true, Ordering::Relaxed);
        let wake = self.outbox.wake.poll_read();
        while self.held.load(Ordering::Relaxed) {
            if self.ctx.stopped() {
                return true;
            }
            wait_for_wake(wake, HELD);
            self.outbox.wake.drain();
        }
        self.refused_sessions.lock().unwrap().0 = 0;
        if !self.rest.challenged.load(Ordering::Relaxed) {
            self.ctx.attention("");
        }
        false
    }

    pub(super) fn connect(&self) -> Failure {
        if self.hold() {
            return Failure::Dropped("stopped".to_owned());
        }
        match self.run() {
            Ok(()) => Failure::Dropped("stopped".to_owned()),
            Err(failure) => failure,
        }
    }

    fn forget_session(&self) {
        self.state.lock().unwrap().session = None;
        self.sequence.store(0, Ordering::Relaxed);
    }

    /// 4004 is the token being refused. A session Discord no longer holds is started again; any
    /// other drop is resumed.
    fn lost(&self, lost: Lost) -> Failure {
        match lost {
            Lost::Closed(4004, _) => {
                // Nothing more is asked over HTTP with a token Discord has refused.
                self.rest.expired.store(true, Ordering::Relaxed);
                Failure::SignedOut("Discord ended the session.".to_owned())
            }
            // The session is gone and the next attempt identifies, so each of these counts
            // towards the limit on doing that.
            Lost::Closed(code @ (4007 | 4009..=4014), reason) => {
                self.identify_failed(format!("gateway closed {code} {}", reason.trim()).trim());
                Failure::Dropped(format!("gateway closed: {code} {reason}"))
            }
            Lost::Closed(code, reason) => Failure::Dropped(format!("gateway closed: {code} {reason}")),
            Lost::Broken(why) => Failure::Dropped(why),
        }
    }

    fn run(&self) -> Result<(), Failure> {
        let session = self.state.lock().unwrap().session.clone();
        let url = match &session {
            Some(session) => session.url.clone(),
            None => address("TINYCLIENT_DISCORD_GATEWAY", GATEWAY),
        };
        let mut wire = Wire::connect(&url, &self.rest.identity().user_agent)?;
        self.outbox.queue.lock().unwrap().reconnected(session.is_some());
        let mut beat = Beat {
            every: Duration::from_millis(41_250),
            next: Instant::now() + Duration::from_secs(60),
            acked: true,
            ready: false,
            identified: false,
        };
        let wake = self.outbox.wake.poll_read();
        loop {
            if self.ctx.stopped() {
                return Ok(());
            }
            if self.rest.expired.load(Ordering::Relaxed) {
                return Err(Failure::SignedOut("Discord ended the session.".to_owned()));
            }
            loop {
                match wire.read() {
                    Ok(Some(text)) => self.frame(&mut wire, text, &mut beat)?,
                    Ok(None) => break,
                    Err(lost) => return Err(self.lost(lost)),
                }
            }
            // What the allowance does not let go yet waits in the queue: see `SendQueue::due`.
            while beat.ready {
                let Some(frame) = self.outbox.queue.lock().unwrap().pop(Instant::now()) else {
                    break;
                };
                wire.send(frame)?;
            }
            if Instant::now() >= beat.next {
                if !beat.acked {
                    return Err(Failure::Dropped("the gateway stopped answering".to_owned()));
                }
                let sequence = self.sequence.load(Ordering::Relaxed);
                let sequence = if sequence == 0 { Value::Null } else { json!(sequence) };
                wire.send(json!({"op": 1, "d": sequence}).to_string())?;
                self.outbox.queue.lock().unwrap().beat();
                beat.acked = false;
                beat.next = Instant::now() + beat.every;
                if beat.ready {
                    // The shell's watch lapses without a command; this is where that is noticed.
                    self.watch(&self.ctx.watching());
                }
            }
            let pending = self.tick();
            let until_beat = beat.next.saturating_duration_since(Instant::now());
            let mut limit = if pending { Duration::from_millis(500) } else { IDLE };
            if beat.ready {
                if let Some(due) = self.outbox.queue.lock().unwrap().due(Instant::now()) {
                    limit = limit.min(due.saturating_duration_since(Instant::now()));
                }
            }
            wire.wait(wake, until_beat.min(limit));
            self.outbox.wake.drain();
        }
    }

    fn identify(&self) -> Value {
        json!({"op": 2, "d": {
            "token": self.token,
            "capabilities": CAPABILITIES,
            "properties": self.rest.identity().properties,
            "presence": {"status": "unknown", "since": 0, "activities": [], "afk": false},
            "compress": false,
            "client_state": {"guild_versions": {}},
        }})
    }

    fn frame(&self, wire: &mut Wire, text: Utf8Bytes, beat: &mut Beat) -> Result<(), Failure> {
        let frame: Frame = serde_json::from_str(text.as_str())?;
        if let Some(sequence) = frame.s {
            self.sequence.store(sequence, Ordering::Relaxed);
        }
        let data = frame.d.map_or("null", RawValue::get);
        match (frame.op, frame.t) {
            // Hello: start the heartbeat somewhere inside the first interval, then resume the
            // session we had or say who we are.
            (10, _) => {
                let hello: Value = serde_json::from_str(data)?;
                let interval = hello["heartbeat_interval"].as_u64().unwrap_or(41_250).max(1000);
                beat.every = Duration::from_millis(interval);
                beat.next = Instant::now() + Duration::from_millis(random() % interval);
                beat.acked = true;
                let session = self.state.lock().unwrap().session.clone();
                let sequence = self.sequence.load(Ordering::Relaxed);
                let hello = match session {
                    Some(session) => json!({"op": 6, "d": {
                        "token": self.token,
                        "session_id": session.id,
                        "seq": sequence,
                    }}),
                    None => {
                        beat.identified = true;
                        self.identify()
                    }
                };
                wire.send(hello.to_string())?;
            }
            (11, _) => {
                beat.acked = true;
                self.outbox.queue.lock().unwrap().answered();
            }
            // The server asked for a heartbeat now.
            (1, _) => beat.next = Instant::now(),
            (7, _) => return Err(Failure::Dropped("Discord asked to reconnect".to_owned())),
            (9, _) => {
                // `true` means the session can still be resumed.
                if data != "true" {
                    self.forget_session();
                    if beat.identified {
                        self.identify_failed("Discord refused the session");
                    }
                }
                std::thread::sleep(jitter(1000, 5000));
                return Err(Failure::Dropped("Discord asked for the session again".to_owned()));
            }
            (0, Some("READY")) => {
                let ready: Ready = match serde_json::from_str(data) {
                    Ok(ready) => ready,
                    // Asking again would get the same answer: it counts as a refused session.
                    Err(err) => {
                        self.identify_failed(&format!("its first message could not be read: {err}"));
                        return Err(err.into());
                    }
                };
                // READY can be many megabytes: the text goes before anything is done with it.
                drop(text);
                self.ready(ready)?;
                self.refused_sessions.lock().unwrap().0 = 0;
                beat.ready = true;
                self.ctx.link(Link::Connected);
            }
            (0, Some("RESUMED")) => {
                self.refused_sessions.lock().unwrap().0 = 0;
                beat.ready = true;
                self.ctx.link(Link::Connected);
                self.resumed();
            }
            (0, Some(event)) => {
                // One event that cannot be read is not a reason to drop the connection.
                if let Err(Failure::SignedOut(why) | Failure::Dropped(why)) = self.dispatch(event, data) {
                    self.log(event, why);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Timers that ride on the socket thread. True while one is running, so the loop looks again
    /// soon.
    fn tick(&self) -> bool {
        let voice = self.voice_tick();
        let search = self.search_tick();
        self.unmute_tick();
        voice || search
    }
}

#[cfg(test)]
mod send_queue_tests {
    use super::*;

    fn window(frame: &str) -> Value {
        let frame: Value = serde_json::from_str(frame).unwrap();
        frame["d"]["subscriptions"]["guild"]["window"].clone()
    }

    /// Everything the queue lets go at `now`, as the socket loop would send it.
    fn flush(queue: &mut SendQueue, now: Instant, sent: &mut Vec<(Instant, bool)>) {
        while let Some(frame) = queue.pop(now) {
            sent.push((now, serde_json::from_str::<Value>(&frame).unwrap()["op"] == 37));
        }
    }

    /// No rolling minute holds more than the allowance, of everything or of subscriptions.
    fn within_allowance(sent: &[(Instant, bool)]) {
        for (index, (start, _)) in sent.iter().enumerate() {
            let minute = sent[index..].iter().take_while(|(at, _)| *at < *start + MINUTE);
            let (all, subscriptions) = minute.fold((0, 0), |(all, subs), (_, sub)| (all + 1, subs + usize::from(*sub)));
            assert!(all <= SENDS_A_MINUTE && subscriptions <= SUBSCRIPTIONS_A_MINUTE, "{all} frames, {subscriptions} subscriptions in a minute");
        }
    }

    #[test]
    fn scrolling_coalesces_only_unsent_snapshots_without_an_artificial_delay() {
        let mut queue = SendQueue::default();
        let now = Instant::now();
        for step in 0..600 { queue.push(37, json!({"subscriptions": {"guild": {"window": step}}})); }
        assert_eq!(queue.subscriptions.len(), 1);
        assert_eq!(window(&queue.pop(now).unwrap()), 599);
        for step in 1..SUBSCRIPTION_BURST {
            queue.push(37, json!({"subscriptions": {"guild": {"window": step}}}));
            assert!(queue.pop(now).is_some(), "a short scroll does not wait for a timer");
        }
        queue.push(37, json!({"subscriptions": {"guild": {"window": "next"}}}));
        assert!(queue.pop(now).is_none(), "past the burst a snapshot is spaced");
        let due = queue.due(now).unwrap();
        assert!(due > now && due - now < Duration::from_secs(1), "{:?}", due - now);
        assert_eq!(window(&queue.pop(due).unwrap()), "next");
    }

    #[test]
    fn coalescing_preserves_guild_unsubscribes_and_command_order() {
        let mut queue = SendQueue::default();
        let now = Instant::now();
        queue.push(37, json!({"subscriptions": {"a": {"typing": true}}}));
        queue.push(37, json!({"subscriptions": {"a": {"typing": false}, "b": {"typing": true}}}));
        queue.push(4, json!({"channel_id": "voice"}));
        let command: Value = serde_json::from_str(&queue.pop(now).unwrap()).unwrap();
        assert_eq!(command["op"], 4);
        let subscription: Value = serde_json::from_str(&queue.pop(now).unwrap()).unwrap();
        assert_eq!(subscription["d"]["subscriptions"], json!({"a": {"typing": false}, "b": {"typing": true}}));
        assert!(queue.pop(now).is_none());
        assert!(queue.due(now).is_none(), "nothing waiting, nothing to wake for");
        queue.reconnected(false);
        queue.push(37, json!({"subscriptions": {"c": {}}}));
        assert!(queue.pop(now).is_some(), "a new connection sends its first subscription immediately");
    }

    /// Five minutes of a viewport moving every 10 ms with a presence change every ten seconds
    /// among it, on a clock the test moves.
    #[test]
    fn a_sustained_scroll_keeps_to_the_allowance_and_ends_on_the_last_viewport() {
        let mut queue = SendQueue::default();
        let start = Instant::now();
        let mut now = start;
        let mut sent = Vec::new();
        for step in 0..30_000u32 {
            queue.push(37, json!({"subscriptions": {"guild": {"window": step}}}));
            if step % 1000 == 0 {
                queue.push(3, json!({"status": "online"}));
                let before = sent.len();
                flush(&mut queue, now, &mut sent);
                assert!(sent[before..].iter().any(|(_, sub)| !sub), "a command waited behind scrolling at {:?}", now - start);
            }
            flush(&mut queue, now, &mut sent);
            assert!(queue.subscriptions.len() <= 1 && queue.commands.is_empty() && queue.subscribed.len() <= 1, "the queue grows");
            assert!(queue.sent.len() <= SENDS_A_MINUTE);
            now += Duration::from_millis(10);
        }
        within_allowance(&sent);
        let delivered = sent.iter().filter(|(_, sub)| *sub).count();
        assert!(delivered < 5 * SUBSCRIPTIONS_A_MINUTE, "{delivered} subscription frames in five minutes");
        // The pace settles with a reserve left: well under the share was spent in the last minute.
        let recent = sent.iter().filter(|(at, sub)| *sub && *at + MINUTE > now).count();
        assert!(recent <= SUBSCRIPTIONS_A_MINUTE * 3 / 4, "{recent} subscriptions in the last minute");

        // The scroll stops: the last viewport goes when the loop next wakes, within a second or two.
        let due = queue.due(now).expect("the last viewport is waiting");
        assert!(due - now <= Duration::from_secs(2), "the last viewport waits {:?}", due - now);
        assert_eq!(window(&queue.pop(due).unwrap()), 29_999);
        assert!(queue.due(due).is_none());

        // A server opened right after still goes at once.
        queue.push(37, json!({"subscriptions": {"other": {"typing": true}}}));
        assert!(queue.pop(due).is_some(), "navigation waited behind scrolling");
    }

    #[test]
    fn a_spent_allowance_waits_for_the_minute_and_keeps_room_for_commands() {
        let mut queue = SendQueue::default();
        let start = Instant::now();
        let mut sent = Vec::new();
        // Servers not yet subscribed each go at once, until the subscription share is spent.
        for guild in 0..SUBSCRIPTIONS_A_MINUTE + 10 {
            queue.push(37, json!({"subscriptions": {guild.to_string(): {"typing": true}}}));
            flush(&mut queue, start + Duration::from_millis(guild as u64), &mut sent);
        }
        assert_eq!(sent.len(), SUBSCRIPTIONS_A_MINUTE);
        assert_eq!(queue.subscriptions.len(), 10, "the rest wait as one frame");
        let now = start + Duration::from_secs(1);
        for _ in 0..SENDS_A_MINUTE { queue.push(3, json!({"status": "idle"})); }
        flush(&mut queue, now, &mut sent);
        assert_eq!(sent.len(), SENDS_A_MINUTE, "commands take what subscriptions leave, and no more");
        assert_eq!(queue.commands.len(), SUBSCRIPTIONS_A_MINUTE);
        within_allowance(&sent);
        // The oldest frame ages out after a minute, and commands go before subscriptions.
        let due = queue.due(now).unwrap();
        assert_eq!(due, start + MINUTE);
        assert!(queue.pop(due - Duration::from_millis(1)).is_none());
        let command: Value = serde_json::from_str(&queue.pop(due).unwrap()).unwrap();
        assert_eq!(command["op"], 3);
    }

    #[test]
    fn a_resumed_connection_keeps_unheard_subscriptions_and_a_new_session_does_not() {
        let mut queue = SendQueue::default();
        let now = Instant::now();
        let subscriptions = |frame: Option<String>| serde_json::from_str::<Value>(&frame.unwrap()).unwrap()["d"]["subscriptions"].clone();
        // Heard: a heartbeat written after it was answered.
        queue.push(37, json!({"subscriptions": {"a": {"typing": true}}}));
        assert!(queue.pop(now).is_some());
        queue.beat();
        queue.answered();
        // Written and not known to be heard, on either side of a heartbeat still unanswered.
        queue.push(37, json!({"subscriptions": {"a": {"typing": false}, "b": {"typing": true}}}));
        assert!(queue.pop(now).is_some());
        queue.beat();
        queue.push(37, json!({"subscriptions": {"b": {"window": 1}}}));
        assert!(queue.pop(now).is_some());
        // Never written.
        queue.push(37, json!({"subscriptions": {"b": {"window": 2}, "c": {"typing": false}}}));
        queue.push(4, json!({"channel_id": "voice"}));
        queue.reconnected(true);
        assert_eq!(subscriptions(queue.pop(now)), json!({"a": {"typing": false}, "b": {"window": 2}, "c": {"typing": false}}));
        assert!(queue.pop(now).is_none(), "commands are not carried to the next connection");

        // What was heard before the drop is not said again.
        queue.beat();
        queue.answered();
        queue.reconnected(true);
        assert!(queue.pop(now).is_none());
        queue.push(37, json!({"subscriptions": {"b": {"typing": true}}}));
        assert!(queue.pop(now).is_some());
        queue.push(37, json!({"subscriptions": {"b": {"window": 3}}}));
        queue.reconnected(false);
        assert!(queue.pop(now).is_none(), "a new session starts with nothing subscribed");
    }

    /// Discord counts each connection's minute on its own, and a suspended machine's clock stands
    /// still, so the last connection's sends must not hold up the next one's.
    #[test]
    fn a_resumed_connection_starts_its_own_minute() {
        let mut queue = SendQueue::default();
        let now = Instant::now();
        let mut sent = Vec::new();
        for guild in 0..SUBSCRIPTIONS_A_MINUTE {
            queue.push(37, json!({"subscriptions": {guild.to_string(): {"typing": true}}}));
            flush(&mut queue, now, &mut sent);
        }
        for _ in 0..SENDS_A_MINUTE { queue.push(3, json!({"status": "idle"})); }
        flush(&mut queue, now, &mut sent);
        assert_eq!(sent.len(), SENDS_A_MINUTE);
        queue.beat();
        queue.answered();
        queue.push(37, json!({"subscriptions": {"guild": {"window": "kept"}}}));
        assert!(queue.due(now).is_some_and(|due| due - now > MINUTE / 2), "the old connection's minute is spent");

        queue.reconnected(true);
        let kept: Value = serde_json::from_str(&queue.pop(now).expect("the kept viewport waits for nothing")).unwrap();
        assert_eq!(kept["d"]["subscriptions"], json!({"guild": {"window": "kept"}}));
        assert!(queue.pop(now).is_none(), "commands are not carried to the next connection");
        queue.push(3, json!({"status": "online"}));
        assert!(queue.pop(now).is_some(), "a command waits for nothing either");
        for step in 1..SUBSCRIPTION_BURST {
            queue.push(37, json!({"subscriptions": {"guild": {"window": step}}}));
            assert!(queue.pop(now).is_some(), "the new connection has its own burst");
        }
    }
}
