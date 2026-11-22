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

/// Records call and voice-message capability flags from `users.getFullUser` and refreshes the DM row when changed.
fn record_call_flags(account: &Account, who: Who, user: &tl::types::UserFull) {
    let flags = peers::CallFlags {
        phone_available: user.phone_calls_available,
        phone_private: user.phone_calls_private,
        video_available: user.video_calls_available,
    };
    let mut dir = account.dir();
    let call_changed = dir.set_call_flags(who, flags);
    let voice_changed = dir.set_voice_forbidden(who, user.voice_messages_forbidden);
    drop(dir);
    if call_changed || voice_changed {
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
                about.pending_requests = chat
                    .requests_pending
                    .map(|n| n.max(0) as u32)
                    .or_else(|| account.live().pending_requests.get(&who).copied())
                    .unwrap_or(0);
                if about.link.is_empty() {
                    if let Some(tl::enums::ExportedChatInvite::ChatInviteExported(inv)) = &chat.exported_invite {
                        if !inv.revoked && !inv.link.is_empty() {
                            about.link = inv.link.clone();
                        }
                    }
                }
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
                about.pending_requests = channel
                    .requests_pending
                    .map(|n| n.max(0) as u32)
                    .or_else(|| account.live().pending_requests.get(&who).copied())
                    .unwrap_or(0);
                if about.link.is_empty() {
                    if let Some(tl::enums::ExportedChatInvite::ChatInviteExported(inv)) = &channel.exported_invite {
                        if !inv.revoked && !inv.link.is_empty() {
                            about.link = inv.link.clone();
                        }
                    }
                }
            }
        }
    }
    Ok(about)
}

fn invite_hash(raw: &str) -> Result<String, Fault> {
    if let Some(hash) = protocol::link::telegram_invite_hash(raw) {
        return Ok(hash);
    }
    let trimmed = raw.trim().trim_start_matches('+');
    if !trimmed.is_empty()
        && trimmed.len() <= 128
        && trimmed.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Ok(trimmed.to_owned());
    }
    Err(Fault::Net("Enter a valid Telegram invite link (t.me/+… or t.me/joinchat/…).".to_owned()))
}

fn chat_peer_and_title(chat: &tl::enums::Chat) -> Option<(Who, String, Option<u32>)> {
    match chat {
        tl::enums::Chat::Chat(c) => Some((Who::Group(c.id), c.title.clone(), Some(c.participants_count.max(0) as u32))),
        tl::enums::Chat::Channel(c) => Some((Who::Channel(c.id), c.title.clone(), c.participants_count.map(|n| n.max(0) as u32))),
        _ => None,
    }
}

fn map_exported_invite(inv: tl::types::ChatInviteExported) -> protocol::ChatInviteLink {
    protocol::ChatInviteLink {
        link: inv.link,
        title: inv.title.unwrap_or_default(),
        permanent: inv.permanent,
        revoked: inv.revoked,
        request_needed: inv.request_needed,
        expire_date: inv.expire_date.map(i64::from),
        usage_limit: inv.usage_limit.map(|n| n.max(0) as u32),
        usage: inv.usage.unwrap_or(0).max(0) as u32,
        pending_requests: inv.requested.unwrap_or(0).max(0) as u32,
    }
}

/// Previews a Telegram invite link without joining (`messages.checkChatInvite`).
pub(super) fn invite_preview(account: &Account, raw: &str) -> Result<protocol::InvitePreview, Fault> {
    let hash = invite_hash(raw)?;
    let checked = match account.ask(&tl::functions::messages::CheckChatInvite { hash: hash.clone() }) {
        Ok(found) => found,
        Err(fault) if fault.is("INVITE_HASH_EXPIRED") || fault.is("INVITE_HASH_INVALID") || fault.is("INVITE_HASH_EMPTY") => {
            return Err(Fault::Net("This invite link is invalid or has expired.".to_owned()));
        }
        Err(fault) => return Err(fault),
    };
    match checked {
        tl::enums::ChatInvite::Already(already) => {
            account.learn(&[], std::slice::from_ref(&already.chat));
            let (who, name, members) = chat_peer_and_title(&already.chat)
                .ok_or_else(|| Fault::Net("Telegram returned an unreadable chat for this invite.".to_owned()))?;
            sync::ensure_row(account, who, None);
            Ok(protocol::InvitePreview {
                code: hash,
                name,
                description: "You are already a member of this chat.".to_owned(),
                icon: None,
                members,
                online: None,
                space_id: None,
                channel_id: Some(who.text()),
                request_needed: false,
            })
        }
        tl::enums::ChatInvite::Invite(inv) => {
            if let Some(ref people) = inv.participants {
                account.learn(people, &[]);
            }
            let mut description = inv.about.unwrap_or_default();
            if inv.request_needed {
                if !description.is_empty() {
                    description.push_str(" · ");
                }
                description.push_str("Admin approval required to join.");
            }
            Ok(protocol::InvitePreview {
                code: hash,
                name: inv.title,
                description,
                icon: None,
                members: Some(inv.participants_count.max(0) as u32),
                online: None,
                space_id: None,
                channel_id: None,
                request_needed: inv.request_needed,
            })
        }
        tl::enums::ChatInvite::Peek(peek) => {
            account.learn(&[], std::slice::from_ref(&peek.chat));
            let (who, name, members) = chat_peer_and_title(&peek.chat)
                .ok_or_else(|| Fault::Net("Telegram returned an unreadable chat for this invite.".to_owned()))?;
            Ok(protocol::InvitePreview {
                code: hash,
                name,
                description: String::new(),
                icon: None,
                members,
                online: None,
                space_id: None,
                channel_id: Some(who.text()),
                request_needed: false,
            })
        }
    }
}

/// Joins a Telegram group or channel by invite link (`messages.importChatInvite`).
/// Returns `(None, Some(conversation_id), false)` when joined, or `(None, None, true)`
/// when Telegram returned `INVITE_REQUEST_SENT` (awaiting admin approval).
pub(super) fn invite_join(account: &Account, raw: &str) -> Result<(Option<String>, Option<String>, bool), Fault> {
    let hash = invite_hash(raw)?;
    match account.ask(&tl::functions::messages::ImportChatInvite { hash: hash.clone() }) {
        Ok(tl::enums::messages::ChatInviteJoinResult::Ok(joined)) => {
            super::learn_updates(account, &joined.updates);
            super::live::answered(account, &joined.updates);
            let chats = match &joined.updates {
                tl::enums::Updates::Updates(u) => u.chats.as_slice(),
                tl::enums::Updates::Combined(u) => u.chats.as_slice(),
                _ => &[],
            };
            let who = chats.iter().find_map(|c| chat_peer_and_title(c).map(|(w, ..)| w));
            if let Some(who) = who {
                sync::ensure_row(account, who, None);
                return Ok((None, Some(who.text()), false));
            }
            Ok((None, None, false))
        }
        Ok(tl::enums::messages::ChatInviteJoinResult::WebView(_)) => {
            Err(Fault::Net("This invite requires confirmation in Telegram's official app.".to_owned()))
        }
        Err(fault) if fault.is("INVITE_REQUEST_SENT") => Ok((None, None, true)),
        Err(fault) if fault.is("USER_ALREADY_PARTICIPANT") => {
            let preview = invite_preview(account, &hash)?;
            Ok((None, preview.channel_id, false))
        }
        Err(fault) if fault.is("INVITE_HASH_EXPIRED") || fault.is("INVITE_HASH_INVALID") || fault.is("INVITE_HASH_EMPTY") => {
            Err(Fault::Net("This invite link is invalid or has expired.".to_owned()))
        }
        Err(fault) => Err(fault),
    }
}

/// A stored conversation id for a channel or megagroup. A URL, a person, or a basic group is not one.
fn public_who(conversation: &str) -> Result<Who, Fault> {
    match Who::parse(conversation) {
        Some((who @ Who::Channel(_), None)) => Ok(who),
        _ => Err(invalid("PEER_ID_INVALID")),
    }
}

/// Shows a public channel or group the account has not joined. Reads one page and does not subscribe.
pub(super) fn preview_public(account: &Account, conversation: &str) -> Result<(), Fault> {
    let who = public_who(conversation)?;
    let recorded = if sync::member(account, who) {
        sync::ensure_row(account, who, None)
    } else {
        sync::ensure_preview_row(account, who)
    };
    if !recorded {
        return Err(invalid("CHANNEL_PRIVATE"));
    }
    sync::history(account, conversation, sync::Around::Newest);
    Ok(())
}

/// Joins one public channel or megagroup (`channels.joinChannel`). A search result stays a preview until this.
pub(super) fn join_public(account: &Account, conversation: &str) -> Result<(), Fault> {
    let who = public_who(conversation)?;
    if sync::member(account, who) {
        return sync::ensure_row(account, who, None).then_some(()).ok_or_else(|| invalid("CHANNEL_PRIVATE"));
    }
    let channel = account.input_channel(who)?;
    match account.ask(&tl::functions::channels::JoinChannel { channel: channel.clone() }) {
        Ok(tl::enums::messages::ChatInviteJoinResult::Ok(joined)) => {
            super::learn_updates(account, &joined.updates);
            super::live::answered(account, &joined.updates);
            if !sync::ensure_row(account, who, None) {
                return Err(invalid("CHANNEL_PRIVATE"));
            }
            similar(account, channel);
            Ok(())
        }
        Ok(tl::enums::messages::ChatInviteJoinResult::WebView(_)) => {
            Err(Fault::Net("This channel requires confirmation in Telegram's official app.".to_owned()))
        }
        Err(fault) if fault.is("INVITE_REQUEST_SENT") => Err(fault),
        Err(fault) if fault.is("USER_ALREADY_PARTICIPANT") => {
            if let Ok(found) = account.ask(&tl::functions::channels::GetChannels { id: vec![channel] }) {
                account.learn(&[], &found.chats());
            }
            if sync::ensure_row(account, who, None) { Ok(()) } else { Err(fault) }
        }
        Err(fault) => Err(fault),
    }
}

/// Channels Telegram suggests after a join. Each one is a preview until the user joins it.
fn similar(account: &Account, channel: tl::enums::InputChannel) {
    let found = match account.ask(&tl::functions::channels::GetChannelRecommendations { channel: Some(channel) }) {
        Ok(found) => found,
        Err(fault) => {
            tracing::warn!("chatd: Telegram: similar channels: {fault}");
            return;
        }
    };
    let chats = found.chats();
    account.learn(&[], &chats);
    for chat in &chats {
        let Some((who, ..)) = chat_peer_and_title(chat) else { continue };
        if sync::member(account, who) {
            sync::ensure_row(account, who, None);
        } else {
            sync::ensure_preview_row(account, who);
        }
    }
}

/// Lists exported invite links for a chat (`messages.getExportedChatInvites`).
pub(super) fn chat_invites(account: &Account, conversation: &str, revoked: bool) -> Result<Vec<protocol::ChatInviteLink>, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let tl::enums::messages::ExportedChatInvites::Invites(found) = account.ask(&tl::functions::messages::GetExportedChatInvites {
        revoked,
        peer,
        admin_id: tl::enums::InputUser::UserSelf,
        offset_date: None,
        offset_link: None,
        limit: 50,
    })?;
    account.learn(&found.users, &[]);
    Ok(found
        .invites
        .into_iter()
        .filter_map(|inv| match inv {
            tl::enums::ExportedChatInvite::ChatInviteExported(exp) => Some(map_exported_invite(exp)),
            _ => None,
        })
        .collect())
}

/// Creates a new invite link for a chat (`messages.exportChatInvite`).
pub(super) fn create_invite(
    account: &Account,
    conversation: &str,
    title: Option<String>,
    expire_date: Option<i64>,
    usage_limit: Option<u32>,
    request_needed: bool,
) -> Result<protocol::ChatInviteLink, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let usage_limit = if request_needed { None } else { usage_limit.and_then(|n| i32::try_from(n).ok()).filter(|&n| n > 0) };
    let exported = account.ask(&tl::functions::messages::ExportChatInvite {
        legacy_revoke_permanent: false,
        request_needed,
        peer,
        expire_date: expire_date.and_then(|t| i32::try_from(t).ok()).filter(|&t| t > 0),
        usage_limit,
        title: title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()),
        subscription_pricing: None,
    })?;
    match exported {
        tl::enums::ExportedChatInvite::ChatInviteExported(exp) => Ok(map_exported_invite(exp)),
        _ => Err(Fault::Net("Telegram did not return an invite link.".to_owned())),
    }
}

/// Revokes an existing invite link (`messages.editExportedChatInvite { revoked: true }`).
pub(super) fn revoke_invite(account: &Account, conversation: &str, link: &str) -> Result<protocol::ChatInviteLink, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let edited = account.ask(&tl::functions::messages::EditExportedChatInvite {
        revoked: true,
        peer,
        link: link.to_owned(),
        expire_date: None,
        usage_limit: None,
        request_needed: None,
        title: None,
    })?;
    let invite = match edited {
        tl::enums::messages::ExportedChatInvite::Invite(one) => {
            account.learn(&one.users, &[]);
            one.invite
        }
        tl::enums::messages::ExportedChatInvite::Replaced(rep) => {
            account.learn(&rep.users, &[]);
            rep.invite
        }
    };
    match invite {
        tl::enums::ExportedChatInvite::ChatInviteExported(exp) => Ok(map_exported_invite(exp)),
        _ => Err(Fault::Net("Telegram did not return the revoked invite link.".to_owned())),
    }
}

/// Lists pending join requests for a chat (`messages.getChatInviteImporters { requested: true }`).
pub(super) fn join_requests(
    account: &Account,
    conversation: &str,
    link: Option<String>,
    limit: u32,
) -> Result<Vec<protocol::JoinRequest>, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let tl::enums::messages::ChatInviteImporters::Importers(found) =
        account.ask(&tl::functions::messages::GetChatInviteImporters {
            requested: true,
            subscription_expired: false,
            peer,
            link,
            q: None,
            offset_date: 0,
            offset_user: tl::enums::InputUser::Empty,
            limit: limit.clamp(1, 100) as i32,
        })?;
    account.learn(&found.users, &[]);
    let dir = account.dir();
    Ok(found
        .importers
        .into_iter()
        .map(|imp| {
            let tl::enums::ChatInviteImporter::Importer(imp) = imp;
            let user_who = Who::User(imp.user_id);
            let known = dir.get(user_who);
            protocol::JoinRequest {
                user_id: user_who.text(),
                name: dir.name(user_who),
                handle: known.map(|k| k.handle.clone()).unwrap_or_default(),
                about: imp.about.unwrap_or_default(),
                date: i64::from(imp.date),
            }
        })
        .collect())
}

/// Approves or declines one (`user_id: Some`) or all (`user_id: None`) pending join requests.
pub(super) fn handle_join_request(
    account: &Account,
    conversation: &str,
    user_id: Option<&str>,
    approved: bool,
    link: Option<String>,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let answer = match user_id {
        Some(raw) => {
            let Some((user_who @ Who::User(_), None)) = Who::parse(raw) else {
                return Err(invalid("USER_ID_INVALID"));
            };
            account.input(user_who)?;
            let input_user = account.dir().input_user(user_who).ok_or_else(|| invalid("USER_ID_INVALID"))?;
            account.ask(&tl::functions::messages::HideChatJoinRequest {
                approved,
                peer,
                user_id: input_user,
            })?
        }
        None => account.ask(&tl::functions::messages::HideAllChatJoinRequests { approved, peer, link })?,
    };
    super::learn_updates(account, &answer);
    super::live::answered(account, &answer);
    if user_id.is_none() {
        account.live().pending_requests.insert(who, 0);
    } else if let Some(count) = account.live().pending_requests.get_mut(&who) {
        *count = count.saturating_sub(1);
    }
    Ok(())
}

/// Per-conversation send rules: merges `default_banned_rights` and personal `banned_rights`,
/// slow mode from `channels.getFullChannel`, and `send_paid_messages_stars` from channel or user.
pub(super) fn send_rules(account: &Account, conversation: &str) -> Result<protocol::SendRules, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let now = crate::hub::now();
    match who {
        Who::User(id) if id == account.me || id == SERVICE => Ok(protocol::SendRules::default()),
        Who::User(_) => {
            let id = account.dir().input_user(who).ok_or_else(|| invalid("USER_ID_INVALID"))?;
            let tl::enums::users::UserFull::Full(full) = account.ask(&tl::functions::users::GetFullUser { id })?;
            account.learn(&full.users, &full.chats);
            let tl::enums::UserFull::Full(user) = full.full_user;
            let mut rules = protocol::SendRules::default();
            if user.voice_messages_forbidden {
                rules.send_voices = false;
            }
            if let Some(stars) = user.send_paid_messages_stars.filter(|&s| s > 0) {
                rules.paid_stars = Some(stars as u64);
            }
            Ok(rules)
        }
        Who::Group(chat_id) => {
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
            account.learn(&full.users, &full.chats);
            let known = account.dir().get(who).cloned().unwrap_or_default();
            Ok(known.send_rules(now, 0, None, None))
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::channels::GetFullChannel { channel })?;
            account.learn(&full.users, &full.chats);
            let (slowmode_seconds, slowmode_next_send, paid_stars) = match full.full_chat {
                tl::enums::ChatFull::ChannelFull(channel) => (
                    channel.slowmode_seconds.filter(|&s| s > 0).map_or(0, |s| s as u32),
                    channel.slowmode_next_send_date.map(i64::from).filter(|&ts| ts > now),
                    channel.send_paid_messages_stars.filter(|&s| s > 0).map(|s| s as u64),
                ),
                tl::enums::ChatFull::Full(_) => (0, None, None),
            };
            let known = account.dir().get(who).cloned().unwrap_or_default();
            Ok(known.send_rules(now, slowmode_seconds, slowmode_next_send, paid_stars))
        }
    }
}

/// Sets or clears Telegram's auto-delete timer (`messages.setHistoryTTL`).
pub(super) fn set_disappearing(account: &Account, conversation: &str, seconds: u32) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let answer = account.ask(&tl::functions::messages::SetHistoryTtl { peer, period: seconds as i32 })?;
    let timer = (seconds > 0).then_some(seconds);
    let _ = account.ctx.disappearing(conversation, timer);
    super::live::answered(account, &answer);
    let has_update = match &answer {
        tl::enums::Updates::Updates(all) => !all.updates.is_empty(),
        tl::enums::Updates::Combined(all) => !all.updates.is_empty(),
        tl::enums::Updates::UpdateShort(_) => true,
        _ => false,
    };
    if !has_update {
        let line = if seconds == 0 {
            "You turned the auto-delete timer off".to_owned()
        } else {
            format!("You set messages to auto-delete after {}", protocol::format_disappearing(seconds))
        };
        let now = crate::hub::now();
        let mut msg = Message::plain(format!("tg-ttl-{now}"), String::new(), line, now, false);
        msg.kind = protocol::MessageKind::System;
        let _ = account.ctx.message(conversation, &msg);
    }
    Ok(())
}

/// Reads Telegram's default auto-delete timer for new chats (`messages.getDefaultHistoryTTL`).
pub(super) fn get_default_disappearing(account: &Account) -> Result<u32, Fault> {
    let tl::enums::DefaultHistoryTtl::Ttl(ttl) = account.ask(&tl::functions::messages::GetDefaultHistoryTtl {})?;
    Ok(ttl.period.max(0) as u32)
}

/// Sets Telegram's default auto-delete timer for new chats (`messages.setDefaultHistoryTTL`).
pub(super) fn set_default_disappearing(account: &Account, seconds: u32) -> Result<(), Fault> {
    account.ask(&tl::functions::messages::SetDefaultHistoryTtl { period: seconds as i32 })?;
    Ok(())
}

fn parse_scheduled_id(raw: &str) -> Result<i32, Fault> {
    raw.strip_prefix('s')
        .unwrap_or(raw)
        .parse::<i32>()
        .map_err(|_| invalid("MESSAGE_ID_INVALID"))
}

/// Lists pending scheduled messages in one conversation (`messages.getScheduledHistory`).
pub(super) fn scheduled_list(account: &Account, conversation: &str) -> Result<Vec<protocol::ScheduledMessage>, Fault> {
    let (who, topic) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let answer = account.ask(&tl::functions::messages::GetScheduledHistory { peer, hash: 0 })?;
    let messages = match answer {
        tl::enums::messages::Messages::Messages(all) => all.messages,
        tl::enums::messages::Messages::Slice(all) => all.messages,
        tl::enums::messages::Messages::ChannelMessages(all) => all.messages,
        tl::enums::messages::Messages::NotModified(_) => Vec::new(),
    };
    let conv_key = protocol::Key::new(&account.ctx.account, conversation);
    let mut out: Vec<protocol::ScheduledMessage> = messages
        .into_iter()
        .filter_map(|raw| {
            let tl::enums::Message::Message(msg) = raw else {
                return None;
            };
            if let Some(wanted_topic) = topic {
                let msg_topic = msg.reply_to.as_ref().and_then(|r| match r {
                    tl::enums::MessageReplyHeader::Header(h) if h.forum_topic => h.reply_to_top_id.or(h.reply_to_msg_id),
                    _ => None,
                });
                if msg_topic != Some(wanted_topic) {
                    return None;
                }
            }
            Some(protocol::ScheduledMessage {
                id: msg.id.to_string(),
                conversation: conv_key.clone(),
                text: msg.message,
                post_at: i64::from(msg.date),
                created: 0,
            })
        })
        .collect();
    out.sort_by_key(|m| m.post_at);
    Ok(out)
}

/// Sends a pending scheduled message immediately (`messages.sendScheduledMessages`).
pub(super) fn send_scheduled_now(account: &Account, conversation: &str, scheduled_id: &str) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let id = parse_scheduled_id(scheduled_id)?;
    let peer = account.input(who)?;
    let answer = account.ask(&tl::functions::messages::SendScheduledMessages { peer, id: vec![id] })?;
    let _ = account.ctx.deleted(conversation, &format!("s{id}"));
    super::live::answered(account, &answer);
    Ok(())
}

/// Cancels a pending scheduled message (`messages.deleteScheduledMessages`).
pub(super) fn cancel_scheduled(account: &Account, conversation: &str, scheduled_id: &str) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let id = parse_scheduled_id(scheduled_id)?;
    let peer = account.input(who)?;
    let answer = account.ask(&tl::functions::messages::DeleteScheduledMessages { peer, id: vec![id] })?;
    let _ = account.ctx.deleted(conversation, &format!("s{id}"));
    super::live::answered(account, &answer);
    Ok(())
}

/// Deletes all messages from one participant in a supergroup/channel (`channels.deleteParticipantHistory`).
pub(super) fn delete_participant_history(account: &Account, conversation: &str, person_id: &str) -> Result<(), Fault> {
    let (who @ Who::Channel(_), _) = Who::parse(conversation).ok_or_else(|| invalid("CHANNEL_INVALID"))? else {
        return Err(invalid("CHANNEL_INVALID"));
    };
    let (person, _) = Who::parse(person_id).ok_or_else(|| invalid("USER_ID_INVALID"))?;
    let allowed = matches!(person, Who::User(_))
        && account.ctx.known(&who.text()).ok().flatten().is_some_and(|row| row.rights.delete_others);
    if !allowed {
        return Err(invalid("CHAT_ADMIN_REQUIRED"));
    }
    let channel = account.input_channel(who)?;
    let participant = account.input(person)?;
    for _ in 0..64 {
        let tl::enums::messages::AffectedHistory::History(affected) =
            account.ask(&tl::functions::channels::DeleteParticipantHistory {
                channel: channel.clone(),
                participant: participant.clone(),
            })?;
        if affected.offset == 0 {
            break;
        }
    }
    let _ = account.ctx.clear_sender_messages(conversation, person_id);
    Ok(())
}

/// Reports specific messages from a participant in a supergroup/channel as spam (`channels.reportSpam`).
pub(super) fn report_spam_messages(
    account: &Account,
    conversation: &str,
    person_id: &str,
    message_ids: &[String],
) -> Result<(), Fault> {
    let (who @ Who::Channel(_), _) = Who::parse(conversation).ok_or_else(|| invalid("CHANNEL_INVALID"))? else {
        return Err(invalid("CHANNEL_INVALID"));
    };
    let (person, _) = Who::parse(person_id).ok_or_else(|| invalid("USER_ID_INVALID"))?;
    if message_ids.is_empty() {
        return Err(invalid("MESSAGE_ID_INVALID"));
    }
    let mut ids = Vec::with_capacity(message_ids.len());
    for raw in message_ids {
        ids.push(raw.parse::<i32>().map_err(|_| invalid("MESSAGE_ID_INVALID"))?);
    }
    let allowed = matches!(person, Who::User(_))
        && account.ctx.known(&who.text()).ok().flatten().is_some_and(|row| row.rights.delete_others);
    if !allowed {
        return Err(invalid("CHAT_ADMIN_REQUIRED"));
    }
    let channel = account.input_channel(who)?;
    let participant = account.input(person)?;
    for chunk in ids.chunks(100) {
        account.ask(&tl::functions::channels::ReportSpam {
            channel: channel.clone(),
            participant: participant.clone(),
            id: chunk.to_vec(),
        })?;
    }
    Ok(())
}

/// Edits and/or reschedules a pending scheduled message (`messages.editMessage` with `schedule_date`).
pub(super) fn reschedule(
    account: &Account,
    conversation: &str,
    scheduled_id: &str,
    text: Option<&str>,
    post_at: i64,
) -> Result<protocol::ScheduledMessage, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    if post_at == protocol::SCHEDULE_WHEN_ONLINE {
        super::check_schedule_target(
            account,
            who,
            super::Options { schedule: Some(protocol::SCHEDULE_WHEN_ONLINE as i32), ..super::Options::default() },
        )?;
    }
    let id = parse_scheduled_id(scheduled_id)?;
    let peer = account.input(who)?;
    let schedule_date = i32::try_from(post_at).map_err(|_| invalid("SCHEDULE_DATE_INVALID"))?;
    let (plain, entities) = match text {
        Some(t) => {
            let (p, e) = super::prepared(account, conversation, t);
            (Some(p), e)
        }
        None => (None, None),
    };
    let answer = account.ask(&tl::functions::messages::EditMessage {
        no_webpage: false,
        invert_media: false,
        peer,
        id,
        message: plain.clone(),
        media: None,
        reply_markup: None,
        entities,
        schedule_date: Some(schedule_date),
        schedule_repeat_period: None,
        quick_reply_shortcut_id: None,
        rich_message: None,
    })?;
    super::live::answered(account, &answer);
    let final_text = plain.unwrap_or_default();
    Ok(protocol::ScheduledMessage {
        id: id.to_string(),
        conversation: protocol::Key::new(&account.ctx.account, conversation),
        text: final_text,
        post_at,
        created: 0,
    })
}

/// Parses `"YYYY-MM-DD"`, `"MM-DD"`, `"--MM-DD"`, or empty string (clear) into Telegram's `Birthday`.
pub(super) fn parse_birthday(raw: &str) -> Result<Option<tl::enums::Birthday>, Fault> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let stripped = trimmed.strip_prefix("--").unwrap_or(trimmed);
    let parts: Vec<&str> = stripped.split('-').collect();
    let (year, month, day) = match parts.as_slice() {
        [y, m, d] => {
            let y: i32 = y.parse().map_err(|_| Fault::Net("Enter birthday as YYYY-MM-DD or MM-DD.".to_owned()))?;
            let m: i32 = m.parse().map_err(|_| Fault::Net("Enter birthday as YYYY-MM-DD or MM-DD.".to_owned()))?;
            let d: i32 = d.parse().map_err(|_| Fault::Net("Enter birthday as YYYY-MM-DD or MM-DD.".to_owned()))?;
            if !(1900..=2100).contains(&y) {
                return Err(Fault::Net("Birthday year must be between 1900 and 2100.".to_owned()));
            }
            (Some(y), m, d)
        }
        [m, d] => {
            let m: i32 = m.parse().map_err(|_| Fault::Net("Enter birthday as YYYY-MM-DD or MM-DD.".to_owned()))?;
            let d: i32 = d.parse().map_err(|_| Fault::Net("Enter birthday as YYYY-MM-DD or MM-DD.".to_owned()))?;
            (None, m, d)
        }
        _ => return Err(Fault::Net("Enter birthday as YYYY-MM-DD or MM-DD.".to_owned())),
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(Fault::Net("Enter a valid calendar date for birthday.".to_owned()));
    }
    Ok(Some(tl::types::Birthday { day, month, year }.into()))
}

/// Parses a custom emoji status specifier (`""` clears; `"5002"` or `"e5002"` sets by document id).
pub(super) fn parse_emoji_status(raw: &str, until: Option<i64>) -> Result<tl::enums::EmojiStatus, Fault> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(tl::enums::EmojiStatus::Empty);
    }
    let digits = trimmed.strip_prefix('e').unwrap_or(trimmed);
    let document_id = digits
        .parse::<i64>()
        .map_err(|_| Fault::Net("Choose a custom emoji id for your Telegram emoji status.".to_owned()))?;
    let until = until.and_then(|ts| i32::try_from(ts).ok()).filter(|&ts| ts > 0);
    Ok(tl::types::EmojiStatus { document_id, until }.into())
}

fn refresh_own_identity(account: &Account) {
    let (name, premium) = {
        let dir = account.dir();
        (dir.name(Who::User(account.me)), dir.get(Who::User(account.me)).is_some_and(|known| known.premium))
    };
    if !name.is_empty() {
        let shown = if premium { format!("{name} · Premium") } else { name };
        let _ = account.ctx.describe(&shown, &Who::User(account.me).text());
    }
}

/// Updates our own Telegram profile fields: name, bio, username, birthday, and/or emoji status.
pub(super) fn set_profile(
    account: &Account,
    name: Option<String>,
    about: Option<String>,
    username: Option<String>,
    emoji: Option<String>,
    birthday: Option<String>,
) -> Result<(), Fault> {
    if name.is_some() || about.is_some() {
        let (first_name, last_name) = match name {
            Some(ref full) => {
                let trimmed = full.trim();
                if trimmed.is_empty() {
                    return Err(Fault::Net("First name cannot be empty.".to_owned()));
                }
                let (first, last) = super::split_name(trimmed);
                (Some(first), Some(last))
            }
            None => (None, None),
        };
        let updated = account.ask(&tl::functions::account::UpdateProfile {
            first_name,
            last_name,
            about: about.map(|s| s.trim().to_owned()),
        })?;
        account.learn(&[updated], &[]);
        refresh_own_identity(account);
    }
    if let Some(raw_username) = username {
        let bare = raw_username.trim().trim_start_matches('@').to_owned();
        if !bare.is_empty() {
            let valid = (5..=32).contains(&bare.len()) && bare.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
            if !valid {
                return Err(Fault::Net("Telegram usernames must be 5–32 letters, digits, or underscores.".to_owned()));
            }
            let available = account.ask(&tl::functions::account::CheckUsername { username: bare.clone() })?;
            if !available {
                return Err(Fault::Net(format!("@{bare} is already taken or unavailable.")));
            }
        }
        let updated = account.ask(&tl::functions::account::UpdateUsername { username: bare })?;
        account.learn(&[updated], &[]);
        refresh_own_identity(account);
    }
    if let Some(raw_birthday) = birthday {
        let parsed = parse_birthday(&raw_birthday)?;
        account.ask(&tl::functions::account::UpdateBirthday { birthday: parsed })?;
    }
    if let Some(raw_emoji) = emoji {
        set_emoji_status(account, &raw_emoji, None)?;
    }
    Ok(())
}

/// Sets or clears our Telegram Premium emoji status (`account.updateEmojiStatus`).
/// Never calls `account.updateStatus { offline: true }`.
pub(super) fn set_emoji_status(account: &Account, raw: &str, until: Option<i64>) -> Result<(), Fault> {
    let status = parse_emoji_status(raw, until)?;
    match account.ask(&tl::functions::account::UpdateEmojiStatus { emoji_status: status }) {
        Ok(_) => Ok(()),
        Err(fault) if fault.is("PREMIUM_ACCOUNT_REQUIRED") => {
            Err(Fault::Net("Setting an emoji status requires Telegram Premium.".to_owned()))
        }
        Err(fault) => Err(fault),
    }
}

/// Uploads a new profile photo (`photos.uploadProfilePhoto`) or deletes the current one
/// (`photos.getUserPhotos` + `photos.deletePhotos`) when `path` is empty.
pub(super) fn set_avatar(account: &Account, path: &str) -> Result<(), Fault> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        let listed = account.ask(&tl::functions::photos::GetUserPhotos {
            user_id: tl::enums::InputUser::UserSelf,
            offset: 0,
            max_id: 0,
            limit: 1,
        })?;
        let photos = match listed {
            tl::enums::photos::Photos::Photos(p) => p.photos,
            tl::enums::photos::Photos::Slice(p) => p.photos,
        };
        if let Some(tl::enums::Photo::Photo(first)) = photos.into_iter().next() {
            let input: tl::enums::InputPhoto = tl::types::InputPhoto {
                id: first.id,
                access_hash: first.access_hash,
                file_reference: first.file_reference,
            }
            .into();
            account.ask(&tl::functions::photos::DeletePhotos { id: vec![input] })?;
        }
        return Ok(());
    }
    let file = super::send::upload_input_file(account, "own-avatar", trimmed).map_err(Fault::Net)?;
    let tl::enums::photos::Photo::Photo(saved) = account.ask(&tl::functions::photos::UploadProfilePhoto {
        fallback: false,
        bot: None,
        file: Some(file),
        video: None,
        video_start_ts: None,
        video_emoji_markup: None,
    })?;
    account.learn(&saved.users, &[]);
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

fn shared_filter(kind: SharedKind) -> tl::enums::MessagesFilter {
    match kind {
        SharedKind::Media => tl::enums::MessagesFilter::InputMessagesFilterPhotoVideo,
        SharedKind::Files => tl::enums::MessagesFilter::InputMessagesFilterDocument,
        SharedKind::Links => tl::enums::MessagesFilter::InputMessagesFilterUrl,
        SharedKind::Music => tl::enums::MessagesFilter::InputMessagesFilterMusic,
        SharedKind::Voice => tl::enums::MessagesFilter::InputMessagesFilterRoundVoice,
        SharedKind::Gifs => tl::enums::MessagesFilter::InputMessagesFilterGif,
    }
}

fn search_filter(filters: &protocol::SearchFilters) -> tl::enums::MessagesFilter {
    if filters.mentions_me {
        return tl::enums::MessagesFilter::InputMessagesFilterMyMentions;
    }
    match filters.has {
        Some(protocol::SearchHas::Images) => tl::enums::MessagesFilter::InputMessagesFilterPhotoVideo,
        Some(protocol::SearchHas::Files) => tl::enums::MessagesFilter::InputMessagesFilterDocument,
        Some(protocol::SearchHas::Links) => tl::enums::MessagesFilter::InputMessagesFilterUrl,
        None => tl::enums::MessagesFilter::InputMessagesFilterEmpty,
    }
}

fn search_filtered(
    account: &Account,
    conversation: &str,
    query: &str,
    filter: tl::enums::MessagesFilter,
    from_id: Option<tl::enums::InputPeer>,
    min_date: i32,
    max_date: i32,
    offset_id: i32,
    limit: i32,
) -> Result<(Vec<Message>, Option<u32>), Fault> {
    let (who, topic) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let request = tl::functions::messages::Search {
        peer: account.input(who)?,
        q: query.chars().take(1024).collect(),
        from_id,
        saved_peer_id: None,
        saved_reaction: None,
        top_msg_id: topic.filter(|topic| *topic != 1),
        filter,
        min_date,
        max_date,
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

fn search(account: &Account, conversation: &str, query: &str, filter: tl::enums::MessagesFilter, offset_id: i32, limit: i32) -> Result<(Vec<Message>, Option<u32>), Fault> {
    search_filtered(account, conversation, query, filter, None, 0, 0, offset_id, limit)
}

/// Text/filtered matches from Telegram in one conversation (or forum topic), paged by `offset_id`
/// via `cursor` with optional sender (`from_id`), date range (`min_date`/`max_date`), and media filter.
pub(super) fn search_messages(
    account: &Account,
    conversation: &str,
    query: &str,
    filters: &protocol::SearchFilters,
    cursor: Option<&str>,
) -> Result<crate::adapters::SearchPage, Fault> {
    let from_id = match &filters.from {
        Some(person) => {
            let (who, _) = Who::parse(&person.id).ok_or_else(|| invalid("USER_ID_INVALID"))?;
            Some(account.input(who)?)
        }
        None => None,
    };
    let min_date = filters.after.and_then(|d| i32::try_from(d).ok()).unwrap_or(0);
    let max_date = filters.before.and_then(|d| i32::try_from(d).ok()).unwrap_or(0);
    let offset_id = cursor.and_then(|c| c.parse::<i32>().ok()).unwrap_or(0);
    let filter = search_filter(filters);
    let (messages, total) = search_filtered(account, conversation, query, filter, from_id, min_date, max_date, offset_id, 100)?;
    let known = account.ctx.known(conversation).ok().flatten();
    let hits: Vec<protocol::MessageHit> = messages.into_iter().take(100).map(|message| protocol::MessageHit {
        conversation: account.ctx.key(conversation),
        message_id: message.id,
        sent: message.sent,
        sender: message.sender.name,
        snippet: crate::store::snippet(&message.text, query),
        title: known.as_ref().map(|row| row.title.clone()).unwrap_or_default(),
        space_title: known.as_ref().map(|row| row.space_title.clone()).unwrap_or_default(),
        outgoing: message.outgoing,
    }).collect();
    let next_cursor = (hits.len() >= 100 && total.is_none_or(|t| t as usize > hits.len()))
        .then(|| hits.last().map(|h| h.message_id.clone()))
        .flatten();
    Ok(crate::adapters::SearchPage { hits, cursor: next_cursor, total })
}

/// Jumps to the message closest to `date` (Unix seconds) via `messages.getHistory` with `offset_date`.
pub(super) fn jump_to_date(account: &Account, conversation: &str, date: i64) -> Result<Option<String>, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let offset_date = i32::try_from(date).map_err(|_| invalid("DATE_INVALID"))?;
    let request = tl::functions::messages::GetHistory {
        peer,
        offset_id: 0,
        offset_date,
        add_offset: -1,
        limit: 1,
        max_id: 0,
        min_id: 0,
        hash: 0,
    };
    let (raw, _) = sync::searched(account, account.ask(&request)?);
    let (messages, wants) = sync::mapped(account, conversation, &raw);
    account.files.want(wants);
    if !messages.is_empty() {
        let _ = account.ctx.history(conversation, &messages);
    }
    Ok(messages.into_iter().next().map(|m| m.id))
}

/// Returns calendar day buckets for a conversation (`messages.getSearchResultsCalendar`).
pub(super) fn search_calendar(
    account: &Account,
    conversation: &str,
    kind: Option<SharedKind>,
    offset_date: i64,
) -> Result<Vec<protocol::SearchCalendarDay>, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let filter = kind.map_or(tl::enums::MessagesFilter::InputMessagesFilterEmpty, shared_filter);
    let tl::enums::messages::SearchResultsCalendar::Calendar(cal) = account.ask(
        &tl::functions::messages::GetSearchResultsCalendar {
            peer,
            saved_peer_id: None,
            filter,
            offset_id: 0,
            offset_date: i32::try_from(offset_date).unwrap_or(0),
        },
    )?;
    account.learn(&cal.users, &cal.chats);
    Ok(cal
        .periods
        .into_iter()
        .map(|tl::enums::SearchResultsCalendarPeriod::Period(p)| protocol::SearchCalendarDay {
            date: i64::from(p.date),
            min_message_id: p.min_msg_id.to_string(),
            max_message_id: p.max_msg_id.to_string(),
            count: p.count.max(0) as u32,
        })
        .collect())
}

/// Parses an optional global search cursor `"<offset_rate>:<peer>:<offset_id>"`.
fn parse_global_cursor(cursor: Option<&str>) -> (i32, Option<Who>, i32) {
    let Some(cursor) = cursor else {
        return (0, None, 0);
    };
    let mut parts = cursor.split(':');
    let rate = parts.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
    let peer = parts.next().and_then(|s| Who::parse(s).map(|(w, _)| w));
    let id = parts.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
    (rate, peer, id)
}

/// Account-wide text search across all Telegram chats (`messages.searchGlobal`), bounded to 100 hits
/// with pagination via `(offset_rate, offset_peer, offset_id)`.
pub(super) fn search_global(
    account: &Account,
    query: &str,
    filters: &protocol::SearchFilters,
    cursor: Option<&str>,
) -> Result<crate::adapters::SearchPage, Fault> {
    const LIMIT: usize = 100;
    const MAX_PAGES: usize = 4;
    let q: String = query.chars().take(1024).collect();
    let min_date = filters.after.and_then(|d| i32::try_from(d).ok()).unwrap_or(0);
    let max_date = filters.before.and_then(|d| i32::try_from(d).ok()).unwrap_or(0);
    let (mut offset_rate, initial_peer, mut offset_id) = parse_global_cursor(cursor);
    let mut offset_peer = initial_peer
        .and_then(|who| account.input(who).ok())
        .unwrap_or(tl::enums::InputPeer::Empty);

    let mut hits = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for _ in 0..MAX_PAGES {
        if hits.len() >= LIMIT {
            break;
        }
        let want = (LIMIT - hits.len()).min(100) as i32;
        let request = tl::functions::messages::SearchGlobal {
            broadcasts_only: false,
            groups_only: false,
            users_only: false,
            folder_id: None,
            q: q.clone(),
            filter: search_filter(filters),
            min_date,
            max_date,
            offset_rate,
            offset_peer: offset_peer.clone(),
            offset_id,
            limit: want,
        };
        let answer = account.ask(&request)?;
        let next_rate = match &answer {
            tl::enums::messages::Messages::Slice(slice) => slice.next_rate,
            _ => None,
        };
        let (raw, _) = sync::searched(account, answer);
        if raw.is_empty() {
            break;
        }
        let mut last_peer_id: Option<(Who, i32)> = None;
        for item in &raw {
            let mapped = {
                let dir = account.dir();
                super::content::map(&dir, item)
            };
            let Some(mapped) = mapped else {
                continue;
            };
            last_peer_id = Some((mapped.peer, mapped.id));
            let is_forum = account.dir().get(mapped.peer).is_some_and(|known| known.forum);
            let conv_id = match (is_forum, mapped.topic) {
                (true, Some(topic)) => peers::thread_id(mapped.peer, topic),
                _ => mapped.peer.text(),
            };
            if !seen.insert((conv_id.clone(), mapped.message.id.clone())) {
                continue;
            }
            let known_row = account.ctx.known(&conv_id).ok().flatten();
            let title = known_row
                .as_ref()
                .map(|row| row.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| account.dir().name(mapped.peer));
            let space_title = known_row.as_ref().map(|row| row.space_title.clone()).unwrap_or_default();
            hits.push(protocol::MessageHit {
                conversation: account.ctx.key(&conv_id),
                message_id: mapped.message.id,
                sent: mapped.message.sent,
                sender: mapped.message.sender.name,
                snippet: crate::store::snippet(&mapped.message.text, query),
                title,
                space_title,
                outgoing: mapped.message.outgoing,
            });
            if hits.len() >= LIMIT {
                break;
            }
        }
        match (next_rate, last_peer_id) {
            (Some(rate), Some((peer, id))) if rate != 0 && (rate != offset_rate || id != offset_id) => {
                offset_rate = rate;
                offset_id = id;
                offset_peer = account.input(peer).unwrap_or(tl::enums::InputPeer::Empty);
            }
            _ => break,
        }
    }
    Ok(hits.into())
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
    if account.dir().get(from_who).is_some_and(|known| known.noforwards) {
        return Err(invalid("CHAT_FORWARDS_RESTRICTED"));
    }
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
            let (to_who, top_msg_id) = if matches!(target.as_str(), "self" | "saved") {
                (Who::User(account.me), None)
            } else {
                Who::parse(target).ok_or_else(|| invalid("PEER_ID_INVALID"))?
            };
            let to_peer = if to_who == Who::User(account.me) {
                tl::enums::InputPeer::PeerSelf
            } else {
                account.input(to_who)?
            };
            let random_id: Vec<i64> = ids
                .iter()
                .enumerate()
                .map(|(m_ix, _)| base.wrapping_add(((t_ix as i64) << 16) + m_ix as i64 + 1))
                .collect();
            let answer = account.ask(&tl::functions::messages::ForwardMessages {
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
            super::live::answered(account, &answer);
            if !note.is_empty() {
                let note_answer = account.ask(&tl::functions::messages::SendMessage {
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
                super::live::answered(account, &note_answer);
            }
            Ok(())
        })();
        results.push((target.clone(), res.map_err(|e| e.words())));
    }
    Ok(results)
}

/// What was shared, from Telegram's own search, with Telegram's count of it as the total.
pub(super) fn shared(account: &Account, conversation: &str, kind: SharedKind, before: Option<&str>, limit: u32) -> Result<SharedPage, Fault> {
    let filter = shared_filter(kind);
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
            SharedKind::Media | SharedKind::Files | SharedKind::Music | SharedKind::Voice | SharedKind::Gifs => {
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

fn tl_privacy_key(key: protocol::PrivacyKey) -> tl::enums::InputPrivacyKey {
    match key {
        protocol::PrivacyKey::PhoneCall => tl::enums::InputPrivacyKey::PhoneCall,
        protocol::PrivacyKey::PhoneP2p => tl::enums::InputPrivacyKey::PhoneP2P,
        protocol::PrivacyKey::VoiceMessages => tl::enums::InputPrivacyKey::VoiceMessages,
        protocol::PrivacyKey::LastSeen => tl::enums::InputPrivacyKey::StatusTimestamp,
        protocol::PrivacyKey::PhoneNumber => tl::enums::InputPrivacyKey::PhoneNumber,
        protocol::PrivacyKey::ProfilePhoto => tl::enums::InputPrivacyKey::ProfilePhoto,
        protocol::PrivacyKey::ChatInvite => tl::enums::InputPrivacyKey::ChatInvite,
    }
}

fn map_privacy_rules(
    account: &Account,
    key: protocol::PrivacyKey,
    rules: &[tl::enums::PrivacyRule],
) -> protocol::PrivacyPolicy {
    use protocol::{PrivacyAudience, PrivacyException, PrivacyPolicy};
    use tl::enums::PrivacyRule as R;
    let dir = account.dir();
    let mut audience = PrivacyAudience::Nobody;
    let mut always_allow = Vec::new();
    let mut never_allow = Vec::new();
    for rule in rules {
        match rule {
            R::PrivacyValueAllowAll => audience = PrivacyAudience::Everybody,
            R::PrivacyValueAllowContacts => audience = PrivacyAudience::Contacts,
            R::PrivacyValueDisallowAll => audience = PrivacyAudience::Nobody,
            R::PrivacyValueAllowUsers(u) => {
                for id in &u.users {
                    let who = Who::User(*id);
                    always_allow.push(PrivacyException { id: who.text(), name: dir.name(who), chat: false });
                }
            }
            R::PrivacyValueAllowChatParticipants(c) => {
                for id in &c.chats {
                    let who = if dir.get(Who::Channel(*id)).is_some() { Who::Channel(*id) } else { Who::Group(*id) };
                    always_allow.push(PrivacyException { id: who.text(), name: dir.name(who), chat: true });
                }
            }
            R::PrivacyValueDisallowUsers(u) => {
                for id in &u.users {
                    let who = Who::User(*id);
                    never_allow.push(PrivacyException { id: who.text(), name: dir.name(who), chat: false });
                }
            }
            R::PrivacyValueDisallowChatParticipants(c) => {
                for id in &c.chats {
                    let who = if dir.get(Who::Channel(*id)).is_some() { Who::Channel(*id) } else { Who::Group(*id) };
                    never_allow.push(PrivacyException { id: who.text(), name: dir.name(who), chat: true });
                }
            }
            _ => {}
        }
    }
    let is_premium = dir.get(Who::User(account.me)).is_some_and(|k| k.premium);
    let premium_only = key == protocol::PrivacyKey::VoiceMessages && !is_premium;
    PrivacyPolicy { audience, always_allow, never_allow, premium_only }
}

/// Reads an account privacy rule list (`account.getPrivacy`).
pub(super) fn get_privacy(account: &Account, key: protocol::PrivacyKey) -> Result<protocol::PrivacyPolicy, Fault> {
    let tl::enums::account::PrivacyRules::Rules(got) =
        account.ask(&tl::functions::account::GetPrivacy { key: tl_privacy_key(key) })?;
    account.learn(&got.users, &got.chats);
    Ok(map_privacy_rules(account, key, &got.rules))
}

/// Writes an account privacy rule list (`account.setPrivacy`), preserving per-user and per-chat
/// exceptions in Telegram's expected rule order (specific exceptions before the base rule).
pub(super) fn set_privacy(
    account: &Account,
    key: protocol::PrivacyKey,
    policy: &protocol::PrivacyPolicy,
) -> Result<protocol::PrivacyPolicy, Fault> {
    use protocol::PrivacyAudience;
    use tl::enums::InputPrivacyRule as IR;
    let mut rules: Vec<IR> = Vec::new();
    let split = |list: &[protocol::PrivacyException]| -> (Vec<tl::enums::InputUser>, Vec<i64>) {
        let dir = account.dir();
        let mut users = Vec::new();
        let mut chats = Vec::new();
        for ex in list {
            let Some((who, None)) = Who::parse(&ex.id) else {
                continue;
            };
            match who {
                Who::User(_) => {
                    if let Some(input) = dir.input_user(who) {
                        users.push(input);
                    }
                }
                Who::Group(id) | Who::Channel(id) => chats.push(id),
            }
        }
        (users, chats)
    };
    let (allow_users, allow_chats) = split(&policy.always_allow);
    if !allow_users.is_empty() {
        rules.push(tl::types::InputPrivacyValueAllowUsers { users: allow_users }.into());
    }
    if !allow_chats.is_empty() {
        rules.push(tl::types::InputPrivacyValueAllowChatParticipants { chats: allow_chats }.into());
    }
    let (deny_users, deny_chats) = split(&policy.never_allow);
    if !deny_users.is_empty() {
        rules.push(tl::types::InputPrivacyValueDisallowUsers { users: deny_users }.into());
    }
    if !deny_chats.is_empty() {
        rules.push(tl::types::InputPrivacyValueDisallowChatParticipants { chats: deny_chats }.into());
    }
    rules.push(match policy.audience {
        PrivacyAudience::Everybody => IR::InputPrivacyValueAllowAll,
        PrivacyAudience::Contacts => IR::InputPrivacyValueAllowContacts,
        PrivacyAudience::Nobody => IR::InputPrivacyValueDisallowAll,
    });
    let tl::enums::account::PrivacyRules::Rules(got) =
        account.ask(&tl::functions::account::SetPrivacy { key: tl_privacy_key(key), rules })?;
    account.learn(&got.users, &got.chats);
    Ok(map_privacy_rules(account, key, &got.rules))
}

fn preset_from(settings: &tl::enums::AutoDownloadSettings) -> protocol::AutoDownloadPreset {
    let tl::enums::AutoDownloadSettings::Settings(settings) = settings;
    protocol::AutoDownloadPreset {
        disabled: settings.disabled,
        photo_size_max: i64::from(settings.photo_size_max),
        video_size_max: settings.video_size_max,
        file_size_max: settings.file_size_max,
    }
}

/// Writes the four fields this app edits onto the preset Telegram already has.
/// Preload, call, story, bitrate and queue fields stay as they were read.
fn preset_to(current: &tl::types::AutoDownloadSettings, preset: &protocol::AutoDownloadPreset) -> tl::enums::AutoDownloadSettings {
    let mut settings = current.clone();
    settings.disabled = preset.disabled;
    settings.photo_size_max = preset.photo_size_max.clamp(0, i64::from(i32::MAX)) as i32;
    settings.video_size_max = preset.video_size_max.max(0);
    settings.file_size_max = preset.file_size_max.max(0);
    tl::enums::AutoDownloadSettings::Settings(settings)
}

fn wifi_caps(preset: &protocol::AutoDownloadPreset) -> (u64, u64, u64) {
    let cap = |bytes: i64| if preset.disabled { 0 } else { bytes.max(0) as u64 };
    (cap(preset.photo_size_max), cap(preset.video_size_max), cap(preset.file_size_max))
}

/// This desktop uses the Wi-Fi preset. Chat-kind gates stay local and default off.
fn rules_of(settings: &protocol::AutoDownload) -> super::peers::DownloadRules {
    let (photo_max, video_max, file_max) = wifi_caps(&settings.wifi);
    super::peers::DownloadRules {
        fetch_full_photos: settings.fetch_full_photos,
        private_chats: settings.private_chats,
        groups: settings.groups,
        channels: settings.channels,
        photo_max,
        video_max,
        file_max,
    }
}

fn local_gates(account: &Account) -> (bool, bool, bool, bool) {
    use protocol::settings::{TELEGRAM_DOWNLOAD_CHANNELS, TELEGRAM_DOWNLOAD_GROUPS, TELEGRAM_DOWNLOAD_PHOTOS, TELEGRAM_DOWNLOAD_PRIVATE};
    (
        account.ctx.account_switch(TELEGRAM_DOWNLOAD_PHOTOS),
        account.ctx.account_switch(TELEGRAM_DOWNLOAD_PRIVATE),
        account.ctx.account_switch(TELEGRAM_DOWNLOAD_GROUPS),
        account.ctx.account_switch(TELEGRAM_DOWNLOAD_CHANNELS),
    )
}

fn remember_download(account: &Account, settings: &protocol::AutoDownload) {
    account.dir().download = rules_of(settings);
}

/// The stored chat-kind gates, plus the Wi-Fi caps when Telegram answers.
/// A miss leaves the caps off and still applies the gates. Sign-on calls this before any
/// message is mapped, so a restart does not wait for the Accounts page.
pub(super) fn load_remembered_download(account: &Account) {
    let (fetch_full_photos, private_chats, groups, channels) = local_gates(account);
    let mut rules = super::peers::DownloadRules {
        fetch_full_photos,
        private_chats,
        groups,
        channels,
        ..super::peers::DownloadRules::default()
    };
    if let Ok(tl::enums::account::AutoDownloadSettings::Settings(got)) = account.ask(&tl::functions::account::GetAutoDownloadSettings {}) {
        let (photo_max, video_max, file_max) = wifi_caps(&preset_from(&got.high));
        (rules.photo_max, rules.video_max, rules.file_max) = (photo_max, video_max, file_max);
    }
    account.dir().download = rules;
}

/// Reads `account.getAutoDownloadSettings` and the local chat-kind gates.
pub(super) fn get_auto_download(account: &Account) -> Result<protocol::AutoDownload, Fault> {
    let tl::enums::account::AutoDownloadSettings::Settings(got) = account.ask(&tl::functions::account::GetAutoDownloadSettings {})?;
    let (fetch_full_photos, private_chats, groups, channels) = local_gates(account);
    let settings = protocol::AutoDownload {
        roaming: preset_from(&got.low),
        mobile: preset_from(&got.medium),
        wifi: preset_from(&got.high),
        fetch_full_photos,
        private_chats,
        groups,
        channels,
    };
    remember_download(account, &settings);
    Ok(settings)
}

/// Writes each preset with `account.saveAutoDownloadSettings` and stores the local gates.
/// The cloud preset is read first. Only `disabled` and the three size caps change.
/// When that read fails, nothing is written over the preset.
pub(super) fn set_auto_download(account: &Account, settings: &protocol::AutoDownload) -> Result<protocol::AutoDownload, Fault> {
    let tl::enums::account::AutoDownloadSettings::Settings(got) = account.ask(&tl::functions::account::GetAutoDownloadSettings {})?;
    let save = |low: bool, high: bool, current: &tl::enums::AutoDownloadSettings, preset: &protocol::AutoDownloadPreset| -> Result<(), Fault> {
        let tl::enums::AutoDownloadSettings::Settings(current) = current;
        let _: bool = account.ask(&tl::functions::account::SaveAutoDownloadSettings { low, high, settings: preset_to(current, preset) })?;
        Ok(())
    };
    save(true, false, &got.low, &settings.roaming)?;
    save(false, false, &got.medium, &settings.mobile)?;
    save(false, true, &got.high, &settings.wifi)?;
    use protocol::settings::{TELEGRAM_DOWNLOAD_CHANNELS, TELEGRAM_DOWNLOAD_GROUPS, TELEGRAM_DOWNLOAD_PHOTOS, TELEGRAM_DOWNLOAD_PRIVATE};
    account.ctx.set_account_switch(TELEGRAM_DOWNLOAD_PHOTOS, settings.fetch_full_photos);
    account.ctx.set_account_switch(TELEGRAM_DOWNLOAD_PRIVATE, settings.private_chats);
    account.ctx.set_account_switch(TELEGRAM_DOWNLOAD_GROUPS, settings.groups);
    account.ctx.set_account_switch(TELEGRAM_DOWNLOAD_CHANNELS, settings.channels);
    remember_download(account, settings);
    Ok(settings.clone())
}

fn free_slots(boosts: &[tl::enums::MyBoost]) -> Vec<i32> {
    boosts
        .iter()
        .filter_map(|one| {
            let tl::enums::MyBoost::Boost(one) = one;
            (one.peer.is_none() && one.cooldown_until_date.is_none()).then_some(one.slot)
        })
        .collect()
}

fn boosts_of(account: &Account, who: Who) -> Result<protocol::ChannelBoosts, Fault> {
    let peer = account.input(who)?;
    let tl::enums::premium::BoostsStatus::Status(status) = account.ask(&tl::functions::premium::GetBoostsStatus { peer })?;
    let can_boost = account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium);
    let slots = if can_boost {
        let tl::enums::premium::MyBoosts::Boosts(mine) = account.ask(&tl::functions::premium::GetMyBoosts {})?;
        account.learn(&mine.users, &mine.chats);
        free_slots(&mine.my_boosts)
    } else {
        Vec::new()
    };
    Ok(protocol::ChannelBoosts {
        level: status.level,
        boosts: status.boosts,
        current_level_boosts: status.current_level_boosts,
        next_level_boosts: status.next_level_boosts,
        my_boost: status.my_boost,
        slots,
        boost_url: status.boost_url,
        can_boost,
    })
}

/// A channel's boost level. Anything that is not a channel is refused.
pub(super) fn get_boosts(account: &Account, conversation: &str) -> Result<protocol::ChannelBoosts, Fault> {
    let (who @ Who::Channel(_), None) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("PEER_ID_INVALID"));
    };
    boosts_of(account, who)
}

/// Gives one free Premium slot to the channel, after the shell has confirmed.
pub(super) fn apply_boost(account: &Account, conversation: &str) -> Result<protocol::ChannelBoosts, Fault> {
    let (who @ Who::Channel(_), None) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("PEER_ID_INVALID"));
    };
    let peer = account.input(who)?;
    let tl::enums::premium::MyBoosts::Boosts(mine) = account.ask(&tl::functions::premium::GetMyBoosts {})?;
    account.learn(&mine.users, &mine.chats);
    let Some(&slot) = free_slots(&mine.my_boosts).first() else {
        return Err(invalid("BOOSTS_EMPTY"));
    };
    let tl::enums::premium::MyBoosts::Boosts(applied) = account.ask(&tl::functions::premium::ApplyBoost { slots: Some(vec![slot]), peer })?;
    account.learn(&applied.users, &applied.chats);
    boosts_of(account, who)
}

fn http_url(url: &str) -> Option<String> {
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_owned())
}

fn menu_of(info: Option<&tl::enums::BotInfo>) -> protocol::BotMenu {
    let Some(tl::enums::BotInfo::Info(info)) = info else {
        return protocol::BotMenu::default();
    };
    let commands = info
        .commands
        .iter()
        .flatten()
        .filter_map(|cmd| {
            let tl::enums::BotCommand::Command(cmd) = cmd;
            let command = cmd.command.trim().trim_start_matches('/').to_owned();
            (!command.is_empty()).then(|| protocol::BotCommand { command, description: cmd.description.clone() })
        })
        .take(32)
        .collect();
    let (menu_label, menu_url) = match &info.menu_button {
        Some(tl::enums::BotMenuButton::Button(button)) => match http_url(&button.url) {
            Some(url) => (button.text.clone(), url),
            None => (String::new(), String::new()),
        },
        _ => (String::new(), String::new()),
    };
    protocol::BotMenu { description: info.description.clone().unwrap_or_default(), commands, menu_label, menu_url }
}

/// Commands and the menu button of a user-bot. A chat that is not one is refused.
pub(super) fn bot_menu(account: &Account, conversation: &str) -> Result<protocol::BotMenu, Fault> {
    let (who @ Who::User(_), None) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("PEER_ID_INVALID"));
    };
    let input = {
        let dir = account.dir();
        if !dir.get(who).is_some_and(|known| known.bot) {
            return Err(invalid("BOT_INVALID"));
        }
        dir.input_user(who).ok_or_else(|| invalid("PEER_ID_INVALID"))?
    };
    let tl::enums::users::UserFull::Full(found) = account.ask(&tl::functions::users::GetFullUser { id: input })?;
    account.learn(&found.users, &found.chats);
    let tl::enums::UserFull::Full(user) = found.full_user;
    Ok(menu_of(user.bot_info.as_ref()))
}

/// `messages.startBot` in this bot's chat. The peer is the bot.
pub(super) fn start_bot(account: &Account, conversation: &str, start_param: &str) -> Result<(), Fault> {
    let (who @ Who::User(_), None) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("PEER_ID_INVALID"));
    };
    let bot = {
        let dir = account.dir();
        if !dir.get(who).is_some_and(|known| known.bot) {
            return Err(invalid("BOT_INVALID"));
        }
        dir.input_user(who).ok_or_else(|| invalid("PEER_ID_INVALID"))?
    };
    let peer = account.input(who)?;
    let random_id = super::random_id(&format!("start:{conversation}:{start_param}"));
    let _: tl::enums::Updates = account.ask(&tl::functions::messages::StartBot { bot, peer, random_id, start_param: start_param.to_owned() })?;
    Ok(())
}

/// Sends this account's own phone as a contact, after the shell confirmed. No phone is a refusal.
pub(super) fn share_phone(account: &Account, conversation: &str) -> Result<(), Fault> {
    let (who @ Who::User(_), None) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("PEER_ID_INVALID"));
    };
    let (name, me) = {
        let dir = account.dir();
        if !dir.get(who).is_some_and(|known| known.bot) {
            return Err(invalid("BOT_INVALID"));
        }
        let name = dir.name(Who::User(account.me));
        let me = dir.input_user(Who::User(account.me)).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
        (name, me)
    };
    let users = account.ask(&tl::functions::users::GetUsers { id: vec![me] })?;
    let phone = users.into_iter().find_map(|user| {
        let tl::enums::User::User(user) = user else { return None };
        (user.id == account.me).then_some(user.phone).flatten().filter(|phone| !phone.is_empty())
    });
    let Some(phone) = phone else {
        return Err(invalid("PHONE_NUMBER_INVALID"));
    };
    let contact = protocol::SharedContact { name, phones: vec![phone], ..protocol::SharedContact::default() };
    super::send::send_contact(account, conversation, &format!("phone-{}", crate::hub::now()), "", None, &contact, super::Options::default())?;
    Ok(())
}

fn tl_report_reason(reason: protocol::ReportReason) -> tl::enums::ReportReason {
    match reason {
        protocol::ReportReason::Spam => tl::enums::ReportReason::InputReportReasonSpam,
        protocol::ReportReason::Violence => tl::enums::ReportReason::InputReportReasonViolence,
        protocol::ReportReason::Illegal => tl::enums::ReportReason::InputReportReasonIllegalDrugs,
        protocol::ReportReason::Impersonation => tl::enums::ReportReason::InputReportReasonFake,
        protocol::ReportReason::Harassment => tl::enums::ReportReason::InputReportReasonPersonalDetails,
        protocol::ReportReason::Other => tl::enums::ReportReason::InputReportReasonOther,
    }
}

fn pick_report_option(options: &[tl::enums::MessageReportOption], reason: protocol::ReportReason) -> Option<Vec<u8>> {
    let keywords: &[&str] = match reason {
        protocol::ReportReason::Spam => &["spam"],
        protocol::ReportReason::Violence => &["violence", "threat"],
        protocol::ReportReason::Illegal => &["illegal", "drug", "abuse"],
        protocol::ReportReason::Impersonation => &["fake", "impersonat", "scam"],
        protocol::ReportReason::Harassment => &["harass", "personal", "bully", "abuse"],
        protocol::ReportReason::Other => &["other"],
    };
    fn unwrap(opt: &tl::enums::MessageReportOption) -> &tl::types::MessageReportOption {
        let tl::enums::MessageReportOption::Option(o) = opt;
        o
    }
    options
        .iter()
        .map(unwrap)
        .find(|o| {
            let lower = o.text.to_lowercase();
            keywords.iter().any(|kw| lower.contains(kw))
        })
        .or_else(|| options.iter().map(unwrap).find(|o| o.text.to_lowercase().contains("other")))
        .or_else(|| options.first().map(unwrap))
        .map(|o| o.option.clone())
}

/// Reports a message (`messages.report`, walking Telegram's option tree) or a user/chat (`account.reportPeer`).
pub(super) fn report(
    account: &Account,
    target: &protocol::ReportTarget,
    reason: protocol::ReportReason,
    note: &str,
    block: bool,
) -> Result<(), Fault> {
    match target {
        protocol::ReportTarget::Message { conversation, message_id } => {
            let (who, _) = Who::parse(&conversation.id).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
            let id = message_id.parse::<i32>().map_err(|_| invalid("MESSAGE_ID_INVALID"))?;
            let peer = account.input(who)?;
            let mut option = Vec::new();
            for _ in 0..4 {
                let result = account.ask(&tl::functions::messages::Report {
                    peer: peer.clone(),
                    id: vec![id],
                    option: option.clone(),
                    message: note.to_owned(),
                })?;
                match result {
                    tl::enums::ReportResult::Reported => break,
                    tl::enums::ReportResult::ChooseOption(choose) => {
                        let Some(next) = pick_report_option(&choose.options, reason) else {
                            break;
                        };
                        if next == option {
                            break;
                        }
                        option = next;
                    }
                    tl::enums::ReportResult::AddComment(add) => {
                        account.ask(&tl::functions::messages::Report {
                            peer: peer.clone(),
                            id: vec![id],
                            option: add.option,
                            message: note.to_owned(),
                        })?;
                        break;
                    }
                }
            }
            if block && matches!(who, Who::User(_)) {
                let _ = super::relate(account, &conversation.id, protocol::RelateAction::Block);
            }
            Ok(())
        }
        protocol::ReportTarget::Person(key) | protocol::ReportTarget::Conversation(key) => {
            let (who, _) = Who::parse(&key.id).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
            let peer = account.input(who)?;
            account.ask(&tl::functions::account::ReportPeer {
                peer,
                reason: tl_report_reason(reason),
                message: note.to_owned(),
            })?;
            if block && matches!(who, Who::User(_)) {
                let _ = super::relate(account, &key.id, protocol::RelateAction::Block);
            }
            Ok(())
        }
    }
}

/// Press an inline callback button under a bot message (`messages.getBotCallbackAnswer`).
pub(super) fn press_button(
    account: &Account,
    conversation_id: &str,
    message_id: &str,
    data_hex: &str,
) -> Result<protocol::ButtonAnswer, Fault> {
    let (who, _) = Who::parse(conversation_id).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let msg_id: i32 = message_id.parse().map_err(|_| invalid("MESSAGE_ID_INVALID"))?;
    let bytes = super::content::hex_decode(data_hex).ok_or_else(|| invalid("DATA_INVALID"))?;
    let peer = account.input(who)?;
    let tl::enums::messages::BotCallbackAnswer::Answer(ans) = account.ask(
        &tl::functions::messages::GetBotCallbackAnswer {
            game: false,
            peer,
            msg_id,
            data: Some(bytes),
            password: None,
        },
    )?;
    Ok(protocol::ButtonAnswer {
        message: ans.message,
        alert: ans.alert,
        url: ans.url,
    })
}

/// Read account-wide notification defaults (`account.getNotifySettings`) and per-chat exceptions
/// (`account.getNotifyExceptions`).
pub(super) fn get_notify_defaults(account: &Account) -> Result<protocol::NotifyDefaults, Fault> {
    let kinds = [
        tl::enums::InputNotifyPeer::InputNotifyUsers,
        tl::enums::InputNotifyPeer::InputNotifyChats,
        tl::enums::InputNotifyPeer::InputNotifyBroadcasts,
    ];
    let mut muted = [false; 3];
    for (index, peer) in kinds.into_iter().enumerate() {
        let tl::enums::PeerNotifySettings::Settings(settings) =
            account.ask(&tl::functions::account::GetNotifySettings { peer })?;
        muted[index] = settings.mute_until.is_some_and(super::filing::mute_holds);
        account.live().muted[index] = muted[index];
    }
    let mut exceptions = Vec::new();
    if let Ok(updates) = account.ask(&tl::functions::account::GetNotifyExceptions {
        compare_sound: false,
        compare_stories: false,
        peer: None,
    }) {
        let list = match updates {
            tl::enums::Updates::Updates(u) => {
                account.learn(&u.users, &u.chats);
                u.updates
            }
            tl::enums::Updates::Combined(u) => {
                account.learn(&u.users, &u.chats);
                u.updates
            }
            tl::enums::Updates::UpdateShort(u) => vec![u.update],
            _ => Vec::new(),
        };
        let dir = account.dir();
        let acc_key = protocol::AccountKey {
            network: protocol::Network::Telegram,
            account: account.me.to_string(),
        };
        for upd in list {
            if let tl::enums::Update::NotifySettings(ns) = upd {
                if let tl::enums::NotifyPeer::Peer(p) = ns.peer {
                    let who = Who::of(&p.peer);
                    let tl::enums::PeerNotifySettings::Settings(s) = ns.notify_settings;
                    let is_muted = s.mute_until.is_some_and(super::filing::mute_holds);
                    exceptions.push(protocol::NotifyException {
                        conversation: protocol::Key::new(&acc_key, who.text()),
                        title: dir.name(who),
                        muted: is_muted,
                    });
                }
            }
        }
    }
    Ok(protocol::NotifyDefaults {
        users_muted: muted[0],
        groups_muted: muted[1],
        channels_muted: muted[2],
        exceptions,
    })
}

/// Update one account-wide notification default scope (`account.updateNotifySettings`) and
/// refresh unset dialogs so they reflect the new default immediately.
pub(super) fn set_notify_default(
    account: &std::sync::Arc<Account>,
    scope: protocol::NotifyDefaultScope,
    muted: bool,
) -> Result<protocol::NotifyDefaults, Fault> {
    let (index, peer) = match scope {
        protocol::NotifyDefaultScope::Users => (0, tl::enums::InputNotifyPeer::InputNotifyUsers),
        protocol::NotifyDefaultScope::Groups => (1, tl::enums::InputNotifyPeer::InputNotifyChats),
        protocol::NotifyDefaultScope::Channels => (2, tl::enums::InputNotifyPeer::InputNotifyBroadcasts),
    };
    let settings = tl::types::InputPeerNotifySettings {
        show_previews: None,
        silent: None,
        mute_until: Some(if muted { i32::MAX } else { 0 }),
        sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_sound: None,
    };
    account.ask(&tl::functions::account::UpdateNotifySettings { peer, settings: settings.into() })?;
    account.live().muted[index] = muted;
    let _ = sync::dialogs(account);
    get_notify_defaults(account)
}

/// Reset all account-wide notification defaults and exceptions (`account.resetNotifySettings`).
pub(super) fn reset_notify_defaults(account: &std::sync::Arc<Account>) -> Result<protocol::NotifyDefaults, Fault> {
    account.ask(&tl::functions::account::ResetNotifySettings {})?;
    account.live().muted = [false; 3];
    let _ = sync::dialogs(account);
    get_notify_defaults(account)
}

/// List Saved Messages sub-dialogs (`messages.getSavedDialogs`) and reaction tags
/// (`messages.getSavedReactionTags`).
pub(super) fn saved_dialogs(account: &Account) -> Result<(Vec<protocol::SavedDialog>, Vec<protocol::SavedTag>), Fault> {
    let raw = account.ask(&tl::functions::messages::GetSavedDialogs {
        exclude_pinned: false,
        parent_peer: None,
        offset_date: 0,
        offset_id: 0,
        offset_peer: tl::enums::InputPeer::Empty,
        limit: 100,
        hash: 0,
    })?;
    let (dialogs_raw, messages_raw) = match raw {
        tl::enums::messages::SavedDialogs::Dialogs(d) => {
            account.learn(&d.users, &d.chats);
            (d.dialogs, d.messages)
        }
        tl::enums::messages::SavedDialogs::Slice(d) => {
            account.learn(&d.users, &d.chats);
            (d.dialogs, d.messages)
        }
        tl::enums::messages::SavedDialogs::NotModified(_) => (Vec::new(), Vec::new()),
    };
    let mut top_by_id: std::collections::HashMap<i32, (String, i64)> = std::collections::HashMap::new();
    for msg in &messages_raw {
        if let tl::enums::Message::Message(m) = msg {
            top_by_id.insert(m.id, (m.message.clone(), i64::from(m.date)));
        }
    }
    let dir = account.dir();
    let mut dialogs = Vec::new();
    for item in dialogs_raw {
        if let tl::enums::SavedDialog::Dialog(d) = item {
            let who = Who::of(&d.peer);
            let title = if who == Who::User(account.me) {
                "My Notes".to_owned()
            } else {
                dir.name(who)
            };
            let (preview, updated) = top_by_id.get(&d.top_message).cloned().unwrap_or_default();
            dialogs.push(protocol::SavedDialog {
                peer_id: who.text(),
                title,
                pinned: d.pinned,
                preview,
                updated,
            });
        }
    }
    let mut tags = Vec::new();
    if let Ok(tl::enums::messages::SavedReactionTags::Tags(found)) = account.ask(
        &tl::functions::messages::GetSavedReactionTags { peer: None, hash: 0 },
    ) {
        for tl::enums::SavedReactionTag::Tag(t) in found.tags {
            let emoji = match t.reaction {
                tl::enums::Reaction::Emoji(e) => e.emoticon,
                tl::enums::Reaction::CustomEmoji(c) => format!("custom:{}", c.document_id),
                tl::enums::Reaction::Paid => "⭐".to_owned(),
                tl::enums::Reaction::Empty => continue,
            };
            tags.push(protocol::SavedTag {
                emoji,
                title: t.title.unwrap_or_default(),
                count: t.count.max(0) as u32,
            });
        }
    }
    Ok((dialogs, tags))
}

/// Fetch Saved Messages filtered by source peer (`messages.getSavedHistory`) and/or reaction tag
/// (`messages.search` with `saved_reaction`).
pub(super) fn saved_history(
    account: &Account,
    peer_id: Option<&str>,
    tag: Option<&str>,
    limit: u32,
) -> Result<Vec<Message>, Fault> {
    let limit = (limit.clamp(1, protocol::PAGE_LIMIT)) as i32;
    let saved_peer = match peer_id.filter(|s| !s.is_empty()) {
        Some(pid) => {
            let (who, _) = Who::parse(pid).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
            Some(account.input(who)?)
        }
        None => None,
    };
    let raw = if let Some(emoji) = tag.filter(|s| !s.is_empty()) {
        let reaction = if let Some(id_str) = emoji.strip_prefix("custom:") {
            let doc_id: i64 = id_str.parse().map_err(|_| invalid("REACTION_INVALID"))?;
            tl::enums::Reaction::CustomEmoji(tl::types::ReactionCustomEmoji { document_id: doc_id })
        } else {
            tl::enums::Reaction::Emoji(tl::types::ReactionEmoji { emoticon: emoji.to_owned() })
        };
        account.ask(&tl::functions::messages::Search {
            peer: tl::enums::InputPeer::PeerSelf,
            q: String::new(),
            from_id: None,
            saved_peer_id: saved_peer,
            saved_reaction: Some(vec![reaction]),
            top_msg_id: None,
            filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
            min_date: 0,
            max_date: 0,
            offset_id: 0,
            add_offset: 0,
            limit,
            max_id: 0,
            min_id: 0,
            hash: 0,
        })?
    } else {
        account.ask(&tl::functions::messages::GetSavedHistory {
            parent_peer: None,
            peer: saved_peer.unwrap_or(tl::enums::InputPeer::PeerSelf),
            offset_id: 0,
            offset_date: 0,
            add_offset: 0,
            limit,
            max_id: 0,
            min_id: 0,
            hash: 0,
        })?
    };
    let messages_raw = match raw {
        tl::enums::messages::Messages::Messages(m) => {
            account.learn(&m.users, &m.chats);
            m.messages
        }
        tl::enums::messages::Messages::Slice(m) => {
            account.learn(&m.users, &m.chats);
            m.messages
        }
        tl::enums::messages::Messages::ChannelMessages(m) => {
            account.learn(&m.users, &m.chats);
            m.messages
        }
        tl::enums::messages::Messages::NotModified(_) => Vec::new(),
    };
    let dir = account.dir();
    let mut out = Vec::new();
    for raw_msg in &messages_raw {
        if let Some(mapped) = super::content::map(&dir, raw_msg) {
            out.push(mapped.message);
        }
    }
    Ok(out)
}

/// List active signed-in devices/sessions (`account.getAuthorizations`).
pub(super) fn active_sessions(account: &Account) -> Result<Vec<protocol::ActiveSession>, Fault> {
    let tl::enums::account::Authorizations::Authorizations(found) =
        account.ask(&tl::functions::account::GetAuthorizations {})?;
    let mut out = Vec::new();
    for tl::enums::Authorization::Authorization(a) in found.authorizations {
        let app = format!("{} {}", a.app_name, a.app_version).trim().to_owned();
        let platform = format!("{} {}", a.platform, a.system_version).trim().to_owned();
        let location = match (a.region.is_empty(), a.country.is_empty()) {
            (false, false) => format!("{}, {}", a.region, a.country),
            (false, true) => a.region,
            (true, false) => a.country,
            (true, true) => String::new(),
        };
        out.push(protocol::ActiveSession {
            id: a.hash.to_string(),
            app,
            device: a.device_model,
            platform,
            location,
            last_active: i64::from(a.date_active),
            current: a.current,
        });
    }
    Ok(out)
}

/// Terminate another active session (`account.resetAuthorization`). Never ends the current session (`hash == 0`).
pub(super) fn end_session(account: &Account, session_id: &str) -> Result<(), Fault> {
    let hash: i64 = session_id.trim().parse().map_err(|_| invalid("HASH_INVALID"))?;
    if hash == 0 {
        return Err(invalid("CURRENT_SESSION_FORBIDDEN"));
    }
    account.ask(&tl::functions::account::ResetAuthorization { hash })?;
    Ok(())
}

/// Fetch unread mention or reaction message IDs in a conversation
/// (`messages.getUnreadMentions` / `messages.getUnreadReactions`).
pub(super) fn unread_items(
    account: &Account,
    conversation_id: &str,
    kind: protocol::UnreadItemKind,
    limit: u32,
) -> Result<Vec<String>, Fault> {
    let (who, topic) = Who::parse(conversation_id).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let peer = account.input(who)?;
    let limit = (limit.clamp(1, protocol::PAGE_LIMIT)) as i32;
    let raw = match kind {
        protocol::UnreadItemKind::Mentions => account.ask(&tl::functions::messages::GetUnreadMentions {
            peer,
            top_msg_id: topic,
            offset_id: 0,
            add_offset: 0,
            limit,
            max_id: 0,
            min_id: 0,
        })?,
        protocol::UnreadItemKind::Reactions => account.ask(&tl::functions::messages::GetUnreadReactions {
            peer,
            top_msg_id: topic,
            saved_peer_id: None,
            offset_id: 0,
            add_offset: 0,
            limit,
            max_id: 0,
            min_id: 0,
        })?,
    };
    let messages_raw = match raw {
        tl::enums::messages::Messages::Messages(m) => {
            account.learn(&m.users, &m.chats);
            m.messages
        }
        tl::enums::messages::Messages::Slice(m) => {
            account.learn(&m.users, &m.chats);
            m.messages
        }
        tl::enums::messages::Messages::ChannelMessages(m) => {
            account.learn(&m.users, &m.chats);
            m.messages
        }
        tl::enums::messages::Messages::NotModified(_) => Vec::new(),
    };
    let dir = account.dir();
    let mut ids = Vec::new();
    let mut mapped_msgs = Vec::new();
    for raw_msg in &messages_raw {
        if let Some(mapped) = super::content::map(&dir, raw_msg) {
            ids.push(mapped.id.to_string());
            mapped_msgs.push(mapped.message);
        }
    }
    if !mapped_msgs.is_empty() {
        let _ = account.ctx.history(conversation_id, &mapped_msgs);
    }
    Ok(ids)
}

/// Mark unread mentions or reactions read (`messages.readMessageContents` / `channels.readMessageContents`
/// / `messages.readMentions` / `messages.readReactions`).
pub(super) fn mark_unread_items_read(
    account: &Account,
    conversation_id: &str,
    kind: protocol::UnreadItemKind,
    message_ids: &[String],
) -> Result<(), Fault> {
    let (who, topic) = Who::parse(conversation_id).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    match kind {
        protocol::UnreadItemKind::Mentions => {
            let parsed_ids: Vec<i32> = message_ids.iter().filter_map(|id| id.parse::<i32>().ok()).collect();
            if !parsed_ids.is_empty() {
                let seen = parsed_ids.len() as u32;
                match who {
                    Who::Channel(_) => {
                        let channel = account.input_channel(who)?;
                        account.ask(&tl::functions::channels::ReadMessageContents { channel, id: parsed_ids })?;
                    }
                    _ => {
                        account.ask(&tl::functions::messages::ReadMessageContents { id: parsed_ids })?;
                    }
                }
                // Seeing one mention tells Telegram about that message. The badge drops by
                // what was just read; an empty list is the separate "mark all" below.
                if let Ok(Some(row)) = account.ctx.known(conversation_id) {
                    let _ = account.ctx.mentions_count(conversation_id, protocol::badge_after_jump(row.mentions, seen));
                }
            } else {
                let peer = account.input(who)?;
                account.ask(&tl::functions::messages::ReadMentions { peer, top_msg_id: topic })?;
                let _ = account.ctx.mentions_count(conversation_id, 0);
            }
        }
        protocol::UnreadItemKind::Reactions => {
            // `messages.readReactions` clears every unread reaction in the chat. A walk
            // through the list names the one just opened; only an empty list is the clear.
            if message_ids.is_empty() {
                let peer = account.input(who)?;
                account.ask(&tl::functions::messages::ReadReactions {
                    peer,
                    top_msg_id: topic,
                    saved_peer_id: None,
                })?;
                let _ = account.ctx.unread_reactions(conversation_id, protocol::badge_after_jump(0, 0));
            } else if let Ok(Some(row)) = account.ctx.known(conversation_id) {
                let seen = message_ids.len() as u32;
                let _ = account.ctx.unread_reactions(conversation_id, protocol::badge_after_jump(row.unread_reactions, seen));
            }
        }
    }
    Ok(())
}

/// Finds the voice or round video attachment on a message to attach a transcript to.
fn transcript_slot(attachments: &mut [protocol::Attachment]) -> Option<&mut protocol::Attachment> {
    let idx = attachments
        .iter()
        .position(|a| !a.waveform.is_empty() || a.round || a.file.kind == protocol::MediaKind::Audio)
        .or_else(|| (!attachments.is_empty()).then_some(0))?;
    attachments.get_mut(idx)
}

/// Requests server-side transcription of a voice or round video message (`messages.transcribeAudio`).
/// Never invoked automatically; persists the result on the stored message's attachment so it survives restarts.
pub(super) fn transcribe(
    account: &Account,
    conversation: &str,
    message_id: &str,
) -> Result<protocol::Transcript, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let msg_id = message_id.parse::<i32>().map_err(|_| invalid("MESSAGE_ID_INVALID"))?;
    let peer = account.input(who)?;
    let transcript = match account.ask(&tl::functions::messages::TranscribeAudio { peer, msg_id }) {
        Ok(tl::enums::messages::TranscribedAudio::Audio(res)) => {
            let trial_remains = res.trial_remains_num.map(|n| n.max(0) as u32);
            let trial_resets_at = res.trial_remains_until_date.map(i64::from);
            let failed = if !res.pending && res.text.trim().is_empty() && trial_remains == Some(0) {
                Some("Free transcriptions used up for this week. Telegram Premium unlocks unlimited transcriptions.".to_owned())
            } else {
                None
            };
            protocol::Transcript {
                id: res.transcription_id,
                text: res.text,
                pending: res.pending,
                failed,
                trial_remains,
                trial_resets_at,
                rated: None,
            }
        }
        Err(fault) if fault.is("PREMIUM_ACCOUNT_REQUIRED") => protocol::Transcript {
            id: 0,
            text: String::new(),
            pending: false,
            failed: Some("Free transcriptions used up for this week. Telegram Premium unlocks unlimited transcriptions.".to_owned()),
            trial_remains: Some(0),
            trial_resets_at: None,
            rated: None,
        },
        Err(fault) if fault.is("TRANSCRIPTION_FAILED") => protocol::Transcript {
            id: 0,
            text: String::new(),
            pending: false,
            failed: Some(fault.words()),
            trial_remains: None,
            trial_resets_at: None,
            rated: None,
        },
        Err(fault) => return Err(fault),
    };
    if let Ok(Some(mut stored)) = account.ctx.stored_message(conversation, message_id) {
        if let Some(att) = transcript_slot(&mut stored.attachments) {
            att.transcript = Some(transcript.clone());
            let _ = account.ctx.history(conversation, std::slice::from_ref(&stored));
        }
    }
    Ok(transcript)
}

/// Applies an incremental or final `updateTranscribedAudio` update onto the stored message.
pub(super) fn apply_transcript_update(
    account: &Account,
    conversation: &str,
    msg_id: i32,
    transcription_id: i64,
    text: &str,
    pending: bool,
) {
    let message_id = msg_id.to_string();
    let Ok(Some(mut stored)) = account.ctx.stored_message(conversation, &message_id) else {
        return;
    };
    let Some(att) = transcript_slot(&mut stored.attachments) else {
        return;
    };
    let mut next = att.transcript.clone().unwrap_or_default();
    next.id = transcription_id;
    next.text = text.to_owned();
    next.pending = pending;
    next.failed = None;
    att.transcript = Some(next);
    let _ = account.ctx.history(conversation, std::slice::from_ref(&stored));
}

/// Rates a completed transcription thumbs up or down (`messages.rateTranscribedAudio`).
pub(super) fn rate_transcript(
    account: &Account,
    conversation: &str,
    message_id: &str,
    transcription_id: i64,
    good: bool,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let msg_id = message_id.parse::<i32>().map_err(|_| invalid("MESSAGE_ID_INVALID"))?;
    let peer = account.input(who)?;
    account.ask(&tl::functions::messages::RateTranscribedAudio {
        peer,
        msg_id,
        transcription_id,
        good,
    })?;
    if let Ok(Some(mut stored)) = account.ctx.stored_message(conversation, message_id) {
        if let Some(att) = transcript_slot(&mut stored.attachments) {
            if let Some(tr) = att.transcript.as_mut() {
                tr.rated = Some(good);
                let _ = account.ctx.history(conversation, std::slice::from_ref(&stored));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_global_cursor_handles_empty_and_valid() {
        assert_eq!(parse_global_cursor(None), (0, None, 0));
        assert_eq!(parse_global_cursor(Some("1700000000:u7003:31")), (1_700_000_000, Some(Who::User(7003)), 31));
        assert_eq!(parse_global_cursor(Some("bad")), (0, None, 0));
    }

    #[test]
    fn report_reason_and_option_mapping() {
        assert!(matches!(tl_report_reason(protocol::ReportReason::Spam), tl::enums::ReportReason::InputReportReasonSpam));
        assert!(matches!(tl_report_reason(protocol::ReportReason::Violence), tl::enums::ReportReason::InputReportReasonViolence));
        assert!(matches!(tl_report_reason(protocol::ReportReason::Illegal), tl::enums::ReportReason::InputReportReasonIllegalDrugs));
        assert!(matches!(tl_report_reason(protocol::ReportReason::Impersonation), tl::enums::ReportReason::InputReportReasonFake));
        assert!(matches!(tl_report_reason(protocol::ReportReason::Harassment), tl::enums::ReportReason::InputReportReasonPersonalDetails));
        assert!(matches!(tl_report_reason(protocol::ReportReason::Other), tl::enums::ReportReason::InputReportReasonOther));

        let opts = vec![
            tl::enums::MessageReportOption::Option(tl::types::MessageReportOption { text: "Spam".into(), option: b"spam".to_vec() }),
            tl::enums::MessageReportOption::Option(tl::types::MessageReportOption { text: "Violence".into(), option: b"violence".to_vec() }),
            tl::enums::MessageReportOption::Option(tl::types::MessageReportOption { text: "Other".into(), option: b"other".to_vec() }),
        ];
        assert_eq!(pick_report_option(&opts, protocol::ReportReason::Spam).as_deref(), Some(&b"spam"[..]));
        assert_eq!(pick_report_option(&opts, protocol::ReportReason::Violence).as_deref(), Some(&b"violence"[..]));
        assert_eq!(pick_report_option(&opts, protocol::ReportReason::Impersonation).as_deref(), Some(&b"other"[..]));
    }

    #[test]
    fn parse_birthday_and_emoji_status_validate_inputs() {
        assert_eq!(parse_birthday("").unwrap(), None);
        assert_eq!(
            parse_birthday("1994-03-14").unwrap(),
            Some(tl::types::Birthday { day: 14, month: 3, year: Some(1994) }.into())
        );
        assert_eq!(
            parse_birthday("--07-04").unwrap(),
            Some(tl::types::Birthday { day: 4, month: 7, year: None }.into())
        );
        assert!(parse_birthday("1994-13-01").is_err());
        assert!(parse_birthday("not-a-date").is_err());

        assert_eq!(parse_emoji_status("", None).unwrap(), tl::enums::EmojiStatus::Empty);
        assert_eq!(
            parse_emoji_status("e5002", Some(1_800_000_000)).unwrap(),
            tl::types::EmojiStatus { document_id: 5002, until: Some(1_800_000_000) }.into()
        );
        assert!(parse_emoji_status("not_an_id", None).is_err());
    }
}
