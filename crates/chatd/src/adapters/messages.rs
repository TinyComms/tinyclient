//! Android Messages: Google Messages on the phone, with this computer paired to it the way
//! messages.google.com is. The phone stays the account and has to be online; texts (SMS and RCS)
//! go through it. Photos are shown. Other files are named and can be opened. Reactions can be
//! read and sent. Calls stay on the phone. When the phone does not answer, texts wait and the
//! account says so. They go when it answers, unless the user cancelled them.

mod bugle;
mod fake;
mod media;
pub mod signin;

use super::{keep_connected, serve_commands, Command, Context, Failure, Refusal, Signed};
use crate::blobs;
use crate::session;
use crate::store::{ConversationRow, PersonRow};
use bugle::Chat;
use protocol::{
    Attachment, Capabilities, Change, ConversationKind, MediaFile, MediaKind, Message, MessageKind,
    Reaction, ReactionSet, Relation, SendState,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

/// Others are shown as typing for this long after the phone says they started.
const TYPING_SHOWN: i64 = 15;
/// Most participants whose pictures are asked of the phone in one request.
const PICTURES: usize = 25;
/// A text waits this long for the phone, then it is left unsent.
const HOLD_FOR: i64 = 15 * 60;
/// The account banner while the relay is up and the phone is not answering.
pub const CANT_REACH: &str = "Can't reach your phone";
/// Under a text held until the phone answers. The row stays, so it can be cancelled.
pub const WAITING: &str = "Waiting for your phone";
/// After [`HOLD_FOR`], the text is not sent.
pub const GAVE_UP: &str = "Not sent. Your phone did not answer in time.";

/// True when a request ended because the phone did not answer, not because it refused.
pub fn phone_quiet(why: &str) -> bool {
    why == bugle::PHONE_QUIET
}

/// A held text is still sent at `now` when the phone answers. After fifteen minutes it is not.
pub fn held_text_waits(since: i64, now: i64) -> bool {
    now.saturating_sub(since) < HOLD_FOR
}

fn scripted() -> bool {
    !session::enabled() && std::env::var_os("TINYCLIENT_MESSAGES_FAKE").is_some()
}

pub fn supported() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    cookies: HashMap<String, String>,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    dest_reg_id: String,
    #[serde(default)]
    pairing_id: String,
    #[serde(default)]
    dest_phone_id: u64,
    account: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    aes: String,
    #[serde(default)]
    hmac: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    ttl: i64,
    #[serde(default)]
    mobile_user: i64,
    #[serde(default)]
    mobile: String,
    #[serde(default)]
    browser_user: i64,
    #[serde(default)]
    browser: String,
    #[serde(default)]
    key: String,
}

pub enum Event {
    Ready,
    Chats(Vec<Chat>),
    Message(bugle::Note),
    Typing {
        conversation: String,
        by: String,
        name: String,
        started: bool,
    },
    Closed(String),
    Ended(String),
    /// The scripted phone answers again. The live relay has no separate notice for this:
    /// a message, a chat list, or a later request that succeeds is what says the phone is back.
    PhoneBack,
}

trait Drive: Send + Sync {
    fn events(&self) -> Option<std::sync::mpsc::Receiver<Event>>;
    fn me(&self) -> (String, String);
    fn refresh(&self) -> Result<(), String>;
    fn history(&self, conversation: &str) -> Result<Vec<bugle::Note>, String>;
    fn send(&self, conversation: &str, text: &str) -> Result<String, String>;
    fn send_file(
        &self,
        conversation: &str,
        caption: &str,
        path: &str,
        name: &str,
    ) -> Result<String, String>;
    fn react(&self, message: &str, emoji: &str, on: bool) -> Result<(), String>;
    fn fetch(
        &self,
        media_id: &str,
        key: &[u8],
        message: &str,
        action: &str,
    ) -> Result<Vec<u8>, String>;
    /// The pictures the phone keeps for these participants, as (participant, bytes).
    fn thumbnails(&self, participants: &[String]) -> Result<Vec<(String, Vec<u8>)>, String>;
    fn mark_read(&self, conversation: &str, message: &str) -> Result<(), String>;
    fn typing(&self, conversation: &str) -> Result<(), String>;
    fn close(&self);
}

struct LiveDrive {
    inner: bugle::Session,
}

impl Drive for LiveDrive {
    fn events(&self) -> Option<std::sync::mpsc::Receiver<Event>> {
        self.inner.events()
    }
    fn me(&self) -> (String, String) {
        self.inner.me()
    }
    fn refresh(&self) -> Result<(), String> {
        self.inner.refresh()
    }
    fn history(&self, conversation: &str) -> Result<Vec<bugle::Note>, String> {
        self.inner.history(conversation)
    }
    fn send(&self, conversation: &str, text: &str) -> Result<String, String> {
        self.inner.send(conversation, text)
    }
    fn send_file(
        &self,
        conversation: &str,
        caption: &str,
        path: &str,
        name: &str,
    ) -> Result<String, String> {
        self.inner.send_file(conversation, caption, path, name)
    }
    fn react(&self, message: &str, emoji: &str, on: bool) -> Result<(), String> {
        self.inner.react(message, emoji, on)
    }
    fn fetch(
        &self,
        media_id: &str,
        key: &[u8],
        message: &str,
        action: &str,
    ) -> Result<Vec<u8>, String> {
        self.inner.fetch(media_id, key, message, action)
    }
    fn thumbnails(&self, participants: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.inner.thumbnails(participants)
    }
    fn mark_read(&self, conversation: &str, message: &str) -> Result<(), String> {
        self.inner.mark_read(conversation, message)
    }
    fn typing(&self, conversation: &str) -> Result<(), String> {
        self.inner.typing(conversation)
    }
    fn close(&self) {
        self.inner.close();
    }
}

fn parse(session: &str) -> Result<Saved, String> {
    let saved: Saved = serde_json::from_str(session)
        .map_err(|_| "the Messages session is unreadable".to_owned())?;
    if saved.account.is_empty() {
        return Err("the Messages session names no account".to_owned());
    }
    Ok(saved)
}

pub fn accounts(session: &str) -> Result<Vec<Signed>, String> {
    let saved = parse(session)?;
    Ok(vec![Signed {
        account: saved.account.clone(),
        name: if saved.name.is_empty() {
            "Messages".to_owned()
        } else {
            saved.name
        },
        session: session.to_owned(),
    }])
}

pub fn without(_session: &str, _account: &str) -> Option<String> {
    None
}

fn open(saved: &Saved) -> Result<Box<dyn Drive>, String> {
    if scripted() {
        return Ok(Box::new(fake::Fake::open(saved)));
    }
    let real = bugle::Saved {
        cookies: saved.cookies.clone(),
        session_id: saved.session_id.clone(),
        dest_reg_id: saved.dest_reg_id.clone(),
        pairing_id: saved.pairing_id.clone(),
        dest_phone_id: saved.dest_phone_id,
        account: saved.account.clone(),
        name: saved.name.clone(),
        aes: saved.aes.clone(),
        hmac: saved.hmac.clone(),
        token: saved.token.clone(),
        ttl: saved.ttl,
        mobile_user: saved.mobile_user,
        mobile: if saved.mobile.is_empty() {
            saved.account.clone()
        } else {
            saved.mobile.clone()
        },
        browser_user: saved.browser_user,
        browser: saved.browser.clone(),
        key: saved.key.clone(),
    };
    Ok(Box::new(LiveDrive {
        inner: bugle::Session::open(real)?,
    }))
}

/// The lab sets `TINYCLIENT_PROVE=messages-transport`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_message_transport() -> Result<(), String> {
    bugle::prove_message_transport()
}

/// The lab sets `TINYCLIENT_PROVE=messages-phone-quiet`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_phone_quiet() -> Result<(), String> {
    if !phone_quiet(bugle::PHONE_QUIET) {
        return Err("a quiet phone was not recognised".into());
    }
    if phone_quiet("Messages takes one file at a time.") {
        return Err("a refusal was treated as the phone being quiet".into());
    }
    let now = 1_700_000_000;
    if !held_text_waits(now - (HOLD_FOR - 1), now) {
        return Err("a text inside fifteen minutes was dropped".into());
    }
    if held_text_waits(now - HOLD_FOR, now) {
        return Err("a text at fifteen minutes was still waiting".into());
    }
    if CANT_REACH.is_empty() || WAITING.is_empty() || GAVE_UP.is_empty() {
        return Err("the phone warning has no words".into());
    }
    Ok(())
}

pub fn reaction_rules() -> protocol::ReactionRules {
    protocol::ReactionRules {
        set: ReactionSet::Any,
        mine: 1,
        replaces: true,
        ..protocol::ReactionRules::default()
    }
}

/// A file the shell may ask for once it is on screen. The bytes are still on the phone or in
/// Google's content store.
struct Held {
    media_id: String,
    key: Vec<u8>,
    message: String,
    action: String,
    mime: String,
}

/// A text this computer has not asked the phone to send yet. Kept only for this connection.
struct WaitingSend {
    conversation: String,
    client_id: String,
    text: String,
    /// Empty for a text. A file keeps the path the shell handed over.
    path: String,
    name: String,
    since: i64,
}

/// Work the phone did not answer. The relay can still be up: that is [`CANT_REACH`], not offline.
#[derive(Default)]
struct Phone {
    down: bool,
    queue: VecDeque<WaitingSend>,
    /// Conversations whose history the phone did not return.
    history: VecDeque<String>,
    /// (conversation, participant) whose picture the phone did not return.
    pictures: Vec<(String, String)>,
}

struct Account {
    ctx: Context,
    drive: Box<dyn Drive>,
    me: String,
    name: String,
    held: Mutex<HashMap<String, Held>>,
    /// Each one-to-one chat's other side, by participant id: whose picture its row shows.
    peers: Mutex<HashMap<String, String>>,
    /// Participants whose picture has been asked of the phone on this connection.
    pictured: Mutex<HashSet<String>>,
    /// The last conversation update, so a send knows SMS from RCS.
    chats: Mutex<HashMap<String, Chat>>,
    phone: Mutex<Phone>,
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.account != ctx.account.account {
        return Err("the Messages session is for another account".to_owned());
    }
    let current: Arc<Mutex<Option<Arc<Account>>>> = Arc::default();
    let (socket_ctx, held, socket_saved) = (ctx.clone(), current.clone(), saved.clone());
    std::thread::Builder::new()
        .name("messages-link".into())
        .spawn(move || {
            keep_connected(&socket_ctx, || connect(&socket_ctx, &socket_saved, &held));
            held.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        })
        .map_err(|err| err.to_string())?;
    std::thread::Builder::new()
        .name("messages-commands".into())
        .spawn(move || {
            let account = || {
                current
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            };
            serve_commands(
                &ctx,
                inbox,
                std::time::Duration::from_millis(200),
                |id| {
                    if let Some(account) = account() {
                        backfill(&account, id);
                    }
                },
                |command| match (account(), command) {
                    (Some(account), command) => serve(&account, command),
                    (None, Command::SignOut(reply)) => {
                        let _ = reply.send(Ok(()));
                    }
                    (None, command) => offline(&ctx, command),
                },
            );
            if let Some(account) = account() {
                account.drive.close();
            }
        })
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn connect(ctx: &Context, saved: &Saved, current: &Mutex<Option<Arc<Account>>>) -> Failure {
    let drive = match open(saved) {
        Ok(drive) => drive,
        Err(why) => return Failure::Dropped(why),
    };
    let Some(events) = drive.events() else {
        return Failure::Dropped("Messages has no events".to_owned());
    };
    let (me, name) = drive.me();
    let account = Arc::new(Account {
        ctx: ctx.clone(),
        drive,
        me,
        name,
        held: Mutex::default(),
        peers: Mutex::default(),
        pictured: Mutex::default(),
        chats: Mutex::default(),
        phone: Mutex::default(),
    });
    ctx.capabilities(Capabilities {
        max_file_bytes: 100 * 1024 * 1024,
        max_files: 1,
        ..Capabilities::default()
    });
    let _ = ctx.describe(
        if account.name.is_empty() {
            "Messages"
        } else {
            &account.name
        },
        &account.me,
    );
    *current
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(account.clone());
    if ctx.stopped() {
        account.drive.close();
        return Failure::Dropped("stopped".to_owned());
    }
    ctx.link(protocol::Link::Connected);
    let ended = loop {
        match events.recv() {
            Ok(Event::Ready) => {
                if let Err(why) = account.drive.refresh() {
                    break Failure::Dropped(why);
                }
            }
            Ok(Event::Closed(why)) => break Failure::Dropped(why),
            Ok(Event::Ended(why)) => break Failure::SignedOut(why),
            Ok(event) => apply(&account, event),
            Err(_) => break Failure::Dropped("the connection closed".to_owned()),
        }
        if ctx.stopped() {
            break Failure::Dropped("stopped".to_owned());
        }
    };
    current
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    account.drive.close();
    ended
}

fn row(chat: &Chat) -> ConversationRow {
    let kind = if chat.group {
        ConversationKind::Group
    } else {
        ConversationKind::Direct
    };
    let mut row = ConversationRow::new(chat.id.clone(), kind, chat.title.clone());
    row.listed = true;
    row.private = !chat.group;
    // A call stays on the phone. The row opens messages.google.com.
    row.call = protocol::CallSupport::Handoff;
    if !chat.group && !chat.mine.is_empty() {
        row.peer = Some(chat.id.clone());
    }
    // The shell already draws this line. It is not `Encryption::EndToEnd`: those words would
    // claim this computer checked the keys, and the paired browser is a separate hop.
    row.topic = bugle::composer_label(chat.carrier, chat.phones);
    row
}

fn carrier_of(account: &Account, id: &str) -> u64 {
    account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(id)
        .map(|chat| chat.carrier)
        .unwrap_or(0)
}

/// A conversation update does not clear encryption: the conversation message has no field for
/// it. A tombstone is what turns it on or off.
fn remember(account: &Account, incoming: &[Chat]) -> Vec<Chat> {
    let mut held = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut kept = Vec::with_capacity(incoming.len());
    for chat in incoming {
        let mut chat = chat.clone();
        if let Some(had) = held.get(&chat.id) {
            chat.phones = chat.phones || had.phones;
        }
        held.insert(chat.id.clone(), chat.clone());
        kept.push(chat);
    }
    kept
}

fn seal(account: &Account, conversation: &str, on: bool) {
    let chat = {
        let mut held = account
            .chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let chat = held.entry(conversation.to_owned()).or_insert_with(|| Chat {
            id: conversation.to_owned(),
            title: "Messages".into(),
            group: false,
            unread: false,
            mine: String::new(),
            peer: String::new(),
            carrier: 0,
            phones: false,
        });
        chat.phones = on;
        chat.clone()
    };
    let _ = account.ctx.conversation(&row(&chat));
}

fn apply(account: &Account, event: Event) {
    match event {
        Event::Chats(chats) => {
            let chats = remember(account, &chats);
            let mut people = Vec::new();
            {
                let mut peers = account.peers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                for chat in chats.iter().filter(|chat| !chat.peer.is_empty()) {
                    peers.insert(chat.id.clone(), chat.peer.clone());
                }
            }
            for chat in &chats {
                // A picture fetched earlier stays the row's when the row is recorded again.
                let avatar = account.ctx.avatar_of(&chat.id);
                let _ = account.ctx.conversation(&ConversationRow { avatar: avatar.clone(), ..row(chat) });
                if chat.unread {
                    let _ = account.ctx.counts(&chat.id, 1, 0, None, 0);
                }
                if !chat.group {
                    people.push(PersonRow {
                        id: chat.id.clone(),
                        name: chat.title.clone(),
                        handle: String::new(),
                        avatar,
                        bot: false,
                        status: String::new(),
                        link: String::new(),
                    });
                }
            }
            if !people.is_empty() {
                let _ = account.ctx.people(&people);
                let owned: Vec<(String, Relation)> = people
                    .iter()
                    .map(|person| (person.id.clone(), Relation::Contact))
                    .collect();
                let _ = account.ctx.relations(&owned);
            }
            nudge(account);
        }
        Event::Message(note) => {
            if note.phones.is_none()
                && account
                    .ctx
                    .known(&note.conversation)
                    .ok()
                    .flatten()
                    .is_none()
            {
                let chat = Chat {
                    id: note.conversation.clone(),
                    title: note.name.clone(),
                    group: false,
                    unread: false,
                    mine: String::new(),
                    peer: String::new(),
                    carrier: 0,
                    phones: false,
                };
                let chat = remember(account, &[chat]);
                let _ = account.ctx.conversation(&row(&chat[0]));
            }
            show(account, &note);
            nudge(account);
        }
        Event::Typing {
            conversation,
            by,
            name,
            started,
        } => {
            if started {
                let name = if name.is_empty() {
                    "Someone".into()
                } else {
                    name
                };
                account.ctx.typing(&conversation, &by, &name, TYPING_SHOWN);
            } else {
                account.ctx.typing_stopped(&conversation, &by);
            }
            nudge(account);
        }
        Event::PhoneBack => {
            let _ = account.ctx.commands.send(Command::OpenHome);
        }
        Event::Ready | Event::Closed(_) | Event::Ended(_) => {}
    }
}

fn show(account: &Account, note: &bugle::Note) {
    if let Some(on) = note.phones {
        seal(account, &note.conversation, on);
        return;
    }
    let _ = account.ctx.message(&note.conversation, &message(account, note));
    let tag = bugle::wire_tag(note.wire, carrier_of(account, &note.conversation));
    if !tag.is_empty() {
        let mut line = Message::plain(format!("{}:kind", note.id), String::new(), tag, note.sent, false);
        line.kind = MessageKind::System;
        let _ = account.ctx.history(&note.conversation, &[line]);
    }
    receipt(account, &note.conversation, note);
}

fn message(account: &Account, note: &bugle::Note) -> Message {
    let outgoing = note.outgoing || note.author == account.me;
    let name = if outgoing {
        account.name.clone()
    } else if note.name.is_empty() {
        "Messages".into()
    } else {
        note.name.clone()
    };
    let author = if outgoing {
        account.me.as_str()
    } else {
        note.author.as_str()
    };
    let mut message = Message::plain(
        note.id.clone(),
        name,
        note.text.clone(),
        note.sent,
        outgoing,
    );
    message.sender.id = author.to_owned();
    let mut shown = Vec::new();
    for (index, piece) in note.pieces.iter().enumerate() {
        if piece.inline.is_empty() && piece.media_id.is_empty() {
            continue;
        }
        let id = format!("messages:{}:{index}", note.id);
        let mut file = MediaFile::new(id.clone(), kind_of(&piece.mime));
        file.width = piece.width;
        file.height = piece.height;
        file.bytes = if piece.size == 0 {
            piece.inline.len() as u64
        } else {
            piece.size
        };
        if !piece.inline.is_empty() {
            if let Some(path) = land(account, &file, &piece.inline) {
                file.path = Some(path);
                file.bytes = piece.inline.len() as u64;
            }
        } else {
            account
                .held
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    id,
                    Held {
                        media_id: piece.media_id.clone(),
                        key: piece.key.clone(),
                        message: note.id.clone(),
                        action: piece.action_id.clone(),
                        mime: piece.mime.clone(),
                    },
                );
        }
        let label = if piece.name.is_empty() {
            "File".into()
        } else {
            piece.name.clone()
        };
        shown.push(Attachment {
            name: label,
            file,
            thumb: None,
            mime: piece.mime.clone(),
            duration: 0,
            spoiler: false,
            sticker: false,
            waveform: Vec::new(),
            round: false,
        });
    }
    message.attachments = shown;
    message.reactions = note
        .reactions
        .iter()
        .map(|mark| Reaction {
            emoji: mark.emoji.clone(),
            file: None,
            count: mark.who.len().max(1) as u32,
            mine: mark.who.iter().any(|who| who == &account.me),
        })
        .collect();
    if message.attachments.is_empty() && note.file {
        message.kind = MessageKind::Unsupported {
            link: format!(
                "https://messages.google.com/web/conversations/{}",
                note.conversation
            ),
        };
        if message.text.is_empty() {
            message.text = "File".into();
        }
    }
    message
}

fn kind_of(mime: &str) -> MediaKind {
    if mime == "image/gif" {
        MediaKind::Gif
    } else if mime.starts_with("image/") {
        MediaKind::Image
    } else {
        MediaKind::File
    }
}

fn media_root() -> std::path::PathBuf {
    protocol::support_dir().join("media")
}

/// Writes opened bytes where the cache keeps them. The address is a name; nothing fetches it.
fn land(account: &Account, file: &MediaFile, bytes: &[u8]) -> Option<String> {
    let ctx = &account.ctx;
    let path = blobs::path_of(&media_root(), &ctx.account, &file.id);
    let part = path.with_extension("part");
    std::fs::create_dir_all(blobs::dir_under(&media_root(), &ctx.account)).ok()?;
    std::fs::write(&part, bytes).ok()?;
    std::fs::rename(&part, &path).ok()?;
    let address = format!("messages://file/{}", file.id);
    let mut store = ctx.hub.store();
    store
        .register_media(&ctx.account, &[(file.clone(), address)])
        .ok()?;
    let landed = store
        .media_landed(
            &ctx.account,
            &file.id,
            &path.to_string_lossy(),
            bytes.len() as u64,
            crate::hub::now(),
        )
        .ok()??;
    drop(store);
    ctx.hub.changed(Change::Media {
        account: ctx.account.clone(),
        file: landed,
    });
    Some(path.to_string_lossy().into_owned())
}

fn wanted(account: &Account, ids: &[String]) {
    for id in ids {
        let held = account
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .map(|held| Held {
                media_id: held.media_id.clone(),
                key: held.key.clone(),
                message: held.message.clone(),
                action: held.action.clone(),
                mime: held.mime.clone(),
            });
        let Some(held) = held else {
            continue;
        };
        match account
            .drive
            .fetch(&held.media_id, &held.key, &held.message, &held.action)
        {
            Ok(bytes) => {
                let mut file = MediaFile::new(id.clone(), kind_of(&held.mime));
                file.bytes = bytes.len() as u64;
                let _ = land(account, &file, &bytes);
            }
            Err(why) => {
                let file = MediaFile::new(id.clone(), kind_of(&held.mime));
                let mut store = account.ctx.hub.store();
                let _ = store.register_media(
                    &account.ctx.account,
                    &[(file, format!("messages://file/{id}"))],
                );
                if let Ok(Some(file)) = store.media_failed(&account.ctx.account, id, &why) {
                    drop(store);
                    account.ctx.hub.changed(Change::Media {
                        account: account.ctx.account.clone(),
                        file,
                    });
                }
            }
        }
    }
}

/// The pictures of the people whose chats are on screen (rows, people, the open conversation):
/// each asked of the phone once a connection, together, off the command thread since the phone
/// may be slow to answer. What lands is the row's and the person's avatar. Groups have none:
/// the shell draws their faces.
fn pictures(account: &Arc<Account>, watching: &protocol::Watching) {
    let on_screen = watching.rows.iter().chain(&watching.people).chain(&watching.conversation);
    let ours: Vec<String> = on_screen.filter(|key| key.account() == account.ctx.account).map(|key| key.id.clone()).collect();
    let wanted: Vec<(String, String)> = {
        let peers = account.peers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut pictured = account.pictured.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut wanted = Vec::new();
        for id in ours {
            let Some(peer) = peers.get(&id) else {
                continue;
            };
            if wanted.len() < PICTURES && pictured.insert(peer.clone()) {
                wanted.push((id, peer.clone()));
            }
        }
        wanted
    };
    if wanted.is_empty() {
        return;
    }
    let account = account.clone();
    let spawned = std::thread::Builder::new().name("messages-pictures".into()).spawn(move || {
        let participants: Vec<String> = wanted.iter().map(|(_, peer)| peer.clone()).collect();
        let pictures = match account.drive.thumbnails(&participants) {
            Ok(pictures) => pictures,
            Err(why) => {
                // A quiet phone is asked again when it answers. Anything else waits for the
                // next connection: the participants stay in `pictured`.
                if phone_quiet(&why) {
                    {
                        let mut pictured = account
                            .pictured
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        for (_, peer) in &wanted {
                            pictured.remove(peer);
                        }
                    }
                    remember_pictures(&account, &wanted);
                    lost_phone(&account);
                }
                return tracing::warn!("chatd: Messages: pictures: {why}");
            }
        };
        for (participant, bytes) in pictures {
            // A new picture is a new id, so the shell never draws the old one in its place.
            let id = format!("messages:avatar:{participant}:{:016x}", blobs::fnv_bytes(&bytes));
            let file = MediaFile::new(id.clone(), MediaKind::Avatar);
            if account.ctx.landed(&file, &format!("messages://avatar/{participant}"), &bytes).is_none() {
                continue;
            }
            for (conversation, _) in wanted.iter().filter(|(_, peer)| *peer == participant) {
                let _ = account.ctx.avatar(conversation, &id);
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("chatd: Messages: {err}");
    }
}

fn offline(ctx: &Context, command: Command) {
    let why = "Messages is offline. Keep your phone online.".to_owned();
    match command {
        Command::Send {
            conversation_id,
            client_id,
            ..
        }
        | Command::SendWith {
            conversation_id,
            client_id,
            ..
        } => {
            ctx.sent(&conversation_id, &client_id, Err(why));
        }
        Command::React { reply, .. } => drop(reply.send(Err(Refusal::Refused(why)))),
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        other => other.unsupported(ctx),
    }
}

fn serve(account: &Arc<Account>, command: Command) {
    let ctx = &account.ctx;
    match command {
        Command::Backfill(id) | Command::Warm(id) => backfill(account, &id),
        Command::History {
            conversation_id, ..
        }
        | Command::HistoryAfter {
            conversation_id, ..
        }
        | Command::HistoryAround {
            conversation_id, ..
        } => {
            backfill(account, &conversation_id);
        }
        Command::MarkRead {
            conversation_id,
            message_id,
        } => {
            if let Err(why) = account.drive.mark_read(&conversation_id, &message_id) {
                tracing::warn!("chatd: Messages: {why}");
            }
        }
        Command::Send {
            conversation_id,
            client_id,
            text,
            ..
        } => post_text(account, &conversation_id, &client_id, text),
        Command::SendWith {
            conversation_id,
            client_id,
            text,
            with,
            ..
        } => match with.files.as_slice() {
            [file] => {
                warn_mms(account, &conversation_id, &client_id);
                let name = if file.name.is_empty() {
                    std::path::Path::new(&file.path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("file")
                        .to_owned()
                } else {
                    file.name.clone()
                };
                post_file(
                    account,
                    &conversation_id,
                    &client_id,
                    text,
                    file.path.clone(),
                    name,
                );
            }
            [] => post_text(account, &conversation_id, &client_id, text),
            _ => ctx.sent(
                &conversation_id,
                &client_id,
                Err("Messages takes one file at a time.".into()),
            ),
        },
        Command::OpenHome => catch_up(account),
        Command::CancelSend {
            conversation_id,
            client_id,
        } => {
            phone(account).queue.retain(|held| {
                held.conversation != conversation_id || held.client_id != client_id
            });
            ctx.sent(&conversation_id, &client_id, Err("Cancelled".into()));
        }
        Command::React {
            message_id,
            emoji,
            on,
            reply,
            ..
        } => {
            let answer = account
                .drive
                .react(&message_id, &emoji, on)
                .map_err(Refusal::Refused);
            let _ = reply.send(answer);
        }
        Command::Wanted(ids) => wanted(account, &ids),
        Command::Watch(watching) => pictures(account, &watching),
        Command::Typing(conversation_id) => {
            if bugle::shows_typing(carrier_of(account, &conversation_id)) {
                let _ = account.drive.typing(&conversation_id);
            }
        }
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        Command::Handoff { reply, .. } => {
            let _ = reply.send(Ok("https://messages.google.com/web/".to_owned()));
        }
        Command::SignOut(reply) => {
            account.drive.close();
            let _ = reply.send(Ok(()));
        }
        other => other.unsupported(ctx),
    }
}

fn warn_mms(account: &Account, conversation: &str, client_id: &str) {
    if carrier_of(account, conversation) != 1 {
        return;
    }
    let mut line = Message::plain(
        format!("mms-warn-{client_id}"),
        String::new(),
        bugle::MMS_WARNING,
        crate::hub::now(),
        false,
    );
    line.kind = MessageKind::System;
    let _ = account.ctx.history(conversation, &[line]);
}

fn outgoing(account: &Account, id: String, text: String) -> Message {
    let mut message = Message::plain(id, account.name.clone(), text, crate::hub::now(), true);
    message.sender.id = account.me.clone();
    message
}

/// The second tick or the read mark, when the phone says one of my messages has it.
fn receipt(account: &Account, conversation: &str, note: &bugle::Note) {
    if !bugle::shows_receipts(carrier_of(account, conversation)) {
        return;
    }
    if !(note.outgoing || note.author == account.me) {
        return;
    }
    let _ = match bugle::receipt(note.status) {
        Some(bugle::Receipt::Delivered) => account.ctx.delivered(conversation, &note.id),
        Some(bugle::Receipt::Read) => account.ctx.seen(conversation, &note.id),
        None => Ok(()),
    };
}

fn backfill(account: &Account, conversation: &str) {
    if phone(account).down {
        remember_history(account, conversation);
        return;
    }
    match account.drive.history(conversation) {
        Ok(notes) => {
            for note in notes {
                show(account, &note);
            }
            mark_up(account);
            nudge(account);
        }
        Err(why) if phone_quiet(&why) => {
            remember_history(account, conversation);
            lost_phone(account);
        }
        Err(why) => tracing::warn!("chatd: Messages: {why}"),
    }
}

fn phone(account: &Account) -> std::sync::MutexGuard<'_, Phone> {
    account.phone.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn mark_up(account: &Account) {
    let mut phone = phone(account);
    if !phone.down {
        return;
    }
    phone.down = false;
    drop(phone);
    account.ctx.attention("");
}

fn lost_phone(account: &Account) {
    phone(account).down = true;
    account.ctx.attention(CANT_REACH);
}

/// Ask the command thread to send what is waiting. The link thread must not call the phone itself.
fn nudge(account: &Account) {
    let waiting = {
        let phone = phone(account);
        phone.down || !phone.queue.is_empty() || !phone.history.is_empty() || !phone.pictures.is_empty()
    };
    if waiting {
        let _ = account.ctx.commands.send(Command::OpenHome);
    }
}

fn hold(account: &Account, item: WaitingSend) {
    let conversation = item.conversation.clone();
    let client_id = item.client_id.clone();
    {
        let mut phone = phone(account);
        phone.queue.retain(|held| held.conversation != conversation || held.client_id != client_id);
        phone.queue.push_back(item);
    }
    account.ctx.sent(&conversation, &client_id, Err(WAITING.to_owned()));
}

fn post_text(account: &Account, conversation: &str, client_id: &str, text: String) {
    let item = WaitingSend {
        conversation: conversation.to_owned(),
        client_id: client_id.to_owned(),
        text,
        path: String::new(),
        name: String::new(),
        since: crate::hub::now(),
    };
    post(account, item);
}

fn post_file(
    account: &Account,
    conversation: &str,
    client_id: &str,
    text: String,
    path: String,
    name: String,
) {
    let item = WaitingSend {
        conversation: conversation.to_owned(),
        client_id: client_id.to_owned(),
        text,
        path,
        name,
        since: crate::hub::now(),
    };
    post(account, item);
}

fn post(account: &Account, item: WaitingSend) {
    if phone(account).down {
        hold(account, item);
        return;
    }
    match ask(account, &item) {
        Ok(message) => {
            mark_up(account);
            account.ctx.sent(&item.conversation, &item.client_id, Ok(message));
            nudge(account);
        }
        Err(why) if phone_quiet(&why) => {
            lost_phone(account);
            hold(account, item);
        }
        Err(why) => account.ctx.sent(&item.conversation, &item.client_id, Err(why)),
    }
}

fn ask(account: &Account, item: &WaitingSend) -> Result<Message, String> {
    if item.path.is_empty() {
        return account
            .drive
            .send(&item.conversation, &item.text)
            .map(|id| outgoing(account, id, item.text.clone()));
    }
    let bytes = std::fs::read(&item.path).unwrap_or_default();
    let mime = bugle::mime_of(&item.name);
    account
        .drive
        .send_file(&item.conversation, &item.text, &item.path, &item.name)
        .map(|id| with_file(account, id, item.text.clone(), &item.name, mime, &bytes))
}

fn with_file(
    account: &Account,
    id: String,
    text: String,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> Message {
    let mut message = outgoing(account, id.clone(), text);
    if bytes.is_empty() {
        return message;
    }
    let mut media = MediaFile::new(format!("messages:{id}:0"), kind_of(mime));
    media.bytes = bytes.len() as u64;
    if let Some(path) = land(account, &media, bytes) {
        media.path = Some(path);
    }
    message.attachments = vec![Attachment {
        name: name.to_owned(),
        file: media,
        thumb: None,
        mime: mime.to_owned(),
        duration: 0,
        spoiler: false,
        sticker: false,
        waveform: Vec::new(),
        round: false,
    }];
    message
}

/// The row is still the one we held. A discard or a retry removed it.
fn still_held(account: &Account, item: &WaitingSend) -> bool {
    let key = account.ctx.key(&item.conversation);
    match account.ctx.hub.store().message(&key, &item.client_id) {
        Ok(Some(message)) => matches!(message.state, SendState::Failed { reason } if reason == WAITING),
        _ => false,
    }
}

/// True when the phone is still quiet and the rest of the queue should wait.
fn release(account: &Account, item: WaitingSend) -> bool {
    if !held_text_waits(item.since, crate::hub::now()) {
        account.ctx.sent(&item.conversation, &item.client_id, Err(GAVE_UP.to_owned()));
        return false;
    }
    if !still_held(account, &item) {
        return false;
    }
    match ask(account, &item) {
        Ok(message) => {
            mark_up(account);
            account.ctx.sent(&item.conversation, &item.client_id, Ok(message));
            false
        }
        Err(why) if phone_quiet(&why) => {
            phone(account).queue.push_front(item);
            lost_phone(account);
            true
        }
        Err(why) => {
            account.ctx.sent(&item.conversation, &item.client_id, Err(why));
            false
        }
    }
}

fn remember_history(account: &Account, conversation: &str) {
    let mut phone = phone(account);
    if phone.history.iter().any(|id| id == conversation) {
        return;
    }
    phone.history.push_back(conversation.to_owned());
}

fn remember_pictures(account: &Account, wanted: &[(String, String)]) {
    let mut phone = phone(account);
    for pair in wanted {
        if !phone.pictures.iter().any(|have| have == pair) {
            phone.pictures.push(pair.clone());
        }
    }
}

/// True when this conversation's history still has to wait.
fn retry_history(account: &Account, conversation: &str) -> bool {
    match account.drive.history(conversation) {
        Ok(notes) => {
            for note in notes {
                show(account, &note);
            }
            mark_up(account);
            false
        }
        Err(why) if phone_quiet(&why) => {
            phone(account).history.push_front(conversation.to_owned());
            lost_phone(account);
            true
        }
        Err(why) => {
            tracing::warn!("chatd: Messages: {why}");
            false
        }
    }
}

fn catch_up(account: &Arc<Account>) {
    loop {
        let next = {
            let mut phone = phone(account);
            phone.queue.pop_front()
        };
        let Some(item) = next else {
            break;
        };
        if release(account, item) {
            return;
        }
    }
    loop {
        let next = {
            let mut phone = phone(account);
            phone.history.pop_front()
        };
        let Some(conversation) = next else {
            break;
        };
        if retry_history(account, &conversation) {
            return;
        }
    }
    let again = {
        let mut phone = phone(account);
        std::mem::take(&mut phone.pictures)
    };
    if again.is_empty() {
        return;
    }
    {
        let mut pictured = account
            .pictured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (_, peer) in &again {
            pictured.remove(peer);
        }
    }
    let rows = again
        .into_iter()
        .map(|(id, _)| account.ctx.key(&id))
        .collect();
    pictures(
        account,
        &protocol::Watching {
            rows,
            ..Default::default()
        },
    );
}
