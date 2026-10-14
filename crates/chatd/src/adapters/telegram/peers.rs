//! Who and what the account has seen: the people, groups and channels Telegram named, with the
//! access hash each needs before it can be addressed again.
//!
//! Ids, as the contract sees them: a person is `u<id>`; a conversation is `u<id>` (the direct
//! chat with that person), `g<id>` (a basic group), `c<id>` (a supergroup or a channel) or
//! `c<id>_<topic>` (one topic of a forum, a `Thread`). The account is the user's own id.

use crate::store::PersonRow;
use grammers_tl_types as tl;
use std::collections::HashMap;

/// Most peers held in memory. Past this the directory starts over; hashes are also kept as
/// notes in the store, so nothing becomes unreachable.
const KEPT: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Who {
    User(i64),
    Group(i64),
    Channel(i64),
}

impl Who {
    pub fn of(peer: &tl::enums::Peer) -> Who {
        match peer {
            tl::enums::Peer::User(user) => Who::User(user.user_id),
            tl::enums::Peer::Chat(chat) => Who::Group(chat.chat_id),
            tl::enums::Peer::Channel(channel) => Who::Channel(channel.channel_id),
        }
    }

    pub fn text(self) -> String {
        match self {
            Who::User(id) => format!("u{id}"),
            Who::Group(id) => format!("g{id}"),
            Who::Channel(id) => format!("c{id}"),
        }
    }

    /// A conversation id back into its peer and, for a forum's thread, its topic.
    pub fn parse(id: &str) -> Option<(Who, Option<i32>)> {
        let (peer, topic) = match id.split_once('_') {
            Some((peer, topic)) => (peer, Some(topic.parse::<i32>().ok()?)),
            None => (id, None),
        };
        let number = peer.get(1..)?.parse::<i64>().ok()?;
        let who = match peer.as_bytes().first()? {
            b'u' => Who::User(number),
            b'g' => Who::Group(number),
            b'c' => Who::Channel(number),
            _ => return None,
        };
        (topic.is_none() || matches!(who, Who::Channel(_))).then_some((who, topic))
    }

    pub fn peer(self) -> tl::enums::Peer {
        match self {
            Who::User(user_id) => tl::types::PeerUser { user_id }.into(),
            Who::Group(chat_id) => tl::types::PeerChat { chat_id }.into(),
            Who::Channel(channel_id) => tl::types::PeerChannel { channel_id }.into(),
        }
    }
}

/// A forum topic's conversation id.
pub fn thread_id(forum: Who, topic: i32) -> String {
    format!("{}_{topic}", forum.text())
}

/// What is remembered of one peer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    /// `None` for a basic group, which needs none, and for a peer only seen in passing (`min`).
    pub hash: Option<i64>,
    pub name: String,
    /// The public username, without the `@`.
    pub handle: String,
    pub bot: bool,
    pub contact: bool,
    /// A channel proper: only its owners post.
    pub broadcast: bool,
    pub forum: bool,
    /// We may read but not write: a channel we do not run, a group we were removed from.
    pub read_only: bool,
    /// The picture's id and the datacenter that serves it. 0 for none.
    pub photo: i64,
    pub photo_dc: i32,
    pub deleted: bool,
}

impl Known {
    /// The id of the picture as a media file. A changed picture is a new id.
    pub fn avatar(&self) -> Option<String> {
        (self.photo != 0).then(|| format!("a{}", self.photo))
    }

    /// As a note in the store: the hash, then the name.
    fn note(&self) -> Option<String> {
        Some(format!("{}\t{}", self.hash?, self.name))
    }
}

fn user_name(user: &tl::types::User) -> String {
    let first = user.first_name.as_deref().unwrap_or("").trim();
    let last = user.last_name.as_deref().unwrap_or("").trim();
    match (first.is_empty(), last.is_empty(), user.deleted) {
        (_, _, true) => "Deleted Account".to_owned(),
        (false, false, _) => format!("{first} {last}"),
        (false, true, _) => first.to_owned(),
        (true, false, _) => last.to_owned(),
        (true, true, _) => user.username.clone().unwrap_or_else(|| "Unknown".to_owned()),
    }
}

fn first_username(username: &Option<String>, usernames: &Option<Vec<tl::enums::Username>>) -> String {
    let active = usernames.iter().flatten().find_map(|name| {
        let tl::enums::Username::Username(name) = name;
        name.active.then(|| name.username.clone())
    });
    username.clone().or(active).unwrap_or_default()
}

fn of_user(user: &tl::types::User) -> Known {
    let (photo, photo_dc) = match &user.photo {
        Some(tl::enums::UserProfilePhoto::Photo(photo)) => (photo.photo_id, photo.dc_id),
        _ => (0, 0),
    };
    Known {
        hash: user.access_hash.filter(|_| !user.min),
        name: user_name(user),
        handle: first_username(&user.username, &user.usernames),
        bot: user.bot,
        contact: user.contact,
        photo,
        photo_dc,
        deleted: user.deleted,
        ..Known::default()
    }
}

fn chat_photo(photo: &tl::enums::ChatPhoto) -> (i64, i32) {
    match photo {
        tl::enums::ChatPhoto::Photo(photo) => (photo.photo_id, photo.dc_id),
        tl::enums::ChatPhoto::Empty => (0, 0),
    }
}

fn of_chat(chat: &tl::enums::Chat) -> Option<(Who, Known)> {
    use tl::enums::Chat;
    Some(match chat {
        Chat::Empty(_) => return None,
        Chat::Chat(chat) => {
            let (photo, photo_dc) = chat_photo(&chat.photo);
            let known = Known {
                name: chat.title.clone(),
                read_only: chat.left || chat.deactivated,
                photo,
                photo_dc,
                ..Known::default()
            };
            (Who::Group(chat.id), known)
        }
        Chat::Forbidden(chat) => {
            let known = Known { name: chat.title.clone(), read_only: true, ..Known::default() };
            (Who::Group(chat.id), known)
        }
        Chat::Channel(channel) => {
            let (photo, photo_dc) = chat_photo(&channel.photo);
            // In a channel proper only its owners and admins with the right post.
            let may_post = channel.creator
                || matches!(&channel.admin_rights, Some(tl::enums::ChatAdminRights::Rights(rights)) if rights.post_messages);
            let known = Known {
                hash: channel.access_hash.filter(|_| !channel.min),
                name: channel.title.clone(),
                handle: first_username(&channel.username, &channel.usernames),
                broadcast: channel.broadcast,
                forum: channel.forum,
                read_only: channel.left || (channel.broadcast && !may_post),
                photo,
                photo_dc,
                ..Known::default()
            };
            (Who::Channel(channel.id), known)
        }
        Chat::ChannelForbidden(channel) => {
            let known = Known {
                hash: Some(channel.access_hash),
                name: channel.title.clone(),
                broadcast: channel.broadcast,
                read_only: true,
                ..Known::default()
            };
            (Who::Channel(channel.id), known)
        }
    })
}

/// What changed when Telegram named some peers.
#[derive(Default)]
pub struct Learned {
    /// People whose row in the store is now out of date.
    pub people: Vec<PersonRow>,
    /// Hashes to keep as notes, by note key.
    pub notes: Vec<(String, String)>,
    /// Groups and channels whose title, picture or rights changed.
    pub chats: Vec<Who>,
}

#[derive(Default)]
pub struct Directory {
    map: HashMap<Who, Known>,
    pub me: i64,
}

pub fn note_key(who: Who) -> String {
    format!("peer:{}", who.text())
}

impl Directory {
    pub fn new(me: i64) -> Directory {
        Directory { map: HashMap::new(), me }
    }

    pub fn get(&self, who: Who) -> Option<&Known> {
        self.map.get(&who)
    }

    pub fn name(&self, who: Who) -> String {
        self.map.get(&who).map_or_else(|| "Unknown".to_owned(), |known| known.name.clone())
    }

    /// Takes in what an answer or an update named. A peer seen only in passing never replaces
    /// the hash of one seen whole.
    pub fn learn(&mut self, users: &[tl::enums::User], chats: &[tl::enums::Chat]) -> Learned {
        let mut learned = Learned::default();
        if self.map.len() + users.len() + chats.len() > KEPT {
            self.map.clear();
        }
        for user in users {
            let tl::enums::User::User(user) = user else {
                continue;
            };
            let who = Who::User(user.id);
            let mut known = of_user(user);
            let before = self.map.get(&who);
            if known.hash.is_none() {
                known.hash = before.and_then(|before| before.hash);
                // A user seen in passing may come without its picture.
                if user.min && known.photo == 0 {
                    if let Some(before) = before {
                        (known.photo, known.photo_dc) = (before.photo, before.photo_dc);
                    }
                }
            }
            if before == Some(&known) {
                continue;
            }
            if before.map(|before| before.hash) != Some(known.hash) {
                learned.notes.extend(known.note().map(|note| (note_key(who), note)));
            }
            learned.people.push(person_row(who, &known));
            self.map.insert(who, known);
        }
        for chat in chats {
            let Some((who, mut known)) = of_chat(chat) else {
                continue;
            };
            let before = self.map.get(&who);
            if known.hash.is_none() {
                known.hash = before.and_then(|before| before.hash);
            }
            if before == Some(&known) {
                continue;
            }
            if before.map(|before| before.hash) != Some(known.hash) {
                learned.notes.extend(known.note().map(|note| (note_key(who), note)));
            }
            if before.is_some() {
                learned.chats.push(who);
            }
            self.map.insert(who, known);
        }
        learned
    }

    /// A peer remembered only as a note from an earlier run.
    pub fn recall(&mut self, who: Who, note: &str) {
        let Some((hash, name)) = note.split_once('\t') else {
            return;
        };
        if let Ok(hash) = hash.parse::<i64>() {
            self.map.entry(who).or_insert_with(|| Known { hash: Some(hash), name: name.to_owned(), ..Known::default() });
        }
    }

    pub fn input_peer(&self, who: Who) -> Option<tl::enums::InputPeer> {
        Some(match who {
            Who::User(user_id) if user_id == self.me => tl::enums::InputPeer::PeerSelf,
            Who::User(user_id) => tl::types::InputPeerUser { user_id, access_hash: self.map.get(&who)?.hash? }.into(),
            Who::Group(chat_id) => tl::types::InputPeerChat { chat_id }.into(),
            Who::Channel(channel_id) => {
                tl::types::InputPeerChannel { channel_id, access_hash: self.map.get(&who)?.hash? }.into()
            }
        })
    }

    pub fn input_channel(&self, who: Who) -> Option<tl::enums::InputChannel> {
        match who {
            Who::Channel(channel_id) => {
                Some(tl::types::InputChannel { channel_id, access_hash: self.map.get(&who)?.hash? }.into())
            }
            _ => None,
        }
    }

    pub fn input_user(&self, who: Who) -> Option<tl::enums::InputUser> {
        match who {
            Who::User(user_id) if user_id == self.me => Some(tl::enums::InputUser::UserSelf),
            Who::User(user_id) => Some(tl::types::InputUser { user_id, access_hash: self.map.get(&who)?.hash? }.into()),
            _ => None,
        }
    }
}

/// A person as the store keeps them. The link opens them in Telegram's own app.
pub fn person_row(who: Who, known: &Known) -> PersonRow {
    let id = match who {
        Who::User(id) | Who::Group(id) | Who::Channel(id) => id,
    };
    PersonRow {
        id: who.text(),
        name: known.name.clone(),
        handle: if known.handle.is_empty() { String::new() } else { format!("@{}", known.handle) },
        avatar: known.avatar(),
        bot: known.bot,
        status: String::new(),
        link: match known.handle.as_str() {
            "" => format!("tg://user?id={id}"),
            handle => format!("https://t.me/{handle}"),
        },
    }
}

/// Where a message opens in Telegram's own app.
pub fn message_link(who: Who, known: Option<&Known>, message: i32) -> String {
    match (who, known.map(|known| known.handle.as_str()).filter(|handle| !handle.is_empty())) {
        (Who::Channel(_), Some(handle)) => format!("https://t.me/{handle}/{message}"),
        (Who::Channel(id), None) => format!("https://t.me/c/{id}/{message}"),
        (Who::User(id), _) => format!("tg://openmessage?user_id={id}&message_id={message}"),
        (Who::Group(id), _) => format!("tg://openmessage?chat_id={id}&message_id={message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_go_both_ways_and_a_topic_needs_a_channel() {
        assert_eq!(Who::parse("u42"), Some((Who::User(42), None)));
        assert_eq!(Who::parse("c9_31"), Some((Who::Channel(9), Some(31))));
        assert_eq!(Who::parse(&thread_id(Who::Channel(9), 1)), Some((Who::Channel(9), Some(1))));
        assert_eq!(Who::parse("g7_3"), None);
        assert_eq!(Who::parse("x1"), None);
        assert_eq!(Who::parse(""), None);
        assert_eq!(Who::Group(7).text(), "g7");
        assert_eq!(message_link(Who::Channel(9), None, 4), "https://t.me/c/9/4");
        assert_eq!(message_link(Who::User(3), None, 4), "tg://openmessage?user_id=3&message_id=4");
    }
}
