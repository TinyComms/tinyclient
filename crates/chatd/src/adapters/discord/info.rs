//! What the info pane, the card and the conversation menu ask of Discord: a profile in a server,
//! a conversation's and a server's info, a mute, leaving a group or closing a DM, the pins, and
//! the server-side search for what was shared. Each is one request at most, and only because
//! the user opened or clicked something.

use super::super::{About, ConversationAbout, Refusal, Reply, SearchScope, SharedPage, SpaceAbout};
use super::model::{self, number, GuildSettings, Posted};
use super::rest::Refused;
use super::{escaped, refusal, Discord};
use crate::hub::now;
use protocol::{ConversationKind, MediaKind, Message, MessageHit, Notify, SharedItem, SharedKind, Style};
use serde::Deserialize;
use serde_json::json;
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
/// Recent message searches kept in memory per account, so the same scope and words are not
/// asked twice in a session.
const REMEMBERED_SEARCHES: usize = 32;

#[derive(Default)]
pub(super) struct MessageSearch {
    in_flight: bool,
    last: Option<Instant>,
    remembered: std::collections::VecDeque<(SearchScope, String, Vec<MessageHit>)>,
}

fn refused(why: impl std::fmt::Display) -> Refusal {
    Refusal::Refused(why.to_string())
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
        let state = crate::hub::lock(&self.state);
        let shared = profile.mutual_guilds.iter().filter_map(|guild| state.guilds.get(&guild.id));
        let text = profile.user_profile.as_ref();
        let mut about = About {
            bio: profile.bio(),
            mutual_spaces: shared.map(|guild| guild.name.clone()).take(protocol::MUTUAL_SPACES).collect(),
            mutual_total: profile.mutual_guilds.len() as u32,
            pronouns: text.map(|text| text.pronouns.clone()).unwrap_or_default(),
            accent: text.and_then(|text| text.accent_color),
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
            let named = member.roles.iter().filter(|role| role.as_str() != space);
            let names = named.filter_map(|role| guild.and_then(|guild| guild.roles.get(&number(role))).map(|role| role.name.clone()));
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
        let state = crate::hub::lock(&self.state);
        let parent = known.parent.as_deref().or(known.group.as_deref());
        let notify = match guild.as_ref().map(|guild| state.guilds.get(guild)) {
            Some(Some(info)) => info.quiet.notify(channel, parent, now()),
            Some(None) => Notify::All,
            None => state.quiet_directs.notify(channel, None, now()),
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
        })
    }

    /// Sets a conversation's notifications as Discord's own menu does: one settings change for
    /// its server (`@me` for direct and group conversations). Discord answers with the server's
    /// settings whole, which are applied as if they came from the gateway.
    pub(super) fn set_mute(&self, channel: &str, notify: Notify) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        let guild = self.guild_of(&known);
        let mut setting = match notify {
            Notify::All => json!({"muted": false, "message_notifications": 0}),
            Notify::Mentions => json!({"muted": false, "message_notifications": 1}),
            Notify::Nothing => json!({"muted": true}),
        };
        setting["mute_config"] = json!({"end_time": null, "selected_time_window": -1});
        let body = json!({"channel_overrides": {channel: setting}});
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
    pub(super) fn leave(&self, channel: &str, close: bool) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        let path = match (known.kind, close) {
            (ConversationKind::Direct, true) => format!("/channels/{channel}"),
            (ConversationKind::Group, false) => format!("/channels/{channel}?silent=false"),
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
            state.directs.retain(|_, id| id != channel);
        }
        self.ctx.conversation_gone(channel).map_err(refused)
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
            });
        }
        Ok(found_hits)
    }
}
