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
    MessageKind, Network, Page, Presence, ReactionSet, EmojiScope, Relation, Request, Response, SendState, Style, Watching,
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
    /// Optional override for `dnd.info` response.
    dnd_info: Mutex<Option<Value>>,
    /// Channel bookmarks keyed by channel_id.
    bookmarks: Mutex<HashMap<String, Vec<Value>>>,
    /// Optional override for `drafts.list` response.
    drafts_list: Mutex<Option<Value>>,
    /// Scheduled messages keyed by scheduled_message_id.
    scheduled: Mutex<Vec<Value>>,
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
            dnd_info: Mutex::default(),
            bookmarks: Mutex::new(HashMap::from([(
                "C0GEN".to_owned(),
                vec![
                    json!({"id": "Bk1", "channel_id": "C0GEN", "title": "Runbook", "link": "https://docs.example.com/runbook", "emoji": ":book:", "type": "link"}),
                    json!({"id": "Bk2", "channel_id": "C0GEN", "title": "Team Canvas", "link": "https://alpha.slack.com/docs/T01ALPHA/F0CANVAS", "emoji": "", "type": "canvas"}),
                ],
            )])),
            drafts_list: Mutex::default(),
            scheduled: Mutex::default(),
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
        // Not in the fixture: a Slack Connect person, so the documented `users.info` path runs
        // and the people-count fixtures stay as they are.
        if token == ALPHA && id == "U0NIA" {
            return Some(json!({
                "id": "U0NIA", "team_id": "T0NORTH", "name": "nia", "deleted": false,
                "real_name": "Nia North", "is_bot": false, "is_app_user": false, "is_stranger": true,
                "profile": {"title": "Buyer", "real_name": "Nia North", "display_name": "Nia North",
                            "status_text": "", "status_emoji": "", "team": "T0NORTH"},
            }));
        }
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
            "chat.scheduleMessage" => {
                let mut list = self.scheduled.lock().unwrap();
                let id = if list.is_empty() {
                    "Q0SCHED".to_owned()
                } else {
                    format!("Q0SCHED{}", list.len() + 1)
                };
                let post_at = param("post_at").parse::<i64>().unwrap();
                let mut entry = json!({
                    "id": id,
                    "channel_id": channel,
                    "post_at": post_at,
                    "date_created": 1760000900,
                    "text": param("text"),
                });
                if !param("thread_ts").is_empty() {
                    entry["thread_ts"] = param("thread_ts").into();
                }
                list.push(entry);
                Some(json!({
                    "ok": true, "channel": channel, "scheduled_message_id": id, "post_at": post_at,
                    "message": {"type": "message", "user": me(token), "text": param("text"), "bot_id": null},
                }))
            }
            "chat.scheduledMessages.list" => {
                let list = self.scheduled.lock().unwrap();
                let filtered: Vec<Value> = list
                    .iter()
                    .filter(|item| channel.is_empty() || item["channel_id"].as_str() == Some(channel))
                    .cloned()
                    .collect();
                Some(json!({"ok": true, "scheduled_messages": filtered}))
            }
            "chat.deleteScheduledMessage" => {
                let target = param("scheduled_message_id");
                let mut list = self.scheduled.lock().unwrap();
                let before = list.len();
                list.retain(|item| item["id"].as_str() != Some(target));
                if list.len() == before {
                    return refused("invalid_scheduled_message_id");
                }
                Some(json!({"ok": true}))
            }
            "users.profile.set" => {
                let asked: Value = serde_json::from_str(param("profile")).unwrap();
                let mut profile = self.user(token, me(token)).unwrap()["profile"].clone();
                profile["status_text"] = asked["status_text"].clone();
                // Slack echoes the requested emoji, or defaults to speech_balloon when words have no emoji.
                let requested_emoji = asked["status_emoji"].as_str().unwrap_or("");
                let emoji = if !requested_emoji.is_empty() {
                    requested_emoji
                } else if asked["status_text"] == "" {
                    ""
                } else {
                    ":speech_balloon:"
                };
                profile["status_emoji"] = emoji.into();
                profile["status_expiration"] = asked["status_expiration"].clone();
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
                if channel == "C0RND" {
                    return ok(json!({"ok": true, "members": ["U0ME", "U0ANA"], "response_metadata": {"next_cursor": ""}}));
                }
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
            "conversations.open" => {
                let users = param("users");
                if users.contains(',') {
                    let parts: Vec<&str> = users.split(',').filter(|u| !u.is_empty()).collect();
                    let handles: Vec<String> = parts
                        .iter()
                        .map(|uid| {
                            self.user(token, uid)
                                .and_then(|u| u["name"].as_str().map(str::to_owned))
                                .unwrap_or_else(|| (*uid).to_lowercase())
                        })
                        .collect();
                    let name = format!("mpdm-justin--{}-1", handles.join("--"));
                    Some(json!({
                        "ok": true, "no_op": false, "already_open": false,
                        "channel": {
                            "id": "G0MPIMNEW", "name": name, "created": 1760000000,
                            "is_mpim": true, "is_private": true, "is_open": true,
                        },
                    }))
                } else {
                    Some(json!({
                        "ok": true, "no_op": false, "already_open": false,
                        "channel": {"id": "D0NEW", "created": 1760000000, "is_im": true, "is_org_shared": false,
                                    "user": users, "is_open": true, "priority": 0},
                    }))
                }
            }
            "conversations.create" => {
                let name = param("name");
                if name.is_empty() {
                    return refused("invalid_name_required");
                }
                if name.len() > 80 {
                    return refused("invalid_name_maxlength");
                }
                if name.contains(' ') || name.chars().any(|c| c.is_ascii_uppercase()) {
                    return refused("invalid_name_punctuation");
                }
                let is_private = param("is_private") == "true";
                let id = if is_private { "C0PRIVNEW" } else { "C0PUBNEW" };
                Some(json!({
                    "ok": true,
                    "channel": {
                        "id": id,
                        "name": name,
                        "is_channel": !is_private,
                        "is_group": is_private,
                        "is_private": is_private,
                        "is_member": true,
                        "created": 1760000000,
                        "topic": {"value": ""},
                        "purpose": {"value": ""}
                    }
                }))
            }
            "conversations.close" => {
                if channel.is_empty() {
                    return refused("invalid_arguments");
                }
                Some(json!({"ok": true}))
            }
            "usergroups.list" => Some(json!({
                "ok": true,
                "usergroups": [
                    {"id": "S0MINE", "handle": "oncall", "name": "On Call", "date_delete": 0, "users": [me(token), "U0ANA"]},
                    {"id": "S0OTHER", "handle": "design", "name": "Design", "date_delete": 0, "users": ["U0ANA", "U0BEN"]},
                    {"id": "S0OFF", "handle": "legacy", "name": "Legacy", "date_delete": 1700000000, "users": [me(token)]},
                ],
            })),
            "dnd.info" => self.dnd_info.lock().unwrap().clone().or_else(|| Some(json!({
                "ok": true,
                "dnd_enabled": false,
                "next_dnd_start_ts": 0,
                "next_dnd_end_ts": 0,
                "snooze_enabled": false,
                "snooze_endtime": 0,
            }))),
            "dnd.teamInfo" => Some(json!({"ok": true, "users": {}})),
            "dnd.setSnooze" => {
                let mins = param("num_minutes").parse::<i64>().unwrap_or(0);
                if mins <= 0 {
                    return refused("missing_duration");
                }
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
                Some(json!({
                    "ok": true,
                    "snooze_enabled": true,
                    "snooze_endtime": now + mins * 60,
                    "snooze_remaining": mins * 60,
                }))
            }
            "dnd.endSnooze" => Some(json!({
                "ok": true,
                "dnd_enabled": false,
                "next_dnd_start_ts": 0,
                "next_dnd_end_ts": 0,
                "snooze_enabled": false,
                "snooze_endtime": 0,
            })),
            "stars.add" | "stars.remove" => {
                if channel.is_empty() {
                    return refused("invalid_arguments");
                }
                Some(json!({"ok": true}))
            }
            "stars.list" => {
                Some(json!({
                    "ok": true,
                    "items": [{
                        "type": "message",
                        "channel": "C0GEN",
                        "message": {"type": "message", "ts": ROOT, "user": "U0ANA", "text": "Saved item"},
                        "date_create": 1760000500
                    }]
                }))
            }
            "reminders.add" => {
                if param("text").is_empty() || param("time").is_empty() {
                    return refused("cannot_parse");
                }
                Some(json!({
                    "ok": true,
                    "reminder": {
                        "id": "Rm01SAVE",
                        "text": param("text"),
                        "time": param("time").parse::<i64>().unwrap_or(0),
                        "complete_ts": 0
                    }
                }))
            }
            "reminders.list" => Some(json!({"ok": true, "reminders": []})),
            "reminders.complete" | "reminders.delete" => {
                if param("reminder").is_empty() {
                    return refused("not_found");
                }
                Some(json!({"ok": true}))
            }
            "subscriptions.thread.add" | "subscriptions.thread.remove" => {
                if channel.is_empty() || param("thread_ts").is_empty() {
                    return refused("thread_not_found");
                }
                Some(json!({"ok": true}))
            }
            "drafts.list" => self.drafts_list.lock().unwrap().clone().or_else(|| Some(json!({
                "ok": true,
                "drafts": []
            }))),
            "drafts.create" => Some(json!({
                "ok": true,
                "draft": {
                    "id": "Dr01LOCAL",
                    "client_last_updated_ts": "1760000900.000000",
                    "destinations": serde_json::from_str::<Value>(param("destinations")).unwrap_or(json!([])),
                    "blocks": serde_json::from_str::<Value>(param("blocks")).unwrap_or(json!([])),
                }
            })),
            "drafts.update" => {
                if param("draft_id").is_empty() {
                    return refused("draft_not_found");
                }
                Some(json!({
                    "ok": true,
                    "draft": {
                        "id": param("draft_id"),
                        "client_last_updated_ts": "1760000910.000000",
                        "destinations": serde_json::from_str::<Value>(param("destinations")).unwrap_or(json!([])),
                        "blocks": serde_json::from_str::<Value>(param("blocks")).unwrap_or(json!([])),
                    }
                }))
            }
            "drafts.delete" => {
                if param("draft_id").is_empty() {
                    return refused("draft_not_found");
                }
                Some(json!({"ok": true}))
            }
            "users.info" => self.user(token, param("user")).map(|user| json!({"ok": true, "user": user})),
            // A team id asks for that workspace. No id is the current one, from the fixture.
            "team.info" if !param("team").is_empty() => {
                let (id, name, domain) = match param("team") {
                    "T0NORTH" => ("T0NORTH", "Northwind", "northwind"),
                    "T0SOUTH" => ("T0SOUTH", "Contoso", "contoso"),
                    other => (other, "Elsewhere", "elsewhere"),
                };
                Some(json!({"ok": true, "team": {"id": id, "name": name, "domain": domain}}))
            }
            "conversations.info" => {
                if channel == "C0PUB" {
                    return ok(json!({
                        "ok": true,
                        "channel": {
                            "id": "C0PUB",
                            "name": "watercooler",
                            "is_channel": true,
                            "is_private": false,
                            "is_member": false,
                            "num_members": 42,
                            "topic": {"value": "Casual chat"},
                            "purpose": {"value": "Watercooler talk"},
                            "created": 1700000000
                        }
                    }));
                }
                if channel == "C0ARCH" {
                    return ok(json!({
                        "ok": true,
                        "channel": {
                            "id": "C0ARCH",
                            "name": "old-proj",
                            "is_channel": true,
                            "is_private": false,
                            "is_archived": true,
                            "is_member": false,
                            "num_members": 3,
                            "topic": {"value": "Retired"},
                            "purpose": {"value": "Archived project"},
                            "created": 1690000000
                        }
                    }));
                }
                let listed = self.fixture(token, "users.conversations").unwrap();
                let known = listed["channels"].as_array().unwrap().iter().find(|known| known["id"] == channel).cloned();
                known.map(|mut known| {
                    if param("include_num_members") == "true" {
                        known["num_members"] = 4.into();
                    }
                    if token == ALPHA && channel == "C0SIDE" {
                        known["is_ext_shared"] = true.into();
                        known["connected_team_ids"] = json!(["T01ALPHA", "T0NORTH"]);
                    }
                    if token == ALPHA && channel == "C0DEP" {
                        known["is_ext_shared"] = true.into();
                        known["is_pending_ext_shared"] = true.into();
                        known["pending_connected_team_ids"] = json!(["T0NORTH"]);
                        known["connected_team_ids"] = json!([]);
                    }
                    // Our own list of workspaces, which includes us. Not the other organisation.
                    if token == ALPHA && channel == "C0GEN" {
                        known["shared_team_ids"] = json!(["T01ALPHA", "T0SOUTH"]);
                    }
                    if token == ALPHA && channel == "C0RND" {
                        known["properties"] = json!({
                            "canvas": {"file_id": "F0CHANCANVAS", "is_empty": false}
                        });
                    }
                    json!({"ok": true, "channel": known})
                })
            }
            "conversations.history" => {
                if channel == "C0PUB" {
                    return ok(json!({
                        "ok": true,
                        "messages": [
                            {"type": "message", "user": "U0ANA", "text": "welcome to watercooler", "ts": "1760000950.000100"}
                        ],
                        "has_more": false
                    }));
                }
                let page = match (param("oldest"), param("inclusive"), param("latest")) {
                    ("", "true", _) => ".around",
                    ("", _, "") => "",
                    ("", ..) => ".older",
                    _ => ".after",
                };
                self.fixture(token, &format!("conversations.history.{channel}{page}"))
            }
            "conversations.replies" => self.fixture(token, &format!("conversations.replies.{channel}")),
            "activity.feed" => {
                let all = vec![
                    json!({
                        "id": "act-1",
                        "feed_ts": "1760000900.000100",
                        "is_unread": true,
                        "item": {
                            "type": "at_mention",
                            "message": {
                                "channel": "C0GEN",
                                "ts": "1760000900.000100",
                                "user": "U0ANA",
                                "text": "hey <@U0ME> check this"
                            }
                        }
                    }),
                    json!({
                        "id": "act-2",
                        "feed_ts": "1760000901.000100",
                        "is_unread": true,
                        "item": {
                            "type": "thread_v2",
                            "message": {
                                "channel": "C0GEN",
                                "ts": "1760000901.000100",
                                "thread_ts": ROOT,
                                "user": "U0BEN",
                                "text": "following up in thread"
                            }
                        }
                    }),
                    json!({
                        "id": "act-3",
                        "feed_ts": "1760000902.000100",
                        "is_unread": true,
                        "item": {
                            "type": "reaction",
                            "reaction": {
                                "channel": "C0GEN",
                                "item_ts": ROOT,
                                "user": "U0ANA",
                                "name": "+1"
                            },
                            "message": {
                                "channel": "C0GEN",
                                "ts": ROOT,
                                "text": "first message"
                            }
                        }
                    }),
                ];
                Some(json!({
                    "ok": true,
                    "items": all,
                    "response_metadata": {"next_cursor": ""}
                }))
            }
            "activity.mark" => Some(json!({"ok": true})),
            "bookmarks.list" => {
                let cid = param("channel_id");
                let list = self.bookmarks.lock().unwrap().get(cid).cloned().unwrap_or_default();
                Some(json!({"ok": true, "bookmarks": list}))
            }
            "bookmarks.add" => {
                let cid = param("channel_id").to_owned();
                let mut map = self.bookmarks.lock().unwrap();
                let entry = map.entry(cid.clone()).or_default();
                let id = format!("BkNew{}", entry.len() + 1);
                let bm = json!({
                    "id": id,
                    "channel_id": cid,
                    "title": param("title"),
                    "link": param("link"),
                    "emoji": param("emoji"),
                    "type": "link",
                });
                entry.push(bm.clone());
                Some(json!({"ok": true, "bookmark": bm}))
            }
            "bookmarks.edit" => {
                let cid = param("channel_id");
                let bid = param("bookmark_id");
                let mut map = self.bookmarks.lock().unwrap();
                let found = map.get_mut(cid).and_then(|list| list.iter_mut().find(|b| b["id"] == bid));
                let Some(bm) = found else {
                    return refused("not_found");
                };
                if !param("title").is_empty() {
                    bm["title"] = param("title").into();
                }
                if !param("link").is_empty() {
                    bm["link"] = param("link").into();
                }
                Some(json!({"ok": true, "bookmark": bm.clone()}))
            }
            "bookmarks.remove" => {
                let cid = param("channel_id");
                let bid = param("bookmark_id");
                if let Some(list) = self.bookmarks.lock().unwrap().get_mut(cid) {
                    list.retain(|b| b["id"] != bid);
                }
                Some(json!({"ok": true}))
            }
            "conversations.list" => {
                let cursor = param("cursor");
                let include_archived = param("exclude_archived") == "false";
                if cursor.is_empty() {
                    let mut list = vec![
                        json!({"id": "C0GEN", "name": "general", "is_channel": true, "is_private": false, "is_member": true, "num_members": 12, "purpose": {"value": "Company wide"}}),
                        json!({"id": "C0PUB", "name": "watercooler", "is_channel": true, "is_private": false, "is_member": false, "num_members": 42, "purpose": {"value": "Watercooler talk"}}),
                    ];
                    if include_archived {
                        list.push(json!({"id": "C0ARCH", "name": "old-proj", "is_channel": true, "is_private": false, "is_archived": true, "is_member": false, "num_members": 3, "purpose": {"value": "Archived project"}}));
                    }
                    Some(json!({
                        "ok": true,
                        "channels": list,
                        "response_metadata": {"next_cursor": "page2"}
                    }))
                } else {
                    Some(json!({
                        "ok": true,
                        "channels": [
                            {"id": "C0DESIGN", "name": "design-crit", "is_channel": true, "is_private": false, "is_member": false, "num_members": 19, "purpose": {"value": "Design critiques"}}
                        ],
                        "response_metadata": {"next_cursor": ""}
                    }))
                }
            }
            "conversations.leave" => {
                if channel == "C0GEN" {
                    return refused("cant_leave_general");
                }
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                Some(json!({"ok": true}))
            }
            "conversations.rename" => {
                let name = param("name");
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                if name == "taken" {
                    return refused("name_taken");
                }
                if name == "forbidden" {
                    return refused("restricted_action");
                }
                Some(json!({
                    "ok": true,
                    "channel": {
                        "id": channel,
                        "name": name,
                        "is_channel": true,
                        "is_private": false
                    }
                }))
            }
            "conversations.archive" => {
                if channel == "C0GEN" {
                    return refused("cant_archive_general");
                }
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                Some(json!({"ok": true}))
            }
            "conversations.unarchive" => {
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                Some(json!({"ok": true}))
            }
            "conversations.invite" => {
                let users = param("users");
                if channel.is_empty() || users.is_empty() {
                    return refused("invalid_arguments");
                }
                if users.split(',').any(|u| u == "U0ALREADY") {
                    return refused("already_in_channel");
                }
                if users.split(',').any(|u| u == "U0RESTRICTED") {
                    return refused("restricted_action");
                }
                Some(json!({"ok": true, "channel": {"id": channel}}))
            }
            "conversations.kick" => {
                let user = param("user");
                if channel.is_empty() || user.is_empty() {
                    return refused("invalid_arguments");
                }
                if user == me(token) {
                    return refused("cant_kick_self");
                }
                if channel == "C0GEN" {
                    return refused("cant_kick_from_general");
                }
                if user == "U0RESTRICTED" {
                    return refused("restricted_action");
                }
                Some(json!({"ok": true}))
            }
            "conversations.setTopic" => {
                let topic = param("topic");
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                if topic == "FORBIDDEN" {
                    return refused("restricted_action");
                }
                if topic.chars().count() > 250 {
                    return refused("too_long");
                }
                Some(json!({"ok": true, "topic": topic}))
            }
            "conversations.setPurpose" => {
                let purpose = param("purpose");
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                if purpose == "FORBIDDEN" {
                    return refused("not_authorized");
                }
                if purpose.chars().count() > 250 {
                    return refused("too_long");
                }
                Some(json!({"ok": true, "purpose": purpose}))
            }
            "pins.add" => {
                let ts = param("timestamp");
                if channel.is_empty() || ts.is_empty() {
                    return refused("invalid_arguments");
                }
                if ts == "1760000000.999999" {
                    return refused("permission_denied");
                }
                Some(json!({"ok": true}))
            }
            "pins.remove" => {
                let ts = param("timestamp");
                if channel.is_empty() || ts.is_empty() {
                    return refused("invalid_arguments");
                }
                if ts == "1760000000.999999" {
                    return refused("no_pin");
                }
                Some(json!({"ok": true}))
            }
            "conversations.join" => {
                if channel.is_empty() {
                    return refused("channel_not_found");
                }
                Some(json!({
                    "ok": true,
                    "channel": {
                        "id": channel,
                        "name": "watercooler",
                        "is_channel": true,
                        "is_private": false,
                        "is_member": true,
                        "num_members": 43,
                        "purpose": {"value": "Watercooler talk"}
                    }
                }))
            }
            "files.list" if channel.is_empty() => {
                let page = param("page").parse::<u32>().unwrap_or(1);
                let types = param("types");
                if page == 1 {
                    let mut files = vec![
                        json!({
                            "id": "F0SNIP1",
                            "name": "deploy.rs",
                            "title": "Deploy helper",
                            "mimetype": "text/plain",
                            "filetype": "rust",
                            "mode": "snippet",
                            "size": 128,
                            "created": 1760004000,
                            "user": me(token),
                            "url_private": format!("{}/files-pri/{}-F0SNIP1/deploy.rs", self.base, team(token)),
                            "permalink": "https://acme.slack.com/files/U0ME/F0SNIP1/deploy.rs",
                        }),
                        json!({
                            "id": "F0EXT1",
                            "name": "Q4 Roadmap",
                            "title": "Q4 Roadmap",
                            "filetype": "gdoc",
                            "pretty_type": "GSuite Document",
                            "mode": "external",
                            "is_external": true,
                            "external_type": "gdrive",
                            "created": 1760003900,
                            "user": "U0ANA",
                            "url_private": "https://docs.google.com/document/d/abc123/edit",
                            "permalink": "https://acme.slack.com/files/U0ANA/F0EXT1/q4-roadmap",
                        }),
                    ];
                    if types == "snippets" {
                        files.retain(|f| f["mode"] == "snippet");
                    }
                    Some(json!({
                        "ok": true,
                        "files": files,
                        "paging": {"count": 25, "total": 3, "page": 1, "pages": 2}
                    }))
                } else {
                    Some(json!({
                        "ok": true,
                        "files": [{
                            "id": "F0PAGE2",
                            "name": "spec.pdf",
                            "title": "Spec PDF",
                            "mimetype": "application/pdf",
                            "filetype": "pdf",
                            "mode": "hosted",
                            "size": 4096,
                            "created": 1760003800,
                            "user": me(token),
                            "url_private": format!("{}/files-pri/{}-F0PAGE2/spec.pdf", self.base, team(token)),
                            "permalink": "https://acme.slack.com/files/U0ME/F0PAGE2/spec.pdf",
                        }],
                        "paging": {"count": 25, "total": 3, "page": 2, "pages": 2}
                    }))
                }
            }
            "files.delete" => {
                let fid = param("file");
                if fid.is_empty() {
                    return refused("file_not_found");
                }
                if fid == "F0OTHER" {
                    return refused("cant_delete_file");
                }
                Some(json!({"ok": true}))
            }
            "gifs.trending" => {
                Some(json!({
                    "ok": true,
                    "results": [
                        {
                            "title": "Party Parrot",
                            "url": "https://media.giphy.com/media/partyparrot/giphy.gif",
                            "preview_url": format!("{}/files-tmb/gif-party.png", self.base),
                            "width": 320,
                            "height": 240,
                        },
                        {
                            "title": "Ship It",
                            "url": "https://media.giphy.com/media/shipit/giphy.gif",
                            "preview_url": format!("{}/files-tmb/gif-shipit.png", self.base),
                            "width": 400,
                            "height": 225,
                        }
                    ],
                    "response_metadata": {"next_cursor": "gif-page-2"}
                }))
            }
            "gifs.search" => {
                let q = param("q");
                Some(json!({
                    "ok": true,
                    "results": [
                        {
                            "title": format!("Found {q}"),
                            "url": format!("https://media.giphy.com/media/{q}/giphy.gif"),
                            "preview_url": format!("{}/files-tmb/gif-search-{q}.png", self.base),
                            "width": 300,
                            "height": 200,
                        }
                    ],
                    "response_metadata": {"next_cursor": ""}
                }))
            }
            "blocks.actions" => {
                let actions = param("actions");
                if actions.contains("\"fail_action\"") {
                    return refused("action_not_found");
                }
                if actions.is_empty() || param("container").is_empty() {
                    return refused("invalid_action");
                }
                Some(json!({"ok": true}))
            }
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

    /// Both workspaces connected, the first one's sidebar and unread state loaded, and the
    /// second one's sidebar too. Connected is not loaded: each workspace writes its tree on its
    /// own, so a test that reads the second one's must wait for it as well.
    fn ready(&self) {
        self.until("both workspaces to connect", || {
            let connected = self.accounts().iter().filter(|account| account.link == Link::Connected).count();
            (connected == 2).then_some(())
        });
        self.until("the sidebar and Slack's unread state", || {
            self.conversation(&alpha("C0DEP")).filter(|row| row.mentions == 2).map(|_| ())
        });
        self.until("the second workspace's sidebar", || {
            self.tree(&beta("T02BETA")).filter(|(groups, rows)| !groups.is_empty() && !rows.is_empty()).map(|_| ())
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
    assert_eq!(shipping.text, "Shipping today 🚀 @Justin :partyparrot:");
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
    // A custom emoji is a picture once the workspace list has been read, which a message
    // that names one does. The picker is not required.
    assert_eq!(reactions, [("🎉", 2, true, false), (":partyparrot:", 1, false, true)]);
    assert_eq!(world.fake.calls(ALPHA, "emoji.list").len(), 1);

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

    // Block Kit buttons render as interactive MessageControls instead of Unsupported placeholders.
    let buttons = by_id("1760000090.000090");
    assert_eq!(buttons.kind, MessageKind::Normal);
    assert_eq!(buttons.controls.len(), 1);
    assert_eq!(buttons.controls[0].block_id, "a1");
    assert_eq!(buttons.controls[0].action_id, "approve");
    assert_eq!(buttons.controls[0].label, "Approve");
    assert_eq!(buttons.controls[0].value, "42");

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

    // A GIF's row picture is Slack's moving copy at the size drawn, never the original.
    let gif = json!({
        "id": "F0GIF", "name": "hop.gif", "title": "hop.gif", "mimetype": "image/gif", "filetype": "gif",
        "size": 2310144, "mode": "hosted", "original_w": 498, "original_h": 280,
        "url_private": "https://files.slack.com/files-pri/T01ALPHA-F0GIF/hop.gif",
        "thumb_360": "https://files.slack.com/files-tmb/T01ALPHA-F0GIF-1c/hop_360.png", "thumb_360_w": 360, "thumb_360_h": 202,
        "thumb_360_gif": "https://files.slack.com/files-tmb/T01ALPHA-F0GIF-1c/hop_360.gif",
        "thumb_480": "https://files.slack.com/files-tmb/T01ALPHA-F0GIF-1c/hop_480.png", "thumb_480_w": 480, "thumb_480_h": 270,
        "thumb_480_gif": "https://files.slack.com/files-tmb/T01ALPHA-F0GIF-1c/hop_480.gif",
        "deanimate_gif": "https://files.slack.com/files-tmb/T01ALPHA-F0GIF-1c/hop_deanimate_gif.png",
    });
    let gif: Value = serde_json::from_str(&gif.to_string().replace("https://files.slack.com", &world.fake.base)).unwrap();
    let hop = "1760000500.000600";
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "file_share", "channel": "C0GEN", "user": "U0BEN", "text": "",
        "files": [gif], "upload": true, "ts": hop, "event_ts": hop, "channel_type": "channel",
    }));
    let hopped = world.until("the GIF to arrive", || world.message(&general, hop));
    let shown = hopped.attachments[0].thumb.clone().unwrap();
    assert_eq!((hopped.attachments[0].file.kind, shown.width, shown.height), (MediaKind::Gif, 480, 270));
    world.fetched(&general, &shown.id, Some(400));
    assert!(world.fake.file("/files-tmb/T01ALPHA-F0GIF-1c/hop_480.gif").is_some(), "{:?}", world.fake.files());
    assert!(world.fake.file("/files-tmb/T01ALPHA-F0GIF-1c/hop_480.png").is_none() && world.fake.file("/files-pri/T01ALPHA-F0GIF/hop.gif").is_none());
    // Gone again, so what follows counts the channel as it was.
    world.fake.push(ALPHA, json!({
        "type": "message", "subtype": "message_deleted", "hidden": true, "channel": "C0GEN",
        "deleted_ts": hop, "ts": "1760000502.500000", "event_ts": "1760000502.500000",
    }));
    world.until("the GIF to go", || world.message(&general, hop).is_none().then_some(()));

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
    assert_eq!(world.ask(Request::Typing { conversation: thread.clone(), activity: protocol::Compose::Typing }), Response::Ok);
    let typing = world.until("typing to reach Slack", || world.fake.heard(ALPHA, "user_typing").pop());
    assert_eq!((typing["channel"].as_str(), typing["thread_ts"].as_str()), (Some("C0GEN"), Some(ROOT)));

    // The huddle of a thread's conversation is its channel's, in the official app.
    let Response::Link { url } = world.ask(Request::Handoff { conversation: thread }) else {
        panic!("no link");
    };
    assert_eq!(url, "https://app.slack.com/huddle/T01ALPHA/C0GEN");
}

#[test]
fn a_message_with_no_replies_can_start_its_thread() {
    let world = World::start();
    world.ready();
    world.general();
    let thread = alpha("C0GEN:1760000300.000300");
    // Looking the conversation up does not invent the row. Sending does.
    assert!(matches!(
        world.ask(Request::Conversation { conversation: thread.clone() }),
        Response::Error { code: ErrorCode::NotFound, .. }
    ));
    let pending = world.send(&thread, "1760400000999999", "In the thread.");
    assert_eq!(pending.state, SendState::Pending);
    let row = world.conversation(&thread).expect("the thread row");
    assert_eq!((row.kind, row.title.as_str()), (ConversationKind::Thread, "Thread in #general"));
    assert_eq!((row.parent.as_deref(), row.root.as_deref()), (Some("C0GEN"), Some("1760000300.000300")));
    let posted = world.until("the first reply to reach Slack", || {
        world.fake.calls(ALPHA, "chat.postMessage").into_iter().find(|call| {
            call.params.get("thread_ts").map(String::as_str) == Some("1760000300.000300")
        })
    });
    assert_eq!(posted.params.get("channel").map(String::as_str), Some("C0GEN"));
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
    let watching = |people: Vec<Key>| match world.ask(Request::Watch(Watching { space: None, conversation: Some(general.clone()), people, members: None, rows: Vec::new(), focused: false })) {
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
        assert_eq!(world.ask(Request::Typing { conversation: general.clone(), activity: protocol::Compose::Typing }), Response::Ok);
    }
    let typing = world.until("typing to reach Slack", || world.fake.heard(ALPHA, "user_typing").pop());
    let first_typed = Instant::now();
    assert_eq!((typing["channel"].as_str(), typing.get("thread_ts")), (Some("C0GEN"), None));
    assert_eq!(world.ask(Request::Typing { conversation: alpha("D0ANA"), activity: protocol::Compose::Typing }), Response::Ok);
    world.until("typing in another conversation", || (typed("D0ANA") == 1).then_some(()));
    sleep(Duration::from_millis(4000));
    assert_eq!(world.ask(Request::Typing { conversation: general.clone(), activity: protocol::Compose::Typing }), Response::Ok);
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
    // The screen's people are queried. The account's own presence is asked for on its own
    // clock, so the query naming the screen may not be the last one in the book.
    assert!(
        world.fake.heard(ALPHA, "presence_query").iter().any(|query| query["ids"] == json!(["U0ANA"])),
        "no presence_query named the person on screen: {:?}",
        world.fake.heard(ALPHA, "presence_query")
    );
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
    assert_eq!(world.ask(Request::Typing { conversation: general.clone(), activity: protocol::Compose::Typing }), Response::Ok);
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
    let react = |emoji: &str, on: bool| world.ask(Request::React { conversation: general.clone(), message_id: shipping.to_owned(), emoji: emoji.to_owned(), on , burst: false });

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
    assert_eq!(refused, Response::Error { code: ErrorCode::Refused, detail: "You have added as many reactions to this message as Slack allows.".to_owned() });
    assert_eq!(reaction("🚫"), None);
    assert_eq!(world.fake.calls(ALPHA, "reactions.add").len(), 4, "a refused reaction is not tried again");

    // What Slack takes: the emoji chatd can name, in any tone, the workspace's own, 23 of mine
    // and 50 different a message. One it cannot name is refused here and never asked of Slack.
    let Response::Reactions { rules, .. } = world.ask(Request::Reactions { conversation: general.clone() }) else {
        panic!("no rules");
    };
    assert_eq!((rules.set, rules.custom, rules.mine, rules.distinct, rules.tones), (ReactionSet::Only, EmojiScope::All, 23, 50, true));
    assert!(rules.only.contains(&"👀".to_owned()));
    assert!(rules.only.len() > 1_500, "{}", rules.only.len());
    assert!(rules.only.iter().any(|emoji| emoji == "🏴󠁧󠁢󠁷󠁬󠁳󠁿"));
    let unnamed = react("\u{E000}", true);
    assert_eq!(unnamed, Response::Error { code: ErrorCode::Refused, detail: "That reaction isn't allowed here.".to_owned() });
    assert_eq!(world.fake.calls(ALPHA, "reactions.add").len(), 4, "never asked of Slack");
    assert_eq!(reaction("\u{E000}"), None);

    // An edit changes nothing until Slack accepts it, and is there when the answer is.
    let mine = "1760000100.000100";
    let edit = Request::Edit {
        conversation: general.clone(),
        message_id: mine.to_owned(),
        text: "good morning <team> & all".to_owned(),
        spans: Vec::new(),
    };
    assert_eq!(world.ask(edit), Response::Ok);
    let edited = world.message(&general, mine).unwrap();
    assert_eq!((edited.text.as_str(), edited.edited, edited.outgoing), ("good morning <team> & all", true, true));
    let updated = world.fake.calls(ALPHA, "chat.update").pop().unwrap();
    assert_eq!((said(&updated, "channel"), said(&updated, "ts")), ("C0GEN".to_owned(), mine.to_owned()));
    assert_eq!(updated.params["text"], "good morning &lt;team&gt; &amp; all");
    // Only my own: chatd refuses the rest before Slack hears of it.
    let theirs = Request::Edit { conversation: general.clone(), message_id: ROOT.to_owned(), text: "mine now".to_owned(), spans: Vec::new() };
    assert!(matches!(world.ask(theirs), Response::Error { code: ErrorCode::Refused, .. }), "someone else's message is not mine to edit");
    assert_eq!(world.fake.calls(ALPHA, "chat.update").len(), 1);

    // A delete, likewise.
    assert_eq!(world.ask(Request::Delete { conversation: general.clone(), message_id: mine.to_owned(), scope: protocol::DeleteScope::Everyone }), Response::Ok);
    assert!(world.message(&general, mine).is_none());
    let deleted = world.fake.calls(ALPHA, "chat.delete").pop().unwrap();
    assert_eq!((said(&deleted, "channel"), said(&deleted, "ts")), ("C0GEN".to_owned(), mine.to_owned()));
    // Slack's own events for both change nothing more.
    sleep(Duration::from_millis(900));
    assert_eq!(world.messages(&general).len(), 6);

    // Slack says no to an edit: nothing changed, and the answer is its reason.
    world.fake.refusing.lock().unwrap().insert("chat.delete".to_owned(), "cant_delete_message".to_owned());
    let refused = world.ask(Request::Delete { conversation: general.clone(), message_id: shipping.to_owned(), scope: protocol::DeleteScope::Everyone });
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
    let away = Request::SetPresence { account: account.clone(), presence: Presence::Idle, status: Some("Lunch".to_owned()), status_emoji: None, clear_at: None };
    assert_eq!(world.ask(away), Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "users.setPresence").pop().unwrap().params["presence"], "away");
    let set = world.fake.calls(ALPHA, "users.profile.set").pop().unwrap();
    let profile: Value = serde_json::from_str(&set.params["profile"]).unwrap();
    assert_eq!((profile["status_text"].as_str(), profile["status_emoji"].as_str()), (Some("Lunch"), Some("")));
    let shown = world.account("T01ALPHA");
    assert_eq!((shown.presence, shown.status.as_str()), (Presence::Idle, "💬 Lunch"));
    // Back, with the status left alone: one call, and the text stays.
    let back = Request::SetPresence { account: account.clone(), presence: Presence::Online, status: None, status_emoji: None, clear_at: None };
    assert_eq!(world.ask(back), Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "users.setPresence").pop().unwrap().params["presence"], "auto");
    assert_eq!(world.fake.calls(ALPHA, "users.profile.set").len(), 1);
    let shown = world.account("T01ALPHA");
    assert_eq!((shown.presence, shown.status.as_str()), (Presence::Online, "💬 Lunch"));
    // Clearing the status.
    let clear = Request::SetPresence { account: account.clone(), presence: Presence::Online, status: Some(String::new()), status_emoji: None, clear_at: None };
    assert_eq!(world.ask(clear), Response::Ok);
    assert_eq!(world.account("T01ALPHA").status, "");
    // Do Not Disturb is not built: it is refused in words and nothing is shown as set.
    let quiet = world.ask(Request::SetPresence { account: account.clone(), presence: Presence::Dnd, status: None, status_emoji: None, clear_at: None });
    assert!(matches!(&quiet, Response::Error { code: ErrorCode::Refused, detail } if detail.contains("Do Not Disturb")), "{quiet:?}");
    assert_eq!(world.account("T01ALPHA").presence, Presence::Online);
    assert_eq!(world.fake.calls(ALPHA, "users.setPresence").len(), 3);
    // Set away from another device: Slack says so on the socket.
    world.fake.push(ALPHA, json!({"type": "manual_presence_change", "presence": "away"}));
    world.until("my own presence from elsewhere", || (world.account("T01ALPHA").presence == Presence::Idle).then_some(()));

    // A profile card is users.info plus one labelled profile, and only because it was opened.
    let before = world.fake.calls(ALPHA, "users.info").len();
    world.until("Ana to be known", || world.conversation(&alpha("D0ANA")));
    let Response::Profile { profile } = world.ask(Request::Profile { person: alpha("U0ANA"), space: None }) else {
        panic!("no profile");
    };
    // The title is a title, not a bio (06, story 3). The display name stays the row; the full
    // name is the card's other line.
    assert_eq!((profile.title.as_str(), profile.bio.as_str()), ("Design lead", ""));
    assert_eq!((profile.time_zone.as_str(), profile.utc_offset), ("Europe/Lisbon", Some(3600)));
    assert_eq!((profile.person.name.as_str(), profile.person.link.as_str()), ("Ana", "slack://user?team=T01ALPHA&id=U0ANA"));
    assert!(profile.fields.iter().any(|(label, value)| label == "Full name" && value == "Ana Lima"));
    assert_eq!(world.fake.calls(ALPHA, "users.info").len(), before + 1);
    let labelled = world.fake.calls(ALPHA, "users.profile.get");
    assert_eq!(labelled.len(), 1);
    assert_eq!((labelled[0].params.get("user").map(String::as_str), labelled[0].params.get("include_labels").map(String::as_str)), (Some("U0ANA"), Some("true")));
    // Opened again from the Space (an answer chatd itself has not kept), Slack is still not
    // asked again: one `users.info` and one labelled profile per person per hour.
    let Response::Profile { profile } = world.ask(Request::Profile { person: alpha("U0ANA"), space: Some(alpha("T01ALPHA")) }) else {
        panic!("no profile");
    };
    assert_eq!(profile.title.as_str(), "Design lead");
    assert_eq!(world.fake.calls(ALPHA, "users.info").len(), before + 1);
    assert_eq!(world.fake.calls(ALPHA, "users.profile.get").len(), 1);

    // Signing out of one workspace has nothing to end on Slack, so it answers at once, and the
    // browser's session, which the other workspace shares, is left alone.
    let asked = Instant::now();
    let Response::Status { accounts, .. } = world.ask(Request::SignOut { account: beta("").account(), erase: false }) else {
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
            ("dnd.info", connections),
            ("drafts.list", connections),
            ("edge:users/info", 1),
            ("team.info", 1),
            ("usergroups.list", connections),
            ("users.channelSections.list", sidebars),
            ("users.conversations", sidebars),
            ("users.prefs.get", connections),
        ];
        if connections > 1 {
            expected.push(("dnd.teamInfo", connections - 1));
        }
        expected.sort();
        expected.into_iter().map(|(method, count)| (method.to_owned(), count)).collect()
    };
    assert_eq!(world.fake.counted(ALPHA), expected(5, 1, 1));
    // Any two calls are at least a fifth of a second apart, whatever they are. A loaded
    // machine can hold one send back and let the next overtake it, so one close pair is
    // allowed; a missing pacer bunches them all.
    let mut times: Vec<Instant> = all.iter().filter(|call| call.token == ALPHA).map(|call| call.at).collect();
    times.sort();
    let close = times.windows(2).filter(|pair| pair[1] - pair[0] < Duration::from_millis(150)).count();
    assert!(
        close <= 1,
        "the calls bunch up: {:?}",
        times.windows(2).map(|pair| pair[1] - pair[0]).collect::<Vec<_>>()
    );

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
    assert!(can.schedule && can.suppress_embeds && can.gifs && !can.stickers && !can.silent);
    assert!(can.search, "Enter asks Slack itself for a workspace or channel");
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
    assert_eq!(world.ask(Request::SetMute { conversation: general.clone(), notify: protocol::Notify::Nothing, until: None }), Response::Ok);
    let set = world.fake.calls(ALPHA, "users.prefs.setNotifications");
    assert_eq!((set[0].params["name"].as_str(), set[0].params["value"].as_str(), set[0].params["channel_id"].as_str()), ("muted", "true", "C0GEN"));
    assert!(world.conversation(&general).unwrap().muted);
    assert_eq!(world.ask(Request::SetMute { conversation: general.clone(), notify: protocol::Notify::Mentions, until: None }), Response::Ok);
    let set = world.fake.calls(ALPHA, "users.prefs.setNotifications");
    assert!(set[1].params["prefs"].contains("mentions_dms") && set[1].params["channel_ids"] == "C0GEN");
    assert!(!world.conversation(&general).unwrap().muted);

    // Pins: includes both pinned messages and pinned files, newest first.
    let pinned = match world.ask(Request::Pinned { conversation: general.clone() }) {
        Response::Pinned { messages, .. } => messages,
        other => panic!("{other:?}"),
    };
    assert_eq!(pinned.len(), 2);
    assert!(pinned.iter().any(|m| m.text == "Release checklist lives in the wiki"));
    assert!(pinned.iter().any(|m| m.text == "logo.png"));

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
    // Links tab is fed from channel bookmarks (bookmarks.list), not files.list.
    match shared(protocol::SharedKind::Links, true) {
        Response::Shared { items, .. } => assert!(!items.is_empty()),
        other => panic!("{other:?}"),
    }
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
    // Filing and unread counts are committed separately; wait for both updates.
    let followed = world.until("the followed thread and its unread count", || world.conversation(&alpha(&thread)).filter(|row| row.followed && row.unread == 1));
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
fn a_search_asks_slacks_own_search_once_and_maps_hits_to_their_conversations() {
    let world = World::start();
    world.ready();
    world.general();

    // One conversation: Slack's own search, narrowed to it, and the hit mapped like the
    // cache's. A channel the sidebar does not know is not a hit we can open.
    let asked = world.ask(Request::SearchNetwork { conversation: Some(alpha("C0GEN")), space: None, query: "deploy".into(), account: None, network: None, filters: protocol::SearchFilters::default(), cursor: None  });
    let Response::MessageHits { hits, .. } = asked else { panic!("{asked:?}") };
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    assert_eq!(hit.conversation, alpha("C0GEN"));
    assert_eq!((hit.message_id.as_str(), hit.sent), ("1760000330.000330", 1760000330));
    assert_eq!((hit.sender.as_str(), hit.title.as_str(), hit.space_title.as_str()), ("Ana", "#general", "Acme"));
    assert!(hit.snippet.contains("deploy") && !hit.snippet.contains('`'), "{}", hit.snippet);
    assert!(!hit.outgoing);

    let calls = world.fake.calls(ALPHA, "search.messages");
    assert_eq!(calls.len(), 1, "one press of Enter, one search");
    assert_eq!((calls[0].params["query"].as_str(), calls[0].params["sort"].as_str()), ("deploy in:#general", "timestamp"));
    assert!(world.fake.calls(BETA, "search.messages").is_empty(), "the other workspace is not asked");

    // The workspace's own search: no channel filter, and every hit in a conversation the
    // sidebar knows, newest first.
    let asked = world.ask(Request::SearchNetwork { conversation: None, space: Some(alpha("T01ALPHA")), query: "deploy".into(), account: None, network: None, filters: protocol::SearchFilters::default(), cursor: None  });
    let Response::MessageHits { hits, .. } = asked else { panic!("{asked:?}") };
    let mapped: Vec<(&str, &str)> = hits.iter().map(|hit| (hit.conversation.id.as_str(), hit.title.as_str())).collect();
    assert_eq!(mapped, [("C0GEN", "#general"), ("C0DEP", "#deploys")]);
    let calls = world.fake.calls(ALPHA, "search.messages");
    assert_eq!((calls.len(), calls[1].params["query"].as_str()), (2, "deploy"));
}

#[test]
fn a_search_slack_will_not_do_is_refused_in_words_rather_than_faked() {
    let world = World::start_with(|fake| {
        fake.refusing.lock().unwrap().insert("search.messages".into(), "not_allowed_token_type".into());
    });
    world.ready();
    // The web session cannot reach Slack's search: the user is told, and no hit is invented.
    let refused = world.ask(Request::SearchNetwork { conversation: Some(alpha("C0GEN")), space: None, query: "deploy".into(), account: None, network: None, filters: protocol::SearchFilters::default(), cursor: None  });
    assert!(matches!(&refused, Response::Error { detail, .. } if detail == "Slack's own search isn't reachable with this session."), "{refused:?}");
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
        files: vec![protocol::OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: false, as_file: false, original: false, view_once: false, media: None }],
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

#[test]
fn a_slack_connect_channel_names_the_other_organisation() {
    let world = World::start();
    world.ready();
    let info = |id: &str| match world.ask(Request::ConversationInfo { conversation: alpha(id) }) {
        Response::ConversationInfo { info } => *info,
        other => panic!("{other:?}"),
    };
    let looked_up = |team: &str| {
        world.fake.calls(ALPHA, "team.info").into_iter().filter(|call| call.params.get("team").map(String::as_str) == Some(team)).count()
    };

    // Joined: the other organisation, not ours, and not a pending invitation.
    let side = info("C0SIDE");
    assert_eq!(side.external, "Northwind");
    assert!(!side.description.contains("Invitation"), "{}", side.description);
    assert_eq!(looked_up("T0NORTH"), 1);

    // Pending: the same name, from the note, and where Slack accepts the invite.
    let deploys = info("C0DEP");
    assert_eq!(deploys.external, "Northwind");
    assert!(deploys.description.contains("Northwind"), "{}", deploys.description);
    assert!(deploys.description.contains("Directories"), "{}", deploys.description);
    assert!(deploys.description.contains("External"), "{}", deploys.description);
    assert_eq!(looked_up("T0NORTH"), 1, "the name was already known");

    // `shared_team_ids` includes us and is not the other organisation.
    let general = info("C0GEN");
    assert_eq!(general.external, "");
    assert_eq!(looked_up("T0SOUTH"), 0);

    // A share from a workspace already named: its domain, with no further lookup.
    let ts = "1760000600.000600";
    world.fake.push(ALPHA, json!({
        "type": "message", "channel": "C0GEN", "user": "U0NIA", "text": "shared from buying",
        "ts": ts, "event_ts": ts,
        "attachments": [{
            "is_share": true, "channel_name": "buying", "text": "the order", "fallback": "the order",
            "from_url": "https://northwind.slack.com/archives/C0BUY/p1760000100000100",
        }],
    }));
    let arrived = world.until("the shared message", || world.message(&alpha("C0GEN"), ts));
    let shared = arrived.forwarded.as_ref().expect("the share");
    assert_eq!((shared.from_space.as_str(), shared.from_title.as_str()), ("Northwind", "#buying"));

    let Response::Profile { profile } = world.ask(Request::Profile { person: alpha("U0NIA"), space: None }) else {
        panic!("no profile");
    };
    assert!(profile.fields.iter().any(|(label, value)| label == "Organisation" && value == "Northwind"), "{:?}", profile.fields);
    assert_eq!(profile.person.mark, Some(protocol::Mark::External));

    let before = world.fake.calls(ALPHA, "conversations.open").len();
    let refused = world.ask(Request::StartDirect { person: alpha("buyer@northwind.example") });
    assert!(matches!(&refused, Response::Error { code: ErrorCode::Refused, detail } if detail.contains("Directories") && detail.contains("External")), "{refused:?}");
    let opened = world.fake.calls(ALPHA, "conversations.open");
    assert_eq!(opened.len(), before);
    assert!(opened.iter().all(|call| !call.params["users"].contains('@')));
}

/// The picture id `emoji.list` gives a custom emoji, without asking the picker.
fn emoji_id(name: &str, url: &str) -> String {
    let hash = url.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    format!("emoji:{name}:{hash:x}")
}

fn emoji_span<'a>(message: &'a Message, name: &str) -> Option<&'a protocol::MediaFile> {
    let needle = format!(":{name}:");
    message.spans.iter().find_map(|span| {
        let Style::Emoji { file } = &span.style else {
            return None;
        };
        let text = &message.text[span.start as usize..span.end as usize];
        (text == needle).then_some(file)
    })
}

#[test]
fn a_custom_emoji_is_a_picture_without_opening_the_picker() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let shipping = world.general().into_iter().find(|message| message.id == "1760000300.000300").unwrap();
    let file = emoji_span(&shipping, "partyparrot").expect("the custom emoji is a picture, and the picker was never opened");
    assert!(file.id.starts_with("emoji:partyparrot:") && file.kind == MediaKind::Emoji, "{}", file.id);
    let reaction = shipping.reactions.iter().find(|reaction| reaction.emoji == ":partyparrot:").unwrap();
    assert_eq!(reaction.file.as_ref().map(|file| file.id.as_str()), Some(file.id.as_str()));
    assert_eq!(world.fake.calls(ALPHA, "emoji.list").len(), 1, "one list for the workspace, not a catalog request");

    let wave = "https://emoji.slack-edge.com/T01ALPHA/waveblob/89ab.gif";
    world.fake.push(ALPHA, json!({
        "type": "user_status_changed",
        "user": {
            "id": "U0ANA", "name": "ana",
            "profile": {"display_name": "Ana", "title": "Design lead", "status_text": "shipping", "status_emoji": ":waveblob:"},
        },
    }));
    let ana = world.until("a custom status emoji", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Slack), relation: None, query: Some("Ana".into()), offset: 0, limit: 10,
        }) else {
            return None;
        };
        people.into_iter().find(|person| person.key == alpha("U0ANA") && person.emoji == ":waveblob:" && person.status.contains("shipping"))
    });
    assert_eq!(ana.emoji, ":waveblob:");
    let id = emoji_id("waveblob", wave);
    let Response::Media { files } = world.ask(Request::Fetch { retry: false, size: None, account: general.account(), ids: vec![id.clone()] }) else {
        panic!("no media");
    };
    assert!(files.iter().any(|file| file.id == id && file.kind == MediaKind::Emoji), "the status emoji is registered as a file");

    world.ask(Request::Watch(Watching { conversation: Some(general.clone()), ..Watching::default() }));
    world.fake.push(ALPHA, json!({
        "type": "message", "channel": "C0GEN", "user": "U0ANA", "text": "go :party-parrot:",
        "ts": "1760000501.000501", "event_ts": "1760000501.000501", "channel_type": "channel",
    }));
    world.until("the new name before it is renamed", || {
        world.message(&general, "1760000501.000501").filter(|message| emoji_span(message, "party-parrot").is_none())
    });
    world.fake.push(ALPHA, json!({
        "type": "emoji_changed", "subtype": "rename",
        "old_name": "partyparrot", "new_name": "party-parrot",
        "value": "https://emoji.slack-edge.com/T01ALPHA/partyparrot/0123abcd4567.gif",
        "event_ts": "1760000500.000500",
    }));
    let renamed = world.until("a rename is a picture without another list", || {
        world.message(&general, "1760000501.000501").and_then(|message| emoji_span(&message, "party-parrot").map(|file| file.id.clone()))
    });
    assert!(renamed.starts_with("emoji:party-parrot:") && world.fake.calls(ALPHA, "emoji.list").len() == 1, "{renamed}");
}

#[test]
fn starring_a_channel_tells_slack_and_a_removal_clears_it() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    assert!(!world.conversation(&general).unwrap().starred);

    let starred = world.ask(Request::Star { conversation: general.clone(), starred: true });
    assert!(matches!(starred, Response::Ok), "{starred:?}");
    let added = world.fake.calls(ALPHA, "stars.add");
    assert_eq!(added.len(), 1, "one star is one stars.add: {added:?}");
    assert_eq!(added[0].params.get("channel").map(String::as_str), Some("C0GEN"));
    assert!(!added[0].params.contains_key("timestamp"), "a channel star carries no message timestamp");
    world.until("#general to be starred", || world.conversation(&general).filter(|row| row.starred).map(|_| ()));

    world.fake.push(ALPHA, json!({
        "type": "star_removed",
        "user": "U0ME",
        "item": {"type": "channel", "channel": "C0GEN"},
        "event_ts": "1760003000.000000",
    }));
    world.until("Slack removing the star to clear it here", || world.conversation(&general).filter(|row| !row.starred).map(|_| ()));

    // A message star is Later, not the channel's star.
    world.fake.push(ALPHA, json!({
        "type": "star_added",
        "user": "U0ME",
        "item": {"type": "message", "channel": "C0GEN", "message": {"ts": "1760000100.000100"}},
        "event_ts": "1760003001.000000",
    }));
    sleep(Duration::from_millis(400));
    assert!(!world.conversation(&general).unwrap().starred, "starring a message does not star the channel");

    let again = world.ask(Request::Star { conversation: general.clone(), starred: true });
    assert!(matches!(again, Response::Ok), "{again:?}");
    let off = world.ask(Request::Star { conversation: general.clone(), starred: false });
    assert!(matches!(off, Response::Ok), "{off:?}");
    let removed = world.fake.calls(ALPHA, "stars.remove");
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert_eq!(removed[0].params.get("channel").map(String::as_str), Some("C0GEN"));
    assert!(!removed[0].params.contains_key("timestamp"));
    assert!(!world.conversation(&general).unwrap().starred);
}

/// A posted `/zoom` (or Meet, Teams, Webex) call is a `call` block. Slack fills `call.v1` when
/// the message is delivered. An old Slack call is `sh_room_created` with the same room clock a
/// huddle uses.
#[test]
fn a_call_block_is_a_meeting_card_with_a_join_link_and_an_old_call_names_itself() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let push = |ts: &str, event: Value| {
        world.fake.push(ALPHA, event);
        world.until("the call message", || world.message(&general, ts))
    };
    let started = 1_700_000_000_i64;

    let zoom = push("1760000600.000100", json!({
        "type": "message", "channel": "C0GEN", "user": "U0ANA", "text": "", "ts": "1760000600.000100",
        "blocks": [{
            "type": "call", "call_id": "R0ZOOM",
            "call": {
                "media_backend_type": "zoom",
                "v1": {
                    "id": "R0ZOOM", "name": "Standup", "join_url": "https://zoom.us/j/123456789",
                    "date_start": started,
                    "active_participants": [{"slack_id": "U0ANA"}, {"slack_id": "U0BEN"}, {"slack_id": "U0ME"}],
                },
            },
        }],
    }));
    assert_eq!(zoom.kind, MessageKind::Normal, "a call is not an interactive placeholder: {}", zoom.text);
    assert!(!zoom.text.contains("interactive message"));
    let card = zoom.card.as_deref().expect("the call is a card");
    assert_eq!((card.title.as_str(), card.text.as_str()), ("Standup", "Zoom meeting · 3 people"));
    let join = zoom.previews.first().expect("Join opens the meeting");
    assert_eq!((join.title.as_str(), join.url.as_str()), ("Join", "https://zoom.us/j/123456789"));

    let ended = push("1760000600.000200", json!({
        "type": "message", "channel": "C0GEN", "user": "U0ANA", "text": "", "ts": "1760000600.000200",
        "blocks": [{
            "type": "call", "call_id": "R0MEET",
            "call": {
                "media_backend_type": "google_meet",
                "v1": {
                    "name": "Planning", "join_url": "https://meet.google.com/abc-defg-hij",
                    "date_start": started, "date_end": started + 25 * 60,
                    "active_participants": [],
                },
            },
        }],
    }));
    let card = ended.card.as_deref().expect("an ended call is still a card");
    assert_eq!((card.title.as_str(), card.text.as_str()), ("Planning", "Ended · 25 min"));
    assert_eq!(ended.previews.first().map(|preview| preview.url.as_str()), Some("https://meet.google.com/abc-defg-hij"));

    let teams = push("1760000600.000300", json!({
        "type": "message", "channel": "C0GEN", "user": "U0BEN", "text": "", "ts": "1760000600.000300",
        "blocks": [{
            "type": "call", "call_id": "R0TEAMS",
            "call": {
                "v1": {
                    "media_backend_type": "microsoft_teams", "name": "Review",
                    "join_url": "https://teams.microsoft.com/l/meetup-join/19:meeting",
                    "date_start": started, "active_participants": [{"slack_id": "U0BEN"}],
                },
            },
        }],
    }));
    assert_eq!(teams.card.as_deref().map(|card| card.text.as_str()), Some("Microsoft Teams meeting · 1 person"));

    let webex = push("1760000600.000400", json!({
        "type": "message", "channel": "C0GEN", "user": "U0BEN", "text": "", "ts": "1760000600.000400",
        "blocks": [{
            "type": "call", "call_id": "R0WEBEX",
            "call": { "v1": { "media_backend_type": "webex", "name": "Sync", "date_start": started, "join_url": "https://webex.example/meet/sync" } },
        }],
    }));
    assert_eq!(webex.card.as_deref().map(|card| card.text.as_str()), Some("Webex meeting"));

    let old = push("1760000600.000500", json!({
        "type": "message", "subtype": "sh_room_created", "channel": "C0GEN", "user": "U0ANA", "text": "",
        "ts": "1760000600.000500",
        "room": { "date_start": started, "date_end": started + 25 * 60, "has_ended": true },
    }));
    assert_eq!((old.kind.clone(), old.text.as_str()), (MessageKind::System, "A call happened · 25 min"));

    let huddle = push("1760000600.000600", json!({
        "type": "message", "subtype": "huddle_thread", "channel": "C0GEN", "user": "U0ANA", "text": "",
        "ts": "1760000600.000600",
        "room": { "date_start": started, "date_end": started + 25 * 60, "has_ended": true, "call_family": "huddle" },
    }));
    assert_eq!(huddle.text, "A huddle happened · 25 min");
}

#[test]
fn closing_a_direct_or_leaving_a_group_dm_calls_conversations_close_and_drops_the_row() {
    let world = World::start();
    world.ready();
    let dm = alpha("D0ANA");
    let group = alpha("G0GRP");
    assert!(world.conversation(&dm).is_some(), "DM starts in the tree");
    assert!(world.conversation(&group).is_some(), "group DM starts in the tree");

    let closed = world.ask(Request::CloseDirect { conversation: dm.clone() });
    assert!(matches!(closed, Response::Ok), "CloseDirect should succeed, got {closed:?}");
    let left = world.ask(Request::Leave { conversation: group.clone() });
    assert!(matches!(left, Response::Ok), "Leave on group DM should succeed, got {left:?}");

    let calls = world.fake.calls(ALPHA, "conversations.close");
    let closed_ids: Vec<&str> = calls.iter().filter_map(|call| call.params.get("channel").map(String::as_str)).collect();
    assert_eq!(closed_ids, ["D0ANA", "G0GRP"]);
    world.until("closed DM to leave the tree", || world.conversation(&dm).is_none().then_some(()));
    world.until("left group DM to leave the tree", || world.conversation(&group).is_none().then_some(()));
}

#[test]
fn usergroup_mention_counts_as_mention_only_for_my_active_groups_and_follows_subteam_events() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let push = |ts: &str, text: &str| {
        world.fake.push(ALPHA, json!({
            "type": "message", "channel": "C0GEN", "user": "U0ANA", "text": text, "ts": ts,
        }));
        world.until("the usergroup message", || world.message(&general, ts))
    };

    let mine = push("1760000700.000100", "<!subteam^S0MINE> check");
    assert!(mine.mentions_me, "a mention of a user group I belong to must set mentions_me");
    assert_eq!(mine.text, "@oncall check");

    let other = push("1760000700.000200", "<!subteam^S0OTHER> check");
    assert!(!other.mentions_me, "a group I do not belong to must not set mentions_me");

    let disabled = push("1760000700.000300", "<!subteam^S0OFF> check");
    assert!(!disabled.mentions_me, "a disabled user group must be ignored");

    // Joining S0OTHER via socket makes it count as a mention.
    world.fake.push(ALPHA, json!({"type": "subteam_self_added", "subteam_id": "S0OTHER"}));
    sleep(Duration::from_millis(200));
    let now_mine = push("1760000700.000400", "<!subteam^S0OTHER> now mine");
    assert!(now_mine.mentions_me, "subteam_self_added should add the group to my groups");

    // Leaving S0MINE via socket stops it counting as a mention.
    world.fake.push(ALPHA, json!({"type": "subteam_self_removed", "subteam_id": "S0MINE"}));
    sleep(Duration::from_millis(200));
    let no_longer = push("1760000700.000500", "<!subteam^S0MINE> no longer mine");
    assert!(!no_longer.mentions_me, "subteam_self_removed should remove the group from my groups");
}

#[test]
fn slack_dnd_snooze_sets_presence_dnd_and_dnd_updated_clears_it() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let world = World::start_with(|fake| {
        *fake.dnd_info.lock().unwrap() = Some(json!({
            "ok": true,
            "dnd_enabled": true,
            "next_dnd_start_ts": 0,
            "next_dnd_end_ts": 0,
            "snooze_enabled": true,
            "snooze_endtime": now + 3600,
        }));
    });
    world.ready();

    // Connecting reads dnd.info and reports our own presence as Dnd while snoozed.
    world.until("own presence to become Dnd from dnd.info", || {
        (world.account("T01ALPHA").presence == Presence::Dnd).then_some(())
    });
    assert_eq!(world.fake.calls(ALPHA, "dnd.info").len(), 1);

    // A mention while DND is active still raises the badge count on the conversation.
    let general = alpha("C0GEN");
    let before = world.conversation(&general).unwrap().mentions;
    world.fake.push(ALPHA, json!({
        "type": "message", "channel": "C0GEN", "user": "U0ANA", "text": "<@U0ME> ping", "ts": "1760000800.000100",
    }));
    world.until("mention badge to rise under DND", || {
        world.conversation(&general).filter(|row| row.mentions > before).map(|_| ())
    });

    // Another user's DND via dnd_updated_user shows as Presence::Dnd.
    world.fake.push(ALPHA, json!({
        "type": "dnd_updated_user",
        "user": "U0ANA",
        "dnd_status": {
            "dnd_enabled": true,
            "next_dnd_start_ts": now - 60,
            "next_dnd_end_ts": now + 3600,
        },
    }));
    world.until("Ana to show as Dnd", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Slack), relation: None, query: Some("Ana".into()), offset: 0, limit: 10,
        }) else {
            return None;
        };
        people.into_iter().find(|p| p.key == alpha("U0ANA") && p.presence == Presence::Dnd).map(|_| ())
    });

    // Ending our snooze via dnd_updated clears Dnd back to Online.
    world.fake.push(ALPHA, json!({
        "type": "dnd_updated",
        "user": "U0ME",
        "dnd_status": {
            "dnd_enabled": false,
            "next_dnd_start_ts": 0,
            "next_dnd_end_ts": 0,
            "snooze_enabled": false,
            "snooze_endtime": 0,
        },
    }));
    world.until("own presence to clear Dnd after dnd_updated", || {
        (world.account("T01ALPHA").presence == Presence::Online).then_some(())
    });
}

#[test]
fn slack_thread_broadcast_posts_to_thread_and_channel() {
    let world = World::start();
    world.ready();

    // Slack advertises thread_broadcast capability.
    assert!(world.account("T01ALPHA").can.thread_broadcast, "Slack should advertise thread_broadcast");

    let general = alpha("C0GEN");
    let thread = alpha(&format!("C0GEN:{ROOT}"));
    // Load general history so the root message is cached for quoting.
    world.messages(&general);
    world.until("root message cached", || world.message(&general, ROOT));

    // Plain reply in thread (broadcast: false) must NOT set reply_broadcast and must NOT appear in #general.
    let plain_cid = "cid-plain-thread";
    let Response::Sent { .. } = world.ask(Request::Send {
        conversation: thread.clone(),
        text: "only in thread".into(),
        client_id: plain_cid.into(),
        reply_to: None,
        with: protocol::Outgoing::default(),
    }) else {
        panic!("expected Sent");
    };
    let plain = world.until("plain thread reply sent", || {
        world.messages(&thread).into_iter().find(|m| m.client_id.as_deref() == Some(plain_cid) && m.state == SendState::Sent)
    });
    let post_plain = world.fake.calls(ALPHA, "chat.postMessage").into_iter().find(|c| c.params.get("text").map(String::as_str) == Some("only in thread")).unwrap();
    assert_eq!(post_plain.params.get("thread_ts").map(String::as_str), Some(ROOT));
    assert!(!post_plain.params.contains_key("reply_broadcast"));
    assert!(world.message(&general, &plain.id).is_none(), "non-broadcast reply should not appear in channel");

    // Broadcast reply in thread (broadcast: true) must set reply_broadcast=true and appear in both thread and #general with a quote.
    let bc_cid = "cid-broadcast-thread";
    let Response::Sent { .. } = world.ask(Request::Send {
        conversation: thread.clone(),
        text: "also in channel".into(),
        client_id: bc_cid.into(),
        reply_to: None,
        with: protocol::Outgoing { broadcast: true, ..protocol::Outgoing::default() },
    }) else {
        panic!("expected Sent");
    };
    let bc = world.until("broadcast thread reply sent", || {
        world.messages(&thread).into_iter().find(|m| m.client_id.as_deref() == Some(bc_cid) && m.state == SendState::Sent)
    });
    let post_bc = world.fake.calls(ALPHA, "chat.postMessage").into_iter().find(|c| c.params.get("text").map(String::as_str) == Some("also in channel")).unwrap();
    assert_eq!(post_bc.params.get("thread_ts").map(String::as_str), Some(ROOT));
    assert_eq!(post_bc.params.get("reply_broadcast").map(String::as_str), Some("true"));

    let in_channel = world.until("broadcast reply recorded in parent channel", || world.message(&general, &bc.id));
    assert_eq!(in_channel.text, "also in channel");
    assert_eq!(in_channel.reply_to.as_ref().map(|q| q.id.as_str()), Some(ROOT));
}

#[test]
fn slack_activity_feed_lists_mentions_threads_reactions_and_marks_read() {
    let world = World::start();
    world.ready();

    let account = AccountKey { network: Network::Slack, account: "T01ALPHA".into() };
    assert!(world.account("T01ALPHA").can.activity_feed, "Slack should advertise activity_feed");

    // Fetch unfiltered activity feed: mention, thread reply, and reaction.
    let Response::ActivityFeed { items, cursor } = world.ask(Request::ActivityFeed {
        account: account.clone(),
        kind: None,
        cursor: None,
        limit: 20,
    }) else {
        panic!("expected Response::ActivityFeed");
    };
    assert!(cursor.is_none());
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].kind, protocol::ActivityItemKind::Mention);
    assert_eq!(items[0].conversation, alpha("C0GEN"));
    assert_eq!(items[0].message_id, "1760000900.000100");
    assert_eq!(items[0].actor, "Ana");
    assert!(items[0].unread);

    assert_eq!(items[1].kind, protocol::ActivityItemKind::ThreadReply);
    assert_eq!(items[1].thread_id.as_deref(), Some(format!("C0GEN:{ROOT}").as_str()));

    assert_eq!(items[2].kind, protocol::ActivityItemKind::Reaction);
    assert_eq!(items[2].emoji, "👍");
    assert_eq!(items[2].message_id, ROOT);
    assert_eq!(items[2].actor, "Ana");

    // Filter by Reaction returns only the reaction row.
    let Response::ActivityFeed { items: reactions, .. } = world.ask(Request::ActivityFeed {
        account: account.clone(),
        kind: Some(protocol::ActivityItemKind::Reaction),
        cursor: None,
        limit: 20,
    }) else {
        panic!("expected Response::ActivityFeed");
    };
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].id, "act-3");

    // Mark one item read calls activity.mark with item_id.
    let Response::Ok = world.ask(Request::ActivityMarkRead {
        account,
        item_id: Some("act-3".into()),
    }) else {
        panic!("expected Response::Ok");
    };
    let mark = world.fake.calls(ALPHA, "activity.mark");
    assert_eq!(mark.len(), 1);
    assert_eq!(mark[0].params.get("item_id").map(String::as_str), Some("act-3"));
}

#[test]
fn slack_bookmarks_list_add_edit_remove_and_socket_invalidation() {
    let world = World::start();
    world.ready();

    assert!(world.account("T01ALPHA").can.bookmarks, "Slack should advertise bookmarks");
    let general = alpha("C0GEN");

    // ConversationInfo reads bookmarks.list: one link bookmark and one canvas bookmark.
    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: general.clone() }) else {
        panic!("expected ConversationInfo");
    };
    assert_eq!(info.bookmarks.len(), 2);
    assert_eq!(info.bookmarks[0].title, "Runbook");
    assert_eq!(info.bookmarks[0].link, "https://docs.example.com/runbook");
    assert_eq!(info.bookmarks[0].kind, protocol::BookmarkKind::Link);
    assert_eq!(info.bookmarks[1].title, "Team Canvas");
    assert_eq!(info.bookmarks[1].kind, protocol::BookmarkKind::Canvas);

    // SharedKind::Links is fed from channel bookmarks.
    let Response::Shared { items, .. } = world.ask(Request::Shared {
        conversation: general.clone(),
        kind: protocol::SharedKind::Links,
        before: None,
        limit: 20,
        search: true,
    }) else {
        panic!("expected Shared");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].url, "https://docs.example.com/runbook");

    // Add a bookmark via Request::BookmarkAdd.
    let Response::Bookmark { bookmark: added } = world.ask(Request::BookmarkAdd {
        conversation: general.clone(),
        title: "Status Page".into(),
        link: "https://status.example.com".into(),
        emoji: String::new(),
    }) else {
        panic!("expected Response::Bookmark");
    };
    assert_eq!(added.title, "Status Page");

    // Edit it via Request::BookmarkEdit.
    let Response::Bookmark { bookmark: edited } = world.ask(Request::BookmarkEdit {
        conversation: general.clone(),
        bookmark_id: added.id.clone(),
        title: Some("System Status".into()),
        link: None,
        emoji: None,
    }) else {
        panic!("expected Response::Bookmark");
    };
    assert_eq!(edited.title, "System Status");

    // Remove it via Request::BookmarkRemove.
    let Response::Ok = world.ask(Request::BookmarkRemove {
        conversation: general.clone(),
        bookmark_id: added.id,
    }) else {
        panic!("expected Response::Ok");
    };

    // Simulate a remote bookmark_deleted event: removes Bk2 on server and invalidates cached ConversationInfo.
    world.fake.bookmarks.lock().unwrap().get_mut("C0GEN").unwrap().retain(|b| b["id"] != "Bk2");
    world.fake.push(ALPHA, json!({
        "type": "bookmark_deleted",
        "channel_id": "C0GEN",
        "bookmark": {"id": "Bk2", "channel_id": "C0GEN"},
    }));
    sleep(Duration::from_millis(200));

    let Response::ConversationInfo { info: refreshed } = world.ask(Request::ConversationInfo { conversation: general }) else {
        panic!("expected ConversationInfo");
    };
    assert_eq!(refreshed.bookmarks.len(), 1);
    assert_eq!(refreshed.bookmarks[0].id, "Bk1");
}

#[test]
fn slack_browse_preview_and_join_public_channel() {
    let world = World::start();
    world.ready();

    assert!(world.account("T01ALPHA").can.browse_channels, "Slack should advertise browse_channels");
    let space = alpha("T01ALPHA");

    // Page 1 of public channels filtered by query "water".
    let Response::BrowsedChannels { channels, cursor } = world.ask(Request::BrowseChannels {
        space: space.clone(),
        query: "water".into(),
        cursor: None,
        limit: 50,
    }) else {
        panic!("expected BrowsedChannels");
    };
    assert_eq!(cursor.as_deref(), Some("page2"));
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].key, alpha("C0PUB"));
    assert_eq!(channels[0].name, "#watercooler");
    assert_eq!(channels[0].members, 42);
    assert!(!channels[0].joined);

    // Page 2 using cursor returns #design-crit.
    let Response::BrowsedChannels { channels: page2, cursor: cursor2 } = world.ask(Request::BrowseChannels {
        space: space.clone(),
        query: String::new(),
        cursor,
        limit: 50,
    }) else {
        panic!("expected BrowsedChannels");
    };
    assert!(cursor2.is_none());
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].name, "#design-crit");

    // Preview #watercooler: recorded as joined=false, read_only=true, outside the sidebar, with history loaded.
    let pub_key = alpha("C0PUB");
    let Response::Conversation { conversation: preview } = world.ask(Request::PreviewChannel {
        conversation: pub_key.clone(),
    }) else {
        panic!("expected Conversation");
    };
    assert!(!preview.joined);
    assert!(preview.read_only);
    assert!(preview.space.is_none(), "previewed channel must stay outside the sidebar");
    let msgs = world.messages(&pub_key);
    assert!(msgs.iter().any(|m| m.text == "welcome to watercooler"), "preview must load history");

    // Join #watercooler: calls conversations.join, marks joined=true, and places it in the sidebar tree.
    let Response::Conversation { conversation: joined } = world.ask(Request::Join {
        conversation: pub_key.clone(),
    }) else {
        panic!("expected Conversation after Join");
    };
    assert!(joined.joined);
    assert!(!joined.read_only);
    assert_eq!(joined.space.as_deref(), Some("T01ALPHA"));
    assert_eq!(world.fake.calls(ALPHA, "conversations.join").len(), 1);
}

#[test]
fn slack_block_kit_images_video_stacked_cards_and_layout() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    let ts = "1760000990.000100";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "ts": ts,
        "text": "Deploy summary",
        "blocks": [
            {"type": "header", "text": {"type": "plain_text", "text": "Release v4"}},
            {"type": "divider"},
            {
                "type": "section",
                "text": {"type": "mrkdwn", "text": "All green"},
                "accessory": {
                    "type": "image",
                    "image_url": "https://a.slack-edge.com/thumb.png",
                    "alt_text": "status badge"
                }
            },
            {
                "type": "image",
                "title": {"type": "plain_text", "text": "Hosted chart"},
                "image_url": "https://i0.wp.com/slack-imgs.com/chart.png",
                "alt_text": "chart"
            },
            {
                "type": "image",
                "title": {"type": "plain_text", "text": "External img"},
                "image_url": "https://untrusted.example.org/pic.png",
                "alt_text": "external"
            },
            {
                "type": "video",
                "title": {"type": "plain_text", "text": "Demo clip"},
                "title_url": "https://example.com/demo",
                "video_url": "https://example.com/demo/embed",
                "thumbnail_url": "https://a.slack-edge.com/video-thumb.jpg",
                "provider_name": "Vimeo",
                "description": {"type": "plain_text", "text": "Walkthrough"}
            },
            {
                "type": "markdown",
                "text": "*Bold note*"
            }
        ],
        "attachments": [
            {
                "color": "#36a64f",
                "author_name": "BuildBot",
                "title": "Build #101",
                "title_link": "https://ci.example.com/101",
                "text": "Passed in 2m",
                "fields": [
                    {"title": "Branch", "value": "main", "short": true},
                    {"title": "Commit", "value": "abc1234", "short": true}
                ],
                "image_url": "https://files.slack.com/files-pri/T01ALPHA-F1/build.png",
                "footer": "CI"
            },
            {
                "color": "#ff9900",
                "author_name": "AlertBot",
                "title": "Staging warning",
                "text": "Disk at 80%",
                "fields": [
                    {"title": "Host", "value": "stage-1", "short": true}
                ],
                "footer": "Monitor"
            }
        ]
    }));

    let msg = world.until("block kit message stored", || world.message(&general, ts));
    // Divider and external image fallback preserved in text; markdown rendered; hosted images attached inline.
    assert!(msg.text.contains("───"), "divider should render as separator line: {}", msg.text);
    assert!(msg.text.contains("[image: External img]"), "unhosted external image must stay named, not fetched");
    assert!(!msg.text.contains("Open in Slack"), "supported video/markdown/divider blocks must not trigger undrawn fallback");

    // Hosted section accessory image attached inline (slack-edge.com).
    assert!(msg.attachments.iter().any(|a| a.name == "status badge" && a.file.kind == MediaKind::Image));

    // Video block mapped to Preview with link and hosted thumbnail.
    let vid = msg.previews.iter().find(|p| p.url == "https://example.com/demo").expect("video block preview");
    assert_eq!(vid.title, "Demo clip");
    assert_eq!(vid.site, "Vimeo");
    assert!(vid.image.is_some(), "hosted video thumbnail should be preserved");

    // Two separate stacked cards with author, title_link, fields, and hosted card image.
    let cards: Vec<_> = msg.all_cards().collect();
    assert_eq!(cards.len(), 2, "both attachments should become separate cards");
    assert_eq!(cards[0].author, "BuildBot");
    assert_eq!(cards[0].title, "Build #101");
    assert_eq!(cards[0].title_link, "https://ci.example.com/101");
    assert_eq!(cards[0].fields.len(), 2);
    assert!(cards[0].image.is_some(), "hosted card image_url should be preserved");
    assert_eq!(cards[1].author, "AlertBot");
    assert_eq!(cards[1].title, "Staging warning");
    assert_eq!(cards[1].fields.len(), 1);
}

#[test]
fn slack_leave_rename_archive_and_browse_archived_channels() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    let side = alpha("C0SIDE");
    assert!(world.account("T01ALPHA").can.archive, "Slack should advertise archive capability");

    // Leaving #general refuses with a clear human-readable message.
    let err = world.ask(Request::Leave { conversation: general.clone() });
    match err {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("general"), "expected clear cant_leave_general message, got: {detail}");
        }
        other => panic!("expected Refused for leaving #general, got {other:?}"),
    }
    assert!(world.conversation(&general).is_some(), "#general must remain in the tree");

    // Rename a channel: strips leading # and updates stored row.
    let renamed = world.ask(Request::ConversationUpdate {
        conversation: side.clone(),
        title: Some("#side-renamed".into()),
        topic: None,
        description: None,
    });
    assert_eq!(renamed, Response::Ok);
    let rename_calls = world.fake.calls(ALPHA, "conversations.rename");
    assert_eq!(rename_calls.len(), 1);
    assert_eq!(rename_calls[0].params["channel"], "C0SIDE");
    assert_eq!(rename_calls[0].params["name"], "side-renamed");
    assert_eq!(world.conversation(&side).unwrap().title, "#side-renamed");

    // Rename refusal surfaces clear message.
    let taken = world.ask(Request::ConversationUpdate {
        conversation: side.clone(),
        title: Some("taken".into()),
        topic: None,
        description: None,
    });
    match taken {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("already exists"), "got: {detail}");
        }
        other => panic!("expected Refused for name_taken, got {other:?}"),
    }

    // Archiving #general refuses with cant_archive_general.
    let arch_gen = world.ask(Request::File {
        conversation: general.clone(),
        pinned: None,
        archived: Some(true),
        folder_add: None,
        folder_remove: None,
    });
    match arch_gen {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("general"), "got: {detail}");
        }
        other => panic!("expected Refused for archiving #general, got {other:?}"),
    }

    // Archive and unarchive a channel.
    let arch = world.ask(Request::File {
        conversation: side.clone(),
        pinned: None,
        archived: Some(true),
        folder_add: None,
        folder_remove: None,
    });
    assert_eq!(arch, Response::Ok);
    assert!(world.fake.calls(ALPHA, "conversations.archive").iter().any(|c| c.params["channel"] == "C0SIDE"));
    let row = world.conversation(&side).unwrap();
    assert!(row.archived && row.read_only, "archived channel must be marked archived and read-only");

    let unarch = world.ask(Request::File {
        conversation: side.clone(),
        pinned: None,
        archived: Some(false),
        folder_add: None,
        folder_remove: None,
    });
    assert_eq!(unarch, Response::Ok);
    assert_eq!(world.fake.calls(ALPHA, "conversations.unarchive").len(), 1);
    let row = world.conversation(&side).unwrap();
    assert!(!row.archived && !row.read_only, "unarchived channel must clear archived and read-only");

    // Leave a regular channel calls conversations.leave and drops it from the tree.
    let left = world.ask(Request::Leave { conversation: side.clone() });
    assert_eq!(left, Response::Ok);
    let leave_calls = world.fake.calls(ALPHA, "conversations.leave");
    assert!(leave_calls.iter().any(|c| c.params["channel"] == "C0SIDE"));
    world.until("left channel to leave the tree", || world.conversation(&side).is_none().then_some(()));

    // Channel browsing includes archived channels and previews them as read-only.
    let Response::BrowsedChannels { channels, .. } = world.ask(Request::BrowseChannels {
        space: alpha("T01ALPHA"),
        query: "old-proj".into(),
        cursor: None,
        limit: 50,
    }) else {
        panic!("expected BrowsedChannels");
    };
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].name, "#old-proj");
    assert!(channels[0].archived, "browsed archived channel must report archived=true");

    let arch_key = alpha("C0ARCH");
    let Response::Conversation { conversation: preview } = world.ask(Request::PreviewChannel {
        conversation: arch_key,
    }) else {
        panic!("expected Conversation for archived channel preview");
    };
    assert!(preview.read_only && preview.archived, "previewed archived channel must be read-only and archived");
}

#[test]
fn slack_invite_and_kick_channel_members() {
    let world = World::start();
    world.ready();

    let rnd = alpha("C0RND");
    let side = alpha("C0SIDE");
    let general = alpha("C0GEN");
    let row = world.conversation(&rnd).unwrap();
    assert!(row.rights.add_members && row.rights.remove_members, "writable Slack channels must advertise add/remove member rights");
    let ro = world.conversation(&side).unwrap();
    assert!(!ro.rights.add_members && !ro.rights.remove_members, "read-only Slack channels must not advertise add/remove member rights");

    // Load the member list first so roster is active.
    let Response::Members { members, .. } = world.ask(Request::Members {
        conversation: rnd.clone(),
        offset: 0,
        limit: 50,
    }) else {
        panic!("expected Members");
    };
    let initial_count = members.len();

    // Invite people to the channel.
    let added = world.ask(Request::MembersAdd {
        conversation: rnd.clone(),
        people: vec!["U0NEW1".into(), "U0NEW2".into()],
    });
    assert_eq!(added, Response::Ok);
    let invites = world.fake.calls(ALPHA, "conversations.invite");
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].params["channel"], "C0RND");
    assert_eq!(invites[0].params["users"], "U0NEW1,U0NEW2");

    // Roster reflects invited members immediately.
    let Response::Members { members: after_add, .. } = world.ask(Request::Members {
        conversation: rnd.clone(),
        offset: 0,
        limit: 50,
    }) else {
        panic!("expected Members after invite");
    };
    assert_eq!(after_add.len(), initial_count + 2);
    assert!(after_add.iter().any(|p| p.key.id == "U0NEW1"));
    assert!(after_add.iter().any(|p| p.key.id == "U0NEW2"));

    // Invite refusal: already_in_channel translated clearly.
    match world.ask(Request::MembersAdd {
        conversation: rnd.clone(),
        people: vec!["U0ALREADY".into()],
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("already"), "got: {detail}");
        }
        other => panic!("expected Refused for already_in_channel, got {other:?}"),
    }

    // Kick a member from the channel.
    let kicked = world.ask(Request::MembersRemove {
        conversation: rnd.clone(),
        person: "U0NEW1".into(),
    });
    assert_eq!(kicked, Response::Ok);
    let kicks = world.fake.calls(ALPHA, "conversations.kick");
    assert_eq!(kicks.len(), 1);
    assert_eq!(kicks[0].params["channel"], "C0RND");
    assert_eq!(kicks[0].params["user"], "U0NEW1");

    // Roster drops the kicked member immediately.
    let Response::Members { members: after_kick, .. } = world.ask(Request::Members {
        conversation: rnd.clone(),
        offset: 0,
        limit: 50,
    }) else {
        panic!("expected Members after kick");
    };
    assert_eq!(after_kick.len(), initial_count + 1);
    assert!(!after_kick.iter().any(|p| p.key.id == "U0NEW1"));

    // Kick refusals: cant_kick_self, cant_kick_from_general, restricted_action.
    match world.ask(Request::MembersRemove {
        conversation: rnd.clone(),
        person: "U0ME".into(),
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("yourself") || detail.to_lowercase().contains("leave"), "got: {detail}");
        }
        other => panic!("expected Refused for cant_kick_self, got {other:?}"),
    }
    match world.ask(Request::MembersRemove {
        conversation: general,
        person: "U0ANA".into(),
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("general"), "got: {detail}");
        }
        other => panic!("expected Refused for cant_kick_from_general, got {other:?}"),
    }
    match world.ask(Request::MembersRemove {
        conversation: rnd,
        person: "U0RESTRICTED".into(),
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("workspace"), "got: {detail}");
        }
        other => panic!("expected Refused for restricted_action, got {other:?}"),
    }
}

#[test]
fn slack_edit_channel_topic_and_description() {
    let world = World::start();
    world.ready();

    let rnd = alpha("C0RND");
    let side = alpha("C0SIDE");
    assert!(world.conversation(&rnd).unwrap().rights.edit_topic, "writable Slack channels must advertise edit_topic");
    assert!(!world.conversation(&side).unwrap().rights.edit_topic, "read-only Slack channels must not advertise edit_topic");

    // Set topic via ConversationUpdate -> conversations.setTopic, updating stored row immediately.
    let set_topic = world.ask(Request::ConversationUpdate {
        conversation: rnd.clone(),
        title: None,
        topic: Some("Ship Wave 4C".into()),
        description: None,
    });
    assert_eq!(set_topic, Response::Ok);
    let topic_calls = world.fake.calls(ALPHA, "conversations.setTopic");
    assert_eq!(topic_calls.len(), 1);
    assert_eq!(topic_calls[0].params["channel"], "C0RND");
    assert_eq!(topic_calls[0].params["topic"], "Ship Wave 4C");
    assert_eq!(world.conversation(&rnd).unwrap().topic, "Ship Wave 4C");

    // Set description via ConversationUpdate -> conversations.setPurpose without overwriting topic.
    let set_desc = world.ask(Request::ConversationUpdate {
        conversation: rnd.clone(),
        title: None,
        topic: None,
        description: Some("Random watercooler channel".into()),
    });
    assert_eq!(set_desc, Response::Ok);
    let purpose_calls = world.fake.calls(ALPHA, "conversations.setPurpose");
    assert_eq!(purpose_calls.len(), 1);
    assert_eq!(purpose_calls[0].params["channel"], "C0RND");
    assert_eq!(purpose_calls[0].params["purpose"], "Random watercooler channel");
    assert_eq!(world.conversation(&rnd).unwrap().topic, "Ship Wave 4C", "setting purpose must not clobber topic");

    // Refusals surface clear human-readable errors.
    match world.ask(Request::ConversationUpdate {
        conversation: rnd.clone(),
        title: None,
        topic: Some("FORBIDDEN".into()),
        description: None,
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("workspace"), "got: {detail}");
        }
        other => panic!("expected Refused for restricted setTopic, got {other:?}"),
    }
    match world.ask(Request::ConversationUpdate {
        conversation: rnd.clone(),
        title: None,
        topic: None,
        description: Some("FORBIDDEN".into()),
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("permission"), "got: {detail}");
        }
        other => panic!("expected Refused for not_authorized setPurpose, got {other:?}"),
    }
    match world.ask(Request::ConversationUpdate {
        conversation: rnd.clone(),
        title: None,
        topic: Some("x".repeat(251)),
        description: None,
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("250") || detail.to_lowercase().contains("long"), "got: {detail}");
        }
        other => panic!("expected Refused for too_long topic, got {other:?}"),
    }

    // Live socket events: channel_topic updates row.topic; channel_purpose invalidates ConversationInfo cache.
    let info_before = world.fake.calls(ALPHA, "conversations.info").len();
    let _ = world.ask(Request::ConversationInfo { conversation: rnd.clone() });
    assert_eq!(world.fake.calls(ALPHA, "conversations.info").len(), info_before + 1);

    world.fake.push(ALPHA, json!({
        "type": "message",
        "subtype": "channel_topic",
        "channel": "C0RND",
        "user": "U0ANA",
        "topic": "Live socket topic",
        "text": "<@U0ANA> set the channel topic: Live socket topic",
        "ts": "1760000991.000100"
    }));
    world.until("channel_topic to update row", || {
        (world.conversation(&rnd)?.topic == "Live socket topic").then_some(())
    });

    world.fake.push(ALPHA, json!({
        "type": "message",
        "subtype": "channel_purpose",
        "channel": "C0RND",
        "user": "U0ANA",
        "purpose": "Live socket purpose",
        "text": "<@U0ANA> set the channel description: Live socket purpose",
        "ts": "1760000992.000100"
    }));
    world.until("channel_purpose message stored", || world.message(&rnd, "1760000992.000100"));
    // Because channel_purpose called info_stale, asking ConversationInfo re-fetches conversations.info!
    let _ = world.ask(Request::ConversationInfo { conversation: rnd.clone() });
    assert!(
        world.fake.calls(ALPHA, "conversations.info").len() >= info_before + 2,
        "channel_purpose should invalidate cached ConversationInfo"
    );
}

#[test]
fn slack_mark_unread_channel_and_thread() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    // Load #general history so messages are cached, then mark read to the newest.
    let msgs = world.until("general history loaded", || {
        let m = world.messages(&general);
        (m.len() >= 3).then_some(m)
    });
    let newest = msgs.last().unwrap().id.clone();
    let second_newest = msgs[msgs.len() - 2].id.clone();
    let third_newest = msgs[msgs.len() - 3].id.clone();
    assert_eq!(world.ask(Request::MarkRead { conversation: general.clone(), message_id: newest }), Response::Ok);
    world.until("mark_read conversations.mark sent", || {
        (!world.fake.calls(ALPHA, "conversations.mark").is_empty()).then_some(())
    });
    assert_eq!(world.conversation(&general).unwrap().unread, 0);

    // Mark unread from the second-newest message: Slack receives conversations.mark with the message BEFORE it (third_newest).
    let marks_before = world.fake.calls(ALPHA, "conversations.mark").len();
    assert_eq!(
        world.ask(Request::MarkUnread {
            conversation: general.clone(),
            message_id: second_newest,
        }),
        Response::Ok
    );
    let row = world.conversation(&general).unwrap();
    assert_eq!(row.unread, 2, "marking unread from 2nd-newest should show 2 unread immediately");
    assert_eq!(row.last_read.as_deref(), Some(third_newest.as_str()));

    let mark_call = world.until("conversations.mark for mark_unread", || {
        let calls = world.fake.calls(ALPHA, "conversations.mark");
        (calls.len() > marks_before).then(|| calls.last().unwrap().clone())
    });
    assert_eq!(mark_call.params["channel"], "C0GEN");
    assert_eq!(mark_call.params["ts"], third_newest);

    // Thread mark-unread: load thread replies, mark read, then mark unread from the newest reply.
    let thread = alpha(&format!("C0GEN:{ROOT}"));
    let tmsgs = world.until("thread replies loaded", || {
        let m = world.messages(&thread);
        (m.len() >= 2).then_some(m)
    });
    let t_newest = tmsgs.last().unwrap().id.clone();
    let t_prev = tmsgs[tmsgs.len() - 2].id.clone();
    assert_eq!(world.ask(Request::MarkRead { conversation: thread.clone(), message_id: t_newest.clone() }), Response::Ok);
    world.until("thread mark_read sent", || {
        (!world.fake.calls(ALPHA, "subscriptions.thread.mark").is_empty()).then_some(())
    });

    let tmarks_before = world.fake.calls(ALPHA, "subscriptions.thread.mark").len();
    assert_eq!(
        world.ask(Request::MarkUnread {
            conversation: thread.clone(),
            message_id: t_newest,
        }),
        Response::Ok
    );
    assert_eq!(world.conversation(&thread).unwrap().unread, 1);

    let tmark = world.until("subscriptions.thread.mark with read=0", || {
        let calls = world.fake.calls(ALPHA, "subscriptions.thread.mark");
        (calls.len() > tmarks_before).then(|| calls.last().unwrap().clone())
    });
    assert_eq!(tmark.params["channel"], "C0GEN");
    assert_eq!(tmark.params["thread_ts"], ROOT);
    assert_eq!(tmark.params["ts"], t_prev);
    assert_eq!(tmark.params["read"], "0");
}

#[test]
fn slack_pin_unpin_socket_events_and_pinned_files() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    let msgs = world.until("general history loaded", || {
        let m = world.messages(&general);
        (!m.is_empty()).then_some(m)
    });
    let target = msgs[0].id.clone();

    // ConversationInfo includes pinned count from pins.list.
    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: general.clone() }) else {
        panic!("expected ConversationInfo");
    };
    assert_eq!(info.pinned, Some(2), "ConversationAbout.pinned should count pinned messages and files");

    // Pinned list returns both message pins and file pins with created_by and created preserved.
    let Response::Pinned { messages: pins, .. } = world.ask(Request::Pinned { conversation: general.clone() }) else {
        panic!("expected Pinned");
    };
    assert_eq!(pins.len(), 2, "expected message pin + file pin");
    let msg_pin = pins.iter().find(|m| m.text == "Release checklist lives in the wiki").expect("message pin");
    assert!(msg_pin.pinned);
    assert_eq!(msg_pin.pinned_by.as_deref(), Some("Ana"));
    assert_eq!(msg_pin.pinned_at, Some(1760000400));
    let file_pin = pins.iter().find(|m| m.text == "logo.png").expect("file pin");
    assert!(file_pin.pinned);
    assert_eq!(file_pin.pinned_by.as_deref(), Some("Ana"));
    assert_eq!(file_pin.pinned_at, Some(1760000300));

    // Pin and unpin a message via Request::Pin -> pins.add / pins.remove.
    assert_eq!(
        world.ask(Request::Pin {
            conversation: general.clone(),
            message_id: target.clone(),
            pinned: true,
            notify: false,
            me_only: false,
        }),
        Response::Ok
    );
    assert_eq!(world.fake.calls(ALPHA, "pins.add").len(), 1);
    assert!(world.message(&general, &target).unwrap().pinned);

    assert_eq!(
        world.ask(Request::Pin {
            conversation: general.clone(),
            message_id: target.clone(),
            pinned: false,
            notify: false,
            me_only: false,
        }),
        Response::Ok
    );
    assert_eq!(world.fake.calls(ALPHA, "pins.remove").len(), 1);
    assert!(!world.message(&general, &target).unwrap().pinned);

    // Permission refusal translates clearly.
    match world.ask(Request::Pin {
        conversation: general.clone(),
        message_id: "1760000000.999999".into(),
        pinned: true,
        notify: false,
        me_only: false,
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("pin"), "got: {detail}");
        }
        other => panic!("expected Refused for permission_denied, got {other:?}"),
    }

    // Live socket events pin_added and pin_removed flip the stored message's pinned flag without reloading history.
    world.fake.push(ALPHA, json!({
        "type": "pin_added",
        "channel_id": "C0GEN",
        "user": "U0ANA",
        "item": {
            "type": "message",
            "channel": "C0GEN",
            "message": {"ts": target}
        }
    }));
    world.until("pin_added sets pinned=true", || {
        world.message(&general, &target)?.pinned.then_some(())
    });

    world.fake.push(ALPHA, json!({
        "type": "pin_removed",
        "channel_id": "C0GEN",
        "user": "U0ANA",
        "item": {
            "type": "message",
            "channel": "C0GEN",
            "message": {"ts": target}
        }
    }));
    world.until("pin_removed clears pinned=false", || {
        (!world.message(&general, &target)?.pinned).then_some(())
    });
}

#[test]
fn slack_share_forward_with_note_and_distinct_received_share() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let dm = alpha("D0ANA");
    let thread = alpha(&format!("C0GEN:{ROOT}"));

    // Ensure history is loaded so the message exists locally.
    let _ = world.ask(Request::Messages { conversation: general.clone(), page: Page::Newest, limit: 20 });

    // Forward a message from #general to a DM and a thread with a note.
    let reply = world.ask(Request::Forward {
        from: general.clone(),
        message_ids: vec![ROOT.to_owned()],
        to: vec![dm.clone(), thread.clone()],
        note: "Please take a look".into(),
    });
    let Response::Forwarded { results } = reply else {
        panic!("expected Forwarded, got {reply:?}");
    };
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok), "results: {results:?}");

    let posts = world.fake.calls(ALPHA, "chat.postMessage");
    let dm_post = posts.iter().find(|c| c.params.get("channel").map(String::as_str) == Some("D0ANA")).expect("DM post");
    assert!(dm_post.params["text"].starts_with("Please take a look\n"), "got: {}", dm_post.params["text"]);
    assert!(dm_post.params["text"].contains("archives/C0GEN/p1760000200000200"), "got: {}", dm_post.params["text"]);
    assert_eq!(dm_post.params.get("unfurl_links").map(String::as_str), Some("true"));

    let thread_post = posts.iter().find(|c| c.params.get("channel").map(String::as_str) == Some("C0GEN")).expect("thread post");
    assert_eq!(thread_post.params.get("thread_ts").map(String::as_str), Some(ROOT));

    // Received `is_share` with BOTH forwarder note and forwarded text keeps them distinct.
    let ts = "1760000700.000700";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "Check this out",
        "ts": ts,
        "event_ts": ts,
        "attachments": [{
            "is_share": true,
            "channel_name": "buying",
            "author_name": "Nia",
            "text": "The order shipped",
            "fallback": "The order shipped",
            "from_url": "https://alpha.slack.com/archives/C0BUY/p1760000100000100",
        }],
    }));
    let arrived = world.until("shared message with note", || world.message(&general, ts));
    assert_eq!(arrived.text, "Check this out");
    let shared = arrived.forwarded.as_ref().expect("forwarded metadata");
    assert_eq!(shared.from_title, "#buying");
    assert_eq!(shared.author, "Nia");
    assert_eq!(shared.text, "The order shipped");
    assert_eq!(shared.link, "https://alpha.slack.com/archives/C0BUY/p1760000100000100");
}

#[test]
fn slack_save_for_later_and_reminders() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    let msgs = world.until("general history loaded", || {
        let m = world.messages(&general);
        (!m.is_empty()).then_some(m)
    });
    let target = msgs[0].id.clone();

    // Save for later -> stars.add with channel + timestamp, flips message.starred = true.
    assert_eq!(
        world.ask(Request::StarMessage {
            conversation: general.clone(),
            message_id: target.clone(),
            starred: true,
        }),
        Response::Ok
    );
    assert!(
        world.fake.calls(ALPHA, "stars.add").iter().any(|c| {
            c.params.get("channel").map(String::as_str) == Some("C0GEN")
                && c.params.get("timestamp").map(String::as_str) == Some(target.as_str())
        }),
        "expected stars.add with channel and timestamp"
    );
    assert!(world.message(&general, &target).unwrap().starred);

    // Listing Later items fetches stars.list and returns saved messages.
    let Response::Pinned { messages: saved, .. } = world.ask(Request::StarredMessages {
        account: general.account(),
        conversation: None,
        limit: 50,
    }) else {
        panic!("expected Pinned response for StarredMessages");
    };
    assert!(!saved.is_empty(), "expected saved items from stars.list");
    assert!(saved.iter().all(|m| m.starred));

    // Remove from Later -> stars.remove with channel + timestamp, flips message.starred = false.
    assert_eq!(
        world.ask(Request::StarMessage {
            conversation: general.clone(),
            message_id: target.clone(),
            starred: false,
        }),
        Response::Ok
    );
    assert!(
        world.fake.calls(ALPHA, "stars.remove").iter().any(|c| {
            c.params.get("channel").map(String::as_str) == Some("C0GEN")
                && c.params.get("timestamp").map(String::as_str) == Some(target.as_str())
        }),
        "expected stars.remove with channel and timestamp"
    );
    assert!(!world.message(&general, &target).unwrap().starred);

    // Set a reminder on a message -> calls reminders.add and stores reminder.
    let future = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 + 3600;
    assert_eq!(
        world.ask(Request::Remind {
            conversation: general.clone(),
            message_id: target.clone(),
            remind_at: Some(future),
        }),
        Response::Ok
    );
    let added = world.fake.calls(ALPHA, "reminders.add");
    assert_eq!(added.len(), 1, "expected reminders.add call");
    assert_eq!(added[0].params.get("time").map(String::as_str), Some(future.to_string().as_str()));

    // Cancel the reminder -> calls reminders.delete.
    assert_eq!(
        world.ask(Request::Remind {
            conversation: general.clone(),
            message_id: target.clone(),
            remind_at: None,
        }),
        Response::Ok
    );
    assert_eq!(world.fake.calls(ALPHA, "reminders.delete").len(), 1, "expected reminders.delete call");
}

#[test]
fn slack_permalinks_and_in_app_link_resolution() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    let msgs = world.until("general history loaded", || {
        let m = world.messages(&general);
        (!m.is_empty()).then_some(m)
    });
    let root = msgs.iter().find(|m| m.id == ROOT).expect("root message");
    assert!(
        root.permalink.ends_with("/archives/C0GEN/p1760000200000200"),
        "expected HTTPS channel permalink, got: {}",
        root.permalink
    );
    assert!(!root.permalink.starts_with("slack://"));

    // Thread reply carries `?thread_ts=...&cid=...`.
    let thread = alpha(&format!("C0GEN:{ROOT}"));
    let reply_ts = "1760000250.000250";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "Thread reply",
        "ts": reply_ts,
        "thread_ts": ROOT,
        "event_ts": reply_ts,
    }));
    let reply = world.until("thread reply stored", || world.message(&thread, reply_ts));
    assert!(
        reply.permalink.ends_with(&format!("/archives/C0GEN/p1760000250000250?thread_ts={ROOT}&cid=C0GEN")),
        "expected thread permalink, got: {}",
        reply.permalink
    );

    // Resolving an HTTPS Slack archive link for a matching workspace jumps in-app.
    let Response::Resolved { conversation, message_id, .. } = world.ask(Request::Resolve {
        url: "https://alpha.slack.com/archives/C0GEN/p1760000200000200".into(),
    }) else {
        panic!("expected Resolved");
    };
    assert_eq!(conversation, Some(general.clone()));
    assert_eq!(message_id.as_deref(), Some(ROOT));

    // Resolving a thread reply link prefers the thread conversation when cached.
    let Response::Resolved { conversation: t_conv, message_id: t_mid, .. } = world.ask(Request::Resolve {
        url: format!("https://alpha.slack.com/archives/C0GEN/p1760000250000250?thread_ts={ROOT}&cid=C0GEN"),
    }) else {
        panic!("expected Resolved for thread");
    };
    assert_eq!(t_conv, Some(thread.clone()));
    assert_eq!(t_mid.as_deref(), Some(reply_ts));

    // A link for a different workspace does not hijack our workspace's channel.
    let Response::Resolved { conversation: other_conv, .. } = world.ask(Request::Resolve {
        url: "https://unrelated-workspace.slack.com/archives/C0GEN/p1760000200000200".into(),
    }) else {
        panic!("expected Resolved");
    };
    assert_eq!(other_conv, None);
}

#[test]
fn slack_notification_prefs_keywords_and_live_pref_change() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    // Live pref_change for `all_notifications_prefs`: C0GEN -> mentions_dms + suppress_at_channel.
    world.fake.push(ALPHA, json!({
        "type": "pref_change",
        "name": "all_notifications_prefs",
        "value": json!({
            "global": {"desktop": "all"},
            "channels": {
                "C0GEN": {
                    "desktop": "mentions_dms",
                    "muted": false,
                    "suppress_at_channel": true
                }
            }
        }).to_string()
    }));
    // Live pref_change for `highlight_words`.
    world.fake.push(ALPHA, json!({
        "type": "pref_change",
        "name": "highlight_words",
        "value": "deploy, incident"
    }));

    // Details pane surfaces Mentions level for C0GEN.
    world.until("C0GEN notify becomes Mentions", || {
        let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: general.clone() }) else {
            return None;
        };
        (info.notify == protocol::Notify::Mentions).then_some(())
    });

    // @channel broadcast is suppressed because suppress_at_channel is true.
    let ts_bc = "1760000801.000801";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "<!channel> heads up",
        "ts": ts_bc,
        "event_ts": ts_bc,
    }));
    let bc = world.until("broadcast message stored", || world.message(&general, ts_bc));
    assert!(!bc.mentions_me, "suppress_at_channel should suppress @channel mention");

    // Keyword match (case-insensitive word boundary) triggers mentions_me.
    let ts_kw = "1760000802.000802";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "Ready to DEPLOY now!",
        "ts": ts_kw,
        "event_ts": ts_kw,
    }));
    let kw = world.until("keyword message stored", || world.message(&general, ts_kw));
    assert!(kw.mentions_me, "highlight_words match should set mentions_me");

    // Substring match without word boundary does NOT trigger mentions_me.
    let ts_non = "1760000803.000803";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "deployment finished cleanly",
        "ts": ts_non,
        "event_ts": ts_non,
    }));
    let non = world.until("non-boundary message stored", || world.message(&general, ts_non));
    assert!(!non.mentions_me, "substring without word boundary should not set mentions_me");
}

#[test]
fn slack_rich_status_emoji_expiration_and_custom_emoji_read() {
    let world = World::start();
    world.ready();

    let account = alpha("C0GEN").account();
    let expiry = 1_900_000_000_i64;

    // Set status with emoji and clear-after expiration -> sent to users.profile.set.
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Online,
            status: Some("Vacation".into()),
            status_emoji: Some("🌴".into()),
            clear_at: Some(expiry),
        }),
        Response::Ok
    );
    let set_call = world.fake.calls(ALPHA, "users.profile.set").pop().expect("users.profile.set call");
    let sent_profile: Value = serde_json::from_str(&set_call.params["profile"]).unwrap();
    assert_eq!(sent_profile["status_text"], "Vacation");
    assert_eq!(sent_profile["status_emoji"], ":palm_tree:");
    assert_eq!(sent_profile["status_expiration"], expiry);

    // Updating only text (status_emoji: None) preserves existing emoji instead of clearing it.
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Online,
            status: Some("Still on vacation".into()),
            status_emoji: None,
            clear_at: Some(expiry),
        }),
        Response::Ok
    );
    let set_call2 = world.fake.calls(ALPHA, "users.profile.set").pop().expect("second users.profile.set");
    let sent_profile2: Value = serde_json::from_str(&set_call2.params["profile"]).unwrap();
    assert_eq!(sent_profile2["status_text"], "Still on vacation");
    assert_eq!(sent_profile2["status_emoji"], ":palm_tree:");
    assert_eq!(sent_profile2["status_expiration"], expiry);

    // Clearing status clears text, emoji, and expiration.
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Online,
            status: Some(String::new()),
            status_emoji: Some(String::new()),
            clear_at: None,
        }),
        Response::Ok
    );
    let clear_call = world.fake.calls(ALPHA, "users.profile.set").pop().expect("clear users.profile.set");
    let cleared: Value = serde_json::from_str(&clear_call.params["profile"]).unwrap();
    assert_eq!(cleared["status_text"], "");
    assert_eq!(cleared["status_emoji"], "");
    assert_eq!(cleared["status_expiration"], 0);

    // Reading another user's custom emoji status preserves `:custom_name:`.
    world.fake.push(ALPHA, json!({
        "type": "user_status_changed",
        "user": {
            "id": "U0ANA",
            "name": "ana",
            "profile": {
                "display_name": "Ana",
                "status_text": "Deploying",
                "status_emoji": ":partyparrot:",
                "status_expiration": expiry
            }
        }
    }));
    world.until("custom emoji status preserved on U0ANA", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Slack),
            relation: None,
            query: Some("Ana".into()),
            offset: 0,
            limit: 10,
        }) else {
            return None;
        };
        let ana = people.into_iter().find(|p| p.key.id == "U0ANA")?;
        (ana.emoji == ":partyparrot:" && ana.status.contains("Deploying")).then_some(())
    });
}

#[test]
fn slack_canvases_and_lists_render_structured_cards_without_file_chips() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");

    // 1. Canvas file (`filetype: "quip"`) with HTML preview & sections -> Message::card, no file chip, no raw HTML.
    let ts_canvas = "1760000901.000901";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "",
        "ts": ts_canvas,
        "event_ts": ts_canvas,
        "files": [{
            "id": "F0CANVAS1",
            "name": "Q4 Launch Plan",
            "title": "Q4 Launch Plan",
            "filetype": "quip",
            "mimetype": "application/vnd.slack-docs",
            "user": "U0ANA",
            "permalink": "https://acme.slack.com/docs/T01ALPHA/F0CANVAS1",
            "url_private": "https://files.slack.com/files-pri/T01ALPHA-F0CANVAS1/canvas",
            "preview": "<h1>Overview</h1><p>Ship &amp; verify all items.</p><ul><li><input type=\"checkbox\" checked>Build</li><li><input type=\"checkbox\">Deploy</li></ul>",
            "sections": [
                {"heading": "Goals", "content": "<p>Zero downtime</p>"}
            ]
        }]
    }));
    let canvas_msg = world.until("canvas message stored", || world.message(&general, ts_canvas));
    assert!(canvas_msg.attachments.is_empty(), "canvas must not render as a bare file chip");
    assert_eq!(canvas_msg.kind, MessageKind::Normal);
    let card = canvas_msg.card.as_deref().expect("canvas card");
    assert_eq!(card.title, "Q4 Launch Plan");
    assert_eq!(card.title_link, "https://acme.slack.com/docs/T01ALPHA/F0CANVAS1");
    assert_eq!(card.footer, "Open in Slack");
    assert!(card.author.starts_with("Slack Canvas"), "author: {}", card.author);
    assert!(card.text.contains("Overview") && card.text.contains("Ship & verify"), "text: {}", card.text);
    assert!(card.text.contains("☑ Build") && card.text.contains("☐ Deploy"), "checklist text: {}", card.text);
    assert!(!card.text.contains("<h1>") && !card.text.contains("<p>"), "must strip raw HTML: {}", card.text);
    assert_eq!(card.fields, vec![("Goals".to_owned(), "Zero downtime".to_owned())]);

    // 2. Canvas with no preview falls back cleanly (never blank, never file chip).
    let ts_empty_canvas = "1760000902.000902";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "",
        "ts": ts_empty_canvas,
        "event_ts": ts_empty_canvas,
        "files": [{
            "id": "F0CANVAS2",
            "title": "Huddle Notes",
            "filetype": "canvas",
            "permalink": "https://acme.slack.com/docs/T01ALPHA/F0CANVAS2"
        }]
    }));
    let empty_canvas = world.until("minimal canvas stored", || world.message(&general, ts_empty_canvas));
    assert!(empty_canvas.attachments.is_empty());
    let fallback_card = empty_canvas.card.as_deref().expect("fallback canvas card");
    assert_eq!(fallback_card.title, "Huddle Notes");
    assert!(!fallback_card.text.is_empty(), "fallback canvas card must not be blank");
    assert_eq!(fallback_card.footer, "Open in Slack");

    // 3. List file (`filetype: "list"`) with schema and items -> structured Card, no file chip.
    let ts_list = "1760000903.000903";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0BEN",
        "text": "",
        "ts": ts_list,
        "event_ts": ts_list,
        "files": [{
            "id": "F0LIST1",
            "title": "Release Tracker",
            "filetype": "list",
            "mimetype": "application/vnd.slack-list",
            "permalink": "https://acme.slack.com/lists/T01ALPHA/F0LIST1",
            "list_metadata": {
                "description": "<b>Wave 6B</b> tasks",
                "schema": [{"name": "Task"}, {"name": "Status"}, {"name": "Assignee"}],
                "items": [
                    {"title": "Canvas reader", "status": "Done", "assignee": "U0ANA", "due_date": "2026-11-22"},
                    {"title": "GIF picker", "status": "In progress", "assignee": "U0BEN"}
                ]
            }
        }]
    }));
    let list_msg = world.until("list message stored", || world.message(&general, ts_list));
    assert!(list_msg.attachments.is_empty(), "list must not render as a bare file chip");
    let list_card = list_msg.card.as_deref().expect("list card");
    assert_eq!(list_card.title, "Release Tracker");
    assert_eq!(list_card.title_link, "https://acme.slack.com/lists/T01ALPHA/F0LIST1");
    assert!(list_card.author.starts_with("Slack List"), "author: {}", list_card.author);
    assert!(list_card.text.contains("Wave 6B tasks") && !list_card.text.contains("<b>"));
    assert!(list_card.text.contains("Columns: Task, Status, Assignee"));
    assert_eq!(list_card.fields.len(), 2);
    assert_eq!(list_card.fields[0].0, "Canvas reader");
    assert!(list_card.fields[0].1.contains("Done") && list_card.fields[0].1.contains("Due 2026-11-22"));
    assert_eq!(list_card.footer, "Open in Slack");

    // 4. Channel canvas from conversations.info (`properties.canvas`) appears in ConversationInfo::bookmarks.
    let random = alpha("C0RND");
    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: random }) else {
        panic!("expected ConversationInfo for C0RND");
    };
    let canvas_bm = info
        .bookmarks
        .iter()
        .find(|b| b.kind == protocol::BookmarkKind::Canvas && b.id == "F0CHANCANVAS")
        .expect("channel canvas surfaced in bookmarks");
    assert_eq!(canvas_bm.title, "Channel canvas");
    assert!(canvas_bm.link.contains("F0CHANCANVAS"));
}

#[test]
fn slack_create_channel_and_start_group_dm() {
    let world = World::start();
    world.ready();

    // Workspace advertises can_create_channels and groups capability (up to 8).
    let space = Key { network: Network::Slack, account: "T01ALPHA".into(), id: "T01ALPHA".into() };
    let Response::Spaces { spaces } = world.ask(Request::Spaces { network: Some(Network::Slack) }) else {
        panic!("expected Spaces");
    };
    assert!(
        spaces.iter().any(|s| s.key == space && s.can_create_channels),
        "Slack workspace should advertise can_create_channels"
    );
    assert_eq!(world.account("T01ALPHA").can.groups, 8);

    // 1. Create a public channel.
    let Response::Conversation { conversation: pub_chan } = world.ask(Request::CreateChannel {
        space: space.clone(),
        name: "launch-ops".into(),
        kind: ConversationKind::Channel,
        private: false,
        group: None,
    }) else {
        panic!("expected Conversation for public CreateChannel");
    };
    assert_eq!(pub_chan.key.id, "C0PUBNEW");
    assert_eq!(pub_chan.title, "#launch-ops");
    assert!(!pub_chan.private);
    assert!(pub_chan.joined);

    // 2. Create a private channel.
    let Response::Conversation { conversation: priv_chan } = world.ask(Request::CreateChannel {
        space: space.clone(),
        name: "secret-plan".into(),
        kind: ConversationKind::Channel,
        private: true,
        group: None,
    }) else {
        panic!("expected Conversation for private CreateChannel");
    };
    assert_eq!(priv_chan.key.id, "C0PRIVNEW");
    assert_eq!(priv_chan.title, "#secret-plan");
    assert!(priv_chan.private);
    let create_calls = world.fake.calls(ALPHA, "conversations.create");
    assert_eq!(create_calls.len(), 2);
    assert_eq!(create_calls[0].params.get("is_private").map(String::as_str), Some("false"));
    assert_eq!(create_calls[1].params.get("is_private").map(String::as_str), Some("true"));

    // 3. Invalid channel name surfaces clear Slack naming rules in words.
    match world.ask(Request::CreateChannel {
        space: space.clone(),
        name: "Bad Name".into(),
        kind: ConversationKind::Channel,
        private: false,
        group: None,
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("lowercase"), "got: {detail}");
        }
        other => panic!("expected Refused for invalid channel name, got {other:?}"),
    }

    // 4. Start a multi-person group DM with two other people (3-person DM including me).
    let Response::Conversation { conversation: mpim } = world.ask(Request::StartGroup {
        account: space.account(),
        people: vec![alpha("U0ANA"), alpha("U0BEN")],
        title: None,
    }) else {
        panic!("expected Conversation for StartGroup");
    };
    assert_eq!(mpim.key.id, "G0MPIMNEW");
    assert_eq!(mpim.kind, ConversationKind::Group);
    assert!(mpim.private);
    assert!(mpim.title.contains("ana") && mpim.title.contains("ben"), "mpim title: {}", mpim.title);
    let open_calls = world.fake.calls(ALPHA, "conversations.open");
    assert!(
        open_calls.iter().any(|c| c.params.get("users").map(String::as_str) == Some("U0ANA,U0BEN")),
        "expected comma-separated users in conversations.open: {open_calls:?}"
    );

    // 5. When workspace restricts channel creation (`restricted_action`), Space hides can_create_channels.
    world.fake.refusing.lock().unwrap().insert("conversations.create".into(), "restricted_action".into());
    match world.ask(Request::CreateChannel {
        space: space.clone(),
        name: "blocked-chan".into(),
        kind: ConversationKind::Channel,
        private: false,
        group: None,
    }) {
        Response::Error { code: ErrorCode::Refused, .. } => {}
        other => panic!("expected Refused for restricted_action, got {other:?}"),
    }
    let Response::Spaces { spaces: updated_spaces } = world.ask(Request::Spaces { network: Some(Network::Slack) }) else {
        panic!("expected Spaces");
    };
    assert!(
        !updated_spaces.iter().find(|s| s.key == space).unwrap().can_create_channels,
        "restricted_action should clear can_create_channels on the Space"
    );
}

#[test]
fn slack_snippets_workspace_files_delete_and_external_files() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let account = general.account();
    assert!(world.account("T01ALPHA").can.manage_files);

    // 1. Incoming Slack snippet renders inline as a `Style::CodeBlock { language: "rust" }` plus hosted file attachment.
    let ts_snip = "1760000951.000951";
    world.fake.push(
        ALPHA,
        json!({
            "type": "message",
            "channel": "C0GEN",
            "user": "U0ANA",
            "text": "",
            "ts": ts_snip,
            "event_ts": ts_snip,
            "files": [{
                "id": "F0SNIPIN",
                "name": "main.rs",
                "title": "Startup snippet",
                "mimetype": "text/plain",
                "filetype": "rust",
                "mode": "snippet",
                "size": 240,
                "lines": 15,
                "lines_more": 12,
                "preview": "fn main() {\n    println!(\"hello\");\n}",
                "url_private": format!("{}/files-pri/T01ALPHA-F0SNIPIN/main.rs", world.fake.base),
                "permalink": "https://acme.slack.com/files/U0ANA/F0SNIPIN/main.rs"
            }]
        }),
    );
    let snip_msg = world.until("snippet message stored", || world.message(&general, ts_snip));
    assert!(snip_msg.text.contains("Startup snippet"));
    assert!(snip_msg.text.contains("fn main()"));
    assert!(snip_msg.text.contains("12 more lines"));
    assert!(
        snip_msg.spans.iter().any(|s| matches!(&s.style, Style::CodeBlock { language } if language == "rust")),
        "snippet should carry Style::CodeBlock with language=rust: {:?}",
        snip_msg.spans
    );
    assert_eq!(snip_msg.attachments.len(), 1, "full snippet file stays downloadable");

    // 2. Externally hosted file (`mode == "external"`, Google Drive) renders a structured Card with open link and NO blob fetch.
    let ts_ext = "1760000952.000952";
    world.fake.push(
        ALPHA,
        json!({
            "type": "message",
            "channel": "C0GEN",
            "user": "U0BEN",
            "text": "",
            "ts": ts_ext,
            "event_ts": ts_ext,
            "files": [{
                "id": "F0DRIVE1",
                "name": "Architecture Doc",
                "title": "Architecture Doc",
                "filetype": "gdoc",
                "pretty_type": "GSuite Document",
                "mode": "external",
                "is_external": true,
                "external_type": "gdrive",
                "url_private": "https://docs.google.com/document/d/xyz/edit",
                "permalink": "https://acme.slack.com/files/U0BEN/F0DRIVE1/arch"
            }]
        }),
    );
    let ext_msg = world.until("external file message stored", || world.message(&general, ts_ext));
    assert!(ext_msg.attachments.is_empty(), "external file must not register a Slack blob attachment");
    let ext_card = ext_msg.card.as_deref().expect("external file card");
    assert_eq!(ext_card.author, "Google Drive");
    assert_eq!(ext_card.title, "Architecture Doc");
    assert_eq!(ext_card.title_link, "https://docs.google.com/document/d/xyz/edit");
    assert_eq!(ext_card.footer, "Open in Google Drive");
    assert!(ext_card.text.contains("GSuite Document"));

    // 3. Creating a snippet passes `snippet_type` to `files.getUploadURLExternal`.
    let files_dir = world.dir.with_extension("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    let tmp_snippet = files_dir.join("hello.py");
    std::fs::write(&tmp_snippet, b"print('hi')\n").unwrap();
    let with = protocol::Outgoing {
        files: vec![protocol::OutgoingFile {
            path: tmp_snippet.to_string_lossy().into_owned(),
            name: "hello.py".into(),
            ..Default::default()
        }],
        snippet_type: Some("python".into()),
        ..Default::default()
    };
    assert!(matches!(
        world.ask(Request::Send {
            conversation: general.clone(),
            client_id: "5671".into(),
            text: "quick script".into(),
            reply_to: None,
            with,
        }),
        Response::Sent { .. }
    ));
    let up_call = world.until("snippet upload requested", || {
        world.fake.calls(ALPHA, "files.getUploadURLExternal").into_iter().last()
    });
    assert_eq!(up_call.params.get("snippet_type").map(String::as_str), Some("python"));

    // 4. Paged workspace-wide `AccountFiles` lists hosted + external files without walking all pages at once.
    let Response::AccountFiles { items: page1, cursor: next1, more: more1 } = world.ask(Request::AccountFiles {
        account: account.clone(),
        query: String::new(),
        kind: None,
        cursor: None,
        limit: 25,
    }) else {
        panic!("expected AccountFiles page 1");
    };
    assert!(more1);
    assert_eq!(next1.as_deref(), Some("2"));
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].message_id, "F0SNIP1");
    assert!(page1[0].attachment.is_some());
    assert_eq!(page1[1].message_id, "F0EXT1");
    assert!(page1[1].attachment.is_none());
    assert_eq!(page1[1].url, "https://docs.google.com/document/d/abc123/edit");

    let Response::AccountFiles { items: page2, cursor: next2, more: more2 } = world.ask(Request::AccountFiles {
        account: account.clone(),
        query: String::new(),
        kind: None,
        cursor: next1,
        limit: 25,
    }) else {
        panic!("expected AccountFiles page 2");
    };
    assert!(!more2 && next2.is_none());
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].message_id, "F0PAGE2");

    // 5. `DeleteFile` deletes own file via `files.delete` and refuses cleanly on `cant_delete_file`.
    assert_eq!(
        world.ask(Request::DeleteFile { account: account.clone(), file_id: "F0SNIP1".into() }),
        Response::Ok
    );
    assert_eq!(
        world.fake.calls(ALPHA, "files.delete").last().and_then(|c| c.params.get("file")).map(String::as_str),
        Some("F0SNIP1")
    );
    match world.ask(Request::DeleteFile { account, file_id: "F0OTHER".into() }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("delete this file"), "unexpected refusal: {detail}");
        }
        other => panic!("expected Refused for cant_delete_file, got {other:?}"),
    }
}

#[test]
fn slack_gif_picker_catalog_and_send() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    let account = general.account();
    world.general();

    let can = world.account("T01ALPHA").can;
    assert!(can.gifs && !can.stickers);

    // 1. Trending GIFs catalog (`query: ""`).
    let Response::Catalog { items: trending, cursor } = world.ask(Request::Catalog {
        account: account.clone(),
        kind: protocol::CatalogKind::Gifs,
        space: None,
        query: String::new(),
        cursor: None,
    }) else {
        panic!("expected trending GIF catalog");
    };
    assert_eq!(trending.len(), 2);
    assert_eq!(trending[0].id, "https://media.giphy.com/media/partyparrot/giphy.gif");
    assert_eq!(trending[0].pack, "Trending");
    assert!(trending[0].animated);
    assert_eq!(trending[0].file.as_ref().map(|f| (f.kind, f.width, f.height)), Some((MediaKind::Image, 320, 240)));
    assert_eq!(cursor.as_deref(), Some("gif-page-2"));

    // 2. Search GIFs (`query: "celebrate"`).
    let Response::Catalog { items: hits, cursor: next } = world.ask(Request::Catalog {
        account: account.clone(),
        kind: protocol::CatalogKind::Gifs,
        space: None,
        query: "celebrate".into(),
        cursor: None,
    }) else {
        panic!("expected search GIF catalog");
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "https://media.giphy.com/media/celebrate/giphy.gif");
    assert_eq!(next, None);
    let search_call = world.fake.calls(ALPHA, "gifs.search").pop().expect("gifs.search called");
    assert_eq!(search_call.params.get("q").map(String::as_str), Some("celebrate"));

    // 3. Sending a picked GIF posts its URL with unfurling left enabled (even if suppress_embeds was set).
    let gif_url = hits[0].id.clone();
    let with = protocol::Outgoing {
        pick: Some(protocol::Pick {
            kind: protocol::CatalogKind::Gifs,
            id: gif_url.clone(),
        }),
        suppress_embeds: true,
        ..Default::default()
    };
    assert!(matches!(
        world.ask(Request::Send {
            conversation: general.clone(),
            client_id: "5691".into(),
            text: "woohoo".into(),
            reply_to: None,
            with,
        }),
        Response::Sent { .. }
    ));
    let post = world.until("GIF post sent to Slack", || {
        world
            .fake
            .calls(ALPHA, "chat.postMessage")
            .into_iter()
            .find(|c| c.params.get("text").is_some_and(|t| t.contains(&gif_url)))
    });
    assert_eq!(post.params.get("text").map(String::as_str), Some(format!("woohoo\n{gif_url}").as_str()));
    assert_eq!(post.params.get("unfurl_links"), None, "picked GIFs must not suppress unfurling");

    // 4. Stickers remain cleanly refused.
    let sticker = protocol::Outgoing {
        pick: Some(protocol::Pick {
            kind: protocol::CatalogKind::Stickers,
            id: "sticker-1".into(),
        }),
        ..Default::default()
    };
    assert!(matches!(
        world.ask(Request::Send {
            conversation: general.clone(),
            client_id: "5692".into(),
            text: String::new(),
            reply_to: None,
            with: sticker,
        }),
        Response::Sent { .. }
    ));
    let failed = world.until("sticker send failed", || {
        world.messages(&general).into_iter().find(|m| {
            m.client_id.as_deref() == Some("5692")
                && matches!(&m.state, SendState::Failed { reason } if reason.contains("no stickers"))
        })
    });
    assert!(matches!(failed.state, SendState::Failed { .. }));
}

#[test]
fn slack_block_kit_interactive_controls_and_actions() {
    let world = World::start();
    world.ready();
    let general = alpha("C0GEN");
    world.general();

    let ts = "1760000980.000980";
    world.fake.push(
        ALPHA,
        json!({
            "type": "message",
            "subtype": "bot_message",
            "bot_id": "B0DEP",
            "app_id": "A0DEPLOY",
            "username": "Deploy Bot",
            "channel": "C0GEN",
            "text": "Release v2.1 ready",
            "ts": ts,
            "event_ts": ts,
            "blocks": [
                {
                    "type": "section",
                    "block_id": "sec1",
                    "text": {"type": "mrkdwn", "text": "*Release v2.1* ready for approval"},
                    "accessory": {
                        "type": "overflow",
                        "action_id": "more_opts",
                        "options": [
                            {"text": {"type": "plain_text", "text": "View logs"}, "value": "logs"},
                            {"text": {"type": "plain_text", "text": "Rollback"}, "value": "rollback"}
                        ]
                    }
                },
                {
                    "type": "actions",
                    "block_id": "act1",
                    "elements": [
                        {
                            "type": "button",
                            "action_id": "approve_btn",
                            "text": {"type": "plain_text", "text": "Approve"},
                            "style": "primary",
                            "value": "rel-2.1",
                            "confirm": {
                                "title": {"type": "plain_text", "text": "Ship release?"},
                                "text": {"type": "mrkdwn", "text": "Deploy v2.1 to production."},
                                "confirm": {"type": "plain_text", "text": "Ship it"},
                                "deny": {"type": "plain_text", "text": "Cancel"}
                            }
                        },
                        {
                            "type": "button",
                            "action_id": "open_pr",
                            "text": {"type": "plain_text", "text": "Open PR"},
                            "url": "https://example.com/pr/42"
                        },
                        {
                            "type": "static_select",
                            "action_id": "env_pick",
                            "placeholder": {"type": "plain_text", "text": "Environment"},
                            "initial_option": {"text": {"type": "plain_text", "text": "Staging"}, "value": "staging"},
                            "options": [
                                {"text": {"type": "plain_text", "text": "Staging"}, "value": "staging"},
                                {"text": {"type": "plain_text", "text": "Production"}, "value": "prod"}
                            ]
                        },
                        {
                            "type": "datepicker",
                            "action_id": "ship_date",
                            "initial_date": "2026-12-01",
                            "placeholder": {"type": "plain_text", "text": "Ship date"}
                        },
                        {
                            "type": "timepicker",
                            "action_id": "ship_time",
                            "initial_time": "14:30"
                        },
                        {
                            "type": "checkboxes",
                            "action_id": "checks",
                            "initial_options": [{"text": {"type": "plain_text", "text": "Notify team"}, "value": "notify"}],
                            "options": [
                                {"text": {"type": "plain_text", "text": "Notify team"}, "value": "notify"},
                                {"text": {"type": "plain_text", "text": "Tag release"}, "value": "tag"}
                            ]
                        },
                        {
                            "type": "radio_buttons",
                            "action_id": "strategy",
                            "initial_option": {"text": {"type": "plain_text", "text": "Canary"}, "value": "canary"},
                            "options": [
                                {"text": {"type": "plain_text", "text": "Canary"}, "value": "canary"},
                                {"text": {"type": "plain_text", "text": "Full"}, "value": "full"}
                            ]
                        }
                    ]
                }
            ]
        }),
    );

    let msg = world.until("interactive Block Kit message stored", || world.message(&general, ts));
    assert_eq!(msg.kind, MessageKind::Normal);
    assert!(!msg.text.contains("Open in Slack"), "parsed controls should not append fallback Open in Slack link");
    assert_eq!(msg.controls.len(), 8);
    assert_eq!(msg.controls[0].element_type, "overflow");
    assert_eq!(msg.controls[1].action_id, "approve_btn");
    assert_eq!(msg.controls[1].confirm.as_ref().map(|c| c.title.as_str()), Some("Ship release?"));
    assert_eq!(
        msg.controls[2].kind,
        protocol::ControlKind::Button { url: "https://example.com/pr/42".into(), style: protocol::ControlStyle::Default }
    );
    assert!(matches!(msg.controls[3].kind, protocol::ControlKind::Select { ref selected, .. } if selected.as_deref() == Some("staging")));
    assert!(matches!(msg.controls[4].kind, protocol::ControlKind::DatePicker { ref initial, .. } if initial.as_deref() == Some("2026-12-01")));
    assert!(matches!(msg.controls[5].kind, protocol::ControlKind::TimePicker { ref initial, .. } if initial.as_deref() == Some("14:30")));
    assert!(matches!(msg.controls[6].kind, protocol::ControlKind::Checkboxes { ref selected, .. } if selected == &["notify"]));
    assert!(matches!(msg.controls[7].kind, protocol::ControlKind::RadioButtons { ref selected, .. } if selected.as_deref() == Some("canary")));

    // 1. Button action dispatches `blocks.actions` with container, action payload, and remembered service_id.
    assert_eq!(
        world.ask(Request::MessageAction {
            conversation: general.clone(),
            message_id: ts.into(),
            block_id: "act1".into(),
            action_id: "approve_btn".into(),
            element_type: "button".into(),
            value: "rel-2.1".into(),
            values: Vec::new(),
        }),
        Response::Ok
    );
    let call = world.fake.calls(ALPHA, "blocks.actions").pop().expect("blocks.actions called");
    assert_eq!(call.params.get("channel_id").map(String::as_str), Some("C0GEN"));
    assert_eq!(call.params.get("service_id").map(String::as_str), Some("A0DEPLOY"));
    let actions: Value = serde_json::from_str(&call.params["actions"]).unwrap();
    assert_eq!(actions[0]["action_id"], "approve_btn");
    assert_eq!(actions[0]["value"], "rel-2.1");
    let container: Value = serde_json::from_str(&call.params["container"]).unwrap();
    assert_eq!(container["message_ts"], ts);

    // 2. Select menu & checkboxes format `selected_option` / `selected_options` properly.
    assert_eq!(
        world.ask(Request::MessageAction {
            conversation: general.clone(),
            message_id: ts.into(),
            block_id: "act1".into(),
            action_id: "env_pick".into(),
            element_type: "static_select".into(),
            value: "prod".into(),
            values: Vec::new(),
        }),
        Response::Ok
    );
    let sel_call = world.fake.calls(ALPHA, "blocks.actions").pop().unwrap();
    let sel_actions: Value = serde_json::from_str(&sel_call.params["actions"]).unwrap();
    assert_eq!(sel_actions[0]["selected_option"]["value"], "prod");

    assert_eq!(
        world.ask(Request::MessageAction {
            conversation: general.clone(),
            message_id: ts.into(),
            block_id: "act1".into(),
            action_id: "checks".into(),
            element_type: "checkboxes".into(),
            value: String::new(),
            values: vec!["notify".into(), "tag".into()],
        }),
        Response::Ok
    );
    let chk_call = world.fake.calls(ALPHA, "blocks.actions").pop().unwrap();
    let chk_actions: Value = serde_json::from_str(&chk_call.params["actions"]).unwrap();
    assert_eq!(chk_actions[0]["selected_options"].as_array().map(Vec::len), Some(2));

    // 3. Socket `view_opened` does not disrupt the socket, and `message_changed` updates controls in place.
    world.fake.push(ALPHA, json!({"type": "view_opened", "view": {"id": "V0MODAL1", "type": "modal"}}));
    world.fake.push(
        ALPHA,
        json!({
            "type": "message",
            "subtype": "message_changed",
            "hidden": true,
            "channel": "C0GEN",
            "ts": "1760000981.000000",
            "event_ts": "1760000981.000000",
            "message": {
                "type": "message",
                "subtype": "bot_message",
                "bot_id": "B0DEP",
                "username": "Deploy Bot",
                "text": "Release v2.1 approved!",
                "ts": ts,
                "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": "✅ Release v2.1 approved!"}}]
            }
        }),
    );
    let updated = world.until("message updated after action", || {
        world.message(&general, ts).filter(|m| m.controls.is_empty() && m.text.contains("approved"))
    });
    assert!(updated.controls.is_empty());

    // 4. Failed action returns a human-readable refusal.
    match world.ask(Request::MessageAction {
        conversation: general,
        message_id: ts.into(),
        block_id: "act1".into(),
        action_id: "fail_action".into(),
        element_type: "button".into(),
        value: String::new(),
        values: Vec::new(),
    }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("no longer available"), "unexpected refusal: {detail}");
        }
        other => panic!("expected Refused for action_not_found, got {other:?}"),
    }
}

#[test]
fn slack_drafts_sync_create_update_delete_and_live_events() {
    let world = World::start_with(|fake| {
        *fake.drafts_list.lock().unwrap() = Some(json!({
            "ok": true,
            "drafts": [{
                "id": "Dr01REMOTE",
                "client_last_updated_ts": "1760000800.000000",
                "destinations": [{"channel_id": "C0DEP"}],
                "blocks": [{
                    "type": "rich_text",
                    "elements": [{
                        "type": "rich_text_section",
                        "elements": [{"type": "text", "text": "Hydrated from Slack"}]
                    }]
                }]
            }]
        }));
    });
    world.ready();

    let dep = alpha("C0DEP");
    let general = alpha("C0GEN");

    // 1. drafts.list on connect hydrates C0DEP's draft.
    world.until("C0DEP draft hydrated from drafts.list", || {
        let row = world.conversation(&dep)?;
        (row.draft == "Hydrated from Slack").then_some(())
    });

    // 2. Writing a draft on C0GEN calls drafts.create, updating calls drafts.update, clearing calls drafts.delete.
    assert_eq!(world.ask(Request::SetDraft { conversation: general.clone(), text: "First draft".into() }), Response::Ok);
    world.until("drafts.create called", || (!world.fake.calls(ALPHA, "drafts.create").is_empty()).then_some(()));
    let created = world.fake.calls(ALPHA, "drafts.create").pop().unwrap();
    assert!(created.params["destinations"].contains("C0GEN"));
    assert!(created.params["blocks"].contains("First draft"));

    assert_eq!(world.ask(Request::SetDraft { conversation: general.clone(), text: "Edited draft".into() }), Response::Ok);
    world.until("drafts.update called", || (!world.fake.calls(ALPHA, "drafts.update").is_empty()).then_some(()));
    let updated = world.fake.calls(ALPHA, "drafts.update").pop().unwrap();
    assert_eq!(updated.params["draft_id"], "Dr01LOCAL");
    assert!(updated.params["blocks"].contains("Edited draft"));

    assert_eq!(world.ask(Request::SetDraft { conversation: general.clone(), text: String::new() }), Response::Ok);
    world.until("drafts.delete called", || (!world.fake.calls(ALPHA, "drafts.delete").is_empty()).then_some(()));
    let deleted = world.fake.calls(ALPHA, "drafts.delete").pop().unwrap();
    assert_eq!(deleted.params["draft_id"], "Dr01LOCAL");

    // 3. Live draft_saved and draft_deleted socket events sync into the store.
    world.fake.push(ALPHA, json!({
        "type": "draft_saved",
        "draft": {
            "id": "Dr02LIVE",
            "client_last_updated_ts": "1999999999.000000",
            "destinations": [{"channel_id": "C0GEN"}],
            "blocks": [{
                "type": "rich_text",
                "elements": [{
                    "type": "rich_text_section",
                    "elements": [{"type": "text", "text": "Typed on phone"}]
                }]
            }]
        }
    }));
    world.until("live draft_saved synced", || {
        let row = world.conversation(&general)?;
        (row.draft == "Typed on phone").then_some(())
    });

    world.fake.push(ALPHA, json!({
        "type": "draft_deleted",
        "draft_id": "Dr02LIVE"
    }));
    world.until("live draft_deleted cleared", || {
        let row = world.conversation(&general)?;
        row.draft.is_empty().then_some(())
    });
}

#[test]
fn slack_dnd_snooze_resume_and_live_dnd_updated() {
    let world = World::start();
    world.ready();

    let account = alpha("C0GEN").account();
    assert!(world.account("T01ALPHA").can.dnd_snooze, "Slack should advertise dnd_snooze capability");

    // 1. Pausing notifications with a clear_at calls dnd.setSnooze and switches presence to Dnd.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Dnd,
            status: None,
            status_emoji: None,
            clear_at: Some(now + 1800),
        }),
        Response::Ok
    );
    let snooze = world.fake.calls(ALPHA, "dnd.setSnooze").pop().expect("dnd.setSnooze called");
    let mins: i64 = snooze.params["num_minutes"].parse().unwrap();
    assert!((29..=31).contains(&mins), "expected ~30 minutes, got {mins}");
    world.until("account presence becomes Dnd", || {
        (world.account("T01ALPHA").presence == Presence::Dnd).then_some(())
    });

    // 2. Resuming notifications (setting Online) calls dnd.endSnooze and clears Dnd.
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Online,
            status: None,
            status_emoji: None,
            clear_at: None,
        }),
        Response::Ok
    );
    assert_eq!(world.fake.calls(ALPHA, "dnd.endSnooze").len(), 1);
    world.until("account presence returns to Online", || {
        (world.account("T01ALPHA").presence == Presence::Online).then_some(())
    });

    // 3. Live dnd_updated socket event sets and clears Dnd.
    world.fake.push(ALPHA, json!({
        "type": "dnd_updated",
        "user": "U0ME",
        "dnd_status": {
            "dnd_enabled": false,
            "next_dnd_start_ts": 0,
            "next_dnd_end_ts": 0,
            "snooze_enabled": true,
            "snooze_endtime": now + 3600,
        }
    }));
    world.until("live dnd_updated sets Dnd", || {
        (world.account("T01ALPHA").presence == Presence::Dnd).then_some(())
    });

    world.fake.push(ALPHA, json!({
        "type": "dnd_updated",
        "user": "U0ME",
        "dnd_status": {
            "dnd_enabled": false,
            "next_dnd_start_ts": 0,
            "next_dnd_end_ts": 0,
            "snooze_enabled": false,
            "snooze_endtime": 0,
        }
    }));
    world.until("live dnd_updated clears Dnd", || {
        (world.account("T01ALPHA").presence == Presence::Online).then_some(())
    });
}

#[test]
fn slack_scheduled_messages_list_cancel_and_reschedule() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    assert!(world.account("T01ALPHA").can.schedule, "Slack should advertise schedule capability");

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let first_at = now + 3600;
    let later_at = now + 7200;

    // 1. Schedule a message.
    let mut with = protocol::Outgoing::default();
    with.schedule = Some(first_at);
    let sent = world.ask(Request::Send {
        conversation: general.clone(),
        client_id: "sched1".into(),
        text: "Standup notes".into(),
        reply_to: None,
        with,
    });
    assert!(matches!(sent, Response::Sent { .. }), "{sent:?}");
    world.until("chat.scheduleMessage called", || {
        (!world.fake.calls(ALPHA, "chat.scheduleMessage").is_empty()).then_some(())
    });

    // 2. List scheduled messages via chat.scheduledMessages.list.
    let Response::ScheduledMessages { messages } = world.ask(Request::ScheduledMessages {
        conversation: general.clone(),
    }) else {
        panic!("expected Response::ScheduledMessages");
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, "Q0SCHED");
    assert_eq!(messages[0].text, "Standup notes");
    assert_eq!(messages[0].post_at, first_at);

    // 3. Edit & reschedule via delete + chat.scheduleMessage.
    let Response::ScheduledMessage { message: updated } = world.ask(Request::RescheduleMessage {
        conversation: general.clone(),
        scheduled_id: "Q0SCHED".into(),
        text: Some("Updated standup notes".into()),
        post_at: later_at,
    }) else {
        panic!("expected Response::ScheduledMessage");
    };
    assert_eq!(updated.text, "Updated standup notes");
    assert_eq!(updated.post_at, later_at);
    assert_eq!(world.fake.calls(ALPHA, "chat.deleteScheduledMessage").len(), 1);
    assert_eq!(world.fake.calls(ALPHA, "chat.scheduleMessage").len(), 2);

    // 4. Cancel the rescheduled message via chat.deleteScheduledMessage.
    assert_eq!(
        world.ask(Request::CancelScheduled {
            conversation: general.clone(),
            scheduled_id: updated.id.clone(),
        }),
        Response::Ok
    );
    assert_eq!(world.fake.calls(ALPHA, "chat.deleteScheduledMessage").len(), 2);

    // 5. List is now empty.
    let Response::ScheduledMessages { messages } = world.ask(Request::ScheduledMessages {
        conversation: general.clone(),
    }) else {
        panic!("expected Response::ScheduledMessages");
    };
    assert!(messages.is_empty());
}

#[test]
fn slack_search_filters_and_no_duplicate_in_modifier() {
    let world = World::start();
    world.ready();

    let general = alpha("C0GEN");
    let filters = protocol::SearchFilters {
        from: Some(alpha("U0ANA")),
        has: Some(protocol::SearchHas::Links),
        before: Some(1_767_225_600),
        after: Some(1_735_689_600),
        mentions_me: false,
        ..Default::default()
    };
    let Response::MessageHits { .. } = world.ask(Request::SearchNetwork {
        conversation: Some(general.clone()),
        space: None,
        account: None,
        network: None,
        query: "deploy".into(),
        filters: filters.clone(),
        cursor: None,
    }) else {
        panic!("expected Response::MessageHits");
    };
    let call = world.fake.calls(ALPHA, "search.messages").pop().expect("search.messages call");
    let sent = &call.params["query"];
    assert!(sent.contains("deploy"), "{sent}");
    assert!(sent.contains("in:#general"), "{sent}");
    assert!(sent.contains("from:<@U0ANA>"), "{sent}");
    assert!(sent.contains("has:link"), "{sent}");
    assert!(sent.contains("before:2026-01-01"), "{sent}");
    assert!(sent.contains("after:2025-01-01"), "{sent}");

    // When user already typed `in:#deployments`, do not append a duplicate `in:#general`.
    let Response::MessageHits { .. } = world.ask(Request::SearchNetwork {
        conversation: Some(general.clone()),
        space: None,
        account: None,
        network: None,
        query: "in:#deployments during:2026-11 release".into(),
        filters: protocol::SearchFilters::default(),
        cursor: None,
    }) else {
        panic!("expected Response::MessageHits");
    };
    let call2 = world.fake.calls(ALPHA, "search.messages").pop().expect("second search.messages call");
    let sent2 = &call2.params["query"];
    assert!(sent2.contains("in:#deployments"), "{sent2}");
    assert!(!sent2.contains("in:#general"), "must not duplicate in: modifier: {sent2}");
    assert!(sent2.contains("during:2026-11"), "{sent2}");
}

#[test]
fn slack_follow_unfollow_thread_and_unread_reply_counting() {
    let world = World::start();
    world.ready();

    assert!(world.account("T01ALPHA").can.follow_threads, "Slack should advertise follow_threads");
    let thread = alpha(&format!("C0GEN:{ROOT}"));

    // 1. Follow the thread -> calls subscriptions.thread.add and marks followed=true.
    assert_eq!(
        world.ask(Request::FollowThread { conversation: thread.clone(), on: true }),
        Response::Ok
    );
    let add = world.fake.calls(ALPHA, "subscriptions.thread.add").pop().expect("subscriptions.thread.add called");
    assert_eq!(add.params["channel"], "C0GEN");
    assert_eq!(add.params["thread_ts"], ROOT);
    world.until("thread marked followed", || {
        world.conversation(&thread).filter(|c| c.followed).map(|_| ())
    });

    // 2. A live socket reply from Ana increments unread reply count to 1 without opening the thread.
    let reply1 = "1760000901.000901";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "Following up on this thread",
        "ts": reply1,
        "thread_ts": ROOT,
        "event_ts": reply1,
    }));
    world.until("followed thread unread becomes 1", || {
        world.conversation(&thread).filter(|c| c.unread == 1).map(|_| ())
    });

    // 3. thread_marked clears unread back to 0.
    world.fake.push(ALPHA, json!({
        "type": "thread_marked",
        "subscription": {
            "channel": "C0GEN",
            "thread_ts": ROOT,
            "unread_count": 0,
            "last_read": reply1,
        }
    }));
    world.until("thread_marked clears unread", || {
        world.conversation(&thread).filter(|c| c.unread == 0).map(|_| ())
    });

    // 4. Unfollow the thread -> calls subscriptions.thread.remove and marks followed=false.
    assert_eq!(
        world.ask(Request::FollowThread { conversation: thread.clone(), on: false }),
        Response::Ok
    );
    let rem = world.fake.calls(ALPHA, "subscriptions.thread.remove").pop().expect("subscriptions.thread.remove called");
    assert_eq!(rem.params["channel"], "C0GEN");
    assert_eq!(rem.params["thread_ts"], ROOT);
    world.until("thread marked unfollowed", || {
        world.conversation(&thread).filter(|c| !c.followed).map(|_| ())
    });

    // 5. A reply in an unfollowed thread does NOT increment unread.
    let reply2 = "1760000902.000902";
    world.fake.push(ALPHA, json!({
        "type": "message",
        "channel": "C0GEN",
        "user": "U0ANA",
        "text": "Noise after unfollow",
        "ts": reply2,
        "thread_ts": ROOT,
        "event_ts": reply2,
    }));
    world.until("reply2 stored in thread", || world.message(&thread, reply2));
    assert_eq!(world.conversation(&thread).unwrap().unread, 0);

    // 6. Live thread_subscribed and thread_unsubscribed socket events flip followed state.
    world.fake.push(ALPHA, json!({
        "type": "thread_subscribed",
        "subscription": {
            "channel": "C0GEN",
            "thread_ts": ROOT,
        }
    }));
    world.until("live thread_subscribed sets followed=true", || {
        world.conversation(&thread).filter(|c| c.followed).map(|_| ())
    });

    world.fake.push(ALPHA, json!({
        "type": "thread_unsubscribed",
        "subscription": {
            "channel": "C0GEN",
            "thread_ts": ROOT,
        }
    }));
    world.until("live thread_unsubscribed sets followed=false", || {
        world.conversation(&thread).filter(|c| !c.followed).map(|_| ())
    });
}
