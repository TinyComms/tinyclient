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
use protocol::{Capabilities, ConversationKind, ConversationRights, ErrorCode, Message, MessageKind, Network, ReactionRules, Request, Response, SendState};
use std::rc::Rc;

/// How far the toolbar rises above its row, and how wide the strip it can occupy is.
const TOOLBAR_RISE: f32 = 14.;
const TOOLBAR_REACH: f32 = 320.;
/// How long a phone's press must hold still before the message's actions come up. Long enough
/// that a scroll never reads as one, short enough that the actions feel like an answer.
pub(super) const LONG_PRESS: std::time::Duration = std::time::Duration::from_millis(450);

/// How many reactors a list names before it says how many are kept back.
const REACTORS_SHOWN: usize = 12;

/// The names one reaction's `who` spells, for a tooltip and a sheet row: the first three
/// resolved, "Someone" where a name is not known yet, then how many more. `None` for a
/// reaction the network kept to a count, which names nobody.
pub(super) fn reactor_words(who: &[String], mut name: impl FnMut(&str) -> String) -> Option<String> {
    match who.len() {
        0 => None,
        1 => Some(format!("{} reacted", said(who, 0, &mut name))),
        2 => Some(format!("{} and {} reacted", said(who, 0, &mut name), said(who, 1, &mut name))),
        others => {
            let head: Vec<String> = who.iter().take(3).enumerate().map(|(at, _)| said(who, at, &mut name)).collect();
            Some(format!("{} and {} others reacted", head.join(", "), others - 3))
        }
    }
}

/// One name in the list, or Someone where this person is not known to the shell yet.
fn said(who: &[String], at: usize, name: &mut impl FnMut(&str) -> String) -> String {
    let one = name(&who[at]);
    if one.is_empty() { "Someone".to_owned() } else { one }
}

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
    /// The reaction whose reactors the menu lists, by its place on the message. `None` for
    /// the message's own menu.
    pub reactor: Option<usize>,
}

/// The forward / copy-to picker, open for one message.
pub struct Forwarding {
    pub message: Rc<Message>,
    /// True when the account supports native `Request::Forward`; false for "Copy to…" fallback.
    pub native: bool,
    /// Same-account candidate conversations from chatd's switcher list (`Request::Find`).
    pub candidates: Vec<protocol::Conversation>,
    /// Chosen target conversations, capped at [`protocol::FORWARD_LIMIT`].
    pub selected: Vec<protocol::Key>,
    /// Optional note sent along with the forwarded message.
    pub note: String,
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
    /// It is on the network: the toolbar shows and it can be acted on.
    pub act: bool,
    /// Reactions may be added to this message (`Capabilities::reactions`).
    pub react: bool,
    pub reply: bool,
    pub thread: bool,
    pub edit: bool,
    /// Either delete-for-everyone, delete-for-me, or discarding an unsent message is offered.
    pub delete: bool,
    /// Pin or unpin this message (`Capabilities::pins` and `ConversationRights::pin`).
    pub pin: bool,
    /// Forward this message natively (`Capabilities::forward`).
    pub forward: bool,
    /// May delete this message for everyone (own message or moderator with `rights.delete_others`).
    pub delete_everyone: bool,
    /// May delete this message on this user's side only (Telegram DM/group, Signal, Google Messages).
    pub delete_me: bool,
    /// Google Messages / Slack Later: message can be starred or saved for later.
    pub star: bool,
    /// Slack Later: message can have a reminder set on it.
    pub remind: bool,
    /// Never left: deleting it is local, and it can be sent again.
    pub unsent: bool,
    /// May report this message to the server's moderators (Tiny `abuse_reports`, other people's messages).
    pub report: bool,
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

/// Whether this message can start a thread that does not exist yet, driven by account
/// capabilities (`Capabilities::threads` and `Capabilities::direct_threads`).
pub(super) fn starts_thread(message: &Message, caps: &Capabilities, kind: ConversationKind) -> bool {
    if !caps.threads
        || !thread_root(message)
        || matches!(kind, ConversationKind::Thread | ConversationKind::Voice | ConversationKind::Forum)
    {
        return false;
    }
    caps.direct_threads || matches!(kind, ConversationKind::Channel | ConversationKind::Announcement)
}

/// Whether `message` is still within `caps.edit_window` at unix second `now`.
fn within_edit_window(message: &Message, caps: &Capabilities, now: i64) -> bool {
    caps.edit_window == 0
        || now <= 0
        || message.sent <= 0
        || now.saturating_sub(message.sent) <= caps.edit_window as i64
}

/// `in_thread`: the row is in the thread pane, where a thread cannot be opened again.
/// `now`: current unix seconds, for checking `Capabilities::edit_window` (0 skips the window check).
pub fn can(
    message: &Message,
    caps: &Capabilities,
    rights: ConversationRights,
    kind: ConversationKind,
    in_thread: bool,
    now: i64,
) -> Can {
    can_with_network(message, Network::Tiny, caps, rights, kind, in_thread, now)
}

/// Like [`can`], taking the network into account for delete-for-me scope while deriving
/// default capabilities for `network`.
pub fn can_with_rights(
    message: &Message,
    network: Network,
    kind: ConversationKind,
    in_thread: bool,
    rights: ConversationRights,
) -> Can {
    let caps = Capabilities {
        reply: network != Network::Slack,
        edit: network != Network::Messages,
        delete: network != Network::Messages,
        threads: matches!(network, Network::Discord | Network::Slack | Network::Teams | Network::Tiny),
        direct_threads: matches!(network, Network::Slack),
        pins: network != Network::Messages,
        forward: network != Network::Messages,
        reactions: true,
        ..Capabilities::default()
    };
    can_with_network(message, network, &caps, rights, kind, in_thread, 0)
}

/// Same as [`can_with_rights`], with the conversation's topic (used to tell RCS from SMS on
/// Google Messages) and the account's `edit_window` in seconds (0 = no limit).
pub fn can_in(
    message: &Message,
    network: Network,
    kind: ConversationKind,
    in_thread: bool,
    topic: &str,
    edit_window: u32,
    now: i64,
) -> Can {
    let caps = Capabilities {
        reply: network != Network::Slack,
        edit: true,
        edit_window,
        delete: network != Network::Messages,
        threads: matches!(network, Network::Discord | Network::Slack | Network::Teams | Network::Tiny),
        direct_threads: matches!(network, Network::Slack),
        pins: network != Network::Messages,
        forward: network != Network::Messages,
        reactions: true,
        ..Capabilities::default()
    };
    can_with_network_topic(message, network, &caps, ConversationRights::default(), kind, in_thread, topic, now)
}

/// Full capability + network + rights check used by the conversation pane.
pub fn can_with_network(
    message: &Message,
    network: Network,
    caps: &Capabilities,
    rights: ConversationRights,
    kind: ConversationKind,
    in_thread: bool,
    now: i64,
) -> Can {
    can_with_network_topic(message, network, caps, rights, kind, in_thread, "", now)
}

/// Full capability + network + rights + topic check used by the conversation pane.
pub fn can_with_network_topic(
    message: &Message,
    network: Network,
    caps: &Capabilities,
    rights: ConversationRights,
    kind: ConversationKind,
    in_thread: bool,
    topic: &str,
    now: i64,
) -> Can {
    let sent = message.state == SendState::Sent;
    let said = matches!(message.kind, MessageKind::Normal | MessageKind::Unsupported { .. }) && !message.deleted;
    let act = sent && said;
    let own = message.outgoing && message.kind == MessageKind::Normal && !message.deleted;
    let messages_rcs = network != Network::Messages || topic.starts_with("RCS message");
    let edit = sent
        && !message.deleted
        && !message.saved_history
        && match message.can_edit {
            Some(allowed) => allowed,
            None => own && caps.edit && messages_rcs && within_edit_window(message, caps, now),
        };
    let delete_everyone = !message.deleted
        && sent
        && match message.can_delete {
            Some(allowed) => allowed,
            None => caps.delete && (own || (said && rights.delete_others)),
        };
    let delete_me = act
        && match network {
            Network::Signal | Network::Messages => true,
            Network::Telegram => !matches!(kind, ConversationKind::Channel | ConversationKind::Announcement),
            _ => false,
        };
    let unsent = !sent && message.outgoing;
    let delete = delete_everyone || delete_me || (!message.deleted && unsent && own);
    Can {
        act,
        react: act && caps.reactions,
        reply: act && caps.reply,
        thread: !in_thread && ((sent && message.thread.is_some()) || starts_thread(message, caps, kind)),
        edit,
        delete,
        pin: act && caps.pins && rights.pin,
        forward: act && caps.forward,
        delete_everyone,
        delete_me,
        star: act && (network == Network::Messages || caps.save_messages),
        remind: act && caps.save_messages,
        unsent,
        report: act && network == Network::Tiny && !message.outgoing && caps.abuse_reports,
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
    /// Pin or unpin a message when `can.pin` is true.
    Pin,
    /// Forward natively (`can.forward`) or "Copy to…" fallback (`can.act && !can.forward`).
    Forward,
    Link,
    Open,
    /// A file attachment that is already on disk.
    SaveAs,
    /// Save an animated GIF attachment to the account's saved GIFs (`Capabilities::save_gif`).
    SaveGif,
    /// This message becomes the first unread one.
    Unread,
    /// Enter multi-message selection mode with this message ticked.
    Select,
    /// Star or unstar a message (Google Messages) / Save for later (Slack).
    Star,
    /// Set a reminder on a message ("Remind me…").
    Remind,
    /// Report another person's message on a Tiny server.
    Report,
    /// A sent message: ask, unless the click held Shift.
    Delete,
}

/// Maximum number of messages that can be selected at once (matches Telegram's 100-message cap).
pub const MULTI_SELECT_CAP: usize = 100;

/// Multi-message selection mode state for a conversation pane.
#[derive(Debug, Default, Clone)]
pub struct MultiSelect {
    pub active: bool,
    pub ids: std::collections::HashSet<String>,
    pub anchor: Option<String>,
    /// True when the bulk-delete confirmation dialog is open.
    pub confirm_delete: bool,
}

impl MultiSelect {
    pub fn enter(&mut self, id: &str) {
        self.active = true;
        self.ids.clear();
        self.ids.insert(id.to_owned());
        self.anchor = Some(id.to_owned());
        self.confirm_delete = false;
    }

    pub fn clear(&mut self) {
        self.active = false;
        self.ids.clear();
        self.anchor = None;
        self.confirm_delete = false;
    }

    pub fn count(&self) -> usize {
        self.ids.len()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Toggles one message id, respecting [`MULTI_SELECT_CAP`]. Returns true if selected after toggle.
    pub fn toggle(&mut self, id: &str) -> bool {
        self.active = true;
        if self.ids.remove(id) {
            self.anchor = Some(id.to_owned());
            false
        } else if self.ids.len() < MULTI_SELECT_CAP {
            self.ids.insert(id.to_owned());
            self.anchor = Some(id.to_owned());
            true
        } else {
            false
        }
    }

    /// Selects the inclusive range between `anchor` and `to_id` in `ordered_ids`, capped at [`MULTI_SELECT_CAP`].
    pub fn toggle_range(&mut self, ordered_ids: &[&str], to_id: &str) {
        self.active = true;
        let Some(anchor) = self.anchor.as_deref() else {
            self.toggle(to_id);
            return;
        };
        let Some(a) = ordered_ids.iter().position(|id| *id == anchor) else {
            self.toggle(to_id);
            return;
        };
        let Some(b) = ordered_ids.iter().position(|id| *id == to_id) else {
            return;
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        for id in &ordered_ids[lo..=hi] {
            if self.ids.len() >= MULTI_SELECT_CAP {
                break;
            }
            self.ids.insert((*id).to_owned());
        }
        self.anchor = Some(to_id.to_owned());
    }
}

/// Formats selected messages in chronological order with sender and time for clipboard copy.
pub fn format_selected_messages(messages: &[&Message]) -> String {
    let mut ordered: Vec<&Message> = messages.to_vec();
    ordered.sort_by_key(|m| m.rank());
    ordered
        .into_iter()
        .map(|m| {
            let body = if m.text.is_empty() { "[Attachment]" } else { m.text.as_str() };
            format!("[{}] {}: {}", time::clock(m.sent), m.shown_sender(), body)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns the selected messages that may be deleted in bulk, along with their [`protocol::DeleteScope`].
/// Bulk delete is intentionally omitted on Discord to avoid enforcement triggers.
pub fn deletable_selected<'a>(
    messages: &[&'a Message],
    network: Network,
    kind: ConversationKind,
    in_thread: bool,
    rights: protocol::ConversationRights,
) -> Vec<(&'a Message, protocol::DeleteScope)> {
    if network == Network::Discord {
        return Vec::new();
    }
    let mut ordered: Vec<&'a Message> = messages.to_vec();
    ordered.sort_by_key(|m| m.rank());
    ordered
        .into_iter()
        .filter_map(|m| {
            let allowed = can_with_rights(m, network, kind, in_thread, rights);
            if allowed.delete_everyone {
                Some((m, protocol::DeleteScope::Everyone))
            } else if allowed.delete_me {
                Some((m, protocol::DeleteScope::Me))
            } else {
                None
            }
        })
        .collect()
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
    can_open_link: bool,
    can_save: bool,
    can_save_gif: bool,
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
    if can.pin {
        acts.push(MenuAct::Pin);
    }
    if can.act {
        acts.push(MenuAct::Forward);
    }
    if has_text {
        acts.push(MenuAct::Copy);
    }
    if has_link {
        acts.push(MenuAct::Link);
        if can_open_link {
            acts.push(MenuAct::Open);
        }
    }
    if can_save {
        acts.push(MenuAct::SaveAs);
    }
    if can_save_gif {
        acts.push(MenuAct::SaveGif);
    }
    if !can.unsent {
        acts.push(MenuAct::Unread);
        acts.push(MenuAct::Select);
    }
    if can.star {
        acts.push(MenuAct::Star);
    }
    if can.remind {
        acts.push(MenuAct::Remind);
    }
    if can.report {
        acts.push(MenuAct::Report);
    }
    if can.delete {
        acts.push(MenuAct::Delete);
    }
    acts
}

impl Pane {
    fn network_label(&self) -> &'static str {
        self.open.as_ref().map_or("", |open| open.key.network.label())
    }

    pub(super) fn can(&self, message: &Message) -> Can {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        match self.open.as_ref() {
            Some(open) => {
                let mut rights = open.rights;
                if open.moderate {
                    rights.delete_others = true;
                }
                if open.can_pin {
                    rights.pin = true;
                }
                can_with_network_topic(
                    message,
                    open.key.network,
                    &open.can,
                    rights,
                    open.kind,
                    self.is_thread(),
                    &open.topic,
                    now,
                )
            }
            None => can_with_network(
                message,
                Network::Tiny,
                &Capabilities::default(),
                ConversationRights::default(),
                ConversationKind::Thread,
                self.is_thread(),
                now,
            ),
        }
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
        if self.menu.is_some()
            || self.picker.is_some()
            || self.confirm.is_some()
            || self.action_confirm.is_some()
            || self.open_select.is_some()
            || self.link_ask.is_some()
            || self.viewer.is_some()
        {
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
        let own = self.menu.is_some()
            || self.picker.is_some()
            || self.confirm.is_some()
            || self.action_confirm.is_some()
            || self.open_select.is_some()
            || self.link_ask.is_some()
            || self.viewer.is_some();
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
            || self.action_confirm.as_ref().is_some_and(|pending| pending.message_id == id)
            || self.open_select.as_ref().is_some_and(|open| open.message_id == id)
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
        } else if self.action_confirm.is_some() {
            self.action_confirm = None;
            cx.notify();
        } else if self.open_select.is_some() {
            self.open_select = None;
            cx.notify();
        } else if self.link_ask.is_some() {
            self.close_link_ask(cx);
        } else if self.multi_select.confirm_delete {
            self.multi_select.confirm_delete = false;
            cx.notify();
        } else if self.multi_select.active {
            self.multi_select.clear();
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

    pub(super) fn say(&self, words: String, cx: &mut Context<Self>) {
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
        let request = Request::React { conversation, message_id: message_id.to_owned(), emoji: emoji.to_owned(), on, burst: false };
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
        if message.saved_history { return; }
        let message = message.clone();
        let _ = self.shell.update(cx, |shell, cx| {
            shell.composer_edit(&message, cx);
            shell.composer_focus(window, cx);
        });
    }

    /// Deletes a message. `at_once` is a Shift-click, which does not ask. A message that
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
        let allowed = self.can(message);
        if !at_once || (allowed.delete_everyone && allowed.delete_me) {
            self.confirm = Some(message.clone());
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        let scope = if allowed.delete_everyone {
            protocol::DeleteScope::Everyone
        } else {
            protocol::DeleteScope::Me
        };
        self.delete_scoped(message, scope, cx);
    }

    pub(super) fn delete_scoped(&mut self, message: &Rc<Message>, scope: protocol::DeleteScope, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        self.confirm = None;
        self.act(Request::Delete { conversation, message_id: message.id.clone(), scope }, "Couldn't delete", cx);
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
        self.revisions = None;
        self.hover = Some(message.id.clone());
        self.menu = Some(Menu { message, at,
            cursor: 0, reactor: None });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The people behind one reaction, where the pointer was. When the message payload did not
    /// carry names and the account supports `Capabilities::reactors`, fetches them on demand and
    /// caches them for the session.
    pub(super) fn open_reactors(&mut self, message: Rc<Message>, reaction: usize, at: Point<Pixels>,
        window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(cx);
        self.revisions = None;
        self.hover = Some(message.id.clone());
        let can_reactors = self.open.as_ref().is_some_and(|open| open.can.reactors);
        let needs_fetch = message.reactions.get(reaction).and_then(|r| {
            if r.who.is_empty() && can_reactors {
                let emoji = r.file.as_ref().map_or_else(|| r.emoji.clone(), |file| file.id.clone());
                Some((message.id.clone(), emoji))
            } else {
                None
            }
        });
        if let Some((msg_id, emoji)) = needs_fetch {
            let cache_key = (msg_id.clone(), emoji.clone());
            if !self.fetched_reactors.contains_key(&cache_key) && self.reactors_loading.as_ref() != Some(&cache_key) {
                if let Some(conversation) = self.key().cloned() {
                    self.reactors_loading = Some(cache_key.clone());
                    let request = Request::Reactors {
                        conversation: conversation.clone(),
                        message_id: msg_id,
                        emoji,
                        burst: false,
                        after: None,
                        limit: 100,
                    };
                    cx.spawn(async move |this, cx| {
                        let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.reactors_loading.as_ref() == Some(&cache_key) {
                                this.reactors_loading = None;
                            }
                            if this.key() == Some(&conversation) {
                                if let Ok(Response::Reactors { reactors, more, .. }) = answer {
                                    this.fetched_reactors.insert(cache_key, (reactors, more));
                                }
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
            }
        }
        self.menu = Some(Menu { message, at,
            cursor: 0, reactor: Some(reaction) });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Pins or unpins a message on the network. Never mutates `message.pinned` optimistically:
    /// chatd emits `Change::Messages` and the pins pane reloads once the network confirms.
    fn toggle_pin(&mut self, message: &Message, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        let pinned = !message.pinned;
        let request = Request::Pin {
            conversation: conversation.clone(),
            message_id: message.id.clone(),
            pinned,
            notify: true,
            me_only: false,
        };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(Response::Ok) => {
                        let _ = this.shell.update(cx, |shell, cx| shell.refresh_pins(&conversation, cx));
                    }
                    Ok(Response::Error { detail, .. }) => {
                        this.say(detail, cx);
                    }
                    Err(err) => {
                        let why = crate::client::trouble_words(if pinned { "pin" } else { "unpin" }, &err);
                        this.say(why, cx);
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// The actionable rows of the open menu, and which one the keyboard has selected.
    fn menu_state(&self, cx: &Context<Self>) -> Option<(Vec<MenuAct>, usize)> {
        let menu = self.menu.as_ref()?;
        // A list of reactors has no rows to act on: the keyboard only closes it.
        if menu.reactor.is_some() {
            return None;
        }
        let message = &menu.message;
        let can = self.can(message);
        let quick = if can.unsent || !can.act {
            0
        } else {
            emoji::quick(self.rules(), &message.reactions, 6).len()
        };
        let has_link = self.link_of(message, cx).is_some();
        let acts = menu_acts(
            can,
            matches!(message.state, SendState::Failed { .. }),
            quick,
            self.adds(message),
            !message.text.is_empty(),
            has_link,
            has_link && self.open.as_ref().is_some_and(|open| open.key.network != Network::Tiny),
            self.saveable_file(message).is_some(),
            self.saveable_gif(message).is_some(),
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
            MenuAct::Link => self.link_of(&message, cx),
            MenuAct::Open => self.open_link_of(&message, cx),
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
            MenuAct::Pin => self.toggle_pin(&message, cx),
            MenuAct::Forward => {
                let native = self.can(&message).forward;
                self.open_forward(message, native, window, cx);
            }
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
            MenuAct::SaveAs => {
                if let Some((path, name)) = self.saveable_file(&message) {
                    self.save_attachment_as(path, name, cx);
                }
            }
            MenuAct::SaveGif => {
                if let (Some(open), Some(id)) = (self.open.as_ref(), self.saveable_gif(&message)) {
                    let request = Request::SaveGif {
                        account: open.key.account(),
                        id,
                        unsave: false,
                    };
                    cx.background_executor().spawn(async move { let _ = ask(&request); }).detach();
                }
            }
            MenuAct::Unread => self.mark_message_unread(&message, cx),
            MenuAct::Select => {
                self.multi_select.enter(&message.id);
                window.focus(&self.focus, cx);
                cx.notify();
            }
            MenuAct::Star => {
                if let Some(conversation) = self.key().cloned() {
                    let request = Request::StarMessage {
                        conversation,
                        message_id: message.id.clone(),
                        starred: !message.starred,
                    };
                    cx.background_executor().spawn(async move { let _ = ask(&request); }).detach();
                }
            }
            MenuAct::Remind => {
                if let Some(conversation) = self.key().cloned() {
                    let due = crate::view::now().saturating_add(3600);
                    let request = Request::Remind {
                        conversation,
                        message_id: message.id.clone(),
                        remind_at: Some(due),
                    };
                    cx.background_executor().spawn(async move { let _ = ask(&request); }).detach();
                }
            }
            MenuAct::Report => {
                if let Some(open) = self.open.as_ref() {
                    let account = open.key.account();
                    let id = message.id.clone();
                    let text = (!message.text.is_empty()).then(|| message.text.clone());
                    let _ = self.shell.update(cx, |shell, cx| {
                        shell.tiny_open_report(account, "message", id, String::new(), text, window, cx);
                    });
                }
            }
            MenuAct::Delete => self.delete(&message, at_once, window, cx),
        }
    }

    /// A file attachment already on disk, as `(path, name)`, for Save as….
    fn saveable_file(&self, message: &Message) -> Option<(String, String)> {
        message.attachments.iter().find_map(|attachment| {
            if super::media::shape(attachment) != super::media::Shape::File {
                return None;
            }
            let path = attachment.file.path.clone().or_else(|| self.known.get(&attachment.file.id).cloned())?;
            Some((path, super::media::disk_name(attachment)))
        })
    }

    /// A GIF attachment's network document id when the account supports saving GIFs.
    fn saveable_gif(&self, message: &Message) -> Option<String> {
        if !self.open.as_ref().is_some_and(|open| open.can.save_gif) {
            return None;
        }
        message.attachments.iter().find_map(|attachment| {
            if super::media::shape(attachment) == super::media::Shape::Gif && !attachment.file.id.is_empty() {
                Some(attachment.file.id.clone())
            } else {
                None
            }
        })
    }

    /// Where a message leads in the network's own app (or in tinyclient for Tiny).
    fn link_of(&self, message: &Message, cx: &Context<Self>) -> Option<String> {
        let shell = self.shell.upgrade()?;
        let shell = shell.read(cx);
        let open = shell.snapshot.as_ref()?.open.as_ref()?;
        let key = match (&self.open, open.key.network) {
            (Some(mine), protocol::Network::Discord) => &mine.key,
            _ => &open.key,
        };
        if key.network == Network::Tiny {
            let origin = shell.tiny_origin(&key.account());
            return super::links::message_link(key, origin.as_deref(), message);
        }
        super::links::message_link_for(self.open.as_ref().map(|mine| &mine.key), open, message)
    }

    fn open_link_of(&self, message: &Message, cx: &Context<Self>) -> Option<String> {
        let shell = self.shell.upgrade()?;
        let shell = shell.read(cx);
        let open = shell.snapshot.as_ref()?.open.as_ref()?;
        let key = match (&self.open, open.key.network) {
            (Some(mine), protocol::Network::Discord) => &mine.key,
            _ => &open.key,
        };
        super::links::open_in_app_link(key, open.space.as_deref(), message)
    }

    /// A phone's message menu: a sheet from the bottom with the message it is about, the quick
    /// reactions as a row of 44 pt discs, then the desktop menu's items as 48 pt rows.
    fn menu_sheet(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::gui::composer::picker::{sheet_panel, sheet_scrim};
        use crate::gui::phone::tokens::{self, PhoneType};
        let Menu { message, at, .. } = self.menu.as_ref()?;
        let can = self.can(message);
        let network = self.network_label();
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
        let who = message.shown_sender().to_owned();
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
                if self.open.as_ref().is_some_and(|open| open.key.network != Network::Tiny) {
                    rows.push(
                        row("sheet-open-in", "icons/external-link.svg", format!("Open in {network}").into(), false)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                cx.open_url(&open);
                            }))
                            .into_any_element(),
                    );
                }
            }
            if let Some((path, name)) = self.saveable_file(message) {
                rows.push(
                    row("sheet-save-as", "icons/folder-plus.svg", "Save as…".into(), false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.save_attachment_as(path.clone(), name.clone(), cx);
                        }))
                        .into_any_element(),
                );
            }
            if let Some(gif_id) = self.saveable_gif(message) {
                let account = self.open.as_ref().map(|open| open.key.account());
                rows.push(
                    row("sheet-save-gif", "icons/star.svg", "Save GIF".into(), false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            if let Some(account) = account.clone() {
                                let request = Request::SaveGif { account, id: gif_id.clone(), unsave: false };
                                cx.background_executor().spawn(async move { let _ = ask(&request); }).detach();
                            }
                        }))
                        .into_any_element(),
                );
            }
            if can.pin {
                let label = if message.pinned { "Unpin" } else { "Pin" };
                rows.push(
                    row("sheet-pin", "icons/pin.svg", label.into(), false)
                        .on_click(then(|this, message, _, cx| this.toggle_pin(message, cx)))
                        .into_any_element(),
                );
            }
            if can.act {
                let label = if can.forward { "Forward…" } else { "Copy to…" };
                rows.push(
                    row("sheet-forward", "icons/corner-up-right.svg", label.into(), false)
                        .on_click(then(|this, message, window, cx| {
                            let native = this.can(message).forward;
                            this.open_forward(message.clone(), native, window, cx);
                        }))
                        .into_any_element(),
                );
            }
            if message.state == SendState::Sent {
                rows.push(
                    row("sheet-unread", "icons/mail.svg", "Mark unread".into(), false)
                        .on_click(then(|this, message, _, cx| this.mark_message_unread(message, cx)))
                        .into_any_element(),
                );
                rows.push(
                    row("sheet-select", "icons/check.svg", "Select".into(), false)
                        .on_click(then(|this, message, window, cx| {
                            this.multi_select.enter(&message.id);
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            if can.report {
                rows.push(rule().into_any_element());
                rows.push(
                    row("sheet-report", "icons/triangle-alert.svg", "Report…".into(), true)
                        .on_click(then(|this, message, window, cx| {
                            if let Some(open) = this.open.as_ref() {
                                let account = open.key.account();
                                let id = message.id.clone();
                                let text = (!message.text.is_empty()).then(|| message.text.clone());
                                let _ = this.shell.update(cx, |shell, cx| {
                                    shell.tiny_open_report(account, "message", id, String::new(), text, window, cx);
                                });
                            }
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
        // Who each reaction came from, where the network said. A chip that is only a count
        // names nobody and is left out.
        let reacted_by: Vec<AnyElement> = message
            .reactions
            .iter()
            .filter(|one| !one.who.is_empty())
            .take(3)
            .map(|one| {
                let names =
                    reactor_words(&one.who, |id| match self.person(id) {
                        Some(person) => self.who(&person, cx).0,
                        None => String::new(),
                    })
                    .unwrap_or_default();
                div()
                    .flex_none()
                    .px(px(20.))
                    .py(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(glyph_of(&one.emoji).text_size(px(tokens::scaled(18.))).line_height(px(tokens::scaled(24.))))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .phone_preview()
                            .text_color(theme::text_soft())
                            .child(names),
                    )
                    .into_any_element()
            })
            .collect();
        let panel = sheet_panel()
            .max_h(px(f32::from(window.viewport_size().height) * 0.85))
            .child(about)
            .child(div().flex_none().h(px(1.)).bg(theme::hairline()))
            .children(reacted_by)
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
        if let Some(reactor) = shown.reactor {
            return self.reactors_layer(&message, reactor, at, window, cx);
        }
        let can = self.can(&message);
        let network = self.network_label();
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
            if can.pin {
                let label = if message.pinned { "Unpin" } else { "Pin" };
                items = items.child(
                    menu_item("pin", Some("icons/pin.svg"), label, None)
                        .when(lit(MenuAct::Pin), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Pin)),
                );
            }
            if can.act {
                let label = if can.forward { "Forward…" } else { "Copy to…" };
                items = items.child(
                    menu_item("forward", Some("icons/corner-up-right.svg"), label, None)
                        .when(lit(MenuAct::Forward), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Forward)),
                );
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
                            .on_click(go(MenuAct::Link)));
                if self.open.as_ref().is_some_and(|open| open.key.network != Network::Tiny) {
                    items = items
                        .child(menu_separator())
                        .child(menu_item("open-in", Some("icons/external-link.svg"), format!("Open in {network}"), None).when(lit(MenuAct::Open), |row| row.bg(theme::selected()))
                            .on_click(go(MenuAct::Open)));
                }
            }
            if self.saveable_file(&message).is_some() {
                items = items.child(
                    menu_item("save-as", Some("icons/folder-plus.svg"), "Save as…", None)
                        .when(lit(MenuAct::SaveAs), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::SaveAs)),
                );
            }
            if self.saveable_gif(&message).is_some() {
                items = items.child(
                    menu_item("save-gif", Some("icons/star.svg"), "Save GIF", None)
                        .when(lit(MenuAct::SaveGif), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::SaveGif)),
                );
            }
            if message.state == SendState::Sent {
                items = items.child(
                    menu_item("unread", Some("icons/mail.svg"), "Mark unread", None)
                        .when(lit(MenuAct::Unread), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Unread)),
                );
                items = items.child(
                    menu_item("select", Some("icons/check.svg"), "Select", None)
                        .when(lit(MenuAct::Select), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Select)),
                );
            }
            if can.star {
                let is_slack = self.open.as_ref().is_some_and(|o| o.key.network == Network::Slack);
                let label = match (is_slack, message.starred) {
                    (true, true) => "Remove from Later",
                    (true, false) => "Save for later",
                    (false, true) => "Unstar message",
                    (false, false) => "Star message",
                };
                items = items.child(
                    menu_item("star", Some("icons/star.svg"), label, None)
                        .when(lit(MenuAct::Star), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Star)),
                );
            }
            if can.remind {
                items = items.child(
                    menu_item("remind", Some("icons/clock.svg"), "Remind me in 1 hour", None)
                        .when(lit(MenuAct::Remind), |row| row.bg(theme::selected()))
                        .on_click(go(MenuAct::Remind)),
                );
            }
            if can.report {
                items = items.child(menu_separator()).child(
                    menu_item_danger("report", Some("icons/triangle-alert.svg"), "Report…")
                        .when(lit(MenuAct::Report), |row| row.bg(theme::danger_soft()))
                        .on_click(go(MenuAct::Report)),
                );
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

    /// Who one reaction came from, where the pointer was: the names the network sent with the
    /// message, in its own order, then how many are kept back. A row opens the person's card;
    /// a person the shell does not know yet reads Someone and opens nothing.
    fn reactors_layer(&self, message: &Rc<Message>, reactor: usize, at: Point<Pixels>,
        window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reaction = message.reactions.get(reactor)?;
        let mut list = menu().w(px(248.));
        if !reaction.who.is_empty() {
            for (ix, id) in reaction.who.iter().enumerate() {
                if ix == REACTORS_SHOWN {
                    list = list.child(menu_separator()).child(menu_item(
                        "reactors-more",
                        None,
                        format!("and {} more", reaction.who.len() - ix),
                        None,
                    ));
                    break;
                }
                let one = match self.person(id) {
                    Some(person) => self.who(&person, cx).0,
                    None => String::new(),
                };
                let (label, person) = if one.is_empty() { ("Someone".to_owned(), None) } else { (one, self.person(id)) };
                // Their card, where this person is known by name.
                let open = person.map(|person| {
                    cx.listener(move |this: &mut Pane, _: &gpui::ClickEvent, window, cx| {
                        this.close_menu(cx);
                        let at = window.mouse_position();
                        let _ = this.shell.update(cx, |shell, cx| shell.open_profile(person.clone(), at, cx));
                    })
                });
                list = list.child(
                    menu_item(("reactor", ix), None, label, None).when_some(open, |row, open| row.on_click(open)),
                );
            }
        } else {
            let emoji_key = reaction.file.as_ref().map_or_else(|| reaction.emoji.clone(), |file| file.id.clone());
            let cache_key = (message.id.clone(), emoji_key);
            if let Some((reactors, more)) = self.fetched_reactors.get(&cache_key) {
                if reactors.is_empty() {
                    list = list.child(menu_item("reactors-empty", None, "No reactions found", None));
                } else {
                    for (ix, person) in reactors.iter().enumerate() {
                        if ix == REACTORS_SHOWN {
                            let extra = reactors.len() - ix;
                            let suffix = if *more { format!("and {extra}+ more") } else { format!("and {extra} more") };
                            list = list.child(menu_separator()).child(menu_item("reactors-more", None, suffix, None));
                            break;
                        }
                        let label = if person.name.is_empty() { "Someone".to_owned() } else { person.name.clone() };
                        let key = person.key.clone();
                        list = list.child(menu_item(("reactor", ix), None, label, None).on_click(cx.listener(
                            move |this: &mut Pane, _: &gpui::ClickEvent, window, cx| {
                                this.close_menu(cx);
                                let at = window.mouse_position();
                                let _ = this.shell.update(cx, |shell, cx| shell.open_profile(key.clone(), at, cx));
                            },
                        )));
                    }
                    if reactors.len() <= REACTORS_SHOWN && *more {
                        list = list.child(menu_separator()).child(menu_item("reactors-more", None, "and more…", None));
                    }
                }
            } else if self.reactors_loading.as_ref() == Some(&cache_key) {
                list = list.child(menu_item("reactors-loading", None, "Loading…", None));
            } else {
                list = list.child(menu_item("reactors-empty", None, "No reactions found", None));
            }
        }
        let list = list.on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));
        Some(placed(
            "reactors-menu-scroll",
            Place::Pointer(at),
            Layer::Menu,
            window,
            fade_in("reactors-menu", 120, list),
        ))
    }

    /// Earlier wordings of the message whose edited mark was clicked.
    fn revisions_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.revisions.as_ref()?;
        let at = open.at;
        let lines: Vec<String> = if !open.ready {
            vec!["Looking…".to_owned()]
        } else if open.rows.is_empty() {
            vec!["Nothing earlier is kept.".to_owned()]
        } else {
            open.rows.iter().map(|row| super::revision_line(row.at, &row.text)).collect()
        };
        let panel = popover()
            .id("earlier-versions")
            .w(px(280.))
            .max_h(px(280.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(lines.into_iter().map(|line| div().type_small().text_color(theme::text()).child(line)))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.revisions = None;
                cx.notify();
            }));
        Some(placed("revisions", Place::Pointer(at), Layer::Menu, window, panel))
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
        let allowed = self.can(message);
        let network = open.key.network.label();
        let sender = message.shown_sender();
        let heading = if !message.outgoing {
            format!("Delete {sender}'s message?")
        } else {
            "Delete this message?".to_owned()
        };
        let consequence = if allowed.delete_me && !allowed.delete_everyone {
            format!("It is removed on your side only. Other people on {network} still see it.")
        } else if !message.outgoing {
            format!("This removes {sender}'s message for everyone in {}. {network} cannot bring it back.", open.title)
        } else if open.kind.is_direct() {
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
                    .child(div().type_ui().font_weight(theme::SEMIBOLD).child(sender.to_owned()))
                    .child(div().type_micro().text_color(theme::text_faint()).child(time::clock(message.sent))),
            )
            .child(div().line_clamp(2).type_ui().text_color(theme::text_soft()).child(words));
        let mut buttons = modal_buttons().child(button("cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
            this.confirm = None;
            cx.notify();
        })));
        if allowed.delete_me {
            let target_me = message.clone();
            let kind = if allowed.delete_everyone { ButtonKind::Secondary } else { ButtonKind::Danger };
            buttons = buttons.child(button("delete-me", kind, "Delete for me").on_click(cx.listener(move |this, _, _, cx| {
                this.delete_scoped(&target_me, protocol::DeleteScope::Me, cx);
            })));
        }
        if allowed.delete_everyone {
            let target_all = message.clone();
            let label = if allowed.delete_me { "Delete for everyone" } else { "Delete" };
            buttons = buttons.child(button("delete", ButtonKind::Danger, label).on_click(cx.listener(move |this, _, _, cx| {
                this.delete_scoped(&target_all, protocol::DeleteScope::Everyone, cx);
            })));
        }
        // A phone's dialog keeps 16 pt of the window either side.
        let room = f32::from(window.viewport_size().width) - 32.;
        let panel = modal(heading)
            .w(px(if crate::gui::phone::active() { room.min(400.) } else { 400. }))
            .child(modal_body(consequence))
            .child(quoted)
            .child(buttons)
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

    /// Opens the Forward / Copy-to conversation picker, loading same-account conversations
    /// from chatd's quick switcher list (`Request::Find`).
    fn open_forward(&mut self, message: Rc<Message>, native: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.key().cloned() else {
            return;
        };
        let account = from.account();
        self.forward = Some(Forwarding {
            message,
            native,
            candidates: Vec::new(),
            selected: Vec::new(),
            note: String::new(),
        });
        window.focus(&self.focus, cx);
        cx.notify();
        let request = Request::Find {
            network: Some(from.network),
            query: String::new(),
            limit: 60,
        };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            if let Ok(Response::Conversations { conversations, .. }) = answer {
                let same_account: Vec<protocol::Conversation> = conversations
                    .into_iter()
                    .filter(|c| c.key.account() == account && !c.read_only)
                    .collect();
                let _ = this.update(cx, |this, cx| {
                    if let Some(fwd) = this.forward.as_mut() {
                        fwd.candidates = same_account;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn toggle_forward_target(&mut self, key: protocol::Key, cx: &mut Context<Self>) {
        let Some(fwd) = self.forward.as_mut() else {
            return;
        };
        if let Some(ix) = fwd.selected.iter().position(|k| *k == key) {
            fwd.selected.remove(ix);
        } else if fwd.selected.len() < protocol::FORWARD_LIMIT {
            fwd.selected.push(key);
        }
        cx.notify();
    }

    fn submit_forward(&mut self, cx: &mut Context<Self>) {
        let Some(from) = self.key().cloned() else {
            return;
        };
        let Some(fwd) = self.forward.take() else {
            return;
        };
        cx.notify();
        if fwd.selected.is_empty() {
            return;
        }
        if fwd.native {
            let request = Request::Forward {
                from,
                message_ids: vec![fwd.message.id.clone()],
                to: fwd.selected,
                note: fwd.note,
            };
            cx.spawn(async move |this, cx| {
                let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
                let _ = this.update(cx, |this, cx| match answer {
                    Ok(Response::Forwarded { results }) => {
                        let failed: Vec<String> = results.into_iter().filter_map(|r| r.error).collect();
                        if let Some(first) = failed.first() {
                            this.say(format!("Couldn't forward: {first}"), cx);
                        }
                    }
                    Ok(Response::Error { detail, .. }) => this.say(detail, cx),
                    Err(err) => this.say(crate::client::trouble_words("forward", &err), cx),
                    _ => {}
                });
            })
            .detach();
        } else {
            // "Copy to…" fallback: re-send text and cached on-disk files as a new message per target.
            let files: Vec<protocol::OutgoingFile> = fwd
                .message
                .attachments
                .iter()
                .filter_map(|att| {
                    let path = att.file.path.clone().or_else(|| self.known.get(&att.file.id).cloned())?;
                    Some(protocol::OutgoingFile { path, name: att.name.clone(), ..protocol::OutgoingFile::default() })
                })
                .collect();
            let text = if fwd.note.trim().is_empty() {
                fwd.message.text.clone()
            } else if fwd.message.text.is_empty() {
                fwd.note.trim().to_owned()
            } else {
                format!("{}\n{}", fwd.note.trim(), fwd.message.text)
            };
            let targets = fwd.selected;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            cx.spawn(async move |this, cx| {
                for (ix, target) in targets.into_iter().enumerate() {
                    let req = Request::Send {
                        conversation: target,
                        client_id: format!("copy-{now}-{ix}"),
                        text: text.clone(),
                        reply_to: None,
                        with: protocol::Outgoing { files: files.clone(), ..protocol::Outgoing::default() },
                    };
                    let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
                    if let Ok(Response::Error { detail, .. }) = answer {
                        let _ = this.update(cx, |this, cx| this.say(detail, cx));
                    }
                }
            })
            .detach();
        }
    }

    fn forward_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let fwd = self.forward.as_ref()?;
        let heading = if fwd.native { "Forward message" } else { "Copy message to…" };
        let count = fwd.selected.len();
        let action_label = if fwd.native {
            if count > 1 { format!("Forward ({count})") } else { "Forward".to_owned() }
        } else if count > 1 {
            format!("Copy ({count})")
        } else {
            "Copy".to_owned()
        };
        let rows = fwd.candidates.iter().take(40).map(|conv| {
            let key = conv.key.clone();
            let picked = fwd.selected.contains(&key);
            let title = if conv.title.is_empty() { conv.key.id.clone() } else { conv.title.clone() };
            div()
                .id(ElementId::Name(format!("fwd-{}", conv.key.id).into()))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .when(picked, |d| d.bg(theme::selected()))
                .hover(|s| s.bg(theme::hover()))
                .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(title))
                .when(picked, |d| d.child(div().type_micro().text_color(theme::accent_text()).child("Selected")))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_forward_target(key.clone(), cx)))
        });
        let panel = modal(heading)
            .w(px(400.))
            .child(modal_body(format!(
                "Choose up to {} conversations on this account.",
                protocol::FORWARD_LIMIT
            )))
            .child(
                div()
                    .id("forward-candidates")
                    .mt(px(8.))
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows),
            )
            .child(
                modal_buttons()
                    .child(button("forward-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.forward = None;
                        cx.notify();
                    })))
                    .child(
                        button("forward-go", ButtonKind::Primary, action_label)
                            .when(count > 0, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_forward(cx)))),
                    ),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let size = window.viewport_size();
        let scrim = div()
            .id("forward-scrim")
            .w(size.width)
            .h(size.height)
            .bg(theme::scrim())
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.forward = None;
                cx.notify();
            }))
            .child(fade_in("forward-panel", 160, panel));
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(scrim)).priority(1).into_any_element())
    }

    fn select_menu_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.open_select.as_ref()?;
        let mut list = menu().w(px(240.));
        for (ix, opt) in open.options.iter().enumerate() {
            let active = open.selected.as_deref() == Some(opt.value.as_str());
            let conversation = open.conversation.clone();
            let msg_id = open.message_id.clone();
            let block_id = open.block_id.clone();
            let action_id = open.action_id.clone();
            let element_type = open.element_type.clone();
            let val = opt.value.clone();
            let values = if element_type.starts_with("multi_") { vec![val.clone()] } else { Vec::new() };
            let confirm = open.confirm.clone();
            let label = if active { format!("✓ {}", opt.label) } else { opt.label.clone() };
            list = list.child(
                menu_item(("ctrl-opt", ix), None, label, None)
                    .when(active, |row| row.bg(theme::selected()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.trigger_control(
                            conversation.clone(),
                            msg_id.clone(),
                            block_id.clone(),
                            action_id.clone(),
                            element_type.clone(),
                            val.clone(),
                            values.clone(),
                            confirm.clone(),
                            cx,
                        );
                    })),
            );
        }
        let list = list.on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.open_select = None;
            cx.notify();
        }));
        Some(placed(
            "control-select-menu",
            Place::Pointer(open.at),
            Layer::Menu,
            window,
            fade_in("control-select", 120, list),
        ))
    }

    fn action_confirm_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.action_confirm.as_ref()?;
        let title = if pending.confirm.title.is_empty() {
            "Confirm action".to_owned()
        } else {
            pending.confirm.title.clone()
        };
        let body = pending.confirm.text.clone();
        let deny = if pending.confirm.deny.is_empty() { "Cancel".to_owned() } else { pending.confirm.deny.clone() };
        let ok = if pending.confirm.confirm.is_empty() { "Confirm".to_owned() } else { pending.confirm.confirm.clone() };
        let to_run = pending.clone();
        let room = f32::from(window.viewport_size().width) - 32.;
        let panel = modal(title)
            .w(px(if crate::gui::phone::active() { room.min(400.) } else { 400. }))
            .when(!body.is_empty(), |m| m.child(modal_body(body)))
            .child(
                modal_buttons()
                    .child(button("ctrl-confirm-cancel", ButtonKind::Secondary, deny).on_click(cx.listener(|this, _, _, cx| {
                        this.action_confirm = None;
                        cx.notify();
                    })))
                    .child(button("ctrl-confirm-ok", ButtonKind::Primary, ok).on_click(cx.listener(move |this, _, window, cx| {
                        this.action_confirm = None;
                        if let Some(url) = &to_run.open_url {
                            if this.key() == Some(&to_run.conversation) {
                                this.open_external_link(url, window, cx);
                            }
                            cx.notify();
                        } else {
                            this.execute_control(
                                to_run.conversation.clone(),
                                to_run.message_id.clone(),
                                to_run.block_id.clone(),
                                to_run.action_id.clone(),
                                to_run.element_type.clone(),
                                to_run.value.clone(),
                                to_run.values.clone(),
                                cx,
                            );
                        }
                    }))),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let size = window.viewport_size();
        let scrim = div()
            .id("action-confirm-scrim")
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
                    this.action_confirm = None;
                    cx.notify();
                }),
            )
            .child(fade_in("action-confirm-panel", 160, panel));
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(scrim)).priority(1).into_any_element())
    }

    pub(super) fn action_layers(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if crate::gui::phone::active() {
            return [
                self.menu_sheet(window, cx),
                self.select_menu_layer(window, cx),
                self.revisions_layer(window, cx),
                self.picker_sheet(window, cx),
                self.confirm_layer(window, cx),
                self.action_confirm_layer(window, cx),
                self.bulk_confirm_layer(window, cx),
                self.forward_layer(window, cx),
                self.link_layer(window, cx),
            ]
            .into_iter()
            .flatten()
            .collect();
        }
        [
            self.menu_layer(window, cx),
            self.select_menu_layer(window, cx),
            self.revisions_layer(window, cx),
            self.picker_layer(window, cx),
            self.confirm_layer(window, cx),
            self.action_confirm_layer(window, cx),
            self.bulk_confirm_layer(window, cx),
            self.forward_layer(window, cx),
            self.link_layer(window, cx),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(super) fn toggle_selected_row(&mut self, id: &str, shift: bool, cx: &mut Context<Self>) {
        if shift {
            let ordered: Vec<&str> = self.window.messages.iter().map(|m| m.id.as_str()).collect();
            self.multi_select.toggle_range(&ordered, id);
        } else {
            self.multi_select.toggle(id);
        }
        if self.multi_select.count() == 0 {
            self.multi_select.clear();
        }
        cx.notify();
    }

    fn selected_messages(&self) -> Vec<&Message> {
        self.window
            .messages
            .iter()
            .filter(|m| self.multi_select.contains(&m.id))
            .map(Rc::as_ref)
            .collect()
    }

    pub(super) fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let msgs = self.selected_messages();
        if msgs.is_empty() {
            return;
        }
        let text = format_selected_messages(&msgs);
        let count = msgs.len();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.multi_select.clear();
        let _ = self.shell.update(cx, |shell, cx| {
            shell.toast(Some(("icons/check.svg", theme::text_soft())), format!("Copied {count} messages"), cx);
        });
        cx.notify();
    }

    pub(super) fn delete_selected_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let conversation = open.key.clone();
        let kind = open.kind;
        let rights = open.rights;
        let targets: Vec<(String, protocol::DeleteScope)> = deletable_selected(
            &self.selected_messages(),
            conversation.network,
            kind,
            self.is_thread(),
            rights,
        )
        .into_iter()
        .map(|(m, scope)| (m.id.clone(), scope))
        .collect();
        self.multi_select.clear();
        cx.notify();
        if targets.is_empty() {
            return;
        }
        // Send deletes paced off-thread so we never burst the network.
        cx.spawn(async move |_, cx| {
            for (ix, (message_id, scope)) in targets.into_iter().enumerate() {
                if ix > 0 {
                    cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
                }
                let req = Request::Delete { conversation: conversation.clone(), message_id, scope };
                let _ = cx.background_executor().spawn(async move { crate::client::ask(&req) }).await;
            }
        })
        .detach();
    }

    pub(super) fn selection_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.multi_select.active {
            return None;
        }
        let open = self.open.as_ref()?;
        let count = self.multi_select.count();
        let deletable = deletable_selected(
            &self.selected_messages(),
            open.key.network,
            open.kind,
            self.is_thread(),
            open.rights,
        )
        .len();
        let mut bar = div()
            .id("selection-bar")
            .absolute()
            .left(px(12.))
            .right(px(12.))
            .bottom(px(12.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(theme::R_CARD))
            .bg(theme::raised())
            .border_1()
            .border_color(super::tokens::border_strong())
            .flex()
            .items_center()
            .gap(px(8.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex_1().type_ui().font_weight(theme::SEMIBOLD).child(format!("{count} selected")))
            .child(
                button("sel-copy", ButtonKind::Secondary, "Copy")
                    .h(px(28.))
                    .px(px(10.))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_selected(cx))),
            );
        if deletable > 0 {
            bar = bar.child(
                button("sel-delete", ButtonKind::Danger, format!("Delete ({deletable})"))
                    .h(px(28.))
                    .px(px(10.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.multi_select.confirm_delete = true;
                        cx.notify();
                    })),
            );
        }
        bar = bar.child(
            button("sel-cancel", ButtonKind::Ghost, "Cancel")
                .h(px(28.))
                .px(px(8.))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.multi_select.clear();
                    cx.notify();
                })),
        );
        Some(bar.into_any_element())
    }

    fn bulk_confirm_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.multi_select.confirm_delete {
            return None;
        }
        let open = self.open.as_ref()?;
        let deletable = deletable_selected(
            &self.selected_messages(),
            open.key.network,
            open.kind,
            self.is_thread(),
            open.rights,
        )
        .len();
        if deletable == 0 {
            return None;
        }
        let room = f32::from(window.viewport_size().width) - 32.;
        let panel = modal(format!("Delete {deletable} messages?"))
            .w(px(if crate::gui::phone::active() { room.min(400.) } else { 400. }))
            .child(modal_body(format!("This removes {deletable} selected messages. This cannot be undone.")))
            .child(
                modal_buttons()
                    .child(button("bulk-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.multi_select.confirm_delete = false;
                        cx.notify();
                    })))
                    .child(button("bulk-delete", ButtonKind::Danger, format!("Delete {deletable}")).on_click(cx.listener(|this, _, _, cx| {
                        this.delete_selected_confirmed(cx);
                    }))),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let size = window.viewport_size();
        let scrim = div()
            .id("bulk-confirm")
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
                    this.multi_select.confirm_delete = false;
                    cx.notify();
                }),
            )
            .child(fade_in("bulk-confirm-panel", 160, panel));
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(scrim)).priority(1).into_any_element())
    }

    /// Keys for whatever floats over the messages. True when the key was theirs.
    /// A menu takes Up, Down, Enter and Escape. Anything else, including Copy, is left alone.
    pub(super) fn action_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        if self.link_key(event, cx) {
            return true;
        }
        if key == "escape" && self.open_select.is_some() {
            self.open_select = None;
            cx.notify();
            return true;
        }
        if key == "escape" && self.action_confirm.is_some() {
            self.action_confirm = None;
            cx.notify();
            return true;
        }
        if key == "escape" && self.forward.is_some() {
            self.forward = None;
            cx.notify();
            return true;
        }
        if key == "escape" && self.multi_select.confirm_delete {
            self.multi_select.confirm_delete = false;
            cx.notify();
            return true;
        }
        if key == "escape" && self.multi_select.active {
            self.multi_select.clear();
            cx.notify();
            return true;
        }
        if key == "escape" && self.revisions.is_some() {
            self.revisions = None;
            cx.notify();
            return true;
        }
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
    fn a_reaction_names_who_it_can_and_a_count_only_one_names_nobody() {
        let known = |id: &str| if id == "u-ghost" { String::new() } else { format!("name of {id}") };
        // A count-only reaction names nobody: no list, no words.
        assert_eq!(reactor_words(&[], known), None);
        assert_eq!(reactor_words(&["u-1".to_owned()], known).as_deref(), Some("name of u-1 reacted"));
        assert_eq!(
            reactor_words(&["u-1".to_owned(), "u-2".to_owned()], known).as_deref(),
            Some("name of u-1 and name of u-2 reacted")
        );
        assert_eq!(
            reactor_words(&["u-1".to_owned(), "u-2".to_owned(), "u-3".to_owned(), "u-4".to_owned(), "u-5".to_owned()], known).as_deref(),
            Some("name of u-1, name of u-2, name of u-3 and 2 others reacted")
        );
        // A person the shell does not know yet is Someone, never a bare id.
        assert_eq!(reactor_words(&["u-ghost".to_owned()], known).as_deref(), Some("Someone reacted"));
        assert_eq!(
            reactor_words(&["u-1".to_owned(), "u-ghost".to_owned()], known).as_deref(),
            Some("name of u-1 and Someone reacted")
        );
    }

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

    fn discord_caps() -> Capabilities {
        Capabilities {
            reply: true,
            edit: true,
            delete: true,
            threads: true,
            pins: true,
            forward: true,
            polls: Some(protocol::PollCapabilities::default()),
            reactions: true,
            ..Capabilities::default()
        }
    }

    fn slack_caps() -> Capabilities {
        Capabilities {
            edit: true,
            delete: true,
            threads: true,
            direct_threads: true,
            pins: true,
            forward: true,
            reactions: true,
            ..Capabilities::default()
        }
    }

    fn teams_caps() -> Capabilities {
        Capabilities {
            reply: true,
            edit: true,
            delete: true,
            threads: true,
            pins: true,
            forward: true,
            reactions: true,
            ..Capabilities::default()
        }
    }

    fn telegram_caps() -> Capabilities {
        Capabilities {
            reply: true,
            edit: true,
            delete: true,
            edit_window: 48 * 3_600,
            pins: true,
            forward: true,
            polls: Some(protocol::PollCapabilities::default()),
            reactions: true,
            ..Capabilities::default()
        }
    }

    fn messages_caps() -> Capabilities {
        Capabilities {
            reactions: true,
            ..Capabilities::default()
        }
    }

    fn tiny_caps() -> Capabilities {
        Capabilities {
            reply: true,
            edit: true,
            delete: true,
            pins: true,
            reactions: true,
            ..Capabilities::default()
        }
    }

    #[test]
    fn a_menu_lists_what_the_message_can_do_in_one_order() {
        let channel = ConversationKind::Channel;
        let rights = ConversationRights::default();
        let said = Message::plain("1", "Mara", "hello", 1, false);
        let on = can(&said, &discord_caps(), rights, channel, false, 10);
        assert_eq!(
            menu_acts(on, false, 2, true, true, true, true, false, false),
            vec![
                MenuAct::Quick(0),
                MenuAct::Quick(1),
                MenuAct::Reply,
                MenuAct::Thread,
                MenuAct::React,
                MenuAct::Forward,
                MenuAct::Copy,
                MenuAct::Link,
                MenuAct::Open,
                MenuAct::Unread,
                MenuAct::Select,
            ]
        );
        // Tiny has Copy link without Open in Tiny.
        let tiny_acts = menu_acts(on, false, 0, true, false, true, false, false, false);
        assert!(tiny_acts.contains(&MenuAct::Link) && !tiny_acts.contains(&MenuAct::Open));
        let mut mine = Message::plain("2", "Robin", "on it", 2, true);
        mine.state = SendState::Failed {
            reason: "Slow mode".into(),
        };
        let failed = can(&mine, &discord_caps(), rights, channel, false, 10);
        assert_eq!(
            menu_acts(failed, true, 3, true, true, false, false, true, false),
            vec![MenuAct::Retry, MenuAct::Copy, MenuAct::Drop]);
        assert_eq!(
            menu_acts(on, false, 0, false, false, false, false, true, false),
            vec![MenuAct::Reply, MenuAct::Thread, MenuAct::Forward, MenuAct::SaveAs, MenuAct::Unread, MenuAct::Select],
        );
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
    fn what_a_message_offers_follows_capabilities_rights_and_edit_window() {
        let channel = ConversationKind::Channel;
        let none = ConversationRights::default();
        let theirs = Message::plain("1", "Mara", "hello", 1_000, false);

        // Discord fixture: inline reply + channel threads, no edit/delete on others without mod rights.
        let on = can(&theirs, &discord_caps(), none, channel, false, 1_100);
        assert_eq!((on.act, on.react, on.reply, on.edit, on.delete, on.thread, on.pin, on.forward), (true, true, true, false, false, true, false, true));
        assert_eq!(thread_label(&theirs), "Reply in thread");
        let discord_dm = can(&theirs, &discord_caps(), none, ConversationKind::Direct, false, 1_100);
        assert!(discord_dm.reply && !discord_dm.thread, "channel-only threads stay inline in DMs");
        assert!(!can(&theirs, &discord_caps(), none, ConversationKind::Voice, false, 1_100).thread);
        assert!(!can(&theirs, &discord_caps(), none, ConversationKind::Forum, false, 1_100).thread);

        // Moderator rights grant delete on others' messages and pin.
        let mod_rights = ConversationRights { pin: true, delete_others: true, manage_members: true, ..ConversationRights::default() };
        let moderated = can(&theirs, &discord_caps(), mod_rights, channel, false, 1_100);
        assert!(moderated.delete && moderated.pin && !moderated.edit);

        // Slack fixture: no inline reply, threads in both channels and DMs.
        let slack = can(&theirs, &slack_caps(), none, channel, false, 1_100);
        assert!(!slack.reply && slack.thread);
        assert!(can(&theirs, &slack_caps(), none, ConversationKind::Direct, false, 1_100).thread);
        assert!(!can(&theirs, &slack_caps(), none, channel, true, 1_100).thread, "a thread does not open inside a thread");
        assert!(!can(&theirs, &slack_caps(), none, ConversationKind::Thread, false, 1_100).thread);
        let mut broadcast = theirs.clone();
        broadcast.reply_to = Some(protocol::ReplyTo {
            id: "parent".into(),
            sender: "Ana".into(),
            snippet: "earlier".into(),
            person: None,
            deleted: false,
            thumb: None,
            story: false,
        });
        assert!(!can(&broadcast, &slack_caps(), none, channel, false, 1_100).thread, "a channel copy of a reply belongs to its parent's thread");
        let mut pending = theirs.clone();
        pending.state = SendState::Pending;
        assert!(!can(&pending, &slack_caps(), none, channel, false, 1_100).thread);
        let mut joined = theirs.clone();
        joined.kind = MessageKind::System;
        assert!(!can(&joined, &slack_caps(), none, channel, false, 1_100).thread);

        // Teams fixture: inline reply + channel/announcement threads, not direct/group.
        let teams = can(&theirs, &teams_caps(), none, channel, false, 1_100);
        assert!(teams.reply && teams.thread);
        assert!(can(&theirs, &teams_caps(), none, ConversationKind::Announcement, false, 1_100).thread);
        assert!(!can(&theirs, &teams_caps(), none, ConversationKind::Direct, false, 1_100).thread);
        assert!(!can(&theirs, &teams_caps(), none, ConversationKind::Group, false, 1_100).thread);

        // Google Messages fixture: no reply, no edit, no delete, no thread, no pin, no forward; reactions only.
        let mut mine_sms = Message::plain("s1", "Me", "hey", 1_000, true);
        let sms = can(&mine_sms, &messages_caps(), mod_rights, ConversationKind::Direct, false, 1_100);
        assert!(sms.act && sms.react && !sms.reply && !sms.edit && !sms.delete && !sms.thread && !sms.pin && !sms.forward);
        // Per-message override can still grant or deny edit/delete explicitly.
        mine_sms.can_edit = Some(true);
        mine_sms.can_delete = Some(true);
        let overridden = can(&mine_sms, &messages_caps(), none, ConversationKind::Direct, false, 1_100);
        assert!(overridden.edit && overridden.delete);

        // Telegram edit_window (48h): edit offered inside window, hidden past window.
        let mine_tg = Message::plain("t1", "Me", "hi", 1_000, true);
        assert!(can(&mine_tg, &telegram_caps(), none, ConversationKind::Direct, false, 1_000 + 3_600).edit);
        assert!(!can(&mine_tg, &telegram_caps(), none, ConversationKind::Direct, false, 1_000 + 48 * 3_600 + 1).edit);

        let mut mine = Message::plain("2", "Robin", "on it", 2, true);
        let own = can(&mine, &discord_caps(), none, channel, false, 100);
        assert!(own.edit && own.delete && !own.unsent);
        mine.thread = Some(protocol::ThreadSummary { id: "t".into(), replies: 1, last: 3, participants: Vec::new(), unread: 0 });
        assert!(can(&mine, &discord_caps(), none, channel, false, 100).thread);
        assert_eq!(thread_label(&mine), "Open thread");
        assert!(can(&mine, &discord_caps(), none, ConversationKind::Direct, false, 100).thread, "a thread that exists still opens");
        assert!(!can(&mine, &discord_caps(), none, channel, true, 100).thread, "a thread does not open inside a thread");

        // Not on the network yet: no toolbar, nothing to edit, and deleting is local.
        mine.state = SendState::Failed { reason: "Slow mode".into() };
        let failed = can(&mine, &discord_caps(), none, channel, false, 100);
        assert_eq!((failed.act, failed.edit, failed.delete, failed.unsent), (false, false, true, true));

        // A tombstone and a system line say nothing that can be acted on.
        let mut gone = Message::plain("3", "Wren", "", 4, true);
        gone.tombstone();
        assert_eq!(can(&gone, &discord_caps(), none, channel, false, 100), Can::default());
        let mut joined = Message::plain("4", "Wren", "joined", 5, false);
        joined.kind = MessageKind::System;
        assert!(!can(&joined, &discord_caps(), none, channel, false, 100).act);
    }

    #[test]
    fn saved_history_cannot_offer_edits_but_keeps_the_owners_delete_action() {
        let mut saved = Message::plain("saved", "Ada", "past", 1, true);
        saved.saved_history = true;
        let actions = can(&saved, &tiny_caps(), ConversationRights::default(), ConversationKind::Direct, false, 10);
        assert!(!actions.edit);
        assert!(actions.delete && actions.reply);
        saved.outgoing = false;
        let neutral = can(&saved, &tiny_caps(), ConversationRights::default(), ConversationKind::Direct, false, 10);
        assert!(!neutral.edit && !neutral.delete);
    }

    #[test]
    fn moderator_can_delete_others_and_delete_for_me_follows_network() {
        let theirs = Message::plain("10", "Wren", "hello", 1, false);
        let mine = Message::plain("11", "Robin", "hi", 2, true);
        let mod_rights = protocol::ConversationRights { delete_others: true, ..protocol::ConversationRights::default() };
        let plain = protocol::ConversationRights::default();

        // Discord moderator sees Delete (for everyone) on another person's row; non-mod does not.
        let dc_mod = can_with_rights(&theirs, Network::Discord, ConversationKind::Channel, false, mod_rights);
        assert!(dc_mod.delete && dc_mod.delete_everyone && !dc_mod.delete_me);
        let dc_plain = can_with_rights(&theirs, Network::Discord, ConversationKind::Channel, false, plain);
        assert!(!dc_plain.delete && !dc_plain.delete_everyone && !dc_plain.delete_me);

        // Google Messages offers only delete-for-me (both on own and others' messages).
        let msg_own = can_with_rights(&mine, Network::Messages, ConversationKind::Direct, false, plain);
        assert!(msg_own.delete && msg_own.delete_me && !msg_own.delete_everyone && !msg_own.edit);
        let msg_theirs = can_with_rights(&theirs, Network::Messages, ConversationKind::Direct, false, plain);
        assert!(msg_theirs.delete && msg_theirs.delete_me && !msg_theirs.delete_everyone);

        // Telegram DM & Signal offer both delete-for-everyone and delete-for-me on own messages,
        // and only delete-for-me on others' messages unless moderator right is granted.
        let tg_own = can_with_rights(&mine, Network::Telegram, ConversationKind::Direct, false, plain);
        assert!(tg_own.delete && tg_own.delete_everyone && tg_own.delete_me);
        let tg_theirs = can_with_rights(&theirs, Network::Telegram, ConversationKind::Group, false, plain);
        assert!(tg_theirs.delete && !tg_theirs.delete_everyone && tg_theirs.delete_me);
        let tg_mod = can_with_rights(&theirs, Network::Telegram, ConversationKind::Group, false, mod_rights);
        assert!(tg_mod.delete && tg_mod.delete_everyone && tg_mod.delete_me);
        // Telegram broadcast channel: delete-for-everyone only.
        let tg_chan = can_with_rights(&theirs, Network::Telegram, ConversationKind::Channel, false, mod_rights);
        assert!(tg_chan.delete && tg_chan.delete_everyone && !tg_chan.delete_me);
    }

    #[test]
    fn multi_select_copies_three_rows_in_order_supports_shift_range_and_respects_cap_and_rights() {
        let m1 = Message::plain("1", "Mara", "first", 1_700_000_000, false);
        let m2 = Message::plain("2", "Robin", "second", 1_700_000_060, true);
        let m3 = Message::plain("3", "Wren", "third", 1_700_000_120, false);

        // Selecting three rows and copying produces three lines in chronological order.
        let copied = format_selected_messages(&[&m3, &m1, &m2]);
        let lines: Vec<&str> = copied.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Mara: first"), "{copied}");
        assert!(lines[1].contains("Robin: second"), "{copied}");
        assert!(lines[2].contains("Wren: third"), "{copied}");

        // Shift-click range selection selects anchor..=target inclusively.
        let mut sel = MultiSelect::default();
        sel.enter("1");
        sel.toggle_range(&["1", "2", "3", "4"], "3");
        assert!(sel.active && sel.count() == 3 && sel.contains("1") && sel.contains("2") && sel.contains("3"));
        assert!(!sel.contains("4"));

        // Cap at MULTI_SELECT_CAP (100).
        let many: Vec<String> = (0..150).map(|i| format!("m{i}")).collect();
        let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
        sel.enter("m0");
        sel.toggle_range(&many_refs, "m149");
        assert_eq!(sel.count(), MULTI_SELECT_CAP);

        // Bulk delete respects rights & skips Discord.
        let plain = protocol::ConversationRights::default();
        let mod_rights = protocol::ConversationRights { delete_others: true, ..Default::default() };
        assert!(
            deletable_selected(&[&m1, &m2, &m3], Network::Discord, ConversationKind::Channel, false, mod_rights).is_empty(),
            "Discord bulk delete stays disabled in selection mode"
        );
        let slack_plain = deletable_selected(&[&m1, &m2, &m3], Network::Slack, ConversationKind::Channel, false, plain);
        assert_eq!(slack_plain.len(), 1);
        assert_eq!(slack_plain[0].0.id, "2");
        assert_eq!(slack_plain[0].1, protocol::DeleteScope::Everyone);

        let slack_mod = deletable_selected(&[&m1, &m2, &m3], Network::Slack, ConversationKind::Channel, false, mod_rights);
        assert_eq!(slack_mod.len(), 3);

        let sms = deletable_selected(&[&m1, &m2], Network::Messages, ConversationKind::Direct, false, plain);
        assert_eq!(sms.len(), 2);
        assert!(sms.iter().all(|(_, scope)| *scope == protocol::DeleteScope::Me));
    }

    #[test]
    fn tiny_moderator_can_delete_others_sent_message_but_not_edit_it() {
        let theirs = Message::plain("m1", "Mara", "hello", 1, false);
        let plain = can_with_rights(&theirs, Network::Tiny, ConversationKind::Channel, false, ConversationRights::default());
        assert!(!plain.delete && !plain.edit);
        let mod_rights = ConversationRights { delete_others: true, ..ConversationRights::default() };
        let mod_can = can_with_rights(&theirs, Network::Tiny, ConversationKind::Channel, false, mod_rights);
        assert!(mod_can.delete && !mod_can.edit);
        let mut gone = theirs.clone();
        gone.deleted = true;
        assert!(!can_with_rights(&gone, Network::Tiny, ConversationKind::Channel, false, mod_rights).delete);
    }

    #[test]
    fn messages_edit_is_only_offered_for_own_rcs_messages_inside_the_window() {
        let now = 1_791_893_000;
        let mine = Message::plain("m1", "Ada", "hi", now - 60, true);
        let theirs = Message::plain("m2", "Grace", "hi", now - 60, false);
        let old = Message::plain("m3", "Ada", "old", now - 1200, true);

        assert!(can_in(&mine, Network::Messages, ConversationKind::Direct, false, "RCS message", 900, now).edit);
        assert!(can_in(&mine, Network::Messages, ConversationKind::Direct, false, "RCS message · Encrypted between phones", 900, now).edit);
        assert!(!can_in(&theirs, Network::Messages, ConversationKind::Direct, false, "RCS message", 900, now).edit);
        assert!(!can_in(&old, Network::Messages, ConversationKind::Direct, false, "RCS message", 900, now).edit);
        assert!(!can_in(&mine, Network::Messages, ConversationKind::Direct, false, "Text message", 900, now).edit);
        assert!(!can_in(&mine, Network::Messages, ConversationKind::Direct, false, "", 900, now).edit);
    }
}
