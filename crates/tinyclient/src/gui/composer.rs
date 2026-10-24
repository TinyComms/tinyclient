//! The composer: the draft, sending it, editing and replying, the typing row above it, and what
//! replaces it when nothing can be sent. One `Composer` per place a message is written: the
//! conversation, and the thread pane.
//!
//! The card holds only what the account can do (`controls`): attach, the field, GIF, emoji and
//! send. Files come in by ⌘U, by pasting a screenshot or Finder files, or by a drop anywhere on
//! the conversation, and wait as chips (`attach`, `tray`) until Return; pasted pixels become a
//! file in `outbox/`. The one picker (`picker`) is shared with reactions.
//!
//! For the other views, all on `Shell`:
//! - `composer(open, link, cx)` is the whole dock: the 20 pt typing row, the card, and the line
//!   under it. Nothing else needs to reserve the typing slot. `thread_composer(thread, title,
//!   link, cx)` is the same for the thread pane; it sends to the thread's key.
//! - `composer_edit(message, cx)` and `composer_reply(message, cx)` put the open conversation's
//!   composer in edit or reply mode; `composer_editing(cx)` is the id of the message being
//!   edited, for tinting its row.
//! - `composer_type(text, window, cx)` and `composer_focus(window, cx)`: keys pressed with no
//!   field focused go to the composer.
//! - `composer_upload(conversation, client_id)` is how far a pending message's files have gone
//!   (`Change::Upload`), and `composer_cancel_upload(conversation, client_id, cx)` stops them.
//! - `unsent_line(conversation, message, cx)` is what a failed row shows under its body: the
//!   reason, Retry and Delete. `None` for a message that went.
//! - `held_sends(conversation)` are the messages sent while the account was offline, which
//!   chatd does not store: draw each as a pending row with `held_line(conversation, held, cx)`
//!   under its body. They wait 60 s for the link (`hold`), then fail in place.
//! - `send_failed_in(conversation)`: a message sent from here did not go and is unresolved.
//! - `composer_links_changed(cx)` and `composer_changed(change, cx)` are called by the shell.

mod attach;
mod colon;
mod complete;
mod controls;
pub(super) mod edit;
mod hold;
mod mention;
mod niceties;
// The picker is the composer's and the reactions'; it lives in `gui/picker`.
#[path = "picker/mod.rs"]
pub(in crate::gui) mod picker;
mod tray;
mod typing;

pub use typing::preview as typing_preview;

use super::components::place::{max_height, placed, Layer, Place};
use super::components::{button, glyph, menu, menu_item, tip, ButtonKind};
use super::input::{InputEvent, InputKey, InputList, InputPaste, PickEmoji, PickGifs, SubmitSilently, TextInput};
use super::people::{ask_later, Hub};
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use crate::view::now;
use attach::{Found, Staged};
use complete::List;
use controls::{controls, Controls};
use gpui::{
    canvas, deferred, div, prelude::*, px, relative, App, Bounds, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable,
    PathPromptOptions, Pixels, Point, Rgba, Subscription, Task, WeakEntity, Window,
};
use hold::{Flight, Held, Hold, Outcome};
use mention::Token;
use picker::emoji::Custom;
use picker::images::Images;
use picker::{Picker, PickerEvent, Setup, Tab};
use protocol::{
    AccountKey, Capabilities, CatalogKind, Change, Conversation, ConversationKind, EmojiScope, ErrorCode, Key, Link, Message, MessageKind,
    Network, Page, Person, Pick, Request, Response, SendState, DRAFT_LIMIT,
};
use std::cell::{Cell, OnceCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use typing::TypingRow;

/// Drafts remembered by this window, one per conversation visited. chatd keeps every draft;
/// these are the ones newer than the rows the lists hold.
const DRAFTS: usize = 64;
/// The card and the bars that replace it are the same height, so the layout never moves.
const CARD: f32 = 40.;
/// A draft goes to chatd once the keys have been still this long.
const DRAFT_QUIET: Duration = Duration::from_millis(500);
/// A completion is looked up once the keys have been still this long.
const LOOKUP_QUIET: Duration = Duration::from_millis(150);
/// Messages read to find my last one, and to learn what became of a send.
const LAST_MINE: u32 = 50;
const SETTLE: u32 = 20;
/// Upload progress kept for pending rows, most recent last.
const UPLOADS: usize = 32;
const NOT_ANSWERING: &str = "tinyclient's background process is not answering.";

/// What the composer keeps between frames: the composers themselves, made when first drawn,
/// and the sends that are not chatd's yet or not settled.
#[derive(Default)]
pub struct State {
    main: OnceCell<Entity<Composer>>,
    thread: OnceCell<Entity<Composer>>,
    hold: Hold,
    flight: Flight,
    /// The one timer of the hold: it exists only while a message waits, and ends at the
    /// moment the oldest runs out of time.
    expiry: Option<Task<()>>,
    /// Files the shell made for sends on their way (pastes, JPEGs), deleted once each send
    /// has gone or was discarded.
    outbox: Vec<(Key, String, Vec<PathBuf>)>,
    /// How far each pending message's files have gone, in bytes: (sent, total).
    uploads: VecDeque<((Key, String), (u64, u64))>,
}

/// A message sent while its account was offline, for the conversation to draw as a row.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldSend {
    pub client_id: String,
    pub text: String,
    /// Unix seconds of the Return.
    pub since: i64,
    pub failed: bool,
}

/// A build that takes pictures and runs driven tests never changes anything on a network or
/// in the store: it draws, and that is all.
fn live() -> bool {
    !cfg!(feature = "gui-shot")
}

/// `--composer <state>`: the state a picture is taken of.
#[cfg(feature = "gui-shot")]
fn staged(what: &str) -> bool {
    std::env::args().skip_while(|arg| arg != "--composer").nth(1).is_some_and(|state| state == what)
}

/// A name for a message that has not been sent yet: digits only and never repeated, because
/// networks take it as their nonce.
fn client_id() -> String {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let micros = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64);
    let next = |last: u64| micros.max(last + 1);
    let last = LAST.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| Some(next(last)));
    next(last.unwrap_or(0)).to_string()
}

thread_local! {
    /// My messages sent without notification while this shell runs, by client id.
    static SILENT: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
}

/// Whether my message went without notification, for the bell-off after its time.
// CONTRACT: `Message` carries no silent flag, so the mark is the shell's own: it shows on the
// pending row and on the echo while this shell runs, and is gone after a restart.
pub(in crate::gui) fn sent_silently(client_id: &str) -> bool {
    SILENT.with_borrow(|silent| silent.contains(client_id))
}

/// Whether others see that we are typing: Settings › Privacy (P1), per account. chatd checks
/// again; asking here saves a request per keystroke burst.
fn sends_typing(account: &protocol::AccountKey) -> bool {
    super::app::prefs::values().typing(account)
}

/// How often a network needs to hear that we are still typing for its line to stay up.
fn typing_interval(network: Network) -> Duration {
    Duration::from_secs(match network {
        Network::Discord | Network::Signal | Network::Messages | Network::Tiny => 8,
        Network::Teams => 6,
        Network::Slack | Network::Telegram => 4,
    })
}

fn placeholder(title: &str, thread: bool) -> String {
    if thread {
        "Reply in thread".to_owned()
    } else {
        format!("Message {title}")
    }
}

/// Slack has no reply outside a thread, so its composer has no reply mode.
fn replies_inline(network: Network) -> bool {
    network != Network::Slack
}

/// A message of mine that the network holds: the only kind that can be edited.
fn editable(message: &Message) -> bool {
    message.outgoing && !message.deleted && message.kind == MessageKind::Normal && message.state == SendState::Sent
}

/// One line of a message for the reply bar.
fn snippet(message: &Message) -> String {
    let mut line = message.text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim().to_owned();
    if line.is_empty() && !message.attachments.is_empty() {
        line = "Attachment".to_owned();
    }
    protocol::clip(&mut line, 160);
    line
}

/// The draft as chatd keeps it: nothing for only spaces, and no longer than it stores.
fn draft_text(field: &str) -> String {
    if field.trim().is_empty() {
        return String::new();
    }
    let mut text = field.to_owned();
    protocol::clip(&mut text, DRAFT_LIMIT);
    text
}

/// Deletes files the shell made, off the main thread.
fn delete_later(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("outbox".into()).stack_size(64 * 1024).spawn(move || attach::delete_made(&attach::outbox(), &paths));
}

/// Where the composer writes to.
#[derive(Clone, PartialEq)]
struct Target {
    key: Key,
    title: String,
    kind: ConversationKind,
    link: Link,
    /// The Space it is in, for `#` completion and the Space's own emoji. A thread pane is not told.
    space: Option<String>,
    read_only: bool,
    /// What the account may send.
    can: Capabilities,
}

/// Why nothing can be typed here, when that is so.
enum Locked {
    SignedOut,
    Expired,
    ReadOnly(String),
}

impl Target {
    fn locked(&self) -> Option<Locked> {
        match (self.link, self.kind) {
            (Link::SignedOut, _) => Some(Locked::SignedOut),
            (Link::Expired, _) => Some(Locked::Expired),
            (_, ConversationKind::Forum) => Some(Locked::ReadOnly("Posts in a forum are started in the network's app.".to_owned())),
            _ if self.read_only => Some(Locked::ReadOnly(format!("You can read {} but not post in it.", self.title))),
            _ => None,
        }
    }

    fn space_key(&self) -> Option<Key> {
        self.space.as_ref().map(|space| Key { id: space.clone(), ..self.key.clone() })
    }
}

/// What was typed, the mentions in it, and the files staged with it.
#[derive(Clone, Default)]
struct Draft {
    text: String,
    tokens: Vec<Token>,
    files: Vec<Staged>,
}

struct Reply {
    id: String,
    name: String,
    snippet: String,
}

struct Editing {
    id: String,
    /// The message as it was put in the field: saving this unchanged is a cancel.
    original: String,
    /// What was in the field before, and the reply it was for. Back on save or cancel.
    stash: Draft,
    reply: Option<Reply>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Offline,
    Refused,
    Note,
    /// The large-ping confirm.
    Warning,
}

/// A send on its way to chatd, as it was when Return was pressed.
struct Outgoing {
    key: Key,
    /// The field then, to tell whether more was typed since.
    field: String,
    shown: String,
    wire: String,
    client_id: String,
    reply_to: Option<String>,
    /// The chips that went, and the files the shell made for them.
    chips: Vec<u64>,
    made: Vec<PathBuf>,
}

/// A drag of files over the window, while it is over this composer's conversation.
#[derive(Clone, PartialEq)]
struct DragOver {
    paths: Vec<PathBuf>,
    folders: bool,
    inside: bool,
}

struct Picking {
    picker: Entity<Picker>,
    _listening: Subscription,
}

/// The buttons the picker opens from.
#[derive(Clone, Copy)]
enum Trigger {
    Gif,
    Smile,
}

/// Bounds drawn last frame, for placing what opens from them.
#[derive(Default)]
struct Anchors {
    dock: Cell<Option<Bounds<Pixels>>>,
    gif: Cell<Option<Bounds<Pixels>>>,
    smile: Cell<Option<Bounds<Pixels>>>,
    send: Cell<Option<Bounds<Pixels>>>,
}

pub struct Composer {
    input: Entity<TextInput>,
    typing: Entity<TypingRow>,
    shell: WeakEntity<Shell>,
    hub: Hub,
    thread: bool,
    target: Option<Target>,
    drafts: HashMap<Key, Draft>,
    draft_order: VecDeque<Key>,
    /// The draft chatd holds for this conversation, as far as this window knows.
    saved: String,
    /// The draft the conversation's row last carried, to notice a row that arrives late.
    offered: String,
    /// Something was typed here since the conversation opened.
    touched: bool,
    /// One timer, alive only between a keystroke and the save 500 ms after the last one.
    draft_timer: Option<Task<()>>,
    /// The mentions in the field, and the text they were last placed against.
    tokens: Vec<Token>,
    seen: String,
    reply: Option<Reply>,
    editing: Option<Editing>,
    /// One line under the card, until the text changes: why something did not happen.
    caption: Option<(String, Tone)>,
    sending: bool,
    /// The conversation the network was last told we are typing in, and when.
    told_typing: Option<(Key, Instant)>,
    list: Option<List>,
    /// The sigil whose list was closed by Escape or by a space with no match.
    dismissed: Option<usize>,
    /// One timer and one read, alive only while a completion is being looked up.
    lookup: Option<Task<()>>,
    focus_next: bool,
    blur_next: bool,
    // Files.
    tray: Vec<Staged>,
    next_chip: u64,
    chip_menu: Option<u64>,
    thumbs: Images,
    drag: Option<DragOver>,
    // The picker and `:`.
    picking: Option<Picking>,
    colon: Option<colon::List>,
    colon_dismissed: Option<usize>,
    /// The account's custom emoji for `:`, asked once per account and Space.
    customs: Vec<Custom>,
    customs_for: Option<(AccountKey, Option<Key>)>,
    // Niceties.
    /// The link chip's ×: the network is asked not to unfurl this message's links.
    no_preview: bool,
    /// Accounts whose replies do not mention the author ("@ Off"), as the user last chose.
    ping_off: HashSet<AccountKey>,
    /// The large-ping confirm: the field it was armed for.
    armed: Option<String>,
    /// Member counts learned for the confirm: (members, online).
    counts: HashMap<Key, (Option<u32>, Option<u32>)>,
    send_menu: bool,
    anchors: Rc<Anchors>,
}

impl Composer {
    fn new(shell: WeakEntity<Shell>, hub: Hub, thread: bool, cx: &mut Context<Self>) -> Composer {
        let input = cx.new(|cx| TextInput::multiline(placeholder("", thread), cx));
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.send(false, cx),
            InputEvent::Changed => this.typed(cx),
        })
        .detach();
        cx.subscribe(&input, |this, _, _: &SubmitSilently, cx| this.send(true, cx)).detach();
        cx.subscribe(&input, |this, _, key: &InputKey, cx| match key {
            InputKey::Escape => this.escape(cx),
            // The field passes Up on only when it is empty.
            InputKey::Up => this.edit_last(cx),
            InputKey::Down => {}
        })
        .detach();
        cx.subscribe(&input, |this, _, key: &InputList, cx| match key {
            InputList::Up => this.step(false, cx),
            InputList::Down => this.step(true, cx),
            InputList::Complete => this.complete_selected(cx),
        })
        .detach();
        cx.subscribe(&input, |this, _, paste: &InputPaste, cx| match paste {
            InputPaste::Files(paths) => this.stage_paths(paths.clone(), cx),
            InputPaste::Image(image) => this.stage_pasted(image.clone(), cx),
        })
        .detach();
        if !thread {
            // What earlier runs left in the outbox, long settled.
            cx.background_executor()
                .spawn(async { attach::sweep(&attach::outbox(), attach::OUTBOX_KEEP, SystemTime::now()) })
                .detach();
        }
        Composer {
            input,
            typing: cx.new(|_| TypingRow::new(hub.clone())),
            shell,
            hub,
            thread,
            target: None,
            drafts: HashMap::new(),
            draft_order: VecDeque::new(),
            saved: String::new(),
            offered: String::new(),
            touched: false,
            draft_timer: None,
            tokens: Vec::new(),
            seen: String::new(),
            reply: None,
            editing: None,
            caption: None,
            sending: false,
            told_typing: None,
            list: None,
            dismissed: None,
            lookup: None,
            focus_next: false,
            blur_next: false,
            tray: Vec::new(),
            next_chip: 1,
            chip_menu: None,
            // Chips and the `:` list are small: each moving one holds little.
            thumbs: Images::with_limits(crate::frames::SMALL),
            drag: None,
            picking: None,
            colon: None,
            colon_dismissed: None,
            customs: Vec::new(),
            customs_for: None,
            no_preview: false,
            ping_off: HashSet::new(),
            armed: None,
            counts: HashMap::new(),
            send_menu: false,
            anchors: Rc::new(Anchors::default()),
        }
    }

    /// Tells the shell something once this update is over. The composer is often updated from
    /// inside the shell's own update or render, where the shell cannot be reached.
    fn shell_later(&self, cx: &mut Context<Self>, then: impl FnOnce(&mut Shell, &mut Context<Shell>) + 'static) {
        let shell = self.shell.clone();
        cx.defer(move |cx| {
            let _ = shell.update(cx, then);
        });
    }

    fn controls(&self) -> Controls {
        match &self.target {
            Some(target) => controls(target.key.network, &target.can, self.editing.is_some()),
            None => Controls::default(),
        }
    }

    /// Points the composer at a conversation. Another conversation swaps the draft: what was
    /// typed here is saved and waits for the next visit. `stored` is the draft on the
    /// conversation's row, which is what a conversation not visited before opens with.
    fn aim(&mut self, target: Target, stored: &str, cx: &mut Context<Self>) {
        if self.target.as_ref() == Some(&target) && self.offered == stored {
            return;
        }
        let moved = self.target.as_ref().map(|old| &old.key) != Some(&target.key);
        let renamed = self.target.as_ref().map(|old| &old.title) != Some(&target.title);
        if moved {
            self.leave(cx);
            let draft = self.drafts.get(&target.key).cloned().unwrap_or_else(|| Draft { text: stored.to_owned(), ..Draft::default() });
            self.saved = draft_text(&draft.text);
            self.set_field(&draft, cx);
            self.tray = draft.files;
            self.offered = stored.to_owned();
            self.touched = false;
            self.reply = None;
            self.caption = None;
            self.sending = false;
            self.no_preview = false;
            self.armed = None;
            self.focus_next = true;
            picker::emoji::set_account(Some(&target.key.account()));
        } else if self.offered != stored {
            // The row was read after the conversation opened: its draft is still wanted if
            // nothing has been typed in the meantime.
            let late = !self.touched && self.editing.is_none() && self.input.read(cx).text().is_empty() && !stored.is_empty();
            self.offered = stored.to_owned();
            if late {
                self.saved = stored.to_owned();
                self.set_field(&Draft { text: stored.to_owned(), ..Draft::default() }, cx);
            }
        }
        if moved || renamed {
            let hint = placeholder(&target.title, self.thread);
            self.input.update(cx, |input, cx| {
                input.set_placeholder(hint);
                cx.notify();
            });
        }
        let silent = controls(target.key.network, &target.can, false).silent;
        self.input.update(cx, |input, _| input.set_alt_submits(silent));
        self.target = Some(target);
        cx.notify();
    }

    /// Leaving a conversation: an edit ends, and the draft is saved and kept for the way back,
    /// its chips with it.
    fn leave(&mut self, cx: &mut Context<Self>) {
        self.end_edit(cx);
        self.save_draft(cx);
        self.draft_timer = None;
        self.close_picker(cx);
        self.chip_menu = None;
        let files = std::mem::take(&mut self.tray);
        let Some(old) = self.target.as_ref().map(|old| old.key.clone()) else {
            return;
        };
        let draft = Draft { text: draft_text(self.input.read(cx).text()), tokens: self.tokens.clone(), files };
        self.remember(old, draft);
    }

    fn remember(&mut self, key: Key, draft: Draft) {
        if self.drafts.insert(key.clone(), draft).is_none() {
            self.draft_order.push_back(key);
            if self.draft_order.len() > DRAFTS {
                if let Some(oldest) = self.draft_order.pop_front() {
                    // A draft dropped takes the files made for it.
                    if let Some(dropped) = self.drafts.remove(&oldest) {
                        delete_later(dropped.files.iter().flat_map(Staged::made).collect());
                    }
                }
            }
        }
    }

    /// Puts another text in the field. Not an edit: no draft is saved and nobody is told.
    fn set_field(&mut self, draft: &Draft, cx: &mut Context<Self>) {
        self.tokens = draft.tokens.clone();
        self.seen = draft.text.clone();
        let highlights = self.tokens.iter().map(|token| token.range.clone()).collect();
        self.input.update(cx, |input, cx| {
            input.set_text(&draft.text, cx);
            input.set_highlights(highlights, cx);
        });
        self.close_list(cx);
        self.dismissed = None;
        self.colon_dismissed = None;
    }

    /// Brings the tokens up to the text as it is now.
    fn follow_text(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_owned();
        if text != self.seen {
            mention::shift(&mut self.tokens, &self.seen, &text);
            self.seen = text;
        }
        let highlights = self.tokens.iter().map(|token| token.range.clone()).collect();
        self.input.update(cx, |input, cx| input.set_highlights(highlights, cx));
    }

    /// Stores the draft with chatd and tells the sidebar, when it differs from what is stored.
    /// Edit mode is not a draft: the text being edited is never saved as one.
    fn save_draft(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.target.as_ref().map(|target| target.key.clone()) else {
            return;
        };
        if self.editing.is_some() {
            return;
        }
        let text = draft_text(self.input.read(cx).text());
        self.store_draft(key, text, cx);
    }

    fn store_draft(&mut self, key: Key, text: String, cx: &mut Context<Self>) {
        let here = self.target.as_ref().is_some_and(|target| target.key == key);
        if (here && text == self.saved) || !live() {
            return;
        }
        if here {
            self.saved = text.clone();
        }
        ask_later(cx, Request::SetDraft { conversation: key.clone(), text: text.clone() }, |_, _| {});
        self.shell_later(cx, move |shell, cx| shell.draft_changed(&key, &text, cx));
    }

    fn typed(&mut self, cx: &mut Context<Self>) {
        self.follow_text(cx);
        self.touched = true;
        self.caption = None;
        // Any edit disarms the large-ping confirm.
        self.armed = None;
        self.look_up(cx);
        cx.notify();
        let Some(target) = &self.target else {
            return;
        };
        if self.editing.is_some() || !live() {
            return;
        }
        // Rescheduled by each key: the old timer is dropped with its task.
        self.draft_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_QUIET).await;
            let _ = this.update(cx, |this, cx| this.save_draft(cx));
        }));
        let network = target.key.network;
        if target.link != Link::Connected || !sends_typing(&target.key.account()) || self.input.read(cx).text().trim().is_empty() {
            return;
        }
        let recently = self
            .told_typing
            .as_ref()
            .is_some_and(|(key, at)| *key == target.key && at.elapsed() < typing_interval(network));
        if recently {
            return;
        }
        self.told_typing = Some((target.key.clone(), Instant::now()));
        ask_later(cx, Request::Typing { conversation: target.key.clone() }, |_, _| {});
    }

    /// Escape: the first that applies. It never clears what was typed.
    fn escape(&mut self, cx: &mut Context<Self>) {
        let list = self.list.take();
        let colon = self.colon.take();
        self.close_list(cx);
        // A list with no row was never on screen, so Escape is not spent on it.
        if let Some(list) = list.filter(|list| !list.rows.is_empty()) {
            self.dismissed = Some(list.trigger.start);
        } else if let Some(colon) = colon.filter(|colon| !colon.cells.is_empty()) {
            self.colon_dismissed = Some(colon.trigger.start);
        } else if self.armed.take().is_some() {
            self.caption = None;
        } else if self.chip_menu.take().is_some() || self.send_menu {
            self.send_menu = false;
        } else if self.editing.is_some() {
            self.end_edit(cx);
        } else if self.reply.take().is_none() {
            // Nothing of the composer's own is up: the next step of the ladder is the window's
            // (a thread, a card), and the composer keeps its focus.
            let shell = self.shell.clone();
            cx.defer(move |cx| {
                let _ = shell.update(cx, |shell, cx| shell.composer_escaped(cx));
            });
        }
        cx.notify();
    }

    // Sending.

    /// Return, or Option-Return for `silent`. chatd answers as soon as the message is stored
    /// as pending, and the row comes from that; the text and chips leave only then, so a
    /// refused send loses nothing. When the account is offline chatd stores nothing, and the
    /// shell holds a text message instead; files wait for the connection in the tray.
    pub fn send(&mut self, silent: bool, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if self.sending || target.locked().is_some() {
            return;
        }
        if self.editing.is_some() {
            self.save_edit(&target, cx);
            return;
        }
        self.follow_text(cx);
        self.send_menu = false;
        let field = self.input.read(cx).text().to_owned();
        if field.trim().is_empty() && self.tray.is_empty() {
            return;
        }
        if let Some(why) = attach::blocker(&self.tray) {
            self.caption = Some((why, Tone::Refused));
            cx.notify();
            return;
        }
        if !live() {
            return;
        }
        let network = target.key.network;
        let confirmed = self.armed.as_ref().is_some_and(|armed| *armed == field);
        // A role or user group reaches people no count here says: it always asks first.
        let mut roles: Vec<String> = Vec::new();
        for token in self.tokens.iter().filter(|token| token.kind == mention::Kind::Role) {
            if field.get(token.range.clone()) == Some(token.label.as_str()) && !roles.contains(&token.label) {
                roles.push(token.label.clone());
            }
        }
        let broadcast = niceties::broadcast(&field, network).is_some();
        if !confirmed && (broadcast || !roles.is_empty()) {
            let count = match (broadcast, self.counts.get(&target.key).copied()) {
                (false, _) => None,
                (true, None) => {
                    self.count_members(&target.key, silent, cx);
                    return;
                }
                (true, Some((members, online))) => niceties::needs_confirm(&field, network, members, online),
            };
            if let Some(words) = niceties::confirm_words(count, &self.place(), &roles) {
                self.arm(field, words, cx);
                return;
            }
        }
        self.armed = None;
        let controls = self.controls();
        let hosts = niceties::link_hosts(&field);
        let reply_to = self.reply.as_ref().map(|reply| reply.id.clone());
        let with = protocol::Outgoing {
            files: self.tray.iter().filter_map(Staged::outgoing).collect(),
            silent: silent && controls.silent,
            suppress_embeds: self.no_preview && controls.link_chip && !hosts.is_empty(),
            ping: reply_to.is_none() || !controls.ping || !self.ping_off.contains(&target.key.account()),
            pick: None,
            schedule: None,
        };
        let (mut shown, mut wire) = (field.trim().to_owned(), mention::wire(&field, &self.tokens, network).trim().to_owned());
        if super::app::prefs::choices().emoticons {
            (shown, wire) = (niceties::emoticons(&shown), niceties::emoticons(&wire));
        }
        let out = Outgoing {
            key: target.key.clone(),
            shown,
            wire,
            field,
            client_id: client_id(),
            reply_to,
            chips: self.tray.iter().map(|chip| chip.id).collect(),
            made: self.tray.iter().flat_map(Staged::made).collect(),
        };
        self.dispatch(out, with, cx);
    }

    fn dispatch(&mut self, out: Outgoing, with: protocol::Outgoing, cx: &mut Context<Self>) {
        let files = !with.files.is_empty();
        if with.silent {
            SILENT.with_borrow_mut(|silent| silent.insert(out.client_id.clone()));
        }
        let request = Request::Send {
            conversation: out.key.clone(),
            client_id: out.client_id.clone(),
            text: out.wire.clone(),
            reply_to: out.reply_to.clone(),
            with,
        };
        self.sending = true;
        self.told_typing = None;
        cx.notify();
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| this.sent(out, files, answer, cx));
        });
    }

    /// A GIF or a sticker from the picker: sent at once, with the reply target; what is typed
    /// stays.
    fn send_pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|target| target.locked().is_none()) else {
            return;
        };
        if self.sending || !live() {
            return;
        }
        let reply_to = self.reply.as_ref().map(|reply| reply.id.clone());
        let ping = reply_to.is_none() || !self.controls().ping || !self.ping_off.contains(&target.key.account());
        let out = Outgoing {
            key: target.key.clone(),
            field: String::new(),
            shown: String::new(),
            wire: String::new(),
            client_id: client_id(),
            reply_to,
            chips: Vec::new(),
            made: Vec::new(),
        };
        self.dispatch(out, protocol::Outgoing { pick: Some(pick), ping, ..protocol::Outgoing::default() }, cx);
    }

    /// The first Return on a broadcast: how many it would reach is asked once.
    fn count_members(&mut self, key: &Key, silent: bool, cx: &mut Context<Self>) {
        self.sending = true;
        let key = key.clone();
        let this = cx.weak_entity();
        ask_later(cx, Request::ConversationInfo { conversation: key.clone() }, move |answer, cx| {
            let counts = match answer {
                Ok(Response::ConversationInfo { info }) => (info.members, info.online),
                _ => (None, None),
            };
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                // The user may have moved on while the count was asked. What is in the field now
                // belongs to wherever they are: it is never sent on this answer's behalf.
                let still_here = this.target.as_ref().is_some_and(|target| target.key == key);
                this.counts.insert(key, counts);
                if still_here {
                    this.send(silent, cx);
                }
            });
        });
    }

    /// The open conversation as the confirm names it: "#general", or a title.
    fn place(&self) -> String {
        self.target.as_ref().map_or_else(String::new, |target| match target.kind {
            ConversationKind::Channel if !target.title.starts_with('#') => format!("#{}", target.title),
            _ => target.title.clone(),
        })
    }

    fn arm(&mut self, field: String, words: String, cx: &mut Context<Self>) {
        self.armed = Some(field);
        self.caption = Some((words, Tone::Warning));
        cx.notify();
    }

    fn sent(&mut self, out: Outgoing, files: bool, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        self.sending = false;
        let refused = match answer {
            Ok(Response::Sent { .. }) => {
                let (key, client_id, made) = (out.key.clone(), out.client_id.clone(), out.made.clone());
                let _ = self.shell.update(cx, |shell, cx| {
                    if !made.is_empty() {
                        shell.composer_state.outbox.push((key.clone(), client_id.clone(), made));
                    }
                    shell.send_started(&key, &client_id, cx)
                });
                None
            }
            Ok(Response::Error { code: ErrorCode::Offline, .. }) if files => {
                self.caption = Some(("Files need a connection. Your text and files are kept.".to_owned(), Tone::Offline));
                cx.notify();
                return;
            }
            Ok(Response::Error { code: ErrorCode::Offline, .. }) => {
                let held = Held::new(
                    out.key.clone(),
                    out.client_id.clone(),
                    out.shown.clone(),
                    out.wire.clone(),
                    out.reply_to.clone(),
                    now(),
                    Instant::now(),
                );
                let held = self.shell.update(cx, |shell, cx| shell.hold_send(held, cx)).unwrap_or(false);
                (!held).then(|| "Not sent. Too many messages are already waiting for a connection.".to_owned())
            }
            Ok(Response::Error { detail, .. }) => Some(format!("Not sent. {detail}")),
            Ok(_) => Some("Not sent. tinyclient's background process answered something it should not have.".to_owned()),
            Err(_) => Some(format!("{NOT_ANSWERING} Not sent.")),
        };
        match refused {
            Some(words) => self.caption = Some((words, Tone::Refused)),
            None => self.went(&out, cx),
        }
        cx.notify();
    }

    /// The message is chatd's or the hold's now: the field empties, the chips that went leave
    /// the tray, and the draft is forgotten.
    fn went(&mut self, out: &Outgoing, cx: &mut Context<Self>) {
        let here = self.target.as_ref().is_some_and(|now| now.key == out.key);
        if !here {
            if let Some(draft) = self.drafts.get_mut(&out.key) {
                draft.files.retain(|chip| !out.chips.contains(&chip.id));
                draft.text.clear();
                draft.tokens.clear();
            }
            self.store_draft(out.key.clone(), String::new(), cx);
            return;
        }
        self.caption = None;
        self.reply = None;
        if out.field.is_empty() && out.chips.is_empty() {
            // A GIF or sticker: what was typed is the next message.
            return;
        }
        self.tray.retain(|chip| !out.chips.contains(&chip.id));
        self.no_preview = false;
        // More was typed while chatd answered: that is the next message, and stays.
        if self.input.read(cx).text() == out.field {
            self.set_field(&Draft::default(), cx);
            self.draft_timer = None;
            self.save_draft(cx);
        }
    }

    // Files.

    /// Paths from ⌘U, a paste or a drop: each is looked at off the main thread (a size and
    /// whether it is a folder) and staged as a chip, refused there if the network cannot take
    /// it. Nothing is uploaded until Return.
    fn stage_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.stage_files(paths, false, cx);
    }

    fn stage_files(&mut self, paths: Vec<PathBuf>, owned: bool, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|target| target.locked().is_none()) else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        if !self.controls().attach {
            self.refuse_files(cx);
            return;
        }
        let this = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let found: Vec<(PathBuf, Found)> = cx
                .background_executor()
                .spawn(async move { paths.into_iter().map(|path| (attach::look(&path), path)).map(|(found, path)| (path, found)).collect() })
                .await;
            let _ = this.update(cx, |this, cx| this.admit(&target.key, found, owned, cx));
        })
        .detach();
    }

    fn admit(&mut self, key: &Key, found: Vec<(PathBuf, Found)>, owned: bool, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|target| target.key == *key) else {
            if owned {
                delete_later(found.into_iter().map(|(path, _)| path).collect());
            }
            return;
        };
        let network = target.key.network;
        let incoming: Vec<Staged> = found
            .into_iter()
            .map(|(path, found)| {
                let id = self.next_chip;
                self.next_chip += 1;
                let mut chip = Staged::new(id, path, found, &target.can, network);
                chip.owned = owned;
                chip
            })
            .collect();
        let converting: Vec<(u64, PathBuf)> = incoming.iter().filter(|chip| chip.converting).map(|chip| (chip.id, chip.path.clone())).collect();
        let kept: Vec<u64> = incoming.iter().map(|chip| chip.id).collect();
        let turned_away: Vec<PathBuf> = incoming.iter().skip(attach::most(&target.can).saturating_sub(self.tray.len())).filter(|chip| chip.owned).map(|chip| chip.path.clone()).collect();
        let left = attach::admit(&mut self.tray, incoming, &target.can);
        delete_later(turned_away);
        self.caption = (left > 0).then(|| (attach::overflow_words(network, &target.can, left), Tone::Note));
        let staged: Vec<(u64, PathBuf)> = converting.into_iter().filter(|(id, _)| kept.contains(id) && self.tray.iter().any(|chip| chip.id == *id)).collect();
        for (id, heic) in staged {
            self.convert(id, heic, cx);
        }
        self.focus_next = true;
        cx.notify();
    }

    /// A HEIC becomes a JPEG off the main thread; the chip sends that unless asked for the
    /// original.
    fn convert(&mut self, id: u64, heic: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let jpeg = cx.background_executor().spawn(async move { attach::heic_to_jpeg(&attach::outbox(), &heic) }).await;
            let _ = this.update(cx, |this, cx| {
                match this.tray.iter_mut().find(|chip| chip.id == id) {
                    Some(chip) => {
                        chip.converting = false;
                        chip.converted = jpeg;
                    }
                    None => delete_later(jpeg.into_iter().collect()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Files offered where none can go: said under the card, and nothing is kept.
    fn refuse_files(&mut self, cx: &mut Context<Self>) {
        let Some(network) = self.target.as_ref().map(|target| target.key.network) else {
            return;
        };
        let why = match self.editing {
            Some(_) => "Files can't be added while editing a message.".to_owned(),
            None => format!("{} can't take files here.", network.label()),
        };
        self.caption = Some((why, Tone::Note));
        cx.notify();
    }

    /// A screenshot pasted: its pixels are written to `outbox/` off the main thread, then
    /// staged like any file.
    fn stage_pasted(&mut self, image: gpui::Image, cx: &mut Context<Self>) {
        if self.target.as_ref().is_none_or(|target| target.locked().is_some()) {
            return;
        }
        if !self.controls().attach {
            self.refuse_files(cx);
            return;
        }
        let extension = image.format.extension();
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn(async move { attach::write_pasted(&attach::outbox(), &image.bytes, extension, SystemTime::now()) })
                .await;
            let _ = this.update(cx, |this, cx| match written {
                Ok(path) => this.stage_files(vec![path], true, cx),
                Err(err) => {
                    this.caption = Some((format!("The picture couldn't be kept for sending: {err}."), Tone::Refused));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ⌘U: the system's file panel, several at once.
    fn attach_files(&mut self, cx: &mut Context<Self>) {
        if !self.controls().attach || self.target.as_ref().is_none_or(|target| target.locked().is_some()) {
            return;
        }
        let asked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = asked.await {
                let _ = this.update(cx, |this, cx| this.stage_paths(paths, cx));
            }
        })
        .detach();
    }

    pub(super) fn remove_chip(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(at) = self.tray.iter().position(|chip| chip.id == id) {
            let chip = self.tray.remove(at);
            delete_later(chip.made());
        }
        self.chip_menu = None;
        self.caption = None;
        self.focus_next = true;
        cx.notify();
    }

    pub(super) fn toggle_chip(&mut self, id: u64, change: impl FnOnce(&mut Staged), cx: &mut Context<Self>) {
        if let Some(chip) = self.tray.iter_mut().find(|chip| chip.id == id) {
            change(chip);
        }
        self.chip_menu = None;
        cx.notify();
    }

    /// A drag of files moved: whether it is over this conversation, and whether it is only
    /// folders. `bounds` are the dock's.
    fn drag_moved(&mut self, position: Point<Pixels>, bounds: Bounds<Pixels>, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.anchors.dock.set(Some(bounds));
        let accepts = self.controls().attach && self.target.as_ref().is_some_and(|target| target.locked().is_none());
        let inside = accepts
            && position.x >= bounds.left()
            && position.x <= bounds.right()
            && position.y <= bounds.bottom()
            && f32::from(position.y) >= theme::STRIP;
        let folders = match &self.drag {
            Some(drag) if drag.paths == paths => drag.folders,
            _ => !paths.is_empty() && paths.iter().all(|path| path.is_dir()),
        };
        let next = Some(DragOver { paths: paths.to_vec(), folders, inside });
        if self.drag != next {
            self.drag = next;
            cx.notify();
        }
    }

    fn dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let inside = self.drag.take().is_some_and(|drag| drag.inside);
        if inside || self.drag.is_none() {
            self.stage_paths(paths.paths().to_vec(), cx);
        }
        cx.notify();
    }

    // The picker.

    fn toggle_picker(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|target| target.locked().is_none()) else {
            return;
        };
        let controls = self.controls();
        if !controls.tabs.contains(&tab) {
            return;
        }
        if let Some(picking) = &self.picking {
            let open = picking.picker.read(cx).tab();
            if open == tab {
                self.close_picker(cx);
                self.focus_next = true;
            } else {
                picking.picker.update(cx, |picker, cx| picker.switch(tab, cx));
            }
            cx.notify();
            return;
        }
        self.close_list(cx);
        let space = match target.can.custom_emoji {
            EmojiScope::Space => target.space_key(),
            _ => None,
        };
        let setup = Setup {
            account: Some(target.key.account()),
            network: Some(target.key.network),
            space,
            tabs: controls.tabs.clone(),
            open: tab,
            custom: controls.custom_emoji,
            known: Vec::new(),
            reacting: false,
            rules: None,
        };
        let picker = cx.new(|cx| Picker::open(setup, window, cx));
        let listening = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            // Shift-Return: chosen, and the picker stays open with its search focused.
            let (event, keep) = match event {
                PickerEvent::Kept(inner) => (inner.as_ref(), true),
                event => (event, false),
            };
            match event {
                PickerEvent::Picked(text) => this.insert_text(text, cx),
                PickerEvent::Custom { name, id, animated } => this.insert_custom(name, id, *animated, cx),
                PickerEvent::Send(pick) => this.send_pick(pick.clone(), cx),
                PickerEvent::Kept(_) | PickerEvent::Closed => {}
            }
            if !keep {
                this.close_picker(cx);
                window.focus(&this.input.focus_handle(cx), cx);
            }
        });
        self.picking = Some(Picking { picker, _listening: listening });
        cx.notify();
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picking.take().is_some() {
            cx.notify();
        }
    }

    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.insert(text, cx));
    }

    /// A custom emoji goes in as its `:name:`, a token the network's own form replaces at send.
    fn insert_custom(&mut self, name: &str, id: &str, animated: bool, cx: &mut Context<Self>) {
        self.follow_text(cx);
        let start = self.input.read(cx).cursor();
        self.input.update(cx, |input, cx| input.insert(name, cx));
        self.follow_text(cx);
        let id = if animated { format!("a:{id}") } else { id.to_owned() };
        self.tokens.push(Token { range: start..start + name.len(), label: name.to_owned(), kind: mention::Kind::Emoji, id });
        self.follow_text(cx);
        cx.notify();
    }

    /// The window's own file landed: a picker's still or a custom emoji.
    fn media_landed(&mut self, file: &protocol::MediaFile, cx: &mut Context<Self>) {
        if let Some(picking) = &self.picking {
            picking.picker.update(cx, |picker, cx| picker.media_landed(file, cx));
        }
        if let Some(custom) = self.customs.iter_mut().find(|custom| custom.file.as_ref().is_some_and(|held| held.id == file.id)) {
            if let Some(held) = custom.file.as_mut() {
                held.path = file.path.clone();
            }
            cx.notify();
        }
    }

    /// Keys pressed with no field focused: they are typed here.
    fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.insert(text, cx));
    }

    fn can_type(&self) -> bool {
        self.target.as_ref().is_some_and(|target| target.locked().is_none())
    }

    // Reply and edit.

    fn reply_to(&mut self, message: &Message, cx: &mut Context<Self>) {
        let Some(target) = &self.target else {
            return;
        };
        if target.locked().is_some() || !replies_inline(target.key.network) || message.state != SendState::Sent || message.deleted {
            return;
        }
        self.end_edit(cx);
        self.reply = Some(Reply { id: message.id.clone(), name: message.sender.name.clone(), snippet: snippet(message) });
        self.focus_next = true;
        cx.notify();
    }

    /// Puts one of my messages in the field to be changed. What was being typed is set aside,
    /// its chips with it: an edit is text only.
    fn edit(&mut self, message: &Message, cx: &mut Context<Self>) {
        if !editable(message) || self.target.as_ref().is_none_or(|target| target.locked().is_some()) {
            return;
        }
        let (stash, reply) = match self.editing.take() {
            Some(already) => (already.stash, already.reply),
            None => {
                self.save_draft(cx);
                self.draft_timer = None;
                self.follow_text(cx);
                let files = std::mem::take(&mut self.tray);
                (Draft { text: self.input.read(cx).text().to_owned(), tokens: self.tokens.clone(), files }, self.reply.take())
            }
        };
        let (text, tokens) = mention::source(message);
        self.editing = Some(Editing { id: message.id.clone(), original: text.clone(), stash, reply });
        self.set_field(&Draft { text, tokens, files: Vec::new() }, cx);
        self.close_picker(cx);
        self.caption = None;
        self.armed = None;
        self.focus_next = true;
        cx.notify();
    }

    /// Leaves edit mode, saved or not, and puts back what was being typed.
    fn end_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.set_field(&editing.stash, cx);
        self.tray = editing.stash.files;
        self.reply = editing.reply;
        self.caption = None;
        cx.notify();
    }

    /// Up in an empty field: my newest message here, from the store.
    fn edit_last(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref().filter(|target| target.locked().is_none()) else {
            return;
        };
        if self.editing.is_some() || !self.tray.is_empty() {
            return;
        }
        let key = target.key.clone();
        let request = Request::Messages { conversation: key.clone(), page: Page::Newest, limit: LAST_MINE };
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Messages { messages, .. }) = answer else {
                return;
            };
            let Some(mine) = messages.iter().rev().find(|message| editable(message)) else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                // The answer is for the field as it was: still here, still empty.
                let same = this.target.as_ref().is_some_and(|target| target.key == key);
                if same && this.editing.is_none() && this.input.read(cx).text().is_empty() {
                    this.edit(mine, cx);
                }
            });
        });
    }

    fn save_edit(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some((id, original)) = self.editing.as_ref().map(|editing| (editing.id.clone(), editing.original.clone())) else {
            return;
        };
        self.follow_text(cx);
        let field = self.input.read(cx).text().to_owned();
        if field.trim() == original.trim() {
            self.end_edit(cx);
            return;
        }
        if field.trim().is_empty() {
            self.caption = Some(("A message can't be saved empty. Delete it from its menu instead.".to_owned(), Tone::Note));
            cx.notify();
            return;
        }
        if !live() {
            return;
        }
        let key = target.key.clone();
        let request = Request::Edit {
            conversation: key.clone(),
            message_id: id.clone(),
            text: mention::wire(&field, &self.tokens, key.network).trim().to_owned(),
        };
        self.sending = true;
        cx.notify();
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| this.edit_answered(&key, &id, answer, cx));
        });
    }

    fn edit_answered(&mut self, key: &Key, id: &str, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        self.sending = false;
        cx.notify();
        let here = self.target.as_ref().is_some_and(|target| target.key == *key);
        if !here || self.editing.as_ref().is_none_or(|editing| editing.id != id) {
            return;
        }
        match answer {
            // The row changes when the network's own word arrives; the field is free again.
            Ok(Response::Ok) => self.end_edit(cx),
            Ok(Response::Error { detail, .. }) => self.caption = Some((format!("Not saved. {detail}"), Tone::Refused)),
            Ok(_) => self.caption = Some(("Not saved. tinyclient's background process answered something it should not have.".to_owned(), Tone::Refused)),
            Err(_) => self.caption = Some((format!("{NOT_ANSWERING} Not saved."), Tone::Refused)),
        }
    }

    // Completion.

    fn close_list(&mut self, cx: &mut Context<Self>) {
        self.list = None;
        self.colon = None;
        self.lookup = None;
        self.input.update(cx, |input, _| input.set_list_open(false));
    }

    fn show_list(&mut self, list: List, cx: &mut Context<Self>) {
        // Return and the arrows are the list's only while it has a row to give.
        let open = !list.rows.is_empty();
        self.list = Some(list);
        self.colon = None;
        self.input.update(cx, |input, _| input.set_list_open(open));
    }

    /// The people the shell already holds who could be meant: those typing here.
    fn known(&self, key: &Key) -> Vec<Person> {
        let typists = self.hub.typists_in(key);
        typists.iter().filter_map(|typist| self.hub.person(&Key { id: typist.id.clone(), ..key.clone() })).collect()
    }

    /// After a keystroke: what the caret is completing, narrowed at once from the rows on show
    /// and asked of the store once the keys rest. `:` is answered here, from the table.
    fn look_up(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let (text, caret) = {
            let input = self.input.read(cx);
            (input.text().to_owned(), input.cursor())
        };
        if let Some(found) = colon::trigger(&text, caret) {
            if self.colon_dismissed == Some(found.start) {
                self.close_list(cx);
                return;
            }
            self.ask_customs(&target, cx);
            let list = colon::List::new(found, &picker::emoji::recent(), &self.customs);
            let open = !list.cells.is_empty();
            self.list = None;
            self.lookup = None;
            self.colon = Some(list);
            self.input.update(cx, |input, _| input.set_list_open(open));
            return;
        }
        self.colon = None;
        self.colon_dismissed = None;
        let Some(trigger) = mention::trigger(&text, caret, &self.tokens) else {
            self.dismissed = None;
            self.close_list(cx);
            return;
        };
        let request = complete::request(&trigger, &target.key, target.space.as_deref());
        let (Some(request), false) = (request, self.dismissed == Some(trigger.start)) else {
            self.close_list(cx);
            return;
        };
        let rows = match &self.list {
            Some(list) => list.narrowed(&trigger),
            None if trigger.kind == mention::Kind::Person => complete::people(self.known(&target.key), Vec::new(), &target.key.account(), &trigger.query),
            None => Vec::new(),
        };
        // A space after no match: this is a sentence, not a name.
        if rows.is_empty() && trigger.query.contains(char::is_whitespace) {
            self.dismissed = Some(trigger.start);
            self.close_list(cx);
            return;
        }
        self.show_list(List { trigger: trigger.clone(), rows, selected: 0 }, cx);
        // One timer, replaced by each key; a read that was overtaken is dropped with its task,
        // so the newest answer is the only one that lands.
        self.lookup = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOOKUP_QUIET).await;
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| this.looked_up(&target, &trigger, answer, cx));
        }));
    }

    /// The account's own emoji for `:`, asked the first time a colon list opens on it.
    fn ask_customs(&mut self, target: &Target, cx: &mut Context<Self>) {
        let account = target.key.account();
        let space = match target.can.custom_emoji {
            EmojiScope::None => return,
            EmojiScope::Space => target.space_key(),
            EmojiScope::All => None,
        };
        let wanted = Some((account.clone(), space.clone()));
        if self.customs_for == wanted {
            return;
        }
        self.customs_for = wanted;
        self.customs.clear();
        let request = Request::Catalog { account: account.clone(), kind: CatalogKind::Emoji, space, query: String::new(), cursor: None };
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Catalog { items, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.customs_for.as_ref().map(|(held, _)| held) != Some(&account) {
                    return;
                }
                this.customs = items
                    .into_iter()
                    .map(|item| Custom {
                        name: if item.name.starts_with(':') { item.name } else { format!(":{}:", item.name) },
                        id: item.id,
                        file: item.file,
                        pack: item.pack,
                        animated: item.animated,
                    })
                    .collect();
                let missing: Vec<String> = this.customs.iter().filter_map(|custom| custom.file.as_ref()).filter(|file| file.path.is_none()).map(|file| file.id.clone()).collect();
                if !missing.is_empty() && live() {
                    ask_later(cx, Request::Fetch { account, ids: missing, size: Some(40), retry: false }, |_, _| {});
                }
                this.look_up(cx);
                cx.notify();
            });
        });
    }

    fn looked_up(&mut self, target: &Target, trigger: &mention::Trigger, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        if self.list.as_ref().is_none_or(|list| list.trigger != *trigger) {
            return;
        }
        let account = target.key.account();
        let rows = match (answer, target.space.as_deref()) {
            (Ok(Response::People { people, .. }), _) => {
                self.hub.learn(&people);
                complete::people(self.known(&target.key), people, &account, &trigger.query)
            }
            (Ok(Response::Conversations { conversations, .. }), Some(space)) => complete::channels(conversations, &account, space, &trigger.query),
            _ => Vec::new(),
        };
        if rows.is_empty() && trigger.query.contains(char::is_whitespace) {
            self.dismissed = Some(trigger.start);
            self.close_list(cx);
        } else {
            self.show_list(List { trigger: trigger.clone(), rows, selected: 0 }, cx);
        }
        cx.notify();
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        if let Some(colon) = &mut self.colon {
            colon.step(down);
        } else if let Some(list) = &mut self.list {
            list.step(down);
        }
        cx.notify();
    }

    fn complete_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = self.colon.as_ref().map(|colon| colon.selected) {
            self.complete_colon(selected, cx);
        } else {
            let selected = self.list.as_ref().map_or(0, |list| list.selected);
            self.complete(selected, cx);
        }
    }

    /// Tab, Return or a click: the row's name takes the place of what was typed to find it,
    /// and is a mention from then on.
    fn complete(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(list) = self.list.take() else {
            return;
        };
        self.close_list(cx);
        self.follow_text(cx);
        let (caret, text_len) = {
            let input = self.input.read(cx);
            (input.cursor(), input.text().len())
        };
        let start = list.trigger.start;
        let Some(row) = list.rows.get(row).filter(|_| start < caret && caret <= text_len) else {
            return;
        };
        let label = row.label();
        self.input.update(cx, |input, cx| input.replace(start..caret, &format!("{label} "), cx));
        // The tokens are moved for this edit here, so the new one is not taken for part of it.
        self.follow_text(cx);
        self.tokens.push(Token { range: start..start + label.len(), label, kind: row.kind, id: row.key.id.clone() });
        self.follow_text(cx);
        cx.notify();
    }

    /// The `:` list's row: a glyph takes the place of `:tad`, or a custom emoji's `:name:` as
    /// a token.
    fn complete_colon(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(list) = self.colon.take() else {
            return;
        };
        self.close_list(cx);
        self.follow_text(cx);
        let (caret, text_len) = {
            let input = self.input.read(cx);
            (input.cursor(), input.text().len())
        };
        let start = list.trigger.start;
        let Some(cell) = list.cells.get(row).filter(|_| start < caret && caret <= text_len) else {
            return;
        };
        let text = picker::emoji::text_of(cell, picker::emoji::tone(), &self.customs);
        picker::emoji::used(&text);
        let custom = match cell {
            picker::emoji::Cell::Custom(ix) => self.customs.get(*ix as usize).cloned().filter(|custom| !custom.id.is_empty()),
            _ => None,
        };
        self.input.update(cx, |input, cx| input.replace(start..caret, &text, cx));
        self.follow_text(cx);
        if let Some(custom) = custom {
            let id = if custom.animated { format!("a:{}", custom.id) } else { custom.id };
            self.tokens.push(Token { range: start..start + text.len(), label: text, kind: mention::Kind::Emoji, id });
            self.follow_text(cx);
        }
        cx.notify();
    }

    // Drawing.

    fn sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(network) = self.target.as_ref().map(|target| target.key.network) else {
            return;
        };
        let _ = self.shell.update(cx, |shell, cx| shell.sign_in(network, cx));
    }

    /// The 40 pt bar that stands where the card would be.
    fn bar(&self, locked: Locked, network: Network, cx: &mut Context<Self>) -> Div {
        let name = network.label();
        let (icon, words, action) = match locked {
            Locked::SignedOut => ("icons/lock.svg", format!("{name} is signed out."), Some("Sign in")),
            Locked::Expired => (
                "icons/circle-alert.svg",
                format!("{name} signed you out. Your messages are still here."),
                Some("Sign in again"),
            ),
            Locked::ReadOnly(sentence) => ("icons/lock.svg", sentence, None),
        };
        let expired = action == Some("Sign in again");
        div()
            .h(px(CARD))
            .pl(px(12.))
            .pr(px(6.))
            .rounded(px(theme::R_FLOAT))
            .border_1()
            .border_color(if expired { theme::danger_soft() } else { theme::border() })
            .when(expired, |bar| bar.bg(theme::danger_soft()))
            .flex()
            .items_center()
            .gap(px(8.))
            .type_ui()
            .text_color(if expired { theme::text() } else { theme::text_soft() })
            .child(glyph(icon, 16., if expired { theme::danger() } else { theme::text_faint() }))
            .child(div().flex_1().min_w_0().truncate().child(words))
            .children(action.map(|label| {
                button("composer-sign-in", ButtonKind::Primary, label)
                    .on_click(cx.listener(|this, _, _, cx| this.sign_in(cx)))
            }))
    }

    /// The 32 pt bar inside the card that says what Return will do: reply, or save an edit.
    fn mode_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let bar = div()
            .h(px(32.))
            .flex_none()
            .pl(px(12.))
            .pr(px(4.))
            .border_b_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::text_soft())
            .cursor_default();
        let strong = |words: String| div().flex_none().max_w(px(220.)).truncate().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(words);
        let faint = |words: String| div().flex_1().min_w_0().truncate().text_color(theme::text_faint()).child(words);
        let close = div()
            .id("mode-close")
            .size(px(24.))
            .flex_none()
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                if this.editing.is_some() {
                    this.end_edit(cx);
                } else {
                    this.reply = None;
                }
                this.focus_next = true;
                cx.notify();
            }))
            .child(glyph("icons/x.svg", 14., theme::text_soft()));
        if self.editing.is_some() {
            return Some(
                bar.child(glyph("icons/pencil.svg", 14., theme::accent_text()))
                    .child(strong("Editing message".to_owned()))
                    .child(div().flex_1())
                    .child(div().flex_none().text_color(theme::text_faint()).child("Return to save · Esc to cancel"))
                    .child(close),
            );
        }
        let reply = self.reply.as_ref()?;
        let account = self.target.as_ref().map(|target| target.key.account());
        let ping = self.controls().ping.then(|| {
            let on = account.as_ref().is_none_or(|account| !self.ping_off.contains(account));
            let first = reply.name.split_whitespace().next().unwrap_or("them").to_owned();
            div()
                .id("reply-ping")
                .flex_none()
                .h(px(24.))
                .px(px(6.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(2.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .font_weight(theme::MEDIUM)
                .text_color(if on { theme::accent_text() } else { theme::text_soft() })
                .child("@")
                .child(if on { "On" } else { "Off" })
                .tooltip(tip(format!("Mention {first} in the reply"), None))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(account) = this.target.as_ref().map(|target| target.key.account()) {
                        if !this.ping_off.remove(&account) {
                            this.ping_off.insert(account);
                        }
                    }
                    cx.notify();
                }))
        });
        Some(
            bar.child(glyph("icons/corner-up-left.svg", 14., theme::accent_text()))
                .child(div().flex_none().child("Replying to"))
                .child(strong(reply.name.clone()))
                .child(faint(reply.snippet.clone()))
                .children(ping)
                .child(close),
        )
    }

    /// A 28 pt icon button on the card, its bounds kept for what it opens.
    fn card_button(&self, id: &'static str, icon: Option<&'static str>, toggled: bool, slot: Option<Trigger>) -> gpui::Stateful<Div> {
        let anchors = self.anchors.clone();
        let content = match icon {
            Some(icon) => glyph(icon, 16., if toggled { theme::text() } else { theme::text_soft() }).into_any_element(),
            // CONTRACT: the design's `gif-16` glyph is not among slice A's assets; the word in a
            // 16 × 12 outline stands in for it.
            None => div()
                .w(px(18.))
                .h(px(13.))
                .rounded(px(3.))
                .border_1()
                .border_color(if toggled { theme::text() } else { theme::text_soft() })
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(7.))
                .line_height(px(8.))
                .font_weight(theme::SEMIBOLD)
                .text_color(if toggled { theme::text() } else { theme::text_soft() })
                .child("GIF")
                .into_any_element(),
        };
        div()
            .id(id)
            .relative()
            .flex_none()
            .size(px(theme::CONTROL))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(toggled, |button| button.bg(theme::selected()))
            .hover(|style| style.bg(theme::hover()))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(content)
            .children(slot.map(|slot| {
                let keep = move |bounds| match slot {
                    Trigger::Gif => anchors.gif.set(Some(bounds)),
                    Trigger::Smile => anchors.smile.set(Some(bounds)),
                };
                canvas(move |bounds, _, _| keep(bounds), |_, _, _, _| {}).absolute().top_0().left_0().size_full()
            }))
    }

    fn card(&mut self, focused: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let controls = self.controls();
        let ready = !self.sending && (!self.input.read(cx).text().trim().is_empty() || !self.tray.is_empty());
        let focus = self.input.focus_handle(cx);
        let open_tab = self.picking.as_ref().map(|picking| picking.picker.read(cx).tab());
        let send_icon = if self.editing.is_some() { "icons/check.svg" } else { "icons/send-horizontal.svg" };
        let send_tip = match (self.editing.is_some(), controls.silent) {
            (true, _) => "Save  ↩",
            (false, true) => "Send  ↩ · Silently  ⌥↩",
            (false, false) => "Send  ↩",
        };
        let attach = controls.attach.then(|| {
            self.card_button("attach", Some("icons/paperclip.svg"), false, None)
                .tooltip(tip("Attach files", Some("⌘U")))
                .on_click(cx.listener(|this, _, _, cx| this.attach_files(cx)))
        });
        let gif = controls.gif.then(|| {
            self.card_button("gif", None, open_tab == Some(Tab::Gifs), Some(Trigger::Gif))
                .tooltip(tip("GIFs", Some("⇧⌘G")))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_picker(Tab::Gifs, window, cx)))
        });
        let smile = controls.emoji.then(|| {
            self.card_button("emoji", Some("icons/smile.svg"), open_tab == Some(Tab::Emoji) || open_tab == Some(Tab::Stickers), Some(Trigger::Smile))
                .tooltip(tip("Emoji", Some("⌘E")))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_picker(Tab::Emoji, window, cx)))
        });
        let anchors = self.anchors.clone();
        let send = div()
            .id("send")
            .relative()
            .size(px(theme::CONTROL))
            .flex_none()
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .when(ready, |send| {
                send.bg(theme::accent())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::accent_hover()))
                    .active(|style| style.bg(theme::accent_pressed()))
            })
            .tooltip(tip(send_tip, None))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.send_menu = this.controls().silent;
                    cx.notify();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| this.send(false, cx)))
            .child(glyph(send_icon, 16., if ready { theme::on_accent() } else { theme::text_disabled() }))
            .child(canvas(move |bounds, _, _| anchors.send.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let row = div()
            .min_h(px(CARD - 2.))
            .pl(px(if controls.attach { 5. } else { 12. }))
            .pr(px(5.))
            .py(px(5.))
            .flex()
            .items_end()
            .children(attach.map(|attach| attach.mr(px(4.))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .py(px(4.))
                    .mr(px(4.))
                    .flex()
                    .type_body()
                    .text_color(theme::text())
                    .child(self.input.clone()),
            )
            .children(gif.map(|gif| gif.mr(px(2.))))
            .children(smile.map(|smile| smile.mr(px(4.))))
            .child(send);
        let armed = self.armed.is_some();
        div()
            .min_h(px(CARD))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::raised())
            .border_1()
            .border_color(match (armed, focused) {
                (true, _) => theme::warning(),
                (false, true) => theme::border_strong(),
                (false, false) => theme::border(),
            })
            .flex()
            .flex_col()
            .cursor_text()
            // A click anywhere on the card is a click into the field.
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| window.focus(&focus, cx))
            .children(self.mode_bar(cx))
            .children(self.tray(window, cx))
            .child(row)
    }

    /// The one line under the card: the link chip, then why the last thing did not happen,
    /// else what the tray will do, else that the network is away.
    fn caption_line(&self, target: &Target, cx: &mut Context<Self>) -> Option<Div> {
        let offline = || {
            let name = target.key.network.label();
            (format!("{name} is offline. A message sent now waits up to 60 seconds for it to come back."), Tone::Offline)
        };
        let unlocked = target.locked().is_none();
        let text = self.input.read(cx).text().to_owned();
        let note = || attach::sending_note(target.key.network, &target.can, &self.tray, &text).map(|words| (words, Tone::Note));
        let said = self.caption.clone().or_else(note).or_else(|| (unlocked && target.link == Link::Offline).then(offline));
        let hosts = if self.controls().link_chip && unlocked { niceties::link_hosts(&text) } else { Vec::new() };
        let chip = niceties::preview_words(&hosts, self.no_preview).map(|words| {
            let suppressed = self.no_preview;
            div()
                .id("link-chip")
                .flex_none()
                .h(px(20.))
                .px(px(6.))
                .rounded(px(theme::R_CHIP))
                .when(!suppressed, |chip| chip.bg(theme::hover()))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(if suppressed { theme::text_faint() } else { theme::text_soft() })
                .when(!suppressed, |chip| chip.child(glyph("icons/link.svg", 12., theme::text_soft())))
                .child(words)
                .child(
                    div()
                        .id("link-chip-toggle")
                        .flex_none()
                        .cursor_pointer()
                        .rounded(px(theme::R_CHIP))
                        .px(px(2.))
                        .hover(|style| style.bg(theme::selected()))
                        .map(|toggle| match suppressed {
                            true => toggle.text_color(theme::accent_text()).child("Undo"),
                            false => toggle.child(glyph("icons/x.svg", 12., theme::text_soft())),
                        })
                        .tooltip(tip(if suppressed { "Let the network show a preview" } else { "Send without a preview" }, None))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.no_preview = !this.no_preview;
                            cx.notify();
                        })),
                )
        });
        if said.is_none() && chip.is_none() {
            return None;
        }
        let said = said.map(|(words, tone)| {
            let (icon, color): (_, Rgba) = match tone {
                Tone::Offline => ("icons/wifi-off.svg", theme::warning()),
                Tone::Refused => ("icons/circle-alert.svg", theme::danger()),
                Tone::Note => ("icons/info.svg", theme::text_soft()),
                Tone::Warning => ("icons/triangle-alert.svg", theme::warning()),
            };
            let words_color = match tone {
                Tone::Warning | Tone::Offline => theme::text(),
                _ => color,
            };
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_color(words_color)
                .child(glyph(icon, 12., color))
                .child(div().min_w_0().truncate().child(words))
        });
        Some(
            div()
                .mt(px(6.))
                .px(px(13.))
                .flex()
                .items_center()
                .gap(px(10.))
                .type_small()
                .children(chip)
                .children(said),
        )
    }

    /// The completion lists, 6 pt above the card and over the messages.
    fn popover(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.colon.as_ref().is_some_and(|colon| !colon.cells.is_empty()) {
            let this = cx.weak_entity();
            let pick = move |row: usize, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.complete_colon(row, cx));
            };
            let tone = picker::emoji::tone();
            let (colon, customs, thumbs) = (self.colon.as_ref()?, &self.customs, &mut self.thumbs);
            // Custom emoji are decoded like the chips, off the main thread.
            let image = |custom: &Custom| {
                let path = custom.file.as_ref()?.path.clone()?;
                match thumbs.get(&path, 40, None, cx, Self::thumb_decoded) {
                    picker::images::Picture::Ready(image) => Some(image),
                    _ => None,
                }
            };
            let drawn = colon::draw(colon, customs, tone, image, pick);
            return Some(deferred(div().absolute().left_0().bottom(relative(1.)).pb(px(6.)).child(drawn)).with_priority(1).into_any_element());
        }
        let list = self.list.as_ref().filter(|list| !list.rows.is_empty())?;
        let this = cx.weak_entity();
        let pick = move |row: usize, _: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |this, cx| this.complete(row, cx));
        };
        let drawn = complete::draw(list, &self.hub, pick, cx);
        Some(deferred(div().absolute().left_0().bottom(relative(1.)).pb(px(6.)).child(drawn)).with_priority(1).into_any_element())
    }

    /// The picker, above the card, right edge on its trigger's, inside the window.
    fn picker_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let picking = self.picking.as_ref()?;
        let tab = picking.picker.read(cx).tab();
        let anchor = match tab {
            Tab::Gifs => self.anchors.gif.get().or(self.anchors.smile.get()),
            _ => self.anchors.smile.get(),
        }?;
        let place = Place::Above(anchor);
        let viewport = window.viewport_size();
        let room = max_height(place, (f32::from(viewport.width), f32::from(viewport.height)));
        picking.picker.update(cx, |picker, cx| picker.set_height(room.min(picker::HEIGHT), cx));
        let panel = div()
            .w(px(picker::WIDTH))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_popover())
            .overflow_hidden()
            .occlude()
            .child(picking.picker.clone())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_picker(cx)));
        Some(placed("composer-picker", place, Layer::Popover, window, panel))
    }

    /// Right-click on Send: what else Return can do here.
    fn send_menu_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.send_menu {
            return None;
        }
        let anchor = self.anchors.send.get()?;
        let items = menu()
            .w(px(260.))
            .child(menu_item("send-silently", Some("icons/bell-off.svg"), "Send without notification", Some("⌥↩")).on_click(cx.listener(|this, _, _, cx| {
                this.send_menu = false;
                this.send(true, cx);
            })))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.send_menu = false;
                cx.notify();
            }));
        Some(placed("send-menu", Place::Above(anchor), Layer::Menu, window, items))
    }

    /// The frame over the conversation while files are dragged over it.
    fn drop_layer(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let drag = self.drag.as_ref().filter(|drag| drag.inside)?;
        let bounds = self.anchors.dock.get()?;
        let target = self.target.as_ref()?;
        let top = theme::STRIP;
        let height = f32::from(bounds.bottom()) - top;
        let (border, wash, icon, color, title, line) = if drag.folders {
            (theme::danger(), theme::danger_soft(), "icons/circle-alert.svg", theme::danger(), "Folders can't be sent".to_owned(), "Zip it first.".to_owned())
        } else {
            let most = attach::most(&target.can);
            let limit = attach::size(target.can.max_file_bytes);
            (
                theme::accent(),
                theme::accent_wash(),
                "icons/paperclip.svg",
                theme::accent_text(),
                format!("Drop to attach to {}", if target.kind == ConversationKind::Channel && !target.title.starts_with('#') { format!("#{}", target.title) } else { target.title.clone() }),
                format!("Up to {most} files · {limit} each on {}", target.key.network.label()),
            )
        };
        let frame = div()
            .w(bounds.size.width)
            .h(px(height))
            .p(px(8.))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.dropped(paths, cx)))
            .child(
                div()
                    .size_full()
                    .rounded(px(theme::R_FLOAT))
                    .border_2()
                    .border_color(border)
                    // The messages stay faintly visible under it; the words must read over them.
                    .bg(Rgba { a: 0.9, ..theme::over(wash, theme::canvas()) })
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .child(glyph(icon, 20., color))
                    .child(div().mt(px(4.)).text_size(px(16.)).line_height(px(22.)).font_weight(theme::SEMIBOLD).text_color(theme::text()).child(title))
                    .child(div().type_ui().text_color(theme::text_soft()).child(line)),
            );
        let at = gpui::point(bounds.left(), px(top));
        Some(deferred(gpui::anchored().position(at).child(frame)).with_priority(Layer::Float as usize).into_any_element())
    }

    #[cfg(feature = "gui-shot")]
    pub fn stage(&mut self, text: Option<String>, typists: Option<Vec<String>>, refused: bool, cx: &mut Context<Self>) {
        if let Some(text) = text {
            self.set_field(&Draft { text, ..Draft::default() }, cx);
        }
        self.typing.update(cx, |typing, _| typing.staged = typists);
        if refused {
            self.caption = Some(("Not sent. You can't post in this channel.".to_owned(), Tone::Refused));
        }
        self.stage_flag(cx);
        cx.notify();
    }

    /// The states `--composer` names. Nothing here asks chatd for anything that changes it.
    #[cfg(feature = "gui-shot")]
    fn stage_flag(&mut self, cx: &mut Context<Self>) {
        let field = |text: &str, tokens: Vec<Token>| Draft { text: text.to_owned(), tokens, files: Vec::new() };
        let Some(target) = self.target.clone() else {
            return;
        };
        let key = target.key.clone();
        if staged("reply") || staged("link") {
            self.reply = Some(Reply {
                id: "1".into(),
                name: "Mara Okafor".into(),
                snippet: "Migration finished clean. Resuming consumers now.".into(),
            });
            let text = if staged("link") { "Checked. Route notes: https://trailnotes.example/creek" } else { "Nice, lag looks flat on my side too" };
            self.set_field(&field(text, Vec::new()), cx);
            if staged("link") {
                self.tray = shot_chips(&target, 1);
            }
        } else if staged("edit") {
            let original = "Sounds good. I'll keep the consumers paused until it finishes.";
            self.editing = Some(Editing { id: "1".into(), original: original.into(), stash: Draft::default(), reply: None });
            self.set_field(&field(original, Vec::new()), cx);
        } else if staged("complete") {
            let person = |name: &str, handle: &str, presence| Person {
                key: Key { id: handle.to_owned(), ..key.clone() },
                name: name.to_owned(),
                handle: handle.to_owned(),
                avatar: None,
                bot: false,
                relation: protocol::Relation::None,
                presence,
                status: String::new(),
                link: String::new(),
                emoji: String::new(),
                activity: None,
                title: String::new(),
                utc_offset: None,
                mark: None,
            };
            let found = vec![
                person("Dev Patel", "devp", protocol::Presence::Online),
                person("Devon Reyes", "devon.reyes", protocol::Presence::Idle),
                person("Nadia Devlin", "ndevlin", protocol::Presence::Offline),
                person("Dev Kapoor", "devk", protocol::Presence::Online),
            ];
            let text = "Thanks @Mara Okafor. Can you take a look, @dev";
            let mara = Token { range: 7..19, label: "@Mara Okafor".into(), kind: mention::Kind::Person, id: "mara".into() };
            self.set_field(&field(text, vec![mara]), cx);
            let trigger = mention::Trigger { kind: mention::Kind::Person, start: text.len() - 4, query: "dev".into() };
            let rows = complete::people(Vec::new(), found, &key.account(), "dev");
            self.show_list(List { trigger, rows, selected: 0 }, cx);
        } else if staged("colon") {
            let text = "We found the marten den :tad";
            self.set_field(&field(text, Vec::new()), cx);
            // Answered like a key press: the table now, the account's own once they arrive.
            self.look_up(cx);
        } else if staged("ping") {
            let text = "@everyone the creek route is on for Saturday";
            self.set_field(&field(text, Vec::new()), cx);
            let words = niceties::confirm_words(Some(1_204), &self.place(), &[]).unwrap_or_default();
            self.arm(text.to_owned(), words, cx);
        } else if staged("ping-role") {
            let text = "@trail-crew the creek route is on for Saturday";
            let crew = Token { range: 0..11, label: "@trail-crew".into(), kind: mention::Kind::Role, id: "41".into() };
            self.set_field(&field(text, vec![crew]), cx);
            let words = niceties::confirm_words(None, &self.place(), &["@trail-crew".to_owned()]).unwrap_or_default();
            self.arm(text.to_owned(), words, cx);
        } else if staged("offline") || staged("waiting") {
            self.set_field(&field("Back in ten", Vec::new()), cx);
        } else if staged("files-offline") {
            self.set_field(&field("And the thermos", Vec::new()), cx);
            self.tray = shot_chips(&target, 2);
            self.caption = Some(("Files need a connection. Your text and files are kept.".to_owned(), Tone::Offline));
        } else if staged("chips") || staged("toobig") || staged("chip-menu") {
            self.set_field(&field("Last night on camera 2", Vec::new()), cx);
            self.tray = shot_chips(&target, if staged("chips") { 4 } else { 5 });
            if staged("toobig") {
                self.caption = attach::blocker(&self.tray).map(|why| (why, Tone::Refused));
            }
            if staged("chip-menu") {
                self.chip_menu = self.tray.get(1).map(|chip| chip.id);
            }
        } else if staged("uploading") && std::env::var_os("TINYCLIENT_DIR").is_some() {
            // Only ever against a scratch daemon: a screenshot must never send on a real account.
            // A real send of two files, silently: with a slow demo upload the pending row shows
            // "Uploading 1 of 2" and the bell-off.
            let dir = std::env::temp_dir().join(format!("tinyclient-shot-upload-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let mut tray = Vec::new();
            for (n, (name, bytes)) in [("trail-notes.pdf", 600_000usize), ("route.gpx", 400_000)].into_iter().enumerate() {
                let path = dir.join(name);
                let _ = std::fs::write(&path, vec![7u8; bytes]);
                tray.push(Staged::new(n as u64 + 1, path, Found::File(bytes as u64), &target.can, target.key.network));
            }
            // Shots never send (`live`); this one does, to the scratch chatd it names only.
            if std::env::var_os("TINYCLIENT_DIR").is_some() {
                let with = protocol::Outgoing { files: tray.iter().filter_map(Staged::outgoing).collect(), silent: true, ..protocol::Outgoing::default() };
                let text = "Notes and the route for Saturday".to_owned();
                let out = Outgoing {
                    key: target.key.clone(),
                    shown: text.clone(),
                    wire: text.clone(),
                    field: text,
                    client_id: client_id(),
                    reply_to: None,
                    chips: Vec::new(),
                    made: Vec::new(),
                };
                self.dispatch(out, with, cx);
            }
        } else if staged("full") {
            let mut tray = Vec::new();
            let left = attach::admit(&mut tray, (0..12).map(|n| shot_chip(&target, n, &format!("trail-{n}.pdf"), Found::File(1_200_000 + n * 1000))).collect(), &target.can);
            self.tray = tray;
            self.caption = Some((attach::overflow_words(key.network, &target.can, left), Tone::Note));
        } else if staged("drop") || staged("drop-folder") {
            self.drag = Some(DragOver { paths: Vec::new(), folders: staged("drop-folder"), inside: true });
        } else if staged("refused") {
            self.caption = Some(("Sending files isn't built yet. Nothing was attached.".to_owned(), Tone::Note));
        }
    }

    /// `--composer picker-*`: the picker open on a tab, after the card has been drawn once.
    #[cfg(feature = "gui-shot")]
    fn stage_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = if staged("picker-gifs") {
            Tab::Gifs
        } else if staged("picker-stickers") {
            Tab::Stickers
        } else if staged("picker-emoji") || staged("picker-search") || staged("picker-tone") {
            Tab::Emoji
        } else {
            return;
        };
        if self.picking.is_some() || self.anchors.smile.get().is_none() {
            return;
        }
        self.toggle_picker(tab, window, cx);
        if let Some(picking) = &self.picking {
            picking.picker.update(cx, |picker, cx| picker.stage(staged("picker-search"), staged("picker-tone"), cx));
        }
    }
}

/// Chips for pictures: the demo's own pictures on disk stand in for a screenshot and photos.
#[cfg(feature = "gui-shot")]
fn shot_chips(target: &Target, count: usize) -> Vec<Staged> {
    let pictures = shot_pictures();
    let picture = |n: usize| pictures.get(n % pictures.len().max(1)).cloned();
    let mut chips = Vec::new();
    if let Some(path) = picture(0) {
        let mut chip = shot_chip(target, 1, "", Found::File(1_800_000));
        chip.path = path;
        chip.name = "Pasted image 9.41.12.png".into();
        chip.kind = attach::Kind::Image;
        chips.push(chip);
    }
    if let Some(path) = picture(1) {
        let mut chip = shot_chip(target, 2, "IMG_0042.HEIC", Found::File(3_100_000));
        chip.converting = false;
        chip.converted = Some(path);
        chips.push(chip);
    }
    if let Some(path) = picture(2) {
        let mut chip = shot_chip(target, 3, "trail-cam.jpg", Found::File(2_200_000));
        chip.path = path;
        chip.spoiler = target.can.spoiler;
        chips.push(chip);
    }
    chips.push(shot_chip(target, 4, "Trail map for the autumn foray.pdf", Found::File(2_400_000)));
    chips.push(shot_chip(target, 5, "marten-night.mov", Found::File(84_000_000)));
    chips.truncate(count);
    chips
}

#[cfg(feature = "gui-shot")]
fn shot_chip(target: &Target, id: u64, name: &str, found: Found) -> Staged {
    Staged::new(id, PathBuf::from("/tmp").join(name), found, &target.can, target.key.network)
}

/// Pictures the demo wrote under `media/`, for the chips' thumbnails.
#[cfg(feature = "gui-shot")]
fn shot_pictures() -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, found: &mut Vec<PathBuf>, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() && depth < 4 {
                walk(&path, found, depth + 1);
            } else if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 20_000) {
                let mut head = [0u8; 4];
                let picture = std::fs::File::open(&path).and_then(|mut file| std::io::Read::read_exact(&mut file, &mut head)).is_ok()
                    && (head == [0x89, b'P', b'N', b'G'] || head[..3] == [0xFF, 0xD8, 0xFF]);
                if picture {
                    found.push(path);
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(&protocol::support_dir().join("media"), &mut found, 0);
    found
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.input.focus_handle(cx);
        if std::mem::take(&mut self.blur_next) {
            window.blur(cx);
        } else if std::mem::take(&mut self.focus_next) {
            window.focus(&focus, cx);
        }
        let conversation = self.target.as_ref().map(|target| target.key.clone());
        let active = window.is_window_active();
        self.typing.update(cx, |typing, cx| typing.show(conversation.as_ref(), active, cx));
        // A drag that ended outside, or was let go elsewhere, leaves no frame.
        #[cfg(not(feature = "gui-shot"))]
        if self.drag.is_some() && !cx.has_active_drag() {
            self.drag = None;
        }
        if self.tray.is_empty() && self.colon.is_none() && self.thumbs.bytes() > 0 {
            self.thumbs.clear(window, cx);
        }
        #[cfg(feature = "gui-shot")]
        self.stage_picker(window, cx);

        let dock = div()
            .flex_none()
            .px(px(20.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .key_context("Composer")
            .on_action(cx.listener(|this, _: &crate::gui::AttachFile, _, cx| this.attach_files(cx)))
            .on_action(cx.listener(|this, _: &PickEmoji, window, cx| this.toggle_picker(Tab::Emoji, window, cx)))
            .on_action(cx.listener(|this, _: &PickGifs, window, cx| this.toggle_picker(Tab::Gifs, window, cx)))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                let paths = event.drag(cx).paths().to_vec();
                this.drag_moved(event.event.position, event.bounds, &paths, cx);
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.dropped(paths, cx)));
        let Some(target) = self.target.clone() else {
            return dock;
        };
        let body = match target.locked() {
            Some(locked) => self.bar(locked, target.key.network, cx),
            None => self.card(focus.is_focused(window), window, cx),
        };
        let anchors = self.anchors.clone();
        // The row sits over the card's text, not its edge.
        dock.child(div().pl(px(13.)).child(self.typing.clone()))
            .child(div().relative().child(body).children(self.popover(cx)))
            .children(self.caption_line(&target, cx))
            .children(self.picker_layer(window, cx))
            .children(self.send_menu_layer(window, cx))
            .children(self.drop_layer(cx))
            .child(canvas(move |bounds, _, _| anchors.dock.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
    }
}

impl Shell {
    fn composer_in(&self, thread: bool, cx: &mut Context<Self>) -> Entity<Composer> {
        let slot = if thread { &self.composer_state.thread } else { &self.composer_state.main };
        slot.get_or_init(|| {
            let (shell, hub) = (cx.weak_entity(), self.people_state.hub());
            cx.new(|cx| Composer::new(shell, hub, thread, cx))
        })
        .clone()
    }

    fn can_of(&self, conversation: &Key) -> Capabilities {
        self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&conversation.account())).map(|account| account.can).unwrap_or_default()
    }

    /// The composer dock of the open conversation.
    pub(super) fn composer(&self, open: &Conversation, link: Link, cx: &mut Context<Self>) -> Div {
        self.people_state.attach(cx);
        self.people_state.hub().watch_conversation(Some(&open.key), cx);
        #[cfg(feature = "gui-shot")]
        let (link, kind) = super::people::shot::locked(link, open.kind);
        #[cfg(feature = "gui-shot")]
        let link = if staged("offline") || staged("waiting") { Link::Offline } else { link };
        #[cfg(not(feature = "gui-shot"))]
        let kind = open.kind;
        let target = Target {
            key: open.key.clone(),
            title: open.title.clone(),
            kind,
            link,
            space: open.space.clone(),
            read_only: open.read_only,
            can: self.can_of(&open.key),
        };
        let composer = self.composer_in(false, cx);
        composer.update(cx, |composer, cx| composer.aim(target, &open.draft, cx));
        #[cfg(feature = "gui-shot")]
        self.stage_composer(&composer, cx);
        let dock = div().flex_none();
        #[cfg(feature = "gui-shot")]
        let dock = dock.children(self.stage_held(&open.key, cx));
        let dock = dock.child(composer).children(self.floating(cx));
        #[cfg(feature = "gui-shot")]
        let dock = dock.children(super::people::shot::sheet());
        self.sweep(cx);
        dock
    }

    /// The same dock for the thread pane. A message typed here goes to the thread.
    #[allow(dead_code)]
    pub(super) fn thread_composer(&self, thread: &Key, title: &str, link: Link, cx: &mut Context<Self>) -> Div {
        let target = Target {
            key: thread.clone(),
            title: title.to_owned(),
            kind: ConversationKind::Thread,
            link,
            space: None,
            read_only: false,
            can: self.can_of(thread),
        };
        let composer = self.composer_in(true, cx);
        // A thread's row is not handed over, so its draft is the one this window remembers.
        composer.update(cx, |composer, cx| composer.aim(target, "", cx));
        div().flex_none().child(composer)
    }

    // Hooks for the conversation and the window.

    /// Puts one of my messages in the open conversation's composer to be edited.
    #[allow(dead_code)]
    pub(super) fn composer_edit(&mut self, message: &Message, cx: &mut Context<Self>) {
        self.composer_in(false, cx).update(cx, |composer, cx| composer.edit(message, cx));
    }

    /// Makes the next message from the open conversation's composer a reply to `message`.
    /// Does nothing on Slack, where a reply is a thread.
    #[allow(dead_code)]
    pub(super) fn composer_reply(&mut self, message: &Message, cx: &mut Context<Self>) {
        self.composer_in(false, cx).update(cx, |composer, cx| composer.reply_to(message, cx));
    }

    /// The id of the message being edited, so its row can be tinted.
    #[allow(dead_code)]
    /// True while the open conversation's composer is answering a message.
    pub(super) fn composer_replying(&self, cx: &App) -> bool {
        self.composer_state.main.get().is_some_and(|main| main.read(cx).reply.is_some())
    }

    pub(super) fn composer_editing(&self, cx: &App) -> Option<String> {
        let composer = self.composer_state.main.get()?.read(cx);
        composer.editing.as_ref().map(|editing| editing.id.clone())
    }

    /// Focuses the open conversation's composer.
    #[allow(dead_code)]
    pub(super) fn composer_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = self.composer_state.main.get().filter(|composer| composer.read(cx).can_type()) else {
            return;
        };
        window.focus(&composer.focus_handle(cx), cx);
    }

    /// A key pressed with no field focused: the composer takes the focus and the text.
    #[allow(dead_code)]
    pub(super) fn composer_type(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = self.composer_state.main.get().filter(|composer| composer.read(cx).can_type()).cloned() else {
            return;
        };
        window.focus(&composer.focus_handle(cx), cx);
        composer.update(cx, |composer, cx| composer.type_text(text, cx));
    }

    /// How far a pending message's files have gone: (bytes sent, bytes in all), from the last
    /// `Change::Upload`. `None` before the first, and once the message has settled.
    #[allow(dead_code)]
    pub(super) fn composer_upload(&self, conversation: &Key, client_id: &str) -> Option<(u64, u64)> {
        let uploads = &self.composer_state.uploads;
        uploads.iter().rev().find(|((key, id), _)| key == conversation && id == client_id).map(|(_, progress)| *progress)
    }

    /// Stops a pending message's upload. It ends `Failed` ("Cancelled"), with Retry.
    #[allow(dead_code)]
    pub(super) fn composer_cancel_upload(&mut self, conversation: &Key, client_id: &str, cx: &mut Context<Self>) {
        if !live() {
            return;
        }
        let request = Request::CancelSend { conversation: conversation.clone(), client_id: client_id.to_owned() };
        self.unsent(request, None, cx);
    }

    // The hold: messages sent while offline.

    fn link_of(&self, conversation: &Key) -> Option<Link> {
        self.snapshot.as_ref().map(|snapshot| snapshot.account_link(conversation))
    }

    /// Starts a message's 60 s wait for its network. False when too much is waiting already.
    fn hold_send(&mut self, held: Held, cx: &mut Context<Self>) -> bool {
        let taken = self.composer_state.hold.hold(held, Instant::now());
        // chatd refused it a moment ago, but the link may have come back since.
        self.composer_links_changed(cx);
        taken
    }

    /// The links changed: what waits for an account that is connected now is sent, and what
    /// waited for one that signed out is gone. The shell calls this from `apply_status`.
    pub(super) fn composer_links_changed(&mut self, cx: &mut Context<Self>) {
        // With chatd away nothing is known; what waits keeps waiting until its time is up.
        if let Some(snapshot) = &self.snapshot {
            let ready = self.composer_state.hold.linked(Instant::now(), |conversation| snapshot.account_link(conversation));
            for held in ready {
                self.send_held(held, cx);
            }
        }
        self.hold_schedule(cx);
        cx.notify();
    }

    /// Keeps the one expiry timer in step with the hold: none when nothing waits.
    fn hold_schedule(&mut self, cx: &mut Context<Self>) {
        let wait = self.composer_state.hold.next_expiry(Instant::now());
        self.composer_state.expiry = wait.map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |shell, cx| {
                    shell.composer_state.hold.expire(Instant::now());
                    shell.hold_schedule(cx);
                    cx.notify();
                });
            })
        });
    }

    /// Sends a held message under the id it has had since Return, so it cannot go twice.
    fn send_held(&mut self, held: Held, cx: &mut Context<Self>) {
        if !live() {
            return;
        }
        let request = Request::Send {
            conversation: held.conversation.clone(),
            client_id: held.client_id.clone(),
            text: held.wire.clone(),
            reply_to: held.reply_to.clone(),
            with: protocol::Outgoing::default(),
        };
        let shell = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let _ = shell.update(cx, |shell, cx| {
                let (key, id) = (&held.conversation, held.client_id.as_str());
                let hold = &mut shell.composer_state.hold;
                match answer {
                    Ok(Response::Sent { .. }) => {
                        hold.sent(key, id);
                        shell.send_started(key, id, cx);
                    }
                    Ok(Response::Error { code: ErrorCode::Offline, .. }) => hold.refused(key, id, true, "", Instant::now()),
                    Ok(Response::Error { detail, .. }) => hold.refused(key, id, false, &detail, Instant::now()),
                    Ok(_) => hold.refused(key, id, false, "tinyclient's background process answered something it should not have.", Instant::now()),
                    Err(_) => hold.refused(key, id, false, NOT_ANSWERING, Instant::now()),
                }
                shell.hold_schedule(cx);
                cx.notify();
            });
        });
    }

    /// The messages of a conversation that wait for its network, or gave up waiting, oldest
    /// first. chatd does not have them: the conversation draws each as a pending row of ours.
    #[allow(dead_code)]
    pub(super) fn held_sends(&self, conversation: &Key) -> Vec<HeldSend> {
        let held = self.composer_state.hold.of(conversation);
        held.map(|held| HeldSend {
            client_id: held.client_id.clone(),
            text: held.text.clone(),
            since: held.since,
            failed: held.failed().is_some(),
        })
        .collect()
    }

    /// What a held row shows under its body: that it waits for the network, or, once it has
    /// given up, why, with Retry and Delete.
    #[allow(dead_code)]
    pub(super) fn held_line(&self, conversation: &Key, held: &HeldSend, cx: &mut Context<Self>) -> Div {
        let reason = self.composer_state.hold.of(conversation).find(|entry| entry.client_id == held.client_id).and_then(Held::failed);
        match reason {
            Some(reason) => self.failed_line(conversation, &held.client_id, reason, cx),
            None => div()
                .mt(px(2.))
                .flex()
                .items_center()
                .gap(px(6.))
                .type_small()
                .text_color(theme::text_faint())
                .child(glyph("icons/wifi-off.svg", 12., theme::warning()))
                .child(format!("Waiting for {}…", conversation.network.label())),
        }
    }

    // What became of a send.

    /// chatd has stored the message as pending; it says so itself in a `Messages` change, which
    /// is what brings the row. From here its outcome is followed for the sidebar's mark.
    fn send_started(&mut self, conversation: &Key, client_id: &str, cx: &mut Context<Self>) {
        self.composer_state.flight.started(conversation, client_id);
        cx.notify();
    }

    /// A message sent from this window did not go and nothing has been done about it: one the
    /// hold gave up on, or one the network refused after chatd took it.
    // A send chatd refuses outright is not marked: its text is still in the field with the
    // reason under it, and there is no row to retry or delete that would ever clear the mark.
    #[allow(dead_code)]
    pub(super) fn send_failed_in(&self, conversation: &Key) -> bool {
        self.composer_state.hold.failed_in(conversation) || self.composer_state.flight.failed_in(conversation)
    }

    /// Every change chatd reports. Uploads are kept for the pending rows; a file the picker
    /// asked for goes to it; and where a send is on its way, messages changing is one read of
    /// the newest few, to see whether it went.
    pub(super) fn composer_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        match change {
            Change::Upload { conversation, client_id, sent, total } => {
                let uploads = &mut self.composer_state.uploads;
                uploads.retain(|((key, id), _)| !(key == conversation && id == client_id));
                uploads.push_back(((conversation.clone(), client_id.clone()), (*sent, *total)));
                if uploads.len() > UPLOADS {
                    uploads.pop_front();
                }
                cx.notify();
                return;
            }
            Change::Media { file, .. } => {
                for composer in [self.composer_state.main.get(), self.composer_state.thread.get()].into_iter().flatten() {
                    let (composer, file) = (composer.clone(), file.clone());
                    // Its own update may be running: it hears of the file once that is over.
                    cx.defer(move |cx| composer.update(cx, |composer, cx| composer.media_landed(&file, cx)));
                }
                return;
            }
            _ => {}
        }
        let Change::Messages { conversation } = change else {
            return;
        };
        if !self.composer_state.flight.in_flight(conversation) {
            return;
        }
        let key = conversation.clone();
        let request = Request::Messages { conversation: key.clone(), page: Page::Newest, limit: SETTLE };
        let shell = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Messages { messages, .. }) = answer else {
                return;
            };
            let outcome_of = |client_id: &str| {
                let found = messages.iter().find(|message| message.client_id.as_deref() == Some(client_id) || message.id == client_id);
                match found.map(|message| &message.state) {
                    None => Outcome::Gone,
                    Some(SendState::Pending) => Outcome::Pending,
                    Some(SendState::Sent) => Outcome::Sent,
                    Some(SendState::Failed { .. }) => Outcome::Failed,
                }
            };
            let _ = shell.update(cx, |shell, cx| {
                // A send that went no longer needs the files made for it, nor its progress.
                let state = &mut shell.composer_state;
                let settled: Vec<String> = state
                    .outbox
                    .iter()
                    .filter(|(held, id, _)| *held == key && matches!(outcome_of(id), Outcome::Sent | Outcome::Gone))
                    .map(|(_, id, _)| id.clone())
                    .collect();
                for id in &settled {
                    if let Some(at) = state.outbox.iter().position(|(held, held_id, _)| *held == key && held_id == id) {
                        let (_, _, made) = state.outbox.remove(at);
                        delete_later(made);
                    }
                }
                state.uploads.retain(|((held, id), _)| *held != key || outcome_of(id) == Outcome::Pending);
                if state.flight.settle(&key, outcome_of) || !settled.is_empty() {
                    cx.notify();
                }
            });
        });
    }

    /// Sends a failed message again. One chatd holds goes back to pending; one the hold gave
    /// up on goes now if its network is there, and otherwise waits another 60 s.
    pub(super) fn retry_send(&mut self, conversation: Key, client_id: String, cx: &mut Context<Self>) {
        if !live() {
            return;
        }
        if self.composer_state.hold.has(&conversation, &client_id) {
            let connected = self.link_of(&conversation) == Some(Link::Connected);
            if let Some(held) = self.composer_state.hold.retry(&conversation, &client_id, connected, Instant::now()) {
                self.send_held(held, cx);
            }
            self.hold_schedule(cx);
            cx.notify();
            return;
        }
        self.composer_state.flight.started(&conversation, &client_id);
        self.unsent(Request::Retry { conversation, client_id }, None, cx);
    }

    /// Removes a message that never left. Local: nobody else ever saw it. Files the shell
    /// made for it go too.
    pub(super) fn discard_send(&mut self, conversation: Key, client_id: String, cx: &mut Context<Self>) {
        if self.composer_state.hold.discard(&conversation, &client_id) {
            self.hold_schedule(cx);
            cx.notify();
            return;
        }
        if !live() {
            return;
        }
        if let Some(at) = self.composer_state.outbox.iter().position(|(key, id, _)| *key == conversation && *id == client_id) {
            let (_, _, made) = self.composer_state.outbox.remove(at);
            delete_later(made);
        }
        self.composer_state.flight.discarded(&conversation, &client_id);
        let row = conversation.clone();
        self.unsent(Request::Discard { conversation, client_id }, Some(row), cx);
    }

    /// `refresh` is the conversation whose row to read again when the request went through:
    /// chatd announces a discarded message to nobody, and the sidebar's preview was it.
    fn unsent(&mut self, request: Request, refresh: Option<Key>, cx: &mut Context<Self>) {
        let shell = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(crate::client::trouble_words(&err)),
            };
            let _ = shell.update(cx, |shell, cx| {
                if let (None, Some(key)) = (&refused, refresh) {
                    shell.refresh_row(key, cx);
                }
                shell.refused = refused;
                cx.notify();
            });
        });
    }

    /// What a message that did not go shows under its body: the network's reason, then Retry
    /// and Delete. A pending message shows nothing here; it is the row that is dimmed.
    #[allow(dead_code)]
    pub(super) fn unsent_line(&self, conversation: &Key, message: &Message, cx: &mut Context<Self>) -> Option<Div> {
        let SendState::Failed { reason } = &message.state else {
            return None;
        };
        let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
        Some(self.failed_line(conversation, &client_id, reason, cx))
    }

    fn failed_line(&self, conversation: &Key, client_id: &str, reason: &str, cx: &mut Context<Self>) -> Div {
        // The id is digits, so it also tells one row's buttons from the next row's.
        let row = client_id.bytes().fold(0u64, |hash, byte| hash.wrapping_mul(31).wrapping_add(byte as u64));
        let small = |id: &'static str, label: &'static str| {
            button((id, row), ButtonKind::Secondary, label)
                .h(px(22.))
                .px(px(8.))
                .type_small()
        };
        let (retry, discard) = (
            (conversation.clone(), client_id.to_owned()),
            (conversation.clone(), client_id.to_owned()),
        );
        div()
            .mt(px(2.))
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::danger())
            .child(glyph("icons/circle-alert.svg", 14., theme::danger()))
            .child(div().min_w_0().truncate().child(format!("Not sent. {reason}")))
            .child(small("retry", "Retry").ml(px(2.)).on_click(cx.listener(move |this, _, _, cx| {
                this.retry_send(retry.0.clone(), retry.1.clone(), cx);
            })))
            .child(small("discard", "Delete").on_click(cx.listener(move |this, _, _, cx| {
                this.discard_send(discard.0.clone(), discard.1.clone(), cx);
            })))
    }

    /// `--composer waiting`: the two lines a held row shows, drawn over the dock so they can be
    /// looked at before the conversation draws held rows. Nothing is held and nothing is sent.
    #[cfg(feature = "gui-shot")]
    fn stage_held(&self, conversation: &Key, cx: &mut Context<Self>) -> Option<Div> {
        if !staged("waiting") {
            return None;
        }
        let waiting = HeldSend { client_id: "1".into(), text: "Back in ten".into(), since: now(), failed: false };
        let lines = div().px(px(33.)).pb(px(4.)).flex().flex_col().gap(px(2.)).type_body().text_color(theme::text());
        Some(
            lines
                .child(div().opacity(0.55).child(waiting.text.clone()))
                .child(self.held_line(conversation, &waiting, cx))
                .child(div().mt(px(8.)).text_color(theme::text_soft()).child("On it, give me five minutes."))
                .child(self.failed_line(conversation, "2", &format!("{} did not come back.", conversation.network.label()), cx)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ids_are_digits_and_never_repeat() {
        let ids: Vec<String> = (0..500).map(|_| client_id()).collect();
        assert!(ids.iter().all(|id| !id.is_empty() && id.len() <= 25 && id.bytes().all(|b| b.is_ascii_digit())));
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn typing_is_shared_on_slack_and_kept_private_on_discord_until_the_user_says() {
        let account = |network| protocol::AccountKey { network, account: "T1".into() };
        assert!(sends_typing(&account(Network::Slack)));
        assert!(!sends_typing(&account(Network::Discord)));
        assert!(typing_interval(Network::Discord) < Duration::from_secs(10));
    }

    #[test]
    fn the_placeholder_names_where_the_message_goes() {
        assert_eq!(placeholder("#platform", false), "Message #platform");
        assert_eq!(placeholder("#platform", true), "Reply in thread");
    }

    fn target(link: Link, kind: ConversationKind, read_only: bool) -> Target {
        let key = Key { network: Network::Slack, account: "T1".into(), id: "C1".into() };
        Target { key, title: "#announcements".into(), kind, link, space: None, read_only, can: Capabilities::default() }
    }

    #[test]
    fn offline_leaves_the_composer_usable_and_read_only_replaces_it() {
        assert!(target(Link::Offline, ConversationKind::Channel, false).locked().is_none());
        assert!(target(Link::Connected, ConversationKind::Direct, false).locked().is_none());
        assert!(matches!(target(Link::SignedOut, ConversationKind::Channel, true).locked(), Some(Locked::SignedOut)));
        assert!(matches!(target(Link::Expired, ConversationKind::Channel, false).locked(), Some(Locked::Expired)));
        let Some(Locked::ReadOnly(sentence)) = target(Link::Connected, ConversationKind::Channel, true).locked() else {
            panic!("a read-only channel takes no message");
        };
        assert_eq!(sentence, "You can read #announcements but not post in it.");
        assert!(matches!(target(Link::Offline, ConversationKind::Forum, false).locked(), Some(Locked::ReadOnly(_))));
    }

    #[test]
    fn only_my_own_delivered_messages_can_be_edited_and_slack_has_no_reply_mode() {
        let mut mine = Message::plain("1", "me", "on it", 0, true);
        assert!(editable(&mine));
        assert!(!editable(&Message::plain("2", "Mara", "on it", 0, false)));
        mine.state = SendState::Pending;
        assert!(!editable(&mine));
        mine.state = SendState::Sent;
        mine.kind = MessageKind::System;
        assert!(!editable(&mine));
        assert!(!replies_inline(Network::Slack));
        assert!(replies_inline(Network::Discord) && replies_inline(Network::Telegram));
    }

    #[test]
    fn the_reply_bar_quotes_one_line_and_a_draft_of_spaces_is_no_draft() {
        let message = Message::plain("1", "Mara", "\n  Migration finished clean.  \nResuming consumers now.", 0, false);
        assert_eq!(snippet(&message), "Migration finished clean.");
        assert_eq!(snippet(&Message::plain("2", "Mara", "", 0, false)), "");
        assert_eq!(draft_text("  \n "), "");
        assert_eq!(draft_text(" kept as typed "), " kept as typed ");
        assert_eq!(draft_text(&"é".repeat(DRAFT_LIMIT + 10)).chars().count(), DRAFT_LIMIT);
    }

    /// Run by hand against a scratch demo daemon (`TINYCLIENT_DIR` set, `chatd --seed-demo`
    /// running): `TC_DRIVE=1 cargo test -p tinyclient --features gui -- --ignored drives`.
    /// Stages a pasted picture and a file the way the composer does, refuses one too big for
    /// the account before any request, sends the rest, and follows the pending row to the end.
    #[test]
    #[ignore]
    fn drives_a_send_with_files_against_a_running_demo() {
        if std::env::var_os("TC_DRIVE").is_none() || std::env::var_os("TINYCLIENT_DIR").is_none() {
            return;
        }
        let Ok(Response::Status { accounts, .. }) = ask(&Request::Status) else { panic!("a demo chatd answers") };
        let discord = accounts.iter().find(|account| account.key.network == Network::Discord).expect("a Discord account");
        let can = discord.can;
        let inbox = Request::Inbox { network: Some(Network::Discord), filter: protocol::InboxFilter::All, offset: 0, limit: 50 };
        let Ok(Response::Conversations { conversations, .. }) = ask(&inbox) else { panic!("an inbox") };
        let open = conversations.iter().find(|c| c.key.network == Network::Discord && !c.read_only).expect("a Discord conversation");

        // A pasted screenshot goes to the outbox; a file from disk stays where it is.
        let outbox = attach::outbox();
        let pasted = attach::write_pasted(&outbox, &vec![7u8; 300_000], "png", SystemTime::now()).expect("written");
        let notes = std::env::temp_dir().join(format!("tc-drive-notes-{}.pdf", std::process::id()));
        std::fs::write(&notes, vec![1u8; 700_000]).unwrap();
        let huge = std::env::temp_dir().join(format!("tc-drive-huge-{}.mov", std::process::id()));
        std::fs::File::create(&huge).unwrap().set_len(can.max_file_bytes + 1).unwrap();

        let mut tray = Vec::new();
        let chips: Vec<Staged> = [&pasted, &notes, &huge]
            .into_iter()
            .enumerate()
            .map(|(id, path)| Staged::new(id as u64, path.clone(), attach::look(path), &can, Network::Discord))
            .collect();
        assert_eq!(attach::admit(&mut tray, chips, &can), 0);
        assert!(tray[2].refused.as_deref().is_some_and(|why| why.starts_with("Too big for Discord")), "{:?}", tray[2].refused);
        assert!(attach::blocker(&tray).is_some(), "Return is held while a chip is refused");
        tray.pop();
        assert!(attach::blocker(&tray).is_none());

        let Ok(Response::Changes { seq, .. }) = ask(&Request::Wait { since: 0 }) else { panic!("changes") };
        let client = client_id();
        let with = protocol::Outgoing { files: tray.iter().filter_map(Staged::outgoing).collect(), ..protocol::Outgoing::default() };
        let send = Request::Send { conversation: open.key.clone(), client_id: client.clone(), text: "Last night on camera 2".into(), reply_to: None, with };
        let Ok(Response::Sent { message }) = ask(&send) else { panic!("stored as pending") };
        assert_eq!(message.state, SendState::Pending);
        assert_eq!(message.attachments.len(), 2, "the pending row lists its files at once");

        let mut since = seq;
        let mut progress = Vec::new();
        let started = Instant::now();
        let mut settled = None;
        while started.elapsed() < Duration::from_secs(20) && settled.is_none() {
            let Ok(Response::Changes { seq, changes, .. }) = ask(&Request::Wait { since }) else { panic!("changes") };
            since = seq;
            for change in changes {
                if let Change::Upload { client_id, sent, total, .. } = change {
                    if client_id == client {
                        progress.push((sent, total));
                    }
                }
            }
            let page = Request::Messages { conversation: open.key.clone(), page: Page::Newest, limit: 10 };
            if let Ok(Response::Messages { messages, .. }) = ask(&page) {
                settled = messages.into_iter().find(|m| m.client_id.as_deref() == Some(client.as_str()) && m.state != SendState::Pending);
            }
        }
        let sent = settled.expect("the send settled");
        assert_eq!(sent.state, SendState::Sent);
        assert_eq!(sent.attachments.len(), 2);
        assert!(progress.len() >= 2, "{progress:?}");
        assert!(progress.windows(2).all(|pair| pair[0].0 <= pair[1].0), "{progress:?}");
        assert_eq!(progress.last().map(|(sent, total)| sent == total), Some(true));
        eprintln!("upload progress: {progress:?}");
        // Settled: the files the shell made for it may go.
        attach::delete_made(&outbox, std::slice::from_ref(&pasted));
        assert!(!pasted.exists());
        let _ = std::fs::remove_file(&notes);
        let _ = std::fs::remove_file(&huge);
    }
}
