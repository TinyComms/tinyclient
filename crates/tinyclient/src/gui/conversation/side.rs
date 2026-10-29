//! The right pane on the window: `pane`'s rules applied as the open conversation changes, the
//! layout's one slot kept in step with them, and the pane's body.
//!
//! The rules run in an observer of the shell, which GPUI calls after every change and before
//! the next frame is drawn: a switch and the pane that follows it land in the same frame, and
//! the old subject is never drawn beside the new conversation.

use crate::gui::pane::{self, Layer, Showing, Stack, Step};
use super::{Pane, PaneEvent};
use crate::client::ask;
use crate::gui::components::{empty_state, glyph};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::layout::{RightKind, RightPane};
use crate::gui::sidebar::chat;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::time;
use crate::view::{age, now};
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, Focusable, FontWeight, HighlightStyle, MouseButton, SharedString, StyledText, Window};
use protocol::{ConversationKind, Key, Link, Message, MessageHit, Network, Request, Response};
use std::ops::Range;
use std::rc::Rc;

/// The window keeps of the right pane.
#[derive(Default)]
pub struct Side {
    pub stack: Stack,
    pub switches: pane::Switches,
    /// Threads put away on leaving their conversation, by conversation.
    threads: pane::Memory<Rc<Message>>,
    /// The message search's page: what was typed, and what the cache holds for it.
    pub search: Option<SearchPage>,
    /// The search pane's field and its wiring, alive while the pane is.
    search_input: Option<SearchField>,
    /// How often the cache was asked for the search pane, counted across pages: a page remade
    /// starts at nothing, and its number must never meet an older ask's.
    search_asks: u32,
    /// The conversation the stack was last fitted to.
    fitted: Option<Key>,
    /// The kind this view last put in the layout's slot, so another view's change can be told
    /// from its own.
    installed: Option<RightKind>,
    /// The pins of a conversation, once asked for.
    pins: Option<Pins>,
    /// The switches were read from what the window remembers.
    restored: bool,
    /// The window is too narrow for the pane beside the conversation: the layout says so each
    /// frame. Details hides then, and a thread waits to be asked for (07 §4).
    narrow: std::cell::Cell<bool>,
    /// Details was opened by hand at this width, as a one-off that replaces the conversation.
    asked: bool,
    /// The conversation region's width in the last frame, for what its strip can hold.
    room: std::cell::Cell<f32>,
}

/// One message search's page: the scope asked, what was typed, and what the cache holds for it.
#[derive(Default)]
pub struct SearchPage {
    pub scope: Scope,
    pub words: String,
    /// What the cache answered: `None` while the read is outstanding or failed.
    pub hits: Option<Vec<MessageHit>>,
    /// The cache could not be asked.
    pub failed: bool,
    /// The hit the keyboard is on (`(conversation, message_id)`): drawn lit, opened on Enter.
    pub picked: Option<(Key, String)>,
    /// How often the cache was asked, so an older answer never lands over a newer one.
    asked: u32,
    /// The shell's ask count the network ask last carried, so a word or scope change drops a
    /// late answer on the way.
    network_asked: u32,
    /// The words the network ask carried: Enter asks once for them while in flight or after a
    /// successful answer, until they or the scope change.
    network_asked_for: Option<String>,
    /// Where the network is being asked right now, while an ask is in flight.
    pub network_asking: Option<String>,
    /// What the network answered on Enter, with where it was asked. `None` until asked, gone
    /// when the words or the scope change.
    pub network: Option<NetworkFound>,
}

/// What the network said on Enter: where that was, and its hits, or its word for why not.
pub struct NetworkFound {
    pub where_at: String,
    pub result: Result<Vec<MessageHit>, String>,
}

/// How far a search asks: the conversation open now, its Space, or the whole cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Everywhere,
    Conversation,
    Space,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Conversation => "This conversation",
            Scope::Space => "This space",
            Scope::Everywhere => "Everywhere",
        }
    }
}

/// A scope's filters for `SearchMessages`: the one conversation, or the Space's key, or neither.
/// A scope about the conversation falls back to everywhere with nothing open, as the switcher's
/// own search row does.
fn scope_filters(scope: Scope, open: Option<(&Key, Option<&str>)>) -> (Option<Key>, Option<Key>) {
    match (scope, open) {
        (Scope::Conversation, Some((key, _))) => (Some(key.clone()), None),
        (Scope::Space, Some((key, Some(space)))) => {
            (None, Some(Key { network: key.network, account: key.account.clone(), id: space.to_owned() }))
        }
        _ => (None, None),
    }
}

/// The search pane's field and the subscriptions that carry its keys to the shell.
struct SearchField {
    input: Entity<TextInput>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// The page a fresh search opens with: the scope, and the words it starts asked for. The
/// switcher's row seeds its words here, so the first ask carries them without relying on the
/// field's `set_text`, which is not an edit and emits nothing.
fn fresh_page(scope: Scope, words: &str) -> SearchPage {
    SearchPage { scope, words: words.to_owned(), ..SearchPage::default() }
}

/// The user's word for what went wrong asking the network.
fn asked_words(code: protocol::ErrorCode, detail: &str) -> String {
    if detail.contains("does not understand") {
        crate::client::OTHER_ANSWER.to_owned()
    } else if detail == "the network did not answer" {
        "The network did not answer. Try again.".to_owned()
    } else if let Some(network) = detail.strip_suffix(" is not signed in") {
        format!("{network} isn't signed in.")
    } else {
        crate::client::refusal_words(code, detail)
    }
}

impl SearchPage {
    /// The hits as listed: the cache's and the network's own merged by message id — the
    /// network's not doubling what the cache had — newest first, each with whether the cache
    /// said it.
    fn listed(&self) -> Vec<(&MessageHit, bool)> {
        let cache = self.hits.as_deref().unwrap_or_default();
        let mut listed: Vec<(&MessageHit, bool)> = cache.iter().map(|hit| (hit, true)).collect();
        if let Some(Ok(found)) = self.network.as_ref().map(|found| &found.result) {
            let seen: std::collections::HashSet<(&Key, &str)> =
                listed.iter().map(|(hit, _)| (&hit.conversation, hit.message_id.as_str())).collect();
            listed.extend(
                found
                    .iter()
                    .filter(|hit| !seen.contains(&(&hit.conversation, hit.message_id.as_str())))
                    .map(|hit| (hit, false)),
            );
        }
        listed.sort_by_key(|(hit, _)| std::cmp::Reverse(hit.sent));
        listed
    }

    fn reset_network(&mut self) {
        self.network = None;
        self.network_asking = None;
        self.network_asked_for = None;
        self.network_asked = 0;
    }

    /// Moves the keyboard pick up or down in the merged list. Up from row 0 clears the pick so
    /// Enter can ask the network again.
    fn move_pick(&mut self, down: bool) {
        let listed = self.listed();
        if listed.is_empty() {
            return;
        }
        let current = self.picked.as_ref().and_then(|(conv, id)| {
            listed.iter().position(|(hit, _)| &hit.conversation == conv && hit.message_id == *id)
        });
        let next = match (current, down) {
            (Some(at), true) => Some((at + 1).min(listed.len() - 1)),
            (Some(0), false) => None,
            (Some(at), false) => Some(at - 1),
            (None, true) => Some(0),
            (None, false) => Some(listed.len() - 1),
        };
        self.picked = next.map(|ix| {
            let (hit, _) = listed[ix];
            (hit.conversation.clone(), hit.message_id.clone())
        });
    }
}

/// The letters of the query inside a hit's text, for the heavier drawing.
fn matched(text: &str, words: &str) -> Vec<Range<usize>> {
    let (lower, needle) = (text.to_lowercase(), words.to_lowercase());
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(&needle) {
        let (start, end) = (from + at, from + at + needle.len());
        // A lowercased index can sit inside a letter that shrank or grew; such a range is skipped.
        if text.is_char_boundary(start) && text.is_char_boundary(end) {
            out.push(start..end);
        }
        from = end;
    }
    out
}

/// The heavier letters a match is drawn with, as the switcher's rows do.
fn lit() -> HighlightStyle {
    HighlightStyle { color: Some(theme::text().into()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }
}

impl Side {
    /// A Details or Members button pressed in a window too narrow for the pane beside the
    /// conversation: it opens once, in place of the conversation, and a second press puts it
    /// away again. The switches are left as they were. False when the window has room.
    fn reveal_once(&mut self, wanted: pane::ChannelSwitch, open: &pane::Open) -> bool {
        if !self.narrow.get() || self.stack.push.is_some() {
            return false;
        }
        let layer = match (wanted, &open.direct) {
            (_, Some(peer)) => peer.clone().map(Layer::Profile),
            (pane::ChannelSwitch::Members, None) => Some(Layer::Members(open.key.clone())),
            (_, None) => Some(Layer::Details(open.key.clone())),
        };
        let Some(layer) = layer else {
            return false;
        };
        let before = self.stack.clone();
        if self.asked && self.stack.base.as_ref() == Some(&layer) {
            self.stack = Stack { base: pane::derived(self.switches, open), push: None };
            self.asked = false;
        } else {
            self.stack = Stack { base: Some(layer), push: None };
            self.asked = true;
        }
        pane::keep_thread(&before, &self.stack, &mut self.threads);
        true
    }
}

struct Pins {
    conversation: Key,
    messages: Option<Result<Vec<Rc<Message>>, String>>,
}

fn kind_of(layer: &Layer) -> RightKind {
    match layer {
        Layer::Profile(_) | Layer::Details(_) | Layer::Space(_) => RightKind::Info,
        Layer::Members(_) => RightKind::Members,
        Layer::Thread { .. } | Layer::Pinned(_) => RightKind::Thread,
        Layer::Search => RightKind::Search,
    }
}

/// The open conversation as the pane's rules need it.
fn pane_open(open: &protocol::Conversation) -> pane::Open {
    let direct = (open.kind == ConversationKind::Direct)
        .then(|| open.peer.as_ref().map(|peer| Key { id: peer.clone(), ..open.key.clone() }));
    pane::Open { key: open.key.clone(), direct }
}

/// The key a thread is read and posted under: its own id, on its conversation's account.
/// Slack's is `{channel}:{root}` even before the first reply, which is the `thread_ts` a send uses.
fn thread_key(conversation: &Key, root: &Message) -> Option<Key> {
    let id = if let Some(thread) = &root.thread {
        thread.id.clone()
    } else if conversation.network == Network::Slack
        && super::actions::slack_thread_root(root)
        && !conversation.id.contains(':')
        && !root.id.contains(':')
    {
        format!("{}:{}", conversation.id, root.id)
    } else {
        return None;
    };
    Some(Key { id, ..conversation.clone() })
}

/// Whether a thread of `conversation` may be drawn, composer and all, beside `open`. The one
/// gate the pane's body passes before it draws a thread: never beside another conversation.
pub(super) fn thread_belongs(conversation: &Key, open: Option<&Key>) -> bool {
    open == Some(conversation)
}

impl Shell {
    fn open_key(&self) -> Option<&Key> {
        self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).map(|open| &open.key)
    }

    /// Starts applying the pane's rules after every change. Called while drawing the window,
    /// whatever it shows: a pane standing in for the conversation must follow too.
    pub(in crate::gui) fn follow_changes(&self, cx: &mut Context<Self>) {
        self.conversation_state.following.get_or_init(|| {
            #[cfg(feature = "gui-shot")]
            super::shot::steps(cx);
            cx.observe_self(|shell, cx| shell.follow_open(cx))
        });
    }

    /// After any change: a new conversation gets the pane its rules say; otherwise another
    /// view's change to the slot is taken in.
    fn follow_open(&mut self, cx: &mut Context<Self>) {
        self.restore_switches();
        let open = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref());
        let key = open.map(|open| open.key.clone());
        if key == self.conversation_state.side.fitted {
            self.take_in(cx);
            return;
        }
        let to = open.map(pane_open);
        let side = &mut self.conversation_state.side;
        side.stack = pane::follow(&side.stack, side.switches, to.as_ref(), &mut side.threads, side.narrow.get());
        side.fitted = key;
        side.asked = false;
        self.install(cx);
    }

    /// Reads the two switches from what the window remembers, once, before anything uses them:
    /// a header button pressed before the first change would otherwise be undone by it.
    fn restore_switches(&mut self) {
        if self.conversation_state.side.restored {
            return;
        }
        let state = &self.layout_state;
        let side = &mut self.conversation_state.side;
        side.restored = true;
        side.switches.direct = state.details_directs;
        side.switches.channel = if state.details_channels { pane::ChannelSwitch::Members } else { pane::ChannelSwitch::Off };
    }

    /// For `--shot`: the channel switch on Members, as a wider window would have left it.
    #[cfg(feature = "gui-shot")]
    pub(super) fn members_switch_on(&mut self, cx: &mut Context<Self>) {
        self.restore_switches();
        self.conversation_state.side.switches.channel = pane::ChannelSwitch::Members;
        self.conversation_state.side.fitted = None;
        self.follow_open(cx);
    }

    /// Puts what the stack says in the layout's slot, and builds the views it needs.
    fn install(&mut self, cx: &mut Context<Self>) {
        self.remember_switches(cx);
        let stack = self.conversation_state.side.stack.clone();
        match stack.thread() {
            Some((conversation, root)) => self.keep_thread(conversation, root, cx),
            None => drop(self.conversation_state.thread.borrow_mut().take()),
        }
        let top = stack.top().cloned();
        if let Some(layer @ (Layer::Profile(_) | Layer::Details(_))) = &top {
            // A person picked inside a Space is shown as that Space knows them.
            let space = self.snapshot.as_ref().and_then(|s| s.open.as_ref()).filter(|open| open.kind != ConversationKind::Direct);
            let space = space.and_then(|open| open.space.clone().map(|id| Key { id, ..open.key.clone() }));
            self.pane_show(layer, space, cx);
        }
        if let Some(Layer::Pinned(conversation)) = &top {
            self.ask_pins(conversation, cx);
        }
        match top {
            Some(layer) => {
                let kind = kind_of(&layer);
                self.conversation_state.side.installed = Some(kind);
                self.open_right(RightPane { kind, title: layer.title().into(), count: None, body: draw_side }, cx);
            }
            None => {
                if self.conversation_state.side.installed.take().is_some() && self.right_kind().is_some() {
                    self.close_right(None, cx);
                }
            }
        }
    }

    /// Another view changed the slot: the ✕, `cmd-.`, a member row's profile, the members
    /// toggle. The stack takes the change in and the slot gets this view's body back.
    fn take_in(&mut self, cx: &mut Context<Self>) {
        let side = &self.conversation_state.side;
        let slot = self.right_kind();
        let mine = side.stack.top().map(kind_of);
        let shown = self.pane_subject(cx);
        if slot == mine {
            // The same kind, but another view may have put someone else in Details.
            let top = side.stack.top().filter(|top| matches!(top, Layer::Profile(_) | Layer::Details(_) | Layer::Space(_)));
            if let (Some(top), Some(shown)) = (top, shown) {
                if *top != shown {
                    let side = &mut self.conversation_state.side;
                    side.stack = pane::open(&side.stack, shown);
                    self.install(cx);
                }
            }
            return;
        }
        let side = &mut self.conversation_state.side;
        match slot {
            // In a narrow window the pane stood in for the conversation, and "‹ #channel" or ✕
            // went back to it: sticky Details goes back to hiding with its switch still on.
            None if side.installed.is_some() && side.narrow.get() && side.stack.base.as_ref().is_some_and(Layer::is_details) => {
                let open = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).map(pane_open);
                let side = &mut self.conversation_state.side;
                side.stack = Stack { base: open.and_then(|open| pane::derived(side.switches, &open)), push: None };
                side.asked = false;
                self.install(cx);
            }
            None if side.installed.is_some() => {
                side.switches = pane::close(&side.stack, side.switches);
                side.stack = Stack::default();
                side.installed = None;
                // A search pane the ✕ closed goes with its stack, exactly as Esc's closing
                // does: no page behind, no field holding the keys.
                side.search = None;
                side.search_input = None;
                self.conversation_state.thread.borrow_mut().take();
            }
            Some(RightKind::Info) => {
                if let Some(shown) = shown {
                    side.asked = true;
                    side.stack = pane::open(&side.stack, shown);
                    self.install(cx);
                }
            }
            Some(RightKind::Members) => {
                if let Some(open) = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).map(pane_open) {
                    let side = &mut self.conversation_state.side;
                    side.switches.channel = pane::ChannelSwitch::Members;
                    side.stack = Stack { base: pane::derived(side.switches, &open), push: None };
                    self.install(cx);
                }
            }
            // Something this view does not draw (search, a sheet): the stack makes way, and a
            // thread in it is kept for its conversation.
            _ => {
                pane::keep_thread(&side.stack, &Stack::default(), &mut side.threads);
                side.stack = Stack::default();
                side.installed = None;
            }
        }
    }

    /// The thread view for this root, made once and kept while the stack holds it.
    fn keep_thread(&mut self, conversation: &Key, root: &Rc<Message>, cx: &mut Context<Self>) {
        let Some(key) = thread_key(conversation, root) else {
            return;
        };
        let held = self.conversation_state.thread.borrow().as_ref().and_then(|thread| thread.read(cx).key().cloned());
        if held.as_ref() == Some(&key) {
            return;
        }
        let title = self.snapshot.as_ref().and_then(|s| s.open.as_ref()).map(|open| open.title.clone()).unwrap_or_default();
        let me = self.me(conversation);
        let shell = cx.weak_entity();
        let root = root.clone();
        let view = cx.new(|cx| {
            let mut view = Pane::new(super::tokens::Metrics::DENSE, shell, cx);
            view.show_thread(key, &title, &me, root, cx);
            view
        });
        *self.conversation_state.thread.borrow_mut() = Some(view);
    }

    /// The reply line under a message of the open conversation was clicked.
    pub(in crate::gui) fn open_thread_of(&mut self, conversation: Key, root: Rc<Message>, cx: &mut Context<Self>) {
        // A click that raced a switch belongs to a conversation no longer open.
        if self.open_key() != Some(&conversation) || thread_key(&conversation, &root).is_none() {
            return;
        }
        let side = &mut self.conversation_state.side;
        side.threads.forget(&conversation);
        side.stack = pane::open(&side.stack, Layer::Thread { conversation, root });
        self.install(cx);
    }

    /// The header's Details button.
    pub(super) fn toggle_details(&mut self, cx: &mut Context<Self>) {
        self.restore_switches();
        let Some(open) = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).map(pane_open) else {
            return;
        };
        let side = &mut self.conversation_state.side;
        let wanted = if open.direct.is_some() { pane::ChannelSwitch::Off } else { pane::ChannelSwitch::Details };
        if !side.reveal_once(wanted, &open) {
            let before = side.stack.clone();
            (side.stack, side.switches) = pane::toggle_details(&side.stack, side.switches, &open);
            pane::keep_thread(&before, &side.stack, &mut side.threads);
        }
        self.install(cx);
    }

    /// The header's Members button.
    pub(super) fn toggle_member_list(&mut self, cx: &mut Context<Self>) {
        self.restore_switches();
        let Some(open) = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).map(pane_open) else {
            return;
        };
        let side = &mut self.conversation_state.side;
        if !side.reveal_once(pane::ChannelSwitch::Members, &open) {
            let before = side.stack.clone();
            (side.stack, side.switches) = pane::toggle_members(&side.stack, side.switches, &open);
            pane::keep_thread(&before, &side.stack, &mut side.threads);
        }
        self.install(cx);
    }

    /// The switcher's "Search messages" row: the pane opens with the words already asked for.
    /// The field is filled to match, whose `set_text` is not an edit and emits nothing - the
    /// ask carries the words, so nothing depends on it emitting.
    pub(in crate::gui) fn open_message_search(&mut self, words: String, window: &mut Window, cx: &mut Context<Self>) {
        self.new_search_with(Scope::Everywhere, &words, window, cx);
    }

    /// Search in the conversation, or everywhere: the pane opens with its field focused, or,
    /// standing already, the scope switches and the asking starts again.
    pub(in crate::gui) fn open_search(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        // When another layer sits on top of Search, starting a search starts over. And a
        // conversation scope with nothing open names nothing — it asks everywhere, as the
        // switcher's row does, so a chip is lit.
        let scope = if scope == Scope::Conversation && self.open_key().is_none() {
            Scope::Everywhere
        } else {
            scope
        };
        if !matches!(self.conversation_state.side.stack.top(), Some(Layer::Search)) {
            self.conversation_state.side.search = None;
            self.conversation_state.side.search_input = None;
            self.new_search_with(scope, "", window, cx);
            return;
        }
        if let Some(page) = self.conversation_state.side.search.as_mut() {
            page.scope = scope;
            page.hits = None;
            page.picked = None;
            page.reset_network();
        }
        self.reask_search(cx);
        if let Some(field) = &self.conversation_state.side.search_input {
            window.focus(&field.input.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// The Edit menu's two finds.
    pub(in crate::gui) fn search_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(Scope::Conversation, window, cx);
    }

    pub(in crate::gui) fn search_everywhere(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(Scope::Everywhere, window, cx);
    }

    /// A fresh search, seeded with the words it starts asked for. `open_search` and the
    /// switcher's row both come through here.
    fn new_search_with(&mut self, scope: Scope, words: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new("Search messages", cx));
        let changed = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Changed => this.search_typed(cx),
            InputEvent::Submit => this.enter_search(cx),
        });
        let keys = cx.subscribe_in(&input, window, |this, _, event: &InputKey, window, cx| match event {
            InputKey::Escape => this.close_search(window, cx),
            InputKey::Up => this.move_search_pick(false, cx),
            InputKey::Down => this.move_search_pick(true, cx),
        });
        {
            let side = &mut self.conversation_state.side;
            side.search_input = Some(SearchField { input, _subscriptions: vec![changed, keys] });
            side.search = Some(fresh_page(scope, words));
            side.stack = pane::open(&side.stack, Layer::Search);
        }
        self.install(cx);
        if let Some(field) = &self.conversation_state.side.search_input {
            window.focus(&field.input.focus_handle(cx), cx);
        }
        if !words.is_empty() {
            // The field is filled to match the ask. `set_text` is not an edit and emits
            // nothing, which is why the ask above carries the words itself.
            if let Some(field) = &self.conversation_state.side.search_input {
                field.input.update(cx, |input, cx| input.set_text(words, cx));
            }
        }
        self.reask_search(cx);
    }

    /// Esc in the field: the pane closes and the conversation's keys come back.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = &mut self.conversation_state.side;
        if side.search.take().is_none() {
            return;
        }
        side.stack = Stack { base: None, push: None };
        side.search_input = None;
        self.install(cx);
        self.composer_focus(window, cx);
    }

    /// What the field holds now is what the cache is asked for.
    fn search_typed(&mut self, cx: &mut Context<Self>) {
        let words = self
            .conversation_state
            .side
            .search_input
            .as_ref()
            .map(|field| field.input.read(cx).text().to_owned())
            .unwrap_or_default();
        let Some(page) = self.conversation_state.side.search.as_mut() else {
            return;
        };
        if words == page.words {
            return;
        }
        page.words = words;
        page.hits = None;
        page.picked = None;
        page.reset_network();
        self.reask_search(cx);
    }

    /// Asks the cache for the page's words within its scope. The pane's scopes name the
    /// conversation open now, so a switch of conversation re-resolves the next ask.
    fn reask_search(&mut self, cx: &mut Context<Self>) {
        let (words, scope, asked) = {
            let side = &mut self.conversation_state.side;
            let Some(page) = side.search.as_mut() else {
                return;
            };
            side.search_asks += 1;
            page.failed = false;
            page.asked = side.search_asks;
            (page.words.clone(), page.scope, page.asked)
        };
        if words.trim().is_empty() {
            if let Some(page) = self.conversation_state.side.search.as_mut() {
                page.hits = None;
                page.reset_network();
            }
            cx.notify();
            return;
        }
        let open = self.open_with_space();
        let (conversation, space) = scope_filters(scope, open.as_ref().map(|(key, space)| (key, space.as_deref())));
        let request = Request::SearchMessages { conversation, network: None, space, query: words.clone(), limit: protocol::PAGE_LIMIT };
        self.off_thread(
            cx,
            move || ask(&request),
            move |this, answer, cx| {
                let Some(page) = this
                    .conversation_state
                    .side
                    .search
                    .as_mut()
                    .filter(|page| page.asked == asked && page.words == words && page.scope == scope)
                else {
                    return;
                };
                match answer {
                    Ok(Response::MessageHits { hits }) => page.hits = Some(hits),
                    Ok(_) | Err(_) => page.failed = true,
                }
                cx.notify();
            },
        );
    }

    /// The conversation open now, with the Space it sits in: what the pane's scopes name.
    fn open_with_space(&self) -> Option<(Key, Option<String>)> {
        let key = self.open_key()?.clone();
        let mut held = self
            .sidebar_state
            .held()
            .cloned()
            .chain(self.trees.values().flat_map(|tree| tree.conversations.iter().cloned()));
        let space = held.find(|conversation| conversation.key == key).and_then(|conversation| conversation.space.clone());
        Some((key, space))
    }

    /// Enter in the field: with a hit picked, open it; otherwise the network itself is asked
    /// when it can search here, or the first hit opens.
    fn enter_search(&mut self, cx: &mut Context<Self>) {
        let (scope, first, picked) = match self.conversation_state.side.search.as_ref() {
            Some(page) => {
                let listed = page.listed();
                let first = listed.first().map(|(hit, _)| (*hit).clone());
                let picked = page.picked.as_ref().and_then(|(conv, id)| {
                    listed.iter().find(|(hit, _)| &hit.conversation == conv && hit.message_id == *id).map(|(hit, _)| (*hit).clone())
                });
                (page.scope, first, picked)
            }
            None => return,
        };
        if let Some(hit) = picked {
            self.open_search_hit(&hit, cx);
            return;
        }
        let can_ask_network = scope != Scope::Everywhere
            && self.open_key().is_some_and(|key| key.network == Network::Discord);
        if !can_ask_network {
            if let Some(hit) = first {
                self.open_search_hit(&hit, cx);
            }
            return;
        }
        self.ask_network(cx);
    }

    /// Asks the network itself for the page's words: a Space's own search, or the open
    /// conversation's channel search.
    fn ask_network(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.conversation_state.side.search.as_ref() else {
            return;
        };
        let (words, scope) = (page.words.clone(), page.scope);
        let trimmed: String = words.trim().chars().take(1024).collect();
        if trimmed.is_empty() {
            return;
        }
        // The same words in the same scope are asked once while in flight or after a
        // successful answer: Enter again, key repeats included, changes nothing until the
        // words or the scope change.
        if page.network_asked_for.as_deref() == Some(trimmed.as_str()) {
            return;
        }
        let (conversation, space, where_at) = match scope {
            Scope::Conversation => {
                let Some(open) = self.snapshot.as_ref().and_then(|s| s.open.as_ref()) else {
                    return;
                };
                let key = open.key.clone();
                let name = if open.title.is_empty() { key.id.clone() } else { open.title.clone() };
                let where_at = format!("{} · {}", key.network.label(), name);
                (Some(key), None, where_at)
            }
            Scope::Space => {
                let Some((key, Some(space))) = self.open_with_space() else {
                    return;
                };
                let known = self
                    .spaces
                    .iter()
                    .find(|known| known.key.network == key.network && known.key.account == key.account && known.key.id == space)
                    .map_or_else(String::new, |known| known.name.clone());
                let where_at = format!("{} · {}", key.network.label(), if known.is_empty() { space.clone() } else { known });
                let space = Key { network: key.network, account: key.account.clone(), id: space };
                (None, Some(space), where_at)
            }
            Scope::Everywhere => return,
        };
        let asked = {
            let side = &mut self.conversation_state.side;
            side.search_asks += 1;
            if let Some(page) = side.search.as_mut() {
                page.network_asked = side.search_asks;
                page.network_asked_for = Some(trimmed.clone());
                page.network_asking = Some(where_at.clone());
            }
            side.search_asks
        };
        cx.notify();
        let request = Request::SearchNetwork { conversation, space, query: trimmed };
        self.off_thread(
            cx,
            move || ask(&request),
            move |this, answer, cx| {
                let Some(page) = this.conversation_state.side.search.as_mut().filter(|page| page.network_asked == asked) else {
                    return;
                };
                page.network_asking = None;
                let result = match answer {
                    Ok(Response::MessageHits { hits }) => Ok(hits),
                    Ok(Response::Error { code, detail }) => Err(asked_words(code, &detail)),
                    Ok(_) => Err(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => Err(crate::client::trouble_words(&err)),
                };
                if result.is_err() {
                    page.network_asked_for = None;
                }
                page.network = Some(NetworkFound { where_at, result });
                cx.notify();
            },
        );
    }

    /// The keyboard moves over the hits; Up from row 0 steps off the list so Enter can ask
    /// the network again.
    fn move_search_pick(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(page) = self.conversation_state.side.search.as_mut() else {
            return;
        };
        page.move_pick(down);
        cx.notify();
    }

    /// A search hit was clicked: its conversation opens on the message, or, being open already,
    /// jumps to it.
    pub(in crate::gui) fn open_search_hit(&mut self, hit: &MessageHit, cx: &mut Context<Self>) {
        let key = hit.conversation.clone();
        // The pane may never have been built: nothing may have opened yet. It is made here, so
        // the landing reaches it before the conversation is told to open.
        let pane = self.pane(cx);
        if self.open.as_ref() == Some(&key) {
            pane.update(cx, |pane, cx| pane.jump_to(&hit.message_id, cx));
            return;
        }
        pane.update(cx, |pane, _| pane.land_next(hit.message_id.clone()));
        let title = if hit.title.is_empty() { hit.conversation.id.clone() } else { hit.title.clone() };
        self.select(key, &title, cx);
    }

    /// For slice D's Details: the open conversation's pinned messages, over Details.
    #[allow(dead_code)]
    pub(in crate::gui) fn open_pins(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.open_key().cloned() else {
            return;
        };
        let side = &mut self.conversation_state.side;
        side.stack = pane::open(&side.stack, Layer::Pinned(conversation));
        self.install(cx);
    }

    /// The two switches go to the window's prefs, which keep them across a restart. Members
    /// or Details beside channels is this session's; the prefs say only on or off.
    fn remember_switches(&mut self, cx: &mut Context<Self>) {
        let switches = self.conversation_state.side.switches;
        let wanted = (switches.channel != pane::ChannelSwitch::Off, switches.direct);
        let state = &mut self.layout_state;
        if (state.details_channels, state.details_directs) != wanted {
            (state.details_channels, state.details_directs) = wanted;
            self.save_prefs(cx);
        }
    }

    /// What the header lights: the kind on screen now. Details hidden by a narrow window is
    /// not on screen, so its button is unlit.
    pub(super) fn pane_shows(&self, kind: fn(&Layer) -> bool) -> bool {
        !self.pane_tucked() && self.conversation_state.side.stack.top().is_some_and(kind)
    }

    /// The layout, each frame: whether the window is too narrow for the pane beside the
    /// conversation, and how wide the conversation is drawn.
    pub(in crate::gui) fn note_room(&self, narrow: bool, room: Option<f32>) {
        let side = &self.conversation_state.side;
        side.narrow.set(narrow);
        if let Some(room) = room {
            side.room.set(room);
        }
    }

    pub(in crate::gui) fn pane_narrow(&self) -> bool {
        self.conversation_state.side.narrow.get()
    }

    /// The conversation region's width in the last frame; 0 before the first.
    pub(super) fn conversation_room(&self) -> f32 {
        self.conversation_state.side.room.get()
    }

    /// Details is on but hidden, because the window is too narrow for it (07 §4).
    pub(in crate::gui) fn pane_tucked(&self) -> bool {
        let side = &self.conversation_state.side;
        pane::tucked(&side.stack, side.narrow.get(), side.asked)
    }

    /// The crumb's way back: the pushed layer goes.
    fn pop(&mut self, cx: &mut Context<Self>) {
        if self.conversation_state.side.stack.push.take().is_some() {
            self.install(cx);
        }
    }

    /// What is up that the Escape ladder (07 §6) passes before the pane. The composer's own
    /// list, edit and reply are its to close while it has the keys.
    fn showing(&self, cx: &mut Context<Self>) -> Showing {
        let viewer = self.conversation_state.pane.get().is_some_and(|pane| pane.read(cx).viewer.is_some());
        Showing {
            autocomplete: false,
            menu: self.layout_state.menu.is_some(),
            card: self.pane_card_open(cx),
            viewer,
            switcher: self.sidebar_state.switcher.is_some(),
            floating_sidebar: self.layout_state.floating,
            editing: self.composer_editing(cx).is_some(),
            replying: self.composer_replying(cx),
        }
    }

    /// Escape (07 §6), one step per press. False when the step belongs to a view that takes
    /// the key itself (the viewer, an edit, the card): nothing here closes in its place, so
    /// Details is never closed for it.
    pub(in crate::gui) fn pane_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let showing = self.showing(cx);
        let step = pane::escape(showing, &self.conversation_state.side.stack);
        self.escape_step(step, Some(window), cx)
    }

    /// For the composer: Escape found nothing of its own to close (no list, no edit, no reply),
    /// so the ladder goes on from there instead of the field letting go of the keys. One press
    /// closes a thread; with nothing open it reads to the newest, and the composer keeps focus.
    #[allow(dead_code)]
    pub(in crate::gui) fn composer_escaped(&mut self, cx: &mut Context<Self>) {
        let showing = Showing { autocomplete: false, editing: false, replying: false, ..self.showing(cx) };
        let step = pane::escape(showing, &self.conversation_state.side.stack);
        self.escape_step(step, None, cx);
    }

    fn escape_step(&mut self, step: Step, window: Option<&mut Window>, cx: &mut Context<Self>) -> bool {
        match step {
            Step::Menu => self.close_menu(cx),
            Step::Switcher => match window {
                Some(window) => self.close_switcher(window, cx),
                None => return false,
            },
            Step::FloatingSidebar => {
                self.layout_state.floating = false;
                cx.notify();
            }
            Step::Pop => self.pop(cx),
            Step::ClosePane => {
                let side = &mut self.conversation_state.side;
                side.stack = Stack { base: None, push: None };
                side.search = None;
                side.search_input = None;
                self.install(cx);
                // The thread's composer may have had the keys: they go back to the conversation's.
                if let Some(window) = window {
                    self.composer_focus(window, cx);
                }
            }
            Step::Latest => {
                // A pane this view did not put there (search results, a sheet) closes first.
                if self.conversation_state.side.installed.is_none() && self.right_kind().is_some() {
                    self.close_right(window, cx);
                    return true;
                }
                if let Some(pane) = self.conversation_state.pane.get().cloned() {
                    pane.update(cx, |pane, cx| pane.go_to_bottom(cx));
                }
                self.mark_open_read(cx);
                if let Some(window) = window {
                    self.composer_focus(window, cx);
                }
            }
            Step::Autocomplete | Step::Card | Step::Viewer | Step::Edit | Step::Reply => return false,
        }
        true
    }

    fn ask_pins(&mut self, conversation: &Key, cx: &mut Context<Self>) {
        let side = &mut self.conversation_state.side;
        if side.pins.as_ref().is_some_and(|pins| pins.conversation == *conversation) {
            return;
        }
        side.pins = Some(Pins { conversation: conversation.clone(), messages: None });
        let request = Request::Pinned { conversation: conversation.clone() };
        let conversation = conversation.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let messages = match answer {
                Ok(Response::Pinned { messages, .. }) => Ok(messages.into_iter().map(Rc::new).collect()),
                Ok(Response::Error { detail, .. }) => Err(detail),
                Ok(_) => Err("The pins could not be read.".to_owned()),
                Err(err) => Err(crate::client::trouble_words(&err)),
            };
            let _ = this.update(cx, |shell, cx| {
                if let Some(pins) = shell.conversation_state.side.pins.as_mut().filter(|pins| pins.conversation == conversation) {
                    pins.messages = Some(messages);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A pin was clicked: the conversation shows it, as a reply quote would.
    fn jump_to_pin(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(pane) = self.conversation_state.pane.get().cloned() {
            let id = id.to_owned();
            pane.update(cx, |pane, cx| pane.jump_to(&id, cx));
        }
    }

    pub(super) fn subscribe_pane(&self, pane: &Entity<Pane>, cx: &mut Context<Self>) {
        self.conversation_state.listening.get_or_init(|| {
            cx.subscribe(pane, |shell, pane, event: &PaneEvent, cx| match event {
                PaneEvent::OpenThread(root) => {
                    if let Some(conversation) = pane.read(cx).key().cloned() {
                        shell.open_thread_of(conversation, root.clone(), cx);
                    }
                }
            })
        });
    }
}

/// The right pane's body, whatever its kind. Nothing is drawn that is about a conversation
/// other than the open one, not even for a frame.
fn draw_side(shell: &Shell, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let stack = &shell.conversation_state.side.stack;
    let open = shell.open_key();
    let Some(top) = stack.top() else {
        return div().into_any_element();
    };
    if stack.layers().filter_map(Layer::conversation).any(|conversation| Some(conversation) != open) {
        return div().into_any_element();
    }
    let body = match top {
        Layer::Profile(_) | Layer::Details(_) | Layer::Space(_) => shell.pane_details(window, cx),
        Layer::Members(_) => shell.pane_members(window, cx),
        Layer::Thread { conversation, .. } => draw_thread(shell, conversation, cx),
        Layer::Pinned(conversation) => draw_pins(shell, conversation, cx),
        Layer::Search => draw_search(shell, window, cx),
    };
    let crumb = stack.push.as_ref().and(stack.base.as_ref()).map(|base| crumb(base.title(), cx));
    div()
        .size_full()
        .flex()
        .flex_col()
        .children(crumb)
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

/// The way back from a pushed layer to the base, at the pane's top.
// CONTRACT: the design puts the crumb in the pane's title strip, which `layout.rs` draws; it
// can call `side::crumb` there and this row goes.
fn crumb(base: &'static str, cx: &mut Context<Shell>) -> AnyElement {
    div()
        .flex_none()
        .h(px(36.))
        .px(px(8.))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme::hairline())
        .child(
            div()
                .id("pane-crumb")
                .h(px(28.))
                .pl(px(4.))
                .pr(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .active(|style| style.bg(theme::pressed()))
                .child(glyph("icons/chevron-left.svg", 16., theme::text_soft()))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text_soft()).child(base))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|shell, _, _, cx| shell.pop(cx))),
        )
        .into_any_element()
}

/// The message search's pane: what was asked, and the cache's hits, newest first. Every hit is
/// the cache's word for it, so a row says "cached"; the networks are a later ask.
fn draw_search(shell: &Shell, _window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let Some(page) = shell.conversation_state.side.search.as_ref() else {
        return div().into_any_element();
    };
    let field = shell.conversation_state.side.search_input.as_ref().map(|field| field.input.clone());
    let head = div()
        .h(px(44.))
        .flex_none()
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(theme::hairline())
        .child(glyph("icons/search.svg", 14., theme::text_soft()))
        .child(match field {
            Some(field) => div().flex_1().min_w_0().child(field).into_any_element(),
            None => div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(page.words.clone()).into_any_element(),
        });
    // The scope's chips: the conversation open now, its Space, or the whole cache. One is the
    // scope asked; a chip about no conversation open is not drawn, for it cannot be asked.
    let open = shell.open_with_space();
    let in_space = open.as_ref().and_then(|(_, space)| space.as_deref()).is_some();
    let mut chips = div().flex_none().px(px(8.)).pt(px(8.)).pb(px(2.)).flex().flex_wrap().gap(px(6.));
    for candidate in [Scope::Conversation, Scope::Space, Scope::Everywhere] {
        let available = match candidate {
            Scope::Conversation => open.is_some(),
            Scope::Space => in_space,
            Scope::Everywhere => true,
        };
        if !available {
            continue;
        }
        let lit = candidate == page.scope;
        chips = chips.child(
            div()
                .id(("search-scope", candidate as usize))
                .px(px(10.))
                .h(px(24.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .type_small()
                .cursor_pointer()
                .text_color(if lit { theme::text() } else { theme::text_soft() })
                .when(lit, |chip| chip.bg(theme::selected()))
                .hover(|style| style.bg(theme::hover()))
                .on_click(cx.listener(move |this, _, window, cx| this.open_search(candidate, window, cx)))
                .child(candidate.label()),
        );
    }
    // What the network said on Enter: a line that names what is being searched, what was
    // searched, or its word for why nothing came.
    let said = if let Some(where_at) = &page.network_asking {
        Some(format!("Asking {where_at}…"))
    } else {
        page.network.as_ref().map(|found| match &found.result {
            Ok(_) => format!("Searched {}.", found.where_at),
            Err(why) => why.clone(),
        })
    };
    let listed = page.listed();
    let body = if page.words.trim().is_empty() {
        empty_state("icons/search.svg", "Search the cache.", "What was said in what tinyclient has read.").into_any_element()
    } else if page.failed {
        empty_state("icons/search.svg", "tinyclient isn't answering.", "The cache could not be asked. Try again in a moment.")
            .into_any_element()
    } else if page.hits.is_none() {
        empty_state("icons/search.svg", "Asking the cache…", "The hits land here as they are read.").into_any_element()
    } else if listed.is_empty() {
        let why = if page.network_asking.is_some() {
            "Waiting for the network's answer."
        } else {
            match page.network.as_ref().map(|found| &found.result) {
                Some(Ok(_)) => "The network found nothing here.",
                Some(Err(why)) => why.as_str(),
                None => "The networks are not asked yet.",
            }
        };
        empty_state("icons/search.svg", "Nothing in the cache yet.", why).into_any_element()
    } else {
        let mut rows = div().id("search-hits").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().p(px(4.));
        for (ix, (hit, cached)) in listed.iter().enumerate() {
            let clicked = (*hit).clone();
            let picked = page
                .picked
                .as_ref()
                .is_some_and(|(conv, id)| conv == &hit.conversation && *id == hit.message_id);
            rows = rows.child(
                div()
                    .id(("hit", ix))
                    .flex_none()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .cursor_pointer()
                    .when(picked, |row| row.bg(theme::selected()))
                    .hover(|style| style.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(page) = this.conversation_state.side.search.as_mut() {
                            page.picked = Some((clicked.conversation.clone(), clicked.message_id.clone()));
                        }
                        this.open_search_hit(&clicked, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(chat::mark(hit.conversation.network, 13., theme::text_faint()))
                            .child(div().flex_1().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(said_at(hit)))
                            .child(div().flex_none().type_small().text_color(theme::text_soft()).child(age(now(), hit.sent))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child({
                                let highlights: Vec<(Range<usize>, HighlightStyle)> =
                                    matched(&hit.snippet, &page.words).into_iter().map(|range| (range, lit())).collect();
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .type_small()
                                    .text_color(theme::text_soft())
                                    .child(StyledText::new(SharedString::from(hit.snippet.clone())).with_highlights(highlights))
                            })
                            .when(*cached, |row| {
                                row.child(div().flex_none().type_micro().text_color(theme::text_soft()).child("cached"))
                            }),
                    ),
            );
        }
        rows.into_any_element()
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(theme::canvas())
        .child(head)
        .child(chips)
        .children(said.map(|line| div().flex_none().px(px(12.)).pb(px(4.)).type_small().text_color(theme::text_soft()).child(line)))
        .child(body)
        .into_any_element()
}

/// The thread and its composer. The composer posts to the thread's own key, and only while
/// that thread belongs to the open conversation.
fn draw_thread(shell: &Shell, conversation: &Key, cx: &mut Context<Shell>) -> AnyElement {
    let thread = shell.conversation_state.thread.borrow().clone();
    let Some(thread) = thread.filter(|_| thread_belongs(conversation, shell.open_key())) else {
        return div().into_any_element();
    };
    let target = thread.read(cx).open.as_ref().map(|open| (open.key.clone(), open.title.clone()));
    let link = shell.snapshot.as_ref().map_or(Link::Offline, |snapshot| snapshot.account_link(conversation));
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(theme::canvas())
        .child(div().flex_1().min_h_0().child(thread))
        .children(target.map(|(key, title)| shell.thread_composer(&key, &title, link, cx)))
        .into_any_element()
}

fn draw_pins(shell: &Shell, conversation: &Key, cx: &mut Context<Shell>) -> AnyElement {
    let pins = shell.conversation_state.side.pins.as_ref().filter(|pins| pins.conversation == *conversation);
    let messages = match pins.and_then(|pins| pins.messages.as_ref()) {
        None => return div().into_any_element(),
        Some(Err(why)) => return empty_state("icons/pin.svg", "Pins aren't available", why).into_any_element(),
        Some(Ok(messages)) if messages.is_empty() => {
            return empty_state("icons/pin.svg", "Nothing pinned", "Pinned messages show here.").into_any_element();
        }
        Some(Ok(messages)) => messages,
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let rows = messages.iter().map(|message| {
        let id = message.id.clone();
        let said = if message.text.is_empty() { "Attachment".to_owned() } else { message.text.clone() };
        div()
            .id(gpui::ElementId::Name(format!("pin-{}", message.id).into()))
            .px(px(theme::PANE_PAD))
            .py(px(8.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().min_w_0().truncate().type_ui().font_weight(theme::SEMIBOLD).child(message.sender.name.clone()))
                    .child(div().flex_none().type_micro().text_color(theme::text_faint()).child(time::stamp(now, message.sent))),
            )
            .child(div().type_ui().text_color(theme::text_soft()).line_clamp(3).child(said))
            .on_click(cx.listener(move |shell, _, _, cx| shell.jump_to_pin(&id, cx)))
    });
    div().id("pins").size_full().overflow_y_scroll().py(px(4.)).children(rows).into_any_element()
}

/// Where the message was said, as the spec's result line has it: the Space it sits in and the
/// conversation, or the conversation's id when it has no title yet.
fn said_at(hit: &MessageHit) -> String {
    let name = if hit.title.is_empty() {
        hit.conversation.id.as_str()
    } else {
        hit.title.as_str()
    };
    if hit.space_title.is_empty() {
        name.to_owned()
    } else {
        format!("{} › {name}", hit.space_title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Network, ReplyTo, ThreadSummary};

    fn key(id: &str) -> Key {
        Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, id)
    }

    #[test]
    fn a_thread_posts_under_its_own_key_and_only_beside_its_conversation() {
        let mut root = Message::plain("m1", "Ines", "root", 0, false);
        // Slack names the thread a first reply would start: the channel and this message.
        let started = thread_key(&key("C_a"), &root).unwrap();
        assert_eq!((started.id.as_str(), started.account()), ("C_a:m1", key("C_a").account()));
        let discord = Key::new(&AccountKey { network: Network::Discord, account: "1".into() }, "C_a");
        assert_eq!(thread_key(&discord, &root), None, "Discord's thread id is a channel that does not exist yet");
        let mut broadcast = root.clone();
        broadcast.reply_to = Some(ReplyTo { id: "parent".into(), sender: "Ana".into(), snippet: "earlier".into(), person: None, deleted: false });
        assert_eq!(thread_key(&key("C_a"), &broadcast), None);
        root.thread = Some(ThreadSummary { id: "C_a:m1".into(), replies: 2, last: 0, participants: Vec::new(), unread: 0 });
        let thread = thread_key(&key("C_a"), &root).unwrap();
        assert_eq!((thread.id.as_str(), thread.account()), ("C_a:m1", key("C_a").account()));
        // Drawn, composer and all, only while its conversation is the one open.
        assert!(thread_belongs(&key("C_a"), Some(&key("C_a"))));
        assert!(!thread_belongs(&key("C_a"), Some(&key("C_b"))));
        assert!(!thread_belongs(&key("C_a"), None));
    }

    #[test]
    fn a_scope_names_what_is_open_and_falls_back_to_everywhere() {
        let channel = key("C_a");
        let space = Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, "guild");

        // Everywhere asks for the whole cache, whatever is open.
        assert_eq!(scope_filters(Scope::Everywhere, None), (None, None));
        assert_eq!(scope_filters(Scope::Everywhere, Some((&channel, Some("guild")))), (None, None));

        // The conversation and its Space name what is open now.
        assert_eq!(scope_filters(Scope::Conversation, Some((&channel, Some("guild")))), (Some(channel.clone()), None));
        assert_eq!(scope_filters(Scope::Space, Some((&channel, Some("guild")))), (None, Some(space)));

        // A direct conversation has no Space, and with nothing open both name nothing: the
        // cache answers as a whole, as the switcher's row does.
        assert_eq!(scope_filters(Scope::Space, Some((&channel, None))), (None, None));
        assert_eq!(scope_filters(Scope::Conversation, None), (None, None));
        assert_eq!(scope_filters(Scope::Space, None), (None, None));
    }

    #[test]
    fn a_hit_row_names_its_space_and_conversation_or_falls_back_to_its_id() {
        let mut hit = MessageHit {
            conversation: key("C_a"),
            message_id: "1".into(),
            sent: 0,
            sender: "Ines".into(),
            snippet: "rules".into(),
            title: "general".into(),
            space_title: "Northwind".into(),
            outgoing: false,
        };
        assert_eq!(said_at(&hit), "Northwind › general");
        hit.space_title.clear();
        assert_eq!(said_at(&hit), "general");
        hit.title.clear();
        assert_eq!(said_at(&hit), "C_a");
        hit.space_title = "Northwind".into();
        assert_eq!(said_at(&hit), "Northwind › C_a");
    }

    #[test]
    fn the_switcher_s_words_start_asked_for() {
        // The switcher's row seeds its words into the page, so the first ask carries them
        // without relying on the field's `set_text`, which is not an edit and emits nothing.
        let page = fresh_page(Scope::Everywhere, "rules");
        assert_eq!((page.scope, page.words.as_str()), (Scope::Everywhere, "rules"));
        assert!(page.hits.is_none() && page.picked.is_none());
    }

    #[test]
    fn the_networks_hits_join_the_cache_s_and_the_picked_message_keeps_its_identity() {
        let mut page = SearchPage { words: "rules".into(), ..SearchPage::default() };
        let hit_of = |id: &str, sent: i64, conversation: &Key| MessageHit {
            conversation: conversation.clone(),
            message_id: id.into(),
            sent,
            sender: "mara".into(),
            snippet: "the rules".into(),
            title: String::new(),
            space_title: String::new(),
            outgoing: false,
        };
        page.hits = Some(vec![hit_of("1", 20, &key("C_a"))]);
        assert_eq!(page.listed().iter().map(|(hit, cached)| (hit.message_id.as_str(), *cached)).collect::<Vec<_>>(), [("1", true)]);
        // Picking the first row records its (conversation, message_id), and Up from row 0 clears it
        // so Enter can ask the network again.
        page.move_pick(true);
        assert_eq!(page.picked, Some((key("C_a"), "1".into())));
        page.move_pick(false);
        assert_eq!(page.picked, None);
        page.move_pick(true);
        // The network's own join newest first, and the one the cache already had dropped by id;
        // the picked message stays "1" even when a newer network hit lands above it.
        page.network = Some(NetworkFound {
            where_at: "Discord · Moss & Lichen".into(),
            result: Ok(vec![hit_of("1", 20, &key("C_a")), hit_of("2", 25, &key("C_b")), hit_of("3", 15, &key("C_c"))]),
        });
        assert_eq!(
            page.listed().iter().map(|(hit, cached)| (hit.message_id.as_str(), *cached)).collect::<Vec<_>>(),
            [("2", false), ("1", true), ("3", false)]
        );
        assert_eq!(page.picked, Some((key("C_a"), "1".into())));
    }
}
