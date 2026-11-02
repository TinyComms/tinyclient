//! Acting on a message: the toolbar over the row under the pointer, the context menu, the
//! reaction picker and the delete dialog, and the requests they send. Nothing changes on screen
//! until chatd says so, except a reaction, which chatd itself shows at once.

use super::emoji::{self, Picker, PickerEvent};
use super::icons::{icon, Icon};
use super::rows::Item;
use super::{Pane, PaneEvent};
use crate::client::ask;
use crate::gui::components::place::{max_height, placed, Layer, Place};
use crate::gui::components::{
    button, fade_in, menu, menu_item, menu_item_danger, menu_separator, menu_step, modal, modal_body, modal_buttons, popover, tip,
    ButtonKind,
};
use crate::gui::theme::{self, Type};
use crate::gui::time;
use gpui::{
    anchored, deferred, div, point, prelude::*, px, AnyElement, Bounds, ClipboardItem, Context, Div, ElementId, Entity,
    KeyDownEvent, MouseButton, Pixels, Point, SharedString, Stateful, Subscription, Window,
};
use protocol::{ConversationKind, ErrorCode, Message, MessageKind, Network, ReactionRules, Request, Response, SendState};
use std::rc::Rc;

/// How far the toolbar rises above its row, and how wide the strip it can occupy is.
const TOOLBAR_RISE: f32 = 14.;
const TOOLBAR_REACH: f32 = 320.;
/// How long a phone's press must hold still before the message's actions come up. Long enough
/// that a scroll never reads as one, short enough that the actions feel like an answer.
pub(super) const LONG_PRESS: std::time::Duration = std::time::Duration::from_millis(450);

const TOOLBAR_H: f32 = 30.;

/// Top of the toolbar within its row. Negative sits above the row. When that rise would
/// leave the viewport, the bar starts at the viewport's top edge, inside the row.
pub(super) fn toolbar_offset(row_top: f32, viewport_top: f32) -> f32 {
    if row_top - TOOLBAR_RISE >= viewport_top {
        -TOOLBAR_RISE
    } else {
        (viewport_top - row_top).max(0.)
    }
}

fn toolbar_box(row: Bounds<Pixels>, viewport: Bounds<Pixels>) -> Bounds<Pixels> {
    let offset = toolbar_offset(f32::from(row.top()), f32::from(viewport.top()));
    let top = row.top() + px(offset);
    let bottom = if offset < 0. {
        row.top() + px(TOOLBAR_RISE + 8.)
    } else {
        top + px(TOOLBAR_H)
    };
    Bounds::from_corners(
        point(row.right() - px(TOOLBAR_REACH), top),
        point(row.right(), bottom),
    )
}

/// Hover ownership is geometric: the toolbar occludes the pane's hitbox, but
/// crossing onto its buttons must keep the current hover owner.
fn pointed_row(position: Point<Pixels>, viewport: Bounds<Pixels>, seen: &[(usize, Bounds<Pixels>)], held: Option<usize>) -> Option<usize> {
    if !viewport.contains(&position) { return None; }
    let toolbar = held.and_then(|held| seen.iter().find(|(ix, bounds)|
        *ix == held && toolbar_box(*bounds, viewport).contains(&position)));
    toolbar.or_else(|| seen.iter().find(|(_, bounds)| bounds.contains(&position))).map(|(ix, _)| *ix)
}

/// The context menu of a message, where the pointer was.
pub struct Menu {
    pub message: Rc<Message>,
    pub at: Point<Pixels>,
    /// Which actionable row the keyboard has selected.
    pub cursor: usize,
}

/// The reaction picker, open for a message.
pub struct Picking {
    pub message: String,
    pub at: Point<Pixels>,
    pub picker: Entity<Picker>,
    _listening: Subscription,
}

/// What may be done to a message, decided apart from any drawing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Can {
    /// It is on the network: the toolbar shows and it can be reacted to.
    pub act: bool,
    pub reply: bool,
    pub thread: bool,
    pub edit: bool,
    pub delete: bool,
    /// Never left: deleting it is local, and it can be sent again.
    pub unsent: bool,
}

/// What the menu and the toolbar call this message's thread.
pub fn thread_label(message: &Message) -> &'static str {
    if message.thread.is_some() { "Open thread" } else { "Reply in thread" }
}

/// A message a first reply can hang off. A reply already shown in the channel belongs to its
/// parent's thread, which this row does not name.
fn thread_root(message: &Message) -> bool {
    message.state == SendState::Sent
        && message.reply_to.is_none()
        && !message.deleted
        && matches!(message.kind, MessageKind::Normal | MessageKind::Unsupported { .. })
}

/// Whether this message can start a thread that does not exist yet. Slack names one in a
/// channel or a direct message. Teams and Discord do, in a channel. A thread, a voice channel
/// and a forum do not start another. Telegram topics, Signal, Messages and Tiny have no such
/// thread.
pub(super) fn starts_thread(message: &Message, network: Network, kind: ConversationKind) -> bool {
    if !thread_root(message) || matches!(kind, ConversationKind::Thread | ConversationKind::Voice | ConversationKind::Forum) {
        return false;
    }
    match network {
        Network::Slack => true,
        Network::Teams | Network::Discord => matches!(kind, ConversationKind::Channel | ConversationKind::Announcement),
        Network::Telegram | Network::Signal | Network::Messages | Network::Tiny => false,
    }
}

/// `in_thread`: the row is in the thread pane, where a thread cannot be opened again.
pub fn can(message: &Message, network: Network, kind: ConversationKind, in_thread: bool) -> Can {
    let sent = message.state == SendState::Sent;
    let said = matches!(message.kind, MessageKind::Normal | MessageKind::Unsupported { .. }) && !message.deleted;
    let act = sent && said;
    let own = message.outgoing && message.kind == MessageKind::Normal && !message.deleted;
    Can {
        act,
        // Slack has no inline reply: its reply is the thread.
        reply: act && network != Network::Slack,
        thread: !in_thread && ((sent && message.thread.is_some()) || starts_thread(message, network, kind)),
        // CONTRACT: nothing says how long a network lets a message be edited, so Edit is offered
        // on every message of mine and a refusal is the network's answer. Messages never edits
        // or deletes, so those items stay off.
        edit: sent && own && network != Network::Messages,
        delete: own && network != Network::Messages,
        unsent: !sent && message.outgoing,
    }
}

/// Where a message leads in the network's own app, for "Copy link" and "Open in <network>".
/// An unsupported message names its own; Discord's and Slack's are built from ids, as the
/// conversation's are (`profile/facts.rs`).
// CONTRACT: `Message` carries no permalink. Telegram's needs a public username the shell does
// not hold, and Teams has none, so those offer neither item.
pub fn message_link(conversation: &protocol::Key, space: Option<&str>, message: &Message) -> Option<String> {
    if let MessageKind::Unsupported { link } = &message.kind {
        if !link.is_empty() {
            return Some(link.clone());
        }
    }
    if message.state != SendState::Sent || message.deleted {
        return None;
    }
    let (channel, id) = (&conversation.id, &message.id);
    match conversation.network {
        Network::Discord => {
            let place = space.unwrap_or("@me");
            Some(format!("https://discord.com/channels/{place}/{channel}/{id}"))
        }
        Network::Slack => space.map(|team| format!("slack://channel?team={team}&id={channel}&message={id}")),
        Network::Telegram | Network::Teams | Network::Signal | Network::Messages | Network::Tiny => None,
    }
}

fn tool(id: impl Into<ElementId>, child: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(26.))
        .h(px(24.))
        .flex_none()
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(theme::selected()))
        .active(|style| style.bg(theme::pressed()))
        // A press here is not a press on the row under it.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(child)
}

/// How many quick reactions a phone's action sheet shows in a window `width` wide: 44 pt discs
/// 8 apart inside 16 pt gutters, one kept for the picker when more may be added; one to six.
fn sheet_quick(width: f32, adds: bool) -> usize {
    let discs = ((width - 32. + 8.) / 52.).floor().max(0.) as usize;
    discs.saturating_sub(usize::from(adds)).clamp(1, 6)
}

fn glyph_of(emoji: &str) -> Div {
    div().text_size(px(15.)).line_height(px(20.)).child(SharedString::from(emoji::display(emoji)))
}

/// One row of a message menu that a click or a key can run. The order is [`menu_acts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAct {
    Retry,
    Copy,
    /// An unsent message, removed here without asking.
    Drop,
    Quick(usize),
    Reply,
    Thread,
    React,
    Edit,
    Link,
    Open,
    /// A sent message: ask, unless the click held Shift.
    Delete,
}

/// What the menu offers, in the order it is drawn and the order Up and Down move.
/// `quick` is how many quick reactions the menu shows. An unsent message ignores the rest.
fn menu_acts(
    can: Can,
    failed: bool,
    quick: usize,
    adds: bool,
    has_text: bool,
    has_link: bool,
) -> Vec<MenuAct> {
    if can.unsent {
        let mut acts = Vec::new();
        if failed {
            acts.push(MenuAct::Retry);
        }
        acts.push(MenuAct::Copy);
        acts.push(MenuAct::Drop);
        return acts;
    }
    let mut acts = Vec::new();
    for slot in 0..quick {
        acts.push(MenuAct::Quick(slot));
    }
    if can.reply {
        acts.push(MenuAct::Reply);
    }
    if can.thread {
        acts.push(MenuAct::Thread);
    }
    if can.act && adds {
        acts.push(MenuAct::React);
    }
    if can.edit {
        acts.push(MenuAct::Edit);
    }
    if has_text {
        acts.push(MenuAct::Copy);
    }
    if has_link {
        acts.push(MenuAct::Link);
        acts.push(MenuAct::Open);
    }
    if can.delete {
        acts.push(MenuAct::Delete);
    }
    acts
}

impl Pane {
    fn network(&self) -> Network {
        self.open.as_ref().map_or(Network::Discord, |open| open.key.network)
    }

    pub(super) fn can(&self, message: &Message) -> Can {
        // No open conversation is not a place a thread can be started from.
        let kind = self.open.as_ref().map(|open| open.kind).unwrap_or(ConversationKind::Thread);
        can(message, self.network(), kind, self.is_thread())
    }

    // Reactions.

    /// What the open conversation takes as reactions, when chatd said.
    pub(super) fn rules(&self) -> Option<&ReactionRules> {
        self.open.as_ref().and_then(|open| open.reactions.as_ref())
    }

    /// Something may be added to this message: the add affordances are drawn.
    pub(super) fn adds(&self, message: &Message) -> bool {
        self.rules().is_none_or(|rules| rules.adds(&message.reactions))
    }

    /// Why this reaction cannot be added to this message, or `None` when it can (or nothing is
    /// known to say otherwise). Taking mine back is always allowed.
    pub(super) fn refusal(&self, message: &Message, emoji: &str) -> Option<String> {
        // A custom emoji is named by an id or `:name:`, which are plain text; a Unicode one never is.
        let custom = emoji.is_ascii() || message.reactions.iter().any(|have| have.emoji == emoji && have.file.is_some());
        self.rules()?.check(&message.reactions, emoji, custom).err()
    }

    /// Asks what the open conversation takes as reactions; chatd keeps the answer five minutes.
    /// When it lands an open picker is narrowed to it.
    pub(super) fn ask_reactions(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        let request = Request::Reactions { conversation: conversation.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let rules = match answer {
                Ok(Response::Reactions { rules, .. }) => Some(rules),
                _ => None,
            };
            let _ = this.update(cx, |this, cx| {
                let Some(open) = this.open.as_mut().filter(|open| open.key == conversation) else {
                    return;
                };
                open.reactions = rules.clone();
                let space = this.space(cx);
                if let Some(picking) = &this.picker {
                    picking.picker.update(cx, |picker, cx| picker.set_rules(rules, space, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The open conversation's Space, where its custom emoji come from.
    fn space(&self, cx: &Context<Self>) -> Option<protocol::Key> {
        let shell = self.shell.upgrade()?;
        let shell = shell.read(cx);
        let open = shell.snapshot.as_ref()?.open.as_ref()?;
        Some(protocol::Key::new(&open.key.account(), open.space.clone()?))
    }

    // The pointer.

    /// Notes which message the pointer is over. Drawn again only when that changes. On a phone
    /// there is no resting pointer: a finger's movement never shows the toolbar, a long press does.
    pub(super) fn point_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if crate::gui::phone::active() {
            return;
        }
        // A menu or the picker belongs to a message: the toolbar stays with it.
        if self.menu.is_some() || self.picker.is_some() || self.confirm.is_some() || self.viewer.is_some() {
            return;
        }
        #[cfg(feature = "gui-shot")]
        if self.script.holds_hover() {
            return;
        }
        let over = {
            let seen = self.seen.borrow();
            let held = self.hover.as_ref().and_then(|id| seen.iter().find(|(ix, _)| self.items.get(*ix).and_then(Item::message).is_some_and(|m| m.id == *id))).map(|(ix, _)| *ix);
            pointed_row(position, self.list.viewport_bounds(), &seen, held)
                .and_then(|ix| self.items.get(ix)).and_then(Item::message)
                .filter(|m| self.can(m).act).map(|m| m.id.clone())
        };
        if over != self.hover {
            self.hover = over;
            cx.notify();
        }
    }

    // The toolbar.

    /// A menu, picker, dialog, viewer or a window-level layer is up: no toolbar shows.
    pub(super) fn overlay_up(&self, cx: &Context<Self>) -> bool {
        let own = self.menu.is_some() || self.picker.is_some() || self.confirm.is_some() || self.viewer.is_some();
        own || self.shell.upgrade().is_some_and(|shell| {
            let shell = shell.read(cx);
            shell.layout_state.menu.is_some() || shell.layout_state.floating || shell.sidebar_state.switcher.is_some() || shell.pane_card_open(cx)
        })
    }

    /// The message whose menu, picker or delete dialog is open: its row keeps `selected`.
    pub(super) fn owns_overlay(&self, id: &str) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.message.id == id)
            || self.picker.as_ref().is_some_and(|picking| picking.message == id)
            || self.confirm.as_ref().is_some_and(|message| message.id == id)
    }

    /// A phone's press on a message went down: the actions come up if it holds still for
    /// [`LONG_PRESS`]. Any other press clears the ones showing, so a tap elsewhere dismisses them.
    pub(super) fn press_message(&mut self, id: &str, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.hover = None;
        let id = id.to_owned();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(LONG_PRESS).await;
            let _ = this.update_in(cx, |this, window, cx| this.hold_message(&id, at, window, cx));
        });
        self.press = Some((task, (f32::from(at.x), f32::from(at.y))));
    }

    /// A press on a message held: its actions come up as a sheet, as the desktop's right-click
    /// menu does, for one that can be acted on or that did not go.
    pub(super) fn hold_message(&mut self, id: &str, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.hover = None;
        let Some(message) = self.find(id) else {
            return;
        };
        let can = self.can(&message);
        if can.act || can.unsent {
            self.open_menu(message, at, window, cx);
        }
    }

    /// The pointer moved during a press. Past the slop it was a scroll, so nothing will show.
    pub(super) fn drag_message(&mut self, at: Point<Pixels>) {
        let to = (f32::from(at.x), f32::from(at.y));
        if self.press.as_ref().is_some_and(|(_, from)| !crate::gui::phone::tokens::within_slop(*from, to)) {
            self.press = None;
        }
    }

    /// The finger lifted before the press became a long one.
    pub(super) fn release_message(&mut self) {
        self.press = None;
    }

    /// Back on a phone, over the messages: the picture viewer, the delete question, the
    /// message's sheet or the reaction picker closes before the conversation does. False when
    /// none of them was up.
    pub(in crate::gui) fn phone_back(&mut self, cx: &mut Context<Self>) -> bool {
        if self.viewer.is_some() {
            self.close_viewer(cx);
        } else if self.confirm.is_some() {
            self.confirm = None;
            cx.notify();
        } else if self.menu.is_some() || self.picker.is_some() {
            self.close_menu(cx);
            self.close_picker(cx);
        } else {
            return false;
        }
        true
    }

    /// Where this row's toolbar sits. Above the row when the rise stays in the viewport,
    /// and at the viewport's top edge inside the row when it would not.
    fn toolbar_rise(&self, message: &Message) -> f32 {
        let seen = self.seen.borrow();
        let row = seen.iter().find(|(ix, _)| {
            self.items
                .get(*ix)
                .and_then(Item::message)
                .is_some_and(|row| row.id == message.id)
        });
        match row {
            Some((_, bounds)) => toolbar_offset(
                f32::from(bounds.top()),
                f32::from(self.list.viewport_bounds().top()),
            ),
            None => -TOOLBAR_RISE,
        }
    }

    /// The toolbar of the row under the pointer: one instance, straddling the row's top edge.
    pub(super) fn toolbar(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Not while text is being selected: it would sit under the drag. Never over a menu or
        // under a scrim (polish/01 §2): the row that owns a menu is lit instead.
        // A phone has no toolbar: its actions are the long-press sheet.
        if crate::gui::phone::active() || self.hover.as_deref() != Some(message.id.as_str()) || self.dragging || self.overlay_up(cx) {
            return None;
        }
        let can = self.can(message);
        if !can.act {
            return None;
        }
        // On a touch screen a tooltip has no pointer to rest under: it is dropped there.
        let phone = crate::gui::phone::active();
        let mut bar = div()
            .absolute()
            .top(px(self.toolbar_rise(message)))
            .right(px(16.))
            .h(px(30.))
            .p(px(2.))
            .rounded(px(theme::R_CARD))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::hairline())
            .shadow(theme::shadow_tooltip())
            .flex()
            .items_center()
            .occlude();
        // Only reactions the conversation takes, and none at all where it takes none.
        let quick = emoji::quick(self.rules(), &message.reactions, 3);
        for (slot, quick) in quick.iter().enumerate() {
            let (id, react) = (message.id.clone(), quick.clone());
            bar = bar.child(tool(("quick", slot), glyph_of(quick)).on_click(cx.listener(move |this, _, _, cx| {
                this.react_toggle(&id, &react, cx);
            })));
        }
        let adds = self.adds(message);
        if !quick.is_empty() || adds {
            bar = bar.child(div().w(px(1.)).h(px(16.)).mx(px(3.)).bg(theme::hairline()));
        }
        if adds {
            let id = message.id.clone();
            bar = bar.child(
                tool("add", icon(Icon::SmilePlus, 16., theme::text_soft()))
                    .when(!phone, |t| t.tooltip(tip("Add reaction", None)))
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.open_picker(&id, event.position(), window, cx);
                    })),
            );
        }
        if can.reply {
            let reply = message.clone();
            bar = bar.child(
                tool("reply", icon(Icon::Reply, 16., theme::text_soft()))
                    .when(!phone, |t| t.tooltip(tip("Reply", None)))
                    .on_click(cx.listener(move |this, _, window, cx| this.reply(&reply, window, cx))),
            );
        }
        if can.thread {
            let root = message.clone();
            bar = bar.child(
                tool("thread", icon(Icon::Thread, 16., theme::text_soft()))
                    .when(!phone, |t| t.tooltip(tip(thread_label(message), None)))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(PaneEvent::OpenThread(root.clone())))),
            );
        }
        if can.edit {
            let edit = message.clone();
            bar = bar.child(
                tool("edit", icon(Icon::Pencil, 16., theme::text_soft()))
                    .when(!phone, |t| t.tooltip(tip("Edit", None)))
                    .on_click(cx.listener(move |this, _, window, cx| this.edit(&edit, window, cx))),
            );
        }
        let more = message.clone();
        bar = bar.child(
            tool("more", icon(Icon::More, 16., theme::text_soft()))
                .when(!phone, |t| t.tooltip(tip("More", None)))
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.open_menu(more.clone(), event.position(), window, cx);
                })),
        );
        Some(bar.into_any_element())
    }

    // Requests.

    fn say(&self, words: String, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx));
    }

    /// Sends a request that changes a message and says so if the network refuses. What it
    /// changed arrives as a `Messages` change, like anything else.
    fn act(&mut self, request: Request, refused: &'static str, cx: &mut Context<Self>) {
        self.act_then(request, refused, |_, _| {}, cx);
    }

    /// The same, doing `then` after a refusal.
    fn act_then(&mut self, request: Request, refused: &'static str, then: impl FnOnce(&mut Pane, &mut Context<Pane>) + 'static, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let why = match answer {
                Ok(Response::Error { code: ErrorCode::Unsupported, .. }) => Some(format!("{refused}: not supported on this network yet.")),
                Ok(Response::Error { detail, .. }) => Some(format!("{refused}: {detail}")),
                Ok(_) => None,
                Err(err) => Some(format!("{refused}: {}", crate::client::trouble_words("send", &err))),
            };
            if let Some(why) = why {
                let _ = this.update(cx, |this, cx| {
                    this.say(why, cx);
                    then(this, cx);
                });
            }
        })
        .detach();
    }

    /// Adds my reaction, or takes it back. One the conversation's rules refuse says why and
    /// is never sent. chatd shows it at once and takes it back if the network refuses, and
    /// then the rules are asked again: they may be what changed.
    pub(super) fn react(&mut self, message_id: &str, emoji: &str, on: bool, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        if on {
            if let Some(why) = self.find(message_id).and_then(|message| self.refusal(&message, emoji)) {
                self.say(format!("Couldn't react: {why}"), cx);
                return;
            }
            // A custom one's id is no glyph to offer again.
            if !emoji.is_ascii() || emoji.starts_with(':') {
                emoji::used(emoji);
            }
        }
        let request = Request::React { conversation, message_id: message_id.to_owned(), emoji: emoji.to_owned(), on };
        self.act_then(request, "Couldn't react", |this, cx| this.ask_reactions(cx), cx);
        cx.notify();
    }

    /// A reaction chosen where the message may already carry it: mine toggles off.
    fn react_toggle(&mut self, message_id: &str, emoji: &str, cx: &mut Context<Self>) {
        let same = |have: &protocol::Reaction| {
            have.emoji == emoji || have.file.as_ref().is_some_and(|file| file.id == emoji) || protocol::bare_emoji(&have.emoji) == protocol::bare_emoji(emoji)
        };
        let mine = self.find(message_id).is_some_and(|message| message.reactions.iter().any(|have| have.mine && same(have)));
        self.react(message_id, emoji, !mine, cx);
    }

    /// Replying and editing happen in the composer, which is the composer's owner's: this
    /// hands it the message and the caret.
    pub(super) fn reply(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) {
        let message = message.clone();
        let _ = self.shell.update(cx, |shell, cx| {
            shell.composer_reply(&message, cx);
            shell.composer_focus(window, cx);
        });
    }

    pub(super) fn edit(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) {
        let message = message.clone();
        let _ = self.shell.update(cx, |shell, cx| {
            shell.composer_edit(&message, cx);
            shell.composer_focus(window, cx);
        });
    }

    /// Deletes for everyone. `at_once` is a Shift-click, which does not ask. A message that
    /// never left is only ever here, so removing it asks nothing either.
    pub(super) fn delete(&mut self, message: &Rc<Message>, at_once: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        if message.state != SendState::Sent {
            let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
            let _ = self.shell.update(cx, |shell, cx| shell.discard_send(conversation, client_id, cx));
            return;
        }
        if !at_once {
            self.confirm = Some(message.clone());
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.confirm = None;
        self.act(Request::Delete { conversation, message_id: message.id.clone() }, "Couldn't delete", cx);
        cx.notify();
    }

    fn copy_text(&mut self, message: &Message, cx: &mut Context<Self>) {
        let selected = self.selection.as_ref().is_some_and(|selection| selection.message == message.id);
        let text = selected.then(|| self.selected_text()).flatten().or_else(|| self.message_text(&message.id));
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    // The menu.

    pub(super) fn open_menu(&mut self, message: Rc<Message>, at: Point<Pixels>,
        window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(cx);
        self.hover = Some(message.id.clone());
        self.menu = Some(Menu { message, at,
            cursor: 0, });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// The actionable rows of the open menu, and which one the keyboard has selected.
    fn menu_state(&self, cx: &Context<Self>) -> Option<(Vec<MenuAct>, usize)> {
        let menu = self.menu.as_ref()?;
        let message = &menu.message;
        let can = self.can(message);
        let quick = if can.unsent || !can.act {
            0
        } else {
            emoji::quick(self.rules(), &message.reactions, 6).len()
        };
        let acts = menu_acts(
            can,
            matches!(message.state, SendState::Failed { .. }),
            quick,
            self.adds(message),
            !message.text.is_empty(),
            self.link_of(message, cx).is_some(),
        );
        let cursor = if acts.is_empty() {
            0
        } else {
            menu.cursor.min(acts.len() - 1)
        };
        Some((acts, cursor))
    }

    /// Does the row the keyboard has selected. A click on Delete passes Shift through as `at_once`.
    fn run_act(
        &mut self,
        act: MenuAct,
        at_once: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let message = menu.message.clone();
        let at = menu.at;
        let emoji = match act {
            MenuAct::Quick(slot) => emoji::quick(self.rules(), &message.reactions, 6)
                .get(slot)
                .cloned(),
            _ => None,
        };
        let link = match act {
            MenuAct::Link | MenuAct::Open => self.link_of(&message, cx),
            _ => None,
        };
        self.close_menu(cx);
        match act {
            MenuAct::Retry => {
                let (Some(conversation), client_id) = (
                    self.key().cloned(),
                    message
                        .client_id
                        .clone()
                        .unwrap_or_else(|| message.id.clone()),
                ) else {
                    return;
                };
                let _ = self.shell.update(cx, |shell, cx| {
                    shell.retry_send(conversation, client_id, cx)
                });
            }
            MenuAct::Copy => self.copy_text(&message, cx),
            MenuAct::Drop => self.delete(&message, true, window, cx),
            MenuAct::Quick(_) => {
                if let Some(emoji) = emoji {
                    self.react_toggle(&message.id, &emoji, cx);
                }
            }
            MenuAct::Reply => self.reply(&message, window, cx),
            MenuAct::Thread => cx.emit(PaneEvent::OpenThread(message)),
            MenuAct::React => self.open_picker(&message.id, at, window, cx),
            MenuAct::Edit => self.edit(&message, window, cx),
            MenuAct::Link => {
                if let Some(link) = link {
                    cx.write_to_clipboard(ClipboardItem::new_string(link));
                }
            }
            MenuAct::Open => {
                if let Some(link) = link {
                    cx.open_url(&link);
                }
            }
            MenuAct::Delete => self.delete(&message, at_once, window, cx),
        }
    }

    /// Where a message leads in the network's own app.
    fn link_of(&self, message: &Message, cx: &Context<Self>) -> Option<String> {
        let shell = self.shell.upgrade()?;
        let shell = shell.read(cx);
        let open = shell.snapshot.as_ref()?.open.as_ref()?;
        // A Slack thread's replies are linked in their channel; a Discord thread is a channel.
        let key = match (&self.open, open.key.network) {
            (Some(mine), protocol::Network::Discord) => &mine.key,
            _ => &open.key,
        };
        message_link(key, open.space.as_deref(), message)
    }

    /// A phone's message menu: a sheet from the bottom with the message it is about, the quick
    /// reactions as a row of 44 pt discs, then the desktop menu's items as 48 pt rows.
    fn menu_sheet(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::gui::composer::picker::{sheet_panel, sheet_scrim};
        use crate::gui::phone::tokens::{self, PhoneType};
        let Menu { message, at, .. } = self.menu.as_ref()?;
        let can = self.can(message);
        let network = self.network().label();
        let row_height = (tokens::scaled(22.) + 26.).max(48.);
        let row = |id: &'static str, icon: &'static str, words: SharedString, danger: bool| {
            let tone = if danger { theme::danger() } else { theme::text() };
            div()
                .id(id)
                .h(px(row_height))
                .mx(px(8.))
                .px(px(12.))
                .rounded(px(theme::R_CARD))
                .flex()
                .items_center()
                .gap(px(14.))
                .cursor_pointer()
                .active(|style| style.bg(theme::pressed()))
                .child(crate::gui::components::glyph(icon, 20., if danger { theme::danger() } else { theme::text_soft() }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(tokens::scaled(16.)))
                        .line_height(px(tokens::scaled(22.)))
                        .text_color(tone)
                        .child(words),
                )
        };
        let rule = || div().flex_none().h(px(1.)).mx(px(20.)).my(px(4.)).bg(theme::hairline());
        let then = |act: fn(&mut Pane, &Rc<Message>, &mut Window, &mut Context<Pane>)| {
            let message = message.clone();
            cx.listener(move |this: &mut Pane, _: &gpui::ClickEvent, window, cx| {
                this.close_menu(cx);
                act(this, &message, window, cx);
            })
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        // What it is about: who said it, when, and its first words.
        let who = if message.outgoing { "You".to_owned() } else { message.sender.name.clone() };
        let said = if message.text.trim().is_empty() { "Attachment".to_owned() } else { message.text.replace('\n', " ") };
        let about = div()
            .flex_none()
            .px(px(20.))
            .pt(px(4.))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().min_w_0().truncate().phone_row().font_weight(theme::SEMIBOLD).child(who))
                    .child(div().flex_none().phone_stamp().text_color(theme::text_faint()).child(time::clock(message.sent))),
            )
            .child(div().line_clamp(2).phone_preview().text_color(theme::text_soft()).child(said));
        if can.unsent {
            if matches!(message.state, SendState::Failed { .. }) {
                rows.push(
                    row("sheet-retry", "icons/refresh-cw.svg", "Retry".into(), false)
                        .on_click(then(|this, message, _, cx| {
                            let (Some(conversation), client_id) = (this.key().cloned(), message.client_id.clone().unwrap_or_else(|| message.id.clone())) else {
                                return;
                            };
                            let _ = this.shell.update(cx, |shell, cx| shell.retry_send(conversation, client_id, cx));
                        }))
                        .into_any_element(),
                );
            }
            rows.push(row("sheet-copy", "icons/copy.svg", "Copy text".into(), false).on_click(then(|this, message, _, cx| this.copy_text(message, cx))).into_any_element());
            rows.push(rule().into_any_element());
            rows.push(
                row("sheet-delete", "icons/trash-2.svg", "Delete".into(), true)
                    .on_click(then(|this, message, window, cx| this.delete(message, true, window, cx)))
                    .into_any_element(),
            );
        } else {
            if can.reply {
                rows.push(row("sheet-reply", "icons/corner-up-left.svg", "Reply".into(), false).on_click(then(|this, message, window, cx| this.reply(message, window, cx))).into_any_element());
            }
            if can.thread {
                rows.push(
                    row("sheet-thread", "icons/message-square.svg", "Open thread".into(), false)
                        .on_click(then(|_, message, _, cx| cx.emit(PaneEvent::OpenThread(message.clone()))))
                        .into_any_element(),
                );
            }
            if can.edit {
                rows.push(row("sheet-edit", "icons/pencil.svg", "Edit".into(), false).on_click(then(|this, message, window, cx| this.edit(message, window, cx))).into_any_element());
            }
            if !message.text.is_empty() {
                rows.push(row("sheet-copy", "icons/copy.svg", "Copy text".into(), false).on_click(then(|this, message, _, cx| this.copy_text(message, cx))).into_any_element());
            }
            if let Some(link) = self.link_of(message, cx) {
                let open = link.clone();
                rows.push(
                    row("sheet-link", "icons/link.svg", "Copy link".into(), false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                        }))
                        .into_any_element(),
                );
                rows.push(
                    row("sheet-open-in", "icons/external-link.svg", format!("Open in {network}").into(), false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            cx.open_url(&open);
                        }))
                        .into_any_element(),
                );
            }
            if can.delete {
                rows.push(rule().into_any_element());
                rows.push(
                    row("sheet-delete", "icons/trash-2.svg", "Delete…".into(), true)
                        .on_click(then(|this, message, window, cx| this.delete(message, false, window, cx)))
                        .into_any_element(),
                );
            }
        }
        // The quick reactions, then the whole picker: one row of discs a thumb can find.
        let quick_row = (can.act).then(|| {
            let adds = self.adds(message);
            // As many 44 pt discs as fit 8 apart inside the 16 pt gutters: six and the picker at
            // 390, five and the picker at 360.
            let fit = sheet_quick(f32::from(window.viewport_size().width), adds);
            let quick = emoji::quick(self.rules(), &message.reactions, fit);
            let disc = |id: ElementId| {
                div()
                    .id(id)
                    .size(px(44.))
                    .flex_none()
                    .rounded_full()
                    .bg(theme::hover())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .active(|style| style.bg(theme::pressed()))
            };
            let mut strip = div().flex_none().px(px(16.)).py(px(6.)).flex().items_center().justify_between();
            for (slot, quick) in quick.into_iter().enumerate() {
                let (id, react) = (message.id.clone(), quick.clone());
                let mine = message.reactions.iter().any(|have| have.mine && protocol::bare_emoji(&have.emoji) == protocol::bare_emoji(&quick));
                strip = strip.child(
                    disc(("sheet-quick", slot).into())
                        .when(mine, |disc| disc.bg(theme::accent_soft()).border_1().border_color(theme::accent()))
                        .child(glyph_of(&quick).text_size(px(22.)).line_height(px(28.)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.react_toggle(&id, &react, cx);
                        })),
                );
            }
            if adds {
                let (id, at) = (message.id.clone(), *at);
                strip = strip.child(
                    disc("sheet-react".into()).child(icon(Icon::SmilePlus, 22., theme::text_soft())).on_click(cx.listener(move |this, _, window, cx| {
                        this.close_menu(cx);
                        this.open_picker(&id, at, window, cx);
                    })),
                );
            }
            strip
        });
        let panel = sheet_panel()
            .max_h(px(f32::from(window.viewport_size().height) * 0.85))
            .child(about)
            .child(div().flex_none().h(px(1.)).bg(theme::hairline()))
            .children(quick_row.map(|strip| div().flex_none().flex().flex_col().child(strip).child(div().h(px(1.)).mt(px(4.)).bg(theme::hairline()))))
            .child(div().id("sheet-rows").flex_1().min_h_0().overflow_y_scroll().pt(px(4.)).flex().flex_col().children(rows));
        let scrim = sheet_scrim(window)
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_menu(cx)))
            .child(panel);
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(fade_in("message-sheet", 160, scrim))).priority(1).into_any_element())
    }

    fn menu_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.menu.as_ref()?;
        let message = shown.message.clone();
        let at = shown.at;
        let can = self.can(&message);
        let network = self.network().label();
        let chosen = self
            .menu_state(cx)
            .and_then(|(acts, cursor)| acts.get(cursor).copied());
        let lit = |act: MenuAct| chosen == Some(act);
        let mut items = menu().w(px(248.));
        let go = |act: MenuAct| {
            cx.listener(move |this: &mut Pane, _: &gpui::ClickEvent, window, cx| {
                this.run_act(
                act, false, window, cx)
            })
        };
        if can.unsent {
            // A row that did not go: send it again, copy it, or drop it.
            if matches!(message.state, SendState::Failed { .. }) {
                items = items.child(menu_item("retry", Some("icons/refresh-cw.svg"), "Retry", None).when(lit(MenuAct::Retry), |row| row.bg (theme::selected())).on_click(go(MenuAct::Retry
                )));
            }
            items = items
                .child(menu_item("copy", Some("icons/copy.svg"), "Copy text", None).when(lit(MenuAct::Copy), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Copy)))
                .child(menu_separator())
                .child(menu_item_danger("delete", Some("icons/trash-2.svg"), "Delete").when(lit(MenuAct::Drop), |row| row.bg(theme::danger_soft()))
                        .on_click(go(MenuAct::Drop)));
        } else {
            let quick = if can.act { emoji::quick(self.rules(), &message.reactions, 6) } else { Vec::new() };
            if !quick.is_empty() {
                let mut strip = div().h(px(36.)).px(px(2.)).flex().items_center().justify_between();
                for (slot, quick) in quick.into_iter().enumerate() {
                    let act = MenuAct::Quick (slot);
                    strip = strip.child(tool(("menu-quick", slot), glyph_of(&quick).text_size(px(18.))).size(px(32.)).when(lit(act), |row| row.bg(theme::selected()))
                            .on_click(go(act
                    )));
                }
                items = items.child(strip).child(menu_separator());
            }
            if can.reply {
                items = items.child(menu_item("reply", Some("icons/corner-up-left.svg"), "Reply", None).when(lit(MenuAct::Reply), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Reply)));
            }
            if can.thread {
                items = items.child(menu_item("thread", Some("icons/message-square.svg"), thread_label(&message), None).when(lit(MenuAct::Thread), |row| row.bg(theme::selected()))
                    .on_click(go(MenuAct::Thread
                )));
            }
            if can.act && self.adds(&message) {
                items = items.child(menu_item("react", Some("icons/smile-plus.svg"), "React", None).when(lit(MenuAct::React), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::React)));
            }
            if can.edit {
                items = items.child(menu_item("edit", Some("icons/pencil.svg"), "Edit", None).when(lit(MenuAct::Edit), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Edit)));
            }
            let selected = self.selection.as_ref().is_some_and(|selection| {
                selection.message == message.id && selection.head.is_some()
            });
            let copy = if selected { "Copy selection" } else { "Copy text" };
            if !message.text.is_empty() {
                items = items
                    .child(menu_separator())
                    .child(menu_item("copy", Some("icons/copy.svg"), copy, Some("⌘C")).when(lit(MenuAct::Copy), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Copy)));
            }
            if self.link_of(&message, cx).is_some() {
                items = items
                    .child(menu_item("link", Some("icons/link.svg"), "Copy link", None).when(lit(MenuAct::Link), |row| row.bg(theme::selected()))
                            .on_click(go(MenuAct::Link)))
                    .child(menu_separator())
                    .child(menu_item("open-in", Some("icons/external-link.svg"), format!("Open in {network}"), None).when(lit(MenuAct::Open), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Open
                    )));
            }
            if can.delete {
                items = items.child(menu_separator()).child(
                    menu_item_danger("delete", Some("icons/trash-2.svg"), "Delete…").when(lit(MenuAct::Delete), |row| row.bg(theme::danger_soft()))
                        .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                        this.run_act(MenuAct::Delete, event.modifiers().shift, window, cx);
                    })),
                );
            }
        }
        let items = items.on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));
        Some(placed(
            "message-menu-scroll",
            Place::Pointer(at),
            Layer::Menu,
            window,
            fade_in("message-menu", 120, items),
        ))
    }

    // The picker.

    pub(super) fn open_picker(&mut self, message_id: &str, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(cx);
        let custom = self.custom_emoji();
        let (rules, account, space) = (self.rules().cloned(), self.key().map(protocol::Key::account), self.space(cx));
        let picker = cx.new(|cx| Picker::new(custom, rules, account, space, window, cx));
        let listening = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            let target = this.picker.as_ref().map(|picking| picking.message.clone());
            if let (PickerEvent::Picked(emoji), Some(target)) = (event, target) {
                this.react_toggle(&target, emoji, cx);
            }
            this.close_picker(cx);
            window.focus(&this.focus, cx);
        });
        self.hover = Some(message_id.to_owned());
        self.picker = Some(Picking { message: message_id.to_owned(), at, picker, _listening: listening });
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            cx.notify();
        }
    }

    /// The custom emoji this conversation has shown, by `:name:`: what its messages and
    /// reactions carry.
    // CONTRACT: no request lists a Space's custom emoji, so the picker offers the ones the
    // messages held have used, which is where their images are known.
    fn custom_emoji(&self) -> Vec<(String, protocol::MediaFile)> {
        let mut found: Vec<(String, protocol::MediaFile)> = Vec::new();
        let mut add = |name: &str, file: &protocol::MediaFile| {
            if name.starts_with(':') && !found.iter().any(|(held, _)| held == name) {
                let mut file = file.clone();
                file.path = file.path.or_else(|| self.known.get(&file.id).cloned());
                found.push((name.to_owned(), file));
            }
        };
        for message in &self.window.messages {
            for span in &message.spans {
                if let (protocol::Style::Emoji { file }, Some(name)) = (&span.style, message.text.get(span.start as usize..span.end as usize)) {
                    add(name, file);
                }
            }
            for reaction in &message.reactions {
                if let Some(file) = &reaction.file {
                    add(&reaction.emoji, file);
                }
            }
        }
        found
    }

    /// A phone's reaction picker: a sheet the window's width, the emoji grid centred in it.
    fn picker_sheet(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::gui::composer::picker::{sheet_panel, sheet_scrim};
        let picking = self.picker.as_ref()?;
        let size = window.viewport_size();
        let (wide, tall) = (f32::from(size.width), f32::from(size.height));
        picking.picker.update(cx, |picker, cx| {
            picker.set_width(wide, cx);
            picker.set_height((tall * 0.6).min(crate::gui::composer::picker::HEIGHT), cx);
        });
        let panel = sheet_panel().child(picking.picker.clone());
        let scrim = sheet_scrim(window).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_picker(cx))).child(panel);
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(fade_in("picker-sheet", 160, scrim))).priority(1).into_any_element())
    }

    fn picker_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picking = self.picker.as_ref()?;
        let place = Place::Pointer(picking.at);
        let viewport = window.viewport_size();
        let room = max_height(
            place,
            (f32::from(viewport.width), f32::from(viewport.height)),
        );
        picking.picker.update(cx, |picker, cx| {
            picker.set_height(room.min(emoji::HEIGHT), cx)
        });
        let panel = popover()
            .p(px(0.))
            .w(px(emoji::WIDTH))
            .max_w(px(emoji::WIDTH))
            .child(picking.picker.clone())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_picker(cx)));
        Some(placed(
            "reaction-picker-scroll",
            place,
            Layer::Popover,
            window,
            fade_in("picker", 120, panel),
        ))
    }

    // The delete dialog.

    fn confirm_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.confirm.as_ref()?;
        let open = self.open.as_ref()?;
        let network = open.key.network.label();
        let consequence = if open.kind.is_direct() {
            format!("It is removed for both of you. {network} cannot bring it back.")
        } else {
            format!("It is removed for everyone in {}. {network} cannot bring it back.", open.title)
        };
        let words = if message.text.is_empty() { "Attachment".to_owned() } else { message.text.clone() };
        let quoted = div()
            .mt(px(12.))
            .p(px(10.))
            .rounded(px(theme::R_CARD))
            .bg(theme::raised())
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().type_ui().font_weight(theme::SEMIBOLD).child(if message.outgoing { "You".to_owned() } else { message.sender.name.clone() }))
                    .child(div().type_micro().text_color(theme::text_faint()).child(time::clock(message.sent))),
            )
            .child(div().line_clamp(2).type_ui().text_color(theme::text_soft()).child(words));
        let target = message.clone();
        // A phone's dialog keeps 16 pt of the window either side.
        let room = f32::from(window.viewport_size().width) - 32.;
        let panel = modal("Delete this message?")
            .w(px(if crate::gui::phone::active() { room.min(400.) } else { 400. }))
            .child(modal_body(consequence))
            .child(quoted)
            .child(
                modal_buttons()
                    .child(button("cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })))
                    .child(button("delete", ButtonKind::Danger, "Delete").on_click(cx.listener(move |this, _, window, cx| {
                        this.delete(&target, true, window, cx);
                    }))),
            )
            // A press on the panel is not a press on the scrim behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let size = window.viewport_size();
        let scrim = div()
            .id("confirm")
            .w(size.width)
            .h(size.height)
            .bg(theme::scrim())
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                }),
            )
            .child(fade_in("confirm-panel", 160, panel));
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(scrim)).priority(1).into_any_element())
    }

    pub(super) fn action_layers(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if crate::gui::phone::active() {
            return [self.menu_sheet(window, cx), self.picker_sheet(window, cx), self.confirm_layer(window, cx)].into_iter().flatten().collect();
        }
        [self.menu_layer(window, cx), self.picker_layer(window, cx), self.confirm_layer(window, cx)].into_iter().flatten().collect()
    }

    /// Keys for whatever floats over the messages. True when the key was theirs.
    /// A menu takes Up, Down, Enter and Escape. Anything else, including Copy, is left alone.
    pub(super) fn action_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        if let Some(message) = self.confirm.clone() {
            match key {
                "enter" => self.delete(&message, true, window, cx),
                "escape" => {
                    self.confirm = None;
                    cx.notify();
                }
                _ => {}
            }
            return true;
        }
        if self.menu.is_some() {
            let bare = !event.keystroke.modifiers.modified();
            match key {
                "escape" => {
                    self.close_menu(cx);
                    self.close_picker(cx);
                    return true;
                }
                "up" | "down" if bare => {
                    let len = self.menu_state(cx).map(|(acts, _)| acts.len()).unwrap_or(0);
                    if let Some(menu) = self.menu.as_mut() {
                        menu.cursor = menu_step(len, menu.cursor, key == "down");
                    }
                    cx.notify();
                    return true;
                }
                "enter" if bare => {
                    if let Some(act) = self
                        .menu_state(cx)
                        .and_then(|(acts, cursor)| acts.get(cursor).copied())
                    {
                        self.run_act(act, false, window, cx);
                    }
                    return true;
                }
                _ => return false,
            }
        }
        if key == "escape" && self.picker.is_some() {
            self.close_picker(cx);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_onto_toolbar_buttons_keeps_the_same_message() {
        let viewport = Bounds::from_corners(point(px(0.), px(0.)), point(px(800.), px(600.)));
        let seen = [
            (0, Bounds::from_corners(point(px(0.), px(100.)), point(px(800.), px(150.)))),
            (1, Bounds::from_corners(point(px(0.), px(150.)), point(px(800.), px(200.)))),
        ];
        let mut held = pointed_row(point(px(400.), px(175.)), viewport, &seen, None);
        assert_eq!(held, Some(1));
        for position in [point(px(700.), px(151.)), point(px(700.), px(140.)), point(px(760.), px(140.))] {
            // The latter points are above the message, in the occluding toolbar.
            held = pointed_row(position, viewport, &seen, held);
            assert_eq!(held, Some(1));
            assert_eq!(pointed_row(position, viewport, &seen, held), held, "stationary redraw/press keeps the button's owner");
        }
        assert_eq!(pointed_row(point(px(100.), px(140.)), viewport, &seen, held), Some(0));
        assert_eq!(pointed_row(point(px(801.), px(140.)), viewport, &seen, held), None);
        assert_eq!(pointed_row(point(px(700.), px(-1.)), viewport, &seen, held), None
        );
    }

    #[test]
    fn a_toolbar_flush_with_the_viewport_moves_into_the_row() {
        let viewport = Bounds::from_corners(point(px(0.), px(0.)), point(px(800.), px(600.)));
        let flush = [(
            0,
            Bounds::from_corners(point(px(0.), px(0.)), point(px(800.), px(80.))),
        )];
        assert_eq!(toolbar_offset(0., 0.), 0.);
        assert_eq!(
            pointed_row(point(px(700.), px(10.)), viewport, &flush, Some(0)),
            Some(0)
        );
        assert_eq!(
            pointed_row(point(px(700.), px(-2.)), viewport, &flush, Some(0)),
            None
        );
        // A few points down: the gap above the row is inside the window and is not the toolbar.
        let near = [(
            0,
            Bounds::from_corners(point(px(0.), px(8.)), point(px(800.), px(80.))),
        )];
        assert_eq!(toolbar_offset(8., 0.), 0.);
        assert_eq!(
            pointed_row(point(px(700.), px(2.)), viewport, &near, Some(0)),
            None
        );
        assert_eq!(
            pointed_row(point(px(700.), px(12.)), viewport, &near, Some(0)),
            Some(0)
        );
    }

    #[test]
    fn a_menu_lists_what_the_message_can_do_in_one_order() {
        let channel = ConversationKind::Channel;
        let said = Message::plain("1", "Mara", "hello", 1, false);
        let on = can(&said, Network::Discord, channel, false);
        assert_eq!(
            menu_acts(on, false, 2, true, true, true),
            vec![
                MenuAct::Quick(0),
                MenuAct::Quick(1),
                MenuAct::Reply,
                MenuAct::Thread,
                MenuAct::React,
                MenuAct::Copy,
                MenuAct::Link,
                MenuAct::Open,
            ]
        );
        let mut mine = Message::plain("2", "Robin", "on it", 2, true);
        mine.state = SendState::Failed {
            reason: "Slow mode".into(),
        };
        let failed = can(&mine, Network::Discord, channel, false);
        assert_eq!(
            menu_acts(failed, true, 3, true, true, false),
            vec![MenuAct::Retry, MenuAct::Copy, MenuAct::Drop]);
    }

    #[test]
    fn the_action_sheet_fits_its_quick_reactions_to_the_phone() {
        assert_eq!(sheet_quick(390., true), 6);
        assert_eq!(sheet_quick(360., true), 5);
        assert_eq!(sheet_quick(360., false), 6);
        assert_eq!(sheet_quick(200., true), 2);
        assert_eq!(sheet_quick(40., true), 1);
    }

    #[test]
    fn a_message_link_is_built_from_ids_where_the_network_has_one() {
        let mut key = protocol::Key { network: Network::Discord, account: "1".into(), id: "C9".into() };
        let said = Message::plain("M3", "Mara", "hello", 1, false);
        assert_eq!(message_link(&key, Some("G7"), &said).as_deref(), Some("https://discord.com/channels/G7/C9/M3"));
        assert_eq!(message_link(&key, None, &said).as_deref(), Some("https://discord.com/channels/@me/C9/M3"));
        key.network = Network::Slack;
        assert_eq!(message_link(&key, None, &said), None, "a Slack link needs its workspace");
        assert_eq!(message_link(&key, Some("T1"), &said).as_deref(), Some("slack://channel?team=T1&id=C9&message=M3"));
        key.network = Network::Telegram;
        assert_eq!(message_link(&key, Some("x"), &said), None);
        // One that never left has no place on the network; an unsupported one names its own.
        key.network = Network::Discord;
        let mut pending = said.clone();
        pending.state = SendState::Pending;
        assert_eq!(message_link(&key, None, &pending), None);
        let mut odd = said.clone();
        odd.kind = MessageKind::Unsupported { link: "https://x.test/1".into() };
        assert_eq!(message_link(&key, None, &odd).as_deref(), Some("https://x.test/1"));
    }

    #[test]
    fn what_a_message_offers_follows_whose_it_is_and_whether_it_left() {
        let channel = ConversationKind::Channel;
        let theirs = Message::plain("1", "Mara", "hello", 1, false);
        let on = can(&theirs, Network::Discord, channel, false);
        assert_eq!((on.act, on.reply, on.edit, on.delete, on.thread), (true, true, false, false, true));
        assert_eq!(thread_label(&theirs), "Reply in thread");
        let discord_dm = can(&theirs, Network::Discord, ConversationKind::Direct, false);
        assert!(discord_dm.reply && !discord_dm.thread, "a Discord direct message stays an inline reply");
        assert!(!can(&theirs, Network::Discord, ConversationKind::Voice, false).thread);
        assert!(!can(&theirs, Network::Discord, ConversationKind::Forum, false).thread);
        // Slack answers in a thread, never inline. The thread can be started before any reply exists:
        // Slack names it by the parent message, in a channel and in a direct message.
        let slack = can(&theirs, Network::Slack, channel, false);
        assert!(!slack.reply);
        assert!(slack.thread);
        assert!(can(&theirs, Network::Slack, ConversationKind::Direct, false).thread);
        assert!(!can(&theirs, Network::Slack, channel, true).thread, "a thread does not open inside a thread");
        assert!(!can(&theirs, Network::Slack, ConversationKind::Thread, false).thread);
        let mut broadcast = theirs.clone();
        broadcast.reply_to = Some(protocol::ReplyTo {
            id: "parent".into(),
            sender: "Ana".into(),
            snippet: "earlier".into(),
            person: None,
            deleted: false,
        });
        assert!(!can(&broadcast, Network::Slack, channel, false).thread, "a channel copy of a reply belongs to its parent's thread");
        let mut pending = theirs.clone();
        pending.state = SendState::Pending;
        assert!(!can(&pending, Network::Slack, channel, false).thread);
        let mut joined = theirs.clone();
        joined.kind = MessageKind::System;
        assert!(!can(&joined, Network::Slack, channel, false).thread);

        // Teams channels thread. A chat stays an inline reply.
        let teams = can(&theirs, Network::Teams, channel, false);
        assert!(teams.reply && teams.thread);
        assert!(can(&theirs, Network::Teams, ConversationKind::Announcement, false).thread);
        assert!(!can(&theirs, Network::Teams, ConversationKind::Direct, false).thread);
        assert!(!can(&theirs, Network::Teams, ConversationKind::Group, false).thread);
        for network in [Network::Telegram, Network::Signal, Network::Messages, Network::Tiny] {
            assert!(!can(&theirs, network, channel, false).thread, "{network:?} has no thread to start");
        }

        let mut mine = Message::plain("2", "Robin", "on it", 2, true);
        let own = can(&mine, Network::Discord, channel, false);
        assert!(own.edit && own.delete && !own.unsent);
        mine.thread = Some(protocol::ThreadSummary { id: "t".into(), replies: 1, last: 3, participants: Vec::new(), unread: 0 });
        assert!(can(&mine, Network::Discord, channel, false).thread);
        assert_eq!(thread_label(&mine), "Open thread");
        assert!(can(&mine, Network::Discord, ConversationKind::Direct, false).thread, "a thread that exists still opens");
        assert!(!can(&mine, Network::Discord, channel, true).thread, "a thread does not open inside a thread");

        // Not on the network yet: no toolbar, nothing to edit, and deleting is local.
        mine.state = SendState::Failed { reason: "Slow mode".into() };
        let failed = can(&mine, Network::Discord, channel, false);
        assert_eq!((failed.act, failed.edit, failed.delete, failed.unsent), (false, false, true, true));

        // A tombstone and a system line say nothing that can be acted on.
        let mut gone = Message::plain("3", "Wren", "", 4, true);
        gone.tombstone();
        assert_eq!(can(&gone, Network::Discord, channel, false), Can { thread: false, ..Can::default() });
        let mut joined = Message::plain("4", "Wren", "joined", 5, false);
        joined.kind = MessageKind::System;
        assert!(!can(&joined, Network::Discord, channel, false).act);
    }
}
