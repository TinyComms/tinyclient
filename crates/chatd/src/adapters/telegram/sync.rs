//! What is asked of Telegram: who we are, the chats and their folders, a page of history, a
//! forum's topics, a channel's sponsored message. Everything goes to the store as it is read.

use super::content::{self, Mapped, Want};
use super::filing::{self, Cursor, Facts, Filed};
use super::peers::{thread_id, Known, Who};
use super::wire::Fault;
use super::{presence, Account, PAGE};
use crate::store::{ConversationRow, Filing, PreviewLine};
use grammers_tl_types as tl;
use protocol::{CallSupport, Capabilities, Card, ConversationKind, EmojiScope, MediaFile, MediaKind, Message, Relation, ReplyTo, Span, Special, Style, Watching};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Pages of the main list read at sign-on, a hundred chats each: the newest 500. The rest are
/// read a page at a time as the list is scrolled to the end of what is loaded.
const DIALOG_PAGES: usize = 5;
const ARCHIVE_PAGES: usize = 1;
/// How close to the end of the loaded list a row on screen asks for the next page.
const NEAR_END: usize = 30;
/// Topics read when a forum is opened.
const TOPICS: i32 = 50;
/// A `HistoryAfter` for the same place is not asked again within this long.
const AFTER_FRESH: Duration = Duration::from_secs(60);
/// Telegram's own rule: a channel's sponsored messages are asked again at most this often.
const ADS_FRESH: Duration = Duration::from_secs(300);
/// Rows whose newest message is fetched for one `Previews`, and the pause between them.
const PREVIEWS: usize = 20;
const PREVIEW_PACE: Duration = Duration::from_secs(1);
/// Telegram's own account, which sends login codes.
pub(super) const SERVICE: i64 = 777_000;

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

/// A chat's row. Telegram has no Spaces: the row has none, and its filing is the caller's.
fn row(account: &Account, who: Who, known: &Known) -> ConversationRow {
    let mut row = ConversationRow::new(who.text(), kind(who, known), known.name.clone());
    row.avatar = known.avatar();
    row.read_only = known.read_only;
    row.mark = known.mark;
    row.bot = known.bot;
    // People, bots, groups and the service chat are the Inbox's; a broadcast channel only when
    // the user stars it.
    row.listed = !(matches!(who, Who::Channel(_)) && known.broadcast);
    match who {
        Who::User(id) if id == account.me => {
            // Saved Messages: the user's own notes, with no presence and no call.
            row.title = "Saved Messages".to_owned();
            row.special = Special::Saved;
        }
        Who::User(SERVICE) => {
            // Telegram's own account: read-only, never a call.
            row.special = Special::Service;
            row.read_only = true;
            row.peer = Some(who.text());
        }
        Who::User(_) => {
            row.peer = Some(who.text());
            // Calls are a later stage: until then a person's call opens in Telegram's own app.
            if !known.bot && !known.deleted {
                row.call = CallSupport::Handoff;
            }
        }
        _ => {}
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
    let fresh = |account: &Account| {
        let mut fresh = row(account, who, &known);
        let filing = {
            let mut live = account.live();
            if !live.filed.facts.contains_key(&who) {
                live.filed.note(who, Facts::default());
            }
            live.filed.filing(who, &known)
        };
        fresh.filing = Some(filing);
        fresh
    };
    if topic.is_none() {
        return account.ctx.conversation(&fresh(account)).is_ok();
    }
    // A topic nobody has listed yet: the forum is asked once, and a topic it does not list (a
    // new one, heard of before its row) is recorded under a plain name until it does.
    if !matches!(account.ctx.known(&who.text()), Ok(Some(_))) && account.ctx.conversation(&fresh(account)).is_err() {
        return false;
    }
    let _ = topics(account, who);
    if matches!(account.ctx.known(&id), Ok(Some(_))) {
        return true;
    }
    let mut thread = ConversationRow::new(id, ConversationKind::Thread, "Topic");
    thread.parent = Some(who.text());
    thread.root = topic.map(|topic| topic.to_string());
    account.ctx.conversation(&thread).is_ok()
}

/// A group or channel was renamed, or its picture or our rights in it changed. Its filing is
/// left as stored.
pub(super) fn refresh_row(account: &Account, who: Who) {
    let Ok(Some(stored)) = account.ctx.known(&who.text()) else {
        return;
    };
    let Some(known) = account.dir().get(who).cloned() else {
        return;
    };
    let mut fresh = row(account, who, &known);
    fresh.position = stored.position;
    fresh.topic = stored.topic;
    let _ = account.ctx.conversation(&fresh);
}

/// What this account may send, from what Telegram says of it: Premium raises the file size and
/// the caption, and allows custom emoji.
fn capabilities(premium: bool) -> Capabilities {
    Capabilities {
        // 2 GB, 4 GB with Premium: 4,000 parts of 512 KB, 8,000 with Premium.
        max_file_bytes: if premium { 4_000 * 1024 * 1024 } else { 2_000 * 1024 * 1024 },
        max_files: 10,
        caption_limit: if premium { 4_096 } else { 1_024 },
        spoiler: true,
        as_file: true,
        gifs: true,
        stickers: true,
        custom_emoji: if premium { EmojiScope::All } else { EmojiScope::None },
        silent: true,
        schedule: true,
        suppress_embeds: true,
        reply_ping: false,
        // Telegram lets a message be edited for 48 hours (unverified for every chat kind).
        edit_window: 48 * 3_600,
    }
}

/// The first requests of a connection: are we still signed in, who are we, what is muted by
/// default, the chats, the contacts.
pub(super) fn sign_on(account: &Account) -> Result<(), Fault> {
    account.ask(&tl::functions::updates::GetState {})?;
    let me = account.ask(&tl::functions::users::GetUsers { id: vec![tl::enums::InputUser::UserSelf] })?;
    account.learn(&me, &[]);
    let (name, premium) = {
        let dir = account.dir();
        (dir.name(Who::User(account.me)), dir.get(Who::User(account.me)).is_some_and(|known| known.premium))
    };
    let _ = account.ctx.describe(&name, &Who::User(account.me).text());
    account.ctx.capabilities(capabilities(premium));
    let kinds = [
        tl::enums::InputNotifyPeer::InputNotifyUsers,
        tl::enums::InputNotifyPeer::InputNotifyChats,
        tl::enums::InputNotifyPeer::InputNotifyBroadcasts,
    ];
    for (index, peer) in kinds.into_iter().enumerate() {
        let tl::enums::PeerNotifySettings::Settings(settings) = account.ask(&tl::functions::account::GetNotifySettings { peer })?;
        account.live().muted[index] = settings.mute_until.is_some_and(|until| i64::from(until) > now());
    }
    dialogs(account)?;
    // An earlier build kept every chat in one invented Space. The rows read just now have left
    // it; what is left (chats past the newest 500) goes with it, and comes back on scroll.
    let _ = account.ctx.spaces(&[]);
    contacts(account)
}

/// Telegram's caching hash over a list of numbers (`core.telegram.org/api/offsets`, "Hash
/// generation"): Telegram answers "not modified" when it matches what it would send.
pub(super) fn telegram_hash(numbers: impl IntoIterator<Item = i64>) -> i64 {
    let mut hash: u64 = 0;
    for number in numbers {
        hash ^= hash >> 21;
        hash ^= hash << 35;
        hash ^= hash >> 4;
        hash = hash.wrapping_add(number as u64);
    }
    hash as i64
}

/// The hash `contacts.getContacts` takes: the saved count it last answered, then the contacts'
/// user ids in order.
pub(super) fn contacts_hash(saved_count: i32, users: &[i64]) -> i64 {
    let mut ids = users.to_vec();
    ids.sort_unstable();
    telegram_hash(std::iter::once(i64::from(saved_count)).chain(ids))
}

/// Where the hash of the contacts last read is kept.
const CONTACTS_HASH: &str = "contacts:hash";

/// The contacts, which are the list the user owns on Telegram, and whether each is online.
/// Asked with the hash of the list last read: an unchanged list costs Telegram nothing, and
/// what was stored of it stands.
fn contacts(account: &Account) -> Result<(), Fault> {
    let hash = account.ctx.note(CONTACTS_HASH).and_then(|hash| hash.parse::<i64>().ok()).unwrap_or(0);
    let tl::enums::contacts::Contacts::Contacts(found) = account.ask(&tl::functions::contacts::GetContacts { hash })? else {
        return Ok(());
    };
    let ids: Vec<i64> = found.contacts.iter().map(|tl::enums::Contact::Contact(contact)| contact.user_id).collect();
    account.ctx.set_note(CONTACTS_HASH, &contacts_hash(found.saved_count, &ids).to_string());
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

/// Reads the chats at sign-on (and again when a default mute changed): the folders first, so
/// every row is written once with its filing; then the newest 500 of the main list and the
/// newest of the archive. Each row names its folders, its pin and whether it is archived;
/// nothing is grouped.
pub(super) fn dialogs(account: &Account) -> Result<(), Fault> {
    let rules = match filing::ask_rules(account) {
        Ok(rules) => rules,
        Err(fault) if fault.flood().is_some() || fault.signed_out() => return Err(fault),
        Err(_) => Vec::new(),
    };
    account.live().filed = Filed { rules, ..Filed::default() };
    filing::report(account);
    for (list, pages) in [(0, DIALOG_PAGES), (1, ARCHIVE_PAGES)] {
        let mut offset = None;
        for _ in 0..pages {
            match read_page(account, list, offset)? {
                Some(next) => offset = Some(next),
                None => break,
            }
            if account.ctx.stopped() {
                break;
            }
        }
    }
    Ok(())
}

/// One page of a list (0 the main list, 1 the archive) from `offset`, written row by row.
/// Answers where the next page starts, if Telegram has one.
fn read_page(account: &Account, list: usize, offset: Option<Cursor>) -> Result<Option<Cursor>, Fault> {
    let start = match offset {
        Some((date, id, who)) => (date, id, account.input(who).unwrap_or(tl::enums::InputPeer::Empty)),
        None => (0, 0, tl::enums::InputPeer::Empty),
    };
    let mut page = page(account, Some(list as i32), start)?;
    let mut next = None;
    for dialog in std::mem::take(&mut page.dialogs) {
        let who = Who::of(&dialog.peer);
        let newest = page.newest.remove(&(who, dialog.top_message));
        let date = match &newest {
            Some(tl::enums::Message::Message(message)) => message.date,
            Some(tl::enums::Message::Service(message)) => message.date,
            _ => 0,
        };
        next = Some((date, dialog.top_message, who));
        record(account, list, &dialog, newest.as_ref());
    }
    let next = next.filter(|_| page.more);
    account.live().filed.ends[list] = next;
    Ok(next)
}

/// One dialog to the store: its row with its filing, its newest message, counts, mute and
/// whether the other side has read my last message.
fn record(account: &Account, list: usize, dialog: &tl::types::Dialog, newest: Option<&tl::enums::Message>) {
    let who = Who::of(&dialog.peer);
    let id = who.text();
    let Some(known) = account.dir().get(who).cloned() else {
        return;
    };
    let is_muted = muted(account, who, &dialog.notify_settings);
    let unread = (dialog.unread_count.max(0) as u32).max(u32::from(dialog.unread_mark));
    let filing = {
        let mut live = account.live();
        let filed = &mut live.filed;
        filed.note(who, Facts { archived: list == 1, muted: is_muted, read: unread == 0 });
        if !filed.order[list].contains(&who) {
            filed.order[list].push(who);
        }
        // Telegram lists the pins first, in their order.
        if dialog.pinned && !filed.pins[list].contains(&who) {
            filed.pins[list].push(who);
        }
        filed.filing(who, &known)
    };
    let mut fresh = row(account, who, &known);
    fresh.filing = Some(filing);
    if account.ctx.conversation(&fresh).is_err() {
        return;
    }
    let mut updated = 0;
    // The dialog list carries each chat's newest message: that is its preview, for free.
    if let Some(mapped) = newest.and_then(|newest| content::map(&account.dir(), newest)) {
        updated = mapped.message.sent;
        if known.forum {
            // A forum's newest message belongs to one of its topics; the row shows its line.
            let line = PreviewLine {
                id: mapped.message.id.clone(),
                text: mapped.message.text.clone(),
                sender: mapped.message.sender.name.clone(),
                mine: mapped.message.outgoing,
                sent: mapped.message.sent,
            };
            let _ = account.ctx.preview(&id, &line);
        } else {
            let _ = account.ctx.history(&id, std::slice::from_ref(&mapped.message));
        }
    }
    let read = (dialog.read_inbox_max_id > 0).then(|| dialog.read_inbox_max_id.to_string());
    let _ = account.ctx.counts(&id, unread, dialog.unread_mentions_count.max(0) as u32, read.as_deref(), updated);
    let _ = account.ctx.muted(&id, is_muted);
    // ✓✓: the other side read my messages up to here. Not for my own notes or a broadcast.
    if dialog.read_outbox_max_id > 0 && who != Who::User(account.me) && !known.broadcast {
        let _ = account.ctx.seen(&id, &dialog.read_outbox_max_id.to_string());
    }
}

/// What the shell has on screen changed: the pictures of the rows on it are fetched, each once,
/// and a list scrolled near the end of what is loaded reads its next page.
pub(super) fn on_screen(account: &Account, watching: &Watching) {
    let rows: Vec<Who> = watching
        .rows
        .iter()
        .filter(|row| row.account() == account.ctx.account)
        .filter_map(|row| Who::parse(&row.id))
        .filter_map(|(who, topic)| topic.is_none().then_some(who))
        .collect();
    let mut pictures = Vec::new();
    for who in &rows {
        let Some(want) = avatar(account, *who) else {
            continue;
        };
        let mut live = account.live();
        if live.pictured.len() >= 4_096 {
            live.pictured.clear();
        }
        if live.pictured.insert(want.file.id.clone()) {
            pictures.push(want);
        }
    }
    account.files.want(pictures);
    for list in 0..2 {
        let next = {
            let live = account.live();
            let order = &live.filed.order[list];
            let tail = &order[order.len().saturating_sub(NEAR_END)..];
            live.filed.ends[list].filter(|_| rows.iter().any(|who| tail.contains(who)))
        };
        if let Some(next) = next {
            if let Err(fault) = read_page(account, list, Some(next)) {
                eprintln!("chatd: Telegram: more chats: {fault}");
            }
        }
    }
    ads_on_screen(account, watching);
}

/// Rows on screen with no preview line: each one's newest message, one request a second. A row
/// read with the dialog list already has its line; these are what that did not cover.
pub(super) fn previews(account: &Account, ids: &[String]) {
    for (at, id) in ids.iter().take(PREVIEWS).enumerate() {
        let Some((who, topic)) = Who::parse(id) else {
            continue;
        };
        // A forum's own row shows its newest topic's line, set with the dialog list.
        if topic.is_none() && account.dir().get(who).is_some_and(|known| known.forum) {
            continue;
        }
        if at > 0 && account.ctx.pause(PREVIEW_PACE) {
            return;
        }
        let raw = match fetch(account, who, topic, &Around::Newest, 1) {
            Ok(raw) => raw,
            Err(fault) => {
                eprintln!("chatd: Telegram: preview: {fault}");
                continue;
            }
        };
        let mut mapped: Vec<Mapped> = {
            let dir = account.dir();
            raw.iter().filter_map(|message| content::map(&dir, message)).collect()
        };
        for one in &mut mapped {
            finish(account, id, one, &HashMap::new());
        }
        let messages: Vec<Message> = mapped.into_iter().map(|one| one.message).collect();
        let _ = account.ctx.history(id, &messages);
    }
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
    let (mut count, mut pinned) = (0, 0);
    for (at, topic) in found.topics.iter().enumerate() {
        let tl::enums::ForumTopic::Topic(topic) = topic else {
            continue;
        };
        let id = thread_id(forum, topic.id);
        let mut thread = ConversationRow::new(id.clone(), ConversationKind::Thread, topic.title.clone());
        thread.parent = Some(forum.text());
        thread.root = Some(topic.id.to_string());
        // Pinned topics first, then as Telegram lists them: the most recently active first.
        thread.position = at as i32 - if topic.pinned { TOPICS } else { 0 };
        thread.read_only = topic.closed && !topic.my;
        // The forum's own pins, in Telegram's order.
        let pin = topic.pinned.then(|| {
            pinned += 1;
            pinned - 1
        });
        thread.filing = Some(Filing { pin, ..Filing::default() });
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
fn fetch(account: &Account, who: Who, topic: Option<i32>, around: &Around, limit: i32) -> Result<Vec<tl::enums::Message>, Fault> {
    let peer = account.input(who)?;
    let number = |id: &str| id.parse::<i32>().unwrap_or(0);
    let (offset_id, add_offset) = match around {
        Around::Newest => (0, 0),
        Around::Before(id) => (number(id), 0),
        // Telegram counts back from the offset: a negative shift reads forward of it.
        Around::After(id) => (number(id), -limit),
        Around::At(id) => (number(id), -limit / 2),
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
    let raw = match fetch(account, who, topic, &around, PAGE) {
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
/// Asked when the channel is opened, at most once in five minutes as Telegram asks; told to
/// Telegram as viewed once it is on screen (`ads_on_screen`), once for each.
// CONTRACT: there is no kind for an advertisement, so it is a message at the foot of the
// channel with a card that says "Sponsored". And nothing tells chatd that the user followed its
// link (the shell opens the address itself), so `messages.clickSponsoredMessage` is never sent:
// the terms want it sent on a click. A `Request` naming a clicked message would carry it.
fn sponsored(account: &Account, channel: Who, conversation: &str, newest: Option<Message>) {
    let Ok(peer) = account.input(channel) else {
        return;
    };
    {
        let mut live = account.live();
        live.ads.retain(|(_, at)| at.elapsed() < ADS_FRESH);
        if live.ads.iter().any(|(place, _)| place == conversation) {
            drop(live);
            ads_on_screen(account, &account.ctx.watching());
            return;
        }
        if live.ads.len() >= 32 {
            live.ads.remove(0);
        }
        live.ads.push((conversation.to_owned(), Instant::now()));
    }
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
        {
            // The same advertisement asked again later is still told as viewed only once.
            let mut live = account.live();
            let viewed = live.shown.iter().any(|(_, random, viewed)| *random == ad.random_id && *viewed);
            live.shown.push((conversation.to_owned(), ad.random_id.clone(), viewed));
        }
        ads_on_screen(account, &account.ctx.watching());
    }
}

/// The open conversation is a channel whose sponsored message has not been told as viewed: it
/// is now, once.
fn ads_on_screen(account: &Account, watching: &Watching) {
    let Some(open) = watching.conversation.as_ref().filter(|open| open.account() == account.ctx.account) else {
        return;
    };
    let due: Vec<Vec<u8>> = {
        let mut live = account.live();
        if live.shown.len() > 64 {
            let extra = live.shown.len() - 64;
            live.shown.drain(..extra);
        }
        let mut due = Vec::new();
        for (place, random, viewed) in live.shown.iter_mut() {
            if *place == open.id && !*viewed {
                *viewed = true;
                due.push(random.clone());
            }
        }
        due
    };
    for random_id in due {
        let _ = account.ask(&tl::functions::messages::ViewSponsoredMessage { random_id });
    }
}

/// A list of Telegram's messages as the contract's, for the conversation they are in, and the
/// pictures they show. Nothing is recorded.
pub(super) fn mapped(account: &Account, conversation: &str, raw: &[tl::enums::Message]) -> (Vec<Message>, Vec<Want>) {
    let mut mapped: Vec<Mapped> = {
        let dir = account.dir();
        raw.iter().filter_map(|message| content::map(&dir, message)).collect()
    };
    let page: HashMap<i32, (String, String, String)> =
        mapped.iter().map(|one| (one.id, (one.message.sender.id.clone(), one.message.sender.name.clone(), one.message.text.clone()))).collect();
    let mut wants = Vec::new();
    for one in &mut mapped {
        finish(account, conversation, one, &page);
        wants.append(&mut one.wants);
    }
    (mapped.into_iter().map(|one| one.message).collect(), wants)
}

/// What a `messages.search` answer holds, and the count Telegram keeps for it.
pub(super) fn searched(account: &Account, answer: tl::enums::messages::Messages) -> (Vec<tl::enums::Message>, Option<u32>) {
    let (messages, chats, users, count) = match answer {
        tl::enums::messages::Messages::Messages(page) => {
            let count = page.messages.len() as u32;
            (page.messages, page.chats, page.users, Some(count))
        }
        tl::enums::messages::Messages::Slice(page) => (page.messages, page.chats, page.users, Some(page.count.max(0) as u32)),
        tl::enums::messages::Messages::ChannelMessages(page) => (page.messages, page.chats, page.users, Some(page.count.max(0) as u32)),
        tl::enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new(), None),
    };
    account.learn(&users, &chats);
    (messages, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contacts_hash_is_telegrams_over_the_saved_count_and_the_sorted_ids() {
        assert_eq!(telegram_hash([]), 0);
        assert_eq!(telegram_hash([5]), 5);
        // One more step by hand: 5 shifted and folded, then 7 added.
        let mut hash: u64 = 5;
        hash ^= hash >> 21;
        hash ^= hash << 35;
        hash ^= hash >> 4;
        assert_eq!(telegram_hash([5, 7]), hash.wrapping_add(7) as i64);
        // The order Telegram listed them in does not matter; the saved count does.
        assert_eq!(contacts_hash(2, &[30, 10]), contacts_hash(2, &[10, 30]));
        assert_ne!(contacts_hash(2, &[10, 30]), contacts_hash(3, &[10, 30]));
        assert_ne!(contacts_hash(1, &[10]), 0);
    }
}
