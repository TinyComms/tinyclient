//! The phone form: the desktop app at a phone's size. Its home is the desktop's own rail and
//! sidebar side by side (the rail at `layout::rail_width`), and whatever the sidebar opens is
//! pushed over it full-screen: a conversation, then the right pane's page (Details, members, a
//! thread, a profile). Menus open as bottom sheets, the switcher full-screen. Every screen here
//! is drawn only below `layout::PHONE`; at 600 and wider the window is the desktop it always was.

pub mod conversation;
pub mod tokens;

use crate::gui::components::{fade_in, icon_button, scrim};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use crate::gui::phone::tokens::PhoneType;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, MouseButton, Pixels, Point, Window};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// This window is a phone, set once per frame from its width. One window, one form, read by
/// deep modules that would otherwise have to thread the width through every signature — the
/// same bargain `theme::text_step` makes.
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_active(on: bool) {
    ACTIVE.store(on, Ordering::Relaxed);
}

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Whether a window `width` wide is a phone, given whether it was one a frame ago. A phone is
/// under `layout::PHONE`; once a window is a phone it stays one until it is 40 pt wider than that,
/// so a width that hovers at the edge (a split screen being dragged, a fold opening) does not
/// flip the layout every frame.
pub fn form(was_phone: bool, width: f32) -> bool {
    let edge = crate::gui::layout::PHONE;
    if was_phone {
        width < edge + 40.
    } else {
        width < edge
    }
}

/// How long a finger rests on a row before it is a long press: the row's menu, not a tap.
pub const LONG_PRESS: Duration = Duration::from_millis(450);

/// A pointer position as the two numbers the slop test wants.
pub fn at(point: Point<Pixels>) -> (f32, f32) {
    (f32::from(point.x), f32::from(point.y))
}

/// A token for a row, from what names it, so a release can be matched to its press.
pub fn token(parts: &[&str]) -> u64 {
    parts.iter().flat_map(|part| part.bytes().chain(std::iter::once(0))).fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Default)]
pub struct State {
    /// Where a left-edge drag began, for the back gesture.
    edge: Option<f32>,
    /// A press on a row that has not yet been released: it acts only if it ends where it began.
    /// A press that travels past the slop is a scroll, and does nothing.
    press: Option<Press>,
    /// Counts presses, so a long-press timer knows its press is still the one down.
    presses: u64,
}

/// A press on a row, and where it began.
#[derive(Clone, Copy)]
struct Press {
    token: u64,
    at: (f32, f32),
    count: u64,
}

impl State {
    /// A finger went down on a row. Answers the press's number, for its long-press timer.
    pub fn press_down(&mut self, token: u64, at: (f32, f32)) -> u64 {
        self.presses += 1;
        self.press = Some(Press { token, at, count: self.presses });
        self.presses
    }

    /// The pointer moved. Past the slop the press is a scroll, and it will not act.
    pub fn press_move(&mut self, to: (f32, f32)) {
        if self.press.is_some_and(|press| !tokens::within_slop(press.at, to)) {
            self.press = None;
        }
    }

    /// The finger lifted. True when it lifted on the row it went down on, within the slop.
    pub fn press_up(&mut self, token: u64, to: (f32, f32)) -> bool {
        match self.press.take() {
            Some(press) => press.token == token && tokens::within_slop(press.at, to),
            None => false,
        }
    }

    /// The long-press timer fired: true, and the press is spent, when press `count` is still
    /// down where it began.
    pub fn press_held(&mut self, count: u64) -> bool {
        if self.press.is_some_and(|press| press.count == count) {
            self.press = None;
            return true;
        }
        false
    }

    pub fn press_cancel(&mut self) {
        self.press = None;
    }

    fn arm_edge(&mut self, x: f32) {
        self.edge = Some(x);
    }

    fn edge_start(&self) -> Option<f32> {
        self.edge
    }

    fn disarm_edge(&mut self) {
        self.edge = None;
    }
}

/// A bottom sheet over the screen: the scrim behind it closes it, a grabber on top, 12 pt top
/// corners, and the home indicator's room under its last row. `tall` caps it below the status bar.
pub fn sheet(id: &'static str, tall: f32, body: impl IntoElement, close: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static) -> Div {
    let cap = tall - tokens::safe_top() - 24.;
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .child(scrim().on_mouse_down(MouseButton::Left, close))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .max_h(px(cap))
                .flex()
                .flex_col()
                .bg(theme::overlay())
                .rounded_t(px(theme::R_MODAL))
                .border_t_1()
                .border_color(theme::overlay_edge())
                .shadow(tokens::shadow_sheet())
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().flex_none().h(px(20.)).flex().items_center().justify_center().child(div().w(px(36.)).h(px(4.)).rounded(px(2.)).bg(theme::border_strong())))
                .child(fade_in(id, 120, div().id(id).flex_1().min_h_0().overflow_y_scroll().px(px(8.)).pb(px(8. + tokens::safe_bottom())).child(body))),
        )
}

/// A sheet's heading: what the sheet is about, 44 tall, under the grabber.
pub fn sheet_title(words: impl Into<gpui::SharedString>) -> Div {
    div().flex_none().h(px(44.)).px(px(8.)).flex().items_center().child(div().min_w_0().truncate().phone_title().child(words.into()))
}

impl Shell {
    /// The phone, whole: what is pushed on top (the right pane's page, else the conversation),
    /// or the home of rail and sidebar; then the switcher and the open menu over it.
    pub(in crate::gui) fn phone_screen(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let width = f32::from(window.viewport_size().width);
        let right = self.layout_state.right.is_some() && !self.pane_tucked();
        let screen: AnyElement = match self.keychain_state(window, cx) {
            Some(waiting) if snapshot.open.is_none() => self.phone_page(waiting).into_any_element(),
            _ if right => self.phone_right_page(window, cx).into_any_element(),
            _ if snapshot.open.is_some() => self.phone_page(self.conversation(snapshot, window, cx)).into_any_element(),
            _ => self.phone_home(snapshot, width, window, cx).into_any_element(),
        };
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .text_color(theme::text())
            .child(screen)
            .children(self.switcher(window, cx))
            .children(self.phone_slack_sheet(window, cx))
            .children(self.phone_menu(snapshot, window, cx))
    }

    /// The home: the desktop's rail and its sidebar beside it, filling the screen. Tiny's page
    /// takes the sidebar's place, as it does in a narrow desktop window.
    fn phone_home(&self, snapshot: &Snapshot, width: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let rest = width - crate::gui::layout::rail_width();
        let tiny = matches!(self.sidebar_state.mode, crate::gui::sidebar::Mode::Network(protocol::Network::Tiny)) && self.tiny_open();
        let list: AnyElement = if tiny {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .pt(px(tokens::safe_top()))
                .pb(px(tokens::safe_bottom()))
                .bg(theme::sidebar())
                .child(self.tiny_page(false, window, cx))
                .into_any_element()
        } else {
            self.sidebar(snapshot, rest, window, cx).into_any_element()
        };
        div().size_full().flex().bg(theme::sidebar()).child(self.rail(snapshot, window, cx)).child(list)
    }

    /// A pushed page: the status bar's band in the strip's color, then the page.
    fn phone_page(&self, page: impl IntoElement) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .child(div().flex_none().w_full().h(px(tokens::safe_top())).bg(theme::canvas()))
            .child(div().flex_1().min_h_0().child(page))
    }

    /// The right pane as a page of its own: back, its title and count, then the desktop pane's
    /// body, full width.
    fn phone_right_page(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(pane) = self.layout_state.right.as_ref() else {
            return div();
        };
        let strip = div()
            .flex_none()
            .h(px(tokens::NAV))
            .pl(px(4.))
            .pr(px(tokens::GUTTER))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                icon_button("phone-right-back", "icons/chevron-left.svg", false)
                    .size(px(tokens::TOUCH))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.close_right(None, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().min_w_0().truncate().phone_title().child(pane.title.clone()))
                    .children(pane.count.map(|count| div().flex_none().phone_preview().text_color(theme::text_faint()).child(count.to_string()))),
            );
        let body = (pane.body)(self, window, cx);
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.phone_drag_back(f32::from(event.position.x), true, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.phone_release_edge()))
            .child(div().flex_none().w_full().h(px(tokens::safe_top())))
            .child(strip)
            .child(div().flex_1().min_h_0().child(body))
            .child(div().flex_none().h(px(tokens::safe_bottom())))
            .child(self.phone_edge(cx))
    }

    /// Back on a phone, one step: the switcher, a menu, the right pane's page, the conversation,
    /// then up out of a Space or a drilled list. False at the home's root, where the
    /// platform's own back leaves the app.
    pub(in crate::gui) fn phone_back(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) -> bool {
        if !active() {
            return false;
        }
        if self.sidebar_state.switcher.is_some() {
            if let Some(window) = window {
                self.close_switcher(window, cx);
                return true;
            }
        }
        if self.layout_state.menu.is_some() {
            self.close_menu(cx);
            return true;
        }
        if self.close_slack_menu(cx) {
            return true;
        }
        if self.layout_state.right.is_some() && !self.pane_tucked() {
            self.close_right(None, cx);
            return true;
        }
        if self.open.is_some() || self.snapshot.as_ref().is_some_and(|snapshot| snapshot.open.is_some()) {
            self.open = None;
            if let Some(snapshot) = &mut self.snapshot {
                snapshot.open = None;
            }
            self.phone_state.press_cancel();
            cx.notify();
            return true;
        }
        if self.sidebar_state.drill.is_some() || matches!(self.sidebar_state.mode, crate::gui::sidebar::Mode::Space(_)) {
            self.up_level(cx);
            return true;
        }
        false
    }

    /// A row's press on a phone: it acts on release within the slop, and a long press opens
    /// `menu`, when the row has one, where the finger is.
    pub(in crate::gui) fn phone_press(&mut self, token: u64, at: (f32, f32), menu: Option<crate::gui::layout::Menu>, cx: &mut Context<Self>) {
        let count = self.phone_state.press_down(token, at);
        let Some(menu) = menu else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LONG_PRESS).await;
            let _ = this.update(cx, |this, cx| {
                if this.phone_state.press_held(count) {
                    this.open_menu(menu, cx);
                }
            });
        })
        .detach();
    }

    /// True when a drag that began in the left 20 pt has gone far enough to be a back swipe.
    pub(in crate::gui) fn phone_drag_back(&mut self, x: f32, target: bool, cx: &mut Context<Self>) -> bool {
        if !target || !active() {
            return false;
        }
        let Some(start) = self.phone_state.edge_start() else {
            return false;
        };
        let far = x - start > 60.;
        if far {
            self.phone_state.disarm_edge();
            return self.phone_back(None, cx);
        }
        false
    }

    /// The 20 pt strip the back gesture owns, over a pushed page's left edge.
    pub(in crate::gui) fn phone_edge(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        // Not `occlude()`: the strip arms the back gesture on a press at its edge, and the presses
        // it does not use (the composer's first control sits just past it) still reach what is
        // under them. Only the drag that began here is a back swipe.
        div()
            .id("phone-edge")
            .absolute()
            .top(px(tokens::safe_top() + tokens::NAV))
            .bottom(px(tokens::safe_bottom()))
            .left_0()
            .w(px(20.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.phone_state.arm_edge(f32::from(event.position.x));
                    cx.stop_propagation();
                }),
            )
    }

    pub(in crate::gui) fn phone_release_edge(&mut self) {
        self.phone_state.disarm_edge();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_flips_below_the_phone_width_and_back_only_past_the_band() {
        let edge = crate::gui::layout::PHONE;
        assert!(form(false, edge - 1.));
        assert!(!form(false, edge));
        // Once a phone, it stays one through the band above the edge ...
        assert!(form(true, edge));
        assert!(form(true, edge + 39.));
        // ... and is a desktop again only past it.
        assert!(!form(true, edge + 40.));
    }

    #[test]
    fn a_tap_is_a_release_on_the_same_row_within_the_slop() {
        let mut state = State::default();
        let row = token(&["inbox", "a"]);
        state.press_down(row, (10., 10.));
        assert!(state.press_up(row, (14., 12.)));
        // Released on another row: nothing.
        state.press_down(row, (10., 10.));
        assert!(!state.press_up(token(&["inbox", "b"]), (10., 10.)));
        // Dragged past the slop first: a scroll, nothing.
        state.press_down(row, (10., 10.));
        state.press_move((10., 60.));
        assert!(!state.press_up(row, (10., 10.)));
    }

    #[test]
    fn a_long_press_spends_the_press_so_its_release_does_nothing() {
        let mut state = State::default();
        let row = token(&["inbox", "a"]);
        let first = state.press_down(row, (0., 0.));
        assert!(state.press_held(first));
        assert!(!state.press_up(row, (0., 0.)));
        // A timer from an earlier press finds a newer one down and leaves it.
        let older = state.press_down(row, (0., 0.));
        state.press_up(row, (0., 0.));
        let newer = state.press_down(row, (0., 0.));
        assert!(!state.press_held(older));
        assert!(state.press_up(row, (0., 0.)));
        let _ = newer;
    }

    #[test]
    fn tokens_tell_rows_apart() {
        assert_eq!(token(&["inbox", "a"]), token(&["inbox", "a"]));
        assert_ne!(token(&["inbox", "a"]), token(&["inbox", "b"]));
        assert_ne!(token(&["ab", "c"]), token(&["a", "bc"]));
    }
}
