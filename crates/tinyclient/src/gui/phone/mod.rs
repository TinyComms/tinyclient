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

use crate::gui::components::{glyph, icon_button, scrim, Avatar};
use crate::gui::people::presence;
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use crate::gui::phone::tokens::PhoneType;
use gpui::{div, prelude::*, px, Context, Div, MouseButton, SharedString, Window};
use protocol::{AccountKey, Conversation, Key};
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
}

impl Default for State {
    fn default() -> Self {
        State { tab: Tab::Needs, stacks: Default::default(), scope: None, scope_open: false, edge: None }
    }
}

impl State {
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
    pub(super) fn phone_screen(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // The status bar's room is part of every phone screen. 44 + 52 is the header band, so a
        // title is never glued to y = 0 and the strip keeps its own 52 below it.
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
                    .h(px(tokens::SAFE_TOP))
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
            .children(self.phone_scope_sheet(snapshot, cx))
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

    /// The tab bar: 56 pt of cells plus the home indicator below them. Cells are fixed: 7 top,
    /// a 22 pt icon, 3, a 16 x 2 indicator, 3, an 11/14 label, 5 bottom. The icon and its badge
    /// never move, whatever the label does.
    fn phone_tabs(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        let needs = inbox::needing(self.sidebar_state.inbox.all());
        let unread = self.sidebar_state.inbox.all().iter().any(|row| row.unread > 0);
        let mentioned = self.sidebar_state.inbox.all().iter().any(|row| row.mentions > 0 && !row.muted);
        let account = snapshot.accounts.iter().find(|account| account.avatar.is_some()).or_else(|| snapshot.accounts.first());
        let cells = Tab::ALL.into_iter().enumerate().map(|(ix, tab)| {
            let on = self.phone_state.tab == tab;
            let color = if on { theme::text() } else { theme::text_faint() };
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
                .pt(px(7.))
                .pb(px(5.))
                .gap(px(3.))
                .cursor_pointer()
                .active(|cell| cell.bg(tokens::pressed()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.phone_state.tab = tab;
                    cx.notify();
                }))
                .child(div().relative().h(px(22.)).w(px(22.)).flex().items_center().justify_center().child(icon).children(badge))
                // A 2 pt bar above the label: the one selected-tab cue, and it does not move.
                .child(div().h(px(2.)).w(px(16.)).rounded(px(1.)).bg(if on { tokens::accent() } else { gpui::rgba(0x00000000) }))
                .child(div().w_full().min_w_0().truncate().text_align(gpui::TextAlign::Center).phone_tab().text_color(color).child(tab.label()))
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
            .child(div().h(px(tokens::SAFE_BOTTOM)))
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
                .child(glyph("icons/chevron-down.svg", 16., theme::text_faint()))
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
    pub(super) fn phone_scope_sheet(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<Div> {
        if !self.phone_state.scope_open {
            return None;
        }
        let row = |id: (&'static str, usize), name: SharedString, key: Option<AccountKey>, on: bool| {
            let pick = key.clone();
            div()
                .id(id)
                .h(px(56.))
                .px(px(12.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .gap(px(12.))
                .when(on, |row| row.bg(tokens::selected()))
                .active(|row| row.bg(tokens::pressed()))
                .cursor_pointer()
                .child(div().flex_1().min_w_0().truncate().phone_row().font_weight(if on { theme::SEMIBOLD } else { theme::REGULAR }).child(name))
                .when(on, |row| row.child(glyph("icons/check.svg", 18., tokens::accent())))
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
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .bg(tokens::raised())
                        .shadow(tokens::shadow_sheet())
                        .rounded_t(px(12.))
                        .border_t_1()
                        .border_color(tokens::hairline_strong())
                        .px(px(20.))
                        .pt(px(20.))
                        .pb(px(20. + tokens::SAFE_BOTTOM))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().h(px(44.)).flex().items_center().justify_between().child(div().phone_title().child("Accounts")).child(
                            icon_button("scope-close", "icons/x.svg", false)
                                .size(px(tokens::TOUCH))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.phone_state.scope_open = false;
                                    cx.notify();
                                })),
                        ))
                        .children(rows),
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
        div()
            .id("phone-edge")
            .absolute()
            .top(px(tokens::NAV))
            .bottom(px(tokens::SAFE_BOTTOM))
            .left_0()
            .w(px(20.))
            .occlude()
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
