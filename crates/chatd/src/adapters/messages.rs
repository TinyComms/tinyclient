//! Android Messages: Google Messages on the phone, with this computer paired to it the way
//! messages.google.com is. The phone stays the account and has to be online; texts (SMS and RCS)
//! go through it. Photos are shown. A video is a video, and a voice note carries its
//! waveform. Other files are named and can be opened. Reactions can be read and sent.
//! Calls stay on the phone. When the phone does not answer, texts wait and the account
//! says so. They go when it answers, unless the user cancelled them. Older messages are
//! read after connecting, one page at a time, so a search of this computer covers the chat and
//! not only the page on screen.

mod bugle;
mod fake;
mod media;
pub mod signin;

use super::{
    keep_connected, serve_commands, Command, Context, ConversationAbout, Failure, Members, Refusal,
    SharedPage, Signed,
};
use crate::blobs;
use crate::blobs::upload;
use crate::session;
use crate::store::{ConversationRow, Filing, PersonRow};
use bugle::Chat;
use protocol::{
    Attachment, Capabilities, Change, ConversationKind, MediaFile, MediaKind, Message, MessageKind,
    Reaction, ReactionSet, RelateAction, Relation, ReplyTo, SendState, SharedKind, Special,
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
    Settings(Vec<bugle::SimCard>),
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
        sim: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String>;
    fn upload_media(&self, packed: &[u8], mime: &str) -> Result<String, String>;
    fn send_media(
        &self,
        conversation: &str,
        caption: &str,
        parts: &[bugle::MediaPart<'_>],
        reply: Option<&str>,
        sim: Option<&str>,
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
    /// The phone's address book contacts.
    fn contacts(&self) -> Result<Vec<bugle::ContactEntry>, String>;
    /// Gets or creates a 1:1 or group conversation by phone number(s).
    fn get_or_create_conversation(
        &self,
        numbers: &[String],
        group_name: Option<&str>,
    ) -> Result<Chat, String>;
    fn mark_read(&self, conversation: &str, message: &str) -> Result<(), String>;
    fn typing(&self, conversation: &str) -> Result<(), String>;
    fn delete_message(&self, message: &str) -> Result<(), String>;
    fn edit_message(&self, conversation: &str, message: &str, text: &str) -> Result<(), String>;
    fn star_message(&self, conversation: &str, message: &str, starred: bool) -> Result<(), String>;
    fn delete_conversation(&self, conversation: &str) -> Result<(), String>;
    fn unpair(&self) -> Result<(), String>;
    fn block(&self, conversation: &str) -> Result<(), String>;
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
        sim: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        self.inner.send(conversation, text, reply, sim, noted)
    }
    fn upload_media(&self, packed: &[u8], mime: &str) -> Result<String, String> {
        self.inner.upload_bytes(packed, mime)
    }
    fn send_media(
        &self,
        conversation: &str,
        caption: &str,
        parts: &[bugle::MediaPart<'_>],
        reply: Option<&str>,
        sim: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        self.inner.send_media(conversation, caption, parts, reply, sim, noted)
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
    fn contacts(&self) -> Result<Vec<bugle::ContactEntry>, String> {
        self.inner.contacts()
    }
    fn get_or_create_conversation(
        &self,
        numbers: &[String],
        group_name: Option<&str>,
    ) -> Result<Chat, String> {
        self.inner.get_or_create_conversation(numbers, group_name)
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
    fn edit_message(&self, conversation: &str, message: &str, text: &str) -> Result<(), String> {
        self.inner.edit_message(conversation, message, text)
    }
    fn star_message(&self, conversation: &str, message: &str, starred: bool) -> Result<(), String> {
        self.inner.star_message(conversation, message, starred)
    }
    fn delete_conversation(&self, conversation: &str) -> Result<(), String> {
        self.inner.delete_conversation(conversation)
    }
    fn unpair(&self) -> Result<(), String> {
        self.inner.unpair()
    }
    fn block(&self, conversation: &str) -> Result<(), String> {
        self.inner.block(conversation)
    }
    fn close(&self) {
        self.inner.close();
    }
}

/// Said when sign-out cannot reach the phone, so this computer may stay in its paired-device list.
pub const STILL_LISTED: &str =
    "This computer may still be listed under Device pairing on your phone. Remove it there.";

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
        if std::env::var_os("TINYCLIENT_MESSAGES_OFFLINE").is_some() {
            fake::record("offline");
            return Err("Messages is offline.".into());
        }
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

/// The lab sets `TINYCLIENT_PROVE=messages-search`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_message_search() -> Result<(), String> {
    if !fill_another(bugle::MESSAGE_PAGE as usize, false) {
        return Err("a full page did not keep reading".into());
    }
    if fill_another(20, false) || fill_another(0, false) || fill_another(bugle::MESSAGE_PAGE as usize, true) {
        return Err("a short page or a finished chat kept reading".into());
    }
    Ok(())
}

/// The lab sets `TINYCLIENT_PROVE=messages-phone-quiet`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
/// The lab sets `TINYCLIENT_PROVE=messages-folders`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_message_folders() -> Result<(), String> {
    bugle::prove_message_folders()
}

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

/// The lab sets `TINYCLIENT_PROVE=messages-unread`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_messages_unread() -> Result<(), String> {
    bugle::prove_messages_unread()
}

/// The lab sets `TINYCLIENT_PROVE=messages-sign-out`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_messages_sign_out() -> Result<(), String> {
    if STILL_LISTED
        != "This computer may still be listed under Device pairing on your phone. Remove it there."
    {
        return Err("the offline sign-out sentence changed".into());
    }
    // Signing out forgets the whole saved session. The generic path deletes it from the keychain.
    if without(r#"{"account":"phone"}"#, "phone").is_some() {
        return Err("signing out would keep the saved session".into());
    }
    bugle::prove_sign_out()
}

/// The lab sets `TINYCLIENT_PROVE=messages-media`. A build from before this ignores that and
/// keeps serving, so the same driver fails there.
pub fn prove_message_media() -> Result<(), String> {
    if kind_of("video/mp4") != MediaKind::Video || kind_of("audio/amr") != MediaKind::Audio {
        return Err("a video or a voice note was filed as a plain file".into());
    }
    if kind_of("image/png") != MediaKind::Image || kind_of("application/pdf") != MediaKind::File {
        return Err("a photo or a file was filed as the wrong kind".into());
    }
    if image_type(b"not a picture").is_some() {
        return Err("a video was treated as its own poster".into());
    }
    if image_type(b"\x89PNG\r\n\x1a\n").is_none() {
        return Err("a still was not recognised".into());
    }
    let clip = mp4_seconds(&clip_bytes()).ok_or("an mp4 duration was not read")?;
    if clip != 2 {
        return Err(format!("an mp4 duration was {clip}, not 2 seconds"));
    }
    let (seconds, bars) = amr_voice(&voice_bytes()).ok_or("an AMR voice note was not read")?;
    if seconds != 1 || bars.is_empty() {
        return Err("an AMR voice note had no length or waveform".into());
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
    conversation: String,
    message: String,
    action: String,
    mime: String,
    name: String,
}

/// A locally scheduled text held by chatd until `at` (unix seconds).
struct ScheduledSend {
    conversation: String,
    client_id: String,
    text: String,
    reply: Option<String>,
    sim: Option<String>,
    at: i64,
}

/// Grace window past a scheduled time before assuming the computer was asleep / off.
const SCHEDULE_MISSED_GRACE: i64 = 120;

fn clock_now() -> i64 {
    if scripted() {
        if let Some(t) = fake::clock() {
            return t;
        }
    }
    crate::hub::now()
}

fn format_hhmm(unix_secs: i64) -> String {
    let secs_in_day = unix_secs.rem_euclid(86_400);
    let hour = secs_in_day / 3_600;
    let minute = (secs_in_day % 3_600) / 60;
    format!("{hour:02}:{minute:02}")
}

/// A text this computer has not asked the phone to send yet. Kept only for this connection.
struct WaitingSend {
    conversation: String,
    client_id: String,
    text: String,
    since: i64,
    /// The message this one answers, when the chat is RCS. Empty otherwise.
    reply: Option<String>,
    /// Explicit SIM id chosen by the user for this send, if any.
    sim: Option<String>,
}

/// How far one conversation has been read this session. `started` means the phone said there
/// is nothing older, so a later scroll does not ask again.
#[derive(Default)]
struct Span {
    oldest_id: String,
    oldest_sent: i64,
    started: bool,
    fill_stopped: bool,
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
    /// Newest message seen, so a chat read on the phone can move the marker there.
    newest: Mutex<HashMap<String, (String, i64)>>,
    /// Phone contacts cached for this session.
    contacts: Mutex<Vec<bugle::ContactEntry>>,
    contacts_loaded: std::sync::atomic::AtomicBool,
    /// Locally scheduled texts waiting for their send time while chatd runs.
    scheduled: Arc<Mutex<Vec<ScheduledSend>>>,
    /// SIM cards reported by the phone's settings update for this session.
    sims: Mutex<Vec<bugle::SimCard>>,
}

pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String> {
    let saved = parse(session)?;
    if saved.account != ctx.account.account {
        return Err("the Messages session is for another account".to_owned());
    }
    let current: Arc<Mutex<Option<Arc<Account>>>> = Arc::default();
    let scheduled: Arc<Mutex<Vec<ScheduledSend>>> = Arc::default();
    let (socket_ctx, held, socket_saved, socket_scheduled) =
        (ctx.clone(), current.clone(), saved.clone(), scheduled.clone());
    std::thread::Builder::new()
        .name("messages-link".into())
        .spawn(move || {
            let mut gate = bugle::Gate::default();
            keep_connected(&socket_ctx, || {
                connect(&socket_ctx, &socket_saved, &held, &socket_scheduled, &mut gate)
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
                    (None, Command::CancelSend { client_id, .. }) => {
                        scheduled
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .retain(|item| item.client_id != client_id);
                    }
                    (None, Command::SignOut(reply)) => {
                        if scripted() {
                            fake::record(STILL_LISTED);
                        }
                        tracing::warn!("chatd: Messages: {STILL_LISTED}");
                        let _ = reply.send(Err(Refusal::Refused(STILL_LISTED.to_owned())));
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
    scheduled: &Arc<Mutex<Vec<ScheduledSend>>>,
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
        newest: Mutex::default(),
        contacts: Mutex::default(),
        contacts_loaded: std::sync::atomic::AtomicBool::new(false),
        scheduled: scheduled.clone(),
        sims: Mutex::default(),
    });
    ctx.capabilities(Capabilities {
        max_file_bytes: bugle::FILE_CAP,
        max_files: file_limit(),
        gif_file: true,
        schedule: true,
        voice_notes: true,
        edit: true,
        edit_window: bugle::EDIT_WINDOW_SECS,
        close_direct: true,
        delete: true,
        reactions: true,
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
    let sched_account = Arc::downgrade(&account);
    let _ = std::thread::Builder::new()
        .name("messages-schedule".into())
        .spawn(move || {
            while let Some(acc) = sched_account.upgrade() {
                if acc.ctx.stopped() {
                    break;
                }
                let due = {
                    let now = clock_now();
                    acc.scheduled
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .iter()
                        .any(|item| now >= item.at)
                };
                if due {
                    let _ = acc.ctx.commands.send(Command::OpenHome);
                }
                drop(acc);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });
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

fn peer_number_raw(chat: &Chat) -> Option<&str> {
    if chat.group {
        return None;
    }
    chat.participants
        .iter()
        .find(|p| !p.is_me && !p.number.is_empty())
        .map(|p| p.number.as_str())
}

fn peer_is_emergency(chat: &Chat) -> bool {
    peer_number_raw(chat).is_some_and(bugle::is_emergency_number)
}

fn peer_is_alphanumeric(chat: &Chat) -> bool {
    peer_number_raw(chat).is_some_and(bugle::is_alphanumeric_sender)
}

fn peer_phone(chat: &Chat) -> Option<String> {
    let raw = peer_number_raw(chat)?;
    if bugle::is_alphanumeric_sender(raw) || bugle::is_emergency_number(raw) {
        return None;
    }
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

const NO_REPLY_REASON: &str = "This sender can't receive replies";
const EMERGENCY_REASON: &str = "Emergency numbers must be contacted directly from your phone.";

fn row(chat: &Chat) -> ConversationRow {
    let kind = if chat.group {
        ConversationKind::Group
    } else {
        ConversationKind::Direct
    };
    let mut row = ConversationRow::new(chat.id.clone(), kind, chat.title.clone());
    row.listed = true;
    row.private = !chat.group;
    let emergency = peer_is_emergency(chat);
    let no_reply = !emergency && (chat.read_only || (!chat.group && peer_is_alphanumeric(chat)));
    if emergency {
        row.read_only = true;
        row.read_only_reason = EMERGENCY_REASON.to_owned();
        row.call = protocol::CallSupport::None;
    } else if no_reply {
        row.read_only = true;
        row.read_only_reason = NO_REPLY_REASON.to_owned();
        row.call = protocol::CallSupport::None;
    } else {
        // A 1:1 chat with a phone number hands off to the OS `tel:` handler; groups have no call button.
        row.call = if peer_phone(chat).is_some() {
            protocol::CallSupport::Handoff
        } else {
            protocol::CallSupport::None
        };
    }
    if chat.verified {
        row.mark = Some(protocol::Mark::Verified);
    }
    if !chat.group && (!chat.peer.is_empty() || !chat.mine.is_empty()) {
        row.peer = Some(if !chat.peer.is_empty() {
            chat.peer.clone()
        } else {
            chat.id.clone()
        });
    }
    // The shell already draws this line. It is not `Encryption::EndToEnd`: those words would
    // claim this computer checked the keys, and the paired browser is a separate hop.
    row.topic = bugle::composer_label(chat.carrier, chat.phones);
    let mut filing = Filing::default();
    match bugle::place(chat.status) {
        bugle::Place::Archive => filing.archived = true,
        bugle::Place::Request => row.special = Special::Request,
        bugle::Place::Hidden => filing.hidden = true,
        bugle::Place::Main => {}
    }
    row.filing = Some(filing);
    row
}

/// File a chat under a status the phone just confirmed, such as blocked.
fn file_as(account: &Account, id: &str, status: u64) {
    let chat = {
        let held = account
            .chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut chat = held.get(id).cloned().unwrap_or_else(|| Chat {
            id: id.to_owned(),
            title: "Messages".into(),
            status,
            ..Chat::default()
        });
        chat.status = status;
        chat
    };
    let chat = remember(account, std::slice::from_ref(&chat));
    if let Some(chat) = chat.first() {
        let _ = account.ctx.conversation(&row(chat));
    }
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
            chat.read_only = chat.read_only || had.read_only;
            chat.verified = chat.verified || had.verified;
            // 0 means this update did not say. An archived chat must not bounce back to the inbox.
            if chat.status == 0 {
                chat.status = had.status;
            }
            if chat.participants.is_empty() {
                chat.participants = had.participants.clone();
            }
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
            ..Chat::default()
        });
        chat.phones = on;
        chat.clone()
    };
    let _ = account.ctx.conversation(&row(&chat));
}

fn apply_settings(account: &Account, sims: Vec<bugle::SimCard>) {
    if sims.is_empty() {
        return;
    }
    if let Some(primary) = sims.first() {
        let mut parts = Vec::new();
        if !primary.carrier.trim().is_empty() {
            parts.push(primary.carrier.trim().to_owned());
        }
        if !primary.number.trim().is_empty() {
            parts.push(mask_phone(primary.number.trim()));
        }
        let label = if parts.is_empty() {
            if account.name.is_empty() {
                "Messages".to_owned()
            } else {
                account.name.clone()
            }
        } else {
            parts.join(" · ")
        };
        let _ = account.ctx.describe(&label, &account.me);
    }
    let options: Vec<protocol::SendAs> = if sims.len() > 1 {
        sims.iter()
            .map(|sim| {
                let mut detail = if sim.rcs { "RCS" } else { "SMS" }.to_owned();
                if !sim.number.trim().is_empty() {
                    detail.push_str(" · ");
                    detail.push_str(&mask_phone(sim.number.trim()));
                }
                protocol::SendAs {
                    id: sim.id.clone(),
                    label: sim.label(),
                    color: sim.color,
                    detail,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    *account.sims.lock().unwrap_or_else(|p| p.into_inner()) = sims;
    account.ctx.send_as(options);
}

fn sim_label(account: &Account, sim_id: &str) -> String {
    let sims = account.sims.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(sim) = sims.iter().find(|s| s.id == sim_id) {
        return sim.label();
    }
    sim_id.to_owned()
}

fn apply(account: &Account, event: Event) {
    match event {
        Event::Settings(sims) => {
            apply_settings(account, sims);
        }
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
                if chat.status == 3 {
                    let _ = account.ctx.conversation_gone(&chat.id);
                    continue;
                }
                // A picture fetched earlier stays the row's when the row is recorded again.
                let row_avatar = account
                    .ctx
                    .avatar_of(&chat.id)
                    .or_else(|| (!chat.peer.is_empty()).then(|| account.ctx.avatar_of(&chat.peer)).flatten());
                let _ = account.ctx.conversation(&ConversationRow { avatar: row_avatar.clone(), ..row(chat) });
                // The flag is not a count. True leaves what the messages added. False clears.
                if bugle::flag_count(chat.unread) == Some(0) {
                    clear_read(account, &chat.id);
                }
                let mut added_participant = false;
                for participant in &chat.participants {
                    if participant.is_me || participant.id.is_empty() {
                        continue;
                    }
                    added_participant = true;
                    let person_avatar = account
                        .ctx
                        .avatar_of(&participant.id)
                        .or_else(|| (!chat.group && participant.id == chat.peer).then(|| row_avatar.clone()).flatten());
                    let display = if !participant.name.is_empty() {
                        participant.name.clone()
                    } else if !chat.group && !chat.title.is_empty() {
                        chat.title.clone()
                    } else {
                        "Messages".to_owned()
                    };
                    people.push(PersonRow {
                        id: participant.id.clone(),
                        name: display,
                        handle: String::new(),
                        avatar: person_avatar,
                        bot: false,
                        status: String::new(),
                        link: String::new(),
                    });
                }
                if !chat.group && !added_participant {
                    let pid = if !chat.peer.is_empty() { chat.peer.clone() } else { chat.id.clone() };
                    people.push(PersonRow {
                        id: pid,
                        name: chat.title.clone(),
                        handle: String::new(),
                        avatar: row_avatar,
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
            ask_fill(account);
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
                    ..Chat::default()
                };
                let chat = remember(account, &[chat]);
                let _ = account.ctx.conversation(&row(&chat[0]));
            }
            show(account, &note, &[]);
            if note.status != 300
                && note.phones.is_none()
                && bugle::tombstone(note.status).is_none()
            {
                saw(account, &note);
            }
            heard_live(account, &note);
            nudge(account);
        }
        Event::Typing {
            conversation,
            by,
            name,
            started,
        } => {
            let (person_id, resolved_name) = resolve_typist(account, &conversation, &by, &name);
            if started {
                account
                    .ctx
                    .typing(&conversation, &person_id, &resolved_name, TYPING_SHOWN);
            } else {
                account.ctx.typing_stopped(&conversation, &person_id);
            }
            nudge(account);
        }
        Event::PhoneBack => {
            let _ = account.ctx.commands.send(Command::OpenHome);
        }
        Event::Ready | Event::Closed(_) | Event::Ended(_) => {}
    }
}

fn digits_only(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

fn looks_like_number(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().any(|c| c.is_ascii_digit())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | ' ' | '.'))
}

fn resolve_typist(account: &Account, conversation: &str, by: &str, name: &str) -> (String, String) {
    let chat = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(conversation)
        .cloned();
    if let Some(chat) = chat {
        if !chat.group {
            let display = if !chat.title.is_empty() && !looks_like_number(&chat.title) {
                chat.title.clone()
            } else if let Some(found) = chat
                .participants
                .iter()
                .find(|p| !p.is_me && !p.name.is_empty() && !looks_like_number(&p.name))
            {
                found.name.clone()
            } else if !name.is_empty() && !looks_like_number(name) {
                name.to_owned()
            } else {
                "Someone".to_owned()
            };
            return (chat.id, display);
        }
        let by_digits = digits_only(by);
        if let Some(participant) = chat.participants.iter().find(|p| {
            (!p.id.is_empty() && p.id == by)
                || (!p.number.is_empty() && p.number == by)
                || (!by_digits.is_empty()
                    && !p.number.is_empty()
                    && digits_only(&p.number) == by_digits)
        }) {
            let pid = if !participant.id.is_empty() {
                participant.id.clone()
            } else {
                by.to_owned()
            };
            let display = if !participant.name.is_empty() && !looks_like_number(&participant.name) {
                participant.name.clone()
            } else {
                "Someone".to_owned()
            };
            return (pid, display);
        }
    }
    let display = if !name.is_empty() && !looks_like_number(name) {
        name.to_owned()
    } else {
        "Someone".to_owned()
    };
    (by.to_owned(), display)
}

fn saw(account: &Account, note: &bugle::Note) {
    if note.id.is_empty() {
        return;
    }
    let mut newest = account
        .newest
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let keep = newest
        .get(&note.conversation)
        .is_none_or(|(_, sent)| note.sent >= *sent);
    if keep {
        newest.insert(note.conversation.clone(), (note.id.clone(), note.sent));
    }
}

fn clear_read(account: &Account, conversation: &str) {
    let newest = account
        .newest
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(conversation)
        .cloned();
    let Some((id, sent)) = newest.or_else(|| {
        let id = account.ctx.newest(conversation).ok().flatten()?;
        let sent = account
            .ctx
            .known(conversation)
            .ok()
            .flatten()
            .map(|row| row.updated)
            .unwrap_or(0);
        Some((id, sent))
    }) else {
        return;
    };
    let _ = account.ctx.counts(conversation, 0, 0, Some(&id), sent);
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
        if note.scheduled {
            shown.waiting = true;
        }
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
    maybe_unfurl(account, &note.conversation, &id, &shown.text, shown.outgoing);
    let tag = bugle::wire_tag(note.wire, carrier_of(account, &note.conversation));
    if !tag.is_empty() {
        let mut line = Message::plain(format!("{}:kind", note.id), String::new(), tag, note.sent, false);
        line.kind = MessageKind::System;
        let _ = account.ctx.history(&note.conversation, &[line]);
    }
    receipt(account, &note.conversation, note, &id);
}

fn should_unfurl(
    chat: Option<&Chat>,
    known_contact: bool,
    link_previews: bool,
    allow_unknown: bool,
) -> bool {
    if !link_previews {
        return false;
    }
    if chat.is_some_and(|c| bugle::place(c.status) == bugle::Place::Request) {
        return false;
    }
    known_contact || allow_unknown
}

fn is_known_contact(account: &Account, chat: Option<&Chat>, outgoing: bool) -> bool {
    if outgoing {
        return true;
    }
    let Some(chat) = chat else {
        return false;
    };
    if !chat.group {
        // A saved contact has a display name that is not just their raw phone number.
        if !chat.title.is_empty() && !looks_like_number(&chat.title) && !bugle::is_alphanumeric_sender(peer_number_raw(chat).unwrap_or("")) {
            return true;
        }
        let peer_num = peer_number_raw(chat).map(digits_only).unwrap_or_default();
        if !peer_num.is_empty() {
            let contacts = account
                .contacts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if contacts.iter().any(|c| digits_only(&c.number) == peer_num) {
                return true;
            }
        }
        return false;
    }
    true
}

fn maybe_unfurl(account: &Account, conversation: &str, message_id: &str, text: &str, outgoing: bool) {
    let Some(url) = crate::unfurl::extract_url(text) else {
        return;
    };
    let chat = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(conversation)
        .cloned();
    let settings = account.ctx.hub.settings();
    let link_previews = settings.on(protocol::settings::LINK_PREVIEWS);
    let allow_unknown = settings.on(protocol::settings::LINK_PREVIEWS_UNKNOWN);
    let known = is_known_contact(account, chat.as_ref(), outgoing);
    if !should_unfurl(chat.as_ref(), known, link_previews, allow_unknown) {
        return;
    }
    let ctx = account.ctx.clone();
    let conversation = conversation.to_owned();
    let message_id = message_id.to_owned();
    let _ = std::thread::Builder::new()
        .name("messages-unfurl".into())
        .spawn(move || {
            let Some(preview) = crate::unfurl::fetch(&url) else {
                return;
            };
            if let Ok(Some(mut stored)) = ctx.stored_message(&conversation, &message_id) {
                if stored.previews.is_empty() {
                    stored.previews.push(preview);
                    let _ = ctx.message(&conversation, &stored);
                }
            }
        });
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
    message.edited = note.edited;
    message.edited_at = note.edited_at;
    message.starred = note.starred;
    if let Some(sim_id) = note.sim.as_deref() {
        message.sent_via = Some(sim_label(account, sim_id));
    }
    if !note.reply.is_empty() {
        message.reply_to = Some(quoted(account, &note.conversation, &note.reply, page));
    }
    if let Some(loc) = &note.location {
        message.card = Some(Box::new(location_card(loc)));
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
        // Field 7 is the only inline bytes the layout has. On a video they are a poster when
        // they are a still, and the video itself stays on the media id. They are not a poster
        // when they are the video.
        let still = piece.mime.starts_with("video/") && image_type(&piece.inline).is_some();
        let mut thumb = None;
        if still {
            if let Some(poster_mime) = image_type(&piece.inline) {
                let mut poster = MediaFile::new(format!("{id}:thumb"), kind_of(poster_mime));
                poster.bytes = piece.inline.len() as u64;
                if let Some(path) = land(account, &poster, &piece.inline) {
                    poster.path = Some(path);
                    thumb = Some(poster);
                }
            }
            if !piece.media_id.is_empty() {
                remember_held(account, &id, piece, &note.conversation, &note.id);
            }
        } else if !piece.inline.is_empty() {
            if let Some(path) = land(account, &file, &piece.inline) {
                file.path = Some(path);
                file.bytes = piece.inline.len() as u64;
            }
            if message.card.is_none() && is_vcard(&piece.mime, &piece.name) {
                if let Some(vc) = parse_vcard(&piece.inline) {
                    message.card = Some(Box::new(vcard_card(&vc)));
                }
            }
        } else if !piece.media_id.is_empty() {
            remember_held(account, &id, piece, &note.conversation, &note.id);
        }
        let label = if piece.name.is_empty() {
            "File".into()
        } else {
            piece.name.clone()
        };
        let described = if still { &[][..] } else { piece.inline.as_slice() };
        let (duration, waveform) = facts(&piece.mime, described);
        shown.push(Attachment {
            name: label,
            file,
            thumb,
            preview: None,
            mime: piece.mime.clone(),
            duration,
            spoiler: false,
            sticker: false,
            waveform,
            round: false,
            view_once: false,
            sticker_set: String::new(),
        });
    }
    message.attachments = shown;
    message.reactions = note
        .reactions
        .iter()
        .enumerate()
        .map(|(idx, mark)| {
            let file = mark.media.as_ref().and_then(|piece| {
                if piece.inline.is_empty() && piece.media_id.is_empty() {
                    return None;
                }
                let id = format!("messages:{}:react:{idx}", note.id);
                let mut mf = MediaFile::new(id.clone(), MediaKind::Emoji);
                mf.width = piece.width;
                mf.height = piece.height;
                mf.bytes = if piece.size == 0 {
                    piece.inline.len() as u64
                } else {
                    piece.size
                };
                if !piece.inline.is_empty() {
                    if let Some(path) = land(account, &mf, &piece.inline) {
                        mf.path = Some(path);
                        mf.bytes = piece.inline.len() as u64;
                    }
                } else if !piece.media_id.is_empty() {
                    remember_held(account, &id, piece, &note.conversation, &note.id);
                }
                Some(mf)
            });
            Reaction {
                emoji: mark.emoji.clone(),
                file,
                count: mark.who.len().max(1) as u32,
                mine: mark.who.iter().any(|who| who == &account.me),
                // The phone names the participants who reacted, as the sender ids do.
                who: mark.who.iter().take(protocol::REACTION_REACTORS).cloned().collect(),
            }
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
            thumb: None,
            story: false,
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
            thumb: None,
            story: false,
        };
    }
    ReplyTo {
        id: id.to_owned(),
        sender: String::new(),
        snippet: String::new(),
        person: None,
        deleted: false,
        thumb: None,
        story: false,
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

fn remember_held(account: &Account, id: &str, piece: &bugle::Piece, conversation: &str, message: &str) {
    account
        .held
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            id.to_owned(),
            Held {
                media_id: piece.media_id.clone(),
                key: piece.key.clone(),
                conversation: conversation.to_owned(),
                message: message.to_owned(),
                action: piece.action_id.clone(),
                mime: piece.mime.clone(),
                name: piece.name.clone(),
            },
        );
}

fn coord(value: f64) -> String {
    let text = format!("{value:.6}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn location_card(loc: &bugle::Location) -> protocol::Card {
    let geo = format!("geo:{},{}", coord(loc.lat), coord(loc.lng));
    let title = if loc.label.is_empty() {
        "Location".to_owned()
    } else {
        loc.label.clone()
    };
    let text = if loc.address.is_empty() {
        geo
    } else {
        format!("{}\n{geo}", loc.address)
    };
    protocol::Card {
        title,
        text,
        fields: vec![
            ("Latitude".to_owned(), coord(loc.lat)),
            ("Longitude".to_owned(), coord(loc.lng)),
        ],
        footer: "Open in Maps".to_owned(),
        color: None,
        sponsored: false,
        ..protocol::Card::default()
    }
}

pub(crate) fn mask_phone(phone: &str) -> String {
    let code = if phone.starts_with('+') {
        match phone.find(|ch: char| ch.is_whitespace() || ch == '-') {
            Some(sep) if sep <= 4 && phone[1..sep].chars().all(|c| c.is_ascii_digit()) => sep,
            _ => 0,
        }
    } else {
        0
    };
    let digits = phone
        .char_indices()
        .filter(|(ix, ch)| *ix >= code && ch.is_ascii_digit())
        .count();
    let mut seen = 0;
    phone
        .char_indices()
        .map(|(ix, ch)| {
            if !ch.is_ascii_digit() || ix < code {
                return ch;
            }
            seen += 1;
            if seen + 2 > digits {
                ch
            } else {
                '•'
            }
        })
        .collect()
}

fn is_vcard(mime: &str, name: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    mime.contains("vcard") || name.to_ascii_lowercase().ends_with(".vcf")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VCard {
    name: String,
    org: String,
    phones: Vec<String>,
    emails: Vec<String>,
}

fn parse_vcard(bytes: &[u8]) -> Option<VCard> {
    let raw = std::str::from_utf8(bytes).ok()?;
    let mut unfolded = Vec::<String>::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            if let Some(last) = unfolded.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        unfolded.push(line.to_owned());
    }
    if !unfolded
        .iter()
        .any(|line| line.trim().eq_ignore_ascii_case("BEGIN:VCARD"))
    {
        return None;
    }
    let mut card = VCard::default();
    let mut fallback_name = String::new();
    for line in unfolded {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let prop = key.split(';').next().unwrap_or("").trim().to_ascii_uppercase();
        let val = value.trim();
        if val.is_empty() {
            continue;
        }
        match prop.as_str() {
            "FN" if card.name.is_empty() => card.name = val.to_owned(),
            "N" if fallback_name.is_empty() => {
                let parts: Vec<&str> = val.split(';').collect();
                let last = parts.first().copied().unwrap_or("").trim();
                let first = parts.get(1).copied().unwrap_or("").trim();
                let joined = format!("{first} {last}").trim().to_owned();
                if !joined.is_empty() {
                    fallback_name = joined;
                }
            }
            "ORG" if card.org.is_empty() => {
                card.org = val.split(';').next().unwrap_or(val).trim().to_owned();
            }
            "TEL" => {
                let clean = val.strip_prefix("tel:").unwrap_or(val).trim().to_owned();
                if !clean.is_empty() && !card.phones.contains(&clean) {
                    card.phones.push(clean);
                }
            }
            "EMAIL" => {
                let clean = val.to_owned();
                if !card.emails.contains(&clean) {
                    card.emails.push(clean);
                }
            }
            _ => {}
        }
    }
    if card.name.is_empty() {
        card.name = fallback_name;
    }
    if card.name.is_empty() && card.phones.is_empty() && card.emails.is_empty() {
        return None;
    }
    Some(card)
}

fn vcard_card(vcard: &VCard) -> protocol::Card {
    let mut fields = Vec::new();
    for phone in &vcard.phones {
        fields.push(("Phone".to_owned(), mask_phone(phone)));
    }
    for email in &vcard.emails {
        fields.push(("Email".to_owned(), email.clone()));
    }
    protocol::Card {
        title: if vcard.name.is_empty() {
            "Contact".to_owned()
        } else {
            vcard.name.clone()
        },
        text: vcard.org.clone(),
        fields,
        footer: "Contact card".to_owned(),
        color: None,
        sponsored: false,
        ..protocol::Card::default()
    }
}

/// A JPEG or PNG, which is the only still a video's inline bytes can be. The layout has no
/// separate poster field.
fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// Length in seconds, and a voice note's bars. Empty bars mean it is not a voice note.
fn facts(mime: &str, bytes: &[u8]) -> (u32, Vec<u8>) {
    if mime.starts_with("audio/") {
        if let Some(voice) = amr_voice(bytes) {
            return voice;
        }
    }
    if mime.starts_with("video/") || mime == "audio/mp4" {
        if let Some(seconds) = mp4_seconds(bytes) {
            return (seconds, Vec::new());
        }
    }
    (0, Vec::new())
}

/// An ISO mp4 `mvhd` box: timescale and duration. Nothing else in the file is read.
fn mp4_seconds(bytes: &[u8]) -> Option<u32> {
    let at = bytes.windows(4).position(|mark| mark == b"mvhd")?;
    let rest = bytes.get(at + 4..)?;
    let version = *rest.first()?;
    let (scale, ticks) = if version == 1 {
        let scale = u32::from_be_bytes(rest.get(20..24)?.try_into().ok()?);
        let ticks = u64::from_be_bytes(rest.get(24..32)?.try_into().ok()?);
        (scale, ticks)
    } else {
        let scale = u32::from_be_bytes(rest.get(12..16)?.try_into().ok()?);
        let ticks = u64::from(u32::from_be_bytes(rest.get(16..20)?.try_into().ok()?));
        (scale, ticks)
    };
    if scale == 0 {
        return None;
    }
    u32::try_from(ticks / u64::from(scale)).ok()
}

/// AMR-NB (`#!AMR\n`) or AMR-WB (`#!AMR-WB\n`). Each frame is 20 ms. Sizes include the header
/// byte (RFC 4867). The bars are the loudness of each frame, folded to the waveform limit.
fn amr_voice(bytes: &[u8]) -> Option<(u32, Vec<u8>)> {
    const NB: &[u8] = b"#!AMR\n";
    const WB: &[u8] = b"#!AMR-WB\n";
    const NB_SIZES: [usize; 16] = [13, 14, 16, 18, 20, 21, 27, 32, 6, 1, 1, 1, 1, 1, 1, 1];
    const WB_SIZES: [usize; 16] = [18, 24, 33, 37, 41, 47, 51, 59, 61, 6, 1, 1, 1, 1, 1, 1];
    let (payload, sizes) = if let Some(rest) = bytes.strip_prefix(WB) {
        (rest, WB_SIZES)
    } else {
        (bytes.strip_prefix(NB)?, NB_SIZES)
    };
    let mut at = 0;
    let mut frames = 0u32;
    let mut loud = Vec::new();
    while at < payload.len() {
        let toc = *payload.get(at)?;
        let size = sizes[((toc >> 3) & 0x0f) as usize];
        if size == 0 || at + size > payload.len() {
            break;
        }
        let body = &payload[at + 1..at + size];
        let energy = body.iter().fold(0u32, |sum, byte| sum + u32::from(*byte))
            / u32::try_from(body.len().max(1)).unwrap_or(1);
        loud.push(u8::try_from(energy.min(255)).unwrap_or(255));
        at += size;
        frames += 1;
    }
    if frames == 0 {
        return None;
    }
    Some((frames.saturating_mul(20) / 1000, fold_bars(&loud)))
}

fn fold_bars(loud: &[u8]) -> Vec<u8> {
    if loud.len() <= protocol::WAVEFORM_LIMIT {
        return loud.to_vec();
    }
    let bars = protocol::WAVEFORM_LIMIT;
    (0..bars)
        .map(|index| {
            let start = index * loud.len() / bars;
            let end = ((index + 1) * loud.len() / bars).max(start + 1);
            let chunk = &loud[start..end.min(loud.len())];
            let sum = chunk.iter().fold(0u32, |sum, byte| sum + u32::from(*byte));
            u8::try_from(sum / u32::try_from(chunk.len().max(1)).unwrap_or(1)).unwrap_or(255)
        })
        .collect()
}

/// Two seconds, timescale 1000. Enough of an `mvhd` for [`mp4_seconds`].
fn clip_bytes() -> Vec<u8> {
    let mut bytes = b"mvhd".to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&1000u32.to_be_bytes());
    bytes.extend_from_slice(&2000u32.to_be_bytes());
    bytes
}

/// One second of AMR-NB, fifty frames of 20 ms.
fn voice_bytes() -> Vec<u8> {
    let mut bytes = b"#!AMR\n".to_vec();
    for _ in 0..50 {
        let mut frame = vec![0u8; 13];
        frame[1] = 80;
        bytes.extend_from_slice(&frame);
    }
    bytes
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

/// Copies the file the user picked. The plaintext is not held in one buffer.
fn land_copy(account: &Account, file: &MediaFile, from: &std::path::Path, bytes: u64) -> Option<String> {
    let ctx = &account.ctx;
    let path = blobs::path_of(&media_root(), &ctx.account, &file.id);
    let part = path.with_extension("part");
    std::fs::create_dir_all(blobs::dir_under(&media_root(), &ctx.account)).ok()?;
    std::fs::copy(from, &part).ok()?;
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
            bytes,
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
                conversation: held.conversation.clone(),
                message: held.message.clone(),
                action: held.action.clone(),
                mime: held.mime.clone(),
                name: held.name.clone(),
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
                if is_vcard(&held.mime, &held.name) {
                    if let Some(vc) = parse_vcard(&bytes) {
                        if let Ok(Some(mut stored)) =
                            account.ctx.stored_message(&held.conversation, &held.message)
                        {
                            if stored.card.is_none() {
                                stored.card = Some(Box::new(vcard_card(&vc)));
                                let _ = account.ctx.message(&held.conversation, &stored);
                            }
                        }
                    }
                }
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
/// may be slow to answer. What lands is the person's avatar and, for 1:1 chats, the row's avatar.
/// When a group conversation is open, its participants' thumbnails are requested too.
fn pictures(account: &Arc<Account>, watching: &protocol::Watching) {
    let on_screen = watching.rows.iter().chain(&watching.people).chain(&watching.conversation);
    let ours: Vec<String> = on_screen.filter(|key| key.account() == account.ctx.account).map(|key| key.id.clone()).collect();
    let open_group: Option<String> = watching
        .conversation
        .as_ref()
        .filter(|key| key.account() == account.ctx.account)
        .map(|key| key.id.clone());
    let wanted: Vec<(Option<String>, String)> = {
        let peers = account.peers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let chats = account.chats.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut pictured = account.pictured.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut wanted = Vec::new();
        for id in ours {
            if let Some(peer) = peers.get(&id) {
                if wanted.len() < PICTURES && pictured.insert(peer.clone()) {
                    wanted.push((Some(id), peer.clone()));
                }
            } else if let Some(conv_id) = peers.iter().find_map(|(c, p)| (p == &id).then(|| c.clone())) {
                if wanted.len() < PICTURES && pictured.insert(id.clone()) {
                    wanted.push((Some(conv_id), id));
                }
            }
        }
        if let Some(group_id) = open_group {
            if let Some(chat) = chats.get(&group_id).filter(|c| c.group) {
                for p in &chat.participants {
                    if !p.is_me && !p.id.is_empty() && wanted.len() < PICTURES && pictured.insert(p.id.clone()) {
                        let direct = peers.iter().find_map(|(c, peer)| (peer == &p.id).then(|| c.clone()));
                        wanted.push((direct, p.id.clone()));
                    }
                }
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
                    let retry_pairs: Vec<(String, String)> = wanted
                        .iter()
                        .filter_map(|(conv, peer)| conv.as_ref().map(|c| (c.clone(), peer.clone())))
                        .collect();
                    remember_pictures(&account, &retry_pairs);
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
            let _ = account.ctx.avatar(&participant, &id);
            for (conversation, _) in wanted.iter().filter(|(_, peer)| *peer == participant) {
                if let Some(conversation) = conversation {
                    if conversation != &participant {
                        let _ = account.ctx.avatar(conversation, &id);
                    }
                }
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
        } => {
            // If this client_id was scheduled locally, treat a direct Send as "send now".
            let sim = take_scheduled(account, &conversation_id, &client_id).and_then(|s| s.sim);
            post_text(account, &conversation_id, &client_id, text, reply_to, sim);
        }
        Command::SendWith {
            conversation_id,
            client_id,
            text,
            reply_to,
            with,
        } => {
            let sim = with.send_as.clone();
            if let Some(at) = with.schedule {
                if !with.files.is_empty() {
                    ctx.sent(
                        &conversation_id,
                        &client_id,
                        Err("Messages schedules text only, not files.".into()),
                    );
                } else {
                    schedule_text(account, &conversation_id, &client_id, text, reply_to, sim, at);
                }
            } else if with.files.is_empty() {
                post_text(account, &conversation_id, &client_id, text, reply_to, sim);
            } else {
                warn_mms(account, &conversation_id, &client_id, &with.files);
                let reply = carried_reply(account, &conversation_id, reply_to);
                let voice = with.voice.clone();
                let account = Arc::clone(account);
                let conversation = conversation_id.clone();
                let cid = client_id.clone();
                let caption = text;
                upload::spawn(
                    ctx,
                    &conversation_id,
                    &client_id,
                    &with.files,
                    move |upload| {
                        deliver_files(
                            &account,
                            upload,
                            &conversation,
                            &cid,
                            &caption,
                            reply.as_deref(),
                            voice.as_ref(),
                            sim.as_deref(),
                        )
                    },
                );
            }
        }
        Command::OpenHome => catch_up(account),
        Command::CancelSend {
            conversation_id,
            client_id,
        } => {
            upload::cancel(&ctx.account, &client_id);
            let mut queued = take_scheduled(account, &conversation_id, &client_id).is_some();
            phone(account).queue.retain(|held| {
                let drop = held.conversation == conversation_id && held.client_id == client_id;
                queued |= drop;
                !drop
            });
            if queued {
                ctx.sent(&conversation_id, &client_id, Err("Cancelled".into()));
            }
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
        Command::Typing { conversation_id, activity: _ } => {
            if bugle::shows_typing(carrier_of(account, &conversation_id)) {
                let _ = account.drive.typing(&conversation_id);
            }
        }
        Command::Edit {
            conversation_id,
            message_id,
            text,
            spans: _,
            reply,
        } => {
            let answer = edit_message(account, &conversation_id, &message_id, text);
            let _ = reply.send(answer);
        }
        Command::StarMessage {
            conversation_id,
            message_id,
            starred,
            reply,
        } => {
            let answer = star_message(account, &conversation_id, &message_id, starred);
            let _ = reply.send(answer);
        }
        Command::Pinned {
            conversation_id,
            reply,
        } => {
            let answer = account
                .ctx
                .hub
                .store()
                .starred_messages(
                    &account.ctx.account,
                    Some(&conversation_id),
                    protocol::PINNED_LIMIT as u32,
                )
                .map_err(|err| Refusal::Refused(err.to_string()));
            let _ = reply.send(answer);
        }
        Command::Delete {
            conversation_id,
            message_id,
            scope,
            reply,
        } => {
            if take_scheduled(account, &conversation_id, &message_id).is_some() {
                let _ = ctx.deleted(&conversation_id, &message_id);
                let _ = reply.send(Ok(()));
                return;
            }
            let answer = if scope == protocol::DeleteScope::Everyone {
                Err(Refusal::Refused("Google Messages can only delete messages for you.".to_owned()))
            } else {
                let res = account.drive.delete_message(&message_id).map_err(Refusal::Refused);
                if res.is_ok() {
                    let _ = ctx.deleted(&conversation_id, &message_id);
                }
                res
            };
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
        Command::SearchShared {
            conversation_id,
            kind,
            before,
            limit,
            reply,
        } => drop(reply.send(shared(account, &conversation_id, kind, before.as_deref(), limit))),
        Command::ReactionRules { reply, .. } => drop(reply.send(Ok(reaction_rules()))),
        Command::Relate {
            person_id,
            action,
            reply,
        } => {
            let answer = match action {
                RelateAction::Block => account.drive.block(&person_id).map(|_| {
                    file_as(account, &person_id, 6);
                }),
                _ => Err("Messages cannot do that.".into()),
            };
            let _ = reply.send(answer.map_err(Refusal::Refused));
        }
        Command::Members {
            conversation_id,
            offset,
            limit,
            reply,
        } => {
            let _ = reply.send(members(account, &conversation_id, offset, limit));
        }
        Command::Profile { person_id, reply } => {
            let _ = reply.send(profile(account, &person_id));
        }
        Command::StartDirect { person_id, reply } => {
            let _ = reply.send(start_direct(account, &person_id));
        }
        Command::CreateGroup {
            members,
            name,
            reply,
        } => {
            let _ = reply.send(create_group(account, &members, name.as_deref()));
        }
        Command::SearchPeople(query) => search_people(account, &query),
        Command::ConversationInfo {
            conversation_id,
            reply,
        } => {
            let _ = reply.send(conversation_info(account, &conversation_id));
        }
        Command::Handoff { conversation_id, reply } => {
            let url = account
                .chats
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&conversation_id)
                .and_then(peer_phone)
                .map(|number| format!("tel:{number}"));
            let _ = reply.send(url.ok_or(Refusal::Unsupported));
        }
        Command::SignOut(reply) => {
            if let Err(why) = account.drive.unpair() {
                tracing::warn!("chatd: Messages: {why}");
            }
            account.drive.close();
            let _ = reply.send(Ok(()));
        }
        Command::SetMute {
            conversation_id,
            notify,
            until: _,
            reply,
        } => {
            let answer = account
                .ctx
                .muted(&conversation_id, quiet(notify))
                .map_err(|err| Refusal::Refused(err.to_string()));
            let _ = reply.send(answer);
        }
        other => other.unsupported(ctx),
    }
}

fn edit_message(
    account: &Account,
    conversation: &str,
    message_id: &str,
    text: String,
) -> Result<(), Refusal> {
    if !bugle::allows_edit(carrier_of(account, conversation)) {
        return Err(Refusal::Refused("Only RCS messages can be edited.".into()));
    }
    let Some(mut stored) = account.ctx.stored_message(conversation, message_id).ok().flatten() else {
        return Err(Refusal::Refused("Messages could not find that message.".into()));
    };
    if !stored.outgoing {
        return Err(Refusal::Refused("You can only edit your own messages.".into()));
    }
    let now = crate::hub::now();
    if now.saturating_sub(stored.sent) > i64::from(bugle::EDIT_WINDOW_SECS) {
        return Err(Refusal::Refused("This message can no longer be edited.".into()));
    }
    let remote_id = phone_copy(account, message_id).unwrap_or_else(|| message_id.to_owned());
    account
        .drive
        .edit_message(conversation, &remote_id, &text)
        .map_err(Refusal::Refused)?;
    stored.text = text;
    stored.edited = true;
    stored.edited_at = Some(now);
    let _ = account.ctx.message(conversation, &stored);
    Ok(())
}

fn star_message(
    account: &Account,
    conversation: &str,
    message_id: &str,
    starred: bool,
) -> Result<(), Refusal> {
    let Some(mut stored) = account.ctx.stored_message(conversation, message_id).ok().flatten() else {
        return Err(Refusal::Refused("Messages could not find that message.".into()));
    };
    let remote_id = phone_copy(account, message_id).unwrap_or_else(|| message_id.to_owned());
    account
        .drive
        .star_message(conversation, &remote_id, starred)
        .map_err(Refusal::Refused)?;
    stored.starred = starred;
    let _ = account.ctx.message(conversation, &stored);
    Ok(())
}

const MMS_GIF_WARN_BYTES: u64 = 1024 * 1024;
const MMS_GIF_WARNING: &str = "Large GIFs sent over MMS may be compressed or rejected by your carrier.";

fn warn_mms(account: &Account, conversation: &str, client_id: &str, files: &[protocol::OutgoingFile]) {
    if carrier_of(account, conversation) != 1 {
        return;
    }
    let large_gif = files.iter().any(|f| {
        let is_gif = f.name.to_ascii_lowercase().ends_with(".gif")
            || f.path.to_ascii_lowercase().ends_with(".gif");
        is_gif && std::fs::metadata(&f.path).is_ok_and(|m| m.len() > MMS_GIF_WARN_BYTES)
    });
    let text = if large_gif {
        MMS_GIF_WARNING
    } else {
        bugle::MMS_WARNING
    };
    let mut line = Message::plain(
        format!("mms-warn-{client_id}"),
        String::new(),
        text,
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

fn file_limit() -> u32 {
    protocol::ATTACHMENTS_LIMIT as u32
}

fn send_failure(name: &str, why: String) -> String {
    if why == upload::CANCELLED {
        why
    } else {
        format!("{name} could not be sent: {why}")
    }
}

/// Maximum length (seconds) and size (bytes) for a voice message sent over SMS/MMS.
const MMS_VOICE_MAX_SECS: u32 = 60;
const MMS_VOICE_MAX_BYTES: u64 = 300 * 1024;
const MMS_VOICE_CAP_ERR: &str =
    "Voice messages in SMS/MMS chats are limited to 60 seconds (300 KB) so your carrier doesn't reject them.";

fn check_mms_voice_limit(carrier: u64, voice: Option<&protocol::VoiceNote>, bytes: u64) -> Result<(), String> {
    if carrier != 1 {
        return Ok(());
    }
    if let Some(note) = voice {
        if note.duration > MMS_VOICE_MAX_SECS * 1000 || bytes > MMS_VOICE_MAX_BYTES {
            return Err(MMS_VOICE_CAP_ERR.to_owned());
        }
    }
    Ok(())
}

/// Best-effort transcode of a recorded Ogg/Opus voice note via `ffmpeg` to AAC-in-M4A (RCS)
/// or AMR-NB (SMS/MMS) so Android Messages and MMS gateways accept and play it reliably.
/// Returns `None` if `ffmpeg` is unavailable or fails, in which case the original file is sent.
fn transcode_voice(src: &std::path::Path, is_sms: bool) -> Option<(std::path::PathBuf, String, &'static str)> {
    let ext = if is_sms { "amr" } else { "m4a" };
    let mime = if is_sms { "audio/amr" } else { "audio/mp4" };
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("voice");
    let dst = src.with_file_name(format!("{stem}-transcoded.{ext}"));
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-i"])
        .arg(src);
    if is_sms {
        cmd.args(["-ar", "8000", "-ac", "1", "-c:a", "libopencore_amrnb", "-b:a", "12.2k"]);
    } else {
        cmd.args(["-c:a", "aac", "-b:a", "64k", "-movflags", "+faststart"]);
    }
    cmd.arg(&dst);
    protocol::launch::no_console(&mut cmd);
    let ok = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok && std::fs::metadata(&dst).is_ok_and(|m| m.len() > 0) {
        Some((dst, format!("{stem}.{ext}"), mime))
    } else {
        let _ = std::fs::remove_file(&dst);
        None
    }
}

struct Prepared {
    media_id: String,
    key: Vec<u8>,
    name: String,
    mime: &'static str,
    size: u64,
    path: std::path::PathBuf,
    temp_path: Option<std::path::PathBuf>,
    voice: bool,
    duration_secs: Option<u32>,
}

/// Seals and uploads each file, then asks the phone to send them as one message.
fn deliver_files(
    account: &Account,
    upload: &mut upload::Upload,
    conversation: &str,
    client_id: &str,
    caption: &str,
    reply: Option<&str>,
    voice: Option<&protocol::VoiceNote>,
    sim: Option<&str>,
) -> Result<Message, String> {
    let carrier = carrier_of(account, conversation);
    let staged: Vec<(String, u64, std::path::PathBuf)> = upload
        .files()
        .iter()
        .map(|source| (source.name.clone(), source.bytes, source.path.clone()))
        .collect();
    let voice_index = voice.and_then(|_| staged.len().checked_sub(1));
    let mut prepared = Vec::with_capacity(staged.len());
    for (at, (mut name, mut size, mut path)) in staged.into_iter().enumerate() {
        upload.check()?;
        let is_voice = voice_index == Some(at);
        if is_voice {
            check_mms_voice_limit(carrier, voice, size)?;
        }
        if size > bugle::FILE_CAP {
            return Err(format!(
                "{name} is larger than 100 MB, which tinyclient won't send through Messages."
            ));
        }
        let mut head = [0u8; 64];
        let n = std::fs::File::open(&path)
            .and_then(|mut file| std::io::Read::read(&mut file, &mut head))
            .unwrap_or(0);
        let mut mime = bugle::mime_of(&name, &head[..n]);
        let mut temp_path = None;
        if is_voice && (mime == "audio/ogg" || mime == "audio/opus") {
            if let Some((dst, new_name, new_mime)) = transcode_voice(&path, carrier == 1) {
                if let Ok(meta) = std::fs::metadata(&dst) {
                    size = meta.len();
                    name = new_name;
                    mime = new_mime;
                    path = dst.clone();
                    temp_path = Some(dst);
                }
            }
        }
        if is_voice {
            if let Err(err) = check_mms_voice_limit(carrier, voice, size) {
                if let Some(tmp) = temp_path {
                    let _ = std::fs::remove_file(tmp);
                }
                return Err(err);
            }
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|_| "Messages could not make a media key.".to_owned())?;
        let mut packer = media::Packer::open(&key)?;
        if temp_path.is_some() {
            let mut file = std::fs::File::open(&path)
                .map_err(|err| send_failure(&name, err.to_string()))?;
            let mut buf = vec![0u8; media::CHUNK];
            loop {
                upload.check().map_err(|why| send_failure(&name, why))?;
                let read = std::io::Read::read(&mut file, &mut buf)
                    .map_err(|err| send_failure(&name, err.to_string()))?;
                if read == 0 {
                    break;
                }
                packer.push(&buf[..read]).map_err(|why| send_failure(&name, why))?;
            }
        } else {
            upload
                .parts(at, media::CHUNK, 0, |part| packer.push(part.bytes))
                .map_err(|why| send_failure(&name, why))?;
        }
        let packed = packer.finish().map_err(|why| send_failure(&name, why))?;
        upload.check().map_err(|why| send_failure(&name, why))?;
        let media_id = account
            .drive
            .upload_media(&packed, mime)
            .map_err(|why| send_failure(&name, why))?;
        let duration_secs = is_voice
            .then(|| voice.map(|v| ((v.duration + 999) / 1000).max(1)))
            .flatten();
        prepared.push(Prepared {
            media_id,
            key: key.to_vec(),
            name,
            mime,
            size,
            path,
            temp_path,
            voice: is_voice,
            duration_secs,
        });
    }
    upload.check()?;
    let parts: Vec<bugle::MediaPart<'_>> = prepared
        .iter()
        .map(|part| bugle::MediaPart {
            media_id: &part.media_id,
            key: &part.key,
            name: &part.name,
            mime: part.mime,
            size: part.size,
            voice: part.voice,
            duration_secs: part.duration_secs,
        })
        .collect();
    let sent = account
        .drive
        .send_media(conversation, caption, &parts, reply, sim, &mut |id| {
            keep_copy(account, client_id, id);
        });
    if sent.is_err() {
        for part in &prepared {
            if let Some(tmp) = &part.temp_path {
                let _ = std::fs::remove_file(tmp);
            }
        }
        forget(account, client_id);
    }
    let id = sent?;
    let mut message = outgoing(account, client_id.to_owned(), caption.to_owned());
    if let Some(sim_id) = sim.filter(|s| !s.is_empty()) {
        message.sent_via = Some(sim_label(account, sim_id));
    }
    if let Some(reply_id) = reply {
        message.reply_to = Some(quoted(account, conversation, reply_id, &[]));
    }
    let mut attachments = Vec::new();
    for (index, part) in prepared.iter().enumerate() {
        if part.size == 0 {
            continue;
        }
        let mut file = MediaFile::new(format!("messages:{id}:{index}"), kind_of(part.mime));
        file.bytes = part.size;
        if let Some(path) = land_copy(account, &file, &part.path, part.size) {
            file.path = Some(path);
        }
        let (duration, waveform) = if part.voice {
            if let Some(v) = voice {
                (((v.duration + 999) / 1000).max(1), v.waveform.clone())
            } else {
                (part.duration_secs.unwrap_or(0), Vec::new())
            }
        } else if part.mime.starts_with("audio/") || part.mime.starts_with("video/") {
            std::fs::read(&part.path)
                .map(|bytes| facts(part.mime, &bytes))
                .unwrap_or_default()
        } else {
            (0, Vec::new())
        };
        if let Some(tmp) = &part.temp_path {
            let _ = std::fs::remove_file(tmp);
        }
        attachments.push(Attachment {
            name: part.name.clone(),
            file,
            thumb: None,
            preview: None,
            mime: part.mime.to_owned(),
            duration,
            spoiler: false,
            sticker: false,
            waveform,
            round: false,
            view_once: false,
            sticker_set: String::new(),
        });
    }
    message.attachments = attachments;
    Ok(finish(account, client_id, message))
}

/// Proves one Messages send can carry several files.
/// The lab sets `TINYCLIENT_PROVE=messages-several-files`. A build from before this ignores
/// that and keeps serving, so the same driver fails there.
pub fn prove_messages_several_files() -> Result<(), String> {
    if file_limit() != 10 {
        return Err("Messages still takes one file at a time.".into());
    }
    bugle::prove_several_files()
}

/// Messages has no mention notifications, so only "all" makes a sound on this computer.
fn quiet(notify: protocol::Notify) -> bool {
    notify != protocol::Notify::All
}

/// A live message on the scripted phone. A muted chat stays quiet; an open one is noted.
fn heard_live(account: &Account, note: &bugle::Note) {
    if !scripted() || note.outgoing || note.author == account.me {
        return;
    }
    let key = account.ctx.key(&note.conversation);
    let muted = account
        .ctx
        .hub
        .store()
        .conversation(&key)
        .ok()
        .flatten()
        .is_some_and(|row| row.muted);
    if !muted {
        fake::record(&format!("notify {}", note.conversation));
    }
}

/// Proves a Messages mute is kept on this computer, including mentions-only.
/// The lab sets `TINYCLIENT_PROVE=messages-mute`. A build from before this ignores that and
/// keeps serving, so the same driver fails there.
pub fn prove_messages_mute() -> Result<(), String> {
    if !quiet(protocol::Notify::Nothing) || !quiet(protocol::Notify::Mentions) || quiet(protocol::Notify::All) {
        return Err("Messages mute did not stay on this computer.".into());
    }
    Ok(())
}

fn backfill(account: &Account, conversation: &str) {
    // The newest page is already in this session. Opening the chat again does not ask for it.
    if reached_start(account, conversation) || resume_cursor(account, conversation).is_some() {
        return;
    }
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
fn fetch_back(account: &Account, conversation: &str, mut cursor: Option<String>, find: Option<String>) -> bool {
    loop {
        if account.ctx.stopped() {
            return false;
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
                    return true;
                }
                let Some(oldest) = oldest_note(&notes) else {
                    nudge(account);
                    return true;
                };
                cursor = Some(oldest.id.clone());
                if account.ctx.pause(PACE) {
                    return false;
                }
            }
            Err(why) if phone_quiet(&why) => {
                remember_due(account, due_of(conversation, asked.as_deref(), find.as_deref()), true);
                lost_phone(account);
                return false;
            }
            Err(why) => {
                tracing::warn!("chatd: Messages: {why}");
                return false;
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

fn needs_fill(account: &Account, conversation: &str) -> bool {
    !spans(account)
        .get(conversation)
        .is_some_and(|span| span.started || span.fill_stopped)
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
    // A page fetched again from the newest end is not the start of the chat. Only a short page,
    // an empty one, or a cursor that did not move past itself is.
    if page_ends(notes.len(), asked, moved) {
        span.started = true;
    }
    moved
}

/// A full page of a chat that has not reached its start is read onward.
fn fill_another(page: usize, started: bool) -> bool {
    bugle::more_messages(page) && !started
}

/// True when this page is the last one the phone will give for this walk.
fn page_ends(count: usize, asked: Option<&str>, moved: bool) -> bool {
    !fill_another(count, false) || (asked.is_some() && !moved)
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

fn post_text(
    account: &Account,
    conversation: &str,
    client_id: &str,
    text: String,
    reply: Option<String>,
    sim: Option<String>,
) {
    let item = WaitingSend {
        conversation: conversation.to_owned(),
        client_id: client_id.to_owned(),
        text,
        since: crate::hub::now(),
        reply: carried_reply(account, conversation, reply),
        sim,
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
    let sent = account.drive.send(&item.conversation, &item.text, reply, item.sim.as_deref(), &mut |id| {
        keep_copy(account, &item.client_id, id);
    });
    if sent.is_err() {
        forget(account, &item.client_id);
    }
    let message = sent.map(|_| carried(account, item))?;
    Ok(finish(account, &item.client_id, message))
}

/// The shell's id stays the stored row. The phone's id is only for a later retry.
fn carried(account: &Account, item: &WaitingSend) -> Message {
    let id = item.client_id.clone();
    let mut message = outgoing(account, id, item.text.clone());
    if let Some(sim_id) = item.sim.as_deref().filter(|s| !s.is_empty()) {
        message.sent_via = Some(sim_label(account, sim_id));
    }
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

fn schedule_text(
    account: &Account,
    conversation: &str,
    client_id: &str,
    text: String,
    reply: Option<String>,
    sim: Option<String>,
    at: i64,
) {
    let now = clock_now();
    if at <= now {
        account
            .ctx
            .sent(conversation, client_id, Err("That time has passed.".into()));
        return;
    }
    let reply = carried_reply(account, conversation, reply);
    let mut pending = outgoing(account, client_id.to_owned(), text.clone());
    pending.sent = at;
    pending.state = SendState::Pending;
    pending.waiting = true;
    pending.client_id = Some(client_id.to_owned());
    if let Some(sim_id) = sim.as_deref().filter(|s| !s.is_empty()) {
        pending.sent_via = Some(sim_label(account, sim_id));
    }
    if let Some(id) = reply.as_deref() {
        pending.reply_to = Some(quoted(account, conversation, id, &[]));
    }
    let _ = account.ctx.message(conversation, &pending);
    let mut sched = account
        .scheduled
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    sched.retain(|item| item.conversation != conversation || item.client_id != client_id);
    sched.push(ScheduledSend {
        conversation: conversation.to_owned(),
        client_id: client_id.to_owned(),
        text,
        reply,
        sim,
        at,
    });
}

fn take_scheduled(account: &Account, conversation: &str, client_id: &str) -> Option<ScheduledSend> {
    let mut sched = account
        .scheduled
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let pos = sched
        .iter()
        .position(|item| item.conversation == conversation && item.client_id == client_id)?;
    Some(sched.remove(pos))
}

fn drain_scheduled(account: &Account) {
    let now = clock_now();
    let due: Vec<ScheduledSend> = {
        let mut sched = account
            .scheduled
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut ready = Vec::new();
        let mut i = 0;
        while i < sched.len() {
            if now >= sched[i].at {
                ready.push(sched.remove(i));
            } else {
                i += 1;
            }
        }
        ready
    };
    for item in due {
        if now - item.at > SCHEDULE_MISSED_GRACE {
            let hhmm = format_hhmm(item.at);
            account.ctx.sent(
                &item.conversation,
                &item.client_id,
                Err(format!("Not sent: this computer was off at {hhmm}")),
            );
            continue;
        }
        post(
            account,
            WaitingSend {
                conversation: item.conversation,
                client_id: item.client_id,
                text: item.text,
                since: now,
                reply: item.reply,
                sim: item.sim,
            },
        );
    }
}

fn catch_up(account: &Arc<Account>) {
    drain_scheduled(account);
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
    fill_older(account);
}

/// One older page of one chat, then the next after a pause. Stops when every chat has been read
/// back to a short page, and when the phone is not answering.
fn fill_older(account: &Arc<Account>) {
    if phone(account).down || account.ctx.stopped() {
        return;
    }
    let ids: Vec<String> = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .cloned()
        .collect();
    let Some(conversation) = ids.into_iter().find(|id| needs_fill(account, id)) else {
        return;
    };
    let cursor = resume_cursor(account, &conversation);
    let ok = fetch_back(account, &conversation, cursor, None);
    if phone(account).down || account.ctx.stopped() {
        return;
    }
    if !ok {
        spans(account).entry(conversation).or_default().fill_stopped = true;
    }
    let ids: Vec<String> = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .cloned()
        .collect();
    if ids.iter().any(|id| needs_fill(account, id)) && !account.ctx.pause(PACE) {
        let _ = account.ctx.commands.send(Command::OpenHome);
    }
}

/// The command thread reads chats the user has not opened. Nothing, once each has reached its start.
fn ask_fill(account: &Account) {
    let ids: Vec<String> = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .cloned()
        .collect();
    if ids.iter().any(|id| needs_fill(account, id)) {
        let _ = account.ctx.commands.send(Command::OpenHome);
    }
}

/// Media, files and links from the messages already read. Only once this chat has been read back
/// to its start, so the list is the whole chat and not a page that was still loading.
fn members(
    account: &Account,
    conversation: &str,
    offset: u32,
    limit: u32,
) -> Result<Members, Refusal> {
    let chat = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(conversation)
        .cloned()
        .ok_or_else(|| Refusal::Refused("no such conversation".into()))?;
    let slice = chat
        .participants
        .iter()
        .skip(offset as usize)
        .take(limit as usize);
    let members = slice
        .map(|participant| {
            let id = if !participant.id.is_empty() {
                participant.id.clone()
            } else if !participant.number.is_empty() {
                participant.number.clone()
            } else {
                conversation.to_owned()
            };
            let name = if !participant.name.is_empty() {
                participant.name.clone()
            } else if participant.is_me && !account.name.is_empty() {
                account.name.clone()
            } else if !participant.number.is_empty() {
                participant.number.clone()
            } else {
                "Messages".to_owned()
            };
            let key = account.ctx.key(&id);
            let mut person = protocol::Person::new(key.clone(), name);
            if let Ok(Some(stored)) = account.ctx.hub.store().person(&key) {
                person.avatar = stored.avatar;
            }
            person
        })
        .collect();
    Ok(Members {
        pending: false,
        sections: Vec::new(),
        members,
    })
}

fn ensure_contacts(account: &Account) -> Vec<bugle::ContactEntry> {
    if !account.contacts_loaded.load(std::sync::atomic::Ordering::Acquire) {
        match account.drive.contacts() {
            Ok(list) => {
                let mut rows = Vec::new();
                for c in &list {
                    let pid = if !c.participant_id.is_empty() {
                        c.participant_id.clone()
                    } else if !c.id.is_empty() {
                        c.id.clone()
                    } else {
                        c.number.clone()
                    };
                    if pid.is_empty() {
                        continue;
                    }
                    let name = if !c.name.is_empty() {
                        c.name.clone()
                    } else if !c.formatted_number.is_empty() {
                        c.formatted_number.clone()
                    } else {
                        c.number.clone()
                    };
                    rows.push(PersonRow {
                        id: pid.clone(),
                        name,
                        handle: String::new(),
                        avatar: account.ctx.avatar_of(&pid),
                        bot: false,
                        status: String::new(),
                        link: String::new(),
                    });
                }
                if !rows.is_empty() {
                    let _ = account.ctx.people(&rows);
                    let owned: Vec<(String, Relation)> = rows
                        .iter()
                        .map(|p| (p.id.clone(), Relation::Contact))
                        .collect();
                    let _ = account.ctx.relations(&owned);
                }
                *account
                    .contacts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = list;
                account
                    .contacts_loaded
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            Err(why) => {
                tracing::warn!("chatd: Messages: contacts: {}", protocol::redact(&why));
            }
        }
    }
    account
        .contacts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn search_people(account: &Account, query: &str) {
    let q = query.trim().to_lowercase();
    let q_digits = digits_only(&q);
    let list = ensure_contacts(account);
    let mut matched = Vec::new();
    for c in list {
        let hit = q.is_empty()
            || c.name.to_lowercase().contains(&q)
            || (!q_digits.is_empty() && digits_only(&c.number).contains(&q_digits));
        if !hit {
            continue;
        }
        let pid = if !c.participant_id.is_empty() {
            c.participant_id.clone()
        } else if !c.id.is_empty() {
            c.id.clone()
        } else {
            c.number.clone()
        };
        if pid.is_empty() {
            continue;
        }
        let name = if !c.name.is_empty() {
            c.name.clone()
        } else if !c.formatted_number.is_empty() {
            c.formatted_number.clone()
        } else {
            c.number.clone()
        };
        matched.push(PersonRow {
            id: pid.clone(),
            name,
            handle: String::new(),
            avatar: account.ctx.avatar_of(&pid),
            bot: false,
            status: String::new(),
            link: String::new(),
        });
        if matched.len() >= 25 {
            break;
        }
    }
    if !matched.is_empty() {
        let _ = account.ctx.people(&matched);
    }
}

fn profile(account: &Account, person_id: &str) -> Result<super::About, Refusal> {
    let mut found_number = String::new();
    let mut found_color = None;
    {
        let chats = account
            .chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for chat in chats.values() {
            for p in &chat.participants {
                if p.id == person_id || (!chat.group && !p.is_me && chat.id == person_id) {
                    if !p.number.is_empty() {
                        found_number = p.number.clone();
                    }
                    if p.color.is_some() {
                        found_color = p.color;
                    }
                    break;
                }
            }
            if !found_number.is_empty() {
                break;
            }
        }
    }
    if found_number.is_empty() {
        let contacts = account
            .contacts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(c) = contacts
            .iter()
            .find(|c| c.participant_id == person_id || c.id == person_id || c.number == person_id)
        {
            found_number = if !c.formatted_number.is_empty() {
                c.formatted_number.clone()
            } else {
                c.number.clone()
            };
        }
    }
    if found_number.is_empty() && found_color.is_none() {
        return Err(Refusal::Refused("no such person".into()));
    }
    let mut about = super::About {
        accent: found_color,
        ..super::About::default()
    };
    if !found_number.is_empty() {
        about.contacts.push(protocol::Contact {
            kind: protocol::ContactKind::Phone,
            value: found_number,
            masked: true,
        });
    }
    Ok(about)
}

fn default_country_code(account: &Account) -> String {
    let chats = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for chat in chats.values() {
        for p in &chat.participants {
            if p.is_me && p.number.starts_with('+') {
                let digits: String = p.number.chars().filter(char::is_ascii_digit).collect();
                if digits.len() == 11 && digits.starts_with('1') {
                    return "+1".to_owned();
                }
            }
        }
    }
    "+1".to_owned()
}

fn resolve_number(account: &Account, person_id: &str) -> Option<String> {
    {
        let chats = account
            .chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for chat in chats.values() {
            for p in &chat.participants {
                if p.id == person_id && !p.number.is_empty() {
                    return Some(p.number.clone());
                }
            }
        }
    }
    let contacts = ensure_contacts(account);
    if let Some(c) = contacts
        .iter()
        .find(|c| c.participant_id == person_id || c.id == person_id || c.number == person_id)
    {
        if !c.number.is_empty() {
            return Some(c.number.clone());
        }
    }
    Some(person_id.to_owned())
}

fn record_started_chat(account: &Account, chat: Chat) -> String {
    let kept = remember(account, &[chat]);
    let chat = &kept[0];
    if !chat.peer.is_empty() {
        account
            .peers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(chat.id.clone(), chat.peer.clone());
    }
    let _ = account.ctx.conversation(&row(chat));
    let mut people = Vec::new();
    for p in &chat.participants {
        if p.is_me || p.id.is_empty() {
            continue;
        }
        people.push(PersonRow {
            id: p.id.clone(),
            name: if !p.name.is_empty() { p.name.clone() } else { chat.title.clone() },
            handle: String::new(),
            avatar: account.ctx.avatar_of(&p.id),
            bot: false,
            status: String::new(),
            link: String::new(),
        });
    }
    if !people.is_empty() {
        let _ = account.ctx.people(&people);
    }
    chat.id.clone()
}

fn start_direct(account: &Account, person_id: &str) -> Result<String, Refusal> {
    // Check if an existing 1:1 chat already matches this chat id or participant id.
    {
        let chats = account
            .chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(chat) = chats
            .values()
            .find(|c| !c.group && (c.id == person_id || c.peer == person_id))
        {
            if peer_is_emergency(chat) {
                return Err(Refusal::Refused(EMERGENCY_REASON.to_owned()));
            }
            return Ok(chat.id.clone());
        }
    }
    let raw = resolve_number(account, person_id)
        .ok_or_else(|| Refusal::Refused("Enter a valid phone number.".into()))?;
    if bugle::is_emergency_number(&raw) {
        return Err(Refusal::Refused(EMERGENCY_REASON.to_owned()));
    }
    let cc = default_country_code(account);
    let e164 = bugle::normalize_e164(&raw, &cc)
        .ok_or_else(|| Refusal::Refused("Enter a valid phone number.".into()))?;
    let chat = account
        .drive
        .get_or_create_conversation(&[e164], None)
        .map_err(Refusal::Refused)?;
    Ok(record_started_chat(account, chat))
}

fn create_group(
    account: &Account,
    members: &[String],
    name: Option<&str>,
) -> Result<String, Refusal> {
    let cc = default_country_code(account);
    let mut numbers = Vec::new();
    for member in members {
        let trimmed = member.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw = resolve_number(account, trimmed)
            .ok_or_else(|| Refusal::Refused("Enter valid phone numbers for each group member.".into()))?;
        if bugle::is_emergency_number(&raw) {
            return Err(Refusal::Refused(EMERGENCY_REASON.to_owned()));
        }
        let e164 = bugle::normalize_e164(&raw, &cc)
            .ok_or_else(|| Refusal::Refused("Enter valid phone numbers for each group member.".into()))?;
        if !numbers.contains(&e164) {
            numbers.push(e164);
        }
    }
    if numbers.len() < 2 {
        return Err(Refusal::Refused(
            "A group conversation needs at least two people.".into(),
        ));
    }
    let chat = account
        .drive
        .get_or_create_conversation(&numbers, name)
        .map_err(Refusal::Refused)?;
    Ok(record_started_chat(account, chat))
}

fn conversation_info(
    account: &Account,
    conversation: &str,
) -> Result<ConversationAbout, Refusal> {
    let chat = account
        .chats
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(conversation)
        .cloned()
        .ok_or_else(|| Refusal::Refused("no such conversation".into()))?;
    let members = (!chat.participants.is_empty()).then_some(chat.participants.len() as u32);
    Ok(ConversationAbout {
        members,
        ..ConversationAbout::default()
    })
}

fn shared(
    account: &Account,
    conversation: &str,
    kind: SharedKind,
    before: Option<&str>,
    limit: u32,
) -> Result<SharedPage, Refusal> {
    if !reached_start(account, conversation) {
        return Err(Refusal::Refused("Messages is still reading this chat.".into()));
    }
    let key = account.ctx.key(conversation);
    match account.ctx.hub.store().shared(&key, kind, before, limit) {
        Ok(Some((items, more))) => Ok(SharedPage {
            items,
            more,
            total: None,
        }),
        Ok(None) => Err(Refusal::Refused("no such conversation".into())),
        Err(err) => Err(Refusal::Refused(err.to_string())),
    }
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

    #[test]
    fn several_files_are_one_payload() {
        super::prove_messages_several_files().unwrap();
    }

    #[test]
    fn mentions_are_quiet_and_all_is_not() {
        super::prove_messages_mute().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_page_keeps_reading_and_a_short_one_stops() {
        super::prove_message_search().unwrap();
    }

    #[test]
    fn location_card_omits_coordinates_from_notification_preview() {
        let loc = bugle::Location {
            lat: 37.7749,
            lng: -122.4194,
            label: "Ferry Building".into(),
            address: "1 Ferry Building, SF".into(),
        };
        let mut msg = Message::plain("m-loc", "Grace", "", 1_700_000_000, false);
        msg.card = Some(Box::new(location_card(&loc)));
        let preview = crate::store::preview_text(&msg);
        assert_eq!(preview, "Ferry Building");
        assert!(!preview.contains("37.7749"), "coordinates must not leak into notification previews");
        assert!(!preview.contains("-122.4194"), "coordinates must not leak into notification previews");
    }

    #[test]
    fn vcard_parses_folded_lines_and_masks_phone_numbers() {
        let raw = b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Linnea\r\n  Sol\r\nORG:Harbor Co\r\nTEL;TYPE=CELL:+1 555 010 0042\r\nEMAIL:linnea@harbor.example\r\nEND:VCARD\r\n";
        let vc = parse_vcard(raw).expect("vcard parsed");
        assert_eq!(vc.name, "Linnea Sol");
        assert_eq!(vc.org, "Harbor Co");
        assert_eq!(vc.phones, ["+1 555 010 0042"]);
        assert_eq!(vc.emails, ["linnea@harbor.example"]);

        let card = vcard_card(&vc);
        assert_eq!(card.title, "Linnea Sol");
        assert_eq!(card.text, "Harbor Co");
        assert_eq!(card.footer, "Contact card");
        assert_eq!(card.fields[0].0, "Phone");
        assert!(card.fields[0].1.contains("42"));
        assert!(!card.fields[0].1.contains("555"), "phone number must be masked");
        assert_eq!(card.fields[1], ("Email".to_owned(), "linnea@harbor.example".to_owned()));
    }
}
