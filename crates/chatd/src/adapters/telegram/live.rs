//! What Telegram says unasked, applied to the store and the hub as it arrives.

use super::content::{self, Mapped};
use super::peers::{thread_id, Who};
use super::sync::{self, conversation_of, ensure_row, finish};
use super::{filing, presence, Account, Options, TYPING_SHOWN};
use grammers_tl_types as tl;
use protocol::Message;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tl::enums::Update;

/// The conversation a peer's update is about; in a forum, the thread of the topic it names,
/// and in a discussion group the comments thread when the update names the echo it answers.
fn place(account: &Account, who: Who, topic: Option<i32>) -> String {
    match who {
        Who::Channel(_) if account.dir().get(who).is_some_and(|known| known.forum) => thread_id(who, topic.unwrap_or(1)),
        Who::Channel(_) => sync::echo_thread(account, who, topic).unwrap_or_else(|| who.text()),
        who => who.text(),
    }
}

/// Maps a message and makes sure its conversation is recorded. `None` for one that cannot be
/// placed (an empty message, a chat Telegram never described).
fn placed(account: &Account, raw: &tl::enums::Message) -> Option<(String, Mapped)> {
    let mut mapped = content::map(&account.dir(), raw)?;
    let conversation = conversation_of(account, &mapped);
    // A topic's first message has the forum's row made for it (`ensure_row`); a comment of
    // the discussion group is mapped under the row the post already wrote, and the group's
    // own row is not what this message calls for.
    let ensure = if conversation == mapped.peer.text() {
        Some((mapped.peer, None))
    } else if account.dir().get(mapped.peer).is_some_and(|known| known.forum) {
        Some((mapped.peer, Some(mapped.topic.unwrap_or(1))))
    } else {
        None
    };
    if !match ensure {
        Some((who, topic)) => ensure_row(account, who, topic),
        None => matches!(account.ctx.known(&conversation), Ok(Some(_))),
    } {
        return None;
    }
    finish(account, &conversation, &mut mapped, &HashMap::new());
    Some((conversation, mapped))
}

/// Albums held while connected. Each is at most ten items, and the list itself is capped.
const ALBUMS: usize = 64;
const ALBUM_PARTS: usize = 10;

pub(super) struct HeldAlbum {
    pub group: i64,
    pub peer: Who,
    pub conversation: String,
    pub written: i32,
    pub parts: Vec<Mapped>,
}

pub(super) struct Folded {
    pub message: Message,
    pub wants: Vec<content::Want>,
    pub peer: Who,
    pub members: Vec<i32>,
    pub replaced: Option<i32>,
}

fn upsert_part(parts: &mut Vec<Mapped>, mut part: Mapped) {
    if let Some(existing) = parts.iter_mut().find(|one| one.id == part.id) {
        if part.message.client_id.is_none() {
            part.message.client_id = existing.message.client_id.clone();
        }
        *existing = part;
        return;
    }
    if parts.len() >= ALBUM_PARTS {
        return;
    }
    parts.push(part);
}

fn album_mut<'a>(live: &'a mut super::Live, peer: Who, group: i64, conversation: &str) -> &'a mut HeldAlbum {
    if !live.albums.iter().any(|album| album.peer == peer && album.group == group) && live.albums.len() >= ALBUMS {
        live.albums.pop_front();
    }
    if let Some(at) = live.albums.iter().position(|album| album.peer == peer && album.group == group) {
        live.albums[at].conversation = conversation.to_owned();
        return &mut live.albums[at];
    }
    live.albums.push_back(HeldAlbum { group, peer, conversation: conversation.to_owned(), written: 0, parts: Vec::new() });
    live.albums.back_mut().expect("the album was just stored")
}

/// Folds one item into its album and returns the row to write. `None` when the item is not grouped.
pub(super) fn fold(account: &Account, conversation: &str, part: Mapped) -> Option<Folded> {
    let group = part.group?;
    let peer = part.peer;
    let mut live = account.live();
    let album = album_mut(&mut live, peer, group, conversation);
    let previous = album.written;
    upsert_part(&mut album.parts, part);
    let merged = content::merge_album(&album.parts);
    album.written = merged.id;
    let replaced = (previous != 0 && previous != merged.id).then_some(previous);
    Some(Folded { message: merged.message, wants: merged.wants, peer, members: merged.members, replaced })
}

/// Remembers the albums on a history page, so a later edit, reaction or deletion finds them.
pub(super) fn remember(account: &Account, conversation: &str, items: &[Mapped]) {
    let mut groups = Vec::new();
    for item in items {
        if let Some(group) = item.group {
            if !groups.contains(&(item.peer, group)) {
                groups.push((item.peer, group));
            }
        }
    }
    if groups.is_empty() {
        return;
    }
    let mut live = account.live();
    for (peer, group) in groups {
        let mut parts: Vec<Mapped> = items.iter().filter(|item| item.peer == peer && item.group == Some(group)).cloned().collect();
        let album = album_mut(&mut live, peer, group, conversation);
        for part in &mut parts {
            if part.message.client_id.is_none() {
                part.message.client_id = album.parts.iter().find(|old| old.id == part.id).and_then(|old| old.message.client_id.clone());
            }
        }
        let previous = album.written;
        album.parts = parts;
        album.written = album.parts.iter().map(|part| part.id).min().unwrap_or(previous);
    }
}

/// The row id an album item was written under.
pub(super) fn written_id(account: &Account, message: i32) -> Option<i32> {
    account.live().albums.iter().find(|album| album.parts.iter().any(|part| part.id == message) || album.written == message).map(|album| album.written)
}

fn group_of(account: &Account, conversation: &str, message: i32) -> Option<i64> {
    account.live().albums.iter().find(|album| album.conversation == conversation && album.parts.iter().any(|part| part.id == message)).map(|album| album.group)
}

fn keep_client_id(account: &Account, conversation: &str, message: &mut Message) {
    if message.client_id.is_some() {
        return;
    }
    if let Ok(Some(stored)) = account.ctx.stored_message(conversation, &message.id) {
        message.client_id = stored.client_id;
    }
}

/// Drops one item from its album and rewrites the grid. `false` when `message` is not in one.
pub(super) fn drop_member(account: &Account, conversation: &str, message: i32) -> bool {
    let outcome = {
        let mut live = account.live();
        let Some(at) = live.albums.iter().position(|album| {
            album.conversation == conversation && (album.parts.iter().any(|part| part.id == message) || album.written == message)
        }) else {
            return false;
        };
        let album = &mut live.albums[at];
        let previous = album.written;
        album.parts.retain(|part| part.id != message);
        if album.parts.is_empty() {
            let written = album.written;
            live.albums.remove(at);
            drop(live);
            let _ = account.ctx.deleted(conversation, &written.to_string());
            if message != written {
                let _ = account.ctx.deleted(conversation, &message.to_string());
            }
            return true;
        } else {
            let merged = content::merge_album(&album.parts);
            let written = if album.parts.iter().any(|part| part.id == previous) { previous } else { merged.id };
            album.written = written;
            let mut row = merged.message;
            row.id = written.to_string();
            Some((written, row, previous))
        }
    };
    if let Some((written, mut row, previous)) = outcome {
        keep_client_id(account, conversation, &mut row);
        let _ = account.ctx.history(conversation, std::slice::from_ref(&row));
        if previous != written {
            let _ = account.ctx.deleted(conversation, &previous.to_string());
        }
        if message != written {
            let _ = account.ctx.deleted(conversation, &message.to_string());
        }
    }
    true
}

const PENDING_CHATS: usize = 100;
const PENDING_MESSAGES: usize = 32;
const CHAT_RETRY: Duration = Duration::from_secs(3);
const CHAT_RETRY_MAX: Duration = Duration::from_secs(120);

struct PendingChat {
    peer: Who,
    messages: VecDeque<Box<tl::enums::Message>>,
    next: Instant,
    retry: Duration,
}

/// Live messages waiting for their chat's dialog and notification settings. These carry over
/// a reconnect, where Telegram may resume the update stream after the messages it already sent.
#[derive(Default)]
pub(super) struct PendingChats {
    chats: VecDeque<PendingChat>,
    flood_until: Option<Instant>,
}

pub(super) fn pending_deadline(account: &Account) -> Option<Instant> {
    let pending = account.pending();
    let next = pending.chats.iter().map(|chat| chat.next).min()?;
    Some(next.max(pending.flood_until.unwrap_or(next)))
}

pub(super) fn flood_until(account: &Account) -> Option<Instant> {
    account.pending().flood_until.filter(|until| Instant::now() < *until)
}

pub(super) fn pause_for_flood(account: &Account, fault: &super::Fault) {
    if let Some(until) = fault.flood().and_then(|seconds| Instant::now().checked_add(Duration::from_secs(seconds))) {
        let mut pending = account.pending();
        pending.flood_until = Some(pending.flood_until.unwrap_or(until).max(until));
    }
}

pub(super) fn retry_pending(account: &Account) {
    let now = Instant::now();
    let peer = {
        let pending = account.pending();
        if pending.flood_until.is_some_and(|until| now < until) {
            return;
        }
        pending.chats.iter().find(|chat| chat.next <= now).map(|chat| chat.peer)
    };
    let Some(peer) = peer else { return };
    let result = sync::recover_live_chat(account, peer);
    if let Err(fault) = &result {
        pause_for_flood(account, fault);
    }
    let mut pending = account.pending();
    let Some(at) = pending.chats.iter().position(|chat| chat.peer == peer) else { return };
    match result {
        Ok(Some(through)) => {
            let Some(chat) = pending.chats.remove(at) else { return };
            drop(pending);
            {
                let mut live = account.live();
                live.recovered_through.retain(|(who, _)| *who != peer);
                if live.recovered_through.len() >= PENDING_CHATS {
                    live.recovered_through.pop_front();
                }
                live.recovered_through.push_back((peer, through));
            }
            for raw in chat.messages {
                // Telegram's dialog already counted these. An update that arrived after the
                // snapshot still counts live. A read on another device also comes with the dialog.
                deliver_message(account, &raw, false, raw.id() <= through);
            }
        }
        _ if sync::membership(account, peer) == Some(false) => {
            pending.chats.remove(at);
        }
        _ => {
            if let Some(mut chat) = pending.chats.remove(at) {
                chat.next = Instant::now() + chat.retry;
                chat.retry = (chat.retry * 2).min(CHAT_RETRY_MAX);
                pending.chats.push_back(chat);
            }
        }
    }
}

/// Queue an arriving update until we know the chat belongs here and how to notify for it.
fn message(account: &Account, raw: &tl::enums::Message, edit: bool) {
    let Some(peer) = sync::message_peer(raw) else { return };
    let membership = sync::membership(account, peer);
    if !matches!(peer, Who::User(_)) && membership != Some(false) {
        let needs_dialog = membership.is_none() || !matches!(account.ctx.known(&peer.text()), Ok(Some(_)));
        let mut pending = account.pending();
        if let Some(chat) = pending.chats.iter_mut().find(|chat| chat.peer == peer) {
            if let Some(held) = chat.messages.iter_mut().find(|held| held.id() == raw.id()) {
                **held = raw.clone();
            } else if !edit {
                if chat.messages.len() >= PENDING_MESSAGES {
                    chat.messages.pop_front();
                }
                chat.messages.push_back(Box::new(raw.clone()));
            }
            return;
        }
        if needs_dialog {
            if !edit {
                if pending.chats.len() >= PENDING_CHATS {
                    pending.chats.pop_front();
                }
                pending.chats.push_back(PendingChat {
                    peer, messages: VecDeque::from([Box::new(raw.clone())]), next: Instant::now(), retry: CHAT_RETRY,
                });
            }
            return;
        }
    }
    let covered_by_dialog = account.live().recovered_through.iter().any(|(who, through)| *who == peer && raw.id() <= *through);
    deliver_message(account, raw, edit, covered_by_dialog);
}

fn dialog_counts(account: &Account, conversation: &str, peer: Who, covered_by_dialog: bool) -> bool {
    let forum = account.dir().get(peer).is_some_and(|known| known.forum);
    covered_by_dialog && (forum || conversation == peer.text())
}

fn publish(account: &Account, conversation: &str, message: &Message, raw_id: i32, fresh: bool, member: bool, counted: bool) {
    if fresh && member && !counted {
        let _ = account.ctx.message(conversation, message);
    } else if fresh && member && past_read(account, conversation, raw_id) {
        let _ = account.ctx.counted_message(conversation, message);
    } else {
        let _ = account.ctx.history(conversation, std::slice::from_ref(message));
    }
}

fn deliver_message(account: &Account, raw: &tl::enums::Message, edit: bool, covered_by_dialog: bool) {
    let Some((conversation, mut mapped)) = placed(account, raw) else {
        return;
    };
    let watching = account.ctx.watching();
    let open = watching.conversation.as_ref().is_some_and(|open| open.account() == account.ctx.account && open.id == conversation);
    let member = sync::member(account, mapped.peer);
    // Nonmembers can view comments. Only the thread on screen gets its live copy;
    // Telegram sends a separate @replies message for anything that needs their attention.
    if !member && !open {
        return;
    }
    if mapped.group.is_none() {
        mapped.group = group_of(account, &conversation, mapped.id);
    }
    let in_album = mapped.group.is_some();
    // An edit of something never stored is not news: it would count as a new unread message.
    // An album item was stored under the group's id, not its own.
    if edit && !in_album && !matches!(account.ctx.stored_message(&conversation, &mapped.message.id), Ok(Some(_))) {
        return;
    }
    // Our own message coming back: it replaces the one the shell is showing as pending.
    if mapped.message.outgoing {
        let mut live = account.live();
        if let Some(at) = live.sent.iter().position(|(id, _)| *id == mapped.id) {
            mapped.message.client_id = live.sent.remove(at).map(|(_, client_id)| client_id);
        }
    }
    if in_album {
        let Some(folded) = fold(account, &conversation, mapped) else {
            return;
        };
        if let Some(old) = folded.replaced {
            let _ = account.ctx.deleted(&conversation, &old.to_string());
        }
        let mut message = folded.message;
        keep_client_id(account, &conversation, &mut message);
        if edit && message.views.is_none() {
            if let Ok(Some(stored)) = account.ctx.stored_message(&conversation, &message.id) {
                message.views = stored.views;
            }
        }
        let fresh = folded.members.len() == 1 && folded.replaced.is_none() && !edit;
        let counted = dialog_counts(account, &conversation, folded.peer, covered_by_dialog);
        publish(account, &conversation, &message, raw.id(), fresh, member, counted);
        for id in &folded.members {
            account.place(folded.peer, *id, &conversation);
        }
        if open {
            sync::reaction_emoji(account, &conversation, std::slice::from_ref(&message));
            sync::text_emoji(account, &conversation, std::slice::from_ref(&message));
            account.files.want(folded.wants);
        }
        return;
    }
    // An edit says nothing of a channel post's views: the count the page read kept stands.
    if edit && mapped.message.views.is_none() {
        if let Ok(Some(stored)) = account.ctx.stored_message(&conversation, &mapped.message.id) {
            mapped.message.views = stored.views;
        }
    }
    let counted = dialog_counts(account, &conversation, mapped.peer, covered_by_dialog);
    publish(account, &conversation, &mapped.message, raw.id(), true, member, counted);
    // What it shows is fetched only when the user is looking at the conversation; otherwise
    // opening it fetches the page it is on.
    if open {
        sync::reaction_emoji(account, &conversation, std::slice::from_ref(&mapped.message));
        sync::text_emoji(account, &conversation, std::slice::from_ref(&mapped.message));
        account.files.want(mapped.wants);
    }
}

/// The message is newer than the conversation's read marker, or the conversation has none.
fn past_read(account: &Account, conversation: &str, id: i32) -> bool {
    account.ctx.read_up_to(conversation).and_then(|read| read.parse::<i32>().ok()).is_none_or(|read| id > read)
}

fn typing(account: &Account, conversation: String, from: Who, action: &tl::enums::SendMessageAction) {
    if from == Who::User(account.me) {
        return;
    }
    match action {
        tl::enums::SendMessageAction::SendMessageCancelAction => account.ctx.typing_stopped(&conversation, &from.text()),
        // Recording a voice note, choosing a sticker and the rest all read as "is typing".
        _ => account.ctx.typing(&conversation, &from.text(), &account.dir().name(from), TYPING_SHOWN),
    }
}

/// Another device read up to `max_id`, leaving `unread`.
fn read(account: &Account, conversation: &str, max_id: i32, unread: i32) {
    let Ok(Some(row)) = account.ctx.known(conversation) else {
        return;
    };
    let unread = unread.max(0) as u32;
    // Telegram counts mentions apart and does not repeat the count here: with nothing left
    // unread there are none, otherwise what was known stands.
    let mentions = if unread == 0 { 0 } else { row.mentions.min(unread) };
    let _ = account.ctx.counts(conversation, unread, mentions, Some(&max_id.to_string()), row.updated);
    if let Some((who, None)) = Who::parse(conversation) {
        filing::facts_changed(account, who, |facts| facts.read = unread == 0);
    }
}

fn reactions(account: &Account, conversation: &str, message: i32, update: &tl::enums::MessageReactions) {
    let Ok(Some(mut stored)) = account.ctx.stored_message(conversation, &message.to_string()) else {
        return;
    };
    stored.reactions = content::reactions_of(update);
    let _ = account.ctx.history(conversation, std::slice::from_ref(&stored));
    // The custom ones' pictures are asked for only while they are on screen; a page read
    // later covers a reaction that arrives to a conversation nobody is in.
    if account.ctx.watching().conversation.as_ref().is_some_and(|open| open.account() == account.ctx.account && open.id == conversation) {
        sync::reaction_emoji(account, conversation, std::slice::from_ref(&stored));
    }
}

pub(super) fn apply(account: &Account, update: &Update) {
    // A pin or a folder: told row by row from what is held, never by reading every chat.
    if filing::apply(account, update) {
        return;
    }
    match update {
        Update::NewMessage(new) => {
            gone_from_schedule(account, &new.message);
            message(account, &new.message, false)
        }
        Update::NewChannelMessage(new) => message(account, &new.message, false),
        Update::EditMessage(edit) => message(account, &edit.message, true),
        Update::EditChannelMessage(edit) => message(account, &edit.message, true),
        Update::MessageId(named) => {
            let mut live = account.live();
            if let Some(at) = live.random.iter().position(|(random, _)| *random == named.random_id) {
                if let Some((_, client_id)) = live.random.remove(at) {
                    if live.sent.len() >= 64 {
                        live.sent.pop_front();
                    }
                    live.sent.push_back((named.id, client_id));
                }
            }
        }
        // Outside a channel a deletion names the messages and not their chat.
        // CONTRACT: the store cannot be asked which conversation holds a message, so only
        // messages seen since this connection began are found; an older one goes when its
        // conversation is next opened and the page shows it missing.
        Update::DeleteMessages(gone) => {
            for chat in &mut account.pending().chats {
                if matches!(chat.peer, Who::Group(_)) {
                    chat.messages.retain(|message| !gone.messages.contains(&message.id()));
                }
            }
            for id in &gone.messages {
                if let Some(conversation) = account.placed(None, *id) {
                    if !drop_member(account, &conversation, *id) {
                        let _ = account.ctx.deleted(&conversation, &id.to_string());
                    }
                }
            }
        }
        Update::DeleteChannelMessages(gone) => {
            let who = Who::Channel(gone.channel_id);
            if let Some(chat) = account.pending().chats.iter_mut().find(|chat| chat.peer == who) {
                chat.messages.retain(|message| !gone.messages.contains(&message.id()));
            }
            let forum = account.dir().get(who).is_some_and(|known| known.forum);
            for id in &gone.messages {
                let conversation = if forum { account.placed(Some(who), *id) } else { Some(who.text()) };
                if let Some(conversation) = conversation {
                    if !drop_member(account, &conversation, *id) {
                        let _ = account.ctx.deleted(&conversation, &id.to_string());
                    }
                }
            }
        }
        Update::UserTyping(typed) => {
            let from = Who::User(typed.user_id);
            typing(account, from.text(), from, &typed.action);
        }
        Update::ChatUserTyping(typed) => typing(account, Who::Group(typed.chat_id).text(), Who::of(&typed.from_id), &typed.action),
        Update::ChannelUserTyping(typed) => {
            let conversation = place(account, Who::Channel(typed.channel_id), typed.top_msg_id);
            typing(account, conversation, Who::of(&typed.from_id), &typed.action);
        }
        Update::UserStatus(status) => {
            let who = Who::User(status.user_id);
            let mine = status.user_id == account.me;
            let seen = super::peers::rough_seen(Some(&status.status));
            if !mine {
                if let Some(row) = account.dir().note_seen(who, seen.clone()) {
                    let _ = account.ctx.people(std::slice::from_ref(&row));
                }
            }
            let (owned, premium) = account.dir().get(who).map(|known| (known.contact, known.premium)).unwrap_or((false, false));
            let line = super::peers::row_status(mine, premium, &seen);
            account.ctx.presence(&who.text(), presence(Some(&status.status)), &line, owned || mine);
        }
        // The name rides the update itself. A short update carries no user object, so the
        // directory is patched here. A phone change is not stored: the next card reads it.
        Update::UserName(renamed) => {
            if let Some(row) = account.dir().rename(renamed.user_id, &renamed.first_name, &renamed.last_name, &renamed.usernames) {
                let _ = account.ctx.people(std::slice::from_ref(&row));
            }
        }
        Update::ReadHistoryInbox(seen) => {
            let conversation = place(account, Who::of(&seen.peer), seen.top_msg_id);
            read(account, &conversation, seen.max_id, seen.still_unread_count);
        }
        Update::ReadChannelInbox(seen) => read(account, &Who::Channel(seen.channel_id).text(), seen.max_id, seen.still_unread_count),
        Update::ChatParticipantDelete(changed) if changed.user_id == account.me => left(account, Who::Group(changed.chat_id)),
        Update::ChannelParticipant(changed) if changed.user_id == account.me => {
            use tl::enums::ChannelParticipant;
            if match &changed.new_participant {
                None | Some(ChannelParticipant::Left(_)) => true,
                Some(ChannelParticipant::Banned(banned)) => banned.left || matches!(&banned.banned_rights, tl::enums::ChatBannedRights::Rights(rights) if rights.view_messages),
                _ => false,
            } {
                left(account, Who::Channel(changed.channel_id));
            }
        }
        // Telegram's second tick: the other side read my messages up to here.
        Update::ReadHistoryOutbox(seen) => {
            let who = Who::of(&seen.peer);
            if who != Who::User(account.me) {
                let _ = account.ctx.seen(&who.text(), &seen.max_id.to_string());
            }
        }
        Update::ReadChannelOutbox(seen) => {
            let who = Who::Channel(seen.channel_id);
            if !account.dir().get(who).is_some_and(|known| known.broadcast || known.forum) {
                let _ = account.ctx.seen(&who.text(), &seen.max_id.to_string());
            }
        }
        // A scheduled message went, or was dropped: its placeholder goes.
        Update::DeleteScheduledMessages(gone) => {
            let conversation = Who::of(&gone.peer).text();
            for id in &gone.messages {
                let _ = account.ctx.deleted(&conversation, &format!("s{id}"));
            }
        }
        Update::MessageReactions(reacted) => {
            let conversation = place(account, Who::of(&reacted.peer), reacted.top_msg_id);
            let message = written_id(account, reacted.msg_id).unwrap_or(reacted.msg_id);
            reactions(account, &conversation, message, &reacted.reactions);
        }
        // A draft typed (or cleared) on the phone while we are here, or ours echoed back.
        Update::DraftMessage(drafted) => {
            let who = Who::of(&drafted.peer);
            let id = if account.dir().get(who).is_some_and(|known| known.forum) {
                // The draft of a forum belongs to the topic it was typed in; none named is
                // Telegram's naming of General.
                thread_id(who, drafted.top_msg_id.unwrap_or(1))
            } else {
                // A comments thread's draft rides the discussion group, kept under the
                // echo of its post: the thread's own id is where this row's draft belongs.
                sync::echo_thread(account, who, drafted.top_msg_id).unwrap_or_else(|| who.text())
            };
            let text = match &drafted.draft {
                tl::enums::DraftMessage::Message(draft) => content::draft_text(&draft.message, draft.entities.as_deref()),
                _ => String::new(),
            };
            let _ = account.ctx.draft(&id, &text);
        }
        Update::NotifySettings(settings) => match &settings.peer {
            tl::enums::NotifyPeer::Peer(peer) => {
                let who = Who::of(&peer.peer);
                let muted = sync::is_muted(account, who, &settings.notify_settings);
                let _ = account.ctx.muted(&who.text(), muted);
                let stamp = filing::mute_stamp(&settings.notify_settings);
                filing::facts_changed(account, who, |facts| {
                    facts.muted = muted;
                    facts.mute_until = stamp;
                });
            }
            tl::enums::NotifyPeer::NotifyForumTopic(topic) => {
                let who = Who::of(&topic.peer);
                let _ = account.ctx.muted(&thread_id(who, topic.top_msg_id), sync::is_muted(account, who, &settings.notify_settings));
            }
            // A default changed: every chat without a setting of its own follows it, which only
            // reading the chats again shows.
            _ => account.live().stale = true,
        },
        // A chat joined, left or changed: what changed came with the update's chats, which
        // `learn` has already written.
        _ => {}
    }
}

pub(super) fn left(account: &Account, who: Who) {
    account.dir().left(who);
    account.pending().chats.retain(|chat| chat.peer != who);
    account.live().recovered_through.retain(|(peer, _)| *peer != who);
    if let Who::Channel(id) = who {
        account.wire.channel_membership(id, false);
    }
    sync::remove_chat(account, who);
}

/// Applies what a request of ours changed, from its own answer: the caller reports the change
/// before it answers the shell. The same updates also arrive in order with the rest, where
/// recording them again changes nothing.
pub(super) fn answered(account: &Account, answer: &tl::enums::Updates) {
    match answer {
        tl::enums::Updates::Updates(all) => {
            account.learn(&all.users, &all.chats);
            all.updates.iter().for_each(|update| apply(account, update));
        }
        tl::enums::Updates::Combined(all) => {
            account.learn(&all.users, &all.chats);
            all.updates.iter().for_each(|update| apply(account, update));
        }
        tl::enums::Updates::UpdateShort(short) => apply(account, &short.update),
        _ => {}
    }
}

/// The message Telegram made of a send, from the answer to it.
pub(super) fn sent(account: &Account, conversation: &str, who: Who, text: &str, reply: Option<i32>, answer: &tl::enums::Updates) -> Option<Message> {
    // The short answer names only the id and the time: the rest is what we sent.
    if let tl::enums::Updates::UpdateShortSentMessage(short) = answer {
        let mut message = Message::plain(short.id.to_string(), "", text, i64::from(short.date), true);
        // A send into a chat that keeps its messages only for a while comes back with the
        // timer the chat gave it, like any other message there.
        message.expires_at = short.ttl_period.filter(|ttl| *ttl > 0).map(|ttl| i64::from(short.date) + i64::from(ttl));
        let me = Who::User(account.me);
        message.sender = protocol::Sender { id: me.text(), name: account.dir().name(me), ..protocol::Sender::default() };
        message.spans = content::spans(text, short.entities.as_deref().unwrap_or(&[]));
        let mut mapped = Mapped {
            peer: who,
            id: short.id,
            topic: None,
            discussion_top: None,
            reply,
            reply_note: None,
            message,
            wants: Vec::new(),
            group: None,
            members: Vec::new(),
        };
        finish(account, conversation, &mut mapped, &HashMap::new());
        return Some(mapped.message);
    }
    let (updates, users, chats) = match answer {
        tl::enums::Updates::Updates(all) => (&all.updates, &all.users, &all.chats),
        tl::enums::Updates::Combined(all) => (&all.updates, &all.users, &all.chats),
        _ => return None,
    };
    account.learn(users, chats);
    let mut outgoing = Vec::new();
    for update in updates {
        let raw = match update {
            Update::NewMessage(new) => &new.message,
            Update::NewChannelMessage(new) => &new.message,
            _ => continue,
        };
        outgoing.push(raw);
    }
    outgoing.sort_by_key(|raw| raw.id());
    if outgoing.iter().any(|raw| content::grouped_id(raw).is_some()) {
        let mut last = None;
        for raw in outgoing {
            let Some((place, mut mapped)) = placed(account, raw) else {
                continue;
            };
            if place != conversation || !mapped.message.outgoing {
                continue;
            }
            if mapped.message.outgoing {
                let mut live = account.live();
                if let Some(at) = live.sent.iter().position(|(id, _)| *id == mapped.id) {
                    mapped.message.client_id = live.sent.remove(at).map(|(_, client_id)| client_id);
                }
            }
            if let Some(folded) = fold(account, conversation, mapped) {
                if let Some(old) = folded.replaced {
                    let _ = account.ctx.deleted(conversation, &old.to_string());
                }
                for id in &folded.members {
                    account.place(folded.peer, *id, conversation);
                }
                last = Some(folded.message);
            }
        }
        return last;
    }
    outgoing.into_iter().find_map(|raw| {
        let (place, mapped) = placed(account, raw)?;
        (place == conversation && mapped.message.outgoing).then_some(mapped.message)
    })
}

/// A scheduled message Telegram has now sent is a message like any other; the placeholder the
/// schedule left goes. Telegram also says so itself (`DeleteScheduledMessages`); this covers
/// an account that hears the new message first.
fn gone_from_schedule(account: &Account, raw: &tl::enums::Message) {
    let tl::enums::Message::Message(sent) = raw else {
        return;
    };
    if !sent.from_scheduled || !sent.out {
        return;
    }
    let conversation = Who::of(&sent.peer_id).text();
    let held = {
        let mut live = account.live();
        let at = live.scheduled.iter().position(|(place, text, _)| *place == conversation && *text == sent.message);
        at.and_then(|at| live.scheduled.remove(at)).map(|(_, _, id)| id)
    };
    if let Some(id) = held {
        let _ = account.ctx.deleted(&conversation, &id);
    }
}

/// A send Telegram scheduled rather than sent: shown as sent now, under an id of its own
/// (`s<id>`), until Telegram sends it at its time and it is replaced by the real message.
// CONTRACT: a message has no "scheduled" state (nor a time it will go), so a scheduled one reads
// as sent now; dated at its time it would sit at the top of the Inbox until then.
pub(super) fn answered_scheduled(account: &Account, conversation: &str, text: &str, opts: Options, answer: &tl::enums::Updates) -> Option<Message> {
    let at = opts.schedule?;
    let updates = match answer {
        tl::enums::Updates::Updates(all) => &all.updates,
        tl::enums::Updates::Combined(all) => &all.updates,
        _ => return None,
    };
    let id = updates.iter().find_map(|update| match update {
        Update::NewScheduledMessage(new) => match &new.message {
            tl::enums::Message::Message(message) => Some(message.id),
            _ => None,
        },
        _ => None,
    })?;
    let me = Who::User(account.me);
    let id = format!("s{id}");
    let _ = at;
    let mut message = Message::plain(id.clone(), account.dir().name(me), text, crate::hub::now(), true);
    message.sender.id = me.text();
    let mut live = account.live();
    if live.scheduled.len() >= 64 {
        live.scheduled.pop_front();
    }
    live.scheduled.push_back((conversation.to_owned(), text.to_owned(), id));
    Some(message)
}
