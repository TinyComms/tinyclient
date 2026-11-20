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
    Reaction, ReactionSet, Relation, ReplyTo, SendState,
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
/// Between extra pages of the inbox or of one chat. The command thread already waits this long
/// after a conversation is first opened.
const PACE: std::time::Duration = std::time::Duration::from_millis(200);

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
    /// `before` is the last message already shown, and its time in milliseconds. `None` is the
    /// newest page. Bugle has no forward cursor: older pages are the only ones it can ask for.
    fn history(&self, conversation: &str, before: Option<(&str, i64)>) -> Result<Vec<bugle::Note>, String>;
    /// The next inbox page. Empty when the phone has no more chats.
    fn older_chats(&self) -> Result<Vec<Chat>, String>;
    /// True when the last inbox page was full and named a later one.
    fn inbox_continues(&self) -> bool;
    fn send(
        &self,
        conversation: &str,
        text: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String>;
    fn send_file(
        &self,
        conversation: &str,
        caption: &str,
        path: &str,
        name: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String>;
    /// Asks the phone to send again a copy it already has.
    fn resend(&self, message: &str) -> Result<(), String>;
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
    fn delete_message(&self, message: &str) -> Result<(), String>;
    fn delete_conversation(&self, conversation: &str) -> Result<(), String>;
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
    fn history(&self, conversation: &str, before: Option<(&str, i64)>) -> Result<Vec<bugle::Note>, String> {
        let cursor = before.map(|(id, millis)| bugle::Cursor { id: id.to_owned(), millis });
        self.inner.history(conversation, cursor.as_ref())
    }
    fn older_chats(&self) -> Result<Vec<Chat>, String> {
        self.inner.older_chats()
    }
    fn inbox_continues(&self) -> bool {
        self.inner.inbox_continues()
    }
    fn send(
        &self,
        conversation: &str,
        text: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        self.inner.send(conversation, text, reply, noted)
    }
    fn send_file(
        &self,
        conversation: &str,
        caption: &str,
        path: &str,
        name: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        self.inner.send_file(conversation, caption, path, name, reply, noted)
    }
    fn resend(&self, message: &str) -> Result<(), String> {
        self.inner.resend(message)
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
    fn delete_message(&self, message: &str) -> Result<(), String> {
        self.inner.delete_message(message)
    }
    fn delete_conversation(&self, conversation: &str) -> Result<(), String> {
        self.inner.delete_conversation(conversation)
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

/// The lab sets `TINYCLIENT_PROVE=messages-file-types`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_message_file_types() -> Result<(), String> {
    bugle::prove_message_file_types()
}

/// The lab sets `TINYCLIENT_PROVE=messages-history`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_message_history() -> Result<(), String> {
    bugle::prove_message_history()
}

/// The lab sets `TINYCLIENT_PROVE=messages-reply`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_message_reply() -> Result<(), String> {
    bugle::prove_message_reply()
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

/// The lab sets `TINYCLIENT_PROVE=messages-unpaired`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_messages_unpaired() -> Result<(), String> {
    bugle::prove_unpaired()
}

/// The lab sets `TINYCLIENT_PROVE=messages-delete`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_messages_delete() -> Result<(), String> {
    bugle::prove_messages_delete()
}

/// The lab checks that a phone failure is not shown as sent. A build without this stays up.
pub fn prove_messages_failed_send() -> Result<(), String> {
    if bugle::progress(13) != bugle::Progress::Failed("That message is too large.") {
        return Err("a too-large text was not failed".into());
    }
    if bugle::progress(5) != bugle::Progress::Pending {
        return Err("a text still sending was marked sent".into());
    }
    if bugle::progress(8) != bugle::Progress::Failed("Not sent. No service.") {
        return Err("a text with no service was marked sent".into());
    }
    if !matches!(bugle::progress(99), bugle::Progress::Failed(_))
        || !matches!(bugle::progress(24), bugle::Progress::Failed(_))
    {
        return Err("an unknown failure was marked sent".into());
    }
    if bugle::progress(2) != bugle::Progress::Sent {
        return Err("a delivered text was not sent".into());
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
    /// The message this one answers, when the chat is RCS. Empty otherwise.
    reply: Option<String>,
}

/// How far one conversation has been read this session. `started` means the phone said there
/// is nothing older, so a later scroll does not ask again.
#[derive(Default)]
struct Span {
    oldest_id: String,
    oldest_sent: i64,
    started: bool,
}

/// One history read the phone did not answer. `before` is the cursor. `find` is a message a
/// forward or around read is walking back toward. A retry keeps both, so it does not start over
/// at the newest page.
struct Due {
    conversation: String,
    before: Option<String>,
    find: Option<String>,
}

/// One text this computer asked the phone to send, keyed by the shell's client id.
#[derive(Clone)]
struct Attempt {
    relay_id: String,
    /// The phone's id for that copy. It starts as the temporary id and changes when the phone
    /// names a different one.
    phone_id: String,
    progress: Option<bugle::Progress>,
    detail: String,
}

/// Work the phone did not answer. The relay can still be up: that is [`CANT_REACH`], not offline.
#[derive(Default)]
struct Phone {
    down: bool,
    queue: VecDeque<WaitingSend>,
    /// History reads the phone did not return.
    history: VecDeque<Due>,
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
    /// Per conversation, the oldest page read this session.
    span: Mutex<HashMap<String, Span>>,
    phone: Mutex<Phone>,
    /// Texts asked of the phone on this connection, by the shell's client id.
    outbox: Mutex<HashMap<String, Attempt>>,
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
            let mut gate = bugle::Gate::default();
            keep_connected(&socket_ctx, || {
                connect(&socket_ctx, &socket_saved, &held, &mut gate)
            });
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
                PACE,
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

fn connect(
    ctx: &Context,
    saved: &Saved,
    current: &Mutex<Option<Arc<Account>>>,
    gate: &mut bugle::Gate,
) -> Failure {
    let drive = match open(saved) {
        Ok(drive) => drive,
        Err(why) => return gate.fail(&why),
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
        span: Mutex::default(),
        phone: Mutex::default(),
        outbox: Mutex::default(),
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
                    break gate.fail(&why);
                }
                // Further inbox pages run on the command thread. This loop has to stay free to
                // read the phone.
                if account.drive.inbox_continues() {
                    let _ = account.ctx.commands.send(Command::OpenHome);
                }
            }
            Ok(Event::Closed(why)) => break gate.fail(&why),
            Ok(Event::Ended(why)) => break Failure::SignedOut(why),
            Ok(event) => apply(&account, event),
            Err(_) => break gate.fail("the connection closed"),
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
            updated: 0,
            phones: false,
            gone: false,
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
                if chat.gone {
                    let _ = account.ctx.conversation_gone(&chat.id);
                    continue;
                }
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
            if note.status != 300
                && note.phones.is_none()
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
                    updated: 0,
                    phones: false,
                    gone: false,
                };
                let chat = remember(account, &[chat]);
                let _ = account.ctx.conversation(&row(&chat[0]));
            }
            show(account, &note, &[]);
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

fn show(account: &Account, note: &bugle::Note, page: &[bugle::Note]) {
    if note.status == 300 {
        let _ = account.ctx.deleted(&note.conversation, &note.id);
        return;
    }
    if let Some(on) = note.phones {
        seal(account, &note.conversation, on);
        return;
    }
    if let Some(words) = bugle::tombstone(note.status) {
        let mut line = Message::plain(format!("{}:note", note.id), String::new(), words, note.sent, false);
        line.kind = MessageKind::System;
        let _ = account.ctx.history(&note.conversation, &[line]);
        return;
    }
    let mut shown = message(account, note, page);
    if note.outgoing || note.author == account.me {
        shown.state = state_of(note);
        if let Some(client_id) = claim(account, note) {
            if shown.attachments.is_empty() || shown.text.is_empty() || shown.reply_to.is_none() {
                if let Ok(Some(had)) = account.ctx.stored_message(&note.conversation, &client_id) {
                    if shown.attachments.is_empty() {
                        shown.attachments = had.attachments;
                    }
                    if shown.text.is_empty() {
                        shown.text = had.text;
                    }
                    if shown.reply_to.is_none() {
                        shown.reply_to = had.reply_to;
                    }
                }
            }
            shown.id = client_id.clone();
            shown.client_id = Some(client_id);
        }
    }
    let id = shown.id.clone();
    let _ = account.ctx.message(&note.conversation, &shown);
    let tag = bugle::wire_tag(note.wire, carrier_of(account, &note.conversation));
    if !tag.is_empty() {
        let mut line = Message::plain(format!("{}:kind", note.id), String::new(), tag, note.sent, false);
        line.kind = MessageKind::System;
        let _ = account.ctx.history(&note.conversation, &[line]);
    }
    receipt(account, &note.conversation, note, &id);
}

fn message(account: &Account, note: &bugle::Note, page: &[bugle::Note]) -> Message {
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
    if !note.reply.is_empty() {
        message.reply_to = Some(quoted(account, &note.conversation, &note.reply, page));
    }
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
            preview: None,
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

/// The one line a reply shows. The store has the sender and the snippet. A message on the same
/// page that is not stored yet is read from that page, so a quote is not blank until a reload.
fn quoted(account: &Account, conversation: &str, id: &str, page: &[bugle::Note]) -> ReplyTo {
    if let Some(stored) = account.ctx.stored_message(conversation, id).ok().flatten() {
        let person = (!stored.sender.id.is_empty()).then(|| account.ctx.key(&stored.sender.id));
        return ReplyTo {
            snippet: if stored.deleted {
                String::new()
            } else {
                stored.text.chars().take(120).collect()
            },
            id: stored.id,
            sender: stored.sender.name,
            person,
            deleted: stored.deleted,
        };
    }
    if let Some(note) = page.iter().find(|note| note.id == id && note.phones.is_none()) {
        let outgoing = note.outgoing || note.author == account.me;
        let sender = if outgoing {
            account.name.clone()
        } else {
            note.name.clone()
        };
        let person_id = if outgoing { account.me.as_str() } else { note.author.as_str() };
        return ReplyTo {
            id: id.to_owned(),
            sender,
            snippet: note.text.chars().take(120).collect(),
            person: (!person_id.is_empty()).then(|| account.ctx.key(person_id)),
            deleted: false,
        };
    }
    ReplyTo {
        id: id.to_owned(),
        sender: String::new(),
        snippet: String::new(),
        person: None,
        deleted: false,
    }
}

fn kind_of(mime: &str) -> MediaKind {
    if mime == "image/gif" {
        MediaKind::Gif
    } else if mime.starts_with("image/") {
        MediaKind::Image
    } else if mime.starts_with("video/") {
        MediaKind::Video
    } else if mime.starts_with("audio/") {
        MediaKind::Audio
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
        Command::History { conversation_id, before } => older(account, &conversation_id, &before),
        Command::HistoryAfter { conversation_id, after } => later(account, &conversation_id, &after),
        Command::HistoryAround { conversation_id, around } => surround(account, &conversation_id, &around),
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
            reply_to,
        } => post_text(account, &conversation_id, &client_id, text, reply_to),
        Command::SendWith {
            conversation_id,
            client_id,
            text,
            reply_to,
            with,
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
                    reply_to,
                );
            }
            [] => post_text(account, &conversation_id, &client_id, text, reply_to),
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
        Command::Delete {
            conversation_id,
            message_id,
            reply,
        } => {
            let answer = account.drive.delete_message(&message_id).map_err(Refusal::Refused);
            if answer.is_ok() {
                let _ = ctx.deleted(&conversation_id, &message_id);
            }
            let _ = reply.send(answer);
        }
        // Leave stays unsupported. Closing the chat is what deletes it on the phone.
        Command::CloseDirect { conversation_id, reply } => {
            let answer = account
                .drive
                .delete_conversation(&conversation_id)
                .map_err(Refusal::Refused);
            if answer.is_ok() {
                let _ = ctx.conversation_gone(&conversation_id);
            }
            let _ = reply.send(answer);
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
/// `id` is the shell's id when this computer asked for the copy, otherwise the phone's.
/// A delivered or read copy leaves the outbox even when this chat does not draw ticks.
fn receipt(account: &Account, conversation: &str, note: &bugle::Note, id: &str) {
    if !(note.outgoing || note.author == account.me) {
        return;
    }
    let mark = bugle::receipt(note.status);
    if bugle::shows_receipts(carrier_of(account, conversation)) {
        let _ = match mark {
            Some(bugle::Receipt::Delivered) => account.ctx.delivered(conversation, id),
            Some(bugle::Receipt::Read) => account.ctx.seen(conversation, id),
            None => Ok(()),
        };
    }
    if mark.is_some() {
        settle_map(&mut outbox_mut(account), id, note.status);
    }
}

fn backfill(account: &Account, conversation: &str) {
    if phone(account).down {
        remember_due(account, due_of(conversation, None, None), false);
        return;
    }
    fetch_back(account, conversation, None, None);
}

/// One older page, when the cache has run out. Nothing, once this session has already read it
/// or the phone has said the conversation starts here.
fn older(account: &Account, conversation: &str, before: &str) {
    if older_held(account, conversation, before) {
        return;
    }
    if phone(account).down {
        remember_due(account, due_of(conversation, Some(before), None), false);
        return;
    }
    fetch_back(account, conversation, Some(before.to_owned()), None);
}

/// Messages after `after`. The store already answers when it holds that page. Bugle can only
/// page backward, so a gap at the tip is the newest page, and an older missing message is a
/// walk back to it.
fn later(account: &Account, conversation: &str, after: &str) {
    if forward_held(account, conversation, after) {
        return;
    }
    if phone(account).down {
        remember_due(account, due_of(conversation, None, Some(after)), false);
        return;
    }
    fetch_back(account, conversation, None, Some(after.to_owned()));
}

/// The page around one message. A cached window that already has older and newer rows is left
/// alone. Otherwise the phone is paged backward until the message is in a page, or a short page
/// says the conversation starts.
fn surround(account: &Account, conversation: &str, id: &str) {
    let cached = holds(account, conversation, id);
    if cached && forward_held(account, conversation, id) && (older_held(account, conversation, id) || reached_start(account, conversation)) {
        return;
    }
    if reached_start(account, conversation) {
        return;
    }
    if phone(account).down {
        remember_due(account, due_of(conversation, resume_cursor(account, conversation).as_deref(), Some(id)), false);
        return;
    }
    // A cached message with nothing after it needs the newest page. Anything else resumes from
    // the oldest page already read, so the newest page is not asked for again.
    let cursor = if cached && !forward_held(account, conversation, id) {
        None
    } else {
        resume_cursor(account, conversation)
    };
    fetch_back(account, conversation, cursor, Some(id.to_owned()));
}

fn due_of(conversation: &str, before: Option<&str>, find: Option<&str>) -> Due {
    Due {
        conversation: conversation.to_owned(),
        before: before.map(str::to_owned),
        find: find.map(str::to_owned),
    }
}

/// Reads pages until `find` is on one of them, or one page when `find` is absent. A quiet phone
/// keeps the cursor it had reached. The next try does not start again at the newest page.
fn fetch_back(account: &Account, conversation: &str, mut cursor: Option<String>, find: Option<String>) {
    loop {
        if account.ctx.stopped() {
            return;
        }
        let asked = cursor.clone();
        match ask_history(account, conversation, asked.as_deref()) {
            Ok(notes) => {
                for note in &notes {
                    show(account, note, &notes);
                }
                mark_up(account);
                let moved = note_span(account, conversation, &notes, asked.as_deref());
                let found = match find.as_deref() {
                    None => true,
                    Some(id) => covers(account, conversation, &notes, id),
                };
                if found || !moved || !bugle::more_messages(notes.len()) {
                    nudge(account);
                    return;
                }
                let Some(oldest) = oldest_note(&notes) else {
                    nudge(account);
                    return;
                };
                cursor = Some(oldest.id.clone());
                if account.ctx.pause(PACE) {
                    return;
                }
            }
            Err(why) if phone_quiet(&why) => {
                remember_due(account, due_of(conversation, asked.as_deref(), find.as_deref()), true);
                lost_phone(account);
                return;
            }
            Err(why) => {
                tracing::warn!("chatd: Messages: {why}");
                return;
            }
        }
    }
}

fn ask_history(account: &Account, conversation: &str, before: Option<&str>) -> Result<Vec<bugle::Note>, String> {
    let cursor = before.map(|id| (id, cursor_at(account, conversation, id)));
    account.drive.history(conversation, cursor)
}

fn cursor_at(account: &Account, conversation: &str, id: &str) -> i64 {
    account
        .ctx
        .stored_message(conversation, id)
        .ok()
        .flatten()
        .map(|message| bugle::cursor_millis(message.sent))
        .unwrap_or(0)
}

fn spans(account: &Account) -> std::sync::MutexGuard<'_, HashMap<String, Span>> {
    account.span.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn reached_start(account: &Account, conversation: &str) -> bool {
    spans(account).get(conversation).is_some_and(|span| span.started)
}

/// The oldest message id already read, when a later page should start there.
fn resume_cursor(account: &Account, conversation: &str) -> Option<String> {
    let spans = spans(account);
    let span = spans.get(conversation)?;
    if span.started || span.oldest_id.is_empty() {
        None
    } else {
        Some(span.oldest_id.clone())
    }
}

fn holds(account: &Account, conversation: &str, id: &str) -> bool {
    account.ctx.stored_message(conversation, id).ok().flatten().is_some()
}

/// True when this scroll's page is already in the store, or the phone has said there is nothing
/// older than it. The cursor itself is not older: that page still has to be fetched.
fn older_held(account: &Account, conversation: &str, before: &str) -> bool {
    let (started, oldest_id, oldest_sent) = {
        let spans = spans(account);
        let Some(span) = spans.get(conversation) else {
            return false;
        };
        (span.started, span.oldest_id.clone(), span.oldest_sent)
    };
    if started {
        return true;
    }
    if oldest_id.is_empty() || oldest_id == before {
        return false;
    }
    account
        .ctx
        .stored_message(conversation, before)
        .ok()
        .flatten()
        .is_some_and(|message| message.sent > oldest_sent)
}

/// True when the store already has a message newer than `after`. Compared by id, as
/// [`Context::stored_ids`] stores them. The newest message itself is not a held page: something
/// may have arrived after it.
fn forward_held(account: &Account, conversation: &str, after: &str) -> bool {
    let Ok(Some(newest)) = account.ctx.newest(conversation) else {
        return false;
    };
    if newest == after {
        return false;
    }
    account
        .ctx
        .stored_ids(conversation, after, &newest, 2)
        .map(|ids| ids.len() > 1)
        .unwrap_or(false)
}

fn oldest_note(notes: &[bugle::Note]) -> Option<&bugle::Note> {
    notes
        .iter()
        .filter(|note| note.phones.is_none() && !note.id.is_empty())
        .min_by(|left, right| left.sent.cmp(&right.sent).then_with(|| left.id.cmp(&right.id)))
}

/// True when `id` is on this page, or the page reached back to its time.
fn covers(account: &Account, conversation: &str, notes: &[bugle::Note], id: &str) -> bool {
    if notes.iter().any(|note| note.id == id) {
        return true;
    }
    let Some(sent) = account.ctx.stored_message(conversation, id).ok().flatten().map(|message| message.sent) else {
        return false;
    };
    oldest_note(notes).is_some_and(|oldest| oldest.sent <= sent)
}

/// Records the oldest message of a page. A short page, an empty page, or a page that did not
/// move past the cursor means the conversation has been read back to its start.
fn note_span(account: &Account, conversation: &str, notes: &[bugle::Note], asked: Option<&str>) -> bool {
    let mut spans = spans(account);
    let span = spans.entry(conversation.to_owned()).or_default();
    let Some(oldest) = oldest_note(notes) else {
        span.started = true;
        return false;
    };
    let moved = match asked {
        Some(id) => oldest.id != id,
        None => span.oldest_id != oldest.id,
    };
    if span.oldest_id.is_empty() || oldest.sent < span.oldest_sent || (oldest.sent == span.oldest_sent && oldest.id < span.oldest_id) {
        span.oldest_id = oldest.id.clone();
        span.oldest_sent = oldest.sent;
    }
    if !moved || !bugle::more_messages(notes.len()) {
        span.started = true;
    }
    moved
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

fn post_text(account: &Account, conversation: &str, client_id: &str, text: String, reply: Option<String>) {
    let item = WaitingSend {
        conversation: conversation.to_owned(),
        client_id: client_id.to_owned(),
        text,
        path: String::new(),
        name: String::new(),
        since: crate::hub::now(),
        reply: carried_reply(account, conversation, reply),
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
    reply: Option<String>,
) {
    let item = WaitingSend {
        conversation: conversation.to_owned(),
        client_id: client_id.to_owned(),
        text,
        path,
        name,
        since: crate::hub::now(),
        reply: carried_reply(account, conversation, reply),
    };
    post(account, item);
}

/// The reply reference, only when this chat is RCS. Anywhere else it is left off the request.
fn carried_reply(account: &Account, conversation: &str, reply: Option<String>) -> Option<String> {
    reply.filter(|id| !id.is_empty() && bugle::allows_reply(carrier_of(account, conversation)))
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
    let reply = item.reply.as_deref();
    if let Some(copy) = phone_copy(account, &item.client_id) {
        mark_pending(account, &item.client_id);
        account.drive.resend(&copy)?;
        return Ok(finish(account, &item.client_id, carried(account, item)));
    }
    let sent = if item.path.is_empty() {
        account.drive.send(&item.conversation, &item.text, reply, &mut |id| {
            keep_copy(account, &item.client_id, id);
        })
    } else {
        account.drive.send_file(
            &item.conversation,
            &item.text,
            &item.path,
            &item.name,
            reply,
            &mut |id| keep_copy(account, &item.client_id, id),
        )
    };
    if sent.is_err() {
        forget(account, &item.client_id);
    }
    let message = sent.map(|_| carried(account, item))?;
    Ok(finish(account, &item.client_id, message))
}

/// The shell's id stays the stored row. The phone's id is only for a later retry.
fn carried(account: &Account, item: &WaitingSend) -> Message {
    let id = item.client_id.clone();
    let message = if item.path.is_empty() {
        outgoing(account, id, item.text.clone())
    } else {
        let bytes = std::fs::read(&item.path).unwrap_or_default();
        let mime = bugle::mime_of(&item.name, &bytes);
        with_file(account, id, item.text.clone(), &item.name, mime, &bytes)
    };
    with_reply(account, message, item)
}

fn keep_copy(account: &Account, client_id: &str, relay_id: &str) {
    keep_copy_map(&mut outbox_mut(account), client_id, relay_id);
}

/// Sent copies with no receipt yet. A failure stays, so Try again can find the phone's copy.
const OUTBOX_CAP: usize = 64;

fn keep_copy_map(outbox: &mut HashMap<String, Attempt>, client_id: &str, relay_id: &str) {
    if !outbox.contains_key(client_id) && outbox.len() >= OUTBOX_CAP {
        if let Some(drop_id) = outbox
            .iter()
            .find(|(_, attempt)| matches!(attempt.progress, Some(bugle::Progress::Sent)))
            .map(|(id, _)| id.clone())
        {
            outbox.remove(&drop_id);
        }
    }
    outbox.insert(
        client_id.to_owned(),
        Attempt {
            relay_id: relay_id.to_owned(),
            phone_id: relay_id.to_owned(),
            progress: None,
            detail: String::new(),
        },
    );
}

fn mark_pending(account: &Account, client_id: &str) {
    if let Some(attempt) = outbox_mut(account).get_mut(client_id) {
        attempt.progress = Some(bugle::Progress::Pending);
        attempt.detail.clear();
    }
}

fn forget(account: &Account, client_id: &str) {
    outbox_mut(account).remove(client_id);
}

fn phone_copy(account: &Account, client_id: &str) -> Option<String> {
    outbox_mut(account).get(client_id).map(|attempt| {
        if attempt.phone_id.is_empty() {
            attempt.relay_id.clone()
        } else {
            attempt.phone_id.clone()
        }
    })
}

/// A status that arrived while the send was still on the wire wins over the ack.
fn finish(account: &Account, client_id: &str, mut message: Message) -> Message {
    if let Some(state) = terminal(account, client_id) {
        message.state = state;
    }
    message
}

fn terminal(account: &Account, client_id: &str) -> Option<SendState> {
    let outbox = outbox_mut(account);
    let attempt = outbox.get(client_id)?;
    match attempt.progress? {
        bugle::Progress::Pending => None,
        bugle::Progress::Sent => Some(SendState::Sent),
        bugle::Progress::Failed(words) => Some(SendState::Failed {
            reason: if attempt.detail.is_empty() {
                words.to_owned()
            } else {
                attempt.detail.clone()
            },
        }),
    }
}

fn state_of(note: &bugle::Note) -> SendState {
    match bugle::progress(note.status) {
        bugle::Progress::Sent => SendState::Sent,
        bugle::Progress::Pending => SendState::Pending,
        bugle::Progress::Failed(words) => SendState::Failed {
            reason: if note.detail.is_empty() {
                words.to_owned()
            } else {
                note.detail.clone()
            },
        },
    }
}

fn hits(attempt: &Attempt, value: &str) -> bool {
    !value.is_empty() && (value == attempt.relay_id || value == attempt.phone_id)
}

/// The shell id of a text this computer sent, when the phone's note names that copy.
fn claim(account: &Account, note: &bugle::Note) -> Option<String> {
    claim_map(&mut outbox_mut(account), note)
}

fn claim_map(outbox: &mut HashMap<String, Attempt>, note: &bugle::Note) -> Option<String> {
    let (client_id, attempt) = outbox
        .iter_mut()
        .find(|(_, attempt)| hits(attempt, &note.tmp) || hits(attempt, &note.id))?;
    if !note.id.is_empty() {
        attempt.phone_id = note.id.clone();
    }
    attempt.progress = Some(bugle::progress(note.status));
    attempt.detail = note.detail.clone();
    Some(client_id.clone())
}

fn settle_map(outbox: &mut HashMap<String, Attempt>, client_id: &str, status: u64) {
    if bugle::receipt(status).is_some() {
        outbox.remove(client_id);
    }
}

fn outbox_mut(account: &Account) -> std::sync::MutexGuard<'_, HashMap<String, Attempt>> {
    account.outbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn with_reply(account: &Account, mut message: Message, item: &WaitingSend) -> Message {
    if let Some(id) = item.reply.as_deref() {
        message.reply_to = Some(quoted(account, &item.conversation, id, &[]));
    }
    message
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
        preview: None,
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

fn remember_due(account: &Account, due: Due, front: bool) {
    let mut phone = phone(account);
    if phone.history.iter().any(|have| {
        have.conversation == due.conversation && have.before == due.before && have.find == due.find
    }) {
        return;
    }
    if front {
        phone.history.push_front(due);
    } else {
        phone.history.push_back(due);
    }
}

fn remember_pictures(account: &Account, wanted: &[(String, String)]) {
    let mut phone = phone(account);
    for pair in wanted {
        if !phone.pictures.iter().any(|have| have == pair) {
            phone.pictures.push(pair.clone());
        }
    }
}

/// True when this read still has to wait. One try: a phone that is still quiet stops the queue.
fn retry_due(account: &Account, due: Due) -> bool {
    fetch_back(account, &due.conversation, due.before, due.find);
    phone(account).down
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
        let Some(due) = next else {
            break;
        };
        if retry_due(account, due) {
            return;
        }
    }
    let again = {
        let mut phone = phone(account);
        std::mem::take(&mut phone.pictures)
    };
    if !again.is_empty() {
        {
            let mut pictured = account
                .pictured
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (_, peer) in &again {
                pictured.remove(peer);
            }
        }
        let rows = again.into_iter().map(|(id, _)| account.ctx.key(&id)).collect();
        pictures(
            account,
            &protocol::Watching {
                rows,
                ..Default::default()
            },
        );
    }
    continue_inbox(account);
}

/// One more inbox page, on the command thread. A quiet phone waits for the next home open.
/// Asking again here would spin while it is still down.
fn continue_inbox(account: &Arc<Account>) {
    if phone(account).down || !account.drive.inbox_continues() {
        return;
    }
    match account.drive.older_chats() {
        Ok(_) => {
            if account.drive.inbox_continues() && !account.ctx.pause(PACE) {
                let _ = account.ctx.commands.send(Command::OpenHome);
            }
        }
        Err(why) if phone_quiet(&why) => lost_phone(account),
        Err(why) => tracing::warn!("chatd: Messages: {why}"),
    }
}

#[cfg(test)]
mod outbox_tests {
    use super::{claim_map, keep_copy_map, settle_map, Attempt};

    fn note(id: &str, tmp: &str, status: u64) -> crate::adapters::messages::bugle::Note {
        crate::adapters::messages::bugle::Note {
            id: id.into(),
            tmp: tmp.into(),
            status,
            ..crate::adapters::messages::bugle::Note::default()
        }
    }

    #[test]
    fn a_delivered_or_read_copy_leaves_and_a_failure_stays() {
        let mut outbox = std::collections::HashMap::new();
        keep_copy_map(&mut outbox, "c-ok", "relay-ok");
        let id = claim_map(&mut outbox, &note("phone-ok", "relay-ok", 2)).unwrap();
        settle_map(&mut outbox, &id, 2);
        assert!(outbox.get("c-ok").is_none(), "a delivered copy stayed in the outbox");

        keep_copy_map(&mut outbox, "c-read", "relay-read");
        let id = claim_map(&mut outbox, &note("phone-read", "relay-read", 11)).unwrap();
        settle_map(&mut outbox, &id, 11);
        assert!(outbox.get("c-read").is_none(), "a read copy stayed in the outbox");

        keep_copy_map(&mut outbox, "c-fail", "relay-fail");
        let id = claim_map(&mut outbox, &note("phone-fail", "relay-fail", 13)).unwrap();
        settle_map(&mut outbox, &id, 13);
        let kept = outbox.get("c-fail").expect("Try again lost the phone's failed copy");
        assert_eq!(kept.phone_id, "phone-fail");
        assert!(matches!(kept.progress, Some(super::bugle::Progress::Failed(_))));
        assert_eq!(id, "c-fail");
    }

    #[test]
    fn a_full_outbox_drops_a_sent_copy_and_keeps_a_failure() {
        let mut sent = std::collections::HashMap::<String, Attempt>::new();
        for n in 0..64 {
            let client = format!("c-{n}");
            let relay = format!("relay-{n}");
            keep_copy_map(&mut sent, &client, &relay);
            claim_map(&mut sent, &note(&format!("phone-{n}"), &relay, 1));
        }
        keep_copy_map(&mut sent, "c-new", "relay-new");
        assert_eq!(sent.len(), 64);
        assert!(sent.contains_key("c-new"));
        let still_sent = sent.values().filter(|attempt| matches!(attempt.progress, Some(super::bugle::Progress::Sent))).count();
        assert_eq!(still_sent, 63);

        let mut failed = std::collections::HashMap::new();
        for n in 0..64 {
            let client = format!("f-{n}");
            let relay = format!("relay-f-{n}");
            keep_copy_map(&mut failed, &client, &relay);
            claim_map(&mut failed, &note(&format!("phone-f-{n}"), &relay, 13));
        }
        keep_copy_map(&mut failed, "f-new", "relay-f-new");
        assert_eq!(failed.len(), 65, "a failed copy was dropped to make room");
        assert!(failed.contains_key("f-new"));
        assert_eq!(failed.keys().filter(|id| *id != "f-new").count(), 64);
    }
}
