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

#[derive(Default)]
pub(super) struct MessageSearch {
    in_flight: bool,
    last: Option<Instant>,
    remembered: std::collections::VecDeque<(SearchScope, String, Vec<MessageHit>)>,
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

#[derive(Deserialize)]
struct GuildCounts {
    #[serde(default)]
    description: Option<String>,
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
        let mut media: Vec<(protocol::MediaFile, String)> = Vec::new();
        let text = profile.user_profile.as_ref();
        let member_text = profile.guild_member_profile.as_ref();
        let banner = space
            .and_then(|guild| {
                member_text
                    .and_then(|p| p.banner.as_deref())
                    .filter(|h| !h.is_empty())
                    .map(|hash| model::member_banner(guild, person, hash))
            })
            .or_else(|| {
                text.and_then(|p| p.banner.as_deref())
                    .filter(|h| !h.is_empty())
                    .or_else(|| profile.user.as_ref().and_then(|u| u.banner.as_deref()).filter(|h| !h.is_empty()))
                    .map(|hash| model::user_banner(person, hash))
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
            let icon = badge.icon.as_deref().filter(|h| !h.is_empty()).map(|hash| {
                let (file, url) = model::badge_icon(hash);
                media.push((file.clone(), url));
                file
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
        let state = crate::hub::lock(&self.state);
        let info = state.guilds.get(guild).ok_or_else(|| refused("This server is gone."))?;
        let roles = info.my_roles.iter().filter(|role| **role != number(guild)).filter_map(|role| info.roles.get(role));
        let notify = match () {
            _ if info.quiet.server_muted(now()) => Notify::Nothing,
            _ if info.quiet.level == 1 => Notify::Mentions,
            _ if info.quiet.level == 2 => Notify::Nothing,
            _ => Notify::All,
        };
        Ok(SpaceAbout {
            description: counts.description.unwrap_or_default(),
            members: counts.approximate_member_count,
            online: counts.approximate_presence_count,
            nickname: info.nick.clone(),
            roles: roles.map(|role| role.name.clone()).take(protocol::PROFILE_ROLES).collect(),
            notify,
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
            let bytes = std::fs::read(file_path).map_err(|err| refused(format!("Could not read picture: {err}")))?;
            if bytes.is_empty() || bytes.len() > 10 * 1024 * 1024 {
                return Err(refused("Choose an image under 10 MB."));
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
        self.ctx.profile_stale(person);
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
