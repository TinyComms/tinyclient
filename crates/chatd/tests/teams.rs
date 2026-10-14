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
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really";

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
            "/api/csa/api/v1/teams/users/me" if bearer("csa") && header("x-skypetoken") == skype => ok(self.fixture("teams.users.me").unwrap()),
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
            _ if path.starts_with("/graph/") && bearer("graph") => self.graph(call, &path["/graph".len()..]),
            _ if path.starts_with("/upload/") => self.upload(call),
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
            let mut created = answer("201 Created", json!({}));
            created.headers.push(("Location", format!("{}/chat/v1/threads/{CHAT_DEE}", self.base)));
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
            let described = match thread {
                CHAT_DEE => self.fixture("thread.dee"),
                GENERAL => self.fixture("thread.general"),
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
                let older = call.query.contains("syncState=3e30cursor1") || call.query.contains("endTime=");
                let name = match conversation {
                    GENERAL if older => "messages.general.older",
                    GENERAL => "messages.general",
                    DEPLOYS => "messages.deploys",
                    CHAT_ANA => "messages.ana",
                    chain if chain == format!("{GENERAL};messageid={ROOT}") => "messages.chain",
                    "48:notifications" => "messages.activity",
                    _ => return ok(json!({"messages": [], "_metadata": {}})),
                };
                ok(self.fixture(name).unwrap())
            }
            ("POST", "messages") => self.post(call, conversation),
            ("PUT", "properties") => ok(json!({})),
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
        let arrived = 1_760_001_000_000 + u64::from(self.posted.fetch_add(1, Ordering::Relaxed)) * 1000;
        let (thread, root) = conversation.split_once(";messageid=").map_or((conversation, None), |(thread, root)| (thread, Some(root.to_owned())));
        let root = root.or_else(|| thread.contains("@thread.tacv2").then(|| arrived.to_string()));
        let mut echo = message(thread, root.as_deref(), arrived, ME, sent["imdisplayname"].as_str().unwrap_or(""), sent["content"].as_str().unwrap_or(""));
        echo["clientmessageid"] = sent["clientmessageid"].clone();
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

    /// Microsoft Graph's OneDrive: an upload session for a file, and a link to share it.
    fn graph(&self, call: &Call, path: &str) -> Reply {
        if call.method == "POST" && path.starts_with("/v1.0/me/drive/root:/Microsoft Teams Chat Files/") && path.ends_with(":/createUploadSession") {
            return ok(json!({"uploadUrl": format!("{}/upload/session-1?sig=x", self.base), "expirationDateTime": "2026-10-15T00:00:00Z"}));
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
        // Short path: a unix socket address holds about 100 bytes.
        let dir = PathBuf::from(format!("/tmp/tinyclient-teams-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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
        let log = if std::env::var_os("TINYCHAT_TEST_LOG").is_some() { Stdio::inherit() } else { Stdio::null() };
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .env("TINYCHAT_DIR", &dir)
            .env("TINYCHAT_SESSION_TEAMS", session.to_string())
            .env("TINYCHAT_TEAMS_LOGIN", &fake.base)
            .env("TINYCHAT_TEAMS_API", &fake.base)
            .env("TINYCHAT_TEAMS_TROUTER", &fake.base)
            .env("TINYCHAT_TEAMS_GRAPH", format!("{}/graph", fake.base))
            .env("TINYCHAT_MEDIA_HOSTS", "127.0.0.1")
            .env_remove("TINYCHAT_SESSION_DISCORD")
            .env_remove("TINYCHAT_SESSION_SLACK")
            .env_remove("TINYCHAT_SESSION_TELEGRAM")
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
        ["1760000050050", "1760000100100", "1760000200200", "1760000250250", "1760000260260", "1760000300300"],
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

    // A card is named and linked into the official app, never blank.
    let card = by("1760000300300");
    assert_eq!(card.text, "Card: Build 512 passed");
    let MessageKind::Unsupported { link } = &card.kind else {
        panic!("{:?}", card.kind);
    };
    assert_eq!(link, &format!("https://teams.microsoft.com/l/message/19%3Aplatform0general%40thread.tacv2/1760000300300?tenantId={HOME}"));

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
    let edit = Request::Edit { conversation: ana.clone(), message_id: sent.id.clone(), text: "hello Ana".to_owned() };
    assert_eq!(world.ask(edit), Response::Ok);
    let put = world.fake.calls("PUT", &path("/messages/1760001000000")).pop().expect("the edit reached Teams");
    assert_eq!((put.json()["content"].as_str(), put.json()["messagetype"].as_str()), (Some("<p>hello Ana</p>"), Some("RichText/Html")));
    let edited = world.message(&ana, &sent.id).unwrap();
    assert_eq!((edited.text.as_str(), edited.edited), ("hello Ana", true));

    // React and take it back, under Teams' name for the emoji.
    let react = |emoji: &str, on: bool| Request::React { conversation: ana.clone(), message_id: "1760000500500".to_owned(), emoji: emoji.to_owned(), on };
    assert_eq!(world.ask(react("👍", true)), Response::Ok);
    let given = world.until("the reaction to reach Teams", || world.fake.calls("PUT", &path("/messages/1760000500500/properties")).pop());
    assert_eq!((given.query.as_str(), given.json()["emotions"]["key"].as_str()), ("name=emotions", Some("like")));
    assert!(given.json()["emotions"]["value"].as_u64().is_some_and(|at| at > 1_700_000_000_000));
    assert_eq!(world.ask(react("👍", false)), Response::Ok);
    let taken = world.until("the reaction to be taken back", || world.fake.calls("DELETE", &path("/messages/1760000500500/properties")).pop());
    assert_eq!(taken.json(), json!({"emotions": {"key": "like"}}));

    // Mark read: the read marker, at that message.
    assert_eq!(world.ask(Request::MarkRead { conversation: ana.clone(), message_id: sent.id.clone() }), Response::Ok);
    let marked = world.until("Teams to be told the chat was read", || world.fake.calls("PUT", &path("/properties")).pop());
    assert_eq!(marked.query, "name=consumptionhorizon");
    let horizon = marked.json()["consumptionhorizon"].as_str().unwrap().to_owned();
    let parts: Vec<&str> = horizon.split(';').collect();
    assert_eq!((parts.len(), parts[0], parts[2]), (3, "1760001000000", "1760001000000"));
    assert!(parts[1].parse::<u64>().unwrap() > 1_700_000_000_000);

    // Typing: one signal however often the draft changes.
    for _ in 0..3 {
        assert_eq!(world.ask(Request::Typing { conversation: ana.clone() }), Response::Ok);
    }
    let typed = world.until("the typing signal", || {
        let signals: Vec<Call> = world.fake.calls("POST", &path("/messages")).into_iter().filter(|call| call.json()["messagetype"] == "Control/Typing").collect();
        (!signals.is_empty()).then_some(signals)
    });
    sleep(Duration::from_millis(400));
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].json(), json!({"content": "", "contenttype": "Application/Message", "messagetype": "Control/Typing"}));

    // Delete: gone here once Teams agreed.
    assert_eq!(world.ask(Request::Delete { conversation: ana.clone(), message_id: sent.id.clone() }), Response::Ok);
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

    // A person's card says what their organization says of them.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key(ANA), space: None }) else {
        panic!("no profile");
    };
    // Title, department and email each in their place; nothing left as a bio.
    assert_eq!((profile.person.name.as_str(), profile.bio.as_str()), ("Ana Ruiz", ""));
    assert_eq!((profile.title.as_str(), profile.department.as_str()), ("Staff Engineer", "Platform"));
    assert_eq!(profile.contacts.iter().map(|contact| contact.value.as_str()).collect::<Vec<_>>(), ["ana@contoso.com"]);

    // Presence is asked for exactly who is on screen, together.
    let Response::Status { seq, .. } = world.ask(Request::Status) else {
        panic!("no status");
    };
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
    let presence = world.fake.calls("POST", "/presence/v1/presence/getpresence/");
    assert_eq!(presence[0].json(), json!([{"mri": ANA}, {"mri": ME}]), "who is on screen, and me");
    // Asked again at once for the same people, Teams is not asked again.
    world.ask(watch());
    sleep(Duration::from_millis(400));
    assert_eq!(world.fake.calls("POST", "/presence/v1/presence/getpresence/").len(), 1);

    // My own presence is set only when I ask.
    assert!(world.fake.calls("PUT", "/presence/v1/me/forceavailability/").is_empty());
    let set = Request::SetPresence { account: key("").account(), presence: Presence::Dnd, status: None };
    assert_eq!(world.ask(set), Response::Ok);
    assert_eq!(world.fake.calls("PUT", "/presence/v1/me/forceavailability/").pop().unwrap().json(), json!({"availability": "DoNotDisturb"}));

    // The member list of a channel, bots marked.
    let Response::Members { members, sections, .. } = world.ask(Request::Members { conversation: key(GENERAL), offset: 0, limit: 50 }) else {
        panic!("no members");
    };
    assert_eq!(sections[0].count, 4);
    let listed: Vec<(&str, bool)> = members.iter().map(|member| (member.name.as_str(), member.bot)).collect();
    assert_eq!(listed, [("Me Myself", false), ("Ana Ruiz", false), ("Ben Okafor", false), ("Deploy Bot", true)]);

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
    assert_eq!((info.members, info.notify), (Some(4), protocol::Notify::All));

    // Mute is set on Teams, then shown.
    assert_eq!(world.ask(Request::SetMute { conversation: grp1.clone(), notify: protocol::Notify::Nothing }), Response::Ok);
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

    // A file is refused until it can be shared with the chat's members alone: an
    // organization-wide link is not what the Teams app makes. Nothing reaches OneDrive.
    let staged = PathBuf::from(format!("/tmp/tinyclient-teams-file-{}", std::process::id()));
    std::fs::create_dir_all(&staged).unwrap();
    let path = staged.join("notes.txt");
    std::fs::write(&path, b"standup notes").unwrap();
    let with = protocol::Outgoing {
        files: vec![protocol::OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: false, as_file: false, original: false }],
        ..Default::default()
    };
    let asked = Request::Send { conversation: key(CHAT_ANA), client_id: "88001".into(), text: "notes attached".into(), reply_to: None, with };
    let answer = world.ask(asked);
    assert!(!matches!(answer, Response::Sent { .. }), "a file went to Teams: {answer:?}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(world.fake.calls("POST", ":/createUploadSession").is_empty());
    assert!(world.fake.calls("POST", "/createLink").is_empty());
    let _ = std::fs::remove_dir_all(&staged);
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
