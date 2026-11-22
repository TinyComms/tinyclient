//! Push: trouter, the socket every Teams client keeps open. The chat service is told to deliver
//! to it (the registration), and from then on each event arrives as a small request that has to
//! be answered. See the notes at the top of `teams.rs` for where each step was read.

use super::{bare, escaped, now_ms, uuid, Auth, Body, Fault, Teams, SPACES, TEAMS};
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
const TICK: Duration = Duration::from_secs(2);
const PING: Duration = Duration::from_secs(30);
/// How often `user.activity: active` is repeated while the user is actively using the window.
const ACTIVITY_HEARTBEAT: Duration = Duration::from_secs(240);
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
    active_sent: bool,
    activity_at: Option<Instant>,
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
        let endpoint = self.ctx.note("endpoint").filter(|id| !id.is_empty()).unwrap_or_else(|| {
            let made = uuid();
            self.ctx.set_note("endpoint", &made);
            made
        });
        self.state().endpoint = Some(endpoint.clone());
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
            active_sent: false,
            activity_at: None,
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
        if push.registered.is_some() {
            self.sync_activity(push)?;
        }
        Ok(())
    }

    fn want_active_presence(&self) -> bool {
        let watching = self.ctx.watching();
        watching.focused && watching.active && self.ctx.hub.settings().presence_activity(&self.ctx.account)
    }

    /// Sends trouter `user.activity` (`active` on focus+input + 240 s heartbeat, `inactive` when
    /// unfocused, idle, or when Settings › Privacy turns active presence off).
    fn sync_activity(&self, push: &mut Push) -> Result<(), Failure> {
        let want = self.want_active_presence();
        if want {
            let due = !push.active_sent || push.activity_at.is_none_or(|at| at.elapsed() >= ACTIVITY_HEARTBEAT);
            if due {
                let frame = json!({"name": "user.activity", "args": [{"state": "active"}]});
                push.socket.send(format!("5:::{frame}"))?;
                push.active_sent = true;
                push.activity_at = Some(Instant::now());
            }
        } else if push.active_sent {
            let frame = json!({"name": "user.activity", "args": [{"state": "inactive"}]});
            push.socket.send(format!("5:::{frame}"))?;
            push.active_sent = false;
            push.activity_at = Some(Instant::now());
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
                self.sync_activity(push)?;
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
        self.state().endpoint = Some(push.endpoint.clone());
        push.registered = Some(Instant::now());
        Ok(())
    }

    /// Best-effort removal of this device's trouter push registration on sign-out.
    /// Uses a short 3-second timeout and only already-cached tokens so it never stalls sign-out.
    pub(super) fn unregister_push(&self) {
        let (endpoint, cached_auth) = {
            let mut state = self.state();
            if state.expired.is_some() {
                state.endpoint = None;
                drop(state);
                self.ctx.set_note("endpoint", "");
                return;
            }
            let endpoint = state.endpoint.take();
            let bearer = state.access.get(SPACES).map(|t| t.value.clone());
            let skype = state.skype.as_ref().map(|t| t.value.clone());
            (endpoint, bearer.zip(skype))
        };
        let endpoint = endpoint.or_else(|| self.ctx.note("endpoint")).filter(|id| !id.is_empty());
        self.ctx.set_note("endpoint", "");
        let Some(endpoint) = endpoint else {
            return;
        };
        let url = format!("{}/registrar/prod/V2/registrations/{}", self.api, escaped(&endpoint));
        if !self.ours(&url) {
            return;
        }
        let Some((bearer, skype)) = cached_auth else {
            return;
        };
        let headers = [
            ("Authorization", format!("Bearer {bearer}")),
            ("X-Skypetoken", skype),
        ];
        let fast: ureq::Agent = crate::adapters::net::apply_proxy(ureq::Agent::config_builder())
            .tls_config(crate::tls::config())
            .timeout_global(Some(Duration::from_secs(3)))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent(&self.session.user_agent)
            .build()
            .into();
        let mut req = fast
            .delete(&url)
            .header("Accept", "application/json")
            .header("BehaviorOverride", "redirectAs404")
            .header("Origin", TEAMS)
            .header("Referer", "https://teams.microsoft.com/");
        for (name, value) in &headers {
            req = req.header(*name, value.as_str());
        }
        if let Err(err) = req.call() {
            tracing::warn!("chatd: Teams: unregister push on sign-out failed: {}", crate::adapters::net::redact(&err.to_string()));
        }
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
                (Some("EventMessage"), Some("ConversationUpdate" | "ThreadUpdate")) => self.read_elsewhere(&body["resource"]),
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
        if id == super::tree::ACTIVITY {
            if let Some(marker) = conversation["properties"]["consumptionhorizon"]
                .as_str()
                .and_then(|h| h.split(';').next())
                .map(super::millis)
                .filter(|&ms| ms > 0)
            {
                let updated = {
                    let mut state = self.state();
                    if state.activity_read_horizon == 0 {
                        if let Some(saved) = self.ctx.note("activity_read_horizon").as_deref().map(super::millis) {
                            state.activity_read_horizon = saved;
                        }
                    }
                    if marker > state.activity_read_horizon {
                        state.activity_read_horizon = marker;
                        Some(marker)
                    } else {
                        None
                    }
                };
                if let Some(horizon) = updated {
                    self.ctx.set_note("activity_read_horizon", &horizon.to_string());
                }
            }
            return Ok(());
        }
        let Some(row) = self.ctx.known(id)? else {
            return Ok(());
        };
        let props = &conversation["properties"];
        if let Some(horizons) = conversation["consumptionhorizons"]
            .as_array()
            .or_else(|| props["consumptionhorizons"].as_array())
        {
            self.apply_member_horizons(id, horizons)?;
        }
        let parse_flag = |val: &Value| -> Option<bool> {
            match val {
                Value::Bool(b) => Some(*b),
                Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
                Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
                _ => None,
            }
        };
        let is_chan = super::tree::is_channel(id);
        let hidden_upd = parse_flag(&props["hidden"]);
        let pinned_upd = parse_flag(&props["pinned"]);
        if hidden_upd.is_some() || pinned_upd.is_some() {
            let pin = match pinned_upd {
                Some(true) => {
                    let mut state = self.state();
                    let list = if is_chan { &mut state.pinned_channels } else { &mut state.pinned_chats };
                    if let Some(pos) = list.iter().position(|p| p == id) {
                        Some(pos as i32)
                    } else {
                        list.push(id.to_owned());
                        Some((list.len() - 1) as i32)
                    }
                }
                Some(false) => {
                    let mut state = self.state();
                    let list = if is_chan { &mut state.pinned_channels } else { &mut state.pinned_chats };
                    list.retain(|p| p != id);
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
        if props.get("pinnedMessages").is_some() || props.get("pinnedMessageIds").is_some() {
            let entries = super::info::parse_pinned_messages(props);
            self.apply_pinned_update(id, entries)?;
            self.ctx.info_stale(id);
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
        let newest = match super::id_of(&conversation["lastMessage"]) {
            newest if !newest.is_empty() => Some(newest),
            _ => self.ctx.newest(id)?,
        };
        let moved_back = row
            .last_read
            .as_deref()
            .is_some_and(|prev| super::millis(read) < super::millis(prev));
        let has_unread = newest.as_deref().is_some_and(|newest| super::millis(newest) > super::millis(read));
        if moved_back || (has_unread && row.unread == 0) {
            let marker = if read == "0" { "" } else { read };
            self.ctx.manual_ack(id, marker, None)?;
            if (has_unread || (moved_back && newest.is_none()))
                && self.ctx.known(id)?.is_some_and(|updated| updated.unread == 0)
            {
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
