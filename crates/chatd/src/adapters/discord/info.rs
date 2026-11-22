//! What the info pane, the card and the conversation menu ask of Discord: a profile in a server,
//! a conversation's and a server's info, a mute, leaving a group or closing a DM, the pins, and
//! the server-side search for what was shared. Each is one request at most, and only because
//! the user opened or clicked something.

use super::super::{About, ConversationAbout, Refusal, Reply, SearchScope, SharedPage, SpaceAbout};
use super::model::{self, number, GuildSettings, Posted};
use super::rest::Refused;
use super::{escaped, refusal, Discord};
use crate::hub::now;
use base64::Engine;
use protocol::{ConversationKind, MediaKind, Message, MessageHit, Notify, SharedItem, SharedKind, Style};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Profiles are watched by Discord's anti-abuse system and rate limited: one in this long.
const PROFILE_EVERY: Duration = Duration::from_secs(3);
/// Profiles take their turn one at a time, on threads of their own.
static PROFILE_TURN: Mutex<()> = Mutex::new(());
/// Discord's search answers 25 messages a page at most.
const SEARCH_PAGE: u32 = 25;
/// Most characters Discord's message search accepts in `content`.
const SEARCH_MAX_CHARS: usize = 1024;
/// Most characters Discord accepts in a private note on a user.
const NOTE_MAX_CHARS: usize = 256;

#[derive(Deserialize)]
struct NotePayload {
    #[serde(default)]
    note: Option<String>,
}

/// Recent message searches kept in memory per account, so the same scope and words are not
/// asked twice in a session.
const REMEMBERED_SEARCHES: usize = 32;

/// Discord thread-member flags: 2 all messages, 4 mentions, 8 nothing.
pub(super) fn thread_notify_word(flags: u32) -> Option<&'static str> {
    if flags & 8 != 0 {
        Some("nothing")
    } else if flags & 4 != 0 {
        Some("mentions")
    } else if flags & 2 != 0 {
        Some("all")
    } else {
        None
    }
}

#[derive(Default)]
pub(super) struct MessageSearch {
    in_flight: bool,
    last: Option<Instant>,
    remembered: std::collections::VecDeque<(SearchScope, String, Vec<MessageHit>)>,
}

/// The file's magic matches the extension. A renamed text file is not an image.
fn image_magic(ext: &str, bytes: &[u8]) -> bool {
    let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    let jpeg = bytes.starts_with(&[0xFF, 0xD8, 0xFF]);
    let gif = bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a");
    let webp = bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP";
    match ext {
        "png" => png,
        "jpg" | "jpeg" => jpeg,
        "gif" => gif,
        "webp" => webp,
        _ => false,
    }
}

fn refused(why: impl std::fmt::Display) -> Refusal {
    Refusal::Refused(why.to_string())
}

/// Extracts `YYYY-MM-DD` from an ISO 8601 timestamp like `2022-05-10T12:00:00+00:00`.
fn iso_date(raw: &str) -> Option<String> {
    let prefix = raw.get(0..10)?;
    model::iso_seconds(raw)?;
    Some(prefix.to_owned())
}
/// Translates refusals for owner-only sensitive guild operations (delete / transfer), including
/// Discord's 2FA requirement (`code == 60003` or `60008`).
fn sensitive_guild_refusal(err: Refused, action: &str) -> Refusal {
    match err {
        Refused::Status { code: 60003, .. } => Refusal::Refused(format!(
            "Discord requires your 2FA code to {action}. Enter your 6-digit authenticator code and try again, or do it in Discord."
        )),
        Refused::Status { code: 60008, .. } => {
            Refusal::Refused("That 2FA code was rejected. Check your authenticator and try again.".to_owned())
        }
        Refused::Challenge => Refusal::Refused(format!(
            "Discord asked for an extra verification step tinyclient cannot complete. Complete this in Discord to {action}."
        )),
        other => refusal(other),
    }
}

/// Builds the JSON payload for `POST`/`PATCH /guilds/{id}/scheduled-events`.
fn build_event_body(edit: &protocol::SpaceEventEdit) -> Result<serde_json::Value, Refusal> {
    let name = edit.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(refused("Event name must be between 1 and 100 characters."));
    }
    if edit.start <= 0 {
        return Err(refused("Choose a valid start time for the event."));
    }
    let entity_type: u8 = match edit.place_kind {
        protocol::EventPlaceKind::Stage => 1,
        protocol::EventPlaceKind::Voice => 2,
        protocol::EventPlaceKind::External => 3,
    };
    let mut body = serde_json::Map::new();
    body.insert("name".into(), json!(name));
    body.insert("privacy_level".into(), json!(2));
    body.insert("entity_type".into(), json!(entity_type));
    body.insert("scheduled_start_time".into(), json!(model::format_iso_utc(edit.start)));
    if !edit.description.trim().is_empty() {
        let desc = edit.description.trim();
        if desc.chars().count() > 1000 {
            return Err(refused("Event description can be at most 1000 characters."));
        }
        body.insert("description".into(), json!(desc));
    }
    match edit.place_kind {
        protocol::EventPlaceKind::Voice | protocol::EventPlaceKind::Stage => {
            let ch = edit
                .channel_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| refused("Choose a voice or stage channel for this event."))?;
            body.insert("channel_id".into(), json!(ch));
            if let Some(end) = edit.end.filter(|e| *e > edit.start) {
                body.insert("scheduled_end_time".into(), json!(model::format_iso_utc(end)));
            }
        }
        protocol::EventPlaceKind::External => {
            let loc = edit
                .location
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| refused("Enter a location for an external event."))?;
            let end = edit
                .end
                .filter(|e| *e > edit.start)
                .ok_or_else(|| refused("External events require an end time after the start time."))?;
            body.insert("channel_id".into(), serde_json::Value::Null);
            body.insert("entity_metadata".into(), json!({"location": loc}));
            body.insert("scheduled_end_time".into(), json!(model::format_iso_utc(end)));
        }
    }
    if let Some(img) = &edit.image {
        let trimmed = img.trim();
        if trimmed.is_empty() {
            body.insert("image".into(), serde_json::Value::Null);
        } else {
            let uri = super::read_image_data_uri(trimmed, super::DISCORD_IMAGE_MAX)?;
            body.insert("image".into(), json!(uri));
        }
    }
    Ok(serde_json::Value::Object(body))
}

/// Returns a new guild feature list with `"INVITES_DISABLED"` present iff `paused` is true.
fn toggle_invites_disabled_feature(features: &[String], paused: bool) -> Vec<String> {
    let mut out: Vec<String> = features
        .iter()
        .filter(|f| f.as_str() != "INVITES_DISABLED")
        .cloned()
        .collect();
    if paused {
        out.push("INVITES_DISABLED".to_owned());
    }
    out
}

#[cfg(test)]
mod invite_tests {
    use super::*;

    #[test]
    fn toggle_invites_disabled_adds_and_removes_idempotently() {
        let base = vec!["COMMUNITY".to_owned(), "VANITY_URL".to_owned()];
        let paused = toggle_invites_disabled_feature(&base, true);
        assert!(paused.iter().any(|f| f == "INVITES_DISABLED"));
        assert_eq!(paused.iter().filter(|f| f.as_str() == "INVITES_DISABLED").count(), 1);
        // Pausing twice never duplicates the feature flag.
        let paused_again = toggle_invites_disabled_feature(&paused, true);
        assert_eq!(paused_again, paused);
        // Resuming removes it while preserving other features.
        let resumed = toggle_invites_disabled_feature(&paused, false);
        assert_eq!(resumed, base);
    }
}

#[cfg(test)]
mod role_tests {
    use super::model::{ADMINISTRATOR, MANAGE_ROLES};
    use super::{may_grant, role_rights};

    #[test]
    fn role_rights_follow_hierarchy_and_skip_managed_and_everyone() {
        let (editable, assignable) = role_rights(true, 5, 4777, 4701, 100, false);
        assert!(editable && assignable, "the owner edits and assigns a role above everyone else");
        let (editable, assignable) = role_rights(false, 5, 4777, 4701, 1, false);
        assert!(editable && assignable, "a role below our highest is editable and assignable");
        let (editable, assignable) = role_rights(false, 5, 4777, 4702, 5, false);
        assert!(!editable && !assignable, "our own highest role is not below us");
        let (editable, assignable) = role_rights(false, 5, 4777, 4703, 10, false);
        assert!(!editable && !assignable, "a role above us is out of reach");
        let (editable, assignable) = role_rights(true, i32::MAX, 4777, 4704, 2, true);
        assert!(!editable && !assignable, "a managed role stays put even for the owner");
        let (editable, assignable) = role_rights(false, 5, 4777, 4777, 0, false);
        assert!(editable && !assignable, "everyone can be edited when it sits below us, never assigned");
    }

    #[test]
    fn may_grant_only_adds_bits_the_actor_holds() {
        assert!(!may_grant(MANAGE_ROLES, 0, ADMINISTRATOR), "Manage Roles alone cannot add Administrator");
        assert!(may_grant(MANAGE_ROLES | ADMINISTRATOR, 0, ADMINISTRATOR));
        assert!(may_grant(u64::MAX, 0, ADMINISTRATOR));
        assert!(may_grant(MANAGE_ROLES, ADMINISTRATOR, 0), "taking away a bit we lack is allowed");
        assert!(may_grant(MANAGE_ROLES, ADMINISTRATOR, ADMINISTRATOR | MANAGE_ROLES));
    }
}

#[derive(Deserialize)]
struct GuildCounts {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    banner: Option<String>,
    #[serde(default)]
    premium_tier: Option<u8>,
    #[serde(default)]
    premium_subscription_count: Option<u32>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    vanity_url_code: Option<String>,
    #[serde(default)]
    system_channel_id: Option<String>,
    #[serde(default)]
    afk_channel_id: Option<String>,
    #[serde(default)]
    afk_timeout: Option<u32>,
    #[serde(default)]
    default_message_notifications: Option<u8>,
    #[serde(default)]
    approximate_member_count: Option<u32>,
    #[serde(default)]
    approximate_presence_count: Option<u32>,
}

#[derive(Deserialize)]
struct Pin {
    message: Posted,
}

#[derive(Deserialize)]
struct Pins {
    #[serde(default)]
    items: Vec<Pin>,
}

#[derive(Deserialize)]
struct Found {
    #[serde(default)]
    total_results: Option<u32>,
    /// Each hit is a list holding the message (older answers added context around it).
    #[serde(default)]
    messages: Option<Vec<Vec<Posted>>>,
    #[serde(default)]
    code: Option<u64>,
}

/// What one message holds of a kind, as the cache's own listing finds it (`Store::shared`).
pub(super) fn shared_items(message: &Message, kind: SharedKind) -> Vec<SharedItem> {
    let item = |attachment, url: &str, title: &str| SharedItem {
        message_id: message.id.clone(),
        sent: message.sent,
        sender: message.sender.name.clone(),
        attachment,
        url: url.to_owned(),
        title: title.to_owned(),
    };
    let pictured = |kind: MediaKind| matches!(kind, MediaKind::Image | MediaKind::Video | MediaKind::Gif);
    match kind {
        SharedKind::Media | SharedKind::Files => {
            let wanted = message.attachments.iter().filter(|attachment| !attachment.sticker && pictured(attachment.file.kind) == (kind == SharedKind::Media));
            wanted.map(|attachment| item(Some(attachment.clone()), "", "")).collect()
        }
        SharedKind::Links => {
            let mut items: Vec<SharedItem> = message.previews.iter().map(|preview| item(None, &preview.url, &preview.title)).collect();
            for span in &message.spans {
                if let Style::Link { url } = &span.style {
                    if !items.iter().any(|known| known.url == *url) {
                        items.push(item(None, url, ""));
                    }
                }
            }
            items
        }
    }
}

/// A Discord snowflake: digits only, at most 20, and not zero. The returned text is the number,
/// so a path never keeps whatever punctuation arrived with it.
fn snowflake(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 20 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let n = raw.parse::<u64>().ok()?;
    (n != 0).then_some(n)
}

fn discord_id(raw: &str, why: &str) -> Result<(u64, String), Refusal> {
    let n = snowflake(raw).ok_or_else(|| refused(why))?;
    Ok((n, n.to_string()))
}

fn role_name(name: &str) -> Result<String, Refusal> {
    let name = name.trim();
    let count = name.chars().count();
    if count == 0 {
        return Err(refused("Enter a role name."));
    }
    if count > 100 {
        return Err(refused("Role name must be between 1 and 100 characters."));
    }
    Ok(name.to_owned())
}

/// `(editable, assignable)`. Managed roles are neither. The owner bypasses the position check;
/// Administrator does not. `@everyone` is never assignable.
fn role_rights(owner: bool, highest: i32, guild_id: u64, role_id: u64, position: i32, managed: bool) -> (bool, bool) {
    if managed {
        return (false, false);
    }
    let editable = owner || position < highest;
    let assignable = editable && role_id != guild_id;
    (editable, assignable)
}

/// Newly set bits must be ones the actor already holds. `u64::MAX` is the owner or Administrator.
/// Clearing a bit the actor lacks is allowed.
fn may_grant(actor: u64, before: u64, after: u64) -> bool {
    if actor == u64::MAX {
        return true;
    }
    let added = after & !before;
    added & !actor == 0
}

fn confirm_admin(before: u64, after: u64, confirmed: bool) -> Result<(), Refusal> {
    let bit = model::ADMINISTRATOR;
    if after & bit != 0 && before & bit == 0 && !confirmed {
        return Err(refused("Confirm before granting the Administrator permission."));
    }
    Ok(())
}

fn highest_of(owner: bool, info: &super::GuildInfo) -> i32 {
    if owner {
        return i32::MAX;
    }
    info.my_roles.iter().filter_map(|id| info.roles.get(id).map(|role| role.position)).max().unwrap_or(0)
}

fn space_role(guild_num: u64, role_id: u64, role: &super::RoleInfo, owner: bool, highest: i32) -> protocol::SpaceRole {
    let (editable, assignable) = role_rights(owner, highest, guild_num, role_id, role.position, role.managed);
    let icon = match (&role.icon, &role.unicode_emoji) {
        (Some(hash), _) => model::role_icon_url(&role_id.to_string(), hash).unwrap_or_default(),
        (None, Some(emoji)) => emoji.clone(),
        (None, None) => String::new(),
    };
    protocol::SpaceRole {
        id: role_id.to_string(),
        name: role.name.clone(),
        color: role.color,
        hoist: role.hoist,
        position: role.position,
        mentionable: role.mentionable,
        managed: role.managed,
        icon,
        permissions: role.permissions,
        editable,
        assignable,
    }
}

/// Picture and emoji are mutually exclusive on the wire. A clear of one leaves the other alone.
fn role_body(edit: &protocol::RoleEdit, include_name: bool) -> Result<serde_json::Map<String, serde_json::Value>, Refusal> {
    let picture_path = edit.icon.as_deref().map(str::trim);
    if picture_path.is_some_and(|path| path.contains("://")) {
        return Err(refused("Choose a picture on this computer."));
    }
    let emoji_value: Option<Option<&str>> = match edit.unicode_emoji.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(text) => {
            if model::role_emoji(Some(text)).is_none() {
                return Err(refused("That emoji cannot be used on a role."));
            }
            Some(Some(text))
        }
    };
    let picture_is_file = picture_path.is_some_and(|path| !path.is_empty());
    if picture_is_file && matches!(emoji_value, Some(Some(_))) {
        return Err(refused("A role can have a picture or an emoji, not both."));
    }
    let picture_value: Option<Option<String>> = match picture_path {
        None => None,
        Some("") => Some(None),
        Some(path) => Some(Some(super::read_image_data_uri(path, super::DISCORD_IMAGE_MAX)?)),
    };
    let mut body = serde_json::Map::new();
    if include_name {
        if let Some(name) = edit.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
            body.insert("name".into(), json!(name));
        }
    }
    if let Some(color) = edit.color {
        if color > 0x00FF_FFFF {
            return Err(refused("Role color must be between #000000 and #FFFFFF."));
        }
        body.insert("color".into(), json!(color));
    }
    if let Some(hoist) = edit.hoist {
        body.insert("hoist".into(), json!(hoist));
    }
    if let Some(mentionable) = edit.mentionable {
        body.insert("mentionable".into(), json!(mentionable));
    }
    if let Some(permissions) = edit.permissions {
        body.insert("permissions".into(), json!(permissions.to_string()));
    }
    match (&picture_value, &emoji_value) {
        (Some(Some(uri)), _) => {
            body.insert("icon".into(), json!(uri));
            body.insert("unicode_emoji".into(), serde_json::Value::Null);
        }
        (_, Some(Some(text))) => {
            body.insert("unicode_emoji".into(), json!(text));
            body.insert("icon".into(), serde_json::Value::Null);
        }
        (Some(None), Some(None)) => {
            body.insert("icon".into(), serde_json::Value::Null);
            body.insert("unicode_emoji".into(), serde_json::Value::Null);
        }
        (Some(None), None) => {
            body.insert("icon".into(), serde_json::Value::Null);
        }
        (None, Some(None)) => {
            body.insert("unicode_emoji".into(), serde_json::Value::Null);
        }
        _ => {}
    }
    Ok(body)
}

#[derive(Clone, Copy)]
struct RoleGate {
    owner: bool,
    highest: i32,
    manage: bool,
    actor: u64,
    guild_num: u64,
}

impl Discord {
    /// A card's profile, off the command thread: it waits its turn (one per [`PROFILE_EVERY`])
    /// there, so sends and everything else behind it go on. A profile still waiting when a
    /// newer one is asked for is dropped unsent; its card was closed or replaced.
    pub(super) fn profile_queued(discord: &Arc<Discord>, person: String, space: Option<String>, reply: Reply<About>) {
        let asked = {
            let mut state = crate::hub::lock(&discord.state);
            state.profile_asked += 1;
            state.profile_asked
        };
        let discord = discord.clone();
        let run = move || {
            let _turn = crate::hub::lock(&PROFILE_TURN);
            let superseded = || crate::hub::lock(&discord.state).profile_asked != asked;
            if superseded() {
                return drop(reply.send(Err(refused("A newer profile was asked for."))));
            }
            let wait = crate::hub::lock(&discord.state).profiled.map_or(Duration::ZERO, |at| PROFILE_EVERY.saturating_sub(at.elapsed()));
            if !wait.is_zero() && discord.ctx.pause(wait) {
                return drop(reply.send(Err(refused("Stopped."))));
            }
            if superseded() {
                return drop(reply.send(Err(refused("A newer profile was asked for."))));
            }
            let _ = reply.send(discord.profile(&person, space.as_deref()));
        };
        if let Err(err) = std::thread::Builder::new().name("discord-profile".into()).spawn(run) {
            tracing::warn!("chatd: Discord: no thread for a profile: {err}");
        }
    }

    /// One person's card: their own words, pronouns and the servers we share; opened from a
    /// server, also their nickname, roles and join date there. Discord does not tell a time
    /// zone. The card draws the picture at 80 points, so its larger sizes are registered here,
    /// for this one person, rather than for everyone ever seen.
    ///
    /// The query is the web client's as the community documents it, unconfirmed; so is
    /// `guild_member` in the answer when `guild_id` is given.
    pub(super) fn profile(&self, person: &str, space: Option<&str>) -> Result<About, Refusal> {
        // `profile_queued` has waited the turn; this marks it taken.
        crate::hub::lock(&self.state).profiled = Some(Instant::now());
        let mut path = format!(
            "/users/{person}/profile?type=popout&with_mutual_guilds=true&with_mutual_friends=false&with_mutual_friends_count=false"
        );
        if let Some(space) = space {
            path.push_str(&format!("&guild_id={space}"));
        }
        let answer = self.call("GET", &path, None).map_err(refusal)?;
        let profile: model::Profile = serde_json::from_str(&answer).map_err(refused)?;
        if let Some(user) = &profile.user {
            let (file, url) = user.avatar();
            let sizes = model::avatar_variants(&file.id);
            let id = file.id.clone();
            let registered = self.ctx.media(&[(file, url)]);
            if let (Ok(()), Some(sizes)) = (registered, sizes) {
                let _ = self.ctx.media_variants(&id, &sizes);
            }
        }
        let mut media: Vec<(protocol::MediaFile, String)> = Vec::new();
        let text = profile.user_profile.as_ref();
        let member_text = profile.guild_member_profile.as_ref();
        let banner = space
            .and_then(|guild| {
                member_text
                    .and_then(|p| p.banner.as_deref())
                    .filter(|h| !h.is_empty())
                    .and_then(|hash| model::member_banner(guild, person, hash))
            })
            .or_else(|| {
                text.and_then(|p| p.banner.as_deref())
                    .filter(|h| !h.is_empty())
                    .or_else(|| profile.user.as_ref().and_then(|u| u.banner.as_deref()).filter(|h| !h.is_empty()))
                    .and_then(|hash| model::user_banner(person, hash))
            });
        let banner_file = banner.map(|(file, url)| {
            media.push((file.clone(), url));
            file
        });
        let accent = member_text
            .and_then(|p| p.accent_color)
            .or_else(|| text.and_then(|p| p.accent_color))
            .or_else(|| profile.user.as_ref().and_then(|u| u.accent_color))
            .or_else(|| member_text.and_then(|p| p.theme_colors.as_ref()).and_then(|c| c.first().copied()))
            .or_else(|| text.and_then(|p| p.theme_colors.as_ref()).and_then(|c| c.first().copied()));

        let mut seen_badges = std::collections::HashSet::new();
        let mut badges = Vec::new();
        for badge in profile.badges.iter().chain(profile.guild_badges.iter()) {
            if !badge.id.is_empty() && !seen_badges.insert(badge.id.clone()) {
                continue;
            }
            let name = if !badge.description.is_empty() {
                badge.description.clone()
            } else {
                badge.id.clone()
            };
            if name.is_empty() {
                continue;
            }
            let icon = badge.icon.as_deref().filter(|h| !h.is_empty()).and_then(|hash| {
                let (file, url) = model::badge_icon(hash)?;
                media.push((file.clone(), url));
                Some(file)
            });
            badges.push(protocol::ProfileBadge { name, icon });
        }
        if !media.is_empty() {
            let _ = self.ctx.media(&media);
        }

        let connections: Vec<protocol::ProfileConnection> = profile
            .connected_accounts
            .iter()
            .filter(|c| !c.kind.is_empty() && !c.name.is_empty())
            .map(|c| protocol::ProfileConnection {
                service: model::connection_label(&c.kind),
                name: c.name.clone(),
                url: model::connection_url(&c.kind, &c.id, &c.name),
                verified: c.verified,
            })
            .collect();
        let server_tag = profile
            .user
            .as_ref()
            .and_then(|u| u.primary_guild.as_ref())
            .filter(|pg| pg.identity_enabled != Some(false))
            .and_then(|pg| pg.tag.clone())
            .unwrap_or_default();
        let mut fields = Vec::new();
        if let Some(date) = profile.premium_since.as_deref().and_then(iso_date) {
            fields.push(("Nitro since".to_owned(), date));
        }
        if let Some(date) = profile.premium_guild_since.as_deref().and_then(iso_date) {
            fields.push(("Boosting since".to_owned(), date));
        }

        let note = if let Some(inline) = profile.note.clone() {
            crate::hub::lock(&self.state).notes.insert(person.to_owned(), inline.clone());
            inline
        } else if let Some(cached) = crate::hub::lock(&self.state).notes.get(person).cloned() {
            cached
        } else {
            match self.call("GET", &format!("/users/@me/notes/{person}"), None) {
                Ok(raw) => {
                    let text = serde_json::from_str::<NotePayload>(&raw)
                        .ok()
                        .and_then(|p| p.note)
                        .unwrap_or_default();
                    crate::hub::lock(&self.state).notes.insert(person.to_owned(), text.clone());
                    text
                }
                Err(Refused::Status { status: 404, .. }) => {
                    crate::hub::lock(&self.state).notes.insert(person.to_owned(), String::new());
                    String::new()
                }
                Err(other) => {
                    self.log("note", other);
                    String::new()
                }
            }
        };

        let state = crate::hub::lock(&self.state);
        let shared = profile.mutual_guilds.iter().filter_map(|guild| state.guilds.get(&guild.id));
        let mut about = About {
            bio: profile.bio(),
            mutual_spaces: shared.map(|guild| guild.name.clone()).take(protocol::MUTUAL_SPACES).collect(),
            mutual_total: profile.mutual_guilds.len() as u32,
            pronouns: text.map(|text| text.pronouns.clone()).unwrap_or_default(),
            accent,
            banner: banner_file,
            badges,
            connections,
            server_tag,
            fields,
            note,
            // An id carries the moment it was made: the day they joined Discord.
            since: Some(model::seconds(person)).filter(|since| *since > 1_420_070_400),
            ..About::default()
        };
        let member = space.zip(profile.guild_member.as_ref());
        if let Some((space, member)) = member {
            let guild = state.guilds.get(space);
            about.nickname = member.nick.clone().unwrap_or_default();
            // Role names in the server's own order of ids, the everyone role left out; colours
            // are never sent on.
            let held: Vec<&String> = member.roles.iter().filter(|role| role.as_str() != space).collect();
            about.role_ids = held.iter().take(100).map(|role| (*role).clone()).collect();
            let names = held.iter().filter_map(|role| guild.and_then(|guild| guild.roles.get(&number(role))).map(|role| role.name.clone()));
            about.roles = names.take(protocol::PROFILE_ROLES).collect();
            about.joined = member.joined_at.as_deref().and_then(model::iso_seconds);
        }
        Ok(about)
    }

    /// A conversation's info, from what READY and the settings already said: nothing is asked
    /// of Discord. Member counts of a server channel need a subscription, so there are none.
    pub(super) fn conversation_info(&self, channel: &str) -> Result<ConversationAbout, Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        let guild = self.guild_of(&known);
        // The topic whole, as kept with the server's channels; the row's is cut to one line.
        let kept = guild.as_ref().and_then(|guild| self.ctx.note(&format!("kept:{guild}")));
        let kept: Vec<model::Kept> = kept.and_then(|kept| serde_json::from_str(&kept).ok()).unwrap_or_default();
        let topic = kept.into_iter().find(|kept| kept.id == channel).and_then(|kept| kept.topic);
        let noted = (known.kind == ConversationKind::Thread).then(|| self.ctx.note(&format!("thread-notify:{channel}"))).flatten();
        let state = crate::hub::lock(&self.state);
        let parent = known.parent.as_deref().or(known.group.as_deref());
        let notify = match noted.as_deref() {
            Some("all") => Notify::All,
            Some("mentions") => Notify::Mentions,
            Some("nothing") => Notify::Nothing,
            _ => match guild.as_ref().map(|guild| state.guilds.get(guild)) {
                Some(Some(info)) => info.quiet.notify(channel, parent, now()),
                Some(None) => Notify::All,
                None => state.quiet_directs.notify(channel, None, now()),
            },
        };
        let members = match known.kind {
            ConversationKind::Direct => Some(2),
            ConversationKind::Group => state.groups.get(channel).map(|group| super::group_seats(&state.me, &group.owner, &group.recipients).len() as u32),
            _ => None,
        };
        Ok(ConversationAbout {
            description: topic.unwrap_or(known.topic),
            members,
            created: Some(model::seconds(channel)).filter(|created| *created > 1_420_070_400),
            notify,
            ..ConversationAbout::default()
        })
    }

    /// A server's info: its description and counts in one request, our nickname, roles and the
    /// notification setting from what READY said.
    ///
    /// Unverified against a real account: whether the web client asks `with_counts` on this
    /// route when its server info opens, rather than reading counts from a subscription.
    pub(super) fn space_info(&self, guild: &str) -> Result<SpaceAbout, Refusal> {
        if !crate::hub::lock(&self.state).guilds.contains_key(guild) {
            return Err(refused("This server is gone."));
        }
        let answer = self.call("GET", &format!("/guilds/{guild}?with_counts=true"), None).map_err(refusal)?;
        let counts: GuildCounts = serde_json::from_str(&answer).map_err(refused)?;
        let (
            description,
            banner_hash,
            boost_level,
            boosts,
            mark,
            nickname,
            roles,
            notify,
            system_channel_id,
            afk_channel_id,
            afk_timeout,
            default_notify,
            vanity_url,
            invites_disabled,
        ) = {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(guild).ok_or_else(|| refused("This server is gone."))?;
            let roles = info
                .my_roles
                .iter()
                .filter(|role| **role != number(guild))
                .filter_map(|role| info.roles.get(role))
                .map(|role| role.name.clone())
                .take(protocol::PROFILE_ROLES)
                .collect();
            let notify = match () {
                _ if info.quiet.server_muted(now()) => Notify::Nothing,
                _ if info.quiet.level == 1 => Notify::Mentions,
                _ if info.quiet.level == 2 => Notify::Nothing,
                _ => Notify::All,
            };
            let description = counts
                .description
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| info.description.clone());
            let banner_hash = counts.banner.or_else(|| info.banner.clone());
            let boost_level = counts.premium_tier.unwrap_or(info.premium_tier);
            let boosts = counts.premium_subscription_count.unwrap_or(info.premium_subscription_count);
            let features = if counts.features.is_empty() { &info.features } else { &counts.features };
            let mark = model::guild_mark(features);
            let invites_disabled = features.iter().any(|f| f == "INVITES_DISABLED");
            let system_channel_id = counts
                .system_channel_id
                .filter(|s| !s.is_empty())
                .or_else(|| info.system_channel_id.clone());
            let afk_channel_id = counts
                .afk_channel_id
                .filter(|s| !s.is_empty())
                .or_else(|| info.afk_channel_id.clone());
            let afk_timeout = counts.afk_timeout.unwrap_or(info.afk_timeout);
            let default_notify = Some(match counts.default_message_notifications.unwrap_or(info.default_message_notifications) {
                1 => Notify::Mentions,
                _ => Notify::All,
            });
            let vanity_url = counts
                .vanity_url_code
                .filter(|s| !s.is_empty())
                .or_else(|| info.vanity_url_code.clone());
            (
                description,
                banner_hash,
                boost_level,
                boosts,
                mark,
                info.nick.clone(),
                roles,
                notify,
                system_channel_id,
                afk_channel_id,
                afk_timeout,
                default_notify,
                vanity_url,
                invites_disabled,
            )
        };
        let banner = match banner_hash.as_deref() {
            Some(hash) => {
                let (file, url) = model::guild_banner(guild, hash);
                let _ = self.ctx.media(&[(file.clone(), url)]);
                Some(file)
            }
            None => None,
        };
        Ok(SpaceAbout {
            description,
            members: counts.approximate_member_count,
            online: counts.approximate_presence_count,
            nickname,
            roles,
            notify,
            banner,
            boost_level,
            boosts,
            mark,
            system_channel_id,
            afk_channel_id,
            afk_timeout,
            default_notify,
            vanity_url,
            invites_disabled,
            ..SpaceAbout::default()
        })
    }

    /// Remembers a thread's own notification level so Details does not show the parent channel's.
    pub(super) fn note_thread_notify(&self, channel: &str, word: &str) {
        self.ctx.set_note(&format!("thread-notify:{channel}"), word);
        self.ctx.info_stale(channel);
    }

    /// Sets a conversation's notifications as Discord's own menu does: one settings change for
    /// its server (`@me` for direct and group conversations). Discord answers with the server's
    /// settings whole, which are applied as if they came from the gateway.
    pub(super) fn set_mute(&self, channel: &str, notify: Notify, until: Option<i64>) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        if known.kind == ConversationKind::Thread {
            let (flags, muted) = match notify {
                Notify::All => (2u32, false),
                Notify::Mentions => (4u32, false),
                Notify::Nothing => (8u32, true),
            };
            let body = json!({
                "flags": flags,
                "muted": muted,
                "mute_config": {"end_time": null, "selected_time_window": -1},
            });
            self.call("PATCH", &format!("/channels/{channel}/thread-members/@me/settings"), Some(body))
                .map_err(refusal)?;
            let word = match notify {
                Notify::All => "all",
                Notify::Mentions => "mentions",
                Notify::Nothing => "nothing",
            };
            self.note_thread_notify(channel, word);
            return self.ctx.muted(channel, notify == Notify::Nothing).map_err(refused);
        }
        let guild = self.guild_of(&known);
        let body = mute_override_body(channel, notify, until, now());
        let path = format!("/users/@me/guilds/{}/settings", guild.as_deref().unwrap_or("@me"));
        let answer = self.call("PATCH", &path, Some(body)).map_err(refusal)?;
        match serde_json::from_str::<GuildSettings>(&answer) {
            Ok(settings) if settings.guild_id == guild => self.settings_changed(settings).map_err(|failed| match failed {
                super::Failure::SignedOut(why) | super::Failure::Dropped(why) => refused(why),
            }),
            // An answer that is not the settings: the row is told what was asked.
            _ => self.ctx.muted(channel, notify == Notify::Nothing).map_err(refused),
        }
    }

    /// Leaves a group DM (`close` false) or closes a DM as Discord's × does (`close` true). Both
    /// are one `DELETE` of the channel; a server's channel is neither. The row goes, as it does
    /// in Discord; a closed DM comes back with its next message.
    pub(super) fn leave(&self, channel: &str, close: bool, silent: bool) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        let path = match (known.kind, close) {
            (ConversationKind::Direct, true) => format!("/channels/{channel}"),
            (ConversationKind::Group, false) => format!("/channels/{channel}?silent={silent}"),
            (_, true) => return Err(refused("Only a direct message can be closed.")),
            (_, false) => return Err(refused("Leave servers and their channels in Discord.")),
        };
        if let Err(failed) = self.call("DELETE", &path, None) {
            // Closing a DM with someone who blocked us is refused. Hide it here instead, so the
            // row and its badge do not come back on the next READY.
            let blocked = matches!(failed, Refused::Status { code: 50007 | 50001 | 50003, .. } | Refused::Status { status: 403, .. });
            if close && blocked {
                return self.ctx.forget_conversation(channel).map_err(refused);
            }
            return Err(refusal(failed));
        }
        {
            let mut state = crate::hub::lock(&self.state);
            state.privates.remove(channel);
            state.groups.remove(channel);
            state.directs.retain(|_, id| id != channel);
        }
        self.ctx.conversation_gone(channel).map_err(refused)
    }

    /// Renames a group DM (`PATCH /channels/{channel}` with `{"name": ...}`).
    pub(super) fn update_group(&self, channel: &str, title: Option<&str>) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        if known.kind != ConversationKind::Group {
            return Err(Refusal::Unsupported);
        }
        let Some(raw) = title else {
            return Err(Refusal::Unsupported);
        };
        let name: String = raw.trim().chars().take(100).collect();
        if name.is_empty() {
            return Err(refused("A group name cannot be empty."));
        }
        let answer = self
            .call("PATCH", &format!("/channels/{channel}"), Some(json!({ "name": name })))
            .map_err(refusal)?;
        if let Ok(updated) = serde_json::from_str::<model::Channel>(&answer) {
            self.direct(&updated, &HashMap::new()).map_err(refused)?;
        } else {
            if let Some(group) = crate::hub::lock(&self.state).groups.get_mut(channel) {
                group.named = true;
            }
            self.ctx.conversation(&super::renamed(&known, &name)).map_err(refused)?;
        }
        self.ctx.info_stale(channel);
        Ok(())
    }

    /// Sets or clears a group DM's icon (`PATCH /channels/{channel}` with a `data:image/...;base64,...`
    /// URI, or `{"icon": null}` when `path` is empty).
    pub(super) fn set_group_icon(&self, channel: &str, path: &str) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        if known.kind != ConversationKind::Group {
            return Err(Refusal::Unsupported);
        }
        let icon_val = if path.trim().is_empty() {
            serde_json::Value::Null
        } else {
            let file_path = std::path::Path::new(path);
            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => return Err(refused("Choose a PNG, JPEG, GIF or WebP image.")),
            };
            let len = std::fs::metadata(file_path)
                .map_err(|err| refused(format!("Could not read picture: {err}")))?
                .len();
            if len == 0 || len > 10 * 1024 * 1024 {
                return Err(refused("Choose an image under 10 MB."));
            }
            let bytes = std::fs::read(file_path).map_err(|err| refused(format!("Could not read picture: {err}")))?;
            if !image_magic(&ext, &bytes) {
                return Err(refused("Choose a PNG, JPEG, GIF or WebP image."));
            }
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            serde_json::Value::String(format!("data:{mime};base64,{encoded}"))
        };
        let answer = self
            .call("PATCH", &format!("/channels/{channel}"), Some(json!({ "icon": icon_val })))
            .map_err(refusal)?;
        if let Ok(updated) = serde_json::from_str::<model::Channel>(&answer) {
            self.direct(&updated, &HashMap::new()).map_err(refused)?;
        }
        self.ctx.info_stale(channel);
        Ok(())
    }

    /// Removes a person from a group DM (`DELETE /channels/{channel}/recipients/{person}`).
    /// Owner-only on Discord; non-owners are refused locally before calling Discord.
    pub(super) fn remove_from_group(&self, channel: &str, person: &str) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        if known.kind != ConversationKind::Group {
            return Err(Refusal::Unsupported);
        }
        {
            let state = crate::hub::lock(&self.state);
            let Some(group) = state.groups.get(channel) else {
                return Err(refused("This group is gone."));
            };
            if group.owner != state.me {
                return Err(refused("Only the group owner can remove people."));
            }
            if person == state.me {
                return Err(refused("Use Leave group to leave this conversation."));
            }
        }
        self.call("DELETE", &format!("/channels/{channel}/recipients/{person}"), None)
            .map_err(refusal)?;
        let title = {
            let mut state = crate::hub::lock(&self.state);
            if let Some(group) = state.groups.get_mut(channel) {
                group.recipients.retain(|id| id != person);
                let named = group.named;
                let recs = group.recipients.clone();
                (!named).then(|| {
                    recs.iter()
                        .filter_map(|id| state.users.get(id).map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
            } else {
                None
            }
        };
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            if let Ok(Some(current)) = self.ctx.known(channel) {
                let _ = self.ctx.conversation(&super::renamed(&current, &title));
            }
        }
        self.ctx.info_stale(channel);
        self.ctx.members_changed(channel);
        Ok(())
    }

    /// Transfers group DM ownership to another member (`PATCH /channels/{channel}` with `{"owner": person}`).
    /// Owner-only on Discord; non-owners and non-members are refused locally.
    pub(super) fn transfer_group_owner(&self, channel: &str, person: &str) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        if known.kind != ConversationKind::Group {
            return Err(Refusal::Unsupported);
        }
        {
            let state = crate::hub::lock(&self.state);
            let Some(group) = state.groups.get(channel) else {
                return Err(refused("This group is gone."));
            };
            if group.owner != state.me {
                return Err(refused("Only the group owner can transfer ownership."));
            }
            if person == state.me || !group.recipients.iter().any(|id| id == person) {
                return Err(refused("Choose another member of this group to make owner."));
            }
        }
        let answer = self
            .call("PATCH", &format!("/channels/{channel}"), Some(json!({ "owner": person })))
            .map_err(refusal)?;
        if let Some(group) = crate::hub::lock(&self.state).groups.get_mut(channel) {
            group.owner = person.to_owned();
        }
        if let Ok(updated) = serde_json::from_str::<model::Channel>(&answer) {
            self.direct(&updated, &HashMap::new()).map_err(refused)?;
        } else {
            let mut row = super::renamed(&known, &known.title);
            row.rights.remove_members = false;
            self.ctx.conversation(&row).map_err(refused)?;
        }
        self.ctx.info_stale(channel);
        self.ctx.members_changed(channel);
        Ok(())
    }

    /// Writes or clears the user's private note on `person` (`PUT /users/@me/notes/{person}`).
    /// Refuses notes longer than 256 characters locally before hitting Discord.
    pub(super) fn set_note(&self, person: &str, text: &str) -> Result<(), Refusal> {
        if text.chars().count() > NOTE_MAX_CHARS {
            return Err(refused(format!("Discord notes can be at most {NOTE_MAX_CHARS} characters.")));
        }
        self.call("PUT", &format!("/users/@me/notes/{person}"), Some(json!({ "note": text })))
            .map_err(refusal)?;
        crate::hub::lock(&self.state).notes.insert(person.to_owned(), text.to_owned());
        // The open card already has this text. Patch the cache; do not refetch the profile.
        self.ctx.patch_profile_note(person, text);
        Ok(())
    }

    /// Leaves a Discord server (`DELETE /users/@me/guilds/{guild}` with `{"lurking": false}`).
    /// Refuses locally if we own the server, since Discord requires transferring ownership or
    /// deleting the server first.
    pub(super) fn leave_space(&self, guild: &str) -> Result<(), Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if info.owner {
                return Err(refused(
                    "You own this server. Transfer ownership or delete it in Discord before leaving.",
                ));
            }
        }
        self.call("DELETE", &format!("/users/@me/guilds/{guild}"), Some(json!({"lurking": false})))
            .map_err(refusal)?;
        {
            let mut state = crate::hub::lock(&self.state);
            state.voice.states.retain(|_, voice| voice.guild.as_deref() != Some(guild));
            if state.members.on_screen.as_deref() == Some(guild) {
                state.members = Default::default();
            }
            state.guilds.remove(guild);
        }
        self.spaces().map_err(refused)
    }

    /// Lists scheduled and active events for a Discord server (`GET /guilds/{guild}/scheduled-events?with_user_count=true`).
    pub(super) fn events(&self, guild: &str) -> Result<Vec<protocol::SpaceEvent>, Refusal> {
        if !crate::hub::lock(&self.state).guilds.contains_key(guild) {
            return Err(refused("This server is gone."));
        }
        let answer = self
            .call("GET", &format!("/guilds/{guild}/scheduled-events?with_user_count=true"), None)
            .map_err(refusal)?;
        let fetched: Vec<model::ScheduledEvent> = serde_json::from_str(&answer).map_err(refused)?;
        let mut covers = Vec::new();
        let stored_list: Vec<super::StoredEvent> = {
            let mut state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get_mut(guild) else {
                return Err(refused("This server is gone."));
            };
            let prev_interest: std::collections::HashMap<String, bool> = info
                .events
                .iter()
                .map(|(id, stored)| (id.clone(), stored.interested))
                .collect();
            info.events.clear();
            for mut ev in fetched {
                if !matches!(ev.status, 1 | 2) {
                    continue;
                }
                if ev.guild_id.is_empty() {
                    ev.guild_id = guild.to_owned();
                }
                if let Some(hash) = ev.image.as_deref().filter(|h| !h.is_empty()) {
                    covers.push(model::event_cover(&ev.id, hash));
                }
                let interested = prev_interest.get(&ev.id).copied().unwrap_or(false);
                info.events.insert(ev.id.clone(), super::StoredEvent { event: ev, interested });
            }
            info.events.values().cloned().collect()
        };
        if !covers.is_empty() {
            let _ = self.ctx.media(&covers);
        }
        let space_key = self.ctx.key(guild);
        let mut out: Vec<protocol::SpaceEvent> = stored_list
            .into_iter()
            .map(|stored| {
                let ev = stored.event;
                let place_kind = match ev.entity_type {
                    1 => protocol::EventPlaceKind::Stage,
                    3 => protocol::EventPlaceKind::External,
                    _ => protocol::EventPlaceKind::Voice,
                };
                let conversation = ev.channel_id.as_deref().filter(|s| !s.is_empty()).map(|id| self.ctx.key(id));
                let place = match place_kind {
                    protocol::EventPlaceKind::External => ev
                        .entity_metadata
                        .as_ref()
                        .and_then(|m| m.location.clone())
                        .unwrap_or_default(),
                    protocol::EventPlaceKind::Voice | protocol::EventPlaceKind::Stage => ev
                        .channel_id
                        .as_deref()
                        .and_then(|ch| self.ctx.known(ch).ok().flatten())
                        .map(|row| row.title)
                        .unwrap_or_default(),
                };
                let status = match ev.status {
                    2 => protocol::EventStatus::Active,
                    3 => protocol::EventStatus::Completed,
                    4 => protocol::EventStatus::Canceled,
                    _ => protocol::EventStatus::Scheduled,
                };
                let cover = ev
                    .image
                    .as_deref()
                    .filter(|h| !h.is_empty())
                    .map(|h| model::event_cover(&ev.id, h).0);
                protocol::SpaceEvent {
                    key: self.ctx.key(&ev.id),
                    space: space_key.clone(),
                    name: ev.name,
                    description: ev.description.unwrap_or_default(),
                    start: ev.scheduled_start_time.as_deref().and_then(model::iso_seconds).unwrap_or(0),
                    end: ev.scheduled_end_time.as_deref().and_then(model::iso_seconds),
                    place_kind,
                    place,
                    conversation,
                    status,
                    interested: stored.interested,
                    count: ev.user_count.unwrap_or(0),
                    cover,
                }
            })
            .collect();
        out.sort_by_key(|ev| (ev.status != protocol::EventStatus::Active, ev.start, ev.key.id.clone()));
        Ok(out)
    }

    /// Marks (`on: true`) or unmarks (`on: false`) ourselves as interested in a server's scheduled event.
    pub(super) fn event_interest(&self, guild: &str, event_id: &str, on: bool) -> Result<(), Refusal> {
        if !crate::hub::lock(&self.state).guilds.contains_key(guild) {
            return Err(refused("This server is gone."));
        }
        let method = if on { "PUT" } else { "DELETE" };
        let path = format!("/guilds/{guild}/scheduled-events/{event_id}/users/@me");
        self.call(method, &path, None).map_err(refusal)?;
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(stored) = state
                .guilds
                .get_mut(guild)
                .and_then(|info| info.events.get_mut(event_id))
            {
                let was = stored.interested;
                stored.interested = on;
                if was != on {
                    let count = stored.event.user_count.unwrap_or(0);
                    stored.event.user_count = Some(if on { count.saturating_add(1) } else { count.saturating_sub(1) });
                }
            }
        }
        self.ctx.events_changed(guild);
        Ok(())
    }

    fn space_event_of(&self, guild: &str, ev: model::ScheduledEvent, interested: bool) -> protocol::SpaceEvent {
        let place_kind = match ev.entity_type {
            1 => protocol::EventPlaceKind::Stage,
            3 => protocol::EventPlaceKind::External,
            _ => protocol::EventPlaceKind::Voice,
        };
        let conversation = ev.channel_id.as_deref().filter(|s| !s.is_empty()).map(|id| self.ctx.key(id));
        let place = match place_kind {
            protocol::EventPlaceKind::External => ev
                .entity_metadata
                .as_ref()
                .and_then(|m| m.location.clone())
                .unwrap_or_default(),
            protocol::EventPlaceKind::Voice | protocol::EventPlaceKind::Stage => ev
                .channel_id
                .as_deref()
                .and_then(|ch| self.ctx.known(ch).ok().flatten())
                .map(|row| row.title)
                .unwrap_or_default(),
        };
        let status = match ev.status {
            2 => protocol::EventStatus::Active,
            3 => protocol::EventStatus::Completed,
            4 => protocol::EventStatus::Canceled,
            _ => protocol::EventStatus::Scheduled,
        };
        let cover = ev
            .image
            .as_deref()
            .filter(|h| !h.is_empty())
            .map(|h| model::event_cover(&ev.id, h).0);
        protocol::SpaceEvent {
            key: self.ctx.key(&ev.id),
            space: self.ctx.key(guild),
            name: ev.name,
            description: ev.description.unwrap_or_default(),
            start: ev.scheduled_start_time.as_deref().and_then(model::iso_seconds).unwrap_or(0),
            end: ev.scheduled_end_time.as_deref().and_then(model::iso_seconds),
            place_kind,
            place,
            conversation,
            status,
            interested,
            count: ev.user_count.unwrap_or(0),
            cover,
        }
    }

    /// Edits a Discord server's overview settings (`PATCH /guilds/{guild}`).
    /// Checks `MANAGE_GUILD` locally before hitting Discord.
    pub(super) fn edit_space(&self, guild: &str, edit: &protocol::SpaceEdit) -> Result<(), Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_space {
                return Err(refused("You need the Manage Server permission to edit server settings."));
            }
        }
        let mut body = serde_json::Map::new();
        if let Some(name) = &edit.name {
            let trimmed = name.trim();
            if !(2..=100).contains(&trimmed.chars().count()) {
                return Err(refused("Server name must be between 2 and 100 characters."));
            }
            body.insert("name".into(), json!(trimmed));
        }
        if let Some(desc) = &edit.description {
            let trimmed = desc.trim();
            if trimmed.chars().count() > 120 {
                return Err(refused("Server description can be at most 120 characters."));
            }
            body.insert(
                "description".into(),
                if trimmed.is_empty() { serde_json::Value::Null } else { json!(trimmed) },
            );
        }
        if let Some(icon) = &edit.icon {
            let trimmed = icon.trim();
            if trimmed.is_empty() {
                body.insert("icon".into(), serde_json::Value::Null);
            } else {
                let uri = super::read_image_data_uri(trimmed, super::DISCORD_IMAGE_MAX)?;
                body.insert("icon".into(), json!(uri));
            }
        }
        if let Some(banner) = &edit.banner {
            let trimmed = banner.trim();
            if trimmed.is_empty() {
                body.insert("banner".into(), serde_json::Value::Null);
            } else {
                let uri = super::read_image_data_uri(trimmed, super::DISCORD_IMAGE_MAX)?;
                body.insert("banner".into(), json!(uri));
            }
        }
        if let Some(sys) = &edit.system_channel_id {
            let trimmed = sys.trim();
            body.insert(
                "system_channel_id".into(),
                if trimmed.is_empty() { serde_json::Value::Null } else { json!(trimmed) },
            );
        }
        if let Some(afk) = &edit.afk_channel_id {
            let trimmed = afk.trim();
            body.insert(
                "afk_channel_id".into(),
                if trimmed.is_empty() { serde_json::Value::Null } else { json!(trimmed) },
            );
        }
        if let Some(timeout) = edit.afk_timeout {
            if !matches!(timeout, 60 | 300 | 900 | 1800 | 3600) {
                return Err(refused("AFK timeout must be 1, 5, 15, 30, or 60 minutes."));
            }
            body.insert("afk_timeout".into(), json!(timeout));
        }
        if let Some(notify) = edit.default_notify {
            let level = match notify {
                Notify::All => 0u8,
                Notify::Mentions | Notify::Nothing => 1u8,
            };
            body.insert("default_message_notifications".into(), json!(level));
        }
        if body.is_empty() {
            return Ok(());
        }
        let raw = self
            .call("PATCH", &format!("/guilds/{guild}"), Some(serde_json::Value::Object(body)))
            .map_err(refusal)?;
        if let Ok(updated) = serde_json::from_str::<model::Guild>(&raw) {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                if updated.name.is_some() {
                    info.name = updated.name().to_owned();
                }
                if edit.icon.is_some() {
                    info.icon = updated.icon().map(str::to_owned);
                }
                if edit.banner.is_some() {
                    info.banner = updated.banner().map(str::to_owned);
                }
                if edit.description.is_some() {
                    info.description = updated.description().unwrap_or_default().to_owned();
                }
                if edit.system_channel_id.is_some() {
                    info.system_channel_id = updated.system_channel_id().map(str::to_owned);
                }
                if edit.afk_channel_id.is_some() {
                    info.afk_channel_id = updated.afk_channel_id().map(str::to_owned);
                }
                if let Some(t) = updated.afk_timeout() {
                    info.afk_timeout = t;
                }
                if let Some(n) = updated.default_message_notifications() {
                    info.default_message_notifications = n;
                }
            }
        }
        self.ctx.space_info_stale(guild);
        self.spaces().map_err(refused)
    }

    /// Deletes a Discord server we own (`POST /guilds/{guild}/delete`).
    pub(super) fn delete_space(&self, guild: &str, mfa_code: Option<&str>) -> Result<(), Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.owner {
                return Err(refused("Only the server owner can delete this server."));
            }
        }
        let code = mfa_code.map(str::trim).filter(|c| !c.is_empty());
        let body = match code {
            Some(c) => json!({"code": c}),
            None => json!({}),
        };
        let path = format!("/guilds/{guild}/delete");
        self.rest
            .call_once_with("POST", &path, Some(body), &[])
            .map_err(|err| sensitive_guild_refusal(err, "delete this server"))?;
        {
            let mut state = crate::hub::lock(&self.state);
            state.voice.states.retain(|_, voice| voice.guild.as_deref() != Some(guild));
            if state.members.on_screen.as_deref() == Some(guild) {
                state.members = Default::default();
            }
            state.guilds.remove(guild);
        }
        self.spaces().map_err(refused)
    }

    /// Transfers ownership of a Discord server we own (`PATCH /guilds/{guild}` with `owner_id`).
    pub(super) fn transfer_space(
        &self,
        guild: &str,
        new_owner_id: &str,
        mfa_code: Option<&str>,
    ) -> Result<(), Refusal> {
        let new_owner_id = new_owner_id.trim();
        if new_owner_id.is_empty() {
            return Err(refused("Choose a member to transfer ownership to."));
        }
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.owner {
                return Err(refused("Only the server owner can transfer ownership."));
            }
            if new_owner_id == state.me {
                return Err(refused("You already own this server."));
            }
        }
        let mut body = json!({"owner_id": new_owner_id});
        if let Some(code) = mfa_code.map(str::trim).filter(|c| !c.is_empty()) {
            body["code"] = json!(code);
        }
        let path = format!("/guilds/{guild}");
        self.rest
            .call_once_with("PATCH", &path, Some(body), &[])
            .map_err(|err| sensitive_guild_refusal(err, "transfer ownership"))?;
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                info.owner = false;
            }
        }
        self.ctx.space_info_stale(guild);
        self.spaces().map_err(refused)
    }

    /// Creates a scheduled event (`POST /guilds/{guild}/scheduled-events`).
    pub(super) fn create_event(
        &self,
        guild: &str,
        edit: &protocol::SpaceEventEdit,
    ) -> Result<protocol::SpaceEvent, Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_events {
                return Err(refused("You need the Manage Events permission to create events."));
            }
        }
        let body = build_event_body(edit)?;
        let raw = self
            .call("POST", &format!("/guilds/{guild}/scheduled-events"), Some(body))
            .map_err(refusal)?;
        let mut ev: model::ScheduledEvent = serde_json::from_str(&raw).map_err(refused)?;
        if ev.guild_id.is_empty() {
            ev.guild_id = guild.to_owned();
        }
        if let Some(hash) = ev.image.as_deref().filter(|h| !h.is_empty()) {
            let _ = self.ctx.media(&[model::event_cover(&ev.id, hash)]);
        }
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                info.events.insert(ev.id.clone(), super::StoredEvent { event: ev.clone(), interested: true });
            }
        }
        self.ctx.events_changed(guild);
        Ok(self.space_event_of(guild, ev, true))
    }

    /// Edits a scheduled event (`PATCH /guilds/{guild}/scheduled-events/{event_id}`).
    pub(super) fn edit_event(
        &self,
        guild: &str,
        event_id: &str,
        edit: &protocol::SpaceEventEdit,
    ) -> Result<protocol::SpaceEvent, Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_events {
                return Err(refused("You need the Manage Events permission to edit events."));
            }
        }
        let body = build_event_body(edit)?;
        let raw = self
            .call("PATCH", &format!("/guilds/{guild}/scheduled-events/{event_id}"), Some(body))
            .map_err(refusal)?;
        let mut ev: model::ScheduledEvent = serde_json::from_str(&raw).map_err(refused)?;
        if ev.guild_id.is_empty() {
            ev.guild_id = guild.to_owned();
        }
        if let Some(hash) = ev.image.as_deref().filter(|h| !h.is_empty()) {
            let _ = self.ctx.media(&[model::event_cover(&ev.id, hash)]);
        }
        let interested = {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                let was = info.events.get(&ev.id).is_some_and(|s| s.interested);
                info.events.insert(ev.id.clone(), super::StoredEvent { event: ev.clone(), interested: was });
                was
            } else {
                false
            }
        };
        self.ctx.events_changed(guild);
        Ok(self.space_event_of(guild, ev, interested))
    }

    /// Deletes or cancels a scheduled event (`DELETE /guilds/{guild}/scheduled-events/{event_id}`).
    pub(super) fn delete_event(&self, guild: &str, event_id: &str) -> Result<(), Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_events {
                return Err(refused("You need the Manage Events permission to delete events."));
            }
        }
        self.call("DELETE", &format!("/guilds/{guild}/scheduled-events/{event_id}"), None)
            .map_err(refusal)?;
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                info.events.remove(event_id);
            }
        }
        self.ctx.events_changed(guild);
        Ok(())
    }

    /// Lists a Discord server's active invites (`GET /guilds/{guild}/invites`), prepending the
    /// server's read-only vanity URL when present, along with the `INVITES_DISABLED` pause flag.
    pub(super) fn space_invites(&self, guild: &str) -> Result<(Vec<protocol::SpaceInvite>, bool), Refusal> {
        let (vanity_known, has_vanity_feature, paused) = {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_space {
                return Err(refused("You need the Manage Server permission to view server invites."));
            }
            (
                info.vanity_url_code.clone(),
                info.features.iter().any(|f| f == "VANITY_URL"),
                info.features.iter().any(|f| f == "INVITES_DISABLED"),
            )
        };

        #[derive(Deserialize)]
        struct InviteChannel {
            #[serde(default)]
            id: String,
            #[serde(default)]
            name: Option<String>,
        }
        #[derive(Deserialize)]
        struct GuildInviteRow {
            code: String,
            #[serde(default)]
            inviter: Option<model::User>,
            #[serde(default)]
            channel: Option<InviteChannel>,
            #[serde(default)]
            uses: u32,
            #[serde(default)]
            max_uses: u32,
            #[serde(default)]
            expires_at: Option<String>,
        }
        #[derive(Deserialize)]
        struct VanityResponse {
            #[serde(default)]
            code: Option<String>,
            #[serde(default)]
            uses: u32,
        }

        let answer = self.call("GET", &format!("/guilds/{guild}/invites"), None).map_err(refusal)?;
        let rows: Vec<GuildInviteRow> = serde_json::from_str(&answer).map_err(refused)?;

        let mut invites = Vec::with_capacity(rows.len() + 1);
        let vanity_pair: Option<(String, u32)> = if let Some(code) = vanity_known.filter(|c| !c.is_empty()) {
            Some((code, 0))
        } else if has_vanity_feature {
            self.call("GET", &format!("/guilds/{guild}/vanity-url"), None)
                .ok()
                .and_then(|raw| serde_json::from_str::<VanityResponse>(&raw).ok())
                .and_then(|v| v.code.filter(|c| !c.is_empty()).map(|c| (c, v.uses)))
        } else {
            None
        };
        if let Some((code, uses)) = vanity_pair {
            invites.push(protocol::SpaceInvite {
                url: format!("https://discord.gg/{code}"),
                code,
                inviter: String::new(),
                channel_id: None,
                channel_name: String::new(),
                uses,
                max_uses: 0,
                expires_at: None,
                vanity: true,
            });
        }

        for row in rows {
            let (channel_id, channel_name) = match row.channel {
                Some(ch) => (
                    (!ch.id.is_empty()).then_some(ch.id),
                    ch.name.unwrap_or_default(),
                ),
                None => (None, String::new()),
            };
            invites.push(protocol::SpaceInvite {
                url: format!("https://discord.gg/{}", row.code),
                code: row.code,
                inviter: row.inviter.map(|u| u.name().to_owned()).unwrap_or_default(),
                channel_id,
                channel_name,
                uses: row.uses,
                max_uses: row.max_uses,
                expires_at: row.expires_at.as_deref().and_then(model::iso_seconds),
                vanity: false,
            });
        }
        Ok((invites, paused))
    }

    /// Revokes one invite link (`DELETE /invites/{code}`). Refuses locally without `MANAGE_GUILD`
    /// or if the code matches the server's vanity URL.
    pub(super) fn revoke_invite(&self, guild: &str, code: &str) -> Result<(), Refusal> {
        let code = code.trim();
        if code.is_empty() {
            return Err(refused("Missing invite code."));
        }
        {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_space {
                return Err(refused("You need the Manage Server permission to revoke invites."));
            }
            if info.vanity_url_code.as_deref() == Some(code) {
                return Err(refused("The server's vanity URL cannot be revoked here."));
            }
        }
        self.call("DELETE", &format!("/invites/{}", escaped(code)), None)
            .map_err(refusal)?;
        Ok(())
    }

    /// Pauses or resumes server invites by toggling Discord's `INVITES_DISABLED` guild feature.
    pub(super) fn set_invites_paused(&self, guild: &str, paused: bool) -> Result<(), Refusal> {
        let updated_features = {
            let state = crate::hub::lock(&self.state);
            let Some(info) = state.guilds.get(guild) else {
                return Err(refused("This server is gone."));
            };
            if !info.space_rights(guild).manage_space {
                return Err(refused("You need the Manage Server permission to pause or resume invites."));
            }
            toggle_invites_disabled_feature(&info.features, paused)
        };
        let raw = self
            .call("PATCH", &format!("/guilds/{guild}"), Some(json!({"features": updated_features})))
            .map_err(refusal)?;
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                if let Ok(updated) = serde_json::from_str::<model::Guild>(&raw) {
                    if !updated.features().is_empty() || !paused {
                        info.features = updated.features().to_vec();
                    } else {
                        info.features = updated_features;
                    }
                } else {
                    info.features = updated_features;
                }
            }
        }
        self.ctx.space_info_stale(guild);
        Ok(())
    }

    fn role_gate(&self, guild: &str) -> Result<RoleGate, Refusal> {
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(guild) else {
            return Err(refused("This server is gone."));
        };
        let owner = info.owner;
        Ok(RoleGate {
            owner,
            highest: highest_of(owner, info),
            manage: info.space_rights(guild).manage_roles,
            actor: info.base_permissions(guild),
            guild_num: number(guild),
        })
    }

    fn role_view(&self, guild: &str, role_id: u64) -> Result<protocol::SpaceRole, Refusal> {
        let state = crate::hub::lock(&self.state);
        let info = state.guilds.get(guild).ok_or_else(|| refused("This server is gone."))?;
        let role = info.roles.get(&role_id).ok_or_else(|| refused("Discord did not return the role."))?;
        let owner = info.owner;
        let highest = highest_of(owner, info);
        Ok(space_role(number(guild), role_id, role, owner, highest))
    }

    fn apply_role(&self, guild: &str, raw: &str) -> Result<protocol::SpaceRole, Refusal> {
        let role: model::Role = serde_json::from_str(raw).map_err(|_| refused("Discord did not return the role."))?;
        let (id, _) = discord_id(&role.id, "Discord did not return the role.")?;
        let _ = self.role_changed(guild, Some(role), None);
        self.role_view(guild, id)
    }

    /// Roles already known from READY and later gateway events. Nothing is asked of Discord.
    pub(super) fn roles(&self, guild: &str) -> Result<Vec<protocol::SpaceRole>, Refusal> {
        let (_, guild) = discord_id(guild, "That server is not a Discord id.")?;
        let state = crate::hub::lock(&self.state);
        let Some(info) = state.guilds.get(&guild) else {
            return Err(refused("This server is gone."));
        };
        if !info.space_rights(&guild).manage_roles {
            return Err(refused("You need the Manage Roles permission to view roles."));
        }
        let owner = info.owner;
        let highest = highest_of(owner, info);
        let guild_num = number(&guild);
        let mut roles: Vec<protocol::SpaceRole> = info.roles.iter().map(|(id, role)| space_role(guild_num, *id, role, owner, highest)).collect();
        drop(state);
        roles.sort_by(|a, b| b.position.cmp(&a.position).then_with(|| a.id.cmp(&b.id)));
        Ok(roles)
    }

    /// `POST /guilds/{id}/roles`. One click, so a 429 is not retried.
    pub(super) fn create_role(&self, guild: &str, edit: &protocol::RoleEdit) -> Result<protocol::SpaceRole, Refusal> {
        let (_, guild) = discord_id(guild, "That server is not a Discord id.")?;
        let gate = self.role_gate(&guild)?;
        if !gate.manage {
            return Err(refused("You need the Manage Roles permission to create a role."));
        }
        role_name(edit.name.as_deref().unwrap_or(""))?;
        let after = edit.permissions.unwrap_or(0);
        if !may_grant(gate.actor, 0, after) {
            return Err(refused("You can only grant permissions you have."));
        }
        confirm_admin(0, after, edit.confirm_admin)?;
        let body = role_body(edit, true)?;
        let raw = self
            .rest
            .call_once_with("POST", &format!("/guilds/{guild}/roles"), Some(serde_json::Value::Object(body)), &[])
            .map_err(refusal)?;
        self.apply_role(&guild, &raw)
    }

    /// `PATCH /guilds/{id}/roles/{role}`. Hierarchy, managed, and Administrator are refused first.
    pub(super) fn edit_role(&self, guild: &str, role_id: &str, edit: &protocol::RoleEdit) -> Result<protocol::SpaceRole, Refusal> {
        let (_, guild) = discord_id(guild, "That server is not a Discord id.")?;
        let (role_num, role_id) = discord_id(role_id, "That role is not a Discord id.")?;
        let gate = self.role_gate(&guild)?;
        if !gate.manage {
            return Err(refused("You need the Manage Roles permission to edit roles."));
        }
        let held = {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(&guild).ok_or_else(|| refused("This server is gone."))?;
            let role = info.roles.get(&role_num).ok_or_else(|| refused("That role is not on this server."))?;
            (role.name.clone(), role.permissions, role.managed, role.position)
        };
        let (name, before, managed, position) = held;
        if managed {
            return Err(refused("Managed roles cannot be edited."));
        }
        let (editable, _) = role_rights(gate.owner, gate.highest, gate.guild_num, role_num, position, false);
        if !editable {
            return Err(refused("That role is at or above your highest role, so you cannot edit it."));
        }
        let everyone = role_num == gate.guild_num;
        if let Some(asked) = edit.name.as_deref() {
            let asked = role_name(asked)?;
            if everyone && asked != name {
                return Err(refused("The everyone role cannot be renamed."));
            }
        }
        let after = edit.permissions.unwrap_or(before);
        if !may_grant(gate.actor, before, after) {
            return Err(refused("You can only grant permissions you have."));
        }
        confirm_admin(before, after, edit.confirm_admin)?;
        let body = role_body(edit, !everyone)?;
        if body.is_empty() {
            return self.role_view(&guild, role_num);
        }
        let raw = self
            .rest
            .call_once_with("PATCH", &format!("/guilds/{guild}/roles/{role_id}"), Some(serde_json::Value::Object(body)), &[])
            .map_err(refusal)?;
        self.apply_role(&guild, &raw)
    }

    /// `PATCH /guilds/{id}/roles` with the new order. An empty list changes nothing.
    pub(super) fn move_roles(&self, guild: &str, roles: &[protocol::RoleMove]) -> Result<(), Refusal> {
        let (_, guild) = discord_id(guild, "That server is not a Discord id.")?;
        let gate = self.role_gate(&guild)?;
        if !gate.manage {
            return Err(refused("You need the Manage Roles permission to edit roles."));
        }
        if roles.is_empty() {
            return Ok(());
        }
        let mut planned = Vec::with_capacity(roles.len());
        for role in roles {
            let (id, id_text) = discord_id(&role.id, "That role is not a Discord id.")?;
            planned.push((id, id_text, role.position));
        }
        let payload = {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(&guild).ok_or_else(|| refused("This server is gone."))?;
            let mut payload = Vec::with_capacity(planned.len());
            for (id, id_text, position) in &planned {
                if *id == gate.guild_num {
                    return Err(refused("The everyone role cannot be moved."));
                }
                let Some(role) = info.roles.get(id) else {
                    return Err(refused("That role is not on this server."));
                };
                if role.managed {
                    return Err(refused("Managed roles cannot be moved."));
                }
                let (editable, _) = role_rights(gate.owner, gate.highest, gate.guild_num, *id, role.position, false);
                if !editable {
                    return Err(refused("That role is at or above your highest role, so you cannot move it."));
                }
                if *position <= 0 {
                    return Err(refused("A role cannot move to the everyone position."));
                }
                if !gate.owner && *position >= gate.highest {
                    return Err(refused("That position is at or above your highest role."));
                }
                payload.push(json!({"id": id_text, "position": position}));
            }
            payload
        };
        let raw = self
            .rest
            .call_once_with("PATCH", &format!("/guilds/{guild}/roles"), Some(json!(payload)), &[])
            .map_err(refusal)?;
        if let Ok(updated) = serde_json::from_str::<Vec<model::Role>>(&raw) {
            for role in updated {
                let _ = self.role_changed(&guild, Some(role), None);
            }
            return Ok(());
        }
        let mut state = crate::hub::lock(&self.state);
        if let Some(info) = state.guilds.get_mut(&guild) {
            for (id, _, position) in &planned {
                if let Some(role) = info.roles.get_mut(id) {
                    role.position = *position;
                }
            }
        }
        Ok(())
    }

    /// `DELETE /guilds/{id}/roles/{role}`.
    pub(super) fn delete_role(&self, guild: &str, role_id: &str) -> Result<(), Refusal> {
        let (_, guild) = discord_id(guild, "That server is not a Discord id.")?;
        let (role_num, role_id) = discord_id(role_id, "That role is not a Discord id.")?;
        let gate = self.role_gate(&guild)?;
        if !gate.manage {
            return Err(refused("You need the Manage Roles permission to edit roles."));
        }
        if role_num == gate.guild_num {
            return Err(refused("The everyone role cannot be deleted."));
        }
        let (managed, position) = {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(&guild).ok_or_else(|| refused("This server is gone."))?;
            let role = info.roles.get(&role_num).ok_or_else(|| refused("That role is not on this server."))?;
            (role.managed, role.position)
        };
        if managed {
            return Err(refused("Managed roles cannot be deleted."));
        }
        let (editable, _) = role_rights(gate.owner, gate.highest, gate.guild_num, role_num, position, false);
        if !editable {
            return Err(refused("That role is at or above your highest role, so you cannot delete it."));
        }
        self.rest.call_once_with("DELETE", &format!("/guilds/{guild}/roles/{role_id}"), None, &[]).map_err(refusal)?;
        let _ = self.role_changed(&guild, None, Some(&role_id));
        Ok(())
    }

    /// `PUT` and `DELETE /guilds/{id}/members/{user}/roles/{role}` for the difference from the
    /// member Discord returns. Refusals happen before any of those writes.
    pub(super) fn set_member_roles(&self, guild: &str, person: &str, desired: &[String]) -> Result<(), Refusal> {
        let (_, guild) = discord_id(guild, "That server is not a Discord id.")?;
        let (_, person) = discord_id(person, "That member is not a Discord id.")?;
        let gate = self.role_gate(&guild)?;
        if !gate.manage {
            return Err(refused("You need the Manage Roles permission to change a member's roles."));
        }
        let mut wanted = Vec::new();
        for raw in desired {
            let (id, _) = discord_id(raw, "That role is not a Discord id.")?;
            if id == gate.guild_num {
                return Err(refused("The everyone role is assigned to every member."));
            }
            if !wanted.contains(&id) {
                wanted.push(id);
            }
        }
        let flags: Vec<(u64, bool)> = {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(&guild).ok_or_else(|| refused("This server is gone."))?;
            info.roles
                .iter()
                .map(|(id, role)| {
                    let (_, assignable) = role_rights(gate.owner, gate.highest, gate.guild_num, *id, role.position, role.managed);
                    (*id, assignable)
                })
                .collect()
        };
        for id in &wanted {
            if !flags.iter().any(|(known, _)| known == id) {
                return Err(refused("That role is not on this server."));
            }
        }
        let raw = self
            .rest
            .call_once_with("GET", &format!("/guilds/{guild}/members/{person}"), None, &[])
            .map_err(refusal)?;
        let member: model::Member = serde_json::from_str(&raw).map_err(refused)?;
        let mut current = Vec::new();
        for id in &member.roles {
            match snowflake(id) {
                Some(n) if n == gate.guild_num => {}
                Some(n) => {
                    if !current.contains(&n) {
                        current.push(n);
                    }
                }
                None => return Err(refused("That role is above yours or managed, so it stays.")),
            }
        }
        let assignable = |id: &u64| flags.iter().find(|(known, _)| known == id).map(|(_, ok)| *ok).unwrap_or(false);
        for id in &current {
            if !wanted.contains(id) && !assignable(id) {
                return Err(refused("That role is above yours or managed, so it stays."));
            }
        }
        for id in &wanted {
            if !current.contains(id) && !assignable(id) {
                return Err(refused("That role cannot be assigned."));
            }
        }
        let mut final_ids: Vec<String> = current.iter().map(u64::to_string).collect();
        for id in &current {
            if wanted.contains(id) {
                continue;
            }
            let id_s = id.to_string();
            self.rest
                .call_once_with("DELETE", &format!("/guilds/{guild}/members/{person}/roles/{id_s}"), None, &[])
                .map_err(refusal)?;
            final_ids.retain(|kept| kept != &id_s);
        }
        for id in &wanted {
            if current.contains(id) {
                continue;
            }
            let id_s = id.to_string();
            self.rest
                .call_once_with("PUT", &format!("/guilds/{guild}/members/{person}/roles/{id_s}"), None, &[])
                .map_err(refusal)?;
            final_ids.push(id_s);
        }
        if person == self.me() {
            let member = model::Member { user_id: Some(self.me()), roles: final_ids, ..model::Member::default() };
            let _ = self.member_changed(&guild, &member);
        }
        Ok(())
    }

    /// The pinned messages, newest pin first, from the route the web client uses now; the older
    /// one answers where that one is not known. Both are the community's documentation,
    /// unconfirmed against a real account.
    pub(super) fn pinned(&self, channel: &str) -> Result<Vec<Message>, Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        let posted: Vec<Posted> = match self.call("GET", &format!("/channels/{channel}/messages/pins?limit=50"), None) {
            Ok(answer) => serde_json::from_str::<Pins>(&answer).map_err(refused)?.items.into_iter().map(|pin| pin.message).collect(),
            Err(super::Refused::Status { status: 404, .. }) => {
                let answer = self.call("GET", &format!("/channels/{channel}/pins"), None).map_err(refusal)?;
                serde_json::from_str(&answer).map_err(refused)?
            }
            Err(other) => return Err(refusal(other)),
        };
        let guild = self.guild_of(&known);
        let mut messages = Vec::with_capacity(posted.len());
        for posted in posted.iter().take(protocol::PINNED_LIMIT) {
            let (mut message, files) = self.convert(posted, guild.as_deref(), known.kind);
            files.register(&self.ctx).map_err(refused)?;
            message.pinned = true;
            messages.push(message);
        }
        Ok(messages)
    }

    /// What was shared in a conversation, from Discord's own search, because the user asked:
    /// the server's search narrowed to the channel, or the DM's own. `before` is a message id;
    /// older hits follow it. The total is Discord's count of messages, given on the first page.
    pub(super) fn search_shared(&self, channel: &str, kind: SharedKind, before: Option<&str>, limit: u32) -> Result<SharedPage, Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        let guild = self.guild_of(&known);
        let has = match kind {
            SharedKind::Media => "has=image&has=video",
            SharedKind::Files => "has=file",
            SharedKind::Links => "has=link",
        };
        let wanted = limit.clamp(1, SEARCH_PAGE);
        let mut path = match &guild {
            Some(guild) => format!("/guilds/{guild}/messages/search?channel_id={channel}&{has}&limit={wanted}"),
            None => format!("/channels/{channel}/messages/search?{has}&limit={wanted}"),
        };
        // Discord's search takes the newest id it may answer; one below the item listed last.
        if let Some(before) = before.map(number).filter(|before| *before > 0) {
            path.push_str(&format!("&max_id={}", before - 1));
        }
        let answer = self.call("GET", &path, None).map_err(refusal)?;
        let found: Found = serde_json::from_str(&answer).map_err(refused)?;
        // A 202 with no hits: the index is still being built.
        let Some(hits) = found.messages.filter(|_| found.code.is_none()) else {
            return Err(refused("Discord is still indexing this conversation. Try again in a moment."));
        };
        let mut items = Vec::new();
        for posted in hits.iter().filter_map(|hit| hit.first()) {
            let (message, files) = self.convert(posted, guild.as_deref(), known.kind);
            files.register(&self.ctx).map_err(refused)?;
            items.extend(shared_items(&message, kind));
        }
        Ok(SharedPage {
            more: hits.len() as u32 >= wanted,
            total: found.total_results.filter(|_| before.is_none()),
            items,
        })
    }

    /// Runs a message search off the command thread, one at a time per account.
    pub(super) fn search_queued(
        discord: &Arc<Discord>,
        query: String,
        scope: SearchScope,
        at: Instant,
        reply: Reply<Vec<MessageHit>>,
    ) {
        let searching = discord.clone();
        let run = move || {
            let _ = reply.send(searching.search_messages(&query, &scope, at));
        };
        if let Err(err) = std::thread::Builder::new().name("discord-search".into()).spawn(run) {
            tracing::warn!("chatd: Discord: no thread for a search: {err}");
        }
    }

    /// What the network itself finds for the pane's words, from Discord's own search: the
    /// server's when a Space is the scope, the channel's when one conversation is. The hits
    /// are mapped like the cache's, so the pane can merge the two lists and tell them apart.
    pub(super) fn search_messages(&self, query: &str, scope: &SearchScope, at: Instant) -> Result<Vec<MessageHit>, Refusal> {
        if at.elapsed() >= super::SEARCH_PATIENCE {
            return Err(refused("That took too long. Try again."));
        }
        let capped: String = query.chars().take(SEARCH_MAX_CHARS).collect();
        let query = capped.trim();
        let key = query.to_lowercase();
        let path = match scope {
            SearchScope::Space(space) => {
                // A Space's id is a guild's: one the account is in, as space_info checks.
                if !crate::hub::lock(&self.state).guilds.contains_key(space) {
                    return Err(refused("This server is gone."));
                }
                format!("/guilds/{space}/messages/search?content={}", escaped(query))
            }
            SearchScope::Account => return Err(Refusal::Unsupported),
            SearchScope::Conversation(channel) => {
                let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
                match self.guild_of(&known) {
                    Some(guild) => {
                        format!("/guilds/{guild}/messages/search?channel_id={channel}&content={}", escaped(query))
                    }
                    None => format!("/channels/{channel}/messages/search?content={}", escaped(query)),
                }
            }
        };
        // One search in flight per account, and at least SEARCH_GAP after the previous finished:
        // no client turns Enter into a burst against Discord's tight search limits.
        {
            let mut search = crate::hub::lock(&self.message_search);
            if let Some((_, _, hits)) = search.remembered.iter().find(|(s, q, _)| s == scope && *q == key) {
                return Ok(hits.clone());
            }
            if search.in_flight || search.last.is_some_and(|last| last.elapsed() < super::members::SEARCH_GAP) {
                return Err(refused("Discord search is paced. Try again in a moment."));
            }
            search.in_flight = true;
        }
        let outcome = self.run_message_search(&path, query, scope);
        let mut search = crate::hub::lock(&self.message_search);
        search.in_flight = false;
        search.last = Some(Instant::now());
        if let Ok(hits) = &outcome {
            if search.remembered.len() >= REMEMBERED_SEARCHES {
                search.remembered.pop_front();
            }
            search.remembered.push_back((scope.clone(), key, hits.clone()));
        }
        outcome
    }

    fn run_message_search(&self, path: &str, query: &str, scope: &SearchScope) -> Result<Vec<MessageHit>, Refusal> {
        let answer = self.call("GET", path, None).map_err(refusal)?;
        let found: Found = serde_json::from_str(&answer).map_err(refused)?;
        // A 202 with no hits: the index is still being built.
        let Some(hits) = found.messages.filter(|_| found.code.is_none()) else {
            return Err(refused("Discord search isn't ready for this server yet. Try again in a moment."));
        };
        let space_id = match scope {
            SearchScope::Space(space) => Some(space.clone()),
            SearchScope::Conversation(channel) => {
                self.ctx.known(channel).ok().flatten().and_then(|known| self.guild_of(&known))
            }
            SearchScope::Account => None,
        };
        let mut found_hits = Vec::new();
        for posted in hits.iter().filter_map(|hit| hit.first()) {
            // Hidden conversations and message requests stay out of it, as they stay out of
            // the cache's own search.
            if !self.ctx.listable(&posted.channel_id) {
                continue;
            }
            let known = self.ctx.known(&posted.channel_id).ok().flatten();
            let age_restricted = known.as_ref().is_some_and(|row| row.age_restricted);
            let kind = known.as_ref().map_or(ConversationKind::Channel, |row| row.kind);
            let title = known.as_ref().map_or_else(String::new, |row| row.title.clone());
            let space_title = known.as_ref().map_or_else(String::new, |row| row.space_title.clone());
            let (message, _) = self.convert(posted, space_id.as_deref(), kind);
            let text = if !message.text.is_empty() {
                message.text
            } else if let Some(first) = message.attachments.first() {
                if first.name.is_empty() { "Attachment".to_owned() } else { first.name.clone() }
            } else {
                posted.content.clone().unwrap_or_default()
            };
            found_hits.push(MessageHit {
                conversation: self.ctx.key(&posted.channel_id),
                message_id: posted.id.clone(),
                sent: model::seconds(&posted.id),
                sender: message.sender.name,
                snippet: crate::store::snippet(&text, query),
                title,
                space_title,
                outgoing: message.outgoing,
                age_restricted,
            });
        }
        Ok(found_hits)
    }

    /// Kicks, bans, times out, untimes out, or renames a guild member, gated by
    /// computed guild permissions and role hierarchy.
    pub(super) fn moderate(
        &self,
        guild: &str,
        target: &str,
        action: protocol::ModerateAction,
        reason: &str,
    ) -> Result<(), Refusal> {
        let me = self.me();
        {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(guild).ok_or_else(|| refused("This server is gone."))?;
            info.can_moderate(guild, &me, target, &action).map_err(refused)?;
        }
        let (method, path, body) = moderate_request(guild, target, &action);
        if reason.trim().is_empty() {
            self.call(method, &path, body).map_err(refusal)?;
        } else {
            let encoded = encode_audit_reason(reason.trim());
            self.rest
                .call_with(method, &path, body, &[("X-Audit-Log-Reason", &encoded)])
                .map_err(refusal)?;
        }
        Ok(())
    }

    /// Lifts a ban on `target` in `guild`, gated by `BAN_MEMBERS`.
    pub(super) fn unban(&self, guild: &str, target: &str, reason: &str) -> Result<(), Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(guild).ok_or_else(|| refused("This server is gone."))?;
            if info.guild_permissions(guild) & model::BAN_MEMBERS == 0 {
                return Err(refused("You don't have permission to unban members."));
            }
        }
        let path = format!("/guilds/{guild}/bans/{target}");
        if reason.trim().is_empty() {
            self.call("DELETE", &path, None).map_err(refusal)?;
        } else {
            let encoded = encode_audit_reason(reason.trim());
            self.rest
                .call_with("DELETE", &path, None, &[("X-Audit-Log-Reason", &encoded)])
                .map_err(refusal)?;
        }
        Ok(())
    }
}

/// Builds the `PATCH /users/@me/guilds/{guild}/settings` body for a timed or endless channel mute.
pub(super) fn mute_override_body(
    channel: &str,
    notify: Notify,
    until: Option<i64>,
    now_secs: i64,
) -> serde_json::Value {
    let mut setting = match notify {
        Notify::All => json!({"muted": false, "message_notifications": 0}),
        Notify::Mentions => json!({"muted": false, "message_notifications": 1}),
        Notify::Nothing => json!({"muted": true}),
    };
    setting["mute_config"] = match (notify, until) {
        (Notify::Nothing, Some(end)) if end > now_secs => {
            let window = (end - now_secs).max(1);
            json!({"end_time": model::iso_timestamp(end), "selected_time_window": window})
        }
        _ => json!({"end_time": null, "selected_time_window": -1}),
    };
    json!({"channel_overrides": {channel: setting}})
}

/// Builds the `(method, path, body)` for a Discord member moderation REST request.
pub(super) fn moderate_request(
    guild: &str,
    target: &str,
    action: &protocol::ModerateAction,
) -> (&'static str, String, Option<serde_json::Value>) {
    match action {
        protocol::ModerateAction::Kick => ("DELETE", format!("/guilds/{guild}/members/{target}"), None),
        protocol::ModerateAction::Ban { delete_seconds } => (
            "PUT",
            format!("/guilds/{guild}/bans/{target}"),
            Some(json!({"delete_message_seconds": (*delete_seconds).min(604_800)})),
        ),
        protocol::ModerateAction::Timeout { until } => (
            "PATCH",
            format!("/guilds/{guild}/members/{target}"),
            Some(json!({"communication_disabled_until": model::iso_timestamp(*until)})),
        ),
        protocol::ModerateAction::Untimeout => (
            "PATCH",
            format!("/guilds/{guild}/members/{target}"),
            Some(json!({"communication_disabled_until": serde_json::Value::Null})),
        ),
        protocol::ModerateAction::Nick { nick } => (
            "PATCH",
            format!("/guilds/{guild}/members/{target}"),
            Some(json!({"nick": if nick.is_empty() { serde_json::Value::Null } else { json!(nick) }})),
        ),
    }
}

/// Percent-encodes `X-Audit-Log-Reason` as Discord expects.
pub(super) fn encode_audit_reason(reason: &str) -> String {
    let mut out = String::with_capacity(reason.len());
    for byte in reason.bytes().take(512) {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod moderation_tests {
    use super::*;
    use crate::adapters::discord::{GuildInfo, RoleInfo};
    use protocol::ModerateAction;

    #[test]
    fn moderate_request_builds_expected_routes_and_bodies() {
        let (m, p, b) = moderate_request("4000", "2001", &ModerateAction::Kick);
        assert_eq!((m, p.as_str(), b), ("DELETE", "/guilds/4000/members/2001", None));

        let (m, p, b) = moderate_request("4000", "2001", &ModerateAction::Ban { delete_seconds: 86_400 });
        assert_eq!(m, "PUT");
        assert_eq!(p, "/guilds/4000/bans/2001");
        assert_eq!(b.unwrap()["delete_message_seconds"], 86_400);

        let (m, p, b) = moderate_request("4000", "2001", &ModerateAction::Timeout { until: 1_700_000_000 });
        assert_eq!(m, "PATCH");
        assert_eq!(p, "/guilds/4000/members/2001");
        assert_eq!(b.unwrap()["communication_disabled_until"], "2023-11-14T22:13:20Z");

        let (m, p, b) = moderate_request("4000", "2001", &ModerateAction::Untimeout);
        assert_eq!(m, "PATCH");
        assert_eq!(p, "/guilds/4000/members/2001");
        assert!(b.unwrap()["communication_disabled_until"].is_null());

        let (m, p, b) = moderate_request("4000", "2001", &ModerateAction::Nick { nick: "NewName".into() });
        assert_eq!(m, "PATCH");
        assert_eq!(p, "/guilds/4000/members/2001");
        assert_eq!(b.unwrap()["nick"], "NewName");

        assert_eq!(encode_audit_reason("Spam link!"), "Spam%20link%21");
    }

    #[test]
    fn can_moderate_enforces_permissions_owner_and_role_hierarchy() {
        let mut guild = GuildInfo::default();
        guild.owner_id = "999".into();
        guild.roles.insert(
            10,
            RoleInfo {
                name: "Mod".into(),
                permissions: model::KICK_MEMBERS | model::MODERATE_MEMBERS | model::MANAGE_NICKNAMES,
                position: 5,
                ..RoleInfo::default()
            },
        );
        guild.roles.insert(
            20,
            RoleInfo {
                name: "Admin".into(),
                permissions: model::BAN_MEMBERS,
                position: 10,
                ..RoleInfo::default()
            },
        );
        guild.roles.insert(
            5,
            RoleInfo {
                name: "Member".into(),
                permissions: 0,
                position: 1,
                ..RoleInfo::default()
            },
        );
        guild.my_roles = vec![10];
        guild.set_member_roles("low_user", vec![5]);
        guild.set_member_roles("high_user", vec![20]);

        // Allowed: kick/timeout/nick on lower role
        assert!(guild.can_moderate("4000", "me", "low_user", &ModerateAction::Kick).is_ok());
        assert!(guild.can_moderate("4000", "me", "low_user", &ModerateAction::Timeout { until: 100 }).is_ok());
        assert!(guild.can_moderate("4000", "me", "low_user", &ModerateAction::Nick { nick: "Bob".into() }).is_ok());
        // Denied: missing BAN_MEMBERS
        assert!(guild.can_moderate("4000", "me", "low_user", &ModerateAction::Ban { delete_seconds: 0 }).is_err());
        // Denied: higher role target
        assert!(guild.can_moderate("4000", "me", "high_user", &ModerateAction::Kick).is_err());
        // Denied: guild owner
        assert!(guild.can_moderate("4000", "me", "999", &ModerateAction::Kick).is_err());
        // Denied: self
        assert!(guild.can_moderate("4000", "me", "me", &ModerateAction::Kick).is_err());
    }

    #[test]
    fn timed_mute_builds_mute_config_and_expires_on_read() {
        let now_secs = 1_700_000_000_i64;
        let one_hour = now_secs + 3_600;
        let body = mute_override_body("4101", Notify::Nothing, Some(one_hour), now_secs);
        let entry = &body["channel_overrides"]["4101"];
        assert_eq!(entry["muted"], true);
        assert_eq!(entry["mute_config"]["selected_time_window"], 3_600);
        assert_eq!(entry["mute_config"]["end_time"], "2023-11-14T23:13:20Z");

        let endless = mute_override_body("4101", Notify::Nothing, None, now_secs);
        assert_eq!(endless["channel_overrides"]["4101"]["mute_config"]["selected_time_window"], -1);
        assert!(endless["channel_overrides"]["4101"]["mute_config"]["end_time"].is_null());

        let unmute = mute_override_body("4101", Notify::All, Some(one_hour), now_secs);
        assert_eq!(unmute["channel_overrides"]["4101"]["muted"], false);
        assert_eq!(unmute["channel_overrides"]["4101"]["mute_config"]["selected_time_window"], -1);

        // Reading back a timed mute from GuildSettings unmutes once `end_time` passes.
        let settings: GuildSettings = serde_json::from_value(json!({
            "guild_id": "4000",
            "channel_overrides": [{
                "channel_id": "4101",
                "muted": true,
                "mute_config": {
                    "end_time": "2023-11-14T23:13:20Z",
                    "selected_time_window": 3600
                }
            }]
        }))
        .unwrap();
        let parsed = super::super::ready::quiet(&settings);
        assert!(parsed.mutes("4101", None, one_hour - 1), "still muted before end_time");
        assert!(!parsed.mutes("4101", None, one_hour), "unmuted at end_time");
        assert!(!parsed.mutes("4101", None, one_hour + 60), "unmuted after end_time");
    }
}
