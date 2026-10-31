//! The phone form: one full-screen surface at a time, four tabs at the bottom, no rail and no
//! sidebar column (`research_notes/mobile/design/01-ia-navigation.md` §0). Every screen here is
//! drawn only below `layout::PHONE`; at 600 and wider the window is the desktop it always was.
//!
//! A tab owns its stack. Today the only thing a stack holds is a conversation, so switching
//! tabs with one open is not reachable (the tab bar hides behind the conversation); the shape
//! is here for the thread and settings routes the next wave pushes.

pub mod conversation;
pub mod inbox;
pub mod search;
pub mod tokens;
pub mod you;

use crate::gui::components::{button, glyph, icon_button, scrim, Avatar, ButtonKind};
use crate::gui::input::{InputEvent, TextInput};
use crate::gui::people::presence;
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use crate::gui::phone::tokens::PhoneType;
use gpui::{div, prelude::*, px, Context, Div, Entity, Focusable, MouseButton, SharedString, Window};
use protocol::{AccountKey, Conversation, Key, Link, Network};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// A count with its noun in the word the number asks for: "1 account", "2 accounts", "0 accounts".
pub fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// Whether a window `width` wide is a phone, given whether it was one a frame ago. A phone is
/// under `layout::PHONE`; once a window is a phone it stays one until it is 40 pt wider than that,
/// so a width that hovers at the edge (a split screen being dragged, a fold opening) does not
/// flip the palette, the layout and the tab bar every frame.
pub fn form(was_phone: bool, width: f32) -> bool {
    let edge = crate::gui::layout::PHONE;
    if was_phone {
        width < edge + 40.
    } else {
        width < edge
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Needs,
    Inbox,
    Search,
    You,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Needs, Tab::Inbox, Tab::Search, Tab::You];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Needs => "Needs me",
            Tab::Inbox => "Inbox",
            Tab::Search => "Search",
            Tab::You => "You",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Tab::Needs => "icons/at-sign.svg",
            Tab::Inbox => "icons/inbox.svg",
            Tab::Search => "icons/search.svg",
            Tab::You => "icons/user.svg",
        }
    }
}

/// One pushed screen.
#[derive(Clone, PartialEq, Eq)]
pub enum Route {
    Conversation { key: Key, title: String },
}

pub struct State {
    pub tab: Tab,
    /// One per tab, so a trip to another tab returns to where it was.
    stacks: [Vec<Route>; 4],
    /// The account or network the two lists are filtered to; `None` is all of them.
    pub scope: Option<AccountKey>,
    /// The account switcher is open over the list.
    pub scope_open: bool,
    /// Where a left-edge drag began, for the back gesture.
    edge: Option<f32>,
    /// A press on a row that has not yet been released: it opens the row only if it ends where it
    /// began. A press that travels past the slop is a scroll, and opens nothing.
    press: Option<Press>,
    /// The Search tab's field, made on the first frame (`Shell::phone_prepare`).
    pub search: Option<Entity<TextInput>>,
}

/// A press on a row, and where it began.
#[derive(Clone)]
pub struct Press {
    key: Key,
    at: (f32, f32),
}

impl Default for State {
    fn default() -> Self {
        State { tab: Tab::Needs, stacks: Default::default(), scope: None, scope_open: false, edge: None, press: None, search: None }
    }
}

impl State {
    /// A finger went down on a row.
    pub fn press_down(&mut self, key: Key, at: (f32, f32)) {
        self.press = Some(Press { key, at });
    }

    /// The pointer moved. Past the slop the press is a scroll, and it will not open anything.
    pub fn press_move(&mut self, to: (f32, f32)) {
        if self.press.as_ref().is_some_and(|press| !tokens::within_slop(press.at, to)) {
            self.press = None;
        }
    }

    /// The finger lifted. True when it lifted on the row it went down on, within the slop.
    pub fn press_up(&mut self, key: &Key, to: (f32, f32)) -> bool {
        match self.press.take() {
            Some(press) => &press.key == key && tokens::within_slop(press.at, to),
            None => false,
        }
    }

    pub fn press_cancel(&mut self) {
        self.press = None;
    }

    fn at_mut(&mut self, tab: Tab) -> &mut Vec<Route> {
        &mut self.stacks[tab as usize]
    }

    /// Keeps the current tab's stack honest with what the shell has open, whichever way it was
    /// opened (a row, a notification, `--open`). No stack when nothing is open.
    pub fn sync(&mut self, open: Option<&Conversation>) {
        let tab = self.tab;
        let stack = self.at_mut(tab);
        match open {
            Some(open) => {
                let top = stack.last().map(|route| match route {
                    Route::Conversation { key, .. } => key == &open.key,
                });
                if top != Some(true) {
                    *stack = vec![Route::Conversation { key: open.key.clone(), title: open.title.clone() }];
                }
            }
            None => stack.clear(),
        }
    }

    pub fn push(&mut self, route: Route) {
        let tab = self.tab;
        self.at_mut(tab).push(route);
    }

    pub fn pop(&mut self) -> Option<Route> {
        let tab = self.tab;
        self.at_mut(tab).pop()
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

impl Shell {
    /// The phone, whole: a pushed conversation or the current tab's root, and the tab bar
    /// wherever it is not a conversation.
    /// Makes what the phone holds across frames: the Search tab's field, once. Called before
    /// the screen is drawn, so the entity exists when a tab asks for focus.
    pub(super) fn phone_prepare(&mut self, cx: &mut Context<Self>) {
        if self.phone_state.search.is_some() {
            return;
        }
        let field = cx.new(|cx| TextInput::new("Search conversations", cx));
        cx.subscribe(&field, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        self.phone_state.search = Some(field);
    }

    /// Moves the keyboard to the Search field: on entering the tab, so typing starts at once.
    pub(super) fn phone_focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(field) = &self.phone_state.search {
            let handle = field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// The network a phone's "Sign in" opens: the first one this phone is not signed in to, in
    /// the first-run order, so the first tap does what the first-run list's default does.
    pub(super) fn phone_sign_in_network(snapshot: &Snapshot) -> Network {
        crate::gui::layout::NETWORK_ORDER
            .into_iter()
            .find(|network| crate::gui::layout::AVAILABLE.contains(network) && snapshot.link(*network) == Link::SignedOut)
            .unwrap_or(Network::Discord)
    }

    /// The one sign-in entry a phone button calls: the same `Shell::sign_in` the desktop's
    /// first-run list, its rail's Add menu and its sidebar banners call.
    pub(super) fn phone_sign_in_button(&self, id: &'static str, label: &'static str, snapshot: &Snapshot, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let network = Self::phone_sign_in_network(snapshot);
        button(id, ButtonKind::Primary, label).on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
    }

    /// Called from the window's render after [`Shell::phone_prepare`].
    pub(super) fn phone_screen(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // The status bar's room is part of every phone screen. 44 + 52 is the header band, so a
        // title is never glued to y = 0 and the strip keeps its own 52 below it. A platform's
        // real inset replaces the 44 (`tokens::set_insets`).
        let screen = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .text_color(theme::text())
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .h(px(tokens::safe_top()))
                    .bg(tokens::surface())
                    .shadow(tokens::top_highlight()),
            );
        if snapshot.open.is_some() {
            return screen.child(self.conversation(snapshot, window, cx));
        }
        let body = match self.phone_state.tab {
            Tab::Needs => self.phone_list(snapshot, true, window, cx),
            Tab::Inbox => self.phone_list(snapshot, false, window, cx),
            Tab::Search => self.phone_search(snapshot, window, cx),
            Tab::You => self.phone_you(snapshot, window, cx),
        };
        screen
            .relative()
            .child(div().flex_1().min_h_0().pb(px(tokens::GUTTER)).child(body))
            .child(self.phone_tabs(snapshot, cx))
            .children(self.phone_scope_sheet(snapshot, window, cx))
    }

    /// Back on a phone: pop the tab's stack, else leave the conversation that was opened from
    /// outside it. Answers false at a tab root, where the platform's own back exits the app.
    pub(super) fn phone_back(&mut self, cx: &mut Context<Self>) -> bool {
        if !active() {
            return false;
        }
        let had = self.phone_state.pop().is_some() || self.open.is_some();
        if !had {
            return false;
        }
        self.open = None;
        if let Some(snapshot) = &mut self.snapshot {
            snapshot.open = None;
        }
        cx.notify();
        true
    }

    /// The tab bar: 56 pt of cells plus the home indicator below them. A cell is 8 above a 22 pt
    /// icon, 10 between it and the label (the 2 pt indicator lives in that gap), 14 of label and
    /// 2 under. The icon and its badge never move, whatever the label does.
    fn phone_tabs(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        let needs = inbox::needing(self.sidebar_state.inbox.all());
        let unread = self.sidebar_state.inbox.all().iter().any(|row| row.unread > 0);
        let mentioned = self.sidebar_state.inbox.all().iter().any(|row| row.mentions > 0 && !row.muted);
        let account = snapshot.accounts.iter().find(|account| account.avatar.is_some()).or_else(|| snapshot.accounts.first());
        let cells = Tab::ALL.into_iter().enumerate().map(|(ix, tab)| {
            let on = self.phone_state.tab == tab;
            // Selected #F2F3F5, unselected #8A909B: the one difference besides the bar.
            let color = if on { tokens::text() } else { tokens::text_faint() };
            let badge = match tab {
                Tab::Needs if needs > 0 => Some(self.tab_badge(needs)),
                Tab::Inbox if unread => Some(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .right(px(-5.))
                        .size(px(8.))
                        .rounded(px(4.))
                        .bg(if mentioned { tokens::accent() } else { theme::text_soft() })
                        .shadow(tokens::ring(tokens::surface(), 2.))
                        .into_any_element(),
                ),
                _ => None,
            };
            let icon: gpui::AnyElement = match (tab, account) {
                (Tab::You, Some(account)) => {
                    let key = Key::new(&account.key, account.me.clone());
                    let name = self
                        .people_state
                        .hub()
                        .person(&key)
                        .map_or_else(|| account.name.clone(), |person| person.name);
                    let mine = Avatar::new(&key, &name, 22.)
                        .image_at(self.person_picture(&key, cx).or_else(|| self.own_avatar(&account.key)))
                        .build();
                    mine.children(
                        presence::Shape::of(self.own_presence())
                            .and_then(|shape| presence::dot(shape, 22., &presence::Ring::on(tokens::surface()))),
                    )
                    .into_any_element()
                }
                _ => glyph(tab.glyph(), 22., color).into_any_element(),
            };
            div()
                .id(("phone-tab", ix))
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_start()
                .pt(px(8.))
                .pb(px(2.))
                .cursor_pointer()
                .active(|cell| cell.bg(tokens::pressed()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.phone_state.tab = tab;
                    if tab == Tab::Search {
                        this.phone_focus_search(window, cx);
                    }
                    cx.notify();
                }))
                .child(div().relative().h(px(22.)).w(px(22.)).flex().items_center().justify_center().child(icon).children(badge))
                // The 10 pt between icon and label, with a 2 x 16 bar in it: the one
                // selected-tab cue, and it does not move the label.
                .child(
                    div()
                        .h(px(10.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().h(px(2.)).w(px(16.)).rounded(px(1.)).bg(if on { tokens::accent() } else { gpui::rgba(0x00000000) })),
                )
                // A label may take two lines at a larger run (`tokens::tab_lines`); at 100% it is one.
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .h(px(tokens::tab_lines(tokens::text_scale()) as f32 * tokens::scaled(14.)))
                        .overflow_hidden()
                        .text_align(gpui::TextAlign::Center)
                        .phone_tab()
                        .text_color(color)
                        .child(tab.label()),
                )
        });
        div()
            .flex_none()
            .flex()
            .flex_col()
            .bg(tokens::surface())
            .shadow(tokens::top_highlight())
            .border_t_1()
            .border_color(tokens::hairline())
            .child(div().h(px(tokens::tab_height())).flex().children(cells))
            .child(div().h(px(tokens::safe_bottom())))
    }

    fn tab_badge(&self, count: u32) -> gpui::AnyElement {
        let words = if count > 99 { "99+".to_owned() } else { count.to_string() };
        div()
            .absolute()
            .top(px(-6.))
            .right(px(-12.))
            .h(px(18.))
            .min_w(px(18.))
            .px(px(5.))
            .rounded(px(9.))
            .bg(tokens::accent())
            // A 2 pt ring of chrome, so the badge reads as a thing on the icon, not a smear.
            .shadow(tokens::ring(tokens::surface(), 2.))
            .flex()
            .items_center()
            .justify_center()
            .child(div().phone_tab().text_color(tokens::on_accent()).child(SharedString::from(words)))
            .into_any_element()
    }

    /// The 52 pt strip a list root wears: its title is the account switcher. `needs` says which
    /// list, for the words underneath.
    pub(super) fn phone_nav(&self, words: &'static str, scope: Option<SharedString>, cx: &mut Context<Self>) -> Div {
        let title = scope.clone().unwrap_or_else(|| SharedString::from(words));
        let title_el: gpui::AnyElement = match scope {
            Some(_) => div()
                .id("phone-scope")
                .h(px(tokens::TOUCH))
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .child(div().phone_title().child(title))
                .child(glyph("icons/chevron-down.svg", 14., theme::text_faint()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.phone_state.scope_open = true;
                    cx.notify();
                }))
                .into_any_element(),
            None => div().phone_title().child(title).into_any_element(),
        };
        div()
            .flex_none()
            .h(px(tokens::NAV))
            .pl(px(16.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .bg(tokens::surface())
            .shadow(tokens::top_highlight())
            .border_b_1()
            .border_color(tokens::hairline())
            .child(title_el)
    }

    /// The account switcher, as a sheet over the list: every account, then all of them. 20 pt of
    /// inner padding, 8 between rows, 12 pt top corners, one hairline and a real shadow.
    pub(super) fn phone_scope_sheet(&self, snapshot: &Snapshot, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.phone_state.scope_open {
            return None;
        }
        let row = |id: (&'static str, usize), name: SharedString, key: Option<AccountKey>, on: bool| {
            let pick = key.clone();
            div()
                .id(id)
                .min_h(px(tokens::sheet_row()))
                .px(px(12.))
                .rounded(px(theme::R_CARD))
                .flex()
                .items_center()
                .gap(px(12.))
                .when(on, |row| row.bg(tokens::selected()))
                .active(|row| row.bg(tokens::pressed()))
                .cursor_pointer()
                .child(div().flex_1().min_w_0().truncate().phone_row().font_weight(if on { theme::SEMIBOLD } else { theme::REGULAR }).child(name))
                .when(on, |row| row.child(glyph("icons/check.svg", 16., tokens::accent())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.phone_state.scope = pick.clone();
                    this.phone_state.scope_open = false;
                    cx.notify();
                }))
        };
        let mut rows = vec![row(("scope-all", 0), "All accounts".into(), None, self.phone_state.scope.is_none())];
        for (ix, account) in snapshot.accounts.iter().enumerate() {
            rows.push(row(
                ("scope-account", ix + 1),
                format!("{} · {}", account.key.network.label(), account.name).into(),
                Some(account.key.clone()),
                self.phone_state.scope.as_ref() == Some(&account.key),
            ));
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .flex_col()
                .justify_end()
                .child(
                    scrim().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.phone_state.scope_open = false;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    // Capped at 60% of the window, so the list behind stays in view; the rows scroll
                    // inside it. The title stays put above them.
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .max_h(px(tokens::sheet_cap(f32::from(window.viewport_size().height))))
                        .bg(tokens::raised())
                        .shadow(tokens::shadow_sheet())
                        .rounded_t(px(theme::R_MODAL))
                        .border_t_1()
                        .border_color(tokens::hairline_strong())
                        .px(px(20.))
                        .pt(px(20.))
                        .pb(px(20. + tokens::safe_bottom()))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().flex_none().h(px(44.)).flex().items_center().justify_between().child(div().phone_title().child("Accounts")).child(
                            icon_button("scope-close", "icons/x.svg", false)
                                .size(px(tokens::TOUCH))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.phone_state.scope_open = false;
                                    cx.notify();
                                })),
                        ))
                        .child(
                            div()
                                .id("scope-rows")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .children(rows),
                        ),
                ),
        )
    }

    /// True when a drag that began in the left 20 pt has gone far enough to be a back swipe.
    pub(super) fn phone_drag_back(&mut self, x: f32, target: bool, cx: &mut Context<Self>) -> bool {
        if !target || !active() {
            return false;
        }
        let Some(start) = self.phone_state.edge_start() else {
            return false;
        };
        let far = x - start > 60.;
        if far {
            self.phone_state.disarm_edge();
            return self.phone_back(cx);
        }
        false
    }

    /// The 20 pt strip the back gesture owns, over the conversation's left edge.
    pub(super) fn phone_edge(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        // Not `occlude()`: the strip arms the back gesture on a press at its edge, and the presses
        // it does not use (the composer's first control sits just past it) still reach what is
        // under them. Only the drag that began here is a back swipe.
        div()
            .id("phone-edge")
            .absolute()
            .top(px(tokens::NAV))
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

    pub(super) fn phone_release_edge(&mut self) {
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
    fn counts_take_the_singular_only_for_one() {
        assert_eq!(counted(1, "network", "networks"), "1 network");
        assert_eq!(counted(3, "network", "networks"), "3 networks");
        assert_eq!(counted(0, "network", "networks"), "0 networks");
    }
}
