//! Push: trouter, the socket every Teams client keeps open. The chat service is told to deliver
//! to it (the registration), and from then on each event arrives as a small request that has to
//! be answered. See the notes at the top of `teams.rs` for where each step was read.

use super::{bare, escaped, now_ms, uuid, Auth, Body, Fault, Teams, SPACES};
use crate::adapters::net::{address, Socket};
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
/// Presence of what is on screen is asked again every this many pings.
const PRESENCE_EVERY: u32 = 4;
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
    pings: u32,
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

/// A gzip member unpacked, at most [`BODY`] bytes of it.
fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 18 || data[..3] != [0x1f, 0x8b, 8] {
        return None;
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + usize::from(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]));
    }
    // The file name, then the comment: each runs to a zero byte.
    for flag in [8, 16] {
        if flags & flag != 0 {
            at += data.get(at..)?.iter().position(|byte| *byte == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec_with_limit(data.get(at..)?, BODY).ok()
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
    String::from_utf8(gunzip(&bytes)?).ok()
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
        let base = address("TINYCHAT_TEAMS_TROUTER", TROUTER);
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
            pings: 0,
            registered: None,
        })
    }

    /// Reads one frame if there is one, and sends what is due: the ping, a registration near
    /// its end, and a fresh look at the presence of whoever is on screen.
    pub(super) fn pump(&self, push: &mut Push) -> Result<(), Failure> {
        if let Some(frame) = push.socket.read()? {
            self.frame(push, &frame)?;
        }
        if push.pinged.elapsed() > PING {
            push.sent += 1;
            push.socket.send(format!("5:{}+::{{\"name\":\"ping\"}}", push.sent))?;
            push.pinged = Instant::now();
            push.pings += 1;
            // The shell's watch lapses without telling anyone, and presence is not pushed here.
            let watching = self.ctx.watching();
            let ours = watching.people.iter().any(|person| person.account() == self.ctx.account);
            if push.pings.is_multiple_of(PRESENCE_EVERY) && ours {
                let _ = self.ctx.commands.send(Command::Watch(watching));
            }
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

    /// `ConversationUpdate`: the read marker moved on another device. Unverified: no client
    /// read handles it; the shape is the conversation object the read marker is written to.
    fn read_elsewhere(&self, conversation: &Value) -> Result<(), Fault> {
        let horizon = conversation["properties"]["consumptionhorizon"].as_str().and_then(|horizon| horizon.split(';').next());
        let (Some(id), Some(read)) = (conversation["id"].as_str(), horizon.filter(|read| super::millis(read) > 0)) else {
            return Ok(());
        };
        let Some(row) = self.ctx.known(id)? else {
            return Ok(());
        };
        let newest = match conversation["lastMessage"]["id"].as_str() {
            Some(newest) => Some(newest.to_owned()),
            None => self.ctx.newest(id)?,
        };
        if newest.is_some_and(|newest| super::millis(&newest) > super::millis(read)) {
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
