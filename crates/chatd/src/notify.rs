//! System notifications, from chatd, so they arrive with the window closed
//! (`docs/product/09-settings.md` N1 to N13 and its silence rules, `docs/design/06-live-signals.md`
//! §6). Three parts:
//!
//! - [`decide`] and [`decide_ring`]: whether a live message or a ring notifies, with what sound
//!   and how much it shows. Pure, so every rule is tested on its own.
//! - [`Notes`]: one notification per conversation, replaced in place with its newest three
//!   lines, withdrawn once the conversation is read anywhere; the ring's own notification,
//!   withdrawn when the ring stops. It posts to a `notifier::Sink` and says which sound to play.
//! - [`start`]: the thread that owns them, fed by the hub ([`Event`]), that plays sounds with
//!   `mediad --sound` and carries out what the user did with a notification through the same
//!   requests the window sends.

use crate::hub::{lock, now, Hub};
use crate::server::Chat;
use notifier::{Activation, Kind, Note, Sink};
use protocol::settings::{self, Values, GLOBAL};
use protocol::{Call, CallState, Conversation, ConversationKind, Key, Message, Network, Request, Response, Special, Watching};
use std::collections::{HashMap, VecDeque};
use protocol::media_host::MediaChild;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// A message older than this when it is heard is catch-up, not news: history at connect.
const STALE: i64 = 120;
/// No second sound for a conversation within this of its last one.
const REPLACED_QUIET: Duration = Duration::from_secs(10);
/// No sound within this of any other.
const ANY_QUIET: Duration = Duration::from_secs(3);
/// Lines kept in a conversation's notification.
const LINES: usize = 3;
/// A shell that asked `Wait` this recently is up; a click older than that starts one.
const SHELL_ALIVE: Duration = Duration::from_secs(45);

/// How much a notification shows (N4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    Full,
    Sender,
    Nothing,
}

impl Preview {
    fn of(word: &str) -> Preview {
        match word {
            "full" => Preview::Full,
            "sender" => Preview::Sender,
            "nothing" => Preview::Nothing,
            _ => Preview::Sender,
        }
    }
}

/// What is true when a message arrives, beyond the settings.
#[derive(Debug, Clone, Default)]
pub struct Moment {
    /// Unix seconds now.
    pub now: i64,
    /// Minutes after local midnight now, for quiet hours.
    pub minute: u32,
    /// The account's own status is Do Not Disturb on the network.
    pub dnd: bool,
    /// I am in a call (joined, connecting or reconnecting).
    pub in_call: bool,
    /// What the window shows, and whether it has focus.
    pub watching: Watching,
    pub locked: bool,
    /// This conversation sounded within [`REPLACED_QUIET`].
    pub sounded_here: bool,
    /// Anything sounded within [`ANY_QUIET`].
    pub sounded_any: bool,
}

/// To post, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Post a notification. Off for a ring while the window has focus: its own card shows it.
    pub banner: bool,
    /// The sound's settings word, or `None` for a silent notification.
    pub sound: Option<String>,
    pub preview: Preview,
    pub mention: bool,
}

/// Whether a quiet-hours window holds `minute`; one that crosses midnight holds both ends.
pub fn in_window(from: u32, to: u32, minute: u32) -> bool {
    match from.cmp(&to) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => (from..to).contains(&minute),
        std::cmp::Ordering::Greater => minute >= from || minute < to,
    }
}

/// N7 and N8: nothing is posted now.
fn hushed(values: &Values, moment: &Moment) -> bool {
    let quiet = values.on(settings::QUIET_HOURS) && in_window(values.minute(settings::QUIET_FROM), values.minute(settings::QUIET_TO), moment.minute);
    quiet || values.paused(moment.now).is_some()
}

/// The level that applies to a conversation: its own, else its account's, else the global one.
fn level<'a>(values: &'a Values, conversation: &Key) -> &'a str {
    let own = values.stored(&settings::conversation_scope(conversation), settings::NOTIFY_ABOUT);
    let account = values.stored(&settings::account_scope(&conversation.account()), settings::NOTIFY_ABOUT);
    own.or(account).unwrap_or_else(|| values.word(GLOBAL, settings::NOTIFY_ABOUT))
}

/// The sound for a message: the conversation's (S6), the account's (S5), then S3 or S2.
fn sound_word(values: &Values, conversation: &Key, mention: bool) -> String {
    let own = values.stored(&settings::conversation_scope(conversation), settings::SOUND);
    let account = values.stored(&settings::account_scope(&conversation.account()), settings::SOUND);
    let message = values.word(GLOBAL, settings::MESSAGE_SOUND);
    let chosen = own.or(account).unwrap_or_else(|| match values.word(GLOBAL, settings::MENTION_SOUND) {
        "same" => message,
        mention_sound if mention => mention_sound,
        _ => message,
    });
    chosen.to_owned()
}

/// The preview that applies now: the locked one while the screen is locked.
fn preview(values: &Values, locked: bool) -> Preview {
    Preview::of(values.word(GLOBAL, if locked { settings::PREVIEW_LOCKED } else { settings::PREVIEW }))
}

/// Whether one live message notifies, in the order of the settings doc's silence rules.
pub fn decide(values: &Values, conversation: &Conversation, message: &Message, moment: &Moment) -> Option<Verdict> {
    let key = &conversation.key;
    // Mine, from any device; catch-up after a connect; the conversation being read.
    if message.outgoing || message.deleted || moment.now - message.sent > STALE {
        return None;
    }
    if moment.watching.focused && moment.watching.conversation.as_ref() == Some(key) {
        return None;
    }
    if !values.on(settings::NOTIFY) || conversation.muted || conversation.hidden || conversation.special == Special::Request {
        return None;
    }
    let mention = message.mentions_me;
    let level = level(values, key);
    // Bots and the archive raise nothing but mentions anywhere (`Conversation::badge`).
    let quiet_row = conversation.bot || conversation.archived;
    let wanted = match level {
        "off" => false,
        "all" => mention || !quiet_row,
        // Direct messages and mentions; a conversation set to mentions only means it.
        _ if values.stored(&settings::conversation_scope(key), settings::NOTIFY_ABOUT).is_some() => mention,
        _ => mention || (conversation.kind.is_direct() && !quiet_row),
    };
    let kind_on = match conversation.kind {
        ConversationKind::Direct => values.on(settings::NOTIFY_DIRECT),
        ConversationKind::Group => values.on(settings::NOTIFY_GROUPS),
        _ if mention => values.on(settings::NOTIFY_MENTIONS),
        _ => true,
    };
    let tiny_on = key.network != Network::Tiny || conversation.kind.is_direct() || values.on(settings::NOTIFY_TINY);
    if !wanted || !kind_on || !tiny_on {
        return None;
    }
    if hushed(values, moment) || moment.dnd {
        return None;
    }
    let in_call = values.word(GLOBAL, settings::IN_CALL);
    if moment.in_call && in_call == "nothing" {
        return None;
    }
    let silent = !values.on(settings::SOUNDS)
        || (moment.in_call && in_call != "sounds")
        // The window has focus on another conversation: a banner, no sound (design 06 §6).
        || moment.watching.focused
        || moment.sounded_here
        || moment.sounded_any;
    let sound = Some(sound_word(values, key, mention)).filter(|word| !silent && word != "none");
    Some(Verdict { banner: true, sound, preview: preview(values, moment.locked), mention })
}

/// Whether a ring posts (N5, N9, N3) and whether it is heard. `None`: nothing at all.
pub fn decide_ring(values: &Values, conversation: &Key, moment: &Moment) -> Option<Verdict> {
    let calls = values.word(GLOBAL, settings::CALLS);
    if calls == "off" || level(values, conversation) == "off" || moment.dnd {
        return None;
    }
    if conversation.network == Network::Tiny && !values.on(settings::NOTIFY_TINY) {
        return None;
    }
    if hushed(values, moment) && !values.on(settings::CALLS_THROUGH) {
        return None;
    }
    let ring = values.word(GLOBAL, settings::RING_SOUND);
    let sound = (calls == "ring" && values.on(settings::SOUNDS) && ring != "none").then(|| ring.to_owned());
    Some(Verdict { banner: !moment.watching.focused, sound, preview: preview(values, moment.locked), mention: false })
}

/// The notification's id for a conversation, stable across messages.
pub fn note_id(conversation: &Key) -> String {
    format!("m:{}:{}:{}", conversation.network.label(), conversation.account, conversation.id)
}

fn ring_id(conversation: &Key) -> String {
    format!("r:{}:{}:{}", conversation.network.label(), conversation.account, conversation.id)
}

/// Where a message was said, for the subtitle: a group's name, `#channel · Space`.
fn place(conversation: &Conversation) -> String {
    match conversation.kind {
        ConversationKind::Direct => String::new(),
        ConversationKind::Group => conversation.title.clone(),
        ConversationKind::Thread => format!("Thread in {}", conversation.title),
        _ if conversation.space_title.is_empty() => format!("#{}", conversation.title.trim_start_matches('#')),
        _ => format!("#{} · {}", conversation.title.trim_start_matches('#'), conversation.space_title),
    }
}

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// One conversation's notification, as last posted.
struct Group {
    /// Newest last: sender and text.
    lines: VecDeque<(String, String)>,
    mention: bool,
    sounded: Option<Instant>,
}

/// Every notification on screen, and what they were for.
pub struct Notes {
    sink: Box<dyn Sink>,
    groups: HashMap<Key, Group>,
    /// Note ids back to their conversations, for what the user does with one.
    ids: HashMap<String, Key>,
    ringing: Option<Key>,
    sounded: Option<Instant>,
}

impl Notes {
    pub fn new(sink: Box<dyn Sink>) -> Notes {
        Notes { sink, groups: HashMap::new(), ids: HashMap::new(), ringing: None, sounded: None }
    }

    /// The conversation a note is about.
    pub fn conversation(&self, id: &str) -> Option<&Key> {
        self.ids.get(id)
    }

    /// The parts of `Moment` this keeps: whether a sound was heard lately.
    pub fn quiet(&self, conversation: &Key, at: Instant) -> (bool, bool) {
        let here = self.groups.get(conversation).and_then(|group| group.sounded).is_some_and(|then| at.duration_since(then) < REPLACED_QUIET);
        let any = self.sounded.is_some_and(|then| at.duration_since(then) < ANY_QUIET);
        (here, any)
    }

    /// A message that notifies: posts or replaces its conversation's notification. Answers the
    /// sound to play, if any.
    pub fn message(&mut self, conversation: &Conversation, message: &Message, verdict: &Verdict, at: Instant) -> Option<String> {
        let key = &conversation.key;
        let group = self.groups.entry(key.clone()).or_insert_with(|| Group { lines: VecDeque::new(), mention: false, sounded: None });
        let text = crate::store::preview_text(message);
        group.lines.push_back((message.sender.name.clone(), text));
        while group.lines.len() > LINES {
            group.lines.pop_front();
        }
        group.mention |= verdict.mention;
        let newest = message.sender.name.clone();
        let several = group.lines.iter().any(|(sender, _)| *sender != newest);
        let body = group
            .lines
            .iter()
            .map(|(sender, text)| if several { format!("{sender}: {text}") } else { text.clone() })
            .collect::<Vec<_>>()
            .join("\n");
        let what = if group.mention { "New mention" } else { "New message" };
        let (title, subtitle, body) = match verdict.preview {
            Preview::Full => (newest.clone(), place(conversation), body),
            Preview::Sender => (newest.clone(), place(conversation), what.to_owned()),
            Preview::Nothing => (key.network.label().to_owned(), String::new(), what.to_owned()),
        };
        let id = note_id(key);
        let can_reply = !conversation.read_only;
        let note = Note {
            id: id.clone(),
            title,
            subtitle,
            body,
            kind: if can_reply { Kind::Message } else { Kind::ReadOnly },
            reply_hint: if can_reply {
                format!("Reply to {}", if conversation.kind.is_direct() { first_name(&newest) } else { &conversation.title })
            } else {
                String::new()
            },
            sound: verdict.sound.as_deref().filter(|word| system_plays(word)).and_then(sound_file),
            locked_body: what.to_owned(),
        };
        if let Err(err) = self.sink.post(&note) {
            tracing::warn!("chatd: notification not shown: {err}");
            return None;
        }
        self.ids.insert(id, key.clone());
        let sound = verdict.sound.clone()?;
        group.sounded = Some(at);
        self.sounded = Some(at);
        (!system_plays(&sound)).then_some(sound)
    }

    /// The conversation was read up to its newest message, here or on another device.
    pub fn read(&mut self, conversation: &Key) {
        if self.groups.remove(conversation).is_some() {
            let id = note_id(conversation);
            self.sink.withdraw(&id);
            self.ids.remove(&id);
        }
    }

    pub fn has(&self, conversation: &Key) -> bool {
        self.groups.contains_key(conversation)
    }

    /// The call as it is now. A ring posts its notification; anything else withdraws it.
    /// Answers whether the ring sound should start (`Some(word)`) or stop (`Some("")`).
    /// `verdict` is [`decide_ring`]'s for the ringing conversation, and `place` and `network`
    /// name it ("#general · Northwind", "Discord").
    pub fn call(&mut self, call: Option<&Call>, verdict: Option<Verdict>, place: &str, network: &str) -> Option<String> {
        let ringing = call.and_then(|call| match &call.state {
            CallState::RingingIn { from } => Some((call.conversation.clone(), from.clone())),
            _ => None,
        });
        match (ringing, self.ringing.clone()) {
            (Some((key, _)), Some(shown)) if key == shown => None,
            (Some((key, from)), _) => {
                self.stop_ring();
                let verdict = verdict?;
                self.ringing = Some(key.clone());
                // A ring nobody is told about still has to be heard: the card is all the
                // window shows, so chatd plays every sound the system will not.
                if !verdict.banner {
                    return verdict.sound;
                }
                let caller = if from.is_empty() { place.to_owned() } else { from };
                let (title, subtitle) = match verdict.preview {
                    Preview::Nothing => (network.to_owned(), String::new()),
                    _ => (caller, if place.is_empty() { format!("{network} · Incoming call") } else { format!("{place} · Incoming call") }),
                };
                let id = ring_id(&key);
                let note = Note {
                    id: id.clone(),
                    title,
                    subtitle,
                    body: "Incoming call".to_owned(),
                    kind: Kind::Call,
                    reply_hint: String::new(),
                    sound: verdict.sound.as_deref().filter(|word| system_plays(word)).and_then(sound_file),
                    locked_body: "Incoming call".to_owned(),
                };
                if let Err(err) = self.sink.post(&note) {
                    tracing::warn!("chatd: ring not shown: {err}");
                    // No banner means no system sound; play it here instead.
                    return verdict.sound;
                }
                self.ids.insert(id, key);
                verdict.sound.filter(|word| !system_plays(word))
            }
            (None, Some(_)) => {
                self.stop_ring();
                Some(String::new())
            }
            (None, None) => None,
        }
    }

    fn stop_ring(&mut self) {
        if let Some(key) = self.ringing.take() {
            let id = ring_id(&key);
            self.sink.withdraw(&id);
            self.ids.remove(&id);
        }
    }

    /// N13: one real notification with the chosen preview and sound. Answers whether the system
    /// lists it, and why not.
    pub fn test(&mut self, values: &Values, locked: bool) -> (bool, String, Option<String>) {
        let what = "This is how messages will look and sound.";
        let (title, body) = match preview(values, locked) {
            Preview::Full => ("tinyclient".to_owned(), what.to_owned()),
            Preview::Sender => ("tinyclient".to_owned(), "New message".to_owned()),
            Preview::Nothing => ("tinyclient".to_owned(), "New message".to_owned()),
        };
        let word = values.word(GLOBAL, settings::MESSAGE_SOUND);
        let sound = (values.on(settings::SOUNDS) && word != "none").then(|| word.to_owned());
        let note = Note {
            id: "test".to_owned(),
            title,
            subtitle: String::new(),
            body,
            kind: Kind::Plain,
            reply_hint: String::new(),
            sound: sound.as_deref().filter(|word| system_plays(word)).and_then(sound_file),
            locked_body: "New message".to_owned(),
        };
        match self.sink.post(&note) {
            Ok(()) => {
                let listed = self.sink.listed("test");
                let detail = if listed { String::new() } else { "The system took it but does not list it: check Focus or Do Not Disturb.".to_owned() };
                (listed, detail, sound.filter(|word| !system_plays(word)))
            }
            Err(err) => (false, err, None),
        }
    }
}

/// The bundled file for a sound word, for systems that play it themselves.
fn sound_file(word: &str) -> Option<String> {
    let stem = match word {
        "pebble" | "drop" | "tap" | "chime" | "glass" => word.to_owned(),
        "soft_ring" => "soft-ring".to_owned(),
        "classic_ring" => "classic-ring".to_owned(),
        _ => return None,
    };
    Some(format!("tinyclient-{stem}"))
}

/// Whether the notification itself makes this sound. False means chatd must: a custom file,
/// a sound added after the bundled seven, or a platform whose notifications are silent.
fn system_plays(word: &str) -> bool {
    notifier::SYSTEM_PLAYS_SOUND && sound_file(word).is_some()
}

/// Minutes after local midnight at `unix`.
pub fn local_minute(unix: i64) -> u32 {
    // SAFETY: the platform's localtime fills the struct handed to it.
    unsafe {
        let mut local: libc::tm = std::mem::zeroed();
        let unix = unix as libc::time_t;
        #[cfg(unix)]
        libc::localtime_r(&unix, &mut local);
        #[cfg(windows)]
        if libc::localtime_s(&mut local, &unix) != 0 {
            return 0;
        }
        (local.tm_hour.clamp(0, 23) * 60 + local.tm_min.clamp(0, 59)) as u32
    }
}

/// What the hub tells the notification thread.
pub enum Event {
    /// A new live message from someone else, stored.
    Message { conversation: Key, message: Box<Message> },
    /// A conversation's row changed: it may have been read.
    Row(Key),
    Call(Option<Call>),
    Activated(Activation),
    Test(Sender<(bool, String)>),
    /// S11: a sound, played once whatever S1 says.
    Play(String),
}

/// Who the notification thread is, to the hub.
pub type Line = Mutex<Sender<Event>>;

/// The ID Windows shows notifications under. Debug builds use their own, so running one never
/// changes how an installed tinyclient's notifications look.
pub fn app() -> notifier::App {
    let release = !cfg!(debug_assertions);
    let id = if release { "dev.tinyclient.app" } else { "dev.tinyclient.app.debug" };
    let name = if release { "tinyclient" } else { "tinyclient (dev)" };
    let exe = std::env::current_exe().ok();
    let icon = exe.as_ref().and_then(|exe| exe.parent()).map(|dir| dir.join("tinyclient.ico")).filter(|icon| icon.is_file()).or_else(|| {
        // Under cargo the icon is in the repository, three levels over `target/<profile>`.
        exe.as_ref()?.ancestors().nth(3).map(|root| root.join("packaging/windows/tinyclient.ico")).filter(|icon| icon.is_file())
    });
    notifier::App { id: id.to_owned(), name: name.to_owned(), icon }
}

/// `TINYCLIENT_NOTIFY=on` turns notifications on in a scratch directory (`TINYCLIENT_DIR`),
/// where they are otherwise off: the tests run real chatds there and must post and play nothing.
pub const NOTIFY_ENV: &str = "TINYCLIENT_NOTIFY";

/// Whether this chatd posts notifications at all.
pub fn enabled() -> bool {
    crate::session::enabled() || std::env::var(NOTIFY_ENV).is_ok_and(|on| on == "on")
}

/// Why the test notification could not be sent when [`enabled`] is false.
pub const DISABLED: &str = "Notifications are off for a scratch directory. TINYCLIENT_NOTIFY=on turns them on.";

/// Starts the notification thread; the hub sends it [`Event`]s from then on.
pub fn start(hub: Arc<Hub>, chat: Weak<Chat>) {
    if !enabled() {
        return;
    }
    let (tell, told) = channel::<Event>();
    let app = app();
    // A build that was never installed makes its ID known; an installed one is the installer's.
    if let Err(err) = app.register(cfg!(debug_assertions)) {
        tracing::warn!("chatd: {err}");
    }
    let back = Mutex::new(tell.clone());
    let activated: notifier::Tell = Arc::new(move |done| {
        let _ = lock(&back).send(Event::Activated(done));
    });
    hub.set_notifier(tell);
    let spawned = std::thread::Builder::new().name("notify".into()).spawn(move || {
        let sink = notifier::system(&app, activated);
        run(&hub, &chat, Notes::new(sink), &told);
    });
    if let Err(err) = spawned {
        tracing::warn!("chatd: notifications: {err}");
    }
}

/// The account's own status is Do Not Disturb.
fn dnd(hub: &Hub, conversation: &Key) -> bool {
    let me = hub.store().accounts().ok().and_then(|rows| rows.into_iter().find(|row| row.key == conversation.account()).map(|row| row.me));
    me.filter(|me| !me.is_empty()).is_some_and(|me| hub.presence(&Key::new(&conversation.account(), &me)) == protocol::Presence::Dnd)
}

/// What is true now. Do Not Disturb is left out: it reads every account, so it is asked only of a
/// message that would otherwise notify.
fn moment(hub: &Hub, conversation: &Key, notes: &Notes, at: Instant) -> Moment {
    let now = now();
    let (sounded_here, sounded_any) = notes.quiet(conversation, at);
    let in_call = hub.calls.describe().0.is_some_and(|call| matches!(call.state, CallState::Joined | CallState::Connecting | CallState::Reconnecting));
    Moment {
        now,
        minute: local_minute(now),
        dnd: false,
        in_call,
        watching: (*hub.watching()).clone(),
        locked: notifier::screen_locked(),
        sounded_here,
        sounded_any,
    }
}

/// One sound through mediad. A ring is kept so it can be stopped.
fn play(word: &str, volume: u32, repeat: Option<u64>, limit: u64) -> Option<MediaChild> {
    let mediad = crate::media::mediad_path().ok()?;
    let mut command = Command::new(mediad);
    command.arg("--sound").arg(word).arg(volume.min(100).to_string()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(gap) = repeat {
        command.arg("--repeat").arg(gap.to_string()).arg("--for").arg(limit.to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up for it.
        command.creation_flags(0x0800_0000);
    }
    // In process (phones) there is no stdio to discard: its pipes are dropped at once, and a
    // sound says nothing on them.
    if protocol::media_host::in_process() {
        return protocol::media_host::spawn(&mut command).map(|spawned| spawned.child).map_err(|err| tracing::warn!("chatd: sound: {err}")).ok();
    }
    command.spawn().map(MediaChild::from).map_err(|err| tracing::warn!("chatd: sound: {err}")).ok()
}

/// How long an incoming ring sounds before it gives up by itself: past the network's own
/// 60 s no-answer deadline, so the two never disagree.
const RING_FOR: u64 = 65;
/// The silence between one ring and the next.
const RING_GAP: u64 = 1_500;
/// A ring back runs under the same deadline, with a longer breath between rings.
const RINGBACK_FOR: u64 = 62;
const RINGBACK_GAP: u64 = 2_000;

/// The word a ring back plays, if it should play at all (S14, S1 and S4).
fn ringback_word(values: &Values) -> Option<String> {
    let word = values.word(GLOBAL, settings::RING_SOUND);
    (values.on(settings::SOUNDS) && values.ringback() && word != "none").then(|| word.to_owned())
}

fn stop(child: &mut Option<MediaChild>) {
    if let Some(mut child) = child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Stops the ring back, if one is playing.
fn stop_back(back: &mut Option<(String, MediaChild)>) {
    if let Some((_, mut child)) = back.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn run(hub: &Arc<Hub>, chat: &Weak<Chat>, mut notes: Notes, told: &Receiver<Event>) {
    let mut ring: Option<MediaChild> = None;
    // The word a running ring back plays, so a change to S4 replaces it rather than layering
    // a second tone over the first.
    let mut back: Option<(String, MediaChild)> = None;
    let mut playing: Vec<MediaChild> = Vec::new();
    while let Ok(event) = told.recv() {
        playing.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        // Rows change many times a second; most need no settings at all.
        let busy = match &event {
            Event::Row(conversation) => !notes.has(conversation),
            _ => false,
        };
        if busy {
            continue;
        }
        let values = hub.settings().clone();
        let volume = values.number(settings::ALERT_VOLUME);
        match event {
            Event::Message { conversation, message } => {
                let Ok(Some(row)) = hub.store().conversation(&conversation) else { continue };
                let at = Instant::now();
                let mut moment = moment(hub, &conversation, &notes, at);
                if decide(&values, &row, &message, &moment).is_none() {
                    continue;
                }
                moment.dnd = dnd(hub, &conversation);
                let Some(verdict) = decide(&values, &row, &message, &moment) else { continue };
                if let Some(word) = notes.message(&row, &message, &verdict, at) {
                    playing.extend(play(&word, volume, None, 0));
                }
            }
            Event::Row(conversation) => {
                if !notes.has(&conversation) {
                    continue;
                }
                let read = hub.store().conversation(&conversation).ok().flatten().is_none_or(|row| row.unread == 0 && row.mentions == 0);
                if read {
                    notes.read(&conversation);
                }
            }
            Event::Call(call) => {
                let ringing = call.as_ref().filter(|call| matches!(call.state, CallState::RingingIn { .. })).map(|call| call.conversation.clone());
                let (verdict, place) = match &ringing {
                    Some(key) => {
                        let row = hub.store().conversation(key).ok().flatten();
                        let place = row.filter(|row| row.kind != ConversationKind::Direct).map(|row| place(&row)).unwrap_or_default();
                        let moment = Moment { dnd: dnd(hub, key), ..moment(hub, key, &notes, Instant::now()) };
                        (decide_ring(&values, key, &moment), place)
                    }
                    None => (None, String::new()),
                };
                // A ring back is mine, not a notification: it plays while I am calling
                // someone, on every network, and stops the instant they answer or I cancel.
                let calling = call.as_ref().is_some_and(|call| call.state == CallState::RingingOut);
                match (calling, ringback_word(&values)) {
                    (true, Some(word)) => {
                        let already = back.as_ref().is_some_and(|(playing, _)| *playing == word);
                        if !already {
                            stop_back(&mut back);
                            if let Some(child) = play(&word, volume, Some(RINGBACK_GAP), RINGBACK_FOR) {
                                tracing::info!("chatd: ringing back with {word}");
                                back = Some((word, child));
                            }
                        }
                    }
                    _ => stop_back(&mut back),
                }
                let network = ringing.as_ref().map_or("", |key| key.network.label());
                match notes.call(call.as_ref(), verdict, &place, network).as_deref() {
                    Some("") => stop(&mut ring),
                    Some(word) => {
                        stop(&mut ring);
                        ring = play(word, volume, Some(RING_GAP), RING_FOR);
                        if ring.is_some() {
                            tracing::info!("chatd: ringing with {word}");
                        }
                    }
                    None => {}
                }
            }
            Event::Activated(done) => act(hub, chat, &notes, done),
            Event::Test(answer) => {
                let (posted, detail, sound) = notes.test(&values, notifier::screen_locked());
                if let Some(word) = sound {
                    playing.extend(play(&word, volume, None, 0));
                }
                let _ = answer.send((posted, detail));
            }
            Event::Play(word) => playing.extend(play(&word, volume, None, 0)),
        }
    }
}

/// Brings the window forward on `conversation`: tells a shell that is up, or starts one.
fn open(hub: &Hub, conversation: &Key) {
    notifier::allow_foreground();
    hub.changed(protocol::Change::Open { conversation: conversation.clone() });
    if hub.shell_asked().elapsed() < SHELL_ALIVE {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let shell = exe.with_file_name(format!("tinyclient{}", std::env::consts::EXE_SUFFIX));
    let Ok(key) = serde_json::to_string(conversation) else { return };
    if let Err(err) = Command::new(&shell).arg(format!("--open={key}")).spawn() {
        tracing::warn!("chatd: could not start {}: {err}", shell.display());
    }
}

/// Carries out what the user did with a notification, as the window would ask it.
fn act(hub: &Hub, chat: &Weak<Chat>, notes: &Notes, done: Activation) {
    let Some(chat) = chat.upgrade() else { return };
    let id = match &done {
        Activation::Open(id) | Activation::MarkRead(id) | Activation::Accept(id) | Activation::Decline(id) => id,
        Activation::Reply { id, .. } => id,
    };
    // The test notification is about nothing.
    let Some(conversation) = notes.conversation(id).cloned() else { return };
    let read = |chat: &Chat| {
        let newest = chat.handle(Request::Messages { conversation: conversation.clone(), page: protocol::Page::Newest, limit: 1 });
        if let Response::Messages { messages, .. } = newest {
            if let Some(last) = messages.iter().rev().find(|message| message.state == protocol::SendState::Sent) {
                let _ = chat.handle(Request::MarkRead { conversation: conversation.clone(), message_id: last.id.clone() });
            }
        }
    };
    match done {
        Activation::Open(_) => open(hub, &conversation),
        Activation::MarkRead(_) => read(&chat),
        Activation::Reply { text, .. } => {
            if let Response::Conversation { conversation: row } = chat.handle(Request::Conversation { conversation: conversation.clone() }) {
                if row.read_only {
                    tracing::warn!("chatd: reply from a notification not sent: you can read {} but not post in it", row.title);
                    open(hub, &conversation);
                    return;
                }
            }
            let client_id = format!("notify-{}-{}", now(), std::process::id());
            let sent = chat.handle(Request::Send { conversation: conversation.clone(), client_id, text, reply_to: None, with: Default::default() });
            if let Response::Error { detail, .. } = sent {
                tracing::warn!("chatd: reply from a notification not sent: {detail}");
                open(hub, &conversation);
            } else {
                read(&chat);
            }
        }
        Activation::Accept(_) => {
            let _ = chat.handle(Request::CallAccept);
            open(hub, &conversation);
        }
        Activation::Decline(_) => {
            let _ = chat.handle(Request::CallDecline);
        }
    }
}

#[cfg(test)]
mod tests;
