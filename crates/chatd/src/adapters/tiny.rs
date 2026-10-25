//! A Tiny server the owner already runs. Every server joined on this Mac shares this network's
//! one keychain item: a single object until a second server is added, then an array of them.
//! Each account is started from its own object. Sign-in is the sheet's (`signin`), never
//! `tinyclient-login`. A signed-in account can list members, invite, revoke an invite, change a
//! role, remove a member, and read the audit. It can rename itself, change its password, and end
//! every one of its sessions. An owner or an admin can make, rename and delete rooms.
//!
//! Each account holds one live socket (`live`): presence, who is in each room's call, and the
//! events that keep the server page current. A room is a voice channel, and joining it takes a
//! seat over that socket. There is no sound yet: no conversations, no media, no WebRTC stack.

pub(crate) mod live;
pub mod signin;

use super::{serve_commands, Command, Context, Signed};
use crate::hub::Hub;
use crate::session;
use protocol::{
    join_host, tiny_can, tiny_name, tiny_password_fits, AccountKey, ErrorCode, Key, Network,
    Presence, Response, TinyAudit, TinyCode, TinyCommunity, TinyInvite, TinyMember, TinyRoom,
    TinySeat,
};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use ureq::tls::{TlsConfig, TlsProvider};

/// How long one request may take. A redirect is not followed, so this is one host.
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Saved {
    base_url: String,
    token: String,
    user_id: String,
    email: String,
    display_name: String,
    role: String,
}

struct Me {
    /// The session tests read this. The account is named by the server's host.
    #[cfg_attr(not(test), allow(dead_code))]
    display_name: String,
    email: String,
}

enum Fault {
    Unauthorized,
    Unreachable,
    /// The server asked for a pause. The password was not checked.
    SlowDown,
    /// The server answered, and the answer was a failure of its own.
    Unavailable,
    BadAnswer,
    /// Words for the sheet, already chosen. Not a password failure.
    Words(&'static str),
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

pub(crate) fn parse(session: &str) -> Result<Saved, String> {
    let saved: Saved = serde_json::from_str(session)
        .map_err(|_| "the Tiny server session is unreadable".to_owned())?;
    if saved.base_url.is_empty() || saved.token.is_empty() || !safe_id(&saved.user_id) {
        return Err("the Tiny server session is incomplete".to_owned());
    }
    if !matches!(saved.role.as_str(), "owner" | "admin" | "member") {
        return Err("the Tiny server session has an unknown role".to_owned());
    }
    Ok(saved)
}

pub fn supported() -> bool {
    true
}

/// One saved server, or every server in an array. A single object is not re-encoded.
fn saved_rows(session: &str) -> Result<Vec<Saved>, String> {
    let value: Value = serde_json::from_str(session)
        .map_err(|_| "the Tiny server session is unreadable".to_owned())?;
    match value {
        Value::Array(rows) => {
            if rows.is_empty() {
                return Err("the Tiny server session is incomplete".to_owned());
            }
            let mut saved = Vec::with_capacity(rows.len());
            for row in rows {
                let text = serde_json::to_string(&row)
                    .map_err(|_| "the Tiny server session is unreadable".to_owned())?;
                saved.push(parse(&text)?);
            }
            Ok(saved)
        }
        _ => Ok(vec![parse(session)?]),
    }
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let value: Value = serde_json::from_str(session)
        .map_err(|_| "the Tiny server session is unreadable".to_owned())?;
    // The name is the host. Two servers can share a display name, and the page switches by it.
    if !value.is_array() {
        let saved = parse(session)?;
        return Ok(vec![Signed {
            account: saved.user_id,
            name: join_host(&saved.base_url),
            session: session.to_owned(),
        }]);
    }
    let mut found = Vec::new();
    for saved in saved_rows(session)? {
        if found.iter().any(|signed: &Signed| signed.account == saved.user_id) {
            continue;
        }
        let one = serde_json::to_string(&saved)
            .map_err(|_| "the Tiny server session is unreadable".to_owned())?;
        found.push(Signed {
            account: saved.user_id,
            name: join_host(&saved.base_url),
            session: one,
        });
    }
    if found.is_empty() {
        return Err("the Tiny server session is incomplete".to_owned());
    }
    Ok(found)
}

/// The session with one server removed. `None` only when that was the last one. An account that
/// is not in the blob leaves the original string untouched.
pub fn without(session: &str, account: &str) -> Option<String> {
    let Ok(value) = serde_json::from_str::<Value>(session) else {
        return Some(session.to_owned());
    };
    if !value.is_array() {
        return match parse(session) {
            Ok(saved) if saved.user_id == account => None,
            _ => Some(session.to_owned()),
        };
    }
    let Some(rows) = value.as_array() else {
        return Some(session.to_owned());
    };
    let mut kept = Vec::new();
    let mut removed = false;
    for row in rows {
        let Ok(text) = serde_json::to_string(row) else {
            return Some(session.to_owned());
        };
        match parse(&text) {
            Ok(saved) if saved.user_id == account => removed = true,
            Ok(saved) => kept.push(saved),
            Err(_) => return Some(session.to_owned()),
        }
    }
    if !removed {
        return Some(session.to_owned());
    }
    match kept.len() {
        0 => None,
        1 => serde_json::to_string(&kept[0]).ok(),
        _ => serde_json::to_string(&kept).ok(),
    }
}

/// Drops a server with the same user or the same address, then appends `fresh`. One server stays
/// a single object, and a replacement of the only server keeps `fresh` as it was given.
pub(crate) fn combine(existing: &str, fresh: &str) -> Result<String, String> {
    let fresh_saved = parse(fresh)?;
    let mut kept = saved_rows(existing)?;
    kept.retain(|saved| saved.user_id != fresh_saved.user_id && saved.base_url != fresh_saved.base_url);
    if kept.is_empty() {
        return Ok(fresh.to_owned());
    }
    kept.push(fresh_saved);
    serde_json::to_string(&kept).map_err(|_| "the Tiny server session is unreadable".to_owned())
}

/// The session with one server's role replaced, in the shape it was stored. `None` when that
/// server is not in it or already has the role.
pub(crate) fn with_role(session: &str, user_id: &str, role: &str) -> Option<String> {
    let value: Value = serde_json::from_str(session).ok()?;
    let mut rows = saved_rows(session).ok()?;
    let saved = rows.iter_mut().find(|saved| saved.user_id == user_id && saved.role != role)?;
    saved.role = role.to_owned();
    if value.is_array() {
        serde_json::to_string(&rows).ok()
    } else {
        serde_json::to_string(&rows[0]).ok()
    }
}

fn saved_for(session: &str, user_id: &str) -> Result<Saved, String> {
    saved_rows(session)?
        .into_iter()
        .find(|saved| saved.user_id == user_id)
        .ok_or_else(|| "the Tiny server session is for another account".to_owned())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .build(),
        )
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent("tinyclient")
        .build()
        .into()
}

/// One request. A 3xx is a failure: the password and the bearer stay on the host that was asked.
fn call(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<(u16, Value), Fault> {
    let agent = agent();
    let bearer = token.map(|token| format!("Bearer {token}"));
    let sent = match method {
        "GET" => {
            let mut request = agent.get(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            request.call()
        }
        "DELETE" => {
            let mut request = agent.delete(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            request.call()
        }
        "POST" => {
            let mut request = agent.post(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            match body {
                Some(body) => request.send_json(body),
                None => request.send_empty(),
            }
        }
        "PATCH" => {
            let mut request = agent.patch(url);
            if let Some(bearer) = bearer.as_deref() {
                request = request.header("Authorization", bearer);
            }
            match body {
                Some(body) => request.send_json(body),
                None => request.send_empty(),
            }
        }
        _ => return Err(Fault::Unreachable),
    };
    let mut response = sent.map_err(|_| Fault::Unreachable)?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(Fault::Unreachable);
    }
    if status == 204 {
        return Ok((status, Value::Null));
    }
    // The body is not logged: a 200 may hold the bearer.
    let parsed = response.body_mut().read_json().unwrap_or(Value::Null);
    Ok((status, parsed))
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{base}{path}")
}

fn post_session(base: &str, email: &str, password: &str) -> Result<Saved, Fault> {
    let (status, body) = call(
        "POST",
        &endpoint(base, "/v1/session"),
        None,
        Some(serde_json::json!({ "email": email, "password": password })),
    )?;
    match status {
        200 => saved_from(base, body),
        401 => Err(Fault::Unauthorized),
        429 => Err(Fault::SlowDown),
        500..=599 => Err(Fault::Unavailable),
        _ => Err(Fault::Unreachable),
    }
}

fn saved_from(base: &str, body: Value) -> Result<Saved, Fault> {
    let answer: SessionAnswer = serde_json::from_value(body).map_err(|_| Fault::BadAnswer)?;
    if answer.token.is_empty()
        || !safe_id(&answer.user_id)
        || !matches!(answer.role.as_str(), "owner" | "admin" | "member")
    {
        return Err(Fault::BadAnswer);
    }
    Ok(Saved {
        base_url: base.to_owned(),
        token: answer.token,
        user_id: answer.user_id,
        email: answer.email,
        display_name: answer.display_name,
        role: answer.role,
    })
}

/// Joining with a code. The same session shape as `post_session`. No bearer is sent.
fn post_accept(
    base: &str,
    token: &str,
    email: &str,
    password: &str,
    display_name: &str,
) -> Result<Saved, Fault> {
    let (status, body) = call(
        "POST",
        &endpoint(base, "/v1/invites/accept"),
        None,
        Some(serde_json::json!({
            "token": token,
            "email": email,
            "password": password,
            "display_name": display_name,
        })),
    )?;
    match status {
        200 => saved_from(base, body),
        401 => Err(Fault::Unauthorized),
        429 => Err(Fault::SlowDown),
        500..=599 => Err(Fault::Unavailable),
        _ => Err(Fault::Words(explain(status, &body, Missing::Code))),
    }
}

#[derive(Deserialize)]
struct SessionAnswer {
    token: String,
    user_id: String,
    email: String,
    display_name: String,
    role: String,
}

#[derive(Deserialize, Default)]
struct MeAnswer {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
}

fn get_me(saved: &Saved) -> Result<Me, Fault> {
    let (status, body) = call(
        "GET",
        &endpoint(&saved.base_url, "/v1/me"),
        Some(&saved.token),
        None,
    )?;
    if status == 401 {
        return Err(Fault::Unauthorized);
    }
    if status != 200 {
        return Err(Fault::Unreachable);
    }
    let answer: MeAnswer = serde_json::from_value(body).unwrap_or_default();
    Ok(Me {
        display_name: answer.display_name,
        email: answer.email,
    })
}

fn delete_session(saved: &Saved) {
    let _ = call(
        "DELETE",
        &endpoint(&saved.base_url, "/v1/session"),
        Some(&saved.token),
        None,
    );
}

const CHECK: &str = "That didn't work. Check it and try again.";
const UNREADABLE: &str = "The server's answer could not be read.";
const UNREACHABLE: &str = "Couldn't reach that server. Check the address and your connection.";
const SIGNED_OUT: &str = "Tiny server signed you out.";
const WRONG_PASSWORD: &str = "Your current password isn't right.";
const PASSWORD_SIZE: &str = "A new password is 12 to 128 characters.";
const NAME_SIZE: &str = "A name is 1 to 80 characters.";
const ROOM_NAME_SIZE: &str = "A room name is 1 to 80 characters.";
const NOT_YOURS: &str = "You can't do that.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Missing {
    Member,
    Code,
    Invite,
    Room,
    Plain,
}

fn error(code: ErrorCode, detail: impl ToString) -> Response {
    Response::Error {
        code,
        detail: detail.to_string(),
    }
}

fn refused(detail: &str) -> Response {
    error(ErrorCode::Refused, detail)
}

fn explain(status: u16, body: &Value, missing: Missing) -> &'static str {
    let code = body.get("error").and_then(Value::as_str).unwrap_or("");
    match (status, code) {
        (401, _) => SIGNED_OUT,
        (403, _) => "You can't do that.",
        (409, "last_owner") => "This server needs an owner. Make someone else the owner first.",
        (409, "already_member") => "That person is already on this server.",
        (409, "email_taken") => "That email is already on this server.",
        (404, _) if missing == Missing::Member => "That person is no longer on this server.",
        (404, _) if missing == Missing::Code => {
            "That code doesn't work. It may have expired or already been used."
        }
        (404, _) if missing == Missing::Invite => "That invite is gone or was already used.",
        (404, _) if missing == Missing::Room => "That room is gone.",
        (400, _) => CHECK,
        (429, _) => "Too many tries. Wait a minute, then try again.",
        (500..=599, _) => "The server had a problem. Try again in a moment.",
        _ => UNREACHABLE,
    }
}

/// The signed-in session for `account`, or a refusal. Nothing is asked of the server here.
fn open(account: &AccountKey) -> Result<Saved, Response> {
    if account.network != Network::Tiny {
        return Err(refused("You can't do that."));
    }
    let session = match session::read(Network::Tiny) {
        session::Stored::Found(session) => session,
        session::Stored::Absent => return Err(refused("You can't do that.")),
        session::Stored::Unreadable(why) => {
            return Err(error(
                ErrorCode::Internal,
                format!("The session could not be read from {}: {why}", session::STORE),
            ));
        }
    };
    match saved_for(&session, &account.account) {
        Ok(saved) => Ok(saved),
        Err(_) => Err(refused("You can't do that.")),
    }
}

fn asked(method: &str, saved: &Saved, path: &str, body: Option<Value>) -> Result<(u16, Value), &'static str> {
    match call(method, &endpoint(&saved.base_url, path), Some(&saved.token), body) {
        Ok(answer) => Ok(answer),
        Err(_) => Err(UNREACHABLE),
    }
}

enum Listed<T> {
    Rows(T),
    /// A 403: the caller may not see this list. The load still succeeds.
    Empty,
    Failed(&'static str),
}

fn listed<T>(saved: &Saved, path: &str, parse: impl FnOnce(Value) -> Result<T, ()>) -> Listed<T> {
    match asked("GET", saved, path, None) {
        Err(detail) => Listed::Failed(detail),
        Ok((403, _)) => Listed::Empty,
        Ok((200, body)) => match parse(body) {
            Ok(rows) => Listed::Rows(rows),
            Err(()) => Listed::Failed(UNREADABLE),
        },
        Ok((status, body)) => Listed::Failed(explain(status, &body, Missing::Plain)),
    }
}

fn members_of(saved: &Saved) -> Result<Vec<TinyMember>, &'static str> {
    match asked("GET", saved, "/v1/members", None) {
        Err(detail) => Err(detail),
        Ok((200, body)) => serde_json::from_value(body).map_err(|_| UNREADABLE),
        Ok((status, body)) => Err(explain(status, &body, Missing::Plain)),
    }
}

#[derive(Deserialize)]
struct RoomWire {
    id: String,
    name: String,
}

/// A server older than rooms answers 404: it has none.
fn rooms_of(saved: &Saved) -> Listed<Vec<TinyRoom>> {
    match asked("GET", saved, "/v1/rooms", None) {
        Err(detail) => Listed::Failed(detail),
        Ok((403 | 404, _)) => Listed::Empty,
        Ok((200, body)) => match serde_json::from_value::<Vec<RoomWire>>(body) {
            Ok(rows) => Listed::Rows(rows.into_iter().map(|row| TinyRoom { id: row.id, name: row.name, participants: Vec::new() }).collect()),
            Err(_) => Listed::Failed(UNREADABLE),
        },
        Ok((status, body)) => Listed::Failed(explain(status, &body, Missing::Plain)),
    }
}

fn invites_of(saved: &Saved) -> Listed<Vec<TinyInvite>> {
    listed(saved, "/v1/invites", |body| serde_json::from_value(body).map_err(|_| ()))
}

#[derive(Deserialize)]
struct AuditWire {
    id: i64,
    #[serde(default)]
    actor_id: Option<String>,
    action: String,
    subject: String,
    #[serde(default)]
    detail: Option<Value>,
    created_at: String,
}

fn detail_text(detail: Option<Value>) -> String {
    match detail {
        None | Some(Value::Null) => String::new(),
        Some(value) => serde_json::to_string(&value).unwrap_or_default(),
    }
}

fn audit_of(saved: &Saved) -> Listed<Vec<TinyAudit>> {
    listed(saved, "/v1/audit?limit=50", |body| {
        let rows: Vec<AuditWire> = serde_json::from_value(body).map_err(|_| ())?;
        Ok(rows
            .into_iter()
            .map(|row| TinyAudit {
                id: row.id,
                actor_id: row.actor_id,
                action: row.action,
                subject: row.subject,
                detail: detail_text(row.detail),
                created_at: row.created_at,
            })
            .collect())
    })
}

/// The page: members, invites and the audit over REST, then the rooms with who is in each
/// call and who is online, as the live socket last told the hub.
pub(crate) fn community(hub: &Hub, account: AccountKey) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    let members = match members_of(&saved) {
        Ok(members) => members,
        Err(detail) => return refused(detail),
    };
    // A 403 on invites or the audit is an empty list. Any other failure fails the load.
    // The role is the one that was saved, not a role from the member list.
    let invites = if tiny_can(&saved.role, "community.invites.read") {
        match invites_of(&saved) {
            Listed::Rows(rows) => rows,
            Listed::Empty => Vec::new(),
            Listed::Failed(detail) => return refused(detail),
        }
    } else {
        Vec::new()
    };
    let audit = if tiny_can(&saved.role, "community.audit.read") {
        match audit_of(&saved) {
            Listed::Rows(rows) => rows,
            Listed::Empty => Vec::new(),
            Listed::Failed(detail) => return refused(detail),
        }
    } else {
        Vec::new()
    };
    let mut rooms = match rooms_of(&saved) {
        Listed::Rows(rows) => rows,
        Listed::Empty => Vec::new(),
        Listed::Failed(detail) => return refused(detail),
    };
    for room in &mut rooms {
        if let Some(inside) = hub.occupants(&Key::new(&account, &room.id)) {
            room.participants = inside
                .people
                .into_iter()
                .map(|occupant| TinySeat { user_id: occupant.person.id, muted: occupant.muted, deafened: occupant.deafened })
                .collect();
        }
    }
    let online = members
        .iter()
        .filter(|member| hub.presence(&Key::new(&account, &member.user_id)) == Presence::Online)
        .map(|member| member.user_id.clone())
        .collect();
    let media = media_of(&saved.base_url);
    Response::TinyCommunity {
        community: TinyCommunity {
            role: saved.role,
            members,
            invites,
            audit,
            origin: saved.base_url,
            rooms,
            online,
            media,
        },
    }
}

/// What `/health` says of the server's audio: `off`, `up` or `down`. Empty when it did not say,
/// as an older server does not. A 503 still says it.
pub(crate) fn media_of(base: &str) -> String {
    match call("GET", &endpoint(base, "/health"), None, None) {
        Ok((_, body)) => body.get("media").and_then(Value::as_str).filter(|media| matches!(*media, "off" | "up" | "down")).unwrap_or("").to_owned(),
        Err(_) => String::new(),
    }
}

/// The signed-in session, when its role may do `id`. Nothing is asked of the server here.
pub(crate) fn may(account: &AccountKey, id: &str) -> Result<Saved, Response> {
    let saved = open(account)?;
    if !tiny_can(&saved.role, id) {
        return Err(refused(NOT_YOURS));
    }
    Ok(saved)
}

pub(crate) fn create_room(account: AccountKey, name: String) -> Response {
    let saved = match may(&account, "community.rooms.create") {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    let Some(name) = tiny_name(&name) else {
        return refused(ROOM_NAME_SIZE);
    };
    match asked("POST", &saved, "/v1/rooms", Some(serde_json::json!({ "name": name }))) {
        Err(detail) => refused(detail),
        Ok((201, _)) => Response::Ok,
        Ok((400, _)) => refused(ROOM_NAME_SIZE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Room)),
    }
}

pub(crate) fn rename_room(account: AccountKey, room_id: String, name: String) -> Response {
    let saved = match may(&account, "community.rooms.rename") {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if !safe_id(&room_id) {
        return refused(CHECK);
    }
    let Some(name) = tiny_name(&name) else {
        return refused(ROOM_NAME_SIZE);
    };
    let path = format!("/v1/rooms/{room_id}");
    match asked("PATCH", &saved, &path, Some(serde_json::json!({ "name": name }))) {
        Err(detail) => refused(detail),
        Ok((200, _)) => Response::Ok,
        Ok((400, _)) => refused(ROOM_NAME_SIZE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Room)),
    }
}

/// Anyone in the room's call leaves it; the server says so over the live socket.
pub(crate) fn delete_room(account: AccountKey, room_id: String) -> Response {
    let saved = match may(&account, "community.rooms.delete") {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if !safe_id(&room_id) {
        return refused(CHECK);
    }
    let path = format!("/v1/rooms/{room_id}");
    match asked("DELETE", &saved, &path, None) {
        Err(detail) => refused(detail),
        Ok((204, _)) => Response::Ok,
        Ok((status, body)) => refused(explain(status, &body, Missing::Room)),
    }
}

/// A call request's account and ids, checked before it reaches the live socket. Kicking is for
/// an owner or an admin; the server checks it again.
pub(crate) fn call_allowed(account: &AccountKey, id: &str, ids: &[&str]) -> Result<(), Response> {
    may(account, id)?;
    if ids.iter().any(|id| !safe_id(id)) {
        return Err(refused(CHECK));
    }
    Ok(())
}

pub(crate) fn invite(account: AccountKey, role: String) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if role != "admin" && role != "member" {
        return refused(CHECK);
    }
    match asked(
        "POST",
        &saved,
        "/v1/invites",
        Some(serde_json::json!({ "role": role })),
    ) {
        Err(detail) => refused(detail),
        Ok((201, body)) => match serde_json::from_value::<TinyCode>(body) {
            Ok(mut code) => {
                // The server's token stays raw. The origin is what the other tinyclient unpacks.
                code.origin = saved.base_url;
                Response::TinyCode { code }
            }
            Err(_) => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

pub(crate) fn set_role(account: AccountKey, user_id: String, role: String) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if !safe_id(&user_id) || !matches!(role.as_str(), "owner" | "admin" | "member") {
        return refused(CHECK);
    }
    let path = format!("/v1/members/{user_id}");
    match asked(
        "PATCH",
        &saved,
        &path,
        Some(serde_json::json!({ "role": role })),
    ) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match body.get("role").and_then(Value::as_str) {
            Some(role) if matches!(role, "owner" | "admin" | "member") => Response::Ok,
            _ => refused(UNREADABLE),
        },
        Ok((status, body)) => refused(explain(status, &body, Missing::Member)),
    }
}

pub(crate) fn remove(account: AccountKey, user_id: String) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if !safe_id(&user_id) {
        return refused(CHECK);
    }
    let path = format!("/v1/members/{user_id}");
    match asked("DELETE", &saved, &path, None) {
        Err(detail) => refused(detail),
        Ok((204, _)) => Response::Ok,
        Ok((status, body)) => refused(explain(status, &body, Missing::Member)),
    }
}

pub(crate) fn revoke(account: AccountKey, invite_id: String) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if !tiny_can(&saved.role, "community.invites.revoke") {
        return refused("You can't do that.");
    }
    if !safe_id(&invite_id) {
        return refused(CHECK);
    }
    let path = format!("/v1/invites/{invite_id}");
    match asked("DELETE", &saved, &path, None) {
        Err(detail) => refused(detail),
        Ok((204, _)) => Response::Ok,
        Ok((status, body)) => refused(explain(status, &body, Missing::Invite)),
    }
}

/// The display name others on the server see. The account itself stays named by its host.
pub(crate) fn rename(account: AccountKey, display_name: String) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    let Some(name) = tiny_name(&display_name) else {
        return refused(NAME_SIZE);
    };
    match asked(
        "PATCH",
        &saved,
        "/v1/me",
        Some(serde_json::json!({ "display_name": name })),
    ) {
        Err(detail) => refused(detail),
        Ok((200, body)) => match serde_json::from_value::<MeAnswer>(body) {
            Ok(me) if !me.display_name.is_empty() => Response::Ok,
            _ => refused(UNREADABLE),
        },
        Ok((400, _)) => refused(NAME_SIZE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// The server ends every other session. This one is kept, so nothing is forgotten here.
pub(crate) fn change_password(account: AccountKey, current: String, new: String) -> Response {
    let saved = match open(&account) {
        Ok(saved) => saved,
        Err(error) => return error,
    };
    if current.is_empty() {
        return refused(WRONG_PASSWORD);
    }
    if !tiny_password_fits(&new) {
        return refused(PASSWORD_SIZE);
    }
    match asked(
        "POST",
        &saved,
        "/v1/me/password",
        Some(serde_json::json!({ "current_password": current, "new_password": new })),
    ) {
        Err(detail) => refused(detail),
        Ok((204, _)) => Response::Ok,
        // A wrong current password is a 401 too. That one is not a sign-out.
        Ok((401, body)) if body.get("error").and_then(Value::as_str) == Some("invalid_credentials") => {
            refused(WRONG_PASSWORD)
        }
        Ok((400, _)) => refused(PASSWORD_SIZE),
        Ok((status, body)) => refused(explain(status, &body, Missing::Plain)),
    }
}

/// Ends every session of this account on the server. A 401 is already the same thing: the
/// caller forgets the session either way.
pub(crate) fn end_sessions(account: &AccountKey) -> Result<(), Response> {
    let saved = open(account)?;
    match asked("DELETE", &saved, "/v1/sessions", None) {
        Err(detail) => Err(refused(detail)),
        Ok((204, _)) | Ok((401, _)) => Ok(()),
        Ok((status, body)) => Err(refused(explain(status, &body, Missing::Plain))),
    }
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    start_with(ctx, session, inbox, live::SERVER)
}

pub(crate) fn start_with(ctx: Context, session: &str, inbox: Receiver<Command>, timing: live::Timing) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.user_id != ctx.account.account {
        return Err("the Tiny server session is for another account".to_owned());
    }
    let live = Arc::new(live::Live::new().map_err(|err| err.to_string())?);
    let (socket, socket_ctx, socket_live) = (saved.clone(), ctx.clone(), live.clone());
    std::thread::Builder::new()
        .name("tiny-live".into())
        .spawn(move || live::keep_live(&socket_ctx, &socket, &socket_live, &timing))
        .map_err(|err| err.to_string())?;
    std::thread::Builder::new()
        .name("tiny-commands".into())
        .spawn(move || {
            serve_commands(
                &ctx,
                inbox,
                Duration::from_millis(200),
                |_| {},
                |command| match command {
                    Command::Tiny(ask) => live.ask(ask),
                    Command::SignOut(reply) => {
                        delete_session(&saved);
                        let _ = reply.send(Ok(()));
                    }
                    other => other.unsupported(&ctx),
                },
            );
            // The socket thread sleeps on the socket; this is how it hears the adapter stopped.
            live.wake();
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn session() -> &'static str {
        r#"{"base_url":"http://127.0.0.1:8080","token":"t","user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner"}"#
    }

    #[test]
    fn a_session_is_one_account_and_a_bad_one_is_an_error() {
        let signed = accounts(session()).unwrap();
        assert_eq!(signed.len(), 1);
        assert_eq!(signed[0].account, "11111111-1111-1111-1111-111111111111");
        assert_eq!(signed[0].name, "127.0.0.1:8080");
        assert_eq!(signed[0].session, session());
        assert!(without(session(), &signed[0].account).is_none());
        assert_eq!(
            without(session(), "someone-else").as_deref(),
            Some(session())
        );
        assert!(accounts("not json").is_err());
        assert!(accounts(r#"{"base_url":"http://127.0.0.1:8080","token":"t","user_id":"../x","email":"a@b.co","display_name":"A","role":"owner"}"#).is_err());
        let saved = parse(session()).unwrap();
        assert_eq!(saved.role, "owner");
        assert_eq!(saved.base_url, "http://127.0.0.1:8080");
        assert_eq!(saved.email, "ada@example.com");
    }

    fn other() -> &'static str {
        r#"{"base_url":"https://other.example","token":"u","user_id":"22222222-2222-2222-2222-222222222222","email":"bea@example.com","display_name":"Bea","role":"member"}"#
    }

    #[test]
    fn a_second_server_stays_beside_the_first_and_the_same_one_replaces() {
        let both = combine(session(), other()).unwrap();
        assert!(both.starts_with('['));
        let signed = accounts(&both).unwrap();
        assert_eq!(signed.len(), 2);
        assert_eq!(signed[0].account, "11111111-1111-1111-1111-111111111111");
        assert_eq!(signed[0].name, "127.0.0.1:8080");
        assert_eq!(signed[0].session, session());
        assert_eq!(signed[1].account, "22222222-2222-2222-2222-222222222222");
        assert_eq!(signed[1].name, "other.example");
        assert_eq!(signed[1].session, other());
        let second = saved_for(&both, "22222222-2222-2222-2222-222222222222").unwrap();
        assert_eq!(second.base_url, "https://other.example");
        assert!(saved_for(&both, "no-such-person").is_err());

        assert_eq!(without(&both, "someone-else").as_deref(), Some(both.as_str()));
        assert_eq!(
            without(&both, "22222222-2222-2222-2222-222222222222").as_deref(),
            Some(session())
        );
        assert!(without(session(), "11111111-1111-1111-1111-111111111111").is_none());

        let moved = r#"{"base_url":"http://127.0.0.1:8080","token":"n","user_id":"33333333-3333-3333-3333-333333333333","email":"cy@example.com","display_name":"Cy","role":"admin"}"#;
        assert_eq!(combine(session(), moved).as_deref(), Ok(moved));
        assert_eq!(combine(session(), session()).as_deref(), Ok(session()));
        let replaced = combine(&both, session()).unwrap();
        let signed = accounts(&replaced).unwrap();
        assert_eq!(signed.len(), 2);
        assert_eq!(signed[0].account, "22222222-2222-2222-2222-222222222222");
        assert_eq!(signed[1].session, session());
        assert!(combine("not json", other()).is_err());
    }

    /// Speaks the session routes on a local socket: wrong password, open, me, delete, and a
    /// redirect that must not be followed.
    #[test]
    fn session_routes_against_a_local_server() {
        let base = serve(|request| {
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("POST /v1/session ") {
                if request.contains("\"password\": \"slow\"") {
                    return http(429, "Too Many Requests", r#"{"error":"slow_down"}"#);
                }
                if request.contains("\"password\": \"boom\"") {
                    return http(500, "Internal Server Error", r#"{"error":"unavailable"}"#);
                }
                if request.contains("\"password\": \"nope\"") {
                    return http(401, "Unauthorized", r#"{"error":"invalid_credentials"}"#);
                }
                return http(
                    200,
                    "OK",
                    r#"{"token":"tok_value","user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner"}"#,
                );
            }
            if first.starts_with("GET /v1/me ") {
                if request.contains("Bearer tok_value") {
                    return http(
                        200,
                        "OK",
                        r#"{"user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner"}"#,
                    );
                }
                return http(401, "Unauthorized", r#"{"error":"unauthorized"}"#);
            }
            if first.starts_with("DELETE /v1/session ") {
                return http(204, "No Content", "");
            }
            if first.starts_with("POST /v1/bounce ") {
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            } else {
                http(404, "Not Found", "{}")
            }
        });

        assert!(matches!(
            post_session(&base, "ada@example.com", "nope"),
            Err(Fault::Unauthorized)
        ));
        assert!(matches!(
            post_session(&base, "ada@example.com", "slow"),
            Err(Fault::SlowDown)
        ));
        assert!(matches!(
            post_session(&base, "ada@example.com", "boom"),
            Err(Fault::Unavailable)
        ));
        let saved = match post_session(&base, "ada@example.com", "correct-password") {
            Ok(saved) => saved,
            Err(_) => panic!("open failed"),
        };
        assert_eq!(saved.user_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(saved.role, "owner");
        assert_eq!(saved.email, "ada@example.com");
        assert_eq!(saved.base_url, base);
        assert_eq!(saved.token, "tok_value");

        let me = match get_me(&saved) {
            Ok(me) => me,
            Err(_) => panic!("me failed"),
        };
        assert_eq!(me.display_name, "Ada");
        assert_eq!(me.email, "ada@example.com");
        delete_session(&saved);

        let bounced = call(
            "POST",
            &endpoint(&base, "/v1/bounce"),
            None,
            Some(serde_json::json!({"email": "ada@example.com", "password": "correct-password"})),
        );
        assert!(matches!(bounced, Err(Fault::Unreachable)));
    }

    /// Against a tinyserver that is already serving. Not part of `cargo test`: it needs
    /// `TINY_LIVE_BASE`, `TINY_LIVE_EMAIL`, and `TINY_LIVE_PASSWORD`.
    #[test]
    #[ignore]
    fn live_owner_round_trip() {
        let base = std::env::var("TINY_LIVE_BASE").expect("TINY_LIVE_BASE");
        let email = std::env::var("TINY_LIVE_EMAIL").expect("TINY_LIVE_EMAIL");
        let password = std::env::var("TINY_LIVE_PASSWORD").expect("TINY_LIVE_PASSWORD");
        let saved = match post_session(&base, &email, &password) {
            Ok(saved) => saved,
            Err(_) => panic!("open failed"),
        };
        assert_eq!(saved.role, "owner");
        assert_eq!(saved.email, email.trim().to_ascii_lowercase());
        let me = match get_me(&saved) {
            Ok(me) => me,
            Err(_) => panic!("me failed"),
        };
        assert_eq!(me.email, saved.email);
        delete_session(&saved);
        assert!(matches!(get_me(&saved), Err(Fault::Unauthorized)));
    }

    /// The account routes against a tinyserver that is already serving, with the same three
    /// variables. The name and the password are put back. It ends every session of that account.
    #[test]
    #[ignore]
    fn live_account_round_trip() {
        let base = std::env::var("TINY_LIVE_BASE").expect("TINY_LIVE_BASE");
        let email = std::env::var("TINY_LIVE_EMAIL").expect("TINY_LIVE_EMAIL");
        let password = std::env::var("TINY_LIVE_PASSWORD").expect("TINY_LIVE_PASSWORD");
        let saved = match post_session(&base, &email, &password) {
            Ok(saved) => saved,
            Err(_) => panic!("open failed"),
        };
        let session = serde_json::to_string(&saved).unwrap();
        let name = saved.display_name.clone();
        let user_id = saved.user_id.clone();
        with_saved(&session, &user_id, |account| {
            assert_eq!(rename(account.clone(), "tinyclient live test".into()), Response::Ok);
            assert_eq!(rename(account.clone(), name.clone()), Response::Ok);
            assert_eq!(rename(account.clone(), "a".repeat(81)), refused(NAME_SIZE));

            let wrong = change_password(account.clone(), "not the password".into(), "tinyclient-live-1".into());
            assert_eq!(wrong, refused(WRONG_PASSWORD));
            let temporary = "tinyclient-live-temporary";
            assert_eq!(change_password(account.clone(), password.clone(), temporary.into()), Response::Ok);
            assert_eq!(change_password(account.clone(), temporary.into(), password.clone()), Response::Ok);

            let code = match invite(account.clone(), "member".into()) {
                Response::TinyCode { code } => code,
                other => panic!("invite: {other:?}"),
            };
            assert_eq!(revoke(account.clone(), code.id.clone()), Response::Ok);
            assert_eq!(revoke(account.clone(), code.id), refused("That invite is gone or was already used."));

            assert!(end_sessions(&account).is_ok());
        });
        assert!(matches!(get_me(&saved), Err(Fault::Unauthorized)));
        assert!(post_session(&base, &email, &password).is_ok(), "the password was put back");
    }

    fn with_session(base: &str, role: &str, run: impl FnOnce(AccountKey)) {
        let user_id = "11111111-1111-1111-1111-111111111111";
        let session = format!(
            r#"{{"base_url":{base:?},"token":"t","user_id":{user_id:?},"email":"ada@example.com","display_name":"Ada","role":{role:?}}}"#
        );
        with_saved(&session, user_id, run);
    }

    fn with_saved(session: &str, user_id: &str, run: impl FnOnce(AccountKey)) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let name = format!("TINYCLIENT_SESSION_{}", Network::Tiny.label().to_uppercase());
        // Not the temp root. A file there is one the user picked, and anything under
        // TINYCLIENT_DIR is refused. These tests share a process with that check.
        let scratch = std::env::temp_dir().join(format!("tinyclient-tiny-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        let _dir = Restore::set("TINYCLIENT_DIR", &scratch);
        let _session = Restore::set(&name, session);
        match crate::session::read(Network::Tiny) {
            crate::session::Stored::Found(_) => {}
            crate::session::Stored::Absent => panic!("tiny session was absent ({name})"),
            crate::session::Stored::Unreadable(why) => panic!("tiny session was unreadable: {why}"),
        }
        run(AccountKey {
            network: Network::Tiny,
            account: user_id.to_owned(),
        });
    }

    /// Puts one environment variable back when the test ends, including on a panic.
    struct Restore {
        key: String,
        previous: Option<std::ffi::OsString>,
    }

    impl Restore {
        fn set(key: &str, value: impl AsRef<std::ffi::OsStr>) -> Restore {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Restore { key: key.to_owned(), previous }
        }
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    #[test]
    fn a_session_scratch_does_not_hide_a_file_the_user_picked() {
        let picked = std::env::temp_dir().join(format!("tinyclient-beside-{}.txt", std::process::id()));
        std::fs::write(&picked, b"hello").unwrap();
        with_session("http://127.0.0.1:9", "member", |_| {
            let answer = crate::blobs::upload::allowed(&picked.to_string_lossy());
            assert!(answer.is_ok(), "{answer:?}");
        });
        let _ = std::fs::remove_file(&picked);
    }

    #[test]
    fn a_member_loads_members_and_does_not_ask_for_invites_or_the_audit() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        let base = serve(move |request| {
            seen.lock().unwrap().push(request.to_owned());
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("GET /v1/members ") {
                return http(
                    200,
                    "OK",
                    r#"[{"user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner","created_at":"2020-01-01T00:00:00Z"}]"#,
                );
            }
            if first.starts_with("GET /v1/rooms ") {
                return http(200, "OK", "[]");
            }
            http(500, "Internal Server Error", r#"{"error":"no"}"#)
        });
        with_session(&base, "member", |account| match community(&test_hub(), account) {
            Response::TinyCommunity { community } => {
                assert_eq!(community.role, "member");
                assert_eq!(community.origin, base);
                assert_eq!(community.members.len(), 1);
                assert_eq!(community.members[0].display_name, "Ada");
                assert_eq!(community.members[0].email, "ada@example.com");
                assert_eq!(community.members[0].role, "owner");
                assert!(community.invites.is_empty());
                assert!(community.audit.is_empty());
            }
            other => panic!("load: {other:?}"),
        });
        let lines = log.lock().unwrap();
        assert!(lines.iter().any(|line| line.contains("GET /v1/members ")));
        assert!(lines
            .iter()
            .all(|line| !line.contains("/v1/invites") && !line.contains("/v1/audit")));
    }

    #[test]
    fn a_forbidden_invite_list_still_returns_the_members() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        let base = serve(move |request| {
            seen.lock().unwrap().push(request.to_owned());
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("GET /v1/members ") {
                return http(
                    200,
                    "OK",
                    r#"[{"user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"member","created_at":"2020-01-01T00:00:00Z"}]"#,
                );
            }
            if first.starts_with("GET /v1/invites ") {
                return http(403, "Forbidden", r#"{"error":"forbidden"}"#);
            }
            if first.starts_with("GET /v1/audit") {
                return http(
                    200,
                    "OK",
                    r#"[{"id":7,"actor_id":"11111111-1111-1111-1111-111111111111","action":"member.role","subject":"22222222-2222-2222-2222-222222222222","detail":{"to":"admin"},"created_at":"2020-01-02T00:00:00Z"},{"id":8,"actor_id":null,"action":"member.remove","subject":"x","detail":null,"created_at":"2020-01-03T00:00:00Z"}]"#,
                );
            }
            if first.starts_with("GET /v1/rooms ") {
                return http(200, "OK", "[]");
            }
            http(500, "Internal Server Error", r#"{"error":"no"}"#)
        });
        with_session(&base, "admin", |account| match community(&test_hub(), account) {
            Response::TinyCommunity { community } => {
                assert_eq!(community.role, "admin");
                assert_eq!(community.members.len(), 1);
                assert_eq!(community.members[0].role, "member");
                assert!(community.invites.is_empty());
                assert_eq!(community.audit.len(), 2);
                assert_eq!(community.audit[0].id, 7);
                assert_eq!(community.audit[0].detail, r#"{"to":"admin"}"#);
                assert_eq!(community.audit[1].detail, "");
                assert_eq!(community.audit[1].actor_id, None);
            }
            other => panic!("load: {other:?}"),
        });
        let lines = log.lock().unwrap();
        assert!(lines.iter().any(|line| line.contains("GET /v1/members ")));
        assert!(lines.iter().any(|line| line.contains("GET /v1/invites ")));
        assert!(lines.iter().any(|line| line.contains("GET /v1/audit?limit=50")));
    }

    fn test_hub() -> Arc<Hub> {
        Arc::new(Hub::new(crate::store::Store::open(std::path::Path::new(":memory:")).expect("a store in memory")))
    }

    const BEA: &str = "22222222-2222-2222-2222-222222222222";
    const LOBBY: &str = "33333333-3333-3333-3333-333333333333";

    /// The rooms come from REST; who sits in each and who is online come from the hub, where
    /// the live socket put them.
    #[test]
    fn the_page_lists_rooms_with_who_is_in_each_call_and_who_is_online() {
        let members = format!(
            r#"[{{"user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada","role":"owner","created_at":"2020-01-01T00:00:00Z"}},{{"user_id":"{BEA}","email":"bea@example.com","display_name":"Bea","role":"member","created_at":"2020-01-01T00:00:00Z"}}]"#
        );
        let rooms = format!(r#"[{{"id":"{LOBBY}","name":"Lobby","created_by":"x","created_at":"t"}},{{"id":"44444444-4444-4444-4444-444444444444","name":"Quiet","created_by":"x","created_at":"t"}}]"#);
        let base = serve(move |request| {
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("GET /v1/members ") {
                return http(200, "OK", &members);
            }
            if first.starts_with("GET /v1/rooms ") {
                return http(200, "OK", &rooms);
            }
            if first.starts_with("GET /health ") {
                assert!(!request.contains("Bearer"), "health is asked without the session");
                return http(200, "OK", r#"{"ok":true,"media":"up"}"#);
            }
            http(200, "OK", "[]")
        });
        let hub = test_hub();
        with_session(&base, "owner", |account| {
            let seat = protocol::Occupant { person: Key::new(&account, BEA), name: "Bea".into(), muted: true, deafened: false, streaming: false };
            hub.set_occupants(Key::new(&account, LOBBY), protocol::Occupants { people: vec![seat], total: 1 });
            hub.set_presence(Key::new(&account, BEA), Presence::Online, "", true);
            match community(&hub, account) {
                Response::TinyCommunity { community } => {
                    assert_eq!(community.rooms.len(), 2);
                    assert_eq!(community.rooms[0].name, "Lobby");
                    assert_eq!(community.rooms[0].participants, [TinySeat { user_id: BEA.into(), muted: true, deafened: false }]);
                    assert_eq!(community.media, "up");
                    assert!(community.rooms[1].participants.is_empty());
                    assert_eq!(community.online, [BEA]);
                }
                other => panic!("load: {other:?}"),
            }
        });
        // A server older than rooms answers 404: no rooms, and the page still loads.
        let older = serve(|request| {
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("GET /v1/rooms ") {
                return http(404, "Not Found", "{}");
            }
            http(200, "OK", "[]")
        });
        with_session(&older, "member", |account| match community(&test_hub(), account) {
            Response::TinyCommunity { community } => {
                assert!(community.rooms.is_empty());
                assert_eq!(community.media, "", "an older server says nothing of audio");
            }
            other => panic!("load: {other:?}"),
        });
    }

    #[test]
    fn an_owner_or_an_admin_makes_renames_and_deletes_rooms_and_a_member_cannot() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        let base = serve(move |request| {
            seen.lock().unwrap().push(request.to_owned());
            let first = request.lines().next().unwrap_or("");
            let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
            if first.starts_with("POST /v1/rooms ") {
                if body.contains("Refused") {
                    return http(400, "Bad Request", r#"{"error":"invalid"}"#);
                }
                return http(201, "Created", &format!(r#"{{"id":"{LOBBY}","name":"Lobby","created_by":"x","created_at":"t"}}"#));
            }
            if first.starts_with(&format!("PATCH /v1/rooms/{LOBBY} ")) {
                return http(200, "OK", &format!(r#"{{"id":"{LOBBY}","name":"Den","created_by":"x","created_at":"t"}}"#));
            }
            if first.starts_with(&format!("DELETE /v1/rooms/{LOBBY} ")) {
                return http(204, "No Content", "");
            }
            http(404, "Not Found", r#"{"error":"not_found"}"#)
        });
        let last = || log.lock().unwrap().last().cloned().unwrap_or_default();
        let count = || log.lock().unwrap().len();
        with_session(&base, "admin", |account| {
            assert_eq!(create_room(account.clone(), "  Lobby  ".into()), Response::Ok);
            assert!(last().starts_with("POST /v1/rooms "));
            assert_eq!(json_body(&last())["name"], "Lobby");
            assert!(last().contains("Bearer t"));
            assert_eq!(create_room(account.clone(), "Refused".into()), refused(ROOM_NAME_SIZE));
            assert_eq!(rename_room(account.clone(), LOBBY.into(), "Den".into()), Response::Ok);
            assert!(last().starts_with(&format!("PATCH /v1/rooms/{LOBBY} ")));
            assert_eq!(json_body(&last())["name"], "Den");
            assert_eq!(rename_room(account.clone(), "44444444".into(), "Den".into()), refused("That room is gone."));
            assert_eq!(delete_room(account.clone(), LOBBY.into()), Response::Ok);
            assert!(last().starts_with(&format!("DELETE /v1/rooms/{LOBBY} ")));
            assert_eq!(delete_room(account.clone(), "44444444".into()), refused("That room is gone."));
            let before = count();
            assert_eq!(create_room(account.clone(), "   ".into()), refused(ROOM_NAME_SIZE));
            assert_eq!(rename_room(account.clone(), LOBBY.into(), "a".repeat(81)), refused(ROOM_NAME_SIZE));
            assert_eq!(rename_room(account.clone(), "a/b".into(), "Den".into()), refused(CHECK));
            assert_eq!(delete_room(account.clone(), "../x".into()), refused(CHECK));
            assert_eq!(count(), before);
            assert!(call_allowed(&account, "community.calls.kick", &[LOBBY, BEA]).is_ok());
            assert_eq!(call_allowed(&account, "community.calls.join", &["a/b"]).unwrap_err(), refused(CHECK));
        });
        with_session(&base, "member", |account| {
            let before = count();
            assert_eq!(create_room(account.clone(), "Lobby".into()), refused(NOT_YOURS));
            assert_eq!(rename_room(account.clone(), LOBBY.into(), "Den".into()), refused(NOT_YOURS));
            assert_eq!(delete_room(account.clone(), LOBBY.into()), refused(NOT_YOURS));
            assert_eq!(call_allowed(&account, "community.calls.kick", &[LOBBY, BEA]).unwrap_err(), refused(NOT_YOURS));
            assert!(call_allowed(&account, "community.calls.join", &[LOBBY]).is_ok());
            assert_eq!(count(), before, "nothing a member may not do is asked of the server");
        });
    }

    #[test]
    fn a_new_role_is_written_into_the_session_it_came_from() {
        let changed = with_role(session(), "11111111-1111-1111-1111-111111111111", "admin").unwrap();
        assert!(!changed.starts_with('['));
        assert_eq!(parse(&changed).unwrap().role, "admin");
        assert_eq!(parse(&changed).unwrap().token, "t");
        assert_eq!(with_role(session(), "11111111-1111-1111-1111-111111111111", "owner"), None, "the same role writes nothing");
        assert_eq!(with_role(session(), "someone-else", "admin"), None);
        let both = combine(session(), other()).unwrap();
        let changed = with_role(&both, "22222222-2222-2222-2222-222222222222", "admin").unwrap();
        assert!(changed.starts_with('['));
        assert_eq!(saved_for(&changed, "22222222-2222-2222-2222-222222222222").unwrap().role, "admin");
        assert_eq!(saved_for(&changed, "11111111-1111-1111-1111-111111111111").unwrap().role, "owner");
    }

    #[test]
    fn an_invite_a_role_and_a_removal_use_the_member_routes() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        let base = serve(move |request| {
            seen.lock().unwrap().push(request.to_owned());
            let first = request.lines().next().unwrap_or("");
            if first.starts_with("POST /v1/invites ") {
                return http(
                    201,
                    "Created",
                    r#"{"id":"inv1","token":"invite-token","role":"member","expires_at":"2026-01-01T00:00:00Z"}"#,
                );
            }
            if first.starts_with("PATCH /v1/members/last-owner ") {
                return http(409, "Conflict", r#"{"error":"last_owner"}"#);
            }
            if first.starts_with("PATCH /v1/members/") {
                return http(200, "OK", r#"{"role":"admin"}"#);
            }
            if first.starts_with("DELETE /v1/members/") {
                return http(204, "No Content", "");
            }
            http(404, "Not Found", r#"{"error":"missing"}"#)
        });
        let refused = |detail: &str| Response::Error {
            code: ErrorCode::Refused,
            detail: detail.to_owned(),
        };
        with_session(&base, "owner", |account| {
            let quiet = || log.lock().unwrap().len();
            let before = quiet();
            assert_eq!(
                set_role(account.clone(), "not a user".into(), "admin".into()),
                refused("That didn't work. Check it and try again.")
            );
            assert_eq!(quiet(), before);
            assert_eq!(
                invite(account.clone(), "owner".into()),
                refused("That didn't work. Check it and try again.")
            );
            assert_eq!(quiet(), before);
            match invite(account.clone(), "member".into()) {
                Response::TinyCode { code } => {
                    assert_eq!(code.token, "invite-token");
                    assert_eq!(code.origin, base);
                    assert_eq!(code.id, "inv1");
                    assert_eq!(code.role, "member");
                    assert_eq!(code.expires_at, "2026-01-01T00:00:00Z");
                }
                other => panic!("invite: {other:?}"),
            }
            let posted = log.lock().unwrap().last().cloned().unwrap_or_default();
            assert!(posted.lines().next().unwrap_or("").starts_with("POST /v1/invites "));
            assert_eq!(json_body(&posted)["role"], "member");
            let id = "22222222-2222-2222-2222-222222222222";
            assert_eq!(set_role(account.clone(), id.into(), "admin".into()), Response::Ok);
            let patched = log.lock().unwrap().last().cloned().unwrap_or_default();
            assert!(patched
                .lines()
                .next()
                .unwrap_or("")
                .starts_with(&format!("PATCH /v1/members/{id} ")));
            assert_eq!(json_body(&patched)["role"], "admin");
            assert_eq!(
                set_role(account.clone(), "last-owner".into(), "member".into()),
                refused("This server needs an owner. Make someone else the owner first.")
            );
            assert_eq!(remove(account.clone(), id.into()), Response::Ok);
            let deleted = log.lock().unwrap().last().cloned().unwrap_or_default();
            assert!(deleted.contains(&format!("DELETE /v1/members/{id} ")), "{deleted}");
            let before = quiet();
            assert_eq!(
                remove(account, "a/b".into()),
                refused("That didn't work. Check it and try again.")
            );
            assert_eq!(quiet(), before);
        });
    }

    /// Revoke, rename, password, and every session, with each error the server can give.
    #[test]
    fn the_account_routes_and_what_their_errors_say() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        let base = serve(move |request| {
            seen.lock().unwrap().push(request.to_owned());
            let first = request.lines().next().unwrap_or("");
            let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
            if first.starts_with("DELETE /v1/invites/gone ") {
                return http(404, "Not Found", r#"{"error":"not_found"}"#);
            }
            if first.starts_with("DELETE /v1/invites/theirs ") {
                return http(403, "Forbidden", r#"{"error":"forbidden"}"#);
            }
            if first.starts_with("DELETE /v1/invites/") {
                return http(204, "No Content", "");
            }
            if first.starts_with("PATCH /v1/me ") {
                if body.contains("Long") {
                    return http(400, "Bad Request", r#"{"error":"invalid"}"#);
                }
                if body.contains("Late") {
                    return http(401, "Unauthorized", r#"{"error":"unauthorized"}"#);
                }
                return http(
                    200,
                    "OK",
                    r#"{"user_id":"11111111-1111-1111-1111-111111111111","email":"ada@example.com","display_name":"Ada L","role":"owner"}"#,
                );
            }
            if first.starts_with("POST /v1/me/password ") {
                if body.contains("wrong-current") {
                    return http(401, "Unauthorized", r#"{"error":"invalid_credentials"}"#);
                }
                if body.contains("ended-session") {
                    return http(401, "Unauthorized", r#"{"error":"unauthorized"}"#);
                }
                if body.contains("server-says-no") {
                    return http(400, "Bad Request", r#"{"error":"invalid"}"#);
                }
                if body.contains("too-many-tries") {
                    return http(429, "Too Many Requests", r#"{"error":"slow_down"}"#);
                }
                return http(204, "No Content", "");
            }
            if first.starts_with("DELETE /v1/sessions ") {
                return http(204, "No Content", "");
            }
            http(500, "Internal Server Error", r#"{"error":"no"}"#)
        });
        let refused = |detail: &str| Response::Error {
            code: ErrorCode::Refused,
            detail: detail.to_owned(),
        };
        let last = || log.lock().unwrap().last().cloned().unwrap_or_default();
        let count = || log.lock().unwrap().len();
        with_session(&base, "owner", |account| {
            let id = "33333333-3333-3333-3333-333333333333";
            assert_eq!(revoke(account.clone(), id.into()), Response::Ok);
            assert!(last().starts_with(&format!("DELETE /v1/invites/{id} ")), "{}", last());
            assert!(last().contains("Bearer t"));
            assert_eq!(
                revoke(account.clone(), "gone".into()),
                refused("That invite is gone or was already used.")
            );
            assert_eq!(revoke(account.clone(), "theirs".into()), refused("You can't do that."));
            let before = count();
            assert_eq!(revoke(account.clone(), "a/b".into()), refused(CHECK));
            assert_eq!(count(), before);

            assert_eq!(rename(account.clone(), "  Ada L  ".into()), Response::Ok);
            assert!(last().starts_with("PATCH /v1/me "));
            assert_eq!(json_body(&last())["display_name"], "Ada L");
            assert_eq!(rename(account.clone(), "Long".into()), refused(NAME_SIZE));
            assert_eq!(rename(account.clone(), "Late".into()), refused(SIGNED_OUT));
            let before = count();
            assert_eq!(rename(account.clone(), "   ".into()), refused(NAME_SIZE));
            assert_eq!(rename(account.clone(), "a".repeat(81)), refused(NAME_SIZE));
            assert_eq!(count(), before);

            let new = "a new password".to_owned();
            assert_eq!(change_password(account.clone(), "old password!".into(), new.clone()), Response::Ok);
            let posted = json_body(&last());
            assert_eq!(posted["current_password"], "old password!");
            assert_eq!(posted["new_password"], "a new password");
            // The one 401 that is not a sign-out.
            let wrong = change_password(account.clone(), "wrong-current".into(), new.clone());
            assert_eq!(wrong, refused("Your current password isn't right."));
            assert_ne!(wrong, refused(SIGNED_OUT));
            assert_eq!(
                change_password(account.clone(), "ended-session".into(), new.clone()),
                refused(SIGNED_OUT)
            );
            assert_eq!(
                change_password(account.clone(), "server-says-no".into(), new.clone()),
                refused(PASSWORD_SIZE)
            );
            assert_eq!(
                change_password(account.clone(), "too-many-tries".into(), new.clone()),
                refused("Too many tries. Wait a minute, then try again.")
            );
            let before = count();
            assert_eq!(change_password(account.clone(), "old password!".into(), "short".into()), refused(PASSWORD_SIZE));
            assert_eq!(change_password(account.clone(), "old password!".into(), "x".repeat(129)), refused(PASSWORD_SIZE));
            assert_eq!(change_password(account.clone(), String::new(), new.clone()), refused(WRONG_PASSWORD));
            assert_eq!(count(), before);

            assert!(end_sessions(&account).is_ok());
            assert!(last().starts_with("DELETE /v1/sessions "));
            assert!(last().contains("Bearer t"));
        });
        with_session(&base, "member", |account| {
            let before = count();
            assert_eq!(revoke(account.clone(), "33333333".into()), refused("You can't do that."));
            assert_eq!(count(), before);
        });
    }

    /// A session the server already ended is still forgotten; a failure is not.
    #[test]
    fn signing_out_everywhere_forgets_an_ended_session_and_keeps_one_on_a_failure() {
        let ended = serve(|_| http(401, "Unauthorized", r#"{"error":"unauthorized"}"#));
        with_session(&ended, "member", |account| assert!(end_sessions(&account).is_ok()));
        let broken = serve(|_| http(500, "Internal Server Error", r#"{"error":"no"}"#));
        with_session(&broken, "member", |account| {
            assert_eq!(
                end_sessions(&account).unwrap_err(),
                refused("The server had a problem. Try again in a moment.")
            );
        });
        with_session("http://127.0.0.1:9", "member", |account| {
            assert_eq!(end_sessions(&account).unwrap_err(), refused(UNREACHABLE));
        });
    }

    fn json_body(request: &str) -> serde_json::Value {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|err| panic!("body {err}"))
    }

    pub(crate) fn http(status: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// One read. A timeout is not the end of the request: the body can arrive in a later packet.
    fn read_some(stream: &mut std::net::TcpStream, got: &mut Vec<u8>, deadline: std::time::Instant) -> bool {
        let mut buf = [0u8; 2048];
        while std::time::Instant::now() < deadline {
            match std::io::Read::read(stream, &mut buf) {
                Ok(0) => return false,
                Err(err) if err.kind() == std::io::ErrorKind::TimedOut || err.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => return false,
                Ok(read) => {
                    got.extend_from_slice(&buf[..read]);
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn serve(handler: impl Fn(&str) -> Vec<u8> + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            listener.set_nonblocking(true).expect("nonblocking");
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                        let mut got = Vec::new();
                        let header_end = loop {
                            if let Some(at) = got.windows(4).position(|window| window == b"\r\n\r\n") {
                                break Some(at + 4);
                            }
                            if !read_some(&mut stream, &mut got, deadline) {
                                break None;
                            }
                        };
                        if let Some(header_end) = header_end {
                            let head = String::from_utf8_lossy(&got[..header_end]);
                            let length = head.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            });
                            if let Some(length) = length {
                                while got.len() < header_end + length && read_some(&mut stream, &mut got, deadline) {}
                            }
                        }
                        let request = String::from_utf8_lossy(&got).into_owned();
                        let _ = std::io::Write::write_all(&mut stream, &handler(&request));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        format!("http://127.0.0.1:{port}")
    }
}
