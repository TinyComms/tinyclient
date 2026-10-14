//! Runs the real chatd against a local fake of Slack and asks it, over its own socket, what the
//! shell would ask.
//!
//! The fake is an HTTP server and a websocket server on localhost. It answers Slack's methods
//! from the hand-written payloads in `fixtures/slack/`, records what it was sent, and pushes
//! socket events when a test tells it to. Nothing here reaches Slack, the keychain or the real
//! support directory.

use protocol::{
    read_frame, write_frame, AccountKey, Badge, CallSupport, Change, Conversation, ConversationKind,
    ErrorCode, Group, GroupKind, InboxFilter, Key, Link, MediaKind, MentionKind, Message,
    MessageKind, Network, Page, Presence, Relation, Request, Response, SendState, Style, Watching,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use protocol::ChatStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::handshake::server::{Request as Upgrade, Response as Upgraded};

fn agent() -> &'static str {
    chatd::adapters::net::browser()
}
const COOKIE: &str = "xoxd-fake%2Bcookie%3D";
const ALPHA: &str = "xoxc-alpha";
const BETA: &str = "xoxc-beta";
/// The thread the fixtures hang off `#general`.
const ROOT: &str = "1760000200.000200";

fn team(token: &str) -> &'static str {
    if token == BETA {
        "T02BETA"
    } else {
        "T01ALPHA"
    }
}

fn me(token: &str) -> &'static str {
    if token == BETA {
        "U0ME2"
    } else {
        "U0ME"
    }
}

/// One request the fake received.
#[derive(Clone, Debug)]
struct Call {
    method: String,
    token: String,
    params: HashMap<String, String>,
    user_agent: String,
    cookie: String,
    /// The body's length: what an upload carried.
    bytes: usize,
    at: Instant,
}

/// One websocket handshake the fake received.
#[derive(Clone, Debug)]
struct Shake {
    token: String,
    user_agent: String,
    cookie: String,
}

struct Reply {
    status: &'static str,
    retry_after: Option<u64>,
    kind: &'static str,
    /// A `d` cookie this answer reissues.
    cookie: Option<String>,
    body: Vec<u8>,
}

fn ok(body: Value) -> Reply {
    Reply {
        status: "200 OK",
        retry_after: None,
        kind: "application/json",
        cookie: None,
        body: body.to_string().into_bytes(),
    }
}

fn limited(seconds: u64) -> Reply {
    Reply {
        status: "429 Too Many Requests",
        retry_after: Some(seconds),
        kind: "application/json",
        cookie: None,
        body: Vec::new(),
    }
}

/// The direct conversations a busy workspace has unread beside the fixtures', oldest first:
/// more than may be warmed.
const BUSY: [&str; 7] = ["D1A", "D1B", "D1C", "D1D", "D1E", "D1F", "D1G"];

fn refused(error: &str) -> Reply {
    ok(json!({"ok": false, "error": error}))
}

struct Fake {
    /// `http://127.0.0.1:port`, which also stands in for `https://files.slack.com`.
    base: String,
    calls: Mutex<Vec<Call>>,
    shakes: Mutex<Vec<Shake>>,
    sockets: Mutex<HashMap<String, Sender<String>>>,
    /// Frames chatd sent on its sockets, by token.
    heard: Mutex<Vec<(String, Value)>>,
    revoked: Mutex<Vec<String>>,
    posted: AtomicU32,
    /// The first workspace has [`BUSY`] unread as well.
    busy: AtomicBool,
    /// Methods that answer with an error instead, by name.
    refusing: Mutex<HashMap<String, String>>,
    /// A `d` cookie to reissue on the first workspace's next `conversations.mark`.
    reissue: Mutex<Option<String>>,
    /// "limit once" has been limited.
    limited: AtomicBool,
}

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let (mut out, mut at) = (Vec::with_capacity(bytes.len()), 0);
    while at < bytes.len() {
        let hex = bytes.get(at + 1..at + 3).and_then(|hex| std::str::from_utf8(hex).ok());
        match (bytes[at], hex.and_then(|hex| u8::from_str_radix(hex, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
                continue;
            }
            (b'+', _) => out.push(b' '),
            (byte, _) => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8(out).unwrap()
}

fn form(body: &[u8]) -> HashMap<String, String> {
    let body = std::str::from_utf8(body).unwrap();
    let pairs = body.split('&').filter_map(|pair| pair.split_once('='));
    pairs.map(|(name, value)| (unescape(name), unescape(value))).collect()
}

impl Fake {
    fn start() -> (Arc<Fake>, String) {
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let fake = Arc::new(Fake {
            base: format!("http://{}", http.local_addr().unwrap()),
            calls: Mutex::default(),
            shakes: Mutex::default(),
            sockets: Mutex::default(),
            heard: Mutex::default(),
            revoked: Mutex::default(),
            posted: AtomicU32::new(0),
            busy: AtomicBool::new(false),
            refusing: Mutex::default(),
            reissue: Mutex::default(),
            limited: AtomicBool::new(false),
        });
        let socket_url = format!("ws://{}/", socket.local_addr().unwrap());
        for (listener, serve) in [(http, Fake::http as fn(Arc<Fake>, TcpStream)), (socket, Fake::socket)] {
            let fake = fake.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let fake = fake.clone();
                    std::thread::spawn(move || serve(fake, stream));
                }
            });
        }
        (fake, socket_url)
    }

    /// A payload from `fixtures/slack/<team>/<name>.json`.
    fn fixture(&self, token: &str, name: &str) -> Option<Value> {
        let path = format!("{}/tests/fixtures/slack/{}/{name}.json", env!("CARGO_MANIFEST_DIR"), team(token));
        let text = std::fs::read_to_string(path).ok()?;
        let encoded = self.base.replace(':', "%3A").replace('/', "%2F");
        let text = text.replace("https://files.slack.com", &self.base).replace("https%3A%2F%2Ffiles.slack.com", &encoded);
        Some(serde_json::from_str(&text).unwrap())
    }

    fn user(&self, token: &str, id: &str) -> Option<Value> {
        let users = self.fixture(token, "users")?;
        users.as_array()?.iter().find(|user| user["id"] == id).cloned()
    }

    // HTTP.

    fn http(self: Arc<Fake>, stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut out = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let mut headers = HashMap::new();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                    return;
                }
                match header.trim_end().split_once(':') {
                    Some((name, value)) => headers.insert(name.to_ascii_lowercase(), value.trim().to_owned()),
                    None => break,
                };
            }
            let length = headers.get("content-length").and_then(|length| length.parse().ok()).unwrap_or(0);
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            let reply = self.answer(&path, &headers, &body);
            let mut more = reply.retry_after.map_or_else(String::new, |seconds| format!("Retry-After: {seconds}\r\n"));
            if let Some(cookie) = reply.cookie {
                more.push_str(&format!("Set-Cookie: d={cookie}; Path=/; Domain=.slack.com; Secure; HttpOnly\r\n"));
            }
            let head = format!(
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{more}\r\n",
                reply.status,
                reply.kind,
                reply.body.len()
            );
            if out.write_all(head.as_bytes()).and_then(|()| out.write_all(&reply.body)).is_err() {
                return;
            }
        }
    }

    fn answer(&self, path: &str, headers: &HashMap<String, String>, body: &[u8]) -> Reply {
        let header = |name: &str| headers.get(name).cloned().unwrap_or_default();
        let mut call = Call {
            method: String::new(),
            token: String::new(),
            params: HashMap::new(),
            user_agent: header("user-agent"),
            cookie: header("cookie"),
            bytes: body.len(),
            at: Instant::now(),
        };
        if let Some(method) = path.strip_prefix("/api/") {
            call.method = method.to_owned();
            call.params = form(body);
            call.token = call.params.get("token").cloned().unwrap_or_default();
            self.calls.lock().unwrap().push(call.clone());
            return self.api(&call);
        }
        if let Some(rest) = path.strip_prefix("/cache/") {
            let asked: Value = serde_json::from_slice(body).unwrap();
            let (space, method) = rest.split_once('/').unwrap();
            call.method = format!("edge:{method}");
            call.token = asked["token"].as_str().unwrap_or_default().to_owned();
            if let Some(query) = asked["query"].as_str() {
                call.params.insert("query".to_owned(), query.to_owned());
            }
            self.calls.lock().unwrap().push(call.clone());
            assert_eq!(space, team(&call.token), "the edge cache is asked under the workspace's own id");
            return self.edge(&call.token, method, &asked);
        }
        // A file: an upload, a thumbnail, a picture.
        call.method = format!("file:{path}");
        call.token = header("authorization").trim_start_matches("Bearer ").to_owned();
        self.calls.lock().unwrap().push(call);
        Reply {
            status: "200 OK",
            retry_after: None,
            kind: "image/png",
            cookie: None,
            body: b"\x89PNG\r\n\x1a\nnot really".to_vec(),
        }
    }

    fn api(&self, call: &Call) -> Reply {
        let token = call.token.as_str();
        if self.revoked.lock().unwrap().iter().any(|revoked| revoked == token) {
            return refused("invalid_auth");
        }
        if let Some(error) = self.refusing.lock().unwrap().get(&call.method) {
            return refused(error);
        }
        let param = |name: &str| call.params.get(name).map(String::as_str).unwrap_or("");
        let channel = param("channel");
        let busy = token == ALPHA && self.busy.load(Ordering::Relaxed);
        // What Slack tells every client of the workspace once it has done what was asked.
        let echo = |event: Value| {
            let socket = self.sockets.lock().unwrap().get(token).cloned();
            std::thread::spawn(move || {
                sleep(Duration::from_millis(300));
                if let Some(socket) = socket {
                    let _ = socket.send(event.to_string());
                }
            });
        };
        let found = match call.method.as_str() {
            "chat.postMessage" => return self.post(call),
            method @ ("reactions.add" | "reactions.remove") => {
                if param("name") == "no_entry_sign" {
                    return refused("too_many_reactions");
                }
                echo(json!({
                    "type": if method == "reactions.add" { "reaction_added" } else { "reaction_removed" },
                    "user": me(token), "reaction": param("name"), "item_user": "U0ANA",
                    "item": {"type": "message", "channel": channel, "ts": param("timestamp")}, "event_ts": "1760002000.000000",
                }));
                Some(json!({"ok": true}))
            }
            "chat.update" => {
                let message = json!({
                    "type": "message", "user": me(token), "team": team(token), "text": param("text"),
                    "edited": {"user": me(token), "ts": "1760002001.000000"},
                });
                let mut changed = message.clone();
                changed["ts"] = param("ts").into();
                echo(json!({
                    "type": "message", "subtype": "message_changed", "hidden": true, "channel": channel,
                    "ts": "1760002001.000000", "event_ts": "1760002001.000000", "message": changed,
                }));
                Some(json!({"ok": true, "channel": channel, "ts": param("ts"), "text": param("text"), "message": message}))
            }
            "chat.delete" => {
                echo(json!({
                    "type": "message", "subtype": "message_deleted", "hidden": true, "channel": channel,
                    "deleted_ts": param("ts"), "ts": "1760002002.000000", "event_ts": "1760002002.000000",
                }));
                Some(json!({"ok": true, "channel": channel, "ts": param("ts")}))
            }
            "users.setPresence" | "users.prefs.setNotifications" => Some(json!({"ok": true})),
            "files.getUploadURLExternal" => {
                let id = format!("F0UP{}", param("filename").len());
                Some(json!({"ok": true, "upload_url": format!("{}/upload/v1/{id}", self.base), "file_id": id}))
            }
            "files.completeUploadExternal" => {
                // Slack shares the files, then tells every client of the message holding them.
                let files: Value = serde_json::from_str(param("files")).unwrap();
                let shared: Vec<Value> = files
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|file| json!({"id": file["id"], "name": file["title"], "title": file["title"], "mimetype": "text/plain", "size": 3,
                                        "url_private": format!("https://files.slack.com/files-pri/{}-{}/f", team(token), file["id"].as_str().unwrap())}))
                    .collect();
                let number = self.posted.fetch_add(1, Ordering::Relaxed) + 1;
                let ts = format!("17600020{number:02}.{number:06}");
                let mut event = json!({"type": "message", "subtype": "file_share", "user": me(token), "channel": param("channel_id"),
                                       "text": param("initial_comment"), "ts": ts, "files": shared, "upload": true});
                if !param("thread_ts").is_empty() {
                    event["thread_ts"] = param("thread_ts").into();
                }
                echo(event);
                Some(json!({"ok": true, "files": files}))
            }
            "chat.scheduleMessage" => Some(json!({
                "ok": true, "channel": channel, "scheduled_message_id": "Q0SCHED", "post_at": param("post_at").parse::<i64>().unwrap(),
                "message": {"type": "message", "user": me(token), "text": param("text"), "bot_id": null},
            })),
            "users.profile.set" => {
                let asked: Value = serde_json::from_str(param("profile")).unwrap();
                let mut profile = self.user(token, me(token)).unwrap()["profile"].clone();
                profile["status_text"] = asked["status_text"].clone();
                // Slack's default for a status with words and no emoji.
                let emoji = if asked["status_text"] == "" { "" } else { ":speech_balloon:" };
                profile["status_emoji"] = emoji.into();
                Some(json!({"ok": true, "username": "justin", "profile": profile}))
            }
            "client.counts" if busy => {
                let mut counts = self.fixture(token, "client.counts").unwrap();
                for (at, id) in BUSY.iter().enumerate() {
                    let latest = format!("17600004{at:02}.000000");
                    counts["ims"].as_array_mut().unwrap().push(json!({
                        "id": id, "last_read": "1760000000.000000", "latest": latest, "updated": latest,
                        "history_invalid": "0", "mention_count": 1, "has_unreads": true,
                    }));
                }
                Some(counts)
            }
            "users.conversations" if busy => {
                let mut listed = self.fixture(token, "users.conversations").unwrap();
                for id in BUSY {
                    listed["channels"].as_array_mut().unwrap().push(json!({
                        "id": id, "created": 1700000000, "is_im": true, "is_org_shared": false, "user": "U0ZOE",
                    }));
                }
                Some(listed)
            }
            "conversations.history" if BUSY.contains(&channel) => Some(json!({"ok": true, "has_more": false, "messages": []})),
            "conversations.members" => {
                // Slack may answer fewer than asked for: two at a time here, so a list of four
                // takes two calls and the cursor is seen at work.
                let listed = self.fixture(token, &format!("conversations.members.{channel}"));
                listed.map(|listed| {
                    let all = listed["members"].as_array().unwrap();
                    let from: usize = param("cursor").parse().unwrap_or(0);
                    let page: Vec<&Value> = all.iter().skip(from).take(2).collect();
                    let next = if from + 2 < all.len() { (from + 2).to_string() } else { String::new() };
                    json!({"ok": true, "members": page, "response_metadata": {"next_cursor": next}})
                })
            }
            "conversations.mark" if token == ALPHA => {
                let mut marked = ok(json!({"ok": true}));
                marked.cookie = self.reissue.lock().unwrap().take();
                return marked;
            }
            "conversations.mark" | "subscriptions.thread.mark" => Some(json!({"ok": true})),
            "conversations.open" => Some(json!({
                "ok": true, "no_op": false, "already_open": false,
                "channel": {"id": "D0NEW", "created": 1760000000, "is_im": true, "is_org_shared": false,
                            "user": param("users"), "is_open": true, "priority": 0},
            })),
            "users.info" => self.user(token, param("user")).map(|user| json!({"ok": true, "user": user})),
            "conversations.info" => {
                let listed = self.fixture(token, "users.conversations").unwrap();
                let known = listed["channels"].as_array().unwrap().iter().find(|known| known["id"] == channel).cloned();
                known.map(|mut known| {
                    if param("include_num_members") == "true" {
                        known["num_members"] = 4.into();
                    }
                    json!({"ok": true, "channel": known})
                })
            }
            "conversations.history" => {
                let page = match (param("oldest"), param("inclusive"), param("latest")) {
                    ("", "true", _) => ".around",
                    ("", _, "") => "",
                    ("", ..) => ".older",
                    _ => ".after",
                };
                self.fixture(token, &format!("conversations.history.{channel}{page}"))
            }
            "conversations.replies" => self.fixture(token, &format!("conversations.replies.{channel}")),
            method => self.fixture(token, method),
        };
        found.map_or_else(|| refused("unknown_method"), ok)
    }

    /// `chat.postMessage`: accepts, names the message, and echoes it on the socket the way Slack
    /// tells every client of a new message. A few texts ask for the ways a send goes wrong.
    fn post(&self, call: &Call) -> Reply {
        let param = |name: &str| call.params.get(name).cloned().unwrap_or_default();
        let (token, text) = (call.token.clone(), param("text"));
        match text.as_str() {
            "limit me" => return limited(1),
            "limit once" if !self.limited.swap(true, Ordering::Relaxed) => return limited(2),
            "not here" => return refused("not_in_channel"),
            _ => {}
        }
        let number = self.posted.fetch_add(1, Ordering::Relaxed) + 1;
        let ts = format!("17600010{number:02}.{number:06}");
        let mut message = json!({
            "type": "message", "user": me(&token), "team": team(&token), "text": text, "ts": ts,
            "client_msg_id": param("client_msg_id"),
        });
        if !param("thread_ts").is_empty() {
            message["thread_ts"] = param("thread_ts").into();
        }
        if param("reply_broadcast") == "true" {
            message["subtype"] = "thread_broadcast".into();
        }
        let mut event = message.clone();
        event["channel"] = param("channel").into();
        event["event_ts"] = ts.clone().into();
        if text.starts_with("slow") {
            // The socket is faster than the call's own answer.
            self.push(&token, event);
            sleep(Duration::from_secs(5));
        } else {
            let sockets = self.sockets.lock().unwrap().get(&token).cloned();
            std::thread::spawn(move || {
                sleep(Duration::from_millis(100));
                if let Some(socket) = sockets {
                    let _ = socket.send(event.to_string());
                }
            });
        }
        ok(json!({"ok": true, "channel": param("channel"), "ts": ts, "message": message}))
    }

    /// The web client's cache service. The second workspace refuses it, as a workspace that
    /// does not offer it would, so its people are looked up the documented way.
    fn edge(&self, token: &str, method: &str, asked: &Value) -> Reply {
        if self.revoked.lock().unwrap().iter().any(|revoked| revoked == token) {
            return refused("invalid_auth");
        }
        if token == BETA {
            return refused("not_allowed_token_type");
        }
        let users = self.fixture(token, "users").unwrap();
        let users = users.as_array().unwrap();
        match method {
            "users/info" => {
                let wanted = asked["updated_ids"].as_object().unwrap();
                let results: Vec<&Value> = users.iter().filter(|user| wanted.contains_key(user["id"].as_str().unwrap())).collect();
                ok(json!({"ok": true, "results": results, "failed_ids": [], "pending_ids": [], "can_interact": {}}))
            }
            "users/search" => {
                let query = asked["query"].as_str().unwrap().to_lowercase();
                let matches = |user: &&Value| user["real_name"].as_str().unwrap().to_lowercase().contains(&query);
                let results: Vec<&Value> = users.iter().filter(matches).collect();
                ok(json!({"ok": true, "results": results, "presence_active_ids": []}))
            }
            _ => refused("unknown_method"),
        }
    }

    // The websocket.

    // The handshake callback's error type is tungstenite's to choose.
    #[allow(clippy::result_large_err)]
    fn socket(self: Arc<Fake>, stream: TcpStream) {
        let mut shake = None;
        let heard = |request: &Upgrade, response: Upgraded| {
            let header = |name: &str| request.headers().get(name).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
            let query = request.uri().query().unwrap_or("");
            let token = query.split('&').find_map(|pair| pair.strip_prefix("token=")).unwrap_or("");
            shake = Some(Shake {
                token: token.to_owned(),
                user_agent: header("user-agent"),
                cookie: header("cookie"),
            });
            Ok::<_, tungstenite::handshake::server::ErrorResponse>(response)
        };
        let Ok(mut socket) = tungstenite::accept_hdr(stream, heard) else {
            return;
        };
        let shake = shake.unwrap();
        let token = shake.token.clone();
        self.shakes.lock().unwrap().push(shake);
        socket.get_ref().set_read_timeout(Some(Duration::from_millis(25))).unwrap();
        let (frames, queued) = channel();
        self.sockets.lock().unwrap().insert(token.clone(), frames);
        let _ = socket.send(tungstenite::Message::text(r#"{"type":"hello"}"#));
        loop {
            for frame in queued.try_iter() {
                if socket.send(tungstenite::Message::text(frame)).is_err() {
                    return;
                }
            }
            match socket.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                    if frame["type"] == "ping" {
                        let pong = json!({"type": "pong", "reply_to": frame["id"]});
                        let _ = socket.send(tungstenite::Message::text(pong.to_string()));
                    }
                    self.heard.lock().unwrap().push((token.clone(), frame));
                }
                Ok(tungstenite::Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
    }

    /// Sends an event to the workspace's socket, as Slack would.
    fn push(&self, token: &str, event: Value) {
        let sockets = self.sockets.lock().unwrap();
        sockets.get(token).expect("chatd has a socket open for the workspace").send(event.to_string()).unwrap();
    }

    /// Every file fetched from the fake, by path.
    fn files(&self) -> Vec<Call> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|call| call.method.starts_with("file:")).cloned().collect()
    }

    fn file(&self, path: &str) -> Option<Call> {
        self.files().into_iter().find(|call| call.method == format!("file:{path}"))
    }

    /// How often each method was asked of a workspace.
    fn counted(&self, token: &str) -> Vec<(String, usize)> {
        let mut counted: Vec<(String, usize)> = Vec::new();
        for call in self.calls.lock().unwrap().iter().filter(|call| call.token == token) {
            match counted.iter_mut().find(|(method, _)| *method == call.method) {
                Some((_, count)) => *count += 1,
                None => counted.push((call.method.clone(), 1)),
            }
        }
        counted.sort();
        counted
    }

    fn calls(&self, token: &str, method: &str) -> Vec<Call> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|call| call.token == token && call.method == method).cloned().collect()
    }

    /// Frames of one type chatd sent on a workspace's socket.
    fn heard(&self, token: &str, kind: &str) -> Vec<Value> {
        let heard = self.heard.lock().unwrap();
        heard.iter().filter(|(from, frame)| from == token && frame["type"] == kind).map(|(_, frame)| frame.clone()).collect()
    }
}

/// A fake Slack and a chatd signed in to its two workspaces, in a scratch directory.
struct World {
    fake: Arc<Fake>,
    socket_url: String,
    child: Child,
    dir: PathBuf,
}

/// Two agents name the same browser when they differ in nothing but its version. The media
/// cache presents the Safari that is installed (`net::browser`), which is the session's own
/// browser on the user's machine and may be a newer one than this file's on the machine the
/// tests run on.
fn same_browser(a: &str, b: &str) -> bool {
    let without_version = |agent: &str| {
        let parts = agent.split(' ').filter(|part| !part.starts_with("Version/"));
        parts.collect::<Vec<_>>().join(" ")
    };
    !a.is_empty() && without_version(a) == without_version(b)
}

fn alpha(id: &str) -> Key {
    Key {
        network: Network::Slack,
        account: "T01ALPHA".to_owned(),
        id: id.to_owned(),
    }
}

fn beta(id: &str) -> Key {
    Key {
        network: Network::Slack,
        account: "T02BETA".to_owned(),
        id: id.to_owned(),
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// The digits chatd wrapped in a `client_msg_id`: a version 4 UUID in form whose remaining hex
/// digits, after the leading `f`s, are the shell's `client_id`.
fn client_id_in(client_msg_id: &str) -> String {
    let groups: Vec<&str> = client_msg_id.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    assert_eq!(lengths, [8, 4, 4, 4, 12], "{client_msg_id} is shaped like a UUID");
    assert!(groups[2].starts_with('4') && groups[3].starts_with('8'), "{client_msg_id} is a version 4 UUID");
    let hex = format!("{}{}{}{}{}", groups[0], groups[1], &groups[2][1..], &groups[3][1..], groups[4]);
    hex.trim_start_matches('f').to_owned()
}

impl World {
    fn start() -> World {
        World::start_with(|_| {})
    }

    /// `setup` arranges the fake before chatd first connects to it.
    fn start_with(setup: impl FnOnce(&Fake)) -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let (fake, socket_url) = Fake::start();
        setup(&fake);
        // Short path: a unix socket address holds about 100 bytes. On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!(
            "/tmp/tinyclient-slack-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let child = World::spawn(&fake, &socket_url, &dir);
        World { fake, socket_url, child, dir }
    }

    fn spawn(fake: &Fake, socket_url: &str, dir: &PathBuf) -> Child {
        // What the sign-in helper prints for a browser session holding two workspaces.
        let session = json!({
            "cookie": COOKIE, "user_agent": agent(), "active": "T01ALPHA",
            "teams": [
                {"id": "T01ALPHA", "name": "Acme", "url": "https://acme.slack.com/", "token": ALPHA, "enterprise_id": ""},
                {"id": "T02BETA", "name": "Side Project", "url": "https://side.slack.com/", "token": BETA, "enterprise_id": ""},
            ],
        });
        let log = if std::env::var_os("TINYCLIENT_TEST_LOG").is_some() { Stdio::inherit() } else { Stdio::null() };
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .env("TINYCLIENT_DIR", dir)
            .env("TINYCLIENT_SESSION_SLACK", session.to_string())
            .env("TINYCLIENT_SLACK_API", format!("{}/", fake.base))
            .env("TINYCLIENT_SLACK_SOCKET", socket_url)
            // The fake also stands in for Slack's file hosts.
            .env("TINYCLIENT_MEDIA_HOSTS", "127.0.0.1")
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .stderr(log)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        child
    }

    /// Quits chatd and runs it again on what it left on disk.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.dir.join("chat.sock"));
        self.fake.sockets.lock().unwrap().clear();
        self.child = World::spawn(&self.fake, &self.socket_url, &self.dir);
    }

    /// Asks for files as the shell does for what is on screen, until `id` is on disk.
    fn fetched(&self, account: &Key, id: &str, size: Option<u32>) -> protocol::MediaFile {
        self.until("the file to be on disk", || {
            let asked = Request::Fetch { retry: false, size, account: account.account(), ids: vec![id.to_owned()] };
            match self.ask(asked) {
                Response::Media { mut files } => files.pop().filter(|file| file.path.is_some()),
                other => panic!("{other:?}"),
            }
        })
    }

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    /// Asks again until `look` finds what it is waiting for.
    fn until<T>(&self, what: &str, look: impl FnMut() -> Option<T>) -> T {
        self.within(Duration::from_secs(20), what, look)
    }

    fn within<T>(&self, limit: Duration, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(found) = look() {
                return found;
            }
            if Instant::now() > deadline {
                let calls = self.fake.calls.lock().unwrap();
                let asked: Vec<String> = calls.iter().map(|call| format!("{} {}", team(&call.token), call.method)).collect();
                panic!("waited {limit:?} for {what}; Slack was asked: {asked:#?}");
            }
            sleep(Duration::from_millis(40));
        }
    }

    fn accounts(&self) -> Vec<protocol::AccountStatus> {
        match self.ask(Request::Status) {
            Response::Status { accounts, .. } => accounts,
            other => panic!("{other:?}"),
        }
    }

    fn account(&self, id: &str) -> protocol::AccountStatus {
        self.accounts().into_iter().find(|account| account.key.account == id).expect("the account is listed")
    }

    fn conversation(&self, conversation: &Key) -> Option<Conversation> {
        match self.ask(Request::Conversation { conversation: conversation.clone() }) {
            Response::Conversation { conversation } => Some(conversation),
            _ => None,
        }
    }

    fn tree(&self, space: &Key) -> Option<(Vec<Group>, Vec<Conversation>)> {
        match self.ask(Request::Tree { space: space.clone(), offset: 0, limit: 500, peek: false }) {
            Response::Tree { groups, conversations, .. } => Some((groups, conversations)),
            _ => None,
        }
    }

    /// The newest page of a conversation; empty until chatd knows the conversation.
    fn messages(&self, conversation: &Key) -> Vec<Message> {
        let asked = Request::Messages { conversation: conversation.clone(), page: Page::Newest, limit: 100 };
        match self.ask(asked) {
            Response::Messages { messages, .. } => messages,
            _ => Vec::new(),
        }
    }

    fn message(&self, conversation: &Key, id: &str) -> Option<Message> {
        self.messages(conversation).into_iter().find(|message| message.id == id)
    }

    /// Both workspaces connected, and the first one's sidebar and unread state loaded.
    fn ready(&self) {
        self.until("both workspaces to connect", || {
            let connected = self.accounts().iter().filter(|account| account.link == Link::Connected).count();
            (connected == 2).then_some(())
        });
        self.until("the sidebar and Slack's unread state", || {
            self.conversation(&alpha("C0DEP")).filter(|row| row.mentions == 2).map(|_| ())
        });
    }

    /// `#general` with its newest page fetched.
    fn general(&self) -> Vec<Message> {
        self.until("the newest page of #general", || {
            Some(self.messages(&alpha("C0GEN"))).filter(|messages| messages.len() >= 7)
        })
    }

    fn send(&self, conversation: &Key, client_id: &str, text: &str) -> Message {
        let asked = Request::Send {
            conversation: conversation.clone(),
            client_id: client_id.to_owned(),
            text: text.to_owned(),
            reply_to: None, with: Default::default(),
        };
        match self.ask(asked) {
            Response::Sent { message } => message,
            other => panic!("{other:?}"),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(self.dir.with_extension("files"));
    }
}

fn ranges(message: &Message) -> Vec<(&str, &Style)> {
    let cut = |span: &protocol::Span| &message.text[span.start as usize..span.end as usize];
    message.spans.iter().map(|span| (cut(span), &span.style)).collect()
}

#[test]
fn one_sign_in_is_two_workspaces_each_with_its_sidebar_and_unread_state() {
    let world = World::start();
    world.ready();

    // Two workspaces of one sign-in are two accounts, each named and each knowing who I am.
    let accounts = world.accounts();
    let listed: Vec<(&str, &str, &str, Link)> = accounts
        .iter()
        .map(|account| (account.key.account.as_str(), account.name.as_str(), account.me.as_str(), account.link))
        .collect();
    assert_eq!(
        listed,
        [("T01ALPHA", "Acme", "U0ME", Link::Connected), ("T02BETA", "Side Project", "U0ME2", Link::Connected)]
    );
    let Response::Spaces { spaces } = world.ask(Request::Spaces { network: Some(Network::Slack) }) else {
        panic!("no spaces");
    };
    let named: Vec<(&str, &str)> = spaces.iter().map(|space| (space.key.id.as_str(), space.name.as_str())).collect();
    assert_eq!(named, [("T01ALPHA", "Acme"), ("T02BETA", "Side Project")]);
    assert_eq!(spaces[0].icon.as_ref().map(|icon| icon.kind), Some(MediaKind::Icon));
    assert_eq!(spaces[1].icon, None, "a workspace with no icon of its own has none");

    // The Space's tree is the sidebar: the user's own section first, then Slack's, in the order
    // the sections chain together, not the order they were listed in.
    let (groups, rows) = world.tree(&alpha("T01ALPHA")).unwrap();
    let sections: Vec<(&str, GroupKind, u32)> = groups.iter().map(|group| (group.name.as_str(), group.kind, group.count)).collect();
    assert_eq!(
        sections,
        [("Shipping", GroupKind::Section, 1), ("Channels", GroupKind::Section, 3), ("Direct messages", GroupKind::Directs, 3)],
        "the channel sections, then every direct conversation under the one group that is theirs"
    );
    let shown: Vec<(&str, &str, ConversationKind, &str)> = rows
        .iter()
        .map(|row| (row.key.id.as_str(), row.title.as_str(), row.kind, row.group.as_deref().unwrap()))
        .collect();
    assert_eq!(
        shown,
        [
            ("C0DEP", "#deploys", ConversationKind::Channel, "S0SHIP"),
            ("C0GEN", "#general", ConversationKind::Channel, "S0CHAN"),
            ("C0RND", "#random", ConversationKind::Channel, "S0CHAN"),
            ("C0SIDE", "#sidecar", ConversationKind::Channel, "S0CHAN"),
            ("D0ANA", "Ana", ConversationKind::Direct, "directs"),
            ("G0GRP", "ana, ben", ConversationKind::Group, "directs"),
            ("D0BOT", "Deploy Bot", ConversationKind::Direct, "directs"),
        ],
        "a direct conversation Slack no longer shows (D0OLD) is not in the sidebar"
    );
    let row = |id: &str| rows.iter().find(|row| row.key.id == id).unwrap();
    assert_eq!(row("D0ANA").peer.as_deref(), Some("U0ANA"));
    assert_eq!(row("D0ANA").avatar.as_ref().map(|avatar| (avatar.kind, avatar.width)), Some((MediaKind::Avatar, 72)));
    assert!(rows.iter().all(|row| row.call == CallSupport::Handoff && row.space.as_deref() == Some("T01ALPHA")));
    // What a row says of itself: where it is, what it is for, who may see it and post in it.
    assert_eq!(row("C0GEN").subtitle(), "Acme › Channels · Company-wide");
    assert_eq!(row("C0SIDE").topic, "Announcements only", "the purpose, where no topic is set");
    let flags = |id: &str| (row(id).private, row(id).read_only);
    assert_eq!((flags("C0GEN"), flags("C0DEP"), flags("C0SIDE"), flags("D0ANA")), ((false, false), (true, false), (false, true), (true, false)));
    // The direct conversations are in the Inbox as well, whatever their Space.
    let Response::Conversations { conversations: inbox, .. } =
        world.ask(Request::Inbox { network: Some(Network::Slack), filter: InboxFilter::Directs, offset: 0, limit: 50 })
    else {
        panic!("no inbox");
    };
    let mut direct: Vec<&str> = inbox.iter().map(|row| row.key.id.as_str()).collect();
    direct.sort_unstable();
    // The bot's and the group's conversations are read and quiet for over 30 days, so they are
    // not listed (polish rule I2) while they stay in the workspace's tree.
    assert_eq!(direct, ["D0ANA"]);
    let month_ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 30 * 86_400;
    for quiet in ["D0BOT", "G0GRP"] {
        assert!(row(quiet).updated < month_ago && row(quiet).unread == 0 && row(quiet).mentions == 0, "{:?}", row(quiet));
    }

    // Unread, mentions and mute are Slack's own: `client.counts` and the muted-channels pref.
    let state = |id: &str| (row(id).unread, row(id).mentions, row(id).muted, row(id).badge());
    assert_eq!(state("C0GEN"), (1, 0, false, Badge::Dot));
    assert_eq!(state("C0DEP"), (2, 2, false, Badge::Count(2)));
    assert_eq!(state("C0RND"), (1, 0, true, Badge::None));
    assert_eq!(state("D0ANA"), (3, 3, false, Badge::Count(3)));
    assert_eq!(state("G0GRP"), (0, 0, false, Badge::None));
    assert_eq!(row("C0GEN").last_read.as_deref(), Some("1760000100.000100"));
    assert_eq!(world.account("T01ALPHA").attention, 5);
    assert_eq!(world.account("T02BETA").attention, 0);

    // The second workspace has no sections of its own, so it gets Slack's default.
    let (groups, rows) = world.tree(&beta("T02BETA")).unwrap();
    assert_eq!(groups.iter().map(|group| group.name.as_str()).collect::<Vec<_>>(), ["Channels"]);
    assert_eq!(rows.iter().map(|row| row.title.as_str()).collect::<Vec<_>>(), ["#main"]);

    // The list I own on Slack is the people I have direct conversations with.
    let asked = Request::People { network: Some(Network::Slack), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 };
    let Response::People { people, .. } = world.ask(asked) else {
        panic!("no people");
    };
    let partners: Vec<(&str, bool, &str)> = people.iter().map(|person| (person.name.as_str(), person.bot, person.status.as_str())).collect();
    // A bot is never among people (polish rule P2), though its DM is a contact's.
    assert_eq!(partners, [("Ana", false, "🎧 Heads down")]);

    // Every request and both sockets carried the browser the user signed in with, and the cookie.
    let calls = world.fake.calls.lock().unwrap().clone();
    for token in [ALPHA, BETA] {
        assert!(calls.iter().any(|call| call.token == token && call.method == "client.counts"));
    }
    for call in &calls {
        assert_eq!(call.user_agent, agent(), "{} was asked as another browser", call.method);
        assert_eq!(call.cookie, format!("d={COOKIE}"), "{}", call.method);
    }
    let shakes = world.fake.shakes.lock().unwrap().clone();
    let mut sockets: Vec<&str> = shakes.iter().map(|shake| shake.token.as_str()).collect();
    sockets.sort_unstable();
    assert_eq!(sockets, [ALPHA, BETA], "each workspace has a socket of its own");
    for shake in &shakes {
        assert_eq!((shake.user_agent.as_str(), shake.cookie.as_str()), (agent(), format!("d={COOKIE}").as_str()));
    }
    // Connecting listed the sidebar and nothing heavier, and fetched no history nobody needs.
    // The socket's address is derived, as the web client derives it, not asked for.
    let asked = |method: &str| calls.iter().any(|call| call.method == method);
    assert!(!asked("client.userBoot") && !asked("users.list") && !asked("conversations.list") && !asked("rtm.connect"));
    let fetched = |channel: &str| world.fake.calls(ALPHA, "conversations.history").iter().any(|call| call.params["channel"] == channel);
    assert!(!fetched("C0GEN") && !fetched("C0RND"));

    // Read on another device: the number goes without anyone asking Slack again.
    world.fake.push(ALPHA, json!({
        "type": "channel_marked", "channel": "C0DEP", "ts": "1760000310.000310",
        "unread_count": 0, "unread_count_display": 0, "num_mentions": 0, "num_mentions_display": 0,
        "mention_count": 0, "mention_count_display": 0, "event_ts": "1760000400.000000",
    }));
    let read = world.until("#deploys to be read elsewhere", || {
        world.conversation(&alpha("C0DEP")).filter(|row| row.mentions == 0)
    });
    assert_eq!((read.unread, read.last_read.as_deref()), (0, Some("1760000310.000310")));
    assert_eq!(world.account("T01ALPHA").attention, 3);

    // Muting and unmuting in Slack arrives as a pref change.
    world.fake.push(ALPHA, json!({"type": "pref_change", "name": "muted_channels", "value": "C0GEN", "event_ts": "1760000401.000000"}));
    world.until("#general to be muted", || world.conversation(&alpha("C0GEN")).filter(|row| row.muted));
    assert!(!world.conversation(&alpha("C0RND")).unwrap().muted, "what left the list is unmuted");

    // Reading here tells Slack, at that message.
    let read = Request::MarkRead { conversation: alpha("C0GEN"), message_id: "1760000300.000300".to_owned() };
    assert_eq!(world.ask(read), Response::Ok);
    let marked = world.until("Slack to be told #general was read", || {
        world.fake.calls(ALPHA, "conversations.mark").pop()
    });
    assert_eq!((marked.params["channel"].as_str(), marked.params["ts"].as_str()), ("C0GEN", "1760000300.000300"));
    assert_eq!(world.conversation(&alpha("C0GEN")).unwrap().unread, 0);
}

#[test]
fn a_channel_reads_as_it_does_in_slack_and_stays_current() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let messages = world.general();
    let ids: Vec<&str> = messages.iter().map(|message| message.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "1760000090.000090", "1760000100.000100", "1760000110.000110", "1760000120.000120",
            "1760000150.000150", ROOT, "1760000300.000300"
        ],
        "oldest first, and the thread's replies are not in the channel"
    );
    let by_id = |id: &str| messages.iter().find(|message| message.id == id).unwrap();

    // Rich text, a mention of me, an image and reactions.
    let shipping = by_id("1760000300.000300");
    assert_eq!(shipping.text, "Shipping today 🚀 @Justin");
    assert!(ranges(shipping).contains(&("today", &Style::Bold)));
    let me_named = Style::Mention { kind: MentionKind::User, id: "U0ME".to_owned() };
    assert!(ranges(shipping).contains(&("@Justin", &me_named)));
    assert!(shipping.mentions_me && !shipping.outgoing);
    assert_eq!((shipping.sender.id.as_str(), shipping.sender.name.as_str()), ("U0ANA", "Ana"));
    let avatar = shipping.sender.avatar.as_ref().expect("Ana's picture is Slack's own and is kept");
    assert_eq!((avatar.id.as_str(), avatar.kind), ("avatar:U0ANA:a1b2c3", MediaKind::Avatar));
    let [image] = &shipping.attachments[..] else {
        panic!("one attachment");
    };
    assert_eq!((image.name.as_str(), image.mime.as_str()), ("diagram.png", "image/png"));
    assert_eq!((image.file.id.as_str(), image.file.kind), ("F0IMG", MediaKind::Image));
    assert_eq!((image.file.width, image.file.height, image.file.bytes), (1600, 900, 48211));
    let thumb = image.thumb.as_ref().unwrap();
    assert_eq!((thumb.width, thumb.height, thumb.kind), (480, 270, MediaKind::Image), "the size a row shows at 1x");
    let reactions: Vec<(&str, u32, bool, bool)> = shipping
        .reactions
        .iter()
        .map(|reaction| (reaction.emoji.as_str(), reaction.count, reaction.mine, reaction.file.is_some()))
        .collect();
    // A custom emoji shows by name: the workspace's list is read when the picker asks for
    // it, never because a message arrived.
    assert_eq!(reactions, [("🎉", 2, true, false), (":partyparrot:", 1, false, false)]);
    assert!(world.fake.calls(ALPHA, "emoji.list").is_empty());

    // Its files are registered with their sizes before any byte is fetched. Only the upload is
    // asked for here: its address is the fake's, and the avatar's and emoji's are Slack's own.
    let wanted = vec![image.file.id.clone(), thumb.id.clone(), "F0NOPE".to_owned()];
    let Response::Media { files } = world.ask(Request::Fetch { retry: false, size: None, account: general.account(), ids: wanted }) else {
        panic!("no media");
    };
    let known: Vec<(&str, MediaKind, u32, u32)> = files.iter().map(|file| (file.id.as_str(), file.kind, file.width, file.height)).collect();
    assert_eq!(known, [("F0IMG", MediaKind::Image, 1600, 900), ("F0IMG:thumb", MediaKind::Image, 480, 270)]);

    // The thumbnail is fetched at the size it is drawn: as registered when the shell names no
    // size or a small one, and the smallest of Slack's other sizes that is large enough for a
    // 2x screen (400 points wide is 800 pixels), never one that would be blurry.
    let small = world.fetched(&general, "F0IMG:thumb", Some(400));
    assert!(world.fake.file("/files-tmb/T01ALPHA-F0IMG-0a1b2c/diagram_480.png").is_some());
    let sharp = world.fetched(&general, "F0IMG:thumb", Some(800));
    assert!(world.fake.file("/files-tmb/T01ALPHA-F0IMG-0a1b2c/diagram_960.png").is_some(), "{:?}", world.fake.files());
    assert_eq!((small.id.as_str(), sharp.id.as_str()), ("F0IMG:thumb", "F0IMG:thumb"), "one file, whatever its size");
    assert!(world.fake.file("/files-tmb/T01ALPHA-F0IMG-0a1b2c/diagram_720.png").is_none(), "too small for 800, so never asked for");
    assert!(world.fake.file("/files-tmb/T01ALPHA-F0IMG-0a1b2c/diagram_1024.png").is_none(), "larger than needed");
    // A screen that wants more than Slack made a thumbnail for gets the picture itself.
    world.fetched(&general, "F0IMG:thumb", Some(1500));
    world.until("the picture itself", || world.fake.files().into_iter().filter(|call| call.method == "file:/files-pri/T01ALPHA-F0IMG/diagram.png").nth(1));

    // The first message of a thread says how much hangs off it.
    let thread = by_id(ROOT).thread.as_ref().unwrap();
    assert_eq!((thread.id.as_str(), thread.replies, thread.last), ("C0GEN:1760000200.000200", 2, 1760000260));
    let faces: Vec<&str> = thread.participants.iter().map(|person| person.id.as_str()).collect();
    assert_eq!(faces, ["U0ME", "U0ANA"], "who replied, the latest first");

    // A bot's card is a card, a join is one grey line, an edit is marked.
    let deploy = by_id("1760000150.000150");
    assert!(deploy.sender.bot && deploy.sender.name == "Deploy Bot" && deploy.kind == MessageKind::Normal);
    let card = deploy.card.as_deref().expect("the bot's attachment is drawn as a card");
    assert_eq!((deploy.text.as_str(), card.title.as_str(), card.text.as_str()), ("", "Deploy #42 succeeded", "main → prod in 4 min"));
    assert_eq!((card.fields.as_slice(), card.color), (&[("Commit".to_owned(), "abc123".to_owned())][..], Some(0x2e_b886)));
    let joined = by_id("1760000120.000120");
    assert_eq!((joined.kind.clone(), joined.text.as_str()), (MessageKind::System, "@Ben Ito has joined the channel"));
    let mine = by_id("1760000100.000100");
    assert!(mine.edited && mine.outgoing && mine.text == "morning");

    // A link's card is Slack's; its picture is kept only when Slack serves it.
    let linked = by_id("1760000110.000110");
    assert_eq!(linked.text, "https://example.com/post");
    assert!(ranges(linked).contains(&("https://example.com/post", &Style::Link { url: "https://example.com/post".to_owned() })));
    let [post, other] = &linked.previews[..] else {
        panic!("two previews");
    };
    assert_eq!((post.url.as_str(), post.site.as_str(), post.title.as_str()), ("https://example.com/post", "Example", "A post"));
    assert_eq!(post.description, "What we learned shipping weekly.");
    assert_eq!(post.image.as_ref().map(|image| (image.width, image.height)), Some((800, 418)));
    assert_eq!(other.image, None);

    // Buttons are not drawn: one line and the way to the real thing.
    let buttons = by_id("1760000090.000090");
    let link = "https://acme.slack.com/archives/C0GEN/p1760000090000090".to_owned();
    assert_eq!(buttons.kind, MessageKind::Unsupported { link });
    assert_eq!(buttons.text, "An interactive message from Deploy Bot");

    // Live: a message with a photo arrives on the socket.
    let before = world.conversation(&general).unwrap().unread;
    let photo = json!({
        "id": "F0NEW", "name": "whiteboard.jpg", "title": "whiteboard.jpg", "mimetype": "image/jpeg", "filetype": "jpg",
        "size": 91822, "mode": "hosted", "original_w": 1200, "original_h": 900,
        "url_private": "https://files.slack.com/files-pri/T01ALPHA-F0NEW/whiteboard.jpg",
        "thumb_360": "https://files.slack.com/files-tmb/T01ALPHA-F0NEW-9f/whiteboard_360.jpg", "thumb_360_w": 360, "thumb_360_h": 270,
        "thumb_480": "https://files.slack.com/files-tmb/T01ALPHA-F0NEW-9f/whiteboard_480.jpg", "thumb_480_w": 480, "thumb_480_h": 360,
    });
    let photo: Value = serde_json::from_str(&photo.to_string().replace("https://files.slack.com", &world.fake.base)).unwrap();
    let live = "1760000500.000500";
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "file_share", "channel": "C0GEN", "user": "U0BEN", "text": "from the offsite",
        "files": [photo], "upload": true, "ts": live, "event_ts": live, "channel_type": "channel",
    }));
    let arrived = world.until("the photo to arrive", || world.message(&general, live));
    assert_eq!((arrived.text.as_str(), arrived.sender.name.as_str()), ("from the offsite", "Ben Ito"));
    let shown = arrived.attachments[0].thumb.as_ref().unwrap();
    assert_eq!((arrived.attachments[0].file.width, arrived.attachments[0].file.height), (1200, 900));
    assert_eq!((shown.id.as_str(), shown.width, shown.height), ("F0NEW:thumb", 480, 360));
    let row = world.conversation(&general).unwrap();
    assert_eq!((row.unread, row.preview.as_str()), (before + 1, "from the offsite"));

    // Reactions come and go one person at a time.
    let react = |kind: &str, user: &str| {
        world.fake.push(ALPHA, json!({
            "type": kind, "user": user, "reaction": "eyes", "item_user": "U0BEN",
            "item": {"type": "message", "channel": "C0GEN", "ts": live}, "event_ts": "1760000501.000000",
        }))
    };
    let seen = |count: u32, mine: bool| {
        world.until("the reaction to change", || {
            let reactions = world.message(&general, live)?.reactions;
            reactions.iter().any(|reaction| (reaction.emoji.as_str(), reaction.count, reaction.mine) == ("👀", count, mine)).then_some(())
        })
    };
    react("reaction_added", "U0ANA");
    seen(1, false);
    react("reaction_added", "U0ME");
    seen(2, true);
    react("reaction_removed", "U0ANA");
    seen(1, true);

    // An edit replaces the text and says so.
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "message_changed", "hidden": true, "channel": "C0GEN",
        "ts": "1760000502.000000", "event_ts": "1760000502.000000",
        "message": {"type": "message", "subtype": "file_share", "user": "U0BEN", "text": "from the *offsite* on Tuesday",
                    "files": [photo], "ts": live, "edited": {"user": "U0BEN", "ts": "1760000502.000000"},
                    "reactions": [{"name": "eyes", "users": ["U0ME"], "count": 1}]},
        "previous_message": {"type": "message", "user": "U0BEN", "text": "from the offsite", "ts": live},
    }));
    let edited = world.until("the edit", || world.message(&general, live).filter(|message| message.edited));
    assert_eq!(edited.text, "from the offsite on Tuesday");
    assert!(ranges(&edited).contains(&("offsite", &Style::Bold)));
    assert_eq!(edited.attachments.len(), 1);
    assert_eq!(world.conversation(&general).unwrap().unread, before + 1, "an edit is not a new message");

    // A delete removes it.
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "message_deleted", "hidden": true, "channel": "C0GEN",
        "deleted_ts": live, "ts": "1760000503.000000", "event_ts": "1760000503.000000",
        "previous_message": {"type": "message", "user": "U0BEN", "text": "from the offsite on Tuesday", "ts": live},
    }));
    world.until("the delete", || world.message(&general, live).is_none().then_some(()));
    assert_eq!(world.messages(&general).len(), 7);
    assert!(world.message(&general, ROOT).is_some(), "only the deleted message went");

    // A huddle is one grey line while it runs and after, and the call itself is the official app's.
    let huddle = "1760000800.000800";
    let room = |ended: bool| json!({
        "id": "R0HUD", "name": "", "media_server": "", "created_by": "U0ANA", "date_start": 1760000800,
        "date_end": if ended { 1760001100 } else { 0 }, "participants": ["U0ANA"], "participant_history": ["U0ANA"],
        "participants_camera_on": [], "participants_camera_off": [], "participants_screenshare_on": [],
        "participants_screenshare_off": [], "canvas_thread_ts": huddle, "thread_root_ts": huddle, "channels": ["C0GEN"],
        "is_dm_call": false, "was_rejected": false, "was_missed": false, "was_accepted": false, "has_ended": ended,
        "background_id": "GRADIENT_03", "is_prewarmed": false, "is_scheduled": false, "call_family": "huddle",
    });
    let started = json!({
        "type": "message", "subtype": "huddle_thread", "user": "USLACKBOT", "channel": "C0GEN", "team": "T01ALPHA",
        "text": "A huddle started", "no_notifications": true, "ts": huddle, "room": room(false),
        "permalink": "https://acme.slack.com/call/R0HUD", "event_ts": huddle, "channel_type": "channel",
    });
    world.fake.push(ALPHA, started.clone());
    let line = world.until("the huddle's line", || world.message(&general, huddle));
    assert_eq!((line.kind.clone(), line.text.as_str()), (MessageKind::System, "A huddle started"));
    let mut ended = started;
    ended["room"] = room(true);
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "message_changed", "hidden": true, "channel": "C0GEN",
        "ts": "1760001100.000000", "event_ts": "1760001100.000000", "message": ended,
    }));
    let line = world.until("the huddle to end", || world.message(&general, huddle).filter(|line| line.text != "A huddle started"));
    assert_eq!((line.kind, line.text.as_str()), (MessageKind::System, "A huddle happened · 5 min"));

    // Scrolling past the oldest cached message asks Slack for the page before it.
    let oldest = "1760000090.000090";
    let older = || Request::Messages { conversation: general.clone(), page: Page::Before { id: oldest.to_owned() }, limit: 50 };
    let Response::Messages { messages, older: more, .. } = world.ask(older()) else {
        panic!("no page");
    };
    assert!(messages.is_empty() && !more);
    let asked = world.until("Slack to be asked for the older page", || {
        world.fake.calls(ALPHA, "conversations.history").into_iter().find(|call| call.params.contains_key("latest"))
    });
    assert_eq!((asked.params["channel"].as_str(), asked.params["latest"].as_str()), ("C0GEN", oldest));
    let first = world.until("the older page", || match world.ask(older()) {
        Response::Messages { mut messages, .. } => messages.pop(),
        _ => None,
    });
    assert_eq!((first.id.as_str(), first.text.as_str()), ("1760000050.000050", "first one in"));

    // Every file was fetched as the browser the session's other requests name. (The fake's
    // host is one the cache fetches from openly; that only `files.slack.com` is handed the
    // token and cookie is the cache's own test, and which headers the adapter hands over is
    // the adapter's.)
    let files = world.fake.files();
    assert!(files.len() >= 4, "{files:?}");
    for call in &files {
        assert!(same_browser(&call.user_agent, agent()), "{} was fetched as {:?}", call.method, call.user_agent);
    }
}

#[test]
fn a_thread_is_a_conversation_that_can_be_read_and_answered() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    world.general();
    let thread = alpha("C0GEN:1760000200.000200");

    // The id on the first message's summary opens the thread: its first message and replies.
    let replies = world.until("the thread's replies", || Some(world.messages(&thread)).filter(|found| found.len() == 3));
    let read: Vec<(&str, &str, &str)> = replies
        .iter()
        .map(|message| (message.id.as_str(), message.sender.name.as_str(), message.text.as_str()))
        .collect();
    assert_eq!(
        read,
        [
            (ROOT, "Ben Ito", "Who is on call?"),
            ("1760000230.000230", "Ana", "Me until Friday."),
            ("1760000260.000260", "Justin", "Then me."),
        ]
    );
    assert!(replies[2].outgoing);
    let row = world.until("the thread's read state", || world.conversation(&thread).filter(|row| row.unread == 1));
    assert_eq!((row.kind, row.title.as_str()), (ConversationKind::Thread, "Thread in #general"));
    assert_eq!((row.parent.as_deref(), row.root.as_deref()), (Some("C0GEN"), Some(ROOT)), "a thread names what it hangs off");
    let hanging = world.until("the unread reply on the first message", || {
        world.message(&general, ROOT)?.thread.filter(|summary| summary.unread == 1)
    });
    assert_eq!(hanging.replies, 2);
    assert_eq!(row.last_read.as_deref(), Some("1760000230.000230"), "Slack's own marker for a thread I follow");
    let in_channel = |id: &str| world.message(&general, id).is_some();
    assert!(!in_channel("1760000230.000230") && !in_channel("1760000260.000260"));

    // A reply arriving live lands in the thread, not the channel, and the count on the first
    // message goes up.
    let live = "1760000600.000600";
    world.fake.push(ALPHA, json!({
        "type": "message", "channel": "C0GEN", "user": "U0ANA", "text": "I can take it", "ts": live,
        "thread_ts": ROOT, "parent_user_id": "U0BEN", "event_ts": live, "channel_type": "channel",
    }));
    let reply = world.until("the live reply", || world.message(&thread, live));
    assert_eq!((reply.text.as_str(), reply.sender.name.as_str()), ("I can take it", "Ana"));
    assert!(!in_channel(live), "a reply is no longer dropped, and no longer in the channel");
    let summary = world.until("the count to rise", || {
        world.message(&general, ROOT)?.thread.filter(|summary| summary.replies == 3)
    });
    assert_eq!(summary.last, 1760000600);
    let faces: Vec<&str> = summary.participants.iter().map(|person| person.id.as_str()).collect();
    assert_eq!(faces, ["U0ANA", "U0ME"], "whoever replied last comes first");

    // Slack's own count, when it comes, is the count.
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "message_replied", "hidden": true, "channel": "C0GEN",
        "ts": "1760000601.000000", "event_ts": "1760000601.000000",
        "message": {"type": "message", "user": "U0BEN", "text": "Who is on call?", "ts": ROOT, "thread_ts": ROOT,
                    "reply_count": 5, "reply_users_count": 2, "latest_reply": "1760000650.000650", "reply_users": ["U0ANA", "U0ME"]},
    }));
    world.until("Slack's count", || world.message(&general, ROOT)?.thread.filter(|summary| summary.replies == 5));

    // A reply also sent to the channel shows in both, pointing at what it answers.
    let shared = "1760000700.000700";
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "thread_broadcast", "channel": "C0GEN", "user": "U0ANA", "text": "Resolved, for everyone", "ts": shared,
        "thread_ts": ROOT, "root": {"type": "message", "user": "U0BEN", "text": "Who is on call?", "ts": ROOT, "thread_ts": ROOT},
        "event_ts": shared, "channel_type": "channel",
    }));
    let broadcast = world.until("the shared reply in the channel", || world.message(&general, shared));
    let answered = broadcast.reply_to.as_ref().unwrap();
    assert_eq!((answered.id.as_str(), answered.sender.as_str(), answered.snippet.as_str()), (ROOT, "Ben Ito", "Who is on call?"));
    assert_eq!((answered.person.as_ref(), answered.deleted), (Some(&alpha("U0BEN")), false));
    assert!(world.message(&thread, shared).is_some());

    // Someone typing in the thread is typing there, not in the channel.
    world.fake.push(ALPHA, json!({"type": "user_typing", "channel": "C0GEN", "user": "U0BEN", "thread_ts": ROOT}));
    let typing_in = |conversation: &Key| match world.ask(Request::Watch(Watching { conversation: Some(conversation.clone()), ..Watching::default() })) {
        Response::Watching { typists, .. } => typists,
        other => panic!("{other:?}"),
    };
    let typists = world.until("typing in the thread", || Some(typing_in(&thread)).filter(|typists| !typists.is_empty()));
    assert_eq!(typists[0].name, "Ben Ito");
    assert!(typing_in(&general).is_empty());

    // Sending to the thread's conversation is a reply in that thread.
    let client_id = "1760400000123456";
    let pending = world.send(&thread, client_id, "On it.");
    assert_eq!((pending.state, pending.id.as_str()), (SendState::Pending, client_id));
    let posted = world.until("the reply to reach Slack", || world.fake.calls(ALPHA, "chat.postMessage").pop());
    assert_eq!((posted.params["channel"].as_str(), posted.params["thread_ts"].as_str()), ("C0GEN", ROOT));
    assert_eq!(posted.params.get("reply_broadcast"), None);
    assert_eq!(client_id_in(&posted.params["client_msg_id"]), client_id);
    let sent = world.until("the reply to be sent", || {
        let ours = |message: &Message| message.client_id.as_deref() == Some(client_id) && message.state == SendState::Sent;
        world.messages(&thread).into_iter().find(ours)
    });
    assert!(sent.id.starts_with("17600010") && sent.outgoing && sent.text == "On it.");
    assert!(!in_channel(&sent.id));
    let mine = world.messages(&thread).into_iter().filter(|message| message.client_id.as_deref() == Some(client_id)).count();
    assert_eq!(mine, 1, "the pending row became the sent one");

    // Reading the thread tells Slack about the thread, not the channel.
    let read = Request::MarkRead { conversation: thread.clone(), message_id: sent.id.clone() };
    assert_eq!(world.ask(read), Response::Ok);
    let marked = world.until("Slack to be told the thread was read", || {
        world.fake.calls(ALPHA, "subscriptions.thread.mark").pop()
    });
    let said = |name: &str| marked.params[name].as_str();
    assert_eq!((said("channel"), said("thread_ts"), said("ts")), ("C0GEN", ROOT, sent.id.as_str()));
    assert!(world.fake.calls(ALPHA, "conversations.mark").is_empty());
    world.until("the first message to show nothing unread", || {
        world.message(&general, ROOT)?.thread.filter(|summary| summary.unread == 0)
    });

    // Typing in the thread says where.
    assert_eq!(world.ask(Request::Typing { conversation: thread.clone() }), Response::Ok);
    let typing = world.until("typing to reach Slack", || world.fake.heard(ALPHA, "user_typing").pop());
    assert_eq!((typing["channel"].as_str(), typing["thread_ts"].as_str()), (Some("C0GEN"), Some(ROOT)));

    // The huddle of a thread's conversation is its channel's, in the official app.
    let Response::Link { url } = world.ask(Request::Handoff { conversation: thread }) else {
        panic!("no link");
    };
    assert_eq!(url, "https://app.slack.com/huddle/T01ALPHA/C0GEN");
}

#[test]
fn sending_typing_presence_and_people_go_through_slack() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    world.general();

    // The echo on the socket carries our own id, so it replaces the pending row without
    // waiting for the call to answer (the fake holds that answer back for five seconds).
    let client_id = "1760400000000001";
    let asked = Instant::now();
    let pending = world.send(&general, client_id, "slow hello <b> & co");
    assert_eq!((pending.state.clone(), pending.id.as_str(), pending.client_id.as_deref()), (SendState::Pending, client_id, Some(client_id)));
    let ours = |message: &Message| message.client_id.as_deref() == Some(client_id);
    let sent = world.within(Duration::from_secs(4), "the echo to replace the pending row", || {
        world.messages(&general).into_iter().find(|message| ours(message) && message.state == SendState::Sent)
    });
    assert!(asked.elapsed() < Duration::from_secs(5), "the echo, not the call's answer, did this");
    assert_eq!((sent.id.as_str(), sent.text.as_str(), sent.outgoing), ("1760001001.000001", "slow hello <b> & co", true));
    assert_eq!(world.messages(&general).into_iter().filter(ours).count(), 1);
    assert!(world.message(&general, client_id).is_none(), "nothing is left under the shell's own id");
    let posted = world.fake.calls(ALPHA, "chat.postMessage").pop().unwrap();
    assert_eq!(posted.params["text"], "slow hello &lt;b&gt; &amp; co", "Slack's three escapes");
    assert_eq!(client_id_in(&posted.params["client_msg_id"]), client_id);
    assert_eq!(posted.params.get("thread_ts"), None);
    // The call's own answer, when it comes, changes nothing.
    sleep(Duration::from_secs(5).saturating_sub(asked.elapsed()) + Duration::from_millis(600));
    assert_eq!(world.messages(&general).into_iter().filter(ours).count(), 1);

    // A refusal is told in words, and the message waits for the user.
    let failed = |client_id: &str, text: &str| {
        world.send(&general, client_id, text);
        world.until("the send to fail", || match world.message(&general, client_id)?.state {
            SendState::Failed { reason } => Some(reason),
            _ => None,
        })
    };
    assert_eq!(failed("1760400000000002", "not here"), "You are not in this channel.");
    assert_eq!(world.fake.calls(ALPHA, "chat.postMessage").len(), 2, "a refused send is not tried again");

    // Rate limited: the whole of `Retry-After` is waited out, the call is made once more, and
    // then the user is told. Two attempts in all, never a third.
    assert_eq!(failed("1760400000000003", "limit me"), "Slack is rate limiting you. Try again in 1 s.");
    let tries = |text: &str| -> Vec<Instant> {
        let posts = world.fake.calls(ALPHA, "chat.postMessage");
        posts.iter().filter(|call| call.params["text"] == text).map(|call| call.at).collect()
    };
    let limited = tries("limit me");
    assert_eq!(limited.len(), 2);
    assert!(limited[1] - limited[0] >= Duration::from_secs(1), "{:?}", limited[1] - limited[0]);
    // Limited once: the retry goes through, and not a moment before Slack said it may.
    world.send(&general, "1760400000000004", "limit once");
    world.until("the limited send to go through", || {
        let ours = |message: &Message| message.client_id.as_deref() == Some("1760400000000004") && message.state == SendState::Sent;
        world.messages(&general).into_iter().find(ours)
    });
    let once = tries("limit once");
    assert_eq!(once.len(), 2);
    assert!(once[1] - once[0] >= Duration::from_secs(2), "{:?}", once[1] - once[0]);
    sleep(Duration::from_millis(1500));
    assert_eq!((tries("limit me").len(), tries("limit once").len()), (2, 2));

    // Someone typing shows on the change stream with when the line goes away, and goes away.
    let Response::Status { seq, .. } = world.ask(Request::Status) else {
        panic!("no status");
    };
    world.fake.push(ALPHA, json!({"type": "user_typing", "channel": "C0GEN", "user": "U0ANA"}));
    let mut since = seq;
    let typist = world.until("Ana typing", || {
        let Response::Changes { seq, changes, .. } = world.ask(Request::Wait { since }) else {
            panic!("no changes");
        };
        since = seq;
        changes.into_iter().find_map(|change| match change {
            Change::Typing { conversation, mut typists } if conversation == general => typists.pop(),
            _ => None,
        })
    });
    assert_eq!((typist.id.as_str(), typist.name.as_str()), ("U0ANA", "Ana"));
    let left = typist.until - now();
    assert!((1..=6).contains(&left), "the line lasts as long as Slack's own: {left} s");
    let watching = |people: Vec<Key>| match world.ask(Request::Watch(Watching { space: None, conversation: Some(general.clone()), people, members: None, rows: Vec::new() })) {
        Response::Watching { people, typists } => (people, typists),
        other => panic!("{other:?}"),
    };
    assert_eq!(watching(Vec::new()).1.len(), 1);
    world.until("the typing line to expire", || watching(Vec::new()).1.is_empty().then_some(()));
    assert!(now() >= typist.until);

    // My own typing reaches Slack at most once every eight seconds for a conversation, however
    // fast the draft changes, and each conversation is paced on its own.
    let typed = |channel: &str| world.fake.heard(ALPHA, "user_typing").iter().filter(|frame| frame["channel"] == channel).count();
    for _ in 0..3 {
        assert_eq!(world.ask(Request::Typing { conversation: general.clone() }), Response::Ok);
    }
    let typing = world.until("typing to reach Slack", || world.fake.heard(ALPHA, "user_typing").pop());
    let first_typed = Instant::now();
    assert_eq!((typing["channel"].as_str(), typing.get("thread_ts")), (Some("C0GEN"), None));
    assert_eq!(world.ask(Request::Typing { conversation: alpha("D0ANA") }), Response::Ok);
    world.until("typing in another conversation", || (typed("D0ANA") == 1).then_some(()));
    sleep(Duration::from_millis(4000));
    assert_eq!(world.ask(Request::Typing { conversation: general.clone() }), Response::Ok);
    sleep(Duration::from_millis(500));
    assert_eq!((typed("C0GEN"), typed("D0ANA")), (1, 1), "three seconds on is still too soon");
    assert!(world.fake.heard(BETA, "user_typing").is_empty());

    // Presence is asked for exactly who is on screen, and shown when Slack says.
    let (people, _) = watching(vec![alpha("U0ANA")]);
    assert_eq!((people[0].name.as_str(), people[0].presence), ("Ana", Presence::Unknown));
    let subscribed = world.until("the presence subscription", || {
        world.fake.heard(ALPHA, "presence_sub").pop().filter(|frame| frame["ids"].as_array().is_some_and(|ids| ids.len() > 1))
    });
    assert_eq!(subscribed["ids"], json!(["U0ANA", "U0ME"]), "who is on screen, and me for the account's own dot");
    assert_eq!(world.fake.heard(ALPHA, "presence_query").pop().map(|query| query["ids"].clone()), Some(json!(["U0ANA"])));
    world.fake.push(ALPHA, json!({"type": "presence_change", "users": ["U0ANA", "U0BEN"], "presence": "active"}));
    world.until("Ana to be online", || (watching(vec![alpha("U0ANA")]).0[0].presence == Presence::Online).then_some(()));

    // The directory is searched on Slack's side: someone never seen here is found by name.
    let search = || Request::People { network: Some(Network::Slack), relation: None, query: Some("zoe".to_owned()), offset: 0, limit: 25 };
    let Response::People { people, .. } = world.ask(search()) else {
        panic!("no people");
    };
    assert!(people.is_empty(), "nobody downloaded the directory");
    let found = world.until("Zoe in the directory", || match world.ask(search()) {
        Response::People { mut people, .. } => people.pop(),
        _ => None,
    });
    assert_eq!((found.name.as_str(), found.handle.as_str(), found.relation), ("Zoe Park", "zoe", Relation::None));
    assert_eq!(found.avatar.map(|avatar| avatar.id), Some("avatar:U0ZOE:c4c4c4".to_owned()));
    assert_eq!(world.fake.calls(ALPHA, "edge:users/search")[0].params["query"], "zoe");
    assert_eq!(found.link, "slack://user?team=T01ALPHA&id=U0ZOE");
    // The shell is told when the search is over, and the same search is not made twice.
    let over = |account: AccountKey| Change::Searched { account, query: "zoe".to_owned() };
    world.until("both workspaces to say the search is over, found or not", || {
        let Response::Changes { changes, .. } = world.ask(Request::Wait { since: seq }) else {
            panic!("no changes");
        };
        (changes.contains(&over(alpha("").account())) && changes.contains(&over(beta("").account()))).then_some(())
    });
    sleep(Duration::from_millis(700));
    assert_eq!(world.ask(search()), world.ask(search()));
    sleep(Duration::from_millis(700));
    assert_eq!(world.fake.calls(ALPHA, "edge:users/search").len(), 1);

    // Starting a direct conversation opens it on Slack and lists it.
    let Response::Conversation { conversation: direct } = world.ask(Request::StartDirect { person: alpha("U0BEN") }) else {
        panic!("no conversation");
    };
    assert_eq!((direct.key.id.as_str(), direct.kind, direct.title.as_str()), ("D0NEW", ConversationKind::Direct, "Ben Ito"));
    assert_eq!((direct.peer.as_deref(), direct.group.as_deref()), (Some("U0BEN"), Some("directs")));
    assert_eq!(world.fake.calls(ALPHA, "conversations.open")[0].params["users"], "U0BEN");
    assert!(world.fake.calls(ALPHA, "chat.postMessage").len() == 6, "opening says nothing to them");

    // The member list comes by window, each person looked up, never the directory.
    let Response::Members { sections, members, .. } = world.ask(Request::Members { conversation: general.clone(), offset: 1, limit: 2 }) else {
        panic!("no members");
    };
    assert_eq!((sections[0].name.as_str(), sections[0].count), ("Members", 4));
    let listed: Vec<(&str, Relation, Presence)> = members.iter().map(|member| (member.name.as_str(), member.relation, member.presence)).collect();
    assert_eq!(
        listed,
        [("Ana", Relation::Contact, Presence::Online), ("Ben Ito", Relation::Contact, Presence::Unknown)],
        "a member row says what is known of their presence at once"
    );
    // Slack pages the list by cursor. The walk is kept: a second window goes on from where the
    // first stopped, and one already walked asks nothing.
    let walked = || -> Vec<String> {
        let calls = world.fake.calls(ALPHA, "conversations.members");
        calls.iter().map(|call| call.params.get("cursor").cloned().unwrap_or_default()).collect()
    };
    assert_eq!(walked(), ["", "2"], "three rows are two of Slack's pages here");
    let window = |offset: u32, limit: u32| match world.ask(Request::Members { conversation: general.clone(), offset, limit }) {
        Response::Members { members, .. } => members.into_iter().map(|member| member.name).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(window(3, 2), ["Deploy Bot"]);
    assert_eq!(window(0, 2), ["Justin", "Ana"]);
    assert_eq!(walked(), ["", "2"], "never from the start again");
    assert_eq!(world.fake.calls(ALPHA, "conversations.info").iter().filter(|call| call.params.contains_key("include_num_members")).count(), 1);
    // Someone leaving is told, so the rows on screen can be read again.
    let Response::Status { seq, .. } = world.ask(Request::Status) else {
        panic!("no status");
    };
    world.fake.push(ALPHA, json!({"type": "member_left_channel", "user": "U0BOT", "channel": "C0GEN", "channel_type": "C", "team": "T01ALPHA"}));
    let mut since = seq;
    world.until("the member list to change", || {
        let Response::Changes { seq, changes, .. } = world.ask(Request::Wait { since }) else {
            panic!("no changes");
        };
        since = seq;
        changes.contains(&Change::Members { conversation: general.clone() }).then_some(())
    });
    assert_eq!(window(2, 5), ["Ben Ito"]);
    assert_eq!(walked(), ["", "2"]);

    // Eight seconds after the first signal the next one goes out.
    sleep(Duration::from_millis(8200).saturating_sub(first_typed.elapsed()));
    assert_eq!(world.ask(Request::Typing { conversation: general.clone() }), Response::Ok);
    world.until("typing again once the eight seconds are over", || (typed("C0GEN") == 2).then_some(()));

    // A huddle opens in the official app.
    let Response::Link { url } = world.ask(Request::Handoff { conversation: general }) else {
        panic!("no link");
    };
    assert_eq!(url, "https://app.slack.com/huddle/T01ALPHA/C0GEN");
}

#[test]
fn a_session_slack_ends_expires_that_workspace_and_not_the_other() {
    let world = World::start();
    world.ready();
    let side = beta("C0SIDE");
    let cached = world.until("the second workspace's channel", || Some(world.messages(&side)).filter(|found| !found.is_empty()));
    // This workspace refuses the batched lookup, so its people are asked for one at a time.
    assert_eq!((cached[0].sender.name.as_str(), cached[0].text.as_str()), ("Pat Kim", "weekend build is up"));
    assert_eq!(world.fake.calls(BETA, "users.info")[0].params["user"], "U0PAT");

    // Slack ends the second workspace's session. The next thing asked of it finds out.
    world.fake.revoked.lock().unwrap().push(BETA.to_owned());
    let members = world.ask(Request::Members { conversation: side.clone(), offset: 0, limit: 10 });
    let Response::Error { code: ErrorCode::Refused, detail } = members else {
        panic!("{members:?}");
    };
    assert_eq!(detail, "Slack signed you out of this workspace. Sign in again.");
    let expired = world.until("the second workspace to expire", || {
        Some(world.account("T02BETA")).filter(|account| account.link == Link::Expired)
    });
    assert!(expired.detail.contains("invalid_auth"), "{}", expired.detail);

    // The first workspace is untouched and still sends.
    assert_eq!(world.account("T01ALPHA").link, Link::Connected);
    let client_id = "1760400000000009";
    world.send(&alpha("C0GEN"), client_id, "still here");
    world.until("the first workspace to send", || {
        let sent = |message: &Message| message.client_id.as_deref() == Some(client_id) && message.state == SendState::Sent;
        world.messages(&alpha("C0GEN")).into_iter().find(sent)
    });

    // What was read on the expired workspace is still readable; sending there is refused.
    assert_eq!(world.messages(&side).len(), 1);
    let refused = world.ask(Request::Send { conversation: side, client_id: "1760400000000010".to_owned(), text: "hello?".to_owned(), reply_to: None, with: Default::default() });
    assert!(matches!(refused, Response::Error { code: ErrorCode::Offline, .. }), "{refused:?}");
}

/// What a shell would see of a person's picture: the file, once it is on disk.
fn pictured(world: &World, file: Option<protocol::MediaFile>) -> protocol::MediaFile {
    let file = file.expect("the person has a picture");
    assert_eq!((file.kind, file.width), (MediaKind::Avatar, 72));
    world.fetched(&alpha(""), &file.id, Some(80))
}

#[test]
fn someone_who_uploaded_no_picture_shows_slacks_default_for_them_and_not_initials() {
    let mut world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let messages = world.general();

    // Ben never uploaded a picture, so his profile names gravatar with Slack's own default as
    // the fallback. That default is his picture: on his messages,
    let ben = messages.iter().find(|message| message.id == ROOT).unwrap().sender.clone();
    assert_eq!(ben.avatar.as_ref().map(|avatar| avatar.id.as_str()), Some("avatar:U0BEN:g2c8d4"));
    let on_disk = pictured(&world, ben.avatar);
    assert!(on_disk.path.is_some_and(|path| std::fs::metadata(path).is_ok_and(|file| file.len() > 0)));
    // at the size a 2x screen draws a row's picture, from Slack and from nobody else,
    let fetched = world.fake.file("/img/avatars/ava_0002-192.png").expect("the default, at twice list size");
    assert!(same_browser(&fetched.user_agent, agent()), "{:?}", fetched.user_agent);
    assert_eq!((fetched.token.as_str(), fetched.cookie.as_str()), ("", ""), "a public picture is not shown the session");
    assert!(world.fake.files().iter().all(|call| call.method.contains("/img/avatars/ava_0002-")), "{:?}", world.fake.files());

    // on the row of a direct conversation with him, in the sidebar and in the Inbox,
    let Response::Conversation { conversation: direct } = world.ask(Request::StartDirect { person: alpha("U0BEN") }) else {
        panic!("no conversation");
    };
    pictured(&world, direct.avatar);
    // A conversation with nothing said in it yet is not in the Inbox (polish rule I2); its row
    // carries the picture wherever it is read.
    let Response::Conversation { conversation: row } = world.ask(Request::Conversation { conversation: alpha("D0NEW") }) else {
        panic!("no row");
    };
    pictured(&world, row.avatar);

    // and as a person.
    let asked = Request::People { network: Some(Network::Slack), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 };
    let Response::People { people, .. } = world.ask(asked) else {
        panic!("no people");
    };
    let listed = people.into_iter().find(|person| person.key.id == "U0BEN").unwrap();
    assert_eq!(pictured(&world, listed.avatar).id, "avatar:U0BEN:g2c8d4");

    // After a restart nobody is asked who Ben is, and his picture is still his.
    let lookups = |world: &World| world.fake.calls(ALPHA, "users.info").len() + world.fake.calls(ALPHA, "edge:users/info").len();
    let before = lookups(&world);
    world.restart();
    world.ready();
    world.until("the socket", || world.fake.sockets.lock().unwrap().contains_key(ALPHA).then_some(()));
    world.fake.push(ALPHA, json!({
        "type": "message", "channel": "C0GEN", "user": "U0BEN", "text": "back again", "ts": "1760000900.000900",
        "event_ts": "1760000900.000900", "channel_type": "channel",
    }));
    let again = world.until("a message of his after the restart", || world.message(&general, "1760000900.000900"));
    assert!(again.sender.avatar.as_ref().is_some_and(|avatar| avatar.path.is_some()), "{:?}", again.sender.avatar);
    assert_eq!(lookups(&world), before);
}

#[test]
fn reacting_editing_and_deleting_ask_slack_and_show_what_slack_says() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    world.general();
    let shipping = "1760000300.000300";
    let reaction = |emoji: &str| world.message(&general, shipping).unwrap().reactions.into_iter().find(|have| have.emoji == emoji);
    let react = |emoji: &str, on: bool| world.ask(Request::React { conversation: general.clone(), message_id: shipping.to_owned(), emoji: emoji.to_owned(), on });

    // A reaction is asked of Slack by name, shows at once, and Slack's own word of it on the
    // socket is the same reaction, not a second one.
    assert_eq!(react("👀", true), Response::Ok);
    let added = world.fake.calls(ALPHA, "reactions.add").pop().unwrap();
    let said = |call: &Call, name: &str| call.params.get(name).cloned().unwrap_or_default();
    assert_eq!((said(&added, "channel"), said(&added, "timestamp"), said(&added, "name")), ("C0GEN".to_owned(), shipping.to_owned(), "eyes".to_owned()));
    assert_eq!(reaction("👀").map(|have| (have.count, have.mine)), Some((1, true)));
    sleep(Duration::from_millis(900));
    assert_eq!(reaction("👀").map(|have| (have.count, have.mine)), Some((1, true)), "the echo counted once");
    // Mine on top of someone else's, a custom one by its name, and with a skin tone.
    assert_eq!(react(":partyparrot:", true), Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "reactions.add").pop().unwrap().params["name"], "partyparrot");
    world.until("my parrot beside Ben's", || reaction(":partyparrot:").filter(|have| have.count == 2 && have.mine));
    assert_eq!(react("👍🏽", true), Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "reactions.add").pop().unwrap().params["name"], "+1::skin-tone-4");
    // Taking one back.
    assert_eq!(react("👀", false), Response::Ok);
    let removed = world.fake.calls(ALPHA, "reactions.remove").pop().unwrap();
    assert_eq!((said(&removed, "timestamp"), said(&removed, "name")), (shipping.to_owned(), "eyes".to_owned()));
    sleep(Duration::from_millis(900));
    assert_eq!(reaction("👀"), None);
    // Slack refuses: the answer says why, and what was shown is taken back.
    let refused = react("🚫", true);
    assert_eq!(refused, Response::Error { code: ErrorCode::Refused, detail: "This message has as many reactions as Slack allows.".to_owned() });
    assert_eq!(reaction("🚫"), None);
    assert_eq!(world.fake.calls(ALPHA, "reactions.add").len(), 4, "a refused reaction is not tried again");

    // An edit changes nothing until Slack accepts it, and is there when the answer is.
    let mine = "1760000100.000100";
    let edit = Request::Edit { conversation: general.clone(), message_id: mine.to_owned(), text: "good morning <team> & all".to_owned() };
    assert_eq!(world.ask(edit), Response::Ok);
    let edited = world.message(&general, mine).unwrap();
    assert_eq!((edited.text.as_str(), edited.edited, edited.outgoing), ("good morning <team> & all", true, true));
    let updated = world.fake.calls(ALPHA, "chat.update").pop().unwrap();
    assert_eq!((said(&updated, "channel"), said(&updated, "ts")), ("C0GEN".to_owned(), mine.to_owned()));
    assert_eq!(updated.params["text"], "good morning &lt;team&gt; &amp; all");
    // Only my own: chatd refuses the rest before Slack hears of it.
    let theirs = Request::Edit { conversation: general.clone(), message_id: ROOT.to_owned(), text: "mine now".to_owned() };
    assert!(matches!(world.ask(theirs), Response::Error { code: ErrorCode::Refused, .. }), "someone else's message is not mine to edit");
    assert_eq!(world.fake.calls(ALPHA, "chat.update").len(), 1);

    // A delete, likewise.
    assert_eq!(world.ask(Request::Delete { conversation: general.clone(), message_id: mine.to_owned() }), Response::Ok);
    assert!(world.message(&general, mine).is_none());
    let deleted = world.fake.calls(ALPHA, "chat.delete").pop().unwrap();
    assert_eq!((said(&deleted, "channel"), said(&deleted, "ts")), ("C0GEN".to_owned(), mine.to_owned()));
    // Slack's own events for both change nothing more.
    sleep(Duration::from_millis(900));
    assert_eq!(world.messages(&general).len(), 6);

    // Slack says no to an edit: nothing changed, and the answer is its reason.
    world.fake.refusing.lock().unwrap().insert("chat.delete".to_owned(), "cant_delete_message".to_owned());
    let refused = world.ask(Request::Delete { conversation: general.clone(), message_id: shipping.to_owned() });
    assert_eq!(refused, Response::Error { code: ErrorCode::Refused, detail: "Slack does not let you delete this message.".to_owned() });
    assert!(world.message(&general, shipping).is_some());

    // A message deleted while a thread hangs off it stays as a tombstone, as Slack keeps it.
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "message_changed", "hidden": true, "channel": "C0GEN",
        "ts": "1760002100.000000", "event_ts": "1760002100.000000",
        "message": {"type": "message", "subtype": "tombstone", "user": "USLACKBOT", "text": "This message was deleted.", "ts": ROOT,
                    "thread_ts": ROOT, "reply_count": 2, "reply_users_count": 2, "latest_reply": "1760000260.000260", "reply_users": ["U0ANA", "U0ME"]},
    }));
    let tombstone = world.until("the tombstone", || world.message(&general, ROOT).filter(|message| message.deleted));
    assert_eq!((tombstone.text.as_str(), tombstone.thread.map(|summary| summary.replies)), ("", Some(2)));
}

#[test]
fn presence_a_profile_and_signing_out() {
    let world = World::start();
    world.ready();
    let account = AccountKey { network: Network::Slack, account: "T01ALPHA".to_owned() };

    // Away with a status: both reach Slack, and the account shows what Slack now shows others.
    let away = Request::SetPresence { account: account.clone(), presence: Presence::Idle, status: Some("Lunch".to_owned()) };
    assert_eq!(world.ask(away), Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "users.setPresence").pop().unwrap().params["presence"], "away");
    let set = world.fake.calls(ALPHA, "users.profile.set").pop().unwrap();
    let profile: Value = serde_json::from_str(&set.params["profile"]).unwrap();
    assert_eq!((profile["status_text"].as_str(), profile["status_emoji"].as_str()), (Some("Lunch"), Some("")));
    let shown = world.account("T01ALPHA");
    assert_eq!((shown.presence, shown.status.as_str()), (Presence::Idle, "💬 Lunch"));
    // Back, with the status left alone: one call, and the text stays.
    let back = Request::SetPresence { account: account.clone(), presence: Presence::Online, status: None };
    assert_eq!(world.ask(back), Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "users.setPresence").pop().unwrap().params["presence"], "auto");
    assert_eq!(world.fake.calls(ALPHA, "users.profile.set").len(), 1);
    let shown = world.account("T01ALPHA");
    assert_eq!((shown.presence, shown.status.as_str()), (Presence::Online, "💬 Lunch"));
    // Clearing the status.
    let clear = Request::SetPresence { account: account.clone(), presence: Presence::Online, status: Some(String::new()) };
    assert_eq!(world.ask(clear), Response::Ok);
    assert_eq!(world.account("T01ALPHA").status, "");
    // Do Not Disturb is not built: it is refused in words and nothing is shown as set.
    let quiet = world.ask(Request::SetPresence { account: account.clone(), presence: Presence::Dnd, status: None });
    assert!(matches!(&quiet, Response::Error { code: ErrorCode::Refused, detail } if detail.contains("Do Not Disturb")), "{quiet:?}");
    assert_eq!(world.account("T01ALPHA").presence, Presence::Online);
    assert_eq!(world.fake.calls(ALPHA, "users.setPresence").len(), 3);
    // Set away from another device: Slack says so on the socket.
    world.fake.push(ALPHA, json!({"type": "manual_presence_change", "presence": "away"}));
    world.until("my own presence from elsewhere", || (world.account("T01ALPHA").presence == Presence::Offline).then_some(()));

    // A profile card is one request, and only because it was opened.
    let before = world.fake.calls(ALPHA, "users.info").len();
    world.until("Ana to be known", || world.conversation(&alpha("D0ANA")));
    let Response::Profile { profile } = world.ask(Request::Profile { person: alpha("U0ANA"), space: None }) else {
        panic!("no profile");
    };
    // The title is a title, not a bio (06, story 3).
    assert_eq!((profile.title.as_str(), profile.bio.as_str()), ("Design lead", ""));
    assert_eq!((profile.time_zone.as_str(), profile.utc_offset), ("Europe/Lisbon", Some(3600)));
    assert_eq!((profile.person.name.as_str(), profile.person.link.as_str()), ("Ana", "slack://user?team=T01ALPHA&id=U0ANA"));
    assert_eq!(world.fake.calls(ALPHA, "users.info").len(), before + 1);
    assert!(world.fake.calls(ALPHA, "users.profile.get").is_empty());
    // Opened again from the Space (an answer chatd itself has not kept), Slack is still not
    // asked again: one `users.info` per person per hour, the card read then answered again.
    let Response::Profile { profile } = world.ask(Request::Profile { person: alpha("U0ANA"), space: Some(alpha("T01ALPHA")) }) else {
        panic!("no profile");
    };
    assert_eq!(profile.title.as_str(), "Design lead");
    assert_eq!(world.fake.calls(ALPHA, "users.info").len(), before + 1);

    // Signing out of one workspace has nothing to end on Slack, so it answers at once, and the
    // browser's session, which the other workspace shares, is left alone.
    let asked = Instant::now();
    let Response::Status { accounts, .. } = world.ask(Request::SignOut { account: beta("").account() }) else {
        panic!("no status");
    };
    assert!(asked.elapsed() < Duration::from_secs(2), "{:?}", asked.elapsed());
    assert!(accounts.iter().all(|account| account.key.account != "T02BETA" || account.link == Link::SignedOut));
    assert!(world.fake.calls(BETA, "auth.signout").is_empty() && world.fake.calls(ALPHA, "auth.signout").is_empty());
    assert_eq!(world.account("T01ALPHA").link, Link::Connected);
}

#[test]
fn reading_forward_from_or_around_an_old_message_asks_slack_only_for_what_is_missing() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    world.general();
    // Of this channel: the conversations that need me are being warmed meanwhile.
    let history = || -> Vec<Call> {
        let calls = world.fake.calls(ALPHA, "conversations.history");
        calls.into_iter().filter(|call| call.params["channel"] == "C0GEN").collect()
    };
    let page = |page: Page| match world.ask(Request::Messages { conversation: general.clone(), page, limit: 50 }) {
        Response::Messages { messages, .. } => messages.into_iter().map(|message| message.id).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let settled = || {
        sleep(Duration::from_millis(600));
        history().len()
    };
    let fetched = settled();

    // Forward from a message inside the page that was just fetched: the cache holds it whole.
    page(Page::After { id: ROOT.to_owned() });
    assert_eq!(settled(), fetched);

    // A message the user jumped to, far back: it and what is before it, then what is after.
    let linked = "1759000500.000500";
    assert!(page(Page::Around { id: linked.to_owned() }).iter().all(|id| id != linked));
    world.until("the linked message", || page(Page::Around { id: linked.to_owned() }).iter().any(|id| id == linked).then_some(()));
    world.until("both halves to be asked for", || (history().len() == fetched + 2).then_some(()));
    let asked: Vec<Call> = history().split_off(fetched);
    let said = |call: &Call, name: &str| call.params.get(name).cloned().unwrap_or_default();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!((said(&asked[0], "latest"), said(&asked[0], "inclusive"), said(&asked[0], "oldest")), (linked.to_owned(), "true".to_owned(), String::new()));
    assert_eq!((said(&asked[1], "oldest"), said(&asked[1], "latest")), (linked.to_owned(), String::new()));
    let around = world.until("what came after it", || {
        Some(page(Page::Around { id: linked.to_owned() })).filter(|around| around.iter().any(|id| id == "1759990100.000100"))
    });
    assert!(around.starts_with(&["1759000400.000400".to_owned(), linked.to_owned(), "1759990100.000100".to_owned()]), "{around:?}");
    // Reading on from there stays inside what was fetched, until its end is reached.
    page(Page::After { id: linked.to_owned() });
    page(Page::Around { id: "1759990100.000100".to_owned() });
    assert_eq!(settled(), fetched + 2);
    page(Page::After { id: "1759990200.000200".to_owned() });
    let next = world.until("the page after the end of what is cached", || history().into_iter().nth(fetched + 2));
    assert_eq!(said(&next, "oldest"), "1759990200.000200");
}

#[test]
fn connecting_asks_little_spaces_what_it_warms_and_a_reconnect_asks_less() {
    let mut world = World::start_with(|fake| fake.busy.store(true, Ordering::Relaxed));
    world.ready();
    let history = |world: &World| world.fake.calls(ALPHA, "conversations.history");
    world.within(Duration::from_secs(40), "the conversations that need me to be warmed", || (history(&world).len() >= 5).then_some(()));
    // Long enough for a sixth to have come, had there been one.
    sleep(Duration::from_secs(7));

    // Nine conversations need me. The five newest are warmed, once each, and no more.
    let warmed = history(&world);
    let channels: Vec<&str> = warmed.iter().map(|call| call.params["channel"].as_str()).collect();
    assert_eq!(channels, ["D1G", "D1F", "D1E", "D1D", "D1C"], "the newest first");
    // Never faster than half of what Slack allows the method (50 a minute: one in 2.4 s), and
    // not in step with one another.
    let gaps: Vec<Duration> = warmed.windows(2).map(|pair| pair[1].at - pair[0].at).collect();
    assert!(gaps.iter().all(|gap| *gap >= Duration::from_millis(2400)), "{gaps:?}");
    let (shortest, longest) = (gaps.iter().min().unwrap(), gaps.iter().max().unwrap());
    assert!(*longest - *shortest > Duration::from_millis(5), "the gaps are jittered: {gaps:?}");

    // Everything a first connection to a workspace asked, by method: one call each to learn who
    // I am, the icon, the unread state, the mutes and (the cache being empty) the sidebar and
    // the people in it; then the five warmed. No boot payload, no directory, no `rtm.connect`.
    let first = world.fake.calls(ALPHA, "auth.test")[0].at;
    let all = world.fake.calls.lock().unwrap().clone();
    assert!(all.iter().filter(|call| call.token == ALPHA).all(|call| call.at - first < Duration::from_secs(40)));
    let expected = |history: usize, connections: usize, sidebars: usize| -> Vec<(String, usize)> {
        let mut expected = vec![
            ("auth.test", connections),
            ("client.counts", connections),
            ("conversations.history", history),
            ("edge:users/info", 1),
            ("team.info", 1),
            ("users.channelSections.list", sidebars),
            ("users.conversations", sidebars),
            ("users.prefs.get", connections),
        ];
        expected.sort();
        expected.into_iter().map(|(method, count)| (method.to_owned(), count)).collect()
    };
    assert_eq!(world.fake.counted(ALPHA), expected(5, 1, 1));
    // Any two calls are at least a fifth of a second apart, whatever they are.
    let mut times: Vec<Instant> = all.iter().filter(|call| call.token == ALPHA).map(|call| call.at).collect();
    times.sort();
    assert!(times.windows(2).all(|pair| pair[1] - pair[0] >= Duration::from_millis(150)));

    // The socket drops. The new connection reads the unread state again and nothing else: no
    // sidebar, no warming, and the socket's address is not asked for.
    world.fake.push(ALPHA, json!({"type": "goodbye"}));
    world.until("the socket to be opened again", || {
        (world.fake.shakes.lock().unwrap().iter().filter(|shake| shake.token == ALPHA).count() == 2).then_some(())
    });
    world.until("the unread state to be read again", || (world.fake.calls(ALPHA, "users.prefs.get").len() == 2).then_some(()));
    sleep(Duration::from_secs(6));
    assert_eq!(world.fake.counted(ALPHA), expected(5, 2, 1));

    // The sidebar was read minutes ago at most, so opening the Space reads nothing either.
    world.tree(&alpha("T01ALPHA")).unwrap();
    sleep(Duration::from_secs(2));
    assert_eq!(world.fake.counted(ALPHA), expected(5, 2, 1));

    // chatd is run again, on its cache: connecting reads the unread state and not the sidebar,
    // and opening the Space reads nothing more. Every DM ever is paged only on a first load.
    world.restart();
    world.ready();
    world.until("the unread state after the restart", || (world.fake.calls(ALPHA, "users.prefs.get").len() == 3).then_some(()));
    sleep(Duration::from_secs(2));
    world.tree(&alpha("T01ALPHA")).unwrap();
    world.tree(&alpha("T01ALPHA")).unwrap();
    sleep(Duration::from_secs(2));
    assert_eq!(world.fake.calls(ALPHA, "users.conversations").len(), 1);
    assert_eq!(world.fake.calls(ALPHA, "client.counts").len(), 3, "one per connection, none for opening");
    // A new run warms again, the same few and no more.
    world.within(Duration::from_secs(40), "the second run's warming", || (history(&world).len() >= 10).then_some(()));
    sleep(Duration::from_secs(7));
    assert_eq!(history(&world).len(), 10);
}

#[test]
fn what_slack_will_not_describe_is_asked_about_once_and_a_workspace_only_stores_its_own() {
    let world = World::start();
    world.ready();
    world.general();
    let said = |token: &str, channel: &str, user: &str, text: &str, ts: &str, team: &str| {
        world.fake.push(token, json!({
            "type": "message", "channel": channel, "user": user, "team": team, "text": text, "ts": ts,
            "event_ts": ts, "channel_type": "channel",
        }));
    };

    // Both workspaces have a channel with the same id. What arrives on a workspace's socket is
    // that workspace's, whatever the event says of itself.
    world.until("the second workspace's channel", || world.conversation(&beta("C0SIDE")));
    said(ALPHA, "C0SIDE", "U0ANA", "said on the first", "1760003000.000100", "T02BETA");
    said(BETA, "C0SIDE", "U0PAT", "said on the second", "1760003001.000100", "T01ALPHA");
    world.until("both to arrive", || world.message(&alpha("C0SIDE"), "1760003000.000100").and(world.message(&beta("C0SIDE"), "1760003001.000100")));
    let texts = |conversation: &Key| world.messages(conversation).into_iter().map(|message| message.text).collect::<Vec<_>>();
    assert_eq!(texts(&alpha("C0SIDE")), ["said on the first"]);
    assert!(texts(&beta("C0SIDE")).iter().all(|text| text != "said on the first") && texts(&beta("C0SIDE")).contains(&"said on the second".to_owned()));
    assert_eq!(world.conversation(&alpha("C0SIDE")).unwrap().title, "#sidecar");
    assert_eq!(world.conversation(&beta("C0SIDE")).unwrap().title, "#main");
    // A channel only the second workspace has, heard on the first one's socket, is nobody's
    // there: not stored, and the second workspace's is untouched.
    for at in 0..3 {
        said(ALPHA, "C0ONLYB", "U0ANA", "not mine", &format!("176000310{at}.000100"), "T02BETA");
    }
    // Someone Slack will not name: their messages still arrive, under their id.
    for at in 0..3 {
        said(ALPHA, "C0GEN", "U0GHOST", &format!("boo {at}"), &format!("176000320{at}.000100"), "T01ALPHA");
    }
    let last = world.until("the last of the stranger's messages", || world.message(&alpha("C0GEN"), "1760003202.000100"));
    assert_eq!((last.sender.id.as_str(), last.sender.name.as_str()), ("U0GHOST", "U0GHOST"));
    assert!(world.conversation(&alpha("C0ONLYB")).is_none() && world.messages(&alpha("C0ONLYB")).is_empty());
    assert!(world.conversation(&beta("C0ONLYB")).is_none());
    // One lookup each, not one per message.
    let asked = |method: &str, name: &str, value: &str| {
        world.fake.calls(ALPHA, method).iter().filter(|call| call.params.get(name).is_some_and(|asked| asked == value)).count()
    };
    assert_eq!(asked("conversations.info", "channel", "C0ONLYB"), 1);
    assert_eq!(asked("users.info", "user", "U0GHOST"), 1);
    assert_eq!(world.fake.calls(BETA, "conversations.info").iter().filter(|call| call.params["channel"] == "C0ONLYB").count(), 0);
}

#[test]
fn a_reissued_cookie_is_used_and_saved_and_a_demand_only_slacks_app_can_meet_stops_everything() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    world.general();

    // Slack reissues the session cookie on some answer. Every request after it carries the new
    // one, and it is what the stored session now holds, with everything else as it was.
    let fresh = "xoxd-reissued%2Bcookie%3D";
    *world.fake.reissue.lock().unwrap() = Some(fresh.to_owned());
    let read = |id: &str| world.ask(Request::MarkRead { conversation: general.clone(), message_id: id.to_owned() });
    assert_eq!(read("1760000200.000200"), Response::Ok);
    world.until("the answer that reissues it", || world.fake.calls(ALPHA, "conversations.mark").pop());
    assert_eq!(read("1760000300.000300"), Response::Ok);
    let next = world.until("the next request", || world.fake.calls(ALPHA, "conversations.mark").into_iter().nth(1));
    assert_eq!(next.cookie, format!("d={fresh}"));
    let saved = world.until("the session to be saved", || std::fs::read_to_string(world.dir.join("session.slack")).ok());
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["cookie"], fresh);
    assert_eq!((saved["user_agent"].as_str(), saved["teams"].as_array().map(Vec::len)), (Some(agent()), Some(2)));
    assert_eq!((saved["teams"][0]["token"].as_str(), saved["teams"][1]["token"].as_str()), (Some(ALPHA), Some(BETA)));

    // Slack wants a second factor set up: only its own app can do that. The account says so,
    // and nothing more is asked of Slack for this workspace.
    world.fake.refusing.lock().unwrap().insert("conversations.replies".to_owned(), "two_factor_setup_required".to_owned());
    world.messages(&alpha("C0GEN:1760000200.000200"));
    let required = world.until("the account to say what Slack wants", || Some(world.account("T01ALPHA").required).filter(|required| !required.is_empty()));
    assert_eq!(required, "Slack wants two-factor authentication set up. Finish that in the Slack app.");
    world.fake.refusing.lock().unwrap().clear();
    let asked = world.fake.calls.lock().unwrap().iter().filter(|call| call.token == ALPHA).count();
    let members = world.ask(Request::Members { conversation: general.clone(), offset: 0, limit: 10 });
    assert_eq!(members, Response::Error { code: ErrorCode::Refused, detail: required });
    world.messages(&alpha("D0ANA"));
    sleep(Duration::from_millis(1500));
    assert_eq!(world.fake.calls.lock().unwrap().iter().filter(|call| call.token == ALPHA).count(), asked);
    assert_eq!(world.account("T02BETA").required, "", "the other workspace is not held up");
}

#[test]
fn rows_on_screen_get_their_newest_message_paced_and_people_carry_what_slack_says_of_them() {
    let world = World::start();
    world.ready();
    world.until("the sidebar", || world.tree(&alpha("T01ALPHA")).filter(|(_, rows)| rows.len() >= 7));
    let limited = |world: &World| -> Vec<Call> {
        let calls = world.fake.calls(ALPHA, "conversations.history");
        calls.into_iter().filter(|call| call.params.get("limit").map(String::as_str) == Some("1")).collect()
    };
    // Four rows with no preview are on screen. The first is asked for at once.
    let rows: Vec<Key> = ["C0RND", "C0SIDE", "G0GRP", "D0BOT"].iter().map(|id| alpha(id)).collect();
    assert!(rows.iter().all(|row| world.conversation(row).unwrap().preview.is_empty()));
    world.ask(Request::Watch(Watching { rows: rows.clone(), ..Watching::default() }));
    world.until("the first preview", || (!limited(&world).is_empty()).then_some(()));
    // The screen moves on before the rest come up: only what is still there is asked for.
    world.ask(Request::Watch(Watching { rows: rows[..2].to_vec(), ..Watching::default() }));
    world.until("the second preview", || (limited(&world).len() == 2).then_some(()));
    sleep(Duration::from_secs(3));
    let asked = limited(&world);
    let channels: Vec<&str> = asked.iter().map(|call| call.params["channel"].as_str()).collect();
    assert_eq!(channels, ["C0RND", "C0SIDE"], "rows that left the screen are not asked for");
    assert!(asked[1].at - asked[0].at >= Duration::from_secs(1), "at most one a second: {:?}", asked[1].at - asked[0].at);
    // The store answers with the preview.
    world.until("the row's preview", || world.conversation(&alpha("C0RND")).filter(|row| row.preview.contains("lunch at noon")));
    // Asked again (the shell repaints), nothing more is fetched.
    world.ask(Request::Watch(Watching { rows: rows[..2].to_vec(), ..Watching::default() }));
    sleep(Duration::from_millis(1500));
    assert_eq!(limited(&world).len(), 2);

    // What Slack's user objects say, kept at no extra request: title, local offset, status
    // emoji and a huddle; a guest is tagged; an app's DM is an app's.
    let people = |query: &str| match world.ask(Request::People { network: Some(Network::Slack), relation: None, query: Some(query.into()), offset: 0, limit: 10 }) {
        Response::People { people, .. } => people,
        other => panic!("{other:?}"),
    };
    let ana = people("Ana").into_iter().find(|person| person.key == alpha("U0ANA")).expect("Ana");
    assert_eq!((ana.title.as_str(), ana.utc_offset, ana.emoji.as_str()), ("Design lead", Some(3600), "🎧"));
    assert_eq!(ana.activity.as_deref().map(|activity| activity.kind), Some(protocol::ActivityKind::Huddle));
    let ben = people("Ben").into_iter().find(|person| person.key == alpha("U0BEN")).expect("Ben, who wrote in #random");
    assert_eq!(ben.mark, Some(protocol::Mark::Guest));
    assert!(world.conversation(&alpha("D0BOT")).unwrap().bot && !world.conversation(&alpha("D0ANA")).unwrap().bot);

    // What the account may send, said at connect.
    let can = world.account("T01ALPHA").can;
    assert_eq!((can.max_file_bytes, can.max_files, can.custom_emoji), (1 << 30, 10, protocol::EmojiScope::All));
    assert!(can.schedule && can.suppress_embeds && !can.gifs && !can.stickers && !can.silent);
}

#[test]
fn info_mute_pins_files_emoji_threads_and_a_scheduled_send_ask_slack_once_each() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let info = match world.ask(Request::ConversationInfo { conversation: general.clone() }) {
        Response::ConversationInfo { info } => info,
        other => panic!("{other:?}"),
    };
    assert_eq!(info.members, Some(4));
    assert_eq!(world.fake.calls(ALPHA, "conversations.info").last().unwrap().params["include_num_members"], "true");
    // A DM's is two people, and asks nothing.
    let before = world.fake.calls(ALPHA, "conversations.info").len();
    match world.ask(Request::ConversationInfo { conversation: alpha("D0ANA") }) {
        Response::ConversationInfo { info } => assert_eq!(info.members, Some(2)),
        other => panic!("{other:?}"),
    }
    assert_eq!(world.fake.calls(ALPHA, "conversations.info").len(), before);

    // Mute and unmute: the web client's notification call, and the row follows.
    assert_eq!(world.ask(Request::SetMute { conversation: general.clone(), notify: protocol::Notify::Nothing }), Response::Ok);
    let set = world.fake.calls(ALPHA, "users.prefs.setNotifications");
    assert_eq!((set[0].params["name"].as_str(), set[0].params["value"].as_str(), set[0].params["channel_id"].as_str()), ("muted", "true", "C0GEN"));
    assert!(world.conversation(&general).unwrap().muted);
    assert_eq!(world.ask(Request::SetMute { conversation: general.clone(), notify: protocol::Notify::Mentions }), Response::Ok);
    let set = world.fake.calls(ALPHA, "users.prefs.setNotifications");
    assert!(set[1].params["prefs"].contains("mentions_dms") && set[1].params["channel_ids"] == "C0GEN");
    assert!(!world.conversation(&general).unwrap().muted);

    // Pins: the pinned messages, not the file pinned on its own.
    let pinned = match world.ask(Request::Pinned { conversation: general.clone() }) {
        Response::Pinned { messages, .. } => messages,
        other => panic!("{other:?}"),
    };
    assert_eq!(pinned.iter().map(|message| message.text.as_str()).collect::<Vec<_>>(), ["Release checklist lives in the wiki"]);

    // Files shared here, from Slack's list, because the user asked; links have no such list.
    let shared = |kind, search| world.ask(Request::Shared { conversation: general.clone(), kind, before: None, limit: 25, search });
    match shared(protocol::SharedKind::Media, true) {
        Response::Shared { items, complete, total, .. } => {
            assert!(complete && total == Some(12));
            assert_eq!((items[0].message_id.as_str(), items[0].attachment.as_ref().unwrap().name.as_str()), ("1760000201.000201", "map.png"));
        }
        other => panic!("{other:?}"),
    }
    let listed = world.fake.calls(ALPHA, "files.list");
    assert_eq!((listed[0].params["channel"].as_str(), listed[0].params["types"].as_str()), ("C0GEN", "images"));
    assert!(matches!(shared(protocol::SharedKind::Links, true), Response::Error { code: ErrorCode::Unsupported, .. }));
    assert_eq!(world.fake.calls(ALPHA, "files.list").len(), 1);

    // The workspace's emoji, from the list kept a day: asked once, then read from the store.
    let emoji = |query: &str| match world.ask(Request::Catalog { account: alpha("x").account(), kind: protocol::CatalogKind::Emoji, space: None, query: query.into(), cursor: None }) {
        Response::Catalog { items, .. } => items.into_iter().map(|item| item.name).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let listed_before = world.fake.calls(ALPHA, "emoji.list").len();
    assert_eq!(emoji("parrot"), [":parrot:", ":partyparrot:"], "an alias of a custom emoji is offered with its picture");
    assert!(!emoji("").contains(&":shipit:".to_owned()), "an alias of a standard emoji is the shell's own list's");
    assert!(emoji("").contains(&":partyparrot:".to_owned()));
    assert!(world.fake.calls(ALPHA, "emoji.list").len() <= listed_before.max(1));

    // The Threads list: the followed threads, asked once, recorded as followed with their
    // newest replies; a thread in a channel not in the sidebar is left alone.
    let threads = |world: &World| match world.ask(Request::Threads { space: alpha("T01ALPHA"), offset: 0, limit: 50 }) {
        Response::Conversations { conversations, .. } => conversations,
        other => panic!("{other:?}"),
    };
    threads(&world);
    let thread = format!("C0GEN:{ROOT}");
    let followed = world.until("the followed thread", || world.conversation(&alpha(&thread)).filter(|row| row.followed));
    assert_eq!(followed.unread, 1, "one reply after the last read");
    assert!(world.messages(&alpha(&thread)).iter().any(|message| message.text == "moved to 3pm"));
    assert!(world.conversation(&alpha("C0ELSEWHERE:1760000100.000100")).is_none());
    threads(&world);
    sleep(Duration::from_millis(500));
    assert_eq!(world.fake.calls(ALPHA, "subscriptions.thread.getView").len(), 1);

    // Scheduled: Slack sends it later; nothing here runs a timer.
    let at = now() + 3600;
    let with = protocol::Outgoing { schedule: Some(at), suppress_embeds: true, ..Default::default() };
    let asked = Request::Send { conversation: general.clone(), client_id: "61".into(), text: "standup <in> 1h".into(), reply_to: None, with };
    assert!(matches!(world.ask(asked), Response::Sent { .. }));
    let scheduled = world.until("the schedule to be asked", || world.fake.calls(ALPHA, "chat.scheduleMessage").pop());
    assert_eq!(scheduled.params["post_at"], at.to_string());
    assert_eq!(scheduled.params["text"], "standup &lt;in&gt; 1h");
    assert_eq!(scheduled.params["unfurl_links"], "false");
    assert!(world.fake.calls(ALPHA, "chat.postMessage").is_empty());
}

#[test]
fn files_go_up_from_their_paths_and_the_socket_names_the_message() {
    let world = World::start();
    world.ready();
    // Beside chatd's directory: it never sends from its own.
    let dir = world.dir.with_extension("files");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("notes.txt");
    std::fs::write(&path, b"abc").unwrap();
    let with = protocol::Outgoing {
        files: vec![protocol::OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: false, as_file: false, original: false }],
        ..Default::default()
    };
    let asked = Request::Send { conversation: alpha("C0GEN"), client_id: "71".into(), text: "the notes".into(), reply_to: None, with };
    assert!(matches!(world.ask(asked), Response::Sent { .. }));
    let sent = world.until("the upload to settle", || {
        world.messages(&alpha("C0GEN")).into_iter().find(|message| message.client_id.as_deref() == Some("71")).filter(|message| message.state != SendState::Pending)
    });
    assert_eq!(sent.state, SendState::Sent);
    assert!(sent.id.starts_with("1760002"), "named by the socket: {}", sent.id);
    assert_eq!(sent.attachments.iter().map(|attachment| attachment.name.as_str()).collect::<Vec<_>>(), ["notes.txt"]);
    let address = world.fake.calls(ALPHA, "files.getUploadURLExternal");
    assert_eq!((address[0].params["filename"].as_str(), address[0].params["length"].as_str()), ("notes.txt", "3"));
    let upload = world.fake.file("/upload/v1/F0UP9").expect("the bytes went to the address Slack gave");
    assert_eq!(upload.bytes, 3);
    let completed = world.fake.calls(ALPHA, "files.completeUploadExternal");
    assert_eq!((completed[0].params["channel_id"].as_str(), completed[0].params["initial_comment"].as_str()), ("C0GEN", "the notes"));
    assert_eq!(completed[0].params["files"], r#"[{"id":"F0UP9","title":"notes.txt"}]"#);
    // Listed once: the socket's copy is the send's own.
    assert_eq!(world.messages(&alpha("C0GEN")).iter().filter(|message| message.id == sent.id).count(), 1);

    // A link preview dismissed: Slack is asked not to unfurl.
    let with = protocol::Outgoing { suppress_embeds: true, ..Default::default() };
    let asked = Request::Send { conversation: alpha("C0GEN"), client_id: "72".into(), text: "see https://example.com".into(), reply_to: None, with };
    assert!(matches!(world.ask(asked), Response::Sent { .. }));
    let posted = world.until("the post", || world.fake.calls(ALPHA, "chat.postMessage").pop());
    assert_eq!((posted.params["unfurl_links"].as_str(), posted.params["unfurl_media"].as_str()), ("false", "false"));
}
