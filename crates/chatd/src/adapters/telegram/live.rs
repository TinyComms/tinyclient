//! What Telegram says unasked, applied to the store and the hub as it arrives.

use super::content::{self, Mapped};
use super::peers::{thread_id, Who};
use super::sync::{self, conversation_of, ensure_row, finish};
use super::{filing, presence, Account, Options, TYPING_SHOWN};
use grammers_tl_types as tl;
use protocol::Message;
use std::collections::HashMap;
use tl::enums::Update;

/// The conversation a peer's update is about; in a forum, the thread of the topic it names.
fn place(account: &Account, who: Who, topic: Option<i32>) -> String {
    match who {
        Who::Channel(_) if account.dir().get(who).is_some_and(|known| known.forum) => thread_id(who, topic.unwrap_or(1)),
        who => who.text(),
    }
}

/// Maps a message and makes sure its conversation is recorded. `None` for one that cannot be
/// placed (an empty message, a chat Telegram never described).
fn placed(account: &Account, raw: &tl::enums::Message) -> Option<(String, Mapped)> {
    let mut mapped = content::map(&account.dir(), raw)?;
    let conversation = conversation_of(account, &mapped);
    let topic = conversation.contains('_').then(|| mapped.topic.unwrap_or(1));
    if !ensure_row(account, mapped.peer, topic) {
        return None;
    }
    finish(account, &conversation, &mut mapped, &HashMap::new());
    Some((conversation, mapped))
}

/// A message arrived, or one changed.
fn message(account: &Account, raw: &tl::enums::Message, edit: bool) {
    let Some((conversation, mut mapped)) = placed(account, raw) else {
        return;
    };
    // An edit of something never stored is not news: it would count as a new unread message.
    if edit && !matches!(account.ctx.stored_message(&conversation, &mapped.message.id), Ok(Some(_))) {
        return;
    }
    // Our own message coming back: it replaces the one the shell is showing as pending.
    if mapped.message.outgoing {
        let mut live = account.live();
        if let Some(at) = live.sent.iter().position(|(id, _)| *id == mapped.id) {
            mapped.message.client_id = live.sent.remove(at).map(|(_, client_id)| client_id);
        }
    }
    let _ = account.ctx.message(&conversation, &mapped.message);
    // What it shows is fetched only when the user is looking at the conversation; otherwise
    // opening it fetches the page it is on.
    let watching = account.ctx.watching();
    if watching.conversation.as_ref().is_some_and(|open| open.account() == account.ctx.account && open.id == conversation) {
        account.files.want(mapped.wants);
    }
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
            for id in &gone.messages {
                if let Some(conversation) = account.placed(None, *id) {
                    let _ = account.ctx.deleted(&conversation, &id.to_string());
                }
            }
        }
        Update::DeleteChannelMessages(gone) => {
            let who = Who::Channel(gone.channel_id);
            let forum = account.dir().get(who).is_some_and(|known| known.forum);
            for id in &gone.messages {
                let conversation = if forum { account.placed(Some(who), *id) } else { Some(who.text()) };
                if let Some(conversation) = conversation {
                    let _ = account.ctx.deleted(&conversation, &id.to_string());
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
            let owned = account.dir().get(who).is_some_and(|known| known.contact);
            account.ctx.presence(&who.text(), presence(Some(&status.status)), "", owned || status.user_id == account.me);
        }
        Update::ReadHistoryInbox(seen) => {
            let conversation = place(account, Who::of(&seen.peer), seen.top_msg_id);
            read(account, &conversation, seen.max_id, seen.still_unread_count);
        }
        Update::ReadChannelInbox(seen) => read(account, &Who::Channel(seen.channel_id).text(), seen.max_id, seen.still_unread_count),
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
            reactions(account, &conversation, reacted.msg_id, &reacted.reactions);
        }
        Update::NotifySettings(settings) => match &settings.peer {
            tl::enums::NotifyPeer::Peer(peer) => {
                let who = Who::of(&peer.peer);
                let muted = sync::is_muted(account, who, &settings.notify_settings);
                let _ = account.ctx.muted(&who.text(), muted);
                filing::facts_changed(account, who, |facts| facts.muted = muted);
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
        let me = Who::User(account.me);
        message.sender = protocol::Sender { id: me.text(), name: account.dir().name(me), ..protocol::Sender::default() };
        message.spans = content::spans(text, short.entities.as_deref().unwrap_or(&[]));
        let mut mapped = Mapped { peer: who, id: short.id, topic: None, reply, message, wants: Vec::new() };
        finish(account, conversation, &mut mapped, &HashMap::new());
        return Some(mapped.message);
    }
    let (updates, users, chats) = match answer {
        tl::enums::Updates::Updates(all) => (&all.updates, &all.users, &all.chats),
        tl::enums::Updates::Combined(all) => (&all.updates, &all.users, &all.chats),
        _ => return None,
    };
    account.learn(users, chats);
    updates.iter().find_map(|update| {
        let raw = match update {
            Update::NewMessage(new) => &new.message,
            Update::NewChannelMessage(new) => &new.message,
            _ => return None,
        };
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
