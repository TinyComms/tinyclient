//! What is asked of Telegram: who we are, the chats and their folders, a page of history, a
//! forum's topics, a channel's sponsored message. Everything goes to the store as it is read.

use super::content::{self, Mapped, Want};
use super::peers::{thread_id, Known, Who};
use super::wire::Fault;
use super::{presence, Account, PAGE, SPACE};
use crate::store::{ConversationRow, SpaceRow};
use grammers_tl_types as tl;
use protocol::{CallSupport, Card, ConversationKind, Group, MediaFile, MediaKind, Message, Relation, ReplyTo, Span, Style};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Chats read at sign-on, a hundred a request. Older ones appear when something happens in them.
const DIALOG_PAGES: usize = 5;
/// Chats whose pictures are fetched at sign-on: about what the first screen of the Inbox shows.
const FIRST_SCREEN: usize = 40;
/// Topics read when a forum is opened.
const TOPICS: i32 = 50;
/// A `HistoryAfter` for the same place is not asked again within this long.
const AFTER_FRESH: Duration = Duration::from_secs(60);

fn now() -> i64 {
    crate::hub::now()
}

/// Whether a chat's own setting, or the default for its kind, silences it.
fn muted(account: &Account, who: Who, settings: &tl::enums::PeerNotifySettings) -> bool {
    let tl::enums::PeerNotifySettings::Settings(settings) = settings;
    match settings.mute_until {
        Some(until) => i64::from(until) > now(),
        None => {
            let broadcast = account.dir().get(who).is_some_and(|known| known.broadcast);
            let kind = match who {
                Who::User(_) => 0,
                Who::Channel(_) if broadcast => 2,
                _ => 1,
            };
            account.live().muted[kind]
        }
    }
}

pub(super) fn is_muted(account: &Account, who: Who, settings: &tl::enums::PeerNotifySettings) -> bool {
    muted(account, who, settings)
}

/// What kind of row a peer is.
fn kind(who: Who, known: &Known) -> ConversationKind {
    match who {
        Who::User(_) => ConversationKind::Direct,
        Who::Group(_) => ConversationKind::Group,
        Who::Channel(_) if known.forum => ConversationKind::Forum,
        Who::Channel(_) if known.broadcast => ConversationKind::Channel,
        Who::Channel(_) => ConversationKind::Group,
    }
}

fn row(account: &Account, who: Who, known: &Known) -> ConversationRow {
    let mut row = ConversationRow::new(who.text(), kind(who, known), known.name.clone());
    if who == Who::User(account.me) {
        row.title = "Saved Messages".to_owned();
    }
    row.space = Some(SPACE.to_owned());
    row.avatar = known.avatar();
    // Every Telegram chat is in the Inbox, channels included.
    row.listed = true;
    row.read_only = known.read_only;
    if let Who::User(id) = who {
        row.peer = Some(who.text());
        // Calls are a later stage: until then a person's call opens in Telegram's own app.
        if id != account.me && !known.bot && !known.deleted {
            row.call = CallSupport::Handoff;
        }
    }
    row
}

/// Records a conversation the store does not have yet: a chat that was not among those read at
/// sign-on, a person the user is about to write to. True once it is there.
pub(super) fn ensure_row(account: &Account, who: Who, topic: Option<i32>) -> bool {
    let id = match topic {
        Some(topic) => thread_id(who, topic),
        None => who.text(),
    };
    if matches!(account.ctx.known(&id), Ok(Some(_))) {
        return true;
    }
    if account.input(who).is_err() && !matches!(who, Who::Group(_)) {
        return false;
    }
    let Some(known) = account.dir().get(who).cloned() else {
        return false;
    };
    if topic.is_none() {
        return account.ctx.conversation(&row(account, who, &known)).is_ok();
    }
    // A topic nobody has listed yet: the forum is asked once, and a topic it does not list (a
    // new one, heard of before its row) is recorded under a plain name until it does.
    if !matches!(account.ctx.known(&who.text()), Ok(Some(_))) && account.ctx.conversation(&row(account, who, &known)).is_err() {
        return false;
    }
    let _ = topics(account, who);
    if matches!(account.ctx.known(&id), Ok(Some(_))) {
        return true;
    }
    let mut thread = ConversationRow::new(id, ConversationKind::Thread, "Topic");
    thread.space = Some(SPACE.to_owned());
    thread.parent = Some(who.text());
    thread.root = topic.map(|topic| topic.to_string());
    account.ctx.conversation(&thread).is_ok()
}

/// A group or channel was renamed, or its picture or our rights in it changed.
pub(super) fn refresh_row(account: &Account, who: Who) {
    let Ok(Some(stored)) = account.ctx.known(&who.text()) else {
        return;
    };
    let Some(known) = account.dir().get(who).cloned() else {
        return;
    };
    let mut fresh = row(account, who, &known);
    fresh.group = stored.group;
    fresh.position = stored.position;
    fresh.topic = stored.topic;
    let _ = account.ctx.conversation(&fresh);
}

/// The first requests of a connection: are we still signed in, who are we, what is muted by
/// default, the chats, the contacts.
pub(super) fn sign_on(account: &Account) -> Result<(), Fault> {
    account.ask(&tl::functions::updates::GetState {})?;
    let me = account.ask(&tl::functions::users::GetUsers { id: vec![tl::enums::InputUser::UserSelf] })?;
    account.learn(&me, &[]);
    let name = account.dir().name(Who::User(account.me));
    let _ = account.ctx.describe(&name, &Who::User(account.me).text());
    let kinds = [
        tl::enums::InputNotifyPeer::InputNotifyUsers,
        tl::enums::InputNotifyPeer::InputNotifyChats,
        tl::enums::InputNotifyPeer::InputNotifyBroadcasts,
    ];
    for (index, peer) in kinds.into_iter().enumerate() {
        let tl::enums::PeerNotifySettings::Settings(settings) = account.ask(&tl::functions::account::GetNotifySettings { peer })?;
        account.live().muted[index] = settings.mute_until.is_some_and(|until| i64::from(until) > now());
    }
    let _ = account.ctx.spaces(&[SpaceRow { id: SPACE.to_owned(), name: "Telegram".to_owned(), icon: None, position: 0 }]);
    dialogs(account)?;
    contacts(account)
}

/// The contacts, which are the list the user owns on Telegram, and whether each is online.
fn contacts(account: &Account) -> Result<(), Fault> {
    let tl::enums::contacts::Contacts::Contacts(found) = account.ask(&tl::functions::contacts::GetContacts { hash: 0 })? else {
        return Ok(());
    };
    account.learn(&found.users, &[]);
    let mut owned = Vec::with_capacity(found.users.len());
    for user in &found.users {
        let tl::enums::User::User(user) = user else {
            continue;
        };
        let id = Who::User(user.id).text();
        account.ctx.presence(&id, presence(user.status.as_ref()), "", true);
        owned.push((id, Relation::Contact));
    }
    let _ = account.ctx.relations(&owned);
    Ok(())
}

/// Which chats a folder holds.
struct Folder {
    id: i32,
    title: String,
    named: HashSet<Who>,
    left_out: HashSet<Who>,
    contacts: bool,
    strangers: bool,
    groups: bool,
    broadcasts: bool,
    bots: bool,
}

fn input_who(peer: &tl::enums::InputPeer, me: i64) -> Option<Who> {
    Some(match peer {
        tl::enums::InputPeer::PeerSelf => Who::User(me),
        tl::enums::InputPeer::User(user) => Who::User(user.user_id),
        tl::enums::InputPeer::Chat(chat) => Who::Group(chat.chat_id),
        tl::enums::InputPeer::Channel(channel) => Who::Channel(channel.channel_id),
        _ => return None,
    })
}

fn folders(account: &Account) -> Vec<Folder> {
    let Ok(tl::enums::messages::DialogFilters::Filters(found)) = account.ask(&tl::functions::messages::GetDialogFilters {}) else {
        return Vec::new();
    };
    let whos = |peers: &[tl::enums::InputPeer]| peers.iter().filter_map(|peer| input_who(peer, account.me)).collect::<HashSet<Who>>();
    let title = |title: &tl::enums::TextWithEntities| {
        let tl::enums::TextWithEntities::Entities(title) = title;
        title.text.clone()
    };
    let one = |filter: &tl::enums::DialogFilter| match filter {
        tl::enums::DialogFilter::Filter(filter) => Some(Folder {
            id: filter.id,
            title: title(&filter.title),
            named: whos(&filter.pinned_peers).union(&whos(&filter.include_peers)).copied().collect(),
            left_out: whos(&filter.exclude_peers),
            contacts: filter.contacts,
            strangers: filter.non_contacts,
            groups: filter.groups,
            broadcasts: filter.broadcasts,
            bots: filter.bots,
        }),
        tl::enums::DialogFilter::Chatlist(list) => Some(Folder {
            id: list.id,
            title: title(&list.title),
            named: whos(&list.pinned_peers).union(&whos(&list.include_peers)).copied().collect(),
            left_out: HashSet::new(),
            contacts: false,
            strangers: false,
            groups: false,
            broadcasts: false,
            bots: false,
        }),
        tl::enums::DialogFilter::Default => None,
    };
    found.filters.iter().filter_map(one).collect()
}

impl Folder {
    fn holds(&self, who: Who, known: &Known) -> bool {
        if self.left_out.contains(&who) {
            return false;
        }
        self.named.contains(&who)
            || match who {
                Who::User(_) if known.bot => self.bots,
                Who::User(_) if known.contact => self.contacts,
                Who::User(_) => self.strangers,
                Who::Channel(_) if known.broadcast => self.broadcasts,
                _ => self.groups,
            }
    }
}

/// One page of chats: the dialogs with what they need (their newest messages, the peers).
struct Page {
    dialogs: Vec<tl::types::Dialog>,
    newest: HashMap<(Who, i32), tl::enums::Message>,
    /// False when Telegram has no more.
    more: bool,
}

fn page(account: &Account, folder: Option<i32>, offset: (i32, i32, tl::enums::InputPeer)) -> Result<Page, Fault> {
    let request = tl::functions::messages::GetDialogs {
        exclude_pinned: false,
        folder_id: folder,
        offset_date: offset.0,
        offset_id: offset.1,
        offset_peer: offset.2,
        limit: 100,
        hash: 0,
    };
    let (dialogs, messages, chats, users, more) = match account.ask(&request)? {
        tl::enums::messages::Dialogs::Dialogs(all) => (all.dialogs, all.messages, all.chats, all.users, false),
        tl::enums::messages::Dialogs::Slice(slice) => {
            let more = slice.dialogs.len() >= 100;
            (slice.dialogs, slice.messages, slice.chats, slice.users, more)
        }
        tl::enums::messages::Dialogs::NotModified(_) => (Vec::new(), Vec::new(), Vec::new(), Vec::new(), false),
    };
    account.learn(&users, &chats);
    let newest = messages
        .into_iter()
        .filter_map(|message| {
            let (peer, id) = match &message {
                tl::enums::Message::Message(message) => (Who::of(&message.peer_id), message.id),
                tl::enums::Message::Service(message) => (Who::of(&message.peer_id), message.id),
                tl::enums::Message::Empty(_) => return None,
            };
            Some(((peer, id), message))
        })
        .collect();
    let dialogs = dialogs.into_iter().filter_map(|dialog| match dialog {
        tl::enums::Dialog::Dialog(dialog) => Some(dialog),
        // The archive's own row: its chats are read as a folder of their own.
        tl::enums::Dialog::Folder(_) => None,
    });
    Ok(Page { dialogs: dialogs.collect(), newest, more })
}

/// Reads the chats and writes the Space's tree: pinned first, then a group per folder, the
/// rest, and the archive.
// CONTRACT: a row sits under one group, so a chat that is in several folders shows under the
// first of them only; and the Inbox has no place for Telegram's own pin, which is the "Pinned"
// group here (a row's `starred` is the user's local pin and is not the adapter's to set).
pub(super) fn dialogs(account: &Account) -> Result<(), Fault> {
    let mut all: Vec<(tl::types::Dialog, Option<tl::enums::Message>, bool)> = Vec::new();
    for archived in [false, true] {
        let mut offset = (0, 0, tl::enums::InputPeer::Empty);
        for _ in 0..if archived { 1 } else { DIALOG_PAGES } {
            let mut page = page(account, Some(i32::from(archived)), offset)?;
            let Some(last) = page.dialogs.last() else {
                break;
            };
            let who = Who::of(&last.peer);
            let date = match page.newest.get(&(who, last.top_message)) {
                Some(tl::enums::Message::Message(message)) => message.date,
                Some(tl::enums::Message::Service(message)) => message.date,
                _ => 0,
            };
            offset = (date, last.top_message, account.input(who).unwrap_or(tl::enums::InputPeer::Empty));
            let more = page.more;
            for dialog in std::mem::take(&mut page.dialogs) {
                let newest = page.newest.remove(&(Who::of(&dialog.peer), dialog.top_message));
                all.push((dialog, newest, archived));
            }
            if !more || account.ctx.stopped() {
                break;
            }
        }
    }
    let folders = folders(account);
    let mut groups = vec![Group::new("pinned", "Pinned", 0)];
    groups.extend(folders.iter().enumerate().map(|(at, folder)| Group::new(format!("f{}", folder.id), folder.title.clone(), at as i32 + 1)));
    groups.push(Group::new("chats", "Chats", folders.len() as i32 + 1));
    groups.push(Group::new("archived", "Archived", folders.len() as i32 + 2));
    let mut rows = Vec::with_capacity(all.len());
    {
        let dir = account.dir();
        for (dialog, _, archived) in &all {
            let who = Who::of(&dialog.peer);
            let Some(known) = dir.get(who) else {
                continue;
            };
            let mut row = row(account, who, known);
            let folder = folders.iter().position(|folder| folder.holds(who, known));
            let (group, order) = match (archived, dialog.pinned, folder) {
                (true, ..) => ("archived".to_owned(), folders.len() + 2),
                (false, true, _) => ("pinned".to_owned(), 0),
                (false, false, Some(at)) => (format!("f{}", folders[at].id), at + 1),
                (false, false, None) => ("chats".to_owned(), folders.len() + 1),
            };
            row.group = Some(group);
            // Telegram's order within a group is the order it listed the chats in.
            row.position = (order * 100_000 + rows.len()) as i32;
            rows.push(row);
        }
    }
    // A group nothing sits under is not drawn.
    groups.retain(|group| rows.iter().any(|row| row.group.as_deref() == Some(group.id.as_str())));
    account.ctx.tree(SPACE, &groups, &rows).map_err(|err| Fault::Net(err.to_string()))?;
    let mut pictures = Vec::new();
    for (at, (dialog, newest, _)) in all.iter().enumerate() {
        let who = Who::of(&dialog.peer);
        let id = who.text();
        let mut updated = 0;
        if let Some(mapped) = newest.as_ref().and_then(|newest| content::map(&account.dir(), newest)) {
            updated = mapped.message.sent;
            // A forum's newest message belongs to one of its topics, not to the forum's row.
            if !account.dir().get(who).is_some_and(|known| known.forum) {
                let _ = account.ctx.history(&id, std::slice::from_ref(&mapped.message));
            }
        }
        let unread = (dialog.unread_count.max(0) as u32).max(u32::from(dialog.unread_mark));
        let read = (dialog.read_inbox_max_id > 0).then(|| dialog.read_inbox_max_id.to_string());
        let _ = account.ctx.counts(&id, unread, dialog.unread_mentions_count.max(0) as u32, read.as_deref(), updated);
        let _ = account.ctx.muted(&id, muted(account, who, &dialog.notify_settings));
        if at < FIRST_SCREEN {
            pictures.extend(avatar(account, who));
        }
    }
    account.files.want(pictures);
    Ok(())
}

/// A peer's picture as something to fetch.
pub(super) fn avatar(account: &Account, who: Who) -> Option<Want> {
    let dir = account.dir();
    let known = dir.get(who)?;
    let mut file = MediaFile::new(known.avatar()?, MediaKind::Avatar);
    (file.width, file.height) = (160, 160);
    Some(Want {
        file,
        dc: known.photo_dc,
        location: tl::types::InputPeerPhotoFileLocation { big: false, peer: dir.input_peer(who)?, photo_id: known.photo }.into(),
        inline: None,
        row: Some(who.text()),
    })
}

/// A forum's topics, as `Thread` rows under it. Returns how many it has.
fn topics(account: &Account, forum: Who) -> Result<usize, Fault> {
    let request = tl::functions::messages::GetForumTopics {
        peer: account.input(forum)?,
        q: None,
        offset_date: 0,
        offset_id: 0,
        offset_topic: 0,
        limit: TOPICS,
    };
    let tl::enums::messages::ForumTopics::Topics(found) = account.ask(&request)?;
    account.learn(&found.users, &found.chats);
    let dates: HashMap<i32, i64> = found
        .messages
        .iter()
        .filter_map(|message| match message {
            tl::enums::Message::Message(message) => Some((message.id, i64::from(message.date))),
            tl::enums::Message::Service(message) => Some((message.id, i64::from(message.date))),
            tl::enums::Message::Empty(_) => None,
        })
        .collect();
    let mut count = 0;
    for (at, topic) in found.topics.iter().enumerate() {
        let tl::enums::ForumTopic::Topic(topic) = topic else {
            continue;
        };
        let id = thread_id(forum, topic.id);
        let mut thread = ConversationRow::new(id.clone(), ConversationKind::Thread, topic.title.clone());
        thread.space = Some(SPACE.to_owned());
        thread.parent = Some(forum.text());
        thread.root = Some(topic.id.to_string());
        // Pinned topics first, then as Telegram lists them: the most recently active first.
        thread.position = at as i32 - if topic.pinned { TOPICS } else { 0 };
        thread.read_only = topic.closed && !topic.my;
        if account.ctx.conversation(&thread).is_err() {
            continue;
        }
        count += 1;
        let read = (topic.read_inbox_max_id > 0).then(|| topic.read_inbox_max_id.to_string());
        let updated = dates.get(&topic.top_message).copied().unwrap_or(i64::from(topic.date));
        let _ = account.ctx.counts(&id, topic.unread_count.max(0) as u32, topic.unread_mentions_count.max(0) as u32, read.as_deref(), updated);
        let _ = account.ctx.muted(&id, muted(account, forum, &topic.notify_settings));
    }
    Ok(count)
}

/// Where in a conversation a page of history is wanted.
pub(super) enum Around<'a> {
    Newest,
    Before(&'a str),
    After(&'a str),
    At(&'a str),
}

/// Fills in what a reply answers from what is already known: the same page, or the store.
fn quote(account: &Account, conversation: &str, page: &HashMap<i32, (String, String, String)>, reply: i32) -> ReplyTo {
    let id = reply.to_string();
    let known = page.get(&reply).cloned().or_else(|| {
        let stored = account.ctx.stored_message(conversation, &id).ok().flatten()?;
        Some((stored.sender.id, stored.sender.name, stored.text))
    });
    match known {
        Some((person, sender, text)) => ReplyTo {
            id,
            sender,
            snippet: text.chars().take(120).collect(),
            person: (!person.is_empty()).then(|| account.ctx.key(&person)),
            deleted: false,
        },
        // Not on this page and never stored: the row says who and what once it is.
        None => ReplyTo { id, sender: String::new(), snippet: String::new(), person: None, deleted: false },
    }
}

/// Where a message belongs: its chat, or in a forum the thread of its topic.
pub(super) fn conversation_of(account: &Account, mapped: &Mapped) -> String {
    match mapped.peer {
        Who::Channel(_) if account.dir().get(mapped.peer).is_some_and(|known| known.forum) => {
            thread_id(mapped.peer, mapped.topic.unwrap_or(1))
        }
        peer => peer.text(),
    }
}

/// Finishes a mapped message for the conversation it goes to: the reply's line.
pub(super) fn finish(account: &Account, conversation: &str, mapped: &mut Mapped, page: &HashMap<i32, (String, String, String)>) {
    if let Some(reply) = mapped.reply {
        mapped.message.reply_to = Some(quote(account, conversation, page, reply));
    }
    account.place(mapped.peer, mapped.id, conversation);
}

/// One request for a page of a conversation's messages, newest first as Telegram answers.
fn fetch(account: &Account, who: Who, topic: Option<i32>, around: &Around) -> Result<Vec<tl::enums::Message>, Fault> {
    let peer = account.input(who)?;
    let number = |id: &str| id.parse::<i32>().unwrap_or(0);
    let (offset_id, add_offset, limit) = match around {
        Around::Newest => (0, 0, PAGE),
        Around::Before(id) => (number(id), 0, PAGE),
        // Telegram counts back from the offset: a negative shift reads forward of it.
        Around::After(id) => (number(id), -PAGE, PAGE),
        Around::At(id) => (number(id), -PAGE / 2, PAGE),
    };
    let answer = match topic {
        // Unverified against a real forum: the General topic (id 1) is read the same way as any
        // other, as the replies to its first message.
        Some(msg_id) => account.ask(&tl::functions::messages::GetReplies {
            peer,
            msg_id,
            offset_id,
            offset_date: 0,
            add_offset,
            limit,
            max_id: 0,
            min_id: 0,
            hash: 0,
        })?,
        None => account.ask(&tl::functions::messages::GetHistory {
            peer,
            offset_id,
            offset_date: 0,
            add_offset,
            limit,
            max_id: 0,
            min_id: 0,
            hash: 0,
        })?,
    };
    let (messages, chats, users) = match answer {
        tl::enums::messages::Messages::Messages(page) => (page.messages, page.chats, page.users),
        tl::enums::messages::Messages::Slice(page) => (page.messages, page.chats, page.users),
        tl::enums::messages::Messages::ChannelMessages(page) => (page.messages, page.chats, page.users),
        tl::enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new()),
    };
    account.learn(&users, &chats);
    Ok(messages)
}

/// Reads one page and records it, and answers its newest message. What a row of it shows is
/// queued to be fetched: the user is looking at this conversation.
pub(super) fn history(account: &Account, conversation: &str, around: Around) -> Option<Message> {
    let (who, topic) = Who::parse(conversation)?;
    if let Around::After(after) = &around {
        let mut live = account.live();
        live.after.retain(|(_, _, at)| at.elapsed() < AFTER_FRESH);
        if live.after.iter().any(|(place, id, _)| place == conversation && id == after) {
            return None;
        }
        if live.after.len() >= 32 {
            live.after.pop_front();
        }
        live.after.push_back((conversation.to_owned(), (*after).to_owned(), Instant::now()));
    }
    let raw = match fetch(account, who, topic, &around) {
        Ok(raw) => raw,
        Err(fault) => {
            eprintln!("chatd: Telegram: history: {fault}");
            return None;
        }
    };
    let mut mapped: Vec<Mapped> = {
        let dir = account.dir();
        raw.iter().filter_map(|message| content::map(&dir, message)).collect()
    };
    let page: HashMap<i32, (String, String, String)> =
        mapped.iter().map(|one| (one.id, (one.message.sender.id.clone(), one.message.sender.name.clone(), one.message.text.clone()))).collect();
    let mut wants = Vec::new();
    let mut authors = HashSet::new();
    for one in &mut mapped {
        finish(account, conversation, one, &page);
        wants.append(&mut one.wants);
        if let Some((author, None)) = Who::parse(&one.message.sender.id) {
            authors.insert(author);
        }
    }
    let messages: Vec<Message> = mapped.into_iter().map(|one| one.message).collect();
    // The newest page is whole: a stored message inside its range that Telegram no longer
    // lists was deleted while we were away.
    if let (Around::Newest, Some(newest), Some(oldest)) = (&around, messages.first(), messages.last()) {
        let stored = account.ctx.stored_ids(conversation, &oldest.id, &newest.id, 200).unwrap_or_default();
        for gone in stored.iter().filter(|id| !page.contains_key(&id.parse::<i32>().unwrap_or(-1))) {
            let _ = account.ctx.deleted(conversation, gone);
        }
    }
    let _ = account.ctx.history(conversation, &messages);
    wants.extend(avatar(account, who));
    wants.extend(authors.into_iter().take(20).filter_map(|author| avatar(account, author)));
    account.files.want(wants);
    messages.into_iter().next()
}

/// The user opened a conversation.
pub(super) fn backfill(account: &Account, conversation: &str) {
    let Some((who, topic)) = Who::parse(conversation) else {
        return;
    };
    account.here();
    let (forum, broadcast) = {
        let dir = account.dir();
        let known = dir.get(who);
        (known.is_some_and(|known| known.forum), known.is_some_and(|known| known.broadcast))
    };
    // A forum's own row holds no messages: opening it lists its topics.
    if forum && topic.is_none() {
        if let Err(fault) = topics(account, who) {
            eprintln!("chatd: Telegram: topics: {fault}");
        }
        return;
    }
    let newest = history(account, conversation, Around::Newest);
    if broadcast {
        sponsored(account, who, conversation, newest);
    }
}

/// A channel's sponsored message, which the terms require a client that shows channels to show.
// CONTRACT: there is no kind for an advertisement and no word of when a row is on screen, so it
// is a message at the foot of the channel with a card that says "Sponsored", recorded when the
// channel is opened, and told to Telegram as seen then. The one before it is taken away.
fn sponsored(account: &Account, channel: Who, conversation: &str, newest: Option<Message>) {
    let Ok(peer) = account.input(channel) else {
        return;
    };
    let note = format!("sponsored:{conversation}");
    if let Some(old) = account.ctx.note(&note).filter(|old| !old.is_empty()) {
        let _ = account.ctx.deleted(conversation, &old);
        account.ctx.set_note(&note, "");
    }
    let request = tl::functions::messages::GetSponsoredMessages { peer, msg_id: None };
    let Ok(tl::enums::messages::SponsoredMessages::Messages(found)) = account.ask(&request) else {
        return;
    };
    account.learn(&found.users, &found.chats);
    let Some(tl::enums::SponsoredMessage::Message(ad)) = found.messages.first() else {
        return;
    };
    let id = format!("ad{}", ad.random_id.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    // It sorts after the channel's newest message without moving the channel in the Inbox.
    let sent = newest.as_ref().map_or_else(now, |newest| newest.sent);
    let mut message = Message::plain(id.clone(), ad.title.clone(), ad.message.clone(), sent, false);
    message.spans = content::spans(&ad.message, ad.entities.as_deref().unwrap_or(&[]));
    // The button is the last line, and the whole of it is the link.
    if !ad.url.is_empty() && !ad.button_text.is_empty() {
        let start = message.text.len() + usize::from(!message.text.is_empty());
        if start > 0 {
            message.text.push('\n');
        }
        message.text.push_str(&ad.button_text);
        message.spans.push(Span { start: start as u32, end: message.text.len() as u32, style: Style::Link { url: ad.url.clone() } });
    }
    message.card = Some(Box::new(Card {
        title: if ad.recommended { "Recommended".to_owned() } else { "Sponsored".to_owned() },
        footer: ad.sponsor_info.clone().or_else(|| ad.additional_info.clone()).unwrap_or_default(),
        ..Card::default()
    }));
    if account.ctx.history(conversation, std::slice::from_ref(&message)).is_ok() {
        account.ctx.set_note(&note, &id);
        // The store previews whatever it recorded last at the newest time. Recording the real
        // newest message again, marked as its own echo so it counts as changed, puts its words
        // back on the row: an advertisement is never what the Inbox shows of a channel.
        if let Some(mut newest) = newest {
            newest.client_id = Some(newest.id.clone());
            let _ = account.ctx.history(conversation, std::slice::from_ref(&newest));
        }
        let _ = account.ask(&tl::functions::messages::ViewSponsoredMessage { random_id: ad.random_id.clone() });
    }
}
