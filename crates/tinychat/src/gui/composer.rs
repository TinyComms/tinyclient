//! The composer: the draft, sending it, editing and replying, the typing row above it, and what
//! replaces it when nothing can be sent. One `Composer` per place a message is written: the
//! conversation, and the thread pane.
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
//! - `unsent_line(conversation, message, cx)` is what a failed row shows under its body: the
//!   reason, Retry and Delete. `None` for a message that went.
//! - `held_sends(conversation)` are the messages sent while the account was offline, which
//!   chatd does not store: draw each as a pending row with `held_line(conversation, held, cx)`
//!   under its body. They wait 60 s for the link (`hold`), then fail in place.
//! - `send_failed_in(conversation)`: a message sent from here did not go and is unresolved.
//! - `composer_links_changed(cx)` and `composer_changed(change, cx)` are called by the shell.

mod complete;
pub(super) mod edit;
mod hold;
mod mention;
mod typing;

pub use typing::preview as typing_preview;

use super::components::{button, glyph, ButtonKind};
use super::input::{InputEvent, InputKey, InputList, InputPaste, TextInput};
use super::people::{ask_later, Hub};
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use crate::view::now;
use complete::List;
use gpui::{
    deferred, div, prelude::*, px, relative, App, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, Rgba,
    Task, WeakEntity, Window,
};
use hold::{Flight, Held, Hold, Outcome};
use mention::Token;
use protocol::{
    Change, Conversation, ConversationKind, ErrorCode, Key, Link, Message, MessageKind, Network, Page, Person, Request,
    Response, SendState, DRAFT_LIMIT,
};
use std::cell::OnceCell;
use std::collections::{HashMap, VecDeque};
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
const NOT_ANSWERING: &str = "tinychat's background process is not answering.";

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

/// `--composer reply|edit|complete|offline|waiting|refused`: the state a picture is taken of.
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

/// Whether others see that we are typing. Constants until there are settings.
fn sends_typing(network: Network) -> bool {
    !matches!(network, Network::Discord)
}

/// How often a network needs to hear that we are still typing for its line to stay up.
fn typing_interval(network: Network) -> Duration {
    Duration::from_secs(match network {
        Network::Discord => 8,
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

/// Where the composer writes to.
#[derive(Clone, PartialEq)]
struct Target {
    key: Key,
    title: String,
    kind: ConversationKind,
    link: Link,
    /// The Space it is in, for `#` completion. A thread pane is not told.
    space: Option<String>,
    read_only: bool,
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
}

/// What was typed and the mentions in it.
#[derive(Clone, Default)]
struct Draft {
    text: String,
    tokens: Vec<Token>,
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
}

impl Composer {
    fn new(shell: WeakEntity<Shell>, hub: Hub, thread: bool, cx: &mut Context<Self>) -> Composer {
        let input = cx.new(|cx| TextInput::multiline(placeholder("", thread), cx));
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.send(cx),
            InputEvent::Changed => this.typed(cx),
        })
        .detach();
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
            InputList::Complete => {
                let selected = this.list.as_ref().map_or(0, |list| list.selected);
                this.complete(selected, cx);
            }
        })
        .detach();
        cx.subscribe(&input, |this, _, _: &InputPaste, cx| this.refuse_files(cx)).detach();
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
            let draft = self.drafts.get(&target.key).cloned().unwrap_or_else(|| Draft { text: stored.to_owned(), tokens: Vec::new() });
            self.saved = draft_text(&draft.text);
            self.set_field(&draft, cx);
            self.offered = stored.to_owned();
            self.touched = false;
            self.reply = None;
            self.caption = None;
            self.sending = false;
            self.focus_next = true;
        } else if self.offered != stored {
            // The row was read after the conversation opened: its draft is still wanted if
            // nothing has been typed in the meantime.
            let late = !self.touched && self.editing.is_none() && self.input.read(cx).text().is_empty() && !stored.is_empty();
            self.offered = stored.to_owned();
            if late {
                self.saved = stored.to_owned();
                self.set_field(&Draft { text: stored.to_owned(), tokens: Vec::new() }, cx);
            }
        }
        if moved || renamed {
            let hint = placeholder(&target.title, self.thread);
            self.input.update(cx, |input, cx| {
                input.set_placeholder(hint);
                cx.notify();
            });
        }
        self.target = Some(target);
        cx.notify();
    }

    /// Leaving a conversation: an edit ends, and the draft is saved and kept for the way back.
    fn leave(&mut self, cx: &mut Context<Self>) {
        self.end_edit(cx);
        self.save_draft(cx);
        self.draft_timer = None;
        let Some(old) = self.target.as_ref().map(|old| old.key.clone()) else {
            return;
        };
        let draft = Draft { text: draft_text(self.input.read(cx).text()), tokens: self.tokens.clone() };
        self.remember(old, draft);
    }

    fn remember(&mut self, key: Key, draft: Draft) {
        if self.drafts.insert(key.clone(), draft).is_none() {
            self.draft_order.push_back(key);
            if self.draft_order.len() > DRAFTS {
                if let Some(oldest) = self.draft_order.pop_front() {
                    self.drafts.remove(&oldest);
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
        if target.link != Link::Connected || !sends_typing(network) || self.input.read(cx).text().trim().is_empty() {
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
        self.close_list(cx);
        // A list with no row was never on screen, so Escape is not spent on it.
        if let Some(list) = list.filter(|list| !list.rows.is_empty()) {
            self.dismissed = Some(list.trigger.start);
        } else if self.editing.is_some() {
            self.end_edit(cx);
        } else if self.reply.take().is_none() {
            self.blur_next = true;
        }
        cx.notify();
    }

    // Sending.

    /// Return. chatd answers as soon as the message is stored as pending, and the row comes
    /// from that; the text leaves the field only then, so a refused send loses nothing. When
    /// the account is offline chatd stores nothing, and the shell holds the message instead.
    pub fn send(&mut self, cx: &mut Context<Self>) {
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
        let field = self.input.read(cx).text().to_owned();
        if field.trim().is_empty() || !live() {
            return;
        }
        let out = Outgoing {
            key: target.key.clone(),
            shown: field.trim().to_owned(),
            wire: mention::wire(&field, &self.tokens, target.key.network).trim().to_owned(),
            field,
            client_id: client_id(),
            reply_to: self.reply.as_ref().map(|reply| reply.id.clone()),
        };
        let request = Request::Send {
            conversation: out.key.clone(),
            client_id: out.client_id.clone(),
            text: out.wire.clone(),
            reply_to: out.reply_to.clone(),
            with: protocol::Outgoing::default(),
        };
        self.sending = true;
        self.told_typing = None;
        cx.notify();
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| this.sent(out, answer, cx));
        });
    }

    fn sent(&mut self, out: Outgoing, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        self.sending = false;
        let refused = match answer {
            Ok(Response::Sent { .. }) => {
                let (key, client_id) = (out.key.clone(), out.client_id.clone());
                let _ = self.shell.update(cx, |shell, cx| shell.send_started(&key, &client_id, cx));
                None
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
            Ok(other) => Some(format!("Not sent: unexpected {other:?}")),
            Err(_) => Some(format!("{NOT_ANSWERING} Not sent.")),
        };
        match refused {
            Some(words) => self.caption = Some((words, Tone::Refused)),
            None => self.went(&out, cx),
        }
        cx.notify();
    }

    /// The message is chatd's or the hold's now: the field empties and the draft is forgotten.
    fn went(&mut self, out: &Outgoing, cx: &mut Context<Self>) {
        let here = self.target.as_ref().is_some_and(|now| now.key == out.key);
        if !here {
            self.remember(out.key.clone(), Draft::default());
            self.store_draft(out.key.clone(), String::new(), cx);
            return;
        }
        self.caption = None;
        self.reply = None;
        // More was typed while chatd answered: that is the next message, and stays.
        if self.input.read(cx).text() == out.field {
            self.set_field(&Draft::default(), cx);
            self.draft_timer = None;
            self.save_draft(cx);
        }
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

    /// Puts one of my messages in the field to be changed. What was being typed is set aside.
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
                (Draft { text: self.input.read(cx).text().to_owned(), tokens: self.tokens.clone() }, self.reply.take())
            }
        };
        let (text, tokens) = mention::source(message);
        self.editing = Some(Editing { id: message.id.clone(), original: text.clone(), stash, reply });
        self.set_field(&Draft { text, tokens }, cx);
        self.caption = None;
        self.focus_next = true;
        cx.notify();
    }

    /// Leaves edit mode, saved or not, and puts back what was being typed.
    fn end_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.set_field(&editing.stash, cx);
        self.reply = editing.reply;
        self.caption = None;
        cx.notify();
    }

    /// Up in an empty field: my newest message here, from the store.
    fn edit_last(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.as_ref().filter(|target| target.locked().is_none()) else {
            return;
        };
        if self.editing.is_some() {
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
            Ok(other) => self.caption = Some((format!("Not saved: unexpected {other:?}"), Tone::Refused)),
            Err(_) => self.caption = Some((format!("{NOT_ANSWERING} Not saved."), Tone::Refused)),
        }
    }

    /// A pasted picture or dropped files. Sending them is not built, and saying nothing would
    /// look like it worked.
    fn refuse_files(&mut self, cx: &mut Context<Self>) {
        self.caption = Some(("Sending files isn't built yet. Nothing was attached.".to_owned(), Tone::Note));
        cx.notify();
    }

    /// Keys pressed with no field focused: they are typed here.
    fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.insert(text, cx));
    }

    fn can_type(&self) -> bool {
        self.target.as_ref().is_some_and(|target| target.locked().is_none())
    }

    // Completion.

    fn close_list(&mut self, cx: &mut Context<Self>) {
        self.list = None;
        self.lookup = None;
        self.input.update(cx, |input, _| input.set_list_open(false));
    }

    fn show_list(&mut self, list: List, cx: &mut Context<Self>) {
        // Return and the arrows are the list's only while it has a row to give.
        let open = !list.rows.is_empty();
        self.list = Some(list);
        self.input.update(cx, |input, _| input.set_list_open(open));
    }

    /// The people the shell already holds who could be meant: those typing here.
    fn known(&self, key: &Key) -> Vec<Person> {
        let typists = self.hub.typists_in(key);
        typists.iter().filter_map(|typist| self.hub.person(&Key { id: typist.id.clone(), ..key.clone() })).collect()
    }

    /// After a keystroke: what the caret is completing, narrowed at once from the rows on show
    /// and asked of the store once the keys rest.
    fn look_up(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let (text, caret) = {
            let input = self.input.read(cx);
            (input.text().to_owned(), input.cursor())
        };
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
        if let Some(list) = &mut self.list {
            list.step(down);
            cx.notify();
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
        Some(
            bar.child(glyph("icons/corner-up-left.svg", 14., theme::accent_text()))
                .child(div().flex_none().child("Replying to"))
                .child(strong(reply.name.clone()))
                .child(faint(reply.snippet.clone()))
                .child(close),
        )
    }

    fn card(&self, focused: bool, cx: &mut Context<Self>) -> Div {
        let ready = !self.sending && !self.input.read(cx).text().trim().is_empty();
        let focus = self.input.focus_handle(cx);
        let send = if self.editing.is_some() { "icons/check.svg" } else { "icons/send-horizontal.svg" };
        let row = div()
            .min_h(px(CARD - 2.))
            .pl(px(12.))
            .pr(px(5.))
            .py(px(5.))
            .flex()
            .items_end()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .py(px(4.))
                    .flex()
                    .type_body()
                    .text_color(theme::text())
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .id("send")
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
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.send(cx)))
                    .child(glyph(send, 16., if ready { theme::on_accent() } else { theme::text_disabled() })),
            );
        div()
            .min_h(px(CARD))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::raised())
            .border_1()
            .border_color(if focused { theme::focus() } else { theme::border() })
            .flex()
            .flex_col()
            .cursor_text()
            // A click anywhere on the card is a click into the field.
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| window.focus(&focus, cx))
            .children(self.mode_bar(cx))
            .child(row)
    }

    /// The one line under the card: why the last thing did not happen, else that the network
    /// is away and what Return does meanwhile.
    fn caption_line(&self, target: &Target) -> Option<Div> {
        let offline = || {
            let name = target.key.network.label();
            (format!("{name} is offline. A message sent now waits up to 60 seconds for it to come back."), Tone::Offline)
        };
        let unlocked = target.locked().is_none();
        let (words, tone) = self.caption.clone().or_else(|| (unlocked && target.link == Link::Offline).then(offline))?;
        let (icon, color): (_, Rgba) = match tone {
            Tone::Offline => ("icons/wifi-off.svg", theme::warning()),
            Tone::Refused => ("icons/circle-alert.svg", theme::danger()),
            Tone::Note => ("icons/info.svg", theme::text_soft()),
        };
        Some(
            div()
                .mt(px(6.))
                .px(px(13.))
                .flex()
                .items_center()
                .gap(px(6.))
                .type_small()
                .text_color(color)
                .child(glyph(icon, 12., color))
                .child(div().min_w_0().truncate().child(words)),
        )
    }

    /// The completion list, 6 pt above the card and over the messages.
    fn popover(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let list = self.list.as_ref().filter(|list| !list.rows.is_empty())?;
        let this = cx.weak_entity();
        let pick = move |row: usize, _: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |this, cx| this.complete(row, cx));
        };
        let drawn = complete::draw(list, &self.hub, pick, cx);
        Some(deferred(div().absolute().left_0().bottom(relative(1.)).pb(px(6.)).child(drawn)).with_priority(1))
    }

    #[cfg(feature = "gui-shot")]
    pub fn stage(&mut self, text: Option<String>, typists: Option<Vec<String>>, refused: bool, cx: &mut Context<Self>) {
        if let Some(text) = text {
            self.set_field(&Draft { text, tokens: Vec::new() }, cx);
        }
        self.typing.update(cx, |typing, _| typing.staged = typists);
        if refused {
            self.caption = Some(("Not sent. You can't post in this channel.".to_owned(), Tone::Refused));
        }
        self.stage_flag(cx);
        cx.notify();
    }

    /// The states `--composer` names. Nothing here asks chatd for anything.
    #[cfg(feature = "gui-shot")]
    fn stage_flag(&mut self, cx: &mut Context<Self>) {
        let field = |text: &str, tokens: Vec<Token>| Draft { text: text.to_owned(), tokens };
        if staged("reply") {
            self.reply = Some(Reply {
                id: "1".into(),
                name: "Mara Okafor".into(),
                snippet: "Migration finished clean. Resuming consumers now.".into(),
            });
            self.set_field(&field("Nice, lag looks flat on my side too", Vec::new()), cx);
        } else if staged("edit") {
            let original = "Sounds good. I'll keep the consumers paused until it finishes.";
            self.editing = Some(Editing { id: "1".into(), original: original.into(), stash: Draft::default(), reply: None });
            self.set_field(&field(original, Vec::new()), cx);
        } else if staged("complete") {
            let Some(key) = self.target.as_ref().map(|target| target.key.clone()) else {
                return;
            };
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
        } else if staged("offline") || staged("waiting") {
            self.set_field(&field("Back in ten", Vec::new()), cx);
        } else if staged("refused") {
            self.refuse_files(cx);
        }
    }
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

        let dock = div()
            .flex_none()
            .px(px(20.))
            .pb(px(16.))
            .flex()
            .flex_col()
            // Dropping stages nothing yet; it must not look as if it did.
            .on_drop(cx.listener(|this, _: &ExternalPaths, _, cx| this.refuse_files(cx)));
        let Some(target) = &self.target else {
            return dock;
        };
        let body = match target.locked() {
            Some(locked) => self.bar(locked, target.key.network, cx),
            None => self.card(focus.is_focused(window), cx),
        };
        // The row sits over the card's text, not its edge.
        dock.child(div().pl(px(13.)).child(self.typing.clone()))
            .child(div().relative().child(body).children(self.popover(cx)))
            .children(self.caption_line(target))
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
                    Ok(other) => hold.refused(key, id, false, &format!("unexpected {other:?}"), Instant::now()),
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

    /// Every change chatd reports. Only messages changing where a send is on its way matter:
    /// that is one read of the newest few, to see whether it went.
    pub(super) fn composer_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
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
                if shell.composer_state.flight.settle(&key, outcome_of) {
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

    /// Removes a message that never left. Local: nobody else ever saw it.
    pub(super) fn discard_send(&mut self, conversation: Key, client_id: String, cx: &mut Context<Self>) {
        if self.composer_state.hold.discard(&conversation, &client_id) {
            self.hold_schedule(cx);
            cx.notify();
            return;
        }
        if !live() {
            return;
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
                Err(err) => Some(err.to_string()),
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
    fn typing_is_shared_on_slack_and_kept_private_on_discord() {
        assert!(sends_typing(Network::Slack));
        assert!(!sends_typing(Network::Discord));
        assert!(typing_interval(Network::Discord) < Duration::from_secs(10));
    }

    #[test]
    fn the_placeholder_names_where_the_message_goes() {
        assert_eq!(placeholder("#platform", false), "Message #platform");
        assert_eq!(placeholder("#platform", true), "Reply in thread");
    }

    fn target(link: Link, kind: ConversationKind, read_only: bool) -> Target {
        let key = Key { network: Network::Slack, account: "T1".into(), id: "C1".into() };
        Target { key, title: "#announcements".into(), kind, link, space: None, read_only }
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
}
