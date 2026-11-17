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
