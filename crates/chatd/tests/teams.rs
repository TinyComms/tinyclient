//! Runs the real chatd against a local fake of Teams and asks it, over its own socket, what the
//! shell would ask.
//!
//! The fake is one HTTP server on localhost that is also the push socket. It stands in for
//! Microsoft's sign-in service, Teams' front door, the chat service, the middle tier, presence,
//! the media store, trouter and the registrar, answers from the hand-written payloads in
//! `fixtures/teams/`, records what it was sent, and pushes events when a test tells it to.
//! Nothing here reaches Microsoft, the keychain or the real support directory.
//!
//! What this proves is that the adapter does what its notes say against payloads shaped the way
//! working clients document them. It proves nothing about Teams itself: see `teams.rs`.

use base64::Engine;
use protocol::{
    read_frame, write_frame, Badge, CallSupport, Change, Conversation, ConversationKind, Group,
    Key, Link, MediaKind, MentionKind, Message, MessageKind, Network, Page, Presence, Relation,
    Request, Response, SendState, Style, Watching,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use protocol::ChatStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tungstenite::handshake::server::{Request as Upgrade, Response as Upgraded};

fn agent() -> &'static str {
    chatd::adapters::net::browser()
}
const HOME: &str = "11111111-1111-4111-8111-111111111111";
const GUEST: &str = "22222222-2222-4222-8222-222222222222";
const ME: &str = "8:orgid:00000000-0000-4000-8000-0000000000aa";
const ANA: &str = "8:orgid:00000000-0000-4000-8000-0000000000a1";
const BEN: &str = "8:orgid:00000000-0000-4000-8000-0000000000b2";
const DEE: &str = "8:orgid:00000000-0000-4000-8000-0000000000d4";
/// The Platform team, whose General channel has the team's own id, as on Teams.
const GENERAL: &str = "19:platform0general@thread.tacv2";
const DEPLOYS: &str = "19:platform0deploys@thread.tacv2";
const CHAT_ANA: &str = "19:00000000-0000-4000-8000-0000000000a1_00000000-0000-4000-8000-0000000000aa@unq.gbl.spaces";
const CHAT_DEE: &str = "19:00000000-0000-4000-8000-0000000000aa_00000000-0000-4000-8000-0000000000d4@unq.gbl.spaces";
/// The post in General the fixtures hang a reply chain off.
const ROOT: &str = "1760000200200";
/// A chain of 450 replies, paged 200 at a time, and a second chain opened at an old reply.
const LONG: &str = "1760000400400";
const JUMP: &str = "1760000410410";
const LONG_REPLIES: u64 = 450;

/// Reply `n` in the long-chain fake. 1 is the oldest reply and 450 the newest.
fn reply_at(n: u64) -> u64 {
    1_760_001_000_000 + n * 1000
}
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really";
/// What the fake returns for a shared spreadsheet, so a fetch can be told from a sign-in page.
const SHEET: &[u8] = b"PK\x03\x04shared-file";

/// One request the fake received.
#[derive(Clone, Debug)]
struct Call {
    method: String,
    /// The path as sent, and with its escapes read.
    raw: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: String,
}

impl Call {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    kind: &'static str,
    body: Vec<u8>,
}

fn ok(body: Value) -> Reply {
    answer("200 OK", body)
}

fn answer(status: &'static str, body: Value) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        kind: "application/json",
        body: body.to_string().into_bytes(),
    }
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

fn form(body: &str) -> HashMap<String, String> {
    let pairs = body.split('&').filter_map(|pair| pair.split_once('='));
    pairs.map(|(name, value)| (unescape(name), unescape(value))).collect()
}

fn iso(ms: u64) -> String {
    // Only the id is read for the time; this is here because Teams always sends one.
    format!("2026-10-09T09:00:{:02}.{:03}0000Z", (ms / 1000) % 60, ms % 1000)
}

/// One of the chat service's message objects.
fn message(conversation: &str, root: Option<&str>, id: u64, from: &str, name: &str, content: &str) -> Value {
    let chain = root.map_or_else(String::new, |root| format!(";messageid={root}"));
    json!({
        "id": id.to_string(), "sequenceId": id % 1000, "clientmessageid": (id * 7919).to_string(), "version": id.to_string(),
        "conversationid": conversation,
        "conversationLink": format!("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/{conversation}{chain}"),
        "type": "Message", "messagetype": "RichText/Html", "contenttype": "text", "content": content, "amsreferences": [],
        "from": format!("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/{from}"), "imdisplayname": name,
        "composetime": iso(id), "originalarrivaltime": iso(id), "properties": {},
    })
}

/// One page of a 450-reply chain, newest first. The first page's cursor is `chaincursor1`,
/// the second's is `chaincursor2`, and the page that reaches reply 1 has no cursor.
fn long_chain(root: &str, query: &str) -> Value {
    let end = query.split('&').find_map(|pair| pair.strip_prefix("endTime=")).and_then(|text| text.parse::<u64>().ok());
    let cursor = query.split('&').find_map(|pair| pair.strip_prefix("syncState="));
    let mut hi = match cursor {
        Some("chaincursor1") => 250,
        Some("chaincursor2") => 50,
        Some(_) => 0,
        None => LONG_REPLIES,
    };
    if let Some(end) = end {
        while hi > 0 && reply_at(hi) >= end {
            hi -= 1;
        }
    }
    if hi == 0 {
        return json!({"messages": [], "_metadata": {}});
    }
    let lo = hi.saturating_sub(199).max(1);
    let mut messages = Vec::new();
    for n in (lo..=hi).rev() {
        messages.push(message(GENERAL, Some(root), reply_at(n), BEN, "Ben Okafor", &format!("<p>reply {n}</p>")));
    }
    if lo == 1 {
        let post = root.parse().unwrap_or(0);
        messages.push(message(GENERAL, Some(root), post, BEN, "Ben Okafor", "<p>long thread</p>"));
    }
    let mut body = json!({"messages": messages, "_metadata": {}});
    if lo > 1 {
        let next = if lo > 51 { "chaincursor1" } else { "chaincursor2" };
        body["_metadata"]["backwardLink"] = json!(format!("https://example.invalid/messages?syncState={next}"));
    }
    body
}

struct Fake {
    /// `http://127.0.0.1:port`, standing in for every Microsoft host.
    base: String,
    calls: Mutex<Vec<Call>>,
    minted: AtomicU32,
    /// Every token minted: tenant, audience, the refresh token presented, the one issued.
    tokens: Mutex<Vec<(String, String, String, String)>>,
    /// Paths answered 401 once, as a service does to a token it no longer takes.
    stale: Mutex<Vec<&'static str>>,
    socket: Mutex<Option<Sender<String>>>,
    /// The push socket's handshakes, and the frames chatd sent on it.
    shakes: Mutex<Vec<(String, HashMap<String, String>)>>,
    heard: Mutex<Vec<String>>,
    pushed: AtomicU32,
    posted: AtomicU32,
    mailbox_ooo: Mutex<Value>,
    /// Optional replacement payload for `/api/csa/api/v1/teams/users/me`.
    teams_override: Mutex<Option<Value>>,
}

impl Fake {
    fn start() -> Arc<Fake> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fake = Arc::new(Fake {
            base: format!("http://{}", listener.local_addr().unwrap()),
            calls: Mutex::default(),
            minted: AtomicU32::new(0),
            tokens: Mutex::default(),
            stale: Mutex::new(vec!["/api/csa/api/v1/teams/users/me"]),
            socket: Mutex::default(),
            shakes: Mutex::default(),
            heard: Mutex::default(),
            pushed: AtomicU32::new(0),
            posted: AtomicU32::new(0),
            mailbox_ooo: Mutex::new(json!({
                "status": "disabled",
                "externalAudience": "none",
                "internalReplyMessage": "",
                "externalReplyMessage": "",
            })),
            teams_override: Mutex::default(),
        });
        let serving = fake.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let fake = serving.clone();
                std::thread::spawn(move || {
                    // The push socket arrives on the same port: told apart by its first line.
                    let mut first = [0u8; 64];
                    let seen = stream.peek(&mut first).unwrap_or(0);
                    if first[..seen].starts_with(b"GET /socket.io/1/websocket/") {
                        fake.socket(stream)
                    } else {
                        fake.http(stream)
                    }
                });
            }
        });
        fake
    }

    /// A payload from `fixtures/teams/`, with Microsoft's hosts replaced by the fake's.
    fn fixture(&self, name: &str) -> Option<Value> {
        let path = format!("{}/tests/fixtures/teams/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let mut text = std::fs::read_to_string(path).ok()?;
        for (host, at) in [
            ("https://emea.ng.msg.teams.microsoft.com", "/chat"),
            ("https://teams.microsoft.com/api/mt/emea", "/api/mt/emea"),
            ("https://presence.teams.microsoft.com", "/presence"),
            ("https://eu-api.asm.skype.com", "/ams"),
        ] {
            text = text.replace(host, &format!("{}{at}", self.base));
        }
        Some(serde_json::from_str(&text).unwrap())
    }

    /// What authz hands out: shaped like a token, with our own id as its `skypeid`.
    fn skype_token() -> String {
        let claims = json!({"skypeid": ME.trim_start_matches("8:"), "exp": 1_900_000_000});
        format!("e30.{}.c2ln", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string()))
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
            let mut parts = line.split_whitespace();
            let (method, target) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("/").to_owned());
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
            let (raw, query) = target.split_once('?').map_or((target.as_str(), ""), |(raw, query)| (raw, query));
            let call = Call {
                method,
                raw: raw.to_owned(),
                path: unescape(raw),
                query: query.to_owned(),
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            };
            self.calls.lock().unwrap().push(call.clone());
            let reply = self.answer(&call);
            let mut head = format!("HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n", reply.status, reply.kind, reply.body.len());
            for (name, value) in &reply.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            if out.write_all(head.as_bytes()).and_then(|()| out.write_all(&reply.body)).is_err() {
                return;
            }
        }
    }

    fn answer(&self, call: &Call) -> Reply {
        let header = |name: &str| call.headers.get(name).map_or("", String::as_str);
        let path = call.path.as_str();
        let unauthorized = || answer("401 Unauthorized", json!({"message": "Authentication failed."}));
        {
            let mut stale = self.stale.lock().unwrap();
            if let Some(at) = stale.iter().position(|known| *known == path) {
                stale.remove(at);
                return unauthorized();
            }
        }
        let skype = Fake::skype_token();
        let bearer = |audience: &str| header("authorization").starts_with(&format!("Bearer at.{audience}."));
        if let Some(tenant) = path.strip_suffix("/oauth2/v2.0/token") {
            return self.token(tenant.trim_matches('/'), &form(&call.body));
        }
        match path {
            "/api/authsvc/v1.0/authz" if bearer("spaces") => {
                let mut authz = self.fixture("authz").unwrap();
                authz["tokens"]["skypeToken"] = skype.into();
                ok(authz)
            }
            "/api/csa/api/v1/teams/users/me" if bearer("csa") && header("x-skypetoken") == skype => {
                let body = self
                    .teams_override
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| self.fixture("teams.users.me").unwrap());
                ok(body)
            }
            "/v4/a" if header("x-skypetoken") == skype => ok(json!({
                "socketio": format!("{}/", self.base), "surl": format!("{}/v4/f/abc123/", self.base),
                "url": format!("{}/v4/f/abc123/", self.base), "ttl": 587_000, "ccid": "cc-1",
                "connectparams": {"sr": "abc123", "issuer": "prod-2", "sp": "connect", "se": "1760600000000", "st": "1760000000000", "sig": "c2VjcmV0K3NpZw=="},
            })),
            "/socket.io/1/" if header("x-skypetoken") == skype => Reply {
                status: "200 OK",
                headers: Vec::new(),
                kind: "text/plain",
                body: b"sid-42:180:180:websocket,xhr-polling".to_vec(),
            },
            "/registrar/prod/V2/registrations" if bearer("spaces") && header("x-skypetoken") == skype => answer("202 Accepted", json!({})),
            _ if call.method == "DELETE"
                && path.starts_with("/registrar/prod/V2/registrations/")
                && bearer("spaces")
                && header("x-skypetoken") == skype =>
            {
                answer("202 Accepted", json!({}))
            }
            _ if path.starts_with("/chat/") => {
                if header("authentication") != format!("skypetoken={skype}") {
                    return unauthorized();
                }
                self.chat(call, &path["/chat".len()..])
            }
            _ if path.starts_with("/api/mt/emea/") && bearer("spaces") && header("x-skypetoken") == skype => self.middle(call),
            // Someone whose presence Teams will not give out just now: a 429, a second after.
            "/presence/v1/presence/getpresence/" if bearer("presence") && call.body.contains("limit-me") => {
                let mut limited = answer("429 Too Many Requests", json!({"errorCode": 429, "message": "API calls quota exceeded!"}));
                limited.headers.push(("Retry-After", "1".to_owned()));
                limited
            }
            "/presence/v1/presence/getpresence/" if bearer("presence") => ok(self.fixture("presence").unwrap()),
            "/presence/v1/me/forceavailability/" if bearer("presence") => ok(json!({})),
            "/presence/v1/me/publishnote" if bearer("presence") => ok(json!({})),
            // A shared file's bytes. The cache asks Graph's shares route; this stands in for it.
            _ if call.method == "GET" && path.contains("/shares/u!") && path.ends_with("/driveItem/content") => Reply {
                status: "200 OK",
                headers: Vec::new(),
                kind: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                body: SHEET.to_vec(),
            },
            _ if path.starts_with("/graph/") && bearer("graph") => self.graph(call, &path["/graph".len()..]),
            _ if path.starts_with("/upload/") => self.upload(call),
            _ if path.starts_with("/ams/v1/objects") && (call.method == "POST" || call.method == "PUT") => {
                if header("authorization") != format!("skype_token {skype}") {
                    return unauthorized();
                }
                if call.method == "POST" && path == "/ams/v1/objects" {
                    let n = self.posted.fetch_add(1, Ordering::Relaxed) + 1;
                    return answer("201 Created", json!({"id": format!("0-neu-d1-img{n}")}));
                }
                if call.method == "PUT" && path.ends_with("/content/imgpsh") {
                    return answer("201 Created", json!({}));
                }
                answer("404 Not Found", json!({"errorCode": 404, "message": "not here"}))
            }
            _ if path.starts_with("/ams/") || path.contains("/profilepicturev2") => Reply {
                status: "200 OK",
                headers: Vec::new(),
                kind: "image/png",
                body: PNG.to_vec(),
            },
            "/api/authsvc/v1.0/authz" | "/api/csa/api/v1/teams/users/me" | "/v4/a" | "/socket.io/1/" | "/registrar/prod/V2/registrations" => unauthorized(),
            _ => answer("404 Not Found", json!({"errorCode": 404, "message": "not here"})),
        }
    }

    /// Microsoft's token service: an access token for the audience asked for and the next
    /// refresh token, or the refusal a tenant gives a grant it has ended.
    fn token(&self, tenant: &str, asked: &HashMap<String, String>) -> Reply {
        let presented = asked.get("refresh_token").cloned().unwrap_or_default();
        let scope = asked.get("scope").cloned().unwrap_or_default();
        let audience = match scope.as_str() {
            scope if scope.starts_with("https://api.spaces.skype.com/.default ") => "spaces",
            scope if scope.starts_with("https://chatsvcagg.teams.microsoft.com/.default ") => "csa",
            scope if scope.starts_with("https://presence.teams.microsoft.com/.default ") => "presence",
            scope if scope.starts_with("https://graph.microsoft.com/.default ") => "graph",
            _ => return answer("400 Bad Request", json!({"error": "invalid_scope", "error_description": "AADSTS70011: The provided value for the input parameter 'scope' is not valid."})),
        };
        let shaped = asked.get("grant_type").map(String::as_str) == Some("refresh_token")
            && asked.get("client_id").map(String::as_str) == Some("1fec8e78-bce4-4aaf-ab1b-5451cc387264")
            && scope.ends_with(" openid profile offline_access");
        if tenant == GUEST || !shaped || !presented.starts_with("rt-") {
            return answer("400 Bad Request", json!({
                "error": "invalid_grant",
                "error_description": "AADSTS700082: The refresh token has expired due to inactivity. Trace ID: 0a1b Correlation ID: 2c3d",
                "error_codes": [700082],
            }));
        }
        let number = self.minted.fetch_add(1, Ordering::Relaxed) + 1;
        let issued = format!("rt-{number}");
        self.tokens.lock().unwrap().push((tenant.to_owned(), audience.to_owned(), presented, issued.clone()));
        ok(json!({
            "token_type": "Bearer", "scope": scope, "expires_in": 3599, "ext_expires_in": 3599,
            "access_token": format!("at.{audience}.{number}"), "refresh_token": issued, "id_token": "e30.e30.c2ln",
        }))
    }

    /// The chat service. `path` is what follows its address.
    fn chat(&self, call: &Call, path: &str) -> Reply {
        if path == "/v1/threads" && call.method == "POST" {
            let body = call.json();
            let target = if body["properties"]["topic"].as_str().is_some_and(|t| !t.is_empty())
                || body["members"].as_array().is_some_and(|m| m.len() > 2)
            {
                "19:tagchat@thread.v2"
            } else {
                CHAT_DEE
            };
            let mut created = answer("201 Created", json!({}));
            created.headers.push(("Location", format!("{}/chat/v1/threads/{target}", self.base)));
            return created;
        }
        if let Some(thread) = path.strip_prefix("/v1/threads/").and_then(|rest| rest.strip_suffix("/consumptionhorizons")) {
            // Everyone else in the launch chat has read past my last message there.
            let horizons = match thread {
                "19:grp2@thread.v2" => json!([
                    {"id": ME, "consumptionhorizon": "1759000000000;1759000001000;1"},
                    {"id": BEN, "consumptionhorizon": "1759000000000;1759000002000;1"},
                    {"id": "8:orgid:00000000-0000-4000-8000-0000000000c3", "consumptionhorizon": "1759000000500;1759000003000;1"},
                ]),
                _ => json!([]),
            };
            return ok(json!({"id": thread, "consumptionhorizons": horizons}));
        }
        if let Some(thread) = path.strip_prefix("/v1/threads/") {
            if call.method == "DELETE" && thread.contains("/members/") {
                return ok(json!({}));
            }
            let described = match thread {
                CHAT_ANA => Some(json!({
                    "id": thread,
                    "properties": {"threadType": "chat"},
                    "members": [{"id": ME, "role": "Admin"}, {"id": ANA, "role": "Admin"}]
                })),
                CHAT_DEE => self.fixture("thread.dee"),
                GENERAL => self.fixture("thread.general"),
                "19:tagchat@thread.v2" => Some(json!({
                    "id": thread,
                    "properties": {"threadType": "chat", "topic": "On-Call"},
                    "members": [{"id": ME, "role": "Admin"}, {"id": ANA, "role": "Admin"}, {"id": BEN, "role": "Admin"}]
                })),
                "19:grp1@thread.v2" => Some(json!({"id": thread, "properties": {"threadType": "chat"}, "members": [{}, {}, {}]})),
                _ => None,
            };
            return described.map_or_else(|| answer("404 Not Found", json!({"errorCode": 404, "message": "no such thread"})), ok);
        }
        let Some(rest) = path.strip_prefix("/v1/users/ME/conversations/") else {
            return answer("404 Not Found", json!({"errorCode": 404, "message": "not here"}));
        };
        let (conversation, tail) = rest.split_once('/').unwrap_or((rest, ""));
        match (call.method.as_str(), tail) {
            ("GET", "messages") => {
                if let Some(root) = [LONG, JUMP].into_iter().find(|root| conversation == format!("{GENERAL};messageid={root}")) {
                    return ok(long_chain(root, &call.query));
                }
                let older = call.query.contains("syncState=3e30cursor1") || call.query.contains("endTime=");
                let name = match conversation {
                    GENERAL if older => "messages.general.older",
                    GENERAL => "messages.general",
                    DEPLOYS => "messages.deploys",
                    CHAT_ANA => "messages.ana",
                    "19:grp1@thread.v2" => {
                        let msgs: Vec<Value> = (1..=8u64)
                            .rev()
                            .map(|i| message("19:grp1@thread.v2", None, 1_759_500_000_000 + i * 100, BEN, "Ben Okafor", &format!("<p>update {i}</p>")))
                            .collect();
                        return ok(json!({"messages": msgs, "_metadata": {}}));
                    }
                    chain if chain == format!("{GENERAL};messageid={ROOT}") => "messages.chain",
                    "48:notifications" => "messages.activity",
                    _ => return ok(json!({"messages": [], "_metadata": {}})),
                };
                ok(self.fixture(name).unwrap())
            }
            ("POST", "messages") => self.post(call, conversation),
            ("POST", _) if tail.starts_with("messages/") && tail.ends_with("/invoke") => {
                ok(json!({"statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": "OK"}))
            }
            ("PUT", "properties") => ok(json!({})),
            ("DELETE", "") => ok(json!({})),
            ("PUT" | "DELETE", _) if tail.starts_with("messages/") => ok(json!({})),
            _ => answer("404 Not Found", json!({"errorCode": 404, "message": "not here"})),
        }
    }

    /// A message posted: accepted, named by the time it arrived, and delivered back on push
    /// the way Teams tells every one of the user's clients. One text asks for a rate limit.
    fn post(&self, call: &Call, conversation: &str) -> Reply {
        let sent = call.json();
        if sent["messagetype"] == "Control/Typing" {
            return answer("201 Created", json!({}));
        }
        if sent["content"].as_str().unwrap_or("").contains("limit me") {
            let mut limited = answer("429 Too Many Requests", json!({"errorCode": 429, "message": "API calls quota exceeded!"}));
            limited.headers.push(("Retry-After", "7".to_owned()));
            return limited;
        }
        if sent["content"].as_str().unwrap_or("").contains("moderated post") {
            return answer("403 Forbidden", json!({"errorCode": 403, "message": "User is not allowed to post in this channel."}));
        }
        let arrived = 1_760_001_000_000 + u64::from(self.posted.fetch_add(1, Ordering::Relaxed)) * 1000;
        let (thread, root) = conversation.split_once(";messageid=").map_or((conversation, None), |(thread, root)| (thread, Some(root.to_owned())));
        let root = root.or_else(|| thread.contains("@thread.tacv2").then(|| arrived.to_string()));
        let mut echo = message(thread, root.as_deref(), arrived, ME, sent["imdisplayname"].as_str().unwrap_or(""), sent["content"].as_str().unwrap_or(""));
        echo["clientmessageid"] = sent["clientmessageid"].clone();
        if sent["properties"].is_object() {
            echo["properties"] = sent["properties"].clone();
        }
        let socket = self.socket.lock().unwrap().clone();
        let frame = self.frame(&json!({"time": iso(arrived), "type": "EventMessage", "resourceType": "NewMessage", "resource": echo}), false);
        std::thread::spawn(move || {
            sleep(Duration::from_millis(100));
            if let Some(socket) = socket {
                let _ = socket.send(frame);
            }
        });
        answer("201 Created", json!({"OriginalArrivalTime": arrived}))
    }

    /// Microsoft Graph: OneDrive chat uploads + per-member `/invite`, channel SharePoint `filesFolder`, mailbox OOO, and user org/profile.
    fn graph(&self, call: &Call, path: &str) -> Reply {
        if call.method == "GET" && path == "/v1.0/users/00000000-0000-4000-8000-0000000000a1" {
            return ok(json!({
                "id": "00000000-0000-4000-8000-0000000000a1",
                "displayName": "Ana Ruiz",
                "mail": "ana@contoso.com",
                "userPrincipalName": "ana@contoso.com",
                "jobTitle": "Staff Engineer",
                "department": "Platform",
                "officeLocation": "Lisbon — Floor 4",
                "pronouns": "she/her",
                "businessPhones": ["+351 21 555 0100"],
                "mobilePhone": "+351 91 555 0199",
                "mailboxSettings": {"timeZone": "GMT Standard Time"},
                "userType": "Member",
            }));
        }
        if call.method == "GET" && path == "/v1.0/users/00000000-0000-4000-8000-0000000000a1/manager" {
            return ok(json!({
                "id": "00000000-0000-4000-8000-0000000000b2",
                "displayName": "Ben Okafor",
                "jobTitle": "SRE",
                "mail": "ben@contoso.com",
                "department": "Platform",
            }));
        }
        if call.method == "GET" && path == "/v1.0/users/00000000-0000-4000-8000-0000000000a1/directReports" {
            return ok(json!({
                "value": [{
                    "id": "00000000-0000-4000-8000-0000000000c3",
                    "displayName": "Cy Tran",
                    "jobTitle": "Designer",
                    "mail": "cy@contoso.com",
                    "department": "Design",
                }]
            }));
        }
        if call.method == "GET" && path.starts_with("/v1.0/users/00000000-0000-4000-8000-0000000000d4") {
            return answer("403 Forbidden", json!({"error": {"code": "Authorization_RequestDenied"}}));
        }
        if call.method == "GET" && path == "/v1.0/me/mailboxSettings/automaticRepliesSetting" {
            return ok(self.mailbox_ooo.lock().unwrap().clone());
        }
        if call.method == "PATCH" && path == "/v1.0/me/mailboxSettings" {
            let body = call.json();
            if body["automaticRepliesSetting"].is_object() {
                *self.mailbox_ooo.lock().unwrap() = body["automaticRepliesSetting"].clone();
            }
            return ok(json!({"automaticRepliesSetting": self.mailbox_ooo.lock().unwrap().clone()}));
        }
        if call.method == "POST" && path.starts_with("/v1.0/me/drive/root:/Microsoft Teams Chat Files/") && path.ends_with(":/createUploadSession") {
            return ok(json!({"uploadUrl": format!("{}/upload/session-1?sig=x", self.base), "expirationDateTime": "2026-10-15T00:00:00Z"}));
        }
        if call.method == "POST" && path.starts_with("/v1.0/me/drive/items/") && path.ends_with("/invite") {
            let body = call.json();
            let has_guest = body["recipients"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["email"].as_str() == Some("dee@contoso.com"));
            if has_guest {
                return answer("403 Forbidden", json!({"error": {"code": "accessDenied", "message": "External sharing is disabled by tenant policy."}}));
            }
            return ok(json!({"value": [{"id": "perm-1", "roles": ["read"]}]}));
        }
        if call.method == "GET" && path.starts_with("/v1.0/teams/") && path.ends_with("/filesFolder") {
            return ok(json!({
                "id": "FOLDER_CH1",
                "name": "General",
                "parentReference": {"driveId": "DRIVE_TEAM1"},
                "webUrl": "https://contoso.sharepoint.com/sites/Platform/Shared Documents/General",
            }));
        }
        if call.method == "POST" && path.starts_with("/v1.0/drives/DRIVE_TEAM1/items/FOLDER_CH1:/") && path.ends_with(":/createUploadSession") {
            return ok(json!({"uploadUrl": format!("{}/upload/channel-session-1?sig=x", self.base), "expirationDateTime": "2026-10-15T00:00:00Z"}));
        }
        if call.method == "POST" && path.starts_with("/v1.0/me/drive/items/") && path.ends_with("/createLink") {
            return ok(json!({"link": {"type": "view", "scope": "organization", "webUrl": "https://contoso-my.sharepoint.com/:t:/g/personal/me/share-1"}}));
        }
        answer("404 Not Found", json!({"error": {"code": "itemNotFound"}}))
    }

    /// An upload session: each range is taken, and the last answers with the item made.
    fn upload(&self, call: &Call) -> Reply {
        let range = call.headers.get("content-range").cloned().unwrap_or_default();
        let (span, total) = range.trim_start_matches("bytes ").split_once('/').unwrap_or(("", "0"));
        let end: u64 = span.split_once('-').and_then(|(_, end)| end.parse().ok()).unwrap_or(0);
        if end + 1 < total.parse::<u64>().unwrap_or(0) {
            return answer("202 Accepted", json!({"nextExpectedRanges": [format!("{}-", end + 1)]}));
        }
        if call.path.contains("channel-session") {
            return answer("201 Created", json!({
                "id": "01CHANITEM", "name": "report.txt", "size": total.parse::<u64>().unwrap_or(0),
                "webUrl": "https://contoso.sharepoint.com/sites/Platform/Shared Documents/General/report.txt",
            }));
        }
        answer("201 Created", json!({
            "id": "01ITEM", "name": "notes.txt", "size": total.parse::<u64>().unwrap_or(0),
            "webUrl": "https://contoso-my.sharepoint.com/personal/me_contoso_com/Documents/Microsoft Teams Chat Files/notes.txt",
        }))
    }

    /// The middle tier: people by id, people by name, and their pictures.
    fn middle(&self, call: &Call) -> Reply {
        let users = self.fixture("users").unwrap();
        let users = users.as_array().unwrap();
        if call.path.ends_with("/beta/users/fetchShortProfile") {
            let wanted = call.json();
            let wanted: Vec<&str> = wanted.as_array().unwrap().iter().filter_map(Value::as_str).collect();
            let found: Vec<&Value> = users.iter().filter(|user| wanted.contains(&user["mri"].as_str().unwrap())).collect();
            return ok(json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": found}));
        }
        if call.path.ends_with("/beta/users/searchV2") {
            let query = call.body.to_lowercase();
            let found: Vec<&Value> = users.iter().filter(|user| user["displayName"].as_str().unwrap().to_lowercase().contains(&query)).collect();
            return ok(json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": found}));
        }
        if call.path.ends_with("/tags") && call.path.contains("/beta/teams/") {
            return ok(json!({
                "tags": [
                    {
                        "id": "tag-oncall",
                        "displayName": "On-Call",
                        "description": "Incident responders",
                        "memberCount": 2,
                        "members": [
                            {"mri": ME, "displayName": "Me Myself"},
                            {"mri": ANA, "displayName": "Ana Ruiz"}
                        ]
                    },
                    {
                        "id": "tag-design",
                        "displayName": "Design",
                        "description": "Product design",
                        "memberCount": 1,
                        "members": [
                            {"mri": "8:orgid:00000000-0000-4000-8000-0000000000c3", "displayName": "Cy Tran"}
                        ]
                    }
                ]
            }));
        }
        if call.path.ends_with("/v1/urlpreview") {
            let asked = call.json();
            let url = asked["url"].as_str().unwrap_or("");
            if url == "https://example.com/release" {
                return ok(json!({
                    "url": url,
                    "title": "TinyClient Release",
                    "description": "Wave 4D Teams improvements",
                    "siteName": "Example",
                    "previewImage": format!("{}/ams/v1/objects/preview-1/views/imgo", self.base),
                }));
            }
        }
        answer("404 Not Found", json!({"errorCode": 404, "message": "not here"}))
    }

    // Push.

    // The handshake callback's error type is tungstenite's to choose.
    #[allow(clippy::result_large_err)]
    fn socket(self: Arc<Fake>, stream: TcpStream) {
        let mut shake = None;
        let heard = |request: &Upgrade, response: Upgraded| {
            let headers = request.headers().iter().map(|(name, value)| (name.as_str().to_owned(), value.to_str().unwrap_or("").to_owned())).collect();
            shake = Some((request.uri().to_string(), headers));
            Ok::<_, tungstenite::handshake::server::ErrorResponse>(response)
        };
        let Ok(mut socket) = tungstenite::accept_hdr(stream, heard) else {
            return;
        };
        self.shakes.lock().unwrap().push(shake.unwrap());
        socket.get_ref().set_read_timeout(Some(Duration::from_millis(25))).unwrap();
        let (frames, queued) = channel();
        *self.socket.lock().unwrap() = Some(frames);
        let _ = socket.send(tungstenite::Message::text("1::"));
        loop {
            for frame in queued.try_iter() {
                if socket.send(tungstenite::Message::text(frame)).is_err() {
                    return;
                }
            }
            match socket.read() {
                Ok(tungstenite::Message::Text(text)) => self.heard.lock().unwrap().push(text.as_str().to_owned()),
                Ok(tungstenite::Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
    }

    /// An event as trouter delivers it: a request to the path the registration named, its body
    /// JSON held as text, or gzipped and base64ed with a header that says so.
    fn frame(&self, event: &Value, packed: bool) -> String {
        let id = self.pushed.fetch_add(1, Ordering::Relaxed) + 1;
        let mut headers = json!({"MS-CV": "x1y2z3.1.0", "trouter-request": "{\"id\":\"r1\"}", "Content-Type": "application/json"});
        let mut body = event.to_string();
        if packed {
            let mut member = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
            member.extend(miniz_oxide::deflate::compress_to_vec(body.as_bytes(), 6));
            member.extend([0u8; 8]);
            body = base64::engine::general_purpose::STANDARD.encode(member);
            headers["X-Microsoft-Skype-Content-Encoding"] = "gzip".into();
        }
        let request = json!({"id": id, "method": "POST", "url": "/v4/f/abc123/messaging", "headers": headers, "body": body});
        format!("3:::{request}")
    }

    fn push(&self, kind: &str, resource: Value, packed: bool) {
        let event = json!({
            "time": iso(1_760_000_900_000), "type": "EventMessage", "resourceType": kind, "resource": resource,
            "resourceLink": "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/x/messages/1",
        });
        let frame = self.frame(&event, packed);
        let socket = self.socket.lock().unwrap();
        socket.as_ref().expect("chatd has the push socket open").send(frame).unwrap();
    }

    fn calls(&self, method: &str, ending: &str) -> Vec<Call> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|call| call.method == method && call.path.ends_with(ending)).cloned().collect()
    }
}

/// A fake Teams and a chatd signed in to two of its tenants, in a scratch directory.
struct World {
    fake: Arc<Fake>,
    child: Child,
    dir: PathBuf,
}

fn key(id: &str) -> Key {
    Key {
        network: Network::Teams,
        account: HOME.to_owned(),
        id: id.to_owned(),
    }
}

fn chain() -> Key {
    key(&format!("{GENERAL};messageid={ROOT}"))
}

impl World {
    fn start() -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let fake = Fake::start();
        // Short path: a unix socket address holds about 100 bytes. On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!("/tmp/tinyclient-teams-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // What the sign-in helper prints for someone who is also a guest in a second tenant.
        let session = json!({
            "client_id": "1fec8e78-bce4-4aaf-ab1b-5451cc387264", "user_agent": agent(),
            "oid": "00000000-0000-4000-8000-0000000000aa", "name": "Me Myself", "upn": "me@contoso.com",
            "tenants": [
                {"id": HOME, "name": "Contoso", "refresh_token": "rt-0"},
                {"id": GUEST, "name": "Fabrikam", "refresh_token": "rt-guest"},
            ],
        });
        let log = if std::env::var_os("TINYCLIENT_TEST_LOG").is_some() { Stdio::inherit() } else { Stdio::null() };
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_SESSION_TEAMS", session.to_string())
            .env("TINYCLIENT_TEAMS_LOGIN", &fake.base)
            .env("TINYCLIENT_TEAMS_API", &fake.base)
            .env("TINYCLIENT_TEAMS_TROUTER", &fake.base)
            .env("TINYCLIENT_TEAMS_GRAPH", format!("{}/graph", fake.base))
            .env("TINYCLIENT_MEDIA_HOSTS", "127.0.0.1")
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env_remove("TINYCLIENT_SESSION_TELEGRAM")
            .stderr(log)
            .spawn()
            .unwrap();
        let world = World { fake, child, dir };
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&world.dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        world
    }

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    /// Asks again until `look` finds what it is waiting for.
    fn until<T>(&self, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
        let limit = Duration::from_secs(20);
        let deadline = Instant::now() + limit;
        loop {
            if let Some(found) = look() {
                return found;
            }
            if Instant::now() > deadline {
                let calls = self.fake.calls.lock().unwrap();
                let asked: Vec<String> = calls.iter().map(|call| format!("{} {}", call.method, call.path)).collect();
                panic!("waited {limit:?} for {what}; Teams was asked: {asked:#?}");
            }
            sleep(Duration::from_millis(40));
        }
    }

    fn account(&self, id: &str) -> protocol::AccountStatus {
        match self.ask(Request::Status) {
            Response::Status { accounts, .. } => accounts.into_iter().find(|account| account.key.account == id).expect("the account is listed"),
            other => panic!("{other:?}"),
        }
    }

    fn conversation(&self, conversation: &Key) -> Option<Conversation> {
        match self.ask(Request::Conversation { conversation: conversation.clone() }) {
            Response::Conversation { conversation } => Some(conversation),
            _ => None,
        }
    }

    fn tree(&self, space: &str) -> Option<(Vec<Group>, Vec<Conversation>)> {
        match self.ask(Request::Tree { space: key(space), offset: 0, limit: 500, peek: false }) {
            Response::Tree { groups, conversations, .. } => Some((groups, conversations)),
            _ => None,
        }
    }

    /// The newest page of a conversation; empty until chatd knows the conversation.
    fn messages(&self, conversation: &Key) -> Vec<Message> {
        match self.ask(Request::Messages { conversation: conversation.clone(), page: Page::Newest, limit: 100 }) {
            Response::Messages { messages, .. } => messages,
            _ => Vec::new(),
        }
    }

    fn message(&self, conversation: &Key, id: &str) -> Option<Message> {
        self.messages(conversation).into_iter().find(|message| message.id == id)
    }

    /// Connected, with the teams, channels, chats and their unread state listed.
    fn ready(&self) {
        // The accounts are listed once chatd has read the sessions, a moment after it answers.
        let listed = || match self.ask(Request::Status) {
            Response::Status { accounts, .. } => accounts.into_iter().find(|account| account.key.account == HOME),
            _ => None,
        };
        self.until("the home tenant to connect", || listed().filter(|account| account.link == Link::Connected));
        self.until("the chat list and Teams' unread state", || self.conversation(&key(CHAT_ANA)).filter(|row| row.unread == 1));
        // The teams and their channels are written after the chats, and the activity feed is
        // asked once all of it is.
        self.until("the teams and their channels", || self.fake.calls("GET", "/48:notifications/messages").first().map(drop));
    }

    /// A conversation with its newest page fetched.
    fn open(&self, conversation: &Key, at_least: usize) -> Vec<Message> {
        self.until("the newest page", || Some(self.messages(conversation)).filter(|messages| messages.len() >= at_least))
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
    }
}

fn ranges(message: &Message) -> Vec<(&str, &Style)> {
    let cut = |span: &protocol::Span| &message.text[span.start as usize..span.end as usize];
    message.spans.iter().map(|span| (cut(span), &span.style)).collect()
}

#[test]
fn one_sign_in_is_a_tenant_each_with_teams_channels_chats_and_their_unread_state() {
    let world = World::start();
    world.ready();

    // The home tenant is up and knows who I am. The tenant that refused to renew the sign-in
    // is expired, in Microsoft's own code, and took nothing else down with it.
    let home = world.account(HOME);
    assert_eq!((home.name.as_str(), home.me.as_str(), home.link), ("Contoso", ME, Link::Connected));
    let guest = world.until("the guest tenant to be refused", || Some(world.account(GUEST)).filter(|account| account.link == Link::Expired));
    assert_eq!(guest.detail, "Microsoft ended this sign-in (AADSTS700082). Sign in again.");
    assert_eq!(world.account(HOME).link, Link::Connected);

    // Spaces are the teams alone: chats belong to none. A deleted team is not one.
    let Response::Spaces { spaces } = world.ask(Request::Spaces { network: Some(Network::Teams) }) else {
        panic!("no spaces");
    };
    let named: Vec<(&str, &str)> = spaces.iter().map(|space| (space.key.id.as_str(), space.name.as_str())).collect();
    assert_eq!(named, [(GENERAL, "Platform"), ("19:design0general@thread.tacv2", "Design")]);

    // A team's tree is its channels: General first, the rest as Teams lists them, without the
    // deleted one. The one the user hid is there but opted out, for "Show hidden".
    let (groups, rows) = world.tree(GENERAL).unwrap();
    assert!(groups.is_empty());
    let shown: Vec<(&str, ConversationKind, bool, bool)> = rows.iter().map(|row| (row.title.as_str(), row.kind, row.private, row.opted_out)).collect();
    assert_eq!(
        shown,
        [
            ("General", ConversationKind::Channel, false, false),
            ("Deploys", ConversationKind::Channel, false, false),
            ("Hidden one", ConversationKind::Channel, false, true),
            ("Leads", ConversationKind::Channel, true, false),
        ]
    );
    assert!(rows.iter().all(|row| row.call == CallSupport::Handoff && row.space.as_deref() == Some(GENERAL)));
    let row = |id: &str| rows.iter().find(|row| row.key.id == id).unwrap();
    // General's newest post is past the read marker; Deploys is read to its end; Leads is muted.
    assert_eq!((row(GENERAL).unread, row(GENERAL).badge(), row(GENERAL).last_read.as_deref()), (1, Badge::Dot, Some("1760000260260")));
    assert_eq!((row(DEPLOYS).unread, row(DEPLOYS).badge()), (0, Badge::None));
    assert!(row("19:platform0leads@thread.tacv2").muted && !row(GENERAL).muted);

    // The chats are the Teams home: no Space, newest first, each with its line from the chat
    // list. The hidden chat is not listed; my Notes are, as Saved.
    let home = || match world.ask(Request::Home { network: Network::Teams, account: Some(key("").account()), folder: None, shelf: Default::default(), offset: 0, limit: 50 }) {
        Response::Conversations { conversations, .. } => conversations,
        other => panic!("{other:?}"),
    };
    let rows = home();
    let shown: Vec<(&str, ConversationKind, &str)> = rows.iter().map(|row| (row.title.as_str(), row.kind, row.key.id.as_str())).collect();
    assert_eq!(
        shown,
        [
            ("Ana Ruiz", ConversationKind::Direct, CHAT_ANA),
            ("Ben Okafor, Cy Tran", ConversationKind::Group, "19:grp1@thread.v2"),
            ("Launch crew", ConversationKind::Group, "19:grp2@thread.v2"),
            ("Workflows", ConversationKind::Direct, "19:00000000-0000-4000-8000-0000000000aa_workflows@unq.gbl.spaces"),
            ("Weekly sync", ConversationKind::Group, "19:meeting_NzQ2YjE@thread.v2"),
            ("Notes", ConversationKind::Direct, "48:notes"),
        ]
    );
    assert!(rows.iter().all(|row| row.space.is_none()), "a chat is in no Space");
    // Direct chats show their actual unread count; channels show a dot unless mentioned.
    assert_eq!((rows[0].peer.as_deref(), rows[0].unread, rows[0].badge()), (Some(ANA), 1, Badge::Count(1)));
    assert_eq!(rows[0].avatar.as_ref().map(|avatar| (avatar.kind, avatar.width)), Some((MediaKind::Avatar, 64)));
    assert_eq!((rows[0].preview.as_str(), rows[0].preview_from.as_str(), rows[0].preview_mine), ("Can you look at the deploy?", "Ana Ruiz", false));
    assert!(rows[2].muted && rows[2].unread == 0, "muted by me, and what I said last is not unread");
    assert!(rows[2].preview_mine && rows[2].preview == "Launch moved to Thursday");
    // An app's chat is titled with the app's name, never with mine (its last message is mine),
    // is a bot row with no presence and no call.
    assert!(rows[3].bot && rows[3].peer.is_none() && rows[3].call == CallSupport::None);
    assert!(rows[4].meeting && !rows[3].meeting);
    assert_eq!((rows[5].special, rows[5].call), (protocol::Special::Saved, CallSupport::None));
    let hidden = world.conversation(&key("19:gone@thread.v2")).expect("a hidden chat is kept");
    assert!(hidden.hidden);

    // The list I own on Teams is the people I have a chat with.
    let asked = Request::People { network: Some(Network::Teams), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 };
    let Response::People { people, .. } = world.ask(asked) else {
        panic!("no people");
    };
    let partners: Vec<(&str, &str, &str)> = people.iter().map(|person| (person.name.as_str(), person.handle.as_str(), person.link.as_str())).collect();
    assert_eq!(partners, [("Ana Ruiz", "ana@contoso.com", "msteams:/l/chat/0/0?users=ana%40contoso.com")]);

    // Connecting listed everything from one call, looked the chats' people up together, and
    // fetched history only for the one unread chat.
    world.until("the unread chat to be warmed", || world.fake.calls("GET", &format!("{CHAT_ANA}/messages")).pop());
    let calls = world.fake.calls.lock().unwrap().clone();
    let asked = |method: &str, ending: &str| calls.iter().filter(|call| call.method == method && call.path.ends_with(ending)).count();
    assert_eq!(asked("POST", "/beta/users/fetchShortProfile"), 1);
    assert_eq!(asked("GET", "/v1/users/ME/conversations"), 0);
    assert_eq!(asked("GET", &format!("{GENERAL}/messages")) + asked("GET", "19:grp1@thread.v2/messages"), 0);
    assert_eq!(asked("POST", "/beta/users/searchV2"), 0, "the directory is never listed");
    for call in &calls {
        assert_eq!(call.headers["user-agent"], agent(), "{} was asked as another browser", call.path);
    }

    // The teams list was refused once (a token the service no longer took): the token was
    // minted again and the call repeated. Every refresh used the token the last one issued,
    // and the newest is what would be in the keychain, beside the other tenant's untouched.
    assert_eq!(asked("GET", "/api/csa/api/v1/teams/users/me"), 2);
    let tokens = world.fake.tokens.lock().unwrap().clone();
    assert!(tokens.iter().all(|(tenant, ..)| tenant == HOME));
    assert_eq!(tokens.iter().filter(|(_, audience, ..)| audience == "csa").count(), 2);
    assert_eq!(tokens[0].2, "rt-0");
    for pair in tokens.windows(2) {
        assert_eq!(pair[1].2, pair[0].3, "a refresh presented the token before last");
    }
    let saved = world.until("the rotated refresh token to be saved", || {
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(world.dir.join("session.teams")).ok()?).ok()?;
        (saved["tenants"][0]["refresh_token"] == tokens.last().unwrap().3.as_str()).then_some(saved)
    });
    assert_eq!((&saved["tenants"][1]["refresh_token"], &saved["user_agent"]), (&json!("rt-guest"), &json!(agent())));

    // Push: one socket as the same browser, said who it is, and had the chat service deliver
    // to it under the endpoint trouter was told. It did not claim the user is at the desk.
    let shakes = world.fake.shakes.lock().unwrap().clone();
    assert_eq!(shakes.len(), 1);
    assert!(shakes[0].0.starts_with("/socket.io/1/websocket/sid-42?v=v4&") && shakes[0].0.contains("sig=c2VjcmV0K3NpZw%3D%3D"), "{}", shakes[0].0);
    assert_eq!((shakes[0].1["user-agent"].as_str(), shakes[0].1["x-skypetoken"].as_str()), (agent(), Fake::skype_token().as_str()));
    let heard = world.fake.heard.lock().unwrap().clone();
    assert!(heard.iter().any(|frame| frame.starts_with("5:::{") && frame.contains("user.authenticate") && frame.contains("Bearer at.spaces.")));
    assert!(!heard.iter().any(|frame| frame.contains("user.activity")));
    let registered = world.fake.calls("POST", "/registrar/prod/V2/registrations");
    assert_eq!(registered.len(), 1);
    let named = world.fake.calls("POST", "/v4/a").pop().unwrap();
    let registration = registered[0].json();
    assert_eq!(format!("epid={}", registration["registrationId"].as_str().unwrap()), named.query);
    assert_eq!(registration["transports"]["TROUTER"][0]["path"], format!("{}/v4/f/abc123/", world.fake.base));
    assert_eq!(registration["clientDescription"]["appId"], "TeamsCDLWebWorker");
}

#[test]
fn a_channel_page_is_the_one_message_model_and_reply_chains_are_threads() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    let page = world.open(&general, 6);
    let ids: Vec<&str> = page.iter().map(|message| message.id.as_str()).collect();
    assert_eq!(
        ids,
        ["1760000050050", "1760000100100", "1760000200200", "1760000250250", "1760000260260", "1760000300300", "1760000310310"],
        "the deleted post and the replies are not in the channel's own page"
    );
    let by = |id: &str| page.iter().find(|message| message.id == id).unwrap();

    // A grey line in words, with names already known from the chat list.
    assert_eq!((by("1760000050050").text.as_str(), &by("1760000050050").kind), ("Ana Ruiz added Ben Okafor", &MessageKind::System));
    assert_eq!((by("1760000260260").text.as_str(), &by("1760000260260").kind), ("A call ended · 3 min", &MessageKind::System));

    // Formatting: text and ranges, with the post's headline as its first line.
    let post = by("1760000100100");
    assert_eq!(post.text, "Release plan\nShip today, not tomorrow: cargo test then the plan, @Me Myself & @General 🙂\nbuild\ndeploy\nlooks good");
    let found = ranges(post);
    let has = |text: &str, style: Style| found.contains(&(text, &style));
    assert!(has("Release plan", Style::Bold) && has("Ship", Style::Bold) && has("today", Style::Italic) && has("not", Style::Strike));
    assert!(has("cargo test", Style::Code) && has("the plan", Style::Link { url: "https://ex.com/plan?a=1&b=2".into() }));
    assert!(has("@Me Myself", Style::Mention { kind: MentionKind::User, id: ME.into() }));
    assert!(has("@General", Style::Mention { kind: MentionKind::Everyone, id: GENERAL.into() }));
    assert!(has("build", Style::ListItem { ordered: true, depth: 0 }) && has("deploy", Style::ListItem { ordered: true, depth: 0 }));
    assert!(has("looks good", Style::Quote));
    assert!(post.edited && post.mentions_me && !post.outgoing);
    assert_eq!((post.sender.id.as_str(), post.sender.name.as_str()), (ANA, "Ana Ruiz"));
    assert_eq!(post.sender.avatar.as_ref().map(|avatar| avatar.kind), Some(MediaKind::Avatar));

    // A pasted picture is an attachment whose size is known before a byte is fetched, and its
    // file is registered: asking for it puts it on disk.
    let picture = by(ROOT);
    assert_eq!(picture.text, "Dashboard after the fix");
    let file = &picture.attachments[0].file;
    assert_eq!((file.id.as_str(), file.kind, file.width, file.height), ("img:0-weu-d1-0123456789abcdef", MediaKind::Image, 640, 360));
    assert_eq!(picture.attachments[0].thumb.as_ref().map(|thumb| thumb.id.as_str()), Some("img:0-weu-d1-0123456789abcdef"));
    let fetched = world.until("the picture to be fetched", || {
        let asked = Request::Fetch { retry: false, size: None, account: general.account(), ids: vec![file.id.clone()] };
        match world.ask(asked) {
            Response::Media { files } => files.into_iter().find(|file| file.path.is_some()),
            _ => None,
        }
    });
    assert_eq!(std::fs::read(fetched.path.unwrap()).unwrap(), PNG);
    assert!(world.fake.calls("GET", "/ams/v1/objects/0-weu-d1-0123456789abcdef/views/imgo").len() == 1);

    // Reactions: each person once however often Teams recorded them, mine marked, none empty.
    let reactions: Vec<(&str, u32, bool)> = picture.reactions.iter().map(|reaction| (reaction.emoji.as_str(), reaction.count, reaction.mine)).collect();
    assert_eq!(reactions, [("👍", 2, true), ("❤️", 1, false)]);

    // A shared file is listed with its name and size, and its name opens it in the browser.
    let shared = by("1760000250250");
    assert_eq!(shared.text, "Numbers are in\nQ4 plan.xlsx");
    let attached = &shared.attachments[0];
    assert_eq!((attached.name.as_str(), attached.file.kind, attached.file.bytes), ("Q4 plan.xlsx", MediaKind::File, 48_211));
    assert!(ranges(shared).contains(&("Q4 plan.xlsx", &Style::Link { url: "https://contoso.sharepoint.com/:x:/s/Platform/EaBcDeF".into() })));
    // Asking for the file fetches it through Graph's shares route. The name stays a link.
    let share = "https://contoso.sharepoint.com/:x:/s/Platform/EaBcDeF";
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(share.as_bytes());
    let content = format!("/v1.0/shares/u!{encoded}/driveItem/content");
    let file_id = attached.file.id.clone();
    let fetched = world.until("the shared file to be fetched or refused", || {
        let asked = Request::Fetch { retry: false, size: None, account: general.account(), ids: vec![file_id.clone()] };
        match world.ask(asked) {
            Response::Media { files } => files.into_iter().find(|file| file.id == file_id && (file.path.is_some() || file.failed.is_some())),
            _ => None,
        }
    });
    let Some(path) = fetched.path else {
        panic!("{}", fetched.failed.unwrap_or_else(|| "the shared file was not fetched".to_owned()));
    };
    assert_eq!(std::fs::read(path).unwrap(), SHEET);
    assert_eq!(world.fake.calls("GET", &content).len(), 1);

    // An adaptive card renders into Message::card and stays a normal message (reactable, quotable).
    let card_msg = by("1760000300300");
    assert_eq!(card_msg.kind, MessageKind::Normal);
    let card = card_msg.card.as_deref().expect("adaptive card parsed into Message::card");
    assert_eq!(card.title, "Build 512 passed");
    assert_eq!(card.actions, vec![protocol::CardAction { label: "Open".into(), url: "https://ci.example/512".into() }]);

    // A Loop component is named and linked to its file, not left as a blank card.
    let looped = by("1760000310310");
    assert_eq!(looped.text, "Loop component · Sprint checklist");
    let MessageKind::Unsupported { link } = &looped.kind else {
        panic!("{:?}", looped.kind);
    };
    assert_eq!(link, "https://contoso.sharepoint.com/:fl:/g/personal/ana/EaBcDeF");

    // The post's replies are a thread: counted on the post, read in a conversation of its own
    // that names its channel and its post.
    let summary = picture.thread.clone().expect("the post has a reply chain");
    assert_eq!((summary.id.as_str(), summary.replies, summary.last), (chain().id.as_str(), 2, 1_760_000_220));
    assert_eq!(summary.participants, [key(ME), key(ANA)]);
    let thread = world.conversation(&chain()).expect("the chain is a conversation");
    assert_eq!((thread.kind, thread.parent.as_deref(), thread.root.as_deref()), (ConversationKind::Thread, Some(GENERAL), Some(ROOT)));
    // Titled by its post, never "Replies in General".
    assert_eq!(thread.title, "Dashboard after the fix");
    // Opening it reads the chain whole, post first, and the post's count is then exact.
    let replies = world.until("the whole chain", || Some(world.messages(&chain())).filter(|replies| replies.len() == 4));
    assert_eq!(replies.iter().map(|reply| reply.text.as_str()).collect::<Vec<_>>(), ["Dashboard after the fix", "nice, which panel?", "on it", "the latency one"]);
    assert!(replies[2].outgoing && !replies[1].outgoing);
    let counted = world.until("the post to count three replies", || world.message(&general, ROOT).filter(|post| post.thread.as_ref().is_some_and(|thread| thread.replies == 3)));
    assert_eq!(counted.thread.unwrap().participants, [key(BEN), key(ME), key(ANA)]);
    assert!(world.fake.calls("GET", &format!("{GENERAL};messageid={ROOT}/messages")).pop().unwrap().raw.contains("%3Bmessageid%3D"));

    // Scrolling past the oldest asks for the page before it by the cursor Teams gave.
    let asked = Request::Messages { conversation: general.clone(), page: Page::Before { id: "1760000050050".to_owned() }, limit: 50 };
    world.ask(asked.clone());
    world.until("the older page", || match world.ask(asked.clone()) {
        Response::Messages { messages, .. } => messages.into_iter().find(|message| message.id == "1759990000000"),
        _ => None,
    });
    let pages = world.fake.calls("GET", &format!("{GENERAL}/messages"));
    assert_eq!(pages.len(), 2);
    assert!(pages[0].query.contains("startTime=1") && pages[0].query.contains("pageSize=50") && !pages[0].query.contains("syncState"));
    assert!(pages[1].query.contains("syncState=3e30cursor1"), "{}", pages[1].query);
}

#[test]
fn push_delivers_messages_edits_reactions_deletes_typing_and_reads() {
    let world = World::start();
    world.ready();
    let ana = key(CHAT_ANA);
    let page = world.open(&ana, 3);
    // A chat reply quotes its original; plain text is not read for markup.
    assert_eq!(page[0].text, "1 < 2 && <b>not bold</b>");
    assert!(page[0].spans.is_empty());
    let quote = page[2].reply_to.clone().expect("the reply quotes");
    assert_eq!(page[2].text, "yes, since noon");
    assert_eq!((quote.id.as_str(), quote.sender.as_str(), quote.snippet.as_str(), quote.person), ("1760000480480", "Me Myself", "is the build green?", Some(key(ME))));

    // Someone types, then says it.
    let mut typing = message(CHAT_ANA, None, 1_760_000_590_000, ANA, "Ana Ruiz", "");
    typing["messagetype"] = "Control/Typing".into();
    typing["contenttype"] = "Application/Message".into();
    world.fake.push("NewMessage", typing, false);
    let watch = || Request::Watch(Watching { conversation: Some(ana.clone()), ..Watching::default() });
    let typist = world.until("Ana to be typing", || match world.ask(watch()) {
        Response::Watching { mut typists, .. } => typists.pop(),
        _ => None,
    });
    assert_eq!((typist.id.as_str(), typist.name.as_str()), (ANA, "Ana Ruiz"));

    world.fake.push("NewMessage", message(CHAT_ANA, None, 1_760_000_600_600, ANA, "Ana Ruiz", "<p>ping</p>"), false);
    let arrived = world.until("the message to arrive", || world.message(&ana, "1760000600600"));
    assert_eq!((arrived.text.as_str(), arrived.sent, arrived.edited), ("ping", 1_760_000_600, false));
    let row = world.conversation(&ana).unwrap();
    assert_eq!((row.unread, row.preview.as_str()), (2, "ping"));
    assert!(matches!(world.ask(watch()), Response::Watching { typists, .. } if typists.is_empty()), "what was typed has been said");

    // An edit is the same message sent again with an edit time; this one arrives gzipped.
    let mut edit = message(CHAT_ANA, None, 1_760_000_600_600, ANA, "Ana Ruiz", "<p>ping<b>!</b></p>");
    edit["properties"] = json!({"edittime": "1760000601000"});
    world.fake.push("MessageUpdate", edit.clone(), true);
    let edited = world.until("the edit", || world.message(&ana, "1760000600600").filter(|message| message.edited));
    assert_eq!(edited.text, "ping!");
    assert_eq!(world.conversation(&ana).unwrap().unread, 2, "an edit is not another unread message");

    // A reaction is the same message again with its reactions; the old edit time rides along.
    edit["properties"]["emotions"] = json!([{"key": "1f389_partypopper", "users": [{"mri": ANA, "time": 1_760_000_602_000u64, "value": "1760000602000"}]}]);
    world.fake.push("MessageUpdate", edit.clone(), false);
    let reacted = world.until("the reaction", || world.message(&ana, "1760000600600").filter(|message| !message.reactions.is_empty()));
    assert_eq!((reacted.reactions[0].emoji.as_str(), reacted.reactions[0].count, reacted.reactions[0].mine, reacted.text.as_str()), ("🎉", 1, false, "ping!"));

    // A delete is the same message again with a delete time and nothing left in it.
    edit["content"] = "".into();
    edit["properties"] = json!({"deletetime": 1_760_000_603_000u64});
    world.fake.push("MessageUpdate", edit, false);
    world.until("the message to go", || world.message(&ana, "1760000600600").is_none().then_some(()));

    // Read on another device: the marker moves and the number goes.
    world.fake.push(
        "ConversationUpdate",
        json!({
            "id": CHAT_ANA, "type": "Conversation", "version": 1_760_000_610_000u64,
            "properties": {"consumptionhorizon": "1760000500500;1760000610000;11931600396359500"},
            "lastMessage": {"id": "1760000500500", "type": "Message", "messagetype": "RichText/Html"},
        }),
        false,
    );
    let read = world.until("the chat to be read elsewhere", || world.conversation(&ana).filter(|row| row.unread == 0));
    assert_eq!(read.last_read.as_deref(), Some("1760000500500"));

    // A reply in a channel goes to its chain, counts on its post, and is not in the channel.
    let general = key(GENERAL);
    world.open(&general, 6);
    world.fake.push("NewMessage", message(GENERAL, Some(ROOT), 1_760_000_700_700, ANA, "Ana Ruiz", "<p>fixed in 513</p>"), false);
    let post = world.until("the post to count the reply", || world.message(&general, ROOT).filter(|post| post.thread.as_ref().is_some_and(|thread| thread.replies == 3)));
    assert_eq!(post.thread.unwrap().last, 1_760_000_700);
    assert!(world.message(&general, "1760000700700").is_none());
    assert_eq!(world.messages(&chain()).last().map(|reply| reply.text.clone()).as_deref(), Some("fixed in 513"));
    // A post in a channel the user hid lands in its row, which stays behind "Show hidden".
    world.fake.push("NewMessage", message("19:platform0hidden@thread.tacv2", Some("1760000710710"), 1_760_000_710_710, ANA, "Ana Ruiz", "<p>psst</p>"), false);
    world.fake.push("NewMessage", message(DEPLOYS, Some("1760000720720"), 1_760_000_720_720, BEN, "Ben Okafor", "<p>v513 is out</p>"), false);
    world.until("the next channel post", || world.conversation(&key(DEPLOYS)).filter(|row| row.unread == 1 && row.preview == "v513 is out"));
    let hidden = world.until("the hidden channel's post", || world.conversation(&key("19:platform0hidden@thread.tacv2")).filter(|row| row.preview == "psst"));
    assert!(hidden.opted_out);

    // Every event was answered, as trouter requires, under its own id.
    let answered = world.until("every event to be answered", || {
        let heard = world.fake.heard.lock().unwrap();
        let answers: Vec<Value> = heard.iter().filter_map(|frame| serde_json::from_str(frame.strip_prefix("3:::")?).ok()).collect();
        (answers.len() as u32 == world.fake.pushed.load(Ordering::Relaxed)).then_some(answers)
    });
    assert!(answered.iter().enumerate().all(|(at, answer)| answer["id"] == at as u64 + 1 && answer["status"] == 200 && answer["body"] == ""));
}

#[test]
fn sending_editing_reacting_deleting_and_reading_go_to_teams() {
    let world = World::start();
    world.ready();
    let ana = key(CHAT_ANA);
    world.open(&ana, 3);
    let path = |tail: &str| format!("/chat/v1/users/ME/conversations/{CHAT_ANA}{tail}");

    // Send: pending at once, then the copy push delivers replaces it under Teams' own id.
    let client_id = "1760000999000111";
    let pending = world.send(&ana, client_id, "hello <there>\nAna");
    assert_eq!((pending.id.as_str(), &pending.state), (client_id, &SendState::Pending));
    let sent = world.until("the message to be accepted", || {
        world.messages(&ana).into_iter().find(|message| message.client_id.as_deref() == Some(client_id) && message.state == SendState::Sent)
    });
    assert_eq!((sent.id.as_str(), sent.text.as_str(), sent.outgoing, sent.sent), ("1760001000000", "hello <there>\nAna", true, 1_760_001_000));
    assert_eq!(sent.sender.id, ME);
    let posted = world.fake.calls("POST", &path("/messages")).pop().unwrap();
    // Exactly these five fields: Teams accepts more and silently drops the message.
    assert_eq!(
        posted.json(),
        json!({
            "clientmessageid": client_id, "content": "<p>hello &lt;there&gt;<br>Ana</p>", "messagetype": "RichText/Html",
            "contenttype": "text", "imdisplayname": "Me Myself",
        })
    );
    // The echo has arrived by now, and there is still one row.
    world.until("the echo on push", || (world.fake.pushed.load(Ordering::Relaxed) == 1).then_some(()));
    sleep(Duration::from_millis(300));
    let copies = world.messages(&ana).into_iter().filter(|message| message.text.starts_with("hello")).count();
    assert_eq!(copies, 1);
    assert_eq!(world.conversation(&ana).unwrap().unread, 0, "answering reads the chat");

    // A rate limit is told to the user, not waited out behind their back.
    world.send(&ana, "1760000999000222", "limit me");
    let failed = world.until("the refused send", || world.message(&ana, "1760000999000222").filter(|message| message.state != SendState::Pending));
    assert_eq!(failed.state, SendState::Failed { reason: "Teams is rate limiting you. Try again in 7 s.".to_owned() });

    // Edit: Teams is asked first, and the message changes once it agreed.
    let edit = Request::Edit { conversation: ana.clone(), message_id: sent.id.clone(), text: "hello Ana".to_owned(), spans: Vec::new() };
    assert_eq!(world.ask(edit), Response::Ok);
    let put = world.fake.calls("PUT", &path("/messages/1760001000000")).pop().expect("the edit reached Teams");
    assert_eq!((put.json()["content"].as_str(), put.json()["messagetype"].as_str()), (Some("<p>hello Ana</p>"), Some("RichText/Html")));
    let edited = world.message(&ana, &sent.id).unwrap();
    assert_eq!((edited.text.as_str(), edited.edited), ("hello Ana", true));

    // React and take it back, under Teams' name for the emoji.
    let react = |emoji: &str, on: bool| Request::React { conversation: ana.clone(), message_id: "1760000500500".to_owned(), emoji: emoji.to_owned(), on , burst: false };
    assert_eq!(world.ask(react("👍", true)), Response::Ok);
    let given = world.until("the reaction to reach Teams", || world.fake.calls("PUT", &path("/messages/1760000500500/properties")).pop());
    assert_eq!((given.query.as_str(), given.json()["emotions"]["key"].as_str()), ("name=emotions", Some("like")));
    assert!(given.json()["emotions"]["value"].as_u64().is_some_and(|at| at > 1_700_000_000_000));
    assert_eq!(world.ask(react("👍", false)), Response::Ok);
    let taken = world.until("the reaction to be taken back", || world.fake.calls("DELETE", &path("/messages/1760000500500/properties")).pop());
    assert_eq!(taken.json(), json!({"emotions": {"key": "like"}}));
    // A skin tone stays on the catalog's tone base (`yes`), not the plain `like` key.
    assert_eq!(world.ask(react("👍🏽", true)), Response::Ok);
    let toned = world.until("the toned reaction to reach Teams", || {
        world.fake.calls("PUT", &path("/messages/1760000500500/properties")).into_iter().rev().find(|call| call.json()["emotions"]["key"].as_str() == Some("yes-tone3"))
    });
    assert!(toned.json()["emotions"]["value"].as_u64().is_some());
    // Squid is in the catalog, so it goes out under that key.
    assert_eq!(world.ask(react("🦑", true)), Response::Ok);
    let squid = world.until("the squid reaction to reach Teams", || {
        world.fake.calls("PUT", &path("/messages/1760000500500/properties")).into_iter().rev().find(|call| call.json()["emotions"]["key"].as_str() == Some("1f991_squid"))
    });
    assert_eq!(squid.json()["emotions"]["key"].as_str(), Some("1f991_squid"));
    let Response::Reactions { rules, .. } = world.ask(Request::Reactions { conversation: ana.clone() }) else {
        panic!("no rules");
    };
    assert_eq!(rules.set, protocol::ReactionSet::Any);
    assert!(rules.tones);
    assert!(rules.only.is_empty());
    // One the catalog cannot name is refused, and Teams is not asked.
    let before = world.fake.calls("PUT", &path("/messages/1760000500500/properties")).len();
    let unkeyed = world.ask(react("\u{e000}", true));
    assert_eq!(
        unkeyed,
        Response::Error { code: protocol::ErrorCode::Refused, detail: "Teams refused (400: tinyclient does not know Teams' name for that emoji yet).".to_owned() }
    );
    assert_eq!(world.fake.calls("PUT", &path("/messages/1760000500500/properties")).len(), before);

    // Mark read: the read marker, at that message.
    assert_eq!(world.ask(Request::MarkRead { conversation: ana.clone(), message_id: sent.id.clone() }), Response::Ok);
    let marked = world.until("Teams to be told the chat was read", || world.fake.calls("PUT", &path("/properties")).pop());
    assert_eq!(marked.query, "name=consumptionhorizon");
    let horizon = marked.json()["consumptionhorizon"].as_str().unwrap().to_owned();
    let parts: Vec<&str> = horizon.split(';').collect();
    assert_eq!((parts.len(), parts[0], parts[2]), (3, "1760001000000", "1760001000000"));
    assert!(parts[1].parse::<u64>().unwrap() > 1_700_000_000_000);

    // P2 off: the chat is read here and Teams is not told.
    let scope = protocol::settings::account_scope(&ana.account());
    let receipts = |value: Option<&str>| Request::Set { scope: scope.clone(), key: protocol::settings::RECEIPTS.to_owned(), value: value.map(str::to_owned) };
    assert_eq!(world.ask(receipts(Some("off"))), Response::Ok);
    let told = world.fake.calls("PUT", &path("/properties")).len();
    assert_eq!(world.ask(Request::MarkRead { conversation: ana.clone(), message_id: "1760000500500".to_owned() }), Response::Ok);
    sleep(Duration::from_millis(400));
    assert_eq!(world.fake.calls("PUT", &path("/properties")).len(), told, "Teams heard a read with receipts off");
    assert_eq!(world.ask(receipts(None)), Response::Ok);

    // Typing: one signal however often the draft changes.
    for _ in 0..3 {
        assert_eq!(world.ask(Request::Typing { conversation: ana.clone(), activity: protocol::Compose::Typing }), Response::Ok);
    }
    let typed = world.until("the typing signal", || {
        let signals: Vec<Call> = world.fake.calls("POST", &path("/messages")).into_iter().filter(|call| call.json()["messagetype"] == "Control/Typing").collect();
        (!signals.is_empty()).then_some(signals)
    });
    sleep(Duration::from_millis(400));
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].json(), json!({"content": "", "contenttype": "Application/Message", "messagetype": "Control/Typing"}));

    // Delete: gone here once Teams agreed.
    assert_eq!(world.ask(Request::Delete { conversation: ana.clone(), message_id: sent.id.clone(), scope: protocol::DeleteScope::Everyone }), Response::Ok);
    let deleted = world.fake.calls("DELETE", &path("/messages/1760001000000")).pop().expect("the delete reached Teams");
    assert_eq!(deleted.query, "behavior=softDelete");
    assert!(world.message(&ana, &sent.id).is_none());

    // A reply typed in a chain's pane goes to the chain and counts on its post.
    let general = key(GENERAL);
    world.open(&general, 6);
    world.until("the chain", || world.conversation(&chain()));
    world.send(&chain(), "1760000999000333", "looking now");
    let reply = world.until("the reply to be accepted", || {
        world.messages(&chain()).into_iter().find(|message| message.client_id.as_deref() == Some("1760000999000333") && message.state == SendState::Sent)
    });
    assert_eq!(reply.text, "looking now");
    let to_chain = world.fake.calls("POST", &format!("{GENERAL};messageid={ROOT}/messages")).pop().expect("posted to the chain");
    assert!(to_chain.raw.ends_with("19%3Aplatform0general%40thread.tacv2%3Bmessageid%3D1760000200200/messages"), "{}", to_chain.raw);
    world.until("the post to count it", || world.message(&general, ROOT).filter(|post| post.thread.as_ref().is_some_and(|thread| thread.replies == 3)));
    assert!(world.message(&general, &reply.id).is_none());
}

#[test]
fn people_are_searched_watched_and_called_through_the_official_app() {
    let world = World::start();
    world.ready();

    // The directory is searched on Teams' side, with the query as the body.
    let asked = || Request::People { network: Some(Network::Teams), relation: None, query: Some("dee".to_owned()), offset: 0, limit: 25 };
    let found = world.until("the search to find Dee", || match world.ask(asked()) {
        Response::People { people, .. } => people.into_iter().find(|person| person.name == "Dee Park"),
        _ => None,
    });
    assert_eq!((found.key.id.as_str(), found.handle.as_str(), found.relation), (DEE, "dee@contoso.com", Relation::None));
    let searched = world.fake.calls("POST", "/beta/users/searchV2");
    assert_eq!((searched.len(), searched[0].body.as_str()), (1, "dee"));

    // Starting a chat creates it on Teams and it is a row at once.
    let Response::Conversation { conversation } = world.ask(Request::StartDirect { person: key(DEE) }) else {
        panic!("no chat");
    };
    assert_eq!((conversation.key.id.as_str(), conversation.kind, conversation.title.as_str()), (CHAT_DEE, ConversationKind::Direct, "Dee Park"));
    assert_eq!((conversation.peer.as_deref(), conversation.space.as_deref()), (Some(DEE), None));
    let created = world.fake.calls("POST", "/chat/v1/threads").pop().unwrap().json();
    assert_eq!(created["members"], json!([{"id": ME, "role": "Admin"}, {"id": DEE, "role": "Admin"}]));
    assert_eq!(created["properties"], json!({"threadType": "chat", "fixedRoster": true, "uniquerosterthread": true}));
    // Asked again, it is the chat already made.
    assert!(matches!(world.ask(Request::StartDirect { person: key(DEE) }), Response::Conversation { conversation } if conversation.key.id == CHAT_DEE));
    assert_eq!(world.fake.calls("POST", "/chat/v1/threads").len(), 1);

    // Opening the card asks for her presence, so the change is in the log from here.
    let Response::Status { seq, .. } = world.ask(Request::Status) else {
        panic!("no status");
    };
    // A person's card says what their organization says of them.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key(ANA), space: None }) else {
        panic!("no profile");
    };
    // Title, department and email each in their place; nothing left as a bio.
    assert_eq!((profile.person.name.as_str(), profile.bio.as_str()), ("Ana Ruiz", ""));
    assert_eq!((profile.title.as_str(), profile.department.as_str()), ("Staff Engineer", "Platform"));
    assert_eq!(
        profile.contacts.iter().map(|contact| contact.value.as_str()).collect::<Vec<_>>(),
        ["ana@contoso.com", "+351 21 555 0100", "+351 91 555 0199"]
    );

    // Presence is asked for exactly who is on screen, together.
    let watch = || Request::Watch(Watching { people: vec![key(ANA)], ..Watching::default() });
    let seen = world.until("Ana's presence", || match world.ask(watch()) {
        Response::Watching { people, .. } => people.into_iter().find(|person| person.presence != Presence::Unknown),
        _ => None,
    });
    // In a meeting is Busy, not do-not-disturb, and what she is doing rides beside it.
    assert_eq!((seen.key.id.as_str(), seen.presence), (ANA, Presence::Busy));
    assert_eq!(seen.activity.as_ref().map(|activity| activity.kind), Some(protocol::ActivityKind::InMeeting));
    // Her note rides with the presence itself, read for its words.
    let Response::Changes { changes, .. } = world.ask(Request::Wait { since: seq }) else {
        panic!("no changes");
    };
    let told = changes.into_iter().find_map(|change| match change {
        Change::Presence { person, presence, status, .. } if person == key(ANA) => Some((presence, status)),
        _ => None,
    });
    // What she is doing comes before her note.
    assert_eq!(told, Some((Presence::Busy, "In a meeting".to_owned())));
    // Opening the card asks for her alone. The watch then asks for her and me, on the adapter thread.
    let together = json!([{"mri": ANA}, {"mri": ME}]);
    world.until("who is on screen, and me", || {
        world.fake.calls("POST", "/presence/v1/presence/getpresence/").into_iter().any(|call| call.json() == together).then_some(())
    });
    // Asked again at once for the same people, Teams is not asked again.
    let asked = world.fake.calls("POST", "/presence/v1/presence/getpresence/").len();
    world.ask(watch());
    sleep(Duration::from_millis(400));
    assert_eq!(world.fake.calls("POST", "/presence/v1/presence/getpresence/").len(), asked);

    // My own presence is set only when I ask.
    assert!(world.fake.calls("PUT", "/presence/v1/me/forceavailability/").is_empty());
    let set = Request::SetPresence { account: key("").account(), presence: Presence::Dnd, status: None, status_emoji: None, clear_at: None };
    assert_eq!(world.ask(set), Response::Ok);
    assert_eq!(world.fake.calls("PUT", "/presence/v1/me/forceavailability/").pop().unwrap().json(), json!({"availability": "DoNotDisturb"}));

    // The team's roster: owners, members and guests, in that order. A bot stays a member.
    let Response::Members { members, sections, .. } = world.ask(Request::Members { conversation: key(GENERAL), offset: 0, limit: 50 }) else {
        panic!("no members");
    };
    let named: Vec<(&str, u32)> = sections.iter().map(|section| (section.name.as_str(), section.count)).collect();
    assert_eq!(named, [("Owners", 1), ("Members", 3), ("Guests", 1)]);
    let listed: Vec<(&str, bool)> = members.iter().map(|member| (member.name.as_str(), member.bot)).collect();
    assert_eq!(listed, [("Me Myself", false), ("Ana Ruiz", false), ("Ben Okafor", false), ("Deploy Bot", true), ("Dee Park", false)]);

    // Calls and meetings are the official app's: a link, never a call here.
    let link = |conversation: &str| match world.ask(Request::Handoff { conversation: key(conversation) }) {
        Response::Link { url } => url,
        other => panic!("{other:?}"),
    };
    assert_eq!(link(CHAT_ANA), "msteams:/l/call/0/0?users=ana%40contoso.com");
    assert_eq!(link("19:grp1@thread.v2"), "msteams:/l/chat/19%3Agrp1%40thread.v2/conversations");
    assert_eq!(
        link(GENERAL),
        format!("msteams:/l/channel/19%3Aplatform0general%40thread.tacv2/General?groupId=e0873025-0000-4000-8000-00000000beef&tenantId={HOME}")
    );
    assert!(matches!(world.ask(Request::CallStart { conversation: key(CHAT_ANA) }), Response::Error { .. }), "a Teams call is never carried here");
}

#[test]
fn a_team_answers_space_info_from_its_own_thread() {
    let world = World::start();
    world.ready();

    let Response::SpaceInfo { info } = world.ask(Request::SpaceInfo { space: key(GENERAL) }) else {
        panic!("no space info");
    };
    assert_eq!(info.space.name.as_str(), "Platform");
    assert_eq!(info.description.as_str(), "How the platform ships");
    assert_eq!(info.members, Some(5));
    assert_eq!(info.roles, ["Owner"]);
    let fetched = world.fake.calls("GET", &format!("/v1/threads/{GENERAL}"));
    assert_eq!(fetched.len(), 1, "the team's thread is read once");
}

#[test]
fn mentions_missed_while_closed_info_mute_seen_hidden_chats_and_files_through_onedrive() {
    let world = World::start();
    world.ready();
    let grp1 = key("19:grp1@thread.v2");

    // The Activity stream is read once at connect: a mention in a chat while tinyclient was
    // closed counts there; one already read (General, before its marker) and a reaction do not.
    let counted = world.until("the missed mention", || world.conversation(&grp1).filter(|row| row.mentions == 1));
    assert_eq!(counted.unread, 1);
    assert_eq!(world.conversation(&key(GENERAL)).unwrap().mentions, 0);
    assert_eq!(world.fake.calls("GET", "/48:notifications/messages").len(), 1);
    let Response::Conversations { counts, .. } = world.ask(Request::Inbox { network: Some(Network::Teams), filter: protocol::InboxFilter::Mentions, offset: 0, limit: 50 }) else {
        panic!("no inbox");
    };
    assert_eq!(counts.mentions, 1);

    // What connecting asked, once each: the token audiences, authz, the one list, one lookup of
    // the chats' people, and one page of the Activity stream; history for the unread chats only.
    world.until("the unread chat to be warmed", || world.fake.calls("GET", &format!("{CHAT_ANA}/messages")).pop());
    let calls = world.fake.calls.lock().unwrap().clone();
    let at_connect: Vec<String> = calls.iter().map(|call| format!("{} {}", call.method, call.path.rsplit('/').next().unwrap_or(""))).collect();
    let lists = calls.iter().filter(|call| call.path.ends_with("/teams/users/me")).count();
    // The list is answered 401 once by the fake (a stale token), so twice.
    assert_eq!(lists, 2, "{at_connect:#?}");
    assert_eq!(calls.iter().filter(|call| call.path.ends_with("/fetchShortProfile")).count(), 1);
    assert_eq!(calls.iter().filter(|call| call.path.contains("/48:")).count(), 1, "the call log is not read");

    // The info pane: one description of the thread.
    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: key(GENERAL) }) else {
        panic!("no info");
    };
    assert_eq!((info.description.as_str(), info.members, info.notify), ("How the platform ships", Some(5), protocol::Notify::All));
    assert_eq!(
        info.link,
        format!("https://teams.microsoft.com/l/channel/19%3Aplatform0general%40thread.tacv2/General?groupId=e0873025-0000-4000-8000-00000000beef&tenantId={HOME}")
    );
    let Response::ConversationInfo { info: chat } = world.ask(Request::ConversationInfo { conversation: grp1.clone() }) else {
        panic!("no chat info");
    };
    assert_eq!(chat.link, "https://teams.microsoft.com/l/chat/19%3Agrp1%40thread.v2/conversations");

    // Mute is set on Teams, then shown.
    assert_eq!(world.ask(Request::SetMute { conversation: grp1.clone(), notify: protocol::Notify::Nothing, until: None }), Response::Ok);
    let muted = world.fake.calls("PUT", "19:grp1@thread.v2/properties").pop().unwrap();
    assert_eq!((muted.query.as_str(), muted.json()), ("name=alerts", json!({"alerts": "false"})));
    assert!(world.conversation(&grp1).unwrap().muted);

    // Opening a chat whose last message is mine asks who has read it: everyone has.
    let launch = key("19:grp2@thread.v2");
    assert!(!world.conversation(&launch).unwrap().seen);
    world.messages(&launch);
    world.until("the launch chat to be seen", || world.conversation(&launch).filter(|row| row.seen));
    assert_eq!(world.fake.calls("GET", "/consumptionhorizons").len(), 1);
    // A chat whose last message is not mine is not asked about.
    world.messages(&key(CHAT_ANA));
    world.until("Ana's chat to be read", || world.fake.calls("GET", &format!("{CHAT_ANA}/messages")).pop());
    sleep(Duration::from_millis(300));
    assert_eq!(world.fake.calls("GET", "/consumptionhorizons").len(), 1);

    // A hidden chat comes back with a new message, as in Teams.
    world.fake.push("NewMessage", message("19:gone@thread.v2", None, 1_760_000_700_700, DEE, "Dee Park", "<p>still there?</p>"), false);
    world.until("the hidden chat to come back", || world.conversation(&key("19:gone@thread.v2")).filter(|row| !row.hidden));

    // A colleague out of office: their card says so, in their own words, until when.
    let search = || Request::People { network: Some(Network::Teams), relation: None, query: Some("dee".to_owned()), offset: 0, limit: 25 };
    world.until("Dee to be found", || match world.ask(search()) {
        Response::People { people, .. } => people.into_iter().find(|person| person.key == key(DEE)),
        _ => None,
    });
    world.until("Dee's presence", || match world.ask(Request::Watch(Watching { people: vec![key(DEE)], ..Watching::default() })) {
        Response::Watching { people, .. } => people.into_iter().find(|person| person.presence == Presence::Idle),
        _ => None,
    });
    let Response::Profile { profile } = world.ask(Request::Profile { person: key(DEE), space: None }) else {
        panic!("no profile");
    };
    assert_eq!((profile.away.as_str(), profile.away_until), ("Back Monday, ask Ben", Some(1_792_396_800)));
    assert_eq!((profile.title.as_str(), profile.department.as_str()), ("Product Manager", "Product"));

    // A file in a 1:1 chat uploads to OneDrive and shares ONLY with the chat's members via
    // Graph `/invite` — never an organization-wide `/createLink`.
    // On Windows "/tmp" has no drive; chatd takes only absolute paths.
    let staged = std::path::absolute(format!("/tmp/tinyclient-teams-file-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&staged).unwrap();
    let path = staged.join("notes.txt");
    std::fs::write(&path, b"standup notes").unwrap();
    let with = protocol::Outgoing {
        files: vec![protocol::OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: false, as_file: false, original: false, view_once: false, media: None }],
        ..Default::default()
    };
    let asked = Request::Send { conversation: key(CHAT_ANA), client_id: "88001".into(), text: "notes attached".into(), reply_to: None, with };
    let answer = world.ask(asked);
    assert!(matches!(answer, Response::Sent { .. }), "chat file send failed: {answer:?}");
    world.until("chat file message sent", || {
        world.messages(&key(CHAT_ANA)).into_iter().find(|m| m.client_id.as_deref() == Some("88001") && m.state == SendState::Sent)
    });
    assert!(!world.fake.calls("POST", ":/createUploadSession").is_empty());
    let invites = world.fake.calls("POST", "/v1.0/me/drive/items/01ITEM/invite");
    assert_eq!(invites.len(), 1, "expected per-member invite");
    let invite_body = invites[0].json();
    assert_eq!(invite_body["recipients"][0]["email"].as_str(), Some("ana@contoso.com"));
    assert_eq!(invite_body["requireSignIn"], true);
    assert_eq!(invite_body["sendInvitation"], false);
    assert!(world.fake.calls("POST", "/createLink").is_empty(), "must never create an organization-wide link");
    let _ = std::fs::remove_dir_all(&staged);
}

/// Walks toward the start of a chain, asking for the page before each edge.
fn reached_reply_one(world: &World, chain: &Key) -> bool {
    let mut page = world.messages(chain);
    if page.is_empty() {
        return false;
    }
    for _ in 0..8 {
        if page.iter().any(|message| message.text == "reply 1") {
            return true;
        }
        let oldest = page.iter().min_by_key(|message| message.id.parse::<u64>().unwrap_or(0)).unwrap().id.clone();
        let asked = Request::Messages { conversation: chain.clone(), page: Page::Before { id: oldest }, limit: 200 };
        let Response::Messages { messages, .. } = world.ask(asked) else {
            return false;
        };
        if messages.is_empty() {
            return false;
        }
        page = messages;
    }
    false
}

#[test]
fn a_chain_of_450_replies_loads_in_three_pages_and_an_old_reply_opens() {
    let world = World::start();
    world.ready();
    let chain = key(&format!("{GENERAL};messageid={LONG}"));
    world.until("the newest replies", || Some(world.messages(&chain)).filter(|messages| messages.iter().any(|message| message.text == "reply 450")));
    world.until("all 450 replies", || reached_reply_one(&world, &chain).then_some(()));
    let pages = world.fake.calls("GET", &format!("{GENERAL};messageid={LONG}/messages"));
    assert_eq!(pages.len(), 3, "{:?}", pages.iter().map(|call| call.query.clone()).collect::<Vec<_>>());
    assert!(pages[0].query.contains("pageSize=200") && pages[0].query.contains("startTime=1") && !pages[0].query.contains("syncState"), "{}", pages[0].query);
    assert!(pages[1].query.contains("syncState=chaincursor1") && pages[1].query.contains("pageSize=200"), "{}", pages[1].query);
    assert!(pages[2].query.contains("syncState=chaincursor2") && pages[2].query.contains("pageSize=200"), "{}", pages[2].query);

    let around = reply_at(10).to_string();
    let jump = key(&format!("{GENERAL};messageid={JUMP}"));
    let asked = Request::Messages { conversation: jump.clone(), page: Page::Around { id: around.clone() }, limit: 50 };
    world.ask(asked.clone());
    world.until("reply 10", || match world.ask(asked.clone()) {
        Response::Messages { messages, .. } => messages.into_iter().find(|message| message.id == around),
        _ => None,
    });
    let jumped = world.fake.calls("GET", &format!("{GENERAL};messageid={JUMP}/messages"));
    assert_eq!(jumped.len(), 1, "{:?}", jumped.iter().map(|call| call.query.clone()).collect::<Vec<_>>());
    assert!(jumped[0].query.contains("pageSize=200") && jumped[0].query.contains(&format!("endTime={}", reply_at(10) + 1)), "{}", jumped[0].query);
}

#[test]
fn presence_is_asked_only_for_who_is_watched_and_a_rate_limit_is_waited_out_once() {
    let world = World::start();
    world.ready();
    // Connected and idle, nobody on screen: presence is pushed or not asked at all.
    sleep(Duration::from_millis(500));
    assert!(world.fake.calls("POST", "/presence/v1/presence/getpresence/").is_empty());

    // Someone on screen whose presence is rate limited: asked, waited out once, asked again,
    // and then left; never a third time.
    let limited = key("8:orgid:limit-me");
    world.ask(Request::Watch(Watching { people: vec![limited], ..Watching::default() }));
    world.until("the second try", || (world.fake.calls("POST", "/presence/v1/presence/getpresence/").len() >= 2).then_some(()));
    sleep(Duration::from_millis(1600));
    assert_eq!(world.fake.calls("POST", "/presence/v1/presence/getpresence/").len(), 2);
}

#[test]
fn leaving_a_group_chat_removes_myself_and_drops_the_row() {
    let world = World::start();
    world.ready();
    let grp1 = key("19:grp1@thread.v2");
    assert!(world.conversation(&grp1).is_some());

    assert_eq!(world.ask(Request::Leave { conversation: grp1.clone() }), Response::Ok);
    let left = world.fake.calls("DELETE", &format!("/v1/threads/19:grp1@thread.v2/members/{ME}"));
    assert_eq!(left.len(), 1);
    assert!(world.conversation(&grp1).is_none());
}

#[test]
fn setting_a_status_message_publishes_the_note_and_availability() {
    let world = World::start();
    world.ready();

    let set = Request::SetPresence {
        account: key("").account(),
        presence: Presence::Dnd,
        status: Some("Heads down until 3".to_owned()),
        status_emoji: None,
        clear_at: None,
    };
    assert_eq!(world.ask(set), Response::Ok);
    let avail = world.fake.calls("PUT", "/presence/v1/me/forceavailability/").pop().expect("availability set");
    assert_eq!(avail.json(), json!({"availability": "DoNotDisturb"}));
    let note = world.fake.calls("PUT", "/presence/v1/me/publishnote").pop().expect("note published");
    assert_eq!(note.json()["message"], json!("Heads down until 3"));
    assert!(note.json()["expiry"].as_str().is_some_and(|e| !e.is_empty()));

    // Clearing the status message publishes an empty note.
    let clear = Request::SetPresence {
        account: key("").account(),
        presence: Presence::Online,
        status: Some(String::new()),
        status_emoji: None,
        clear_at: None,
    };
    assert_eq!(world.ask(clear), Response::Ok);
    let cleared = world.fake.calls("PUT", "/presence/v1/me/publishnote").pop().expect("note cleared");
    assert_eq!(cleared.json()["message"], json!(""));
}

#[test]
fn sending_a_mention_writes_the_span_and_properties_mentions() {
    let world = World::start();
    world.ready();
    let ana = key(CHAT_ANA);
    world.open(&ana, 3);

    let client_id = "1760000999000444";
    world.send(&ana, client_id, &format!("hi <@{ANA}|Ana Ruiz> check <#{GENERAL}|General>"));
    let sent = world.until("the mention message to be accepted", || {
        world.messages(&ana).into_iter().find(|message| message.client_id.as_deref() == Some(client_id) && message.state == SendState::Sent)
    });
    let posted = world.fake.calls("POST", &format!("/chat/v1/users/ME/conversations/{CHAT_ANA}/messages")).pop().expect("posted");
    let body = posted.json();
    assert_eq!(
        body["content"].as_str(),
        Some("<p>hi <span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Ana Ruiz</span> check <span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"1\">General</span></p>")
    );
    let mentions: Vec<Value> = serde_json::from_str(body["properties"]["mentions"].as_str().expect("properties.mentions string")).unwrap();
    assert_eq!(mentions.len(), 2);
    assert_eq!((mentions[0]["itemid"].as_u64(), mentions[0]["mri"].as_str(), mentions[0]["mentionType"].as_str(), mentions[0]["displayName"].as_str()), (Some(0), Some(ANA), Some("person"), Some("Ana Ruiz")));
    assert_eq!((mentions[1]["itemid"].as_u64(), mentions[1]["mri"].as_str(), mentions[1]["mentionType"].as_str(), mentions[1]["displayName"].as_str()), (Some(1), Some(GENERAL), Some("channel"), Some("General")));
    assert_eq!(sent.text, "hi @Ana Ruiz check @General");
    let found = ranges(&sent);
    assert!(found.contains(&("@Ana Ruiz", &Style::Mention { kind: MentionKind::User, id: ANA.into() })));
    assert!(found.contains(&("@General", &Style::Mention { kind: MentionKind::Everyone, id: GENERAL.into() })));
}

#[test]
fn editing_a_message_preserves_formatting_mentions_images_and_subject() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    world.open(&general, 6);

    let img_url = format!("{}/ams/v1/objects/0-weu-d1-editimg/views/imgo", world.fake.base);
    let post_id = 1_760_000_800_800u64;
    let mut post = message(
        GENERAL,
        Some(&post_id.to_string()),
        post_id,
        ME,
        "Me Myself",
        &format!(
            "<p>Ship <b>today</b> with <span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Ana Ruiz</span></p>\
             <p><img src=\"{img_url}\" alt=\"chart\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"400\" height=\"300\"></p>"
        ),
    );
    post["properties"] = json!({
        "subject": "Release plan",
        "mentions": json!([{"@type": "http://schema.skype.com/Mention", "itemid": 0, "mri": ANA, "mentionType": "person", "displayName": "Ana Ruiz"}]).to_string(),
    });
    world.fake.push("NewMessage", post, false);
    let stored = world.until("the outgoing post to arrive", || world.message(&general, &post_id.to_string()));
    assert_eq!(stored.attachments.len(), 1);

    let edit = Request::Edit {
        conversation: general.clone(),
        message_id: post_id.to_string(),
        text: format!("**Release plan**\nShip **tomorrow** with <@{ANA}|Ana Ruiz>"),
        spans: Vec::new(),
    };
    assert_eq!(world.ask(edit), Response::Ok);
    let put = world.fake.calls("PUT", &format!("/chat/v1/users/ME/conversations/{GENERAL}/messages/{post_id}")).pop().expect("edit PUT");
    let body = put.json();
    let html = body["content"].as_str().unwrap_or("");
    assert!(html.contains("<strong>tomorrow</strong>"), "bold stripped: {html}");
    assert!(html.contains("itemtype=\"http://schema.skype.com/Mention\""), "mention markup stripped: {html}");
    assert!(html.contains("0-weu-d1-editimg/views/imgo"), "inline image stripped: {html}");
    assert!(!html.contains("Release plan"), "subject doubled in body: {html}");
    assert_eq!(body["properties"]["subject"].as_str(), Some("Release plan"));
    let mentions: Vec<Value> = serde_json::from_str(body["properties"]["mentions"].as_str().expect("properties.mentions")).unwrap();
    assert_eq!(mentions.len(), 1);
    assert_eq!((mentions[0]["mri"].as_str(), mentions[0]["displayName"].as_str()), (Some(ANA), Some("Ana Ruiz")));

    let updated = world.message(&general, &post_id.to_string()).unwrap();
    assert_eq!(updated.text, "Release plan\nShip tomorrow with @Ana Ruiz");
    let found = ranges(&updated);
    assert!(found.contains(&("Release plan", &Style::Bold)));
    assert!(found.contains(&("tomorrow", &Style::Bold)));
    assert!(found.contains(&("@Ana Ruiz", &Style::Mention { kind: MentionKind::User, id: ANA.into() })));
    assert_eq!(updated.attachments.len(), 1);
}

#[test]
fn reply_to_my_channel_post_marks_chain_and_channel_unread_while_untouched_post_does_not() {
    let world = World::start();
    world.ready();
    let deploys = key(DEPLOYS);
    world.open(&deploys, 1);
    assert_eq!(world.conversation(&deploys).unwrap().unread, 0);

    // 1. A reply to someone else's post I never touched does not raise unread on the chain or channel.
    let ben_root = "1760000400400";
    let ben_chain = key(&format!("{DEPLOYS};messageid={ben_root}"));
    world.fake.push("NewMessage", message(DEPLOYS, Some(ben_root), 1_760_000_850_000, ANA, "Ana Ruiz", "<p>nice</p>"), false);
    let ben_post = world.until("Ben's post to count the reply", || {
        world.message(&deploys, ben_root).filter(|post| post.thread.as_ref().is_some_and(|thread| thread.replies == 1))
    });
    assert_eq!(ben_post.thread.unwrap().unread, 0);
    assert_eq!(world.conversation(&ben_chain).unwrap().unread, 0);
    assert_eq!(world.conversation(&deploys).unwrap().unread, 0);

    // 2. A reply to a post I wrote raises unread on the chain, the post's thread summary, and the channel.
    let client_id = "1760000999000555";
    world.send(&deploys, client_id, "deploying v514?");
    let mine = world.until("my post to be accepted", || {
        world.messages(&deploys).into_iter().find(|message| message.client_id.as_deref() == Some(client_id) && message.state == SendState::Sent)
    });
    assert_eq!(world.conversation(&deploys).unwrap().unread, 0);
    let my_chain = key(&format!("{DEPLOYS};messageid={}", mine.id));
    let reply_id = 1_760_001_500_000u64;
    world.fake.push("NewMessage", message(DEPLOYS, Some(&mine.id), reply_id, ANA, "Ana Ruiz", "<p>go ahead</p>"), false);
    let my_post = world.until("my post to count the reply", || {
        world.message(&deploys, &mine.id).filter(|post| post.thread.as_ref().is_some_and(|thread| thread.replies == 1))
    });
    assert_eq!(my_post.thread.unwrap().unread, 1);
    assert_eq!(world.conversation(&my_chain).unwrap().unread, 1);
    assert!(world.conversation(&deploys).unwrap().unread >= 1);

    // 3. Marking the chain read clears ThreadSummary.unread on the post.
    assert_eq!(world.ask(Request::MarkRead { conversation: my_chain.clone(), message_id: reply_id.to_string() }), Response::Ok);
    let read_post = world.until("my post thread unread to clear", || {
        world.message(&deploys, &mine.id).filter(|post| post.thread.as_ref().is_some_and(|thread| thread.unread == 0))
    });
    assert_eq!(read_post.thread.unwrap().unread, 0);
}

#[test]
fn chat_pin_hide_and_delete_sync_with_teams() {
    let world = World::start();
    world.ready();

    // Capabilities advertise chat pinning, hiding (close_direct), and deleting (clear_history).
    let caps = match world.ask(Request::Status) {
        Response::Status { accounts, .. } => accounts.into_iter().find(|a| a.key.account == HOME).unwrap().can,
        other => panic!("unexpected: {other:?}"),
    };
    assert!(caps.pin_chats > 0 && caps.close_direct && caps.clear_history);

    let grp1 = key("19:grp1@thread.v2");
    let grp2 = key("19:grp2@thread.v2");
    assert_eq!(world.conversation(&grp1).unwrap().pin, None);

    // Pin grp1 and grp2 in order.
    assert_eq!(
        world.ask(Request::File { conversation: grp1.clone(), pinned: Some(true), archived: None, folder_add: None, folder_remove: None }),
        Response::Ok
    );
    let pinned_call = world.fake.calls("PUT", "19:grp1@thread.v2/properties").pop().unwrap();
    assert_eq!((pinned_call.query.as_str(), pinned_call.json()), ("name=pinned", json!({"pinned": "true"})));
    assert_eq!(world.conversation(&grp1).unwrap().pin, Some(0));

    assert_eq!(
        world.ask(Request::File { conversation: grp2.clone(), pinned: Some(true), archived: None, folder_add: None, folder_remove: None }),
        Response::Ok
    );
    assert_eq!(world.conversation(&grp2).unwrap().pin, Some(1));

    // Unpin grp1.
    assert_eq!(
        world.ask(Request::File { conversation: grp1.clone(), pinned: Some(false), archived: None, folder_add: None, folder_remove: None }),
        Response::Ok
    );
    let unpinned_call = world.fake.calls("PUT", "19:grp1@thread.v2/properties").pop().unwrap();
    assert_eq!((unpinned_call.query.as_str(), unpinned_call.json()), ("name=pinned", json!({"pinned": "false"})));
    assert_eq!(world.conversation(&grp1).unwrap().pin, None);

    // Hide grp1 via CloseDirect; it becomes hidden and writes hidden=true.
    assert_eq!(world.ask(Request::CloseDirect { conversation: grp1.clone() }), Response::Ok);
    let hidden_call = world.fake.calls("PUT", "19:grp1@thread.v2/properties").pop().unwrap();
    assert_eq!((hidden_call.query.as_str(), hidden_call.json()), ("name=hidden", json!({"hidden": "true"})));
    assert!(world.conversation(&grp1).unwrap().hidden);

    // A ConversationUpdate from another device unhides grp1 and pins it.
    world.fake.push(
        "ConversationUpdate",
        json!({"id": "19:grp1@thread.v2", "properties": {"hidden": "false", "pinned": "true"}}),
        false,
    );
    world.until("grp1 unhidden and pinned by push", || {
        world.conversation(&grp1).filter(|row| !row.hidden && row.pin.is_some())
    });

    // Delete grp1 for me only via ClearHistory { delete_chat: true }.
    assert_eq!(
        world.ask(Request::ClearHistory { conversation: grp1.clone(), delete_chat: true, revoke: false }),
        Response::Ok
    );
    assert!(!world.fake.calls("DELETE", "19:grp1@thread.v2").is_empty());
    assert!(world.conversation(&grp1).is_none());

    // Channels cannot be deleted as chats.
    assert!(matches!(
        world.ask(Request::ClearHistory { conversation: key(GENERAL), delete_chat: true, revoke: false }),
        Response::Error { .. }
    ));
}

#[test]
fn link_previews_received_sent_and_opted_out() {
    let world = World::start();
    world.ready();

    let caps = match world.ask(Request::Status) {
        Response::Status { accounts, .. } => accounts.into_iter().find(|a| a.key.account == HOME).unwrap().can,
        other => panic!("unexpected: {other:?}"),
    };
    assert!(caps.suppress_embeds);

    // 1. Received message with web link preview and M365 file link card.
    let chat = key(CHAT_ANA);
    let mut incoming = message(CHAT_ANA, None, 1_760_000_950_000, ANA, "Ana Ruiz", "<p>See https://example.com/doc</p>");
    incoming["properties"]["links"] = json!(json!([
        {
            "url": "https://example.com/doc",
            "title": "Architecture Doc",
            "description": "Design overview",
            "siteName": "Example",
            "previewImage": format!("{}/ams/v1/objects/img-1/views/imgo", world.fake.base),
        },
        {
            "url": "https://contoso.sharepoint.com/:w:/s/Platform/Doc1",
            "fileName": "Spec.docx",
            "fileType": "docx",
            "description": "Shared specification"
        }
    ]).to_string());
    world.fake.push("NewMessage", incoming, false);
    let received = world.until("received message with previews", || world.message(&chat, "1760000950000"));
    assert_eq!(received.previews.len(), 2);
    assert_eq!((received.previews[0].title.as_str(), received.previews[0].site.as_str()), ("Architecture Doc", "Example"));
    assert!(received.previews[0].image.is_some());
    assert_eq!((received.previews[1].title.as_str(), received.previews[1].site.as_str()), ("Spec.docx", "Word · SharePoint"));

    // 2. Sent message with a URL unfurls through Teams' middle tier and attaches properties.links.
    world.send(&chat, "1760000960000001", "Check https://example.com/release");
    let sent = world.until("sent message with preview", || {
        world.messages(&chat).into_iter().find(|m| m.client_id.as_deref() == Some("1760000960000001") && m.state == SendState::Sent)
    });
    assert_eq!(sent.previews.len(), 1);
    assert_eq!(sent.previews[0].title, "TinyClient Release");
    assert_eq!(world.fake.calls("POST", "/v1/urlpreview").len(), 1);
    let posted = world.fake.calls("POST", &format!("{CHAT_ANA}/messages")).pop().unwrap();
    assert!(posted.json()["properties"]["links"].as_str().unwrap_or("").contains("TinyClient Release"));

    // 3. Opting out via suppress_embeds skips unfurl and omits properties.links.
    match world.ask(Request::Send {
        conversation: chat.clone(),
        client_id: "1760000960000002".to_owned(),
        text: "No preview https://example.com/release".to_owned(),
        reply_to: None,
        with: protocol::Outgoing { suppress_embeds: true, ..Default::default() },
    }) {
        Response::Sent { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
    let opted_out = world.until("opted-out message sent", || {
        world.messages(&chat).into_iter().find(|m| m.client_id.as_deref() == Some("1760000960000002") && m.state == SendState::Sent)
    });
    assert!(opted_out.previews.is_empty());
    assert_eq!(world.fake.calls("POST", "/v1/urlpreview").len(), 1, "suppress_embeds must not call urlpreview");
    let posted2 = world.fake.calls("POST", &format!("{CHAT_ANA}/messages")).pop().unwrap();
    assert!(posted2.json()["properties"]["links"].is_null());
}

#[test]
fn notification_levels_read_write_and_live_update() {
    let world = World::start();
    world.ready();

    let general = key(GENERAL);
    let grp1 = key("19:grp1@thread.v2");

    // Unmuted channel with no explicit override starts at All; setting Mentions updates both alerts and channelNotificationSettings.
    let info = match world.ask(Request::ConversationInfo { conversation: general.clone() }) {
        Response::ConversationInfo { info } => info,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(info.notify, protocol::Notify::All);

    assert_eq!(
        world.ask(Request::SetMute { conversation: general.clone(), notify: protocol::Notify::Mentions, until: None }),
        Response::Ok
    );
    let chan_calls = world.fake.calls("PUT", &format!("{GENERAL}/properties"));
    assert!(chan_calls.iter().any(|c| c.query == "name=alerts" && c.json() == json!({"alerts": "false", "mentions": "true"})));
    assert!(chan_calls.iter().any(|c| c.query == "name=channelNotificationSettings" && c.json()["channelNotificationSettings"]["allPosts"] == "off" && c.json()["channelNotificationSettings"]["mentions"] == "bannerAndFeed"));
    assert!(!world.conversation(&general).unwrap().muted);

    // Update channel back to All: writes alerts=true and channelNotificationSettings allPosts=bannerAndFeed.
    assert_eq!(
        world.ask(Request::SetMute { conversation: general.clone(), notify: protocol::Notify::All, until: None }),
        Response::Ok
    );
    let chan_calls = world.fake.calls("PUT", &format!("{GENERAL}/properties"));
    assert!(chan_calls.iter().any(|c| c.query == "name=alerts" && c.json() == json!({"alerts": "true"})));
    assert!(chan_calls.iter().any(|c| c.query == "name=channelNotificationSettings" && c.json()["channelNotificationSettings"]["allPosts"] == "bannerAndFeed"));
    assert!(!world.conversation(&general).unwrap().muted);

    // Update chat to Mentions: writes alerts=false + mentions=true and does NOT mark row muted.
    assert_eq!(
        world.ask(Request::SetMute { conversation: grp1.clone(), notify: protocol::Notify::Mentions, until: None }),
        Response::Ok
    );
    let grp_call = world.fake.calls("PUT", "19:grp1@thread.v2/properties").pop().unwrap();
    assert_eq!((grp_call.query.as_str(), grp_call.json()), ("name=alerts", json!({"alerts": "false", "mentions": "true"})));
    assert!(!world.conversation(&grp1).unwrap().muted);

    // Live ConversationUpdate push muting grp1 reflects in row.muted.
    world.fake.push(
        "ConversationUpdate",
        json!({"id": "19:grp1@thread.v2", "properties": {"alerts": "false"}}),
        false,
    );
    world.until("grp1 muted by ConversationUpdate", || {
        world.conversation(&grp1).filter(|row| row.muted)
    });
}

#[test]
fn scheduled_chat_message_send_edit_cancel_and_channel_refusal() {
    let world = World::start();
    world.ready();

    let caps = match world.ask(Request::Status) {
        Response::Status { accounts, .. } => accounts.into_iter().find(|a| a.key.account == HOME).unwrap().can,
        other => panic!("unexpected: {other:?}"),
    };
    assert!(caps.schedule);

    let chat = key(CHAT_ANA);
    let future_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600;

    // 1. Schedule a message in a chat.
    match world.ask(Request::Send {
        conversation: chat.clone(),
        client_id: "1760000970000001".to_owned(),
        text: "Remind me in an hour".to_owned(),
        reply_to: None,
        with: protocol::Outgoing { schedule: Some(future_ts), ..Default::default() },
    }) {
        Response::Sent { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
    let scheduled = world.until("scheduled message sent", || {
        world.messages(&chat).into_iter().find(|m| m.client_id.as_deref() == Some("1760000970000001") && m.state == SendState::Sent)
    });
    assert!(scheduled.id.starts_with("scheduled:"), "got id {}", scheduled.id);
    assert_eq!(scheduled.sent, future_ts);
    let posted = world.fake.calls("POST", &format!("{CHAT_ANA}/messages")).pop().unwrap();
    assert!(posted.json()["properties"]["scheduledTime"].as_str().is_some_and(|s| s.ends_with('Z')));

    // 2. Edit the scheduled message preserves scheduledTime and targets the raw message id.
    let raw_id = scheduled.id.trim_start_matches("scheduled:").to_owned();
    assert_eq!(
        world.ask(Request::Edit {
            conversation: chat.clone(),
            message_id: scheduled.id.clone(),
            text: "Updated reminder".to_owned(),
            spans: Vec::new(),
        }),
        Response::Ok
    );
    let edited_call = world.fake.calls("PUT", &format!("{CHAT_ANA}/messages/{raw_id}")).pop().unwrap();
    assert!(edited_call.json()["properties"]["scheduledTime"].as_str().is_some_and(|s| s.ends_with('Z')));

    // 3. Cancel (delete) the scheduled message sends DELETE for the raw id and removes it.
    assert_eq!(
        world.ask(Request::Delete {
            conversation: chat.clone(),
            message_id: scheduled.id.clone(),
            scope: protocol::DeleteScope::Everyone,
        }),
        Response::Ok
    );
    assert!(!world.fake.calls("DELETE", &format!("{CHAT_ANA}/messages/{raw_id}")).is_empty());
    assert!(world.message(&chat, &scheduled.id).is_none());

    // 4. Scheduling in a channel is refused cleanly without posting to Teams.
    let general = key(GENERAL);
    let posts_before = world.fake.calls("POST", &format!("{GENERAL}/messages")).len();
    match world.ask(Request::Send {
        conversation: general.clone(),
        client_id: "1760000970000002".to_owned(),
        text: "Channel schedule attempt".to_owned(),
        reply_to: None,
        with: protocol::Outgoing { schedule: Some(future_ts), ..Default::default() },
    }) {
        Response::Sent { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
    let failed = world.until("channel scheduled send fails", || {
        world.messages(&general).into_iter().find(|m| {
            m.client_id.as_deref() == Some("1760000970000002") && matches!(m.state, SendState::Failed { .. })
        })
    });
    assert!(matches!(failed.state, SendState::Failed { ref reason } if reason.contains("channels")));
    assert_eq!(world.fake.calls("POST", &format!("{GENERAL}/messages")).len(), posts_before);
}

#[test]
fn unread_count_from_horizon_and_mark_unread_sync() {
    let world = World::start();
    world.ready();

    let grp1 = key("19:grp1@thread.v2");

    // 1. Set consumption horizon at msg 1 (1759500000100), then open grp1 (8 messages: 100..=800).
    //    Backfill counts the 7 messages past the horizon instead of collapsing to 1.
    world.fake.push(
        "ConversationUpdate",
        json!({
            "id": "19:grp1@thread.v2",
            "properties": {"consumptionhorizon": "1759500000100;1759500000150;1759500000100"},
            "lastMessage": {"id": "1759500000800"}
        }),
        false,
    );
    world.until("grp1 horizon recorded", || {
        world.conversation(&grp1).filter(|row| row.last_read.as_deref() == Some("1759500000100"))
    });
    world.open(&grp1, 8);
    let counted = world.until("grp1 unread count refined to 7", || {
        world.conversation(&grp1).filter(|row| row.unread == 7)
    });
    assert_eq!(counted.unread, 7);

    // 2. Mark read at msg 8, then MarkUnread from msg 5 (1759500000500) -> writes prior msg (1759500000400) to Teams.
    assert_eq!(world.ask(Request::MarkRead { conversation: grp1.clone(), message_id: "1759500000800".into() }), Response::Ok);
    assert_eq!(world.conversation(&grp1).unwrap().unread, 0);
    world.until("MarkRead PUT consumptionhorizon", || {
        world.fake.calls("PUT", "19:grp1@thread.v2/properties").into_iter().find(|c| {
            c.query == "name=consumptionhorizon"
                && c.json()["consumptionhorizon"].as_str().unwrap_or("").starts_with("1759500000800;")
        })
    });
    assert_eq!(world.ask(Request::MarkUnread { conversation: grp1.clone(), message_id: "1759500000500".into() }), Response::Ok);
    let unread_call = world.until("MarkUnread PUT consumptionhorizon", || {
        world.fake.calls("PUT", "19:grp1@thread.v2/properties").into_iter().find(|c| {
            c.query == "name=consumptionhorizon"
                && c.json()["consumptionhorizon"].as_str().unwrap_or("").starts_with("1759500000400;")
        })
    });
    assert!(unread_call.json()["consumptionhorizon"].as_str().unwrap_or("").starts_with("1759500000400;"));

    // 3. Another device moving consumptionhorizon backwards via ConversationUpdate rewinds last_read and unread count
    //    (with a clientTimestamp newer than our local MarkRead write).
    assert_eq!(world.ask(Request::MarkRead { conversation: grp1.clone(), message_id: "1759500000800".into() }), Response::Ok);
    assert_eq!(world.conversation(&grp1).unwrap().unread, 0);
    let newer_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 60_000;
    world.fake.push(
        "ConversationUpdate",
        json!({
            "id": "19:grp1@thread.v2",
            "properties": {"consumptionhorizon": format!("1759500000300;{newer_ts};1759500000300")},
            "lastMessage": {"id": "1759500000800"}
        }),
        false,
    );
    let rewound = world.until("grp1 rewound to 5 unread", || {
        world.conversation(&grp1).filter(|row| row.last_read.as_deref() == Some("1759500000300") && row.unread == 5)
    });
    assert_eq!(rewound.unread, 5);
}

#[test]
fn adaptive_connector_and_hero_cards_render_and_malformed_falls_back() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    world.open(&general, 6);
    let fixtures = world.fake.fixture("cards").expect("cards fixture");

    for name in ["connector_alert", "adaptive_facts_buttons", "hero_card", "malformed_card"] {
        world.fake.push("NewMessage", fixtures[name].clone(), false);
    }
    world.until("all four card messages to arrive", || {
        let msgs = world.messages(&general);
        ["1760000801001", "1760000801002", "1760000801003", "1760000801004"]
            .iter()
            .all(|id| msgs.iter().any(|m| m.id == *id))
            .then_some(())
    });

    // 1. Office 365 connector alert
    let alert = world.message(&general, "1760000801001").unwrap();
    assert_eq!(alert.kind, MessageKind::Normal);
    let alert_card = alert.card.as_deref().expect("connector card");
    assert_eq!(alert_card.title, "High latency on checkout");
    assert_eq!(alert_card.author, "PagerDuty");
    assert_eq!(alert_card.color, Some(0xD93025));
    assert_eq!(alert_card.fields, vec![("Service".into(), "checkout-api".into()), ("P99".into(), "2450 ms".into())]);
    assert!(alert_card.image.is_none(), "external connector image must not be fetched");
    assert!(alert_card.actions.iter().any(|a| a.label == "View Incident" && a.url == "https://pagerduty.example/incidents/904"));
    assert!(alert_card.actions.iter().any(|a| a.label == "Acknowledge · Open in Teams"));
    assert!(alert_card.actions.iter().any(|a| a.label == "Latency chart" && a.url == "https://cdn.pagerduty.example/chart.png"));

    // 2. Adaptive card with facts, table, hosted + external image, OpenUrl + Submit
    let adaptive = world.message(&general, "1760000801002").unwrap();
    assert_eq!(adaptive.kind, MessageKind::Normal);
    let ac = adaptive.card.as_deref().expect("adaptive card");
    assert_eq!(ac.title, "Release v2.4.0 approval needed");
    assert_eq!(ac.fields, vec![("Environment".into(), "Production".into()), ("Commit".into(), "a7fa2e22".into())]);
    assert!(ac.text.contains("Check | Result") && ac.text.contains("Tests | Pass"), "{}", ac.text);
    assert!(ac.image.is_some(), "Teams-hosted image should be registered");
    assert!(ac.actions.iter().any(|a| a.label == "External banner" && a.url == "https://external.example/banner.png"));
    assert!(ac.actions.iter().any(|a| a.label == "Diff" && a.url == "https://github.com/org/repo/compare/v2.3...v2.4"));
    assert!(ac.actions.iter().any(|a| a.label == "Approve · Open in Teams"));

    // 3. Hero card
    let hero = world.message(&general, "1760000801003").unwrap();
    assert_eq!(hero.kind, MessageKind::Normal);
    let hc = hero.card.as_deref().expect("hero card");
    assert_eq!(hc.title, "PR #42: Fix timeouts");
    assert_eq!(hc.author, "tinyclient / main");
    assert_eq!(hc.text, "All checks green.");
    assert!(hc.image.is_none(), "external hero image must not be fetched");
    assert!(hc.actions.iter().any(|a| a.label == "View PR" && a.url == "https://github.com/org/repo/pull/42"));
    assert!(hc.actions.iter().any(|a| a.label == "Merge · Open in Teams"));

    // 4. Malformed card falls back cleanly to Unsupported (never a blank row)
    let broken = world.message(&general, "1760000801004").unwrap();
    assert!(broken.card.is_none());
    assert_eq!(broken.text, "Card");
    assert!(matches!(broken.kind, MessageKind::Unsupported { ref link } if link.contains("1760000801004")));
}

#[test]
fn channel_post_subject_announcements_moderation_and_threads_layout() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    world.open(&general, 6);

    // 1. Account advertises channel_subject capability.
    assert!(world.account(HOME).can.channel_subject);

    // 2. Sending a channel post with a subject emits properties.subject and bolds the first line.
    let client_id = "1760000999000617";
    let req = Request::Send {
        conversation: general.clone(),
        client_id: client_id.to_owned(),
        text: "Deployment window is Friday 10:00 UTC.".to_owned(),
        reply_to: None,
        with: protocol::Outgoing {
            subject: Some("Maintenance window".to_owned()),
            ..protocol::Outgoing::default()
        },
    };
    assert!(matches!(world.ask(req), Response::Sent { .. }));
    let sent = world.until("channel post with subject accepted", || {
        world.messages(&general).into_iter().find(|m| m.client_id.as_deref() == Some(client_id) && m.state == SendState::Sent)
    });
    assert_eq!(sent.text, "Maintenance window\nDeployment window is Friday 10:00 UTC.");
    assert!(ranges(&sent).contains(&("Maintenance window", &Style::Bold)));
    let posted = world.fake.calls("POST", &format!("/chat/v1/users/ME/conversations/{GENERAL}/messages")).pop().unwrap();
    assert_eq!(posted.json()["properties"]["subject"].as_str(), Some("Maintenance window"));

    // 3. Moderated channel 403 refusal surfaces a clear moderation message.
    let mod_id = "1760000999000618";
    world.send(&general, mod_id, "moderated post");
    let refused = world.until("moderated post refusal", || {
        world.message(&general, mod_id).filter(|m| m.state != SendState::Pending)
    });
    assert_eq!(
        refused.state,
        SendState::Failed { reason: "Only channel moderators can start new posts here.".to_owned() }
    );

    // 4. Announcement post extracts headline + subhead, treats banner as decoration, marks row.
    let mut ann = message(GENERAL, Some("1760000810001"), 1_760_000_810_001, ANA, "Ana Ruiz", "<p>All hands at 14:00.</p>");
    ann["properties"] = json!({
        "cardType": "announcement",
        "title": "Q4 Town Hall",
        "subject": "Engineering & Product",
        "bannerColor": "#5B5FC7",
        "bannerImage": "https://external.example/banner.jpg"
    });
    world.fake.push("NewMessage", ann, false);
    let stored_ann = world.until("announcement to arrive", || world.message(&general, "1760000810001"));
    assert!(stored_ann.announcement);
    assert_eq!(stored_ann.kind, MessageKind::Normal);
    assert_eq!(stored_ann.text, "Q4 Town Hall\nEngineering & Product\nAll hands at 14:00.");
    assert!(ranges(&stored_ann).contains(&("Q4 Town Hall", &Style::Bold)));
    assert!(ranges(&stored_ann).contains(&("Engineering & Product", &Style::Italic)));
    assert!(stored_ann.attachments.is_empty(), "announcement banner is decoration only");

    // 5. Threads-layout channel reply using rootMessageId (without ;messageid= in conversationLink)
    // still maps to the post's reply chain.
    let mut threaded_reply = message(GENERAL, None, 1_760_000_810_050, BEN, "Ben Okafor", "<p>Count me in!</p>");
    threaded_reply["rootMessageId"] = json!("1760000810001");
    world.fake.push("NewMessage", threaded_reply, false);
    let ann_with_reply = world.until("announcement thread summary", || {
        world.message(&general, "1760000810001").filter(|m| m.thread.as_ref().is_some_and(|t| t.replies == 1))
    });
    assert_eq!(ann_with_reply.thread.unwrap().replies, 1);
    let ann_chain = key(&format!("{GENERAL};messageid=1760000810001"));
    assert_eq!(world.messages(&ann_chain).last().map(|m| m.text.as_str()), Some("Count me in!"));
}

#[test]
fn forwarded_messages_are_read_with_origin_metadata_and_forwarded_to_multiple_chats() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    let ana = key(CHAT_ANA);
    let grp1 = key("19:grp1@thread.v2");
    world.open(&general, 6);
    world.open(&ana, 3);

    // 1. Account advertises native forward capability.
    assert!(world.account(HOME).can.forward);

    // 2. Receiving a forwarded Teams message maps original sender, conversation, link, note, and body.
    let orig_link = format!("https://teams.microsoft.com/l/message/19%3Aplatform0general%40thread.tacv2/1760000100100?tenantId={HOME}");
    let mut inbound = message(
        CHAT_ANA,
        None,
        1_760_000_820_001,
        ANA,
        "Ana Ruiz",
        &format!(
            "<p>FYI see below</p>\
             <blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Forward\" itemid=\"1760000100100\">\
             <strong itemprop=\"mri\" itemid=\"{BEN}\">Ben Okafor</strong>\
             <span itemprop=\"time\" itemid=\"1760000100100\"></span>\
             <span itemprop=\"conversation\">General</span>\
             <a itemprop=\"sourceLink\" href=\"{orig_link}\"></a>\
             <p itemprop=\"preview\">Deploy is scheduled for 16:00.</p></blockquote>"
        ),
    );
    inbound["properties"] = json!({
        "forwardedMessage": json!({
            "originalMessageId": "1760000100100",
            "originalConversationId": GENERAL,
            "originalConversationTitle": "General",
            "originalSenderDisplayName": "Ben Okafor",
            "originalArrivalTime": 1_760_000_100_100u64,
            "deepLink": orig_link,
        }).to_string()
    });
    world.fake.push("NewMessage", inbound, false);
    let received = world.until("forwarded message to arrive", || world.message(&ana, "1760000820001"));
    assert_eq!(received.text, "FYI see below\nDeploy is scheduled for 16:00.");
    let fwd = received.forwarded.as_deref().expect("Message::forwarded populated");
    assert_eq!(fwd.from_title, "Ben Okafor");
    assert_eq!(fwd.from_space, "General");
    assert_eq!(fwd.link, orig_link);

    // 3. Forwarding a message with a shared file to two chats sends forward markup, optional note,
    // and preserves file references without re-uploading to OneDrive.
    let uploads_before = world.fake.calls("POST", ":/createUploadSession").len();
    let req = Request::Forward {
        from: general.clone(),
        message_ids: vec!["1760000250250".to_owned()],
        to: vec![ana.clone(), grp1.clone()],
        note: "Sharing Q4 numbers".to_owned(),
    };
    let Response::Forwarded { results } = world.ask(req) else {
        panic!("expected Response::Forwarded");
    };
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok), "{results:?}");
    assert_eq!(world.fake.calls("POST", ":/createUploadSession").len(), uploads_before, "forwarded files must not be re-uploaded");

    for target_id in [CHAT_ANA, "19:grp1@thread.v2"] {
        let sent = world
            .fake
            .calls("POST", &format!("/chat/v1/users/ME/conversations/{target_id}/messages"))
            .pop()
            .expect("forwarded post reached target");
        let body = sent.json();
        let html = body["content"].as_str().unwrap_or("");
        assert!(html.contains("Sharing Q4 numbers"), "{html}");
        assert!(html.contains("http://schema.skype.com/Forward"), "{html}");
        assert!(html.contains("Ana Ruiz"), "{html}");
        assert!(html.contains("Numbers are in"), "{html}");
        assert!(body["properties"]["forwardedMessage"].as_str().is_some(), "forwardedMessage property present");
        let files: Vec<Value> = serde_json::from_str(body["properties"]["files"].as_str().expect("preserved files property")).unwrap();
        assert_eq!(files[0]["fileName"].as_str(), Some("Q4 plan.xlsx"));
    }
}

#[test]
fn important_and_urgent_messages_are_parsed_and_sent_with_properties_importance() {
    let world = World::start();
    world.ready();
    let ana = key(CHAT_ANA);
    world.open(&ana, 3);

    // 1. Account advertises importance capability.
    assert!(world.account(HOME).can.importance);

    // 2. Receive Normal, Important (`high`), and Urgent (`urgent`) fixtures.
    let fixtures = world.fake.fixture("importance").expect("importance fixture");
    for kind in ["normal", "important", "urgent"] {
        world.fake.push("NewMessage", fixtures[kind].clone(), false);
    }
    world.until("all three importance fixtures", || {
        let msgs = world.messages(&ana);
        ["1760000830001", "1760000830002", "1760000830003"]
            .iter()
            .all(|id| msgs.iter().any(|m| m.id == *id))
            .then_some(())
    });
    assert_eq!(world.message(&ana, "1760000830001").unwrap().importance, protocol::Importance::Normal);
    assert_eq!(world.message(&ana, "1760000830002").unwrap().importance, protocol::Importance::Important);
    assert_eq!(world.message(&ana, "1760000830003").unwrap().importance, protocol::Importance::Urgent);

    // 3. Send Important and Urgent messages and verify properties.importance on the wire.
    let send_with_imp = |client_id: &str, text: &str, importance: protocol::Importance| {
        let req = Request::Send {
            conversation: ana.clone(),
            client_id: client_id.to_owned(),
            text: text.to_owned(),
            reply_to: None,
            with: protocol::Outgoing { importance, ..protocol::Outgoing::default() },
        };
        assert!(matches!(world.ask(req), Response::Sent { .. }));
        world.until("sent message accepted", || {
            world
                .messages(&ana)
                .into_iter()
                .find(|m| m.client_id.as_deref() == Some(client_id) && m.state == SendState::Sent)
        })
    };

    let imp_msg = send_with_imp("1760000999000627", "Review before EOD", protocol::Importance::Important);
    assert_eq!(imp_msg.importance, protocol::Importance::Important);
    let imp_call = world
        .fake
        .calls("POST", &format!("/chat/v1/users/ME/conversations/{CHAT_ANA}/messages"))
        .into_iter()
        .find(|c| c.json()["clientmessageid"] == "1760000999000627")
        .expect("important POST");
    assert_eq!(imp_call.json()["properties"]["importance"].as_str(), Some("high"));

    let urg_msg = send_with_imp("1760000999000628", "Site down!", protocol::Importance::Urgent);
    assert_eq!(urg_msg.importance, protocol::Importance::Urgent);
    let urg_call = world
        .fake
        .calls("POST", &format!("/chat/v1/users/ME/conversations/{CHAT_ANA}/messages"))
        .into_iter()
        .find(|c| c.json()["clientmessageid"] == "1760000999000628")
        .expect("urgent POST");
    assert_eq!(urg_call.json()["properties"]["importance"].as_str(), Some("urgent"));
}

#[test]
fn signing_out_removes_push_registration_for_signed_out_tenant() {
    let world = World::start();
    world.ready();

    // Wait until trouter registered the endpoint id.
    let reg_id = world.until("initial push registration", || {
        world
            .fake
            .calls("POST", "/registrar/prod/V2/registrations")
            .into_iter()
            .find_map(|c| c.json()["registrationId"].as_str().map(str::to_owned))
    });
    assert!(!reg_id.is_empty());

    // Sign out of the home tenant; only that tenant's push registration should be deleted.
    // The answer is status, as every other network's sign-out is: the home account is gone and
    // the guest tenant stays Expired with the refusal it already had at connect. No token is
    // minted on the way to the registrar DELETE.
    let minted = world.fake.calls("POST", "/oauth2/v2.0/token").len();
    let answer = world.ask(Request::SignOut {
        account: protocol::AccountKey {
            network: Network::Teams,
            account: HOME.to_owned(),
        },
        erase: false,
    });
    let Response::Status { accounts, .. } = answer else {
        panic!("sign-out returns status: {answer:?}");
    };
    assert!(accounts.iter().all(|status| status.key.account != HOME), "home tenant is signed out: {accounts:?}");
    assert!(
        accounts.iter().any(|status| status.key.account == GUEST && status.link == Link::Expired),
        "the other tenant is left as it was: {accounts:?}"
    );
    assert_eq!(world.fake.calls("POST", "/oauth2/v2.0/token").len(), minted, "sign-out must not refresh a token");

    let delete_path = format!("/registrar/prod/V2/registrations/{reg_id}");
    let deleted = world.until("registrar DELETE on sign-out", || {
        world.fake.calls("DELETE", &delete_path).into_iter().next()
    });
    assert!(
        deleted.headers.get("authorization").is_some_and(|v| v.starts_with("Bearer at.spaces.")),
        "DELETE carries spaces bearer: {:?}",
        deleted.headers
    );
    assert!(
        deleted.headers.contains_key("x-skypetoken"),
        "DELETE carries X-Skypetoken: {:?}",
        deleted.headers
    );
}

#[test]
fn sending_files_in_channels_uses_sharepoint_folder_and_refused_external_invite_fails_clearly() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    world.open(&general, 2);

    // Account advertises file upload capabilities.
    assert!(world.account(HOME).can.max_file_bytes > 0);
    assert!(world.account(HOME).can.max_files > 0);

    let staged = std::path::absolute(format!("/tmp/tinyclient-teams-chfile-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&staged).unwrap();
    let report = staged.join("report.txt");
    std::fs::write(&report, b"quarterly report").unwrap();

    // 1. Channel upload goes to the channel's SharePoint filesFolder (no /invite, no /createLink).
    let with = protocol::Outgoing {
        files: vec![protocol::OutgoingFile {
            path: report.to_string_lossy().into_owned(),
            name: String::new(),
            spoiler: false,
            as_file: false,
            original: false,
            view_once: false,
            media: None,
        }],
        ..Default::default()
    };
    let sent = world.ask(Request::Send {
        conversation: general.clone(),
        client_id: "88002".into(),
        text: "Channel report".into(),
        reply_to: None,
        with,
    });
    assert!(matches!(sent, Response::Sent { .. }), "{sent:?}");
    world.until("channel file message sent", || {
        world
            .messages(&general)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("88002") && m.state == SendState::Sent)
    });
    assert!(!world.fake.calls("GET", "/filesFolder").is_empty(), "queried channel filesFolder");
    assert!(
        !world.fake.calls("POST", "/v1.0/drives/DRIVE_TEAM1/items/FOLDER_CH1:/report.txt:/createUploadSession").is_empty(),
        "uploaded to channel SharePoint drive"
    );
    assert!(world.fake.calls("POST", "/invite").is_empty(), "channels do not call /invite");
    assert!(world.fake.calls("POST", "/createLink").is_empty(), "must never call /createLink");
    let ch_post = world
        .fake
        .calls("POST", &format!("/chat/v1/users/ME/conversations/{GENERAL}/messages"))
        .into_iter()
        .find(|c| c.json()["clientmessageid"] == "88002")
        .expect("channel message POST");
    let files_prop: Vec<Value> = serde_json::from_str(ch_post.json()["properties"]["files"].as_str().unwrap()).unwrap();
    assert_eq!(files_prop[0]["permissionScope"].as_str(), Some("channel"));
    assert_eq!(files_prop[0]["fileChicletState"]["serviceName"].as_str(), Some("sharepoint"));

    // 2. Chat with a guest whose /invite returns 403 surfaces a clear policy refusal and does not post.
    let Response::Conversation { conversation: dee_row } = world.ask(Request::StartDirect { person: key(DEE) }) else {
        panic!("expected Response::Conversation for Dee");
    };
    let dee_chat = dee_row.key;
    let with_guest = protocol::Outgoing {
        files: vec![protocol::OutgoingFile {
            path: report.to_string_lossy().into_owned(),
            name: String::new(),
            spoiler: false,
            as_file: false,
            original: false,
            view_once: false,
            media: None,
        }],
        ..Default::default()
    };
    let guest_sent = world.ask(Request::Send {
        conversation: dee_chat.clone(),
        client_id: "88003".into(),
        text: "For Dee".into(),
        reply_to: None,
        with: with_guest,
    });
    assert!(matches!(guest_sent, Response::Sent { .. }), "{guest_sent:?}");
    let failed = world.until("guest file send failed", || {
        world
            .messages(&dee_chat)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("88003") && matches!(m.state, SendState::Failed { .. }))
    });
    let SendState::Failed { reason: err_msg } = failed.state else {
        panic!("expected SendState::Failed");
    };
    assert!(
        err_msg.contains("external") || err_msg.contains("guest"),
        "expected clear guest/external policy error, got: {err_msg:?}"
    );
    assert!(
        world
            .fake
            .calls("POST", &format!("/chat/v1/users/ME/conversations/{CHAT_DEE}/messages"))
            .into_iter()
            .all(|c| c.json()["clientmessageid"] != "88003"),
        "failed invite must not post message"
    );
    let _ = std::fs::remove_dir_all(&staged);
}

#[test]
fn out_of_office_profile_fetches_presence_and_mailbox_settings_round_trip() {
    let world = World::start();
    world.ready();
    assert!(world.account(HOME).can.out_of_office);

    // 1. Opening Dee's profile directly (with NO prior Request::Watch) fetches presence so OOO appears.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key(DEE), space: None }) else {
        panic!("expected Response::Profile for Dee");
    };
    assert_eq!(profile.away, "Back Monday, ask Ben");
    assert_eq!(profile.away_until, Some(1_792_396_800));

    // 2. Reading our own automatic replies queries Graph mailboxSettings.
    let account = protocol::AccountKey {
        network: Network::Teams,
        account: HOME.to_owned(),
    };
    let Response::OutOfOffice { ooo } = world.ask(Request::GetOutOfOffice { account: account.clone() }) else {
        panic!("expected Response::OutOfOffice");
    };
    assert!(!ooo.enabled);

    // 3. Updating automatic replies sends PATCH /v1.0/me/mailboxSettings and updates own profile OOO.
    let wanted = protocol::OutOfOffice {
        enabled: true,
        message: "Out at summit, back Thursday".to_owned(),
        start: Some(1_792_396_800),
        end: Some(1_792_656_000),
        send_external: true,
        external_message: "External: back Thursday".to_owned(),
    };
    let Response::OutOfOffice { ooo: saved } = world.ask(Request::SetOutOfOffice {
        account: account.clone(),
        ooo: wanted.clone(),
    }) else {
        panic!("expected Response::OutOfOffice after set");
    };
    assert_eq!(saved, wanted);
    let patch = world
        .fake
        .calls("PATCH", "/graph/v1.0/me/mailboxSettings")
        .pop()
        .expect("PATCH mailboxSettings reached Graph");
    let sent = patch.json();
    assert_eq!(sent["automaticRepliesSetting"]["status"].as_str(), Some("scheduled"));
    assert_eq!(sent["automaticRepliesSetting"]["internalReplyMessage"].as_str(), Some("Out at summit, back Thursday"));
    assert_eq!(sent["automaticRepliesSetting"]["externalAudience"].as_str(), Some("all"));
    assert_eq!(sent["automaticRepliesSetting"]["externalReplyMessage"].as_str(), Some("External: back Thursday"));

    // Reading back confirms Graph persisted it.
    let Response::OutOfOffice { ooo: reread } = world.ask(Request::GetOutOfOffice { account }) else {
        panic!("expected Response::OutOfOffice on re-read");
    };
    assert_eq!(reread, wanted);
}

#[test]
fn rich_org_card_populates_phones_office_local_time_org_chart_and_refreshes_avatar() {
    let world = World::start();
    world.ready();

    // 1. Ana's profile pulls Graph user details, manager, and direct reports.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key(ANA), space: None }) else {
        panic!("expected Response::Profile for Ana");
    };
    assert_eq!(profile.office, "Lisbon — Floor 4");
    assert_eq!(profile.pronouns, "she/her");
    assert_eq!(profile.time_zone, "GMT Standard Time");
    assert_eq!(profile.utc_offset, Some(0));
    let phones: Vec<(&str, bool)> = profile
        .contacts
        .iter()
        .filter(|c| c.kind == protocol::ContactKind::Phone)
        .map(|c| (c.value.as_str(), c.masked))
        .collect();
    assert_eq!(phones, [("+351 21 555 0100", true), ("+351 91 555 0199", true)]);
    let manager = profile.manager.as_ref().expect("manager populated");
    assert_eq!(manager.key, key(BEN));
    assert_eq!(manager.name, "Ben Okafor");
    assert_eq!(profile.reports.len(), 1);
    assert_eq!(profile.reports[0].key, key("8:orgid:00000000-0000-4000-8000-0000000000c3"));
    assert_eq!(profile.reports[0].name, "Cy Tran");

    // 2. Guest whose Graph user endpoint returns 403 still returns short-profile fields cleanly.
    let Response::Profile { profile: dee } = world.ask(Request::Profile { person: key(DEE), space: None }) else {
        panic!("expected Response::Profile for guest Dee");
    };
    assert_eq!(dee.title, "Product Manager");
    assert_eq!(dee.office, "Lisbon");
    assert!(dee.manager.is_none());
}

#[test]
fn polls_praise_and_approvals_cards_render_vote_create_and_act() {
    let world = World::start();
    world.ready();
    let ana = key(CHAT_ANA);
    world.open(&ana, 3);

    // 1. Account advertises poll capability.
    let poll_can = world.account(HOME).can.polls.expect("Teams advertises polls");
    assert!(poll_can.max_options >= 10 && poll_can.multiple && poll_can.anonymous);

    // 2. Push incoming Poll, Praise, and Approval Adaptive Cards.
    let mut poll_msg = message(CHAT_ANA, None, 1_760_000_860_001, ANA, "Ana Ruiz", "");
    poll_msg["messagetype"] = "RichText/Html".into();
    poll_msg["properties"] = json!({
        "cards": json!([{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "appId": "ef56c0de-36fc-4ef8-b417-3d82ba9d073c",
            "content": json!({
                "type": "AdaptiveCard",
                "version": "1.5",
                "body": [
                    {"type": "TextBlock", "text": "Lunch today?", "weight": "bolder", "size": "medium"},
                    {
                        "type": "Input.ChoiceSet",
                        "id": "pollChoice",
                        "isMultiSelect": false,
                        "choices": [
                            {"title": "Ramen", "value": "opt_ramen"},
                            {"title": "Tacos", "value": "opt_tacos"}
                        ]
                    }
                ],
                "actions": [{"type": "Action.Submit", "title": "Submit vote", "data": {"pollId": "p-99", "action": "vote"}}]
            }).to_string()
        }]).to_string()
    });

    let mut praise_msg = message(CHAT_ANA, None, 1_760_000_860_002, ANA, "Ana Ruiz", "");
    praise_msg["messagetype"] = "RichText/Html".into();
    praise_msg["properties"] = json!({
        "cards": json!([{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "appId": "a6b63365-31a4-4f43-92ec-710b71557af9",
            "content": json!({
                "type": "AdaptiveCard",
                "version": "1.5",
                "body": [
                    {"type": "TextBlock", "text": "Ana sent praise to Ben Okafor"},
                    {"type": "TextBlock", "text": "Awesome", "weight": "bolder", "size": "large"},
                    {"type": "TextBlock", "text": "Great launch!"}
                ]
            }).to_string()
        }]).to_string()
    });

    let mut approval_msg = message(CHAT_ANA, None, 1_760_000_860_003, ANA, "Ana Ruiz", "");
    approval_msg["messagetype"] = "RichText/Html".into();
    approval_msg["properties"] = json!({
        "cards": json!([{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "appId": "7c316234-ded0-4f95-8a83-8453d0876592",
            "content": json!({
                "type": "AdaptiveCard",
                "version": "1.5",
                "body": [
                    {"type": "TextBlock", "text": "Deploy hotfix #12", "weight": "bolder", "size": "medium"},
                    {"type": "FactSet", "facts": [
                        {"title": "Requester", "value": "Ana Ruiz"},
                        {"title": "Status", "value": "Requested"}
                    ]}
                ],
                "actions": [
                    {"type": "Action.Submit", "id": "approve", "title": "Approve", "data": {"approvalId": "a-12", "decision": "Approve"}},
                    {"type": "Action.Submit", "id": "reject", "title": "Reject", "data": {"approvalId": "a-12", "decision": "Reject"}}
                ]
            }).to_string()
        }]).to_string()
    });

    for m in [poll_msg, praise_msg, approval_msg] {
        world.fake.push("NewMessage", m, false);
    }
    world.until("poll, praise, and approval messages stored", || {
        let msgs = world.messages(&ana);
        ["1760000860001", "1760000860002", "1760000860003"]
            .iter()
            .all(|id| msgs.iter().any(|m| m.id == *id))
            .then_some(())
    });

    let p = world.message(&ana, "1760000860001").unwrap();
    let poll = p.poll.as_ref().expect("native poll on message");
    assert_eq!(poll.question, "Lunch today?");
    assert_eq!(poll.choices.len(), 2);

    let pr = world.message(&ana, "1760000860002").unwrap();
    let pr_card = pr.card.as_ref().expect("praise card");
    assert_eq!(pr_card.author, "Praise");
    assert_eq!(pr_card.title, "Awesome");
    assert_eq!(pr_card.fields, vec![("To".into(), "Ben Okafor".into())]);

    let ap = world.message(&ana, "1760000860003").unwrap();
    let ap_card = ap.card.as_ref().expect("approval card");
    assert_eq!(ap_card.author, "Approval");
    assert_eq!(ap_card.title, "Deploy hotfix #12");
    assert_eq!(ap.controls.len(), 2);
    assert_eq!(ap.controls[0].label, "Approve");
    assert_eq!(ap.controls[1].label, "Reject");

    // 3. Vote on the poll via Request::Poll -> hits /invoke and updates local mine.
    let vote_res = world.ask(Request::Poll {
        conversation: ana.clone(),
        message_id: "1760000860001".to_owned(),
        votes: Some(vec![1]),
    });
    assert!(matches!(vote_res, Response::Ok), "{vote_res:?}");
    let vote_call = world
        .fake
        .calls("POST", "/messages/1760000860001/invoke")
        .pop()
        .expect("poll vote invoke reached Teams");
    assert_eq!(vote_call.json()["value"]["action"]["data"]["pollChoice"].as_str(), Some("opt_tacos"));
    let updated_poll = world.message(&ana, "1760000860001").unwrap().poll.unwrap();
    assert_eq!(updated_poll.mine, vec![1]);
    assert_eq!(updated_poll.choices[1].count, 1);

    // 4. Approve via Request::MessageAction -> hits /invoke with approval data.
    let act_res = world.ask(Request::MessageAction {
        conversation: ana.clone(),
        message_id: "1760000860003".to_owned(),
        block_id: ap.controls[0].block_id.clone(),
        action_id: ap.controls[0].action_id.clone(),
        element_type: ap.controls[0].element_type.clone(),
        value: ap.controls[0].value.clone(),
        values: Vec::new(),
    });
    assert!(matches!(act_res, Response::Ok), "{act_res:?}");
    let apr_call = world
        .fake
        .calls("POST", "/messages/1760000860003/invoke")
        .pop()
        .expect("approval invoke reached Teams");
    assert_eq!(apr_call.json()["value"]["action"]["data"]["decision"].as_str(), Some("Approve"));

    // 5. Create a new poll via Request::Send with Outgoing::poll.
    let created = world.ask(Request::Send {
        conversation: ana.clone(),
        client_id: "1760000999000650".to_owned(),
        text: String::new(),
        reply_to: None,
        with: protocol::Outgoing {
            poll: Some(protocol::PollDraft {
                question: "Release day?".to_owned(),
                options: vec!["Tuesday".to_owned(), "Wednesday".to_owned()],
                multiple: false,
                duration_secs: None,
                anonymous: true,
            }),
            ..protocol::Outgoing::default()
        },
    });
    assert!(matches!(created, Response::Sent { .. }), "{created:?}");
    let sent_poll = world.until("created poll stored", || {
        world
            .messages(&ana)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("1760000999000650") && m.state == SendState::Sent)
    });
    let sp = sent_poll.poll.as_ref().expect("created message carries native poll");
    assert_eq!(sp.question, "Release day?");
    assert_eq!(sp.choices.len(), 2);
    assert!(sp.anonymous);
}

#[test]
fn active_presence_heartbeat_privacy_toggle_timed_status_and_reset() {
    let world = World::start();
    world.ready();

    let account = protocol::AccountKey {
        network: Network::Teams,
        account: HOME.to_owned(),
    };

    // 1. Focused + active Watch triggers trouter `user.activity` = "active".
    let _ = world.ask(Request::Watch(Watching {
        focused: true,
        active: true,
        ..Watching::default()
    }));
    world.until("user.activity active frame", || {
        world
            .fake
            .heard
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.contains("\"user.activity\"") && f.contains("\"active\""))
            .then_some(())
    });

    // 2. Unfocused / idle transitions to `user.activity` = "inactive".
    let _ = world.ask(Request::Watch(Watching {
        focused: false,
        active: false,
        ..Watching::default()
    }));
    world.until("user.activity inactive frame", || {
        world
            .fake
            .heard
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.contains("\"user.activity\"") && f.contains("\"inactive\""))
            .then_some(())
    });

    // 3. Turning off Privacy `presence_activity` suppresses future "active" heartbeats.
    let scope = protocol::settings::account_scope(&account);
    assert_eq!(
        world.ask(Request::Set {
            scope,
            key: protocol::settings::PRESENCE_ACTIVITY.to_owned(),
            value: Some("off".to_owned()),
        }),
        Response::Ok
    );
    let active_before = world
        .fake
        .heard
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f.contains("\"user.activity\"") && f.contains("\"active\""))
        .count();
    let _ = world.ask(Request::Watch(Watching {
        focused: true,
        active: true,
        ..Watching::default()
    }));
    sleep(Duration::from_millis(2500));
    let active_after = world
        .fake
        .heard
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f.contains("\"user.activity\"") && f.contains("\"active\""))
        .count();
    assert_eq!(active_after, active_before, "privacy opt-out must suppress active heartbeats");

    // 4. Timed forced availability sends `expiry` to forceavailability.
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Busy,
            status: Some("Deep work".to_owned()),
            status_emoji: None,
            clear_at: Some(1_800_000_000),
        }),
        Response::Ok
    );
    let put = world
        .fake
        .calls("PUT", "/presence/v1/me/forceavailability/")
        .pop()
        .expect("timed forceavailability PUT");
    assert_eq!(put.json()["availability"].as_str(), Some("Busy"));
    assert_eq!(put.json()["expiry"].as_str(), Some("2027-01-15T08:00:00.000Z"));
    let note = world
        .fake
        .calls("PUT", "/presence/v1/me/publishnote")
        .pop()
        .expect("timed publishnote PUT");
    assert_eq!(note.json()["expiry"].as_str(), Some("2027-01-15T08:00:00.000Z"));

    // 5. Reset status (`Presence::Unknown`, `status: None`) sends DELETE /v1/me/forceavailability/.
    assert_eq!(
        world.ask(Request::SetPresence {
            account,
            presence: Presence::Unknown,
            status: None,
            status_emoji: None,
            clear_at: None,
        }),
        Response::Ok
    );
    assert!(
        !world.fake.calls("DELETE", "/presence/v1/me/forceavailability/").is_empty(),
        "Reset status must DELETE /v1/me/forceavailability/"
    );
}

#[test]
fn team_tags_fetch_mentions_me_outgoing_wire_format_and_group_chat() {
    let world = World::start();
    world.ready();
    let general = key(GENERAL);
    world.open(&general, 2);

    // 1. SpaceTags and SpaceInfo expose the team's tags with member counts and `mine`.
    let Response::SpaceTags { tags, .. } = world.ask(Request::SpaceTags { space: general.clone() }) else {
        panic!("expected Response::SpaceTags");
    };
    assert_eq!(tags.len(), 2);
    let design = tags.iter().find(|t| t.id == "tag-design").expect("Design tag");
    let oncall = tags.iter().find(|t| t.id == "tag-oncall").expect("On-Call tag");
    assert!(!design.mine && design.member_count == 1);
    assert!(oncall.mine && oncall.member_count == 2);
    assert_eq!(oncall.members.len(), 2);

    let Response::SpaceInfo { info } = world.ask(Request::SpaceInfo { space: general.clone() }) else {
        panic!("expected Response::SpaceInfo");
    };
    assert_eq!(info.tags.len(), 2);

    // 2. Incoming message mentioning our tag sets `mentions_me = true`; another tag keeps it false.
    let mut ping_mine = message(GENERAL, Some("1760000880001"), 1_760_000_880_001, ANA, "Ana Ruiz", "<p><at id=\"0\">On-Call</at> alert!</p>");
    ping_mine["properties"] = json!({
        "mentions": json!([{"id": 0, "mri": "tag-oncall", "mentionType": "tag", "displayName": "On-Call"}]).to_string()
    });
    let mut ping_other = message(GENERAL, Some("1760000880002"), 1_760_000_880_002, ANA, "Ana Ruiz", "<p><at id=\"0\">Design</at> fyi</p>");
    ping_other["properties"] = json!({
        "mentions": json!([{"id": 0, "mri": "tag-design", "mentionType": "tag", "displayName": "Design"}]).to_string()
    });
    world.fake.push("NewMessage", ping_mine, false);
    world.fake.push("NewMessage", ping_other, false);
    world.until("both tag mention messages stored", || {
        let msgs = world.messages(&general);
        ["1760000880001", "1760000880002"]
            .iter()
            .all(|id| msgs.iter().any(|m| m.id == *id))
            .then_some(())
    });
    let mine_msg = world.message(&general, "1760000880001").unwrap();
    assert!(mine_msg.mentions_me, "tag we belong to must set mentions_me");
    let other_msg = world.message(&general, "1760000880002").unwrap();
    assert!(!other_msg.mentions_me, "tag we do not belong to must not set mentions_me");

    // 3. Outgoing message with `<@&tag-oncall|On-Call>` posts `mentionType: "tag"` on the wire.
    let sent = world.ask(Request::Send {
        conversation: general.clone(),
        client_id: "1760000999000667".to_owned(),
        text: "Paging <@&tag-oncall|On-Call>".to_owned(),
        reply_to: None,
        with: protocol::Outgoing::default(),
    });
    assert!(matches!(sent, Response::Sent { .. }), "{sent:?}");
    world.until("outgoing tag mention accepted", || {
        world
            .messages(&general)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("1760000999000667") && m.state == SendState::Sent)
    });
    let post = world
        .fake
        .calls("POST", &format!("/chat/v1/users/ME/conversations/{GENERAL}/messages"))
        .into_iter()
        .find(|c| c.json()["clientmessageid"] == "1760000999000667")
        .expect("tag mention POST");
    let mentions: Vec<Value> = serde_json::from_str(post.json()["properties"]["mentions"].as_str().unwrap()).unwrap();
    assert_eq!(mentions[0]["mentionType"].as_str(), Some("tag"));
    assert_eq!(mentions[0]["mri"].as_str(), Some("tag-oncall"));

    // 4. Starting a group chat from a tag's members creates the chat with the tag's name as topic.
    assert!(world.account(HOME).can.groups > 0);
    let Response::Conversation { conversation: group_chat } = world.ask(Request::StartGroup {
        account: protocol::AccountKey {
            network: Network::Teams,
            account: HOME.to_owned(),
        },
        people: vec![key(ANA), key(BEN)],
        title: Some("On-Call".to_owned()),
    }) else {
        panic!("expected Response::Conversation from StartGroup");
    };
    assert_eq!(group_chat.key.id, "19:tagchat@thread.v2");
    assert_eq!(group_chat.title, "On-Call");
}

#[test]
fn activity_feed_lists_mentions_replies_reactions_missed_calls_and_marks_read() {
    let world = World::start();
    world.ready();

    // 1. Account advertises activity_feed capability.
    assert!(world.account(HOME).can.activity_feed);

    // 2. Fetch full activity feed from 48:notifications.
    let account = protocol::AccountKey { network: Network::Teams, account: HOME.to_owned() };
    let Response::ActivityFeed { items, .. } = world.ask(Request::ActivityFeed {
        account: account.clone(),
        kind: None,
        cursor: None,
        limit: 50,
    }) else {
        panic!("expected Response::ActivityFeed");
    };
    assert_eq!(items.len(), 6, "expected all 6 activity items: {items:?}");

    let mention = items.iter().find(|i| i.id == "1760000900001").expect("mention item");
    assert_eq!(mention.kind, protocol::ActivityItemKind::Mention);
    assert_eq!(mention.conversation, key("19:grp1@thread.v2"));
    assert_eq!(mention.message_id, "1759600000000");
    assert!(mention.unread);

    let reaction = items.iter().find(|i| i.id == "1760000900003").expect("reaction item");
    assert_eq!(reaction.kind, protocol::ActivityItemKind::Reaction);
    assert_eq!(reaction.emoji, "👍");
    assert_eq!(reaction.text, "Shipped the fix");

    let reply = items.iter().find(|i| i.id == "1760000900004").expect("reply item");
    assert_eq!(reply.kind, protocol::ActivityItemKind::ThreadReply);
    assert_eq!(reply.conversation, key(GENERAL));
    assert_eq!(reply.thread_id.as_deref(), Some("19:platform0general@thread.tacv2;messageid=1760000200200"));
    assert_eq!(reply.message_id, "1760000210210");
    assert_eq!(reply.actor, "Ana Ruiz");

    let missed = items.iter().find(|i| i.id == "1760000900005").expect("missed call item");
    assert_eq!(missed.kind, protocol::ActivityItemKind::Other);
    assert_eq!(missed.text, "Missed call");

    let added = items.iter().find(|i| i.id == "1760000900006").expect("added-to-team item");
    assert_eq!(added.kind, protocol::ActivityItemKind::Invite);
    assert_eq!(added.text, "Added you to Design");

    // 3. Kind filter narrows the returned items.
    let Response::ActivityFeed { items: reactions_only, .. } = world.ask(Request::ActivityFeed {
        account: account.clone(),
        kind: Some(protocol::ActivityItemKind::Reaction),
        cursor: None,
        limit: 50,
    }) else {
        panic!("expected filtered Response::ActivityFeed");
    };
    assert_eq!(reactions_only.len(), 1);
    assert_eq!(reactions_only[0].id, "1760000900003");

    // 4. Marking a single item read sends PUT isRead and clears unread on refetch.
    assert_eq!(
        world.ask(Request::ActivityMarkRead {
            account: account.clone(),
            item_id: Some("1760000900001".to_owned()),
        }),
        Response::Ok
    );
    let mark_call = world
        .fake
        .calls("PUT", "/48:notifications/messages/1760000900001/properties")
        .pop()
        .expect("PUT isRead for activity item");
    assert_eq!(mark_call.query, "name=isRead");

    let Response::ActivityFeed { items: after_one, .. } = world.ask(Request::ActivityFeed {
        account: account.clone(),
        kind: None,
        cursor: None,
        limit: 50,
    }) else {
        panic!("expected Response::ActivityFeed after mark-one");
    };
    assert!(!after_one.iter().find(|i| i.id == "1760000900001").unwrap().unread);
    assert!(after_one.iter().find(|i| i.id == "1760000900004").unwrap().unread);

    // 5. Marking all read advances 48:notifications consumptionhorizon and clears remaining unread.
    assert_eq!(
        world.ask(Request::ActivityMarkRead { account: account.clone(), item_id: None }),
        Response::Ok
    );
    assert!(
        world
            .fake
            .calls("PUT", "/48:notifications/properties")
            .iter()
            .any(|c| c.query == "name=consumptionhorizon"),
        "expected PUT consumptionhorizon on 48:notifications"
    );
    let Response::ActivityFeed { items: after_all, .. } = world.ask(Request::ActivityFeed {
        account,
        kind: None,
        cursor: None,
        limit: 50,
    }) else {
        panic!("expected Response::ActivityFeed after mark-all");
    };
    assert!(after_all.iter().all(|i| !i.unread), "all activity items should be read");
}

#[test]
fn inline_pasted_images_upload_to_ams_and_send_as_richtext_html() {
    let world = World::start();
    world.ready();
    let ana = key(CHAT_ANA);
    world.open(&ana, 3);

    // 1. Account advertises attachments. Documents stay on the member-scoped OneDrive path
    // (#658), so images_only stays off; a pasted image still goes to AMS.
    let can = world.account(HOME).can;
    assert!(can.max_file_bytes > 0, "Teams should allow image attachments");
    assert!(can.max_files > 0);
    assert!(!can.images_only, "Teams file upload is not image-only");

    // 2. Stage a valid PNG screenshot (with a real IHDR header for 320x200).
    let staged = std::path::absolute(format!("/tmp/tinyclient-teams-ams-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&staged).unwrap();
    let img_path = staged.join("screenshot.png");
    let mut png_bytes = Vec::from(&b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..]);
    png_bytes.extend_from_slice(&320u32.to_be_bytes());
    png_bytes.extend_from_slice(&200u32.to_be_bytes());
    png_bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    std::fs::write(&img_path, &png_bytes).unwrap();

    // 3. Send the pasted PNG screenshot with caption text.
    let client_id = "1760000999000647";
    let sent_resp = world.ask(Request::Send {
        conversation: ana.clone(),
        client_id: client_id.into(),
        text: "See attached screenshot".into(),
        reply_to: None,
        with: protocol::Outgoing {
            files: vec![protocol::OutgoingFile {
                path: img_path.to_string_lossy().into_owned(),
                name: "screenshot.png".into(),
                spoiler: false,
                as_file: false,
                original: false,
                view_once: false,
                media: None,
            }],
            ..Default::default()
        },
    });
    assert!(matches!(sent_resp, Response::Sent { .. }), "image send should be accepted: {sent_resp:?}");

    let sent_msg = world.until("image message sent", || {
        world
            .messages(&ana)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some(client_id) && m.state == SendState::Sent)
    });
    assert!(sent_msg.text.contains("See attached screenshot"));
    assert_eq!(sent_msg.attachments.len(), 1);
    assert_eq!(sent_msg.attachments[0].file.kind, MediaKind::Image);

    // 4. Verify AMS object creation (POST /ams/v1/objects) and image upload (PUT .../content/imgpsh).
    let create_call = world
        .fake
        .calls("POST", "/ams/v1/objects")
        .pop()
        .expect("POST /ams/v1/objects");
    let create_json = create_call.json();
    assert_eq!(create_json["type"].as_str(), Some("pish/image"));
    assert_eq!(create_json["filename"].as_str(), Some("screenshot.png"));
    assert!(create_json["permissions"][CHAT_ANA].is_array());

    let put_call = world
        .fake
        .calls("PUT", "/content/imgpsh")
        .pop()
        .expect("PUT /ams/v1/objects/{id}/content/imgpsh");
    assert!(
        put_call.headers.get("authorization").is_some_and(|v| v.starts_with("skype_token ")),
        "AMS PUT must carry Authorization: skype_token ..."
    );

    // 5. Verify posted RichText/Html carries the inline AMS <img> tag with dimensions.
    let post_call = world
        .fake
        .calls("POST", &format!("/chat/v1/users/ME/conversations/{CHAT_ANA}/messages"))
        .into_iter()
        .find(|c| c.json()["clientmessageid"] == client_id)
        .expect("chat POST with inline image");
    let post_body = post_call.json();
    let html = post_body["content"].as_str().unwrap_or("");
    assert!(html.contains("See attached screenshot"), "{html}");
    assert!(html.contains("http://schema.skype.com/AMSImage"), "{html}");
    assert!(html.contains("/ams/v1/objects/"), "{html}");
    assert!(html.contains("/views/imgo"), "{html}");
    assert!(html.contains("width=\"320\"") && html.contains("height=\"200\""), "{html}");

    let _ = std::fs::remove_dir_all(&staged);
}

#[test]
fn read_by_lists_readers_and_live_seen_updates_on_horizon_push() {
    let world = World::start();
    world.ready();

    // 1. Account advertises read_by capability.
    assert!(world.account(HOME).can.read_by);

    // 2. Request::ReadBy on Launch crew (19:grp2@thread.v2) returns members who read past message id.
    let grp2 = key("19:grp2@thread.v2");
    let Response::ReadBy { readers } = world.ask(Request::ReadBy {
        conversation: grp2.clone(),
        message_id: "1759000000000".into(),
    }) else {
        panic!("expected Response::ReadBy");
    };
    let reader_ids: Vec<&str> = readers.iter().map(|r| r.person.id.as_str()).collect();
    assert!(reader_ids.contains(&BEN), "Ben read 1759000000000: {readers:?}");
    assert!(
        reader_ids.contains(&"8:orgid:00000000-0000-4000-8000-0000000000c3"),
        "third member read past 1759000000000: {readers:?}"
    );
    assert!(!reader_ids.contains(&ME), "self should not be listed in ReadBy");

    // For a newer message (1759000000250), only member c3 (horizon 1759000000500) has read it.
    let Response::ReadBy { readers: partial } = world.ask(Request::ReadBy {
        conversation: grp2,
        message_id: "1759000000250".into(),
    }) else {
        panic!("expected Response::ReadBy for newer message");
    };
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].person.id, "8:orgid:00000000-0000-4000-8000-0000000000c3");

    // 3. Live Seen update: send a message in Ana's 1:1 chat; initially not seen, then push a
    //    ThreadActivity/MemberConsumptionHorizonUpdate from Ana and verify `seen` flips to true live.
    let ana = key(CHAT_ANA);
    world.open(&ana, 3);
    let cid = "1760000999000654";
    world.send(&ana, cid, "Did you see the PR?");
    let sent = world.until("message sent to Ana", || {
        world.messages(&ana).into_iter().find(|m| m.client_id.as_deref() == Some(cid) && m.state == SendState::Sent)
    });
    assert!(!world.conversation(&ana).unwrap().seen, "not seen before peer reads it");

    let horizon_push = json!({
        "id": "1760001999999",
        "conversationid": CHAT_ANA,
        "conversationLink": format!("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/{CHAT_ANA}"),
        "type": "Message",
        "messagetype": "ThreadActivity/MemberConsumptionHorizonUpdate",
        "contenttype": "text",
        "content": json!({
            "id": ANA,
            "consumptionhorizon": format!("{};1760001999999;{}", sent.id, sent.id)
        }).to_string(),
        "from": format!("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/{ANA}"),
        "properties": {}
    });
    world.fake.push("NewMessage", horizon_push, false);
    let seen_row = world.until("Ana chat becomes seen live", || {
        world.conversation(&ana).filter(|row| row.seen)
    });
    assert!(seen_row.seen);
}

#[test]
fn shared_channels_are_distinguished_from_private_and_external_orgs_marked() {
    let world = World::start();
    world.ready();

    // Replace the teams/users/me payload with a team having standard (0), private (1),
    // same-org shared (2), and an external-org shared channel whose host team we are not a member of.
    let ext_tenant = "99999999-9999-4999-8999-999999999999";
    let custom = json!({
        "teams": [
            {
                "id": GENERAL,
                "displayName": "Platform",
                "isDeleted": false,
                "isArchived": false,
                "tenantId": HOME,
                "channels": [
                    {
                        "id": GENERAL,
                        "displayName": "General",
                        "isGeneral": true,
                        "isFavorite": true,
                        "isDeleted": false,
                        "channelType": 0,
                        "tenantId": HOME
                    },
                    {
                        "id": "19:platform0leads@thread.tacv2",
                        "displayName": "Leads",
                        "isGeneral": false,
                        "isFavorite": true,
                        "isDeleted": false,
                        "channelType": 1,
                        "tenantId": HOME
                    },
                    {
                        "id": "19:platform0shared@thread.tacv2",
                        "displayName": "Cross-Team Sync",
                        "isGeneral": false,
                        "isFavorite": true,
                        "isDeleted": false,
                        "channelType": 2,
                        "tenantId": HOME
                    }
                ]
            }
        ],
        "sharedChannels": [
            {
                "id": "19:partner0extshared@thread.tacv2",
                "displayName": "Vendor Launch",
                "parentTeamId": "19:partner0general@thread.tacv2",
                "parentTeamName": "Partner Engineering",
                "isGeneral": false,
                "isFavorite": true,
                "isDeleted": false,
                "channelType": 2,
                "tenantId": ext_tenant
            }
        ],
        "chats": []
    });
    *world.fake.teams_override.lock().unwrap() = Some(custom);
    // Trigger a fresh teams/users/me listing via trouter.message_loss.
    world.fake.socket.lock().unwrap().as_ref().unwrap().send("5:99+::{\"name\":\"trouter.message_loss\"}".to_owned()).unwrap();

    // Wait for the external shared channel to appear under its host team.
    let ext_row = world.until("external shared channel listed", || {
        world.conversation(&key("19:partner0extshared@thread.tacv2"))
    });
    assert!(ext_row.shared, "channelType 2 must set shared=true");
    assert!(!ext_row.private, "shared channel must not be marked private (lock)");
    assert_eq!(ext_row.mark, Some(protocol::Mark::External), "cross-tenant shared channel must be marked External");
    assert_eq!(ext_row.space.as_deref(), Some("19:partner0general@thread.tacv2"));

    // Standard, private, and same-org shared channels in Platform.
    let std_row = world.conversation(&key(GENERAL)).unwrap();
    assert!(!std_row.private && !std_row.shared && std_row.mark.is_none());

    let priv_row = world.conversation(&key("19:platform0leads@thread.tacv2")).unwrap();
    assert!(priv_row.private && !priv_row.shared && priv_row.mark.is_none());

    let same_org_shared = world.conversation(&key("19:platform0shared@thread.tacv2")).unwrap();
    assert!(same_org_shared.shared, "same-org channelType 2 must set shared=true");
    assert!(!same_org_shared.private, "same-org shared channel must not set private=true");
    assert_eq!(same_org_shared.mark, None, "same-org shared channel should not be marked External");
}

#[test]
fn channels_can_be_shown_hidden_and_pinned_with_filing_updates() {
    let world = World::start();
    world.ready();

    // 1. All channels of Platform (including hidden `Hidden one`) are listed, with hidden flagged opted_out.
    let deploys = key(DEPLOYS);
    let hidden = key("19:platform0hidden@thread.tacv2");
    assert!(!world.conversation(&deploys).unwrap().opted_out);
    assert!(world.conversation(&hidden).unwrap().opted_out);
    assert_eq!(world.conversation(&deploys).unwrap().pin, None);

    // 2. Pin Deploys channel -> PUT properties?name=pinned and Filing::pin updated.
    assert_eq!(
        world.ask(Request::File {
            conversation: deploys.clone(),
            pinned: Some(true),
            archived: None,
            folder_add: None,
            folder_remove: None,
        }),
        Response::Ok
    );
    assert!(
        world
            .fake
            .calls("PUT", &format!("/{DEPLOYS}/properties"))
            .iter()
            .any(|c| c.query == "name=pinned" && c.json()["pinned"] == "true"),
        "expected PUT pinned=true for Deploys channel"
    );
    assert_eq!(world.conversation(&deploys).unwrap().pin, Some(0));

    // 3. Unpin Deploys channel -> PUT properties?name=pinned=false and Filing::pin cleared.
    assert_eq!(
        world.ask(Request::File {
            conversation: deploys.clone(),
            pinned: Some(false),
            archived: None,
            folder_add: None,
            folder_remove: None,
        }),
        Response::Ok
    );
    assert_eq!(world.conversation(&deploys).unwrap().pin, None);

    // 4. Hide Deploys channel (`archived: Some(true)`) -> PUT channelSettings isFavorite=false, opted_out=true.
    assert_eq!(
        world.ask(Request::File {
            conversation: deploys.clone(),
            pinned: None,
            archived: Some(true),
            folder_add: None,
            folder_remove: None,
        }),
        Response::Ok
    );
    assert!(
        world
            .fake
            .calls("PUT", &format!("/{DEPLOYS}/properties"))
            .iter()
            .any(|c| c.query == "name=channelSettings" && c.json()["channelSettings"]["isFavorite"] == false),
        "expected PUT channelSettings isFavorite=false when hiding channel"
    );
    assert!(world.conversation(&deploys).unwrap().opted_out, "hidden channel should have opted_out=true");

    // 5. Show Hidden one (`archived: Some(false)`) -> PUT channelSettings isFavorite=true, opted_out=false.
    assert_eq!(
        world.ask(Request::File {
            conversation: hidden.clone(),
            pinned: None,
            archived: Some(false),
            folder_add: None,
            folder_remove: None,
        }),
        Response::Ok
    );
    assert!(
        world
            .fake
            .calls("PUT", &format!("/19:platform0hidden@thread.tacv2/properties"))
            .iter()
            .any(|c| c.query == "name=channelSettings" && c.json()["channelSettings"]["isFavorite"] == true),
        "expected PUT channelSettings isFavorite=true when showing channel"
    );
    assert!(!world.conversation(&hidden).unwrap().opted_out, "shown channel should have opted_out=false");
}
