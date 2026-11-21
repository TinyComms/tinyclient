//! Server management on a tinyserver that has it (`docs/server-management.md` there): what the
//! server can do (`GET /v1/capabilities`), what this account may do (`GET /v1/me`), the server's
//! settings and icon, roles, categories, bans, and every change to them.
//!
//! A server from before this answers the capabilities route with 404. It then has no features,
//! this account has no `TinyAccess`, and everything here is refused in words before the server
//! is asked; the page offers what it always did and decides by the saved legacy role.
//!
//! Every change is checked here only for shape (ids, lengths, ranges) and for what the cached
//! access allows; the server checks permissions and hierarchy again and its refusal is put in
//! words. Nothing is changed locally: the server's live socket tells every page, and the page
//! reads the server again.

// The refusal is the answer to the shell, as everywhere in `tiny`: boxing it buys nothing here.
#![allow(clippy::result_large_err)]

use super::{asked, endpoint, explain, open, refused, rows, safe_id, tiny_name, Missing, Saved, CHECK, NOT_YOURS, UNREACHABLE, UNREADABLE};
use protocol::{
    tiny_allowed, AccountKey, Response, TinyAccess, TinyAuditFilter, TinyBan, TinyCapabilities, TinyCategory, TinyCode,
    TinyBot, TinyEmoji, TinyInviteOptions, TinyLayoutGroup, TinyRole, TinyRoleEdit, TinyRoom, TinyRoomEdit, TinySeat, TinyServerEdit, TinyServerInfo,
    TinyClaimLink, TinyPasskeys, TinyReportDetail, TinyReportFilter, TinyReportSummary, TinyReportUpdate, TinySetup, TinySetupApplied, TinySetupApply, TinySetupRefusal,
    TINY_ALL, TINY_BITRATE_MAX, TINY_BITRATE_MIN, TINY_DESCRIPTION_MAX, TINY_FEATURE_ABUSE_REPORTS, TINY_FEATURE_AUDIT_QUERY, TINY_FEATURE_BANS,
    TINY_FEATURE_CATEGORIES, TINY_FEATURE_CUSTOM_EMOJI, TINY_FEATURE_INVITE_OPTIONS, TINY_FEATURE_ROLES, TINY_FEATURE_SERVER, TINY_FEATURE_STICKERS, TINY_FEATURE_TIMEOUTS,
    TINY_FEATURE_FILE_SCAN, TINY_FEATURE_FIRST_RUN_SETUP, TINY_FEATURE_UPLOAD_LIMITS, TINY_ICON_MAX, TINY_SERVER_NAME_MAX, TINY_UPLOAD_MAX,
    TINY_WELCOME_MAX,
};
use protocol::tiny_words::{LAST_PASSKEY, OLDER, SIGN_IN_AGAIN};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

const BOT_NAME: &str = "A bot name is 1 to 80 characters.";
const BOT_SCOPES: &str = "A bot can be granted rooms.read, rooms.write, and members.read.";
const CATEGORY_NAME_SIZE: &str = "A category name is 1 to 80 characters.";
const ROLE_NAME_SIZE: &str = "A role name is 1 to 80 characters.";
const SERVER_NAME_SIZE: &str = "A server name is 1 to 80 characters.";
const DESCRIPTION_SIZE: &str = "A description is at most 500 characters.";
const WELCOME_SIZE: &str = "A welcome message is at most 2000 characters.";
const COLOR: &str = "A colour is # and six hex digits, like #3B82F6.";
const BITRATE: &str = "A room's bitrate is 16 to 128 kb/s.";
const USES: &str = "An invite can be used 1 to 1000 times.";
const EXPIRY: &str = "An invite lasts from a minute to 30 days.";
const AFK: &str = "The AFK timeout is 0 (off) up to a day.";
const UPLOAD_LIMIT: &str = "An upload limit is 1 byte to 100 MiB.";
const REASON_SIZE: &str = "A reason is at most 500 characters.";
const ICON_SIZE: &str = "That picture is too big. The most is 256 KB.";
const ICON_KIND: &str = "That picture isn't a PNG, JPEG or WebP.";
const PASSKEY_LABEL_MAX: usize = 60;
const PASSKEY_GONE: &str = "That passkey is gone.";
pub(crate) const DELETE_LAST_OWNER: &str = "You're the only owner. Hand the server to someone first.";
const REPORT_NOTE_MAX: usize = 2000;
const REPORT_EVIDENCE_MAX: usize = 32_768;
const REPORT_KINDS: [&str; 3] = ["message", "member", "channel"];
const REPORT_CATEGORIES: [&str; 8] = ["spam", "harassment", "hate", "violence", "sexual", "illegal", "impersonation", "other"];
pub(crate) const REPORT_RATE_LIMIT: &str = "You've sent a lot of reports. Try again later.";
pub(crate) const REPORT_NOT_FOUND: &str = "That is no longer available.";
const REPORT_NOTE_SIZE: &str = "A note is at most 2000 characters.";
const REPORT_EVIDENCE_SIZE: &str = "That message is too long to attach as evidence.";
/// The longest reason the audit keeps from here.
const REASON_MAX: usize = 500;
/// The longest value of a setup choice, such as `activity_visibility`'s `everyone`, and the longest
/// run of text, rows in a list and options the server's setup outline is kept to.
const SETUP_WORD_MAX: usize = 32;
const SETUP_TEXT_MAX: usize = 2000;
const SETUP_ROWS_MAX: usize = 16;
/// Rows of one list read at most: roles, categories, bans, rooms in a layout.
const LIST_MAX: usize = 1000;

/// What chatd last learned of one signed-in server, so a request is checked without asking the
/// server first. Keyed by server and user: two servers, or two tests, never mix.
#[derive(Debug, Clone, Default)]
pub(crate) struct Known {
    /// The capabilities route was read (a 404 counts: an older server).
    pub checked: bool,
    pub capabilities: TinyCapabilities,
    pub access: Option<TinyAccess>,
    /// The rooms as last listed, without who sits in them.
    pub rooms: Vec<TinyRoom>,
    /// Who sits in each room's call with what the server says of them, from the live socket.
    pub seats: HashMap<String, Vec<TinySeat>>,
    /// The sign-in provider's name, once asked (empty when the server did not say).
    pub provider: Option<String>,
    /// Community custom emoji and stickers last fetched from `GET /v1/emoji`.
    pub emoji: Vec<TinyEmoji>,
}

static KNOWN: Mutex<BTreeMap<String, Known>> = Mutex::new(BTreeMap::new());

fn key(saved: &Saved) -> String {
    format!("{}\n{}", saved.base_url, saved.user_id)
}

pub(crate) fn known(saved: &Saved) -> Known {
    crate::hub::lock(&KNOWN).get(&key(saved)).cloned().unwrap_or_default()
}

pub(crate) fn remember(saved: &Saved, change: impl FnOnce(&mut Known)) {
    change(crate::hub::lock(&KNOWN).entry(key(saved)).or_default());
}

/// What a room asks of a call in it, as last listed: the bitrate to encode at, and whether only
/// push-to-talk may open the microphone (the room says so, or this account lacks voice
/// activation there). A room not listed yet is the defaults.
pub(crate) fn room_audio(saved: &Saved, room_id: &str) -> (u32, bool) {
    let was = known(saved);
    let Some(room) = was.rooms.iter().find(|room| room.id == room_id) else {
        return (protocol::TINY_BITRATE_DEFAULT, false);
    };
    let no_voice_activity = was.access.as_ref().is_some_and(|access| !access.has_in(room, protocol::TINY_VOICE_ACTIVITY));
    (room.bitrate(), room.ptt_only || no_voice_activity)
}

/// The room only takes end-to-end encrypted calls (`e2ee_required`), as the server last said.
pub(crate) fn room_e2ee_required(saved: &Saved, room_id: &str) -> bool {
    known(saved).rooms.iter().any(|room| room.id == room_id && room.e2ee_required)
}

/// `GET /v1/capabilities`. Anything but a readable 200 is an older server: no features.
pub(crate) fn capabilities_of(saved: &Saved) -> TinyCapabilities {
    match asked("GET", saved, "/v1/capabilities", None) {
        Ok((200, body)) => serde_json::from_value(body).unwrap_or_default(),
        _ => TinyCapabilities::default(),
    }
}

/// What this account may do, from `GET /v1/me` on a server with roles.
pub(crate) fn access_of(saved: &Saved) -> Option<TinyAccess> {
    match asked("GET", saved, "/v1/me", None) {
        Ok((200, body)) if body.get("permissions").is_some() || body.get("owner").is_some() => serde_json::from_value(body).ok(),
        _ => None,
    }
}

/// The capabilities and access, read now and kept. The page refreshes them on each load.
pub(crate) fn learn(saved: &Saved) -> Known {
    let capabilities = capabilities_of(saved);
    let access = if capabilities.features.is_empty() { None } else { access_of(saved) };
    remember(saved, |known| {
        known.checked = true;
        known.capabilities = capabilities;
        known.access = access;
    });
    known(saved)
}

/// The session, when the server has `feature` (unless `None`) and this account may do `id`.
/// What was learned is used; a server not yet looked at is asked first.
fn allowed(account: &AccountKey, feature: Option<&str>, id: &str) -> Result<Saved, Response> {
    let saved = open(account)?;
    let mut was = known(&saved);
    if !was.checked {
        was = learn(&saved);
    }
    if let Some(feature) = feature {
        if !was.capabilities.has(feature) {
            return Err(refused(OLDER));
        }
    }
    if !tiny_allowed(was.access.as_ref(), &saved.role, id) {
        return Err(refused(NOT_YOURS));
    }
    Ok(saved)
}

/// One change: allowed, then sent, then any 2xx is done.
#[allow(clippy::too_many_arguments)]
fn change(account: &AccountKey, feature: Option<&str>, id: &str, method: &str, path: &str, body: Option<Value>, missing: Missing) -> Response {
    let saved = match allowed(account, feature, id) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    sent(&saved, method, path, body, missing)
}

fn sent(saved: &Saved, method: &str, path: &str, body: Option<Value>, missing: Missing) -> Response {
    match asked(method, saved, path, body) {
        Err(detail) => refused(detail),
        Ok((200..=299, _)) => Response::Ok,
        Ok((status, body)) => refused(explain(status, &body, missing)),
    }
}

fn ids_ok(ids: &[&str]) -> bool {
    ids.iter().all(|id| safe_id(id))
}

/// Trimmed, and at most `max` characters, or `None` past it.
fn text_within(text: &str, max: usize) -> Option<&str> {
    let text = text.trim();
    (text.chars().count() <= max).then_some(text)
}

fn reason(text: &str) -> Result<&str, Response> {
    text_within(text, REASON_MAX).ok_or_else(|| refused(REASON_SIZE))
}

/// `#RRGGBB`.
fn color_ok(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// For a query string: everything but unreserved characters as `%XX`.
fn escape(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// An id the server sent that may be cleared: `Some("")` is `null`.
fn id_or_null(id: &str) -> Value {
    if id.is_empty() {
        Value::Null
    } else {
        Value::String(id.to_owned())
    }
}

// The page's reads.

/// `GET /v1/server`, with the icon fetched once into the account's media and named there.
pub(crate) fn server_of(saved: &Saved, account: &AccountKey) -> Option<TinyServerInfo> {
    let mut info: TinyServerInfo = match asked("GET", saved, "/v1/server", None) {
        Ok((200, body)) => serde_json::from_value(body).ok()?,
        _ => return None,
    };
    info.icon = info.icon.filter(|hash| !hash.is_empty() && hash.len() <= 128 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    for word in [&mut info.provider_profile, &mut info.activity_visibility, &mut info.sign_in_methods] {
        protocol::clip(word, SETUP_WORD_MAX);
    }
    protocol::clip(&mut info.setup_completed_at, SETUP_WORD_MAX);
    if let Some(hash) = info.icon.clone() {
        info.icon_path = icon_file(saved, account, &hash).map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
    }
    Some(info)
}

/// The icon with this hash on disk, fetched when it is not there yet. A new icon is a new hash
/// and so a new file; the shell never draws a stale one.
fn icon_file(saved: &Saved, account: &AccountKey, hash: &str) -> Option<PathBuf> {
    let path = crate::blobs::path_of(&protocol::support_dir().join("media"), account, &format!("tiny-icon-{hash}"));
    if path.is_file() {
        return Some(path);
    }
    let bytes = fetch_icon(saved)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let partial = path.with_extension("part");
    std::fs::write(&partial, &bytes).ok()?;
    std::fs::rename(&partial, &path).ok()?;
    Some(path)
}

fn fetch_icon(saved: &Saved) -> Option<Vec<u8>> {
    let response = super::agent()
        .get(&endpoint(&saved.base_url, "/v1/server/icon"))
        .header("Authorization", &format!("Bearer {}", saved.token))
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    let mut body = response.into_body();
    body.as_reader().take(TINY_ICON_MAX as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= TINY_ICON_MAX && image_kind(&bytes).is_some()).then_some(bytes)
}

/// Fetches each member's picture once per hash, beside the server icon, and names the file on
/// the member. A hash already on disk is not fetched again.
pub(crate) fn remember_avatars(saved: &Saved, account: &AccountKey, members: &mut [protocol::TinyMember]) {
    for member in members {
        let Some(hash) = member.avatar.as_deref().filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())) else {
            member.avatar_path.clear();
            continue;
        };
        if !safe_id(&member.user_id) {
            continue;
        }
        if let Some(path) = avatar_file(saved, account, &member.user_id, hash) {
            member.avatar_path = path.to_string_lossy().into_owned();
        }
    }
}

fn avatar_file(saved: &Saved, account: &AccountKey, user_id: &str, hash: &str) -> Option<PathBuf> {
    let path = crate::blobs::path_of(&protocol::support_dir().join("media"), account, &format!("tiny-avatar-{user_id}-{hash}"));
    if path.is_file() {
        return Some(path);
    }
    let bytes = fetch_avatar(saved, user_id)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let partial = path.with_extension("part");
    std::fs::write(&partial, &bytes).ok()?;
    std::fs::rename(&partial, &path).ok()?;
    Some(path)
}

fn fetch_avatar(saved: &Saved, user_id: &str) -> Option<Vec<u8>> {
    let response = super::agent()
        .get(&endpoint(&saved.base_url, &format!("/v1/users/{user_id}/avatar")))
        .header("Authorization", &format!("Bearer {}", saved.token))
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    let mut body = response.into_body();
    body.as_reader().take(TINY_ICON_MAX as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= TINY_ICON_MAX && image_kind(&bytes).is_some()).then_some(bytes)
}

/// The type of a picture the server takes, by its first bytes.
fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn list<T: serde::de::DeserializeOwned>(saved: &Saved, path: &str, what: &str) -> Vec<T> {
    match asked("GET", saved, path, None) {
        Ok((200, body)) => {
            let mut read: Vec<T> = rows(body, what).unwrap_or_default();
            read.truncate(LIST_MAX);
            read
        }
        _ => Vec::new(),
    }
}

/// Highest position first.
pub(crate) fn roles_of(saved: &Saved) -> Vec<TinyRole> {
    let mut roles: Vec<TinyRole> = list(saved, "/v1/roles", "roles");
    roles.sort_by_key(|role| std::cmp::Reverse(role.position));
    roles
}

/// Top first.
pub(crate) fn categories_of(saved: &Saved) -> Vec<TinyCategory> {
    let mut categories: Vec<TinyCategory> = list(saved, "/v1/categories", "categories");
    categories.sort_by_key(|category| category.position);
    categories
}

/// Text channels from `GET /v1/channels`. A 403, 404 or unreadable answer leaves it empty and
/// does not fail the page: an older server has no such route.
pub(crate) fn channels_of(saved: &Saved) -> Vec<protocol::TinyChannel> {
    match asked("GET", saved, "/v1/channels", None) {
        Ok((200, body)) => {
            let value = body.get("channels").cloned().unwrap_or(body);
            let mut read: Vec<protocol::TinyChannel> = rows(value, "channels").unwrap_or_default();
            read.truncate(LIST_MAX);
            read
        }
        _ => Vec::new(),
    }
}

pub(crate) fn bans_of(saved: &Saved) -> Vec<TinyBan> {
    list(saved, "/v1/bans", "bans")
}

/// What the page adds on a server with features: the server's settings, roles, categories and,
/// for someone who may ban, the bans. Each is left empty on a failure; the page still loads.
pub(crate) struct Extras {
    pub server: Option<TinyServerInfo>,
    pub roles: Vec<TinyRole>,
    pub categories: Vec<TinyCategory>,
    pub bans: Vec<TinyBan>,
}

pub(crate) fn extras(saved: &Saved, account: &AccountKey, was: &Known) -> Extras {
    let has = |feature| was.capabilities.has(feature);
    let may = |id| tiny_allowed(was.access.as_ref(), &saved.role, id);
    Extras {
        server: has(TINY_FEATURE_SERVER).then(|| server_of(saved, account)).flatten(),
        roles: if has(TINY_FEATURE_ROLES) { roles_of(saved) } else { Vec::new() },
        categories: if has(TINY_FEATURE_CATEGORIES) { categories_of(saved) } else { Vec::new() },
        bans: if has(TINY_FEATURE_BANS) && may("community.bans.read") { bans_of(saved) } else { Vec::new() },
    }
}

// Server.

pub(crate) fn server_update(account: AccountKey, edit: TinyServerEdit) -> Response {
    let mut body = Map::new();
    if let Some(name) = &edit.name {
        match name.trim() {
            name if name.is_empty() || name.chars().count() > TINY_SERVER_NAME_MAX => return refused(SERVER_NAME_SIZE),
            name => body.insert("name".into(), json!(name)),
        };
    }
    if let Some(description) = &edit.description {
        let Some(description) = text_within(description, TINY_DESCRIPTION_MAX) else {
            return refused(DESCRIPTION_SIZE);
        };
        body.insert("description".into(), json!(description));
    }
    if let Some(welcome) = &edit.welcome {
        let Some(welcome) = text_within(welcome, TINY_WELCOME_MAX) else {
            return refused(WELCOME_SIZE);
        };
        body.insert("welcome".into(), json!(welcome));
    }
    for (field, room) in [("default_room_id", &edit.default_room_id), ("afk_room_id", &edit.afk_room_id)] {
        if let Some(room) = room {
            if !room.is_empty() && !safe_id(room) {
                return refused(CHECK);
            }
            body.insert(field.into(), id_or_null(room));
        }
    }
    if let Some(secs) = edit.afk_timeout_secs {
        if !(0..=86_400).contains(&secs) {
            return refused(AFK);
        }
        body.insert("afk_timeout_secs".into(), json!(secs));
    }
    if edit.image_max_bytes.is_some() || edit.file_max_bytes.is_some() {
        if let Err(answer) = allowed(&account, Some(TINY_FEATURE_UPLOAD_LIMITS), "community.server.edit") {
            return answer;
        }
    }
    for (field, bytes) in [("image_max_bytes", edit.image_max_bytes), ("file_max_bytes", edit.file_max_bytes)] {
        if let Some(bytes) = bytes {
            if !(1..=TINY_UPLOAD_MAX).contains(&bytes) {
                return refused(UPLOAD_LIMIT);
            }
            body.insert(field.into(), json!(bytes));
        }
    }
    if let Some(allow) = edit.allow_archives {
        if let Err(answer) = allowed(&account, Some(TINY_FEATURE_FILE_SCAN), "community.server.edit") {
            return answer;
        }
        body.insert("allow_archives".into(), json!(allow));
    }
    if body.is_empty() {
        return Response::Ok;
    }
    change(&account, Some(TINY_FEATURE_SERVER), "community.server.edit", "PATCH", "/v1/server", Some(Value::Object(body)), Missing::Plain)
}

/// The picture at `path` as the icon, or with `None` no icon. The file is read here, checked,
/// and sent as it is; the server keeps it.
pub(crate) fn server_icon(account: AccountKey, path: Option<String>) -> Response {
    let saved = match allowed(&account, Some(TINY_FEATURE_SERVER), "community.server.edit") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let Some(path) = path else {
        return sent(&saved, "DELETE", "/v1/server/icon", None, Missing::Plain);
    };
    let real = match crate::blobs::upload::allowed(&path) {
        Ok((real, _)) => real,
        Err(why) => return refused(&format!("That picture {why}")),
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&real).and_then(|file| file.take(TINY_ICON_MAX as u64 + 1).read_to_end(&mut bytes));
    if read.is_err() {
        return refused("That picture couldn't be read.");
    }
    if bytes.len() > TINY_ICON_MAX {
        return refused(ICON_SIZE);
    }
    let Some(kind) = image_kind(&bytes) else {
        return refused(ICON_KIND);
    };
    match put_bytes(&saved, "/v1/server/icon", kind, &bytes) {
        Err(detail) => refused(detail),
        Ok((200..=299, _)) => Response::Ok,
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// This account's own picture, or with `None` no picture. Any member may set their own.
/// A server from before pictures answers 404, which is said in words.
pub(crate) fn user_avatar(account: AccountKey, path: Option<String>) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let Some(path) = path else {
        return match asked("DELETE", &saved, "/v1/me/avatar", None) {
            Err(detail) => refused(detail),
            Ok((200..=299, _)) => Response::Ok,
            Ok((404, _)) => refused(OLDER),
            Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
        };
    };
    let real = match crate::blobs::upload::allowed(&path) {
        Ok((real, _)) => real,
        Err(why) => return refused(&format!("That picture {why}")),
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&real).and_then(|file| file.take(TINY_ICON_MAX as u64 + 1).read_to_end(&mut bytes));
    if read.is_err() {
        return refused("That picture couldn't be read.");
    }
    if bytes.len() > TINY_ICON_MAX {
        return refused(ICON_SIZE);
    }
    let Some(kind) = image_kind(&bytes) else {
        return refused(ICON_KIND);
    };
    match put_bytes(&saved, "/v1/me/avatar", kind, &bytes) {
        Err(detail) => refused(detail),
        Ok((200..=299, _)) => Response::Ok,
        Ok((404, _)) => refused(OLDER),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

fn put_bytes(saved: &Saved, path: &str, kind: &str, bytes: &[u8]) -> Result<(u16, Value), &'static str> {
    let sent = super::agent()
        .put(&endpoint(&saved.base_url, path))
        .header("Authorization", &format!("Bearer {}", saved.token))
        .header("Content-Type", kind)
        .send(bytes);
    let mut response = sent.map_err(|_| UNREACHABLE)?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(UNREACHABLE);
    }
    let body = if status == 204 { Value::Null } else { response.body_mut().read_json().unwrap_or(Value::Null) };
    Ok((status, body))
}

// First-run setup.

/// The session, when the server has setup and this account is an owner. Setup says what the
/// community keeps about everyone in it, so an admin with every permission is not enough: the
/// server answers anyone else 403, and nothing is asked for them here.
fn owner_only(account: &AccountKey) -> Result<Saved, Response> {
    let saved = open(account)?;
    let mut was = known(&saved);
    if !was.checked {
        was = learn(&saved);
    }
    if !was.capabilities.has(TINY_FEATURE_FIRST_RUN_SETUP) {
        return Err(refused(OLDER));
    }
    if !was.access.as_ref().is_some_and(|access| access.owner) {
        return Err(refused(NOT_YOURS));
    }
    Ok(saved)
}

/// Holds what the server wrote to what a window can draw. The server's text is shown as it comes,
/// so only its size is limited.
fn setup_bounded(mut setup: TinySetup) -> TinySetup {
    fn words(text: &mut String) {
        protocol::clip(text, SETUP_TEXT_MAX);
    }
    setup.presets.truncate(SETUP_ROWS_MAX);
    setup.choices.truncate(SETUP_ROWS_MAX);
    setup.always.truncate(SETUP_ROWS_MAX);
    setup.members.truncate(SETUP_ROWS_MAX * 8);
    for member in &mut setup.members {
        [&mut member.user_id, &mut member.display_name].into_iter().for_each(words);
    }
    setup.always.iter_mut().for_each(words);
    for preset in &mut setup.presets {
        [&mut preset.id, &mut preset.title, &mut preset.summary, &mut preset.unavailable].into_iter().for_each(words);
    }
    for choice in &mut setup.choices {
        [&mut choice.id, &mut choice.title, &mut choice.question].into_iter().for_each(words);
        choice.options.truncate(SETUP_ROWS_MAX);
        for option in &mut choice.options {
            [&mut option.id, &mut option.title, &mut option.effect].into_iter().for_each(words);
            option.needs.truncate(SETUP_ROWS_MAX);
            option.needs.iter_mut().for_each(words);
        }
    }
    setup
}

/// `GET /v1/setup`: what the owner is asked, in the server's words, with what a choice would touch.
pub(crate) fn setup_read(account: AccountKey) -> Response {
    let saved = match owner_only(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("GET", &saved, "/v1/setup", None) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<TinySetup>(body) {
            Ok(setup) => Response::TinySetup { setup: setup_bounded(setup) },
            Err(_) => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// A value the server names a preset or a choice's option by: short, lower case and underscores.
/// The server checks it again and refuses one it does not know.
fn setup_word(word: &str) -> bool {
    !word.is_empty() && word.len() <= SETUP_WORD_MAX && word.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

fn setup_body(apply: &TinySetupApply) -> Result<Value, Response> {
    let mut body = Map::new();
    for (field, value) in [
        ("preset", &apply.preset),
        ("provider_profile", &apply.provider_profile),
        ("activity_visibility", &apply.activity_visibility),
        ("sign_in_methods", &apply.sign_in_methods),
    ] {
        if let Some(value) = value {
            if !setup_word(value) {
                return Err(refused(CHECK));
            }
            body.insert(field.into(), json!(value));
        }
    }
    // Left off unless asked for: the server reads a missing flag as no.
    for (field, on) in [("erase_emails", apply.erase_emails), ("allow_lockout", apply.allow_lockout)] {
        if on {
            body.insert(field.into(), json!(true));
        }
    }
    Ok(Value::Object(body))
}

/// A 409 from setup that the owner can do something about.
fn setup_refusal(body: &Value) -> Option<TinySetupRefusal> {
    let error = body.get("error").and_then(Value::as_str)?;
    match error {
        "passkeys_unavailable" => Some(TinySetupRefusal::PasskeysUnavailable),
        "owner_needs_passkey" => Some(TinySetupRefusal::OwnerNeedsPasskey),
        "owner_needs_provider" => Some(TinySetupRefusal::OwnerNeedsProvider),
        "members_without_passkey" | "members_without_provider" => {
            let count = body.get("count").and_then(Value::as_u64).map_or(0, |count| u32::try_from(count).unwrap_or(u32::MAX));
            Some(if error == "members_without_passkey" { TinySetupRefusal::MembersWithoutPasskey { count } } else { TinySetupRefusal::MembersWithoutProvider { count } })
        }
        _ => None,
    }
}

/// `PUT /v1/setup`. An empty `apply` keeps everything as it is and finishes setup. A refusal the
/// owner can act on is its own answer; anything else is said in words.
pub(crate) fn setup_apply(account: AccountKey, apply: TinySetupApply) -> Response {
    let body = match setup_body(&apply) {
        Ok(body) => body,
        Err(answer) => return answer,
    };
    let saved = match owner_only(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("PUT", &saved, "/v1/setup", Some(body)) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<TinySetupApplied>(body) {
            Ok(applied) => Response::TinySetupApplied { applied },
            Err(_) => refused(UNREADABLE),
        },
        Ok((409, body)) => match setup_refusal(&body) {
            Some(refusal) => Response::TinySetupRefused { refusal },
            // A reason from a newer server than this: nothing was changed.
            None => refused(CHECK),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// `POST /v1/me/passkeys/link`, then the link in the system browser. The link is a bearer: it
/// goes to the browser and nowhere else, and is never logged or handed back.
pub(crate) fn passkey_link(account: AccountKey) -> Response {
    passkey_link_with(account, &super::browser::system())
}

pub(crate) fn passkey_link_with(account: AccountKey, browser: &super::browser::Opener) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("POST", &saved, "/v1/me/passkeys/link", None) {
        Err(detail) => refused(detail),
        Ok((200 | 201, body)) => match body.get("url").and_then(Value::as_str) {
            Some(url) => match browser(url) {
                Ok(()) => Response::Ok,
                Err(why) => refused(&why),
            },
            None => refused(UNREADABLE),
        },
        // Making a link wants a sign-in within the last ten minutes, so a stolen old session
        // cannot plant a passkey of its own.
        Ok((403, body)) if body.get("error").and_then(Value::as_str) == Some("recent_sign_in_required") => refused(SIGN_IN_AGAIN),
        Ok((404, _)) => refused(OLDER),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// `POST /v1/setup/claim-links`: a link for each member asked for, to add a passkey. Owners only.
/// The links are bearers: they go back to the window that asked and are never logged.
pub(crate) fn setup_claim_links(account: AccountKey, user_ids: Vec<String>) -> Response {
    if user_ids.is_empty() || !user_ids.iter().all(|id| safe_id(id)) {
        return refused(CHECK);
    }
    let saved = match owner_only(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("POST", &saved, "/v1/setup/claim-links", Some(json!({ "user_ids": user_ids }))) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match body.get("links").cloned().map(serde_json::from_value::<Vec<TinyClaimLink>>) {
            Some(Ok(links)) => Response::TinyClaimLinks { links },
            _ => refused(UNREADABLE),
        },
        Ok((404, _)) => refused(OLDER),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// `GET /v1/me/passkeys`: this account's own passkeys and how many recovery codes are left.
pub(crate) fn passkeys_read(account: AccountKey) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("GET", &saved, "/v1/me/passkeys", None) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<TinyPasskeys>(body) {
            Ok(mut passkeys) => {
                passkeys.passkeys.truncate(LIST_MAX);
                for passkey in &mut passkeys.passkeys {
                    protocol::clip(&mut passkey.label, PASSKEY_LABEL_MAX);
                }
                Response::TinyPasskeys { passkeys }
            }
            Err(_) => refused(UNREADABLE),
        },
        Ok((404, _)) => refused(OLDER),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// `PATCH /v1/me/passkeys/{id}`: a name for one of this account's passkeys, 1 to 60 characters.
pub(crate) fn passkey_rename(account: AccountKey, id: String, label: String) -> Response {
    let label = label.trim();
    if !safe_id(&id) || label.is_empty() || label.chars().count() > PASSKEY_LABEL_MAX {
        return refused(CHECK);
    }
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("PATCH", &saved, &format!("/v1/me/passkeys/{id}"), Some(json!({ "label": label }))) {
        Err(detail) => refused(detail),
        Ok((200..=299, _)) => Response::Ok,
        Ok((404, _)) => refused(PASSKEY_GONE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// `DELETE /v1/me/passkeys/{id}`. The server keeps the last passkey when no recovery code is left.
pub(crate) fn passkey_remove(account: AccountKey, id: String) -> Response {
    if !safe_id(&id) {
        return refused(CHECK);
    }
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("DELETE", &saved, &format!("/v1/me/passkeys/{id}"), None) {
        Err(detail) => refused(detail),
        Ok((200..=299, _)) => Response::Ok,
        Ok((403, body)) if recent_sign_in_needed(&body) => refused(SIGN_IN_AGAIN),
        Ok((403, _)) => refused(LAST_PASSKEY),
        Ok((404, _)) => refused(PASSKEY_GONE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// `POST /v1/me/recovery-codes`: a fresh set, shown once and never logged.
pub(crate) fn recovery_codes(account: AccountKey) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("POST", &saved, "/v1/me/recovery-codes", None) {
        Err(detail) => refused(detail),
        Ok((200 | 201, body)) => match body.get("codes").cloned().map(serde_json::from_value::<Vec<String>>) {
            Some(Ok(codes)) => Response::TinyRecoveryCodes { codes },
            _ => refused(UNREADABLE),
        },
        Ok((403, _)) => refused(SIGN_IN_AGAIN),
        Ok((404, _)) => refused(OLDER),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

fn recent_sign_in_needed(body: &Value) -> bool {
    body.get("error").and_then(Value::as_str) == Some("recent_sign_in_required")
}

// Roles.

fn role_body(role: &TinyRoleEdit, creating: bool) -> Result<Value, Response> {
    let mut body = Map::new();
    match &role.name {
        Some(name) => match tiny_name(name) {
            Some(name) => {
                body.insert("name".into(), json!(name));
            }
            None => return Err(refused(ROLE_NAME_SIZE)),
        },
        None if creating => return Err(refused(ROLE_NAME_SIZE)),
        None => {}
    }
    if let Some(color) = &role.color {
        if !color.is_empty() && !color_ok(color) {
            return Err(refused(COLOR));
        }
        body.insert("color".into(), id_or_null(color));
    }
    if let Some(permissions) = role.permissions {
        if permissions & !TINY_ALL != 0 {
            return Err(refused(CHECK));
        }
        body.insert("permissions".into(), json!(permissions));
    }
    if let Some(hoist) = role.hoist {
        body.insert("hoist".into(), json!(hoist));
    }
    Ok(Value::Object(body))
}

pub(crate) fn role_create(account: AccountKey, role: TinyRoleEdit) -> Response {
    match role_body(&role, true) {
        Ok(body) => change(&account, Some(TINY_FEATURE_ROLES), "community.roles.manage", "POST", "/v1/roles", Some(body), Missing::Role),
        Err(answer) => answer,
    }
}

pub(crate) fn role_update(account: AccountKey, role_id: String, role: TinyRoleEdit) -> Response {
    if !safe_id(&role_id) {
        return refused(CHECK);
    }
    match role_body(&role, false) {
        Ok(body) => change(&account, Some(TINY_FEATURE_ROLES), "community.roles.manage", "PATCH", &format!("/v1/roles/{role_id}"), Some(body), Missing::Role),
        Err(answer) => answer,
    }
}

pub(crate) fn role_delete(account: AccountKey, role_id: String) -> Response {
    if !safe_id(&role_id) {
        return refused(CHECK);
    }
    change(&account, Some(TINY_FEATURE_ROLES), "community.roles.manage", "DELETE", &format!("/v1/roles/{role_id}"), None, Missing::Role)
}

pub(crate) fn role_order(account: AccountKey, role_ids: Vec<String>) -> Response {
    if role_ids.len() > LIST_MAX || !role_ids.iter().all(|id| safe_id(id)) {
        return refused(CHECK);
    }
    change(&account, Some(TINY_FEATURE_ROLES), "community.roles.manage", "PUT", "/v1/roles/order", Some(json!({ "ids": role_ids })), Missing::Role)
}

// Members.

pub(crate) fn member_roles(account: AccountKey, user_id: String, role_ids: Vec<String>) -> Response {
    if !safe_id(&user_id) || role_ids.len() > LIST_MAX || !role_ids.iter().all(|id| safe_id(id)) {
        return refused(CHECK);
    }
    let path = format!("/v1/members/{user_id}/roles");
    change(&account, Some(TINY_FEATURE_ROLES), "community.members.role", "PUT", &path, Some(json!({ "role_ids": role_ids })), Missing::Member)
}

pub(crate) fn member_owner(account: AccountKey, user_id: String, owner: bool) -> Response {
    if !safe_id(&user_id) {
        return refused(CHECK);
    }
    let path = format!("/v1/members/{user_id}/owner");
    change(&account, Some(TINY_FEATURE_ROLES), "community.members.owner", "PUT", &path, Some(json!({ "owner": owner })), Missing::Member)
}

pub(crate) fn timeout(account: AccountKey, user_id: String, until: Option<String>, why: String) -> Response {
    if !safe_id(&user_id) || until.as_ref().is_some_and(|until| until.is_empty() || until.len() > 64) {
        return refused(CHECK);
    }
    let why = match reason(&why) {
        Ok(why) => why,
        Err(answer) => return answer,
    };
    let mut body = json!({ "until": until });
    if !why.is_empty() {
        body["reason"] = json!(why);
    }
    let path = format!("/v1/members/{user_id}/timeout");
    change(&account, Some(TINY_FEATURE_TIMEOUTS), "community.members.timeout", "PUT", &path, Some(body), Missing::Member)
}

/// `TinyRemove` with a reason. An older server takes the removal and ignores the reason.
pub(crate) fn kick(account: AccountKey, user_id: String, why: String) -> Response {
    if !safe_id(&user_id) {
        return refused(CHECK);
    }
    let why = match reason(&why) {
        Ok(why) => why,
        Err(answer) => return answer,
    };
    let path = if why.is_empty() { format!("/v1/members/{user_id}") } else { format!("/v1/members/{user_id}?reason={}", escape(why)) };
    change(&account, None, "community.members.remove", "DELETE", &path, None, Missing::Member)
}

// Bans.

pub(crate) fn ban(account: AccountKey, user_id: String, why: String, expires_at: Option<String>) -> Response {
    if !safe_id(&user_id) || expires_at.as_ref().is_some_and(|at| at.is_empty() || at.len() > 64) {
        return refused(CHECK);
    }
    let why = match reason(&why) {
        Ok(why) => why,
        Err(answer) => return answer,
    };
    let mut body = json!({ "user_id": user_id });
    if !why.is_empty() {
        body["reason"] = json!(why);
    }
    if let Some(at) = expires_at {
        body["expires_at"] = json!(at);
    }
    change(&account, Some(TINY_FEATURE_BANS), "community.members.ban", "POST", "/v1/bans", Some(body), Missing::Member)
}

pub(crate) fn unban(account: AccountKey, user_id: String) -> Response {
    if !safe_id(&user_id) {
        return refused(CHECK);
    }
    change(&account, Some(TINY_FEATURE_BANS), "community.members.ban", "DELETE", &format!("/v1/bans/{user_id}"), None, Missing::Ban)
}

// Categories and layout.

pub(crate) fn category_create(account: AccountKey, name: String) -> Response {
    let Some(name) = tiny_name(&name) else {
        return refused(CATEGORY_NAME_SIZE);
    };
    change(&account, Some(TINY_FEATURE_CATEGORIES), "community.categories.manage", "POST", "/v1/categories", Some(json!({ "name": name })), Missing::Category)
}

pub(crate) fn category_rename(account: AccountKey, category_id: String, name: String) -> Response {
    if !safe_id(&category_id) {
        return refused(CHECK);
    }
    let Some(name) = tiny_name(&name) else {
        return refused(CATEGORY_NAME_SIZE);
    };
    let path = format!("/v1/categories/{category_id}");
    change(&account, Some(TINY_FEATURE_CATEGORIES), "community.categories.manage", "PATCH", &path, Some(json!({ "name": name })), Missing::Category)
}

pub(crate) fn category_delete(account: AccountKey, category_id: String) -> Response {
    if !safe_id(&category_id) {
        return refused(CHECK);
    }
    let path = format!("/v1/categories/{category_id}");
    change(&account, Some(TINY_FEATURE_CATEGORIES), "community.categories.manage", "DELETE", &path, None, Missing::Category)
}

pub(crate) fn layout(account: AccountKey, groups: Vec<TinyLayoutGroup>) -> Response {
    let count: usize = groups.iter().map(|group| group.room_ids.len().max(group.items.len())).sum();
    let ids_fine = groups.iter().all(|group| {
        group.category_id.as_deref().is_none_or(safe_id)
            && group.room_ids.iter().all(|id| safe_id(id))
            && group.items.iter().all(|item| safe_id(&item.id))
    });
    if groups.len() > LIST_MAX || count > LIST_MAX || !ids_fine {
        return refused(CHECK);
    }
    let saved = match allowed(&account, Some(TINY_FEATURE_CATEGORIES), "community.categories.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let mixed = known(&saved).capabilities.has(protocol::TINY_FEATURE_CHANNEL_LAYOUT);
    let categories: Vec<Value> = if mixed {
        groups
            .iter()
            .map(|group| {
                let items: Vec<Value> = if !group.items.is_empty() {
                    group
                        .items
                        .iter()
                        .map(|item| {
                            let kind = match item.kind {
                                protocol::TinyLayoutItemKind::Channel => "channel",
                                protocol::TinyLayoutItemKind::Room => "room",
                            };
                            json!({ "kind": kind, "id": item.id })
                        })
                        .collect()
                } else {
                    group.room_ids.iter().map(|id| json!({ "kind": "room", "id": id })).collect()
                };
                json!({ "id": group.category_id, "items": items })
            })
            .collect()
    } else {
        groups
            .iter()
            .map(|group| {
                let room_ids: Vec<&str> = if !group.room_ids.is_empty() {
                    group.room_ids.iter().map(String::as_str).collect()
                } else {
                    group
                        .items
                        .iter()
                        .filter(|item| item.kind == protocol::TinyLayoutItemKind::Room)
                        .map(|item| item.id.as_str())
                        .collect()
                };
                json!({ "id": group.category_id, "room_ids": room_ids })
            })
            .collect()
    };
    sent(&saved, "PUT", "/v1/layout", Some(json!({ "categories": categories })), Missing::Room)
}

// Rooms.

fn room_body(room: &TinyRoomEdit, creating: bool) -> Result<Value, Response> {
    let mut body = Map::new();
    match &room.name {
        Some(name) => match tiny_name(name) {
            Some(name) => {
                body.insert("name".into(), json!(name));
            }
            None => return Err(refused(super::ROOM_NAME_SIZE)),
        },
        None if creating => return Err(refused(super::ROOM_NAME_SIZE)),
        None => {}
    }
    if let Some(category) = &room.category_id {
        if !category.is_empty() && !safe_id(category) {
            return Err(refused(CHECK));
        }
        body.insert("category_id".into(), id_or_null(category));
    }
    if let Some(position) = room.position {
        body.insert("position".into(), json!(position));
    }
    if let Some(topic) = &room.topic {
        let Some(topic) = text_within(topic, TINY_DESCRIPTION_MAX) else {
            return Err(refused("A topic is at most 500 characters."));
        };
        body.insert("topic".into(), json!(topic));
    }
    if let Some(limit) = room.user_limit {
        if limit > 99 {
            return Err(refused("A room holds at most 99 people."));
        }
        body.insert("user_limit".into(), json!(limit));
    }
    if let Some(kbps) = room.bitrate_kbps {
        if !(TINY_BITRATE_MIN..=TINY_BITRATE_MAX).contains(&kbps) {
            return Err(refused(BITRATE));
        }
        body.insert("bitrate_kbps".into(), json!(kbps));
    }
    if let Some(ptt) = room.ptt_only {
        body.insert("ptt_only".into(), json!(ptt));
    }
    if let Some(video) = room.video_enabled {
        body.insert("video_enabled".into(), json!(video));
    }
    Ok(Value::Object(body))
}

/// A room with settings. Only the name goes to a server without categories, so an older one
/// still makes it.
pub(crate) fn room_create(account: AccountKey, room: TinyRoomEdit) -> Response {
    let saved = match allowed(&account, None, "community.rooms.create") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let mut body = match room_body(&room, true) {
        Ok(body) => body,
        Err(answer) => return answer,
    };
    if !known(&saved).capabilities.has(TINY_FEATURE_CATEGORIES) {
        body = json!({ "name": body["name"] });
    }
    sent(&saved, "POST", "/v1/rooms", Some(body), Missing::Room)
}

const CHANNEL_TOPIC_SIZE: &str = "A topic is at most 1024 characters.";
const CHANNELS_UNSUPPORTED: &str = "This server is older than text channels.";
const E2EE_NOT_SET_UP: &str = "This device isn't set up for encrypted messages yet. Open Encryption in the server's settings.";

fn channel_body(channel: &protocol::TinyChannelEdit, creating: bool) -> Result<(Map<String, Value>, Option<String>), Response> {
    let mut body = Map::new();
    let mut kept_name = None;
    match &channel.name {
        Some(name) => match tiny_name(name) {
            Some(name) => {
                kept_name = Some(name.to_owned());
                body.insert("name".into(), json!(name));
            }
            None => return Err(refused(super::ROOM_NAME_SIZE)),
        },
        None if creating => return Err(refused(super::ROOM_NAME_SIZE)),
        None => {}
    }
    if let Some(category) = &channel.category_id {
        if !category.is_empty() && !safe_id(category) {
            return Err(refused(CHECK));
        }
        body.insert("category_id".into(), id_or_null(category));
    }
    if let Some(position) = channel.position {
        body.insert("position".into(), json!(position));
    }
    if let Some(topic) = &channel.topic {
        let Some(topic) = text_within(topic, protocol::TINY_CHANNEL_TOPIC_MAX) else {
            return Err(refused(CHANNEL_TOPIC_SIZE));
        };
        body.insert("topic".into(), json!(topic));
    }
    Ok((body, kept_name))
}

pub(crate) fn channel_create(account: AccountKey, channel: protocol::TinyChannelEdit) -> Response {
    let saved = match allowed(&account, None, "community.channels.create") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let (mut body, kept_name) = match channel_body(&channel, true) {
        Ok(parsed) => parsed,
        Err(answer) => return answer,
    };
    // If this device is not ready for encryption and the server requires it (or caller asked for
    // it), refuse with the same words a new direct message uses.
    if let Some(device) = super::e2ee::started(&saved) {
        let (ready, mode) = {
            let device = device.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            (device.ready(), device.mode().to_owned())
        };
        if !ready && (channel.e2ee == Some(true) || mode == "new_required" || mode == "required") {
            return refused(E2EE_NOT_SET_UP);
        }
    }
    // Send `e2ee` only when true so a server from before encrypted text gets the body it has
    // always taken.
    if channel.e2ee == Some(true) {
        body.insert("e2ee".into(), json!(true));
    }
    // When no position is given, send one past the highest position among text channels in that
    // category (older servers default to 0, which ties and sorts by name).
    if channel.position.is_none() {
        let target_cat = channel.category_id.as_deref().filter(|id| !id.is_empty());
        let existing = channels_of(&saved);
        let next_pos = existing
            .iter()
            .filter(|ch| ch.category_id.as_deref() == target_cat)
            .map(|ch| ch.position)
            .max()
            .map(|max| max.saturating_add(1))
            .unwrap_or(0);
        body.insert("position".into(), json!(next_pos));
    }
    match asked("POST", &saved, "/v1/channels", Some(Value::Object(body))) {
        Err(detail) => refused(detail),
        Ok((200 | 201, answer)) => {
            let id = answer
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| answer.pointer("/channel/id").and_then(Value::as_str))
                .unwrap_or("")
                .to_owned();
            let name = answer
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| answer.pointer("/channel/name").and_then(Value::as_str))
                .map(str::to_owned)
                .or(kept_name)
                .unwrap_or_default();
            if !safe_id(&id) {
                return refused(super::UNREADABLE);
            }
            Response::TinyChannelCreated { id, name }
        }
        Ok((404, _)) => refused(CHANNELS_UNSUPPORTED),
        Ok((status, answer)) => refused(explain(status, &answer, Missing::Channel)),
    }
}

pub(crate) fn channel_update(account: AccountKey, channel_id: String, channel: protocol::TinyChannelEdit) -> Response {
    if !safe_id(&channel_id) {
        return refused(CHECK);
    }
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let (body, _) = match channel_body(&channel, false) {
        Ok(parsed) => parsed,
        Err(answer) => return answer,
    };
    // Never send `e2ee` on PATCH; the server refuses it.
    sent(&saved, "PATCH", &format!("/v1/channels/{channel_id}"), Some(Value::Object(body)), Missing::Channel)
}

pub(crate) fn channel_delete(account: AccountKey, channel_id: String) -> Response {
    if !safe_id(&channel_id) {
        return refused(CHECK);
    }
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    sent(&saved, "DELETE", &format!("/v1/channels/{channel_id}"), None, Missing::Channel)
}

pub(crate) fn room_update(account: AccountKey, room_id: String, room: TinyRoomEdit) -> Response {
    if !safe_id(&room_id) {
        return refused(CHECK);
    }
    let body = match room_body(&room, false) {
        Ok(body) => body,
        Err(answer) => return answer,
    };
    let only_name = body.as_object().is_some_and(|body| body.keys().all(|key| key == "name"));
    // A rename alone is what an older server takes, under its own name.
    let (feature, id) = if only_name { (None, "community.rooms.rename") } else { (Some(TINY_FEATURE_CATEGORIES), "community.rooms.edit") };
    change(&account, feature, id, "PATCH", &format!("/v1/rooms/{room_id}"), Some(body), Missing::Room)
}

pub(crate) fn override_set(account: AccountKey, room_id: String, role_id: String, allow: u64, deny: u64) -> Response {
    if !ids_ok(&[&room_id, &role_id]) || (allow | deny) & !TINY_ALL != 0 || allow & deny != 0 {
        return refused(CHECK);
    }
    let path = format!("/v1/rooms/{room_id}/overrides/{role_id}");
    change(&account, Some(TINY_FEATURE_ROLES), "community.rooms.overrides", "PUT", &path, Some(json!({ "allow": allow, "deny": deny })), Missing::Room)
}

pub(crate) fn override_clear(account: AccountKey, room_id: String, role_id: String) -> Response {
    if !ids_ok(&[&room_id, &role_id]) {
        return refused(CHECK);
    }
    let path = format!("/v1/rooms/{room_id}/overrides/{role_id}");
    change(&account, Some(TINY_FEATURE_ROLES), "community.rooms.overrides", "DELETE", &path, None, Missing::Room)
}

pub(crate) fn channel_override_set(account: AccountKey, channel_id: String, role_id: String, allow: u64, deny: u64) -> Response {
    if !ids_ok(&[&channel_id, &role_id]) || (allow | deny) & !TINY_ALL != 0 || allow & deny != 0 {
        return refused(CHECK);
    }
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let path = format!("/v1/channels/{channel_id}/overrides/{role_id}");
    sent(&saved, "PUT", &path, Some(json!({ "allow": allow, "deny": deny })), Missing::Channel)
}

pub(crate) fn channel_override_clear(account: AccountKey, channel_id: String, role_id: String) -> Response {
    if !ids_ok(&[&channel_id, &role_id]) {
        return refused(CHECK);
    }
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let path = format!("/v1/channels/{channel_id}/overrides/{role_id}");
    sent(&saved, "DELETE", &path, None, Missing::Channel)
}

// Invites and the audit.

pub(crate) fn invite_with(account: AccountKey, options: TinyInviteOptions) -> Response {
    let id = if options.role_ids.is_empty() { "community.invites.create" } else { "community.invites.roles" };
    let saved = match allowed(&account, Some(TINY_FEATURE_INVITE_OPTIONS), id) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    if options.role_ids.len() > LIST_MAX || !options.role_ids.iter().all(|id| safe_id(id)) {
        return refused(CHECK);
    }
    let mut body = json!({});
    if !options.role_ids.is_empty() {
        body["role_ids"] = json!(options.role_ids);
    }
    let Some(label) = text_within(&options.label, protocol::TINY_NAME_MAX) else {
        return refused("A label is at most 80 characters.");
    };
    if !label.is_empty() {
        body["label"] = json!(label);
    }
    if let Some(uses) = options.max_uses {
        if !(1..=1000).contains(&uses) {
            return refused(USES);
        }
        body["max_uses"] = json!(uses);
    }
    if !options.default_expiry {
        if options.expires_in_secs.is_some_and(|secs| !(60..=2_592_000).contains(&secs)) {
            return refused(EXPIRY);
        }
        // `null` is never, which the server allows only owners and server managers.
        body["expires_in_secs"] = json!(options.expires_in_secs);
    }
    match asked("POST", &saved, "/v1/invites", Some(body)) {
        Err(detail) => refused(detail),
        Ok((200 | 201, body)) => match serde_json::from_value::<TinyCode>(body) {
            Ok(mut code) => {
                code.origin = saved.base_url;
                Response::TinyCode { code }
            }
            Err(_) => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// One page of the audit. A server without `audit_query` has only its newest rows: the first
/// page without filters is those, and any other page is empty.
pub(crate) fn audit(account: AccountKey, filter: TinyAuditFilter) -> Response {
    let saved = match allowed(&account, None, "community.audit.read") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let limit = match filter.limit {
        0 => 50,
        limit => limit.min(200),
    };
    if filter.actor.as_deref().is_some_and(|actor| !safe_id(actor)) || filter.action.len() > 64 {
        return refused(CHECK);
    }
    let filtered = filter.before.is_some() || filter.actor.is_some() || !filter.action.is_empty();
    let mut path = format!("/v1/audit?limit={limit}");
    if known(&saved).capabilities.has(TINY_FEATURE_AUDIT_QUERY) {
        if let Some(before) = filter.before {
            path.push_str(&format!("&before={before}"));
        }
        if let Some(actor) = &filter.actor {
            path.push_str(&format!("&actor={actor}"));
        }
        if !filter.action.is_empty() {
            path.push_str(&format!("&action={}", escape(&filter.action)));
        }
    } else if filtered {
        return Response::TinyAudit { audit: Vec::new() };
    }
    match asked("GET", &saved, &path, None) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match super::audit_rows(body) {
            Ok(audit) => Response::TinyAudit { audit },
            Err(()) => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

const REPORT_STATUSES: [&str; 4] = ["open", "reviewing", "actioned", "dismissed"];
const REPORT_KINDS: [&str; 3] = ["message", "member", "channel"];
const REPORT_NOTE_MAX: usize = 2000;
const REPORT_GONE: &str = "That report isn't available.";
const REPORT_FORBIDDEN: &str = "You don't have permission to manage reports.";

/// One page of the server's abuse reports (`GET /v1/reports`). Kept in memory only; never cached or logged.
pub(crate) fn reports_list(account: AccountKey, filter: TinyReportFilter) -> Response {
    let saved = match allowed(&account, Some(TINY_FEATURE_ABUSE_REPORTS), "community.reports.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    if !filter.status.is_empty() && !REPORT_STATUSES.contains(&filter.status.as_str()) {
        return refused(CHECK);
    }
    if !filter.kind.is_empty() && !REPORT_KINDS.contains(&filter.kind.as_str()) {
        return refused(CHECK);
    }
    if filter.before.as_deref().is_some_and(|id| !safe_id(id)) {
        return refused(CHECK);
    }
    let limit = match filter.limit {
        0 => 50,
        limit => limit.min(200),
    };
    let mut path = format!("/v1/reports?limit={limit}");
    if !filter.status.is_empty() {
        path.push_str(&format!("&status={}", escape(&filter.status)));
    }
    if !filter.kind.is_empty() {
        path.push_str(&format!("&kind={}", escape(&filter.kind)));
    }
    if let Some(before) = &filter.before {
        path.push_str(&format!("&before={}", escape(before)));
    }
    match asked("GET", &saved, &path, None) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<Vec<TinyReportSummary>>(body) {
            Ok(reports) => Response::TinyReports { reports },
            Err(_) => refused(UNREADABLE),
        },
        Ok((403, _)) => refused(REPORT_FORBIDDEN),
        Ok((404, _)) => refused(REPORT_GONE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Full abuse report (`GET /v1/reports/{id}`). Never cached or logged; 404 never hints that a
/// report about the caller is hidden.
pub(crate) fn report_read(account: AccountKey, report_id: String) -> Response {
    if !safe_id(&report_id) {
        return refused(CHECK);
    }
    let saved = match allowed(&account, Some(TINY_FEATURE_ABUSE_REPORTS), "community.reports.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("GET", &saved, &format!("/v1/reports/{report_id}"), None) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<TinyReportDetail>(body) {
            Ok(report) => Response::TinyReport { report: Box::new(report) },
            Err(_) => refused(UNREADABLE),
        },
        Ok((403, _)) => refused(REPORT_FORBIDDEN),
        Ok((404, _)) => refused(REPORT_GONE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Update an abuse report (`PATCH /v1/reports/{id}`): status, moderator note, linked audit id, or
/// legal hold (`preserve`, owners only).
pub(crate) fn report_work(account: AccountKey, report_id: String, update: TinyReportUpdate) -> Response {
    if !safe_id(&report_id) {
        return refused(CHECK);
    }
    let saved = match allowed(&account, Some(TINY_FEATURE_ABUSE_REPORTS), "community.reports.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let mut body = Map::new();
    if let Some(status) = update.status {
        if !REPORT_STATUSES.contains(&status.as_str()) {
            return refused(CHECK);
        }
        body.insert("status".to_owned(), Value::String(status));
    }
    if let Some(note) = update.note {
        let trimmed = note.trim();
        if trimmed.is_empty() || trimmed.chars().count() > REPORT_NOTE_MAX {
            return refused("A note is 1 to 2000 characters.");
        }
        body.insert("note".to_owned(), Value::String(trimmed.to_owned()));
    }
    if let Some(audit_id) = update.link_audit_id {
        if audit_id <= 0 {
            return refused(CHECK);
        }
        body.insert("link_audit_id".to_owned(), json!(audit_id));
    }
    if let Some(preserve) = update.preserve {
        body.insert("preserve".to_owned(), Value::Bool(preserve));
    }
    if body.is_empty() {
        return refused(CHECK);
    }
    match asked("PATCH", &saved, &format!("/v1/reports/{report_id}"), Some(Value::Object(body))) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<TinyReportDetail>(body) {
            Ok(report) => Response::TinyReport { report: Box::new(report) },
            Err(_) => refused(UNREADABLE),
        },
        Ok((403, _)) => refused(REPORT_FORBIDDEN),
        Ok((404, _)) => refused(REPORT_GONE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

const BOT_SCOPE: [&str; 3] = ["rooms.read", "rooms.write", "members.read"];

/// The server's bots. The rows have no token.
pub(crate) fn bots_list(account: AccountKey) -> Response {
    let saved = match allowed(&account, None, "community.bots.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("GET", &saved, "/v1/bots", None) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<Vec<TinyBot>>(body) {
            Ok(bots) => Response::TinyBots { bots },
            Err(_) => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Creates a bot. The token comes back once, in `TinyBotCreated`, and is not stored here.
pub(crate) fn bots_create(account: AccountKey, name: String, scopes: Vec<String>) -> Response {
    let saved = match allowed(&account, None, "community.bots.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let Some(name) = text_within(&name, 80).filter(|name| !name.is_empty()) else {
        return refused(BOT_NAME);
    };
    if scopes.len() > BOT_SCOPE.len() || scopes.iter().any(|scope| !BOT_SCOPE.contains(&scope.as_str())) {
        return refused(BOT_SCOPES);
    }
    let mut granted = Vec::new();
    for scope in scopes {
        if !granted.contains(&scope) {
            granted.push(scope);
        }
    }
    match asked("POST", &saved, "/v1/bots", Some(json!({ "name": name, "scopes": granted }))) {
        Err(detail) => refused(detail),
        Ok((201, body)) => match created_bot(body) {
            Some((bot, token)) => Response::TinyBotCreated { bot, token },
            None => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Revokes one bot. A 204 is done.
pub(crate) fn bots_revoke(account: AccountKey, bot_id: String) -> Response {
    if !safe_id(&bot_id) {
        return refused(CHECK);
    }
    let saved = match allowed(&account, None, "community.bots.manage") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    sent(&saved, "DELETE", &format!("/v1/bots/{bot_id}"), None, Missing::Plain)
}

/// `POST /v1/bots` returns the bot and its one-time token. The token is not logged.
fn created_bot(body: Value) -> Option<(TinyBot, String)> {
    let token = body.get("token").and_then(Value::as_str).filter(|token| !token.is_empty())?.to_owned();
    let bot = serde_json::from_value(body).ok()?;
    Some((bot, token))
}

/// Permanently deletes this account on the server (`DELETE /v1/me`). On 2xx, wipes local E2EE
/// state and cached knowledge for this server without calling `DELETE /v1/session`. On any refusal
/// (including `409 last_owner`), nothing local is wiped.
pub(crate) fn delete_account(account: &AccountKey) -> Result<(), Response> {
    let saved = open(account)?;
    match asked("DELETE", &saved, "/v1/me", Some(json!({ "confirm": "delete my account" }))) {
        Err(detail) => Err(refused(detail)),
        Ok((200..=299, _)) => {
            super::e2ee::forget_local(&saved);
            crate::hub::lock(&KNOWN).remove(&key(&saved));
            Ok(())
        }
        Ok((409, body)) if body.get("error").and_then(Value::as_str) == Some("last_owner") => {
            Err(refused(DELETE_LAST_OWNER))
        }
        Ok((status, body)) => Err(refused(explain(status, &body, Missing::Plain))),
    }
}

/// Files an abuse report (`POST /v1/reports`) for a message, member, or channel.
/// Never logs `note` or `evidence`.
pub(crate) fn report_create(
    account: AccountKey,
    kind: String,
    target_id: String,
    category: String,
    note: Option<String>,
    evidence: Option<String>,
) -> Response {
    if !REPORT_KINDS.contains(&kind.as_str()) || !safe_id(&target_id) || !REPORT_CATEGORIES.contains(&category.as_str()) {
        return refused(CHECK);
    }
    let has_disallowed_control = |text: &str| text.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'));
    let note = match note {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > REPORT_NOTE_MAX {
                return refused(REPORT_NOTE_SIZE);
            } else if has_disallowed_control(trimmed) {
                return refused(CHECK);
            } else {
                Some(trimmed.to_owned())
            }
        }
        None => None,
    };
    let evidence = match evidence {
        Some(raw) if !raw.is_empty() => {
            if raw.len() > REPORT_EVIDENCE_MAX {
                return refused(REPORT_EVIDENCE_SIZE);
            }
            if has_disallowed_control(&raw) {
                return refused(CHECK);
            }
            Some(raw)
        }
        _ => None,
    };
    let saved = match allowed(&account, Some(TINY_FEATURE_ABUSE_REPORTS), "community.members.read") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let mut payload = Map::new();
    payload.insert("kind".to_owned(), Value::String(kind));
    payload.insert("target_id".to_owned(), Value::String(target_id));
    payload.insert("category".to_owned(), Value::String(category));
    if let Some(note) = note {
        payload.insert("note".to_owned(), Value::String(note));
    }
    if let Some(evidence) = evidence {
        payload.insert("evidence".to_owned(), Value::String(evidence));
    }
    match asked("POST", &saved, "/v1/reports", Some(Value::Object(payload))) {
        Err(detail) => refused(detail),
        Ok((201, body)) => Response::TinyReported {
            duplicate: body.get("duplicate").and_then(Value::as_bool).unwrap_or(false),
        },
        Ok((200, body)) => Response::TinyReported {
            duplicate: body.get("duplicate").and_then(Value::as_bool).unwrap_or(true),
        },
        Ok((429, _)) => refused(REPORT_RATE_LIMIT),
        Ok((404, _)) => refused(REPORT_NOT_FOUND),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Validates a Tiny custom emoji / sticker name: 2 to 32 lowercase ASCII alphanumerics or `_`.
pub(crate) fn valid_emoji_name(name: &str) -> bool {
    let len = name.len();
    (2..=32).contains(&len) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Fetches `GET /v1/emoji` and caches the list in [`KNOWN`]. Also downloads each image into the
/// local media directory up front so displaying an encrypted message never triggers an on-demand
/// request for an emoji image.
pub(crate) fn fetch_emoji_set(saved: &Saved, account: &AccountKey) -> Vec<TinyEmoji> {
    let Ok((200, body)) = asked("GET", saved, "/v1/emoji", None) else {
        return Vec::new();
    };
    let mut items: Vec<TinyEmoji> = serde_json::from_value(body).unwrap_or_default();
    items.retain(|item| safe_id(&item.id) && !item.name.is_empty());
    for item in &mut items {
        if item.kind.is_empty() {
            item.kind = "emoji".to_owned();
        }
        if let Some(path) = emoji_file(saved, account, item) {
            item.path = path.to_string_lossy().into_owned();
        }
    }
    remember(saved, |known| known.emoji = items.clone());
    items
}

fn emoji_file(saved: &Saved, account: &AccountKey, item: &TinyEmoji) -> Option<PathBuf> {
    let key_part = if !item.sha256.is_empty() && item.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("tiny-emoji-{}-{}", item.id, &item.sha256[..item.sha256.len().min(16)])
    } else {
        format!("tiny-emoji-{}", item.id)
    };
    let path = crate::blobs::path_of(&protocol::support_dir().join("media"), account, &key_part);
    if path.is_file() {
        return Some(path);
    }
    let response = super::agent()
        .get(&endpoint(&saved.base_url, &format!("/v1/emoji/{}", item.id)))
        .header("Authorization", &format!("Bearer {}", saved.token))
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    response.into_body().into_reader().take(512 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() || bytes.len() > 512 * 1024 {
        return None;
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    let partial = path.with_extension("part");
    std::fs::write(&partial, &bytes).ok()?;
    std::fs::rename(&partial, &path).ok()?;
    Some(path)
}

/// Lists community custom emoji and stickers (`GET /v1/emoji`).
pub(crate) fn emoji_list(account: AccountKey) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let items = fetch_emoji_set(&saved, &account);
    Response::TinyEmojiList { items }
}

/// Uploads a new custom emoji or sticker (`POST /v1/emoji`, multipart).
pub(crate) fn emoji_create(account: AccountKey, name: String, kind: String, path: String) -> Response {
    let name = name.trim().to_ascii_lowercase();
    if !valid_emoji_name(&name) {
        return refused("Name must be 2 to 32 lowercase letters, numbers, or underscores.");
    }
    let kind = if kind == "sticker" { "sticker" } else { "emoji" };
    let feature = if kind == "sticker" { TINY_FEATURE_STICKERS } else { TINY_FEATURE_CUSTOM_EMOJI };
    let saved = match allowed(&account, Some(feature), "community.server.edit") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let real = match crate::blobs::upload::allowed(&path) {
        Ok((real, _)) => real,
        Err(why) => return refused(&format!("That picture {why}")),
    };
    let max_bytes: u64 = if kind == "sticker" { 512 * 1024 } else { 256 * 1024 };
    let mut bytes = Vec::new();
    if std::fs::File::open(&real).and_then(|f| f.take(max_bytes + 1).read_to_end(&mut bytes)).is_err() {
        return refused("That picture couldn't be read.");
    }
    if bytes.len() as u64 > max_bytes {
        return refused(if kind == "sticker" { "That sticker is too big. The most is 512 KB." } else { "That emoji is too big. The most is 256 KB." });
    }
    let mime = image_kind(&bytes).or_else(|| bytes.starts_with(b"GIF8").then_some("image/gif"));
    let Some(mime) = mime else {
        return refused("That picture isn't a PNG, JPEG, WebP, or GIF.");
    };
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    };
    let boundary = format!("----tinyemoji{}", crate::hub::now());
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n{name}\r\n").as_bytes());
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\n{kind}\r\n").as_bytes());
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}.{ext}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let response = super::agent()
        .post(&endpoint(&saved.base_url, "/v1/emoji"))
        .header("Authorization", &format!("Bearer {}", saved.token))
        .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send(&body[..]);
    match response {
        Err(_) => refused(UNREACHABLE),
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.into_body().read_to_string().unwrap_or_default();
            let val: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            match status {
                200 | 201 => {
                    let _ = fetch_emoji_set(&saved, &account);
                    Response::Ok
                }
                409 if val.get("error").and_then(Value::as_str) == Some("name_taken") => refused("That name is already taken."),
                409 if val.get("error").and_then(Value::as_str) == Some("emoji_limit") => refused("This server has reached its custom emoji limit."),
                _ => refused(explain(status, &val, Missing::Plain)),
            }
        }
    }
}

/// Renames a custom emoji or sticker (`PATCH /v1/emoji/{id}`).
pub(crate) fn emoji_rename(account: AccountKey, emoji_id: String, name: String) -> Response {
    let name = name.trim().to_ascii_lowercase();
    if !safe_id(&emoji_id) || !valid_emoji_name(&name) {
        return refused("Name must be 2 to 32 lowercase letters, numbers, or underscores.");
    }
    let saved = match allowed(&account, Some(TINY_FEATURE_CUSTOM_EMOJI), "community.server.edit") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    match asked("PATCH", &saved, &format!("/v1/emoji/{emoji_id}"), Some(json!({ "name": name }))) {
        Err(detail) => refused(detail),
        Ok((200..=299, _)) => {
            let _ = fetch_emoji_set(&saved, &account);
            Response::Ok
        }
        Ok((409, body)) if body.get("error").and_then(Value::as_str) == Some("name_taken") => refused("That name is already taken."),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Deletes a custom emoji or sticker (`DELETE /v1/emoji/{id}`).
pub(crate) fn emoji_delete(account: AccountKey, emoji_id: String) -> Response {
    if !safe_id(&emoji_id) {
        return refused(CHECK);
    }
    let saved = match allowed(&account, Some(TINY_FEATURE_CUSTOM_EMOJI), "community.server.edit") {
        Ok(saved) => saved,
        Err(answer) => return answer,
    };
    let res = sent(&saved, "DELETE", &format!("/v1/emoji/{emoji_id}"), None, Missing::Plain);
    if res == Response::Ok {
        let _ = fetch_emoji_set(&saved, &account);
    }
    res
}

/// The checks before a call request goes to the live socket: the server has the feature, this
/// account may, and the ids are ids.
pub(crate) fn call_allowed(account: &AccountKey, feature: &str, needs: &[&str], ids: &[&str]) -> Result<(), Response> {
    for id in needs {
        allowed(account, Some(feature), id)?;
    }
    if !ids_ok(ids) {
        return Err(refused(CHECK));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
