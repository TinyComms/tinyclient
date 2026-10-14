//! The rail: one button per place. Inbox and People, one per signed-in network, the Spaces the
//! user pinned, then add and their own avatar. A button says what waits inside it and nothing
//! about its connection: trouble shows as a banner in the pane concerned.

use super::components::{badge_dot, divider, glyph, network_icon, tip, Avatar, Shape};
use super::layout::{drags_window, Menu, RAIL_WIDTH, TOP_BAR};
use super::people::presence;
use super::sidebar::Mode;
use super::state::{Shell, Snapshot};
use super::theme;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, ElementId, SharedString, Stateful, Window};
use protocol::{Link, Network};

/// What a rail button's tooltip says after its name.
fn waiting(attention: u32, unread: bool) -> String {
    match (attention, unread) {
        (0, false) => String::new(),
        (0, true) => " · Unread".to_owned(),
        (1, _) => " · 1 waiting".to_owned(),
        (count, _) => format!(" · {count} waiting"),
    }
}

/// One button of the rail.
struct Place {
    mode: Mode,
    content: AnyElement,
    /// The number of things that need me inside, and whether anything else is unread.
    waiting: (u32, bool),
    /// A mention of me is inside: the dot (or the Inbox's number) is accent.
    mention: bool,
    /// Only the Inbox carries a number (rule 5); every other place a dot.
    counted: bool,
    /// The account is in trouble: drawn faint, the pane says why.
    faint: bool,
    name: SharedString,
    /// Its position, which is its `cmd-digit`.
    index: usize,
}

impl Shell {
    /// The places `cmd-1` to `cmd-9` reach, in the rail's order.
    pub(super) fn places(&self) -> Vec<Mode> {
        let networks = self.snapshot.iter().flat_map(|snapshot| snapshot.signed_in()).map(Mode::Network);
        let pins = self.sidebar_state.pins.iter().filter(|pin| self.spaces.iter().any(|s| &s.key == *pin));
        [Mode::Inbox, Mode::People].into_iter().chain(networks).chain(pins.cloned().map(Mode::Space)).collect()
    }

    fn rail_button(&self, id: impl Into<ElementId>, place: Place, cx: &mut Context<Self>) -> Stateful<Div> {
        let Place { mode, content, waiting: (attention, unread), mention, counted, faint, name, index: place } = place;
        let lit = self.sidebar_state.mode == mode;
        const KEYS: [&str; 9] = ["⌘1", "⌘2", "⌘3", "⌘4", "⌘5", "⌘6", "⌘7", "⌘8", "⌘9"];
        div()
            .id(id)
            .relative()
            .size(px(44.))
            .flex_none()
            .rounded(px(theme::R_FLOAT))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(lit, |button| button.bg(theme::selected()))
            .when(!lit, |button| button.hover(|style| style.bg(theme::hover())))
            .tooltip(tip(format!("{name}{}", waiting(attention, unread)), KEYS.get(place).copied()))
            .on_click(cx.listener(move |this, _, window, cx| this.visit(mode.clone(), window, cx)))
            .child(div().when(faint, |content| content.opacity(0.4)).child(content))
            .children(rail_badge(attention, unread, mention, counted))
    }

    /// A rail click: the place opens in the sidebar. In a window too narrow to keep the
    /// sidebar in the row, it floats over the conversation.
    pub(super) fn visit(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.layout_state;
        let width = f32::from(window.viewport_size().width);
        // A rail click always brings a hidden sidebar back.
        state.sidebar_hidden = false;
        if super::layout::plan(width, state.sidebar_width, false, None).sidebar.is_none() {
            state.floating = !(state.floating && self.sidebar_state.mode == mode);
        }
        self.go(mode, cx);
        cx.notify();
    }

    pub(super) fn rail(&self, snapshot: &Snapshot, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mode = &self.sidebar_state.mode;
        let tint = |lit: bool| if lit { theme::text() } else { theme::text_soft() };
        let rule = || div().w(px(28.)).flex_none().child(divider());
        let mut place = 0;
        let mut next = || {
            place += 1;
            place - 1
        };

        let inbox = Place {
            mode: Mode::Inbox,
            content: glyph("icons/inbox.svg", 20., tint(*mode == Mode::Inbox)).into_any_element(),
            waiting: (snapshot.attention(None), snapshot.unread(None)),
            mention: self.sidebar_state.counts.mentions > 0,
            counted: true,
            faint: false,
            name: "Inbox".into(),
            index: next(),
        };
        let inbox = self.rail_button("rail-inbox", inbox, cx);
        let people = Place {
            mode: Mode::People,
            content: glyph("icons/users.svg", 20., tint(*mode == Mode::People)).into_any_element(),
            waiting: (0, self.sidebar_state.friend_requests > 0),
            mention: false,
            counted: false,
            faint: false,
            name: "People".into(),
            index: next(),
        };
        let people = self.rail_button("rail-people", people, cx);

        let mut places = div().flex().flex_col().items_center().gap(px(8.)).child(rule());
        for network in snapshot.signed_in() {
            let link = snapshot.link(network);
            let workspaces = snapshot.accounts_of(network);
            let name = match (link, workspaces) {
                (Link::Connected, 0 | 1) => network.label().to_owned(),
                (Link::Connected, n) => format!("{} · {n} {}", network.label(), if network == Network::Discord || network == Network::Telegram { "accounts" } else { "workspaces" }),
                (other, _) => format!("{} · {}", network.label(), super::components::link_word(other)),
            };
            let trouble = snapshot.accounts.iter().any(|a| a.key.network == network && (a.link == Link::Expired || !a.required.is_empty()));
            let mentioned = self.sidebar_state.inbox.all().iter().any(|c| c.key.network == network && c.mentions > 0 && !c.muted);
            let place = Place {
                mode: Mode::Network(network),
                content: network_icon(network, 24.).into_any_element(),
                waiting: (snapshot.attention(Some(network)), snapshot.unread(Some(network))),
                mention: mentioned,
                counted: false,
                faint: trouble,
                name: name.into(),
                index: next(),
            };
            places = places.child(self.rail_button(("rail-network", network as usize), place, cx));
        }
        // CONTRACT: the window's prefs (slice A's) keep pinned Spaces only, so a pinned Telegram
        // folder (`04` §4, P2) has nowhere to be remembered yet.
        let pins: Vec<_> = self
            .sidebar_state
            .pins
            .iter()
            .filter_map(|pin| self.spaces.iter().find(|space| &space.key == pin))
            .collect();
        if !pins.is_empty() {
            places = places.child(rule());
        }
        for (ix, space) in pins.into_iter().enumerate() {
            let icon = self.media_path(&space.key, space.icon.as_ref());
            let avatar = Avatar::new(&space.key, &space.name, 36.).shape(Shape::Place).image_at(icon).build();
            let place = Place {
                mode: Mode::Space(space.key.clone()),
                content: avatar.into_any_element(),
                waiting: (space.attention, space.unread),
                mention: space.attention > 0,
                counted: false,
                faint: false,
                name: space.name.clone().into(),
                index: next(),
            };
            places = places.child(self.rail_button(("rail-pin", ix), place, cx));
        }

        // My picture: the first signed-in account that has one, fetched if it is not here yet.
        let me = snapshot.accounts.iter().find(|account| account.avatar.is_some()).or(snapshot.accounts.first());
        let mine = match me {
            Some(account) => {
                let key = protocol::Key::new(&account.key, account.me.clone());
                let name = self.people_state.hub().person(&key).map_or_else(|| account.name.clone(), |person| person.name);
                Avatar::new(&key, &name, 32.).image_at(self.person_picture(&key, cx).or_else(|| self.own_avatar(&account.key)))
            }
            None => Avatar::named("", 32.),
        };
        let mine = mine.build().children(
            presence::Shape::of(self.own_presence()).and_then(|shape| presence::dot(shape, 32., &presence::Ring::on(theme::rail()))),
        );
        let chrome = |id: &'static str, menu: Menu, words: &'static str| {
            let lit = matches!((&self.layout_state.menu, &menu), (Some(Menu::Add), Menu::Add) | (Some(Menu::Own), Menu::Own));
            div()
                .id(id)
                .size(px(44.))
                .flex_none()
                .rounded(px(theme::R_FLOAT))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(lit, |button| button.bg(theme::selected()))
                .hover(|style| style.bg(theme::hover()))
                .tooltip(tip(words, None))
                .on_click(cx.listener(move |this, _, _, cx| this.open_menu(menu.clone(), cx)))
        };

        div()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .bg(theme::rail())
            .border_r_1()
            .border_color(theme::hairline())
            .child(drags_window(div().h(px(TOP_BAR)).w_full().flex_none()))
            .child(div().flex().flex_col().items_center().gap(px(8.)).child(inbox).child(people))
            // The networks and pins scroll together; what is above and below never moves.
            .child(div().id("rail-places").mt(px(8.)).flex_1().min_h_0().w_full().overflow_y_scroll().child(places))
            .child(
                div()
                    .flex_none()
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .child(chrome("rail-add", Menu::Add, "Add an account").size(px(32.)).child(glyph("icons/plus.svg", 16., theme::text_faint())))
                    .child(chrome("rail-me", Menu::Own, "You").child(mine)),
            )
    }
}

/// The badge on a rail button: the Inbox's one number (accent when a mention is inside, else
/// outlined), a dot everywhere else, ringed in the rail's color.
fn rail_badge(attention: u32, unread: bool, mention: bool, counted: bool) -> Option<Div> {
    let ring = |badge: Div| badge.border_2().border_color(theme::rail());
    if counted && attention > 0 {
        let words = if attention > 99 { "99+".to_owned() } else { attention.to_string() };
        let pill = div()
            .h(px(20.))
            .min_w(px(20.))
            .px(px(5.))
            .rounded(px(10.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.))
            .font_weight(theme::SEMIBOLD);
        let pill = if mention {
            pill.bg(theme::mention()).text_color(theme::on_accent()).border_2().border_color(theme::rail())
        } else {
            pill.bg(theme::rail()).text_color(theme::text()).border_1().border_color(theme::border_strong())
        };
        return Some(div().absolute().top(px(-4.)).right(px(-6.)).child(pill.child(words)));
    }
    if attention == 0 && !unread {
        return None;
    }
    let color = if mention || (!counted && attention > 0) { theme::mention() } else { theme::unread() };
    Some(div().absolute().top(px(-2.)).right(px(-2.)).rounded(px(6.)).child(ring(div().rounded(px(6.))).bg(theme::rail()).child(badge_dot(8., color).size(px(8.)))))
}
