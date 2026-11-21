//! Push: trouter, the socket every Teams client keeps open. The chat service is told to deliver
//! to it (the registration), and from then on each event arrives as a small request that has to
//! be answered. See the notes at the top of `teams.rs` for where each step was read.

use super::{bare, escaped, now_ms, uuid, Auth, Body, Fault, Teams, SPACES};
use crate::adapters::net::{address, gunzip, Socket};
use crate::adapters::{Command, Failure};
use base64::Engine;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const TROUTER: &str = "https://go.trouter.teams.microsoft.com";
/// The client version trouter and the registrar are told, as purple-teams sends it now.
const VERSION: &str = "49/25113001312";
const TRACE: &str = "2024.23.01.2";
/// How long the socket waits for a frame before looking at what else is due.
const TICK: Duration = Duration::from_secs(5);
const PING: Duration = Duration::from_secs(30);
/// How long a registration lasts, in seconds, and how long before its end it is made again.
const REGISTERED: u64 = 86_400;
const RENEW: Duration = Duration::from_secs(REGISTERED - 600);
/// The largest event body unpacked.
const BODY: usize = 4 * 1024 * 1024;

pub(super) struct Push {
    socket: Socket,
    /// Where the chat service is to deliver, and what trouter asked to be shown again.
    surl: String,
    params: Value,
    endpoint: String,
    sent: u64,
    pinged: Instant,
    registered: Option<Instant>,
}

/// The payload of a socket.io 0.9 frame: what follows its third colon.
fn payload(frame: &str) -> Option<&str> {
    frame.splitn(4, ':').nth(3)
}

/// The id a `5:N+::` frame asks to be acknowledged under.
fn ack(frame: &str) -> Option<&str> {
    let id = frame.split(':').nth(1)?.trim_end_matches('+');
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

/// An event's body as text: as sent, or unpacked when its headers say it was packed.
fn unpacked(request: &Value) -> Option<String> {
    let body = request["body"].as_str()?;
    let headers = request["headers"].as_object();
    let packed = headers.into_iter().flatten().any(|(name, value)| {
        name.eq_ignore_ascii_case("X-Microsoft-Skype-Content-Encoding") && value.as_str().is_some_and(|how| how.eq_ignore_ascii_case("gzip"))
    });
    if !packed {
        return Some(body.to_owned());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(body).ok()?;
    String::from_utf8(gunzip(&bytes, BODY)?).ok()
}

impl Teams {
    /// Opens the socket: trouter names a server and signs the visit, a socket.io session is
    /// opened there, and the websocket joins it. Nothing is delivered until the registration,
    /// which is made when the socket says it is up.
    pub(super) fn open(&self) -> Result<Push, Failure> {
        // One endpoint for as long as the account is here: Teams limits how many a user has.
        let endpoint = self.ctx.note("endpoint").unwrap_or_else(|| {
            let made = uuid();
            self.ctx.set_note("endpoint", &made);
            made
        });
        let base = address("TINYCLIENT_TEAMS_TROUTER", TROUTER);
        let base = base.trim_end_matches('/');
        let named = self.call("POST", &format!("{base}/v4/a?epid={}", escaped(&endpoint)), Auth::Push, Body::None, true)?.body;
        let socketio = named["socketio"].as_str().unwrap_or(base).trim_end_matches('/').to_owned();
        let surl = named["surl"].as_str().unwrap_or("").to_owned();
        if surl.is_empty() || !self.ours(&surl) || !self.ours(&socketio) {
            return Err(Failure::Dropped("trouter named no address of Microsoft's".to_owned()));
        }
        let query = |visit: u32| {
            let mut query = "v=v4".to_owned();
            for (name, value) in named["connectparams"].as_object().into_iter().flatten() {
                query.push_str(&format!("&{}={}", escaped(name), escaped(value.as_str().unwrap_or(""))));
            }
            let told = json!({"cv": TRACE, "ua": "TeamsCDL", "hr": "", "v": VERSION}).to_string();
            query.push_str(&format!("&tc={}&con_num={}_{visit}&epid={}", escaped(&told), now_ms(), escaped(&endpoint)));
            if let Some(ccid) = named["ccid"].as_str().filter(|ccid| !ccid.is_empty()) {
                query.push_str(&format!("&ccid={}", escaped(ccid)));
            }
            query.push_str("&auth=true&timeout=40");
            query
        };
        let session = self.call("GET", &format!("{socketio}/socket.io/1/?{}", query(1)), Auth::Push, Body::None, true)?.text;
        let id = session.split(':').next().unwrap_or("").trim();
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)) {
            return Err(Failure::Dropped("trouter opened no session".to_owned()));
        }
        let at = match socketio.split_once("://") {
            Some(("http", rest)) => format!("ws://{rest}"),
            Some((_, rest)) => format!("wss://{rest}"),
            None => return Err(Failure::Dropped("trouter named no address".to_owned())),
        };
        let url = format!("{at}/socket.io/1/websocket/{id}?{}", query(2));
        let (skype, _) = self.skype()?;
        let headers = [("X-Skypetoken", skype.as_str()), ("User-Agent", self.session.user_agent.as_str())];
        // The address carries trouter's signature, and the library's error repeats the address.
        let socket = Socket::connect(&url, &headers, TICK)
            .map_err(|err| Failure::Dropped(format!("the push socket at {} did not open ({:?})", bare(&at), err.kind())))?;
        Ok(Push {
            socket,
            surl,
            params: named["connectparams"].clone(),
            endpoint,
            sent: 0,
            pinged: Instant::now(),
            registered: None,
        })
    }

    /// Reads one frame if there is one, and sends what is due: the ping and a registration near
    /// its end. Presence is never asked on a timer: it comes pushed, or when the shell starts
    /// watching someone (`Command::Watch`).
    pub(super) fn pump(&self, push: &mut Push) -> Result<(), Failure> {
        if let Some(frame) = push.socket.read()? {
            self.frame(push, &frame)?;
        }
        if push.pinged.elapsed() > PING {
            push.sent += 1;
            push.socket.send(format!("5:{}+::{{\"name\":\"ping\"}}", push.sent))?;
            push.pinged = Instant::now();
        }
        if push.registered.is_some_and(|at| at.elapsed() > RENEW) {
            self.register(push)?;
        }
        Ok(())
    }

    fn frame(&self, push: &mut Push, frame: &str) -> Result<(), Failure> {
        match frame.as_bytes().first() {
            // The session is up: say who we are, then have the chat service deliver here.
            Some(b'1') => {
                let bearer = self.token(SPACES)?;
                let hello = json!({"name": "user.authenticate", "args": [{
                    "headers": {"X-Ms-Test-User": "False", "Authorization": format!("Bearer {bearer}"), "X-MS-Migration": "True"},
                    "connectparams": push.params,
                }]});
                push.socket.send(format!("5:::{hello}"))?;
                self.register(push)?;
            }
            Some(b'2') => push.socket.send("2::".to_owned())?,
            Some(b'3') => {
                let Some(request) = payload(frame).and_then(|text| serde_json::from_str::<Value>(text).ok()) else {
                    return Ok(());
                };
                // Answered before it is read: trouter sends again what is not answered.
                let answer = json!({"id": request["id"], "status": 200, "body": ""});
                push.socket.send(format!("3:::{answer}"))?;
                if let Some(body) = unpacked(&request) {
                    self.delivered(request["url"].as_str().unwrap_or(""), &body);
                }
            }
            Some(b'5') => {
                if let Some(id) = ack(frame) {
                    push.socket.send(format!("6:{id}::"))?;
                }
                let event = payload(frame).and_then(|text| serde_json::from_str::<Value>(text).ok());
                if event.is_some_and(|event| event["name"] == "trouter.message_loss") {
                    // Something was not delivered: read the lists again and deliver here anew.
                    self.state().resync = true;
                    let _ = self.ctx.commands.send(Command::OpenSpace(String::new()));
                    self.register(push)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Tells the chat service to deliver this user's events to the socket. Only the messaging
    /// registration is made: the calling ones would ring here, and calls are not carried.
    fn register(&self, push: &mut Push) -> Result<(), Failure> {
        let body = json!({
            "clientDescription": {
                "appId": "TeamsCDLWebWorker", "aesKey": "", "languageId": "en-US", "platform": "edge",
                "templateKey": "TeamsCDLWebWorker_2.1", "platformUIVersion": VERSION,
            },
            "registrationId": push.endpoint,
            "nodeId": "",
            "transports": {"TROUTER": [{"context": "", "path": push.surl, "ttl": REGISTERED}]},
        });
        let url = format!("{}/registrar/prod/V2/registrations", self.api);
        self.call("POST", &url, Auth::Both(SPACES), Body::Json(&body), true)?;
        push.registered = Some(Instant::now());
        Ok(())
    }

    /// One event, by the path it was delivered to.
    fn delivered(&self, url: &str, body: &str) {
        let path = bare(url).trim_end_matches('/');
        let Ok(body) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let handled = if path.ends_with("/messaging") {
            match (body["type"].as_str(), body["resourceType"].as_str()) {
                (Some("EventMessage"), Some("NewMessage")) => self.heard(&body["resource"], true),
                (Some("EventMessage"), Some("MessageUpdate")) => self.heard(&body["resource"], false),
                (Some("EventMessage"), Some("ConversationUpdate")) => self.read_elsewhere(&body["resource"]),
                _ => Ok(()),
            }
        } else if path.ends_with("/TeamsUnifiedPresenceService") || path.ends_with("/unifiedPresenceService") {
            self.presences(&body["presence"]);
            Ok(())
        } else {
            Ok(())
        };
        self.report(handled.map_err(Failure::from));
    }

    /// `ConversationUpdate`: the read marker or conversation properties (hidden, pinned) changed
    /// on another device.
    fn read_elsewhere(&self, conversation: &Value) -> Result<(), Fault> {
        let Some(id) = conversation["id"].as_str() else {
            return Ok(());
        };
        let Some(row) = self.ctx.known(id)? else {
            return Ok(());
        };
        let props = &conversation["properties"];
        let parse_flag = |val: &Value| -> Option<bool> {
            match val {
                Value::Bool(b) => Some(*b),
                Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
                Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
                _ => None,
            }
        };
        let hidden_upd = parse_flag(&props["hidden"]);
        let pinned_upd = parse_flag(&props["pinned"]);
        if hidden_upd.is_some() || pinned_upd.is_some() {
            let pin = match pinned_upd {
                Some(true) => {
                    let mut state = self.state();
                    if let Some(pos) = state.pinned_chats.iter().position(|p| p == id) {
                        Some(pos as i32)
                    } else {
                        state.pinned_chats.push(id.to_owned());
                        Some((state.pinned_chats.len() - 1) as i32)
                    }
                }
                Some(false) => {
                    self.state().pinned_chats.retain(|p| p != id);
                    None
                }
                None => row.pin,
            };
            let filing = crate::store::Filing {
                hidden: hidden_upd.unwrap_or(row.hidden),
                pin,
                ..crate::store::Filing::default()
            };
            self.ctx.filing(id, &filing)?;
        }
        if let Some(level) = super::info::parse_notify(props, super::tree::is_channel(id)) {
            self.state().notify.insert(id.to_owned(), level);
            self.ctx.set_note(&format!("notify:{id}"), super::info::notify_word(level));
            self.ctx.muted(id, level == protocol::Notify::Nothing)?;
        }
        let mut parts = props["consumptionhorizon"].as_str().into_iter().flat_map(|h| h.split(';'));
        let horizon = parts.next();
        let client_ts = parts.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let Some(read) = horizon.filter(|read| *read == "0" || super::millis(read) > 0) else {
            return Ok(());
        };
        if client_ts > 0 {
            let mut state = self.state();
            if state.horizon_ms.get(id).is_some_and(|&prev_ts| client_ts < prev_ts) {
                return Ok(());
            }
            state.horizon_ms.insert(id.to_owned(), client_ts);
        }
        let newest = match conversation["lastMessage"]["id"].as_str() {
            Some(newest) => Some(newest.to_owned()),
            None => self.ctx.newest(id)?,
        };
        let moved_back = row
            .last_read
            .as_deref()
            .is_some_and(|prev| super::millis(read) < super::millis(prev));
        let has_unread = newest.as_deref().is_some_and(|newest| super::millis(newest) > super::millis(read));
        if moved_back || (has_unread && row.unread == 0) {
            let marker = if read == "0" { "" } else { read };
            self.ctx.manual_ack(id, marker, None)?;
            if has_unread && self.ctx.known(id)?.is_some_and(|updated| updated.unread == 0) {
                self.ctx.mark_unread(id)?;
            }
        } else if has_unread {
            self.ctx.counts(id, row.unread, row.mentions, Some(read), 0)?;
        } else {
            self.ctx.counts(id, 0, 0, Some(read), 0)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_cut_at_their_third_colon() {
        assert_eq!(payload("3:::{\"id\":7,\"url\":\"/v4/f/x/messaging\"}"), Some("{\"id\":7,\"url\":\"/v4/f/x/messaging\"}"));
        assert_eq!(payload("5:12+::{\"name\":\"trouter.message_loss\"}"), Some("{\"name\":\"trouter.message_loss\"}"));
        assert_eq!(payload("1::"), None);
        assert_eq!((ack("5:12+::{}"), ack("5:::{}"), ack("5:x::{}")), (Some("12"), None, None));
    }

    #[test]
    fn a_packed_body_is_unpacked_and_a_plain_one_left_alone() {
        // `{"type":"EventMessage"}` through gzip, with a file name in the header.
        let mut packed = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3, b'a', 0];
        packed.extend(miniz_oxide::deflate::compress_to_vec(br#"{"type":"EventMessage"}"#, 6));
        packed.extend([0u8; 8]);
        let body = base64::engine::general_purpose::STANDARD.encode(&packed);
        let request = json!({"headers": {"x-microsoft-skype-content-encoding": "GZIP"}, "body": body});
        assert_eq!(unpacked(&request).as_deref(), Some(r#"{"type":"EventMessage"}"#));
        let plain = json!({"headers": {"MS-CV": "x"}, "body": "{\"a\":1}"});
        assert_eq!(unpacked(&plain).as_deref(), Some("{\"a\":1}"));
        assert_eq!(unpacked(&json!({"headers": {"X-Microsoft-Skype-Content-Encoding": "gzip"}, "body": "bm90IGd6aXA="})), None);
    }
}
