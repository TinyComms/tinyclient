//! Microsoft Teams chat (not calls), for work and school accounts, with the session and the
//! services the Teams client itself uses. One adapter is one tenant.
//!
//! Microsoft documents none of this. What follows is what working third-party clients do, read
//! from their source on 2026-10-13, and where each point stands. [C] = present in a maintained
//! client's code or fixtures. [U] = not confirmed by any source read; test first on a real
//! account. Nothing here has been run against Teams.
//!
//! Sources: EionRobb/purple-teams (f7eb519, Sep 2026), YourSandwich/mautrix-teams (d02a207, Sep
//! 2026), IanTerzo/Squads (f977552, Sep 2026), eisbaw/ost (Feb 2026), fossteams/teams-api
//! (2022, real JSON fixtures). Not read: teams-for-linux, Better Teams.
//!
//! Sign-in (`crates/login/src/teams.rs`)
//! - Client id `1fec8e78-bce4-4aaf-ab1b-5451cc387264` (Teams' own desktop client), redirect
//!   `https://login.microsoftonline.com/common/oauth2/nativeclient`, tenant `organizations` [C:
//!   purple-teams, ost, mautrix]. The code is redeemed at `/{tenant}/oauth2/v2.0/token` with
//!   `scope=https://api.spaces.skype.com/.default openid profile offline_access` [C].
//! - PKCE on that authorize request [U]: purple-teams sends none, and the one client that sends
//!   a verifier (Squads) uses the web client's redirect. It is standard for a public client.
//! - One refresh token mints access tokens for other audiences by changing `scope` [C]:
//!   `api.spaces.skype.com` (authz, middle tier, trouter), `chatsvcagg.teams.microsoft.com`
//!   (teams and chats list), `presence.teams.microsoft.com`.
//! - A guest tenant is reached by naming its id in the token address with the same refresh
//!   token [U: the only mechanism in any source, and both clients have open bugs on it].
//! - Conditional access: purple-teams #78 (AADSTS53003) was worked around for the device-code
//!   flow by sending a browser User-Agent; it is still open. A tenant that requires a managed
//!   device blocks this whatever is sent.
//! - Personal accounts are not built: another client id (`8ec6bc83-…`), `consumers`, another
//!   authz, chat host, push host and registrar, with an unresolved rate-limit bug in the only
//!   client that does it.
//!
//! Tokens
//! - `POST https://teams.microsoft.com/api/authsvc/v1.0/authz`, empty body, bearer token for
//!   `api.spaces.skype.com`, answers `tokens.skypeToken`, `tokens.expiresIn`, `region` and
//!   `regionGtms` (`chatService`, `middleTier`, `ams`, `unifiedPresence`, …) [C, with a fixture].
//!   The chat service's address cannot be derived from the region; it is read from there [C].
//! - Our own id is read from the skype token's `skypeid` claim [U], else the sign-in's `oid`.
//!
//! Chat service (`regionGtms.chatService`, `Authentication: skypetoken=…`) [C: mautrix, ost]
//! - `GET /v1/users/ME/conversations/{id}/messages?view=…&pageSize=…&startTime=…`, newest
//!   first; `_metadata.backwardLink` carries the `syncState` of the next older page [C].
//!   `endTime` for a page before a time [C: purple-teams, which itself allows that the service
//!   may ignore it]. A page after or around a message [U].
//! - Send: `POST …/messages` with `clientmessageid`, `content`, `messagetype: RichText/Html`,
//!   `contenttype: text`, `imdisplayname`; the answer's `OriginalArrivalTime` is the id [C].
//!   Extra fields make Teams answer 201 and drop the message [C: mautrix's warning].
//! - Edit: `PUT …/messages/{id}`, same body [C: mautrix only]. Delete: `DELETE
//!   …/messages/{id}?behavior=softDelete` [C: Squads; mautrix sends it bare].
//! - Reactions: `PUT` / `DELETE …/messages/{id}/properties?name=emotions` [C].
//! - Read marker: `PUT …/conversations/{id}/properties?name=consumptionhorizon`,
//!   `{"consumptionhorizon":"{id};{now ms};{id}"}` [C].
//! - Typing: a message of type `Control/Typing` with `contenttype: Application/Message` [C].
//! - A chat is created with `POST /v1/threads`; its id comes back in `Location` [C].
//! - A channel post's replies: conversation `{channel};messageid={root}` [C: Squads]. In a flat
//!   page every message's `conversationLink` ends in `;messageid={root}` [C: fixture]. Whether
//!   the `;` goes raw or escaped in a path [U]; edit, delete and react inside a chain [U].
//!
//! Teams, channels and chats: `GET https://teams.microsoft.com/api/csa/api/v1/teams/users/me`
//! with a `chatsvcagg` bearer [C, with a fixture]. That hidden channels are `isFavorite:
//! false`, that the listed order is the user's, how unread is told for a chat (`isRead`), a
//! meeting's `meetingInformation` and a chat's `lastMessage.content` are read off field names
//! [U]. Mentions unread at connect come from one page of the Activity stream
//! (`48:notifications`), its `properties.activity` shape unverified [U].
//!
//! People: `POST {middleTier}/beta/users/fetchShortProfile` with a list of ids, `searchV2` with
//! the query as the body, pictures at `…/users/{id}/profilepicturev2?size=HR64x64` behind an
//! `authtoken` cookie [C]. Presence: `POST {unifiedPresence}/v1/presence/getpresence/`, own
//! through `PUT /v1/me/forceavailability/` [C].
//!
//! Pictures in messages are on AMS (`*.asm.skype.com`, `*.asyncgw.teams.microsoft.com`) and
//! need `Authorization: skype_token …` [C]. Files are SharePoint links in `properties.files`
//! [C] and need another audience, so they open in the browser.
//!
//! Push is trouter [C: purple-teams, mautrix, ost]: `POST go.trouter.teams.microsoft.com/v4/a`,
//! a socket.io 0.9 session, a websocket, `user.authenticate`, and a registration at
//! `teams.microsoft.com/registrar/prod/V2/registrations` that points the chat service at it.
//! Events arrive as requests to answer (`3:::`), `EventMessage` with `NewMessage` or
//! `MessageUpdate` [C]. The chat service's own long poll is gone (purple-teams 58c649f, Apr
//! 2025), so there is no fallback. A read marker moved elsewhere (`ConversationUpdate`) [U].
//! Not sent: `user.activity: active`, which the other clients send. A daemon that is always
//! on would tell Teams the user is always at the desk, and Teams holds back phone
//! notifications for someone who is. If nothing arrives live on a real account, try it first.
//!
//! Deep links: `…/l/meetup-join/{thread}/0` [C]; `msteams:/l/call/0/0?users=`, `/l/chat/…`,
//! `/l/channel/…` and `/l/message/…` are Microsoft's documented forms, in no client read [U].
//!
//! For tests (`tests/teams.rs`), read through `net::address`: `TINYCLIENT_TEAMS_LOGIN` replaces
//! `https://login.microsoftonline.com`, `TINYCLIENT_TEAMS_API` replaces
//! `https://teams.microsoft.com` (authz, the teams list, the registrar; the addresses authz
//! then hands out must be under it), `TINYCLIENT_TEAMS_TROUTER` replaces
//! `https://go.trouter.teams.microsoft.com`. The session comes from `TINYCLIENT_SESSION_TEAMS`.

mod html;
mod info;
pub(crate) mod message;
mod people;
mod push;
mod send;
mod tree;

use super::net::{address, browser};
use super::{keep_connected, serve_commands, Command, Context, Download, Failure, Refusal, Signed};
use base64::Engine;
use people::Known;
use protocol::Link;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Teams' own desktop client, the one registration third-party clients sign in as.
const CLIENT: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
const LOGIN: &str = "https://login.microsoftonline.com";
const TEAMS: &str = "https://teams.microsoft.com";
/// Token audiences.
const SPACES: &str = "https://api.spaces.skype.com";
const AGGREGATOR: &str = "https://chatsvcagg.teams.microsoft.com";
const PRESENCE: &str = "https://presence.teams.microsoft.com";
/// The second audience: Microsoft Graph, for the user's OneDrive (files in chats). Unverified
/// that Teams' client id may mint it.
const GRAPH: &str = "https://graph.microsoft.com";
/// Hosts a service address handed out by Teams may be on. A token goes nowhere else.
const OURS: &[&str] = &["teams.microsoft.com", "skype.com", "teams.cloud.microsoft", "teams.office.com", "graph.microsoft.com"];

/// Messages per page of history, and per opening of a reply chain.
const PAGE: u32 = 50;
const CHAIN: u32 = 200;
/// The pause after a history fetch.
const PACE: Duration = Duration::from_millis(1500);
/// Calls of any kind are kept this far apart, so nothing this adapter does is a burst.
const GAP: Duration = Duration::from_millis(250);
/// The longest a 429 is waited out in place. A longer one fails the call.
const PATIENCE: u64 = 30;
/// A 429 that says no `Retry-After` is taken to mean this many seconds.
const LIMITED_FOR: u64 = 30;
/// A token this close to its end is replaced before it is used.
const EARLY: Duration = Duration::from_secs(300);
/// Typing is told at most this often per conversation, and shown this long when heard.
const TYPING_EVERY: Duration = Duration::from_secs(8);
const TYPING_SHOWN: i64 = 8;
/// A list the shell opens again within this time is not fetched again; push keeps it current.
const TREE_FRESH: Duration = Duration::from_secs(600);
/// People kept in memory between store lookups.
const REMEMBERED: usize = 1_000;

/// One tenant's part of the sign-in: what `accounts` hands to `start`.
#[derive(Deserialize)]
struct Session {
    tenant: String,
    #[serde(default)]
    name: String,
    refresh_token: String,
    #[serde(default)]
    client_id: String,
    /// The User-Agent of the window the user signed in with; every request carries it.
    #[serde(default)]
    user_agent: String,
    /// The user's object id in their home tenant.
    #[serde(default)]
    oid: String,
    /// The user's own name, as Teams shows it beside what they send.
    #[serde(default)]
    user: String,
}

/// Why a call failed. Deliberately not `Display`: it becomes a [`Failure`] or words for the
/// user, never a raw string.
#[derive(Debug)]
enum Fault {
    /// The session is over, in words for the account's banner.
    Auth(String),
    /// Rate limited, for this many seconds.
    Limited(u64),
    /// The service said no.
    Refused { status: u16, message: String },
    Net(String),
}

impl From<Fault> for Failure {
    fn from(fault: Fault) -> Failure {
        match fault {
            Fault::Auth(why) => Failure::SignedOut(why),
            Fault::Limited(seconds) => Failure::Dropped(format!("rate limited for {seconds} s")),
            Fault::Refused { status, message } => Failure::Dropped(format!("{status} {message}")),
            Fault::Net(why) => Failure::Dropped(why),
        }
    }
}

impl From<rusqlite::Error> for Fault {
    fn from(err: rusqlite::Error) -> Fault {
        Fault::Net(err.to_string())
    }
}

/// A fault in words the user can read.
fn reason(fault: Fault) -> String {
    match fault {
        Fault::Limited(seconds) => format!("Teams is rate limiting you. Try again in {seconds} s."),
        Fault::Auth(why) => why,
        Fault::Net(_) => "Teams could not be reached.".to_owned(),
        Fault::Refused { status: 403, .. } => "Teams does not let you do that here.".to_owned(),
        Fault::Refused { status: 404, .. } => "Teams cannot find that any more.".to_owned(),
        Fault::Refused { status: 413, .. } => "The message is too long for Teams.".to_owned(),
        Fault::Refused { status, message } if message.is_empty() => format!("Teams refused ({status})."),
        Fault::Refused { status, message } => format!("Teams refused ({status}: {message})."),
    }
}

/// Why Microsoft would not renew the sign-in, for the account's banner. `description` is
/// Entra's own, which starts with its `AADSTS` code.
fn ended(error: &str, description: &str) -> String {
    let code = description.split(|c: char| !c.is_ascii_alphanumeric()).find(|word| word.starts_with("AADSTS")).unwrap_or(error);
    let policy = ["AADSTS53000", "AADSTS53001", "AADSTS53002", "AADSTS53003", "AADSTS53004", "AADSTS530002", "AADSTS530003", "AADSTS50105"];
    if policy.contains(&code) {
        format!("Your organization's policy does not allow this app ({code}). Teams stays in the Teams app.")
    } else {
        format!("Microsoft ended this sign-in ({code}). Sign in again.")
    }
}

/// Where this tenant's services are, as authz said.
struct Place {
    chat: String,
    middle: String,
    presence: String,
}

struct Token {
    value: String,
    until: Instant,
}

impl Token {
    fn fresh(&self) -> bool {
        self.until.saturating_duration_since(Instant::now()) > EARLY
    }
}

#[derive(Default)]
struct State {
    refresh: String,
    access: HashMap<&'static str, Token>,
    skype: Option<Token>,
    place: Option<Arc<Place>>,
    /// Our own id, `8:orgid:…`.
    me: String,
    /// Set by any call that finds the session over, so the push thread ends it.
    expired: Option<String>,
    people: HashMap<String, Known>,
    /// What was already written this run and need not be again: pictures registered, and
    /// the names of reactions learned.
    noted: HashSet<String>,
    /// Who the user has a one-to-one chat with, and which chat it is.
    partners: HashMap<String, String>,
    typed: Option<(String, Instant)>,
    /// Whose presence was last asked for, and when.
    watched: Vec<String>,
    watched_at: Option<Instant>,
    /// When the teams and chats were last listed.
    listed: Option<Instant>,
    /// Push has just reconnected, or said it lost something.
    resync: bool,
    /// Out-of-office notes from presence, and until when: what a card says under "away".
    away: HashMap<String, (String, Option<i64>)>,
}

/// What a call presents.
#[derive(Clone, Copy)]
enum Auth {
    /// The chat service's own header.
    Skype,
    Bearer(&'static str),
    /// A bearer token and the skype token beside it, as the middle tier takes them.
    Both(&'static str),
    /// The skype token as trouter takes it.
    Push,
}

enum Body<'a> {
    None,
    Json(&'a Value),
    Text(&'a str),
}

struct Answer {
    body: Value,
    /// The body as it came, for the one answer that is not JSON.
    text: String,
    /// The `Location` header: where what was created is.
    location: String,
}

struct Teams {
    ctx: Context,
    agent: ureq::Agent,
    session: Session,
    /// Where Microsoft's sign-in service is, and Teams' own front door.
    login: String,
    api: String,
    /// Microsoft Graph, for OneDrive.
    graph: String,
    /// A test moved the front door: the addresses it hands out are its own.
    moved: Option<String>,
    state: Mutex<State>,
    /// Held while a token is minted, and while authz is asked, so two threads do not both ask.
    minting: Mutex<()>,
    settling: Mutex<()>,
    /// When anything was last asked.
    asked: Mutex<Instant>,
}

fn tenants(session: &Value) -> &[Value] {
    session["tenants"].as_array().map_or(&[][..], Vec::as_slice)
}

/// One account per tenant the sign-in reached: the user's own and those they are a guest in.
pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let parsed: Value = serde_json::from_str(session).map_err(|err| err.to_string())?;
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    let mut found: Vec<Signed> = Vec::new();
    for tenant in tenants(&parsed) {
        let (id, refresh) = (text(&tenant["id"]), text(&tenant["refresh_token"]));
        if id.is_empty() || refresh.is_empty() || found.iter().any(|signed| signed.account == id) {
            continue;
        }
        let name = Some(text(&tenant["name"])).filter(|name| !name.is_empty()).unwrap_or_else(|| "Teams".to_owned());
        let session = json!({
            "tenant": id, "name": name, "refresh_token": refresh,
            "client_id": text(&parsed["client_id"]), "user_agent": text(&parsed["user_agent"]),
            "oid": text(&parsed["oid"]), "user": text(&parsed["name"]),
        });
        found.push(Signed {
            account: id,
            name,
            session: session.to_string(),
        });
    }
    if found.is_empty() {
        return Err("the Teams session has no tenant".to_owned());
    }
    Ok(found)
}

/// The session without one tenant, or `None` when it was the last.
pub fn without(session: &str, account: &str) -> Option<String> {
    let mut parsed: Value = serde_json::from_str(session).ok()?;
    let tenants = parsed["tenants"].as_array_mut()?;
    tenants.retain(|tenant| tenant["id"] != account);
    if tenants.is_empty() {
        return None;
    }
    Some(parsed.to_string())
}

/// The stored session with one tenant's refresh token replaced.
fn renewed(stored: &str, tenant: &str, refresh: &str) -> Option<String> {
    let mut parsed: Value = serde_json::from_str(stored).ok()?;
    let entry = parsed["tenants"].as_array_mut()?.iter_mut().find(|entry| entry["id"] == tenant)?;
    entry["refresh_token"] = refresh.into();
    Some(parsed.to_string())
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let mut session: Session = serde_json::from_str(session).map_err(|err| err.to_string())?;
    if session.user_agent.is_empty() {
        session.user_agent = browser().to_owned();
    }
    if session.client_id.is_empty() {
        session.client_id = CLIENT.to_owned();
    }
    let api = address("TINYCLIENT_TEAMS_API", TEAMS).trim_end_matches('/').to_owned();
    let agent = ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent(&session.user_agent)
        .build()
        .into();
    let state = State {
        refresh: session.refresh_token.clone(),
        ..State::default()
    };
    let teams = Arc::new(Teams {
        ctx,
        agent,
        login: address("TINYCLIENT_TEAMS_LOGIN", LOGIN).trim_end_matches('/').to_owned(),
        moved: (api != TEAMS).then(|| api.clone()),
        graph: address("TINYCLIENT_TEAMS_GRAPH", GRAPH).trim_end_matches('/').to_owned(),
        api,
        session,
        state: Mutex::new(state),
        minting: Mutex::new(()),
        settling: Mutex::new(()),
        asked: Mutex::new(Instant::now()),
    });
    let socket = teams.clone();
    std::thread::spawn(move || keep_connected(&socket.ctx, || socket.connect()));
    std::thread::spawn(move || {
        serve_commands(
            &teams.ctx,
            inbox,
            PACE,
            |id| teams.report(teams.backfill(id).map_err(Failure::from)),
            |command| teams.serve(command),
        )
    });
    Ok(())
}

/// A reply chain is a conversation of its own: its channel and the post that started it.
fn chain_id(channel: &str, root: &str) -> String {
    format!("{channel};messageid={root}")
}

fn chain_of(conversation: &str) -> Option<(&str, &str)> {
    conversation.split_once(";messageid=")
}

/// The channel or chat a conversation id is in, a reply chain's included.
fn thread_of(conversation: &str) -> &str {
    chain_of(conversation).map_or(conversation, |(channel, _)| channel)
}

/// Text as one path segment or query value: everything but letters, digits and `-._~` escaped.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// An address without its query, which may carry a signature, for anything that is printed.
fn bare(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// The host of an `https` address, or `None` for anything that is not plainly one.
fn https_host(url: &str) -> Option<&str> {
    let authority = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    authority.split(':').next()
}

fn within(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.'))
}

/// An id Teams writes as text in one place and as a number in another.
fn id_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

/// Milliseconds since 1970 from a message id, which is the time the service took it.
fn millis(id: &str) -> i64 {
    id.parse::<i64>().ok().filter(|ms| *ms > 0).unwrap_or(0)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as i64)
}

/// Unix seconds from `2021-04-11T09:31:49.3620000Z`. 0 for anything else.
fn seconds(iso: &str) -> i64 {
    let number = |range: std::ops::Range<usize>| iso.get(range).and_then(|part| part.parse::<i64>().ok());
    let (Some(year), Some(month), Some(day)) = (number(0..4), number(5..7), number(8..10)) else {
        return 0;
    };
    let (Some(hour), Some(minute), Some(second)) = (number(11..13), number(14..16), number(17..19)) else {
        return 0;
    };
    // Days from the civil date, counting years from March so the leap day falls last.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    days * 86_400 + hour * 3600 + minute * 60 + second
}

/// 16 bytes nobody can guess, as a version 4 UUID.
fn uuid() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut source| source.read_exact(&mut bytes));
    if read.is_err() {
        bytes = (u128::from(now_ms().unsigned_abs()) * 0x9E37_79B9_7F4A_7C15).to_be_bytes();
    }
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// One claim of a token's middle part. Nothing is verified: it is only read for our own id.
fn claim(token: &str, name: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims[name].as_str().map(str::to_owned)
}

impl Teams {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn me(&self) -> String {
        self.state().me.clone()
    }

    fn report(&self, result: Result<(), Failure>) {
        match result {
            Ok(()) => {}
            // The push thread ends the session; a command only notes it.
            Err(Failure::SignedOut(why)) => {
                self.state().expired.get_or_insert(why);
            }
            Err(Failure::Dropped(why)) => tracing::warn!("chatd: Teams: {}", super::net::redact(&why)),
        }
    }

    /// True for an address Teams' own services are at.
    fn ours(&self, url: &str) -> bool {
        self.moved.as_ref().is_some_and(|base| url.starts_with(base.as_str()))
            || (self.graph != GRAPH && url.starts_with(self.graph.as_str()))
            || https_host(url).is_some_and(|host| OURS.iter().any(|domain| within(host, domain)))
    }

    /// True for an address a picture in a message may be fetched from: the media store.
    fn pictures(&self, url: &str) -> bool {
        let media = |host: &str| within(host, "asm.skype.com") || within(host, "asyncgw.teams.microsoft.com");
        self.moved.as_ref().is_some_and(|base| url.starts_with(base.as_str())) || https_host(url).is_some_and(media)
    }

    /// Waits until the last call is [`GAP`] behind.
    fn pace(&self) {
        let mut asked = self.asked.lock().unwrap();
        let since = asked.elapsed();
        if since < GAP {
            sleep(GAP - since);
        }
        *asked = Instant::now();
    }

    // Tokens.

    /// An access token for one audience, minted from the refresh token when the one held is
    /// missing or near its end. Microsoft answers with the next refresh token, which is saved.
    fn token(&self, scope: &'static str) -> Result<String, Fault> {
        let _one = self.minting.lock().unwrap();
        if let Some(token) = self.state().access.get(scope).filter(|token| token.fresh()) {
            return Ok(token.value.clone());
        }
        let refresh = self.state().refresh.clone();
        let url = format!("{}/{}/oauth2/v2.0/token", self.login, escaped(&self.session.tenant));
        let scopes = format!("{scope}/.default openid profile offline_access");
        let form = [
            ("client_id", self.session.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("scope", scopes.as_str()),
        ];
        self.pace();
        let mut response = self.agent.post(&url).send_form(form).map_err(|err| Fault::Net(format!("token: {err}")))?;
        let status = response.status().as_u16();
        let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        if let (200, Some(access)) = (status, body["access_token"].as_str()) {
            let life = body["expires_in"].as_u64().unwrap_or(3600).clamp(60, 86_400);
            let token = Token {
                value: access.to_owned(),
                until: Instant::now() + Duration::from_secs(life),
            };
            let next = body["refresh_token"].as_str().filter(|next| !next.is_empty() && *next != refresh);
            let mut state = self.state();
            state.access.insert(scope, token);
            if let Some(next) = next {
                state.refresh = next.to_owned();
                drop(state);
                let tenant = &self.session.tenant;
                self.ctx.save_session(|stored| renewed(stored, tenant, next));
            }
            return Ok(access.to_owned());
        }
        let error = body["error"].as_str().unwrap_or("");
        // What Entra answers when no retry can help: the grant is gone, or policy forbids it.
        const OVER: &[&str] = &["invalid_grant", "interaction_required", "invalid_client", "unauthorized_client", "consent_required", "login_required"];
        if matches!(status, 400 | 401 | 403) && OVER.contains(&error) {
            let why = ended(error, body["error_description"].as_str().unwrap_or(""));
            self.state().expired = Some(why.clone());
            return Err(Fault::Auth(why));
        }
        Err(Fault::Net(format!("token: {status} {error}")))
    }

    fn forget(&self, auth: Auth) {
        let mut state = self.state();
        match auth {
            Auth::Skype | Auth::Push => state.skype = None,
            Auth::Bearer(scope) => drop(state.access.remove(scope)),
            Auth::Both(scope) => {
                state.access.remove(scope);
                state.skype = None;
            }
        }
    }

    /// The skype token and where this tenant's services are: one call to authz, repeated when
    /// the token nears its end or a service refuses it.
    fn skype(&self) -> Result<(String, Arc<Place>), Fault> {
        let held = || {
            let state = self.state();
            let token = state.skype.as_ref().filter(|token| token.fresh())?;
            Some((token.value.clone(), state.place.clone()?))
        };
        if let Some(held) = held() {
            return Ok(held);
        }
        let _one = self.settling.lock().unwrap();
        if let Some(held) = held() {
            return Ok(held);
        }
        let url = format!("{}/api/authsvc/v1.0/authz", self.api);
        let mut refused = None;
        for _ in 0..2 {
            let bearer = self.token(SPACES)?;
            self.pace();
            let sent = self
                .agent
                .post(&url)
                .header("Authorization", &format!("Bearer {bearer}"))
                .header("Accept", "application/json; ver=1.0")
                .send_empty();
            let mut response = sent.map_err(|err| Fault::Net(format!("authz: {err}")))?;
            let status = response.status().as_u16();
            let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
            if status == 401 {
                self.forget(Auth::Bearer(SPACES));
                refused = Some(Fault::Auth("Teams no longer accepts this sign-in. Sign in again.".to_owned()));
                continue;
            }
            let Some(token) = body["tokens"]["skypeToken"].as_str().filter(|_| status == 200) else {
                let code = body["errorCode"].as_str().unwrap_or("");
                // No licence, or a tenant that keeps Teams from this user: retrying cannot help.
                if status == 403 {
                    let why = format!("This account cannot use Teams ({code}).");
                    self.state().expired = Some(why.clone());
                    return Err(Fault::Auth(why));
                }
                return Err(Fault::Net(format!("authz: {status} {code}")));
            };
            let gtms = &body["regionGtms"];
            let named = |name: &str| gtms[name].as_str().map(|url| url.trim_end_matches('/').to_owned()).filter(|url| self.ours(url));
            let (Some(chat), Some(middle)) = (named("chatService"), named("middleTier")) else {
                return Err(Fault::Net("authz named no chat service of Microsoft's".to_owned()));
            };
            let place = Arc::new(Place {
                chat,
                middle,
                presence: named("unifiedPresence").unwrap_or_else(|| PRESENCE.to_owned()),
            });
            let life = body["tokens"]["expiresIn"].as_u64().unwrap_or(3600).clamp(60, 86_400);
            let me = claim(token, "skypeid").map(|id| format!("8:{id}")).or_else(|| {
                Some(&self.session.oid).filter(|oid| !oid.is_empty()).map(|oid| format!("8:orgid:{oid}"))
            });
            let mut state = self.state();
            state.skype = Some(Token {
                value: token.to_owned(),
                until: Instant::now() + Duration::from_secs(life),
            });
            state.place = Some(place.clone());
            if let Some(me) = me {
                state.me = me;
            }
            return Ok((token.to_owned(), place));
        }
        Err(refused.unwrap_or(Fault::Net("authz".to_owned())))
    }

    fn place(&self) -> Result<Arc<Place>, Fault> {
        self.skype().map(|(_, place)| place)
    }

    // Calls.

    fn headers(&self, auth: Auth) -> Result<Vec<(&'static str, String)>, Fault> {
        Ok(match auth {
            Auth::Skype => vec![("Authentication", format!("skypetoken={}", self.skype()?.0))],
            Auth::Push => vec![("X-Skypetoken", self.skype()?.0)],
            Auth::Bearer(scope) => vec![("Authorization", format!("Bearer {}", self.token(scope)?))],
            Auth::Both(scope) => {
                vec![("Authorization", format!("Bearer {}", self.token(scope)?)), ("X-Skypetoken", self.skype()?.0)]
            }
        })
    }

    fn send(&self, method: &str, url: &str, headers: &[(&'static str, String)], body: &Body) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        macro_rules! dressed {
            ($request:expr) => {{
                let mut request = $request
                    .header("Accept", "application/json")
                    .header("BehaviorOverride", "redirectAs404")
                    .header("Origin", TEAMS)
                    .header("Referer", "https://teams.microsoft.com/");
                for (name, value) in headers {
                    request = request.header(*name, value.as_str());
                }
                request
            }};
        }
        match (method, body) {
            ("GET", _) => dressed!(self.agent.get(url)).call(),
            ("DELETE", Body::Json(body)) => dressed!(self.agent.delete(url)).force_send_body().send_json(*body),
            ("DELETE", _) => dressed!(self.agent.delete(url)).call(),
            ("PUT", Body::Json(body)) => dressed!(self.agent.put(url)).send_json(*body),
            ("PUT", _) => dressed!(self.agent.put(url)).send_empty(),
            (_, Body::Json(body)) => dressed!(self.agent.post(url)).send_json(*body),
            (_, Body::Text(text)) => dressed!(self.agent.post(url)).header("Content-Type", "application/json").send(*text),
            (_, Body::None) => dressed!(self.agent.post(url)).send_empty(),
        }
    }

    /// One call to a Teams service. A token the service refuses is minted again once; a short
    /// rate limit is waited out when `patient`, and a send is not: the user is told instead.
    fn call(&self, method: &str, url: &str, auth: Auth, body: Body, patient: bool) -> Result<Answer, Fault> {
        if let Some(why) = self.state().expired.clone() {
            return Err(Fault::Auth(why));
        }
        if !self.ours(url) {
            return Err(Fault::Net(format!("{} is not a Teams address", bare(url))));
        }
        let (mut renewed, mut limited, mut wait) = (false, false, 0);
        for _ in 0..3 {
            let headers = self.headers(auth)?;
            self.pace();
            // The library's word for a bad address repeats the address, which may be signed.
            let failed = |err: ureq::Error| match err {
                ureq::Error::BadUri(_) => Fault::Net(format!("{}: not an address", bare(url))),
                err => Fault::Net(format!("{}: {err}", bare(url))),
            };
            let mut response = self.send(method, url, &headers, &body).map_err(failed)?;
            let status = response.status().as_u16();
            let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
            let (retry, location) = (header("retry-after").parse::<u64>().ok(), header("location"));
            let text = response.body_mut().read_to_string().unwrap_or_default();
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            match status {
                200..=299 => return Ok(Answer { body, text, location }),
                401 if !renewed => {
                    renewed = true;
                    self.forget(auth);
                }
                // Waited out once at most: a second 429 fails the call.
                429 => {
                    wait = retry.unwrap_or(LIMITED_FOR);
                    if limited || !patient || wait > PATIENCE {
                        break;
                    }
                    limited = true;
                    sleep(Duration::from_secs(wait));
                }
                _ => {
                    let message = [&body["message"], &body["errorCode"], &body["standardizedError"]["errorDescription"]];
                    let message = message.into_iter().filter_map(Value::as_str).next().unwrap_or("");
                    return Err(Fault::Refused {
                        status,
                        message: message.chars().take(120).collect(),
                    });
                }
            }
        }
        Err(Fault::Limited(wait))
    }

    /// A call to the chat service, under its own address.
    fn chat(&self, method: &str, path: &str, body: Body, patient: bool) -> Result<Answer, Fault> {
        let url = format!("{}{path}", self.place()?.chat);
        self.call(method, &url, Auth::Skype, body, patient)
    }

    // The connection.

    /// One connection, from the first token to the push socket dropping.
    fn connect(&self) -> Failure {
        match self.run() {
            Ok(()) => Failure::Dropped("stopped".to_owned()),
            Err(failure) => failure,
        }
    }

    fn run(&self) -> Result<(), Failure> {
        // A sign-in that was replaced starts clean; one that is over stays over.
        self.skype()?;
        self.ctx.describe(&self.session.name, &self.me())?;
        let mut push = self.open()?;
        self.ctx.link(Link::Connected);
        // Files are off until they can be shared with the chat's members only. The OneDrive path
        // below made an organization-wide link, which is not what the Teams app does and shows
        // up in a company's audit logs.
        self.ctx.capabilities(protocol::Capabilities { max_file_bytes: 0, max_files: 0, ..protocol::Capabilities::default() });
        {
            let mut state = self.state();
            state.watched.clear();
            state.resync = true;
        }
        // The teams and chats are the command thread's to list, paced, so push never waits.
        let _ = self.ctx.commands.send(Command::OpenSpace(String::new()));
        while !self.ctx.stopped() {
            if let Some(why) = self.state().expired.clone() {
                return Err(Failure::SignedOut(why));
            }
            self.pump(&mut push)?;
        }
        Ok(())
    }

    // Commands.

    fn serve(self: &Arc<Self>, command: Command) {
        let refuse = |fault: Fault| Refusal::Refused(reason(fault));
        match command {
            Command::History { conversation_id, before } => self.report(self.older(&conversation_id, &before).map_err(Failure::from)),
            Command::HistoryAfter { conversation_id, after } => self.report(self.after(&conversation_id, &after).map_err(Failure::from)),
            Command::HistoryAround { conversation_id, around } => self.report(self.around(&conversation_id, &around).map_err(Failure::from)),
            Command::OpenSpace(_) | Command::OpenHome => self.report(self.open_space().map_err(Failure::from)),
            Command::MarkRead { conversation_id, message_id } => self.report(self.mark_read(&conversation_id, &message_id).map_err(Failure::from)),
            Command::Send { conversation_id, client_id, text, reply_to } => {
                self.post(&conversation_id, &client_id, &text, reply_to.as_deref())
            }
            Command::React { conversation_id, message_id, emoji, on, reply } => {
                let _ = reply.send(self.react(&conversation_id, &message_id, &emoji, on).map_err(refuse));
            }
            Command::ReactionRules { reply, .. } => drop(reply.send(Ok(message::reaction_rules()))),
            Command::Edit { conversation_id, message_id, text, reply } => {
                let _ = reply.send(self.edit(&conversation_id, &message_id, &text).map_err(refuse));
            }
            Command::Delete { conversation_id, message_id, reply } => {
                let _ = reply.send(self.delete(&conversation_id, &message_id).map_err(refuse));
            }
            Command::Typing(conversation) => self.typing(&conversation),
            Command::Watch(_) => self.report(self.watch().map_err(Failure::from)),
            Command::SearchPeople(query) => {
                self.report(self.search(&query).map_err(Failure::from));
                self.ctx.searched(&query);
            }
            Command::Members { conversation_id, offset, limit, reply } => {
                let _ = reply.send(self.members(&conversation_id, offset, limit).map_err(refuse));
            }
            Command::StartDirect { person_id, reply } => {
                let _ = reply.send(self.start_direct(&person_id).map_err(refuse));
            }
            Command::Profile { person_id, reply } => {
                let _ = reply.send(self.profile(&person_id).map_err(refuse));
            }
            Command::ConversationInfo { conversation_id, reply } => {
                let _ = reply.send(self.conversation_info(&conversation_id).map_err(refuse));
            }
            Command::SetMute { conversation_id, notify, reply } => {
                let _ = reply.send(self.set_mute(&conversation_id, notify).map_err(refuse));
            }
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                self.send_with(&conversation_id, &client_id, &text, reply_to.as_deref(), *with);
            }
            Command::CancelSend { client_id, .. } => self.cancel_send(&client_id),
            // Each row's line comes with the chat list (`lastMessage`); a row still without one
            // has no message, or one the list did not show.
            Command::Previews(_) => {}
            Command::SetPresence { presence, status, reply } => {
                let _ = reply.send(self.set_presence(presence, status.as_deref()).map_err(refuse));
            }
            Command::Download { url, reply, .. } => {
                let _ = reply.send(self.download(url).map_err(refuse));
            }
            Command::Handoff { conversation_id, reply } => {
                let _ = reply.send(Ok(self.call_link(&conversation_id)));
            }
            // The refresh token is Microsoft's to revoke, from the account's own security
            // page; no client read ends it from here. Signing out forgets it.
            Command::SignOut(reply) => drop(reply.send(Ok(()))),
            command => command.unsupported(&self.ctx),
        }
    }

    /// How the media cache fetches a registered file: the media store takes the skype token,
    /// a person's picture the bearer token as the web client's cookie.
    fn download(&self, url: String) -> Result<Download, Fault> {
        let mut headers = vec![("User-Agent".to_owned(), self.session.user_agent.clone())];
        if self.pictures(&url) {
            headers.push(("Authorization".to_owned(), format!("skype_token {}", self.skype()?.0)));
        } else if self.ours(&url) {
            let cookie = format!("authtoken=Bearer={}&Origin={TEAMS}", self.token(SPACES)?);
            headers.push(("Cookie".to_owned(), cookie));
            headers.push(("Referer".to_owned(), "https://teams.microsoft.com/".to_owned()));
        }
        Ok(Download { url, headers })
    }

    /// Tells the conversation the user is typing, at most once per [`TYPING_EVERY`].
    fn typing(&self, conversation: &str) {
        {
            let mut state = self.state();
            let recent = |(place, at): &(String, Instant)| place == conversation && at.elapsed() < TYPING_EVERY;
            if state.typed.as_ref().is_some_and(recent) {
                return;
            }
            state.typed = Some((conversation.to_owned(), Instant::now()));
        }
        let body = json!({"content": "", "contenttype": "Application/Message", "messagetype": "Control/Typing"});
        let path = format!("/v1/users/ME/conversations/{}/messages", escaped(conversation));
        if let Err(Fault::Auth(why)) = self.chat("POST", &path, Body::Json(&body), false) {
            self.state().expired.get_or_insert(why);
        }
    }

    fn mark_read(&self, conversation: &str, message: &str) -> Result<(), Fault> {
        // A message of ours that Teams has not named yet has no place in its order.
        if millis(message) < 1_000_000_000_000 {
            return Ok(());
        }
        // Teams keeps one marker for a channel, its reply chains included.
        let path = format!("/v1/users/ME/conversations/{}/properties?name=consumptionhorizon", escaped(thread_of(conversation)));
        let horizon = json!({"consumptionhorizon": format!("{message};{};{message}", now_ms())});
        self.chat("PUT", &path, Body::Json(&horizon), true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> String {
        json!({
            "client_id": CLIENT, "user_agent": "Safari/1", "oid": "me-oid", "name": "Ana Ruiz",
            "tenants": [
                {"id": "11111111-aaaa", "name": "Contoso", "refresh_token": "rt-home"},
                {"id": "22222222-bbbb", "name": "", "refresh_token": "rt-guest"},
                {"id": "33333333-cccc", "name": "Not redeemed", "refresh_token": ""},
            ],
        })
        .to_string()
    }

    #[test]
    fn one_sign_in_is_one_account_per_tenant_and_each_can_be_removed() {
        let found = accounts(&session()).unwrap();
        let listed: Vec<(&str, &str)> = found.iter().map(|signed| (signed.account.as_str(), signed.name.as_str())).collect();
        assert_eq!(listed, [("11111111-aaaa", "Contoso"), ("22222222-bbbb", "Teams")]);
        let guest: Session = serde_json::from_str(&found[1].session).unwrap();
        assert_eq!((guest.tenant.as_str(), guest.refresh_token.as_str()), ("22222222-bbbb", "rt-guest"));
        assert_eq!((guest.client_id.as_str(), guest.user_agent.as_str(), guest.oid.as_str()), (CLIENT, "Safari/1", "me-oid"));

        let left = without(&session(), "11111111-aaaa").unwrap();
        let ids: Vec<String> = accounts(&left).unwrap().into_iter().map(|signed| signed.account).collect();
        assert_eq!(ids, ["22222222-bbbb"]);
        assert_eq!(without(&without(&left, "33333333-cccc").unwrap(), "22222222-bbbb"), None);
        assert!(accounts("{}").is_err() && accounts("not json").is_err());
    }

    #[test]
    fn a_renewed_refresh_token_replaces_only_its_tenants() {
        let saved = renewed(&session(), "22222222-bbbb", "rt-next").unwrap();
        let found = accounts(&saved).unwrap();
        let tokens: Vec<String> = found.iter().map(|signed| serde_json::from_str::<Session>(&signed.session).unwrap().refresh_token).collect();
        assert_eq!(tokens, ["rt-home", "rt-next"]);
        assert_eq!(renewed(&session(), "no-such-tenant", "rt-next"), None);
    }

    #[test]
    fn ids_times_and_addresses_are_read_as_teams_writes_them() {
        assert_eq!(chain_of("19:general@thread.tacv2;messageid=1760000200200"), Some(("19:general@thread.tacv2", "1760000200200")));
        assert_eq!(chain_of("19:a_b@unq.gbl.spaces"), None);
        assert_eq!(thread_of(&chain_id("19:x@thread.tacv2", "17")), "19:x@thread.tacv2");
        assert_eq!(escaped("19:x@thread.tacv2;messageid=1"), "19%3Ax%40thread.tacv2%3Bmessageid%3D1");
        assert_eq!(seconds("2021-04-11T09:31:49.3620000Z"), 1_618_133_509);
        assert_eq!(seconds("2024-02-29T23:59:59Z"), 1_709_251_199);
        assert_eq!(seconds("soon"), 0);
        assert_eq!((millis("1618133509362"), millis("abc"), millis("-4")), (1_618_133_509_362, 0, 0));
        assert_eq!(bare("https://x.trouter.teams.microsoft.com/v4/f/abc?sig=secret"), "https://x.trouter.teams.microsoft.com/v4/f/abc");
        assert_eq!(https_host("https://teams.microsoft.com:pw@evil.example/a"), None);
        assert!(within("emea.ng.msg.teams.microsoft.com", "teams.microsoft.com"));
        assert!(!within("notteams.microsoft.com.evil.example", "teams.microsoft.com"));
        let id = uuid();
        assert_eq!(id.split('-').map(str::len).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(id.split('-').nth(2).unwrap().starts_with('4'));
    }

    #[test]
    fn our_own_id_is_read_from_the_skype_token() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"skypeid":"orgid:0000-me","exp":1}"#);
        assert_eq!(claim(&format!("e30.{payload}.sig"), "skypeid").as_deref(), Some("orgid:0000-me"));
        assert_eq!(claim("opaque", "skypeid"), None);
    }

    #[test]
    fn a_refused_renewal_says_whether_signing_in_again_can_help() {
        let policy = ended("invalid_grant", "AADSTS53003: Access has been blocked by Conditional Access policies. Trace ID: x");
        assert!(policy.contains("AADSTS53003") && policy.contains("organization"), "{policy}");
        let gone = ended("invalid_grant", "AADSTS700082: The refresh token has expired due to inactivity.");
        assert_eq!(gone, "Microsoft ended this sign-in (AADSTS700082). Sign in again.");
        assert_eq!(ended("invalid_grant", ""), "Microsoft ended this sign-in (invalid_grant). Sign in again.");
        assert_eq!(reason(Fault::Limited(7)), "Teams is rate limiting you. Try again in 7 s.");
        assert_eq!(reason(Fault::Refused { status: 403, message: "x".into() }), "Teams does not let you do that here.");
    }
}
