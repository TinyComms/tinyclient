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
use std::collections::VecDeque;
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
const SENDS_A_MINUTE: usize = 60;
/// This many sessions refused in a row and no more are asked for until the user does something.
const IDENTIFIES: u32 = 3;
/// How often a held adapter looks up, to notice it was stopped.
const HELD: Duration = Duration::from_secs(30);

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
        self.outbox.queue.lock().unwrap().push_back(json!({"op": op, "d": d}).to_string());
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
            None => address("TINYCHAT_DISCORD_GATEWAY", GATEWAY),
        };
        let mut wire = Wire::connect(&url, &self.rest.identity().user_agent)?;
        self.outbox.queue.lock().unwrap().clear();
        let mut beat = Beat {
            every: Duration::from_millis(41_250),
            next: Instant::now() + Duration::from_secs(60),
            acked: true,
            ready: false,
            identified: false,
        };
        let mut sent = VecDeque::new();
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
            // Discord closes a socket that sends more than 120 frames a minute. Half of that is
            // ours to use; anything beyond waits in the queue.
            while sent.front().is_some_and(|at: &Instant| at.elapsed() > Duration::from_secs(60)) {
                sent.pop_front();
            }
            while beat.ready && sent.len() < SENDS_A_MINUTE {
                let Some(frame) = self.outbox.queue.lock().unwrap().pop_front() else {
                    break;
                };
                wire.send(frame)?;
                sent.push_back(Instant::now());
            }
            if Instant::now() >= beat.next {
                if !beat.acked {
                    return Err(Failure::Dropped("the gateway stopped answering".to_owned()));
                }
                let sequence = self.sequence.load(Ordering::Relaxed);
                let sequence = if sequence == 0 { Value::Null } else { json!(sequence) };
                wire.send(json!({"op": 1, "d": sequence}).to_string())?;
                beat.acked = false;
                beat.next = Instant::now() + beat.every;
                if beat.ready {
                    // The shell's watch lapses without a command; this is where that is noticed.
                    self.watch(&self.ctx.watching());
                }
            }
            let pending = self.tick();
            let until_beat = beat.next.saturating_duration_since(Instant::now());
            let limit = if pending { Duration::from_millis(500) } else { IDLE };
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
            (11, _) => beat.acked = true,
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
        let members = self.members_tick();
        let voice = self.voice_tick();
        let search = self.search_tick();
        self.unmute_tick();
        members || voice || search
    }
}
