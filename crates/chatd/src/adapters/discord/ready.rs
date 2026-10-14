//! READY: everything the session starts with, written to the store and then dropped. Also the
//! server, channel and settings events that change the same things afterwards.

use super::super::{Command, Failure};
use super::model::{
    self, number, seconds, Channel, Guild, GuildSettings, Kept, Member, MuteConfig, PresenceUpdate,
    ReadState, Relationship, Role, User, Versioned, VoiceState, ADMINISTRATOR, SEND_MESSAGES, VIEW_CHANNEL,
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
    Quiet {
        muted_until: settings.muted.then(|| until(settings.mute_config.as_ref())),
        suppress_everyone: settings.suppress_everyone,
        suppress_roles: settings.suppress_roles,
        channels: muted.map(|channel| (channel.channel_id.clone(), until(channel.mute_config.as_ref()))).collect(),
        hide_muted: settings.hide_muted_channels,
        opt_in: settings.flags & model::OPT_IN_CHANNELS_ON != 0,
        opted_in: opted.map(|channel| channel.channel_id.clone()).collect(),
        level: settings.message_notifications,
        levels: levels.map(|channel| (channel.channel_id.clone(), channel.message_notifications)).collect(),
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

impl GuildInfo {
    /// Discord's permission rule for a channel: the roles' own permissions, then the channel's
    /// overwrites for everyone, for our roles, and for us.
    fn permissions(&self, guild: &str, me: &str, channel: &Channel) -> u64 {
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
        Some(ConversationRow {
            group: parent.filter(|_| category.is_some()).map(str::to_owned),
            position: place(category, channel.is_voice(), channel.position),
            call: super::voice::support(channel.kind),
            topic: one_line(channel.topic.as_deref()),
            read_only: written && self.permissions(guild, me, channel) & SEND_MESSAGES == 0,
            private: self.private(guild, channel),
            opted_out: self.quiet.opted_out(&channel.id, parent),
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
    counts: Vec<(String, u32, u32, Option<String>, i64)>,
}

impl Discord {
    fn guild_info(&self, guild: &Guild, member: Option<&Member>, me: &str, position: i32, quiet: Quiet) -> GuildInfo {
        let roles = guild.roles.iter().map(|role| {
            let info = RoleInfo {
                name: role.name.clone(),
                permissions: role.permissions,
            };
            (number(&role.id), info)
        });
        let categories = guild.channels.iter().filter(|channel| channel.kind == model::CATEGORY);
        let categories = categories.map(|category| (category.id.clone(), (category.position, category.name.clone().unwrap_or_default())));
        GuildInfo {
            name: guild.name().to_owned(),
            icon: guild.icon().map(str::to_owned),
            owner: guild.owner() == me,
            position,
            joined: guild.joined_at.as_deref().and_then(model::iso_seconds).unwrap_or(0),
            roles: roles.collect(),
            my_roles: member.map_or_else(Vec::new, |member| member.roles.iter().map(|role| number(role)).collect()),
            categories: categories.collect(),
            channels: Vec::new(),
            quiet,
            nick: member.and_then(|member| member.nick.clone()).unwrap_or_default(),
            emojis: guild.emojis.iter().filter(|emoji| emoji.available).filter_map(|emoji| {
                Some(super::Pickable {
                    id: emoji.id.clone()?,
                    name: emoji.name.clone(),
                    animated: emoji.animated,
                    format: 0,
                })
            }).collect(),
            stickers: guild.stickers.iter().filter(|sticker| sticker.available).map(|sticker| super::Pickable {
                id: sticker.id.clone(),
                name: sticker.name.clone(),
                animated: sticker.format_type != 1,
                format: sticker.format_type,
            }).collect(),
        }
    }

    /// A server's tree: only what we may see, with empty categories left out and the threads we
    /// are in under their channel.
    fn tree(&self, guild: &Guild, info: &mut GuildInfo, me: &str, read: &HashMap<String, ReadState>) -> Tree {
        let mut rows = Vec::new();
        let mut counts = Vec::new();
        let mut placed: HashMap<&str, (Option<String>, i32)> = HashMap::new();
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
            (channel.id.clone(), u32::from(unread).max(u32::from(mentions > 0)), mentions, read_to, last.map_or(0, seconds))
        };
        for channel in &guild.channels {
            if channel.kind == model::CATEGORY || !info.can_view(&guild.id, me, channel) {
                continue;
            }
            let Some(row) = info.row(&guild.id, me, channel) else {
                continue;
            };
            placed.insert(&channel.id, (row.group.clone(), row.position));
            info.remember(&channel.id, channel.parent_id.as_deref());
            // A voice channel's chat and a forum's newest post are not "unread" in the tree.
            if matches!(row.kind, ConversationKind::Channel | ConversationKind::Announcement) {
                counts.push(standing(channel, Some(info.joined)));
            }
            rows.push(row);
        }
        let mut under: HashMap<&str, i32> = HashMap::new();
        for thread in &guild.threads {
            let archived = thread.thread_metadata.as_ref().is_some_and(|metadata| metadata.archived);
            let Some((group, position)) = thread.parent_id.as_deref().and_then(|parent| placed.get(parent)) else {
                continue;
            };
            if archived || !thread.is_thread() {
                continue;
            }
            let nth = under.entry(thread.parent_id.as_deref().unwrap_or("")).or_insert(0);
            *nth = (*nth + 1).min(99);
            let name = thread.name.clone().unwrap_or_else(|| "Thread".to_owned());
            rows.push(ConversationRow {
                group: group.clone(),
                position: position + *nth,
                parent: thread.parent_id.clone(),
                // A thread started from a message has that message's id.
                root: Some(thread.id.clone()),
                // A server's threads in READY are the ones we were added to.
                filing: Some(Filing {
                    followed: true,
                    ..Filing::default()
                }),
                ..ConversationRow::new(&thread.id, ConversationKind::Thread, name)
            });
            info.remember(&thread.id, thread.parent_id.as_deref());
            counts.push(standing(thread, None));
        }
        let used: HashSet<&str> = rows.iter().filter_map(|row| row.group.as_deref()).collect();
        let categories = guild.channels.iter().filter(|channel| channel.kind == model::CATEGORY);
        let now = now();
        let groups = categories.filter(|category| used.contains(category.id.as_str())).map(|category| Group {
            // A muted category starts collapsed. The server's own mute is on the Space.
            muted: info.quiet.channels.get(&category.id).is_some_and(|until| *until > now),
            ..header(&category.id, category.name.as_deref().unwrap_or(""), category.position)
        });
        Tree {
            groups: groups.collect(),
            rows,
            counts,
        }
    }

    /// Writes one server's tree, with each row's unread state and mute, and keeps what is needed
    /// to work the tree out again when a role or our own membership changes.
    fn plant(&self, guild: &Guild, info: &mut GuildInfo, me: &str, read: &HashMap<String, ReadState>) -> rusqlite::Result<()> {
        let tree = self.tree(guild, info, me, read);
        self.ctx.tree(&guild.id, &tree.groups, &tree.rows)?;
        let now = now();
        // CONTRACT: unread state and mute are written a row at a time, each its own commit.
        // The first connect of an account with 3,000 channels is some 6,000 of them and takes
        // about 2.5 s (`tests/discord.rs`, the READY of real size); later ones change little.
        for (id, unread, mentions, read_to, updated) in &tree.counts {
            self.ctx.counts(id, *unread, *mentions, read_to.as_deref(), *updated)?;
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

    /// Works out a server's visible channels again from what is kept of them, after a role's
    /// permissions or our own roles changed. Unread state and threads stay as they are: the
    /// store keeps both through a tree replacement.
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
        for (row, muted) in tree.rows.iter().zip(muted) {
            self.ctx.muted(&row.id, muted)?;
        }
        let flags = self.state.lock().unwrap().guilds.get(guild_id).map(|info| (info.quiet.server_muted(now()), info.quiet.hide_muted));
        if let Some((muted, hide)) = flags {
            self.ctx.space_flags(guild_id, muted, hide)?;
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
                    let fresh = RoleInfo {
                        name: role.name.clone(),
                        permissions: role.permissions,
                    };
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

    /// Our own membership of a server changed: with other roles we may see other channels.
    pub(super) fn member_changed(&self, guild: &str, member: &Member) -> Result<(), Failure> {
        if member.id() != self.me() {
            return Ok(());
        }
        let mut roles: Vec<u64> = member.roles.iter().map(|role| number(role)).collect();
        roles.sort_unstable();
        let moved = {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(guild) else {
                return Ok(());
            };
            let mut before = std::mem::replace(&mut info.my_roles, roles.clone());
            before.sort_unstable();
            before != roles
        };
        if moved {
            self.replant(guild)?;
        }
        Ok(())
    }

    /// Every server as a Space, in the user's order.
    pub(super) fn spaces(&self) -> rusqlite::Result<()> {
        let now = now();
        let mut flags = Vec::new();
        let (rows, files): (Vec<SpaceRow>, Vec<(MediaFile, String)>) = {
            let state = self.state.lock().unwrap();
            flags.extend(state.guilds.iter().map(|(id, info)| (id.clone(), info.quiet.server_muted(now), info.quiet.hide_muted)));
            let listed = state.guilds.iter().map(|(id, info)| {
                let icon = info.icon.as_deref().map(|hash| model::guild_icon(id, hash));
                let row = SpaceRow {
                    id: id.clone(),
                    name: info.name.clone(),
                    icon: icon.as_ref().map(|(file, _)| file.id.clone()),
                    position: info.position,
                };
                (row, icon)
            });
            let (rows, icons): (Vec<_>, Vec<_>) = listed.unzip();
            (rows, icons.into_iter().flatten().collect())
        };
        self.ctx.media(&files)?;
        self.ctx.spaces(&rows)?;
        // A server's own mute and "hide muted channels", once its Space exists to hold them.
        for (id, muted, hide) in flags {
            self.ctx.space_flags(&id, muted, hide)?;
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
                }
                if let Some(hash) = &channel.icon {
                    let (file, url) = model::group_icon(&channel.id, hash);
                    row.avatar = Some(file.id.clone());
                    files.push((file, url));
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
    pub(super) fn capabilities(&self, premium: u8) {
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
            custom_emoji: emoji,
            silent: true,
            schedule: false,
            suppress_embeds: true,
            reply_ping: true,
            edit_window: 0,
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
        self.ctx.describe(ready.user.name(), &me)?;
        self.capabilities(ready.user.premium_type);
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
            state.rows = kept;
            state.relations = relations.iter().cloned().collect();
            state.directs.clear();
            state.privates.clear();
            state.presence.clear();
            state.doing.clear();
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
            icons.extend(info.icon.as_deref().map(|hash| model::guild_icon(&guild.id, hash)));
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
        // The Space on screen is subscribed again on the new session.
        self.sync_subscription();
        Ok(())
    }

    /// Our own presence and status line, as the settings have them: at connect, and whenever
    /// they change on any device.
    pub(super) fn own_status(&self, status: Vec<u8>) {
        let (word, text) = proto::own_status(&status);
        let presence = match word.as_str() {
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            "invisible" => Presence::Offline,
            _ => Presence::Online,
        };
        let me = {
            let mut state = self.state.lock().unwrap();
            state.status_settings = status;
            state.own = (presence, text.clone());
            state.me.clone()
        };
        self.ctx.presence(&me, presence, &text, true);
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
        self.voice_states(Some(&guild.id), &guild.voice_states);
        self.plant(&guild, &mut info, &me, &HashMap::new())?;
        self.state.lock().unwrap().guilds.insert(guild.id.clone(), info);
        Ok(self.spaces()?)
    }

    /// A server renamed or given a new icon. The payload names only what it wants shown.
    pub(super) fn guild_updated(&self, guild: Guild) -> Result<(), Failure> {
        {
            let mut state = self.state.lock().unwrap();
            let Some(info) = state.guilds.get_mut(&guild.id) else {
                return Ok(());
            };
            if guild.name.is_some() || guild.properties.as_ref().is_some_and(|properties| properties.name.is_some()) {
                info.name = guild.name().to_owned();
            }
            info.icon = guild.icon().map(str::to_owned);
        }
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
        let (row, muted, heads) = {
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
            let muted = info.quiet.mutes(&channel.id, channel.parent_id.as_deref(), now());
            (row, muted, heads)
        };
        if let Some(head) = heads {
            self.ctx.group(&guild, &head)?;
        }
        match row {
            Some(mut row) => {
                row.space = Some(guild);
                self.ctx.conversation(&row)?;
                self.ctx.muted(&row.id, muted)?;
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
        let category = {
            let mut state = self.state.lock().unwrap();
            state.privates.remove(&channel.id);
            state.directs.retain(|_, id| *id != channel.id);
            state.covered.remove(&channel.id);
            let info = channel.guild_id.as_ref().and_then(|guild| state.guilds.get_mut(guild));
            info.is_some_and(|info| {
                info.channels.retain(|(id, _)| *id != number(&channel.id));
                info.categories.remove(&channel.id).is_some()
            })
        };
        if let Some(guild) = &channel.guild_id {
            self.keep(guild, |kept| kept.retain(|known| known.id != channel.id));
            if category {
                self.ctx.group_gone(guild, &channel.id)?;
            }
        }
        if self.ctx.known(&channel.id)?.is_some() {
            self.ctx.conversation_gone(&channel.id)?;
        }
        Ok(())
    }

    /// A thread we are in, or one already listed, under its channel. An archived one leaves the
    /// tree but keeps its messages.
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
        let mut row = ConversationRow {
            parent: Some(parent.key.id.clone()),
            root: Some(thread.id.clone()),
            // Joined, or our membership came with it: followed. Otherwise left as it was.
            filing: (joined || thread.member.is_some()).then(|| Filing {
                followed: true,
                ..Filing::default()
            }),
            ..ConversationRow::new(&thread.id, ConversationKind::Thread, name)
        };
        let archived = thread.thread_metadata.as_ref().is_some_and(|metadata| metadata.archived);
        if let Some(guild) = &parent.space {
            self.ctx.set_note(&format!("guild:{}", thread.id), guild);
        }
        if !archived {
            row.space = parent.space.clone();
            row.group = parent.group.clone();
            row.position = parent.position + 1;
            let mut state = self.state.lock().unwrap();
            if let Some(info) = parent.space.as_ref().and_then(|guild| state.guilds.get_mut(guild)) {
                info.remember(&thread.id, thread.parent_id.as_deref());
            }
        }
        // The note `unlist_threads` reads at the next connect follows what is listed now.
        if let Some(guild) = &parent.space {
            let note = format!("threads:{guild}");
            let before = self.ctx.note(&note).unwrap_or_default();
            let mut listed: Vec<&str> = before.split(',').filter(|id| !id.is_empty() && *id != thread.id).collect();
            if !archived {
                listed.push(&thread.id);
            }
            let listed = listed.join(",");
            if listed != before {
                self.ctx.set_note(&note, &listed);
            }
        }
        Ok(self.ctx.conversation(&row)?)
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

    fn reschedule_unmute(&self) {
        let now = now();
        let mut state = self.state.lock().unwrap();
        let ends = state.guilds.values().filter_map(|info| info.quiet.next_end(now));
        let next = ends.chain(state.quiet_directs.next_end(now)).min();
        state.next_unmute = next;
    }

    /// A timed mute ran out: the rows it covered are no longer dimmed.
    pub(super) fn unmute_tick(&self) {
        let due = self.state.lock().unwrap().next_unmute.is_some_and(|at| at <= now());
        if !due {
            return;
        }
        let guilds: Vec<String> = self.state.lock().unwrap().guilds.keys().cloned().collect();
        for guild in guilds.iter().map(|guild| Some(guild.as_str())).chain([None]) {
            if let Err(err) = self.apply_quiet(guild) {
                self.log("mute", err);
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
}
