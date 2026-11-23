//! Telegram Stories (#540): active story rings, peer story viewing, reactions, private story
//! replies (`inputReplyToStory`), live `updateStory`/`updateReadStories` handling, and hydration
//! of forwarded/shared stories (`MessageMedia::Story`) in chat.
//!
//! Privacy rule (adapter header): `stories.readStories` is NEVER called during prefetch,
//! sign-on, `getAllStories`, `getPeerStories`, or `getStoriesByID`. It is sent ONLY from
//! [`read_story`], which runs when the user explicitly opens or advances to a story.

use super::content::{self, Mapped, Want};
use super::peers::Who;
use super::wire::Fault;
use super::{live, random_id, remember_random, Account};
use crate::hub::now;
use grammers_tl_types as tl;
use protocol::{PeerStoryRing, SharedStory, Story, StoryMediaKind};
use std::collections::{HashMap, HashSet};

/// Per-connection story state kept inside `Account::live`.
#[derive(Default)]
pub(super) struct State {
    /// Active stories grouped by author peer.
    by_peer: HashMap<Who, PeerBucket>,
    /// Cached fetched stories for forwarded/shared story hydration (`(peer, story_id)` -> item).
    /// `None` means the story was deleted or expired on Telegram.
    shared_cache: HashMap<(Who, i32), Option<CachedStory>>,
    /// Pagination/sync state token from `stories.getAllStories`.
    all_state: Option<String>,
}

#[derive(Default, Clone)]
struct PeerBucket {
    max_read_id: i32,
    stories: Vec<Story>,
    /// Story IDs reported as `StoryItemSkipped` whose full media has not yet been fetched.
    skipped: HashSet<i32>,
    /// Whether any skipped story is marked close-friends.
    skipped_close_friends: HashSet<i32>,
}

#[derive(Clone)]
struct CachedStory {
    author_name: String,
    caption: String,
    caption_spans: Vec<protocol::Span>,
    media_kind: StoryMediaKind,
    attachment: Option<protocol::Attachment>,
    media: Option<protocol::MediaFile>,
    thumb: Option<protocol::MediaFile>,
    wants: Vec<Want>,
}

fn reaction_to_tl(emoji: Option<&str>) -> tl::enums::Reaction {
    match emoji.map(str::trim).filter(|s| !s.is_empty()) {
        None => tl::enums::Reaction::Empty,
        Some(text) => {
            if let Some(id) = text.strip_prefix('e').and_then(|s| s.parse::<i64>().ok()) {
                tl::types::ReactionCustomEmoji { document_id: id }.into()
            } else {
                tl::types::ReactionEmoji { emoticon: text.to_owned() }.into()
            }
        }
    }
}

fn reaction_from_tl(reaction: Option<&tl::enums::Reaction>) -> Option<String> {
    match reaction? {
        tl::enums::Reaction::Emoji(e) => Some(e.emoticon.clone()),
        tl::enums::Reaction::CustomEmoji(c) => Some(format!("e{}", c.document_id)),
        tl::enums::Reaction::Paid => Some("⭐".to_owned()),
        tl::enums::Reaction::Empty => None,
    }
}

fn map_story_item(
    account: &Account,
    who: Who,
    item: &tl::types::StoryItem,
    max_read_id: i32,
    wants: &mut Vec<Want>,
) -> Story {
    let (author_name, rules) = {
        let dir = account.dir();
        (dir.name(who), dir.download)
    };
    let (media_kind, _attachment, media, thumb, duration_ms) =
        content::story_media(&item.media, wants, who, false, rules);
    let caption = item.caption.clone().unwrap_or_default();
    let caption_spans = content::spans(&caption, item.entities.as_deref().unwrap_or(&[]));
    let (views_count, reactions_count) = match &item.views {
        Some(tl::enums::StoryViews::Views(v)) => (
            Some(v.views_count.max(0) as u32),
            v.reactions_count.map(|c| c.max(0) as u32),
        ),
        None => (None, None),
    };
    let own = item.out || who == Who::User(account.me);
    let seen = own || item.id <= max_read_id;
    Story {
        id: item.id.to_string(),
        peer: account.ctx.key(&who.text()),
        author_name,
        posted_at: i64::from(item.date),
        expires_at: i64::from(item.expire_date),
        seen,
        close_friends: item.close_friends,
        pinned: item.pinned,
        own,
        noforwards: item.noforwards,
        media_kind,
        media,
        thumb,
        duration_ms,
        caption,
        caption_spans,
        my_reaction: reaction_from_tl(item.sent_reaction.as_ref()),
        views_count,
        reactions_count,
    }
}

fn ingest_peer_stories(account: &Account, peer_stories: &tl::types::PeerStories, queue_media: bool) -> Who {
    let who = Who::of(&peer_stories.peer);
    let now_s = now();
    let max_read_id = peer_stories.max_read_id.unwrap_or(0);
    let mut wants = Vec::new();
    let mut stories = Vec::new();
    let mut skipped = HashSet::new();
    let mut skipped_cf = HashSet::new();

    for raw in &peer_stories.stories {
        match raw {
            tl::enums::StoryItem::Item(item) => {
                if i64::from(item.expire_date) > 0 && i64::from(item.expire_date) <= now_s && !item.pinned {
                    continue;
                }
                stories.push(map_story_item(account, who, item, max_read_id, &mut wants));
            }
            tl::enums::StoryItem::Skipped(skip) => {
                if i64::from(skip.expire_date) > 0 && i64::from(skip.expire_date) <= now_s {
                    continue;
                }
                skipped.insert(skip.id);
                if skip.close_friends {
                    skipped_cf.insert(skip.id);
                }
                let own = who == Who::User(account.me);
                stories.push(Story {
                    id: skip.id.to_string(),
                    peer: account.ctx.key(&who.text()),
                    author_name: account.dir().name(who),
                    posted_at: i64::from(skip.date),
                    expires_at: i64::from(skip.expire_date),
                    seen: own || skip.id <= max_read_id,
                    close_friends: skip.close_friends,
                    pinned: false,
                    own,
                    noforwards: false,
                    media_kind: StoryMediaKind::Unsupported,
                    media: None,
                    thumb: None,
                    duration_ms: None,
                    caption: String::new(),
                    caption_spans: Vec::new(),
                    my_reaction: None,
                    views_count: None,
                    reactions_count: None,
                });
            }
            tl::enums::StoryItem::Deleted(_) => {}
        }
    }
    stories.sort_by_key(|s| s.id.parse::<i32>().unwrap_or(0));

    if queue_media && !wants.is_empty() {
        account.files.want(wants);
    }

    let mut live = account.live();
    let entry = live.stories.by_peer.entry(who).or_default();
    entry.max_read_id = entry.max_read_id.max(max_read_id);
    // Recompute seen flags using the monotonic max_read_id.
    let read_floor = entry.max_read_id;
    for story in &mut stories {
        let sid = story.id.parse::<i32>().unwrap_or(0);
        if story.own || sid <= read_floor {
            story.seen = true;
        }
    }
    entry.stories = stories;
    entry.skipped = skipped;
    entry.skipped_close_friends = skipped_cf;
    who
}

fn rings_snapshot(account: &Account) -> Vec<PeerStoryRing> {
    let now_s = now();
    let raw: Vec<(Who, String, u32, u32, i32, i32, bool)> = {
        let live = account.live();
        let mut out = Vec::new();
        for (&who, bucket) in &live.stories.by_peer {
            let active: Vec<&Story> = bucket
                .stories
                .iter()
                .filter(|s| s.expires_at == 0 || s.expires_at > now_s || s.pinned)
                .collect();
            if active.is_empty() {
                continue;
            }
            let max_id = active
                .iter()
                .filter_map(|s| s.id.parse::<i32>().ok())
                .max()
                .unwrap_or(0);
            let unseen = active
                .iter()
                .filter(|s| {
                    let sid = s.id.parse::<i32>().unwrap_or(0);
                    !s.own && !s.seen && sid > bucket.max_read_id
                })
                .count() as u32;
            let close_friends = active.iter().any(|s| {
                let sid = s.id.parse::<i32>().unwrap_or(0);
                s.close_friends && !s.seen && sid > bucket.max_read_id
            });
            let author_name = active
                .first()
                .map(|s| s.author_name.clone())
                .unwrap_or_default();
            out.push((who, author_name, active.len() as u32, unseen, max_id, bucket.max_read_id, close_friends));
        }
        out
    };
    let mut rings: Vec<PeerStoryRing> = raw
        .into_iter()
        .map(|(who, author_name, total, unseen, max_id, max_read_id, close_friends)| {
            let author_name = if author_name.is_empty() {
                account.dir().name(who)
            } else {
                author_name
            };
            PeerStoryRing {
                peer: account.ctx.key(&who.text()),
                author_name,
                total,
                unseen,
                max_id,
                max_read_id,
                close_friends,
            }
        })
        .collect();
    // Unseen rings first, then by most recent story id descending, then by peer id.
    rings.sort_by(|a, b| {
        b.has_unseen()
            .cmp(&a.has_unseen())
            .then_with(|| b.max_id.cmp(&a.max_id))
            .then_with(|| a.peer.id.cmp(&b.peer.id))
    });
    rings
}

fn publish_rings(account: &Account) {
    let rings = rings_snapshot(account);
    account.ctx.stories(rings);
}

/// Loads active story rings across all contacts via `stories.getAllStories`.
/// Never sends `stories.readStories`.
pub(super) fn load_all(account: &Account) -> Result<Vec<PeerStoryRing>, Fault> {
    let prev_state = account.live().stories.all_state.clone();
    let answer = account.ask(&tl::functions::stories::GetAllStories {
        next: false,
        hidden: false,
        state: prev_state,
    })?;
    match answer {
        tl::enums::stories::AllStories::Stories(all) => {
            account.learn(&all.users, &all.chats);
            account.live().stories.all_state = (!all.state.is_empty()).then_some(all.state);
            for peer_stories in &all.peer_stories {
                let tl::enums::PeerStories::Stories(ps) = peer_stories;
                ingest_peer_stories(account, ps, false);
            }
        }
        tl::enums::stories::AllStories::NotModified(nm) => {
            if !nm.state.is_empty() {
                account.live().stories.all_state = Some(nm.state);
            }
        }
    }
    let rings = rings_snapshot(account);
    account.ctx.stories(rings.clone());
    Ok(rings)
}

/// Fetches stories: all rings when `peer_id` is `None`, or a specific peer's stories via
/// `stories.getPeerStories` (plus `stories.getStoriesByID` for any skipped stubs).
/// Never sends `stories.readStories`.
pub(super) fn list(
    account: &Account,
    peer_id: Option<&str>,
) -> Result<(Vec<PeerStoryRing>, Vec<Story>), Fault> {
    let Some(peer_id) = peer_id.filter(|s| !s.is_empty()) else {
        let rings = load_all(account)?;
        return Ok((rings, Vec::new()));
    };
    let (who, _) = Who::parse(peer_id)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let peer = account.input(who)?;
    let tl::enums::stories::PeerStories::Stories(resp) =
        account.ask(&tl::functions::stories::GetPeerStories { peer: peer.clone() })?;
    account.learn(&resp.users, &resp.chats);
    let tl::enums::PeerStories::Stories(ps) = &resp.stories;
    ingest_peer_stories(account, ps, true);

    // If any stories in this peer's list were `StoryItemSkipped`, fetch their full media via
    // `stories.getStoriesByID` — still never calling `stories.readStories`.
    let skipped_ids: Vec<i32> = account
        .live()
        .stories
        .by_peer
        .get(&who)
        .map(|b| b.skipped.iter().copied().collect())
        .unwrap_or_default();
    if !skipped_ids.is_empty() {
        if let Ok(tl::enums::stories::Stories::Stories(full)) =
            account.ask(&tl::functions::stories::GetStoriesById { peer, id: skipped_ids })
        {
            account.learn(&full.users, &full.chats);
            let max_read_id = account
                .live()
                .stories
                .by_peer
                .get(&who)
                .map(|b| b.max_read_id)
                .unwrap_or(0);
            let mut wants = Vec::new();
            let mut hydrated = Vec::new();
            for raw in &full.stories {
                if let tl::enums::StoryItem::Item(item) = raw {
                    hydrated.push(map_story_item(account, who, item, max_read_id, &mut wants));
                }
            }
            if !wants.is_empty() {
                account.files.want(wants);
            }
            let mut live = account.live();
            if let Some(bucket) = live.stories.by_peer.get_mut(&who) {
                for story in hydrated {
                    let sid = story.id.parse::<i32>().unwrap_or(0);
                    bucket.skipped.remove(&sid);
                    if let Some(existing) = bucket.stories.iter_mut().find(|s| s.id == story.id) {
                        *existing = story;
                    } else {
                        bucket.stories.push(story);
                    }
                }
                bucket.stories.sort_by_key(|s| s.id.parse::<i32>().unwrap_or(0));
            }
        }
    }

    let stories = account
        .live()
        .stories
        .by_peer
        .get(&who)
        .map(|b| b.stories.clone())
        .unwrap_or_default();
    let rings = rings_snapshot(account);
    account.ctx.stories(rings.clone());
    Ok((rings, stories))
}

/// Marks a peer's stories read up to `story_id` (`stories.readStories`).
/// Called ONLY when the user explicitly opens or advances to that story.
pub(super) fn read_story(account: &Account, peer_id: &str, story_id: &str) -> Result<(), Fault> {
    let (who, _) = Who::parse(peer_id)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let max_id: i32 = story_id
        .parse()
        .map_err(|_| Fault::Rpc { code: 400, name: "STORY_ID_INVALID".to_owned(), value: None })?;
    let peer = account.input(who)?;
    account.here();
    let _read = account.ask(&tl::functions::stories::ReadStories { peer, max_id })?;
    {
        let mut live = account.live();
        let bucket = live.stories.by_peer.entry(who).or_default();
        bucket.max_read_id = bucket.max_read_id.max(max_id);
        let floor = bucket.max_read_id;
        for story in &mut bucket.stories {
            if story.id.parse::<i32>().unwrap_or(0) <= floor {
                story.seen = true;
            }
        }
    }
    publish_rings(account);
    Ok(())
}

/// Sends or clears a reaction on a story (`stories.sendReaction`).
pub(super) fn react_story(
    account: &Account,
    peer_id: &str,
    story_id: &str,
    emoji: Option<&str>,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(peer_id)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let sid: i32 = story_id
        .parse()
        .map_err(|_| Fault::Rpc { code: 400, name: "STORY_ID_INVALID".to_owned(), value: None })?;
    let peer = account.input(who)?;
    account.here();
    let reaction = reaction_to_tl(emoji);
    let answer = account.ask(&tl::functions::stories::SendReaction {
        add_to_recent: emoji.is_some(),
        peer,
        story_id: sid,
        reaction,
    })?;
    live::answered(account, &answer);
    {
        let mut live = account.live();
        if let Some(bucket) = live.stories.by_peer.get_mut(&who) {
            if let Some(story) = bucket.stories.iter_mut().find(|s| s.id == story_id) {
                story.my_reaction = emoji
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
        }
    }
    Ok(())
}

/// Sends a private reply to a story via `messages.sendMessage` with `inputReplyToStory`.
/// Preserves `ReplyTo { story: true, .. }` on the delivered DM message.
pub(super) fn reply_story(
    account: &Account,
    peer_id: &str,
    story_id: &str,
    client_id: &str,
    text: &str,
) -> Result<(String, protocol::Message), Fault> {
    let (who, _) = Who::parse(peer_id)
        .ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let sid: i32 = story_id
        .parse()
        .map_err(|_| Fault::Rpc { code: 400, name: "STORY_ID_INVALID".to_owned(), value: None })?;
    let peer = account.input(who)?;
    let conversation = who.text();
    account.here();
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    let (plain, entities) = super::prepared(account, &conversation, text);
    let reply_to = Some(
        tl::types::InputReplyToStory {
            peer: peer.clone(),
            story_id: sid,
        }
        .into(),
    );
    let request = tl::functions::messages::SendMessage {
        no_webpage: false,
        silent: false,
        background: false,
        clear_draft: false,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer,
        reply_to,
        message: plain.clone(),
        random_id: random,
        reply_markup: None,
        entities,
        schedule_date: None,
        schedule_repeat_period: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        allow_paid_stars: None,
        suggested_post: None,
        rich_message: None,
    };
    let answer = account.ask(&request)?;
    let mut message = live::sent(account, &conversation, who, &plain, None, &answer)
        .ok_or_else(|| Fault::Net("Telegram accepted the story reply but did not name it".to_owned()))?;
    // Ensure story reply context is preserved even on `UpdateShortSentMessage`.
    if message.reply_to.as_ref().is_none_or(|r| !r.story) {
        let sender = account.dir().name(who);
        message.reply_to = Some(protocol::ReplyTo {
            id: sid.to_string(),
            sender,
            snippet: "Story".to_owned(),
            person: Some(account.ctx.key(&conversation)),
            deleted: false,
            thumb: None,
            story: true,
        });
    }
    let _ = account.ctx.message(&conversation, &message);
    Ok((conversation, message))
}

/// Applies a live `updateStory` push from Telegram.
pub(super) fn apply_story(account: &Account, update: &tl::types::UpdateStory) {
    let who = Who::of(&update.peer);
    match &update.story {
        tl::enums::StoryItem::Deleted(del) => {
            let mut live = account.live();
            if let Some(bucket) = live.stories.by_peer.get_mut(&who) {
                bucket.stories.retain(|s| s.id != del.id.to_string());
                bucket.skipped.remove(&del.id);
                bucket.skipped_close_friends.remove(&del.id);
            }
            live.stories.shared_cache.insert((who, del.id), None);
        }
        tl::enums::StoryItem::Item(item) => {
            let max_read_id = account
                .live()
                .stories
                .by_peer
                .get(&who)
                .map(|b| b.max_read_id)
                .unwrap_or(0);
            let mut wants = Vec::new();
            let story = map_story_item(account, who, item, max_read_id, &mut wants);
            if !wants.is_empty() {
                account.files.want(wants);
            }
            let mut live = account.live();
            let bucket = live.stories.by_peer.entry(who).or_default();
            bucket.skipped.remove(&item.id);
            if let Some(existing) = bucket.stories.iter_mut().find(|s| s.id == story.id) {
                *existing = story;
            } else {
                bucket.stories.push(story);
                bucket.stories.sort_by_key(|s| s.id.parse::<i32>().unwrap_or(0));
            }
        }
        tl::enums::StoryItem::Skipped(skip) => {
            let own = who == Who::User(account.me);
            let author_name = account.dir().name(who);
            let mut live = account.live();
            let bucket = live.stories.by_peer.entry(who).or_default();
            let seen = own || skip.id <= bucket.max_read_id;
            bucket.skipped.insert(skip.id);
            if skip.close_friends {
                bucket.skipped_close_friends.insert(skip.id);
            }
            let sid = skip.id.to_string();
            if !bucket.stories.iter().any(|s| s.id == sid) {
                bucket.stories.push(Story {
                    id: sid,
                    peer: account.ctx.key(&who.text()),
                    author_name,
                    posted_at: i64::from(skip.date),
                    expires_at: i64::from(skip.expire_date),
                    seen,
                    close_friends: skip.close_friends,
                    pinned: false,
                    own,
                    noforwards: false,
                    media_kind: StoryMediaKind::Unsupported,
                    media: None,
                    thumb: None,
                    duration_ms: None,
                    caption: String::new(),
                    caption_spans: Vec::new(),
                    my_reaction: None,
                    views_count: None,
                    reactions_count: None,
                });
                bucket.stories.sort_by_key(|s| s.id.parse::<i32>().unwrap_or(0));
            }
        }
    }
    publish_rings(account);
}

/// Applies a live `updateReadStories` push from Telegram.
pub(super) fn apply_read(account: &Account, update: &tl::types::UpdateReadStories) {
    let who = Who::of(&update.peer);
    {
        let mut live = account.live();
        let bucket = live.stories.by_peer.entry(who).or_default();
        bucket.max_read_id = bucket.max_read_id.max(update.max_id);
        let floor = bucket.max_read_id;
        for story in &mut bucket.stories {
            if story.id.parse::<i32>().unwrap_or(0) <= floor {
                story.seen = true;
            }
        }
    }
    publish_rings(account);
}

/// Hydrates a forwarded/shared story (`MessageMedia::Story`) whose `StoryItem` was not embedded
/// inline, using `stories.getStoriesByID`. Never sends `stories.readStories`.
pub(super) fn hydrate_shared(account: &Account, story_peer: Who, story_id: i32, mapped: &mut Mapped) {
    let cached = {
        let live = account.live();
        live.stories.shared_cache.get(&(story_peer, story_id)).cloned()
    };

    let resolved = match cached {
        Some(entry) => entry,
        None => {
            let fetched = account.input(story_peer).ok().and_then(|peer| {
                let tl::enums::stories::Stories::Stories(resp) = account
                    .ask(&tl::functions::stories::GetStoriesById {
                        peer,
                        id: vec![story_id],
                    })
                    .ok()?;
                account.learn(&resp.users, &resp.chats);
                let rules = account.dir().download;
                resp.stories.into_iter().find_map(|raw| match raw {
                    tl::enums::StoryItem::Item(item) if item.id == story_id => {
                        let mut wants = Vec::new();
                        let (media_kind, attachment, media, thumb, _) =
                            content::story_media(&item.media, &mut wants, story_peer, false, rules);
                        let caption = item.caption.unwrap_or_default();
                        let caption_spans =
                            content::spans(&caption, item.entities.as_deref().unwrap_or(&[]));
                        Some(CachedStory {
                            author_name: account.dir().name(story_peer),
                            caption,
                            caption_spans,
                            media_kind,
                            attachment,
                            media,
                            thumb,
                            wants,
                        })
                    }
                    _ => None,
                })
            });
            account
                .live()
                .stories
                .shared_cache
                .insert((story_peer, story_id), fetched.clone());
            fetched
        }
    };

    let peer_key = account.ctx.key(&story_peer.text());
    let via_mention = mapped
        .message
        .story
        .as_ref()
        .is_some_and(|s| s.via_mention);

    match resolved {
        Some(story) => {
            if let Some(att) = story.attachment {
                if !mapped
                    .message
                    .attachments
                    .iter()
                    .any(|existing| existing.file.id == att.file.id)
                {
                    mapped.message.attachments.push(att);
                }
            }
            mapped.wants.extend(story.wants);
            mapped.message.story = Some(Box::new(SharedStory {
                peer: peer_key,
                story_id: story_id.to_string(),
                author_name: story.author_name,
                caption: story.caption,
                caption_spans: story.caption_spans,
                media_kind: story.media_kind,
                media: story.media,
                thumb: story.thumb,
                expired: false,
                via_mention,
            }));
        }
        None => {
            let author_name = account.dir().name(story_peer);
            mapped.message.story = Some(Box::new(SharedStory {
                peer: peer_key,
                story_id: story_id.to_string(),
                author_name,
                caption: String::new(),
                caption_spans: Vec::new(),
                media_kind: StoryMediaKind::Unsupported,
                media: None,
                thumb: None,
                expired: true,
                via_mention,
            }));
        }
    }
}
