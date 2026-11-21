//! The panes: a person's card (in a group too, with their admin title), a chat's info, its
//! pins and what was shared in it, and the two things a pane may change: the mute and leaving.
//! Each is one request, made because the user opened or pressed something.

use super::peers::{self, Who};
use super::sync::{self, SERVICE};
use super::wire::Fault;
use super::Account;
use crate::adapters::{About, ConversationAbout, SharedPage};
use grammers_tl_types as tl;
use protocol::{Contact, ContactKind, Mark, Message, Notify, SharedItem, SharedKind, Style, Warning};

fn invalid(name: &str) -> Fault {
    Fault::Rpc { code: 400, name: name.to_owned(), value: None }
}

/// A shared group or supergroup. A broadcast channel is not a group in common.
fn mutual_title(chat: &tl::enums::Chat) -> Option<String> {
    match chat {
        tl::enums::Chat::Chat(chat) if !chat.left && !chat.deactivated && !chat.title.is_empty() => Some(chat.title.clone()),
        tl::enums::Chat::Channel(channel) if !channel.broadcast && !channel.left && !channel.title.is_empty() => Some(channel.title.clone()),
        _ => None,
    }
}

/// `14 March`, or `14 March 1990` when Telegram sent a year. A day or month outside the
/// calendar is left off.
pub(super) fn birthday_line(day: i32, month: i32, year: Option<i32>) -> Option<String> {
    let name = match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => return None,
    };
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(match year {
        Some(year) if (1..=9_999).contains(&year) => format!("{day} {name} {year}"),
        _ => format!("{day} {name}"),
    })
}

/// Records call capability flags from `users.getFullUser` and refreshes the DM row when changed.
fn record_call_flags(account: &Account, who: Who, user: &tl::types::UserFull) {
    let flags = peers::CallFlags {
        phone_available: user.phone_calls_available,
        phone_private: user.phone_calls_private,
        video_available: user.video_calls_available,
    };
    if account.dir().set_call_flags(who, flags) {
        sync::refresh_row(account, who);
    }
}

/// A person's card: what `users.getFullUser` says, with what is already known of them.
pub(super) fn profile(account: &Account, person_id: &str) -> Result<About, Fault> {
    let Some((who @ Who::User(user_id), None)) = Who::parse(person_id) else {
        return Err(invalid("USER_ID_INVALID"));
    };
    account.input(who)?;
    let id = account.dir().input_user(who).ok_or_else(|| invalid("USER_ID_INVALID"))?;
    let tl::enums::users::UserFull::Full(full) = account.ask(&tl::functions::users::GetFullUser { id: id.clone() })?;
    account.learn(&full.users, &full.chats);
    let tl::enums::UserFull::Full(user) = full.full_user;
    record_call_flags(account, who, &user);
    if user.blocked {
        account.set_user_blocked(user_id, true);
        let _ = account.ctx.relation(person_id, protocol::Relation::Blocked);
    }
    let mut about = About { bio: user.about.unwrap_or_default(), mutual_total: user.common_chats_count.max(0) as u32, ..About::default() };
    // A failure here keeps the count and an empty list. The card still opens.
    if let Ok(shared) = account.ask(&tl::functions::messages::GetCommonChats { user_id: id, max_id: 0, limit: 100 }) {
        let chats = shared.chats();
        account.learn(&[], &chats);
        about.mutual_spaces = chats.iter().filter_map(mutual_title).collect();
        about.mutual_total = about.mutual_total.max(about.mutual_spaces.len() as u32);
    }
    let this = full.users.iter().find_map(|found| match found {
        tl::enums::User::User(found) if found.id == user_id => Some(found),
        _ => None,
    });
    if let Some(this) = this {
        // Shown only where Telegram returns it; chatd masks it until clicked.
        if let Some(phone) = this.phone.as_ref().filter(|phone| !phone.is_empty()) {
            about.contacts.push(Contact { kind: ContactKind::Phone, value: format!("+{phone}"), masked: true });
        }
        if let Some(tl::enums::UserStatus::Offline(offline)) = &this.status {
            about.last_seen = Some(i64::from(offline.was_online));
        }
    }
    let known = account.dir().get(who).cloned().unwrap_or_default();
    let mine = user_id == account.me;
    account.ctx.presence(
        &who.text(),
        super::presence(this.and_then(|user| user.status.as_ref())),
        &peers::row_status(mine, known.premium, &known.seen),
        known.contact || mine,
    );
    if !known.handle.is_empty() {
        about.fields.push(("Username".to_owned(), format!("@{}", known.handle)));
    }
    if let Some(this) = this {
        let also: Vec<String> = this
            .usernames
            .iter()
            .flatten()
            .filter_map(|name| {
                let tl::enums::Username::Username(name) = name;
                (name.active && name.username != known.handle && !name.username.is_empty()).then(|| format!("@{}", name.username))
            })
            .collect();
        if !also.is_empty() {
            about.fields.push(("Also".to_owned(), also.join(", ")));
        }
    }
    if let Some(line) = user.birthday.as_ref().and_then(|day| {
        let tl::enums::Birthday::Birthday(day) = day;
        birthday_line(day.day, day.month, day.year)
    }) {
        about.fields.push(("Birthday".to_owned(), line));
    }
    if let Some(tl::enums::BusinessLocation::Location(place)) = &user.business_location {
        if !place.address.is_empty() {
            about.fields.push(("Location".to_owned(), place.address.clone()));
        }
    }
    if let Some(tl::enums::BusinessWorkHours::Hours(hours)) = &user.business_work_hours {
        let word = if hours.open_now { "Open now" } else { "Closed" };
        let line = if hours.timezone_id.is_empty() { word.to_owned() } else { format!("{word} · {}", hours.timezone_id) };
        about.fields.push(("Hours".to_owned(), line));
    }
    if let Some(name) = user.personal_channel_id.and_then(|id| account.dir().get(Who::Channel(id)).map(|known| known.name.clone())) {
        if !name.is_empty() {
            about.fields.push(("Channel".to_owned(), name));
        }
    }
    about.warning = match known.mark {
        Some(Mark::Scam) => Some(Warning::Scam),
        Some(Mark::Fake) => Some(Warning::Fake),
        _ => None,
    };
    Ok(about)
}

/// The same card opened inside a group or channel: their admin title there and when they
/// joined, from one more request. A basic group keeps no titles.
pub(super) fn profile_in(account: &Account, person_id: &str, place: &str) -> Result<About, Fault> {
    let mut about = profile(account, person_id)?;
    let (Some((who, None)), Some((chat @ Who::Channel(_), _))) = (Who::parse(person_id), Who::parse(place)) else {
        return Ok(about);
    };
    let request = tl::functions::channels::GetParticipant { channel: account.input_channel(chat)?, participant: account.input(who)? };
    let tl::enums::channels::ChannelParticipant::Participant(found) = match account.ask(&request) {
        Ok(found) => found,
        // Not a member there (any more): the card without it.
        Err(fault) if fault.is("USER_NOT_PARTICIPANT") => return Ok(about),
        Err(fault) => return Err(fault),
    };
    account.learn(&found.users, &found.chats);
    let (rank, joined) = match found.participant {
        tl::enums::ChannelParticipant::Creator(one) => (one.rank, None),
        tl::enums::ChannelParticipant::Admin(one) => (one.rank, Some(one.date)),
        tl::enums::ChannelParticipant::Participant(one) => (None, Some(one.date)),
        tl::enums::ChannelParticipant::ParticipantSelf(one) => (None, Some(one.date)),
        _ => (None, None),
    };
    about.nickname = rank.unwrap_or_default();
    about.joined = joined.map(i64::from);
    Ok(about)
}

/// A chat's info pane.
pub(super) fn conversation(account: &Account, conversation: &str) -> Result<ConversationAbout, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let known = account.dir().get(who).cloned().unwrap_or_default();
    let notify = match account.ctx.known(conversation) {
        Ok(Some(row)) if row.muted => Notify::Nothing,
        _ => Notify::All,
    };
    let mut about = ConversationAbout {
        notify,
        link: if known.handle.is_empty() { String::new() } else { format!("https://t.me/{}", known.handle) },
        ..ConversationAbout::default()
    };
    match who {
        Who::User(id) if id == account.me || id == SERVICE => {}
        Who::User(_) => {
            let id = account.dir().input_user(who).ok_or_else(|| invalid("USER_ID_INVALID"))?;
            let tl::enums::users::UserFull::Full(full) = account.ask(&tl::functions::users::GetFullUser { id })?;
            account.learn(&full.users, &full.chats);
            let tl::enums::UserFull::Full(user) = full.full_user;
            record_call_flags(account, who, &user);
            about.description = user.about.unwrap_or_default();
            about.members = Some(2);
            about.disappearing = user.ttl_period.filter(|&s| s > 0).map(|s| s as u32);
            let _ = account.ctx.disappearing(conversation, about.disappearing);
        }
        Who::Group(chat_id) => {
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
            account.learn(&full.users, &full.chats);
            if let tl::enums::ChatFull::Full(chat) = full.full_chat {
                about.description = chat.about;
                about.disappearing = chat.ttl_period.filter(|&s| s > 0).map(|s| s as u32);
                let _ = account.ctx.disappearing(conversation, about.disappearing);
                if let tl::enums::ChatParticipants::Participants(list) = chat.participants {
                    about.members = Some(list.participants.len() as u32);
                    about.creator = list
                        .participants
                        .iter()
                        .find_map(|one| match one {
                            tl::enums::ChatParticipant::Creator(one) => Some(account.dir().name(Who::User(one.user_id))),
                            _ => None,
                        })
                        .unwrap_or_default();
                }
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::channels::GetFullChannel { channel })?;
            account.learn(&full.users, &full.chats);
            if let tl::enums::ChatFull::ChannelFull(channel) = full.full_chat {
                about.description = channel.about;
                about.members = channel.participants_count.map(|count| count.max(0) as u32);
                about.online = channel.online_count.map(|count| count.max(0) as u32);
                about.disappearing = channel.ttl_period.filter(|&s| s > 0).map(|s| s as u32);
                let _ = account.ctx.disappearing(conversation, about.disappearing);
            }
        }
    }
    Ok(about)
}

/// Sets or clears Telegram's auto-delete timer (`messages.setHistoryTTL`).
pub(super) fn set_disappearing(account: &Account, conversation: &str, seconds: u32) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    account.ask(&tl::functions::messages::SetHistoryTtl { peer, period: seconds as i32 })?;
    let timer = (seconds > 0).then_some(seconds);
    let _ = account.ctx.disappearing(conversation, timer);
    let line = if seconds == 0 {
        "Disappearing messages are off".to_owned()
    } else {
        format!("Disappearing messages are set to {}", protocol::format_disappearing(seconds))
    };
    let now = crate::hub::now();
    let mut msg = Message::plain(format!("tg-ttl-{now}"), String::new(), line, now, false);
    msg.kind = protocol::MessageKind::System;
    let _ = account.ctx.message(conversation, &msg);
    Ok(())
}

/// The network's own mute. Telegram has no "mentions only" for a chat: a muted chat still
/// counts its mentions, so that is what `Mentions` sets (unverified that every app notifies them).
/// `until` is when that mute ends, as Unix seconds. Absent is muted until it is turned off.
/// A time that has already passed is not muted.
pub(super) fn set_mute(account: &Account, conversation: &str, notify: Notify, until: Option<i64>) -> Result<(), Fault> {
    let (who, topic) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    if who == Who::User(SERVICE) {
        return Err(Fault::Rpc { code: 400, name: "SERVICE_NEVER_MUTED".to_owned(), value: None });
    }
    let peer = account.input(who)?;
    let asked = notify != Notify::All;
    let stamp = if !asked {
        0
    } else {
        match until {
            None => i32::MAX,
            Some(unix) => unix.clamp(0, i64::from(i32::MAX)) as i32,
        }
    };
    let muted = super::filing::mute_holds(stamp);
    let settings = tl::types::InputPeerNotifySettings {
        show_previews: None,
        silent: None,
        mute_until: Some(stamp),
        sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_sound: None,
    };
    let peer = match topic {
        Some(top_msg_id) => tl::types::InputNotifyForumTopic { peer, top_msg_id }.into(),
        None => tl::types::InputNotifyPeer { peer }.into(),
    };
    account.ask(&tl::functions::account::UpdateNotifySettings { peer, settings: settings.into() })?;
    let _ = account.ctx.muted(conversation, muted);
    super::filing::facts_changed(account, who, |facts| {
        facts.muted = muted;
        if topic.is_none() {
            // No end time once it is off, so a later topic mute is not cleared as expired.
            facts.mute_until = (stamp != 0).then_some(i64::from(stamp));
        }
    });
    Ok(())
}

/// Leaves a group or a channel; history stays on Telegram for the others.
pub(super) fn leave(account: &Account, conversation: &str) -> Result<(), Fault> {
    let Some((who, None)) = Who::parse(conversation) else {
        return Err(invalid("PEER_NOT_A_GROUP"));
    };
    let answer = match who {
        Who::Group(chat_id) => account.ask(&tl::functions::messages::DeleteChatUser {
            revoke_history: false,
            chat_id,
            user_id: tl::enums::InputUser::UserSelf,
        })?,
        Who::Channel(_) => account.ask(&tl::functions::channels::LeaveChannel { channel: account.input_channel(who)? })?,
        _ => return Err(invalid("PEER_NOT_A_GROUP")),
    };
    super::live::answered(account, &answer);
    super::live::left(account, who);
    Ok(())
}

/// Clears a chat's history (`delete_chat == false`) or deletes the chat (`delete_chat == true`).
/// `revoke` also removes the other side's copy where Telegram allows it. Loops while Telegram
/// returns a non-zero `offset`.
pub(super) fn clear_history(account: &Account, conversation: &str, delete_chat: bool, revoke: bool) -> Result<(), Fault> {
    let Some((who, None)) = Who::parse(conversation) else {
        return Err(invalid("PEER_ID_INVALID"));
    };
    match who {
        Who::User(_) | Who::Group(_) => {
            let peer = account.input(who)?;
            let mut pages = 0u32;
            loop {
                let tl::enums::messages::AffectedHistory::History(affected) = account.ask(
                    &tl::functions::messages::DeleteHistory {
                        just_clear: !delete_chat,
                        revoke,
                        peer: peer.clone(),
                        max_id: 0,
                        min_date: None,
                        max_date: None,
                    },
                )?;
                pages += 1;
                if affected.offset == 0 || pages >= 64 {
                    break;
                }
            }
            if delete_chat {
                super::live::left(account, who);
            } else {
                let _ = account.ctx.clear_messages(conversation);
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let answer = account.ask(&tl::functions::channels::DeleteHistory {
                for_everyone: revoke,
                channel,
                max_id: 0,
            })?;
            super::live::answered(account, &answer);
            if delete_chat {
                super::live::left(account, who);
            } else {
                let _ = account.ctx.clear_messages(conversation);
            }
        }
    }
    Ok(())
}

fn search(account: &Account, conversation: &str, query: &str, filter: tl::enums::MessagesFilter, offset_id: i32, limit: i32) -> Result<(Vec<Message>, Option<u32>), Fault> {
    let (who, topic) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let request = tl::functions::messages::Search {
        peer: account.input(who)?,
        q: query.chars().take(1024).collect(),
        from_id: None,
        saved_peer_id: None,
        saved_reaction: None,
        top_msg_id: topic.filter(|topic| *topic != 1),
        filter,
        min_date: 0,
        max_date: 0,
        offset_id,
        add_offset: 0,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    };
    let (raw, count) = sync::searched(account, account.ask(&request)?);
    let (messages, wants) = sync::mapped(account, conversation, &raw);
    account.files.want(wants);
    Ok((messages, count))
}

/// Text matches from Telegram, bounded to one page and mapped like cached search hits.
pub(super) fn search_messages(account: &Account, conversation: &str, query: &str) -> Result<Vec<protocol::MessageHit>, Fault> {
    let (messages, _) = search(account, conversation, query, tl::enums::MessagesFilter::InputMessagesFilterEmpty, 0, 100)?;
    let known = account.ctx.known(conversation).ok().flatten();
    Ok(messages.into_iter().take(100).map(|message| protocol::MessageHit {
        conversation: account.ctx.key(conversation),
        message_id: message.id,
        sent: message.sent,
        sender: message.sender.name,
        snippet: crate::store::snippet(&message.text, query),
        title: known.as_ref().map(|row| row.title.clone()).unwrap_or_default(),
        space_title: known.as_ref().map(|row| row.space_title.clone()).unwrap_or_default(),
        outgoing: message.outgoing,
    }).collect())
}

/// The pinned messages, newest first.
pub(super) fn pinned(account: &Account, conversation: &str) -> Result<Vec<Message>, Fault> {
    Ok(search(account, conversation, "", tl::enums::MessagesFilter::InputMessagesFilterPinned, 0, protocol::PINNED_LIMIT as i32)?.0)
}

pub(super) fn pin(
    account: &Account,
    conversation: &str,
    message_id: &str,
    pinned: bool,
    notify: bool,
    me_only: bool,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let id = message_id.parse::<i32>().map_err(|_| invalid("MESSAGE_ID_INVALID"))?;
    let peer = account.input(who)?;
    account.ask(&tl::functions::messages::UpdatePinnedMessage {
        silent: !notify,
        unpin: !pinned,
        pm_oneside: me_only,
        peer,
        id,
    })?;
    let _ = account.ctx.pinned(conversation, message_id, pinned);
    Ok(())
}

pub(super) fn forward(
    account: &Account,
    from: &str,
    message_ids: &[String],
    targets: &[String],
    note: &str,
) -> Result<Vec<(String, Result<(), String>)>, Fault> {
    let (from_who, _) = Who::parse(from).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let from_peer = account.input(from_who)?;
    let ids: Vec<i32> = message_ids
        .iter()
        .filter_map(|id| id.parse::<i32>().ok())
        .collect();
    if ids.is_empty() {
        return Err(invalid("MESSAGE_ID_INVALID"));
    }
    let note = note.trim();
    let mut results = Vec::with_capacity(targets.len());
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as i64);
    for (t_ix, target) in targets.iter().enumerate() {
        let res: Result<(), Fault> = (|| {
            let (to_who, top_msg_id) = Who::parse(target).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
            let to_peer = account.input(to_who)?;
            let random_id: Vec<i64> = ids
                .iter()
                .enumerate()
                .map(|(m_ix, _)| base.wrapping_add(((t_ix as i64) << 16) + m_ix as i64 + 1))
                .collect();
            account.ask(&tl::functions::messages::ForwardMessages {
                silent: false,
                background: false,
                with_my_score: false,
                drop_author: false,
                drop_media_captions: false,
                noforwards: false,
                allow_paid_floodskip: false,
                from_peer: from_peer.clone(),
                id: ids.clone(),
                random_id,
                to_peer: to_peer.clone(),
                top_msg_id,
                reply_to: None,
                schedule_date: None,
                send_as: None,
                quick_reply_shortcut: None,
                video_timestamp: None,
                effect: None,
                schedule_repeat_period: None,
                allow_paid_stars: None,
                suggested_post: None,
            })?;
            if !note.is_empty() {
                account.ask(&tl::functions::messages::SendMessage {
                    no_webpage: false,
                    silent: false,
                    background: false,
                    clear_draft: false,
                    noforwards: false,
                    update_stickersets_order: false,
                    invert_media: false,
                    allow_paid_floodskip: false,
                    rich_message: None,
                    peer: to_peer,
                    reply_to: None,
                    message: note.to_owned(),
                    random_id: base.wrapping_add(((t_ix as i64) << 16) + 9999),
                    reply_markup: None,
                    entities: None,
                    schedule_date: None,
                    schedule_repeat_period: None,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    allow_paid_stars: None,
                    suggested_post: None,
                })?;
            }
            Ok(())
        })();
        results.push((target.clone(), res.map_err(|e| format!("{e:?}"))));
    }
    Ok(results)
}

/// What was shared, from Telegram's own search, with Telegram's count of it as the total.
pub(super) fn shared(account: &Account, conversation: &str, kind: SharedKind, before: Option<&str>, limit: u32) -> Result<SharedPage, Fault> {
    let filter = match kind {
        SharedKind::Media => tl::enums::MessagesFilter::InputMessagesFilterPhotoVideo,
        SharedKind::Files => tl::enums::MessagesFilter::InputMessagesFilterDocument,
        SharedKind::Links => tl::enums::MessagesFilter::InputMessagesFilterUrl,
    };
    let limit = limit.clamp(1, 100);
    let offset = before.and_then(|id| id.parse::<i32>().ok()).unwrap_or(0);
    let (messages, total) = search(account, conversation, "", filter, offset, limit as i32)?;
    let more = messages.len() as u32 >= limit;
    let mut items = Vec::new();
    for message in messages {
        let item = |attachment, url: String, title: String| SharedItem {
            message_id: message.id.clone(),
            sent: message.sent,
            sender: message.sender.name.clone(),
            attachment,
            url,
            title,
        };
        match kind {
            SharedKind::Media | SharedKind::Files => {
                items.extend(message.attachments.iter().map(|attachment| item(Some(attachment.clone()), String::new(), String::new())));
            }
            SharedKind::Links => {
                let mut urls: Vec<(String, String)> = message.previews.iter().map(|preview| (preview.url.clone(), preview.title.clone())).collect();
                for span in &message.spans {
                    if let Style::Link { url } = &span.style {
                        if !urls.iter().any(|(known, _)| known == url) {
                            urls.push((url.clone(), String::new()));
                        }
                    }
                }
                items.extend(urls.into_iter().map(|(url, title)| item(None, url, title)));
            }
        }
    }
    Ok(SharedPage { items, more, total })
}

/// Recent calls across all chats on this Telegram account via `messages.search` with an empty
/// peer and `inputMessagesFilterPhoneCalls`, paged by `offset_id`.
pub(super) fn call_history(
    account: &Account,
    missed: bool,
    before: Option<&str>,
    limit: u32,
) -> Result<crate::adapters::CallHistoryPage, Fault> {
    let limit = limit.clamp(1, 100);
    let offset_id = before.and_then(|id| id.parse::<i32>().ok()).unwrap_or(0);
    let request = tl::functions::messages::Search {
        peer: tl::enums::InputPeer::Empty,
        q: String::new(),
        from_id: None,
        saved_peer_id: None,
        saved_reaction: None,
        top_msg_id: None,
        filter: tl::types::InputMessagesFilterPhoneCalls { missed }.into(),
        min_date: 0,
        max_date: 0,
        offset_id,
        add_offset: 0,
        limit: limit as i32,
        max_id: 0,
        min_id: 0,
        hash: 0,
    };
    let answer = account.ask(&request)?;
    let (raw, users, chats) = match answer {
        tl::enums::messages::Messages::Messages(list) => (list.messages, list.users, list.chats),
        tl::enums::messages::Messages::Slice(list) => (list.messages, list.users, list.chats),
        tl::enums::messages::Messages::ChannelMessages(list) => (list.messages, list.users, list.chats),
        tl::enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new()),
    };
    account.learn(&users, &chats);
    let raw_len = raw.len() as u32;
    let dir = account.dir();
    let mut entries = Vec::with_capacity(raw.len());
    for item in &raw {
        let Some(mapped) = super::content::map(&dir, item) else {
            continue;
        };
        let Some(note) = mapped.message.call else {
            continue;
        };
        let peer_id = mapped.peer.text();
        let name = dir.name(mapped.peer);
        let person_id = match mapped.peer {
            Who::User(_) => peer_id.clone(),
            _ => String::new(),
        };
        entries.push(protocol::CallLogEntry {
            conversation: protocol::Key::new(&account.ctx.account, peer_id),
            person_id,
            name,
            message_id: mapped.message.id,
            outgoing: mapped.message.outgoing,
            note,
            time: mapped.message.sent,
        });
    }
    let next = entries.last().map(|e| e.message_id.clone());
    let more = raw_len >= limit && next.is_some();
    Ok(crate::adapters::CallHistoryPage { entries, next, more })
}

/// Deletes call entries from the account's call log via `messages.deleteMessages`.
pub(super) fn delete_call_entries(account: &Account, message_ids: &[String], revoke: bool) -> Result<(), Fault> {
    let ids: Vec<i32> = message_ids.iter().filter_map(|id| id.parse::<i32>().ok()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    account.ask(&tl::functions::messages::DeleteMessages { revoke, id: ids })?;
    Ok(())
}
