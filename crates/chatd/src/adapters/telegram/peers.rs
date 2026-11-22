//! Who and what the account has seen: the people, groups and channels Telegram named, with the
//! access hash each needs before it can be addressed again.
//!
//! Ids, as the contract sees them: a person is `u<id>`; a conversation is `u<id>` (the direct
//! chat with that person), `g<id>` (a basic group), `c<id>` (a supergroup or a channel) or
//! `c<id>_<topic>` (one topic of a forum, a `Thread`). The account is the user's own id.

use crate::store::{PersonDetails, PersonRow};
use grammers_tl_types as tl;
use protocol::Mark;
use std::collections::HashMap;

/// Telegram's @replies peer, on the production network and the test network.
pub fn is_replies(who: Who) -> bool {
    matches!(who, Who::User(1_271_266_957 | 708_513))
}

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

/// Call capability flags from `users.getFullUser`. `None` on [`Known`] means the full profile
/// has not been fetched yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallFlags {
    pub phone_available: bool,
    pub phone_private: bool,
    pub video_available: bool,
}

/// Subset of Telegram's `ChatBannedRights` flags relevant to composing and sending.
/// Stored as a plain `Eq` struct so [`Known`] keeps `Eq` without depending on TL derives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BannedRights {
    pub send_messages: bool,
    pub send_plain: bool,
    pub send_media: bool,
    pub send_photos: bool,
    pub send_videos: bool,
    pub send_roundvideos: bool,
    pub send_audios: bool,
    pub send_voices: bool,
    pub send_docs: bool,
    pub send_stickers: bool,
    pub send_gifs: bool,
    pub embed_links: bool,
    pub send_polls: bool,
    pub until_date: i32,
}

impl BannedRights {
    pub fn from_tl(rights: &tl::enums::ChatBannedRights) -> BannedRights {
        let tl::enums::ChatBannedRights::Rights(r) = rights;
        BannedRights {
            send_messages: r.send_messages,
            send_plain: r.send_plain,
            send_media: r.send_media,
            send_photos: r.send_photos,
            send_videos: r.send_videos,
            send_roundvideos: r.send_roundvideos,
            send_audios: r.send_audios,
            send_voices: r.send_voices,
            send_docs: r.send_docs,
            send_stickers: r.send_stickers,
            send_gifs: r.send_gifs,
            embed_links: r.embed_links,
            send_polls: r.send_polls,
            until_date: r.until_date,
        }
    }

    pub fn active(&self, now: i64) -> bool {
        self.until_date == 0 || i64::from(self.until_date) > now
    }

    fn apply(&self, rules: &mut protocol::SendRules) {
        if self.send_messages {
            rules.send_messages = false;
        }
        if self.send_plain {
            rules.send_plain = false;
        }
        if self.send_media {
            rules.send_media = false;
            rules.send_photos = false;
            rules.send_videos = false;
            rules.send_round_videos = false;
            rules.send_audios = false;
            rules.send_voices = false;
            rules.send_files = false;
        }
        if self.send_photos {
            rules.send_photos = false;
        }
        if self.send_videos {
            rules.send_videos = false;
        }
        if self.send_roundvideos {
            rules.send_round_videos = false;
        }
        if self.send_audios {
            rules.send_audios = false;
        }
        if self.send_voices {
            rules.send_voices = false;
        }
        if self.send_docs {
            rules.send_files = false;
        }
        if self.send_stickers {
            rules.send_stickers = false;
        }
        if self.send_gifs {
            rules.send_gifs = false;
        }
        if self.embed_links {
            rules.embed_links = false;
        }
        if self.send_polls {
            rules.send_polls = false;
        }
    }
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
    /// Membership from a full chat description. A partial channel does not say.
    pub member: Option<bool>,
    /// We may read but not write: a channel we do not run, a group we were removed from.
    pub read_only: bool,
    /// We created this group or channel.
    pub creator: bool,
    /// We hold admin rights in this group or channel.
    pub admin: bool,
    /// Group-wide default send restrictions (apply to non-admin members).
    pub default_banned: Option<BannedRights>,
    /// Personal send restrictions placed on us in a supergroup/channel.
    pub personal_banned: Option<BannedRights>,
    /// Per-conversation moderation / pin rights from Telegram's creator/admin/banned flags.
    pub rights: protocol::ConversationRights,
    /// The picture's id and the datacenter that serves it. 0 for none.
    pub photo: i64,
    pub photo_dc: i32,
    pub deleted: bool,
    /// Telegram's own word on the account: verified, or flagged as a scam or an impostor.
    pub mark: Option<Mark>,
    /// Telegram Premium. Ours decides what this account may send. Someone else's is shown on their row.
    pub premium: bool,
    /// Rough last-seen words ("last seen recently"). Empty when Telegram gave an exact time,
    /// or none. Never shown on our own row.
    pub seen: String,
    /// Call availability and privacy flags from `users.getFullUser`.
    pub call_flags: Option<CallFlags>,
    /// Protected chat: Telegram refuses forwarding or saving content from here.
    pub noforwards: bool,
}

/// The tag Telegram's flags give: a warning wins over a tick.
fn mark(verified: bool, scam: bool, fake: bool) -> Option<Mark> {
    match (scam, fake, verified) {
        (true, ..) => Some(Mark::Scam),
        (_, true, _) => Some(Mark::Fake),
        (_, _, true) => Some(Mark::Verified),
        _ => None,
    }
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

    /// Merges group `default_banned` (for non-admins) and `personal_banned` (for non-creators),
    /// slow-mode fields from `ChannelFull`, and paid-message star price into [`protocol::SendRules`].
    pub fn send_rules(
        &self,
        now: i64,
        slowmode_seconds: u32,
        slowmode_next_send: Option<i64>,
        paid_stars: Option<u64>,
    ) -> protocol::SendRules {
        let mut rules = protocol::SendRules::default();
        if self.broadcast && self.read_only {
            rules.send_messages = false;
            return rules;
        }
        if !self.creator && !self.admin {
            if let Some(default_banned) = self.default_banned.filter(|r| r.active(now)) {
                default_banned.apply(&mut rules);
                if default_banned.send_messages && default_banned.until_date > 0 {
                    rules.ban_until = Some(i64::from(default_banned.until_date));
                }
            }
            if slowmode_seconds > 0 {
                rules.slowmode_seconds = slowmode_seconds;
                rules.slowmode_next_send = slowmode_next_send.filter(|&ts| ts > now);
            }
            if let Some(stars) = paid_stars.filter(|&s| s > 0) {
                rules.paid_stars = Some(stars);
            }
        }
        if !self.creator {
            if let Some(personal) = self.personal_banned.filter(|r| r.active(now)) {
                personal.apply(&mut rules);
                if personal.send_messages && personal.until_date > 0 {
                    let until = i64::from(personal.until_date);
                    rules.ban_until = Some(rules.ban_until.map_or(until, |prev| prev.max(until)));
                }
            }
        }
        if self.read_only {
            rules.send_messages = false;
        }
        rules
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

/// Telegram's rough last-seen words. An empty status is not a guess at "long ago".
pub(super) fn rough_seen(status: Option<&tl::enums::UserStatus>) -> String {
    match status {
        Some(tl::enums::UserStatus::Recently(_)) => "last seen recently".to_owned(),
        Some(tl::enums::UserStatus::LastWeek(_)) => "last seen within a week".to_owned(),
        Some(tl::enums::UserStatus::LastMonth(_)) => "last seen within a month".to_owned(),
        _ => String::new(),
    }
}

/// The line under a name. Rough last-seen words take the status line; the Premium badge stays
/// the line when there are none. Our own row stays empty.
pub(super) fn row_status(mine: bool, premium: bool, seen: &str) -> String {
    if mine {
        return String::new();
    }
    if !seen.is_empty() {
        return seen.to_owned();
    }
    super::premium::badge(premium, false).to_owned()
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
        mark: mark(user.verified, user.scam, user.fake),
        premium: user.premium,
        seen: rough_seen(user.status.as_ref()),
        ..Known::default()
    }
}

fn chat_photo(photo: &tl::enums::ChatPhoto) -> (i64, i32) {
    match photo {
        tl::enums::ChatPhoto::Photo(photo) => (photo.photo_id, photo.dc_id),
        tl::enums::ChatPhoto::Empty => (0, 0),
    }
}

/// Whether a specific ban flag in `rights` is currently active (`until_date == 0` never expires).
fn banned_active(rights: Option<&tl::enums::ChatBannedRights>, check: impl Fn(&tl::types::ChatBannedRights) -> bool) -> bool {
    let Some(tl::enums::ChatBannedRights::Rights(rights)) = rights else {
        return false;
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|age| age.as_secs()).unwrap_or(0);
    check(rights) && (rights.until_date == 0 || u64::try_from(rights.until_date).unwrap_or(0) > now)
}

/// `send_messages` is on, and the ban has not expired. `until_date` of 0 does not expire.
fn send_banned(rights: &tl::enums::ChatBannedRights) -> bool {
    banned_active(Some(rights), |r| r.send_messages)
}

/// A member cannot post. The creator always can. A personal ban stops that one member. The
/// group's own ban stops everyone who is not an admin.
fn member_cannot_send(creator: bool, admin: bool, default_banned: Option<&tl::enums::ChatBannedRights>, personal: Option<&tl::enums::ChatBannedRights>) -> bool {
    if creator {
        return false;
    }
    personal.is_some_and(send_banned) || (!admin && default_banned.is_some_and(send_banned))
}

fn of_chat(chat: &tl::enums::Chat) -> Option<(Who, Known)> {
    use tl::enums::Chat;
    Some(match chat {
        Chat::Empty(_) => return None,
        Chat::Chat(chat) => {
            let (photo, photo_dc) = chat_photo(&chat.photo);
            let admin = match &chat.admin_rights {
                Some(tl::enums::ChatAdminRights::Rights(r)) => Some(r),
                None => None,
            };
            let pin_banned = matches!(
                &chat.default_banned_rights,
                Some(tl::enums::ChatBannedRights::Rights(r)) if r.pin_messages
            );
            let invite_banned = matches!(
                &chat.default_banned_rights,
                Some(tl::enums::ChatBannedRights::Rights(r)) if r.invite_users
            );
            let active = !chat.left && !chat.deactivated;
            let can_info = chat.creator || admin.is_some_and(|r| r.change_info);
            let can_ban = chat.creator || admin.is_some_and(|r| r.ban_users);
            let rights = protocol::ConversationRights {
                pin: active && (chat.creator || admin.is_some_and(|r| r.pin_messages) || !pin_banned),
                delete_others: active && (chat.creator || admin.is_some_and(|r| r.delete_messages)),
                manage_members: active && (chat.creator || admin.is_some_and(|r| r.ban_users || r.invite_users)),
                add_members: active && (chat.creator || admin.is_some_and(|r| r.invite_users) || !invite_banned),
                remove_members: active && can_ban,
                rename: active && can_info,
                edit_topic: active && can_info,
                edit_picture: active && can_info,
                ban_members: active && can_ban,
                // Basic groups have no channel admin log, `channels.editAdmin`, or slow mode.
                promote_admins: false,
                manage_settings: false,
                view_admin_log: false,
                transfer_owner: active && chat.creator,
                anonymous: active && admin.is_some_and(|r| r.anonymous),
                ..protocol::ConversationRights::default()
            };
            let known = Known {
                name: chat.title.clone(),
                member: Some(active),
                read_only: chat.left || chat.deactivated || member_cannot_send(chat.creator, chat.admin_rights.is_some(), chat.default_banned_rights.as_ref(), None),
                creator: chat.creator,
                admin: chat.admin_rights.is_some(),
                default_banned: chat.default_banned_rights.as_ref().map(BannedRights::from_tl),
                rights,
                photo,
                photo_dc,
                noforwards: chat.noforwards,
                ..Known::default()
            };
            (Who::Group(chat.id), known)
        }
        Chat::Forbidden(chat) => {
            let known = Known { name: chat.title.clone(), member: Some(false), read_only: true, ..Known::default() };
            (Who::Group(chat.id), known)
        }
        Chat::Channel(channel) => {
            let (photo, photo_dc) = chat_photo(&channel.photo);
            let admin = match &channel.admin_rights {
                Some(tl::enums::ChatAdminRights::Rights(r)) => Some(r),
                None => None,
            };
            // In a channel proper only its owners and admins with the right post. A megagroup
            // is a channel too: members cannot post when the group, or their own rights, say so.
            let may_post = channel.creator || admin.is_some_and(|r| r.post_messages);
            let restricted = if channel.broadcast {
                !may_post
            } else {
                member_cannot_send(channel.creator, channel.admin_rights.is_some(), channel.default_banned_rights.as_ref(), channel.banned_rights.as_ref())
            };
            let pin_banned = matches!(
                &channel.default_banned_rights,
                Some(tl::enums::ChatBannedRights::Rights(r)) if r.pin_messages
            ) || matches!(
                &channel.banned_rights,
                Some(tl::enums::ChatBannedRights::Rights(r)) if r.pin_messages
            );
            let topics_banned = banned_active(channel.default_banned_rights.as_ref(), |r| r.manage_topics)
                || banned_active(channel.banned_rights.as_ref(), |r| r.manage_topics);
            let invite_banned = matches!(
                &channel.default_banned_rights,
                Some(tl::enums::ChatBannedRights::Rights(r)) if r.invite_users
            ) || matches!(
                &channel.banned_rights,
                Some(tl::enums::ChatBannedRights::Rights(r)) if r.invite_users
            );
            let active = !channel.left;
            let can_info = channel.creator || admin.is_some_and(|r| r.change_info);
            let can_ban = channel.creator || admin.is_some_and(|r| r.ban_users);
            let rights = protocol::ConversationRights {
                pin: active
                    && if channel.broadcast {
                        channel.creator || admin.is_some_and(|r| r.edit_messages || r.pin_messages)
                    } else {
                        channel.creator || admin.is_some_and(|r| r.pin_messages) || !pin_banned
                    },
                delete_others: active && (channel.creator || admin.is_some_and(|r| r.delete_messages)),
                manage_members: active && (channel.creator || admin.is_some_and(|r| r.ban_users || r.invite_users)),
                manage_topics: active && channel.forum && (channel.creator || admin.is_some_and(|r| r.manage_topics) || !topics_banned),
                add_members: active
                    && if channel.broadcast {
                        channel.creator || admin.is_some_and(|r| r.invite_users)
                    } else {
                        channel.creator || admin.is_some_and(|r| r.invite_users) || !invite_banned
                    },
                remove_members: active && can_ban,
                rename: active && can_info,
                edit_topic: active && can_info,
                edit_picture: active && can_info,
                ban_members: active && can_ban,
                promote_admins: active && (channel.creator || admin.is_some_and(|r| r.add_admins)),
                manage_settings: active && can_info,
                view_admin_log: active && (channel.creator || admin.is_some()),
                transfer_owner: active && channel.creator,
                anonymous: active && admin.is_some_and(|r| r.anonymous),
                ..protocol::ConversationRights::default()
            };
            let known = Known {
                hash: channel.access_hash.filter(|_| !channel.min),
                name: channel.title.clone(),
                handle: first_username(&channel.username, &channel.usernames),
                broadcast: channel.broadcast,
                forum: channel.forum,
                member: (!channel.min).then_some(!channel.left),
                read_only: channel.left || restricted,
                creator: channel.creator,
                admin: channel.admin_rights.is_some(),
                default_banned: channel.default_banned_rights.as_ref().map(BannedRights::from_tl),
                personal_banned: channel.banned_rights.as_ref().map(BannedRights::from_tl),
                rights,
                photo,
                photo_dc,
                mark: mark(channel.verified, channel.scam, channel.fake),
                noforwards: channel.noforwards,
                ..Known::default()
            };
            (Who::Channel(channel.id), known)
        }
        Chat::ChannelForbidden(channel) => {
            let known = Known {
                hash: Some(channel.access_hash),
                name: channel.title.clone(),
                broadcast: channel.broadcast,
                member: Some(false),
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
    /// The tags of those people, for their rows beyond `PersonRow`.
    pub details: Vec<(String, PersonDetails)>,
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

    /// The peer whose public username is `handle`, without the `@`.
    pub fn by_handle(&self, handle: &str) -> Option<Who> {
        let want = handle.trim_start_matches('@');
        if want.is_empty() {
            return None;
        }
        self.map.iter().find(|(_, known)| known.handle.eq_ignore_ascii_case(want)).map(|(who, _)| *who)
    }

    /// People this account has learned, id then what is known of them.
    pub fn users(&self) -> Vec<(i64, Known)> {
        self.map.iter().filter_map(|(who, known)| match who { Who::User(id) => Some((*id, known.clone())), _ => None }).collect()
    }

    /// Finds a known peer (user or channel) by public username, case-insensitively.
    pub fn find_by_handle(&self, handle: &str) -> Option<Who> {
        let bare = handle.trim().trim_start_matches('@');
        if bare.is_empty() {
            return None;
        }
        self.map
            .iter()
            .find(|(_, known)| !known.handle.is_empty() && known.handle.eq_ignore_ascii_case(bare))
            .map(|(who, _)| *who)
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
            known.call_flags = before.and_then(|before| before.call_flags);
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
            learned.people.push(shown_person(self.me, who, &known));
            let title = if known.premium { super::premium::BADGE.to_owned() } else { String::new() };
            learned.details.push((who.text(), PersonDetails { mark: known.mark, title, ..PersonDetails::default() }));
            self.map.insert(who, known);
        }
        for chat in chats {
            let Some((who, mut known)) = of_chat(chat) else {
                continue;
            };
            let before = self.map.get(&who);
            known.call_flags = before.and_then(|before| before.call_flags);
            if known.hash.is_none() {
                known.hash = before.and_then(|before| before.hash);
            }
            if matches!(chat, tl::enums::Chat::Channel(channel) if channel.min) {
                known.member = before.and_then(|before| before.member);
                if let Some(before) = before {
                    known.read_only = before.read_only;
                    known.creator = before.creator;
                    known.admin = before.admin;
                    known.default_banned = before.default_banned;
                    known.personal_banned = before.personal_banned;
                    known.rights = before.rights;
                }
            }
            if before == Some(&known) {
                continue;
            }
            if before.map(|before| before.hash) != Some(known.hash) {
                learned.notes.extend(known.note().map(|note| (note_key(who), note)));
            }
            if before.is_some() || known.member == Some(false) {
                learned.chats.push(who);
            }
            self.map.insert(who, known);
        }
        learned
    }

    pub fn set_call_flags(&mut self, who: Who, flags: CallFlags) -> bool {
        let known = self.map.entry(who).or_default();
        if known.call_flags == Some(flags) {
            return false;
        }
        known.call_flags = Some(flags);
        true
    }

    pub fn left(&mut self, who: Who) {
        let known = self.map.entry(who).or_default();
        known.member = Some(false);
        known.read_only = true;
    }

    /// A peer remembered only as a note from an earlier run.
    pub fn recall(&mut self, who: Who, note: &str) {
        let Some((hash, name)) = note.split_once('\t') else {
            return;
        };
        if let Ok(hash) = hash.parse::<i64>() {
            let known = self.map.entry(who).or_insert_with(|| Known { name: name.to_owned(), ..Known::default() });
            known.hash.get_or_insert(hash);
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

    /// A last-seen wording changed. The row is republished when the words did.
    pub fn note_seen(&mut self, who: Who, seen: String) -> Option<PersonRow> {
        let me = self.me;
        let known = self.map.get_mut(&who)?;
        if known.seen == seen {
            return None;
        }
        known.seen = seen;
        Some(shown_person(me, who, known))
    }

    /// A name or username told on its own, without a whole user. An unknown person is left
    /// alone: the next user object still carries the hash.
    pub fn rename(&mut self, user_id: i64, first: &str, last: &str, usernames: &[tl::enums::Username]) -> Option<PersonRow> {
        let me = self.me;
        let known = self.map.get_mut(&Who::User(user_id))?;
        let first = first.trim();
        let last = last.trim();
        let name = match (first.is_empty(), last.is_empty()) {
            (false, false) => format!("{first} {last}"),
            (false, true) => first.to_owned(),
            (true, false) => last.to_owned(),
            (true, true) => known.name.clone(),
        };
        let handle = if usernames.is_empty() {
            known.handle.clone()
        } else {
            first_username(&None, &Some(usernames.to_vec()))
        };
        if known.name == name && known.handle == handle {
            return None;
        }
        known.name = name;
        known.handle = handle;
        Some(shown_person(me, Who::User(user_id), known))
    }

    pub fn input_user(&self, who: Who) -> Option<tl::enums::InputUser> {
        match who {
            Who::User(user_id) if user_id == self.me => Some(tl::enums::InputUser::UserSelf),
            Who::User(user_id) => Some(tl::types::InputUser { user_id, access_hash: self.map.get(&who)?.hash? }.into()),
            _ => None,
        }
    }
}

/// A person as the store keeps them, with Premium named where the row can show it.
/// Our own free account's link is the purchase hand-off. Our own status text stays empty.
pub fn shown_person(me: i64, who: Who, known: &Known) -> PersonRow {
    let mut row = person_row(who, known);
    row.status = row_status(who == Who::User(me), known.premium, &known.seen);
    if who == Who::User(me) && !known.premium {
        row.link = super::premium::OFFER.to_owned();
    }
    row
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
    use grammers_tl_types::{Deserializable, Serializable};

    type Field<'a> = &'a dyn Fn(&mut Vec<u8>);

    fn blank<T: Deserializable>(fields: &[Field]) -> T {
        let mut bytes = Vec::new();
        fields.iter().for_each(|field| field(&mut bytes));
        T::from_bytes(&bytes).expect("a blank value reads back")
    }

    fn put<T: Serializable>(value: T) -> impl Fn(&mut Vec<u8>) {
        move |bytes| value.serialize(bytes)
    }

    /// A megagroup. Flags off, so every optional field is absent.
    fn megagroup(title: &str) -> tl::types::Channel {
        let mut channel: tl::types::Channel =
            blank(&[&put(0u32), &put(0u32), &put(9i64), &put(title.to_owned()), &put(tl::enums::ChatPhoto::Empty), &put(1i32)]);
        channel.megagroup = true;
        channel
    }

    /// `send_messages` banned until `until`. Zero does not expire.
    fn ban_send(until: i32) -> tl::enums::ChatBannedRights {
        let rights: tl::types::ChatBannedRights = blank(&[&put(2u32), &put(until)]);
        rights.into()
    }

    fn admin(post_messages: bool) -> tl::enums::ChatAdminRights {
        let rights: tl::types::ChatAdminRights = blank(&[&put(if post_messages { 2u32 } else { 0 })]);
        rights.into()
    }

    fn read_only(chat: tl::enums::Chat) -> bool {
        of_chat(&chat).expect("a chat").1.read_only
    }

    #[test]
    fn a_megagroup_members_cannot_post_in_is_read_only_and_its_admins_still_can() {
        let mut members = megagroup("/g/'s Tech Memes Chat");
        members.default_banned_rights = Some(ban_send(0));
        assert!(read_only(members.clone().into()));

        let mut creator = members.clone();
        creator.creator = true;
        assert!(!read_only(creator.into()));

        let mut admin_of = members.clone();
        admin_of.admin_rights = Some(admin(false));
        assert!(!read_only(admin_of.into()));

        // A ban that already ended does not lock the chat.
        members.default_banned_rights = Some(ban_send(1));
        assert!(!read_only(members.into()));
    }

    #[test]
    fn a_personal_send_ban_locks_that_member_until_it_ends() {
        let mut channel = megagroup("memes");
        channel.banned_rights = Some(ban_send(0));
        assert!(read_only(channel.clone().into()));

        channel.creator = true;
        assert!(!read_only(channel.clone().into()));
        channel.creator = false;

        channel.banned_rights = Some(ban_send(1));
        assert!(!read_only(channel.into()));
    }

    #[test]
    fn a_basic_group_members_cannot_post_in_is_read_only() {
        let chat = tl::types::Chat {
            creator: false,
            left: false,
            deactivated: false,
            call_active: false,
            call_not_empty: false,
            noforwards: false,
            id: 7,
            title: "crew".into(),
            photo: tl::enums::ChatPhoto::Empty,
            participants_count: 3,
            date: 1,
            version: 1,
            migrated_to: None,
            admin_rights: None,
            default_banned_rights: Some(ban_send(0)),
        };
        assert!(read_only(chat.clone().into()));
        let creator = tl::types::Chat { creator: true, ..chat };
        assert!(!read_only(creator.into()));
    }

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

    #[test]
    fn send_rules_merges_default_and_personal_bans_and_exempts_admins() {
        let mut channel = megagroup("trail");
        let default_rights = tl::types::ChatBannedRights {
            view_messages: false,
            send_messages: false,
            send_media: true,
            send_stickers: true,
            send_gifs: false,
            send_games: false,
            send_inline: false,
            embed_links: true,
            send_polls: true,
            change_info: false,
            invite_users: false,
            pin_messages: false,
            manage_topics: false,
            send_photos: false,
            send_videos: false,
            send_roundvideos: false,
            send_audios: false,
            send_voices: false,
            send_docs: false,
            send_plain: false,
            edit_rank: false,
            send_reactions: false,
            until_date: 0,
        };
        let personal_rights = tl::types::ChatBannedRights {
            send_gifs: true,
            send_messages: true,
            until_date: 2_000,
            ..default_rights.clone()
        };
        channel.default_banned_rights = Some(default_rights.into());
        channel.banned_rights = Some(personal_rights.into());
        let known = of_chat(&channel.clone().into()).expect("a chat").1;
        let rules = known.send_rules(1_500, 30, Some(1_520), Some(15));
        assert!(!rules.send_messages);
        assert!(!rules.send_media);
        assert!(!rules.send_photos && !rules.send_voices && !rules.send_files);
        assert!(!rules.send_stickers && !rules.send_gifs && !rules.embed_links && !rules.send_polls);
        assert_eq!(rules.ban_until, Some(2_000));
        assert_eq!(rules.slowmode_seconds, 30);
        assert_eq!(rules.slowmode_next_send, Some(1_520));
        assert_eq!(rules.paid_stars, Some(15));

        // After the personal mute expires, only group default bans remain.
        let after = known.send_rules(2_500, 30, None, None);
        assert!(!after.send_media && !after.send_stickers && after.send_gifs);
        assert_eq!(after.ban_until, None);

        // Admins bypass default bans, slow mode, and paid stars.
        let mut as_admin = channel.clone();
        as_admin.banned_rights = None;
        as_admin.admin_rights = Some(admin(false));
        let admin_rules = of_chat(&as_admin.into()).expect("a chat").1.send_rules(1_500, 30, Some(1_520), Some(15));
        assert!(admin_rules.send_messages && admin_rules.send_media && admin_rules.send_stickers);
        assert_eq!(admin_rules.slowmode_seconds, 0);
        assert_eq!(admin_rules.paid_stars, None);
    }
}
