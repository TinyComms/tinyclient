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
//! - A personal Microsoft account is a second sign-in: client id `8ec6bc83-69c8-4392-8f08-b3c986009232`,
//!   authority `consumers`, scope `https://mtsvc.fl.teams.microsoft.com/teams.mt.readwrite`, and
//!   `POST https://teams.live.com/api/auth/v1.0/authz/consumer`. Entra guest access stays elsewhere.
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
//!   The key is the catalog's (`like`, `1f389_partypopper`). A skin tone is
//!   `{base}-toneN`, and a thumbs-up's toned base is `yes`, not `like`
//!   [C: mautrix-teams d02a207].
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
//! [C]. The bytes are Graph's documented `GET /shares/u!{base64url}/driveItem/content`, with
//! `Prefer: redeemSharingLinkIfNecessary` [C: mautrix]. Graph redirects to SharePoint, and
//! that hop does not get the Graph bearer. A document's name stays a link to the sharing address.
//!
//! Push is trouter [C: purple-teams, mautrix, ost]: `POST go.trouter.teams.microsoft.com/v4/a`,
//! a socket.io 0.9 session, a websocket, `user.authenticate`, and a registration at
//! `teams.microsoft.com/registrar/prod/V2/registrations` that points the chat service at it.
//! Events arrive as requests to answer (`3:::`), `EventMessage` with `NewMessage` or
//! `MessageUpdate` [C]. The chat service's own long poll is gone (purple-teams 58c649f, Apr
//! 2025), so there is no fallback. A read marker moved elsewhere (`ConversationUpdate`) [U].
//! `user.activity` (`active` / `inactive`) is sent only while the window is focused AND receiving
//! recent input (and the per-account `presence_activity` Privacy setting is on), heartbeat every
//! 240 s; when unfocused, idle, or opted out it sends `inactive` so mobile notifications stay normal.
//!
//! Deep links: `…/l/meetup-join/{thread}/0` [C]; `msteams:/l/call/0/0?users=`, `/l/chat/…`,
//! `/l/channel/…` and `/l/message/…` are Microsoft's documented forms, in no client read [U].
//!
//! For tests (`tests/teams.rs`), read through `net::address`: `TINYCLIENT_TEAMS_LOGIN` replaces
//! `https://login.microsoftonline.com`, `TINYCLIENT_TEAMS_API` replaces
//! `https://teams.microsoft.com` (authz, the teams list, the registrar; the addresses authz
//! then hands out must be under it), `TINYCLIENT_TEAMS_TROUTER` replaces
//! `https://go.trouter.teams.microsoft.com`. The session comes from `TINYCLIENT_SESSION_TEAMS`.

mod card;
pub(crate) mod cloud;
mod emoji;
mod html;

pub(crate) use cloud::{cloud_for, TeamsCloud};
use cloud::Audience;

/// The lab's teams-markdown driver. A build without this returns to the server and stays up.
pub fn prove_teams_markdown() -> Result<(), String> {
    html::prove_markdown()
}
mod info;
mod call_log;
mod meeting_media;
mod meeting_create;
mod links;
pub(crate) mod message;

/// The lab's teams-reactions driver. A build without this returns to the server and stays up.
pub fn prove_reactions() -> Result<(), String> {
    message::prove_reactions()
}

/// The lab's teams-loop-component driver. A build without this returns to the server and stays up.
pub fn prove_loop_component() -> Result<(), String> {
    message::prove_loop_component()
}

/// The lab's teams-file-download driver. A build without this returns to the server and stays up.
pub fn prove_shared_file() -> Result<(), String> {
    let share = "https://contoso.sharepoint.com/:x:/s/Platform/EaBcDeF";
    let id = share_id(share);
    if !id.starts_with("u!") || id.contains(['+', '/', '=']) {
        return Err(format!("a share id is not unpadded base64url: {id}"));
    }
    let url = graph_content(GRAPH, share);
    if url != format!("{GRAPH}/v1.0/shares/{id}/driveItem/content") {
        return Err(format!("a shared file is not fetched from Graph: {url}"));
    }
    let kind = |name| message::shared_kind(name);
    if kind("Q4 plan.xlsx") != protocol::MediaKind::File || kind("board.png") != protocol::MediaKind::Image || kind("walk.mp4") != protocol::MediaKind::Video {
        return Err("a shared picture is still listed as a document".to_owned());
    }
    Ok(())
}

/// The lab's teams-team-info driver. A build without this returns to the server and stays up.
pub fn prove_team_info() -> Result<(), String> {
    people::prove_team_roster()
}
mod people;
mod picker;
mod push;
mod search;
mod send;
mod tags;
mod tree;

/// The lab's teams-chain-paging driver. A build without this returns to the server and stays up.
pub fn prove_chain_paging() -> Result<(), String> {
    message::prove_chain_paging()
}

/// The lab's teams-copy-link driver. A build without this returns to the server and stays up.
pub fn prove_share_links() -> Result<(), String> {
    links::prove_share_links()
}

/// What a Teams account may send. Files stay off: the old OneDrive path made an
/// organization-wide link.
pub(crate) fn capability_ceiling() -> protocol::Capabilities {
    protocol::Capabilities {
        max_file_bytes: 250 * 1024 * 1024,
        max_files: protocol::ATTACHMENTS_LIMIT as u32,
        reply: true,
        edit: true,
        delete: true,
        threads: true,
        pins: true,
        save_messages: true,
        activity_feed: true,
        search: true,
        search_global: true,
        groups: 250,
        blocks: true,
        browse_channels: true,
        join_space_by_invite: true,
        leave_space: true,
        pin_chats: 15,
        close_direct: true,
        clear_history: true,
        suppress_embeds: true,
        schedule: true,
        voice_notes: true,
        gifs: true,
        stickers: true,
        translate: true,
        forward: true,
        reactions: true,
        mention_format: protocol::MentionFormat::Teams,
        mention_spans: true,
        importance: true,
        channel_subject: true,
        out_of_office: true,
        polls: Some(protocol::PollCapabilities {
            max_options: 10,
            max_duration_secs: 0,
            multiple: true,
            anonymous: true,
        }),
        read_by: true,
        ..protocol::Capabilities::default()
    }
}

/// A team channel's call opens in Teams.
pub(crate) fn channel_call() -> protocol::CallSupport {
    protocol::CallSupport::Handoff
}

/// A chat opens in Teams, except a bot and the user's own Notes.
pub(crate) fn chat_call(bot: bool, notes: bool) -> protocol::CallSupport {
    if bot || notes { protocol::CallSupport::None } else { protocol::CallSupport::Handoff }
}

use super::net::{address, browser};
use super::{keep_connected, serve_commands, Command, Context, Download, Failure, Refusal, Signed};
use base64::Engine;
use people::Known;
use protocol::Link;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Teams' own desktop client, the one registration third-party clients sign in as.
const CLIENT: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
/// The client a personal Microsoft account signs in as (Teams free).
const PERSONAL_CLIENT: &str = "8ec6bc83-69c8-4392-8f08-b3c986009232";
/// The one scope a personal account's spaces token is asked for. Work accounts use `/.default`.
const PERSONAL_SCOPE: &str = "https://mtsvc.fl.teams.microsoft.com/teams.mt.readwrite openid profile offline_access";

/// The scope string minted for one audience. Personal spaces is the consumer read-write scope.
/// Every other audience stays `{audience}/.default`.
fn spaces_scope(cloud: TeamsCloud, scope: Audience, audience: &str) -> String {
    if cloud == TeamsCloud::Personal && scope == Audience::Spaces {
        PERSONAL_SCOPE.to_owned()
    } else {
        format!("{audience}/.default openid profile offline_access")
    }
}

/// The consumer spaces scope, for the accounts-wave13 prove hook.
pub(crate) fn personal_scope() -> &'static str {
    PERSONAL_SCOPE
}

/// Where a personal account asks for its skype token.
pub(crate) fn consumer_authz(api: &str) -> String {
    format!("{}/api/auth/v1.0/authz/consumer", api.trim_end_matches('/'))
}
const TEAMS: &str = cloud::WORLDWIDE.teams;
/// Microsoft Graph default (worldwide), kept for `prove_shared_file`.
const GRAPH: &str = cloud::WORLDWIDE.graph;
/// Microsoft Search (Substrate) audience for server-side Teams search.
const SUBSTRATE: &str = "https://substrate.office.com";
/// Hosts a Microsoft Search (Substrate) token may be sent to.
const SUBSTRATE_HOSTS: &[&str] = &[
    "substrate.office.com",
    "teams.microsoft.com",
    "teams.cloud.microsoft",
];

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
/// Tight timeout for Teams' synchronous link-preview unfurl call before posting.
pub(super) const URLPREVIEW_TIMEOUT: Duration = Duration::from_millis(1500);
/// Tight timeout for typing indicator POSTs so slow responses never back up.
const TYPING_TIMEOUT: Duration = Duration::from_secs(5);
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
    /// Microsoft 365 cloud (`worldwide`, `gcc_high`, `dod`, `china`, `personal`).
    #[serde(default)]
    cloud: String,
    /// A personal Microsoft account. The consumer host and client id follow from this.
    #[serde(default)]
    personal: bool,
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
        Fault::Refused { status: 403, message } => {
            if message.contains("moderator")
                || message.contains("Only channel")
                || message.contains("Only team")
                || message.contains("organization's policy")
                || message.contains("last owner")
            {
                message
            } else {
                "Teams does not let you do that here.".to_owned()
            }
        }
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
    urlp: Option<String>,
    search: Option<String>,
    ams: Option<String>,
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
    access: HashMap<Audience, Token>,
    skype: Option<Token>,
    place: Option<Arc<Place>>,
    /// Our own id, `8:orgid:…`.
    me: String,
    /// Set by any call that finds the session over, so the push thread ends it.
    expired: Option<String>,
    /// Latest `clientTimestamp` (ms) written or applied per conversation for `consumptionhorizon`,
    /// so stale out-of-order `ConversationUpdate` echoes cannot rewind a newer local read.
    horizon_ms: HashMap<String, i64>,
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
    /// Subject and inline image HTML remembered for our own messages so edits keep them.
    own_meta: HashMap<(String, String), message::OwnMeta>,
    /// Raw `properties.files` JSON remembered for messages with shared files so forwarding
    /// references them without re-uploading.
    file_props: HashMap<(String, String), Vec<Value>>,
    /// Chat ids pinned in Teams, in Teams' own order.
    pinned_chats: Vec<String>,
    /// Pinned messages per conversation/thread id.
    pinned_messages: HashMap<String, Vec<info::PinnedEntry>>,
    /// Saved (bookmarked) message ids for this account.
    saved_ids: HashSet<String>,
    /// Channel ids pinned in Teams, in Teams' own order.
    pinned_channels: Vec<String>,
    /// Per-conversation notification level as reported by Teams.
    notify: HashMap<String, protocol::Notify>,
    /// Pending scheduled messages: raw Teams message id -> scheduled unix seconds.
    scheduled: HashMap<String, i64>,
    /// Active trouter registration endpoint id, removed on sign-out.
    endpoint: Option<String>,
    /// Adaptive Card invoke metadata for poll messages: `(conversation, message_id)` -> `PollInvoke`.
    polls: HashMap<(String, String), card::PollInvoke>,
    /// Team tags per Space id, and tag ids the signed-in user belongs to.
    tags: HashMap<String, Vec<protocol::TeamTag>>,
    my_tags: HashSet<String>,
    /// Newest activity item timestamp (ms) marked read across the `48:notifications` stream.
    activity_read_horizon: i64,
    /// Individual activity item ids marked read locally / on Teams.
    activity_read_ids: HashSet<String>,
    /// Mapping from activity item id -> `(thread_id, source_message_id)` for syncing read state.
    activity_targets: HashMap<String, (String, String)>,
    /// Newest activity item id seen from `48:notifications`.
    activity_newest_ms: i64,
    /// When the last server-side search ran, for separate pacing.
    search_at: Option<Instant>,
    /// Blocked user MRIs on Teams.
    blocked: HashSet<String>,
    /// Last listed teams (Spaces), kept so leaving one updates the Spaces list cleanly.
    spaces: Vec<crate::store::SpaceRow>,
    /// Activity feed item ids marked read locally this session.
    activity_read: HashSet<String>,
    /// Per-thread member consumption horizons (thread_id -> member_id -> arrival_ms), excluding me.
    member_horizons: HashMap<String, HashMap<String, i64>>,
    /// Tenant policy flags learned from `authz` settings.
    giphy_disabled: bool,
    stickers_disabled: bool,
    translation_disabled: bool,
    /// Dimensions and alt titles remembered for GIFs and stickers from the picker catalog.
    catalog_meta: HashMap<String, (u32, u32, String)>,
    /// Meeting chats and calendar events remembered this run, by id.
    meetings: HashMap<String, protocol::MeetingEvent>,
    voicemail_next: Option<String>,
    transcript_token: Option<(String, Token)>,
}

/// What a call presents.
#[derive(Clone, Copy)]
enum Auth {
    /// The chat service's own header.
    Skype,
    /// AMS media store header (`Authorization: skype_token ...`).
    Ams,
    Bearer(Audience),
    /// A bearer token and the skype token beside it, as the middle tier takes them.
    Both(Audience),
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

/// Per-audience mint locks so a slow Graph/Substrate/Presence/Aggregator token mint never blocks
/// `Spaces` sends, and two threads for the same audience still coalesce into one mint.
#[derive(Default)]
struct MintLocks {
    spaces: Mutex<()>,
    aggregator: Mutex<()>,
    presence: Mutex<()>,
    graph: Mutex<()>,
    substrate: Mutex<()>,
    pub(super) transcript: Mutex<()>,
}

impl MintLocks {
    fn for_audience(&self, scope: Audience) -> &Mutex<()> {
        match scope {
            Audience::Spaces => &self.spaces,
            Audience::Aggregator => &self.aggregator,
            Audience::Presence => &self.presence,
            Audience::Graph => &self.graph,
            Audience::Substrate => &self.substrate,
        }
    }
}

/// Coalesced single-slot state for the `"teams-typing"` worker.
#[derive(Default)]
struct TypingSlot {
    pending: Option<String>,
    stopped: bool,
}

struct Teams {
    ctx: Context,
    agent: ureq::Agent,
    session: Session,
    /// Sovereign/commercial cloud table for this tenant.
    cloud: TeamsCloud,
    /// Where Microsoft's sign-in service is, and Teams' own front door.
    login: String,
    api: String,
    /// Microsoft Graph, for OneDrive.
    graph: String,
    /// Trouter push entry host.
    trouter: String,
    /// A test moved the front door: the addresses it hands out are its own.
    moved: Option<String>,
    state: Mutex<State>,
    /// Per-audience mutexes held while minting, and while authz is asked, so two threads do not
    /// duplicate the same mint while independent audiences proceed concurrently.
    minting: MintLocks,
    settling: Mutex<()>,
    /// Independent pace slots: background sync vs interactive sends.
    asked: Mutex<Option<Instant>>,
    send_asked: Mutex<Option<Instant>>,
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
        let cloud = Some(text(&tenant["cloud"]))
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| text(&parsed["cloud"]));
        let own_client = text(&tenant["client_id"]);
        let client = if own_client.is_empty() { text(&parsed["client_id"]) } else { own_client };
        let personal = tenant["personal"].as_bool().unwrap_or(false) || cloud == "personal";
        let mut session = json!({
            "tenant": id, "name": name, "refresh_token": refresh,
            "client_id": client, "user_agent": text(&parsed["user_agent"]),
            "oid": text(&parsed["oid"]), "user": text(&parsed["name"]),
            "cloud": cloud,
        });
        if personal {
            session["personal"] = json!(true);
        }
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

/// Writes a session's top-level cloud onto each tenant that does not name one, so a later
/// personal sign-in cannot relabel a work tenant.
fn stamp(session: &mut Value) {
    let cloud = session["cloud"].as_str().unwrap_or("").to_owned();
    let personal = session["personal"].as_bool().unwrap_or(false) || cloud == "personal";
    let Some(list) = session["tenants"].as_array_mut() else {
        return;
    };
    for tenant in list {
        if !cloud.is_empty() && tenant["cloud"].as_str().unwrap_or("").is_empty() {
            tenant["cloud"] = json!(cloud);
        }
        if personal && tenant["personal"].as_bool() != Some(false) && tenant["cloud"].as_str() == Some("personal") {
            tenant["personal"] = json!(true);
        }
    }
}

/// A finished sign-in joins the tenants already stored. The same tenant replaces its own
/// refresh token. An empty refresh token is not a tenant. A work client id already stored is
/// left as it is; a personal tenant carries its own client id.
pub fn combine(existing: &str, fresh: &str) -> Result<String, String> {
    let mut base: Value = serde_json::from_str(existing).map_err(|err| err.to_string())?;
    let mut incoming: Value = serde_json::from_str(fresh).map_err(|err| err.to_string())?;
    stamp(&mut base);
    stamp(&mut incoming);
    if let Some(agent) = incoming["user_agent"].as_str().filter(|agent| !agent.is_empty()) {
        base["user_agent"] = json!(agent);
    }
    for key in ["oid", "name", "upn"] {
        if base[key].as_str().unwrap_or("").is_empty() {
            if let Some(value) = incoming[key].as_str().filter(|value| !value.is_empty()) {
                base[key] = json!(value);
            }
        }
    }
    if base["client_id"].as_str().unwrap_or("").is_empty() {
        if let Some(value) = incoming["client_id"].as_str().filter(|value| !value.is_empty()) {
            base["client_id"] = json!(value);
        }
    }
    let mut kept = tenants(&base).to_vec();
    for tenant in tenants(&incoming) {
        let id = tenant["id"].as_str().unwrap_or("");
        let refresh = tenant["refresh_token"].as_str().unwrap_or("");
        if id.is_empty() || refresh.is_empty() {
            continue;
        }
        kept.retain(|one| one["id"].as_str() != Some(id));
        kept.push(tenant.clone());
    }
    if kept.is_empty() {
        return Err("the Teams session has no tenant".to_owned());
    }
    base["tenants"] = Value::Array(kept);
    Ok(base.to_string())
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
    if session.personal || session.cloud == "personal" {
        session.personal = true;
        session.cloud = "personal".to_owned();
    }
    if session.client_id.is_empty() {
        session.client_id = if session.personal { PERSONAL_CLIENT } else { CLIENT }.to_owned();
    }
    let cloud = TeamsCloud::parse(&session.cloud);
    let ep = cloud.endpoints();
    cloud::register(&ctx.account, cloud);
    let api = address("TINYCLIENT_TEAMS_API", ep.teams).trim_end_matches('/').to_owned();
    let agent = crate::adapters::net::apply_proxy(ureq::Agent::config_builder())
        .tls_config(crate::tls::config())
        .max_idle_age(Duration::from_secs(30))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
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
        cloud,
        login: address("TINYCLIENT_TEAMS_LOGIN", ep.login).trim_end_matches('/').to_owned(),
        moved: (api != ep.teams).then(|| api.clone()),
        graph: address("TINYCLIENT_TEAMS_GRAPH", ep.graph).trim_end_matches('/').to_owned(),
        trouter: address("TINYCLIENT_TEAMS_TROUTER", ep.trouter).trim_end_matches('/').to_owned(),
        api,
        session,
        state: Mutex::new(state),
        minting: MintLocks::default(),
        settling: Mutex::new(()),
        asked: Mutex::new(None),
        send_asked: Mutex::new(None),
    });
    let socket = teams.clone();
    std::thread::spawn(move || keep_connected(&socket.ctx, || socket.connect()));
    let _ = std::thread::Builder::new().name("teams".into()).spawn(move || teams.run_workers(inbox));
    Ok(())
}

/// Whether a `ureq` error indicates a stale pooled socket dropped under us.
fn dropped_socket(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::ConnectionFailed => true,
        ureq::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

/// Methods safe to retry once on a dropped pooled socket: idempotent GETs and
/// `clientmessageid`-keyed message POSTs.
fn retryable_on_drop(method: &str, body: &Body) -> bool {
    if method == "GET" {
        return true;
    }
    if method == "POST" {
        if let Body::Json(val) = body {
            return val.get("clientmessageid").and_then(Value::as_str).is_some_and(|id| !id.is_empty());
        }
    }
    false
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

/// Graph's share id: `u!` plus the sharing URL in unpadded base64url.
fn share_id(url: &str) -> String {
    format!("u!{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_bytes()))
}

/// Where the media cache fetches a sharing link. Graph answers the bytes, or redirects to them.
fn graph_content(graph: &str, share_url: &str) -> String {
    format!("{}/v1.0/shares/{}/driveItem/content", graph.trim_end_matches('/'), share_id(share_url))
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

/// ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SS.000Z`) from Unix seconds.
pub(super) fn iso_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400) + 719_468;
    let secs = unix_secs.rem_euclid(86_400);
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let hour = secs / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

/// 16 bytes nobody can guess, as a version 4 UUID.
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
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
        crate::hub::lock(&self.state)
    }

    /// Non-blocking front-end router decoupling sends, file uploads, cancels, typing, and
    /// downloads from `serve_commands`.
    fn run_workers(self: &Arc<Self>, inbox: Receiver<Command>) {
        let (send_tx, send_rx) = std::sync::mpsc::channel::<Command>();
        let (download_tx, download_rx) = std::sync::mpsc::channel::<Command>();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Command>();
        let typing_slot = Arc::new((Mutex::new(TypingSlot::default()), Condvar::new()));

        let send_teams = self.clone();
        let _ = std::thread::Builder::new().name("teams-send".into()).spawn(move || {
            while !send_teams.ctx.stopped() {
                let Ok(command) = send_rx.recv() else { break };
                if matches!(command, Command::Stop) {
                    break;
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match command {
                    Command::Send { conversation_id, client_id, text, reply_to } => {
                        send_teams.post(&conversation_id, &client_id, &text, reply_to.as_deref());
                    }
                    Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                        send_teams.send_with(&conversation_id, &client_id, &text, reply_to.as_deref(), *with);
                    }
                    _ => {}
                }));
                if outcome.is_err() {
                    tracing::error!("chatd: Teams: send worker panicked");
                }
            }
        });

        let typing_teams = self.clone();
        let typing_rx = typing_slot.clone();
        let _ = std::thread::Builder::new().name("teams-typing".into()).spawn(move || {
            let (lock, cvar) = &*typing_rx;
            while !typing_teams.ctx.stopped() {
                let conversation = {
                    let mut guard = crate::hub::lock(lock);
                    while !guard.stopped && guard.pending.is_none() && !typing_teams.ctx.stopped() {
                        guard = cvar.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    if guard.stopped || typing_teams.ctx.stopped() {
                        break;
                    }
                    guard.pending.take()
                };
                if let Some(conversation) = conversation {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        typing_teams.typing(&conversation);
                    }));
                }
            }
        });

        let dl_teams = self.clone();
        let _ = std::thread::Builder::new().name("teams-download".into()).spawn(move || {
            let refuse = |fault: Fault| Refusal::Refused(reason(fault));
            while !dl_teams.ctx.stopped() {
                let Ok(command) = download_rx.recv() else { break };
                match command {
                    Command::Stop => break,
                    Command::Download { url, reply, .. } => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _ = reply.send(dl_teams.download(url).map_err(refuse));
                        }));
                    }
                    _ => {}
                }
            }
        });

        let cmd_teams = self.clone();
        let _ = std::thread::Builder::new().name("teams-cmd".into()).spawn(move || {
            serve_commands(
                &cmd_teams.ctx,
                cmd_rx,
                PACE,
                |id| cmd_teams.report(cmd_teams.backfill(id).map_err(Failure::from)),
                |command| cmd_teams.serve(command),
            );
        });

        self.route_inbox(inbox, send_tx, download_tx, cmd_tx, typing_slot);
    }

    fn route_inbox(
        self: &Arc<Self>,
        inbox: Receiver<Command>,
        send_tx: Sender<Command>,
        download_tx: Sender<Command>,
        cmd_tx: Sender<Command>,
        typing_slot: Arc<(Mutex<TypingSlot>, Condvar)>,
    ) {
        while !self.ctx.stopped() {
            let Ok(command) = inbox.recv() else { break };
            match command {
                Command::Stop => {
                    let _ = send_tx.send(Command::Stop);
                    let _ = download_tx.send(Command::Stop);
                    let _ = cmd_tx.send(Command::Stop);
                    break;
                }
                cmd @ Command::Send { .. } => {
                    let _ = send_tx.send(cmd);
                }
                Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                    if with.pick.is_none() && with.schedule.is_none() && with.poll.is_none() && !with.files.is_empty() {
                        self.spawn_file_send(&conversation_id, &client_id, &text, reply_to.as_deref(), *with);
                    } else {
                        let _ = send_tx.send(Command::SendWith { conversation_id, client_id, text, reply_to, with });
                    }
                }
                Command::CancelSend { client_id, .. } => {
                    self.cancel_send(&client_id);
                }
                Command::Typing { conversation_id, .. } => {
                    let (lock, cvar) = &*typing_slot;
                    crate::hub::lock(lock).pending = Some(conversation_id);
                    cvar.notify_one();
                }
                cmd @ Command::Download { .. } => {
                    let _ = download_tx.send(cmd);
                }
                other => {
                    let _ = cmd_tx.send(other);
                }
            }
        }
        {
            let (lock, cvar) = &*typing_slot;
            crate::hub::lock(lock).stopped = true;
            cvar.notify_all();
        }
        drop(send_tx);
        drop(download_tx);
        drop(cmd_tx);
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

    /// True for an address Teams' own services are at, strictly scoped to this session's cloud.
    fn ours(&self, url: &str) -> bool {
        let ep = self.cloud.endpoints();
        self.moved.as_ref().is_some_and(|base| url.starts_with(base.as_str()))
            || (self.graph != ep.graph && url.starts_with(self.graph.as_str()))
            || https_host(url).is_some_and(|host| ep.ours.iter().any(|domain| within(host, domain)))
    }

    /// True for an address a Microsoft Search (Substrate) bearer token may be sent to.
    fn substrate_host(&self, url: &str) -> bool {
        self.moved.as_ref().is_some_and(|base| url.starts_with(base.as_str()))
            || https_host(url).is_some_and(|host| SUBSTRATE_HOSTS.iter().any(|domain| within(host, domain)))
    }

    /// True for an address a picture in a message may be fetched from: this cloud's media store.
    fn pictures(&self, url: &str) -> bool {
        let ep = self.cloud.endpoints();
        let media = |host: &str| ep.media_hosts.iter().any(|domain| within(host, domain));
        self.moved.as_ref().is_some_and(|base| url.starts_with(base.as_str())) || https_host(url).is_some_and(media)
    }

    /// Reserves the next paced slot without holding the mutex across the sleep, using
    /// `ctx.pause` so shutdown interrupts immediately. Returns `true` if stopped.
    fn pace_slot(&self, slot: &Mutex<Option<Instant>>, gap: Duration) -> bool {
        let wait = {
            let mut last = crate::hub::lock(slot);
            let now = Instant::now();
            let next = match *last {
                Some(prev) if prev + gap > now => prev + gap,
                _ => now,
            };
            *last = Some(next);
            next.saturating_duration_since(now)
        };
        if wait.is_zero() {
            false
        } else {
            self.ctx.pause(wait)
        }
    }

    /// Waits until the last background call is [`GAP`] behind. True if stopped while waiting.
    fn pace(&self) -> bool {
        self.pace_slot(&self.asked, GAP)
    }

    /// Independent pace slot for interactive sends so background sync never stalls sends.
    fn pace_send(&self) -> bool {
        self.pace_slot(&self.send_asked, GAP)
    }

    // Tokens.

    /// An access token for one audience, minted from the refresh token when the one held is
    /// missing or near its end. Microsoft answers with the next refresh token, which is saved.
    fn token(&self, scope: Audience) -> Result<String, Fault> {
        if let Some(token) = self.state().access.get(&scope).filter(|token| token.fresh()) {
            return Ok(token.value.clone());
        }
        let _one = crate::hub::lock(self.minting.for_audience(scope));
        if let Some(token) = self.state().access.get(&scope).filter(|token| token.fresh()) {
            return Ok(token.value.clone());
        }
        let refresh = self.state().refresh.clone();
        let url = format!("{}/{}/oauth2/v2.0/token", self.login, escaped(&self.session.tenant));
        let audience = self.cloud.endpoints().audience(scope);
        let scopes = spaces_scope(self.cloud, scope, audience);
        let form = [
            ("client_id", self.session.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("scope", scopes.as_str()),
        ];
        let stopped = if scope == Audience::Spaces { self.pace_send() } else { self.pace() };
        if stopped {
            return Err(Fault::Net("token: stopped".to_owned()));
        }
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
            Auth::Skype | Auth::Ams | Auth::Push => state.skype = None,
            Auth::Bearer(scope) => drop(state.access.remove(&scope)),
            Auth::Both(scope) => {
                state.access.remove(&scope);
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
        let _one = crate::hub::lock(&self.settling);
        if let Some(held) = held() {
            return Ok(held);
        }
        let url = if self.cloud == TeamsCloud::Personal {
            consumer_authz(&self.api)
        } else {
            format!("{}/api/authsvc/v1.0/authz", self.api)
        };
        let mut refused = None;
        for _ in 0..2 {
            let bearer = self.token(Audience::Spaces)?;
            if self.pace_send() {
                return Err(Fault::Net("authz: stopped".to_owned()));
            }
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
                self.forget(Auth::Bearer(Audience::Spaces));
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
                presence: named("unifiedPresence").unwrap_or_else(|| self.cloud.endpoints().presence.to_owned()),
                urlp: named("urlp"),
                search: gtms["search"]
                    .as_str()
                    .map(|url| url.trim_end_matches('/').to_owned())
                    .filter(|url| self.substrate_host(url)),
                ams: named("ams").or_else(|| named("amsV2")),
            });
            let life = body["tokens"]["expiresIn"].as_u64().unwrap_or(3600).clamp(60, 86_400);
            let me = claim(token, "skypeid").map(|id| format!("8:{id}")).or_else(|| {
                Some(&self.session.oid).filter(|oid| !oid.is_empty()).map(|oid| {
                    if self.cloud == TeamsCloud::Personal {
                        format!("8:live:{oid}")
                    } else {
                        format!("8:orgid:{oid}")
                    }
                })
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
            let disabled = |keys: &[&str]| {
                [&body["regionSettings"], &body["tenantSettings"], &body["featureSettings"]]
                    .into_iter()
                    .any(|bag| keys.iter().any(|k| bag[*k] == Value::Bool(false)))
            };
            if disabled(&["isGiphyEnabled", "allowGiphy", "giphyEnabled"]) {
                state.giphy_disabled = true;
            }
            if disabled(&["isStickersEnabled", "allowStickers", "stickersEnabled"]) {
                state.stickers_disabled = true;
            }
            if disabled(&["isMessageTranslationEnabled", "allowMessageTranslation", "messageTranslationEnabled"]) {
                state.translation_disabled = true;
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
            Auth::Ams => vec![("Authorization", format!("skype_token {}", self.skype()?.0))],
            Auth::Push => vec![("X-Skypetoken", self.skype()?.0)],
            Auth::Bearer(scope) => vec![("Authorization", format!("Bearer {}", self.token(scope)?))],
            Auth::Both(scope) => {
                vec![("Authorization", format!("Bearer {}", self.token(scope)?)), ("X-Skypetoken", self.skype()?.0)]
            }
        })
    }

    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&'static str, String)],
        body: &Body,
        timeout: Option<Duration>,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let origin = self.cloud.endpoints().teams;
        let referer = format!("{origin}/");
        macro_rules! dressed {
            ($request:expr) => {{
                let req = match timeout {
                    Some(limit) => $request
                        .config()
                        .timeout_global(Some(limit))
                        .timeout_connect(Some(limit.min(Duration::from_secs(10))))
                        .timeout_recv_response(Some(limit))
                        .build(),
                    None => $request,
                };
                let mut request = req
                    .header("Accept", "application/json")
                    .header("BehaviorOverride", "redirectAs404")
                    .header("Origin", origin)
                    .header("Referer", referer.as_str());
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
            ("PATCH", Body::Json(body)) => dressed!(self.agent.patch(url)).send_json(*body),
            ("PATCH", _) => dressed!(self.agent.patch(url)).send_empty(),
            (_, Body::Json(body)) => dressed!(self.agent.post(url)).send_json(*body),
            (_, Body::Text(text)) => dressed!(self.agent.post(url)).header("Content-Type", "application/json").send(*text),
            (_, Body::None) => dressed!(self.agent.post(url)).send_empty(),
        }
    }

    /// One call to a Teams service. A token the service refuses is minted again once; a short
    /// rate limit is waited out when `patient`, and a send is not: the user is told instead.
    fn call(&self, method: &str, url: &str, auth: Auth, body: Body, patient: bool) -> Result<Answer, Fault> {
        self.call_with_timeout(method, url, auth, body, patient, None)
    }

    fn call_with_timeout(
        &self,
        method: &str,
        url: &str,
        auth: Auth,
        body: Body,
        patient: bool,
        timeout: Option<Duration>,
    ) -> Result<Answer, Fault> {
        if let Some(why) = self.state().expired.clone() {
            return Err(Fault::Auth(why));
        }
        let allowed = match auth {
            Auth::Bearer(scope) | Auth::Both(scope) if scope == Audience::Substrate => self.substrate_host(url),
            _ => self.ours(url),
        };
        if !allowed {
            return Err(Fault::Net(format!("{} is not a Teams address", bare(url))));
        }
        let retryable = retryable_on_drop(method, &body);
        let (mut renewed, mut limited, mut retried_socket, mut wait) = (false, false, false, 0);
        for _ in 0..4 {
            let headers = self.headers(auth)?;
            let stopped = if patient { self.pace() } else { self.pace_send() };
            if stopped {
                return Err(Fault::Net(format!("{}: stopped", bare(url))));
            }
            // The library's word for a bad address repeats the address, which may be signed.
            let failed = |err: ureq::Error| match err {
                ureq::Error::BadUri(_) => Fault::Net(format!("{}: not an address", bare(url))),
                err => Fault::Net(format!("{}: {err}", bare(url))),
            };
            let mut response = match self.send(method, url, &headers, &body, timeout) {
                Ok(response) => response,
                Err(err) if !retried_socket && retryable && dropped_socket(&err) => {
                    retried_socket = true;
                    tracing::debug!(url = bare(url), "chatd: Teams: pooled connection dropped; retrying once");
                    continue;
                }
                Err(err) => return Err(failed(err)),
            };
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
                    if self.ctx.pause(Duration::from_secs(wait)) {
                        return Err(Fault::Net(format!("{}: stopped", bare(url))));
                    }
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

    /// Single-attempt tight-timeout POST with no 401 token-refresh retry, no 429 sleep, and no
    /// pacing wait — used for opportunistic link unfurling so slow `urlpreview` never stalls sends.
    pub(super) fn call_fast_post(&self, url: &str, auth: Auth, body: &Value, timeout: Duration) -> Option<Value> {
        if self.state().expired.is_some() || !self.ours(url) {
            return None;
        }
        let headers = self.headers(auth).ok()?;
        let mut response = self.send("POST", url, &headers, &Body::Json(body), Some(timeout)).ok()?;
        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            return None;
        }
        let text = response.body_mut().read_to_string().ok()?;
        serde_json::from_str(&text).ok()
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
        let (giphy_disabled, stickers_disabled, translation_disabled) = {
            let state = self.state();
            (state.giphy_disabled, state.stickers_disabled, state.translation_disabled)
        };
        // Pasted images go inline through Teams' AMS media store, scoped to the conversation.
        // Other files use the member-scoped upload: recipients are resolved before the
        // session is created, and the drive item is deleted if the invite fails.
        self.ctx.capabilities(protocol::Capabilities {
            gifs: !giphy_disabled && self.cloud == TeamsCloud::Worldwide,
            stickers: !stickers_disabled,
            translate: !translation_disabled,
            ..capability_ceiling()
        });
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
            Command::MarkUnread { conversation_id, message_id, mentions: _ } => {
                self.report(self.mark_unread(&conversation_id, &message_id).map_err(Failure::from))
            }
            Command::Send { conversation_id, client_id, text, reply_to } => {
                self.post(&conversation_id, &client_id, &text, reply_to.as_deref())
            }
            Command::React { conversation_id, message_id, emoji, on, burst: _, reply } => {
                let _ = reply.send(self.react(&conversation_id, &message_id, &emoji, on).map_err(refuse));
            }
            Command::Vote { conversation_id, message_id, votes, reply } => {
                let _ = reply.send(self.vote(&conversation_id, &message_id, votes.as_deref()).map_err(refuse));
            }
            Command::MessageAction { conversation_id, message_id, block_id, action_id, element_type: _, value, values: _, reply } => {
                let _ = reply.send(self.invoke_card_action(&conversation_id, &message_id, &block_id, &action_id, &value).map(|()| None).map_err(refuse));
            }
            Command::ReactionRules { reply, .. } => drop(reply.send(Ok(message::reaction_rules()))),
            Command::Edit { conversation_id, message_id, text, spans, suppress_embeds: _, keep_attachments: _, reply } => {
                let _ = reply.send(self.edit(&conversation_id, &message_id, &text, &spans).map_err(refuse));
            }
            Command::Delete { conversation_id, message_id, scope, reply } => {
                if scope == protocol::DeleteScope::Me {
                    let _ = reply.send(Err(Refusal::Refused("Teams only supports deleting for everyone.".to_owned())));
                } else {
                    let _ = reply.send(self.delete(&conversation_id, &message_id).map_err(refuse));
                }
            }
            Command::Typing { conversation_id: conversation, activity: _ } => self.typing(&conversation),
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
            Command::StartGroup { people, title, reply } => {
                let _ = reply.send(self.start_group(&people, title.as_deref()).map_err(refuse));
            }
            Command::MembersAdd { conversation_id, people, share_history, reply } => {
                let _ = reply.send(self.members_add(&conversation_id, &people, share_history).map_err(refuse));
            }
            Command::MembersRemove { conversation_id, person, reply } => {
                let _ = reply.send(self.members_remove(&conversation_id, &person).map_err(refuse));
            }
            Command::ConversationUpdate { conversation_id, title, topic, description, reply } => {
                let _ = reply.send(
                    self.update_conversation(
                        &conversation_id,
                        title.as_deref(),
                        topic.as_deref(),
                        description.as_deref(),
                    )
                    .map_err(refuse),
                );
            }
            Command::Profile { person_id, reply } => {
                let _ = reply.send(self.profile(&person_id).map_err(refuse));
            }
            Command::ConversationInfo { conversation_id, reply } => {
                let _ = reply.send(self.conversation_info(&conversation_id).map_err(refuse));
            }
            Command::SpaceInfo { space_id, reply } => {
                let _ = reply.send(self.space_info(&space_id).map_err(refuse));
            }
            Command::SpaceTags { space_id, reply } => {
                let _ = reply.send(self.space_tags(&space_id).map_err(refuse));
            }
            Command::ReadBy { conversation_id, message_id, reply } => {
                let _ = reply.send(self.read_by(&conversation_id, &message_id).map_err(refuse));
            }
            Command::SetMute { conversation_id, notify, until: _, reply } => {
                let _ = reply.send(self.set_mute(&conversation_id, notify).map_err(refuse));
            }
            Command::Leave { conversation_id, silent: _, reply } => {
                let _ = reply.send(self.leave(&conversation_id).map_err(refuse));
            }
            Command::LeaveSpace { space_id, reply } => {
                let _ = reply.send(self.leave_team(&space_id).map_err(refuse));
            }
            Command::BrowseChannels { space_id, query, cursor, limit, reply } => {
                let _ = reply.send(self.browse_channels(&space_id, &query, cursor.as_deref(), limit).map_err(refuse));
            }
            Command::PreviewChannel { conversation_id, reply } => {
                let _ = reply.send(self.preview_channel(&conversation_id).map_err(refuse));
            }
            Command::Join { conversation_id, reply } => {
                let _ = reply.send(self.join_channel_or_team(&conversation_id).map_err(refuse));
            }
            Command::CreateChannel { space_id, name, kind: _, private, group: _, reply, .. } => {
                let _ = reply.send(self.create_channel(&space_id, &name, private).map_err(refuse));
            }
            Command::InvitePreview { code, reply, .. } => {
                let _ = reply.send(self.invite_preview(&code).map_err(refuse));
            }
            Command::InviteJoin { code, reply } => {
                let _ = reply.send(self.invite_join(&code).map(|(space, conversation)| (space, conversation, false)).map_err(refuse));
            }
            Command::File { conversation_id, pinned, archived, folder_add: _, folder_remove: _, reply } => {
                let _ = reply.send(self.file_conversation(&conversation_id, pinned, archived).map_err(refuse));
            }
            Command::Pinned { conversation_id, reply } => {
                let _ = reply.send(self.pinned_messages(&conversation_id).map_err(refuse));
            }
            Command::Pin { conversation_id, message_id, pinned, notify: _, me_only: _, reply } => {
                let _ = reply.send(self.pin_message(&conversation_id, &message_id, pinned).map_err(refuse));
            }
            Command::StarMessage { conversation_id, message_id, starred, reply } => {
                let _ = reply.send(self.save_message(&conversation_id, &message_id, starred).map_err(refuse));
            }
            Command::StarredMessages { conversation_id, limit, reply } => {
                let _ = reply.send(self.saved_messages(conversation_id.as_deref(), limit).map_err(refuse));
            }
            Command::ActivityFeed { kind, cursor, limit, reply } => {
                let _ = reply.send(self.activity_feed_page(kind, cursor.as_deref(), limit).map_err(refuse));
            }
            Command::ActivityMarkRead { item_id, reply } => {
                let _ = reply.send(self.activity_mark_read(item_id.as_deref()).map_err(refuse));
            }
            Command::Search { query, scope, filters, cursor, at, reply } => {
                let found = self.search_messages(&query, &scope, &filters, cursor.as_deref(), at);
                let _ = reply.send(found.map(|hits| super::SearchPage { hits, cursor: None, total: None }).map_err(refuse));
            }
            Command::Relate { person_id, action, reply } => {
                let _ = reply.send(self.relate(&person_id, action).map_err(refuse));
            }
            Command::AnswerMessageRequest { conversation_id, accept, reply } => {
                let _ = reply.send(self.answer_external_request(&conversation_id, accept).map_err(refuse));
            }
            Command::CloseDirect { conversation_id, reply } => {
                let _ = reply.send(self.hide_chat(&conversation_id).map_err(refuse));
            }
            Command::ClearHistory { conversation_id, delete_chat, revoke: _, reply } => {
                let _ = reply.send(self.delete_chat(&conversation_id, delete_chat).map_err(refuse));
            }
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                self.send_with(&conversation_id, &client_id, &text, reply_to.as_deref(), *with);
            }
            Command::Forward { from_conversation_id, message_ids, to_conversation_ids, note, reply } => {
                let _ = reply.send(Ok(self.forward(&from_conversation_id, &message_ids, &to_conversation_ids, &note)));
            }
            Command::CancelSend { client_id, .. } => self.cancel_send(&client_id),
            // Each row's line comes with the chat list (`lastMessage`); a row still without one
            // has no message, or one the list did not show.
            Command::Previews(_) => {}
            Command::SetPresence { presence, status, status_emoji: _, clear_at, reply } => {
                let _ = reply.send(self.set_presence(presence, status.as_deref(), clear_at).map_err(refuse));
            }
            Command::GetOutOfOffice { reply } => {
                let _ = reply.send(self.get_out_of_office().map_err(refuse));
            }
            Command::SetOutOfOffice { ooo, reply } => {
                let _ = reply.send(self.set_out_of_office(&ooo).map_err(refuse));
            }
            Command::Download { url, reply, .. } => {
                let _ = reply.send(self.download(url).map_err(refuse));
            }
            Command::Handoff { conversation_id, reply } => {
                let _ = reply.send(Ok(self.call_link(&conversation_id)));
            }
            Command::Catalog { kind, query, cursor, reply, .. }
                if matches!(kind, protocol::CatalogKind::Gifs | protocol::CatalogKind::Stickers) =>
            {
                let _ = reply.send(self.catalog(kind, &query, cursor.as_deref()).map_err(refuse));
            }
            Command::Translate { conversation_id, message_id, to, reply } => {
                let _ = reply.send(self.translate(&conversation_id, &message_id, &to).map_err(refuse));
            }
            Command::CallHistory { missed, before, limit, reply } => {
                let _ = reply.send(self.call_history(missed, before.as_deref(), limit).map_err(refuse));
            }
            Command::Voicemails { before, reply } => {
                let _ = reply.send(self.voicemails(before.as_deref()).map_err(refuse));
            }
            Command::VoicemailAudio { id, attachment, reply } => {
                let _ = reply.send(self.voicemail_audio(&id, &attachment).map_err(refuse));
            }
            Command::ChangeVoicemail { id, action, reply } => {
                let _ = reply.send(self.change_voicemail(&id, action).map_err(refuse));
            }
            Command::Meetings { reply } => {
                let _ = reply.send(self.meetings().map_err(refuse));
            }
            Command::MeetingTranscript { conversation_id, message_id, reply } => {
                let _ = reply.send(self.meeting_transcript(&conversation_id, &message_id).map_err(refuse));
            }
            Command::MeetingInvitees { conversation_id, reply } => {
                let _ = reply.send(self.meeting_invitees(&conversation_id).map_err(refuse));
            }
            Command::CreateMeeting { conversation_id, draft, reply } => {
                let _ = reply.send(self.create_meeting(&conversation_id, &draft).map_err(refuse));
            }
            Command::MeetingRsvp { id, response, propose, reply } => {
                let _ = reply.send(self.meeting_rsvp(&id, response, propose).map_err(refuse));
            }
            // Remove the trouter push registration at Microsoft before forgetting the local token.
            // Entra v2.0 public clients have no single-refresh-token revocation endpoint, and
            // Graph's `/me/revokeSignInSessions` ends every Microsoft session across all devices,
            // so session revocation stays on `https://myaccount.microsoft.com/`.
            Command::SignOut(reply) => {
                self.unregister_push();
                cloud::unregister(&self.ctx.account);
                let _ = reply.send(Ok(()));
            }
            command => command.unsupported(&self.ctx),
        }
    }

    /// How the media cache fetches a registered file: a shared file through Graph, a picture
    /// from the media store with the skype token, a person's picture with the bearer as the
    /// web client's cookie.
    fn download(&self, url: String) -> Result<Download, Fault> {
        let mut headers = vec![("User-Agent".to_owned(), self.session.user_agent.clone())];
        if let Some(shared) = self.shared_download(&url) {
            headers.push(("Authorization".to_owned(), format!("Bearer {}", self.token(Audience::Graph)?)));
            headers.push(("Prefer".to_owned(), "redeemSharingLinkIfNecessary".to_owned()));
            return Ok(Download { url: shared, headers, sealed: None });
        }
        if self.pictures(&url) {
            headers.push(("Authorization".to_owned(), format!("skype_token {}", self.skype()?.0)));
        } else if self.ours(&url) {
            let origin = self.cloud.endpoints().teams;
            let cookie = format!("authtoken=Bearer={}&Origin={origin}", self.token(Audience::Spaces)?);
            headers.push(("Cookie".to_owned(), cookie));
            headers.push(("Referer".to_owned(), format!("{origin}/")));
        }
        Ok(Download { url, headers, sealed: None })
    }

    /// Graph's content address for a sharing link, or the address itself when it already is one.
    /// The bearer is Graph's. The cache attaches these headers only on a session host, and
    /// SharePoint is not one, so the redirect does not carry the bearer.
    fn shared_download(&self, url: &str) -> Option<String> {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        if path.contains("/v1.0/shares/") && path.ends_with("/driveItem/content") {
            return self.ours(url).then(|| url.to_owned());
        }
        let host = https_host(url)?;
        self.cloud
            .endpoints()
            .sharepoint
            .iter()
            .any(|domain| within(host, domain))
            .then(|| graph_content(&self.graph, url))
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
        let Ok(place) = self.place() else { return };
        let body = json!({"content": "", "contenttype": "Application/Message", "messagetype": "Control/Typing"});
        let url = format!("{}/v1/users/ME/conversations/{}/messages", place.chat, escaped(conversation));
        if let Err(Fault::Auth(why)) = self.call_with_timeout("POST", &url, Auth::Skype, Body::Json(&body), false, Some(TYPING_TIMEOUT)) {
            self.state().expired.get_or_insert(why);
        }
    }

    fn mark_read(&self, conversation: &str, message: &str) -> Result<(), Fault> {
        // A message of ours that Teams has not named yet has no place in its order.
        if millis(message) < 1_000_000_000_000 {
            return Ok(());
        }
        // Teams keeps one marker for a channel, its reply chains included.
        let thread = thread_of(conversation);
        let ts = now_ms();
        let path = format!("/v1/users/ME/conversations/{}/properties?name=consumptionhorizon", escaped(thread));
        let horizon = json!({"consumptionhorizon": format!("{message};{ts};{message}")});
        self.chat("PUT", &path, Body::Json(&horizon), true)?;
        self.state().horizon_ms.insert(thread.to_owned(), ts);
        if let Some((channel, root)) = chain_of(conversation) {
            self.clear_chain_unread(channel, root)?;
        }
        self.mark_activity_for_conversation(thread, message);
        Ok(())
    }

    /// Moves Teams' consumption horizon back to `message` (the last message that still counts
    /// as read, or `"0"` when marking from the very start) so "Mark as unread" syncs across devices.
    fn mark_unread(&self, conversation: &str, message: &str) -> Result<(), Fault> {
        let marker = if millis(message) >= 1_000_000_000_000 { message } else { "0" };
        let thread = thread_of(conversation);
        let ts = now_ms();
        let path = format!(
            "/v1/users/ME/conversations/{}/properties?name=consumptionhorizon",
            escaped(thread)
        );
        let horizon = json!({"consumptionhorizon": format!("{marker};{ts};{marker}")});
        self.chat("PUT", &path, Body::Json(&horizon), true)?;
        self.state().horizon_ms.insert(thread.to_owned(), ts);
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
        assert!(!guest.personal);

        let personal = json!({
            "client_id": PERSONAL_CLIENT, "personal": true, "cloud": "personal",
            "oid": "live-oid", "name": "Pat Live",
            "tenants": [{"id": "9188040d-6c67-4c5b-b112-36a304b66dad", "name": "Personal", "refresh_token": "rt-personal", "client_id": PERSONAL_CLIENT, "personal": true, "cloud": "personal"}],
        }).to_string();
        let joined = combine(&session(), &personal).unwrap();
        let both = accounts(&joined).unwrap();
        assert_eq!(both.len(), 3);
        let live: Session = serde_json::from_str(&both[2].session).unwrap();
        assert!(live.personal);
        assert_eq!(live.client_id, PERSONAL_CLIENT);
        assert_eq!(live.cloud, "personal");
        let home: Session = serde_json::from_str(&both[0].session).unwrap();
        assert!(!home.personal);
        assert_eq!(home.client_id, CLIENT);
        assert_eq!(spaces_scope(TeamsCloud::Personal, Audience::Spaces, "https://mtsvc.fl.teams.microsoft.com"), PERSONAL_SCOPE);
        assert_eq!(consumer_authz("https://teams.live.com"), "https://teams.live.com/api/auth/v1.0/authz/consumer");

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
        let share = "https://contoso.sharepoint.com/:x:/s/Platform/EaBcDeF";
        let id = share_id(share);
        assert!(id.starts_with("u!") && !id.contains(['+', '/', '=']));
        assert_eq!(graph_content(GRAPH, share), format!("{GRAPH}/v1.0/shares/{id}/driveItem/content"));
        assert_eq!(graph_content("http://127.0.0.1:9/graph/", share), format!("http://127.0.0.1:9/graph/v1.0/shares/{id}/driveItem/content"));
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

    #[test]
    fn gcc_high_session_builds_only_gcc_high_addresses_and_refuses_worldwide_hosts() {
        let gov_session = json!({
            "client_id": CLIENT, "user_agent": "Safari/1", "oid": "gov-oid", "name": "Sam Gov",
            "cloud": "gcc_high",
            "tenants": [{"id": "gov-tenant-1", "name": "Agency", "refresh_token": "rt-gov"}],
        })
        .to_string();
        let found = accounts(&gov_session).unwrap();
        assert_eq!(found.len(), 1);
        let per_tenant: Session = serde_json::from_str(&found[0].session).unwrap();
        assert_eq!(TeamsCloud::parse(&per_tenant.cloud), TeamsCloud::GccHigh);

        let ep = TeamsCloud::GccHigh.endpoints();
        assert_eq!(ep.login, "https://login.microsoftonline.us");
        assert_eq!(ep.teams, "https://gov.teams.microsoft.us");
        assert_eq!(ep.spaces, "https://gov.teams.microsoft.us");
        assert_eq!(ep.aggregator, "https://chatsvcagg.gov.teams.microsoft.us");
        assert_eq!(ep.presence, "https://presence.gov.teams.microsoft.us");
        assert_eq!(ep.graph, "https://graph.microsoft.us");
        assert_eq!(ep.trouter, "https://go.trouter.gov.teams.microsoft.us");

        let dir = std::env::temp_dir().join(format!("tiny-teams-gcchigh-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = crate::store::Store::open(&dir.join("chat.sqlite")).unwrap();
        let hub = Arc::new(crate::hub::Hub::new(store));
        let acct = protocol::AccountKey { network: protocol::Network::Teams, account: "gov-tenant-1".into() };
        let (_adapter, ctx, _rx) = super::super::local(acct, hub);
        let teams = Teams {
            ctx,
            agent: ureq::Agent::config_builder().build().into(),
            cloud: TeamsCloud::GccHigh,
            login: ep.login.to_owned(),
            api: ep.teams.to_owned(),
            graph: ep.graph.to_owned(),
            trouter: ep.trouter.to_owned(),
            moved: None,
            session: per_tenant,
            state: Mutex::new(State { refresh: "rt-gov".to_owned(), ..State::default() }),
            minting: MintLocks::default(),
            settling: Mutex::new(()),
            asked: Mutex::new(None),
            send_asked: Mutex::new(None),
        };

        // GCC High accepts only GCC High hosts and refuses worldwide, DoD, and China hosts.
        assert!(teams.ours("https://gov.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(teams.ours("https://usgov.ng.msg.gov.teams.microsoft.us/v1/users/ME"));
        assert!(teams.ours("https://graph.microsoft.us/v1.0/me"));
        assert!(!teams.ours("https://teams.microsoft.com/api/authsvc/v1.0/authz"));
        assert!(!teams.ours("https://emea.ng.msg.teams.microsoft.com/v1/users/ME"));
        assert!(!teams.ours("https://graph.microsoft.com/v1.0/me"));
        assert!(!teams.ours("https://dod.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(!teams.ours("https://teams.microsoft.cn/api/authsvc/v1.0/authz"));
        assert!(!teams.ours("https://teams.microsoftonline.cn/api/authsvc/v1.0/authz"));
        assert!(!teams.ours("https://api.fl.spaces.skype.us/"));
        assert!(!teams.ours("https://usgov-api.asm.skype.us/v1/objects/1"));

        // Media store and SharePoint links stay strictly inside GCC High.
        assert!(teams.pictures("https://usgov.asyncgw.gov.teams.microsoft.us/v1/objects/1/views/imgo"));
        assert!(!teams.pictures("https://usgov-api.asm.skype.us/v1/objects/1/views/imgo"));
        assert!(!teams.pictures("https://eu-api.asm.skype.com/v1/objects/1/views/imgo"));
        assert!(teams.shared_download("https://agency.sharepoint.us/:x:/s/Ops/Abc").is_some_and(|u| u.starts_with("https://graph.microsoft.us/v1.0/shares/u!")));
        assert!(teams.shared_download("https://contoso.sharepoint.com/:x:/s/Ops/Abc").is_none());

        // `call` refuses before touching the network or minting a token for a worldwide URL.
        let refused = teams.call("GET", "https://teams.microsoft.com/api/csa/api/v1/teams/users/me", Auth::Bearer(Audience::Aggregator), Body::None, false);
        assert!(matches!(refused, Err(Fault::Net(msg)) if msg.contains("is not a Teams address")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_teams(tag: &str, base: &str) -> (Arc<Teams>, super::super::Adapter, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("tiny-teams-{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = crate::store::Store::open(&dir.join("chat.sqlite")).unwrap();
        let hub = Arc::new(crate::hub::Hub::new(store));
        let acct = protocol::AccountKey { network: protocol::Network::Teams, account: format!("tenant-{tag}") };
        let (adapter, ctx, _rx) = super::super::local(acct, hub);
        let agent = ureq::Agent::config_builder()
            .max_idle_age(Duration::from_secs(30))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(20)))
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build()
            .into();
        let mut state = State {
            refresh: "rt-test".to_owned(),
            me: "8:orgid:me-test".to_owned(),
            skype: Some(Token {
                value: "skype-fresh".to_owned(),
                until: Instant::now() + Duration::from_secs(3600),
            }),
            place: Some(Arc::new(Place {
                chat: format!("{base}/chat"),
                middle: format!("{base}/middle"),
                presence: format!("{base}/presence"),
                urlp: None,
                search: None,
                ams: Some(format!("{base}/ams")),
            })),
            ..State::default()
        };
        state.access.insert(
            Audience::Spaces,
            Token {
                value: "spaces-fresh".to_owned(),
                until: Instant::now() + Duration::from_secs(3600),
            },
        );
        let teams = Arc::new(Teams {
            ctx,
            agent,
            cloud: TeamsCloud::Worldwide,
            login: format!("{base}/login"),
            api: base.to_owned(),
            graph: format!("{base}/graph"),
            trouter: format!("{base}/trouter"),
            moved: Some(base.to_owned()),
            session: Session {
                tenant: format!("tenant-{tag}"),
                name: "Test".to_owned(),
                refresh_token: "rt-test".to_owned(),
                client_id: CLIENT.to_owned(),
                user_agent: "Test/1".to_owned(),
                oid: "me-test".to_owned(),
                user: "Test User".to_owned(),
                cloud: String::new(),
                personal: false,
            },
            state: Mutex::new(state),
            minting: MintLocks::default(),
            settling: Mutex::new(()),
            asked: Mutex::new(None),
            send_asked: Mutex::new(None),
        });
        (teams, adapter, dir)
    }

    #[test]
    fn unfurl_link_slow_response_does_not_stall_sending() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let done_rx = Arc::new(Mutex::new(done_rx));
        let server_done = done_rx.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let done_rx = server_done.clone();
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
                    if path.starts_with("/middle/v1/urlpreview") {
                        // Simulate an unresponsive/slow urlpreview (>4s).
                        let _ = crate::hub::lock(&done_rx).recv_timeout(Duration::from_secs(5));
                        let body = r#"{"title":"Too Late"}"#;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                    } else if path.contains("/messages") {
                        let body = r#"{"OriginalArrivalTime":1760005000100}"#;
                        let resp = format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                    }
                });
            }
        });

        let (teams, _adapter, dir) = test_teams("slow-unfurl", &base);
        let started = Instant::now();
        let sent = teams
            .posted_with(
                "19:chat@unq.gbl.spaces",
                "77001",
                "see https://example.com/slow",
                None,
                &protocol::Outgoing::default(),
                None,
            )
            .expect("send should succeed even when urlpreview stalls");
        let elapsed = started.elapsed();
        let _ = done_tx.send(());

        assert_eq!(sent.id, "1760005000100");
        assert!(sent.previews.is_empty(), "timed-out urlpreview should be skipped cleanly");
        assert!(
            elapsed < Duration::from_millis(3500),
            "slow urlpreview stalled send for {elapsed:?} (expected bounded by 1.5s timeout)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_returns_fresh_cached_audience_without_waiting_on_other_mint_lock() {
        let (teams, _adapter, dir) = test_teams("token-cache", "http://127.0.0.1:9");
        // Hold another audience's mint lock (and even Spaces's own lock) while Spaces has a fresh token.
        let _graph_lock = crate::hub::lock(teams.minting.for_audience(Audience::Graph));
        let _spaces_lock = crate::hub::lock(teams.minting.for_audience(Audience::Spaces));
        let started = Instant::now();
        let tok = teams.token(Audience::Spaces).expect("fresh cached Spaces token");
        assert_eq!(tok, "spaces-fresh");
        assert!(started.elapsed() < Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn original_arrival_time_parses_both_casings() {
        assert_eq!(message::arrival_time_id(&json!({"OriginalArrivalTime": 1_760_005_000_100u64})), "1760005000100");
        assert_eq!(message::arrival_time_id(&json!({"OriginalArrivalTime": "1760005000100"})), "1760005000100");
        assert_eq!(message::arrival_time_id(&json!({"originalarrivaltime": 1_760_005_000_200u64})), "1760005000200");
        assert_eq!(message::arrival_time_id(&json!({"originalarrivaltime": "1760005000200"})), "1760005000200");
        assert_eq!(message::arrival_time_id(&json!({"originalArrivalTime": 1_760_005_000_300u64})), "1760005000300");
        assert_eq!(message::arrival_time_id(&json!({})), "");
    }
}
