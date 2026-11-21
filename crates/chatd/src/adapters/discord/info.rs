//! What the info pane, the card and the conversation menu ask of Discord: a profile in a server,
//! a conversation's and a server's info, a mute, leaving a group or closing a DM, the pins, and
//! the server-side search for what was shared. Each is one request at most, and only because
//! the user opened or clicked something.

use super::super::{About, ConversationAbout, Refusal, Reply, SearchScope, SharedPage, SpaceAbout};
use super::model::{self, number, GuildSettings, Posted};
use super::rest::Refused;
use super::{escaped, refusal, Discord};
use base64::Engine;
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
            "/users/{person}/profile?type=modal&with_mutual_guilds=true&with_mutual_friends=true&with_mutual_friends_count=true"
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
        let member_profile = space.and_then(|_| profile.guild_member_profile.as_ref());
        let mut server_profile = false;
        let mut server_avatar = None;
        if let Some((guild_id, hash)) = space.zip(
            profile
                .guild_member
                .as_ref()
                .and_then(|m| m.avatar.as_deref())
                .filter(|h| !h.is_empty()),
        ) {
            let (file, url) = model::member_avatar(guild_id, person, hash);
            let sizes = model::avatar_variants(&file.id);
            let id = file.id.clone();
            let registered = self.ctx.media(&[(file.clone(), url)]);
            if let (Ok(()), Some(sizes)) = (registered, sizes) {
                let _ = self.ctx.media_variants(&id, &sizes);
            }
            server_avatar = Some(file);
            server_profile = true;
        }
        let member_banner_hash = space.and_then(|_| {
            member_profile
                .and_then(|p| p.banner.as_deref())
                .filter(|h| !h.is_empty())
                .or_else(|| {
                    profile
                        .guild_member
                        .as_ref()
                        .and_then(|m| m.banner.as_deref())
                        .filter(|h| !h.is_empty())
                })
        });
        let global_banner_hash = profile
            .user_profile
            .as_ref()
            .and_then(|p| p.banner.as_deref())
            .filter(|h| !h.is_empty())
            .or_else(|| profile.user.as_ref().and_then(|u| u.banner.as_deref()).filter(|h| !h.is_empty()));
        let banner = if let (Some(guild_id), Some(hash)) = (space, member_banner_hash) {
            let (file, url) = model::member_banner(guild_id, person, hash);
            let _ = self.ctx.media(&[(file.clone(), url)]);
            server_profile = true;
            Some(file)
        } else if let Some(hash) = global_banner_hash {
            let (file, url) = model::user_banner(person, hash);
            let _ = self.ctx.media(&[(file.clone(), url)]);
            Some(file)
        } else {
            None
        };
        let mut friend_files = Vec::new();
        for friend in profile.mutual_friends.iter().take(25) {
            friend_files.push(friend.avatar());
        }
        if !friend_files.is_empty() {
            let _ = self.ctx.media(&friend_files);
        }
        let state = crate::hub::lock(&self.state);
        // Count known servers before the list is capped. An id we have never seen stays out.
        let known_spaces: Vec<protocol::MutualSpace> = profile
            .mutual_guilds
            .iter()
            .filter_map(|mg| {
                let guild = state.guilds.get(&mg.id)?;
                Some(protocol::MutualSpace {
                    key: self.ctx.key(&mg.id),
                    name: guild.name.clone(),
                    nickname: mg.nick.clone().unwrap_or_default(),
                })
            })
            .collect();
        let mutual_total = known_spaces.len() as u32;
        let mutual_space_items: Vec<protocol::MutualSpace> = known_spaces.into_iter().take(protocol::MUTUAL_SPACES).collect();
        let mutual_spaces: Vec<String> = mutual_space_items.iter().map(|s| s.name.clone()).collect();
        // An unknown id is not a friend. Inventing Friend here lets the card's learn
        // replace a blocked or ignored person. Anyone who is not already Friend is left out.
        let mutual_people: Vec<protocol::Person> = profile
            .mutual_friends
            .iter()
            .take(25)
            .filter_map(|friend| {
                let relation = state.relations.get(&friend.id).copied().unwrap_or(protocol::Relation::None);
                if relation != protocol::Relation::Friend {
                    return None;
                }
                let (avatar, _) = friend.avatar();
                let name = state.users.get(&friend.id).cloned().unwrap_or_else(|| friend.name().to_owned());
                Some(protocol::Person {
                    key: self.ctx.key(&friend.id),
                    name,
                    handle: friend.username.clone(),
                    avatar: Some(avatar),
                    bot: friend.bot,
                    relation,
                    link: model::user_link(&friend.id),
                    ..protocol::Person::new(self.ctx.key(&friend.id), "")
                })
            })
            .collect();
        let mutual_friends = profile
            .mutual_friends_count
            .unwrap_or(profile.mutual_friends.len() as u32)
            .max(mutual_people.len() as u32);
        let text = profile.user_profile.as_ref();
        let bio = match member_profile.map(|p| p.bio.as_str()).filter(|b| !b.is_empty()) {
            Some(server_bio) => {
                server_profile = true;
                let mut bio = server_bio.to_owned();
                protocol::clip(&mut bio, 1_000);
                bio
            }
            None => profile.bio(),
        };
        let pronouns = match member_profile.map(|p| p.pronouns.as_str()).filter(|p| !p.is_empty()) {
            Some(server_pronouns) => {
                server_profile = true;
                server_pronouns.to_owned()
            }
            None => text.map(|text| text.pronouns.clone()).unwrap_or_default(),
        };
        let accent = match member_profile.and_then(|p| p.accent_color) {
            Some(color) => {
                server_profile = true;
                Some(color)
            }
            None => text
                .and_then(|text| text.accent_color)
                .or_else(|| profile.user.as_ref().and_then(|u| u.accent_color)),
        };
        let mut about = About {
            bio,
            mutual_spaces,
            mutual_space_items,
            mutual_people,
            mutual_friends,
            mutual_total,
            pronouns,
            accent,
            banner,
            avatar: server_avatar,
            server_profile,
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
        let can_change_nickname = info.owner
            || (info.base_permissions(guild) & (model::ADMINISTRATOR | model::CHANGE_NICKNAME)) != 0;
        Ok(SpaceAbout {
            description: counts.description.unwrap_or_default(),
            members: counts.approximate_member_count,
            online: counts.approximate_presence_count,
            nickname: info.nick.clone(),
            roles: roles.map(|role| role.name.clone()).take(protocol::PROFILE_ROLES).collect(),
            notify,
            can_change_nickname,
        })
    }

    /// Edits our own Discord display name (`PATCH /users/@me`), global profile (`PATCH /users/@me/profile`),
    /// or Nitro per-server profile (`PATCH /guilds/{id}/profile/@me`).
    pub(super) fn set_profile(
        &self,
        name: Option<String>,
        about: Option<String>,
        pronouns: Option<String>,
        accent: Option<u32>,
        banner: Option<String>,
        space_id: Option<String>,
    ) -> Result<(), Refusal> {
        let premium = crate::hub::lock(&self.state).premium;
        if let Some(guild) = space_id {
            if premium == 0 {
                return Err(refused("Per-server profiles require Discord Nitro."));
            }
            if !crate::hub::lock(&self.state).guilds.contains_key(&guild) {
                return Err(refused("This server is gone."));
            }
            let mut body = serde_json::Map::new();
            if let Some(mut bio) = about {
                let trimmed = bio.trim().to_owned();
                bio = trimmed;
                protocol::clip(&mut bio, 190);
                body.insert("bio".into(), json!(bio));
            }
            if let Some(mut p) = pronouns {
                let trimmed = p.trim().to_owned();
                p = trimmed;
                protocol::clip(&mut p, 40);
                body.insert("pronouns".into(), json!(p));
            }
            if let Some(color) = accent {
                body.insert("accent_color".into(), json!(color & 0xFF_FFFF));
            }
            if let Some(path) = banner {
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    body.insert("banner".into(), serde_json::Value::Null);
                } else {
                    let uri = image_data_uri(trimmed, true)?;
                    body.insert("banner".into(), json!(uri));
                }
            }
            if !body.is_empty() {
                self.call("PATCH", &format!("/guilds/{guild}/profile/@me"), Some(serde_json::Value::Object(body)))
                    .map_err(profile_refusal)?;
            }
            return Ok(());
        }

        if let Some(name) = name {
            let mut trimmed = name.trim().to_owned();
            protocol::clip(&mut trimmed, 32);
            let value = if trimmed.is_empty() { serde_json::Value::Null } else { json!(trimmed) };
            self.call("PATCH", "/users/@me", Some(json!({"global_name": value})))
                .map_err(profile_refusal)?;
        }

        let mut profile_body = serde_json::Map::new();
        if let Some(about) = about {
            let mut trimmed = about.trim().to_owned();
            protocol::clip(&mut trimmed, 190);
            profile_body.insert("bio".into(), json!(trimmed));
        }
        if let Some(pronouns) = pronouns {
            let mut trimmed = pronouns.trim().to_owned();
            protocol::clip(&mut trimmed, 40);
            profile_body.insert("pronouns".into(), json!(trimmed));
        }
        if let Some(color) = accent {
            profile_body.insert("accent_color".into(), json!(color & 0xFF_FFFF));
        }
        if let Some(path) = banner {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                profile_body.insert("banner".into(), serde_json::Value::Null);
            } else {
                if premium == 0 {
                    return Err(refused("Profile banners require Discord Nitro."));
                }
                let uri = image_data_uri(trimmed, true)?;
                profile_body.insert("banner".into(), json!(uri));
            }
        }
        if !profile_body.is_empty() {
            self.call("PATCH", "/users/@me/profile", Some(serde_json::Value::Object(profile_body)))
                .map_err(profile_refusal)?;
        }
        Ok(())
    }

    /// Sets or clears our own Discord avatar via `PATCH /users/@me`.
    pub(super) fn set_avatar(&self, path: &str) -> Result<(), Refusal> {
        let premium = crate::hub::lock(&self.state).premium;
        let trimmed = path.trim();
        let value = if trimmed.is_empty() {
            serde_json::Value::Null
        } else {
            let uri = image_data_uri(trimmed, premium > 0)?;
            json!(uri)
        };
        self.call("PATCH", "/users/@me", Some(json!({"avatar": value})))
            .map_err(profile_refusal)?;
        Ok(())
    }

    /// Sets or clears our nickname in a Discord server via `PATCH /guilds/{id}/members/@me`.
    pub(super) fn set_nickname(&self, guild: &str, nickname: Option<String>) -> Result<(), Refusal> {
        {
            let state = crate::hub::lock(&self.state);
            let info = state.guilds.get(guild).ok_or_else(|| refused("This server is gone."))?;
            let allowed = info.owner
                || (info.base_permissions(guild) & (model::ADMINISTRATOR | model::CHANGE_NICKNAME)) != 0;
            if !allowed {
                return Err(refused("You don't have permission to change your nickname in this server."));
            }
        }
        let cleaned = nickname.and_then(|n| {
            let mut trimmed = n.trim().to_owned();
            protocol::clip(&mut trimmed, 32);
            (!trimmed.is_empty()).then_some(trimmed)
        });
        let value = match &cleaned {
            Some(nick) => json!(nick),
            None => serde_json::Value::Null,
        };
        self.call("PATCH", &format!("/guilds/{guild}/members/@me"), Some(json!({"nick": value})))
            .map_err(profile_refusal)?;
        if let Some(info) = crate::hub::lock(&self.state).guilds.get_mut(guild) {
            info.nick = cleaned.unwrap_or_default();
        }
        Ok(())
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

    /// Recent mentions from before tinyclient was open (`GET /users/@me/mentions`).
    /// Mapped through `convert()` without writing into channel message history; respects hidden
    /// channels, message requests, and age-gated (NSFW) channels.
    pub(super) fn mentions(
        &self,
        before: Option<&str>,
        everyone: bool,
        roles: bool,
        space: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<protocol::MentionHit>, bool, Option<String>), Refusal> {
        let cap = limit.clamp(1, 25);
        let mut path = format!("/users/@me/mentions?limit={cap}&roles={roles}&everyone={everyone}");
        if let Some(before) = before.filter(|b| !b.is_empty()) {
            path.push_str(&format!("&before={}", escaped(before)));
        }
        if let Some(guild) = space.filter(|g| !g.is_empty()) {
            path.push_str(&format!("&guild_id={}", escaped(guild)));
        }
        let answer = self.call("GET", &path, None).map_err(refusal)?;
        let raw: Vec<Posted> = serde_json::from_str(&answer).map_err(refused)?;
        let more = raw.len() >= cap as usize;
        // The next page starts before the last raw id, even when every hit on this page was
        // filtered out. One call returns one page.
        let cursor = more.then(|| raw.last().map(|posted| posted.id.clone())).flatten();
        let nsfw = crate::hub::lock(&self.state).nsfw_channels.clone();
        let mut hits = Vec::new();
        for posted in &raw {
            if !self.ctx.listable(&posted.channel_id) || nsfw.contains(&posted.channel_id) {
                continue;
            }
            let known = self.ctx.known(&posted.channel_id).ok().flatten();
            if known.as_ref().is_some_and(|row| row.special == protocol::Special::Request) {
                continue;
            }
            let kind = known.as_ref().map_or(ConversationKind::Channel, |row| row.kind);
            let title = known.as_ref().map_or_else(String::new, |row| row.title.clone());
            let space_title = known.as_ref().map_or_else(String::new, |row| row.space_title.clone());
            let guild = posted
                .guild_id
                .as_deref()
                .or_else(|| known.as_ref().and_then(|row| row.space.as_deref()));
            let (message, files) = self.convert(posted, guild, kind);
            if !files.files.is_empty() {
                let _ = self.ctx.media(&files.files);
            }
            let text = if !message.text.is_empty() {
                message.text
            } else if let Some(first) = message.attachments.first() {
                if first.name.is_empty() { "Attachment".to_owned() } else { first.name.clone() }
            } else {
                posted.content.clone().unwrap_or_default()
            };
            hits.push(protocol::MentionHit {
                conversation: self.ctx.key(&posted.channel_id),
                message_id: posted.id.clone(),
                sent: model::seconds(&posted.id),
                sender: message.sender.name,
                sender_avatar: message.sender.avatar,
                text,
                title,
                space_title,
                mentions_everyone: posted.mention_everyone,
                mentions_role: !posted.mention_roles.is_empty(),
            });
        }
        Ok((hits, more, cursor))
    }

    /// Dismisses a recent mention (`DELETE /users/@me/mentions/{message_id}`).
    pub(super) fn dismiss_mention(&self, message_id: &str) -> Result<(), Refusal> {
        let id = message_id.trim();
        if id.is_empty() {
            return Err(refused("Missing mention message id."));
        }
        self.call("DELETE", &format!("/users/@me/mentions/{}", escaped(id)), None)
            .map_err(refusal)?;
        Ok(())
    }
}

/// Largest avatar or banner image file accepted before base64-encoding into a Discord data URI.
const MAX_PROFILE_IMAGE_BYTES: usize = 10 << 20;

fn profile_refusal(failed: Refused) -> Refusal {
    match failed {
        Refused::Status { code: 50035, ref message, .. } if !message.is_empty() => {
            Refusal::Refused(format!("Discord rejected the profile change: {message}"))
        }
        other => refusal(other),
    }
}

/// Reads a local image file, validates its magic bytes and size, and encodes it as a
/// `data:image/{png,jpeg,webp,gif};base64,...` URI for Discord's profile endpoints.
fn image_data_uri(path: &str, allow_gif: bool) -> Result<String, Refusal> {
    let len = std::fs::metadata(path).map_err(|err| refused(format!("Could not read image: {err}")))?.len();
    if len == 0 {
        return Err(refused("The chosen image file is empty."));
    }
    if len > MAX_PROFILE_IMAGE_BYTES as u64 {
        return Err(refused("Profile images must be 10 MB or smaller."));
    }
    let bytes = std::fs::read(path).map_err(|err| refused(format!("Could not read image: {err}")))?;
    if bytes.is_empty() {
        return Err(refused("The chosen image file is empty."));
    }
    if bytes.len() > MAX_PROFILE_IMAGE_BYTES {
        return Err(refused("Profile images must be 10 MB or smaller."));
    }
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if !allow_gif {
            return Err(refused("Animated avatars and banners require Discord Nitro."));
        }
        "image/gif"
    } else {
        return Err(refused("Choose a PNG, JPEG, WebP, or GIF image."));
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn image_data_uri_validates_format_and_nitro_gif_gate() {
        let dir = std::env::temp_dir().join(format!("tinyclient-discord-img-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let png = dir.join("a.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let uri = image_data_uri(png.to_str().unwrap(), false).unwrap();
        assert!(uri.starts_with("data:image/png;base64,"), "{uri}");

        let jpg = dir.join("a.jpg");
        std::fs::write(&jpg, b"\xFF\xD8\xFF\xE0jpg").unwrap();
        assert!(image_data_uri(jpg.to_str().unwrap(), false).unwrap().starts_with("data:image/jpeg;base64,"));

        let webp = dir.join("a.webp");
        std::fs::write(&webp, b"RIFF1234WEBPdata").unwrap();
        assert!(image_data_uri(webp.to_str().unwrap(), false).unwrap().starts_with("data:image/webp;base64,"));

        let gif = dir.join("a.gif");
        std::fs::write(&gif, b"GIF89adata").unwrap();
        assert!(image_data_uri(gif.to_str().unwrap(), false).is_err(), "GIF requires Nitro");
        assert!(image_data_uri(gif.to_str().unwrap(), true).unwrap().starts_with("data:image/gif;base64,"));

        let txt = dir.join("a.txt");
        std::fs::write(&txt, b"not an image").unwrap();
        assert!(image_data_uri(txt.to_str().unwrap(), true).is_err());

        let huge = dir.join("huge.png");
        let file = std::fs::File::create(&huge).unwrap();
        file.set_len((10 << 20) + 1).unwrap();
        drop(file);
        let err = image_data_uri(huge.to_str().unwrap(), true).unwrap_err();
        let words = format!("{err:?}");
        assert!(words.contains("10 MB"), "{words}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
