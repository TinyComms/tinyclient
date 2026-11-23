//! Discord, signed in as the user: the gateway websocket for everything live, the HTTP API for
//! history and for what the user does.
//!
//! The socket thread (`gateway.rs`) holds the gateway, turns its events into
//! `Context` calls and is the only one that writes to the socket; the command thread serves
//! HTTP commands and hands gateway sends to the socket thread's outbox. A separate
//! viewport thread serves cache reads and Watch updates without waiting for HTTP/backfill.
//!
//! What stays in memory is bounded and listed on [`State`]: roles and ids per server, the lists
//! the user owns, one visible member window, the voice states of the servers. Trees, people and
//! messages go to the store as READY and the events arrive.

mod account;
mod audit;
mod automod;
mod admin;
mod content;
mod events;
mod expressions;

pub(super) fn prove_code_colors() -> Result<(), String> {
    protocol::highlight::prove_code_colors()
}
/// Its socket waits are shared with the Tiny server's live socket.
pub(super) mod gateway;
mod info;
mod manage;
mod commands;
mod interactions;
mod members;
mod model;
mod polls;
mod picker;
mod proto;
mod ready;
mod rest;
mod send;
mod voice;
mod voice_ux;

use super::{serve_commands, keep_connected, Command, Context, Download, Failure, Refusal, Reply, Signed};
use crate::blobs::upload;
use crate::store::ConversationRow;
use base64::Engine;
use content::{Files, Names, Place};
use model::{number, seconds, Posted, User};
use protocol::{Conversation, ConversationKind, Key, Message, MessageKind, Presence, RelateAction, Relation, SendState, ThreadSummary};
use rest::{Refused, Rest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The pause after a history fetch.
const PACE: Duration = Duration::from_millis(700);
/// A search asked and not answered within this does not reach Discord at all: the shell has
/// already been told the network did not answer.
const SEARCH_PATIENCE: Duration = Duration::from_secs(25);
/// Unread conversations fetched ahead of being opened, once per run.
const WARM: usize = 5;
/// A page of history, as the web client asks for it.
const PAGE: usize = 50;
/// Discord's own limit on a message. Nitro (`premium_type` 2) is twice this.
const LONGEST: usize = 2000;
/// Discord's limit on a group DM, including ourselves (at most 9 friends).
const GROUP_DM_LIMIT: usize = 10;

/// Characters one message may hold. Nitro is 4,000. Every other plan, including Nitro Basic,
/// stays at 2,000. A server's boosts are not part of this.
pub(super) fn message_limit(premium: u8) -> usize {
    if premium == 2 { 2 * LONGEST } else { LONGEST }
}

/// What a Nitro account may send. Lower plans are the same constructor with a smaller file cap.
pub(crate) fn capability_ceiling() -> protocol::Capabilities {
    Discord::account_capabilities(2)
}

/// A direct or group DM is called here. Discord's own system account is not.
pub(crate) fn private_call(system: bool) -> protocol::CallSupport {
    if system { protocol::CallSupport::None } else { protocol::CallSupport::Native }
}

/// A voice channel is joined here.
pub(crate) fn voice_channel_call() -> protocol::CallSupport {
    voice::support(model::VOICE)
}

/// A stage is joined here: listen, raise a hand, and speak when invited.
pub(crate) fn stage_channel_call() -> protocol::CallSupport {
    voice::support(model::STAGE)
}

fn too_long(premium: u8, text: &str) -> Option<String> {
    let limit = message_limit(premium);
    (text.chars().count() > limit).then(|| format!("Message too long: Discord allows {limit} characters."))
}

/// The lab's discord-voice-bitrate driver. A 32 kb/s channel is sent at 32, and the other
/// bounds stay put. A build from before the fix ignores the variable and stays up.
pub(super) fn prove_voice_bitrate() -> Result<(), String> {
    use protocol::VoiceQuality::*;
    if voice::voice_kbps(32_000, false, Standard) != 32 {
        return Err("a 32 kb/s channel is sent at 32".to_owned());
    }
    if voice::voice_kbps(8_000, false, Standard) != 8 {
        return Err("8 kb/s stays 8".to_owned());
    }
    if voice::voice_kbps(384_000, false, High) != 384 {
        return Err("a boosted channel stays at 384".to_owned());
    }
    if voice::voice_kbps(96_000, false, DataSaver) != 32 {
        return Err("data saver caps the channel".to_owned());
    }
    if voice::voice_kbps(8_000, false, DataSaver) != 8 {
        return Err("data saver does not lift a quiet channel".to_owned());
    }
    if voice::voice_kbps(0, true, Standard) != 64 {
        return Err("a direct call stays at 64".to_owned());
    }
    if voice::voice_kbps(512_000, false, Standard) != 510 {
        return Err("libopus stereo caps at 510".to_owned());
    }
    Ok(())
}

/// The lab's discord-voice-move driver. A same-guild move is followed. A direct call, another
/// guild, and the channel we are already in are not. A build from before the fix ignores the
/// variable and stays up.
pub(super) fn prove_voice_move() -> Result<(), String> {
    use voice::Follow::*;
    match voice::follow(Some("4000"), Some("4203"), Some("4000"), "4201") {
        Moved(channel) if channel == "4203" => {}
        other => return Err(format!("a same-guild move is followed, got {other:?}")),
    }
    if voice::follow(Some("4000"), Some("4201"), Some("4000"), "4201") != Here {
        return Err("the same channel stays".to_owned());
    }
    if voice::follow(Some("9"), Some("1"), Some("4000"), "4201") != Elsewhere {
        return Err("another guild is not this call".to_owned());
    }
    if voice::follow(None, Some("9"), None, "3001") != Elsewhere {
        return Err("a direct call is not followed".to_owned());
    }
    if voice::follow(Some("4000"), None, Some("4000"), "4201") != Left {
        return Err("a null channel is a leave".to_owned());
    }
    Ok(())
}

/// The lab's discord-wave17b driver. A stage is joined here, an activity names its application,
/// soundboard gain is the setting times the sound, and raising a hand stays suppressed.
pub(super) fn prove_wave17b() -> Result<(), String> {
    if stage_channel_call() != protocol::CallSupport::Native {
        return Err("a stage is joined here".to_owned());
    }
    let activity = voice_ux::parse_activity(
        r#"{"channel_id":"4201","guild_id":"4000","embedded_activity":{"application_id":"88","users":[{"id":"2001"}]}}"#,
    )
    .ok_or_else(|| "an activity names its application".to_owned())?;
    if activity.application_id.as_deref() != Some("88") {
        return Err("an activity names its application".to_owned());
    }
    if voice_ux::effect_gain(0, 1.0) != 0 || voice_ux::effect_gain(100, 0.5) != 50 {
        return Err("soundboard gain is the setting times the sound".to_owned());
    }
    let body = voice_ux::stage_body("4202", protocol::StageVoiceAction::RequestToSpeak, "2026-11-22T00:00:00Z");
    if body.get("suppress").and_then(|value| value.as_bool()) != Some(true) {
        return Err("raising a hand stays suppressed".to_owned());
    }
    let sounds = voice_ux::parse_soundboard(r#"{"guild_id":"4000","soundboard_sounds":[{"sound_id":"q","name":"Quack"}]}"#);
    if sounds.first().map(|sound| sound.name.as_str()) != Some("Quack") {
        return Err("a guild soundboard lists the named sound".to_owned());
    }
    Ok(())
}

/// The lab's discord-nitro-length driver. `send`, `edit` and `send_with` all ask [`too_long`].
/// A build from before the fix ignores the variable and stays up.
pub(super) fn prove_nitro_length() -> Result<(), String> {
    if message_limit(2) != 4000 || [0, 1, 3].iter().any(|plan| message_limit(*plan) != 2000) {
        return Err("nitro is 4000 characters and every other plan stays at 2000".to_owned());
    }
    let three = "n".repeat(3000);
    if too_long(2, &three).is_some() {
        return Err("nitro accepts 3000 characters".to_owned());
    }
    match too_long(0, &three) {
        Some(why) if why.contains("2000") => {}
        other => return Err(format!("a free account refuses 3000 locally, got {other:?}")),
    }
    match too_long(2, &"n".repeat(4001)) {
        Some(why) if why.contains("4000") => Ok(()),
        other => Err(format!("nitro refuses 4001, got {other:?}")),
    }
}

/// At most one typing signal per conversation in this long.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// Most addresses Discord renews in one request.
const RENEWED_TOGETHER: usize = 50;
/// Conversations whose fetched stretches of history are remembered, and stretches of each.
const COVERED: usize = 64;
const STRETCHES: usize = 8;

#[derive(Deserialize)]
struct Session {
    token: String,
}

/// A server's settings as the user made them in Discord. Times are unix seconds; `i64::MAX` is
/// "until I turn it back on".
struct Quiet {
    muted_until: Option<i64>,
    suppress_everyone: bool,
    suppress_roles: bool,
    /// Discord `notify_highlights`: 0 default, 1 disabled (suppressed), 2 enabled.
    notify_highlights: u8,
    mute_scheduled_events: bool,
    mobile_push: bool,
    channels: HashMap<String, i64>,
    /// "Hide muted channels", as the user set it for the server.
    hide_muted: bool,
    /// "Show all channels" is off: only what is opted in shows.
    opt_in: bool,
    /// Channels and categories opted in.
    opted_in: HashSet<String>,
    /// Raw guild settings `flags`, kept so toggling one bit preserves all others.
    raw_flags: u32,
    /// Raw per-channel override `flags`, kept so toggling one bit preserves all others.
    channel_flags: HashMap<String, u32>,
    /// The server's notification level (0 all, 1 mentions, 2 nothing), and the channels and
    /// categories that set their own.
    level: u8,
    levels: HashMap<String, u8>,
    /// Categories collapsed in Discord (`channel_overrides[].collapsed == true`).
    collapsed: HashSet<String>,
}

/// Coalesces rapid category collapse/expand clicks per guild so a burst is one PATCH.
const COLLAPSE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct CollapseQueue {
    pending: HashMap<String, (HashMap<String, bool>, Instant)>,
}

impl CollapseQueue {
    pub(super) fn push(&mut self, guild: &str, category: &str, collapsed: bool, now: Instant) {
        let entry = self.pending.entry(guild.to_owned()).or_insert_with(|| (HashMap::new(), now));
        entry.0.insert(category.to_owned(), collapsed);
        entry.1 = now;
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Vec<(String, HashMap<String, bool>)> {
        let due_guilds: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) >= COLLAPSE_DEBOUNCE)
            .map(|(guild, _)| guild.clone())
            .collect();
        due_guilds
            .into_iter()
            .filter_map(|guild| self.pending.remove(&guild).map(|(map, _)| (guild, map)))
            .collect()
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

pub(super) fn collapse_settings_body(overrides: &HashMap<String, bool>) -> Value {
    let mut map = serde_json::Map::new();
    for (category, collapsed) in overrides {
        map.insert(category.clone(), json!({"collapsed": collapsed}));
    }
    json!({"channel_overrides": map})
}

impl Default for Quiet {
    fn default() -> Quiet {
        Quiet {
            muted_until: None,
            suppress_everyone: false,
            suppress_roles: false,
            notify_highlights: 0,
            mute_scheduled_events: false,
            mobile_push: true,
            channels: HashMap::new(),
            hide_muted: false,
            opt_in: false,
            opted_in: HashSet::new(),
            level: 0,
            levels: HashMap::new(),
            raw_flags: 0,
            channel_flags: HashMap::new(),
            collapsed: HashSet::new(),
        }
    }
}

/// One sticker from a standard pack (`GET /sticker-packs`).
struct PackedSticker {
    id: String,
    name: String,
    format: u8,
    pack: String,
}

/// One of a server's custom emoji or stickers, kept for the picker: about 40 bytes each.
struct Pickable {
    id: String,
    name: String,
    animated: bool,
    /// A sticker's format; 0 for an emoji.
    format: u8,
    /// The roles an emoji is limited to; empty for everyone.
    roles: Box<[u64]>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct RoleInfo {
    pub name: String,
    pub permissions: u64,
    pub position: i32,
    pub color: u32,
    pub hoist: bool,
    pub mentionable: bool,
    pub managed: bool,
    /// Discord's icon hash, never a URL.
    pub icon: Option<String>,
    pub unicode_emoji: Option<String>,
}

impl RoleInfo {
    fn remember(role: &model::Role) -> Self {
        Self {
            name: role.name.clone(),
            permissions: role.permissions,
            color: role.color & 0x00FF_FFFF,
            hoist: role.hoist,
            position: role.position,
            mentionable: role.mentionable,
            managed: role.managed,
            icon: model::role_icon_hash(role.icon.as_deref()),
            unicode_emoji: model::role_emoji(role.unicode_emoji.as_deref()),
        }
    }
}

#[derive(Clone)]
pub(super) struct StoredEvent {
    pub event: model::ScheduledEvent,
    pub interested: bool,
}

/// Whether a live transition of a scheduled event should post a local notification.
pub(super) fn should_notify_scheduled_event(
    was_status: u8,
    now_status: u8,
    interested: bool,
    mute_scheduled_events: bool,
    server_muted: bool,
) -> bool {
    was_status != 2 && now_status == 2 && interested && !mute_scheduled_events && !server_muted
}

/// What is kept of a server once its tree is in the store.
#[derive(Default)]
struct GuildInfo {
    name: String,
    icon: Option<String>,
    owner: bool,
    owner_id: String,
    /// Where it sits in the user's own order.
    position: i32,
    /// Unix seconds. A channel never read is unread only for what came after this.
    joined: i64,
    roles: HashMap<u64, RoleInfo>,
    my_roles: Vec<u64>,
    /// Cached member role IDs (user_id -> role IDs) for role colors/icons & moderation hierarchy.
    member_roles: HashMap<String, Vec<u64>>,
    /// Category id to its position and name, to place a channel created later and to write
    /// its header when the first channel we may see appears under it.
    categories: HashMap<String, (i32, String)>,
    /// Ids only (channel, parent), to apply a changed mute setting to every row. The rows
    /// themselves are in the store.
    channels: Vec<(u64, u64)>,
    quiet: Quiet,
    /// Our own nickname there.
    nick: String,
    /// Server boost tier (0..=3), which can raise the file upload limit for everyone in it.
    premium_tier: u8,
    /// Number of active server boosts.
    premium_subscription_count: u32,
    /// Banner hash from Discord (`banners/{guild}/{hash}.webp`).
    banner: Option<String>,
    /// Server description shown in Details.
    description: String,
    /// Discord feature flags (e.g. `VERIFIED`, `PARTNERED`, `INVITES_DISABLED`).
    features: Vec<String>,
    /// Server vanity invite code, if any.
    vanity_url_code: Option<String>,
    /// System messages channel id.
    system_channel_id: Option<String>,
    /// AFK voice channel id.
    afk_channel_id: Option<String>,
    /// AFK timeout in seconds.
    afk_timeout: u32,
    /// Default server notification level (0 all, 1 mentions).
    default_message_notifications: u8,
    /// Unix seconds until our own server timeout ends, if timed out.
    timeout_until: Option<i64>,
    /// Channels always shown by the server's onboarding (`GET /guilds/{id}/onboarding`).
    onboarding_defaults: HashSet<String>,
    /// Whether `GET /guilds/{id}/onboarding` has already been fetched in this session.
    onboarding_fetched: bool,
    /// Rules not accepted yet. Not a permission.
    pending: bool,
    /// Membership flag BYPASSES_VERIFICATION. It skips the verification lock only.
    bypass_verification: bool,
    /// Guild verification level when Discord sent one.
    verification: Option<u8>,
    /// The server has onboarding questions. That alone does not lock the composer.
    onboarding: bool,
    /// COMPLETED_ONBOARDING. Not a permission grant.
    onboarding_done: bool,
    /// A moderator still has to approve the join.
    manual: bool,
    /// MEMBER_VERIFICATION_GATE_ENABLED. The rules form is fetched when this or `pending` is set.
    rules: bool,
    /// Our verified email, copied off the session so a row can be built without locking state.
    verified: Option<bool>,
    /// Whether we have a phone, copied the same way. The number is never stored.
    phone: Option<bool>,
    emojis: Vec<Pickable>,
    stickers: Vec<Pickable>,
    events: HashMap<String, StoredEvent>,
    /// True when at least one visible channel in this guild permits `CREATE_INSTANT_INVITE`.
    can_invite: bool,
}

impl GuildInfo {
    /// Stores a member's role IDs (bounded so huge servers never grow unbounded).
    pub(super) fn set_member_roles(&mut self, user_id: &str, roles: Vec<u64>) {
        if self.member_roles.len() >= 2048 && !self.member_roles.contains_key(user_id) {
            self.member_roles.clear();
        }
        self.member_roles.insert(user_id.to_owned(), roles);
    }

    /// Resolves the top colored role (highest `(position, id)` with non-zero `color`) and
    /// top role icon/emoji (highest `(position, id)` with `icon` or non-empty `unicode_emoji`).
    pub(super) fn resolve_role_style(&self, role_ids: &[u64]) -> (Option<u32>, Option<(protocol::MediaFile, String)>, Option<String>) {
        let color = role_ids
            .iter()
            .filter_map(|id| self.roles.get(id).filter(|r| r.color != 0).map(|r| ((r.position, *id), r.color)))
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, color)| color);
        let top_badge = role_ids
            .iter()
            .filter_map(|id| {
                let r = self.roles.get(id)?;
                let has_icon = r.icon.as_deref().is_some_and(|h| !h.is_empty());
                let has_emoji = r.unicode_emoji.as_deref().is_some_and(|e| !e.is_empty());
                (has_icon || has_emoji).then_some(((r.position, *id), *id, r))
            })
            .max_by_key(|(rank, _, _)| *rank);
        let (icon, emoji) = match top_badge {
            Some((_, role_id, role)) => {
                if let Some(hash) = role.icon.as_deref().filter(|h| !h.is_empty()) {
                    (Some(model::role_icon(&role_id.to_string(), hash)), None)
                } else {
                    (None, role.unicode_emoji.clone().filter(|e| !e.is_empty()))
                }
            }
            None => (None, None),
        };
        (color, icon, emoji)
    }

    pub(super) fn member_role_style(&self, user_id: &str) -> (Option<u32>, Option<(protocol::MediaFile, String)>, Option<String>) {
        let Some(roles) = self.member_roles.get(user_id) else {
            return (None, None, None);
        };
        self.resolve_role_style(roles)
    }

    /// Computed guild-level permissions for our account (owner or ADMINISTRATOR => u64::MAX).
    pub(super) fn guild_permissions(&self, guild_id: &str) -> u64 {
        if self.owner {
            return u64::MAX;
        }
        let everyone = number(guild_id);
        let of = |role: &u64| self.roles.get(role).map_or(0, |r| r.permissions);
        let perms = self.my_roles.iter().fold(of(&everyone), |acc, r| acc | of(r));
        if perms & model::ADMINISTRATOR != 0 {
            u64::MAX
        } else {
            perms
        }
    }

    /// Highest role position among `role_ids`, or 0 when none.
    pub(super) fn highest_role_position(&self, role_ids: &[u64]) -> i32 {
        role_ids
            .iter()
            .filter_map(|id| self.roles.get(id).map(|r| r.position))
            .max()
            .unwrap_or(0)
    }

    /// Checks whether our account has both the required permission bit and role hierarchy
    /// to perform `action` on `target_id` in `guild_id`.
    pub(super) fn can_moderate(
        &self,
        guild_id: &str,
        me: &str,
        target_id: &str,
        action: &protocol::ModerateAction,
    ) -> Result<(), &'static str> {
        if target_id == me {
            return Err("You cannot moderate yourself.");
        }
        if !self.owner_id.is_empty() && target_id == self.owner_id {
            return Err("Cannot moderate the server owner.");
        }
        let perms = self.guild_permissions(guild_id);
        let (needed, missing_msg) = match action {
            protocol::ModerateAction::Kick => (model::KICK_MEMBERS, "You don't have permission to kick members."),
            protocol::ModerateAction::Ban { .. } => (model::BAN_MEMBERS, "You don't have permission to ban members."),
            protocol::ModerateAction::Timeout { .. } | protocol::ModerateAction::Untimeout => {
                (model::MODERATE_MEMBERS, "You don't have permission to time out members.")
            }
            protocol::ModerateAction::Nick { .. } => (model::MANAGE_NICKNAMES, "You don't have permission to change nicknames."),
        };
        if perms & needed == 0 {
            return Err(missing_msg);
        }
        if !self.owner {
            if let Some(target_roles) = self.member_roles.get(target_id) {
                let my_top = self.highest_role_position(&self.my_roles);
                let their_top = self.highest_role_position(target_roles);
                if my_top <= their_top {
                    return Err("Cannot moderate a member with an equal or higher role.");
                }
            }
        }
        Ok(())
    }
}

/// The gateway session, to resume it after a dropped socket.
#[derive(Clone)]
struct Resumable {
    id: String,
    url: String,
}

/// One group DM. `recipients` are the other people. A named group keeps its title when they change.
pub(super) struct GroupDm {
    pub owner: String,
    pub recipients: Vec<String>,
    pub named: bool,
}

/// Everyone in a group, in list order: the other people, then us. The owner is the second value.
pub(super) fn group_seats(me: &str, owner: &str, recipients: &[String]) -> Vec<(String, bool)> {
    let mut ids: Vec<String> = recipients.iter().filter(|id| !id.is_empty() && id.as_str() != me).cloned().collect();
    if !me.is_empty() {
        ids.push(me.to_owned());
    }
    ids.into_iter().map(|id| (id.clone(), !owner.is_empty() && id == owner)).collect()
}

/// The body of a deliberate unread. The web client sends these two fields and nothing else:
/// the message in the path is the last one that still counts as read.
pub(super) fn manual_ack_body(mentions: u32) -> serde_json::Value {
    json!({"manual": true, "mention_count": mentions})
}

/// An older acknowledgement puts the badge back only when the other device asked to.
/// A repeat of a marker we already passed (a blocked DM whose ack was refused) stays ignored.
pub(super) fn older_ack_applies(manual: bool) -> bool {
    manual
}

/// `TINYCLIENT_PROVE=discord-wave15-share`. The preview, the stream key, a named viewer and
/// the identity post are what this build sends.
pub(super) fn prove_wave15_share() -> Result<(), String> {
    voice::prove_wave15_share()
}

/// `TINYCLIENT_PROVE=discord-wave15a3`. Multi-stream Go Live watch map keeps multiple streams
/// active simultaneously and unwatching one preserves the others.
pub(super) fn prove_wave15a3() -> Result<(), String> {
    voice::prove_wave15a3()
}

/// `TINYCLIENT_PROVE=discord-mark-unread`. The manual ack body is the one Discord expects, and
/// an older ack moves the marker only when it is manual.
pub(super) fn prove_mark_unread() -> Result<(), String> {
    let body = manual_ack_body(2);
    if body["manual"] != true || body["mention_count"] != 2 {
        return Err("a manual unread ack is manual with the mention count".to_owned());
    }
    if body.get("token").is_some() || body.get("last_viewed").is_some() {
        return Err("a manual unread ack carries no read token".to_owned());
    }
    if older_ack_applies(false) || !older_ack_applies(true) {
        return Err("only a manual ack may move the marker backwards".to_owned());
    }
    Ok(())
}

/// `TINYCLIENT_PROVE=discord-group-members`. A four-person group lists four, and one leaving leaves three.
pub(super) fn prove_group_dm() -> Result<(), String> {
    let four = ["2001".to_owned(), "2002".to_owned(), "2006".to_owned()];
    let seats = group_seats("1000", "2001", &four);
    if seats.len() != 4 || !seats[0].1 || seats[3].0 != "1000" {
        return Err("a four-person group lists the others and then us, and marks the owner".to_owned());
    }
    let three = ["2001".to_owned(), "2002".to_owned()];
    if group_seats("1000", "2001", &three).len() == 3 {
        Ok(())
    } else {
        Err("someone leaving leaves three".to_owned())
    }
}

/// A stored row with a new title, keeping what a group DM was already showing.
pub(super) fn renamed(known: &protocol::Conversation, title: &str) -> crate::store::ConversationRow {
    let mut row = crate::store::ConversationRow::new(&known.key.id, known.kind, title);
    row.space = known.space.clone();
    row.group = known.group.clone();
    row.position = known.position;
    row.avatar = known.avatar.as_ref().map(|file| file.id.clone());
    row.peer = known.peer.clone();
    row.call = known.call;
    row.topic = known.topic.clone();
    row.parent = known.parent.clone();
    row.root = known.root.clone();
    row.read_only = known.read_only;
    row.private = known.private;
    row.bot = known.bot;
    row.meeting = known.meeting;
    row.special = known.special;
    row.mark = known.mark;
    row.opted_out = known.opted_out;
    row.limit = known.limit;
    row.max_file_bytes = known.max_file_bytes;
    row.read_only_reason = known.read_only_reason.clone();
    row.rights = known.rights;
    row.spam = known.spam;
    row.rights = known.rights;
    row.slowmode_seconds = known.slowmode_seconds;
    row.locked = known.locked;
    row.age_restricted = known.age_restricted;
    row
}

/// Filing written with a thread row. Folders, pin, and hidden stay as stored.
pub(super) fn kept_filing(known: Option<&protocol::Conversation>, archived: bool, followed: bool) -> crate::store::Filing {
    crate::store::Filing {
        folders: known.map(|row| row.folders.clone()).unwrap_or_default(),
        pin: known.and_then(|row| row.pin),
        hidden: known.is_some_and(|row| row.hidden),
        archived,
        followed,
    }
}

/// Everything the adapter remembers. All of it is bounded by what the user owns or has on screen.
#[derive(Default)]
struct State {
    me: String,
    /// Verified email. READY writes it, and so does a later event once Discord says it is verified.
    /// Missing is unknown.
    verified: Option<bool>,
    /// Whether this account has a phone. The number is never stored. Written the same way.
    /// Missing is unknown.
    phone: Option<bool>,
    session: Option<Resumable>,
    guilds: HashMap<String, GuildInfo>,
    /// Friends, pending requests and direct partners by id: the name shown for them.
    users: HashMap<String, String>,
    relations: HashMap<String, Relation>,
    /// The People rows of those relationships, to store a friend's status line when it changes.
    rows: HashMap<String, crate::store::PersonRow>,
    /// Person id to the direct conversation with them.
    directs: HashMap<String, String>,
    /// Every direct and group conversation's id, to apply a changed mute setting to.
    privates: HashSet<String>,
    /// Group DMs by channel id: who is in them, for the member list.
    groups: HashMap<String, GroupDm>,
    quiet_directs: Quiet,
    /// Friends' presence, replayed after a resume because chatd forgets it when the link drops.
    presence: HashMap<String, (Presence, String)>,
    /// When the earliest timed mute runs out.
    next_unmute: Option<i64>,
    members: members::Watched,
    search: members::Search,
    voice: voice::Voice,
    typed: HashMap<String, Instant>,
    warmed: bool,
    warming: HashSet<String>,
    /// What the last acknowledgement answered with; the web client sends it back on the next.
    ack_token: Option<String>,
    /// Session cache of resolved invites (keyed by `"code"` or `"code:event_id"`).
    invite_cache: HashMap<String, Result<protocol::InvitePreview, String>>,
    /// Session cache of resolved scheduled events (keyed by `(guild_id, event_id)`).
    event_cache: HashMap<(String, String), Result<protocol::ScheduledEvent, String>>,
    /// Recent invite/event resolve timestamps, paced to avoid scraping bursts.
    invite_recent: VecDeque<Instant>,
    /// Our own presence and status line as the settings have them, and those settings as
    /// Discord sent them (`proto::status_settings`), to change one part and send back the rest.
    own: (Presence, String),
    own_emoji: String,
    own_emoji_id: Option<u64>,
    own_status_until: Option<i64>,
    /// The "Playing" name this session publishes. Only `SetActivity` writes it.
    own_activity: String,
    status_settings: Vec<u8>,
    /// Our guild folders protobuf block (field 14) and parsed folder list, kept so reordering
    /// or folder edits preserve unknown fields byte-for-byte.
    guild_folders_settings: Vec<u8>,
    folders: Vec<proto::GuildFolder>,
    /// Raw `PreloadedUserSettings.privacy` bytes (field 8), preserved across updates.
    privacy_settings: Vec<u8>,
    /// Raw `PreloadedUserSettings.text_and_images` bytes (field 6). Explicit content and the
    /// DM spam filter are written back into this block. Empty until Discord sends one: a
    /// made-up field 6 would reset every other text-and-image setting.
    text_and_images: Vec<u8>,
    /// Raw `FrecencyUserSettings` (`settings-proto/2`). Empty until the picker first opens.
    /// Favourite lists are the only part written back. Frecency scores are read for the
    /// "Frequently used" section and never written: a wrong score would replace the account's
    /// frecency on every device.
    frecency: Vec<u8>,
    frecency_loaded: bool,
    /// Standard sticker packs from `GET /sticker-packs`, once a session, Nitro only.
    standard_stickers: Vec<PackedSticker>,
    standard_stickers_loaded: bool,
    /// Per conversation, the stretches of history fetched whole in this session, as message
    /// ids from and to: reading on inside one asks Discord nothing.
    covered: HashMap<String, Vec<(u64, u64)>>,
    /// Server channels the shell has opened. A live message is kept from then on, even when
    /// the channel had nothing cached yet.
    opened: HashSet<String>,
    /// Files whose addresses ran out, waiting to be renewed together.
    renewing: Vec<(String, String, Reply<Download>)>,
    /// The last addresses renewed, old to fresh.
    renewed: HashMap<String, String>,
    /// Our plan (`premium_type`), which says what may be sent.
    premium: u8,
    /// The authentication session, distinct from the resumable gateway session.
    auth_session_id_hash: Option<String>,
    /// `true` only when Discord's sensitive-media filter is set to Show (`0`); otherwise flagged media is blurred.
    show_explicit_media: bool,
    /// Friends' status emoji and what they are doing, as their presence last said, so a voice
    /// state can be told beside it.
    doing: HashMap<String, (String, Option<protocol::Activity>)>,
    /// Cached private notes on people (`user_id -> note`), updated on fetch, write, and `USER_NOTE_UPDATE`.
    notes: HashMap<String, String>,
    /// Vanity codes from Discovery, by server id. A code only, never a URL.
    discovered_vanity: HashMap<String, String>,
    /// When the last profile was asked for: the route is watched, so one per 3 s at most.
    profiled: Option<Instant>,
    /// How many profiles have been asked for: one still waiting its turn that is not the newest
    /// was superseded, and is dropped rather than sent (`info.rs`).
    profile_asked: u64,
    /// Per channel, the read acknowledgement last sent and the newest one held back since
    /// (`mark_read`): one per channel per [`ACK_EVERY`].
    acks: HashMap<String, Ack>,
    /// Age-gated (NSFW) channel ids, kept so recent mentions skip them.
    nsfw_channels: HashSet<String>,
    /// Debounced category collapse/expand writes waiting to PATCH guild settings.
    collapses: CollapseQueue,
    /// In-flight interaction nonces waiting on gateway settlement or modal popups.
    pending_interactions: HashMap<String, interactions::PendingInteraction>,
    /// Application-command indexes for this session: `guild:{id}`, `dm:{channel}`, or `user`.
    command_index: HashMap<String, Vec<protocol::AppCommand>>,
    /// Autocomplete waiters keyed by interaction nonce. The command thread waits; the gateway sends.
    autocomplete: HashMap<String, std::sync::mpsc::Sender<Vec<protocol::CommandChoice>>>,
    /// Per thread, our own thread-member notification flags and muted flag.
    thread_members: HashMap<String, model::ThreadMember>,
}

/// One channel's read acknowledgements.
#[derive(Default)]
struct Ack {
    sent: Option<Instant>,
    /// The newest message read since `sent`, sent when the 3 s are up.
    held: Option<String>,
}

/// Read acknowledgements to one channel are at most this often; the newest goes.
const ACK_EVERY: Duration = Duration::from_secs(3);

/// Frames the command thread wants sent, and a local socket pair that wakes the socket thread.
struct Outbox {
    queue: Mutex<gateway::SendQueue>,
    wake: gateway::WakePair,
}

struct Discord {
    ctx: Context,
    rest: Rest,
    token: String,
    state: Mutex<State>,
    outbox: Outbox,
    /// The last sequence number heard, for heartbeats and for resuming.
    sequence: AtomicU64,
    /// How many sessions in a row Discord refused to start, and what it said last.
    refused_sessions: Mutex<(u32, String)>,
    /// No session is asked for until the user does something (`gateway.rs::hold`).
    held: AtomicBool,
    /// This account's message searches: one in flight at a time, paced from when the last
    /// finished, with recent answers kept in memory.
    message_search: Mutex<info::MessageSearch>,
    /// Serialises queued message searches per account so rapid Enters wait their turn.
    search_turn: Mutex<()>,
    /// One Go Live request at a time, so a stop cannot land before the start it was meant to end.
    stream_gate: Mutex<()>,
}

/// The user id a token starts with (its first part is the id in base64), so the account is named
/// by who it is.
fn user_of(token: &str) -> Option<String> {
    let first = token.split('.').next()?;
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(first.trim_end_matches('=')).ok()?;
    let id = String::from_utf8(bytes).ok()?;
    (!id.is_empty() && id.len() <= 20 && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

fn unreadable() -> String {
    "the Discord session is unreadable".to_owned()
}

/// One token inside the keychain item. A session from before several accounts were kept is the
/// one object `{"token","user_agent"}`. Several are `accounts`.
struct Row {
    token: String,
    id: String,
    name: String,
}

fn account_id(token: &str, id: &str) -> String {
    if let Some(user) = user_of(token) {
        return user;
    }
    let id = id.trim();
    let plain = !id.is_empty()
        && id != "."
        && id != ".."
        && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if plain { id.to_owned() } else { "main".to_owned() }
}

fn stored_text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// `(user_agent, rows)`. One account stays the legacy object; an empty token is not an account.
fn rows(session: &str) -> Result<(String, Vec<Row>), String> {
    let parsed: Value = serde_json::from_str(session).map_err(|_| unreadable())?;
    let agent = stored_text(&parsed, "user_agent");
    if let Some(list) = parsed.get("accounts").and_then(Value::as_array).filter(|list| !list.is_empty()) {
        let mut found = Vec::new();
        for entry in list {
            let token = stored_text(entry, "token");
            if token.is_empty() {
                continue;
            }
            found.push(Row { id: account_id(&token, &stored_text(entry, "id")), token, name: stored_text(entry, "name") });
        }
        if found.is_empty() {
            return Err(unreadable());
        }
        return Ok((agent, found));
    }
    let token = stored_text(&parsed, "token");
    if token.is_empty() {
        return Err(unreadable());
    }
    Ok((agent, vec![Row { id: account_id(&token, ""), token, name: String::new() }]))
}

/// What `start` is given: one token, so each account is its own adapter.
fn one_session(agent: &str, token: &str) -> String {
    if agent.is_empty() {
        json!({"token": token}).to_string()
    } else {
        json!({"token": token, "user_agent": agent}).to_string()
    }
}

/// One account is the legacy object. Several are a list. Nothing left is `None`.
fn pack(agent: &str, rows: &[Row]) -> Option<String> {
    match rows {
        [] => None,
        [one] => Some(one_session(agent, &one.token)),
        many => {
            let accounts: Vec<Value> = many
                .iter()
                .map(|row| {
                    let mut entry = json!({"token": row.token, "id": row.id});
                    if !row.name.is_empty() {
                        entry["name"] = Value::String(row.name.clone());
                    }
                    entry
                })
                .collect();
            let mut stored = json!({"accounts": accounts});
            if !agent.is_empty() {
                stored["user_agent"] = Value::String(agent.to_owned());
            }
            Some(stored.to_string())
        }
    }
}

/// One account per token, named by the user's id when the token shows it.
pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let (agent, rows) = rows(session)?;
    let mut found = Vec::new();
    for row in rows {
        if found.iter().any(|signed: &Signed| signed.account == row.id) {
            continue;
        }
        let name = if row.name.is_empty() { "Discord".to_owned() } else { row.name };
        found.push(Signed { account: row.id, name, session: one_session(&agent, &row.token) });
    }
    if found.is_empty() {
        return Err(unreadable());
    }
    Ok(found)
}

/// The session with one account taken out. `None` only when that was the last one. An account
/// that is not in it leaves the original string untouched.
pub fn without(session: &str, account: &str) -> Option<String> {
    let (agent, mut rows) = rows(session).ok()?;
    let before = rows.len();
    rows.retain(|row| row.id != account);
    if rows.len() == before {
        return Some(session.to_owned());
    }
    pack(&agent, &rows)
}

/// A finished sign-in joins the accounts already stored. The same user replaces their own token.
pub fn combine(existing: &str, fresh: &str) -> Result<String, String> {
    let (old_agent, mut kept) = rows(existing)?;
    let (new_agent, fresh_rows) = rows(fresh)?;
    let agent = if new_agent.is_empty() { old_agent } else { new_agent };
    for row in fresh_rows {
        kept.retain(|one| one.id != row.id);
        kept.push(row);
    }
    pack(&agent, &kept).ok_or_else(unreadable)
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let session: Session = serde_json::from_str(session).map_err(|err| err.to_string())?;
    let wake = gateway::WakePair::new().map_err(|err| err.to_string())?;
    // Nothing here touches the network or runs a program: who the session says it is (the
    // browser's version, the web client's build) is worked out on the adapter's own threads,
    // so chatd is serving before any of it.
    let discord = Arc::new(Discord {
        rest: Rest::new(session.token.clone(), ctx.clone()),
        ctx,
        token: session.token,
        state: Mutex::new(State::default()),
        outbox: Outbox {
            queue: Mutex::new(gateway::SendQueue::default()),
            wake,
        },
        sequence: AtomicU64::new(0),
        refused_sessions: Mutex::new((0, String::new())),
        held: AtomicBool::new(false),
        message_search: Mutex::new(info::MessageSearch::default()),
        search_turn: Mutex::new(()),
        stream_gate: Mutex::new(()),
    });
    let posting = discord.clone();
    discord.ctx.hub.calls.on_identity(move |conversation, identity| {
        if conversation.account() != posting.ctx.account || posting.ctx.stopped() {
            return;
        }
        posting.publish_dave_key(identity);
    });
    let socket = discord.clone();
    std::thread::spawn(move || keep_connected(&socket.ctx, || socket.connect()));
    let (commands, network_inbox) = channel();
    let viewport = discord.clone();
    std::thread::spawn(move || {
        // Cache reads and desired viewport updates must not sit behind history
        // backfill sleeps or an HTTP request. Preserve their order on one thread.
        while let Ok(command) = inbox.recv() {
            match command {
                command @ (Command::Members { .. } | Command::Watch(_) | Command::OpenSpace(_) | Command::SearchPeople(_)) => viewport.command(command),
                command => {
                    let stop = matches!(&command, Command::Stop);
                    if commands.send(command).is_err() || stop { break; }
                }
            }
        }
    });
    std::thread::spawn(move || {
        // A send with files uploads on a thread of its own, so the commands behind it (its own
        // cancel among them) are not held up for as long as the upload takes.
        let other = |command| match command {
            Command::SendWith { conversation_id, client_id, text, reply_to, with } => {
                discord.asked();
                let sending = discord.clone();
                let (channel, client) = (conversation_id.clone(), client_id.clone());
                upload::spawn(&discord.ctx, &conversation_id, &client_id, &with.files.clone(), move |up| {
                    sending.send_with(&channel, &client, &text, reply_to.as_deref(), &with, up)
                });
            }
            // A post with files uploads before Discord will name the thread. That waits on its
            // own thread, so the commands behind it (a cancel among them) are not held up.
            Command::ForumPost { conversation_id, title, text, tags, with, reply } if !with.files.is_empty() => {
                discord.asked();
                let sending = discord.clone();
                let started = std::thread::Builder::new().name("forum-post".into()).spawn(move || {
                    answer(reply, sending.forum_post(&conversation_id, &title, &text, &tags, &with));
                });
                if let Err(err) = started {
                    discord.log("forum post", err);
                }
            }
            // A search can wait on Discord's tight search limits: it runs on a thread of its
            // own, so the sends and reactions behind it are not held up, and one queued past
            // the shell's patience does not ask at all.
            Command::Search { query, scope, filters, cursor, at, reply } => {
                discord.asked();
                if at.elapsed() < SEARCH_PATIENCE {
                    Discord::search_queued(&discord, query, scope, filters, cursor, at, reply);
                } else {
                    drop(reply);
                }
            }
            // A profile waits its turn (one per 3 s) on a thread of its own, so a send behind it
            // is never held up, and one superseded while it waits is dropped.
            Command::Profile { person_id, reply } => Discord::profile_queued(&discord, person_id, None, reply),
            Command::ProfileIn { person_id, space_id, reply } => Discord::profile_queued(&discord, person_id, Some(space_id), reply),
            command => discord.command(command),
        };
        serve_commands(&discord.ctx, network_inbox, PACE, |id| discord.backfill(id), other)
    });
    Ok(())
}

/// A pause a person might make: `from` to `to` milliseconds.
fn jitter(from: u64, to: u64) -> Duration {
    Duration::from_millis(from + gateway::random() % (to - from).max(1))
}

/// Names for the ids inside one message.
struct Lookup<'a> {
    discord: &'a Discord,
    guild: Option<&'a str>,
    mentions: &'a [User],
}

const ENTITY_CHOICES: usize = 100;

impl Lookup<'_> {
    fn user_choices(&self) -> Vec<(String, String)> {
        let state = crate::hub::lock(&self.discord.state);
        let mut rows: Vec<(String, String)> = state
            .users
            .iter()
            .filter(|(id, name)| !id.is_empty() && !name.is_empty())
            .map(|(id, name)| (id.clone(), name.clone()))
            .collect();
        for user in self.mentions {
            if user.id.is_empty() || rows.iter().any(|(id, _)| id == &user.id) {
                continue;
            }
            let nick = user.member.as_ref().and_then(|member| member.nick.clone());
            let name = nick.filter(|nick| !nick.is_empty()).unwrap_or_else(|| user.name().to_owned());
            if !name.is_empty() {
                rows.push((user.id.clone(), name));
            }
        }
        rows
    }

    fn role_choices(&self) -> Vec<(String, String)> {
        let state = crate::hub::lock(&self.discord.state);
        let Some(guild) = self.guild.and_then(|id| state.guilds.get(id)) else {
            return Vec::new();
        };
        guild
            .roles
            .iter()
            .filter(|(id, role)| **id != 0 && !role.name.is_empty())
            .map(|(id, role)| (id.to_string(), role.name.clone()))
            .collect()
    }

    fn channel_choices(&self) -> Vec<(String, String)> {
        let ids: Vec<String> = {
            let state = crate::hub::lock(&self.discord.state);
            let Some(guild) = self.guild.and_then(|id| state.guilds.get(id)) else {
                return Vec::new();
            };
            guild.channels.iter().map(|(id, _)| id.to_string()).collect()
        };
        let mut rows = Vec::new();
        for id in ids {
            if rows.iter().any(|(known, _)| known == &id) {
                continue;
            }
            let Ok(Some(row)) = self.discord.ctx.known(&id) else {
                continue;
            };
            if !matches!(
                row.kind,
                ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Voice | ConversationKind::Forum
            ) {
                continue;
            }
            let name = row.title.trim_start_matches('#').trim();
            if name.is_empty() {
                continue;
            }
            rows.push((id, name.to_owned()));
        }
        rows
    }
}

fn sort_choices(rows: &mut Vec<(String, String)>) {
    rows.sort_by(|a, b| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()).then(a.0.cmp(&b.0)));
    rows.truncate(ENTITY_CHOICES);
}

impl Names for Lookup<'_> {
    fn choices(&self, kind: u8) -> Vec<(String, String)> {
        let mut rows = match kind {
            5 => self.user_choices(),
            6 => self.role_choices(),
            7 => {
                let mut rows = self.user_choices();
                for role in self.role_choices() {
                    if !rows.iter().any(|(id, _)| id == &role.0) {
                        rows.push(role);
                    }
                }
                rows
            }
            8 => self.channel_choices(),
            _ => return Vec::new(),
        };
        sort_choices(&mut rows);
        rows
    }

    fn user(&self, id: &str) -> Option<String> {
        if let Some(user) = self.mentions.iter().find(|user| user.id == id) {
            let nick = user.member.as_ref().and_then(|member| member.nick.clone());
            return Some(nick.filter(|nick| !nick.is_empty()).unwrap_or_else(|| user.name().to_owned()));
        }
        crate::hub::lock(&self.discord.state).users.get(id).cloned()
    }

    fn role(&self, id: &str) -> Option<String> {
        let state = crate::hub::lock(&self.discord.state);
        let guild = state.guilds.get(self.guild?)?;
        guild.roles.get(&number(id)).map(|role| role.name.clone())
    }

    fn channel(&self, id: &str) -> Option<String> {
        self.discord.ctx.known(id).ok().flatten().map(|row| row.title)
    }

    fn space(&self, id: &str) -> Option<String> {
        crate::hub::lock(&self.discord.state).guilds.get(id).map(|guild| guild.name.clone())
    }
}

/// An emoji as a path names it: every byte that is not plainly safe in a path, escaped.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Which messages a page of history is.
enum Stretch<'a> {
    Newest,
    Before(&'a str),
    After(&'a str),
    Around(&'a str),
}

fn refusal(refused: Refused) -> Refusal {
    Refusal::Refused(refused.reason())
}

/// Where the starter message lives, and the newest reply id already on its chip.
struct ThreadChip {
    parent: String,
    starter: String,
    seen: u64,
}

fn chip_key(thread: &str) -> String {
    format!("thread-chip:{thread}")
}

/// One new reply on a starter's thread chip. `seen` is the newest reply id already
/// counted, so a reply at or behind it does not move the count.
fn fold_reply(summary: &mut ThreadSummary, seen: &mut u64, reply_id: u64, author: Option<&Key>, unread: u32) -> bool {
    if reply_id == 0 || reply_id <= *seen {
        return false;
    }
    *seen = reply_id;
    summary.replies = summary.replies.saturating_add(1);
    summary.last = seconds(&reply_id.to_string());
    summary.unread = unread;
    if let Some(author) = author.filter(|author| !author.id.is_empty()).cloned() {
        summary.participants.retain(|person| person.id != author.id);
        summary.participants.insert(0, author);
        summary.participants.truncate(protocol::THREAD_FACES);
    }
    true
}

/// `TINYCLIENT_PROVE=discord-thread-chip`. Two replies would be the count and the faces;
/// a third and fourth keep only the newest three, and an id already counted stays put.
pub(super) fn prove_thread_chip() -> Result<(), String> {
    let account = protocol::AccountKey { network: protocol::Network::Discord, account: "1".into() };
    let person = |id: &str| Key::new(&account, id);
    let (a, b, c, d) = (person("a"), person("b"), person("c"), person("d"));
    let mut summary = ThreadSummary {
        id: "t".into(),
        replies: 4,
        last: 0,
        participants: Vec::new(),
        unread: 0,
    };
    let mut seen = 100u64;
    if fold_reply(&mut summary, &mut seen, 100, Some(&a), 1) || summary.replies != 4 {
        return Err("a reply already in the count was counted again".to_owned());
    }
    let stamp = (1_700_000_000_000u64 - 1_420_070_400_000) << 22;
    let ids = [stamp + 1, stamp + 2, stamp + 3, stamp + 4];
    let authors = [&a, &b, &c, &d];
    for (at, (id, author)) in ids.iter().zip(authors).enumerate() {
        if !fold_reply(&mut summary, &mut seen, *id, Some(author), (at as u32) + 1) {
            return Err("a new reply did not count".to_owned());
        }
    }
    if summary.replies != 8 {
        return Err(format!("the chip counted {} replies, not 8", summary.replies));
    }
    let faces: Vec<&str> = summary.participants.iter().map(|person| person.id.as_str()).collect();
    if faces != ["d", "c", "b"] {
        return Err(format!("the chip showed {faces:?}, not the newest three"));
    }
    if summary.unread != 4 || summary.last == 0 {
        return Err("the chip did not keep the unread count and the time of the newest reply".to_owned());
    }
    if fold_reply(&mut summary, &mut seen, ids[3], Some(&d), 4) {
        return Err("the same reply was counted twice".to_owned());
    }
    Ok(())
}

impl Discord {
    fn log(&self, what: &str, why: impl std::fmt::Display) {
        tracing::warn!("chatd: Discord: {what}: {why}");
    }

    fn me(&self) -> String {
        crate::hub::lock(&self.state).me.clone()
    }

    /// A request, with the socket thread woken when it shows the session is over.
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, Refused> {
        let answer = self.rest.call(method, path, body);
        if matches!(answer, Err(Refused::Expired)) {
            self.wake();
        }
        answer
    }

    /// One-shot request (never retried on 429; wakes socket on 401).
    /// One-shot request (no 429 retry, per-request captcha failure), waking the socket on 401.
    fn call_once(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, Refused> {
        let answer = self.rest.call_once_with(method, path, body, &[]);
        if matches!(answer, Err(Refused::Expired)) {
            self.wake();
        }
        answer
    }

    /// A message object into the model. Its files are returned for registering.
    fn convert(&self, posted: &Posted, guild: Option<&str>, kind: ConversationKind) -> (Message, Files) {
        let me = self.me();
        let lookup = Lookup {
            discord: self,
            guild,
            mentions: &posted.mentions,
        };
        // Outside a server a friend goes by the nickname the user gave them.
        let author = posted.author.as_ref().filter(|_| guild.is_none() && posted.webhook_id.is_none());
        let author = author.and_then(|author| crate::hub::lock(&self.state).users.get(&author.id).cloned());
        let blur_explicit = !crate::hub::lock(&self.state).show_explicit_media;
        let place = Place {
            account: &self.ctx.account,
            me: &me,
            guild,
            names: &lookup,
            author,
            blur_explicit,
        };
        let (mut message, mut files) = content::message(posted, &place);
        if let (Some(guild_id), None) = (guild, posted.webhook_id.as_ref()) {
            let author_id = &message.sender.id;
            if !author_id.is_empty() {
                let mut state = crate::hub::lock(&self.state);
                if let Some(info) = state.guilds.get_mut(guild_id) {
                    let (color, icon, emoji) = if let Some(member) = posted.member.as_ref().filter(|m| !m.roles.is_empty()) {
                        let role_ids: Vec<u64> = member.roles.iter().map(|r| number(r)).collect();
                        info.set_member_roles(author_id, role_ids.clone());
                        info.resolve_role_style(&role_ids)
                    } else {
                        info.member_role_style(author_id)
                    };
                    message.sender.role_color = color;
                    message.sender.role_emoji = emoji;
                    if let Some((file, url)) = icon {
                        message.sender.role_icon = Some(file.clone());
                        files.files.push((file, url));
                    }
                }
            }
        }
        message.mentions_me = self.mentions_me(posted, guild, kind, &me);
        (message, files)
    }

    /// Discord's own rule for a mention, with the suppress settings the user made there.
    fn mentions_me(&self, posted: &Posted, guild: Option<&str>, kind: ConversationKind, me: &str) -> bool {
        if posted.author.as_ref().is_some_and(|author| author.id == me) {
            return false;
        }
        let named = posted.mentions.iter().any(|user| user.id == me);
        // Every message of a direct conversation already counts; only a group's can single me out.
        if kind == ConversationKind::Direct {
            return false;
        }
        let state = crate::hub::lock(&self.state);
        let Some(info) = guild.and_then(|guild| state.guilds.get(guild)) else {
            return named || posted.mention_everyone;
        };
        let role = posted.mention_roles.iter().any(|role| info.my_roles.contains(&number(role)));
        named || (posted.mention_everyone && !info.quiet.suppress_everyone) || (role && !info.quiet.suppress_roles)
    }

    fn read_chip(&self, thread: &str) -> ThreadChip {
        let raw = self.ctx.note(&chip_key(thread)).unwrap_or_default();
        let mut parts = raw.split('\t');
        ThreadChip {
            parent: parts.next().unwrap_or("").to_owned(),
            starter: parts.next().unwrap_or("").to_owned(),
            seen: parts.next().unwrap_or("").parse().unwrap_or(0),
        }
    }

    fn write_chip(&self, thread: &str, chip: &ThreadChip) {
        self.ctx.set_note(&chip_key(thread), &format!("{}\t{}\t{}", chip.parent, chip.starter, chip.seen));
    }

    /// The message that carries `thread` is the starter. Its id is not always the thread's.
    fn remember_starter(&self, thread: &model::Channel, parent: &str, starter: &str) {
        let mut chip = self.read_chip(&thread.id);
        if !parent.is_empty() {
            chip.parent = parent.to_owned();
        }
        if !starter.is_empty() {
            chip.starter = starter.to_owned();
        }
        if let Some(last) = thread.last_message_id.as_deref() {
            let seen = number(last);
            if seen > chip.seen {
                chip.seen = seen;
            }
        }
        self.write_chip(&thread.id, &chip);
    }

    /// A page brought the starter again. Keep faces and a count we already learned, and
    /// take the thread row's unread, which is the same read state as any channel.
    fn carried_thread(&self, channel: &str, message: &mut Message) -> rusqlite::Result<()> {
        let Some(summary) = message.thread.as_mut() else {
            return Ok(());
        };
        if let Some(row) = self.ctx.known(&summary.id)? {
            summary.unread = row.unread;
        }
        if let Some(stored) = self.ctx.stored_message(channel, &message.id)? {
            if let Some(had) = stored.thread.filter(|had| had.id == summary.id) {
                if summary.participants.is_empty() {
                    summary.participants = had.participants;
                }
                summary.replies = summary.replies.max(had.replies);
                if summary.last < had.last {
                    summary.last = had.last;
                }
            }
        }
        Ok(())
    }

    /// `THREAD_UPDATE`: Discord's count and newest id, when they are ahead of the chip.
    /// Faces stay; this event does not name who replied.
    pub(super) fn note_thread_update(&self, thread: &model::Channel) -> rusqlite::Result<()> {
        let mut chip = self.read_chip(&thread.id);
        if chip.parent.is_empty() {
            if let Some(parent) = thread.parent_id.as_deref() {
                chip.parent = parent.to_owned();
            }
        }
        if chip.starter.is_empty() && !chip.parent.is_empty() {
            let guess = self
                .ctx
                .known(&thread.id)?
                .and_then(|known| known.root)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| thread.id.clone());
            if self.ctx.stored_message(&chip.parent, &guess)?.is_some() {
                chip.starter = guess;
            }
        }
        if let Some(last) = thread.last_message_id.as_deref().filter(|id| !id.is_empty()) {
            let seen = number(last);
            if seen > chip.seen {
                chip.seen = seen;
            }
        }
        let parent = chip.parent.clone();
        let starter = chip.starter.clone();
        self.write_chip(&thread.id, &chip);
        if parent.is_empty() || starter.is_empty() {
            return Ok(());
        }
        let Some(mut message) = self.ctx.stored_message(&parent, &starter)? else {
            return Ok(());
        };
        let Some(summary) = message.thread.as_mut().filter(|summary| summary.id == thread.id) else {
            return Ok(());
        };
        let mut changed = false;
        if let Some(count) = thread.message_count {
            if count > summary.replies {
                summary.replies = count;
                changed = true;
            }
        }
        if let Some(last) = thread.last_message_id.as_deref().filter(|id| !id.is_empty()) {
            let at = seconds(last);
            if number(last) >= chip.seen && at != summary.last {
                summary.last = at;
                changed = true;
            }
        }
        if let Some(row) = self.ctx.known(&thread.id)? {
            if summary.unread != row.unread {
                summary.unread = row.unread;
                changed = true;
            }
        }
        if changed {
            self.ctx.history(&parent, &[message])?;
        }
        Ok(())
    }

    /// A reply arrived in a thread whose starter is already stored.
    fn fold_thread_reply(&self, posted: &Posted, thread: &Conversation) -> rusqlite::Result<()> {
        let mut chip = self.read_chip(&thread.key.id);
        let mut learned = false;
        if chip.parent.is_empty() {
            if let Some(parent) = thread.parent.as_deref() {
                chip.parent = parent.to_owned();
                learned = true;
            }
        }
        if chip.starter.is_empty() {
            if let Some(root) = thread.root.as_deref().filter(|id| !id.is_empty()) {
                chip.starter = root.to_owned();
                learned = true;
            }
        }
        let reply = number(&posted.id);
        let starter_id = chip.starter.clone();
        let starter_message = posted.id == starter_id || posted.id == thread.key.id || thread.root.as_deref() == Some(posted.id.as_str());
        if posted.id.is_empty() || starter_message || reply == 0 || reply <= chip.seen {
            if learned {
                self.write_chip(&thread.key.id, &chip);
            }
            return Ok(());
        }
        let parent = chip.parent.clone();
        if parent.is_empty() || starter_id.is_empty() {
            return Ok(());
        }
        let Some(mut message) = self.ctx.stored_message(&parent, &starter_id)? else {
            return Ok(());
        };
        let summary = message.thread.get_or_insert(ThreadSummary {
            id: thread.key.id.clone(),
            replies: 0,
            last: 0,
            participants: Vec::new(),
            unread: 0,
        });
        if !summary.id.is_empty() && summary.id != thread.key.id {
            return Ok(());
        }
        summary.id = thread.key.id.clone();
        let outgoing = posted.author.as_ref().is_some_and(|author| author.id == self.me());
        let already = self.ctx.stored_message(&thread.key.id, &posted.id)?.is_some();
        let unread = if already || outgoing { thread.unread } else { thread.unread.saturating_add(1) };
        let author = posted.author.as_ref().filter(|author| !author.id.is_empty()).map(|author| Key::new(&self.ctx.account, &author.id));
        if !fold_reply(summary, &mut chip.seen, reply, author.as_ref(), unread) {
            return Ok(());
        }
        self.write_chip(&thread.key.id, &chip);
        self.ctx.history(&parent, &[message])
    }

    /// Records the thread a message started, and folds a reply onto the starter's chip.
    fn thread_of(&self, posted: &Posted, conversation: &Conversation) -> rusqlite::Result<()> {
        if let Some(thread) = &posted.thread {
            if conversation.kind != ConversationKind::Thread {
                self.remember_starter(thread, &conversation.key.id, &posted.id);
            }
            if self.ctx.known(&thread.id)?.is_none() {
                // Outside the tree: a thread we are not in is not listed there. It names its
                // parent, so it goes when the channel goes.
                let title = thread.name.clone().unwrap_or_else(|| "Thread".to_owned());
                if let Some(guild) = self.guild_of(conversation) {
                    self.ctx.set_note(&format!("guild:{}", thread.id), &guild);
                }
                self.ctx.conversation(&ConversationRow {
                    parent: Some(conversation.key.id.clone()),
                    // A thread started from a message has that message's id on Discord. The
                    // note from `remember_starter` is what the chip follows when it differs.
                    root: Some(thread.id.clone()),
                    age_restricted: thread.nsfw || conversation.age_restricted,
                    ..ConversationRow::new(&thread.id, ConversationKind::Thread, title)
                })?;
            }
        }
        if conversation.kind == ConversationKind::Thread {
            self.fold_thread_reply(posted, conversation)?;
        }
        Ok(())
    }

    /// Remembers that the messages `from..=to` of a conversation are stored whole.
    fn cover(&self, channel: &str, from: u64, to: u64) {
        let mut state = crate::hub::lock(&self.state);
        if state.covered.len() >= COVERED && !state.covered.contains_key(channel) {
            state.covered.clear();
        }
        let stretches = state.covered.entry(channel.to_owned()).or_default();
        stretches.push((from, to));
        stretches.sort_unstable();
        let mut joined: Vec<(u64, u64)> = Vec::with_capacity(stretches.len());
        for stretch in stretches.drain(..) {
            match joined.last_mut() {
                Some(last) if stretch.0 <= last.1 => last.1 = last.1.max(stretch.1),
                _ => joined.push(stretch),
            }
        }
        // The newest stretches are the ones being read.
        let extra = joined.len().saturating_sub(STRETCHES);
        *stretches = joined.split_off(extra);
    }

    /// True when what follows message `after` is already stored: it lies inside a stretch
    /// fetched in this session, short of its end.
    fn covered_after(&self, channel: &str, after: u64) -> bool {
        let state = crate::hub::lock(&self.state);
        let stretches = state.covered.get(channel);
        stretches.is_some_and(|stretches| stretches.iter().any(|(from, to)| *from <= after && after < *to))
    }

    /// One page of a conversation's history into the store.
    fn page(&self, conversation: &Conversation, stretch: Stretch) -> Result<(), Failure> {
        let channel = &conversation.key.id;
        let query = match stretch {
            Stretch::Newest => format!("limit={PAGE}"),
            Stretch::Before(id) => format!("before={id}&limit={PAGE}"),
            Stretch::After(id) => format!("after={id}&limit={PAGE}"),
            Stretch::Around(id) => format!("around={id}&limit={PAGE}"),
        };
        let text = self.call("GET", &format!("/channels/{channel}/messages?{query}"), None)?;
        let posted: Vec<Posted> = serde_json::from_str(&text)?;
        drop(text);
        let guild = self.guild_of(conversation);
        let mut messages = Vec::with_capacity(posted.len());
        let mut files = Files::default();
        for posted in &posted {
            let (mut message, referred) = self.convert(posted, guild.as_deref(), conversation.kind);
            files.merge(referred);
            self.thread_of(posted, conversation)?;
            self.carried_thread(channel, &mut message)?;
            messages.push(message);
        }
        let full = messages.len() >= PAGE;
        if matches!(stretch, Stretch::Newest) && full {
            self.close_gap(channel, &messages)?;
        }
        files.register(&self.ctx)?;
        self.ctx.history(channel, &messages)?;
        let ids = messages.iter().map(|message| number(&message.id));
        let (Some(oldest), Some(newest)) = (ids.clone().min(), ids.max()) else {
            return Ok(());
        };
        self.forget_deleted(channel, &messages, oldest, newest)?;
        // What the page proves is stored: itself, and up to whatever it was asked from.
        let (from, to) = match stretch {
            Stretch::Newest => (oldest, u64::MAX),
            Stretch::Before(id) => (oldest, number(id)),
            Stretch::After(id) => (number(id), if full { newest } else { u64::MAX }),
            Stretch::Around(_) => (oldest, newest),
        };
        self.cover(channel, from, to);
        Ok(())
    }

    /// What is stored between the ends of a page Discord just sent, and is not in it, was
    /// deleted while we were away.
    fn forget_deleted(&self, channel: &str, page: &[Message], oldest: u64, newest: u64) -> rusqlite::Result<()> {
        let stored = self.ctx.stored_ids(channel, &oldest.to_string(), &newest.to_string(), crate::store::KEEP_MESSAGES)?;
        for id in stored.iter().filter(|id| !page.iter().any(|message| message.id == **id)) {
            // Only what Discord named: a message of ours still on its way has no id of its.
            let sent = self.ctx.stored_message(channel, id)?.is_some_and(|message| message.state == SendState::Sent);
            if sent && (oldest..=newest).contains(&number(id)) {
                self.ctx.deleted(channel, id)?;
            }
        }
        Ok(())
    }

    /// The newest page does not reach what is stored: everything between was never fetched.
    /// Stored messages older than the page go, so the cache has no silent hole and scrolling up
    /// asks Discord again.
    fn close_gap(&self, channel: &str, page: &[Message]) -> rusqlite::Result<()> {
        let Some(oldest) = page.iter().map(|message| number(&message.id)).min() else {
            return Ok(());
        };
        for _ in 0..crate::store::KEEP_MESSAGES {
            match self.ctx.newest(channel)? {
                Some(stored) if number(&stored) < oldest => self.ctx.deleted(channel, &stored)?,
                _ => break,
            }
        }
        Ok(())
    }

    /// The server a conversation is in. A thread recorded outside a tree has it in a note.
    fn guild_of(&self, conversation: &Conversation) -> Option<String> {
        match (&conversation.space, conversation.kind) {
            (Some(space), _) => Some(space.clone()),
            (None, ConversationKind::Thread) => self.ctx.note(&format!("guild:{}", conversation.key.id)),
            _ => None,
        }
    }

    fn backfill(&self, channel: &str) {
        // The shell asked for this channel. Live messages are kept from here, including the
        // ones that arrive while the history page is still empty.
        crate::hub::lock(&self.state).opened.insert(channel.to_owned());
        // Warming is nobody's click, so it does not run at a machine's pace.
        if crate::hub::lock(&self.state).warming.remove(channel) {
            std::thread::sleep(jitter(400, 2500));
        } else {
            self.asked();
        }
        let conversation = match self.ctx.known(channel) {
            Ok(Some(conversation)) => conversation,
            _ => return,
        };
        // A forum's posts are its threads; it has no messages of its own.
        if conversation.kind == ConversationKind::Forum {
            return;
        }
        self.fetch(&conversation, Stretch::Newest);
    }

    fn fetch(&self, conversation: &Conversation, stretch: Stretch) {
        if let Err(Failure::SignedOut(why) | Failure::Dropped(why)) = self.page(conversation, stretch) {
            self.log("history", why);
        }
    }

    /// Resolves `emoji` to the path segment Discord expects (either the unicode emoji,
    /// `name:id` for a custom emoji already in that form, or looked up from `:name:` / `emoji:<id>`).
    fn reaction_emoji(&self, channel: &str, message: &str, emoji: &str) -> Result<String, Refusal> {
        let spelled = emoji.len() > 2 && emoji.starts_with(':') && emoji.ends_with(':');
        if spelled || emoji.starts_with("emoji:") {
            let stored = self.ctx.stored_message(channel, message).ok().flatten();
            let reactions = stored.map(|stored| stored.reactions).unwrap_or_default();
            let known = reactions.into_iter().find(|reaction| match &reaction.file {
                Some(file) => reaction.emoji == emoji || file.id == emoji,
                None => false,
            });
            let id = known.as_ref().and_then(|reaction| reaction.file.as_ref()).map(|file| file.id.clone());
            let id = id.or_else(|| emoji.starts_with("emoji:").then(|| emoji.to_owned()));
            // CONTRACT: `:name:` says which custom emoji only while it is on the message. One
            // that is not there has to come as its file's id.
            let Some(id) = id.as_deref().and_then(|id| id.strip_prefix("emoji:")) else {
                return Err(Refusal::Refused("That emoji is not on the message, so tinyclient cannot tell Discord which one it is.".to_owned()));
            };
            let name = known.map_or_else(|| "emoji".to_owned(), |reaction| reaction.emoji.trim_matches(':').to_owned());
            Ok(format!("{name}:{id}"))
        } else {
            Ok(emoji.to_owned())
        }
    }

    /// Adds or takes back our reaction. `emoji` is the emoji itself, or a custom one: as the
    /// message spells it (`:name:`) when it is already there, or as the id of its registered
    /// file (`emoji:<id>`). Discord wants a custom one as `name:id`.
    ///
    /// The query is what the web client adds; it is from the community documentation and
    /// unconfirmed against a real account. So is sending a placeholder name with an id whose
    /// name is not known here (Discord is said to go by the id).
    fn react(&self, channel: &str, message: &str, emoji: &str, on: bool, burst: bool) -> Result<(), Refusal> {
        if on && burst && !matches!(crate::hub::lock(&self.state).premium, 1..=3) {
            return Err(Refusal::Refused("Super reactions require Discord Nitro.".to_owned()));
        }
        let named = self.reaction_emoji(channel, message, emoji)?;
        let reaction = format!("/channels/{channel}/messages/{message}/reactions/{}", escaped(&named));
        let rtype = if burst { 1 } else { 0 };
        let asked = match on {
            true => self.call("PUT", &format!("{reaction}/%40me?location=Message%20Reaction%20Picker&type={rtype}"), None),
            false => self.call("DELETE", &format!("{reaction}/{rtype}/%40me?location=Message%20Inline%20Button&burst={burst}"), None),
        };
        asked.map(drop).map_err(refusal)
    }

    /// Moderator reaction removal:
    /// - `emoji: None`: `DELETE /channels/{channel}/messages/{message}/reactions`
    /// - `emoji: Some(e), user_id: None`: `DELETE /channels/{channel}/messages/{message}/reactions/{e}`
    /// - `emoji: Some(e), user_id: Some(u)`: `DELETE /channels/{channel}/messages/{message}/reactions/{e}/{u}`
    fn clear_reactions(&self, channel: &str, message: &str, emoji: Option<&str>, user_id: Option<&str>) -> Result<(), Refusal> {
        let base = format!("/channels/{channel}/messages/{message}/reactions");
        match (emoji, user_id) {
            (None, _) => {
                self.call("DELETE", &base, None).map_err(refusal)?;
                if let Ok(Some(mut stored)) = self.ctx.stored_message(channel, message) {
                    if !stored.reactions.is_empty() {
                        stored.reactions.clear();
                        let _ = self.ctx.message(channel, &stored);
                    }
                }
                Ok(())
            }
            (Some(emoji), None) => {
                let named = self.reaction_emoji(channel, message, emoji)?;
                self.call("DELETE", &format!("{base}/{}", escaped(&named)), None).map_err(refusal)?;
                if let Ok(Some(mut stored)) = self.ctx.stored_message(channel, message) {
                    let before = stored.reactions.len();
                    stored.reactions.retain(|r| {
                        r.emoji != emoji && !r.file.as_ref().is_some_and(|f| f.id == emoji)
                    });
                    if stored.reactions.len() != before {
                        let _ = self.ctx.message(channel, &stored);
                    }
                }
                Ok(())
            }
            (Some(emoji), Some(user)) => {
                let named = self.reaction_emoji(channel, message, emoji)?;
                self.call("DELETE", &format!("{base}/{}/{}", escaped(&named), escaped(user)), None)
                    .map(drop)
                    .map_err(refusal)
            }
        }
    }

    /// Fetches one page of users who reacted to `message` with `emoji`.
    fn reactors(
        &self,
        channel: &str,
        message: &str,
        emoji: &str,
        burst: bool,
        after: Option<&str>,
        limit: u32,
    ) -> Result<super::ReactorsPage, Refusal> {
        let named = self.reaction_emoji(channel, message, emoji)?;
        let limit = if limit == 0 { 100 } else { limit.clamp(1, 100) };
        let rtype = if burst { 1 } else { 0 };
        let mut path = format!(
            "/channels/{channel}/messages/{message}/reactions/{}?limit={limit}&type={rtype}",
            escaped(&named)
        );
        if let Some(after) = after.filter(|a| !a.is_empty()) {
            path.push_str(&format!("&after={}", escaped(after)));
        }
        let body = self.call("GET", &path, None).map_err(refusal)?;
        let users: Vec<User> = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let more = users.len() as u32 >= limit;
        let mut files = Vec::with_capacity(users.len());
        let mut reactors = Vec::with_capacity(users.len());
        let known_names = {
            let state = crate::hub::lock(&self.state);
            users
                .iter()
                .map(|u| (u.id.clone(), state.users.get(&u.id).cloned()))
                .collect::<HashMap<_, _>>()
        };
        for user in users {
            let (avatar, url) = user.avatar();
            let name = known_names
                .get(&user.id)
                .and_then(|n| n.as_deref())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| user.name())
                .to_owned();
            files.push((avatar.clone(), url));
            reactors.push(protocol::Reactor {
                key: self.ctx.key(&user.id),
                name,
                avatar: Some(avatar),
            });
        }
        let _ = self.ctx.media(&files);
        Ok(super::ReactorsPage { reactors, more })
    }

    /// Edits one of our messages, suppresses its embeds (preserving existing flag bits), or
    /// removes attachments by listing the kept attachment ids. The store changes with Discord's
    /// answer.
    fn edit(
        &self,
        channel: &str,
        message: &str,
        text: &str,
        suppress_embeds: bool,
        keep_attachments: Option<&[String]>,
    ) -> Result<(), Refusal> {
        let suppress_only = suppress_embeds && text.is_empty() && keep_attachments.is_none();
        let mut body = serde_json::Map::new();
        if !suppress_only {
            let premium = self.state.lock().unwrap().premium;
            if let Some(why) = too_long(premium, text) {
                return Err(Refusal::Refused(why));
            }
            body.insert("content".to_owned(), json!(text));
        }
        if suppress_embeds {
            // Read current flags before patching so existing bits (e.g. IS_VOICE_MESSAGE,
            // CROSSPOSTED, SUPPRESS_NOTIFICATIONS) are preserved rather than cleared.
            // A miss refuses: inventing 0 and sending SUPPRESS_EMBEDS alone would clear them.
            let existing = self.message_flags(channel, message)?;
            body.insert("flags".to_owned(), json!(existing | model::SUPPRESS_EMBEDS));
        }
        if let Some(keep) = keep_attachments {
            let kept: Vec<Value> = keep
                .iter()
                .map(|id| json!({"id": id.strip_prefix("att:").unwrap_or(id)}))
                .collect();
            body.insert("attachments".to_owned(), Value::Array(kept));
        }
        let path = format!("/channels/{channel}/messages/{message}");
        let edited = self.call("PATCH", &path, Some(Value::Object(body))).map_err(refusal)?;
        let unreadable = |err: String| Refusal::Refused(format!("Discord's answer was unreadable: {err}"));
        let mut posted: Posted = serde_json::from_str(&edited).map_err(|err| unreadable(err.to_string()))?;
        if posted.channel_id.is_empty() {
            posted.channel_id = channel.to_owned();
        }
        self.updated(posted).map_err(|failed| match failed {
            Failure::SignedOut(why) | Failure::Dropped(why) => unreadable(why),
        })
    }

    /// Reads the current `flags` bitmask of one message so PATCH merges rather than overwrites it.
    /// The single-message route is required: a list that does not contain the id is a miss, and
    /// a miss must not be treated as flags 0.
    fn message_flags(&self, channel: &str, message: &str) -> Result<u64, Refusal> {
        let path = format!("/channels/{channel}/messages/{message}");
        let body = self.call("GET", &path, None).map_err(refusal)?;
        let posted: Posted = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        if posted.id != message {
            return Err(Refusal::Refused("That message is gone.".to_owned()));
        }
        posted
            .flags
            .ok_or_else(|| Refusal::Refused("Discord's answer was unreadable: missing flags".to_owned()))
    }

    /// Fetches the raw markdown source of a single message for lossless editing.
    fn source(&self, channel: &str, message: &str) -> Result<String, Refusal> {
        let path = format!("/channels/{channel}/messages?around={message}&limit=1");
        let body = self.call("GET", &path, None).map_err(refusal)?;
        let found: Vec<Posted> =
            serde_json::from_str(&body).map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let posted = found.into_iter().find(|p| p.id == message).ok_or_else(|| Refusal::Refused("That message is gone.".to_owned()))?;
        Ok(posted.content.unwrap_or_default())
    }

    fn delete(&self, channel: &str, message: &str) -> Result<(), Refusal> {
        let ephemeral = self.ctx.stored_message(channel, message).ok().flatten().is_some_and(|stored| stored.ephemeral);
        let removed = self.call("DELETE", &format!("/channels/{channel}/messages/{message}"), None).map_err(refusal);
        // Dismiss always drops an only-you reply. Discord may refuse the DELETE.
        if ephemeral {
            self.ctx.deleted(channel, message).map_err(|err| Refusal::Refused(err.to_string()))?;
            return Ok(());
        }
        removed?;
        self.ctx.deleted(channel, message).map_err(|err| Refusal::Refused(err.to_string()))
    }

    fn pin(&self, channel: &str, message: &str, pinned: bool) -> Result<(), Refusal> {
        let method = if pinned { "PUT" } else { "DELETE" };
        // The route the pins are read from; a server that does not have it yet takes the older one.
        match self.call(method, &format!("/channels/{channel}/messages/pins/{message}"), None) {
            Err(Refused::Status { status: 404, .. }) => self.call(method, &format!("/channels/{channel}/pins/{message}"), None).map_err(refusal)?,
            other => other.map_err(refusal)?,
        };
        self.ctx.pinned(channel, message, pinned).map_err(|err| Refusal::Refused(err.to_string()))
    }

    fn forward(
        &self,
        from: &str,
        message_ids: &[String],
        targets: &[String],
        note: &str,
    ) -> Result<Vec<(String, Result<(), String>)>, Refusal> {
        let mut results = Vec::with_capacity(targets.len());
        let note = note.trim();
        let from_guild = self.ctx.known(from).ok().flatten().and_then(|c| self.guild_of(&c));
        for target in targets {
            if self.ctx.known(target).ok().flatten().is_some_and(|c| c.read_only) {
                results.push((target.clone(), Err("This conversation is read-only.".to_owned())));
                continue;
            }
            let mut err: Option<String> = None;
            for mid in message_ids {
                let body = forward_message_body(from, mid, from_guild.as_deref());
                if let Err(e) = self.call("POST", &format!("/channels/{target}/messages"), Some(body)) {
                    err = Some(match refusal(e) {
                        Refusal::Refused(why) => why,
                        Refusal::Unsupported => "Forwarding is not supported.".to_owned(),
                    });
                    break;
                }
            }
            if err.is_none() && !note.is_empty() {
                let body = serde_json::json!({ "content": note });
                if let Err(e) = self.call("POST", &format!("/channels/{target}/messages"), Some(body)) {
                    err = Some(match refusal(e) {
                        Refusal::Refused(why) => why,
                        Refusal::Unsupported => "Could not send note.".to_owned(),
                    });
                }
            }
            results.push((target.clone(), err.map_or(Ok(()), Err)));
        }
        Ok(results)
    }

    /// Files one message with Discord Trust & Safety. One try: a 429 or a captcha is the answer,
    /// not a reason to ask again. A person or a server is left to Discord's own app.
    fn file_report(&self, target: &protocol::ReportTarget, reason: protocol::ReportReason) -> Result<(), Refusal> {
        let protocol::ReportTarget::Message { conversation, message_id } = target else {
            return Err(Refusal::Unsupported);
        };
        let Some((variant, breadcrumbs)) = message_report(reason) else {
            return Err(Refusal::Refused(
                "Discord has no general report for this. Pick spam, harassment, violence, illegal content, or impersonation.".into(),
            ));
        };
        let body = json!({
            "version": "1.0",
            "variant": variant,
            "language": "en",
            "breadcrumbs": breadcrumbs,
            "elements": {},
            "channel_id": conversation.id,
            "message_id": message_id,
            "name": "message",
        });
        self.rest.call_once_with("POST", "/reporting/message", Some(body), &[]).map(drop).map_err(refusal)
    }
    /// Publishes (crossposts) a message in an announcement channel (`POST /channels/{id}/messages/{msg}/crosspost`).
    /// A message the store already has as published is left alone. A body that is not a message
    /// is the same unreadable fault as any other Discord answer we cannot apply.
    fn publish(&self, channel: &str, message: &str) -> Result<(), Refusal> {
        if self.ctx.stored_message(channel, message).ok().flatten().is_some_and(|stored| stored.published) {
            return Ok(());
        }
        let raw = self
            .call("POST", &format!("/channels/{channel}/messages/{message}/crosspost"), None)
            .map_err(refusal)?;
        let posted: Posted = serde_json::from_str(&raw)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        if posted.id != message || posted.channel_id != channel {
            return Err(Refusal::Refused("Discord's answer was unreadable: not a message".to_owned()));
        }
        self.updated(posted).map_err(|failed| match failed {
            Failure::SignedOut(why) | Failure::Dropped(why) => {
                Refusal::Refused(format!("Discord's answer was unreadable: {why}"))
            }
        })
    }

    /// Follows an announcement channel into `into_channel_id` (`POST /channels/{id}/followers`).
    fn follow_announcement(&self, channel: &str, into_channel_id: &str) -> Result<(), Refusal> {
        self.call(
            "POST",
            &format!("/channels/{channel}/followers"),
            Some(json!({ "webhook_channel_id": into_channel_id })),
        )
        .map(drop)
        .map_err(refusal)
    }

    /// Sends or answers a friend request, withdraws or ends a friendship, or blocks. One request:
    /// managing friends is what Discord's anti-abuse system watches most closely.
    ///
    /// Accepts or declines/ignores a message request.
    fn answer_message_request(&self, channel: &str, accept: bool) -> Result<(), Refusal> {
        if accept {
            self.call("PUT", &format!("/channels/{channel}/recipients/@me"), Some(json!({"consent_status": 2})))
                .map_err(refusal)?;
            if let Ok(Some(known)) = self.ctx.known(channel) {
                let mut row = renamed(&known, &known.title);
                row.special = protocol::Special::None;
                row.spam = false;
                self.ctx.conversation(&row).map_err(|err| Refusal::Refused(err.to_string()))?;
            }
            Ok(())
        } else {
            self.call("DELETE", &format!("/channels/{channel}"), None).map_err(refusal)?;
            {
                let mut state = crate::hub::lock(&self.state);
                state.privates.remove(channel);
                state.directs.retain(|_, id| id != channel);
                state.covered.remove(channel);
            }
            self.ctx.conversation_gone(channel).map_err(|err| Refusal::Refused(err.to_string()))
        }
    }

    /// The bodies and the context header are the web client's as the community documents them,
    /// unconfirmed against a real account.
    fn relate(&self, person: &str, action: RelateAction) -> Result<(), Refusal> {
        if action == RelateAction::Add {
            let state = crate::hub::lock(&self.state);
            if person == state.me || state.relations.get(person).is_some_and(|relation| *relation != Relation::None) {
                return Err(Refusal::Refused("A friend request cannot be sent to this person.".into()));
            }
        }
        let context = |location: &str| base64::engine::general_purpose::STANDARD.encode(json!({"location": location}).to_string());
        let (method, path, body, location, becomes) = match action {
            RelateAction::Add => ("PUT", format!("/users/@me/relationships/{person}"), Some(json!({})), "User Profile", Relation::Outgoing),
            RelateAction::Accept => ("PUT", format!("/users/@me/relationships/{person}"), Some(json!({})), "Friends", Relation::Friend),
            RelateAction::Block => ("PUT", format!("/users/@me/relationships/{person}"), Some(json!({"type": 2})), "ContextMenu", Relation::Blocked),
            RelateAction::Unblock => ("DELETE", format!("/users/@me/relationships/{person}"), None, "Blocked", Relation::None),
            RelateAction::IgnoreUser => ("PUT", format!("/users/@me/relationships/{person}/ignore"), None, "ContextMenu", Relation::Ignored),
            RelateAction::UnignoreUser => ("DELETE", format!("/users/@me/relationships/{person}/ignore"), None, "ContextMenu", Relation::None),
            RelateAction::Ignore | RelateAction::Cancel | RelateAction::Remove => {
                ("DELETE", format!("/users/@me/relationships/{person}"), None, "Friends", Relation::None)
            }
        };
        let context = context(location);
        self.rest.call_with(method, &path, body, &[("X-Context-Properties", &context)]).map_err(refusal)?;
        {
            let mut state = crate::hub::lock(&self.state);
            match becomes {
                Relation::None => {
                    state.relations.remove(person);
                    state.presence.remove(person);
                }
                becomes => drop(state.relations.insert(person.to_owned(), becomes)),
            }
        }
        self.ctx.relation(person, becomes).map_err(|err| Refusal::Refused(err.to_string()))
    }

    /// Sends a friend request by username (`POST /users/@me/relationships`). Never retried
    /// automatically; Discord's anti-abuse system watches friend requests closely.
    fn add_friend(&self, raw: &str) -> Result<(), Refusal> {
        let (username, discriminator) = parse_discord_username(raw).map_err(Refusal::Refused)?;
        let body = match discriminator {
            Some(disc) => json!({ "username": username, "discriminator": disc }),
            None => json!({ "username": username, "discriminator": Value::Null }),
        };
        let context = base64::engine::general_purpose::STANDARD.encode(json!({"location": "Add Friend"}).to_string());
        self.rest
            .call_once_with("POST", "/users/@me/relationships", Some(body), &[("X-Context-Properties", &context)])
            .map(drop)
            .map_err(add_friend_refusal)
    }

    /// Sets or clears a private nickname for a friend (`PATCH /users/@me/relationships/{id}`).
    /// Only allowed when the person is currently a friend.
    fn set_friend_nickname(&self, person: &str, nickname: Option<String>) -> Result<(), Refusal> {
        let is_friend = crate::hub::lock(&self.state).relations.get(person) == Some(&Relation::Friend);
        if !is_friend {
            return Err(Refusal::Refused("Only friends can be given a nickname.".to_owned()));
        }
        let nick = nickname.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        if nick.as_deref().is_some_and(|n| n.chars().count() > 32) {
            return Err(Refusal::Refused("Friend nickname must be 32 characters or fewer.".to_owned()));
        }
        let body = json!({ "nickname": nick });
        self.call("PATCH", &format!("/users/@me/relationships/{person}"), Some(body)).map_err(refusal)?;
        if let Some(nick) = nick {
            let updated = {
                let mut state = crate::hub::lock(&self.state);
                state.users.insert(person.to_owned(), nick.clone());
                state.rows.get_mut(person).map(|row| {
                    row.name = nick;
                    row.clone()
                })
            };
            if let Some(row) = updated {
                let _ = self.ctx.people(&[row]);
            }
        }
        Ok(())
    }

    /// Previews a Discord server invite (`GET /invites/{code}?with_counts=true&with_expiration=true`),
    /// optionally including a scheduled event (`&guild_scheduled_event_id=...`). Session-cached
    /// per code and paced so scrolling never bursts Discord.
    fn invite_preview(&self, raw: &str, event_id: Option<&str>) -> Result<protocol::InvitePreview, Refusal> {
        let code = parse_discord_invite_code(raw).map_err(Refusal::Refused)?;
        let query_event = raw
            .split_once('?')
            .and_then(|(_, q)| q.split('#').next())
            .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("event=")))
            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
        let event_id = event_id.filter(|id| !id.is_empty()).or(query_event);
        let cache_key = match event_id {
            Some(ev) => format!("{code}:{ev}"),
            None => code.clone(),
        };
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(cached) = state.invite_cache.get(&cache_key).cloned() {
                return match cached {
                    Ok(mut preview) => {
                        if let Some(gid) = preview.space_id.as_deref() {
                            preview.joined = state.guilds.contains_key(gid);
                        }
                        Ok(preview)
                    }
                    Err(msg) => Err(Refusal::Refused(msg)),
                };
            }
            let now = Instant::now();
            while state.invite_recent.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60)) {
                state.invite_recent.pop_front();
            }
            if state.invite_recent.len() >= 12 {
                return Err(Refusal::Refused("Too many invite previews right now. Try again in a moment.".to_owned()));
            }
            state.invite_recent.push_back(now);
        }
        let mut path = format!("/invites/{}?with_counts=true&with_expiration=true", escaped(&code));
        if let Some(ev) = event_id {
            path.push_str(&format!("&guild_scheduled_event_id={}", escaped(ev)));
        }
        let body = match self.call("GET", &path, None) {
            Ok(body) => body,
            Err(err) => {
                let refused = invite_refusal(err);
                if let Refusal::Refused(ref msg) = refused {
                    if msg.contains("invalid or has expired") {
                        crate::hub::lock(&self.state).invite_cache.insert(cache_key, Err(msg.clone()));
                    }
                }
                return Err(refused);
            }
        };
        let parsed: Value = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let guild = parsed.get("guild").filter(|v| v.is_object());
        let space_id = guild
            .and_then(|g| g.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let name = guild
            .and_then(|g| g.get("name"))
            .and_then(Value::as_str)
            .or_else(|| parsed.get("channel").and_then(|c| c.get("name")).and_then(Value::as_str))
            .unwrap_or(&code)
            .to_owned();
        let description = guild
            .and_then(|g| g.get("description"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let icon = space_id
            .as_deref()
            .zip(guild.and_then(|g| g.get("icon")).and_then(Value::as_str))
            .and_then(|(gid, hash)| {
                let (file, url) = model::guild_icon(gid, hash)?;
                let _ = self.ctx.media(&[(file.clone(), url)]);
                Some(file)
            });
        let channel_id = parsed
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let event = parsed
            .get("guild_scheduled_event")
            .filter(|v| v.is_object())
            .and_then(|ev| parse_scheduled_event(ev, space_id.as_deref()));
        let joined = {
            let state = crate::hub::lock(&self.state);
            space_id.as_deref().is_some_and(|gid| state.guilds.contains_key(gid))
        };
        let preview = protocol::InvitePreview {
            code: parsed.get("code").and_then(Value::as_str).unwrap_or(&code).to_owned(),
            name,
            description,
            icon,
            members: parsed.get("approximate_member_count").and_then(Value::as_u64).map(|n| n as u32),
            online: parsed.get("approximate_presence_count").and_then(Value::as_u64).map(|n| n as u32),
            space_id,
            channel_id,
            request_needed: false,
            joined,
            event,
        };
        crate::hub::lock(&self.state).invite_cache.insert(cache_key, Ok(preview.clone()));
        Ok(preview)
    }

    /// Joins a Discord server from an invite code (`POST /invites/{code}`). Never retries on
    /// captcha or refusal; `GUILD_CREATE` on the gateway plants the server tree.
    fn invite_join(&self, raw: &str) -> Result<(Option<String>, Option<String>, bool), Refusal> {
        let code = parse_discord_invite_code(raw).map_err(Refusal::Refused)?;
        let context = base64::engine::general_purpose::STANDARD.encode(json!({"location": "Join Guild"}).to_string());
        let path = format!("/invites/{}", escaped(&code));
        let body = self
            .rest
            .call_once_with("POST", &path, Some(json!({})), &[("X-Context-Properties", &context)])
            .map_err(invite_refusal)?;
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let space_id = parsed
            .get("guild")
            .and_then(|g| g.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let channel_id = parsed
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        {
            let mut state = crate::hub::lock(&self.state);
            for (k, v) in &mut state.invite_cache {
                if k == &code || k.starts_with(&format!("{code}:")) {
                    if let Ok(preview) = v {
                        preview.joined = true;
                    }
                }
            }
        }
        Ok((space_id, channel_id, false))
    }

    /// Creates a `discord.gg` invite link for a channel (`POST /channels/{id}/invites`), a
    /// server (first channel allowing `CREATE_INSTANT_INVITE`), or a friend invite
    /// (`POST /users/@me/invites`). One-shot per click; permissions are checked locally first.
    fn create_invite(
        &self,
        conversation_id: Option<&str>,
        space_id: Option<&str>,
        max_age: u32,
        max_uses: u32,
        temporary: bool,
    ) -> Result<String, Refusal> {
        let (path, body) = match (conversation_id, space_id) {
            (Some(channel), _) => {
                let known = self
                    .ctx
                    .known(channel)
                    .map_err(|err| Refusal::Refused(err.to_string()))?
                    .ok_or_else(|| Refusal::Refused("This conversation is gone.".to_owned()))?;
                if let Some(guild) = self.guild_of(&known) {
                    if !self.can_invite_channel(&guild, channel) {
                        return Err(Refusal::Refused(
                            "You do not have permission to create an invite for this channel.".to_owned(),
                        ));
                    }
                } else if known.kind != ConversationKind::Group {
                    return Err(Refusal::Refused("Only server channels or group DMs can create an invite.".to_owned()));
                }
                (
                    format!("/channels/{channel}/invites"),
                    json!({
                        "max_age": max_age,
                        "max_uses": max_uses,
                        "temporary": temporary,
                        "flags": 0,
                    }),
                )
            }
            (None, Some(guild)) => {
                let Some(channel) = self.invite_channel(guild) else {
                    return Err(Refusal::Refused(
                        "You do not have permission to create an invite for this server.".to_owned(),
                    ));
                };
                (
                    format!("/channels/{channel}/invites"),
                    json!({
                        "max_age": max_age,
                        "max_uses": max_uses,
                        "temporary": temporary,
                        "flags": 0,
                    }),
                )
            }
            (None, None) => ("/users/@me/invites".to_owned(), json!({})),
        };
        let answer = self.call_once("POST", &path, Some(body)).map_err(invite_refusal)?;
        let parsed: Value = serde_json::from_str(&answer)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let code = parsed
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| model::cdn_token(code))
            .ok_or_else(|| Refusal::Refused("Discord did not return an invite code.".to_owned()))?;
        Ok(format!("https://discord.gg/{code}"))
    }

    /// Resolves a user-supplied status emoji string into `(emoji_id, emoji_name, animated)`.
    /// Accepts Unicode glyphs (e.g. `"📅"`), custom Discord formats (`"<a:ferris:7001>"`,
    /// `"<:ferris:7001>"`, `"ferris:7001"`), or a server emoji shortcode (`":ferris:"`).
    fn resolve_status_emoji(state: &State, raw: &str) -> (Option<u64>, String, bool) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return (None, String::new(), false);
        }
        // `<a:name:id>` or `<:name:id>`
        if let Some(inner) = trimmed.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
            let parts: Vec<&str> = inner.split(':').collect();
            if parts.len() == 3 {
                if let Ok(id) = parts[2].parse::<u64>() {
                    return (Some(id), parts[1].to_owned(), parts[0] == "a");
                }
            }
        }
        // `name:id`
        if let Some((name, id_str)) = trimmed.split_once(':') {
            if !name.is_empty() && !id_str.is_empty() {
                if let Ok(id) = id_str.parse::<u64>() {
                    let animated = state
                        .guilds
                        .values()
                        .flat_map(|g| g.emojis.iter())
                        .find(|e| e.id == id_str)
                        .is_some_and(|e| e.animated);
                    return (Some(id), name.to_owned(), animated);
                }
            }
        }
        // `:shortcode:` lookup in guild custom emojis
        if let Some(code) = trimmed.strip_prefix(':').and_then(|s| s.strip_suffix(':')) {
            if let Some(found) = state.guilds.values().flat_map(|g| g.emojis.iter()).find(|e| e.name == code) {
                if let Ok(id) = found.id.parse::<u64>() {
                    return (Some(id), found.name.clone(), found.animated);
                }
            }
        }
        // A re-save publishes the bare name. Keep the id we already resolved for it.
        if state.own_emoji_id.is_some() && trimmed == state.own_emoji {
            let animated = state
                .own_emoji_id
                .and_then(|id| {
                    let sid = id.to_string();
                    state
                        .guilds
                        .values()
                        .flat_map(|g| g.emojis.iter())
                        .find(|e| e.id == sid)
                        .map(|e| e.animated)
                })
                .unwrap_or(false);
            return (state.own_emoji_id, trimmed.to_owned(), animated);
        }
        // A bare custom-emoji name, the form the status line shows.
        if let Some(found) = state.guilds.values().flat_map(|g| g.emojis.iter()).find(|e| e.name == trimmed) {
            if let Ok(id) = found.id.parse::<u64>() {
                return (Some(id), found.name.clone(), found.animated);
            }
        }
        (None, trimmed.to_owned(), false)
    }

    /// Resolves a Discord scheduled event (`GET /guilds/{guild}/scheduled-events/{event}?with_user_count=true`).
    fn event_preview(&self, guild_id: &str, event_id: &str) -> Result<protocol::ScheduledEvent, Refusal> {
        let key = (guild_id.to_owned(), event_id.to_owned());
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(cached) = state.event_cache.get(&key).cloned() {
                return cached.map_err(Refusal::Refused);
            }
            let now = Instant::now();
            while state.invite_recent.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60)) {
                state.invite_recent.pop_front();
            }
            if state.invite_recent.len() >= 12 {
                return Err(Refusal::Refused("Too many event lookups right now. Try again in a moment.".to_owned()));
            }
            state.invite_recent.push_back(now);
        }
        let path = format!(
            "/guilds/{}/scheduled-events/{}?with_user_count=true",
            escaped(guild_id),
            escaped(event_id)
        );
        let body = match self.call("GET", &path, None) {
            Ok(body) => body,
            Err(err) => {
                let refused = invite_refusal(err);
                if let Refusal::Refused(ref msg) = refused {
                    if msg.contains("invalid or has expired") {
                        crate::hub::lock(&self.state).event_cache.insert(key, Err(msg.clone()));
                    }
                }
                return Err(refused);
            }
        };
        let parsed: Value = serde_json::from_str(&body)
            .map_err(|err| Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let event = parse_scheduled_event(&parsed, Some(guild_id))
            .ok_or_else(|| Refusal::Refused("Discord's event response was incomplete.".to_owned()))?;
        crate::hub::lock(&self.state).event_cache.insert(key, Ok(event.clone()));
        Ok(event)
    }

    /// Marks or unmarks ourselves as interested in a Discord scheduled event
    /// (`PUT` / `DELETE /guilds/{guild}/scheduled-events/{event}/users/@me`).
    fn event_link_interest(&self, guild_id: &str, event_id: &str, interested: bool) -> Result<(), Refusal> {
        let method = if interested { "PUT" } else { "DELETE" };
        let path = format!(
            "/guilds/{}/scheduled-events/{}/users/@me",
            escaped(guild_id),
            escaped(event_id)
        );
        self.call(method, &path, None).map_err(refusal)?;
        let mut state = crate::hub::lock(&self.state);
        if let Some(Ok(ev)) = state.event_cache.get_mut(&(guild_id.to_owned(), event_id.to_owned())) {
            if ev.interested != interested {
                ev.interested = interested;
                if let Some(count) = ev.interested_count.as_mut() {
                    *count = if interested { count.saturating_add(1) } else { count.saturating_sub(1) };
                }
            }
        }
        for cached in state.invite_cache.values_mut() {
            if let Ok(preview) = cached {
                if let Some(ev) = preview.event.as_mut().filter(|e| e.id == event_id) {
                    if ev.interested != interested {
                        ev.interested = interested;
                        if let Some(count) = ev.interested_count.as_mut() {
                            *count = if interested { count.saturating_add(1) } else { count.saturating_sub(1) };
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Public servers (`GET /discoverable-guilds`, or `/search` when there is a query).
    /// A URL in the listing is never requested and never stored as a join target.
    fn discover(&self, query: &str, category: u32, offset: u32, limit: u32) -> Result<(Vec<protocol::DiscoveredGuild>, u32), Refusal> {
        let query: String = query.trim().chars().take(100).collect();
        let limit = if limit == 0 { 24 } else { limit.min(48) };
        let path = if query.is_empty() {
            let mut path = format!("/discoverable-guilds?limit={limit}&offset={offset}");
            if category != 0 {
                path.push_str(&format!("&categories={category}"));
            }
            path
        } else {
            let mut path = format!("/discoverable-guilds/search?query={}&limit={limit}&offset={offset}", escaped(&query));
            if category != 0 {
                path.push_str(&format!("&category_id={category}"));
            }
            path
        };
        let body = self.call("GET", &path, None).map_err(refusal)?;
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let total = parsed.get("total").and_then(Value::as_u64).unwrap_or(0).min(u64::from(u32::MAX)) as u32;
        let mut guilds = Vec::new();
        let mut vanity = Vec::new();
        for row in parsed.get("guilds").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = row.get("id").and_then(Value::as_str).filter(|id| discovery_snowflake(id)) else {
                continue;
            };
            let Some(name) = row.get("name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()) else {
                continue;
            };
            // The route is already the public list. A features list that names something else is not.
            let listed = row.get("features").and_then(Value::as_array);
            if listed.is_some_and(|list| !list.is_empty() && !list.iter().any(|flag| flag.as_str() == Some("DISCOVERABLE"))) {
                continue;
            }
            let icon = row.get("icon").and_then(Value::as_str).filter(|hash| icon_hash(hash)).unwrap_or("").to_owned();
            let description: String = row.get("description").and_then(Value::as_str).unwrap_or("").chars().take(300).collect();
            if let Some(code) = row.get("vanity_url_code").and_then(Value::as_str).filter(|code| discovery_code(code)) {
                vanity.push((id.to_owned(), code.to_owned()));
            }
            let count = |key: &str| row.get(key).and_then(Value::as_u64).unwrap_or(0).min(u64::from(u32::MAX)) as u32;
            guilds.push(protocol::DiscoveredGuild {
                id: id.to_owned(),
                name: name.to_owned(),
                description,
                icon,
                members: count("approximate_member_count"),
                online: count("approximate_presence_count"),
            });
        }
        if !vanity.is_empty() {
            let mut state = crate::hub::lock(&self.state);
            if state.discovered_vanity.len() > 128 {
                state.discovered_vanity.clear();
            }
            for (id, code) in vanity {
                state.discovered_vanity.insert(id, code);
            }
        }
        Ok((guilds, total))
    }

    /// Joins a public server by its id (`PUT /guilds/{id}/members/@me`). A vanity code stored
    /// from that listing is the only fallback, and only when it is a code.
    fn join_discovered(&self, guild_id: &str) -> Result<(Option<String>, Option<String>), Refusal> {
        if !discovery_snowflake(guild_id) {
            return Err(Refusal::Refused("That is not a server you can join from Discovery.".to_owned()));
        }
        let vanity = crate::hub::lock(&self.state).discovered_vanity.get(guild_id).cloned().filter(|code| discovery_code(code));
        let path = format!("/guilds/{guild_id}/members/@me");
        match self.rest.call_once_with("PUT", &path, Some(json!({})), &[]) {
            Ok(_) => Ok((Some(guild_id.to_owned()), None)),
            Err(err) => match vanity {
                Some(code) => self.invite_join(&code).map(|(space, channel, _)| (space, channel)),
                None => Err(refusal(err)),
            },
        }
    }

    /// The rules form and onboarding questions for one server. Phone and email, when we know
    /// they are missing, point at Discord's own app. Nothing in the JSON is opened or fetched.
    fn space_gate(&self, guild_id: &str) -> Result<protocol::SpaceGate, Refusal> {
        if !discovery_snowflake(guild_id) {
            return Err(Refusal::Refused("That is not a server.".to_owned()));
        }
        let (mut gate, rules, onboarding) = self.gate_snapshot(guild_id)?;
        if rules {
            if let Ok(form) = self.get_json(&format!("/guilds/{guild_id}/member-verification")) {
                fill_form(&mut gate, &form);
            }
        }
        if onboarding {
            if let Ok(board) = self.get_json(&format!("/guilds/{guild_id}/onboarding")) {
                fill_prompts(&mut gate, &board);
            }
        }
        Ok(gate)
    }

    /// Accepts the rules. The form is read again and sent back whole, with this member's answers
    /// filled in. A required check Discord has to do itself is not submitted.
    fn accept_rules(&self, guild_id: &str, answers: &[String]) -> Result<protocol::SpaceGate, Refusal> {
        if !discovery_snowflake(guild_id) {
            return Err(Refusal::Refused("That is not a server.".to_owned()));
        }
        let form = self.get_json(&format!("/guilds/{guild_id}/member-verification"))?;
        let fields = form.get("form_fields").and_then(Value::as_array).cloned().unwrap_or_default();
        if fields.is_empty() {
            return Err(Refusal::Refused("This server has no rules to accept.".to_owned()));
        }
        let mut filled = Vec::with_capacity(fields.len());
        for (index, mut field) in fields.into_iter().enumerate() {
            if !field.is_object() {
                filled.push(field);
                continue;
            }
            let kind = field.get("field_type").and_then(Value::as_str).unwrap_or("").to_owned();
            let required = field.get("required").and_then(Value::as_bool).unwrap_or(false);
            let answer = answers.get(index).map(String::as_str).unwrap_or("");
            match kind.as_str() {
                "TERMS" => {
                    let yes = matches!(answer.to_ascii_lowercase().as_str(), "true" | "yes" | "1");
                    if required && !yes {
                        return Err(Refusal::Refused("Accept the rules to continue.".to_owned()));
                    }
                    field["response"] = json!(yes);
                }
                "MULTIPLE_CHOICE" => {
                    if answer.is_empty() {
                        if required {
                            return Err(Refusal::Refused("Pick one of the choices.".to_owned()));
                        }
                    } else {
                        let choice: u64 = answer.parse().map_err(|_| Refusal::Refused("Pick one of the choices.".to_owned()))?;
                        field["response"] = json!(choice);
                    }
                }
                "TEXT_INPUT" | "PARAGRAPH" => {
                    let text: String = answer.chars().take(1000).collect();
                    if required && text.trim().is_empty() {
                        return Err(Refusal::Refused("Fill in every required answer.".to_owned()));
                    }
                    if !text.is_empty() {
                        field["response"] = json!(text);
                    }
                }
                "VERIFICATION" if required => {
                    return Err(Refusal::Refused("That check can only be done in Discord.".to_owned()));
                }
                _ if required => {
                    return Err(Refusal::Refused("This server asks for something that can only be finished in Discord.".to_owned()));
                }
                _ => {}
            }
            filled.push(field);
        }
        let version = form.get("version").cloned().unwrap_or(Value::Null);
        let body = json!({"form_fields": filled, "version": version});
        let text = self
            .rest
            .call_once_with("PUT", &format!("/guilds/{guild_id}/requests/@me"), Some(body), &[])
            .map_err(refusal)?;
        let parsed = parse_body(&text);
        if accept_cleared(&parsed) {
            {
                let mut state = crate::hub::lock(&self.state);
                if let Some(info) = state.guilds.get_mut(guild_id) {
                    info.pending = false;
                }
            }
            let _ = self.replant(guild_id);
        }
        let (mut gate, _, _) = self.gate_snapshot(guild_id)?;
        fill_form(&mut gate, &form);
        Ok(gate)
    }

    /// Sends onboarding answers. Option ids that were not in the questions are dropped.
    /// Discord assigns any roles itself; this does not.
    fn submit_onboarding(&self, guild_id: &str, options: &[String]) -> Result<(), Refusal> {
        if !discovery_snowflake(guild_id) {
            return Err(Refusal::Refused("That is not a server.".to_owned()));
        }
        let onboarding = crate::hub::lock(&self.state).guilds.get(guild_id).is_some_and(|info| info.onboarding);
        if !onboarding {
            return Err(Refusal::Refused("This server has no setup questions.".to_owned()));
        }
        let board = self.get_json(&format!("/guilds/{guild_id}/onboarding"))?;
        let allowed = prompt_option_ids(&board);
        let mut picked = Vec::new();
        for id in options {
            if discovery_snowflake(id) && allowed.contains(id) && !picked.iter().any(|kept: &String| kept == id) {
                picked.push(id.to_owned());
            }
            if picked.len() == 750 {
                break;
            }
        }
        if !options.is_empty() && picked.is_empty() {
            return Err(Refusal::Refused("Those answers are not part of this server's questions.".to_owned()));
        }
        let (prompts_seen, responses_seen) = if options.is_empty() {
            (serde_json::Map::new(), serde_json::Map::new())
        } else {
            seen_maps(&board, &picked)
        };
        let body = json!({
            "onboarding_responses": picked,
            "onboarding_prompts_seen": prompts_seen,
            "onboarding_responses_seen": responses_seen,
        });
        self.rest
            .call_once_with("POST", &format!("/guilds/{guild_id}/onboarding-responses"), Some(body), &[])
            .map_err(refusal)?;
        Ok(())
    }

    fn gate_snapshot(&self, guild_id: &str) -> Result<(protocol::SpaceGate, bool, bool), Refusal> {
        let state = crate::hub::lock(&self.state);
        let me = state.me.clone();
        let Some(info) = state.guilds.get(guild_id) else {
            return Err(Refusal::Refused("You are not in that server.".to_owned()));
        };
        let reason = info.gate_reason(crate::hub::now(), &me);
        let mut gate = protocol::SpaceGate {
            pending: info.pending,
            verification: info.verification.unwrap_or(0),
            manual: info.manual,
            onboarding: info.onboarding,
            reason: reason.clone(),
            ..protocol::SpaceGate::default()
        };
        if reason.contains("verified phone") || reason.contains("verified email") {
            gate.external = "https://discord.com/app".to_owned();
        }
        Ok((gate, info.pending || info.rules, info.onboarding))
    }

    fn get_json(&self, path: &str) -> Result<Value, Refusal> {
        let text = self.call("GET", path, None).map_err(refusal)?;
        Ok(parse_body(&text))
    }

    /// Our own presence, and custom status (text, emoji, clear-after) unless all three are `None`.
    /// Both are settings: the web client saves them (protobuf, `settings-proto/1`) and tells the
    /// gateway (op 3), and so does this.
    ///
    /// Unverified against a real account: the field numbers of the status settings are the
    /// community's (`proto.rs`), and the whole status block is sent back so nothing in it is
    /// lost if Discord replaces rather than merges it.
    fn set_presence(
        &self,
        presence: Presence,
        status: Option<String>,
        status_emoji: Option<String>,
        clear_at: Option<i64>,
    ) -> Result<(), Refusal> {
        let word = match presence {
            Presence::Idle => "idle",
            Presence::Dnd => "dnd",
            Presence::Offline => "invisible",
            Presence::Online | Presence::Unknown | Presence::Busy => "online",
        };
        let changing_custom = status.is_some() || status_emoji.is_some() || clear_at.is_some();
        let (kept, custom, animated) = {
            let state = crate::hub::lock(&self.state);
            let kept = state.status_settings.clone();
            if changing_custom {
                let text = status.unwrap_or_else(|| state.own.1.clone());
                let (emoji_id, emoji_name, animated) = match status_emoji {
                    Some(raw) => Self::resolve_status_emoji(&state, &raw),
                    None => {
                        let anim = state
                            .own_emoji_id
                            .and_then(|id| {
                                let sid = id.to_string();
                                state
                                    .guilds
                                    .values()
                                    .flat_map(|g| g.emojis.iter())
                                    .find(|e| e.id == sid)
                                    .map(|e| e.animated)
                            })
                            .unwrap_or(false);
                        (state.own_emoji_id, state.own_emoji.clone(), anim)
                    }
                };
                let empty = text.is_empty() && emoji_name.is_empty() && emoji_id.is_none();
                let expires_at_ms = if empty {
                    None
                } else {
                    clear_at.filter(|&s| s > 0).map(|s| (s as u64).saturating_mul(1000))
                };
                (
                    kept,
                    proto::CustomStatus { text, emoji_id, emoji_name, expires_at_ms },
                    animated,
                )
            } else {
                let anim = state
                    .own_emoji_id
                    .and_then(|id| {
                        let sid = id.to_string();
                        state
                            .guilds
                            .values()
                            .flat_map(|g| g.emojis.iter())
                            .find(|e| e.id == sid)
                            .map(|e| e.animated)
                    })
                    .unwrap_or(false);
                (
                    kept,
                    proto::CustomStatus {
                        text: state.own.1.clone(),
                        emoji_id: state.own_emoji_id,
                        emoji_name: state.own_emoji.clone(),
                        expires_at_ms: state
                            .own_status_until
                            .filter(|&s| s > 0)
                            .map(|s| (s as u64).saturating_mul(1000)),
                    },
                    anim,
                )
            }
        };
        let settings = proto::with_custom_status(&kept, word, changing_custom.then_some(&custom));
        let body = json!({"settings": proto::settings(&settings)});
        self.call("PATCH", "/users/@me/settings-proto/1", Some(body)).map_err(refusal)?;
        let playing = crate::hub::lock(&self.state).own_activity.clone();
        let mut activities = Vec::new();
        if !custom.is_empty() {
                let emoji_json = if let Some(id) = custom.emoji_id {
                    json!({"id": id.to_string(), "name": custom.emoji_name, "animated": animated})
                } else if !custom.emoji_name.is_empty() {
                    json!({"id": null, "name": custom.emoji_name, "animated": false})
                } else {
                    Value::Null
                };
            activities.push(json!({"name": "Custom Status", "type": 4, "state": custom.text, "emoji": emoji_json}));
        }
        // The local "Playing" name goes out with the status, as `set_activity` sends it.
        if !playing.is_empty() {
            activities.push(json!({"name": playing, "type": 0}));
        }
        self.tell(3, json!({"status": word, "since": 0, "activities": activities, "afk": false}));
        let until = custom.expires_at_ms.map(|ms| (ms / 1000) as i64);
        {
            let mut state = crate::hub::lock(&self.state);
            state.status_settings = settings;
            state.own = (presence, custom.text.clone());
            state.own_emoji = custom.emoji_name.clone();
            state.own_emoji_id = custom.emoji_id;
            state.own_status_until = until;
        }
        let me = self.me();
        self.ctx.presence(&me, presence, &custom.text, true);
        self.ctx.activity(&me, &custom.emoji_name, None, true);
        self.ctx.status_until(&me, until, true);
        self.reschedule_unmute();
        Ok(())
    }

    /// Reorders a server, moves it into or out of a folder, or drops it onto another server to
    /// make a folder. Discord stores that list in the settings protobuf (`settings-proto/1`).
    ///
    /// Unverified against a real account: the field numbers are the community's (`proto.rs`),
    /// and the folder block is sent back with every unknown field inside it kept.
    fn move_space(&self, space_id: &str, before: Option<&str>, folder: Option<&str>, into_space: Option<&str>) -> Result<(), Refusal> {
        self.known_guild(space_id)?;
        if let Some(other) = into_space {
            self.known_guild(other)?;
        }
        let (current, new_id) = {
            let state = crate::hub::lock(&self.state);
            (state.folders.clone(), fresh_folder_id(&state.folders))
        };
        let next = proto::move_guild(&current, space_id, before, folder, into_space, new_id).map_err(|err| Refusal::Refused(err.to_owned()))?;
        self.save_folders(&current, next)
    }

    /// Renames or recolors a server folder. `name` absent leaves the name; `color` absent
    /// leaves the color, and `Some(None)` clears it.
    fn set_folder(&self, folder_id: &str, name: Option<String>, color: Option<Option<u32>>) -> Result<(), Refusal> {
        let id = folder_id
            .trim()
            .parse::<i64>()
            .map_err(|_| Refusal::Refused("That folder is not on this account.".to_owned()))?;
        let current = crate::hub::lock(&self.state).folders.clone();
        let mut next = current.clone();
        proto::edit_guild_folder(&mut next, id, name.as_deref(), color).map_err(|err| Refusal::Refused(err.to_owned()))?;
        self.save_folders(&current, next)
    }

    fn known_guild(&self, id: &str) -> Result<(), Refusal> {
        let numeric = !id.is_empty() && id.len() <= 20 && id.bytes().all(|byte| byte.is_ascii_digit());
        let known = numeric && crate::hub::lock(&self.state).guilds.contains_key(id);
        if known { Ok(()) } else { Err(Refusal::Refused("That server is not on this account.".to_owned())) }
    }

    /// Writes the folder list when it changed, then records it. A list Discord already has is
    /// not sent again, so a retry after the store write failed does not move the server twice.
    fn save_folders(&self, current: &[proto::GuildFolder], next: Vec<proto::GuildFolder>) -> Result<(), Refusal> {
        let block = if next != current {
            let raw = crate::hub::lock(&self.state).guild_folders_settings.clone();
            let block = proto::with_guild_folders(&raw, &next);
            let body = json!({"settings": proto::settings_with_folders(&block)});
            self.call("PATCH", "/users/@me/settings-proto/1", Some(body)).map_err(refusal)?;
            Some(block)
        } else {
            None
        };
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(block) = block {
                state.guild_folders_settings = block;
            }
            let placed: HashSet<String> = next.iter().flat_map(|folder| folder.guild_ids.iter().cloned()).collect();
            let mut unplaced: Vec<String> = state.guilds.keys().filter(|id| !placed.contains(*id)).cloned().collect();
            unplaced.sort_by(|left, right| {
                let pos = |id: &String| state.guilds.get(id).map(|info| info.position).unwrap_or(0);
                pos(left).cmp(&pos(right)).then(left.cmp(right))
            });
            let mut position = 0i32;
            let ordered = unplaced.into_iter().chain(next.iter().flat_map(|folder| folder.guild_ids.iter().cloned()));
            for id in ordered {
                if let Some(info) = state.guilds.get_mut(&id) {
                    info.position = position;
                    position = position.saturating_add(1);
                }
            }
            state.folders = next.clone();
        }
        self.record_folders(&next).map_err(|err| Refusal::Refused(err.to_string()))?;
        self.spaces().map_err(|err| Refusal::Refused(err.to_string()))
    }
    /// Publishes the local user's "Playing" name on this session. Empty clears it.
    /// A later status change sends it again, and a presence that names us does not replace it.
    fn set_activity(&self, mut name: String) -> Result<(), Refusal> {
        protocol::clip(&mut name, 128);
        let (presence, text, playing) = {
            let mut state = crate::hub::lock(&self.state);
            state.own_activity = name;
            let (presence, text) = state.own.clone();
            (presence, text, state.own_activity.clone())
        };
        let word = presence_word(presence);
        self.tell(3, json!({"status": word, "since": 0, "activities": session_activities(&text, &playing), "afk": false}));
        Ok(())
    }

    /// Acknowledges reading up to `message`. At most one per channel per [`ACK_EVERY`]: a read
    /// inside that time is held, and the newest held one is sent when the time is up, once.
    fn mark_read(&self, channel: &str, message: &str) {
        // Opening a message request reads nothing on Discord: its sender is not told.
        let request = self.ctx.known(channel).ok().flatten().is_some_and(|row| row.special == protocol::Special::Request);
        if request {
            return;
        }
        let message = {
            let mut state = crate::hub::lock(&self.state);
            // Only channels inside their 3 s are worth keeping.
            if state.acks.len() > 256 {
                state.acks.retain(|_, ack| ack.held.is_some() || ack.sent.is_some_and(|at| at.elapsed() < ACK_EVERY));
            }
            let ack = state.acks.entry(channel.to_owned()).or_default();
            let first = ack.held.is_none();
            let newest = match ack.held.take() {
                Some(held) if number(&held) > number(message) => held,
                _ => message.to_owned(),
            };
            if let Some(left) = ack.sent.map(|at| ACK_EVERY.saturating_sub(at.elapsed())).filter(|left| !left.is_zero()) {
                // The first read held in this window sends the newest when the window ends.
                ack.held = Some(newest.clone());
                if first {
                    let (ctx, channel) = (self.ctx.clone(), channel.to_owned());
                    let _ = std::thread::Builder::new().name("discord-ack".into()).stack_size(64 * 1024).spawn(move || {
                        if !ctx.pause(left) {
                            let _ = ctx.commands.send(Command::MarkRead { conversation_id: channel, message_id: newest });
                        }
                    });
                }
                return;
            }
            ack.sent = Some(Instant::now());
            newest
        };
        let message = message.as_str();
        let token = crate::hub::lock(&self.state).ack_token.clone();
        // `last_viewed` is the day, counted from Discord's epoch, as the web client sends it.
        // Unconfirmed against a real account.
        let day = (crate::hub::now() - 1_420_070_400) / 86_400;
        let body = json!({"token": token, "last_viewed": day});
        match self.call("POST", &format!("/channels/{channel}/messages/{message}/ack"), Some(body)) {
            Ok(text) => {
                let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                crate::hub::lock(&self.state).ack_token = answer["token"].as_str().map(str::to_owned);
            }
            // A blocked person, or a DM Discord will not let us ack. The web client falls back
            // to the bulk route. The store is already read; this only tries to move Discord's
            // marker so the next READY does not put the badge back.
            Err(refused) if ack_refused(&refused) => {
                let reads = [(channel.to_owned(), message.to_owned())];
                if let Err(again) = self.ack_bulk(&reads) {
                    self.log("mark read", again);
                }
            }
            Err(refused) => self.log("mark read", refused),
        }
    }

    /// Moves the marker back to `message`, the last one that still counts as read. A forward
    /// ack held for this channel is dropped so the pacer does not read it again.
    fn mark_unread(&self, channel: &str, message: &str, mentions: u32) {
        if message.is_empty() {
            return;
        }
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(ack) = state.acks.get_mut(channel) {
                ack.held = None;
            }
        }
        let body = manual_ack_body(mentions);
        if let Err(refused) = self.call("POST", &format!("/channels/{channel}/messages/{message}/ack"), Some(body)) {
            self.log("mark unread", refused);
        }
    }

    /// Discord's "mark inbox read": one request, up to 50 channels. `reads` is `(channel, message)`.
    fn ack_bulk(&self, reads: &[(String, String)]) -> Result<(), Refused> {
        for body in ack_bulk_payloads(reads) {
            self.call("POST", "/read-states/ack-bulk", Some(body))?;
        }
        Ok(())
    }

    /// Discord still says this channel is unread. If we have already read through its newest
    /// message, that report is the old marker (a blocked DM refuses the ack) and it is dropped.
    /// A message newer than our marker still counts, and our marker is not moved backwards.
    fn settled(&self, channel: &str, newest: Option<&str>, unread: u32, mentions: u32, marker: Option<String>) -> (u32, u32, Option<String>) {
        if unread == 0 && mentions == 0 {
            return (unread, mentions, marker);
        }
        let Some(newest) = newest.filter(|id| !id.is_empty()) else {
            return (unread, mentions, marker);
        };
        let stored = self.ctx.known(channel).ok().flatten().and_then(|row| row.last_read).filter(|id| !id.is_empty());
        let Some(stored) = stored else {
            return (unread, mentions, marker);
        };
        if number(&stored) >= number(newest) {
            return (0, 0, None);
        }
        if marker.as_deref().is_some_and(|id| number(&stored) > number(id)) {
            return (unread, mentions, None);
        }
        (unread, mentions, marker)
    }

    /// Updates live slowmode cooldown after a send succeeds or is refused with Discord code 20016.
    pub(super) fn note_send_slowmode(&self, channel: &str, conversation: &Conversation, outcome: &Result<String, Refused>) {
        match outcome {
            Ok(_) if conversation.slowmode_seconds > 0 && !conversation.rights.slowmode_exempt => {
                self.ctx.slowmode_until(channel, crate::hub::now() + i64::from(conversation.slowmode_seconds));
            }
            Err(Refused::Status { code: 20016, retry_after, .. }) => {
                let wait = retry_after
                    .filter(|seconds| seconds.is_finite())
                    .map(|seconds| seconds.clamp(1.0, 21_600.0))
                    .unwrap_or(1.0)
                    .ceil() as i64;
                self.ctx.slowmode_until(channel, crate::hub::now() + wait);
            }
            _ => {}
        }
    }

    fn send(&self, channel: &str, client_id: &str, text: &str, reply_to: Option<&str>) -> Result<Message, String> {
        let premium = self.state.lock().unwrap().premium;
        if let Some(why) = too_long(premium, text) {
            return Err(why);
        }
        let conversation = self.ctx.known(channel).map_err(|err| err.to_string())?;
        let conversation = conversation.ok_or_else(|| "This conversation is gone.".to_owned())?;
        let guild = self.guild_of(&conversation);
        // As on the path with files: without Nitro, an emoji this server cannot send goes as its link.
        let content = if matches!(premium, 1..=3) {
            text.to_owned()
        } else {
            send::rewrite_locked_emoji(text, &self.sendable_emoji(guild.as_deref()))
        };
        let mut body = json!({
            "content": content,
            "nonce": client_id,
            "tts": false,
            "flags": 0,
            "mobile_network_type": "unknown",
        });
        if let Some(reply_to) = reply_to {
            let mut reference = json!({"channel_id": channel, "message_id": reply_to});
            if let Some(guild) = &guild {
                reference["guild_id"] = json!(guild);
            }
            body["message_reference"] = reference;
            body["allowed_mentions"] = json!({"parse": ["users", "roles", "everyone"], "replied_user": true});
        }
        let asked = self.call("POST", &format!("/channels/{channel}/messages"), Some(body));
        self.note_send_slowmode(channel, &conversation, &asked);
        let accepted = asked.map_err(|refused| refused.reason())?;
        let posted: Posted = serde_json::from_str(&accepted).map_err(|err| format!("Discord's answer was unreadable: {err}"))?;
        let (message, files) = self.convert(&posted, guild.as_deref(), conversation.kind);
        let _ = files.register(&self.ctx);
        Ok(message)
    }

    fn typing(&self, channel: &str) {
        {
            let mut state = crate::hub::lock(&self.state);
            if state.typed.get(channel).is_some_and(|at| at.elapsed() < TYPING_EVERY) {
                return;
            }
            state.typed.retain(|_, at| at.elapsed() < TYPING_EVERY);
            state.typed.insert(channel.to_owned(), Instant::now());
        }
        if let Err(refused) = self.call("POST", &format!("/channels/{channel}/typing"), None) {
            self.log("typing", refused);
        }
    }

    /// The direct conversation with a person. One request, and only when the user asked: opening
    /// conversations is something Discord watches.
    fn start_direct(&self, person: &str) -> Result<String, Refusal> {
        if let Some(known) = crate::hub::lock(&self.state).directs.get(person) {
            return Ok(known.clone());
        }
        let refuse = |why: String| Refusal::Refused(why);
        let created = self
            .call("POST", "/users/@me/channels", Some(json!({"recipients": [person]})))
            .map_err(|refused| refuse(refused.reason()))?;
        let channel: model::Channel = serde_json::from_str(&created).map_err(|err| refuse(err.to_string()))?;
        self.direct(&channel, &HashMap::new()).map_err(|err| refuse(err.to_string()))?;
        Ok(channel.id)
    }

    /// Starts a new group DM with friends (`POST /users/@me/channels`). One-shot per click:
    /// Discord watches group creation closely. Only friends may be added, up to 10 people total.
    fn start_group(&self, people: &[String], title: Option<&str>) -> Result<String, Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let recipients: Vec<String> = {
            let state = crate::hub::lock(&self.state);
            let mut out = Vec::new();
            for id in people {
                if id.is_empty() || id == &state.me || out.contains(id) {
                    continue;
                }
                if state.relations.get(id) != Some(&Relation::Friend) {
                    return Err(refuse("Only Discord friends can be added to a group DM.".to_owned()));
                }
                out.push(id.clone());
            }
            out
        };
        if recipients.is_empty() {
            return Err(refuse("Choose at least one friend to start a group.".to_owned()));
        }
        if recipients.len() + 1 > GROUP_DM_LIMIT {
            return Err(refuse(format!("A Discord group DM can have at most {GROUP_DM_LIMIT} people.")));
        }
        let created = self
            .call_once("POST", "/users/@me/channels", Some(json!({ "recipients": recipients })))
            .map_err(refusal)?;
        let mut channel: model::Channel = serde_json::from_str(&created).map_err(|err| refuse(err.to_string()))?;
        if let Some(name) = title.map(str::trim).filter(|t| !t.is_empty()) {
            if channel.kind == model::GROUP_DM {
                if let Ok(patched) = self.call("PATCH", &format!("/channels/{}", channel.id), Some(json!({ "name": name }))) {
                    if let Ok(updated) = serde_json::from_str::<model::Channel>(&patched) {
                        channel = updated;
                    } else {
                        channel.name = Some(name.to_owned());
                    }
                }
            }
        }
        self.direct(&channel, &HashMap::new()).map_err(|err| refuse(err.to_string()))?;
        Ok(channel.id)
    }

    /// Adds friends to a group DM (`PUT /channels/{id}/recipients/{user}`), or escalates a 1:1 DM
    /// into a new group DM (`POST /users/@me/channels` with `[peer, ...added]`), as Discord does.
    fn add_to_group(&self, channel: &str, people: &[String]) -> Result<(), Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let known = self
            .ctx
            .known(channel)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This conversation is gone.".to_owned()))?;
        let (me, added, peer_for_direct) = {
            let state = crate::hub::lock(&self.state);
            let mut out = Vec::new();
            for id in people {
                if id.is_empty() || id == &state.me || out.contains(id) {
                    continue;
                }
                if state.relations.get(id) != Some(&Relation::Friend) {
                    return Err(refuse("Only Discord friends can be added to a group DM.".to_owned()));
                }
                out.push(id.clone());
            }
            let peer = known.peer.clone().or_else(|| {
                state
                    .directs
                    .iter()
                    .find(|(_, ch)| ch.as_str() == channel)
                    .map(|(user, _)| user.clone())
            });
            if known.kind == ConversationKind::Direct {
                if let Some(peer_id) = peer.as_deref().filter(|id| !id.is_empty() && *id != state.me) {
                    if state.relations.get(peer_id) != Some(&Relation::Friend) {
                        return Err(refuse("Only Discord friends can be added to a group DM.".to_owned()));
                    }
                }
            }
            (state.me.clone(), out, peer)
        };
        if added.is_empty() {
            return Err(refuse("Choose at least one friend to add.".to_owned()));
        }
        match known.kind {
            ConversationKind::Direct => {
                let mut recipients = Vec::new();
                if let Some(peer) = peer_for_direct.filter(|p| !p.is_empty() && p != &me) {
                    recipients.push(peer);
                }
                for id in added {
                    if !recipients.contains(&id) {
                        recipients.push(id);
                    }
                }
                if recipients.len() + 1 > GROUP_DM_LIMIT {
                    return Err(refuse(format!("A Discord group DM can have at most {GROUP_DM_LIMIT} people.")));
                }
                let created = self
                    .call_once("POST", "/users/@me/channels", Some(json!({ "recipients": recipients })))
                    .map_err(refusal)?;
                let new_channel: model::Channel = serde_json::from_str(&created).map_err(|err| refuse(err.to_string()))?;
                self.direct(&new_channel, &HashMap::new()).map_err(|err| refuse(err.to_string()))?;
                Ok(())
            }
            ConversationKind::Group => {
                let (to_add, current_count) = {
                    let state = crate::hub::lock(&self.state);
                    let existing = state.groups.get(channel).map(|g| g.recipients.clone()).unwrap_or_default();
                    let count = existing.len() + 1;
                    let to_add: Vec<String> = added.into_iter().filter(|id| !existing.contains(id)).collect();
                    (to_add, count)
                };
                if to_add.is_empty() {
                    return Ok(());
                }
                if current_count + to_add.len() > GROUP_DM_LIMIT {
                    return Err(refuse(format!("A Discord group DM can have at most {GROUP_DM_LIMIT} people.")));
                }
                for user in &to_add {
                    self.call("PUT", &format!("/channels/{channel}/recipients/{user}"), None).map_err(refusal)?;
                    let title = {
                        let mut state = crate::hub::lock(&self.state);
                        if let Some(group) = state.groups.get_mut(channel) {
                            if !group.recipients.contains(user) {
                                group.recipients.push(user.clone());
                            }
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
                            let _ = self.ctx.conversation(&renamed(&current, &title));
                        }
                    }
                    self.ctx.info_stale(channel);
                    self.ctx.members_changed(channel);
                }
                Ok(())
            }
            _ => Err(Refusal::Unsupported),
        }
    }

    /// Creates a Discord thread, attached to `message_id` when `Some` or standalone when `None`.
    /// Never retried automatically: Discord watches thread creation closely.
    fn start_thread(
        &self,
        channel: &str,
        message_id: Option<&str>,
        name: &str,
        private: bool,
        auto_archive_minutes: Option<u32>,
        first_message: &str,
    ) -> Result<String, Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let parent = self
            .ctx
            .known(channel)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This conversation is gone.".to_owned()))?;
        if !matches!(parent.kind, ConversationKind::Channel | ConversationKind::Announcement) {
            return Err(refuse("A thread can only be started in a channel.".to_owned()));
        }
        if private && !parent.rights.create_private_threads {
            return Err(refuse("You do not have permission to create private threads here.".to_owned()));
        }
        if !private && !parent.rights.create_public_threads {
            return Err(refuse("You do not have permission to create threads here.".to_owned()));
        }
        let name = thread_name(name);
        if let Some(message_id) = message_id {
            if private {
                return Err(refuse("A thread started from a message cannot be private.".to_owned()));
            }
            let stored = self
                .ctx
                .stored_message(channel, message_id)
                .map_err(|err| refuse(err.to_string()))?
                .ok_or_else(|| refuse("That message is not here.".to_owned()))?;
            if stored.deleted || matches!(stored.kind, MessageKind::System) {
                return Err(refuse("That message cannot start a thread.".to_owned()));
            }
            if let Some(existing) = stored.thread.as_ref() {
                let stub = model::Channel {
                    id: existing.id.clone(),
                    kind: model::PUBLIC_THREAD,
                    name: Some(name.clone()),
                    parent_id: Some(parent.key.id.clone()),
                    ..model::Channel::default()
                };
                self.record_thread(&parent, &stub, Some(message_id), &name)?;
                return Ok(existing.id.clone());
            }
            let mut body = json!({ "name": name });
            if let Some(minutes) = auto_archive_minutes {
                body["auto_archive_duration"] = json!(minutes);
            }
            let created = self
                .call_once("POST", &format!("/channels/{channel}/messages/{message_id}/threads"), Some(body))
                .map_err(refusal)?;
            let thread: model::Channel = serde_json::from_str(&created)
                .map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
            let title = thread.name.clone().filter(|title| !title.trim().is_empty()).unwrap_or_else(|| name.clone());
            self.record_thread(&parent, &thread, Some(message_id), &title)?;
            self.remember_starter(&thread, channel, message_id);
            let unread = self.ctx.known(&thread.id).ok().flatten().map(|row| row.unread).unwrap_or(0);
            let mut message = stored;
            message.thread = Some(ThreadSummary {
                id: thread.id.clone(),
                replies: thread.message_count.unwrap_or(0),
                last: thread.last_message_id.as_deref().map_or(0, seconds),
                participants: Vec::new(),
                unread,
            });
            self.ctx.history(channel, &[message]).map_err(|err| refuse(err.to_string()))?;
            return Ok(thread.id);
        }

        // Standalone thread from the channel/composer.
        if private && parent.kind == ConversationKind::Announcement {
            return Err(refuse("Private threads cannot be created in an announcement channel.".to_owned()));
        }
        let thread_type = if parent.kind == ConversationKind::Announcement {
            model::NEWS_THREAD
        } else if private {
            model::PRIVATE_THREAD
        } else {
            model::PUBLIC_THREAD
        };
        let mut body = json!({
            "name": name,
            "type": thread_type,
        });
        if let Some(minutes) = auto_archive_minutes {
            body["auto_archive_duration"] = json!(minutes);
        }
        if private {
            body["invitable"] = json!(true);
        }
        let created = self
            .call_once("POST", &format!("/channels/{channel}/threads"), Some(body))
            .map_err(refusal)?;
        let thread: model::Channel = serde_json::from_str(&created)
            .map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
        let title = thread.name.clone().filter(|title| !title.trim().is_empty()).unwrap_or_else(|| name.clone());
        self.record_thread(&parent, &thread, None, &title)?;
        if !first_message.trim().is_empty() {
            let nonce = format!("{}", crate::hub::now().unsigned_abs().saturating_mul(1000).saturating_add(1));
            match self.send(&thread.id, &nonce, first_message.trim(), None) {
                Ok(message) => {
                    let _ = self.ctx.message(&thread.id, &message);
                }
                Err(why) => self.log("thread first message", why),
            }
        }
        Ok(thread.id)
    }

    /// Adds one or more users to a Discord thread (`PUT /channels/{thread}/thread-members/{user}`).
    fn add_members(&self, channel: &str, people: &[String]) -> Result<(), Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let known = self
            .ctx
            .known(channel)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This conversation is gone.".to_owned()))?;
        if known.kind != ConversationKind::Thread {
            return Err(Refusal::Unsupported);
        }
        for person in people {
            let person = person.trim();
            if person.is_empty() {
                continue;
            }
            self.call("PUT", &format!("/channels/{channel}/thread-members/{}", escaped(person)), None)
                .map_err(refusal)?;
        }
        self.ctx.info_stale(channel);
        self.ctx.members_changed(channel);
        Ok(())
    }

    /// Updates our membership (`followed`) and sidebar tree listing for one Discord thread.
    pub(super) fn set_thread_membership(&self, thread_id: &str, joined: bool) -> rusqlite::Result<()> {
        let Some(known) = self.ctx.known(thread_id)? else {
            return Ok(());
        };
        if known.kind != ConversationKind::Thread {
            return Ok(());
        }
        let guild = self.guild_of(&known);
        let parent = known.parent.as_deref().and_then(|id| self.ctx.known(id).ok().flatten());
        let forum = parent.as_ref().is_some_and(|row| row.kind == ConversationKind::Forum);
        let in_tree = joined && !known.archived && !forum;
        let mut row = renamed(&known, &known.title);
        if in_tree {
            if row.space.is_none() {
                if let Some(parent) = parent.as_ref() {
                    row.space = parent.space.clone();
                    row.group = parent.group.clone();
                    row.position = parent.position + 1;
                }
            }
        } else {
            row.space = None;
            row.group = None;
        }
        row.filing = Some(kept_filing(Some(&known), known.archived, joined));
        if let Some(guild) = &guild {
            self.note_thread_listed(guild, thread_id, known.parent.as_deref(), in_tree);
        }
        self.ctx.conversation(&row)
    }

    /// Joins (`on == true`) or leaves (`on == false`) a Discord thread.
    fn follow_thread(&self, thread_id: &str, on: bool) -> Result<(), Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let known = self
            .ctx
            .known(thread_id)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This thread is gone.".to_owned()))?;
        if known.kind != ConversationKind::Thread {
            return Err(Refusal::Unsupported);
        }
        let method = if on { "PUT" } else { "DELETE" };
        self.call(method, &format!("/channels/{thread_id}/thread-members/@me"), None)
            .map_err(refusal)?;
        self.set_thread_membership(thread_id, on).map_err(|err| refuse(err.to_string()))?;
        self.ctx.info_stale(thread_id);
        self.ctx.members_changed(thread_id);
        Ok(())
    }

    /// Lists active or archived threads of one Discord channel and records each so clicking
    /// opens it without joining first.
    fn channel_threads(
        &self,
        channel_id: &str,
        archived: bool,
        query: &str,
        cursor: Option<&str>,
    ) -> Result<super::ChannelThreadsPage, Refusal> {
        #[derive(serde::Deserialize, Default)]
        struct ThreadMemberId {
            #[serde(default)]
            id: String,
        }
        #[derive(serde::Deserialize, Default)]
        struct ThreadsList {
            #[serde(default)]
            threads: Vec<model::Channel>,
            /// Membership rows sit beside `threads`, keyed by the thread id.
            #[serde(default)]
            members: Vec<ThreadMemberId>,
            #[serde(default)]
            has_more: bool,
        }
        fn thread_member_ids(members: &[ThreadMemberId]) -> HashSet<String> {
            members.iter().map(|member| member.id.clone()).filter(|id| !id.is_empty()).collect()
        }

        let refuse = |why: String| Refusal::Refused(why);
        let parent = self
            .ctx
            .known(channel_id)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This conversation is gone.".to_owned()))?;
        if !matches!(parent.kind, ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Forum) {
            return Err(refuse("Only channels and forums have threads.".to_owned()));
        }
        let trimmed = query.trim();
        let (raw_threads, member_ids, next_cursor, more) = if !archived {
            let offset: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
            let mut path = format!(
                "/channels/{channel_id}/threads/search?archived=false&sort_by=last_message_time&sort_order=desc&limit=25"
            );
            if !trimmed.is_empty() {
                path.push_str(&format!("&name={}", escaped(trimmed)));
            }
            if offset > 0 {
                path.push_str(&format!("&offset={offset}"));
            }
            let body = self.call("GET", &path, None).map_err(refusal)?;
            let parsed: ThreadsList = serde_json::from_str(&body)
                .map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
            let members = thread_member_ids(&parsed.members);
            let next = (parsed.has_more && !parsed.threads.is_empty()).then(|| (offset + parsed.threads.len()).to_string());
            let more = next.is_some();
            (parsed.threads, members, next, more)
        } else {
            let mut path = format!("/channels/{channel_id}/threads/archived/public?limit=25");
            if let Some(before) = cursor.filter(|c| !c.is_empty()) {
                path.push_str(&format!("&before={}", escaped(before)));
            }
            let body = self.call("GET", &path, None).map_err(refusal)?;
            let parsed: ThreadsList = serde_json::from_str(&body)
                .map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
            let last_ts = parsed
                .threads
                .last()
                .and_then(|t| t.thread_metadata.as_ref())
                .and_then(|m| m.archive_timestamp.clone());
            let mut members = thread_member_ids(&parsed.members);
            let any = !parsed.threads.is_empty();
            let mut combined = parsed.threads;
            if cursor.is_none() {
                if let Ok(priv_body) = self.call("GET", &format!("/channels/{channel_id}/users/@me/threads/archived/private?limit=25"), None) {
                    if let Ok(priv_parsed) = serde_json::from_str::<ThreadsList>(&priv_body) {
                        members.extend(thread_member_ids(&priv_parsed.members));
                        for thread in priv_parsed.threads {
                            if !combined.iter().any(|existing| existing.id == thread.id) {
                                combined.push(thread);
                            }
                        }
                    }
                }
            }
            if !trimmed.is_empty() {
                let needle = trimmed.to_lowercase();
                combined.retain(|t| t.name.as_deref().unwrap_or("").to_lowercase().contains(&needle));
            }
            let next = (parsed.has_more && any).then_some(last_ts).flatten();
            (combined, members, next.clone(), next.is_some())
        };

        let guild = self.guild_of(&parent);
        let exempt = guild
            .as_deref()
            .and_then(|g| self.channel_permission_bits(g, &parent.key.id))
            .map_or(parent.rights.slowmode_exempt || parent.rights.manage_threads, |perms| ready::slowmode_exempt(perms, true));
        let clock = crate::hub::now();
        let timed_out = guild.as_deref().and_then(|g| {
            let state = self.state.lock().unwrap();
            state.guilds.get(g).and_then(|info| info.timed_out(clock))
        });
        let mut items = Vec::with_capacity(raw_threads.len());
        for thread in raw_threads {
            let existing = self.ctx.known(&thread.id).ok().flatten();
            let title = thread.name.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Thread".to_owned());
            let is_archived = archived || thread.thread_metadata.as_ref().is_some_and(|m| m.archived);
            let is_locked = thread.thread_metadata.as_ref().is_some_and(|m| m.locked);
            let followed = member_ids.contains(&thread.id) || thread.member.is_some() || existing.as_ref().is_some_and(|k| k.followed);
            let mut row = ConversationRow::new(&thread.id, ConversationKind::Thread, &title);
            row.parent = Some(parent.key.id.clone());
            row.root = existing.as_ref().and_then(|k| k.root.clone()).or_else(|| Some(thread.id.clone()));
            row.private = thread.kind == model::PRIVATE_THREAD;
            row.rights = parent.rights;
            row.rights.add_members = timed_out.is_none() && (parent.rights.send_in_threads || parent.rights.manage_threads);
            row.rights.rename = timed_out.is_none() && (parent.rights.manage_threads || thread.owner_id.as_deref() == Some(self.me().as_str()));
            row.rights.slowmode_exempt = exempt;
            row.slowmode_seconds = thread.rate_limit_per_user;
            row.locked = is_locked;
            let (read_only, read_only_reason) = ready::thread_read_only(timed_out, clock, is_locked, &row.rights);
            row.read_only = read_only;
            row.read_only_reason = read_only_reason;
            let in_tree = followed && !is_archived && parent.kind != ConversationKind::Forum;
            if in_tree {
                row.space = parent.space.clone();
                row.group = parent.group.clone();
                row.position = parent.position + 1;
            }
            if let Some(guild) = &guild {
                self.note_thread_listed(guild, &thread.id, Some(channel_id), in_tree);
            }
            row.filing = Some(kept_filing(existing.as_ref(), is_archived, followed));
            let _ = self.ctx.conversation(&row);
            self.release_slowmode(&thread.id, row.slowmode_seconds, row.rights.slowmode_exempt);
            let conversation = self
                .ctx
                .known(&thread.id)
                .ok()
                .flatten()
                .unwrap_or_else(|| Conversation::new(self.ctx.key(&thread.id), ConversationKind::Thread, &title));
            let last_activity = thread
                .last_message_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(seconds)
                .or_else(|| {
                    thread
                        .thread_metadata
                        .as_ref()
                        .and_then(|m| m.archive_timestamp.as_deref())
                        .and_then(model::iso_seconds)
                })
                .unwrap_or(conversation.updated);
            items.push(protocol::ChannelThread {
                conversation,
                replies: thread.message_count.unwrap_or(0),
                last_activity,
                locked: is_locked,
            });
        }
        Ok(super::ChannelThreadsPage {
            threads: items,
            cursor: next_cursor,
            more,
        })
    }

    /// Updates a Discord thread (`PATCH /channels/{thread}`) with only the provided fields.
    fn thread_update(
        &self,
        thread_id: &str,
        name: Option<String>,
        archived: Option<bool>,
        locked: Option<bool>,
        auto_archive_minutes: Option<u32>,
        slowmode_seconds: Option<u32>,
    ) -> Result<(), Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let known = self
            .ctx
            .known(thread_id)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This thread is gone.".to_owned()))?;
        if known.kind != ConversationKind::Thread {
            return Err(refuse("Only threads can be updated here.".to_owned()));
        }
        let mut body = serde_json::Map::new();
        if let Some(name) = name {
            body.insert("name".to_owned(), json!(thread_name(&name)));
        }
        if let Some(archived) = archived {
            body.insert("archived".to_owned(), json!(archived));
        }
        if let Some(locked) = locked {
            body.insert("locked".to_owned(), json!(locked));
        }
        if let Some(minutes) = auto_archive_minutes {
            body.insert("auto_archive_duration".to_owned(), json!(minutes));
        }
        if let Some(secs) = slowmode_seconds {
            body.insert("rate_limit_per_user".to_owned(), json!(secs));
        }
        if body.is_empty() {
            return Ok(());
        }
        let answer = self
            .call("PATCH", &format!("/channels/{thread_id}"), Some(Value::Object(body)))
            .map_err(refusal)?;
        let mut updated: model::Channel = serde_json::from_str(&answer)
            .map_err(|err| refuse(format!("Discord's answer was unreadable: {err}")))?;
        if updated.id.is_empty() {
            updated.id = thread_id.to_owned();
        }
        if updated.parent_id.is_none() {
            updated.parent_id = known.parent.clone();
        }
        self.thread_changed(updated, known.followed).map_err(|failed| match failed {
            Failure::SignedOut(why) | Failure::Dropped(why) => refuse(why),
        })?;
        self.ctx.info_stale(thread_id);
        Ok(())
    }

    /// Deletes a Discord thread (`DELETE /channels/{thread}`).
    fn delete_thread(&self, thread_id: &str) -> Result<(), Refusal> {
        let refuse = |why: String| Refusal::Refused(why);
        let known = self
            .ctx
            .known(thread_id)
            .map_err(|err| refuse(err.to_string()))?
            .ok_or_else(|| refuse("This thread is gone.".to_owned()))?;
        if known.kind != ConversationKind::Thread {
            return Err(refuse("Only threads can be deleted here.".to_owned()));
        }
        self.call("DELETE", &format!("/channels/{thread_id}"), None).map_err(refusal)?;
        let guild = self.guild_of(&known);
        {
            let mut state = crate::hub::lock(&self.state);
            state.covered.remove(thread_id);
            if let Some(info) = guild.as_deref().and_then(|g| state.guilds.get_mut(g)) {
                info.channels.retain(|(id, _)| *id != number(thread_id));
            }
        }
        if let Some(guild) = guild {
            let note = format!("threads:{guild}");
            let before = self.ctx.note(&note).unwrap_or_default();
            let after: Vec<&str> = before.split(',').filter(|id| !id.is_empty() && *id != thread_id).collect();
            let after = after.join(",");
            if after != before {
                self.ctx.set_note(&note, &after);
            }
        }
        self.ctx.conversation_gone(thread_id).map_err(|err| refuse(err.to_string()))
    }

    /// The thread's conversation, when it is not recorded yet. Its root is the message it hangs
    /// off (when attached to one). A thread kept outside the tree still needs its server, which
    /// `guild_of` reads back.
    fn record_thread(
        &self,
        parent: &Conversation,
        thread: &model::Channel,
        root: Option<&str>,
        title: &str,
    ) -> Result<(), Refusal> {
        let refuse = |err: rusqlite::Error| Refusal::Refused(err.to_string());
        if self.ctx.known(&thread.id).map_err(refuse)?.is_some() {
            return Ok(());
        }
        let in_tree = parent.kind != ConversationKind::Forum;
        if let Some(guild) = self.guild_of(parent) {
            self.note_thread_listed(&guild, &thread.id, Some(&parent.key.id), in_tree);
        }
        let me = self.me();
        let is_owner = thread.owner_id.as_deref() == Some(me.as_str());
        let guild = self.guild_of(parent);
        let exempt = guild
            .as_deref()
            .and_then(|g| self.channel_permission_bits(g, &parent.key.id))
            .map_or(parent.rights.slowmode_exempt || parent.rights.manage_threads, |perms| ready::slowmode_exempt(perms, true));
        let clock = crate::hub::now();
        let timed_out = guild.as_deref().and_then(|g| {
            let state = self.state.lock().unwrap();
            state.guilds.get(g).and_then(|info| info.timed_out(clock))
        });
        let open = timed_out.is_none();
        let mut rights = parent.rights;
        rights.add_members = open && (rights.send_in_threads || rights.manage_threads);
        rights.rename = open && (rights.manage_threads || is_owner);
        rights.slowmode_exempt = exempt;
        let locked = thread.thread_metadata.as_ref().is_some_and(|metadata| metadata.locked);
        let (read_only, read_only_reason) = ready::thread_read_only(timed_out, clock, locked, &rights);
        let mut row = ConversationRow::new(&thread.id, ConversationKind::Thread, title);
        row.parent = Some(parent.key.id.clone());
        row.root = root.map(str::to_owned);
        if in_tree {
            row.space = parent.space.clone();
            row.group = parent.group.clone();
            row.position = parent.position + 1;
        }
        row.private = thread.kind == model::PRIVATE_THREAD;
        row.read_only = read_only;
        row.read_only_reason = read_only_reason;
        row.age_restricted = parent.age_restricted;
        row.rights = rights;
        row.slowmode_seconds = thread.rate_limit_per_user;
        row.locked = locked;
        row.filing = Some(kept_filing(None, false, true));
        self.ctx.conversation(&row).map_err(refuse)?;
        self.release_slowmode(&thread.id, row.slowmode_seconds, exempt);
        Ok(())
    }

    /// Where a registered file is fetched now. Attachment addresses are signed and run out
    /// (`ex` is the hex time); one that has is put aside to be exchanged for a fresh one.
    ///
    /// CONTRACT: `Command::Download` names one file, but Discord renews up to fifty addresses
    /// in one request and the media cache asks about every waiting file in a row. So an
    /// expired one is not answered at once: it waits in `renewing` until a `Download` with no
    /// id, which this sends to its own inbox behind whatever is already queued, renews them
    /// all together.
    fn download(&self, id: String, url: String, reply: Reply<Download>) {
        if id.is_empty() && url.is_empty() {
            return self.renew();
        }
        let expires = url.split(['?', '&']).find_map(|part| i64::from_str_radix(part.strip_prefix("ex=")?, 16).ok());
        if expires.is_none_or(|expires| expires >= crate::hub::now() + 60) {
            return answer(reply, Ok(self.from(url)));
        }
        // Asked about twice (two fetches renewing at once each name the other's file): the
        // answer Discord already gave is given again.
        let renewed = crate::hub::lock(&self.state).renewed.get(&url).cloned();
        if let Some(fresh) = renewed {
            return answer(reply, Ok(self.from(fresh)));
        }
        let first = {
            let mut state = crate::hub::lock(&self.state);
            state.renewing.push((id, url, reply));
            state.renewing.len() == 1
        };
        if first {
            let (nobody, _) = std::sync::mpsc::channel();
            let marker = Command::Download {
                id: String::new(),
                url: String::new(),
                reply: nobody,
            };
            let _ = self.ctx.commands.send(marker);
        }
    }

    fn from(&self, url: String) -> Download {
        Download {
            url,
            headers: vec![("User-Agent".to_owned(), self.rest.identity().user_agent.clone())],
            sealed: None,
        }
    }

    /// Exchanges every address put aside for a fresh one, fifty to a request, records each
    /// (`Context::media_refreshed`) so it is used from now on, and answers whoever asked.
    ///
    /// A resized picture's address is the attachment's with our own size appended
    /// (`content::resized`): Discord is asked about the attachment's, and the size goes back on.
    fn renew(&self) {
        let waiting = std::mem::take(&mut crate::hub::lock(&self.state).renewing);
        let plain = |url: &str| -> (String, String) {
            let cut = url.find("&format=webp&width=").or_else(|| url.find("?format=webp&width="));
            cut.map_or_else(|| (url.to_owned(), String::new()), |cut| (url[..cut].to_owned(), url[cut + 1..].to_owned()))
        };
        for batch in waiting.chunks(RENEWED_TOGETHER) {
            let mut urls: Vec<String> = batch.iter().map(|(_, url, _)| plain(url).0).collect();
            urls.sort();
            urls.dedup();
            let asked = self.call("POST", "/attachments/refresh-urls", Some(json!({"attachment_urls": urls})));
            let renewed: Vec<(String, String)> = match &asked {
                Ok(text) => {
                    let answer: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                    let pairs = answer["refreshed_urls"].as_array().map_or(&[][..], Vec::as_slice).iter();
                    let pairs = pairs.filter_map(|pair| Some((pair["original"].as_str()?.to_owned(), pair["refreshed"].as_str()?.to_owned())));
                    pairs.collect()
                }
                Err(_) => Vec::new(),
            };
            for (id, url, reply) in batch {
                let (attachment, size) = plain(url);
                let fresh = renewed.iter().find(|(original, _)| *original == attachment).map(|(_, fresh)| match size.is_empty() {
                    true => fresh.clone(),
                    false => format!("{fresh}{}{size}", if fresh.contains('?') { '&' } else { '?' }),
                });
                let outcome = match (fresh, &asked) {
                    (Some(fresh), _) => {
                        if let Err(err) = self.ctx.media_refreshed(id, &fresh) {
                            self.log("media", err);
                        }
                        let mut state = crate::hub::lock(&self.state);
                        if state.renewed.len() >= 4 * RENEWED_TOGETHER {
                            state.renewed.clear();
                        }
                        state.renewed.insert(url.clone(), fresh.clone());
                        drop(state);
                        Ok(self.from(fresh))
                    }
                    // Not renewed: the cache is told where the file was, and finds it gone.
                    (None, Ok(_)) => Ok(self.from(url.clone())),
                    (None, Err(refused)) => Err(Refusal::Refused(refused.reason())),
                };
                let _ = reply.send(outcome);
            }
        }
    }

    fn handoff(&self, channel: &str) -> Result<String, Refusal> {
        let conversation = self.ctx.known(channel).ok().flatten();
        let conversation = conversation.ok_or_else(|| Refusal::Refused("This conversation is gone.".to_owned()))?;
        let guild = self.guild_of(&conversation);
        Ok(voice::handoff_link(guild.as_deref(), channel))
    }

    fn queue_collapse(&self, guild: &str, category: &str, collapsed: bool) {
        {
            let mut state = crate::hub::lock(&self.state);
            if let Some(info) = state.guilds.get_mut(guild) {
                if collapsed {
                    info.quiet.collapsed.insert(category.to_owned());
                } else {
                    info.quiet.collapsed.remove(category);
                }
            } else {
                return;
            }
            state.collapses.push(guild, category, collapsed, Instant::now());
        }
        self.outbox.wake.signal();
    }

    pub(super) fn collapse_tick(&self) -> bool {
        let (due, remaining) = {
            let mut state = crate::hub::lock(&self.state);
            let due = state.collapses.take_due(Instant::now());
            (due, state.collapses.has_pending())
        };
        for (guild, overrides) in due {
            let body = collapse_settings_body(&overrides);
            let path = format!("/users/@me/guilds/{guild}/settings");
            if let Err(refused) = self.call("PATCH", &path, Some(body)) {
                self.log("collapse", refused);
            }
        }
        remaining
    }

    fn command(&self, command: Command) {
        // Everything but these is the user doing something, which is what an adapter that
        // stopped asking for a session was waiting for. The screen moving counts when it moved
        // to something of this account; a watch that lapsed names nothing.
        let ours = |key: &Option<protocol::Key>| key.as_ref().is_some_and(|key| key.account() == self.ctx.account);
        let unasked = match &command {
            Command::Watch(watching) => !ours(&watching.conversation) && !ours(&watching.space),
            Command::Warm(_)
            | Command::Members { .. }
            | Command::Download { .. }
            | Command::Handoff { .. }
            | Command::CallLeave(_)
            | Command::CallAudio { .. }
            | Command::SignOut(_) => true,
            _ => false,
        };
        if !unasked {
            self.asked();
        }
        match command {
            // `serve_commands` keeps these two for itself.
            Command::Backfill(id) => self.backfill(&id),
            Command::Warm(id) => {
                self.backfill(&id);
                self.ctx.shrink_memory();
            }
            Command::History { conversation_id, before } => {
                if let Ok(Some(conversation)) = self.ctx.known(&conversation_id) {
                    self.fetch(&conversation, Stretch::Before(&before));
                }
            }
            // Sent for every page read forward: Discord is asked only when what follows is not
            // already stored from this session.
            Command::HistoryAfter { conversation_id, after } => {
                if self.covered_after(&conversation_id, number(&after)) {
                    return;
                }
                if let Ok(Some(conversation)) = self.ctx.known(&conversation_id) {
                    self.fetch(&conversation, Stretch::After(&after));
                }
            }
            Command::HistoryAround { conversation_id, around } => {
                let at = number(&around);
                let inside = {
                    let state = crate::hub::lock(&self.state);
                    let stretches = state.covered.get(&conversation_id);
                    stretches.is_some_and(|stretches| stretches.iter().any(|(from, to)| (*from..=*to).contains(&at)))
                };
                if inside && self.ctx.stored_message(&conversation_id, &around).ok().flatten().is_some() {
                    return;
                }
                if let Ok(Some(conversation)) = self.ctx.known(&conversation_id) {
                    self.fetch(&conversation, Stretch::Around(&around));
                }
            }
            Command::OpenSpace(space) => self.open_space(&space),
            Command::Collapse { space_id, group_id, collapsed } => {
                self.queue_collapse(&space_id, &group_id, collapsed);
            }
            // A page of this forum's posts, including archived ones. Active posts also arrive
            // on the socket; the search is what reaches the rest, one page at a time.
            Command::OpenForum { conversation_id, sort, tag, query, offset, reply } => {
                answer(reply, self.forum_posts(&conversation_id, sort, tag, query, offset))
            }
            Command::MarkRead { conversation_id, message_id } => self.mark_read(&conversation_id, &message_id),
            Command::MarkUnread { conversation_id, message_id, mentions } => self.mark_unread(&conversation_id, &message_id, mentions),
            Command::AckBulk { reads } => {
                if let Err(refused) = self.ack_bulk(&reads) {
                    self.log("mark read", refused);
                }
            }
            Command::Send { conversation_id, client_id, text, reply_to } => {
                let outcome = self.send(&conversation_id, &client_id, &text, reply_to.as_deref());
                self.ctx.sent(&conversation_id, &client_id, outcome);
            }
            Command::React { conversation_id, message_id, emoji, on, burst, reply } => {
                answer(reply, self.react(&conversation_id, &message_id, &emoji, on, burst))
            }
            Command::Reactors { conversation_id, message_id, emoji, burst, after, limit, reply } => {
                answer(reply, self.reactors(&conversation_id, &message_id, &emoji, burst, after.as_deref(), limit))
            }
            Command::ClearReactions { conversation_id, message_id, emoji, user_id, reply } => {
                answer(reply, self.clear_reactions(&conversation_id, &message_id, emoji.as_deref(), user_id.as_deref()))
            }
            Command::Edit {
                conversation_id,
                message_id,
                text,
                spans: _,
                suppress_embeds,
                keep_attachments,
                reply,
            } => answer(
                reply,
                self.edit(&conversation_id, &message_id, &text, suppress_embeds, keep_attachments.as_deref()),
            ),
            Command::Source { conversation_id, message_id, reply } => {
                answer(reply, self.source(&conversation_id, &message_id))
            }
            Command::Delete { conversation_id, message_id, scope, reply } => {
                if scope == protocol::DeleteScope::Me {
                    let _ = reply.send(Err(Refusal::Refused("Discord only supports deleting for everyone.".to_owned())));
                } else {
                    answer(reply, self.delete(&conversation_id, &message_id));
                }
            }
            Command::Relate { person_id, action, reply } => answer(reply, self.relate(&person_id, action)),
            Command::AddFriend { username, reply } => answer(reply, self.add_friend(&username)),
            Command::SetNote { person_id, text, reply } => answer(reply, self.set_note(&person_id, &text)),
            Command::SetFriendNickname { person_id, nickname, reply } => {
                answer(reply, self.set_friend_nickname(&person_id, nickname))
            }
            Command::Moderate { space_id, person_id, action, reason, reply } => {
                answer(reply, self.moderate(&space_id, &person_id, action, &reason))
            }
            Command::Unban { space_id, person_id, reason, reply } => {
                answer(reply, self.unban(&space_id, &person_id, &reason))
            }
            Command::SetPresence { presence, status, status_emoji, clear_at, reply } => {
                answer(reply, self.set_presence(presence, status, status_emoji, clear_at))
            }
            Command::SetProfile { name, about, pronouns, accent, banner, space_id, reply, .. } => {
                answer(reply, self.set_profile(name, about, pronouns, accent, banner, space_id))
            }
            Command::SetAvatar { path, reply } => answer(reply, self.set_avatar(&path)),
            Command::SetNickname { space_id, nickname, reply } => answer(reply, self.set_nickname(&space_id, nickname)),
            Command::SetActivity { name, reply } => answer(reply, self.set_activity(name)),
            Command::Profile { person_id, reply } => answer(reply, self.profile(&person_id, None)),
            Command::ProfileIn { person_id, space_id, reply } => answer(reply, self.profile(&person_id, Some(&space_id))),
            Command::ConversationInfo { conversation_id, reply } => answer(reply, self.conversation_info(&conversation_id)),
            Command::SpaceInfo { space_id, reply } => answer(reply, self.space_info(&space_id)),
            Command::SetSpaceOptions {
                space_id,
                notify,
                suppress_everyone,
                suppress_roles,
                highlights,
                events,
                mobile_push,
                reset_channel,
                reply,
            } => answer(
                reply,
                self.set_space_options(
                    &space_id,
                    notify,
                    suppress_everyone,
                    suppress_roles,
                    highlights,
                    events,
                    mobile_push,
                    reset_channel.as_deref(),
                ),
            ),
            Command::ActiveSessions { reply } => answer(reply, self.active_sessions()),
            Command::EndSession { session_id, reply } => answer(reply, self.end_session(&session_id)),
            Command::EndOtherDiscordSessions { reply } => answer(reply, self.end_other_sessions()),
            Command::DiscordAccountEntries { list, reply } => answer(reply, self.account_entries(list)),
            Command::RemoveDiscordAccountEntry { target, reply } => answer(reply, self.remove_account_entry(target)),
            Command::GetNetworkPrivacy { reply } => answer(reply, self.get_network_privacy()),
            Command::SetNetworkPrivacy { privacy, reply } => answer(reply, self.set_network_privacy(privacy)),
            Command::SetSpacePrivacy { space_id, privacy, reply } => answer(reply, self.set_space_privacy(&space_id, privacy)),
            Command::InvitePreview { code, event_id, reply } => {
                answer(reply, self.invite_preview(&code, event_id.as_deref()))
            }
            Command::InviteJoin { code, reply } => answer(reply, self.invite_join(&code)),
            Command::CreateInviteLink { conversation_id, space_id, max_age, max_uses, temporary, reply } => {
                answer(reply, self.create_invite(conversation_id.as_deref(), space_id.as_deref(), max_age, max_uses, temporary))
            }
            Command::EventPreview { space_id, event_id, reply } => {
                answer(reply, self.event_preview(&space_id, &event_id))
            }
            Command::EventLinkInterest { space_id, event_id, interested, reply } => {
                answer(reply, self.event_link_interest(&space_id, &event_id, interested))
            }
            Command::SetMute { conversation_id, notify, until, reply } => answer(reply, self.set_mute(&conversation_id, notify, until)),
            Command::Leave { conversation_id, silent, reply } => answer(reply, self.leave(&conversation_id, false, silent)),
            Command::TransferOwner { conversation_id, person, reply } => {
                answer(reply, self.transfer_group_owner(&conversation_id, &person))
            }
            Command::CreateSpace { name, icon, template, reply } => {
                answer(reply, self.create_space(&name, icon.as_deref(), template.as_deref()))
            }
            Command::Events { space_id, reply } => answer(reply, self.events(&space_id)),
            Command::EventInterest { space_id, event_id, on, reply } => {
                answer(reply, self.event_interest(&space_id, &event_id, on))
            }
            Command::EditSpace { space_id, edit, reply } => answer(reply, self.edit_space(&space_id, &edit)),
            Command::DeleteSpace { space_id, mfa_code, reply } => {
                answer(reply, self.delete_space(&space_id, mfa_code.as_deref()))
            }
            Command::TransferSpace { space_id, new_owner_id, mfa_code, reply } => {
                answer(reply, self.transfer_space(&space_id, &new_owner_id, mfa_code.as_deref()))
            }
            Command::CreateEvent { space_id, event, reply } => answer(reply, self.create_event(&space_id, &event)),
            Command::EditEvent { space_id, event_id, event, reply } => {
                answer(reply, self.edit_event(&space_id, &event_id, &event))
            }
            Command::DeleteEvent { space_id, event_id, reply } => {
                answer(reply, self.delete_event(&space_id, &event_id))
            }
            Command::SpaceInvites { space_id, reply } => answer(reply, self.space_invites(&space_id)),
            Command::RevokeSpaceInvite { space_id, code, reply } => answer(reply, self.revoke_invite(&space_id, &code)),
            Command::SetInvitesPaused { space_id, paused, reply } => {
                answer(reply, self.set_invites_paused(&space_id, paused))
            }
            Command::Roles { space_id, reply } => answer(reply, self.roles(&space_id)),
            Command::CreateRole { space_id, edit, reply } => answer(reply, self.create_role(&space_id, &edit)),
            Command::EditRole { space_id, role_id, edit, reply } => answer(reply, self.edit_role(&space_id, &role_id, &edit)),
            Command::MoveRoles { space_id, roles, reply } => answer(reply, self.move_roles(&space_id, &roles)),
            Command::DeleteRole { space_id, role_id, reply } => answer(reply, self.delete_role(&space_id, &role_id)),
            Command::SetMemberRoles { space_id, person_id, roles, reply } => {
                answer(reply, self.set_member_roles(&space_id, &person_id, &roles))
            }
            Command::SetSpaceNotify { space_id, notify, until, category, reply } => {
                answer(reply, self.set_space_notify(&space_id, notify, until, category.as_deref()))
            }
            Command::FollowChannel { conversation_id, on, reply } => {
                answer(reply, self.follow_channel(&conversation_id, on))
            }
            Command::SetSpaceView { space_id, show_all, hide_muted, reply } => {
                answer(reply, self.set_space_view(&space_id, show_all, hide_muted))
            }
            Command::BrowseChannels { space_id, query, cursor: _, limit, reply } => {
                answer(reply, self.browse_channels(&space_id, &query, limit))
            }
            Command::CreateChannel { space_id, name, kind, private, group, reply, .. } => {
                answer(reply, self.create_channel(&space_id, &name, kind, private, group.as_deref()))
            }
            Command::CreateCategory { space_id, name, reply } => {
                answer(reply, self.create_category(&space_id, &name))
            }
            Command::EditChannel {
                conversation_id,
                name,
                topic,
                rate_limit_per_user,
                nsfw,
                user_limit,
                parent_id,
                reply,
            } => answer(
                reply,
                self.edit_channel(
                    &conversation_id,
                    name.as_deref(),
                    topic.as_deref(),
                    rate_limit_per_user,
                    nsfw,
                    user_limit,
                    parent_id.as_ref().map(|p| p.as_deref()),
                ),
            ),
            Command::MoveChannels { space_id, order, reply } => {
                answer(reply, self.move_channels(&space_id, &order))
            }
            Command::DeleteChannel { conversation_id, reply } => {
                answer(reply, self.delete_channel(&conversation_id))
            }
            Command::ChannelPermissions { conversation_id, reply } => {
                answer(reply, self.channel_permissions(&conversation_id))
            }
            Command::SetOverwrite { conversation_id, target_id, target_kind, allow, deny, reply } => {
                answer(reply, self.set_overwrite(&conversation_id, &target_id, target_kind, allow, deny))
            }
            Command::DeleteOverwrite { conversation_id, target_id, reply } => {
                answer(reply, self.delete_overwrite(&conversation_id, &target_id))
            }
            Command::SyncChannelPermissions { conversation_id, reply } => {
                answer(reply, self.sync_channel_permissions(&conversation_id))
            }
            Command::AuditLog { space_id, person, action, before, limit, reply } => {
                answer(reply, self.audit_log(&space_id, person.as_deref(), action, before.as_deref(), limit))
            }
            Command::AutomodRules { space_id, reply } => answer(reply, self.automod_rules(&space_id)),
            Command::SaveAutomodRule { space_id, rule, reply } => answer(reply, self.save_automod_rule(&space_id, &rule)),
            Command::DeleteAutomodRule { space_id, rule_id, reply } => answer(reply, self.delete_automod_rule(&space_id, &rule_id)),
            Command::SaveSpaceSafety { space_id, safety, reply } => answer(reply, self.save_space_safety(&space_id, &safety)),
            Command::Discover { query, category, offset, limit, reply } => {
                answer(reply, self.discover(&query, category, offset, limit))
            }
            Command::JoinDiscovered { guild_id, reply } => answer(reply, self.join_discovered(&guild_id)),
            Command::SpaceGate { space_id, reply } => answer(reply, self.space_gate(&space_id)),
            Command::AcceptRules { space_id, answers, reply } => answer(reply, self.accept_rules(&space_id, &answers)),
            Command::SubmitOnboarding { space_id, options, reply } => answer(reply, self.submit_onboarding(&space_id, &options)),
            Command::LeaveSpace { space_id, reply } => answer(reply, self.leave_space(&space_id)),
            Command::SpaceIntegrations { space_id, reply } => answer(reply, self.space_integrations(&space_id)),
            Command::CreateWebhook { conversation_id, name, reply } => {
                answer(reply, self.create_webhook(&conversation_id, &name))
            }
            Command::EditWebhook { space_id, webhook_id, name, channel_id, reply } => {
                answer(reply, self.edit_webhook(&space_id, &webhook_id, name, channel_id))
            }
            Command::DeleteWebhook { space_id, webhook_id, reply } => {
                answer(reply, self.delete_webhook(&space_id, &webhook_id))
            }
            Command::RemoveIntegration { space_id, integration_id, reply } => {
                answer(reply, self.remove_integration(&space_id, &integration_id))
            }
            Command::SpaceExpressions { space_id, reply } => answer(reply, self.space_expressions(&space_id)),
            Command::CreateEmoji { space_id, name, path, reply } => {
                answer(reply, self.create_emoji(&space_id, &name, &path))
            }
            Command::EditEmoji { space_id, emoji_id, name, roles, reply } => {
                answer(reply, self.edit_emoji(&space_id, &emoji_id, name, roles))
            }
            Command::DeleteEmoji { space_id, emoji_id, reply } => {
                answer(reply, self.delete_emoji(&space_id, &emoji_id))
            }
            Command::CreateSticker { space_id, name, tags, description, path, reply } => {
                answer(reply, self.create_sticker(&space_id, &name, &tags, &description, &path))
            }
            Command::EditSticker { space_id, sticker_id, name, tags, description, reply } => {
                answer(reply, self.edit_sticker(&space_id, &sticker_id, name, tags, description))
            }
            Command::DeleteSticker { space_id, sticker_id, reply } => {
                answer(reply, self.delete_sticker(&space_id, &sticker_id))
            }
            Command::CloseDirect { conversation_id, reply } => answer(reply, self.leave(&conversation_id, true, false)),
            Command::AnswerMessageRequest { conversation_id, accept, reply } => {
                answer(reply, self.answer_message_request(&conversation_id, accept))
            }
            Command::Pinned { conversation_id, reply } => answer(reply, self.pinned(&conversation_id)),
            Command::Pin { conversation_id, message_id, pinned, reply, .. } => {
                answer(reply, self.pin(&conversation_id, &message_id, pinned))
            }
            Command::Forward { from_conversation_id, message_ids, to_conversation_ids, note, reply } => {
                answer(reply, self.forward(&from_conversation_id, &message_ids, &to_conversation_ids, &note))
            }
            Command::Report { target, reason, note: _, block: _, reply } => answer(reply, self.file_report(&target, reason)),
            Command::ReportReasons { target, reply } => answer(reply, report_reasons(&target)),
            Command::Publish { conversation_id, message_id, reply } => {
                answer(reply, self.publish(&conversation_id, &message_id))
            }
            Command::Follow { conversation_id, into_channel_id, reply } => {
                answer(reply, self.follow_announcement(&conversation_id, &into_channel_id))
            }
            Command::SearchShared { conversation_id, kind, before, limit, reply } => {
                answer(reply, self.search_shared(&conversation_id, kind, before.as_deref(), limit))
            }
            Command::Mentions { before, everyone, roles, space_id, limit, reply } => {
                answer(reply, self.mentions(before.as_deref(), everyone, roles, space_id.as_deref(), limit))
            }
            Command::DismissMention { message_id, reply } => answer(reply, self.dismiss_mention(&message_id)),
            Command::MoveSpace { space_id, before, folder, into_space, reply } => {
                answer(reply, self.move_space(&space_id, before.as_deref(), folder.as_deref(), into_space.as_deref()))
            }
            Command::SetFolder { folder_id, name, color, reply } => {
                answer(reply, self.set_folder(&folder_id, name, color))
            }
            Command::Search { reply, .. } => drop(reply.send(Err(Refusal::Unsupported))),
            Command::Catalog { kind, space_id, query, cursor, reply } => {
                answer(reply, self.catalog(kind, space_id.as_deref(), &query, cursor.as_deref()))
            }
            Command::Favourite { kind, id, on, reply } => answer(reply, self.favourite(kind, &id, on)),
            Command::ReactionRules { conversation_id, reply } => drop(reply.send(Ok(self.reaction_rules(&conversation_id)))),
            Command::CancelSend { client_id, .. } => upload::cancel(&self.ctx.account, &client_id),
            // Never: a fetch per DM is the burst that flags accounts. chatd does not send it,
            // and it is refused here too should it ever arrive.
            Command::Previews(_) => {}
            Command::Typing { conversation_id, activity: _ } => self.typing(&conversation_id),
            Command::Watch(watching) => {
                self.watch(&watching);
                self.voice_watch(&watching);
            }
            Command::SearchPeople(query) => self.search_people(&query),
            Command::Members { conversation_id, offset, limit, reply } => {
                self.members(conversation_id, offset, limit, reply)
            }
            Command::StartGroup { people, title, reply } => answer(reply, self.start_group(&people, title.as_deref())),
            Command::MembersRemove { conversation_id, person, reply } => {
                answer(reply, self.remove_from_group(&conversation_id, &person))
            }
            Command::ConversationUpdate { conversation_id, title, topic, description: _, reply } => {
                // A group DM is renamed; anything else is a server channel's name and topic.
                let group = self.ctx.known(&conversation_id).ok().flatten().is_some_and(|row| row.kind == ConversationKind::Group);
                let done = if group {
                    self.update_group(&conversation_id, title.as_deref())
                } else {
                    self.edit_channel(&conversation_id, title.as_deref(), topic.as_deref(), None, None, None, None)
                };
                answer(reply, done)
            }
            Command::ConversationPicture { conversation_id, path, reply } => {
                answer(reply, self.set_group_icon(&conversation_id, &path))
            }
            Command::MembersAdd { conversation_id, people, reply, .. } => {
                // A thread takes thread members; anything else Discord lets us add to is a group DM.
                let added = match self.add_members(&conversation_id, &people) {
                    Err(Refusal::Unsupported) => self.add_to_group(&conversation_id, &people),
                    done => done,
                };
                answer(reply, added)
            }
            Command::FollowThread { conversation_id, on, reply } => {
                answer(reply, self.follow_thread(&conversation_id, on))
            }
            Command::ChannelThreads { conversation_id, archived, query, cursor, reply } => {
                answer(reply, self.channel_threads(&conversation_id, archived, &query, cursor.as_deref()))
            }
            Command::ThreadUpdate {
                conversation_id,
                name,
                archived,
                locked,
                auto_archive_minutes,
                slowmode_seconds,
                reply,
            } => answer(
                reply,
                self.thread_update(&conversation_id, name, archived, locked, auto_archive_minutes, slowmode_seconds),
            ),
            Command::DeleteThread { conversation_id, reply } => {
                answer(reply, self.delete_thread(&conversation_id))
            }
            Command::StartDirect { person_id, reply } => drop(reply.send(self.start_direct(&person_id))),
            Command::StartThread {
                conversation_id,
                message_id,
                name,
                private,
                auto_archive_minutes,
                first_message,
                reply,
            } => drop(reply.send(self.start_thread(
                &conversation_id,
                message_id.as_deref(),
                &name,
                private,
                auto_archive_minutes,
                &first_message,
            ))),
            Command::ForumInfo { conversation_id, reply } => answer(reply, self.forum_info(&conversation_id)),
            Command::ForumPost { conversation_id, title, text, tags, with, reply } => {
                answer(reply, self.forum_post(&conversation_id, &title, &text, &tags, &with))
            }
            Command::Download { id, url, reply } => self.download(id, url, reply),
            Command::Handoff { conversation_id, reply } => drop(reply.send(self.handoff(&conversation_id))),
            Command::StageVoice { conversation_id, action, person_id, reply } => {
                answer(reply, self.stage_voice(&conversation_id, action, person_id.as_deref()))
            }
            Command::Soundboard { conversation_id, reply } => answer(reply, self.soundboard(&conversation_id)),
            Command::PlaySoundboard { conversation_id, sound_id, guild_id, reply } => {
                answer(reply, self.play_soundboard(&conversation_id, &sound_id, guild_id.as_deref()))
            }
            Command::Vote { conversation_id, message_id, votes, reply } => {
                answer(reply, self.vote_poll(&conversation_id, &message_id, votes))
            }
            Command::SendRules { conversation_id, reply } => answer(reply, self.poll_send_rules(&conversation_id)),
            Command::PollVoters { conversation_id, message_id, choice, after, reply } => {
                answer(reply, self.poll_voters(&conversation_id, &message_id, choice, after.as_deref()))
            }
            Command::DiscordVoiceSettings { conversation_id, reply } => {
                answer(reply, self.discord_voice_settings(&conversation_id))
            }
            Command::EditDiscordVoiceChannel { conversation_id, edit, reply } => {
                answer(reply, self.edit_discord_voice_channel(&conversation_id, edit))
            }
            Command::SetDiscordCallRegion { conversation_id, region, reply } => {
                answer(reply, self.set_discord_call_region(&conversation_id, &region))
            }
            Command::SetVoiceChannelStatus { conversation_id, status, reply } => {
                answer(reply, self.set_voice_status(&conversation_id, status))
            }
            Command::CallModerate { conversation_id, person_id, server_muted, server_deafened, reply } => {
                answer(reply, self.moderate_voice(&conversation_id, &person_id, server_muted, server_deafened))
            }
            Command::CallMove { conversation_id, person_id, to, reply } => {
                answer(reply, self.move_in_voice(&conversation_id, &person_id, to.as_deref()))
            }
            Command::CallJoin(conversation_id) => self.call_join(&conversation_id),
            Command::CallLeave(conversation_id) => self.call_leave(&conversation_id),
            Command::CallAudio { muted, deafened } => self.call_audio(muted, deafened),
            Command::CallPicture(picture) => self.call_picture(picture),
            Command::CallRing { conversation_id, people, reply } => {
                answer(reply, self.call_ring(&conversation_id, people))
            }
            Command::CallStopRinging { conversation_id, people, reply } => {
                answer(reply, self.call_stop_ringing(&conversation_id, people))
            }
            Command::CallAddPeople { conversation_id, people, reply } => {
                answer(reply, self.call_add_people(&conversation_id, people))
            }
            Command::MessageAction {
                conversation_id,
                message_id,
                block_id,
                action_id,
                element_type,
                value,
                values,
                reply,
            } => answer(
                reply,
                self.message_action(
                    &conversation_id,
                    &message_id,
                    &block_id,
                    &action_id,
                    &element_type,
                    &value,
                    values,
                ).map(|()| None),
            ),
            Command::Commands { conversation_id, query, kind, reply } => {
                answer(reply, self.commands(&conversation_id, &query, kind))
            }
            Command::CommandAutocomplete {
                conversation_id,
                command_id,
                application_id,
                version,
                name,
                options,
                focused,
                reply,
            } => answer(
                reply,
                self.command_autocomplete(&conversation_id, &command_id, &application_id, &version, &name, &options, &focused),
            ),
            Command::RunCommand {
                conversation_id,
                command_id,
                application_id,
                version,
                kind,
                name,
                arguments,
                target_id,
                files,
                reply,
            } => answer(
                reply,
                self.run_command(
                    &conversation_id,
                    &command_id,
                    &application_id,
                    &version,
                    kind,
                    &name,
                    &arguments,
                    target_id.as_deref(),
                    &files,
                ),
            ),
            Command::ModalSubmit {
                conversation_id,
                modal_id,
                application_id,
                custom_id,
                values,
                reply,
            } => answer(
                reply,
                self.modal_submit(&conversation_id, &modal_id, &application_id, &custom_id, values),
            ),
            // The user asked to sign out, so the token should stop working, not just be
            // forgotten: the answer goes once Discord has ended the session, or refused to.
            Command::SignOut(done) => {
                if let Err(err) = self.ctx.drop_ephemeral() {
                    self.log("sign out", err);
                }
                let body = json!({"provider": null, "voip_provider": null});
                if let Err(refused) = self.call("POST", "/auth/logout", Some(body)) {
                    self.log("sign out", refused);
                }
                let _ = done.send(Ok(()));
            }
            command => command.unsupported(&self.ctx),
        }
    }

    /// Creates a new Discord server (`POST /guilds`). Uses `call_once_with` so a 429 or captcha
    /// challenge fails cleanly without retrying or locking the whole session.
    fn create_space(
        &self,
        name: &str,
        icon_path: Option<&str>,
        template: Option<&str>,
    ) -> Result<(String, Option<String>), Refusal> {
        let icon_uri = match icon_path.map(str::trim).filter(|p| !p.is_empty()) {
            Some(path) => Some(read_image_data_uri(path, DISCORD_IMAGE_MAX)?),
            None => None,
        };
        let body = build_create_guild_body(name, icon_uri.as_deref(), template)?;
        let raw = self
            .rest
            .call_once_with("POST", "/guilds", Some(body), &[])
            .map_err(create_space_refusal)?;
        let parsed: Value = serde_json::from_str(&raw).map_err(|err| Refusal::Refused(err.to_string()))?;
        let id = parsed
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Refusal::Refused("Discord did not return the new server id.".to_owned()))?
            .to_owned();
        let system_channel = parsed
            .get("system_channel_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Ok((id, system_channel))
    }
}

/// Maximum size for Discord guild icons and banners (8 MiB).
pub(super) const DISCORD_IMAGE_MAX: usize = 8 * 1024 * 1024;

/// Identifies PNG, JPEG, GIF, or WebP from file magic bytes.
pub(super) fn detect_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Safely reads an allowed local image file and encodes it as a `data:<mime>;base64,...` URI
/// suitable for Discord's guild/role image fields.
pub(super) fn read_image_data_uri(path: &str, max_bytes: usize) -> Result<String, Refusal> {
    use std::io::Read;
    let (real, _) = upload::allowed(path).map_err(|why| Refusal::Refused(format!("That picture {why}")))?;
    let mut bytes = Vec::new();
    std::fs::File::open(&real)
        .and_then(|file| file.take(max_bytes as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| Refusal::Refused("That picture could not be read.".to_owned()))?;
    if bytes.len() > max_bytes {
        let mb = (max_bytes / (1024 * 1024)).max(1);
        return Err(Refusal::Refused(format!("That picture is too big (limit {mb} MiB).")));
    }
    let Some(mime) = detect_image_mime(&bytes) else {
        return Err(Refusal::Refused("That picture isn't a PNG, JPEG, GIF, or WebP.".to_owned()));
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

/// Builds the JSON payload for `POST /guilds`, including optional starter channels for templates.
fn build_create_guild_body(name: &str, icon_uri: Option<&str>, template: Option<&str>) -> Result<Value, Refusal> {
    let name = name.trim();
    let chars = name.chars().count();
    if !(2..=100).contains(&chars) {
        return Err(Refusal::Refused("Server name must be between 2 and 100 characters.".to_owned()));
    }
    let (channels, system_channel_id): (Vec<Value>, Option<&str>) = match template.map(str::trim).unwrap_or("") {
        "gaming" => (
            vec![
                json!({"id": "1", "type": 4, "name": "Text Channels"}),
                json!({"id": "2", "type": 0, "name": "general", "parent_id": "1"}),
                json!({"id": "3", "type": 0, "name": "clips-and-highlights", "parent_id": "1"}),
                json!({"id": "4", "type": 4, "name": "Voice Channels"}),
                json!({"id": "5", "type": 2, "name": "Lobby", "parent_id": "4"}),
                json!({"id": "6", "type": 2, "name": "Gaming", "parent_id": "4"}),
            ],
            Some("2"),
        ),
        "study" => (
            vec![
                json!({"id": "1", "type": 4, "name": "Information"}),
                json!({"id": "2", "type": 0, "name": "general", "parent_id": "1"}),
                json!({"id": "3", "type": 0, "name": "homework-help", "parent_id": "1"}),
                json!({"id": "4", "type": 0, "name": "resources", "parent_id": "1"}),
                json!({"id": "5", "type": 4, "name": "Study Rooms"}),
                json!({"id": "6", "type": 2, "name": "Study Room", "parent_id": "5"}),
            ],
            Some("2"),
        ),
        "friends" => (
            vec![
                json!({"id": "1", "type": 4, "name": "Text Channels"}),
                json!({"id": "2", "type": 0, "name": "general", "parent_id": "1"}),
                json!({"id": "3", "type": 0, "name": "memes", "parent_id": "1"}),
                json!({"id": "4", "type": 4, "name": "Voice Channels"}),
                json!({"id": "5", "type": 2, "name": "Lounge", "parent_id": "4"}),
            ],
            Some("2"),
        ),
        "community" => (
            vec![
                json!({"id": "1", "type": 4, "name": "Welcome"}),
                json!({"id": "2", "type": 0, "name": "welcome", "parent_id": "1"}),
                json!({"id": "3", "type": 0, "name": "announcements", "parent_id": "1"}),
                json!({"id": "4", "type": 0, "name": "general", "parent_id": "1"}),
                json!({"id": "5", "type": 4, "name": "Voice"}),
                json!({"id": "6", "type": 2, "name": "Town Hall", "parent_id": "5"}),
            ],
            Some("2"),
        ),
        _ => (Vec::new(), None),
    };
    Ok(json!({
        "name": name,
        "icon": icon_uri,
        "channels": channels,
        "system_channel_id": system_channel_id,
    }))
}

/// Translates Discord's refusal codes on `POST /guilds` into clear words.
fn create_space_refusal(err: Refused) -> Refusal {
    match err {
        Refused::Challenge => Refusal::Refused(
            "Discord asked for a captcha check tinyclient cannot complete. Create this server in Discord.".to_owned(),
        ),
        Refused::Limited(_) | Refused::Paused(_) | Refused::Status { status: 429, .. } => {
            Refusal::Refused("Too many server creation requests. Wait a moment and try again.".to_owned())
        }
        Refused::Status { code: 30001, .. } => {
            Refusal::Refused("You have reached Discord's server limit (100 servers, or 200 with Nitro).".to_owned())
        }
        Refused::Status { code: 40002, .. } => {
            Refusal::Refused("Verify your Discord email address before creating a server.".to_owned())
        }
        Refused::Status { code: 50035, .. } => {
            Refusal::Refused("Check the server name (2 to 100 characters) and picture, then try again.".to_owned())
        }
        Refused::Status { status: 400, ref message, .. } if message.to_ascii_lowercase().contains("captcha") => {
            Refusal::Refused(
                "Discord asked for a captcha check tinyclient cannot complete. Create this server in Discord.".to_owned(),
            )
        }
        other => refusal(other),
    }
}

/// A new folder id, a snowflake from now, distinct from the folders already stored.
fn fresh_folder_id(folders: &[proto::GuildFolder]) -> i64 {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as u64).unwrap_or(0);
    let snowflake = millis.saturating_sub(1_420_070_400_000) << 22;
    let mut id = i64::try_from(snowflake).unwrap_or(1).max(1);
    for _ in 0..64 {
        if !folders.iter().any(|folder| folder.id == Some(id)) {
            return id;
        }
        id = id.saturating_add(1);
    }
    id
}
/// Message-menu node ids (`POST /reporting/message`). A path Discord does not have is a 400,
/// so Other is refused. Illegal content uses the illicit-goods leaf, not a child-safety one.
fn message_report(reason: protocol::ReportReason) -> Option<(&'static str, &'static [u64])> {
    match reason {
        protocol::ReportReason::Spam => Some(("98", &[7, 98])),
        protocol::ReportReason::Harassment => Some(("101", &[7, 76, 101])),
        protocol::ReportReason::Violence => Some(("117", &[7, 90, 117])),
        protocol::ReportReason::Illegal => Some(("124", &[7, 80, 124])),
        protocol::ReportReason::Impersonation => Some(("163", &[7, 80, 121, 163])),
        protocol::ReportReason::Other => None,
    }
}

/// The body `POST /channels/{target}/messages` sends for one forwarded message.
fn forward_message_body(from: &str, message_id: &str, guild: Option<&str>) -> Value {
    let mut reference = json!({
        "type": 1,
        "channel_id": from,
        "message_id": message_id,
    });
    if let Some(guild) = guild {
        reference["guild_id"] = json!(guild);
    }
    json!({
        "content": "",
        "message_reference": reference,
    })
}

/// `TINYCLIENT_PROVE=discord-forward-report`. A forward is an empty type-1 reference, a
/// report uses a captured message-menu leaf, and every snapshot is shown.
pub(super) fn prove_forward_report() -> Result<(), String> {
    let body = forward_message_body("4101", "1300000000000000100", Some("4000"));
    let reference = &body["message_reference"];
    if body["content"] != ""
        || reference["type"] != 1
        || reference["channel_id"] != "4101"
        || reference["message_id"] != "1300000000000000100"
        || reference["guild_id"] != "4000"
    {
        return Err("a forward is an empty message with a type-1 reference and the source guild".to_owned());
    }
    let (variant, crumbs) = message_report(protocol::ReportReason::Spam).ok_or("spam is a message report")?;
    if variant != "98" || crumbs != [7, 98] {
        return Err("spam uses the captured message-menu leaf".to_owned());
    }
    if message_report(protocol::ReportReason::Other).is_some() {
        return Err("Discord has no general report, so Other is refused".to_owned());
    }

    struct Known;
    impl Names for Known {
        fn user(&self, _: &str) -> Option<String> { None }
        fn role(&self, _: &str) -> Option<String> { None }
        fn channel(&self, id: &str) -> Option<String> { (id == "4101").then(|| "general".to_owned()) }
        fn space(&self, id: &str) -> Option<String> { (id == "4000").then(|| "Rustaceans".to_owned()) }
    }
    let account = protocol::AccountKey { network: protocol::Network::Discord, account: "acct".into() };
    let place = Place { account: &account, me: "me", guild: None, names: &Known, author: None, blur_explicit: false };
    let snap = |text: &str, id: &str| Posted {
        content: Some(text.to_owned()),
        attachments: Some(vec![model::Attachment { id: id.into(), filename: format!("{id}.txt"), size: 12, url: format!("https://cdn.discordapp.com/attachments/4101/{id}/{id}.txt"), ..Default::default() }]),
        ..Default::default()
    };
    let posted = Posted {
        id: "1300000000000000999".into(),
        channel_id: "3001".into(),
        message_reference: Some(model::Reference {
            kind: 1,
            channel_id: Some("4101".into()),
            message_id: Some("1300000000000000100".into()),
            guild_id: Some("4000".into()),
        }),
        message_snapshots: vec![
            model::Snapshot { message: Box::new(snap("First snapshot", "1")) },
            model::Snapshot { message: Box::new(snap("Second snapshot", "2")) },
        ],
        ..Default::default()
    };
    let (msg, _) = content::message(&posted, &place);
    if !msg.text.contains("First snapshot") || !msg.text.contains("Second snapshot") || msg.attachments.len() != 2 {
        return Err("every forwarded snapshot and its file is shown".to_owned());
    }
    let forwarded = msg.forwarded.as_ref().ok_or("a type-1 reference names where it came from")?;
    if forwarded.from_title != "#general" || forwarded.from_space != "Rustaceans" {
        return Err("a forward names the channel and the server".to_owned());
    }
    Ok(())
}

fn report_reasons(target: &protocol::ReportTarget) -> Result<Vec<protocol::ReportReason>, Refusal> {
    match target {
        protocol::ReportTarget::Message { .. } => Ok(vec![
            protocol::ReportReason::Spam,
            protocol::ReportReason::Harassment,
            protocol::ReportReason::Violence,
            protocol::ReportReason::Illegal,
            protocol::ReportReason::Impersonation,
        ]),
        _ => Err(Refusal::Unsupported),
    }
}

/// Discord's thread name: one line, at most 100 characters. Empty becomes "Thread".
fn thread_name(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = flat.chars().take(100).collect();
    let name = name.trim().to_owned();
    if name.is_empty() { "Thread".to_owned() } else { name }
}

/// Discord will not move this channel's read marker. 50007 is "cannot message this user"
/// (blocked, or they closed DMs). 50001 and 50003 are missing access and an unknown channel.
fn ack_refused(refused: &Refused) -> bool {
    matches!(refused, Refused::Status { code: 50007 | 50001 | 50003, .. } | Refused::Status { status: 403, .. })
}

/// Parses a user-typed Discord username (`name`, `@name`, or legacy `name#1234`).
fn parse_discord_username(raw: &str) -> Result<(String, Option<u16>), String> {
    let trimmed = raw.trim().trim_start_matches('@').trim();
    if trimmed.is_empty() {
        return Err("Enter a Discord username.".to_owned());
    }
    if let Some((name, disc)) = trimmed.split_once('#') {
        let name = name.trim();
        let disc = disc.trim();
        if name.is_empty() {
            return Err("Enter a username like name or name#1234.".to_owned());
        }
        if disc == "0" || disc == "0000" {
            return Ok((name.to_owned(), None));
        }
        if disc.len() == 4 && disc.bytes().all(|b| b.is_ascii_digit()) {
            let parsed = disc.parse::<u16>().map_err(|_| "Enter a username like name or name#1234.".to_owned())?;
            return Ok((name.to_owned(), Some(parsed)));
        }
        return Err("Enter a username like name or name#1234.".to_owned());
    }
    Ok((trimmed.to_owned(), None))
}

/// Translates Discord's refusal codes on `POST /users/@me/relationships` into clear words.
fn add_friend_refusal(err: Refused) -> Refusal {
    match err {
        Refused::Challenge => Refusal::Refused(
            "Discord asked for a captcha check tinyclient cannot complete. Send this friend request in Discord.".to_owned(),
        ),
        Refused::Limited(_) | Refused::Paused(_) | Refused::Status { status: 429, .. } => {
            Refusal::Refused("Too many friend requests. Wait a moment and try again.".to_owned())
        }
        Refused::Status { code: 80000 | 80003, .. } => {
            Refusal::Refused("You cannot send a friend request to yourself.".to_owned())
        }
        Refused::Status { code: 80001 | 80004, .. } => {
            Refusal::Refused("No Discord user was found with that username. Check the spelling and try again.".to_owned())
        }
        Refused::Status { code: 80002, .. } => {
            Refusal::Refused("You have blocked this user, or they have blocked you.".to_owned())
        }
        Refused::Status { code: 80007, .. } => Refusal::Refused("This person is already your friend.".to_owned()),
        Refused::Status { code: 50007 | 80008, .. } => {
            Refusal::Refused("This person does not accept friend requests.".to_owned())
        }
        Refused::Status { code: 30001 | 30002 | 80006, .. } => {
            Refusal::Refused("You have reached Discord's friend limit.".to_owned())
        }
        Refused::Status { status: 400, ref message, .. } if message.to_ascii_lowercase().contains("captcha") => {
            Refusal::Refused(
                "Discord asked for a captcha check tinyclient cannot complete. Send this friend request in Discord.".to_owned(),
            )
        }
        other => refusal(other),
    }
}

fn presence_word(presence: Presence) -> &'static str {
    match presence {
        Presence::Idle => "idle",
        Presence::Dnd => "dnd",
        Presence::Offline => "invisible",
        Presence::Online | Presence::Unknown | Presence::Busy => "online",
    }
}

/// This session's gateway activities: the custom status, then the local "Playing" name.
/// Empty is an empty list, which clears only this session. No application id is invented.
fn session_activities(status: &str, playing: &str) -> Value {
    let mut activities = Vec::new();
    if !status.is_empty() {
        activities.push(json!({"name": "Custom Status", "type": 4, "state": status, "emoji": null}));
    }
    if !playing.is_empty() {
        activities.push(json!({"name": playing, "type": 0}));
    }
    Value::Array(activities)
}

fn parse_body(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Value::Null;
    }
    serde_json::from_str(trimmed).unwrap_or(Value::Null)
}

fn clip_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn text_at(value: &Value, key: &str, max: usize) -> String {
    value.get(key).and_then(Value::as_str).map(|text| clip_chars(text, max)).unwrap_or_default()
}

fn string_list(value: Option<&Value>, max_items: usize, max_chars: usize) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().filter_map(Value::as_str).take(max_items).map(|text| clip_chars(text, max_chars)).collect()
        })
        .unwrap_or_default()
}

fn choice_list(value: Option<&Value>) -> Vec<String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .take(12)
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("label").and_then(Value::as_str))
                .or_else(|| item.get("value").and_then(Value::as_str))
                .map(|text| clip_chars(text, 200))
        })
        .collect()
}

/// Copies the form's words. It does not request anything those words mention.
fn fill_form(gate: &mut protocol::SpaceGate, form: &Value) {
    if let Some(description) = form.get("description").and_then(Value::as_str) {
        gate.description = clip_chars(description, 2000);
    }
    let Some(fields) = form.get("form_fields").and_then(Value::as_array) else {
        return;
    };
    gate.fields.clear();
    for field in fields.iter().take(5) {
        gate.fields.push(protocol::GateField {
            label: text_at(field, "label", 200),
            kind: text_at(field, "field_type", 40),
            values: string_list(field.get("values"), 8, 2000),
            required: field.get("required").and_then(Value::as_bool).unwrap_or(false),
            choices: choice_list(field.get("choices")),
        });
    }
}

fn fill_prompts(gate: &mut protocol::SpaceGate, board: &Value) {
    let Some(prompts) = board.get("prompts").and_then(Value::as_array) else {
        return;
    };
    gate.prompts.clear();
    for prompt in prompts.iter().take(12) {
        let mut options = Vec::new();
        if let Some(list) = prompt.get("options").and_then(Value::as_array) {
            for option in list.iter().take(25) {
                let Some(id) = option.get("id").and_then(Value::as_str).filter(|id| discovery_snowflake(id)) else {
                    continue;
                };
                let label = option.get("title").or_else(|| option.get("label")).and_then(Value::as_str).unwrap_or("");
                options.push(protocol::GateOption { id: id.to_owned(), label: clip_chars(label, 200) });
            }
        }
        let label = prompt.get("title").or_else(|| prompt.get("label")).and_then(Value::as_str).unwrap_or("");
        gate.prompts.push(protocol::GatePrompt { label: clip_chars(label, 200), options });
    }
}

fn prompt_option_ids(board: &Value) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Some(prompts) = board.get("prompts").and_then(Value::as_array) else {
        return ids;
    };
    for prompt in prompts {
        let Some(list) = prompt.get("options").and_then(Value::as_array) else {
            continue;
        };
        for option in list {
            if let Some(id) = option.get("id").and_then(Value::as_str).filter(|id| discovery_snowflake(id)) {
                ids.insert(id.to_owned());
            }
        }
    }
    ids
}

fn seen_maps(board: &Value, picked: &[String]) -> (serde_json::Map<String, Value>, serde_json::Map<String, Value>) {
    let mut prompts_seen = serde_json::Map::new();
    let mut responses_seen = serde_json::Map::new();
    let Some(prompts) = board.get("prompts").and_then(Value::as_array) else {
        return (prompts_seen, responses_seen);
    };
    for prompt in prompts {
        if let Some(id) = prompt.get("id").and_then(Value::as_str).filter(|id| discovery_snowflake(id)) {
            prompts_seen.insert(id.to_owned(), Value::Bool(true));
        }
        let Some(list) = prompt.get("options").and_then(Value::as_array) else {
            continue;
        };
        for option in list {
            let Some(id) = option.get("id").and_then(Value::as_str) else {
                continue;
            };
            if picked.iter().any(|picked| picked == id) {
                responses_seen.insert(id.to_owned(), Value::Bool(true));
            }
        }
    }
    (prompts_seen, responses_seen)
}

/// Pending is cleared only when Discord says the join is through.
fn accept_cleared(body: &Value) -> bool {
    let approved = |value: &Value| {
        value.get("pending") == Some(&Value::Bool(false))
            || value.get("application_status").and_then(Value::as_str) == Some("APPROVED")
    };
    approved(body) || body.get("member").is_some_and(approved) || body.get("guild_member").is_some_and(approved)
}

/// Digits only. A URL is not a server id.
fn discovery_snowflake(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// A vanity code from a listing: letters, digits, and hyphens. Never a URL.
fn discovery_code(code: &str) -> bool {
    (2..=32).contains(&code.len()) && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Discord's icon hash. A slash or a colon means it is a URL, which is dropped.
fn icon_hash(hash: &str) -> bool {
    (1..=64).contains(&hash.len()) && hash.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Extracts a Discord invite code from `discord.gg/<code>`, `discord.com/invite/<code>`,
/// `discordapp.com/invite/<code>`, or a bare invite code.
fn parse_discord_invite_code(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches(|c| matches!(c, '<' | '>' | '"' | '\''));
    if trimmed.is_empty() {
        return Err("Enter a valid Discord invite link or code.".to_owned());
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let without_www = without_scheme.strip_prefix("www.").unwrap_or(without_scheme);
    let candidate = if let Some(rest) = without_www.strip_prefix("discord.gg/") {
        rest
    } else if let Some(rest) = without_www.strip_prefix("discord.com/invite/") {
        rest
    } else if let Some(rest) = without_www.strip_prefix("discordapp.com/invite/") {
        rest
    } else if without_www.contains('/') || without_www.contains('.') {
        return Err("Enter a valid Discord invite link or code.".to_owned());
    } else {
        without_www
    };
    let code = candidate
        .split(['?', '#', '/'])
        .next()
        .unwrap_or("")
        .trim();
    if code.len() < 2
        || code.len() > 64
        || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Enter a valid Discord invite link or code.".to_owned());
    }
    Ok(code.to_owned())
}

/// Translates Discord's refusal codes on `/invites/{code}` into clear words.
fn invite_refusal(err: Refused) -> Refusal {
    match err {
        Refused::Challenge => Refusal::Refused(
            "Discord asked for a captcha check tinyclient cannot complete. Join this server in Discord.".to_owned(),
        ),
        Refused::Limited(_) | Refused::Paused(_) | Refused::Status { status: 429, .. } => {
            Refusal::Refused("Too many invite requests. Wait a moment and try again.".to_owned())
        }
        Refused::Status { code: 10006, .. } | Refused::Status { status: 404, .. } => {
            Refusal::Refused("This invite is invalid or has expired.".to_owned())
        }
        Refused::Status { code: 40007, .. } => Refusal::Refused("You are banned from this server.".to_owned()),
        Refused::Status { code: 30001, .. } => {
            Refusal::Refused("You have reached Discord's server limit (100 servers, or 200 with Nitro).".to_owned())
        }
        Refused::Status { code: 50013 | 50001, .. } => {
            Refusal::Refused("You do not have permission to join this server.".to_owned())
        }
        Refused::Status { status: 400, ref message, .. } if message.to_ascii_lowercase().contains("captcha") => {
            Refusal::Refused(
                "Discord asked for a captcha check tinyclient cannot complete. Join this server in Discord.".to_owned(),
            )
        }
        other => refusal(other),
    }
}

/// Parses a Discord `guild_scheduled_event` JSON object into [`protocol::ScheduledEvent`].
fn parse_scheduled_event(ev: &Value, fallback_guild: Option<&str>) -> Option<protocol::ScheduledEvent> {
    let id = ev.get("id").and_then(Value::as_str)?.to_owned();
    let guild_id = ev
        .get("guild_id")
        .and_then(Value::as_str)
        .or(fallback_guild)
        .unwrap_or_default()
        .to_owned();
    let name = ev.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
    let start = ev
        .get("scheduled_start_time")
        .and_then(Value::as_str)
        .and_then(model::iso_seconds);
    let place = ev
        .get("entity_metadata")
        .and_then(|m| m.get("location"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let interested_count = ev.get("user_count").and_then(Value::as_u64).map(|n| n as u32);
    Some(protocol::ScheduledEvent {
        id,
        guild_id,
        name,
        start,
        place,
        interested_count,
        interested: false,
    })
}

/// Sends the answer to a command the shell is waiting on.
fn answer<T>(reply: Reply<T>, outcome: Result<T, Refusal>) {
    let _ = reply.send(outcome);
}

/// Builds `POST /read-states/ack-bulk` request bodies in chunks of 50, skipping empty message ids.
pub(super) fn ack_bulk_payloads(reads: &[(String, String)]) -> Vec<Value> {
    let valid: Vec<(&str, &str)> = reads
        .iter()
        .filter(|(_, message)| !message.is_empty())
        .map(|(channel, message)| (channel.as_str(), message.as_str()))
        .collect();
    valid
        .chunks(50)
        .map(|chunk| {
            let states: Vec<Value> = chunk
                .iter()
                .map(|(channel, message)| json!({"channel_id": channel, "message_id": message, "read_state_type": 0}))
                .collect();
            json!({"read_states": states})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_number_is_not_kept_and_a_missing_one_is_unknown() {
        let missing: User = serde_json::from_str(r#"{"id":"1","username":"a"}"#).unwrap();
        assert_eq!(missing.phone, None);
        assert_eq!(missing.verified, None);
        let empty: User = serde_json::from_str(r#"{"id":"1","username":"a","phone":null,"verified":null}"#).unwrap();
        assert_eq!(empty.phone, Some(false));
        assert_eq!(empty.verified, None);
        let blank: User = serde_json::from_str(r#"{"id":"1","username":"a","phone":"","verified":false}"#).unwrap();
        assert_eq!(blank.phone, Some(false));
        assert_eq!(blank.verified, Some(false));
        let set: User = serde_json::from_str(r#"{"id":"1","username":"a","phone":"+15555550100","verified":true}"#).unwrap();
        assert_eq!(set.phone, Some(true));
        assert!(!format!("{set:?}").contains("555"));
    }

    #[test]
    fn the_account_is_named_by_the_user_id_in_the_token() {
        // "MTAwMA" is base64 for "1000".
        let found = accounts(r#"{"token":"MTAwMA.Gabcde.signature"}"#).unwrap();
        assert_eq!(found[0].account, "1000");
        let started: Session = serde_json::from_str(&found[0].session).unwrap();
        assert_eq!(started.token, "MTAwMA.Gabcde.signature");
        // A token that does not start with an id still signs in.
        assert_eq!(accounts(r#"{"token":"opaque"}"#).unwrap()[0].account, "main");
    }

    #[test]
    fn two_accounts_stay_apart_and_signing_one_out_keeps_the_other() {
        // "MjAwMA" is base64 for "2000".
        let first = r#"{"token":"MTAwMA.Gabcde.signature","user_agent":"Safari/26.0"}"#;
        let second = r#"{"token":"MjAwMA.Gabcde.signature","user_agent":"Safari/26.0"}"#;
        let both = combine(first, second).unwrap();
        let found = accounts(&both).unwrap();
        assert_eq!(found.iter().map(|signed| signed.account.as_str()).collect::<Vec<_>>(), ["1000", "2000"]);
        assert!(found.iter().all(|signed| serde_json::from_str::<Session>(&signed.session).is_ok()));
        assert_ne!(found[0].session, found[1].session);
        // The same person replaces their token and does not become a third account.
        let again = combine(&both, r#"{"token":"MTAwMA.replaced.signature"}"#).unwrap();
        let ids: Vec<String> = accounts(&again).unwrap().into_iter().map(|signed| signed.account).collect();
        assert_eq!(ids, ["2000", "1000"]);
        assert!(accounts(&again).unwrap().iter().any(|signed| signed.session.contains("replaced")));
        // Signing out of one leaves the other as the single-token session start already reads.
        let left = without(&both, "1000").unwrap();
        let kept = accounts(&left).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].account, "2000");
        assert!(kept[0].session.contains("MjAwMA"));
        assert!(!kept[0].session.contains("accounts"));
        assert_eq!(without(&left, "2000"), None);
        // An account that is not stored is left untouched.
        assert_eq!(without(&both, "nope").as_deref(), Some(both.as_str()));
        assert!(accounts("not json").is_err());
    }

    #[test]
    fn parse_discord_username_handles_pomelo_and_legacy_forms() {
        assert_eq!(parse_discord_username("alice").unwrap(), ("alice".into(), None));
        assert_eq!(parse_discord_username("  @alice  ").unwrap(), ("alice".into(), None));
        assert_eq!(parse_discord_username("alice#0").unwrap(), ("alice".into(), None));
        assert_eq!(parse_discord_username("alice#1234").unwrap(), ("alice".into(), Some(1234)));
        assert!(parse_discord_username("   ").is_err());
        assert!(parse_discord_username("alice#12").is_err());
        assert!(parse_discord_username("#1234").is_err());
    }

    #[test]
    fn add_friend_refusal_translates_discord_codes() {
        let msg = |r: Refused| match add_friend_refusal(r) {
            Refusal::Refused(m) => m,
            Refusal::Unsupported => panic!("unexpected unsupported"),
        };
        assert!(msg(Refused::Status { status: 400, code: 80004, message: String::new(), retry_after: None }).contains("No Discord user"));
        assert!(msg(Refused::Status { status: 400, code: 80007, message: String::new(), retry_after: None }).contains("already your friend"));
        assert!(msg(Refused::Status { status: 400, code: 80002, message: String::new(), retry_after: None }).contains("blocked"));
        assert!(msg(Refused::Status { status: 400, code: 80008, message: String::new(), retry_after: None }).contains("does not accept friend requests"));
        assert!(msg(Refused::Challenge).contains("captcha"));
        assert!(msg(Refused::Limited(10.0)).contains("Too many friend requests"));
    }

    #[test]
    fn parse_discord_invite_code_extracts_urls_and_bare_codes() {
        assert_eq!(parse_discord_invite_code("rustlang").unwrap(), "rustlang");
        assert_eq!(parse_discord_invite_code("https://discord.gg/AbC-123_x?event=1").unwrap(), "AbC-123_x");
        assert_eq!(parse_discord_invite_code("discord.gg/rust").unwrap(), "rust");
        assert_eq!(parse_discord_invite_code("https://discord.com/invite/rustlang").unwrap(), "rustlang");
        assert_eq!(parse_discord_invite_code("http://www.discordapp.com/invite/rustlang/").unwrap(), "rustlang");
        assert!(parse_discord_invite_code("").is_err());
        assert!(parse_discord_invite_code("https://example.com/invite/abc").is_err());
        assert!(parse_discord_invite_code("https://discord.gg/").is_err());
    }

    #[test]
    fn invite_refusal_translates_discord_codes() {
        let msg = |r: Refused| match invite_refusal(r) {
            Refusal::Refused(m) => m,
            Refusal::Unsupported => panic!("unexpected unsupported"),
        };
        assert!(msg(Refused::Status { status: 404, code: 10006, message: String::new(), retry_after: None }).contains("invalid or has expired"));
        assert!(msg(Refused::Status { status: 403, code: 40007, message: String::new(), retry_after: None }).contains("banned"));
        assert!(msg(Refused::Status { status: 400, code: 30001, message: String::new(), retry_after: None }).contains("server limit"));
        assert!(msg(Refused::Challenge).contains("captcha"));
    }

    #[test]
    fn ack_bulk_payloads_chunk_at_50_and_skip_empty_message_ids() {
        let mut reads: Vec<(String, String)> = (1..=55)
            .map(|i| (format!("chan{i}"), format!("msg{i}")))
            .collect();
        reads.push(("empty_chan".into(), String::new()));
        let payloads = ack_bulk_payloads(&reads);
        assert_eq!(payloads.len(), 2, "55 valid reads split into chunks of 50 and 5");
        assert_eq!(payloads[0]["read_states"].as_array().unwrap().len(), 50);
        assert_eq!(payloads[1]["read_states"].as_array().unwrap().len(), 5);
        assert_eq!(payloads[0]["read_states"][0]["channel_id"], "chan1");
        assert_eq!(payloads[0]["read_states"][0]["message_id"], "msg1");
        assert_eq!(payloads[0]["read_states"][0]["read_state_type"], 0);
    }

    #[test]
    fn forward_report_prove_passes() {
        prove_forward_report().expect("forward and report");
    }

    #[test]
    fn create_space_payload_templates_image_detection_and_refusals() {
        let blank = build_create_guild_body("  My Club  ", None, None).unwrap();
        assert_eq!(blank["name"], "My Club");
        assert!(blank["icon"].is_null());
        assert_eq!(blank["channels"].as_array().unwrap().len(), 0);
        assert!(blank["system_channel_id"].is_null());

        let gaming = build_create_guild_body("Raid Night", Some("data:image/png;base64,AA=="), Some("gaming")).unwrap();
        assert_eq!(gaming["name"], "Raid Night");
        assert_eq!(gaming["icon"], "data:image/png;base64,AA==");
        assert!(gaming["channels"].as_array().unwrap().len() >= 4);
        assert_eq!(gaming["system_channel_id"], "2");

        assert!(build_create_guild_body("a", None, None).is_err());
        assert!(build_create_guild_body(&"x".repeat(101), None, None).is_err());

        assert_eq!(detect_image_mime(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(detect_image_mime(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(detect_image_mime(b"GIF89a..."), Some("image/gif"));
        assert_eq!(detect_image_mime(b"RIFF1234WEBP"), Some("image/webp"));
        assert_eq!(detect_image_mime(b"not-an-image"), None);
        assert!(read_image_data_uri("relative/path.png", DISCORD_IMAGE_MAX).is_err());

        let msg = |r: Refused| match create_space_refusal(r) {
            Refusal::Refused(m) => m,
            Refusal::Unsupported => panic!("unexpected unsupported"),
        };
        assert!(msg(Refused::Status { status: 400, code: 30001, message: String::new(), retry_after: None }).contains("server limit"));
        assert!(msg(Refused::Status { status: 403, code: 40002, message: String::new(), retry_after: None }).contains("Verify your Discord email"));
        assert!(msg(Refused::Limited(10.0)).contains("Too many server creation requests"));
        assert!(msg(Refused::Challenge).contains("captcha"));
    }

    #[test]
    fn scheduled_event_notification_respects_interest_and_mute_flags() {
        // Transitions from scheduled (1) to active (2) while interested and unmuted -> notify.
        assert!(should_notify_scheduled_event(1, 2, true, false, false));
        // Already active (2 -> 2) -> do not notify twice.
        assert!(!should_notify_scheduled_event(2, 2, true, false, false));
        // Not interested -> do not notify.
        assert!(!should_notify_scheduled_event(1, 2, false, false, false));
        // mute_scheduled_events set -> do not notify.
        assert!(!should_notify_scheduled_event(1, 2, true, true, false));
        // Whole server muted -> do not notify.
        assert!(!should_notify_scheduled_event(1, 2, true, false, true));
        // Completed (3) or canceled (4) -> do not notify.
        assert!(!should_notify_scheduled_event(2, 3, true, false, false));
        assert!(!should_notify_scheduled_event(1, 4, true, false, false));
    }
}
