//! READY: everything the session starts with, written to the store and then dropped. Also the
//! server, channel and settings events that change the same things afterwards.

use super::super::{Command, Failure};
use super::model::{
    self, number, seconds, Channel, Guild, GuildSettings, Kept, Member, MuteConfig, PresenceUpdate,
    ReadState, Relationship, Role, User, Versioned, VoiceState, ADMINISTRATOR, BAN_MEMBERS, CONNECT,
    CREATE_INSTANT_INVITE, CREATE_PRIVATE_THREADS, CREATE_PUBLIC_THREADS, KICK_MEMBERS, MANAGE_CHANNELS, MANAGE_EVENTS, MANAGE_GUILD,
    MANAGE_MESSAGES, MANAGE_ROLES, MANAGE_THREADS, MANAGE_WEBHOOKS, PIN_MESSAGES, SEND_MESSAGES, SEND_MESSAGES_IN_THREADS, VIEW_CHANNEL,
};
use super::{proto, Discord, GuildInfo, Quiet, Resumable, RoleInfo, WARM};
use crate::hub::now;
use crate::store::{ConversationRow, Filing, FolderRow, PersonRow, SpaceRow};
use protocol::{CallSupport, ConversationKind, Group, MediaFile, Presence, Relation, Special};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
struct Folder {
    #[serde(default)]
    guild_ids: Vec<String>,
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    color: Option<u32>,
}

/// Settings as JSON, which only older sessions still get.
#[derive(Deserialize)]
struct LegacySettings {
    #[serde(default)]
    guild_folders: Vec<Folder>,
    #[serde(default)]
    guild_positions: Vec<String>,
    #[serde(default)]
    explicit_content_filter: Option<u8>,
}

/// The parts of READY that are used. Everything else in it is skipped while parsing.
#[derive(Deserialize)]
pub struct Ready {
    user: User,
    session_id: String,
    #[serde(default)]
    resume_gateway_url: Option<String>,
    /// Every user the rest of the payload names by id, when the session asked for that.
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    relationships: Vec<Relationship>,
    #[serde(default)]
    private_channels: Vec<Channel>,
    #[serde(default)]
    guilds: Vec<Guild>,
    /// Our own member of each server, in the order of `guilds`.
    #[serde(default)]
    merged_members: Vec<Vec<Member>>,
    #[serde(default)]
    read_state: Option<Versioned<ReadState>>,
    #[serde(default)]
    user_guild_settings: Option<Versioned<GuildSettings>>,
    #[serde(default)]
    user_settings: Option<LegacySettings>,
    #[serde(default)]
    user_settings_proto: Option<String>,
    #[serde(default)]
    notes: HashMap<String, String>,
    #[serde(default)]
    presences: Vec<PresenceUpdate>,
}

#[derive(Deserialize, Default)]
struct MergedPresences {
    #[serde(default)]
    friends: Vec<PresenceUpdate>,
}

#[derive(Deserialize)]
struct SupplementalGuild {
    id: String,
    #[serde(default)]
    voice_states: Vec<VoiceState>,
}

/// What follows READY: friends' presence and who is in which voice channel.
#[derive(Deserialize)]
pub struct Supplemental {
    #[serde(default)]
    merged_presences: MergedPresences,
    #[serde(default)]
    guilds: Vec<SupplementalGuild>,
}

/// When a mute runs out: `i64::MAX` for one with no end.
fn until(config: Option<&MuteConfig>) -> i64 {
    let end = config.and_then(|config| config.end_time.as_deref());
    end.and_then(model::iso_seconds).unwrap_or(i64::MAX)
}

pub(super) fn quiet(settings: &GuildSettings) -> Quiet {
    let overrides = &settings.channel_overrides;
    let muted = overrides.iter().filter(|channel| channel.muted);
    let opted = overrides.iter().filter(|channel| channel.flags & model::OPT_IN_ENABLED != 0);
    let levels = overrides.iter().filter(|channel| channel.message_notifications < 3);
    let collapsed = overrides.iter().filter(|channel| channel.collapsed == Some(true));
    Quiet {
        muted_until: settings.muted.then(|| until(settings.mute_config.as_ref())),
        suppress_everyone: settings.suppress_everyone,
        suppress_roles: settings.suppress_roles,
        channels: muted.map(|channel| (channel.channel_id.clone(), until(channel.mute_config.as_ref()))).collect(),
        hide_muted: settings.hide_muted_channels,
        mute_scheduled_events: settings.mute_scheduled_events,
        opt_in: settings.flags & model::OPT_IN_CHANNELS_ON != 0,
        opted_in: opted.map(|channel| channel.channel_id.clone()).collect(),
        level: settings.message_notifications,
        levels: levels.map(|channel| (channel.channel_id.clone(), channel.message_notifications)).collect(),
        collapsed: collapsed.map(|channel| channel.channel_id.clone()).collect(),
    }
}

impl Quiet {
    /// Muted now: the server, the channel, or the category it is in.
    pub(super) fn mutes(&self, channel: &str, parent: Option<&str>, now: i64) -> bool {
        let on = |until: Option<&i64>| until.is_some_and(|until| *until > now);
        on(self.muted_until.as_ref()) || on(self.channels.get(channel)) || on(parent.and_then(|parent| self.channels.get(parent)))
    }

    /// The server as a whole is muted now.
    pub(super) fn server_muted(&self, now: i64) -> bool {
        self.muted_until.is_some_and(|until| until > now)
    }

    /// Not opted in to, where the user turned "Show all channels" off: neither the channel nor
    /// its category is opted in. Unverified against a real account: the flag bits are the
    /// community's, and the channels a server's onboarding always shows are not known here, so
    /// such a server may show fewer channels than Discord does until they are opted in.
    pub(super) fn opted_out(&self, channel: &str, parent: Option<&str>) -> bool {
        self.opt_in && !self.opted_in.contains(channel) && !parent.is_some_and(|parent| self.opted_in.contains(parent))
    }

    /// The network's notification setting for one conversation, mute first.
    pub(super) fn notify(&self, channel: &str, parent: Option<&str>, now: i64) -> protocol::Notify {
        if self.mutes(channel, parent, now) {
            return protocol::Notify::Nothing;
        }
        let level = self.levels.get(channel).or_else(|| parent.and_then(|parent| self.levels.get(parent))).copied();
        match level.unwrap_or(self.level) {
            1 => protocol::Notify::Mentions,
            2 => protocol::Notify::Nothing,
            _ => protocol::Notify::All,
        }
    }

    /// The next time one of its mutes runs out.
    fn next_end(&self, now: i64) -> Option<i64> {
        let ends = self.muted_until.iter().chain(self.channels.values());
        ends.copied().filter(|end| *end > now && *end != i64::MAX).min()
    }
}

/// Where a channel sits in its server, as one number that orders the whole tree: uncategorised
/// channels first, then each category's, text before voice, each in Discord's own order. It is
/// computed from the channel alone so that one created later lands in the right place without
/// the tree being held.
fn place(category: Option<i32>, voice: bool, position: i32) -> i32 {
    let rank = category.map_or(0, |position| position.clamp(0, 998) + 1);
    rank * 2_000_000 + i32::from(voice) * 1_000_000 + position.clamp(0, 9_999) * 100
}

fn kind_of(channel: &Channel) -> Option<ConversationKind> {
    Some(match channel.kind {
        model::TEXT => ConversationKind::Channel,
        model::NEWS => ConversationKind::Announcement,
        model::VOICE | model::STAGE => ConversationKind::Voice,
        model::FORUM | model::MEDIA_FORUM => ConversationKind::Forum,
        model::NEWS_THREAD | model::PUBLIC_THREAD | model::PRIVATE_THREAD => ConversationKind::Thread,
        _ => return None,
    })
}

/// A category's header: just above the first row `place` puts under it.
fn header(id: &str, name: &str, position: i32) -> Group {
    Group::new(id, name, place(Some(position), false, 0) - 1)
}

/// A topic as one line.
fn one_line(topic: Option<&str>) -> String {
    topic.unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Human-readable reason for a server timeout.
pub(super) fn timeout_reason(until: i64, now: i64) -> String {
    let left = until.saturating_sub(now);
    if left <= 0 {
        return String::new();
    }
    let phrase = if left >= 86_400 {
        let days = (left + 43_200) / 86_400;
        if days == 1 { "1 day".to_owned() } else { format!("{days} days") }
    } else if left >= 3_600 {
        let hours = (left + 1_800) / 3_600;
        if hours == 1 { "1 hour".to_owned() } else { format!("{hours} hours") }
    } else if left >= 60 {
        let mins = (left + 30) / 60;
        if mins == 1 { "1 minute".to_owned() } else { format!("{mins} minutes") }
    } else {
        "less than a minute".to_owned()
    };
    format!("You are timed out in this server for {phrase}.")
}

/// Discord exempts moderators and channel/thread managers from slowmode.
pub(super) fn slowmode_exempt(perms: u64, thread: bool) -> bool {
    let mask = if thread {
        MANAGE_MESSAGES | MANAGE_CHANNELS | MANAGE_THREADS
    } else {
        MANAGE_MESSAGES | MANAGE_CHANNELS
    };
    perms & mask != 0
}

/// A thread is read-only while this account is timed out, while it is locked and this account
/// cannot manage threads, or while this account cannot send in threads.
pub(super) fn thread_read_only(timed_out: Option<i64>, now: i64, locked: bool, rights: &protocol::ConversationRights) -> (bool, String) {
    if let Some(until) = timed_out {
        (true, timeout_reason(until, now))
    } else if locked && !rights.manage_threads {
        (true, "This thread is locked.".to_owned())
    } else if !rights.send_in_threads {
        (true, String::new())
    } else {
        (false, String::new())
    }
}

impl GuildInfo {
    /// Unix seconds when our server timeout ends, if still active.
    pub(super) fn timed_out(&self, now: i64) -> Option<i64> {
        self.timeout_until.filter(|until| *until > now)
    }

    /// Guild-level base permissions across `@everyone` and our assigned roles (without channel overwrites).
    pub(super) fn base_permissions(&self, guild: &str) -> u64 {
        if self.owner {
            return u64::MAX;
        }
        let everyone = number(guild);
        let of = |role: &u64| self.roles.get(role).map_or(0, |role| role.permissions);
        let allowed = self.my_roles.iter().fold(of(&everyone), |all, role| all | of(role));
        if allowed & ADMINISTRATOR != 0 {
            return u64::MAX;
        }
        allowed
    }

    /// Management rights advertised to the shell on `protocol::Space`.
    pub(super) fn space_rights(&self, guild: &str) -> protocol::SpaceRights {
        let perms = self.base_permissions(guild);
        protocol::SpaceRights {
            manage_space: self.owner || perms & MANAGE_GUILD != 0,
            manage_roles: self.owner || perms & MANAGE_ROLES != 0,
            manage_events: self.owner || perms & MANAGE_EVENTS != 0,
            owner: self.owner,
        }
    }

    /// Discord's permission rule for a channel: the roles' own permissions, then the channel's
    /// overwrites for everyone, for our roles, and for us.
    pub(super) fn permissions(&self, guild: &str, me: &str, channel: &Channel) -> u64 {
        if self.owner {
            return u64::MAX;
        }
        let everyone = number(guild);
        let of = |role: &u64| self.roles.get(role).map_or(0, |role| role.permissions);
        let mut allowed = self.my_roles.iter().fold(of(&everyone), |all, role| all | of(role));
        if allowed & ADMINISTRATOR != 0 {
            return u64::MAX;
        }
        let overwrites = &channel.permission_overwrites;
        if let Some(all) = overwrites.iter().find(|over| !over.member && number(&over.id) == everyone) {
            allowed = (allowed & !all.deny) | all.allow;
        }
        let mine = overwrites.iter().filter(|over| !over.member && self.my_roles.contains(&number(&over.id)));
        let (allow, deny) = mine.fold((0, 0), |(allow, deny), over| (allow | over.allow, deny | over.deny));
        allowed = (allowed & !deny) | allow;
        if let Some(own) = overwrites.iter().find(|over| over.member && over.id == me) {
            allowed = (allowed & !own.deny) | own.allow;
        }
        allowed
    }

    fn can_view(&self, guild: &str, me: &str, channel: &Channel) -> bool {
        self.permissions(guild, me, channel) & VIEW_CHANNEL != 0
    }

    /// True for a channel someone with no role cannot see: it is drawn with a lock.
    fn private(&self, guild: &str, channel: &Channel) -> bool {
        let everyone = number(guild);
        let mut allowed = self.roles.get(&everyone).map_or(0, |role| role.permissions);
        let overwrites = &channel.permission_overwrites;
        if let Some(all) = overwrites.iter().find(|over| !over.member && number(&over.id) == everyone) {
            allowed = (allowed & !all.deny) | all.allow;
        }
        allowed & (VIEW_CHANNEL | ADMINISTRATOR) == 0
    }

    /// The row for a channel we may see, placed under its category.
    fn row(&self, guild: &str, me: &str, channel: &Channel) -> Option<ConversationRow> {
        let kind = kind_of(channel)?;
        let parent = channel.parent_id.as_deref();
        let category = parent.and_then(|parent| self.categories.get(parent)).map(|(position, _)| *position);
        let name = channel.name.clone().unwrap_or_else(|| "channel".to_owned());
        let written = matches!(kind, ConversationKind::Channel | ConversationKind::Announcement);
        let clock = now();
        let timed_out = self.timed_out(clock);
        let perms = self.permissions(guild, me, channel);
        let can_connect = perms & CONNECT != 0;
        let read_only_reason = timed_out.map(|until| timeout_reason(until, clock)).unwrap_or_default();
        let read_only = timed_out.is_some() || (written && perms & SEND_MESSAGES == 0);
        let rights = protocol::ConversationRights {
            pin: timed_out.is_none() && (perms & (MANAGE_MESSAGES | PIN_MESSAGES) != 0),
            delete_others: timed_out.is_none() && (perms & MANAGE_MESSAGES != 0),
            manage_members: timed_out.is_none() && (perms & (KICK_MEMBERS | BAN_MEMBERS | MANAGE_ROLES) != 0),
            create_public_threads: timed_out.is_none() && written && (perms & CREATE_PUBLIC_THREADS != 0),
            create_private_threads: timed_out.is_none() && kind == ConversationKind::Channel && (perms & CREATE_PRIVATE_THREADS != 0),
            send_in_threads: timed_out.is_none() && (perms & SEND_MESSAGES_IN_THREADS != 0),
            manage_threads: timed_out.is_none() && (perms & MANAGE_THREADS != 0),
            slowmode_exempt: slowmode_exempt(perms, false),
            create_invite: timed_out.is_none() && (perms & CREATE_INSTANT_INVITE != 0),
            manage_webhooks: timed_out.is_none() && (perms & MANAGE_WEBHOOKS != 0),
            ..protocol::ConversationRights::default()
        };
        let call = if timed_out.is_some() || (channel.kind == model::VOICE && !can_connect) {
            CallSupport::None
        } else {
            super::voice::support(channel.kind)
        };
        Some(ConversationRow {
            group: parent.filter(|_| category.is_some()).map(str::to_owned),
            position: place(category, channel.is_voice(), channel.position),
            call,
            // A voice channel's occupant limit, for the tree's `4/12`.
            limit: (channel.kind == model::VOICE).then_some(channel.user_limit).unwrap_or(0),
            topic: one_line(channel.topic.as_deref()),
            read_only,
            read_only_reason,
            rights,
            private: self.private(guild, channel) || (channel.kind == model::VOICE && !can_connect),
            opted_out: self.quiet.opted_out(&channel.id, parent),
            max_file_bytes: super::send::tier_upload_limit(self.premium_tier),
            slowmode_seconds: channel.rate_limit_per_user,
            age_restricted: channel.nsfw,
            ..ConversationRow::new(&channel.id, kind, name)
        })
    }

    fn remember(&mut self, channel: &str, parent: Option<&str>) {
        let entry = (number(channel), parent.map_or(0, number));
        match self.channels.iter_mut().find(|(id, _)| *id == entry.0) {
            Some(known) => *known = entry,
            None => self.channels.push(entry),
        }
    }
}

/// What a server's rows are, and how each stands: unread, mentions, the newest read, when the
/// newest was sent.
struct Tree {
    groups: Vec<Group>,
    rows: Vec<ConversationRow>,
    /// A forum's posts: recorded, but with no place in the tree (`polish/01` §3).
    posts: Vec<ConversationRow>,
    counts: Vec<(String, u32, u32, Option<String>, i64, Option<String>)>,
}

impl Discord {
    fn guild_info(&self, guild: &Guild, member: Option<&Member>, me: &str, position: i32, quiet: Quiet) -> GuildInfo {
        let roles = guild.roles.iter().map(|role| (number(&role.id), RoleInfo::remember(role)));
        let mut member_roles = HashMap::new();
        for m in &guild.members {
            let uid = m.id();
            if !uid.is_empty() && !m.roles.is_empty() {
                member_roles.insert(uid.to_owned(), m.roles.iter().map(|r| number(r)).collect());
            }
        }
        let categories = guild.channels.iter().filter(|channel| channel.kind == model::CATEGORY);
        let categories = categories.map(|category| (category.id.clone(), (category.position, category.name.clone().unwrap_or_default())));
        GuildInfo {
            name: guild.name().to_owned(),
            icon: guild.icon().map(str::to_owned),
            owner: guild.owner() == me,
            owner_id: guild.owner().to_owned(),
            position,
            joined: guild.joined_at.as_deref().and_then(model::iso_seconds).unwrap_or(0),
            roles: roles.collect(),
            my_roles: member.map_or_else(Vec::new, |member| member.roles.iter().map(|role| number(role)).collect()),
            member_roles,
            categories: categories.collect(),
            channels: Vec::new(),
            quiet,
            nick: member.and_then(|member| member.nick.clone()).unwrap_or_default(),
            premium_tier: guild.premium_tier().unwrap_or(0),
            premium_subscription_count: guild.premium_subscription_count().unwrap_or(0),
            banner: guild.banner().map(str::to_owned),
            description: guild.description().unwrap_or_default().to_owned(),
            features: guild.features().to_vec(),
            vanity_url_code: guild.vanity_url_code().map(str::to_owned),
            system_channel_id: guild.system_channel_id().map(str::to_owned),
            afk_channel_id: guild.afk_channel_id().map(str::to_owned),
            afk_timeout: guild.afk_timeout().unwrap_or(300),
            default_message_notifications: guild.default_message_notifications().unwrap_or(0),
            timeout_until: member.and_then(|member| member.communication_disabled_until.as_deref()).and_then(model::iso_seconds),
            emojis: guild.emojis.iter().filter(|emoji| emoji.available).filter_map(|emoji| {
                Some(super::Pickable {
                    id: emoji.id.clone()?,
                    name: emoji.name.clone(),
                    animated: emoji.animated,
                    format: 0,
                    roles: emoji.roles.iter().map(|role| number(role)).collect(),
                })
            }).collect(),
            stickers: guild.stickers.iter().filter(|sticker| sticker.available).map(|sticker| super::Pickable {
                id: sticker.id.clone(),
                name: sticker.name.clone(),
                animated: sticker.format_type != 1,
                format: sticker.format_type,
                roles: Box::default(),
            }).collect(),
            can_invite: false,
            events: guild
                .guild_scheduled_events
                .iter()
                .filter(|ev| matches!(ev.status, 1 | 2))
                .map(|ev| {
                    (
                        ev.id.clone(),
                        super::StoredEvent {
                            event: ev.clone(),
                            interested: false,
                        },
                    )
                })
                .collect(),
        }
    }

    /// A server's tree: only what we may see, with empty categories left out and the threads we
    /// are in under their channel.
    fn tree(&self, guild: &Guild, info: &mut GuildInfo, me: &str, read: &HashMap<String, ReadState>) -> Tree {
        let mut rows = Vec::new();
        let mut counts = Vec::new();
        let mut placed: HashMap<&str, (Option<String>, i32, protocol::ConversationRights, u64, bool)> = HashMap::new();
        info.channels.clear();
        let standing = |channel: &Channel, joined: Option<i64>| {
            let state = read.get(&channel.id);
            let last = channel.last_message_id.as_deref();
            let marker = state.map_or(0, |state| state.last_message_id);
            let unread = last.is_some_and(|last| match (state, joined) {
                (Some(_), _) => number(last) > marker,
                // Never read: only what arrived after we joined is new.
                (None, Some(joined)) => seconds(last) > joined,
                (None, None) => false,
            });
            let read_to = (marker > 0).then(|| marker.to_string());
            let mentions = state.map_or(0, |state| state.mention_count);
            (
                channel.id.clone(),
                u32::from(unread).max(u32::from(mentions > 0)),
                mentions,
                read_to,
                last.map_or(0, seconds),
                last.map(str::to_owned),
            )
        };
        for channel in &guild.channels {
            if channel.kind == model::CATEGORY || !info.can_view(&guild.id, me, channel) {
                continue;
            }
            let Some(row) = info.row(&guild.id, me, channel) else {
                continue;
            };
            let perms = info.permissions(&guild.id, me, channel);
            placed.insert(&channel.id, (row.group.clone(), row.position, row.rights, perms, row.age_restricted));
            info.remember(&channel.id, channel.parent_id.as_deref());
            // A voice channel's chat is not "unread" in the tree. A forum's own marker is:
            // its posts hold no row to carry a badge, so the forum itself counts them, and a
            // collapsed category never hides something that needs me (`polish/01` §2).
            if matches!(row.kind, ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Forum) {
                counts.push(standing(channel, Some(info.joined)));
            }
            rows.push(row);
        }
        // A forum's posts are never tree rows (`polish/01` §3): the topics drill and the
        // landing read them under the forum, so they are recorded without a place in it.
        let forums: HashSet<&str> = guild
            .channels
            .iter()
            .filter(|channel| matches!(channel.kind, model::FORUM | model::MEDIA_FORUM))
            .map(|channel| channel.id.as_str())
            .collect();
        let mut posts = Vec::new();
        let mut under: HashMap<&str, i32> = HashMap::new();
        for thread in &guild.threads {
            let archived = thread.thread_metadata.as_ref().is_some_and(|metadata| metadata.archived);
            let Some((group, position, parent_rights, parent_perms, parent_nsfw)) = thread.parent_id.as_deref().and_then(|parent| placed.get(parent)) else {
                continue;
            };
            if archived || !thread.is_thread() {
                continue;
            }
            let name = thread.name.clone().unwrap_or_else(|| "Thread".to_owned());
            let clock = now();
            let timed_out = info.timed_out(clock);
            let mut thread_rights = *parent_rights;
            let is_owner = thread.owner_id.as_deref() == Some(me);
            thread_rights.add_members = timed_out.is_none() && (thread_rights.send_in_threads || thread_rights.manage_threads);
            thread_rights.rename = timed_out.is_none() && (thread_rights.manage_threads || is_owner);
            thread_rights.slowmode_exempt = slowmode_exempt(*parent_perms, true);
            let locked = thread.thread_metadata.as_ref().is_some_and(|m| m.locked);
            let (read_only, read_only_reason) = thread_read_only(timed_out, clock, locked, &thread_rights);
            let row = ConversationRow {
                parent: thread.parent_id.clone(),
                // A thread started from a message has that message's id.
                root: Some(thread.id.clone()),
                private: thread.kind == model::PRIVATE_THREAD,
                read_only,
                read_only_reason,
                rights: thread_rights,
                slowmode_seconds: thread.rate_limit_per_user,
                locked,
                age_restricted: thread.nsfw || *parent_nsfw,
                // A server's threads in READY are the ones we were added to.
                filing: Some(Filing {
                    followed: true,
                    ..Filing::default()
                }),
                ..ConversationRow::new(&thread.id, ConversationKind::Thread, name)
            };
            // The server's own channels hold the tree places; a post takes none.
            counts.push(standing(thread, None));
            if forums.contains(thread.parent_id.as_deref().unwrap_or("")) {
                posts.push(row);
                continue;
            }
            let nth = under.entry(thread.parent_id.as_deref().unwrap_or("")).or_insert(0);
            *nth = (*nth + 1).min(99);
            rows.push(ConversationRow {
                group: group.clone(),
                position: position + *nth,
                ..row
            });
            info.remember(&thread.id, thread.parent_id.as_deref());
        }
        let used: HashSet<&str> = rows.iter().filter_map(|row| row.group.as_deref()).collect();
        let categories = guild.channels.iter().filter(|channel| channel.kind == model::CATEGORY);
        let now = now();
        let groups = categories.filter(|category| used.contains(category.id.as_str())).map(|category| Group {
            // A muted category starts collapsed; Discord's own collapsed flag stays in step.
            collapsed: info.quiet.collapsed.contains(&category.id),
            muted: info.quiet.channels.get(&category.id).is_some_and(|until| *until > now),
            ..header(&category.id, category.name.as_deref().unwrap_or(""), category.position)
        });
        info.can_invite = rows.iter().any(|row| row.rights.create_invite);
        Tree {
            groups: groups.collect(),
            rows,
            posts,
            counts,
        }
    }

    /// Writes one server's tree, with each row's unread state and mute, and keeps what is needed
    /// to work the tree out again when a role or our own membership changes.
    fn plant(&self, guild: &Guild, info: &mut GuildInfo, me: &str, read: &HashMap<String, ReadState>) -> rusqlite::Result<()> {
        let tree = self.tree(guild, info, me, read);
        self.ctx.tree(&guild.id, &tree.groups, &tree.rows)?;
        for group in &tree.groups {
            self.ctx.set_collapsed(&guild.id, &group.id, group.collapsed)?;
        }
        self.release_idle_slowmode(&tree.rows);
        // A post is told alone: it holds no place in the tree, so the tree's replacement
        // would not keep it.
        for post in &tree.posts {
            self.ctx.conversation(post)?;
        }
        self.release_idle_slowmode(&tree.posts);
        let now = now();
        // CONTRACT: unread state and mute are written a row at a time, each its own commit.
        // The first connect of an account with 3,000 channels is some 6,000 of them and takes
        // about 2.5 s (`tests/discord.rs`, the READY of real size); later ones change little.
        for (id, unread, mentions, read_to, updated, newest) in &tree.counts {
            let interesting = *unread > 0 || *mentions > 0;
            if interesting {
                if let Some(newest) = newest {
                    self.ctx.note_newest(id, newest)?;
                }
            }
            let (unread, mentions, read_to) = if interesting {
                self.settled(id, newest.as_deref(), *unread, *mentions, read_to.clone())
            } else {
                (*unread, *mentions, read_to.clone())
            };
            self.ctx.counts(id, unread, mentions, read_to.as_deref(), *updated)?;
        }
        for row in &tree.rows {
            self.ctx.muted(&row.id, info.quiet.mutes(&row.id, row.group.as_deref(), now))?;
        }
        let kept: Vec<Kept> = guild.channels.iter().map(Kept::of).collect();
        self.ctx.set_note(&format!("kept:{}", guild.id), &serde_json::to_string(&kept).unwrap_or_default());
        self.unlist_threads(&guild.id, &tree.rows)
    }

    /// A thread that names its parent stays through a tree replacement, so one that was
    /// archived or left while we were away has to be taken out of the tree by name. The ids
    /// listed last time are in a note; whichever of them is not listed now keeps its messages
    /// and loses its place.
    fn unlist_threads(&self, guild: &str, rows: &[ConversationRow]) -> rusqlite::Result<()> {
        let note = format!("threads:{guild}");
        let listed: Vec<&str> = rows.iter().filter(|row| row.kind == ConversationKind::Thread).map(|row| row.id.as_str()).collect();
        let before = self.ctx.note(&note).unwrap_or_default();
        for gone in before.split(',').filter(|id| !id.is_empty() && !listed.contains(id)) {
            let Some(known) = self.ctx.known(gone)?.filter(|known| known.space.is_some()) else {
                continue;
            };
            self.ctx.set_note(&format!("guild:{gone}"), guild);
            self.ctx.conversation(&ConversationRow {
                parent: known.parent,
                root: known.root,
                ..ConversationRow::new(gone, ConversationKind::Thread, known.title)
            })?;
        }
        let now_listed = listed.join(",");
        if now_listed != before {
            self.ctx.set_note(&note, &now_listed);
        }
        Ok(())
    }

    /// Changes what is kept of a server's channels.
    fn keep(&self, guild: &str, change: impl FnOnce(&mut Vec<Kept>)) {
        let note = format!("kept:{guild}");
        let mut kept: Vec<Kept> = self.ctx.note(&note).and_then(|kept| serde_json::from_str(&kept).ok()).unwrap_or_default();
        change(&mut kept);
        self.ctx.set_note(&note, &serde_json::to_string(&kept).unwrap_or_default());
    }

    /// Computes our effective Discord permission bitmask for a kept channel in `guild`.
    pub(super) fn channel_permissions(&self, guild: &str, channel_id: &str) -> Option<u64> {
        let kept: Vec<Kept> = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|kept| serde_json::from_str(&kept).ok())?;
        let channel = kept.into_iter().find(|entry| entry.id == channel_id)?.channel();
        let me = self.me();
        let state = self.state.lock().unwrap();
        let info = state.guilds.get(guild)?;
        Some(info.permissions(guild, &me, &channel))
    }

    /// Works out a server's visible channels again from what is kept of them, after a role's
    /// permissions or our own roles changed. Unread counts stay. Child threads keep their
    /// messages and their slowmode interval, and pick up the new rights.
    fn replant(&self, guild_id: &str) -> rusqlite::Result<()> {
        let kept: Vec<Kept> = match self.ctx.note(&format!("kept:{guild_id}")).map(|kept| serde_json::from_str(&kept)) {
            Some(Ok(kept)) => kept,
            _ => return Ok(()),
        };
        let guild = Guild {
            id: guild_id.to_owned(),
            channels: kept.into_iter().map(Kept::channel).collect(),
            ..Guild::default()
        };
        let me = self.me();
        let (tree, muted) = {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(guild_id) else {
                return Ok(());
            };
            let threads = std::mem::take(&mut info.channels);
            let tree = self.tree(&guild, info, &me, &HashMap::new());
            // Threads are not in what is kept; those under a channel still visible stay known.
            let visible: HashSet<u64> = info.channels.iter().map(|(id, _)| *id).collect();
            let threads = threads.into_iter().filter(|(id, parent)| !visible.contains(id) && visible.contains(parent));
            info.channels.extend(threads);
            let now = now();
            let muted: Vec<bool> = tree.rows.iter().map(|row| info.quiet.mutes(&row.id, row.group.as_deref(), now)).collect();
            (tree, muted)
        };
        self.ctx.tree(guild_id, &tree.groups, &tree.rows)?;
        self.release_idle_slowmode(&tree.rows);
        self.release_idle_slowmode(&tree.posts);
        for row in &tree.rows {
            if matches!(row.kind, ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Forum) {
                self.sync_child_threads(&row.id)?;
            }
        }
        for (row, muted) in tree.rows.iter().zip(muted) {
            self.ctx.muted(&row.id, muted)?;
        }
        let flags = self.state.lock().unwrap().guilds.get(guild_id).map(|info| {
            (info.quiet.server_muted(now()), info.quiet.hide_muted, info.can_invite, info.space_rights(guild_id))
        });
        if let Some((muted, hide, can_invite, rights)) = flags {
            self.ctx.space_flags(guild_id, muted, hide)?;
            self.ctx.set_space_can_create_invites(guild_id, can_invite)?;
            self.ctx.space_rights(guild_id, rights)?;
        }
        Ok(())
    }

    /// A role was created, changed or deleted. Only a change to what it permits moves the tree.
    pub(super) fn role_changed(&self, guild: &str, role: Option<Role>, deleted: Option<&str>) -> Result<(), Failure> {
        let moved = {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(guild) else {
                return Ok(());
            };
            match (role, deleted) {
                (Some(role), _) => {
                    let fresh = RoleInfo::remember(&role);
                    let before = info.roles.insert(number(&role.id), fresh);
                    // Everyone holds the server's own role; any other counts only if we do.
                    let ours = role.id == guild || info.my_roles.contains(&number(&role.id));
                    ours && before.is_none_or(|before| before.permissions != role.permissions)
                }
                (None, Some(id)) => info.roles.remove(&number(id)).is_some() && info.my_roles.contains(&number(id)),
                (None, None) => false,
            }
        };
        if moved {
            self.replant(guild)?;
        }
        Ok(())
    }

    /// A server member changed: refresh cached roles/colors/icons, and if it is us,
    /// recompute channel visibility and server timeout state.
    pub(super) fn member_changed(&self, guild: &str, member: &Member) -> Result<(), Failure> {
        let uid = member.id();
        let mut roles: Vec<u64> = member.roles.iter().map(|role| number(role)).collect();
        roles.sort_unstable();
        let is_me = uid == self.me();
        let timeout_until = member.communication_disabled_until.as_deref().and_then(model::iso_seconds);
        let (moved, timeout_changed, roster_refresh, icon_file) = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            let Some(info) = state.guilds.get_mut(guild) else {
                return Ok(());
            };
            if !uid.is_empty() {
                info.set_member_roles(uid, roles.clone());
            }
            let (color, icon, emoji) = info.resolve_role_style(&roles);
            let icon_file = icon.clone();
            let mut roster_refresh: Option<String> = None;
            if state.members.on_screen.as_deref() == Some(guild) {
                if let Some(roster) = state.members.roster.as_mut() {
                    let mut touched = false;
                    for entry in roster.rows.values_mut() {
                        if let super::members::Entry::Member(person) = entry {
                            if person.key.id == uid {
                                person.role_color = color;
                                person.role_icon = icon.as_ref().map(|(f, _)| f.clone());
                                person.role_emoji = emoji.clone();
                                if let Some(nick) = member.nick.as_deref().filter(|n| !n.is_empty()) {
                                    person.name = nick.to_owned();
                                }
                                touched = true;
                            }
                        }
                    }
                    if touched {
                        roster_refresh = Some(roster.channel.clone());
                    }
                }
            }
            if !is_me {
                (false, false, roster_refresh, icon_file)
            } else {
                let mut before = std::mem::replace(&mut info.my_roles, roles.clone());
                before.sort_unstable();
                let prev_timeout = std::mem::replace(&mut info.timeout_until, timeout_until);
                (before != roles, prev_timeout != timeout_until, roster_refresh, icon_file)
            }
        };
        if let Some(file) = icon_file {
            let _ = self.ctx.media(&[file]);
        }
        if let Some(channel) = roster_refresh {
            self.ctx.members_changed(&channel);
        }
        if moved || timeout_changed {
            self.replant(guild)?;
        }
        if timeout_changed {
            self.reschedule_unmute();
        }
        Ok(())
    }

    /// Every server as a Space, in the user's order.
    pub(super) fn spaces(&self) -> rusqlite::Result<()> {
        let now = now();
        let mut flags = Vec::new();
        let (rows, files): (Vec<SpaceRow>, Vec<(MediaFile, String)>) = {
            let state = self.state.lock().unwrap();
            flags.extend(state.guilds.iter().map(|(id, info)| {
                (id.clone(), info.quiet.server_muted(now), info.quiet.hide_muted, info.can_invite, info.space_rights(id))
            }));
            let mut rows = Vec::with_capacity(state.guilds.len());
            let mut files = Vec::new();
            for (id, info) in &state.guilds {
                let icon = info.icon.as_deref().and_then(|hash| model::guild_icon(id, hash));
                rows.push(SpaceRow {
                    id: id.clone(),
                    name: info.name.clone(),
                    icon: icon.as_ref().map(|(file, _)| file.id.clone()),
                    position: info.position,
                });
                if let Some(icon) = icon {
                    files.push(icon);
                }
                if let Some(hash) = info.banner.as_deref() {
                    files.push(model::guild_banner(id, hash));
                }
                for stored in info.events.values() {
                    if let Some(hash) = stored.event.image.as_deref().filter(|h| !h.is_empty()) {
                        files.push(model::event_cover(&stored.event.id, hash));
                    }
                }
            }
            (rows, files)
        };
        self.ctx.media(&files)?;
        self.ctx.spaces(&rows)?;
        // A server's own mute, "hide muted channels", invite permission, and management rights once its Space exists.
        for (id, muted, hide, can_invite, rights) in flags {
            self.ctx.space_flags(&id, muted, hide)?;
            self.ctx.set_space_can_create_invites(&id, can_invite)?;
            self.ctx.space_rights(&id, rights)?;
        }
        Ok(())
    }

    /// A direct or group conversation's row, its people and their pictures. `known` resolves
    /// recipients that arrive as ids only.
    fn direct_row(
        &self,
        channel: &Channel,
        known: &HashMap<String, User>,
        files: &mut Vec<(MediaFile, String)>,
        people: &mut Vec<PersonRow>,
    ) -> Option<ConversationRow> {
        let kind = match channel.kind {
            model::DM => ConversationKind::Direct,
            model::GROUP_DM => ConversationKind::Group,
            _ => return None,
        };
        let by_id = channel.recipient_ids.iter().filter_map(|id| known.get(id));
        let recipients: Vec<&User> = channel.recipients.iter().chain(by_id).collect();
        let mut state = self.state.lock().unwrap();
        let mut names = Vec::with_capacity(recipients.len());
        for user in &recipients {
            // A friend's nickname, recorded with the relationships, stays.
            let name = state.users.get(&user.id).cloned().unwrap_or_else(|| user.name().to_owned());
            files.push(user.avatar());
            people.push(user.person(Some(&name)));
            state.users.insert(user.id.clone(), name.clone());
            names.push(name);
        }
        let mut row = ConversationRow::new(&channel.id, kind, names.join(", "));
        row.call = CallSupport::Native;
        // Unverified against a real account: the flags are the community's names. A request
        // and one Discord judged spam both wait behind Message Requests.
        if channel.is_message_request || channel.is_spam {
            row.special = Special::Request;
            row.spam = channel.is_spam;
        }
        match (kind, recipients.first()) {
            (ConversationKind::Direct, Some(peer)) => {
                row.peer = Some(peer.id.clone());
                row.avatar = Some(peer.avatar().0.id);
                row.bot = peer.bot;
                if peer.system {
                    // Discord's own account: no replies, no call.
                    row.special = Special::Service;
                    row.read_only = true;
                    row.call = CallSupport::None;
                }
                state.directs.insert(peer.id.clone(), channel.id.clone());
            }
            (ConversationKind::Direct, None) => return None,
            _ => {
                if let Some(name) = channel.name.as_deref().filter(|name| !name.is_empty()) {
                    row.title = name.to_owned();
                } else if recipients.is_empty() {
                    if let Some(group) = state.groups.get(&channel.id) {
                        let fallback: Vec<&str> = group
                            .recipients
                            .iter()
                            .filter_map(|id| state.users.get(id).map(String::as_str))
                            .collect();
                        if !fallback.is_empty() {
                            row.title = fallback.join(", ");
                        }
                    }
                }
                if let Some((file, url)) = channel.icon.as_deref().and_then(|hash| model::group_icon(&channel.id, hash)) {
                    row.avatar = Some(file.id.clone());
                    files.push((file, url));
                }
                let owner = channel
                    .owner_id
                    .clone()
                    .or_else(|| state.groups.get(&channel.id).map(|g| g.owner.clone()))
                    .unwrap_or_default();
                let named = channel.name.as_deref().is_some_and(|name| !name.is_empty());
                row.rights.add_members = true;
                row.rights.rename = true;
                row.rights.edit_picture = true;
                row.rights.create_invite = true;
                row.rights.remove_members = !owner.is_empty() && owner == state.me;
                // A create or update can carry recipient ids and no user objects. Resolving
                // nobody must not replace the list READY already recorded, while still updating
                // owner and named state.
                if !recipients.is_empty() {
                    state.groups.insert(channel.id.clone(), super::GroupDm {
                        owner,
                        recipients: recipients.iter().map(|user| user.id.clone()).collect(),
                        named,
                    });
                } else if let Some(group) = state.groups.get_mut(&channel.id) {
                    if !owner.is_empty() {
                        group.owner = owner;
                    }
                    group.named = named;
                }
            }
        }
        state.privates.insert(channel.id.clone());
        Some(row)
    }

    /// Records one direct or group conversation: created, renamed, or just started by us.
    pub(super) fn direct(&self, channel: &Channel, known: &HashMap<String, User>) -> rusqlite::Result<()> {
        let (mut files, mut people) = (Vec::new(), Vec::new());
        let Some(row) = self.direct_row(channel, known, &mut files, &mut people) else {
            return Ok(());
        };
        self.ctx.media(&files)?;
        self.ctx.people(&people)?;
        self.ctx.conversation(&row)
    }

    /// Who we are, from READY or a change to it: the account's name, and our own row in People,
    /// which is where the account's picture is read from. The row goes first, so the status
    /// the name announces already has the picture.
    pub(super) fn myself(&self, user: &User) -> rusqlite::Result<()> {
        self.state.lock().unwrap().users.insert(user.id.clone(), user.name().to_owned());
        self.ctx.media(&[user.avatar()])?;
        self.ctx.people(&[user.person(None)])?;
        self.ctx.describe(user.name(), &user.id)
    }

    /// The user's server folders as JSON settings or protobuf ones carry them. Only a folder with
    /// an id is one; a server alone in a nameless one is simply not in a folder.
    fn folders(ready: &Ready) -> Option<Vec<proto::GuildFolder>> {
        let legacy = ready.user_settings.as_ref().map(|settings| {
            let folders = settings.guild_folders.iter().map(|folder| proto::GuildFolder {
                id: folder.id,
                name: folder.name.clone().unwrap_or_default(),
                color: folder.color,
                guild_ids: folder.guild_ids.clone(),
            });
            folders.collect::<Vec<_>>()
        });
        legacy.filter(|folders| !folders.is_empty()).or_else(|| ready.user_settings_proto.as_deref().and_then(proto::guild_folders))
    }

    /// Records the server folders. One with no id holds a server that is in no folder.
    pub(super) fn record_folders(&self, folders: &[proto::GuildFolder]) -> rusqlite::Result<()> {
        let rows: Vec<FolderRow> = folders
            .iter()
            .filter_map(|folder| Some((folder.id?, folder)))
            .enumerate()
            .map(|(position, (id, folder))| FolderRow {
                id: id.to_string(),
                name: folder.name.clone(),
                position: position as i32,
                color: folder.color,
                spaces: folder.guild_ids.clone(),
                pinned: Vec::new(),
            })
            .collect();
        self.ctx.folders(&rows)
    }

    /// What this account may send, from its plan. Unverified against a real account: the free
    /// file limit is the documented 10 MiB (a press report says 20 MB since August 2026; the
    /// smaller one is used until a real session says otherwise), and a server's boosts, which
    /// raise it there, are not counted.
    pub(super) fn capabilities(&self, premium: u8, age_restricted_blocked: bool) {
        let (max_file_bytes, emoji) = match premium {
            2 => (500 << 20, protocol::EmojiScope::All),
            1 | 3 => (50 << 20, protocol::EmojiScope::All),
            _ => (10 << 20, protocol::EmojiScope::Space),
        };
        self.ctx.capabilities(protocol::Capabilities {
            max_file_bytes,
            max_files: 10,
            caption_limit: 0,
            spoiler: true,
            as_file: false,
            gifs: true,
            stickers: true,
            search: true,
            custom_emoji: emoji,
            silent: true,
            schedule: false,
            suppress_embeds: true,
            reply_ping: true,
            timed_mute: true,
            text_limit: super::message_limit(premium) as u32,
            edit_window: 0,
            file_scan: false,
            allow_archives: false,
            voice_notes: false,
            direct_calls: false,
            revisions: false,
            close_direct: true,
            clear_history: false,
            reply: true,
            edit: true,
            delete: true,
            threads: true,
            follow_threads: true,
            direct_threads: false,
            pins: true,
            forward: true,
            abuse_reports: true,
            polls: None,
            reactions: true,
            reactors: true,
            gif_file: false,
            mention_format: protocol::MentionFormat::Discord,
            mention_spans: false,
            burst_reactions: false,
            blocks: true,
            add_friend_by_username: true,
            friend_nickname: true,
            join_space_by_invite: true,
            leave_space: true,
            groups: 9,
            create_friend_invite: true,
            rich_status: true,
            person_notes: true,
            age_restricted_blocked,
            announcement_follow: true,
            create_space: true,
            ..Default::default()
        });
        self.state.lock().unwrap().premium = premium;
    }

    /// The user's own order of servers: folders flattened, then anything they do not mention on
    /// top, newest first, which is where Discord puts a server just joined.
    fn order(ready: &Ready) -> Vec<String> {
        let legacy = ready.user_settings.as_ref().map(|settings| {
            let folders: Vec<String> = settings.guild_folders.iter().flat_map(|folder| folder.guild_ids.clone()).collect();
            if folders.is_empty() { settings.guild_positions.clone() } else { folders }
        });
        let stated = legacy.filter(|order| !order.is_empty());
        let stated = stated.or_else(|| ready.user_settings_proto.as_deref().and_then(proto::guild_order));
        let stated = stated.unwrap_or_default();
        let mut unplaced: Vec<&Guild> = ready.guilds.iter().filter(|guild| !stated.contains(&guild.id)).collect();
        unplaced.sort_by(|a, b| b.joined_at.cmp(&a.joined_at));
        unplaced.into_iter().map(|guild| guild.id.clone()).chain(stated).collect()
    }

    /// Takes READY apart into the store. Only [`super::State`] outlives this call.
    pub(super) fn ready(&self, ready: Ready) -> Result<(), Failure> {
        let me = ready.user.id.clone();
        self.myself(&ready.user)?;
        self.capabilities(ready.user.premium_type, ready.user.nsfw_allowed == Some(false));
        let blur_explicit = ready
            .user_settings_proto
            .as_deref()
            .map(proto::blur_explicit_media)
            .unwrap_or_else(|| ready.user_settings.as_ref().and_then(|s| s.explicit_content_filter).is_none_or(|v| v != 0));
        self.state.lock().unwrap().show_explicit_media = !blur_explicit;
        let order = Self::order(&ready);
        let folders = Self::folders(&ready);
        let known: HashMap<String, User> = ready.users.into_iter().map(|user| (user.id.clone(), user)).collect();
        let read = ready.read_state.map_or_else(Vec::new, Versioned::into_entries);
        let read: HashMap<String, ReadState> =
            read.into_iter().filter(|state| state.read_state_type == 0).map(|state| (state.id.clone(), state)).collect();
        let mut settings: HashMap<Option<String>, Quiet> = ready
            .user_guild_settings
            .map_or_else(Vec::new, Versioned::into_entries)
            .iter()
            .map(|settings| (settings.guild_id.clone(), quiet(settings)))
            .collect();

        // The lists the user owns: friends, requests either way, blocked.
        let (mut files, mut people, mut relations) = (Vec::new(), Vec::new(), Vec::new());
        let (mut names, mut kept) = (HashMap::new(), HashMap::new());
        for relationship in &ready.relationships {
            let id = relationship.person();
            let Some(user) = relationship.user.as_ref().or_else(|| known.get(id)) else {
                continue;
            };
            let row = user.person(relationship.nickname.as_deref());
            files.push(user.avatar());
            names.insert(id.to_owned(), row.name.clone());
            people.push(row);
            kept.insert(id.to_owned(), user.person(relationship.nickname.as_deref()));
            relations.push((id.to_owned(), relationship.relation()));
        }
        {
            let mut state = self.state.lock().unwrap();
            let resume = ready.resume_gateway_url.as_deref().map(|url| format!("{}/?encoding=json&v=9", url.trim_end_matches('/')));
            state.session = resume.map(|url| Resumable {
                id: ready.session_id.clone(),
                url,
            });
            state.me = me.clone();
            state.users = names;
            state.users.insert(me.clone(), ready.user.name().to_owned());
            state.rows = kept;
            state.relations = relations.iter().cloned().collect();
            state.directs.clear();
            state.privates.clear();
            state.groups.clear();
            state.presence.clear();
            state.doing.clear();
            state.notes = ready.notes.clone();
            state.guilds.clear();
            state.quiet_directs = settings.remove(&None).unwrap_or_default();
            // Whatever happened between the sessions was not heard: nothing fetched before
            // is known to be whole any more.
            state.covered.clear();
            // A new session starts with no subscriptions and no voice connection.
            state.members.forget_sent();
            state.voice.reset();
        }

        // Direct and group conversations, with where each stands.
        let mut rows = Vec::new();
        for channel in &ready.private_channels {
            if let Some(row) = self.direct_row(channel, &known, &mut files, &mut people) {
                rows.push((row, channel.last_message_id.as_deref()));
            }
        }
        self.ctx.media(&files)?;
        self.ctx.people(&people)?;
        self.ctx.relations(&relations)?;
        let now = now();
        let mut unread_directs = Vec::new();
        for (row, last) in &rows {
            self.ctx.conversation(row)?;
            let state = read.get(&row.id);
            let marker = state.map_or(0, |state| state.last_message_id);
            // Discord counts every unread message of a direct conversation as a mention of us.
            let behind = state.is_some() && last.is_some_and(|last| number(last) > marker);
            let unread = if behind { state.map_or(1, |state| state.mention_count.max(1)) } else { 0 };
            let read_to = (marker > 0).then(|| marker.to_string());
            let updated = last.map_or(0, seconds);
            if let Some(last) = last.filter(|id| !id.is_empty()) {
                self.ctx.note_newest(&row.id, last)?;
            }
            let (unread, _, read_to) = self.settled(&row.id, *last, unread, 0, read_to);
            self.ctx.counts(&row.id, unread, 0, read_to.as_deref(), updated)?;
            let muted = self.state.lock().unwrap().quiet_directs.mutes(&row.id, None, now);
            self.ctx.muted(&row.id, muted)?;
            // A request is never warmed: opening one must not look read to its sender.
            if unread > 0 && !muted && row.special != Special::Request {
                unread_directs.push((updated, row.id.clone()));
            }
        }

        // Servers: what is kept of each, then its tree.
        let mut icons = Vec::new();
        for (index, guild) in ready.guilds.iter().enumerate() {
            if guild.unavailable == Some(true) {
                continue;
            }
            let merged = ready.merged_members.get(index).and_then(|members| members.iter().find(|member| member.id() == me));
            let member = merged.or_else(|| guild.members.iter().find(|member| member.id() == me));
            let position = order.iter().position(|id| *id == guild.id).unwrap_or(order.len()) as i32;
            let quiet = settings.remove(&Some(guild.id.clone())).unwrap_or_default();
            let mut info = self.guild_info(guild, member, &me, position, quiet);
            self.plant(guild, &mut info, &me, &read)?;
            icons.extend(info.icon.as_deref().and_then(|hash| model::guild_icon(&guild.id, hash)));
            self.voice_states(Some(&guild.id), &guild.voice_states);
            self.state.lock().unwrap().guilds.insert(guild.id.clone(), info);
        }
        self.spaces()?;
        if let Some(folders) = &folders {
            self.record_folders(folders)?;
        }
        self.reschedule_unmute();
        for presence in &ready.presences {
            self.presence(presence);
        }
        if let Some(status) = ready.user_settings_proto.as_deref().and_then(proto::status_settings) {
            self.own_status(status);
        }

        // A few unread conversations are fetched ahead, once per run and never on a reconnect.
        let warm = {
            let mut state = self.state.lock().unwrap();
            let first = !std::mem::replace(&mut state.warmed, true);
            unread_directs.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
            unread_directs.truncate(if first { WARM } else { 0 });
            state.warming.extend(unread_directs.iter().map(|(_, id)| id.clone()));
            unread_directs
        };
        for (_, id) in warm {
            let _ = self.ctx.commands.send(Command::Warm(id));
        }
        // The Space on screen is subscribed again on the new session, placed against the
        // servers just read, so a Watch sent while connecting is not left unsubscribed.
        self.rewatch();
        Ok(())
    }

    /// Our own presence and custom status (text, emoji, clear-after), as the settings have them:
    /// at connect, and whenever they change on any device.
    pub(super) fn own_status(&self, status: Vec<u8>) {
        let (word, mut custom) = proto::own_custom_status(&status);
        let presence = match word.as_str() {
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            "invisible" => Presence::Offline,
            _ => Presence::Online,
        };
        let clock_ms = (now().max(0) as u64).saturating_mul(1000);
        let expired = custom.expires_at_ms.is_some_and(|ms| ms <= clock_ms);
        if expired {
            custom = proto::CustomStatus::default();
        }
        let stored = if expired {
            let word_out = match presence {
                Presence::Idle => "idle",
                Presence::Dnd => "dnd",
                Presence::Offline => "invisible",
                _ => "online",
            };
            proto::with_custom_status(&status, word_out, Some(&proto::CustomStatus::default()))
        } else {
            status
        };
        let until = custom.expires_at_ms.map(|ms| (ms / 1000) as i64);
        let me = {
            let mut state = self.state.lock().unwrap();
            state.status_settings = stored;
            state.own = (presence, custom.text.clone());
            state.own_emoji = custom.emoji_name.clone();
            state.own_emoji_id = custom.emoji_id;
            state.own_status_until = until;
            state.me.clone()
        };
        self.ctx.presence(&me, presence, &custom.text, true);
        self.ctx.activity(&me, &custom.emoji_name, None, true);
        self.ctx.status_until(&me, until, true);
        self.reschedule_unmute();
    }

    pub(super) fn supplemental(&self, more: Supplemental) {
        for presence in &more.merged_presences.friends {
            self.presence(presence);
        }
        for guild in &more.guilds {
            self.voice_states(Some(&guild.id), &guild.voice_states);
        }
    }

    /// Friends are an owned list, so their presence is kept and replayed; anyone else's is
    /// passed on and kept by chatd only while the shell watches them.
    pub(super) fn presence(&self, update: &PresenceUpdate) {
        let id = update.id();
        if id.is_empty() {
            return;
        }
        let (owned, restated) = {
            let mut state = self.state.lock().unwrap();
            let owned = state.relations.get(id) == Some(&Relation::Friend);
            if owned {
                state.presence.insert(id.to_owned(), (update.presence(), update.status().to_owned()));
            }
            // The status line is stored with the person, so People shows it as last seen.
            let row = state.rows.get_mut(id).filter(|row| owned && row.status != update.status());
            let restated = row.map(|row| {
                row.status = update.status().to_owned();
                row.clone()
            });
            (owned, restated)
        };
        if let Some(row) = restated {
            if let Err(err) = self.ctx.people(&[row]) {
                self.log("status", err);
            }
        }
        self.ctx.presence(id, update.presence(), update.status(), owned);
        let (emoji, doing) = (update.emoji().to_owned(), update.doing());
        if owned {
            self.state.lock().unwrap().doing.insert(id.to_owned(), (emoji.clone(), doing.clone()));
            self.tell_activity(id);
        } else {
            self.ctx.activity(id, &emoji, doing, false);
        }
    }

    /// A friend's status emoji and what they are doing: in voice, from their voice state, wins
    /// over a game, since it is what says they can be joined.
    pub(super) fn tell_activity(&self, person: &str) {
        let (emoji, doing, voice) = {
            let state = self.state.lock().unwrap();
            let (emoji, doing) = state.doing.get(person).cloned().unwrap_or_default();
            let voice = state.voice.states.get(person).map(|heard| (heard.channel().to_owned(), heard.guild.clone()));
            let voice = voice.map(|(channel, guild)| {
                let place = guild.as_ref().and_then(|guild| state.guilds.get(guild)).map(|info| info.name.clone());
                (channel, guild, place.unwrap_or_default())
            });
            (emoji, doing, voice)
        };
        let doing = match voice {
            Some((channel, guild, place)) => {
                let name = self.ctx.known(&channel).ok().flatten().map(|row| row.title).unwrap_or_default();
                let mut voice = protocol::Activity {
                    kind: if guild.is_some() { protocol::ActivityKind::InVoice } else { protocol::ActivityKind::InCall },
                    name,
                    place,
                    conversation: Some(self.ctx.key(&channel)),
                };
                voice.bound();
                Some(voice)
            }
            None => doing,
        };
        self.ctx.activity(person, &emoji, doing, true);
    }

    /// After a resume: chatd dropped what it knew live when the link went down, and Discord
    /// does not send friends' presence again.
    pub(super) fn resumed(&self) {
        let known: Vec<_> = {
            let state = self.state.lock().unwrap();
            state.presence.iter().map(|(id, (presence, status))| (id.clone(), *presence, status.clone())).collect()
        };
        for (id, presence, status) in known {
            self.ctx.presence(&id, presence, &status, true);
        }
    }

    /// A server joined, or one that was unavailable at READY arriving.
    pub(super) fn guild_created(&self, guild: Guild) -> Result<(), Failure> {
        if guild.unavailable == Some(true) {
            return Ok(());
        }
        let me = self.me();
        let (position, quiet) = {
            let mut state = self.state.lock().unwrap();
            match state.guilds.remove(&guild.id) {
                Some(old) => (old.position, old.quiet),
                // New servers go on top, as in Discord.
                None => (state.guilds.values().map(|info| info.position).min().unwrap_or(0) - 1, Quiet::default()),
            }
        };
        let member = guild.members.iter().find(|member| member.id() == me);
        let mut info = self.guild_info(&guild, member, &me, position, quiet);
        // The channels are planted before the voice states are read, as at READY: a state's
        // stage is known from the rows the tree just planted.
        self.plant(&guild, &mut info, &me, &HashMap::new())?;
        self.voice_states(Some(&guild.id), &guild.voice_states);
        self.state.lock().unwrap().guilds.insert(guild.id.clone(), info);
        Ok(self.spaces()?)
    }

    /// A server renamed or given a new icon. The payload names only what it wants shown.
    pub(super) fn guild_updated(&self, guild: Guild) -> Result<(), Failure> {
        let me = self.me();
        let tier_changed = {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(&guild.id) else {
                return Ok(());
            };
            if guild.name.is_some() || guild.properties.as_ref().is_some_and(|properties| properties.name.is_some()) {
                info.name = guild.name().to_owned();
            }
            info.icon = guild.icon().map(str::to_owned);
            info.banner = guild.banner().map(str::to_owned);
            if let Some(description) = guild.description() {
                info.description = description.to_owned();
            }
            if let Some(count) = guild.premium_subscription_count() {
                info.premium_subscription_count = count;
            }
            if !guild.features().is_empty() {
                info.features = guild.features().to_vec();
            }
            if let Some(code) = guild.vanity_url_code() {
                info.vanity_url_code = Some(code.to_owned());
            }
            if let Some(ch) = guild.system_channel_id() {
                info.system_channel_id = Some(ch.to_owned());
            }
            if let Some(ch) = guild.afk_channel_id() {
                info.afk_channel_id = Some(ch.to_owned());
            }
            if let Some(timeout) = guild.afk_timeout() {
                info.afk_timeout = timeout;
            }
            if let Some(level) = guild.default_message_notifications() {
                info.default_message_notifications = level;
            }
            if !guild.owner().is_empty() {
                info.owner = guild.owner() == me;
            }
            match guild.premium_tier() {
                Some(tier) if tier != info.premium_tier => {
                    info.premium_tier = tier;
                    true
                }
                _ => false,
            }
        };
        if tier_changed {
            self.replant(&guild.id)?;
        }
        self.ctx.space_info_stale(&guild.id);
        Ok(self.spaces()?)
    }

    pub(super) fn guild_deleted(&self, guild: Guild) -> Result<(), Failure> {
        // Unavailable is an outage, not leaving: the Space and its cache stay.
        if guild.unavailable == Some(true) {
            return Ok(());
        }
        let left = {
            let mut state = self.state.lock().unwrap();
            state.voice.states.retain(|_, voice| voice.guild.as_deref() != Some(guild.id.as_str()));
            if state.members.on_screen.as_deref() == Some(guild.id.as_str()) {
                state.members = Default::default();
            }
            state.guilds.remove(&guild.id).is_some()
        };
        if left {
            self.spaces()?;
        }
        Ok(())
    }

    /// A channel created, moved, renamed or given new permissions.
    pub(super) fn channel_changed(&self, channel: Channel) -> Result<(), Failure> {
        let Some(guild) = channel.guild_id.clone() else {
            return Ok(self.direct(&channel, &HashMap::new())?);
        };
        if !self.state.lock().unwrap().guilds.contains_key(&guild) {
            return Ok(());
        }
        self.keep(&guild, |kept| match kept.iter_mut().find(|known| known.id == channel.id) {
            Some(known) => *known = Kept::of(&channel),
            None => kept.push(Kept::of(&channel)),
        });
        if channel.kind == model::CATEGORY {
            return self.category_changed(&guild, &channel);
        }
        let me = self.me();
        let (row, muted, heads, invite_became_true, invite_lost) = {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(&guild) else {
                return Ok(());
            };
            // Who may see a channel is worked out when the channel itself arrives, here and at
            // connect, and again for the whole server when a role or our membership changes.
            let row = info.can_view(&guild, &me, &channel).then(|| info.row(&guild, &me, &channel)).flatten();
            // The first channel we may see under a category brings the category's header.
            let headed = |group: &String| info.channels.iter().any(|(_, parent)| *parent == number(group));
            let heads = row.as_ref().and_then(|row| row.group.clone()).filter(|group| !headed(group));
            let heads = heads.and_then(|group| info.categories.get(&group).map(|(position, name)| header(&group, name, *position)));
            if row.is_some() {
                info.remember(&channel.id, channel.parent_id.as_deref());
            } else {
                info.channels.retain(|(id, _)| *id != number(&channel.id));
            }
            let invite_became_true = if row.as_ref().is_some_and(|r| r.rights.create_invite) && !info.can_invite {
                info.can_invite = true;
                true
            } else {
                false
            };
            let this_can = row.as_ref().is_some_and(|r| r.rights.create_invite);
            let invite_lost = info.can_invite && !this_can;
            let muted = info.quiet.mutes(&channel.id, channel.parent_id.as_deref(), now());
            (row, muted, heads, invite_became_true, invite_lost)
        };
        if let Some(head) = heads {
            self.ctx.group(&guild, &head)?;
        }
        if invite_became_true {
            self.ctx.set_space_can_create_invites(&guild, true)?;
        } else if invite_lost && self.invite_channel(&guild).is_none() {
            if let Some(info) = self.state.lock().unwrap().guilds.get_mut(&guild) {
                info.can_invite = false;
            }
            self.ctx.set_space_can_create_invites(&guild, false)?;
        }
        match row {
            Some(mut row) => {
                row.space = Some(guild);
                self.ctx.conversation(&row)?;
                self.ctx.muted(&row.id, muted)?;
                self.release_slowmode(&row.id, row.slowmode_seconds, row.rights.slowmode_exempt);
                self.sync_child_threads(&row.id)?;
            }
            // It was visible and no longer is.
            None if self.ctx.known(&channel.id)?.is_some() => self.ctx.conversation_gone(&channel.id)?,
            None => {}
        }
        Ok(())
    }

    /// A category created, renamed or moved. A new name is one group written; a new position
    /// moves every channel under it, so the tree is worked out again. One with no channel we
    /// may see has no header, as at connect.
    fn category_changed(&self, guild: &str, category: &Channel) -> Result<(), Failure> {
        let name = category.name.clone().unwrap_or_default();
        let (before, used) = {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(guild) else {
                return Ok(());
            };
            let before = info.categories.insert(category.id.clone(), (category.position, name.clone()));
            (before, info.channels.iter().any(|(_, parent)| *parent == number(&category.id)))
        };
        match before {
            _ if !used => {}
            Some((position, _)) if position != category.position => self.replant(guild)?,
            Some((_, known)) if known == name => {}
            _ => self.ctx.group(guild, &header(&category.id, &name, category.position))?,
        }
        Ok(())
    }

    pub(super) fn channel_deleted(&self, channel: Channel) -> Result<(), Failure> {
        let (category, had_invite) = {
            let mut state = self.state.lock().unwrap();
            state.privates.remove(&channel.id);
            state.directs.retain(|_, id| *id != channel.id);
            state.covered.remove(&channel.id);
            let had_invite = channel
                .guild_id
                .as_ref()
                .and_then(|guild| state.guilds.get(guild))
                .is_some_and(|info| info.can_invite);
            let info = channel.guild_id.as_ref().and_then(|guild| state.guilds.get_mut(guild));
            let category = info.is_some_and(|info| {
                info.channels.retain(|(id, _)| *id != number(&channel.id));
                info.categories.remove(&channel.id).is_some()
            });
            (category, had_invite)
        };
        if let Some(guild) = &channel.guild_id {
            self.keep(guild, |kept| kept.retain(|known| known.id != channel.id));
            if category {
                self.ctx.group_gone(guild, &channel.id)?;
            }
            if had_invite && self.invite_channel(guild).is_none() {
                if let Some(info) = self.state.lock().unwrap().guilds.get_mut(guild) {
                    info.can_invite = false;
                }
                self.ctx.set_space_can_create_invites(guild, false)?;
            }
        }
        if self.ctx.known(&channel.id)?.is_some() {
            self.ctx.conversation_gone(&channel.id)?;
        }
        Ok(())
    }

    /// A thread we are in, or one already listed, under its channel. An archived one leaves the
    /// tree but keeps its messages; a forum's post is recorded, and was never a tree row.
    pub(super) fn thread_changed(&self, thread: Channel, joined: bool) -> Result<(), Failure> {
        let known = self.ctx.known(&thread.id)?;
        if known.is_none() && !joined && thread.member.is_none() {
            return Ok(());
        }
        let parent = match thread.parent_id.as_deref() {
            Some(parent) => self.ctx.known(parent)?,
            None => None,
        };
        // A thread under a channel we cannot see is not ours to list.
        let Some(parent) = parent else {
            return Ok(());
        };
        let name = thread.name.clone().unwrap_or_else(|| "Thread".to_owned());
        let exempt = parent
            .space
            .as_deref()
            .and_then(|guild| self.channel_permissions(guild, &parent.key.id))
            .map_or(parent.rights.slowmode_exempt || parent.rights.manage_threads, |perms| slowmode_exempt(perms, true));
        let me = self.me();
        let is_owner = thread.owner_id.as_deref() == Some(me.as_str());
        let clock = now();
        let timed_out = parent.space.as_deref().and_then(|guild| {
            let state = self.state.lock().unwrap();
            state.guilds.get(guild).and_then(|info| info.timed_out(clock))
        });
        let open = timed_out.is_none();
        let mut thread_rights = parent.rights;
        thread_rights.add_members = open && (thread_rights.send_in_threads || thread_rights.manage_threads);
        thread_rights.rename = open && (thread_rights.manage_threads || is_owner);
        thread_rights.slowmode_exempt = exempt;
        let archived = thread.thread_metadata.as_ref().is_some_and(|metadata| metadata.archived);
        let locked = thread.thread_metadata.as_ref().is_some_and(|metadata| metadata.locked);
        let followed = joined || thread.member.is_some() || known.as_ref().is_some_and(|k| k.followed);
        let (read_only, read_only_reason) = thread_read_only(timed_out, clock, locked, &thread_rights);
        let mut row = ConversationRow {
            parent: Some(parent.key.id.clone()),
            root: Some(thread.id.clone()),
            private: thread.kind == model::PRIVATE_THREAD,
            read_only,
            read_only_reason,
            rights: thread_rights,
            slowmode_seconds: thread.rate_limit_per_user,
            locked,
            age_restricted: thread.nsfw || parent.age_restricted,
            filing: Some(super::kept_filing(known.as_ref(), archived, followed)),
            ..ConversationRow::new(&thread.id, ConversationKind::Thread, name)
        };
        // An archived thread leaves the tree, and a forum's posts were never in it
        // (`polish/01` §3): both stay recorded, read by their parent.
        let in_tree = !archived && followed && parent.kind != ConversationKind::Forum;
        if in_tree {
            row.space = parent.space.clone();
            row.group = parent.group.clone();
            row.position = parent.position + 1;
        }
        if let Some(guild) = &parent.space {
            self.note_thread_listed(guild, &thread.id, thread.parent_id.as_deref(), in_tree);
        }
        self.ctx.conversation(&row)?;
        self.release_slowmode(&thread.id, row.slowmode_seconds, row.rights.slowmode_exempt);
        self.note_thread_update(&thread)?;
        Ok(())
    }

    /// Lists a thread under its server, or takes it back out, and remembers it on the guild
    /// while it stays in the sidebar tree.
    pub(super) fn note_thread_listed(&self, guild: &str, thread_id: &str, parent_id: Option<&str>, in_tree: bool) {
        self.ctx.set_note(&format!("guild:{thread_id}"), guild);
        let note = format!("threads:{guild}");
        let before = self.ctx.note(&note).unwrap_or_default();
        let mut listed: Vec<&str> = before.split(',').filter(|id| !id.is_empty() && *id != thread_id).collect();
        if in_tree {
            listed.push(thread_id);
        }
        let after = listed.join(",");
        if after != before {
            self.ctx.set_note(&note, &after);
        }
        if in_tree {
            let mut state = self.state.lock().unwrap();
            if let Some(info) = state.guilds.get_mut(guild) {
                info.remember(thread_id, parent_id);
            }
        }
    }

    /// Drops a live slowmode countdown when the interval is off or this account is exempt.
    pub(super) fn release_slowmode(&self, id: &str, seconds: u32, exempt: bool) {
        if seconds == 0 || exempt {
            self.ctx.slowmode_until(id, 0);
        }
    }

    fn release_idle_slowmode(&self, rows: &[ConversationRow]) {
        for row in rows {
            self.release_slowmode(&row.id, row.slowmode_seconds, row.rights.slowmode_exempt);
        }
    }

    /// Rewrites stored child threads after the parent's permissions or slowmode change.
    /// The stored interval stays; exemption and read-only follow the parent channel's permissions.
    fn sync_child_threads(&self, parent_id: &str) -> rusqlite::Result<()> {
        let Some(parent) = self.ctx.known(parent_id)? else {
            return Ok(());
        };
        let children = self.ctx.child_threads(parent_id)?;
        if children.is_empty() {
            return Ok(());
        }
        let perms = parent.space.as_deref().and_then(|guild| self.channel_permissions(guild, parent_id));
        let clock = now();
        let timed_out = parent.space.as_deref().and_then(|guild| {
            let state = self.state.lock().unwrap();
            state.guilds.get(guild).and_then(|info| info.timed_out(clock))
        });
        let exempt = match perms {
            Some(bits) => slowmode_exempt(bits, true),
            None => parent.rights.slowmode_exempt || parent.rights.manage_threads,
        };
        let open = timed_out.is_none();
        for child in children {
            let mut row = super::renamed(&child, &child.title);
            let mut rights = parent.rights;
            rights.add_members = open && (rights.send_in_threads || rights.manage_threads);
            // A stored rename from MANAGE_THREADS does not survive losing that role. An owner rename does.
            rights.rename = open && (rights.manage_threads || (child.rights.rename && !child.rights.manage_threads));
            rights.slowmode_exempt = exempt;
            let (read_only, read_only_reason) = thread_read_only(timed_out, clock, child.locked, &rights);
            row.rights = rights;
            row.read_only = read_only;
            row.read_only_reason = read_only_reason;
            self.ctx.conversation(&row)?;
            // A zero mask is left untouched by the upsert, so write the computed bits on their own.
            self.ctx.rights(&row.id, row.rights)?;
            self.release_slowmode(&child.key.id, child.slowmode_seconds, exempt);
        }
        Ok(())
    }

    /// The user changed a mute or suppress setting in Discord.
    pub(super) fn settings_changed(&self, settings: GuildSettings) -> Result<(), Failure> {
        let fresh = quiet(&settings);
        {
            let mut state = self.state.lock().unwrap();
            match &settings.guild_id {
                None => state.quiet_directs = fresh,
                Some(guild) => match state.guilds.get_mut(guild) {
                    Some(info) => info.quiet = fresh,
                    None => return Ok(()),
                },
            }
        }
        self.apply_quiet(settings.guild_id.as_deref())?;
        // What is opted in, hidden or collapsed lives on the tree and the Space.
        if let Some(guild) = &settings.guild_id {
            self.replant(guild)?;
            let cats: Vec<(String, bool)> = {
                let state = self.state.lock().unwrap();
                state
                    .guilds
                    .get(guild)
                    .map(|info| {
                        info.categories
                            .keys()
                            .map(|id| (id.clone(), info.quiet.collapsed.contains(id)))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for (cat_id, is_collapsed) in cats {
                self.ctx.set_collapsed(guild, &cat_id, is_collapsed)?;
            }
        }
        self.reschedule_unmute();
        Ok(())
    }

    /// Writes the mute of every row a settings entry covers.
    fn apply_quiet(&self, guild: Option<&str>) -> rusqlite::Result<()> {
        let now = now();
        let rows: Vec<(String, bool)> = {
            let state = self.state.lock().unwrap();
            match guild.map(|guild| state.guilds.get(guild)) {
                None => state.privates.iter().map(|id| (id.clone(), state.quiet_directs.mutes(id, None, now))).collect(),
                Some(None) => Vec::new(),
                Some(Some(info)) => {
                    let each = info.channels.iter().map(|(channel, parent)| {
                        let (channel, parent) = (channel.to_string(), parent.to_string());
                        let muted = info.quiet.mutes(&channel, Some(&parent), now);
                        (channel, muted)
                    });
                    each.collect()
                }
            }
        };
        for (id, muted) in rows {
            self.ctx.muted(&id, muted)?;
        }
        Ok(())
    }

    pub(super) fn reschedule_unmute(&self) {
        let now = now();
        let mut state = self.state.lock().unwrap();
        let ends = state.guilds.values().filter_map(|info| info.quiet.next_end(now));
        let timeouts = state.guilds.values().filter_map(|info| info.timed_out(now));
        let next = ends
            .chain(timeouts)
            .chain(state.quiet_directs.next_end(now))
            .chain(state.own_status_until)
            .min();
        state.next_unmute = next;
    }

    /// True when the user has `CREATE_INSTANT_INVITE` (bit 0) on `channel` inside `guild`.
    pub(super) fn can_invite_channel(&self, guild: &str, channel_id: &str) -> bool {
        let kept: Vec<Kept> = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|kept| serde_json::from_str(&kept).ok())
            .unwrap_or_default();
        let Some(channel) = kept.into_iter().find(|k| k.id == channel_id).map(Kept::channel) else {
            return false;
        };
        if channel.kind == model::CATEGORY || channel.is_thread() {
            return false;
        }
        let clock = now();
        let state = self.state.lock().unwrap();
        let Some(info) = state.guilds.get(guild) else {
            return false;
        };
        info.timed_out(clock).is_none()
            && info.can_view(guild, &state.me, &channel)
            && (info.permissions(guild, &state.me, &channel) & CREATE_INSTANT_INVITE) != 0
    }

    /// Picks the first visible channel in `guild` (in Discord's sidebar order, text before voice)
    /// where the user has `CREATE_INSTANT_INVITE` (bit 0), matching Discord's server-level action.
    pub(super) fn invite_channel(&self, guild: &str) -> Option<String> {
        let kept: Vec<Kept> = self
            .ctx
            .note(&format!("kept:{guild}"))
            .and_then(|kept| serde_json::from_str(&kept).ok())
            .unwrap_or_default();
        let clock = now();
        let state = self.state.lock().unwrap();
        let info = state.guilds.get(guild)?;
        if info.timed_out(clock).is_some() {
            return None;
        }
        let me = &state.me;
        let mut candidates: Vec<(i32, String)> = kept
            .into_iter()
            .map(Kept::channel)
            .filter(|ch| {
                kind_of(ch).is_some()
                    && !ch.is_thread()
                    && info.can_view(guild, me, ch)
                    && (info.permissions(guild, me, ch) & CREATE_INSTANT_INVITE) != 0
            })
            .map(|ch| {
                let category = ch
                    .parent_id
                    .as_deref()
                    .and_then(|parent| info.categories.get(parent))
                    .map(|(position, _)| *position);
                (place(category, ch.is_voice(), ch.position), ch.id)
            })
            .collect();
        candidates.sort_by_key(|(pos, _)| *pos);
        candidates.into_iter().next().map(|(_, id)| id)
    }

    /// A timed mute, server timeout, or custom status expiry ran out: update local state.
    pub(super) fn unmute_tick(&self) {
        let clock = now();
        let due = self.state.lock().unwrap().next_unmute.is_some_and(|at| at <= clock);
        if !due {
            return;
        }
        let (guilds, expired_timeouts, expired_status): (Vec<String>, Vec<String>, Option<(String, Presence)>) = {
            let mut state = self.state.lock().unwrap();
            let guilds = state.guilds.keys().cloned().collect();
            let mut expired = Vec::new();
            for (id, info) in state.guilds.iter_mut() {
                if info.timeout_until.is_some_and(|until| until <= clock) {
                    info.timeout_until = None;
                    expired.push(id.clone());
                }
            }
            let expired_status = if state.own_status_until.is_some_and(|until| until <= clock) {
                let word = match state.own.0 {
                    Presence::Idle => "idle",
                    Presence::Dnd => "dnd",
                    Presence::Offline => "invisible",
                    _ => "online",
                };
                let cleared = proto::with_custom_status(&state.status_settings, word, Some(&proto::CustomStatus::default()));
                state.status_settings = cleared;
                state.own.1.clear();
                state.own_emoji.clear();
                state.own_emoji_id = None;
                state.own_status_until = None;
                Some((state.me.clone(), state.own.0))
            } else {
                None
            };
            (guilds, expired, expired_status)
        };
        if let Some((me, presence)) = expired_status {
            self.ctx.presence(&me, presence, "", true);
            self.ctx.activity(&me, "", None, true);
            self.ctx.status_until(&me, None, true);
        }
        for guild in guilds.iter().map(|guild| Some(guild.as_str())).chain([None]) {
            if let Err(err) = self.apply_quiet(guild) {
                self.log("mute", err);
            }
        }
        for guild in expired_timeouts {
            if let Err(err) = self.replant(&guild) {
                self.log("timeout", err);
            }
        }
        self.reschedule_unmute();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_orders_text_before_voice_and_categories_by_their_position() {
        let loose = place(None, false, 3);
        let first_text = place(Some(0), false, 9);
        let first_voice = place(Some(0), true, 0);
        let second = place(Some(1), false, 0);
        assert!(loose < place(Some(0), false, 0) - 1, "uncategorised channels come before the first header");
        assert!(first_text < first_voice && first_voice < second - 1);
        assert!(place(Some(5000), true, 50_000) > 0, "absurd positions do not overflow");
    }

    #[test]
    fn collapsed_category_override_seeds_quiet_and_debounces_writes() {
        let settings: GuildSettings = serde_json::from_value(serde_json::json!({
            "guild_id": "4000",
            "channel_overrides": [
                { "channel_id": "4100", "collapsed": true, "muted": false },
                { "channel_id": "4200", "collapsed": false, "muted": false },
                { "channel_id": "4300", "muted": true }
            ]
        }))
        .unwrap();
        let parsed = quiet(&settings);
        assert!(parsed.collapsed.contains("4100"), "collapsed: true must be tracked");
        assert!(!parsed.collapsed.contains("4200"), "collapsed: false must not be tracked");
        assert!(!parsed.collapsed.contains("4300"), "missing collapsed defaults to expanded");

        let mut queue = super::super::CollapseQueue::default();
        let t0 = std::time::Instant::now();
        queue.push("4000", "4100", true, t0);
        queue.push("4000", "4100", false, t0 + std::time::Duration::from_millis(100));
        queue.push("4000", "4200", true, t0 + std::time::Duration::from_millis(150));
        assert!(queue.take_due(t0 + std::time::Duration::from_millis(300)).is_empty(), "burst must debounce");
        let due = queue.take_due(t0 + std::time::Duration::from_millis(700));
        assert_eq!(due.len(), 1, "burst of clicks in one guild coalesces into one PATCH");
        let (guild, overrides) = &due[0];
        assert_eq!(guild, "4000");
        let body = super::super::collapse_settings_body(overrides);
        assert_eq!(body["channel_overrides"]["4100"]["collapsed"], false);
        assert_eq!(body["channel_overrides"]["4200"]["collapsed"], true);
    }

    #[test]
    fn resolves_highest_nonzero_role_color_and_role_icon_and_updates_on_member_change() {
        let mut guild = GuildInfo::default();
        // Low role with color + emoji
        guild.roles.insert(
            1,
            RoleInfo {
                name: "Member".into(),
                permissions: 0,
                position: 1,
                color: 0x112233,
                icon: None,
                unicode_emoji: Some("⭐".into()),
                ..Default::default()
            },
        );
        // High role with color == 0 (transparent) and icon hash
        guild.roles.insert(
            2,
            RoleInfo {
                name: "Booster".into(),
                permissions: 0,
                position: 10,
                color: 0,
                icon: Some("hash_abc".into()),
                unicode_emoji: None,
                ..Default::default()
            },
        );
        // Mid role with non-zero color
        guild.roles.insert(
            3,
            RoleInfo {
                name: "Mod".into(),
                permissions: 0,
                position: 5,
                color: 0xe74c3c,
                icon: None,
                unicode_emoji: None,
                ..Default::default()
            },
        );

        let (color, icon, emoji) = guild.resolve_role_style(&[1, 2, 3]);
        assert_eq!(color, Some(0xe74c3c), "highest position role with non-zero color wins");
        assert_eq!(
            icon.as_ref().map(|(f, _)| f.id.as_str()),
            Some("roleicon:2:hash_abc"),
            "highest position role with icon wins"
        );
        assert_eq!(emoji, None, "icon hash takes precedence on highest icon role");

        // When member loses roles 2 and 3 (e.g. GUILD_MEMBER_UPDATE), style falls back to role 1
        guild.set_member_roles("u1", vec![1]);
        let (color2, icon2, emoji2) = guild.member_role_style("u1");
        assert_eq!(color2, Some(0x112233));
        assert!(icon2.is_none());
        assert_eq!(emoji2.as_deref(), Some("⭐"));
    }
}
