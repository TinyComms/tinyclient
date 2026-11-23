//! The window's regions and what fills each (`docs/design/02-layout-navigation.md`, section 1):
//! `| rail 72 | sidebar 316 | conversation | right pane |`, each starting with a 52 pt strip
//! that drags the window. The right pane, the sidebar's People frame and the call dock are
//! slots the other slices fill.

use super::components::{
    brand_mark, brand_state, button, button_disabled, divider, fade_in, glyph, icon_button, menu, menu_item,
    menu_item_check, menu_item_danger, menu_label, menu_separator, network_icon, place, placed, popover, scrim, tip,
    toast, ButtonKind, Layer, Place, NO_GLYPH,
};
use super::chrome::{self, End};
use super::phone::tokens::PhoneType;
use super::state::{Shell, Snapshot};
use super::theme::{self, Type};
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Div, MouseButton, MouseMoveEvent, Pixels, Point, Rgba,
    SharedString, Window, WindowControlArea,
};
use protocol::{CallState, Conversation, Key, Link, Network, Notify, Presence};
use std::rc::Rc;
use std::time::Duration;

pub const RAIL_WIDTH: f32 = 72.;
pub const SIDEBAR_WIDTH: f32 = 316.;
pub const SIDEBAR_RANGE: (f32, f32) = (260., 420.);
/// The right pane: one width for every kind, dragged within the range (`07` R6).
pub const RIGHT_WIDTH: f32 = 300.;
pub const RIGHT_RANGE: (f32, f32) = (260., 420.);
/// The conversation is the only region that gives up width, down to this.
pub const CONVERSATION_MIN: f32 = 392.;
/// A window may be a phone: the minimum is the smallest phone in portrait, not a desktop
/// window that happens to be narrow (`20-implementation-map.md` §1.2).
pub const WINDOW_MIN: (f32, f32) = (360., 480.);
/// Below this width a window is a phone: one full-screen surface and a tab bar, never a rail
/// beside a list. Above every phone in portrait, below every tablet split.
pub const PHONE: f32 = 600.;

/// Whether this window is drawn as a phone.
pub fn phone(window: f32) -> bool {
    window < PHONE
}

/// The rail on a phone: the desktop's buttons and badges, 8 pt narrower so a 360 pt screen
/// keeps 296 for the sidebar.
pub const RAIL_PHONE: f32 = 64.;

/// The rail's width in this window's form.
pub fn rail_width() -> f32 {
    if super::phone::active() {
        RAIL_PHONE
    } else {
        RAIL_WIDTH
    }
}
/// Height of the strip that starts every region, level with the traffic lights.
pub const TOP_BAR: f32 = 52.;
/// Networks in the order the rail and first launch list them.
pub const NETWORK_ORDER: [Network; 7] = [Network::Discord, Network::Slack, Network::Telegram, Network::Teams, Network::Signal, Network::Messages, Network::Tiny];
/// The networks tinyclient signs in to today. The others are shown as coming later.
pub const AVAILABLE: [Network; 7] = NETWORK_ORDER;

/// Lets a strip of chrome move the window, as a title bar would. Windows does it all itself
/// from the drag area: move, snap, double-click to maximize, the window menu. Elsewhere a press
/// moves it and a double click does what the title bar's would.
pub fn drags_window(strip: Div) -> Div {
    let strip = strip.window_control_area(WindowControlArea::Drag);
    if cfg!(windows) {
        return strip;
    }
    strip
        .on_mouse_down(MouseButton::Left, |event, window, _| match event.click_count {
            2 if cfg!(target_os = "macos") => window.titlebar_double_click(),
            2 => window.zoom_window(),
            _ => window.start_window_move(),
        })
        .on_mouse_down(MouseButton::Right, |event, window, _| window.show_window_menu(event.position))
}

/// What another slice draws into one of this slice's frames. `None` draws nothing.
pub type Draw = fn(&Shell, &mut Window, &mut Context<Shell>) -> Option<AnyElement>;

/// The hooks the other slices fill. Each is set once, where `Shell` is built.
#[derive(Default)]
pub struct Slots {
    /// The People list, placed under the sidebar's title strip in People mode.
    pub people: Option<Draw>,
    /// The call dock, at the bottom of the sidebar in every mode.
    pub call_dock: Option<Draw>,
    /// The popover of the rail's own avatar: status and Settings.
    pub own_menu: Option<Draw>,
    /// A person's presence, for the avatars of direct rows.
    // The sidebar now asks the people view directly; the slot stays filled for other frames.
    #[allow(dead_code)]
    pub presence: Option<fn(&Shell, &Key) -> Presence>,
    /// `typing…` in place of a direct row's preview, while someone is.
    pub typing: Option<fn(&Shell, &Key) -> Option<SharedString>>,
    /// The switcher's last row: search messages for these words.
    pub search: Option<fn(&mut Shell, String, &mut Window, &mut Context<Shell>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RightKind {
    Thread,
    Members,
    /// One person: who they are and what was shared with them.
    Info,
    /// The search pane: the field, its scopes, the cache's hits.
    Search,
}

/// What fills the right pane. One slot: opening one replaces the other.
pub struct RightPane {
    pub kind: RightKind,
    pub title: SharedString,
    /// Shown after the title: replies, members.
    pub count: Option<u32>,
    pub body: fn(&Shell, &mut Window, &mut Context<Shell>) -> AnyElement,
}

/// A menu or popover that is open. One at a time; a click outside closes it.
#[derive(Clone)]
pub enum Menu {
    /// The rail's add button.
    Add,
    /// The rail's own avatar.
    Own,
    Network(Network),
    Space(Key),
    /// A server folder's context menu, where the pointer was. The key is that account's folder.
    Folder(Key, Point<Pixels>),
    /// A category header's context menu, where the pointer was.
    Group { space: Key, id: String, name: String, muted: bool, at: Point<Pixels> },
    /// A conversation row's context menu, where the pointer was.
    Row(Rc<Conversation>, Point<Pixels>),
    VoiceOccupant(Rc<Conversation>, protocol::Occupant, Point<Pixels>),
}

/// Discord folder colours, `0xRRGGBB`.
const FOLDER_COLOURS: [u32; 8] = [0x5865F2, 0x57F287, 0xFEE75C, 0xEB459E, 0xED4245, 0xE67E22, 0x3498DB, 0x99AAB5];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Sidebar,
    Right,
}

/// The toast on show: which one, its glyph and color, its words.
type Toast = (u64, Option<(&'static str, Rgba)>, SharedString, Option<std::path::PathBuf>);

pub struct State {
    pub sidebar_width: f32,
    pub right_width: f32,
    /// `cmd-\`.
    pub sidebar_hidden: bool,
    /// The sidebar is drawn over the conversation, in a window too narrow to hold both.
    pub floating: bool,
    pub resizing: Option<Edge>,
    pub menu: Option<Menu>,
    pub right: Option<RightPane>,
    /// The row of the first-launch card that has the keyboard.
    pub first_run: usize,
    /// The two Details switches (rule 2), remembered; see `Shell::details_switch`.
    pub details_channels: bool,
    pub details_directs: bool,
    toast: Option<Toast>,
    toasts: u64,
    /// A newer release is waiting. Stays until the toast is dismissed.
    pub update_alert: bool,
    /// Shell-level external link confirmation when no conversation pane is mounted.
    link_ask: Option<super::conversation::links::LinkAsk>,
}

impl Default for State {
    fn default() -> Self {
        State {
            sidebar_width: SIDEBAR_WIDTH,
            right_width: RIGHT_WIDTH,
            sidebar_hidden: false,
            floating: false,
            resizing: None,
            menu: None,
            right: None,
            first_run: 0,
            details_channels: false,
            details_directs: false,
            toast: None,
            toasts: 0,
            update_alert: false,
            link_ask: None,
        }
    }
}

/// Where each region goes for a window this wide. The narrow rules are arithmetic.
#[derive(Debug, PartialEq)]
pub struct Plan {
    /// The sidebar's width when it sits in the row; `None` when it has left it.
    pub sidebar: Option<f32>,
    /// The right pane's width, and whether it replaces the conversation rather than sit beside.
    pub right: Option<(f32, bool)>,
}

pub fn plan(window: f32, sidebar: f32, hidden: bool, right: Option<f32>) -> Plan {
    // A phone has no room for two columns: the sidebar never sits in the row, and the right
    // pane always replaces the conversation it was opened from.
    if phone(window) {
        return Plan { sidebar: None, right: right.map(|_right| (window, true)) };
    }
    let fits = window - RAIL_WIDTH - sidebar >= CONVERSATION_MIN;
    let sidebar = (fits && !hidden).then_some(sidebar);
    let left = RAIL_WIDTH + sidebar.unwrap_or(0.);
    let right = right.map(|right| {
        let beside = window - left - right >= CONVERSATION_MIN;
        (if beside { right } else { window - left }, !beside)
    });
    Plan { sidebar, right }
}

/// Add the Discord parent column only when both lists and the conversation fit.
fn discord_overview_width(window: f32, sidebar: f32, hidden: bool, right: Option<f32>, server: bool) -> f32 {
    const OVERVIEW: f32 = 280.;
    if server && !hidden && window >= RAIL_WIDTH + sidebar + OVERVIEW + CONVERSATION_MIN + right.unwrap_or(0.) {
        OVERVIEW
    } else {
        0.
    }
}

/// The right pane would replace the conversation here rather than sit beside it: Details
/// hides then, and a thread or search stands in for the conversation (07 §4).
pub fn narrow(window: f32, sidebar: f32, hidden: bool, right: f32) -> bool {
    plan(window, sidebar, hidden, Some(right)).right.is_some_and(|(_, replaces)| replaces)
}

/// The conversation region's width as drawn, for what its strip can hold.
pub fn conversation_width(window: f32, plan: &Plan) -> f32 {
    if phone(window) {
        return window;
    }
    let beside = plan.right.filter(|(_, replaces)| !replaces).map_or(0., |(width, _)| width);
    window - RAIL_WIDTH - plan.sidebar.unwrap_or(0.) - beside
}

impl Shell {
    // The right pane.

    /// Opens the right pane with this view, replacing what was there.
    pub(super) fn open_right(&mut self, pane: RightPane, cx: &mut Context<Self>) {
        self.layout_state.right = Some(pane);
        cx.notify();
    }

    pub(super) fn close_right(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let was_open = self.layout_state.right.take().is_some();
        // The keys go back to the composer when a pane was open, as Esc's closing of it does.
        if was_open {
            if let Some(window) = window {
                self.composer_focus(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn right_kind(&self) -> Option<RightKind> {
        self.layout_state.right.as_ref().map(|pane| pane.kind)
    }

    /// For the conversation strip, first in it: the button that brings the sidebar back when
    /// it is not in the row, because the window is too narrow or it was hidden. `None` while
    /// the sidebar is in the row.
    pub(super) fn panel_button(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = &self.layout_state;
        let width = f32::from(window.viewport_size().width);
        plan(width, state.sidebar_width, state.sidebar_hidden, None).sidebar.is_none().then(|| {
            icon_button("panel", "icons/panel-left.svg", state.floating)
                .aria_label("Show sidebar")
                .aria_expanded(state.floating)
                .size(px(24.))
                .tooltip(tip("Show sidebar", Some("⌘\\")))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)))
                .into_any_element()
        })
    }

    /// For the conversation strip, after the panel button: back and forward through the
    /// conversations that were open. Each dims when it leads nowhere.
    pub(super) fn travel_buttons(&self, cx: &mut Context<Self>) -> [AnyElement; 2] {
        let button = |back: bool| {
            let (id, icon, words, keys) =
                if back { ("travel-back", "icons/arrow-left.svg", "Back", "⌘[") } else { ("travel-forward", "icons/arrow-right.svg", "Forward", "⌘]") };
            let can = self.can_travel(back);
            icon_button(id, icon, false)
                .aria_label(words)
                .aria_disabled(!can)
                .tab_stop(can)
                .size(px(24.))
                .when(!can, |button| button.opacity(0.4).cursor_default())
                .tooltip(tip(words, Some(keys)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.travel(back, cx)))
                .into_any_element()
        };
        [button(true), button(false)]
    }

    /// The controls that start the conversation strip, in order: the panel button when the
    /// sidebar is out of the row, then back and forward.
    pub(super) fn strip_controls(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .children(self.panel_button(window, cx))
            .children(self.travel_buttons(cx))
    }

    /// `cmd-\`: in a window wide enough to hold the sidebar it hides or shows it; in a narrower
    /// one it floats it over the conversation or puts it away.
    pub(super) fn toggle_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        let state = &mut self.layout_state;
        let width = f32::from(window.viewport_size().width);
        if plan(width, state.sidebar_width, false, None).sidebar.is_some() {
            state.sidebar_hidden = !state.sidebar_hidden;
            state.floating = false;
        } else {
            state.floating = !state.floating;
        }
        cx.notify();
    }

    /// The conversation region with nothing open: its strip still brings the sidebar back and
    /// goes back and forward.
    fn nothing_open(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                drags_window(div())
                    .h(px(TOP_BAR))
                    .flex_none()
                    .px(px(theme::PANE_PAD))
                    .flex()
                    .items_center()
                    .child(self.strip_controls(window, cx)),
            )
            .child(
                brand_state("Pick a conversation", "⌘K jumps anywhere.")
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.)),
            )
    }

    /// Says the result of something the user just did that is not visible where they look.
    /// One at a time; it leaves after four seconds.
    pub(super) fn toast(
        &mut self,
        icon: Option<(&'static str, Rgba)>,
        words: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.place_toast(icon, words, None, cx);
    }

    /// A save's confirmation: the same toast, and a click shows the file.
    pub(super) fn toast_reveal(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.place_toast(Some(("icons/check.svg", super::theme::text_soft())), crate::host::offer_reveal(crate::host::SAVED_DOWNLOADS), Some(path), cx);
    }

    fn place_toast(&mut self, icon: Option<(&'static str, Rgba)>, words: impl Into<SharedString>, reveal: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        let state = &mut self.layout_state;
        state.toasts += 1;
        let id = state.toasts;
        state.toast = Some((id, icon, words.into(), reveal));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                if this.layout_state.toast.as_ref().is_some_and(|(shown, ..)| *shown == id) {
                    this.layout_state.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn open_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        if matches!(menu, Menu::Own) && !matches!(self.layout_state.menu, Some(Menu::Own)) {
            self.own_menu_opened(cx);
        }
        self.layout_state.menu = Some(menu);
        cx.notify();
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.layout_state.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Closes the top layer: switcher, menu, floating sidebar, right pane. False when there was
    /// none, so the key can mean something else.
    pub(super) fn close_top(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // On a phone Escape is back: one pushed page, sheet or switcher at a time.
        if super::phone::active() {
            return self.phone_back(Some(window), cx);
        }
        // The invite dialog is painted above the switcher, so Escape closes it first.
        if self.sidebar_state.create_invite.is_some() {
            self.sidebar_state.create_invite = None;
            cx.notify();
            return true;
        }
        if self.layout_state.link_ask.take().is_some() {
            cx.notify();
            return true;
        }
        if self.sidebar_state.switcher.is_some() {
            self.close_switcher(window, cx);
        } else if self.close_discord_admin(cx) {
            return true;
        } else if self.sidebar_state.delete_channel.is_some() {
            self.sidebar_state.delete_channel = None;
        } else if self.sidebar_state.move_channels.is_some() {
            self.sidebar_state.move_channels = None;
        } else if self.sidebar_state.edit_channel.is_some() {
            self.sidebar_state.edit_channel = None;
        } else if self.sidebar_state.create_category.is_some() {
            self.sidebar_state.create_category = None;
        } else if self.sidebar_state.create_channel.is_some() {
            self.sidebar_state.create_channel = None;
        } else if self.sidebar_state.browse_channels.is_some() {
            self.sidebar_state.browse_channels = None;
        } else if self.layout_state.menu.is_some() {
            self.layout_state.menu = None;
        } else if self.call_unfull(window, cx) {
            return true;
        } else if self.tiny_escape(cx) {
            return true;
        } else if self.call_escape(cx) {
            return true;
        } else if self.layout_state.floating {
            self.layout_state.floating = false;
        } else {
            // The pane's steps of the ladder, down to "to the newest". A step it hands back
            // (an edit, the viewer) belongs to whoever has the keys; nothing closes in its place.
            return self.pane_escape(window, cx);
        }
        cx.notify();
        true
    }

    fn resize(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        super::lock::touch();
        let Some(edge) = self.layout_state.resizing else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.layout_state.resizing = None;
            self.save_prefs(cx);
            return;
        }
        let x = f32::from(event.position.x);
        let overview = discord_overview_width(f32::from(window.viewport_size().width), self.layout_state.sidebar_width, self.layout_state.sidebar_hidden, self.layout_state.right.as_ref().filter(|_| !self.pane_tucked()).map(|_| self.layout_state.right_width), matches!(&self.sidebar_state.mode, super::sidebar::Mode::Space(space) if space.network == Network::Discord));
        let state = &mut self.layout_state;
        match edge {
            Edge::Sidebar => state.sidebar_width = (x - RAIL_WIDTH - overview).round().clamp(SIDEBAR_RANGE.0, SIDEBAR_RANGE.1),
            Edge::Right => {
                let window = f32::from(window.viewport_size().width);
                state.right_width = (window - x).round().clamp(RIGHT_RANGE.0, RIGHT_RANGE.1);
            }
        }
        cx.notify();
    }

    /// The 6 pt strip over a pane's hairline that resizes it; a double click resets.
    fn resize_handle(&self, edge: Edge, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = div()
            .id(match edge {
                Edge::Sidebar => "resize-sidebar",
                Edge::Right => "resize-right",
            })
            .absolute()
            .top_0()
            .h_full()
            .w(px(6.))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.layout_state.resizing = Some(edge);
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    match edge {
                        Edge::Sidebar => this.layout_state.sidebar_width = SIDEBAR_WIDTH,
                        Edge::Right => this.layout_state.right_width = RIGHT_WIDTH,
                    }
                    this.save_prefs(cx);
                    cx.notify();
                }
            }));
        match edge {
            Edge::Sidebar => handle.right(px(-3.)),
            Edge::Right => handle.left(px(-3.)),
        }
    }

    fn right_pane(&self, width: f32, replaces: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let pane = self.layout_state.right.as_ref()?;
        let back_to = self.snapshot.as_ref().and_then(|s| s.open.as_ref()).map(|open| open.title.clone());
        // The window's own buttons end this strip at the window's right edge, on Windows and a
        // client-drawn Linux frame. The close dismisses the pane, which is an app action, so it
        // leads the strip, at the other end from that cluster, and never trails the title: there
        // it would sit beside minimize and read as another window button.
        let close_words = if pane.kind == RightKind::Thread { "Close thread" } else { "Close" };
        let close = icon_button("right-close", "icons/x.svg", false)
            .tooltip(tip(close_words, Some("⌘.")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| this.close_right(Some(window), cx)));
        let title = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_baseline()
            .gap(px(8.))
            .child(div().min_w_0().truncate().type_title().child(pane.title.clone()))
            .children(pane.count.map(|count| {
                div().flex_none().type_ui().text_color(theme::text_faint()).child(count.to_string())
            }));
        // Over the conversation it stands for, the pane is a page with a heading: the person or
        // channel it belongs to is centred over the body, not tucked into the crumb beside the
        // title, where it reads as a path instead of a name.
        let heading = back_to.filter(|_| replaces).map(|name| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(TOP_BAR))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .min_w_0()
                        .max_w(px((width * 0.5).max(80.)))
                        .truncate()
                        .type_ui()
                        .font_weight(theme::MEDIUM)
                        .text_color(theme::text_soft())
                        .child(name),
                )
        });
        // The strip's own padding sits on an inner row, so a heading can centre on the whole pane
        // rather than on what the window's buttons leave of it.
        let row = div()
            .h_full()
            .pl(px(theme::PANE_PAD))
            .pr(px(chrome::end_pad(End::Right, 12.)))
            .flex()
            .items_center()
            .gap(px(8.))
            // With the sidebar out of the row too, the floating sidebar is one click away here
            // as in the conversation's strip.
            .when(replaces, |row| row.children(self.panel_button(window, cx)))
            // With the conversation gone from the row, this chevron is the way back to it, and so
            // the way out of the pane: it leads in the close's place, and there is no second one.
            .when(replaces, |row| {
                row.child(
                    icon_button("right-back", "icons/chevron-left.svg", false)
                        .tooltip(tip(close_words, Some("⌘.")))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, window, cx| this.close_right(Some(window), cx))),
                )
            })
            // Beside the conversation nothing leads the strip, so the close does.
            .when(!replaces, |row| row.child(close))
            .when(pane.kind == RightKind::Thread && !super::phone::active() && self.thread_is_open(), |row| {
                row.child(
                    icon_button("open-thread-window", "icons/picture-in-picture-2.svg", false)
                        .tooltip(tip("Open in New Window", None))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| this.pop_out_open_thread(cx))),
                )
            })
            .child(title);
        let strip = drags_window(div())
            .relative()
            .h(px(TOP_BAR))
            .flex_none()
            .border_b_1()
            .border_color(theme::hairline())
            .child(row)
            .children(heading);
        let resizable = !replaces;
        Some(
            div()
                .relative()
                .w(px(width))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .bg(theme::sidebar())
                .when(!replaces, |pane| pane.border_l_1().border_color(theme::hairline()))
                .child(strip)
                .child(div().flex_1().min_h_0().child((pane.body)(self, window, cx)))
                .when(resizable, |pane| pane.child(self.resize_handle(Edge::Right, cx))),
        )
    }

    /// Nothing is signed in: no rail, no sidebar, one centered column.
    fn first_run(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        // A phone has its own form: one column, the networks a phone can sign in to.
        if super::phone::active() {
            return self.phone_first_run(cx);
        }
        let row = |ix: usize, network: Network| {
            let available = AVAILABLE.contains(&network);
            let waiting = self.signing_in == Some(network);
            let dimmed = self.signing_in.is_some() && !waiting;
            let focused = available && self.layout_state.first_run == ix && self.signing_in.is_none();
            let name = match network {
                Network::Teams => "Microsoft Teams",
                other => other.label(),
            };
            let line: SharedString = match (available, waiting, network) {
                (false, ..) => "Coming in a later version".into(),
                (_, true, _) => format!("Finish signing in in the {name} window.").into(),
                (_, _, Network::Discord) => "Servers, direct messages and voice".into(),
                (_, _, Network::Slack) => "Every workspace from one sign-in".into(),
                (_, _, Network::Telegram) => "Scan a code with your phone, or use your number".into(),
                (_, _, Network::Teams) => "Work and school accounts; calls open in Teams".into(),
                (_, _, Network::Signal) => "Link this device from your primary Signal app".into(),
                (_, _, Network::Messages) => "Pair from your Android phone. It has to stay online.".into(),
                (_, _, Network::Tiny) => "A server you already run".into(),
            };
            let failed = self.sign_in_failed.as_ref().filter(|(failed, _)| *failed == network);
            let action = match (available, waiting) {
                (false, _) => div().type_small().text_color(theme::text_faint()).child("Not available yet").into_any_element(),
                (true, true) => button_disabled("Waiting…").into_any_element(),
                (true, false) => {
                    let kind = if focused { ButtonKind::Primary } else { ButtonKind::Secondary };
                    button(("sign-in", ix), kind, "Sign in").tab_stop(false).into_any_element()
                }
            };
            div()
                .id(("network", ix))
                .h(px(64.))
                .px(px(16.))
                .flex()
                .items_center()
                .gap(px(12.))
                .when(dimmed, |row| row.opacity(0.4))
                .when(focused || waiting, |row| row.bg(theme::hover()))
                .when(available && !dimmed && !waiting, |row| {
                    row.cursor_pointer()
                        .hover(|style| style.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
                })
                .child(network_icon(network, 32.).when(!available, |icon| icon.opacity(0.55)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .type_ui()
                                .font_weight(theme::SEMIBOLD)
                                .text_color(if available { theme::text() } else { theme::text_soft() })
                                .child(name),
                        )
                        .child(div().type_small().text_color(theme::text_soft()).child(line))
                        .children(failed.map(|(_, why)| div().type_small().text_color(theme::danger()).child(why.clone())))
                        .when(network == Network::Teams && available && !waiting, |column| {
                            column.child(
                                div()
                                    .id(("teams-personal", ix))
                                    .type_small()
                                    .text_color(theme::accent_text())
                                    .cursor_pointer()
                                    .child("Personal Microsoft account")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.sign_in_personal(cx);
                                    })),
                            )
                        }),
                )
                .child(action)
        };
        let _ = snapshot;
        let mut card = div()
            .w(px(if super::phone::active() { 326. } else { 440. }))
            .rounded(px(theme::R_CARD))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .flex()
            .flex_col();
        for (ix, network) in NETWORK_ORDER.into_iter().enumerate() {
            if ix > 0 {
                card = card.child(divider());
            }
            card = card.child(row(ix, network));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(drags_window(div().h(px(TOP_BAR)).flex_none()))
            .child(
                div()
                    .flex_1()
                    .pb(px(TOP_BAR))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(brand_mark(theme::BRAND_MARK))
                    .child(div().mt(px(16.)).type_headline().child("Welcome to tinyclient"))
                    .child(
                        div()
                            .mt(px(6.))
                            .mb(px(28.))
                            .px(px(24.))
                            .max_w(px(360.))
                            .text_align(gpui::TextAlign::Center)
                            .type_body()
                            .text_color(theme::text_soft())
                            .child("Sign in to one network to start. Add the others whenever you like."),
                    )
                    .child(card)
                    .child(
                        div()
                            .mt(px(16.))
                            .type_small()
                            .text_color(theme::text_faint())
                            .child(crate::host::sign_in_kept()),
                    ),
            )
    }

    /// The first-launch card's keys: up and down move, Return signs in.
    pub(super) fn first_run_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let nothing = self.snapshot.as_ref().is_some_and(|s| s.signed_in().is_empty());
        if !nothing {
            return false;
        }
        let state = &mut self.layout_state;
        match key {
            "down" => state.first_run = (state.first_run + 1).min(AVAILABLE.len() - 1),
            "up" => state.first_run = state.first_run.saturating_sub(1),
            "enter" => {
                let network = NETWORK_ORDER[state.first_run];
                self.sign_in(network, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The open menu, over everything, with a catcher behind it that closes it.
    fn menu_layer(&self, snapshot: &Snapshot, sidebar: f32, overview: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let open = self.layout_state.menu.as_ref()?;
        let tall = f32::from(window.viewport_size().height);
        // The rail's add button and own avatar, and the sidebar strip's trailing button.
        let rail_button = |from_bottom: f32| place::anchor(16., tall - from_bottom - 40., 40., 40.);
        let strip_left = RAIL_WIDTH + if matches!(open, Menu::Space(_)) { overview } else { 0. };
        let strip_width = if overview > 0. && matches!(open, Menu::Network(_)) { overview } else { sidebar };
        let strip_button = place::anchor(strip_left + strip_width - 12. - 28., 12., 28., 28.);
        // Discord, a multi-server Tiny title, and a server name are the buttons on the left.
        // The 28-point box at the strip's right is the ellipsis, and it sits under Unread.
        let named = match open {
            Menu::Network(Network::Discord) => true,
            Menu::Network(Network::Tiny) => snapshot.accounts_of(Network::Tiny) > 1,
            Menu::Space(_) => true,
            _ => false,
        };
        let title_width = (strip_width * 0.55).clamp(140., 260.);
        let title_anchor = place::anchor(strip_left + 8., 6., title_width, 32.);
        let menu_anchor = if named { title_anchor } else { strip_button };
        let placed: AnyElement = match open {
            Menu::Add => {
                let content = fade_in("menu-add", 120, self.add_menu(snapshot, cx));
                placed("menu-add-at", Place::Beside(rail_button(52.)), Layer::Popover, window, content)
            }
            Menu::Own => {
                let content = self.slots.own_menu.and_then(|draw| draw(self, window, cx));
                let content = content.unwrap_or_else(|| self.accounts_menu(snapshot, cx).into_any_element());
                let content = fade_in("menu-own", 120, popover().p(px(4.)).child(content));
                placed("menu-own-at", Place::Beside(rail_button(8.)), Layer::Popover, window, content)
            }
            Menu::Network(_) => {
                let items = menu().w(px(220.)).children(self.menu_items(open, snapshot, cx));
                placed("menu-network-at", Place::Below(menu_anchor), Layer::Menu, window, fade_in("menu-network", 120, items))
            }
            Menu::Space(_) => {
                let items = menu().w(px(220.)).children(self.menu_items(open, snapshot, cx));
                placed("menu-space-at", Place::Below(menu_anchor), Layer::Menu, window, fade_in("menu-space", 120, items))
            }
            Menu::Group { at, .. } => {
                let items = menu().w(px(220.)).children(self.menu_items(open, snapshot, cx));
                placed("menu-group-at", Place::Pointer(*at), Layer::Menu, window, fade_in("menu-group", 120, items))
            }
            Menu::Row(_, position) | Menu::VoiceOccupant(_, _, position) => {
                let items = menu().w(px(220.)).children(self.menu_items(open, snapshot, cx));
                placed("menu-row-at", Place::Pointer(*position), Layer::Menu, window, fade_in("menu-row", 120, items))
            }
            Menu::Folder(_, position) => {
                let items = menu().w(px(240.)).children(self.menu_items(open, snapshot, cx));
                placed("menu-folder-at", Place::Pointer(*position), Layer::Menu, window, fade_in("menu-folder", 120, items))
            }
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_menu(cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _, cx| this.close_menu(cx)))
                .child(placed),
        )
    }

    /// The items of a Network, Space or Row menu, the same as a popover or as a phone's sheet.
    fn menu_items(&self, open: &Menu, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items: Vec<AnyElement> = Vec::new();
        match open {
            Menu::Add | Menu::Own => {}
            Menu::VoiceOccupant(channel, occupant, _) => {
                items.push(menu_label(occupant.name.clone()).into_any_element());
                items.extend(self.discord_voice_actions(channel, &occupant.person, occupant.server_muted, occupant.server_deafened, cx));
            }
            Menu::Network(network) => {
                let network = *network;
                let current = self.tiny_account().cloned();
                let many = network == Network::Tiny && snapshot.accounts_of(network) > 1;
                for account in snapshot.accounts.iter().filter(|a| a.key.network == network) {
                    let key = account.key.clone();
                    if many {
                        let on = current.as_ref() == Some(&account.key);
                        let show = key.clone();
                        items.push(
                            menu_item_check(("show-server", items_id(&account.key.account)), on, account.name.clone(), None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.prefer_tiny(show.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    } else {
                        items.push(menu_label(account.name.clone()).into_any_element());
                    }
                    items.push(
                        menu_item_danger(("sign-out", items_id(&account.key.account)), Some("icons/log-out.svg"), "Sign out")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.sign_out(key.clone(), false, cx);
                            }))
                            .into_any_element(),
                    );
                }
                if network == Network::Tiny {
                    items.push(menu_separator().into_any_element());
                    items.push(
                        menu_item("join-server", Some("icons/log-in.svg"), "Join a server", None)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_menu(cx);
                                this.sign_in(Network::Tiny, cx);
                            }))
                            .into_any_element(),
                    );
                } else if network == Network::Telegram {
                    if let Some(account) = snapshot
                        .accounts
                        .iter()
                        .find(|a| a.key.network == Network::Telegram && a.link == Link::Connected)
                        .map(|a| a.key.clone())
                    {
                        let join_account = account.clone();
                        items.push(menu_separator().into_any_element());
                        items.push(
                            menu_item("tg-new-channel", Some("icons/plus.svg"), "New channel…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_create_channel(Key::new(&account, ""), None, cx);
                                }))
                                .into_any_element(),
                        );
                        items.push(
                            menu_item("tg-join-invite", Some("icons/log-in.svg"), "Join by invite link…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_join_invite(join_account.clone(), None, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                } else {
                    let join_label = if network == Network::Teams { "Join a team…" } else { "Join a server…" };
                    let join_account = snapshot
                        .accounts
                        .iter()
                        .find(|a| a.key.network == network && a.link == Link::Connected && a.can.join_space_by_invite)
                        .map(|a| a.key.clone());
                    let create_account = snapshot
                        .accounts
                        .iter()
                        .find(|a| a.key.network == network && a.link == Link::Connected && a.can.create_space)
                        .map(|a| a.key.clone());
                    if join_account.is_some() || create_account.is_some() {
                        items.push(menu_separator().into_any_element());
                    }
                    if let Some(account) = join_account {
                        items.push(
                            menu_item("join-server-invite", Some("icons/log-in.svg"), join_label, None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_join_server(account.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    if let Some(account) = create_account {
                        items.push(
                            menu_item("create-server", Some("icons/plus.svg"), "Create a server…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_create_server(account.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    if network == Network::Discord && snapshot.accounts_of(network) > 0 {
                        items.push(menu_separator().into_any_element());
                        items.push(
                            menu_item("add-discord", Some("icons/plus.svg"), "Add another Discord account", None)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.sign_in(Network::Discord, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                }
            }
            // CONTRACT: of the Space menu in `02-layout-navigation.md` pinning and mark-as-read
            // have a request behind them. Nothing mutes a whole Space, and a Space carries no
            // link to open it in the network's app, so those items are left out.
            Menu::Space(space) => {
                let pinned = self.sidebar_state.pins.contains(space);
                let found_space = self.spaces.iter().find(|s| &s.key == space);
                let can_create = found_space.is_some_and(|s| s.can_create_channels);
                let can_settings = space.network == Network::Discord
                    && found_space.is_some_and(|s| s.rights.any_settings());
                let can_invite = self.spaces.iter().any(|s| &s.key == space && s.can_create_invites);
                let space_name = self
                    .spaces
                    .iter()
                    .find(|s| &s.key == space)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "this server".to_owned());
                let key = space.clone();
                let shown = space.clone();
                let read = space.clone();
                if can_invite {
                    let invite_space = space.clone();
                    let invite_name = space_name.clone();
                    items.push(
                        menu_item("space-invite", Some("icons/user-plus.svg"), "Invite people…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_create_invite(
                                    super::sidebar::CreateInviteTarget::Space(invite_space.clone(), invite_name.clone()),
                                    cx,
                                );
                            }))
                            .into_any_element(),
                    );
                }
                if can_settings {
                    let settings_space = space.clone();
                    items.push(
                        menu_item("space-settings", Some("icons/settings.svg"), "Server settings…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_discord_settings(settings_space.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if can_create {
                    let create_space = space.clone();
                    items.push(
                        menu_item("create-channel", Some("icons/plus.svg"), "Create channel…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_create_channel(create_space.clone(), None, cx);
                            }))
                            .into_any_element(),
                    );
                }
                let space_entry = self.spaces.iter().find(|s| &s.key == space);
                if space.network == Network::Discord
                    && space_entry.is_some_and(|s| s.can_manage_webhooks || s.can_manage_guild)
                {
                    let int_space = space.clone();
                    let int_name = space_entry.map(|s| s.name.clone()).unwrap_or_else(|| "Server".to_owned());
                    items.push(
                        menu_item("space-integrations", Some("icons/settings.svg"), "Integrations…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_discord_integrations(int_space.clone(), int_name.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if space.network == Network::Discord
                    && space_entry.is_some_and(|s| s.can_manage_expressions)
                {
                    let expr_space = space.clone();
                    let expr_name = space_entry.map(|s| s.name.clone()).unwrap_or_else(|| "Server".to_owned());
                    items.push(
                        menu_item("space-expressions", Some("icons/smile.svg"), "Emoji & Stickers…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_discord_expressions(expr_space.clone(), expr_name.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                items.push(
                    menu_item("details", Some("icons/info.svg"), "Details", None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.details_space(shown.clone(), cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    menu_item("pin", Some("icons/pin.svg"), if pinned { "Unpin from rail" } else { "Pin to rail" }, None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.pin(key.clone(), !pinned, cx);
                        }))
                        .into_any_element(),
                );
                let has_badge = self.spaces.iter().any(|s| &s.key == space && (s.unread || s.attention > 0));
                if has_badge {
                    items.push(
                        menu_item("space-read", Some("icons/check.svg"), "Mark as read", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.mark_space_read(read.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if space.network == Network::Discord {
                    let space_row = self.spaces.iter().find(|s| &s.key == space);
                    let space_muted = space_row.is_some_and(|s| s.muted);
                    let opt_in_on = space_row.is_some_and(|s| s.opt_in_channels);
                    let hide_muted = space_row.is_some_and(|s| s.hide_muted);
                    let manage_channels = space_row.is_some_and(|s| s.rights.manage_channels);
                    if manage_channels {
                        let cat_space = space.clone();
                        let move_space = space.clone();
                        items.push(
                            menu_item("space-create-category", Some("icons/folder-plus.svg"), "Create category…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_create_category(cat_space.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                        items.push(
                            menu_item("space-move-channels", Some("icons/grip-vertical.svg"), "Reorder channels…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_move_channels(move_space.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    let browse_space = space.clone();
                    let view_all_space = space.clone();
                    let hide_muted_space = space.clone();
                    items.push(menu_separator().into_any_element());
                    items.push(
                        menu_item("space-browse-channels", Some("icons/list.svg"), "Browse channels…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_browse_channels(browse_space.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                    if space_row.is_some_and(|row| row.rights.manage_server) {
                        let safety_space = space.clone();
                        let safety_name = space_row.as_ref().map(|row| row.name.clone()).unwrap_or_else(|| "Server".to_owned());
                        items.push(
                            menu_item("space-safety", Some("icons/settings.svg"), "Safety setup…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_safety(safety_space.clone(), safety_name.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    if space_row.is_some_and(|row| row.rights.view_audit_log) {
                        let audit_space = space.clone();
                        let audit_name = space_row.map(|row| row.name.clone()).unwrap_or_else(|| "Server".to_owned());
                        items.push(
                            menu_item("space-audit-log", Some("icons/shield.svg"), "Audit log…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_audit_log(audit_space.clone(), audit_name.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    items.push(
                        menu_item_check("space-show-all", !opt_in_on, "Show all channels", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.set_space_view(view_all_space.clone(), Some(opt_in_on), None, cx);
                            }))
                            .into_any_element(),
                    );
                    items.push(
                        menu_item_check("space-hide-muted", hide_muted, "Hide muted channels", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.set_space_view(hide_muted_space.clone(), None, Some(!hide_muted), cx);
                            }))
                            .into_any_element(),
                    );
                    items.push(menu_separator().into_any_element());
                    if space_muted {
                        let unmute_space = space.clone();
                        items.push(
                            menu_item("space-unmute", Some("icons/bell.svg"), "Unmute server", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.set_space_notify(unmute_space.clone(), Notify::All, None, None, cx);
                                }))
                                .into_any_element(),
                        );
                    } else {
                        items.push(menu_label("Mute server").into_any_element());
                        for (ix, (label, secs)) in [
                            ("For 15 minutes", Some(15 * 60i64)),
                            ("For 1 hour", Some(60 * 60)),
                            ("For 8 hours", Some(8 * 60 * 60)),
                            ("For 24 hours", Some(24 * 60 * 60)),
                            ("Until I turn it back on", None),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let mute_space = space.clone();
                            items.push(
                                menu_item(SharedString::from(format!("space-mute-{ix}")), Some("icons/bell-off.svg"), label, None)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        let until = secs.map(|s| crate::view::now() + s);
                                        this.set_space_notify(mute_space.clone(), Notify::Nothing, until, None, cx);
                                    }))
                                    .into_any_element(),
                            );
                        }
                    }
                }
                let can_leave = snapshot.accounts.iter().any(|a| {
                    a.key == space.account() && a.link == Link::Connected && a.can.leave_space
                });
                if can_leave {
                    let leave_key = space.clone();
                    let default_noun = if space.network == Network::Teams { "this team" } else { "this server" };
                    let leave_label = if space.network == Network::Teams { "Leave team…" } else { "Leave server…" };
                    let leave_name = if found_space.is_some() { space_name.clone() } else { default_noun.to_owned() };
                    items.push(menu_separator().into_any_element());
                    items.push(
                        menu_item_danger("space-leave", Some("icons/log-out.svg"), leave_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_leave_space(leave_key.clone(), leave_name.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
            Menu::Group { space, id, name, muted, .. } => {
                if space.network == Network::Discord {
                    let space_row = self.spaces.iter().find(|s| &s.key == space);
                    let manage_channels = space_row.is_some_and(|s| s.rights.manage_channels);
                    let manage_roles = space_row.is_some_and(|s| s.rights.manage_roles);
                    if manage_channels || manage_roles {
                        if manage_channels {
                            let create_space = space.clone();
                            let create_cat = id.clone();
                            items.push(
                                menu_item("group-create-channel", Some("icons/plus.svg"), "Create channel…", None)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        this.open_create_channel(create_space.clone(), Some(create_cat.clone()), cx);
                                    }))
                                    .into_any_element(),
                            );
                            let rename_space = space.clone();
                            let rename_id = id.clone();
                            let rename_name = name.clone();
                            items.push(
                                menu_item("group-rename", Some("icons/pencil.svg"), "Rename category…", None)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        this.open_edit_category(rename_space.clone(), rename_id.clone(), rename_name.clone(), cx);
                                    }))
                                    .into_any_element(),
                            );
                        }
                        if manage_roles {
                            let perm_key = Key { id: id.clone(), ..space.clone() };
                            items.push(
                                menu_item("group-permissions", Some("icons/lock.svg"), "Category permissions…", None)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        this.open_channel_permissions(perm_key.clone(), cx);
                                    }))
                                    .into_any_element(),
                            );
                        }
                        if manage_channels {
                            let delete_key = Key { id: id.clone(), ..space.clone() };
                            let delete_name = name.clone();
                            items.push(
                                menu_item_danger("group-delete", Some("icons/trash-2.svg"), "Delete category…")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        this.open_delete_channel(delete_key.clone(), delete_name.clone(), true, cx);
                                    }))
                                    .into_any_element(),
                            );
                        }
                        items.push(menu_separator().into_any_element());
                    }
                    if *muted {
                        let space_key = space.clone();
                        let cat_id = id.clone();
                        items.push(
                            menu_item("group-unmute", Some("icons/bell.svg"), "Unmute category", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.set_space_notify(space_key.clone(), Notify::All, None, Some(cat_id.clone()), cx);
                                }))
                                .into_any_element(),
                        );
                    } else {
                        items.push(menu_label("Mute category").into_any_element());
                        for (ix, (label, secs)) in [
                            ("For 15 minutes", Some(15 * 60i64)),
                            ("For 1 hour", Some(60 * 60)),
                            ("For 8 hours", Some(8 * 60 * 60)),
                            ("For 24 hours", Some(24 * 60 * 60)),
                            ("Until I turn it back on", None),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let space_key = space.clone();
                            let cat_id = id.clone();
                            items.push(
                                menu_item(SharedString::from(format!("group-mute-{ix}")), Some("icons/bell-off.svg"), label, None)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        let until = secs.map(|s| crate::view::now() + s);
                                        this.set_space_notify(space_key.clone(), Notify::Nothing, until, Some(cat_id.clone()), cx);
                                    }))
                                    .into_any_element(),
                            );
                        }
                    }
                }
            }
            Menu::Row(conversation, _) => {
                let (key, starred) = (conversation.key.clone(), conversation.starred);
                let unpinned = conversation.clone();
                let join = conversation.clone();
                let shown = conversation.clone();
                let ours = snapshot
                    .call
                    .as_ref()
                    .is_some_and(|call| call.conversation == conversation.key && !matches!(call.state, CallState::Failed { .. }));
                // A native voice channel joins from its menu too; a stage never does (07 §1).
                if super::sidebar::joins_from_list(&**conversation) && !ours {
                    items.push(
                        menu_item("join", Some("icons/volume-2.svg"), "Join voice", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.call_start(join.key.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                items.push(
                    menu_item("star", Some("icons/pin.svg"), if starred { "Unpin from Inbox" } else { "Pin to top of Inbox" }, None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.star(key.clone(), !starred, cx);
                        }))
                        .into_any_element(),
                );
                if conversation.rights.create_invite {
                    let invite_key = conversation.key.clone();
                    let invite_name = conversation.title.clone();
                    items.push(
                        menu_item("row-invite", Some("icons/user-plus.svg"), "Invite people…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_create_invite(
                                    super::sidebar::CreateInviteTarget::Channel(invite_key.clone(), invite_name.clone()),
                                    cx,
                                );
                            }))
                            .into_any_element(),
                    );
                }
                if conversation.space.is_some() {
                    items.push(
                        menu_item("reveal", Some("icons/corner-down-right.svg"), "Show in sidebar", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.reveal_in_sidebar(&shown, cx);
                            }))
                            .into_any_element(),
                    );
                }
                items.push(menu_separator().into_any_element());
                if conversation.unread > 0 || conversation.mentions > 0 {
                    let read = conversation.key.clone();
                    items.push(
                        menu_item("mark-read", Some("icons/check.svg"), "Mark as read", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.mark_conversation_read(read.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                } else {
                    let unread = conversation.key.clone();
                    items.push(
                        menu_item("mark-unread", Some("icons/mail.svg"), "Mark as unread", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.mark_conversation_unread(unread.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                let can = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.accounts.iter().find(|a| a.key == conversation.key.account()).map(|a| a.can))
                    .unwrap_or_default();
                if can.timed_mute {
                    if conversation.muted {
                        let unmute = conversation.key.clone();
                        items.push(
                            menu_item("unmute", Some("icons/bell.svg"), "Unmute notifications", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.set_row_mute(unmute.clone(), false, cx);
                                }))
                                .into_any_element(),
                        );
                    } else {
                        const PRESETS: &[(&str, &str, Option<i64>)] = &[
                            ("mute-15m", "Mute for 15 minutes", Some(15 * 60)),
                            ("mute-1h", "Mute for 1 hour", Some(60 * 60)),
                            ("mute-3h", "Mute for 3 hours", Some(3 * 60 * 60)),
                            ("mute-8h", "Mute for 8 hours", Some(8 * 60 * 60)),
                            ("mute-24h", "Mute for 24 hours", Some(24 * 60 * 60)),
                            ("mute-forever", "Mute until I turn it back on", None),
                        ];
                        for &(id, label, secs) in PRESETS {
                            let target = conversation.key.clone();
                            items.push(
                                menu_item(id, Some("icons/bell-off.svg"), label, None)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_menu(cx);
                                        let until = secs.map(|s| crate::view::now() + s);
                                        this.set_row_mute_until(target.clone(), true, until, cx);
                                    }))
                                    .into_any_element(),
                            );
                        }
                    }
                }
                if super::people::can_pin_on_network(conversation, &can) {
                    let net_pin_key = conversation.key.clone();
                    let next_pin = conversation.pin.is_none();
                    let words = super::people::network_pin_label(conversation);
                    items.push(
                        menu_item("network-pin", Some("icons/pin.svg"), words, None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.file_conversation(net_pin_key.clone(), Some(next_pin), None, None, None, cx);
                            }))
                            .into_any_element(),
                    );
                }
                if super::people::can_archive(conversation, &can) {
                    let arch_key = conversation.key.clone();
                    let next_archived = !conversation.archived;
                    let words = if conversation.archived { "Unarchive" } else { "Archive" };
                    items.push(
                        menu_item("archive", Some("icons/folder.svg"), words, None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.file_conversation(arch_key.clone(), None, Some(next_archived), None, None, cx);
                            }))
                            .into_any_element(),
                    );
                }
                if super::people::can_edit_folders(conversation, &can) {
                    let acct = conversation.key.account();
                    for (ix, folder) in self.sidebar_state.home.folders.iter().filter(|f| f.key.account() == acct).enumerate() {
                        let in_folder = conversation.folders.contains(&folder.key.id);
                        let folder_id = folder.key.id.clone();
                        let row_key = conversation.key.clone();
                        let label = if in_folder {
                            format!("Remove from {}", folder.name)
                        } else {
                            format!("Add to {}", folder.name)
                        };
                        items.push(
                            menu_item(SharedString::from(format!("folder-toggle-{ix}")), Some("icons/folder.svg"), label, None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    if in_folder {
                                        this.file_conversation(row_key.clone(), None, None, None, Some(folder_id.clone()), cx);
                                    } else {
                                        this.file_conversation(row_key.clone(), None, None, Some(folder_id.clone()), None, cx);
                                    }
                                }))
                                .into_any_element(),
                        );
                    }
                }
                if super::people::can_close_direct(conversation, &can) {
                    let closed = conversation.key.clone();
                    items.push(
                        menu_item("close-dm", Some("icons/x.svg"), "Close conversation", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.close_direct(closed.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if super::people::can_clear_history(conversation, &can) {
                    let cleared = conversation.as_ref().clone();
                    items.push(
                        menu_item("clear-history", Some("icons/trash-2.svg"), "Clear history…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.confirm_clear_history(cleared.clone(), false, cx);
                            }))
                            .into_any_element(),
                    );
                    let deleted = conversation.as_ref().clone();
                    items.push(
                        menu_item("delete-chat", Some("icons/trash-2.svg"), "Delete chat…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.confirm_clear_history(deleted.clone(), true, cx);
                            }))
                            .into_any_element(),
                    );
                }
                // A phone has no Return key to name.
                let keys = if super::phone::active() { None } else { Some("↵") };
                let window_key = unpinned.key.clone();
                items.push(
                    menu_item("open", Some(NO_GLYPH), "Open", keys)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.select(unpinned.key.clone(), &unpinned.title, cx);
                        }))
                        .into_any_element(),
                );
                let joined = self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref())
                    .is_some_and(|call| call.conversation == conversation.key && call.state == protocol::CallState::Joined);
                if super::sidebar::can_set_discord_voice_status(conversation, joined) {
                    let row = conversation.clone();
                    items.push(menu_item("set-voice-status", Some("icons/pencil.svg"), "Set channel status…", None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.open_voice_status(&row, cx);
                        })).into_any_element());
                }
                if conversation.key.network == Network::Discord
                    && !conversation.kind.is_direct()
                    && conversation.kind != protocol::ConversationKind::Thread
                    && (conversation.rights.rename || conversation.rights.manage_permissions || conversation.rights.delete_channel)
                {
                    items.push(menu_separator().into_any_element());
                    if conversation.rights.rename {
                        let edited = (**conversation).clone();
                        items.push(
                            menu_item("discord-edit-channel", Some("icons/pencil.svg"), "Edit channel…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_edit_channel(&edited, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    if conversation.rights.manage_permissions {
                        let perm_key = conversation.key.clone();
                        items.push(
                            menu_item("discord-channel-perms", Some("icons/lock.svg"), "Permissions…", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_channel_permissions(perm_key.clone(), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    if conversation.rights.delete_channel {
                        let del_key = conversation.key.clone();
                        let del_name = conversation.title.clone();
                        items.push(
                            menu_item_danger("discord-delete-channel", Some("icons/trash-2.svg"), "Delete channel…")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.open_delete_channel(del_key.clone(), del_name.clone(), false, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                }
                if !super::phone::active() {
                    items.push(
                        menu_item("open-window", Some("icons/picture-in-picture-2.svg"), "Open in New Window", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.pop_out_conversation(window_key.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if conversation.kind.is_direct() {
                    let row = (**conversation).clone();
                    items.push(menu_separator().into_any_element());
                    items.push(
                        menu_item_danger("forget", Some("icons/trash-2.svg"), "Delete from inbox")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.forget_conversation(row.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
            Menu::Folder(folder, _) => {
                let stored = self.sidebar_state.home.folders.iter().find(|known| &known.key == folder).cloned();
                let can = self.manages_space_folders(&folder.account());
                if can {
                    let rename = folder.clone();
                    items.push(
                        menu_item("folder-rename", Some("icons/pencil.svg"), "Rename…", None)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.open_folder_rename(rename.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                    let mut swatches = div().px(px(8.)).py(px(4.)).flex().flex_wrap().items_center().gap(px(6.));
                    for (ix, color) in FOLDER_COLOURS.iter().copied().enumerate() {
                        let on = stored.as_ref().and_then(|known| known.color) == Some(color);
                        let paint = folder.clone();
                        swatches = swatches.child(
                            div()
                                .id(("folder-colour", ix))
                                .size(px(18.))
                                .rounded(px(9.))
                                .bg(gpui::rgb(color))
                                .when(on, |dot| dot.border_2().border_color(theme::text()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.set_discord_folder(paint.clone(), None, Some(Some(color)), cx);
                                })),
                        );
                    }
                    items.push(swatches.into_any_element());
                    if stored.as_ref().is_some_and(|known| known.color.is_some()) {
                        let clear = folder.clone();
                        items.push(
                            menu_item("folder-clear-colour", Some("icons/x.svg"), "Clear colour", None)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.set_discord_folder(clear.clone(), None, Some(None), cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    items.push(menu_separator().into_any_element());
                }
                let read = folder.clone();
                items.push(
                    menu_item("folder-read", Some("icons/check.svg"), "Mark folder as read", None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.mark_folder_read(read.clone(), cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        items
    }

    /// The open menu on a phone: a bottom sheet holding what the desktop's popover holds, each
    /// row a full-width 44 pt target, headed by what it is about.
    pub(super) fn phone_menu(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let open = self.layout_state.menu.as_ref()?;
        let tall = f32::from(window.viewport_size().height);
        let close = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| this.close_menu(cx));
        let column = || div().w_full().flex().flex_col();
        let body: AnyElement = match open {
            Menu::Add => column().child(super::phone::sheet_title("Add an account")).children(self.add_rows(snapshot, cx)).into_any_element(),
            Menu::Own => self.phone_own(snapshot, window, cx).into_any_element(),
            Menu::Network(network) => {
                let head = div().flex().items_center().gap(px(10.)).px(px(8.)).h(px(44.)).child(network_icon(*network, 20.)).child(div().phone_title().child(network.label()));
                column().child(head).children(self.menu_items(open, snapshot, cx)).into_any_element()
            }
            Menu::Space(space) => {
                let found = self.spaces.iter().find(|s| &s.key == space);
                let name = found.map_or_else(String::new, |s| s.name.clone());
                let icon = found.and_then(|s| self.media_path(&s.key, s.icon.as_ref()));
                let head = div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(8.))
                    .h(px(52.))
                    .child(super::components::Avatar::new(space, &name, 28.).shape(super::components::Shape::Place).image_at(icon).build())
                    .child(div().min_w_0().truncate().phone_title().child(name));
                column().child(head).children(self.menu_items(open, snapshot, cx)).into_any_element()
            }
            Menu::Folder(folder, _) => {
                let name = self
                    .sidebar_state
                    .home
                    .folders
                    .iter()
                    .find(|f| &f.key == folder)
                    .map(|f| if f.name.is_empty() { "Folder".to_owned() } else { f.name.clone() })
                    .unwrap_or_else(|| "Folder".to_owned());
                let head = div().flex().items_center().gap(px(10.)).px(px(8.)).h(px(44.)).child(super::components::glyph("icons/folder.svg", 20., theme::text_soft())).child(div().min_w_0().truncate().phone_title().child(name));
                column().child(head).children(self.menu_items(open, snapshot, cx)).into_any_element()
            }
            Menu::Group { name, .. } => {
                column().child(super::phone::sheet_title(name.clone())).children(self.menu_items(open, snapshot, cx)).into_any_element()
            }
            Menu::VoiceOccupant(_, _, _) => column().children(self.menu_items(open, snapshot, cx)).into_any_element(),
            Menu::Row(conversation, _) => {
                // What the sheet is about: the row's own face and title, and where it lives.
                let place = conversation.space_title.trim();
                let place = if place.is_empty() || conversation.kind.is_direct() { conversation.key.network.label().to_owned() } else { format!("{} · {place}", conversation.key.network.label()) };
                let head = div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(12.))
                    .pt(px(4.))
                    .pb(px(12.))
                    .child(self.conversation_avatar(conversation, 40., theme::overlay(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().min_w_0().truncate().phone_title().child(conversation.title.clone()))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(network_icon(conversation.key.network, 14.))
                                    .child(div().min_w_0().truncate().phone_preview().text_color(theme::text_soft()).child(place)),
                            ),
                    );
                column().child(head).child(menu_separator()).children(self.phone_row_items(conversation, cx)).into_any_element()
            }
        };
        Some(super::phone::sheet("phone-menu", tall, body, close))
    }

    /// A conversation row's sheet on a phone: the desktop's row menu, then what Details and its
    /// "…" offer for the conversation, each where its network can do it.
    fn phone_row_items(&self, conversation: &Conversation, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items: Vec<AnyElement> = Vec::new();
        let row = conversation.clone();
        let key = row.key.clone();
        let network = key.network;
        let act = |id: &'static str, icon: &'static str, words: SharedString, then: Box<dyn Fn(&mut Shell, &mut Context<Shell>)>| {
            menu_item(id, Some(icon), words, None)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_menu(cx);
                    then(this, cx);
                }))
                .into_any_element()
        };
        let opened = row.clone();
        items.push(act("row-open", "icons/message-square.svg", "Open".into(), Box::new(move |this, cx| this.select(opened.key.clone(), &opened.title, cx))));
        if row.unread > 0 || row.mentions > 0 {
            let read = key.clone();
            items.push(act("row-read", "icons/check.svg", "Mark as read".into(), Box::new(move |this, cx| this.mark_read(read.clone(), cx))));
        } else {
            let unread = key.clone();
            items.push(act("row-unread", "icons/mail.svg", "Mark as unread".into(), Box::new(move |this, cx| this.mark_conversation_unread(unread.clone(), cx))));
        }
        let starred = row.starred;
        let star = key.clone();
        let words = if starred { "Unpin from Inbox" } else { "Pin to top of Inbox" };
        items.push(act("row-star", "icons/pin.svg", words.into(), Box::new(move |this, cx| this.star(star.clone(), !starred, cx))));
        // CONTRACT: muting has a request on Discord, Slack, Teams and Telegram, and is kept locally on Tiny.
        if matches!(network, Network::Discord | Network::Slack | Network::Teams | Network::Telegram | Network::Tiny) {
            let timed_mute = self
                .snapshot
                .as_ref()
                .and_then(|s| s.accounts.iter().find(|a| a.key == key.account()).map(|a| a.can.timed_mute))
                .unwrap_or(false);
            let muted = row.muted;
            if !muted && timed_mute {
                const PRESETS: &[(&str, &str, Option<i64>)] = &[
                    ("row-mute-15m", "Mute for 15 minutes", Some(15 * 60)),
                    ("row-mute-1h", "Mute for 1 hour", Some(60 * 60)),
                    ("row-mute-3h", "Mute for 3 hours", Some(3 * 60 * 60)),
                    ("row-mute-8h", "Mute for 8 hours", Some(8 * 60 * 60)),
                    ("row-mute-24h", "Mute for 24 hours", Some(24 * 60 * 60)),
                    ("row-mute-forever", "Mute until I turn it back on", None),
                ];
                for &(id, label, secs) in PRESETS {
                    let mute = key.clone();
                    items.push(act(
                        id,
                        "icons/bell-off.svg",
                        label.into(),
                        Box::new(move |this, cx| {
                            let until = secs.map(|s| crate::view::now() + s);
                            this.set_row_mute_until(mute.clone(), true, until, cx);
                        }),
                    ));
                }
            } else {
                let mute = key.clone();
                let (icon, words) = if muted { ("icons/bell.svg", "Unmute notifications") } else { ("icons/bell-off.svg", "Mute notifications") };
                items.push(act("row-mute", icon, words.into(), Box::new(move |this, cx| this.set_row_mute(mute.clone(), !muted, cx))));
            }
        }
        let details = key.clone();
        let person = row.kind == protocol::ConversationKind::Direct;
        let words = if person { "Profile" } else { "Details" };
        items.push(act("row-details", if person { "icons/user.svg" } else { "icons/info.svg" }, words.into(), Box::new(move |this, cx| this.details_conversation(details.clone(), cx))));
        if let Some(space) = row.space.as_ref().map(|space| Key { id: space.clone(), ..key.clone() }) {
            items.push(act("row-reveal", "icons/corner-down-right.svg", "Show in sidebar".into(), Box::new(move |this, cx| this.go(super::sidebar::Mode::Space(space.clone()), cx))));
        }
        let (link, leave) = self.row_reach(&row);
        if let Some(link) = link {
            items.push(menu_separator().into_any_element());
            let copied = link.clone();
            items.push(act(
                "row-link",
                "icons/link.svg",
                "Copy link".into(),
                Box::new(move |this, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()));
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Link copied".to_owned(), cx);
                }),
            ));
            items.push(act("row-open-in", "icons/external-link.svg", format!("Open in {}", network.label()).into(), Box::new(move |_, cx| cx.open_url(&link))));
        }
        if row.call == protocol::CallSupport::Handoff && network == Network::Slack {
            let handoff_key = key.clone();
            items.push(act(
                "row-huddle",
                "icons/headphones.svg",
                "Huddle in Slack".into(),
                Box::new(move |this, cx| this.call_handoff(handoff_key.clone(), cx)),
            ));
        }
        let can = self
            .snapshot
            .as_ref()
            .and_then(|s| s.accounts.iter().find(|a| a.key == key.account()).map(|a| a.can))
            .unwrap_or_default();
        if super::people::can_pin_on_network(&row, &can) {
            let net_pin_key = key.clone();
            let next_pin = row.pin.is_none();
            let words = super::people::network_pin_label(&row);
            items.push(act(
                "row-network-pin",
                "icons/pin.svg",
                words.into(),
                Box::new(move |this, cx| this.file_conversation(net_pin_key.clone(), Some(next_pin), None, None, None, cx)),
            ));
        }
        if super::people::can_archive(&row, &can) {
            let arch_key = key.clone();
            let next_archived = !row.archived;
            let words = if row.archived { "Unarchive" } else { "Archive" };
            items.push(act(
                "row-archive",
                "icons/folder.svg",
                words.into(),
                Box::new(move |this, cx| this.file_conversation(arch_key.clone(), None, Some(next_archived), None, None, cx)),
            ));
        }
        if super::people::can_edit_folders(&row, &can) {
            let acct = key.account();
            for (ix, folder) in self.sidebar_state.home.folders.iter().filter(|f| f.key.account() == acct).enumerate() {
                let in_folder = row.folders.contains(&folder.key.id);
                let folder_id = folder.key.id.clone();
                let row_key = key.clone();
                let label = if in_folder {
                    format!("Remove from {}", folder.name)
                } else {
                    format!("Add to {}", folder.name)
                };
                items.push(
                    menu_item(SharedString::from(format!("row-folder-toggle-{ix}")), Some("icons/folder.svg"), label, None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            if in_folder {
                                this.file_conversation(row_key.clone(), None, None, None, Some(folder_id.clone()), cx);
                            } else {
                                this.file_conversation(row_key.clone(), None, None, Some(folder_id.clone()), None, cx);
                            }
                        }))
                        .into_any_element(),
                );
            }
        }
        if network == Network::Slack && can.sidebar_sections {
            if let Some(space_id) = row.space.as_ref() {
                let space_key = Key {
                    network,
                    account: key.account.clone(),
                    id: space_id.clone(),
                };
                if let Some(tree) = self.trees.get(&space_key) {
                    let targets: Vec<(String, String)> = tree
                        .groups
                        .iter()
                        .filter(|g| g.kind == protocol::GroupKind::Section && row.group.as_deref() != Some(g.id.as_str()))
                        .map(|g| (g.id.clone(), g.name.clone()))
                        .collect();
                    if !targets.is_empty() {
                        items.push(menu_separator().into_any_element());
                        items.push(menu_label("Move to section").into_any_element());
                        for (ix, (to_id, to_name)) in targets.into_iter().enumerate() {
                            let sp = space_key.clone();
                            let conv = key.clone();
                            let from = row.group.clone();
                            items.push(
                                menu_item(
                                    SharedString::from(format!("row-move-section-{ix}")),
                                    Some("icons/folder.svg"),
                                    to_name,
                                    None,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_menu(cx);
                                    this.move_to_section(sp.clone(), conv.clone(), from.clone(), to_id.clone(), cx);
                                }))
                                .into_any_element(),
                            );
                        }
                    }
                }
            }
        }
        let close = super::people::can_close_direct(&row, &can);
        let clear = super::people::can_clear_history(&row, &can);
        if close || clear || leave {
            items.push(menu_separator().into_any_element());
        }
        if close {
            let closed = key.clone();
            let words = if network == Network::Teams { "Hide chat" } else { "Close conversation" };
            items.push(act("row-close", "icons/x.svg", words.into(), Box::new(move |this, cx| this.close_direct(closed.clone(), cx))));
        }
        if clear {
            let cleared = row.clone();
            items.push(act("row-clear", "icons/trash-2.svg", "Clear history…".into(), Box::new(move |this, cx| this.confirm_clear_history(cleared.clone(), false, cx))));
            let deleted = row.clone();
            items.push(act("row-delete-chat", "icons/trash-2.svg", "Delete chat…".into(), Box::new(move |this, cx| this.confirm_clear_history(deleted.clone(), true, cx))));
        }
        if leave {
            let left = row.clone();
            let words = if row.kind == protocol::ConversationKind::Group { "Leave group" } else { "Leave" };
            items.push(
                menu_item_danger("row-leave", Some("icons/log-out.svg"), words)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_menu(cx);
                        this.confirm_leave(left.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }
        if row.kind.is_direct() {
            let forgotten = row.clone();
            if !(close || leave) {
                items.push(menu_separator().into_any_element());
            }
            items.push(
                menu_item_danger("row-forget", Some("icons/trash-2.svg"), "Delete from inbox")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_menu(cx);
                        this.forget_conversation(forgotten.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }
        items
    }

    /// The rail avatar's sheet on a phone: who I am, my presence on each account and Settings
    /// (the desktop's own status menu, full width), then adding an account.
    fn phone_own(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let status = self.slots.own_menu.and_then(|draw| draw(self, window, cx));
        let status = status.unwrap_or_else(|| self.accounts_menu(snapshot, cx).into_any_element());
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(status)
            .child(menu_item("own-add", Some("icons/plus.svg"), "Add an account", None).on_click(cx.listener(|this, _, _, cx| this.open_menu(Menu::Add, cx))))
    }

    /// The add popover: networks not signed in, another Slack workspace, another Telegram
    /// account, another Tiny server, then what comes later.
    fn add_menu(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        popover().p(px(4.)).w(px(300.)).flex().flex_col().children(self.add_rows(snapshot, cx))
    }

    /// The add menu's rows: 48 in the popover, 56 in a phone's sheet.
    fn add_rows(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<gpui::Stateful<Div>> {
        let phone = super::phone::active();
        let row = |ix: usize, network: Network, title: SharedString, line: SharedString, available: bool, personal: bool| {
            let waiting = self.signing_in == Some(network);
            let failed = self.sign_in_failed.as_ref().filter(|(failed, _)| *failed == network).map(|(_, why)| why.clone());
            div()
                .id(("add", ix))
                .h(px(theme::ROW_TWO))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(10.))
                .when(waiting, |row| row.bg(theme::hover()))
                .when(available && self.signing_in.is_none(), |row| {
                    row.cursor_pointer()
                        .hover(|style| style.bg(theme::selected()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if personal {
                                this.sign_in_personal(cx);
                            } else {
                                this.sign_in(network, cx);
                            }
                        }))
                })
                .child(network_icon(network, 24.).when(!available, |icon| icon.opacity(0.4)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().when(phone, |title| title.phone_row()).text_color(if available { theme::text() } else { theme::text_soft() }).child(title))
                        .child(
                            div()
                                .truncate()
                                .type_small()
                                .text_color(if failed.is_some() { theme::danger() } else { theme::text_soft() })
                                .child(failed.map(SharedString::from).unwrap_or(if waiting {
                                    "Finish signing in in the window that opened.".into()
                                } else {
                                    line
                                })),
                        ),
                )
        };
        let mut list = Vec::new();
        let mut add = |network, title: &str, line: &str, available, personal: bool| {
            let ix = list.len() + 1;
            list.push(row(ix, network, title.to_owned().into(), line.to_owned().into(), available, personal).when(phone, |row| row.h(px(56.))));
        };
        // A phone offers only the networks it can sign in to (`signin::welcome::on_phone`), as
        // its first run does.
        let offered = |network: &Network| !phone || super::signin::welcome::on_phone(*network);
        for network in AVAILABLE.into_iter().filter(offered) {
            if snapshot.link(network) == Link::SignedOut {
                let caption = if phone && network == Network::Messages {
                    "Pair by QR; arrives while open"
                } else {
                    "Sign in"
                };
                add(network, network.label(), caption, true, false);
                if network == Network::Teams {
                    let (title, line) = another_account(Network::Teams).unwrap();
                    add(network, title, line, true, true);
                }
            } else if network == Network::Slack {
                add(network, "Add a Slack workspace", "Sign in to another workspace", true, false);
            } else if network == Network::Telegram {
                add(network, "Add another Telegram account", "Sign in to another account", true, false);
            } else if network == Network::Discord {
                let (title, line) = another_account(Network::Discord).unwrap();
                add(network, title, line, true, false);
            } else if network == Network::Teams {
                let (title, line) = another_account(Network::Teams).unwrap();
                add(network, title, line, true, true);
            } else if network == Network::Tiny {
                add(network, "Add a Tiny server", "Join with a code", true, false);
            }
        }
        for network in NETWORK_ORDER.into_iter().filter(|network| !AVAILABLE.contains(network)).filter(offered) {
            add(network, network.label(), "Not available yet", false, false);
        }
        list
    }

    /// What the own-avatar popover shows until the people slice fills it: who is signed in.
    fn accounts_menu(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        let mut list = div().w(px(240.)).flex().flex_col().child(menu_label("Signed in"));
        for account in &snapshot.accounts {
            let word = match account.link {
                Link::Connected => None,
                other => Some(super::components::link_word(other)),
            };
            list = list.child(
                div()
                    .h(px(theme::ROW_ONE))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(network_icon(account.key.network, 16.))
                    .child(div().flex_1().min_w_0().truncate().child(account.name.clone()))
                    .children(word.map(|word| div().type_small().text_color(theme::text_soft()).child(word))),
            );
        }
        list.child(menu_separator()).child(
            menu_item("own-settings", Some("icons/settings.svg"), "Settings…", Some("⌘,"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_menu(cx);
                    super::settings::open(super::settings::Section::last(), cx);
                })),
        )
    }
}

/// A number for an element id from an account's name.
fn items_id(account: &str) -> usize {
    account.bytes().fold(0usize, |hash, byte| hash.wrapping_mul(31).wrapping_add(byte as usize))
}

fn call_status(state: Option<&CallState>) -> &'static str {
    match state {
        None => "No active call",
        Some(CallState::RingingIn { .. }) => "Incoming call",
        Some(CallState::RingingOut) => "Calling",
        Some(CallState::Connecting) => "Connecting call",
        Some(CallState::Joined) => "Call connected",
        Some(CallState::Reconnecting) => "Reconnecting call",
        Some(CallState::Failed { .. }) => "Call failed",
    }
}

#[test]
fn a_reconnecting_call_changes_the_announced_status() {
    assert_eq!(call_status(Some(&CallState::Joined)), "Call connected");
    assert_eq!(call_status(Some(&CallState::Reconnecting)), "Reconnecting call");
    assert_eq!(call_status(None), "No active call");
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `TINYCLIENT_FRAMES=1` counts the window's frames, to see that an idle one draws none.
        #[cfg(feature = "gui-shot")]
        if std::env::var_os("TINYCLIENT_FRAMES").is_some() {
            static FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let frame = FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("tinyclient: frame {frame} at {} ms", super::LAUNCHED.get().map_or(0, |at| at.elapsed().as_millis()));
        }
        super::feed_platform_insets(window);
        // The form is decided once per frame, with hysteresis (`phone::form`), and the phone's
        // own entities exist before any of its screens is drawn.
        let viewport = window.viewport_size();
        let phone_form = super::phone::form(super::phone::active(), f32::from(viewport.width), f32::from(viewport.height));
        super::phone::set_active(phone_form);
        super::phone::tokens::set_landscape(phone_form && viewport.width > viewport.height);
        let sharing = super::streamer::sharing(self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref()));
        super::streamer::sync(sharing);
        self.arm_streamer_watch(cx);
        // A phone's window does not shrink for the soft keyboard (Android's NativeActivity and
        // iOS's full-screen view both keep their size and report the keyboard as an inset), so
        // the shell lifts its content above it; the composer then sits on the keyboard's edge
        // (`tokens::bottom_padding`).
        let keyboard = if super::phone::active() { super::phone::tokens::keyboard() } else { 0. };
        super::lock::tick();
        if super::lock::held() {
            let ringing = self.snapshot.as_ref().is_some_and(|snapshot| {
                let incoming = |call: &protocol::Call| matches!(call.state, CallState::RingingIn { .. });
                snapshot.call.as_ref().is_some_and(incoming) || snapshot.ring.as_ref().is_some_and(incoming)
            });
            let cover = self.lock_cover(ringing, window, cx);
            let locked_root = div()
                .size_full()
                .relative()
                .flex()
                .bg(theme::canvas())
                .text_color(theme::text())
                .type_ui()
                .font_family(theme::FONT)
                .when(keyboard > 0., |root| root.pb(px(keyboard)))
                .track_focus(&self.lock_focus)
                .key_context("ScreenLock")
                // Swallow app-level window-opening actions and any keys not handled by the unlock
                // input, so the lock cannot be bypassed from the keyboard.
                .on_action(|_: &super::Settings, _, _| {})
                .on_action(|_: &super::About, _, _| {})
                .on_action(|_: &super::KeyboardShortcuts, _, _| {})
                .on_action(|_: &super::ShowLogs, _, _| {})
                .on_action(|_: &super::CheckForUpdates, _, _| {})
                .on_action(|_: &super::NewMessage, _, _| {})
                .on_action(|_: &super::AddAccount, _, _| {})
                .on_key_down(cx.listener(|_, _, _, cx| cx.stop_propagation()))
                .child(cover);
            return if phone_form { locked_root } else { locked_root.children(window_chrome(window)) };
        }
        let root = self
            .keys(div(), cx)
            .size_full()
            .relative()
            .flex()
            .bg(theme::canvas())
            .text_color(theme::text())
            .type_ui()
            .font_family(theme::FONT)
            .on_mouse_move(cx.listener(Self::resize))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.layout_state.resizing.take().is_some() {
                        this.save_prefs(cx);
                    }
                }),
            );
        if let Some(account) = super::signin::take_open_existing(cx) {
            self.go(super::sidebar::Mode::Network(protocol::Network::Tiny), cx);
            self.prefer_tiny(account, cx);
        }
        let root = root.when(keyboard > 0., |root| root.pb(px(keyboard)));
        let root = root.children(super::signin::layer(window, cx));
        #[cfg(feature = "gui-shot")]
        if self.sheet {
            return root.child(super::shot::sheet());
        }
        self.note_place(window, cx);
        self.follow_changes(cx);
        self.refresh_menus(cx);
        self.note_tiny_page(cx);
        // Before the first answer the window is still drawn: rail, strips, an empty list, and
        // what it waits on. Never a blank canvas, never a developer message.
        let placeholder;
        let snapshot = match &self.snapshot {
            Some(snapshot) => snapshot,
            None => {
                placeholder = Snapshot::waiting();
                &placeholder
            }
        };
        let root = root.child(super::components::live_status(
            "call-state-status",
            call_status(self.call_shown(snapshot).as_ref().map(|call| &call.state)),
        ));
        // While the keychain is still being read, accounts arrive one network at a time: the
        // first-launch screen waits until it is done.
        let settled = !self.service.keychain.reading && self.shown().empty_state().is_none();
        let width = f32::from(window.viewport_size().width);
        if phone_form {
            if self.snapshot.is_some() && snapshot.signed_in().is_empty() && settled {
                return root.child(self.first_run(snapshot, cx));
            }
            // A phone has no window buttons to clear, and every pane it shows is the whole
            // screen: never beside the conversation, so sticky Details waits until asked.
            chrome::clear_end();
            self.note_room(true, Some(width));
            let screen = self.phone_screen(snapshot, window, cx);
            let root = root.child(screen);
            #[cfg(any(target_os = "android", target_os = "ios"))]
            let root = root.children(self.phone_share_layer(window, cx));
            return root
                .children(self.telegram_call_modal(window, cx))
                .children(self.meeting_layer(cx))
                .children(self.story_viewer_layer(window, cx))
                .children(self.toast_layer(width, cx))
                .children(super::settings::phone_page(window, cx))
                .children(self.streamer_mark(window, cx));
        }
        if self.snapshot.is_some() && snapshot.signed_in().is_empty() && settled {
            return root.child(self.first_run(snapshot, cx)).children(window_chrome(window)).children(self.streamer_mark(window, cx));
        }

        let state = &self.layout_state;
        // Where a sign-in window should open: over this one.
        let origin = window.bounds().origin;
        self.window_at.set(Some((f32::from(origin.x) as i32, f32::from(origin.y) as i32)));
        // Too narrow for the pane beside the conversation: sticky Details hides (07 §4).
        self.note_room(narrow(width, state.sidebar_width, state.sidebar_hidden, state.right_width), None);
        let right_width = state.right.as_ref().filter(|_| !self.pane_tucked()).map(|_| state.right_width);
        let overview = discord_overview_width(width, state.sidebar_width, state.sidebar_hidden, right_width, matches!(&self.sidebar_state.mode, super::sidebar::Mode::Space(space) if space.network == Network::Discord));
        let plan = plan(width - overview, state.sidebar_width, state.sidebar_hidden, right_width);
        self.note_room(self.pane_narrow(), Some(conversation_width(width - overview, &plan)));
        let floating = plan.sidebar.is_none() && state.floating;
        let replaced = plan.right.is_some_and(|(_, replaces)| replaces);
        let call_view = self.call_view_active(snapshot);
        // With nothing open on a phone, the list is the whole screen: there is no sidebar
        // column to fall back to, and the conversation only exists when something is open.
        let list_alone = if call_view {
            false
        } else if phone(width) {
            snapshot.open.is_none()
        } else {
            snapshot.open.is_none() && plan.sidebar.is_none() && !state.sidebar_hidden && !floating && !replaced
        };
        // Where the window's own buttons sit, on Windows and a Linux that leaves them to us.
        let end = match () {
            _ if plan.right.is_some() => End::Right,
            _ if list_alone && !(matches!(self.sidebar_state.mode, super::sidebar::Mode::Network(Network::Tiny)) && self.tiny_open()) => End::List,
            _ => End::Conversation,
        };
        chrome::set_end(end, window);

        let desktop = root.child(self.rail(snapshot, window, cx))
            .when(overview > 0., |root| root.child(self.discord_overview(snapshot, overview, window, cx)))
            .children(plan.sidebar.map(|width| {
                self.sidebar(snapshot, width, window, cx).child(self.resize_handle(Edge::Sidebar, cx))
            }))
            .when(list_alone, |root| {
                // A narrow window has no pane beside the list. Tiny draws its people in the
                // page, since the sidebar is not there.
                if matches!(self.sidebar_state.mode, super::sidebar::Mode::Network(Network::Tiny)) && self.tiny_open() {
                    root.child(div().flex_1().min_w_0().h_full().child(self.tiny_page(false, window, cx)))
                } else {
                    root.child(self.sidebar(snapshot, width - RAIL_WIDTH, window, cx))
                }
            })
            .when(!replaced && !list_alone, |root| {
                let pane = match (&snapshot.open, self.keychain_state(window, cx)) {
                    (_, Some(waiting)) => waiting,
                    (_, None) if call_view => self.call_view_pane(snapshot, window, cx),
                    (Some(_), None) => self.conversation(snapshot, window, cx),
                    (None, None) if self.tiny_open() => self.tiny_page(plan.sidebar.is_some() || floating, window, cx),
                    (None, None) => self.nothing_open(window, cx),
                };
                root.child(div().flex_1().min_w_0().h_full().child(pane))
            })
            .children(plan.right.and_then(|(width, replaces)| self.right_pane(width, replaces, window, cx)))
            .children(self.banner_layer(plan.sidebar, cx))
            .when(floating, |root| {
                let width = state.sidebar_width.min(width - RAIL_WIDTH);
                root.child(
                    scrim().left(px(RAIL_WIDTH)).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.layout_state.floating = false;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(RAIL_WIDTH))
                        .h_full()
                        .shadow(theme::shadow_pane())
                        .occlude()
                        .child(self.sidebar(snapshot, width, window, cx)),
                )
            })
            .children(self.switcher(window, cx))
            .children(self.create_channel_modal(cx))
            .children(self.create_invite_modal(cx))
            .children(self.create_category_modal(cx))
            .children(self.edit_channel_modal(cx))
            .children(self.voice_status_modal(cx))
            .children(self.voice_move_modal(cx))
            .children(self.voice_settings_modal(cx))
            .children(self.move_channels_modal(cx))
            .children(self.delete_channel_modal(cx))
            .children(self.folder_edit_modal(cx))
            .children(self.forum_post_modal(cx))
            .children(self.join_server_modal(cx))
            .children(self.conference_link_modal(cx))
            .children(self.create_server_modal(cx))
            .children(self.events_modal(cx))
            .children(self.server_gate_modal(cx))
            .children(self.leave_space_modal(cx))
            .children(self.discord_settings_layer(window, cx))
            .children(self.browse_channels_modal(cx))
            .children(self.discord_admin_layer(cx))
            .children(self.discord_admin_modal(window, cx))
            .children(self.link_ask_modal(window, cx))
            .children(self.call_full_stage(snapshot, cx))
            .children(self.telegram_call_modal(window, cx))
            .children(self.tiny_layer(window, cx))
            .children(self.menu_layer(snapshot, plan.sidebar.unwrap_or(state.sidebar_width), overview, window, cx))
            .children(self.meeting_layer(cx))
            .children(self.story_viewer_layer(window, cx))
            .children(self.toast_layer(width, cx))
            .children(window_chrome(window))
            .children(self.streamer_mark(window, cx));
        let desktop = if let Some(live_bar) = self.live_location_bar(cx) {
            desktop.child(div().absolute().top_0().left(px(RAIL_WIDTH)).right_0().child(live_bar))
        } else {
            desktop
        };
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let desktop = desktop.children(self.phone_share_layer(window, cx));
        desktop
    }
}

impl Shell {
    /// The only thing drawn while the screen lock is held. A ring can still be answered.
    fn lock_cover(&mut self, ringing: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let error = super::lock::unlock_error();
        let asking = super::lock::asking();
        #[cfg(target_os = "linux")]
        let phrase = {
            let phrase = self.lock_field(cx);
            let phrase_focus = gpui::Focusable::focus_handle(phrase.read(cx), cx);
            if !self.lock_focus.is_focused(window) && !phrase_focus.is_focused(window) {
                window.focus(&phrase_focus, cx);
            }
            phrase
        };
        #[cfg(not(target_os = "linux"))]
        if !self.lock_focus.is_focused(window) {
            window.focus(&self.lock_focus, cx);
        }
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(theme::canvas())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .px(px(24.))
            .child(div().type_title().child("tinyclient is locked"))
            .child(div().max_w(px(420.)).type_small().text_color(theme::text_soft()).child(super::lock::CAPTION))
            .when(ringing, |cover| {
                cover.child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(button("lock-answer", ButtonKind::Primary, "Answer").on_click(cx.listener(|this, _, _, cx| this.call_accept(cx))))
                        .child(button("lock-decline", ButtonKind::Secondary, "Decline").on_click(cx.listener(|this, _, _, cx| this.call_decline(cx)))),
                )
            })
            .when(cfg!(target_os = "linux"), |cover| {
                #[cfg(target_os = "linux")]
                let cover = cover.child(div().w(px(240.)).child(phrase));
                cover
            })
            .child(
                button("lock-unlock", ButtonKind::Primary, if asking { "Asking the system…" } else { "Unlock" }).on_click(cx.listener(|this, _, _, cx| this.unlock_screen(cx))),
            )
            .when(!error.is_empty(), |cover| cover.child(div().type_small().text_color(theme::danger()).child(error)))
    }

    #[cfg(target_os = "linux")]
    fn lock_field(&mut self, cx: &mut Context<Self>) -> gpui::Entity<super::input::TextInput> {
        if self.lock_phrase.is_none() {
            self.lock_phrase = Some(cx.new(|cx| super::input::TextInput::secret("Passphrase", cx)));
        }
        self.lock_phrase.clone().expect("the passphrase field was just created")
    }

    fn unlock_screen(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "linux")]
        {
            let phrase = self.lock_phrase.as_ref().map(|input| input.read(cx).text().to_owned()).unwrap_or_default();
            if super::lock::unlock_phrase(&phrase) {
                if let Some(input) = &self.lock_phrase {
                    input.update(cx, |input, cx| input.clear(cx));
                }
                cx.notify();
                return;
            }
            cx.notify();
            return;
        }
        #[cfg(not(target_os = "linux"))]
        {
            if super::lock::asking() {
                return;
            }
            super::lock::set_asking(true);
            cx.notify();
            self.off_thread(
                cx,
                super::lock::verify_owner,
                |_, result, cx| {
                    super::lock::set_asking(false);
                    match result {
                        Ok(outcome) if super::lock::accept_outcome(outcome) => super::lock::unlock_verified(),
                        Ok(outcome) => super::lock::set_unlock_error(super::lock::outcome_words(outcome)),
                        Err(words) => super::lock::set_unlock_error(words),
                    }
                    cx.notify();
                },
            );
        }
    }
}

impl Shell {
    /// One poll while "while sharing" also watches for OBS or Streamlabs. Idle windows otherwise
    /// would not notice the broadcast app until the next paint.
    fn arm_streamer_watch(&mut self, cx: &mut Context<Self>) {
        if !super::streamer::take_watch() {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let keep = this
                    .update(cx, |_, cx| {
                        let keep = super::streamer::watch_wanted();
                        if keep {
                            cx.notify();
                        }
                        keep
                    })
                    .unwrap_or(false);
                if !keep {
                    super::streamer::release_watch();
                    break;
                }
            }
        })
        .detach();
    }

    /// The title-bar mark while Streamer mode is on. One click stores Off.
    fn streamer_mark(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !super::streamer::is_active() {
            return None;
        }
        let chip = div()
            .id("streamer-off")
            .absolute()
            .top(px(12.))
            .right(px(chrome::width(window) + 12.))
            .occlude()
            .h(px(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::hover())
            .border_1()
            .border_color(theme::border())
            .type_small()
            .font_weight(theme::MEDIUM)
            .text_color(theme::text())
            .cursor_pointer()
            .tooltip(tip("Streamer mode is on. Click to turn it off.", None))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.streamer_turn_off(cx)))
            .child(glyph("icons/eye-off.svg", 14., theme::text_soft()))
            .child("Streamer mode");
        Some(gpui::deferred(chip).priority(4).into_any_element())
    }

    fn streamer_turn_off(&mut self, cx: &mut Context<Self>) {
        let mut values = super::app::prefs::values();
        values.set(protocol::settings::GLOBAL, protocol::settings::STREAMER_MODE, Some("off".into()));
        super::app::prefs::set_values(values);
        let request = protocol::Request::Set {
            scope: protocol::settings::GLOBAL.to_owned(),
            key: protocol::settings::STREAMER_MODE.to_owned(),
            value: Some("off".into()),
        };
        cx.background_executor().spawn(async move { crate::client::ask(&request) }).detach();
        cx.notify();
    }
}

/// The window's own buttons and resize edges, where the platform leaves them to the window.
fn window_chrome(window: &Window) -> impl Iterator<Item = AnyElement> {
    let edges = chrome::edges(window).map(IntoElement::into_any_element);
    let buttons = chrome::buttons(window, |_, _, cx: &mut App| super::app::menubar::close_window(cx));
    edges.into_iter().chain(buttons)
}

impl Shell {
    /// Opens an external web link using the mounted conversation pane when one is open, or
    /// handles confirmation at the shell level when no conversation is open.
    pub(in crate::gui) fn open_external_url(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_some() {
            let pane = self.pane(cx);
            pane.update(cx, |pane, cx| pane.open_external_link(url, window, cx));
            return;
        }
        if super::conversation::links::is_mailto(url) {
            cx.open_url(url);
            return;
        }
        let Some(dest) = protocol::link::Destination::parse(url) else {
            return;
        };
        let choices = super::app::prefs::choices();
        let trusted = super::app::prefs::trusted_links().contains(&dest.domain);
        if super::conversation::links::opens_at_once(&dest, choices.confirm_links, trusted) {
            cx.open_url(&dest.target(choices.strip_tracking));
            return;
        }
        self.layout_state.link_ask = Some(super::conversation::links::LinkAsk { dest, remember: false });
        cx.notify();
    }

    fn close_link_ask(&mut self, cx: &mut Context<Self>) {
        if self.layout_state.link_ask.take().is_some() {
            cx.notify();
        }
    }

    fn go_to_link(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = self.layout_state.link_ask.take() else {
            return;
        };
        if ask.remember && ask.can_remember() {
            let mut trusted = super::app::prefs::trusted_links();
            if trusted.add(&ask.dest.domain) {
                super::app::prefs::set_trusted_links(trusted);
                self.save_prefs(cx);
            }
        }
        cx.open_url(&ask.target());
        cx.notify();
    }

    fn link_ask_modal(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.layout_state.link_ask.as_ref()?;
        let dest = &ask.dest;
        let target = ask.target();

        let mut body = div().mt(px(12.)).flex().flex_col().gap(px(10.));
        let mut site = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(div().type_title().font_weight(theme::SEMIBOLD).child(dest.domain_shown.clone()));
        if dest.host_shown != dest.domain_shown {
            site = site.child(div().type_ui().text_color(theme::text_soft()).child(dest.host_shown.clone()));
        }
        body = body.child(site);

        let note = |warn: bool, words: String| {
            div()
                .p(px(10.))
                .rounded(px(theme::R_CARD))
                .bg(if warn { theme::warning_soft() } else { theme::raised() })
                .type_ui()
                .text_color(if warn { theme::text() } else { theme::text_soft() })
                .child(words)
        };
        match dest.risk {
            protocol::link::Risk::Lookalike => {
                body = body.child(note(
                    true,
                    format!(
                        "Careful: this name mixes alphabets, or uses letters that look like Latin ones. It may be pretending to be another site. Its real address is {}.",
                        dest.host
                    ),
                ));
            }
            protocol::link::Risk::Idn => {
                body = body.child(note(
                    false,
                    format!("This site's name uses letters outside A to Z. Its address on the internet is {}.", dest.host),
                ));
            }
            protocol::link::Risk::None => {}
        }
        if dest.has_userinfo {
            body = body.child(note(
                true,
                "Careful: the part of the address before the @ is a user name, not the site. The site is the one above.".to_owned(),
            ));
        }
        body = body.child(
            div()
                .id("shell-link-address")
                .p(px(10.))
                .rounded(px(theme::R_CARD))
                .bg(theme::raised())
                .line_clamp(4)
                .type_small()
                .text_color(theme::text_soft())
                .child(target.clone()),
        );
        if ask.can_remember() {
            let on = ask.remember;
            body = body.child(
                div()
                    .id("shell-link-remember")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .type_ui()
                    .child(
                        div()
                            .flex_none()
                            .size(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if on { theme::accent() } else { theme::border_strong() })
                            .when(on, |box_| box_.bg(theme::accent()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::on_accent())
                            .child(if on { "✓" } else { "" }),
                    )
                    .child(format!("Don't ask again for {}", dest.domain_shown))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(ask) = this.layout_state.link_ask.as_mut() {
                            ask.remember = !ask.remember;
                            cx.notify();
                        }
                    })),
            );
        }

        let room = f32::from(window.viewport_size().width) - 32.;
        let copy = target.clone();
        let panel = super::components::modal("Open this link?")
            .w(px(if super::phone::active() { room.min(440.) } else { 440. }))
            .child(body)
            .child(
                super::components::modal_buttons()
                    .child(button("shell-link-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close_link_ask(cx))))
                    .child(button("shell-link-copy", ButtonKind::Secondary, "Copy link").on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                        this.close_link_ask(cx);
                    })))
                    .child(button("shell-link-open", ButtonKind::Primary, "Open").on_click(cx.listener(|this, _, _, cx| this.go_to_link(cx)))),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let size = window.viewport_size();
        let scrim = div()
            .id("shell-link-ask")
            .w(size.width)
            .h(size.height)
            .bg(theme::scrim())
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_link_ask(cx)))
            .child(fade_in("shell-link-ask-panel", 160, panel));
        Some(gpui::deferred(gpui::anchored().position(gpui::point(px(0.), px(0.))).child(scrim)).priority(1).into_any_element())
    }

    /// The toasts, 16 from the bottom-right: the crash toast stays until dismissed and sits
    /// above any other.
    fn toast_layer(&self, window_width: f32, cx: &mut Context<Self>) -> Option<Div> {
        let width = place::toast_width(window_width);
        let crash = self.crash_toast(width, cx);
        let update = self.update_toast(width, cx);
        let toast = self.layout_state.toast.as_ref().map(|(id, icon, words, reveal)| {
            let (id, icon, words) = (*id, *icon, words.clone());
            match reveal.clone() {
                Some(path) => {
                    let body = toast(icon, words)
                        .id(("toast-body", id as usize))
                        .w(px(width))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.reveal_path(&path);
                            this.layout_state.toast = None;
                            cx.notify();
                        }));
                    fade_in(("toast", id as usize), 160, body).into_any_element()
                }
                None => fade_in(("toast", id as usize), 160, toast(icon, words).w(px(width))).into_any_element(),
            }
        });
        if crash.is_none() && toast.is_none() && update.is_none() {
            return None;
        }
        // On a phone the toast clears the home indicator.
        let bottom = 16. + if super::phone::active() { super::phone::tokens::safe_bottom() } else { 0. };
        Some(
            div()
                .absolute()
                .right(px(16.))
                .bottom(px(bottom))
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(toast)
                .children(update.map(|update| fade_in("update-toast", 160, div().child(update))))
                .children(crash.map(|crash| fade_in("crash-toast", 160, div().child(crash)))),
        )
    }
}

/// An extra sign-in once the usual one exists. Teams is a personal Microsoft account, not a
/// second work sign-in. Discord is another Discord account.
pub(crate) fn another_account(network: Network) -> Option<(&'static str, &'static str)> {
    match network {
        Network::Discord => Some(("Add another Discord account", "Sign in to another account")),
        Network::Teams => Some(("Personal Microsoft account", "Teams free, teams.live.com")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_account_names_discord_and_personal_teams() {
        assert_eq!(another_account(Network::Discord), Some(("Add another Discord account", "Sign in to another account")));
        assert_eq!(another_account(Network::Teams), Some(("Personal Microsoft account", "Teams free, teams.live.com")));
        assert_eq!(another_account(Network::Slack), None);
    }

    #[test]
    fn discord_columns_keep_a_readable_conversation_and_yield_to_narrow_windows() {
        let minimum = RAIL_WIDTH + SIDEBAR_WIDTH + 280. + CONVERSATION_MIN;
        assert_eq!(discord_overview_width(minimum, SIDEBAR_WIDTH, false, None, true), 280.);
        assert_eq!(discord_overview_width(minimum - 1., SIDEBAR_WIDTH, false, None, true), 0.);
        assert_eq!(discord_overview_width(1800., SIDEBAR_WIDTH, true, None, true), 0.);
        assert_eq!(discord_overview_width(1800., SIDEBAR_WIDTH, false, None, false), 0.);
        assert_eq!(discord_overview_width(minimum, SIDEBAR_WIDTH, false, Some(RIGHT_WIDTH), true), 0.);
        let window = minimum + RIGHT_WIDTH;
        let overview = discord_overview_width(window, SIDEBAR_WIDTH, false, Some(RIGHT_WIDTH), true);
        let planned = plan(window - overview, SIDEBAR_WIDTH, false, Some(RIGHT_WIDTH));
        assert_eq!(planned.right, Some((RIGHT_WIDTH, false)));
        assert_eq!(conversation_width(window - overview, &planned), CONVERSATION_MIN);
    }

    #[test]
    fn the_conversation_gives_up_width_then_the_right_pane_replaces_it_then_the_sidebar_leaves() {
        // Room for everything.
        assert_eq!(plan(1240., 316., false, Some(340.)), Plan { sidebar: Some(316.), right: Some((340., false)) });
        // 1120 is the last width at which a thread sits beside the conversation.
        assert_eq!(plan(1120., 316., false, Some(340.)).right, Some((340., false)));
        assert_eq!(plan(1119., 316., false, Some(340.)).right, Some((731., true)));
        // The members list is narrower, so it holds on longer.
        assert_eq!(plan(1020., 316., false, Some(240.)).right, Some((240., false)));
        // 780 keeps the sidebar; one point less and it leaves the row.
        assert_eq!(plan(780., 316., false, None).sidebar, Some(316.));
        assert_eq!(plan(779., 316., false, None).sidebar, None);
        // Below the phone boundary the right pane takes the whole window: there is no rail
        // beside it to keep.
        assert_eq!(plan(540., 316., false, Some(340.)), Plan { sidebar: None, right: Some((540., true)) });
        // A sidebar dragged narrow keeps its place in a narrower window.
        assert_eq!(plan(730., 260., false, None).sidebar, Some(260.));
        // Hidden by hand: the conversation takes its room.
        assert_eq!(plan(1240., 316., true, Some(340.)), Plan { sidebar: None, right: Some((340., false)) });
    }

    /// What is drawn at each width of the table in `design/polish/07` §7, for Members (sticky
    /// Details) and for a thread.
    fn drawn(window: f32, sticky: bool) -> (Option<f32>, &'static str) {
        use crate::gui::pane::{tucked, Layer, Stack};
        use protocol::{AccountKey, Network};
        let key = Key::new(&AccountKey { network: Network::Discord, account: "1".into() }, "c");
        let stack = Stack { base: Some(Layer::Members(key)), push: None };
        let tight = narrow(window, SIDEBAR_WIDTH, false, RIGHT_WIDTH);
        let hidden = sticky && tucked(&stack, tight, false);
        let plan = plan(window, SIDEBAR_WIDTH, false, (!hidden).then_some(RIGHT_WIDTH));
        let pane = match plan.right {
            None => "conversation alone",
            Some((_, false)) => "pane beside",
            Some((_, true)) => "pane replaces",
        };
        (plan.sidebar, pane)
    }

    #[test]
    fn a_phone_is_one_column_and_the_sidebar_never_sits_beside_it() {
        assert!(phone(390.) && !phone(600.));
        // 390 asks for a 316 sidebar and a 300 thread: both would leave nothing, so neither
        // is beside the conversation. The right pane takes the whole window.
        assert_eq!(plan(390., 316., false, None), Plan { sidebar: None, right: None });
        assert_eq!(plan(390., 316., false, Some(300.)), Plan { sidebar: None, right: Some((390., true)) });
        assert_eq!(conversation_width(390., &plan(390., 316., false, None)), 390.);
        // A hidden sidebar and a floating one change nothing on a phone.
        assert_eq!(plan(390., 316., true, None).sidebar, None);
        // The first width that is not a phone is unchanged by the phone rule.
        assert_eq!(plan(600., 316., false, None).sidebar, None);
        assert_eq!(plan(780., 316., false, None).sidebar, Some(316.));
    }

    #[test]
    fn the_window_can_be_a_phone() {
        assert_eq!(WINDOW_MIN, (360., 480.));
        assert!(WINDOW_MIN.0 < PHONE);
        assert!(phone(WINDOW_MIN.0));
    }

    #[test]
    fn details_hides_and_a_thread_replaces_at_each_width_of_the_table() {
        assert_eq!(drawn(1440., true), (Some(316.), "pane beside"));
        assert_eq!(drawn(1120., true), (Some(316.), "pane beside"));
        assert_eq!(conversation_width(1120., &plan(1120., 316., false, Some(300.))), 432.);
        // 860: Details hides and the conversation always shows; a thread stands in for it.
        assert_eq!(drawn(860., true), (Some(316.), "conversation alone"));
        assert_eq!(drawn(860., false), (Some(316.), "pane replaces"));
        assert_eq!(conversation_width(860., &plan(860., 316., false, None)), 472.);
        // 640: the sidebar floats on demand; the conversation is 568.
        assert_eq!(drawn(640., true), (None, "conversation alone"));
        assert_eq!(drawn(640., false), (None, "pane replaces"));
        assert_eq!(conversation_width(640., &plan(640., 316., false, None)), 568.);
        // 1080 is the narrowest window that holds it beside.
        assert!(narrow(1079., 316., false, 300.) && !narrow(1080., 316., false, 300.));
    }
}
