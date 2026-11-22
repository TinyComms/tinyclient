//! A conversation's member list, in the sections Telegram already has: the owner, the admins
//! (with the title they were given), the bots, then everyone else. Contacts come first inside
//! a section.
//!
//! A channel's list is three asks (`admins`, `bots`, `recent`), capped, and kept for a minute
//! so the next page does not ask again. Telegram flood-limits these. A group that hides its
//! members says so; an empty list would look like a group with nobody in it.
//!
//! The member list has no query of its own. While a channel is on screen, a people search also
//! asks that channel, and anyone it names joins the same list a contact search fills.

use super::peers::Who;
use super::wire::Fault;
use super::Account;
use crate::adapters::Members;
use grammers_tl_types as tl;
use protocol::{MemberSection, Relation};
use std::time::{Duration, Instant};

/// How long one conversation's roster is reused. Paging stays inside it.
const FRESH: Duration = Duration::from_secs(60);
/// Telegram's own cap on one `channels.getParticipants` answer. We never ask for more.
const FETCH: i32 = 200;
/// A people search asks the open channel for this many matches.
const SEARCH: i32 = 25;

/// Where one person sits in the list. The order is the order of the sections.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    Owner,
    Admin,
    Bot,
    Member,
}

const ORDER: [(Bucket, &str); 4] = [
    (Bucket::Owner, "Owner"),
    (Bucket::Admin, "Admins"),
    (Bucket::Bot, "Bots"),
    (Bucket::Member, "Members"),
];

#[derive(Clone)]
struct Seat {
    id: i64,
    rank: String,
    bucket: Bucket,
    contact: bool,
}

/// One conversation's roster, built once and sliced for each page.
#[derive(Clone)]
pub(super) struct Held {
    conversation: String,
    at: Instant,
    seats: Vec<Seat>,
    sections: Vec<MemberSection>,
}

fn invalid() -> Fault {
    Fault::Rpc {
        code: 400,
        name: "PEER_ID_INVALID".to_owned(),
        value: None,
    }
}

fn hidden() -> Fault {
    Fault::Rpc {
        code: 400,
        name: "PARTICIPANTS_HIDDEN".to_owned(),
        value: None,
    }
}

/// One window of a conversation's members. `offset` counts people, in section order.
pub(super) fn list(
    account: &Account,
    conversation: &str,
    offset: u32,
    limit: u32,
) -> Result<Members, Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(invalid)?;
    let key = who.text();
    if let Some(held) = fresh(account, &key) {
        return Ok(window(account, &held, offset, limit));
    }
    let held = match who {
        Who::User(id) => direct(account, id),
        Who::Group(id) => group(account, id)?,
        Who::Channel(id) => channel(account, id)?,
    };
    let page = window(account, &held, offset, limit);
    account.live().roster = Some(held);
    Ok(page)
}

/// While a channel is open, a people search also asks that channel. A miss is ignored: the
/// contact search has already been asked, and its answer still stands.
pub(super) fn search_open(account: &Account, query: &str) {
    let query = query.trim();
    if query.chars().count() < 2 {
        return;
    }
    let open = account.ctx.watching();
    let Some(conversation) = open.conversation.as_ref() else {
        return;
    };
    if conversation.network != account.ctx.account.network
        || conversation.account != account.ctx.account.account
    {
        return;
    }
    // A forum topic is still that channel. The search is of the channel, not the topic.
    let Some((Who::Channel(id), _)) = Who::parse(&conversation.id) else {
        return;
    };
    let filter = tl::enums::ChannelParticipantsFilter::ChannelParticipantsSearch(
        tl::types::ChannelParticipantsSearch {
            q: query.to_owned(),
        },
    );
    let _ = participants(account, Who::Channel(id), filter, SEARCH);
}

fn fresh(account: &Account, key: &str) -> Option<Held> {
    let live = account.live();
    let held = live.roster.as_ref()?;
    (held.conversation == key && held.at.elapsed() < FRESH).then(|| held.clone())
}

/// A direct chat is the two people in it, the account first.
fn direct(account: &Account, id: i64) -> Held {
    let seats = [account.me, id]
        .into_iter()
        .map(|id| seat(account, id, String::new(), Bucket::Member))
        .collect();
    remember(Who::User(id), seats, sections_of_direct())
}

fn sections_of_direct() -> Vec<MemberSection> {
    vec![MemberSection {
        name: "Members".to_owned(),
        count: 2,
    }]
}

fn group(account: &Account, chat_id: i64) -> Result<Held, Fault> {
    let tl::enums::messages::ChatFull::Full(full) =
        account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
    account.learn(&full.users, &full.chats);
    let mut seats = Vec::new();
    match &full.full_chat {
        tl::enums::ChatFull::Full(chat) => match &chat.participants {
            tl::enums::ChatParticipants::Forbidden(_) => return Err(hidden()),
            tl::enums::ChatParticipants::Participants(list) => {
                seats.extend(
                    list.participants
                        .iter()
                        .map(|one| seat_from_group(account, one)),
                );
            }
        },
        tl::enums::ChatFull::ChannelFull(_) => {}
    }
    arrange(&mut seats);
    let sections = sections_of(&seats, None);
    Ok(remember(Who::Group(chat_id), seats, sections))
}

fn channel(account: &Account, id: i64) -> Result<Held, Fault> {
    let who = Who::Channel(id);
    let tl::enums::messages::ChatFull::Full(full) =
        account.ask(&tl::functions::channels::GetFullChannel {
            channel: account.input_channel(who)?,
        })?;
    account.learn(&full.users, &full.chats);
    let total = match &full.full_chat {
        tl::enums::ChatFull::ChannelFull(channel) if channel.participants_hidden => {
            return Err(hidden())
        }
        tl::enums::ChatFull::ChannelFull(channel) => channel.participants_count,
        _ => None,
    };
    // Hidden was refused above, so these asks are of a list Telegram will answer.
    let mut seats = Vec::new();
    take(
        account,
        &mut seats,
        who,
        tl::enums::ChannelParticipantsFilter::ChannelParticipantsAdmins,
        false,
    )?;
    take(
        account,
        &mut seats,
        who,
        tl::enums::ChannelParticipantsFilter::ChannelParticipantsBots,
        true,
    )?;
    take(
        account,
        &mut seats,
        who,
        tl::enums::ChannelParticipantsFilter::ChannelParticipantsRecent,
        false,
    )?;
    arrange(&mut seats);
    let sections = sections_of(&seats, total);
    Ok(remember(who, seats, sections))
}

fn take(
    account: &Account,
    seats: &mut Vec<Seat>,
    who: Who,
    filter: tl::enums::ChannelParticipantsFilter,
    force_bot: bool,
) -> Result<(), Fault> {
    let page = participants(account, who, filter, FETCH)?;
    for one in &page.participants {
        consider(account, seats, one, force_bot);
    }
    Ok(())
}

fn participants(
    account: &Account,
    who: Who,
    filter: tl::enums::ChannelParticipantsFilter,
    limit: i32,
) -> Result<tl::types::channels::ChannelParticipants, Fault> {
    let request = tl::functions::channels::GetParticipants {
        channel: account.input_channel(who)?,
        filter,
        offset: 0,
        limit,
        hash: 0,
    };
    match account.ask(&request)? {
        tl::enums::channels::ChannelParticipants::Participants(page) => {
            account.learn(&page.users, &page.chats);
            Ok(page)
        }
        tl::enums::channels::ChannelParticipants::NotModified => {
            Ok(tl::types::channels::ChannelParticipants {
                count: 0,
                participants: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            })
        }
    }
}

fn consider(
    account: &Account,
    seats: &mut Vec<Seat>,
    one: &tl::enums::ChannelParticipant,
    force_bot: bool,
) {
    let Some((id, rank, kind)) = classified(one) else {
        return;
    };
    if seats.iter().any(|seat| seat.id == id) {
        return;
    }
    let bucket = if force_bot {
        Bucket::Bot
    } else {
        match kind {
            Bucket::Member
                if account
                    .dir()
                    .get(Who::User(id))
                    .is_some_and(|known| known.bot) =>
            {
                Bucket::Bot
            }
            other => other,
        }
    };
    seats.push(seat(account, id, rank, bucket));
}

/// Creator and admin keep their place. Everyone else is a member until the bot flag says otherwise.
fn classified(one: &tl::enums::ChannelParticipant) -> Option<(i64, String, Bucket)> {
    Some(match one {
        tl::enums::ChannelParticipant::Creator(one) => {
            (one.user_id, words(&one.rank), Bucket::Owner)
        }
        tl::enums::ChannelParticipant::Admin(one) => (one.user_id, words(&one.rank), Bucket::Admin),
        tl::enums::ChannelParticipant::Participant(one) => {
            (one.user_id, words(&one.rank), Bucket::Member)
        }
        tl::enums::ChannelParticipant::ParticipantSelf(one) => {
            (one.user_id, words(&one.rank), Bucket::Member)
        }
        tl::enums::ChannelParticipant::Banned(_) | tl::enums::ChannelParticipant::Left(_) => {
            return None
        }
    })
}

fn seat_from_group(account: &Account, one: &tl::enums::ChatParticipant) -> Seat {
    let (id, rank, bucket) = match one {
        tl::enums::ChatParticipant::Creator(one) => (one.user_id, words(&one.rank), Bucket::Owner),
        tl::enums::ChatParticipant::Admin(one) => (one.user_id, words(&one.rank), Bucket::Admin),
        tl::enums::ChatParticipant::Participant(one) => {
            let bot = account
                .dir()
                .get(Who::User(one.user_id))
                .is_some_and(|known| known.bot);
            (
                one.user_id,
                words(&one.rank),
                if bot { Bucket::Bot } else { Bucket::Member },
            )
        }
    };
    seat(account, id, rank, bucket)
}

fn seat(account: &Account, id: i64, rank: String, bucket: Bucket) -> Seat {
    let contact = account
        .dir()
        .get(Who::User(id))
        .is_some_and(|known| known.contact);
    Seat {
        id,
        rank,
        bucket,
        contact,
    }
}

fn words(rank: &Option<String>) -> String {
    rank.clone().unwrap_or_default()
}

/// Contacts first inside a section. People who arrived together keep that order.
fn arrange(seats: &mut [Seat]) {
    seats.sort_by_key(|seat| (seat.bucket, !seat.contact));
}

fn sections_of(seats: &[Seat], total: Option<i32>) -> Vec<MemberSection> {
    let count = |bucket: Bucket| seats.iter().filter(|seat| seat.bucket == bucket).count() as u32;
    let staff = count(Bucket::Owner) + count(Bucket::Admin) + count(Bucket::Bot);
    let held = count(Bucket::Member);
    // The channel's own count covers everyone. What we did not hold is a tail the window leaves empty.
    let members = match total {
        Some(total) => u32::try_from(total)
            .unwrap_or(0)
            .saturating_sub(staff)
            .max(held),
        None => held,
    };
    let counts = [
        count(Bucket::Owner),
        count(Bucket::Admin),
        count(Bucket::Bot),
        members,
    ];
    ORDER
        .iter()
        .zip(counts)
        .filter(|(_, count)| *count > 0)
        .map(|((_, name), count)| MemberSection {
            name: (*name).to_owned(),
            count,
        })
        .collect()
}

fn remember(who: Who, seats: Vec<Seat>, sections: Vec<MemberSection>) -> Held {
    Held {
        conversation: who.text(),
        at: Instant::now(),
        seats,
        sections,
    }
}

fn window(account: &Account, held: &Held, offset: u32, limit: u32) -> Members {
    let start = offset as usize;
    let stop = start.saturating_add(limit as usize);
    let mut cursor = 0usize;
    let mut members = Vec::new();
    for (bucket, name) in ORDER {
        let Some(section) = held.sections.iter().find(|section| section.name == name) else {
            continue;
        };
        let count = section.count as usize;
        let filled: Vec<&Seat> = held
            .seats
            .iter()
            .filter(|seat| seat.bucket == bucket)
            .collect();
        let from = start.saturating_sub(cursor).min(filled.len());
        let to = stop.saturating_sub(cursor).min(filled.len());
        if from < to {
            members.extend(filled[from..to].iter().map(|seat| {
                let relation = if seat.contact {
                    Relation::Contact
                } else {
                    Relation::None
                };
                super::person(account, Who::User(seat.id), relation, &seat.rank)
            }));
        }
        cursor = cursor.saturating_add(count);
        if cursor >= stop {
            break;
        }
    }
    Members {
        pending: false,
        sections: held.sections.clone(),
        members,
    }
}

/// The lab's check: an owner, a contact admin, another admin and a bot land in that order, and
/// a rank query keeps the admin it names. No network.
pub(super) fn prove() -> Result<(), String> {
    let mut seats = vec![
        Seat {
            id: 2,
            rank: "Guide".to_owned(),
            bucket: Bucket::Admin,
            contact: false,
        },
        Seat {
            id: 4,
            rank: String::new(),
            bucket: Bucket::Bot,
            contact: false,
        },
        Seat {
            id: 1,
            rank: String::new(),
            bucket: Bucket::Owner,
            contact: false,
        },
        Seat {
            id: 3,
            rank: "Quartermaster".to_owned(),
            bucket: Bucket::Admin,
            contact: true,
        },
    ];
    arrange(&mut seats);
    let sections = sections_of(&seats, Some(4));
    let names: Vec<&str> = sections
        .iter()
        .map(|section| section.name.as_str())
        .collect();
    if names != ["Owner", "Admins", "Bots"] {
        return Err(format!("sections were {names:?}"));
    }
    let order: Vec<i64> = seats.iter().map(|seat| seat.id).collect();
    if order != [1, 3, 2, 4] {
        return Err(format!("order was {order:?}"));
    }
    let kept: Vec<i64> = seats
        .iter()
        .filter(|seat| seat.rank.to_lowercase().contains("quartermaster"))
        .map(|seat| seat.id)
        .collect();
    if kept != [3] {
        return Err(format!("a rank query kept {kept:?}"));
    }
    Ok(())
}

/// Extracts the created group or channel peer from the `Updates` returned by
/// `messages.createChat` or `channels.createChannel`.
fn created_peer(updates: &tl::enums::Updates) -> Option<Who> {
    let chats = match updates {
        tl::enums::Updates::Updates(u) => u.chats.as_slice(),
        tl::enums::Updates::Combined(u) => u.chats.as_slice(),
        _ => &[],
    };
    chats.iter().find_map(|chat| match chat {
        tl::enums::Chat::Chat(c) => Some(Who::Group(c.id)),
        tl::enums::Chat::Channel(c) => Some(Who::Channel(c.id)),
        _ => None,
    })
}

/// Formats a user-facing note for people Telegram could not add directly (`missing_invitees`).
pub(super) fn missing_invitee_words(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let who = names.join(", ");
    Some(format!(
        "Couldn't add {who} due to their privacy settings. Share an invite link instead."
    ))
}

fn resolve_input_users(account: &Account, people: &[String]) -> Result<Vec<(Who, tl::enums::InputUser)>, Fault> {
    let mut out = Vec::with_capacity(people.len());
    for raw in people {
        let Some((who @ Who::User(_), None)) = Who::parse(raw) else {
            return Err(Fault::Net(format!("Unknown Telegram person: {raw}")));
        };
        account.input(who)?;
        let input = account
            .dir()
            .input_user(who)
            .ok_or_else(|| Fault::Rpc { code: 400, name: "USER_ID_INVALID".to_owned(), value: None })?;
        out.push((who, input));
    }
    Ok(out)
}

fn names_of_missing(account: &Account, missing: &[tl::enums::MissingInvitee]) -> Vec<String> {
    let dir = account.dir();
    missing
        .iter()
        .map(|m| {
            let tl::enums::MissingInvitee::Invitee(m) = m;
            let name = dir.name(Who::User(m.user_id));
            if name.is_empty() {
                format!("user {}", m.user_id)
            } else {
                name
            }
        })
        .collect()
}

/// Creates a Telegram basic group (`messages.createChat`), records its row, and posts a note
/// if Telegram returned any `missing_invitees`.
pub(super) fn start_group(account: &Account, people: &[String], title: Option<&str>) -> Result<String, Fault> {
    let resolved = resolve_input_users(account, people)?;
    if resolved.is_empty() {
        return Err(Fault::Net("Pick at least one person for the group.".to_owned()));
    }
    let title = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_owned(),
        None => {
            let dir = account.dir();
            let names: Vec<String> = resolved
                .iter()
                .take(4)
                .map(|(who, _)| {
                    let full = dir.name(*who);
                    full.split_whitespace().next().unwrap_or(&full).to_owned()
                })
                .filter(|n| !n.is_empty())
                .collect();
            if names.is_empty() { "New group".to_owned() } else { names.join(", ") }
        }
    };
    let users: Vec<tl::enums::InputUser> = resolved.into_iter().map(|(_, u)| u).collect();
    let tl::enums::messages::InvitedUsers::Users(invited) = account.ask(&tl::functions::messages::CreateChat {
        users,
        title,
        ttl_period: None,
    })?;
    super::learn_updates(account, &invited.updates);
    super::live::answered(account, &invited.updates);
    let who = created_peer(&invited.updates)
        .ok_or_else(|| Fault::Net("Telegram created the group but did not name it.".to_owned()))?;
    super::sync::ensure_row(account, who, None);
    let id = who.text();
    let missing_names = names_of_missing(account, &invited.missing_invitees);
    if let Some(note) = missing_invitee_words(&missing_names) {
        let now = crate::hub::now();
        let mut msg = protocol::Message::plain(format!("tg-missing-{now}"), String::new(), note, now, false);
        msg.kind = protocol::MessageKind::System;
        let _ = account.ctx.message(&id, &msg);
    }
    Ok(id)
}

/// Creates a Telegram broadcast channel or supergroup (`channels.createChannel`), optionally
/// claiming a public `@username` (`channels.checkUsername` + `channels.updateUsername`).
pub(super) fn create_channel(
    account: &Account,
    name: &str,
    kind: protocol::ConversationKind,
    private: bool,
    description: Option<&str>,
    username: Option<&str>,
) -> Result<String, Fault> {
    let broadcast = !matches!(kind, protocol::ConversationKind::Group | protocol::ConversationKind::Forum);
    let megagroup = !broadcast;
    let forum = kind == protocol::ConversationKind::Forum;
    let answer = account.ask(&tl::functions::channels::CreateChannel {
        broadcast,
        megagroup,
        for_import: false,
        forum,
        title: name.to_owned(),
        about: description.unwrap_or("").trim().to_owned(),
        geo_point: None,
        address: None,
        ttl_period: None,
    })?;
    super::learn_updates(account, &answer);
    super::live::answered(account, &answer);
    let who = created_peer(&answer)
        .ok_or_else(|| Fault::Net("Telegram created the channel but did not name it.".to_owned()))?;
    if !private {
        if let Some(raw_user) = username.map(|u| u.trim().trim_start_matches('@')).filter(|u| !u.is_empty()) {
            let valid = (5..=32).contains(&raw_user.len())
                && raw_user.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
            if !valid {
                return Err(Fault::Net(
                    "Telegram channel usernames must be 5–32 letters, digits, or underscores.".to_owned(),
                ));
            }
            let input_channel = account.input_channel(who)?;
            let available = account.ask(&tl::functions::channels::CheckUsername {
                channel: input_channel.clone(),
                username: raw_user.to_owned(),
            })?;
            if !available {
                return Err(Fault::Net(format!("@{raw_user} is already taken or unavailable.")));
            }
            account.ask(&tl::functions::channels::UpdateUsername {
                channel: input_channel.clone(),
                username: raw_user.to_owned(),
            })?;
            if let Ok(found) = account.ask(&tl::functions::channels::GetChannels { id: vec![input_channel] }) {
                account.learn(&[], &found.chats());
            }
        }
    }
    super::sync::ensure_row(account, who, None);
    Ok(who.text())
}

/// Adds people to an existing Telegram basic group (`messages.addChatUser`) or channel/supergroup
/// (`channels.inviteToChannel`). Surfaces privacy / `missing_invitees` / `PEER_FLOOD` refusals.
pub(super) fn add_members(account: &Account, conversation: &str, people: &[String]) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(invalid)?;
    let resolved = resolve_input_users(account, people)?;
    if resolved.is_empty() {
        return Err(Fault::Net("Pick at least one person to add.".to_owned()));
    }
    let mut missing_names: Vec<String> = Vec::new();
    match who {
        Who::Group(chat_id) => {
            for (person_who, user_id) in resolved {
                match account.ask(&tl::functions::messages::AddChatUser {
                    chat_id,
                    user_id,
                    fwd_limit: 50,
                }) {
                    Ok(tl::enums::messages::InvitedUsers::Users(invited)) => {
                        super::learn_updates(account, &invited.updates);
                        super::live::answered(account, &invited.updates);
                        missing_names.extend(names_of_missing(account, &invited.missing_invitees));
                    }
                    Err(fault) if fault.is("USER_PRIVACY_RESTRICTED") || fault.is("USER_NOT_MUTUAL_CONTACT") => {
                        let name = account.dir().name(person_who);
                        missing_names.push(if name.is_empty() { person_who.text() } else { name });
                    }
                    Err(fault) if fault.is("PEER_FLOOD") => {
                        return Err(Fault::Net(
                            "Telegram rate-limited adding people. Share an invite link instead.".to_owned(),
                        ));
                    }
                    Err(fault) => return Err(fault),
                }
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let users: Vec<tl::enums::InputUser> = resolved.into_iter().map(|(_, u)| u).collect();
            match account.ask(&tl::functions::channels::InviteToChannel { channel, users }) {
                Ok(tl::enums::messages::InvitedUsers::Users(invited)) => {
                    super::learn_updates(account, &invited.updates);
                    super::live::answered(account, &invited.updates);
                    missing_names.extend(names_of_missing(account, &invited.missing_invitees));
                }
                Err(fault) if fault.is("PEER_FLOOD") => {
                    return Err(Fault::Net(
                        "Telegram rate-limited adding people. Share an invite link instead.".to_owned(),
                    ));
                }
                Err(fault) => return Err(fault),
            }
        }
        Who::User(_) => {
            return Err(Fault::Net("Direct chats cannot add more members. Create a group instead.".to_owned()));
        }
    }
    account.live().roster = None;
    if let Some(note) = missing_invitee_words(&missing_names) {
        return Err(Fault::Net(note));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_invitee_words_formats_privacy_refusal() {
        assert_eq!(missing_invitee_words(&[]), None);
        let note = missing_invitee_words(&["Tobin Reyes".into(), "Nia Pell".into()]).unwrap();
        assert!(note.contains("Tobin Reyes, Nia Pell"));
        assert!(note.contains("invite link"));
    }
}
