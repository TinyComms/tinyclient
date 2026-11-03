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
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
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
/// Where a comments thread writes through, held for the connection: capped, as everything
/// the account holds while it lives.
const DISCUSSIONS: usize = 32;
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
        file_scan: false,
        allow_archives: false,
        voice_notes: true,
    }
}

/// The first requests of a connection: are we still signed in, who are we, what is muted by
/// default, the chats, the contacts.
pub(super) fn sign_on(account: &Arc<Account>) -> Result<(), Fault> {
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

/// One page of a list, and where the next page starts, if Telegram has one.
type PageRead = (Page, Option<Cursor>);

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
/// nothing is grouped. A page is written while the one after it is asked for, so the lists
/// paint what exists as it exists, and the boot does not wait for the writing it need not.
pub(super) fn dialogs(account: &Arc<Account>) -> Result<(), Fault> {
    let rules = match filing::ask_rules(account) {
        Ok(rules) => rules,
        Err(fault) if fault.flood().is_some() || fault.signed_out() => return Err(fault),
        Err(_) => Vec::new(),
    };
    account.live().filed = Filed { rules, ..Filed::default() };
    filing::report(account);
    for (list, most) in [(0usize, DIALOG_PAGES), (1usize, ARCHIVE_PAGES)] {
        let mut offset = None;
        let mut inflight = Some(ask_page(account, list, offset));
        let mut caught = 0usize;
        while caught < most {
            let read = match inflight.take() {
                Some(landed) => landed.recv().unwrap_or_else(|_| Err(Fault::Net("the page ask ended".into()))),
                None => fetch_page(account, list, offset),
            };
            let (mut page, next) = read?;
            caught += 1;
            // The page after this one is asked while this one is written, so long as more of
            // the list is due at sign-on.
            if next.is_some() && caught < most && !account.ctx.stopped() {
                inflight = Some(ask_page(account, list, next));
            }
            record_page(account, list, &mut page, next);
            match next {
                Some(next) if caught < most && !account.ctx.stopped() => offset = Some(next),
                _ => break,
            }
        }
    }
    // Folder pins name chats the pages may never have carried: a pin whose dialog lies past
    // the newest 500 would sit missing for ever, so what nothing stored says of is asked
    // for alone. The order the folders hold is already in the store.
    pinned_missing(account);
    Ok(())
}

/// The chats the folder rules pin that no dialog page carried: each one dialog, asked of
/// `getPeerDialogs`, so a pin never sits missing only because it lies far down the list.
fn pinned_missing(account: &Account) {
    let rules = account.live().filed.rules.clone();
    let mut asked = HashSet::new();
    for rule in rules {
        for who in rule.pinned {
            if !asked.insert(who) {
                continue;
            }
            let id = who.text();
            if matches!(account.ctx.known(&id), Ok(Some(_))) {
                continue;
            }
            let Ok(peer) = account.input(who) else {
                continue;
            };
            let request = tl::functions::messages::GetPeerDialogs {
                peers: vec![tl::enums::InputDialogPeer::Peer(tl::types::InputDialogPeer { peer })],
            };
            let found = match account.ask(&request) {
                Ok(tl::enums::messages::PeerDialogs::Dialogs(found)) => found,
                _ => {
                    continue;
                }
            };
            account.learn(&found.users, &found.chats);
            for dialog in found.dialogs {
                if let tl::enums::Dialog::Dialog(dialog) = dialog {
                    let list = usize::from(dialog.folder_id == Some(1));
                    let newest = found
                        .messages
                        .iter()
                        .find(|message| match message {
                            tl::enums::Message::Message(before) => before.id == dialog.top_message,
                            tl::enums::Message::Service(before) => before.id == dialog.top_message,
                            tl::enums::Message::Empty(_) => false,
                        });
                    record(account, list, &dialog, newest);
                }
            }
        }
    }
}

/// One page of a list (0 the main list, 1 the archive) from `offset`, read whole and written
/// whole, as the scrolling list reads it. Answers where the next page starts, if Telegram has
/// one.
fn read_page(account: &Account, list: usize, offset: Option<Cursor>) -> Result<Option<Cursor>, Fault> {
    let (mut page, next) = fetch_page(account, list, offset)?;
    record_page(account, list, &mut page, next);
    Ok(next)
}

/// One page read (0 the main list, 1 the archive) from `offset`, with nothing written yet:
/// its dialogs in the order Telegram gave, and where the next page starts, if Telegram has
/// one. The next page is asked before this one is written, so the sign-on is not chained to
/// the writing of every page it asked for.
fn fetch_page(account: &Account, list: usize, offset: Option<Cursor>) -> Result<PageRead, Fault> {
    let start = match offset {
        Some((date, id, who)) => (date, id, account.input(who).unwrap_or(tl::enums::InputPeer::Empty)),
        None => (0, 0, tl::enums::InputPeer::Empty),
    };
    let page = page(account, Some(list as i32), start)?;
    let next = page.dialogs.last().and_then(|dialog| {
        let who = Who::of(&dialog.peer);
        let date = match page.newest.get(&(who, dialog.top_message)) {
            Some(tl::enums::Message::Message(newest)) => newest.date,
            Some(tl::enums::Message::Service(newest)) => newest.date,
            _ => 0,
        };
        Some((date, dialog.top_message, who))
    });
    let next = next.filter(|_| page.more);
    Ok((page, next))
}

/// The next page, read on its own so it is at the wire while the page before it is written.
/// Answers on a channel of its own; where no thread could be had, the page is read where the
/// caller stands, as the sign-on always did.
fn ask_page(account: &Arc<Account>, list: usize, offset: Option<Cursor>) -> Receiver<Result<PageRead, Fault>> {
    let (sent, landed) = channel();
    let spare = sent.clone();
    let speak = account.clone();
    let asked = std::thread::Builder::new().name("telegram-pages".into()).spawn(move || {
        let _ = sent.send(fetch_page(&speak, list, offset));
    });
    if asked.is_err() {
        let _ = spare.send(fetch_page(account, list, offset));
    }
    landed
}

/// A page of the list, written row by row after it was read: its dialogs in the order Telegram
/// gave, and then where the list goes on.
fn record_page(account: &Account, list: usize, page: &mut Page, next: Option<Cursor>) {
    for dialog in std::mem::take(&mut page.dialogs) {
        let who = Who::of(&dialog.peer);
        let newest = page.newest.remove(&(who, dialog.top_message));
        record(account, list, &dialog, newest.as_ref());
    }
    account.live().filed.ends[list] = next;
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
    // Telegram's word on what is unread here. Where this user read further than the dialog says,
    // the read was told while the link was down and lost on the whole: the tell goes again, and
    // the counts are asked of the dialog once more, for the read may not have been the last word
    // on the unread (`GetPeerDialogs` answers what the tell left). If that answer fails, the row
    // stands untouched: the tell is on its way and the next dialog count is the truth.
    let mut unread = unread;
    let mut mentions = dialog.unread_mentions_count.max(0) as u32;
    let mut read = (dialog.read_inbox_max_id > 0).then(|| dialog.read_inbox_max_id.to_string());
    let mut counted = true;
    if let Some(up_to) = account.ctx.read_up_to(&id) {
        let mine = up_to.parse::<i32>().ok();
        let theirs = read.as_deref().and_then(|value| value.parse::<i32>().ok());
        if mine.is_some_and(|mine| theirs.is_some_and(|theirs| mine > theirs)) {
            super::mark_read(account, &id, &up_to);
            match peer_dialog(account, who) {
                Some(fresh) => {
                    unread = (fresh.unread_count.max(0) as u32).max(u32::from(fresh.unread_mark));
                    mentions = fresh.unread_mentions_count.max(0) as u32;
                    read = (fresh.read_inbox_max_id > 0).then(|| fresh.read_inbox_max_id.to_string());
                }
                None => counted = false,
            }
        }
    }
    if counted {
        let _ = account.ctx.counts(&id, unread, mentions, read.as_deref(), updated);
    }
    let _ = account.ctx.muted(&id, is_muted);
    // The cloud draft rides the dialog: what the phone left on this chat shows on its row as
    // "Draft:", and a draft the phone dropped is dropped here too. An empty one clears.
    let draft_text = match &dialog.draft {
        Some(tl::enums::DraftMessage::Message(draft)) => draft.message.as_str(),
        _ => "",
    };
    let _ = account.ctx.draft(&id, draft_text);
    // ✓✓: the other side read my messages up to here. Not for my own notes or a broadcast.
    if dialog.read_outbox_max_id > 0 && who != Who::User(account.me) && !known.broadcast {
        let _ = account.ctx.seen(&id, &dialog.read_outbox_max_id.to_string());
    }
}

/// One dialog as the network counts it right now, what a read told again leaves.
fn peer_dialog(account: &Account, who: Who) -> Option<tl::types::Dialog> {
    let peer = account.input(who).ok()?;
    let request = tl::functions::messages::GetPeerDialogs { peers: vec![tl::enums::InputDialogPeer::Peer(tl::types::InputDialogPeer { peer })] };
    match account.ask(&request) {
        Ok(tl::enums::messages::PeerDialogs::Dialogs(found)) => {
            found.dialogs.into_iter().find_map(|dialog| match dialog {
                tl::enums::Dialog::Dialog(dialog) => Some(dialog),
                _ => None,
            })
        }
        Err(fault) => {
            eprintln!("chatd: Telegram: the read told again: {fault}");
            None
        }
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
                tracing::warn!("chatd: Telegram: more chats: {fault}");
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
                tracing::warn!("chatd: Telegram: preview: {fault}");
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
        original: false,
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
        // The topic's own icon color: the tile it draws wherever its list shows. Its picture:
        // the custom emoji the icon names, shown wherever the row stands.
        thread.icon = Some(topic.icon_color);
        if let Some(icon) = topic.icon_emoji_id {
            thread.avatar = Some(format!("e{icon}"));
        }
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
    fetch_icons(account, forum, &found.topics);
    Ok(count)
}

/// An emoji document asked whole: the animated file itself (its Lottie play), so the tile
/// animates like the official apps', and a still emoji is the same file whole. The still
/// thumbnail inside the document is neither: it is smaller than the play it belongs to.
fn emoji_want(document: &tl::types::Document, row: Option<String>) -> Want {
    Want {
        file: MediaFile::new(format!("e{}", document.id), MediaKind::Emoji),
        dc: document.dc_id,
        location: tl::enums::InputFileLocation::InputDocumentFileLocation(tl::types::InputDocumentFileLocation {
            id: document.id,
            access_hash: document.access_hash,
            file_reference: document.file_reference.clone(),
            thumb_size: String::new(),
        }),
        inline: None,
        row,
        original: false,
    }
}

/// The custom emoji a set of document ids names, asked in one batch. Telegram answers only
/// what it knows; an error leaves the caller without documents (the tiles fall back to text).
fn emoji_documents(account: &Account, ids: &[i64], why: &str) -> Vec<tl::types::Document> {
    let documents = match account.ask(&tl::functions::messages::GetCustomEmojiDocuments { document_id: ids.to_vec() }) {
        Ok(documents) => documents,
        Err(fault) => {
            eprintln!("chatd: Telegram: {why}: {fault}");
            return Vec::new();
        }
    };
    documents.into_iter().map(|document| match document {
        tl::enums::Document::Document(document) => Some(document),
        tl::enums::Document::Empty(_) => None,
    }).flatten().collect()
}

/// The documents an id asks of a set: Telegram answers only the emoji it still keeps.
fn documents_of<'a>(documents: &'a [tl::types::Document], id: i64) -> Option<&'a tl::types::Document> {
    documents.iter().find(|document| document.id == id)
}

/// The topics' own pictures: one request for every icon the pages say, and a fetch for each
/// that is not on disk yet (the same file may hang off several topics). The pictures are the
/// emoji documents Telegram names in the topic list, small and shown at list size.
fn fetch_icons(account: &Account, forum: Who, topics: &[tl::enums::ForumTopic]) {
    // The topics that carry an icon: one pair per (emoji, row), so the rows that hang off
    // the same picture are each told when their picture lands.
    let mut icons: Vec<(i64, String)> = Vec::new();
    for topic in topics {
        let tl::enums::ForumTopic::Topic(topic) = topic else {
            continue;
        };
        let Some(icon) = topic.icon_emoji_id else {
            continue;
        };
        let row = thread_id(forum, topic.id);
        if icons.iter().any(|(held, held_row)| *held == icon && *held_row == row) {
            continue;
        }
        icons.push((icon, row));
    }
    let mut ids = icons.iter().map(|(icon, _)| *icon).collect::<Vec<i64>>();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return;
    }
    let documents = emoji_documents(account, &ids, "topic icons");
    // The picture's row: the topic whose row hung it, so the list refreshes on its turn.
    let wants = icons.iter().filter_map(|(icon, row)| {
        documents_of(&documents, *icon).map(|document| emoji_want(document, Some(row.clone())))
    }).collect();
    account.files.want(wants);
}

/// The custom emoji a set of document ids names, fetched whole into the media cache under the
/// name each id carries, so the rows that show them draw the picture instead of their text.
/// The rows named by `conversation` read again when the files land.
fn fetch_emoji(account: &Account, mut ids: Vec<i64>, conversation: &str, why: &str) {
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return;
    }
    let documents = emoji_documents(account, &ids, why);
    let wants = ids.iter().filter_map(|id| {
        documents_of(&documents, *id).map(|document| emoji_want(document, Some(conversation.to_owned())))
    }).collect();
    account.files.want(wants);
}

/// A reaction's custom emoji: its document asked for and the file fetched into the media
/// cache under the same name the reaction carries, whole, the way a topic's icon is. The
/// rows read again when the pictures land, and the chips draw them instead of `:emoji:`.
pub(super) fn reaction_emoji(account: &Account, conversation: &str, messages: &[Message]) {
    fetch_emoji(
        account,
        messages.iter().flat_map(|message| content::reaction_emoji_ids(&message.reactions)).collect(),
        conversation,
        "reaction emoji",
    );
}

/// A message's own custom emoji: the premium emoji its text names, fetched whole under the
/// name its span carries (`e<document id>`). The text is the alt glyph Telegram chose; the row
/// draws the picture in its place once the file lands, and the glyph until then.
pub(super) fn text_emoji(account: &Account, conversation: &str, messages: &[Message]) {
    fetch_emoji(account, messages.iter().flat_map(content::text_emoji_ids).collect(), conversation, "message emoji");
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

/// Where a message belongs: its chat, in a forum the thread of its topic, and in a
/// discussion group the thread of the comments a reply of it answers.
pub(super) fn conversation_of(account: &Account, mapped: &Mapped) -> String {
    match mapped.peer {
        Who::Channel(_) if account.dir().get(mapped.peer).is_some_and(|known| known.forum) => {
            thread_id(mapped.peer, mapped.topic.unwrap_or(1))
        }
        Who::Channel(_) => comments_thread(account, mapped).unwrap_or_else(|| mapped.peer.text()),
        peer => peer.text(),
    }
}

/// Finishes a mapped message for the conversation it goes to: the reply's line.
pub(super) fn finish(account: &Account, conversation: &str, mapped: &mut Mapped, page: &HashMap<i32, (String, String, String)>) {
    if let Some(reply) = mapped.reply {
        // The echo of the post is the thread's own anchor, not a message anyone has before
        // them: a comment that answers the post carries no quote line for it. A comment
        // answering another comment in the thread names that one.
        let anchor = account.live().discussion.iter().any(|(held, _, echo)| *held == conversation && *echo == reply);
        if !anchor {
            mapped.message.reply_to = Some(quote(account, conversation, page, reply));
        }
    }
    account.place(mapped.peer, mapped.id, conversation);
    comment_thread(account, conversation, mapped);
}

/// The thread a channel post's comments hang off. The post carries the count; the row it opens
/// under is written with it, so the footer's ask never lands on a conversation that is not
/// there. The newest comment's time and the comments' unread only come with the pages that
/// read them, so the values a stored post keeps are kept on its way in again.
fn comment_thread(account: &Account, conversation: &str, mapped: &mut Mapped) {
    let Some(thread) = &mut mapped.message.thread else {
        return;
    };
    if let Ok(Some(stored)) = account.ctx.stored_message(conversation, &mapped.message.id) {
        if let Some(old) = &stored.thread {
            if old.id == thread.id {
                if old.last > thread.last {
                    thread.last = old.last;
                }
                thread.unread = old.unread;
            }
        }
    }
    let id = thread.id.clone();
    if matches!(account.ctx.known(&id), Ok(Some(_))) {
        return;
    }
    let mut row = ConversationRow::new(id, ConversationKind::Thread, "Comments");
    row.parent = Some(mapped.peer.text());
    row.root = Some(mapped.message.id.clone());
    // The row stands shut until the open of it learned where its comments go: without
    // that, a comment has nowhere to be written in Telegram's own terms.
    row.read_only = true;
    let _ = account.ctx.conversation(&row);
}

/// The discussion group behind a channel post and the echo of the post there: what
/// `getDiscussionMessage` names for the thread. Asked once (the cache of the connection
/// holds the answer), for its sends and for the comments Telegram maps back into it.
fn discussion(account: &Account, channel: Who, post: i32) -> Result<(Who, i32), Fault> {
    let thread = thread_id(channel, post);
    let held = account.live().discussion.iter().find(|(place, _, _)| *place == thread).map(|(_, who, echo)| (*who, *echo));
    if let Some(held) = held {
        return Ok(held);
    }
    let request = tl::functions::messages::GetDiscussionMessage { peer: account.input(channel)?, msg_id: post };
    let tl::enums::messages::DiscussionMessage::Message(found) = account.ask(&request)?;
    account.learn(&found.users, &found.chats);
    // How many comments the user has not read comes with the discussion, not with the post.
    comment_unread(account, channel, post, found.unread_count);
    // The answer leads with the post itself, standing in the group: the id in it is what
    // a send answers, and the group it stands in is where a send goes.
    let Some((group, echo)) = found.messages.iter().find_map(|message| match message {
        tl::enums::Message::Message(echo) => Some((Who::of(&echo.peer_id), echo.id)),
        _ => None,
    }) else {
        return Err(Fault::Net("Telegram said the discussion with nothing in it.".to_owned()));
    };
    let mut live = account.live();
    if live.discussion.len() >= DISCUSSIONS {
        live.discussion.clear();
    }
    live.discussion.push_back((thread, group, echo));
    Ok((group, echo))
}

/// Where a send in a comments thread goes: the discussion group behind the post, answering
/// the echo of the post there, as Telegram's own apps keep them. `None` for any other
/// conversation, which sends under its own peer and header.
pub(super) fn comment_target(account: &Account, conversation: &str) -> Result<Option<(Who, i32)>, Fault> {
    let Some((who @ Who::Channel(_), Some(post))) = Who::parse(conversation) else {
        return Ok(None);
    };
    // A broadcast channel, and not a forum: its threads are the comments of a post, which
    // are held in the linked discussion group.
    if !account.dir().get(who).is_some_and(|known| known.broadcast) {
        return Ok(None);
    }
    Ok(Some(discussion(account, who, post)?))
}

/// The row of a comments thread was written read-only, because a comment had nowhere to
/// go until Telegram named the discussion group behind the post. It does now: an open
/// that got its answer says post, once, and keeps everything else the row stood for.
fn comment_row(account: &Account, thread: &str, channel: Who, post: i32) {
    let Ok(Some(row)) = account.ctx.known(thread) else {
        return;
    };
    if !row.read_only {
        return;
    }
    let mut fresh = ConversationRow::new(thread, ConversationKind::Thread, row.title.as_str());
    fresh.parent = Some(channel.text());
    fresh.root = Some(post.to_string());
    let _ = account.ctx.conversation(&fresh);
}

/// The comments thread a message in the discussion group belongs to: one already opened
/// names the echo its reply answers. `None` leaves the message with the group's own id,
/// for a conversation of the group itself.
fn comments_thread(account: &Account, mapped: &Mapped) -> Option<String> {
    echo_thread(account, mapped.peer, mapped.reply)
}

/// The comments thread of the discussion group an update belongs to: the thread that was
/// opened names the echo its post says again there, and the update carries it as its `top`.
/// `None` for the group's own messages.
pub(super) fn echo_thread(account: &Account, group: Who, top: Option<i32>) -> Option<String> {
    let echo = top?;
    account.live().discussion.iter().find(|(_, held, at)| *held == group && *at == echo).map(|(thread, _, _)| thread.clone())
}

/// The footer's "last reply": the newest comment's time, written onto the post it hangs off
/// once its comments were read. A page of the channel read later would otherwise stamp the
/// post anew without one (`comment_thread` keeps the value on its way in).
fn comment_time(account: &Account, post_at: Who, root: i32, sent: i64) {
    let Ok(Some(mut post)) = account.ctx.stored_message(&post_at.text(), &root.to_string()) else {
        return;
    };
    let Some(section) = post.thread.as_mut().filter(|section| section.last < sent) else {
        return;
    };
    section.last = sent;
    let _ = account.ctx.history(&post_at.text(), std::slice::from_ref(&post));
}

/// The comments' unread count, which comes with where the discussion is, onto the post whose
/// footer shows it. A count that did not change writes nothing.
pub(super) fn comment_unread(account: &Account, channel: Who, root: i32, unread: i32) {
    let Ok(Some(mut post)) = account.ctx.stored_message(&channel.text(), &root.to_string()) else {
        return;
    };
    let Some(section) = post.thread.as_mut() else {
        return;
    };
    let unread = unread.max(0) as u32;
    if section.unread == unread {
        return;
    }
    section.unread = unread;
    let _ = account.ctx.history(&channel.text(), std::slice::from_ref(&post));
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
            tracing::warn!("chatd: Telegram: history: {fault}");
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
    let mut messages: Vec<Message> = mapped.into_iter().map(|one| one.message).collect();
    // A broadcast channel's posts show how many saw them. One ask covers the page, and the
    // count is written with the posts below rather than fetched again for each.
    if topic.is_none() && account.dir().get(who).is_some_and(|known| known.broadcast) {
        views(account, who, &mut messages);
    }
    // The newest page is whole: a stored message inside its range that Telegram no longer
    // lists was deleted while we were away.
    if let (Around::Newest, Some(newest), Some(oldest)) = (&around, messages.first(), messages.last()) {
        let stored = account.ctx.stored_ids(conversation, &oldest.id, &newest.id, 200).unwrap_or_default();
        for gone in stored.iter().filter(|id| !page.contains_key(&id.parse::<i32>().unwrap_or(-1))) {
            let _ = account.ctx.deleted(conversation, gone);
        }
    }
    let _ = account.ctx.history(conversation, &messages);
    // A thread of comments read: its newest time is the post's footer's "last reply", and the
    // post carries the count but not the time. A forum's topics are asked the same way, but
    // they carry no comment section and stand as rows of their own.
    if let Some((parent, Some(root))) = Who::parse(conversation) {
        if account.dir().get(parent).is_some_and(|known| known.broadcast) {
            if let Some(newest) = messages.first().map(|message| message.sent) {
                comment_time(account, parent, root, newest);
            }
        }
    }
    wants.extend(avatar(account, who));
    wants.extend(authors.into_iter().take(20).filter_map(|author| avatar(account, author)));
    // The reactions' custom emoji on this page: their files land for the chips while the
    // user is looking at the conversation.
    reaction_emoji(account, conversation, &messages);
    // The premium emoji in the page's own text: their files land for the rows the same way,
    // and the alt glyph stands only until they do.
    text_emoji(account, conversation, &messages);
    account.files.want(wants);
    messages.into_iter().next()
}

/// The views of a channel's posts, in one ask for the page. Telegram counts a view when the
/// app tells it to (`increment`), and reading a page is not the same as each post having been
/// on screen, so nothing is added to the count here: what Telegram says stands.
fn views(account: &Account, channel: Who, messages: &mut [Message]) {
    let ids: Vec<i32> = messages.iter().filter_map(|message| message.id.parse().ok()).collect();
    if ids.is_empty() {
        return;
    }
    let Ok(peer) = account.input(channel) else {
        return;
    };
    let request = tl::functions::messages::GetMessagesViews { peer, id: ids, increment: false };
    let Ok(tl::enums::messages::MessageViews::Views(found)) = account.ask(&request) else {
        return;
    };
    account.learn(&found.users, &found.chats);
    // Telegram answers in the order it was asked: no id comes back with a count.
    let counts: HashMap<i32, u32> = request
        .id
        .iter()
        .zip(found.views.iter())
        .filter_map(|(id, one)| {
            let tl::enums::MessageViews::Views(counts) = one;
            Some((*id, counts.views?.max(0) as u32))
        })
        .collect();
    for message in messages {
        if let Some(count) = message.id.parse().ok().and_then(|id| counts.get(&id)) {
            message.views = Some(*count);
        }
    }
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
            tracing::warn!("chatd: Telegram: topics: {fault}");
        }
        return;
    }
    // A comments thread under a channel post: its open asks Telegram once where the
    // comments are written — the row said read-only until it did — and with the answer
    // the page of the thread reads as any other's, under the post's id as before.
    if broadcast {
        if let Some(post) = topic {
            match discussion(account, who, post) {
                Ok(_) => comment_row(account, conversation, who, post),
                Err(fault) => tracing::warn!("chatd: Telegram: comments: {fault}"),
            }
        }
    }
    let newest = history(account, conversation, Around::Newest);
    // The sponsored post belongs at the foot of the channel's own page; a comments thread
    // under one of its posts is read the same way but is not that page.
    if broadcast && topic.is_none() {
        sponsored(account, who, conversation, newest);
    }
}

/// A channel's sponsored message, which the terms require a client that shows channels to show.
/// Asked when the channel is opened, at most once in five minutes as Telegram asks; told to
/// Telegram as viewed once it is on screen (`ads_on_screen`), once for each, and told of a
/// follow when its link is followed (`followed`).
// CONTRACT: there is no kind for an advertisement, so it is a message at the foot of the
// channel with a card marked `sponsored` that says "Sponsored".
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
        sponsored: true,
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

/// The user followed a link out of a message (the shell is opening it). Only a broadcast
/// channel's ad names a `random_id` (`random_of`): a post's id or anything else decodes to
/// nothing, and Telegram hears nothing, though the link opens all the same.
pub(super) fn followed(account: &Account, conversation: &str, message_id: &str) {
    let Some((who, _)) = Who::parse(conversation) else {
        return;
    };
    if !account.dir().get(who).is_some_and(|known| known.broadcast) {
        return;
    }
    let Some(random_id) = random_of(message_id) else {
        return;
    };
    let _ = account.ask(&tl::functions::messages::ClickSponsoredMessage { media: false, fullscreen: false, random_id });
}

/// The ad id the store knows the ad by is its `random_id` in hex behind "ad" (`sponsored`);
/// the bytes come back.
fn random_of(message_id: &str) -> Option<Vec<u8>> {
    let hex = message_id.strip_prefix("ad")?;
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(&hex[at..at + 2], 16).ok()).collect()
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

    #[test]
    fn an_ad_id_decodes_to_its_random_id_and_any_other_id_decodes_to_nothing() {
        assert_eq!(random_of("adad01"), Some(vec![0xad, 0x01]));
        assert_eq!(random_of("adc0ffee"), Some(vec![0xc0, 0xff, 0xee]));
        assert_eq!(random_of("5"), None);
        assert_eq!(random_of("adlemon"), None);
        assert_eq!(random_of("ad"), None);
    }
}
