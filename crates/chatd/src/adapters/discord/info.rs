//! What the info pane, the card and the conversation menu ask of Discord: a profile in a server,
//! a conversation's and a server's info, a mute, leaving a group or closing a DM, the pins, and
//! the server-side search for what was shared. Each is one request at most, and only because
//! the user opened or clicked something.

use super::super::{About, ConversationAbout, Failure, ForumPosts, Refusal, Reply, SearchPage, SearchScope, SharedPage, SpaceAbout};
use super::model::{self, number, GuildSettings, Posted};
use super::rest::Refused;
use super::{escaped, refusal, Discord};
use crate::hub::now;
use crate::store::{PostCard, PreviewLine};
use protocol::{
    ConversationKind, MediaKind, Message, MessageHit, Notify, SearchFilters, SearchHas, SearchSort,
    SharedItem, SharedKind, Style, TopicSort,
};
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
/// Discord's forum thread search answers 25 posts a page at most.
const FORUM_PAGE: u32 = 25;
/// Most characters Discord's forum search accepts in `name`.
const FORUM_NAME_MAX: usize = 100;
/// Most characters Discord's message search accepts in `content`.
const SEARCH_MAX_CHARS: usize = 1024;
/// Recent message searches kept in memory per account, so the same scope and words are not
/// asked twice in a session.
const REMEMBERED_SEARCHES: usize = 32;

#[derive(Default)]
pub(super) struct MessageSearch {
    last: Option<Instant>,
    remembered: std::collections::VecDeque<(SearchScope, String, SearchPage)>,
}

fn search_has_param(has: SearchHas) -> &'static str {
    match has {
        SearchHas::Files => "file",
        SearchHas::Links => "link",
        SearchHas::Images => "image",
        SearchHas::Videos => "video",
        SearchHas::Embeds => "embed",
        SearchHas::Sounds => "sound",
        SearchHas::Stickers => "sticker",
        SearchHas::Polls => "poll",
    }
}

/// Builds Discord's search URL and resolved offset from structured filters, sort, and cursor.
fn build_search_path(
    base: &str,
    scope_channel: Option<&str>,
    query: &str,
    filters: &SearchFilters,
    cursor: Option<&str>,
    me: &str,
) -> (String, u32) {
    let mut params: Vec<String> = Vec::new();
    if let Some(ch) = scope_channel {
        params.push(format!("channel_id={}", escaped(ch)));
    } else if let Some(in_ch) = filters.in_channel.as_ref().filter(|k| !k.id.is_empty()) {
        params.push(format!("channel_id={}", escaped(&in_ch.id)));
    }
    if !query.is_empty() {
        params.push(format!("content={}", escaped(query)));
    }
    if let Some(from) = filters.from.as_ref().filter(|k| !k.id.is_empty()) {
        params.push(format!("author_id={}", escaped(&from.id)));
    }
    if let Some(mentions) = filters.mentions.as_ref().filter(|k| !k.id.is_empty()) {
        params.push(format!("mentions={}", escaped(&mentions.id)));
    } else if filters.mentions_me && !me.is_empty() {
        params.push(format!("mentions={}", escaped(me)));
    }
    if let Some(has) = filters.has {
        params.push(format!("has={}", search_has_param(has)));
    }
    if let Some(after) = filters.after.filter(|ts| *ts > 0) {
        params.push(format!("min_id={}", model::snowflake_from_unix(after)));
    }
    if let Some(before) = filters.before.filter(|ts| *ts > 0) {
        params.push(format!("max_id={}", model::snowflake_from_unix(before)));
    }
    if let Some(pinned) = filters.pinned {
        params.push(format!("pinned={}", if pinned { "true" } else { "false" }));
    }
    if let Some(kind) = filters.author_type.as_deref() {
        let lower = kind.trim().to_ascii_lowercase();
        if matches!(lower.as_str(), "user" | "bot" | "webhook") {
            params.push(format!("author_type={lower}"));
        }
    }
    match filters.sort {
        SearchSort::Newest => {
            params.push("sort_by=timestamp".into());
            params.push("sort_order=desc".into());
        }
        SearchSort::Oldest => {
            params.push("sort_by=timestamp".into());
            params.push("sort_order=asc".into());
        }
        SearchSort::Relevant => {
            params.push("sort_by=relevance".into());
            params.push("sort_order=desc".into());
        }
    }
    let offset: u32 = cursor
        .and_then(|c| c.parse().ok())
        .unwrap_or(filters.offset);
    if offset > 0 {
        params.push(format!("offset={offset}"));
    }
    let path = if params.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", params.join("&"))
    };
    (path, offset)
}

fn refused(why: impl std::fmt::Display) -> Refusal {
    Refusal::Refused(why.to_string())
}

fn failure_words(failed: Failure) -> Refusal {
    let why = match failed {
        Failure::SignedOut(why) | Failure::Dropped(why) => why,
    };
    refused(why)
}

fn forum_search_failed(failed: Refused, plain: bool) -> Result<Option<ForumPosts>, Refusal> {
    let denied = matches!(failed, Refused::Status { status: 403, .. } | Refused::Status { code: 50001, .. });
    if denied || !plain {
        return Err(refusal(failed));
    }
    Ok(None)
}

/// `GET /channels/{id}/threads/search`. `name` is the documented text query. `archived` is left
/// off so both active and archived posts come back. Nothing pages past this one request.
fn forum_search_path(channel: &str, sort: TopicSort, tag: &str, query: &str, offset: u32) -> String {
    let sort_by = match sort {
        TopicSort::Activity => "last_message_time",
        TopicSort::Created => "creation_time",
    };
    let mut params = vec![format!("sort_by={sort_by}"), "sort_order=desc".to_owned(), format!("limit={FORUM_PAGE}")];
    if !tag.is_empty() {
        params.push(format!("tag={}", escaped(tag)));
    }
    let name: String = query.chars().take(FORUM_NAME_MAX).collect();
    if !name.is_empty() {
        params.push(format!("name={}", escaped(&name)));
    }
    if offset > 0 {
        params.push(format!("offset={offset}"));
    }
    format!("/channels/{}/threads/search?{}", escaped(channel), params.join("&"))
}

fn post_thumb(message: &Message) -> String {
    let pictured = |kind: MediaKind| matches!(kind, MediaKind::Image | MediaKind::Gif | MediaKind::Video);
    message
        .attachments
        .iter()
        .find(|attachment| !attachment.spoiler && (pictured(attachment.file.kind) || attachment.thumb.is_some()))
        .map(|attachment| attachment.thumb.as_ref().map(|thumb| thumb.id.clone()).unwrap_or_else(|| attachment.file.id.clone()))
        .unwrap_or_default()
}

fn top_reaction(message: &Message) -> String {
    message
        .reactions
        .iter()
        .max_by_key(|reaction| reaction.count.saturating_add(reaction.burst))
        .filter(|reaction| reaction.count.saturating_add(reaction.burst) > 0)
        .map(|reaction| reaction.emoji.clone())
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct ThreadSearch {
    #[serde(default)]
    threads: Vec<model::Channel>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    total_results: Option<u32>,
    #[serde(default)]
    first_messages: Vec<Posted>,
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

    /// A forum's posting rules, from the channel note READY kept. Nothing is asked of Discord.
    pub(super) fn forum_info(&self, channel: &str) -> Result<protocol::ForumInfo, Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
        if known.kind != ConversationKind::Forum {
            return Err(refused("This is not a forum."));
        }
        let guild = self.guild_of(&known).ok_or_else(|| refused("This forum is not here."))?;
        let note = self.ctx.note(&format!("kept:{guild}"));
        let kept: Vec<model::Kept> = note.and_then(|kept| serde_json::from_str(&kept).ok()).unwrap_or_default();
        let kept = kept.into_iter().find(|kept| kept.id == channel).ok_or_else(|| refused("This forum is not here."))?;
        let tags = kept.available_tags.iter().map(|tag| {
            let emoji = tag
                .emoji_name
                .clone()
                .filter(|name| !name.is_empty())
                .or_else(|| tag.emoji_id.clone().filter(|id| !id.is_empty()))
                .unwrap_or_default();
            protocol::ForumTag { id: tag.id.clone(), name: tag.name.clone(), emoji, moderated: tag.moderated }
        });
        Ok(protocol::ForumInfo {
            forum: known.key,
            guidelines: kept.topic.unwrap_or_default(),
            tags: tags.collect(),
            require_tag: kept.flags & model::REQUIRE_TAG != 0,
            media: kept.kind == model::MEDIA_FORUM,
            default_slowmode: kept.default_thread_rate_limit_per_user,
        })
    }

    /// One page of a forum's posts. Archived posts are in this search; nothing walks every page.
    /// `Ok(None)` is an unfiltered first page Discord did not answer, so the cache can.
    pub(super) fn forum_posts(
        &self,
        channel: &str,
        sort: TopicSort,
        tag: Option<String>,
        query: Option<String>,
        offset: u32,
    ) -> Result<Option<ForumPosts>, Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("You cannot read this channel."))?;
        if known.kind != ConversationKind::Forum {
            return Err(refused("This is not a forum."));
        }
        let tag = tag.unwrap_or_default();
        let query = query.unwrap_or_default();
        let plain = offset == 0 && sort == TopicSort::Activity && tag.trim().is_empty() && query.trim().is_empty();
        let path = forum_search_path(channel, sort, tag.trim(), query.trim(), offset);
        let body = match self.call("GET", &path, None) {
            Ok(body) => body,
            Err(failed) => return forum_search_failed(failed, plain),
        };
        let parsed: ThreadSearch = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                if plain {
                    return Ok(None);
                }
                return Err(refused(err));
            }
        };
        let guild = self.guild_of(&known);
        let wanted = tag.trim();
        let ThreadSearch { threads, has_more, total_results, first_messages } = parsed;
        let mut ids = Vec::new();
        for thread in threads.into_iter().take(FORUM_PAGE as usize) {
            if thread.parent_id.as_deref() != Some(channel) {
                continue;
            }
            if !wanted.is_empty() && !thread.applied_tags.iter().any(|have| have == wanted) {
                continue;
            }
            self.thread_changed(thread.clone(), false).map_err(failure_words)?;
            if self.ctx.known(&thread.id).map_err(refused)?.is_none() {
                continue;
            }
            let (thumbnail, reaction) = match first_messages.iter().find(|posted| posted.channel_id == thread.id) {
                Some(posted) => self.record_starter(&thread, posted, guild.as_deref()),
                None => (String::new(), String::new()),
            };
            let _ = self.ctx.post_card(&thread.id, &PostCard {
                tags: thread.applied_tags.clone(),
                message_count: thread.message_count.unwrap_or(0),
                total_message_sent: thread.total_message_sent.unwrap_or(0),
                thumbnail,
                reaction,
                owner: thread.owner_id.clone().unwrap_or_default(),
            });
            ids.push(thread.id);
        }
        let kept = ids.len() as u32;
        let total = total_results.unwrap_or_else(|| {
            if has_more { offset.saturating_add(kept).saturating_add(1) } else { offset.saturating_add(kept) }
        });
        Ok(Some(ForumPosts { ids, total }))
    }

    /// The starter message, its picture, and its strongest reaction. A failure here keeps the post.
    fn record_starter(&self, thread: &model::Channel, posted: &Posted, guild: Option<&str>) -> (String, String) {
        let (message, files) = self.convert(posted, guild, ConversationKind::Thread);
        let _ = files.register(&self.ctx);
        let _ = self.ctx.history(&thread.id, std::slice::from_ref(&message));
        if !message.text.is_empty() {
            let sent = thread.last_message_id.as_deref().map(model::seconds).filter(|sent| *sent > 0).unwrap_or(message.sent);
            let _ = self.ctx.preview(&thread.id, &PreviewLine {
                id: message.id.clone(),
                text: message.text.clone(),
                sender: message.sender.name.clone(),
                mine: message.outgoing,
                sent,
                expires_at: message.expires_at,
            });
        }
        (post_thumb(&message), top_reaction(&message))
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
    pub(super) fn set_mute(&self, channel: &str, notify: Notify, until: Option<i64>) -> Result<(), Refusal> {
        let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
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

    /// Runs a message search off the command thread, queued one at a time per account.
    pub(super) fn search_queued(
        discord: &Arc<Discord>,
        query: String,
        scope: SearchScope,
        filters: SearchFilters,
        cursor: Option<String>,
        at: Instant,
        reply: Reply<SearchPage>,
    ) {
        let searching = discord.clone();
        let run = move || {
            let _ = reply.send(searching.search_messages(&query, &scope, &filters, cursor.as_deref(), at));
        };
        if let Err(err) = std::thread::Builder::new().name("discord-search".into()).spawn(run) {
            tracing::warn!("chatd: Discord: no thread for a search: {err}");
        }
    }

    /// What the network itself finds for the pane's words and filters, from Discord's own search:
    /// the server's when a Space is the scope, the channel's when one conversation is, or all DMs
    /// (`/users/@me/messages/search`) for account scope. Paced requests queue and wait out the
    /// remaining gap rather than refusing immediately.
    pub(super) fn search_messages(
        &self,
        query: &str,
        scope: &SearchScope,
        filters: &SearchFilters,
        cursor: Option<&str>,
        at: Instant,
    ) -> Result<SearchPage, Refusal> {
        if at.elapsed() >= super::SEARCH_PATIENCE {
            return Err(refused("That took too long. Try again."));
        }
        let capped: String = query.chars().take(SEARCH_MAX_CHARS).collect();
        let query = capped.trim();
        let (base, scope_channel, me) = match scope {
            SearchScope::Space(space) => {
                let state = crate::hub::lock(&self.state);
                if !state.guilds.contains_key(space) {
                    return Err(refused("This server is gone."));
                }
                (format!("/guilds/{space}/messages/search"), None, state.me.clone())
            }
            SearchScope::Account => {
                let me = crate::hub::lock(&self.state).me.clone();
                ("/users/@me/messages/search".to_owned(), None, me)
            }
            SearchScope::Conversation(channel) => {
                let known = self.ctx.known(channel).map_err(refused)?.ok_or_else(|| refused("This conversation is gone."))?;
                let me = crate::hub::lock(&self.state).me.clone();
                match self.guild_of(&known) {
                    Some(guild) => (format!("/guilds/{guild}/messages/search"), Some(channel.as_str()), me),
                    None => (format!("/channels/{channel}/messages/search"), None, me),
                }
            }
        };
        let (path, offset) = build_search_path(&base, scope_channel, query, filters, cursor, &me);
        let cache_key = path.to_lowercase();
        {
            let search = crate::hub::lock(&self.message_search);
            if let Some((_, _, page)) = search.remembered.iter().find(|(s, k, _)| s == scope && *k == cache_key) {
                return Ok(page.clone());
            }
        }
        // One search in flight per account, waiting out SEARCH_GAP between requests rather than
        // refusing with an error.
        let _turn = crate::hub::lock(&self.search_turn);
        if at.elapsed() >= super::SEARCH_PATIENCE {
            return Err(refused("That took too long. Try again."));
        }
        let wait = {
            let search = crate::hub::lock(&self.message_search);
            if let Some((_, _, page)) = search.remembered.iter().find(|(s, k, _)| s == scope && *k == cache_key) {
                return Ok(page.clone());
            }
            search
                .last
                .map_or(Duration::ZERO, |last| super::members::SEARCH_GAP.saturating_sub(last.elapsed()))
        };
        if !wait.is_zero() {
            if at.elapsed() + wait >= super::SEARCH_PATIENCE {
                return Err(refused("That took too long. Try again."));
            }
            if self.ctx.pause(wait) {
                return Err(refused("Stopped."));
            }
        }
        let outcome = self.run_message_search(&path, query, scope, offset);
        let mut search = crate::hub::lock(&self.message_search);
        search.last = Some(Instant::now());
        if let Ok(page) = &outcome {
            if search.remembered.len() >= REMEMBERED_SEARCHES {
                search.remembered.pop_front();
            }
            search.remembered.push_back((scope.clone(), cache_key, page.clone()));
        }
        outcome
    }

    fn run_message_search(&self, path: &str, query: &str, scope: &SearchScope, offset: u32) -> Result<SearchPage, Refusal> {
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
        let raw_count = hits.len();
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
        let next_offset = offset.saturating_add(SEARCH_PAGE);
        let cursor = found
            .total_results
            .and_then(|total| (next_offset < total && raw_count > 0).then(|| next_offset.to_string()));
        Ok(SearchPage {
            hits: found_hits,
            cursor,
            total: found.total_results,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key, Network};

    fn key(id: &str) -> Key {
        Key::new(&AccountKey { network: Network::Discord, account: "1000".into() }, id)
    }

    #[test]
    fn discord_search_path_maps_all_filters_dates_sort_and_offset() {
        let filters = SearchFilters {
            from: Some(key("2001")),
            mentions: Some(key("2002")),
            in_channel: Some(key("4101")),
            has: Some(SearchHas::Videos),
            before: Some(1_767_225_600),
            after: Some(1_735_689_600),
            mentions_me: false,
            pinned: Some(true),
            author_type: Some("bot".into()),
            sort: SearchSort::Oldest,
            offset: 25,
        };
        let (path, offset) = build_search_path(
            "/guilds/4000/messages/search",
            None,
            "deploy",
            &filters,
            None,
            "1000",
        );
        assert_eq!(offset, 25);
        assert!(path.starts_with("/guilds/4000/messages/search?"), "{path}");
        assert!(path.contains("channel_id=4101"), "{path}");
        assert!(path.contains("content=deploy"), "{path}");
        assert!(path.contains("author_id=2001"), "{path}");
        assert!(path.contains("mentions=2002"), "{path}");
        assert!(path.contains("has=video"), "{path}");
        assert!(path.contains(&format!("min_id={}", model::snowflake_from_unix(1_735_689_600))), "{path}");
        assert!(path.contains(&format!("max_id={}", model::snowflake_from_unix(1_767_225_600))), "{path}");
        assert!(path.contains("pinned=true"), "{path}");
        assert!(path.contains("author_type=bot"), "{path}");
        assert!(path.contains("sort_by=timestamp&sort_order=asc"), "{path}");
        assert!(path.contains("offset=25"), "{path}");

        // Cursor overrides filters.offset, mentions_me maps to own user id, and Relevant maps to relevance.
        let rel = SearchFilters {
            mentions_me: true,
            sort: SearchSort::Relevant,
            has: Some(SearchHas::Polls),
            author_type: Some("invalid".into()),
            ..SearchFilters::default()
        };
        let (path2, offset2) = build_search_path(
            "/users/@me/messages/search",
            None,
            "",
            &rel,
            Some("50"),
            "1000",
        );
        assert_eq!(offset2, 50);
        assert!(path2.starts_with("/users/@me/messages/search?"), "{path2}");
        assert!(!path2.contains("content="), "{path2}");
        assert!(path2.contains("mentions=1000"), "{path2}");
        assert!(path2.contains("has=poll"), "{path2}");
        assert!(!path2.contains("author_type="), "unknown author_type must be ignored: {path2}");
        assert!(path2.contains("sort_by=relevance&sort_order=desc"), "{path2}");
        assert!(path2.contains("offset=50"), "{path2}");
    }

    #[test]
    fn snowflake_from_unix_round_trips_seconds() {
        let ts = 1_735_689_600;
        let sf = model::snowflake_from_unix(ts);
        assert_eq!(model::seconds(&sf.to_string()), ts);
        assert_eq!(model::snowflake_from_unix(0), 0);
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
