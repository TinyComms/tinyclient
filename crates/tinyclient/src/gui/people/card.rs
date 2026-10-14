//! What floats over the window for people: the profile card (the pane's top third, 300 wide),
//! the "…" menus of a person and a conversation, and the one confirm before a block, an
//! unfriending or a leave. One at a time; Escape or a click outside closes it.

use super::info::profile::draw;
use super::info::profile::facts::{self, Act, Call, CardBody, Item, Seen, Surface};
use super::presence::Ring;
use super::{existing_direct, in_call, only_draws, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{
    button, button_with_icon, fade_in, menu, menu_item, menu_item_danger, menu_separator, modal_body,
    modal_buttons, network_icon, scrim, Avatar, ButtonKind, Shape as Plate,
};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    anchored, deferred, div, point, prelude::*, px, ClipboardItem, Context, Div, FocusHandle, KeyDownEvent, Pixels,
    Point, WeakEntity, Window,
};
use protocol::{Conversation, Key, Link, Notify, Person, Profile, RelateAction, Request, Response};

const CARD_WIDTH: f32 = 300.;
const CONFIRM_WIDTH: f32 = 360.;

#[derive(Clone, PartialEq)]
pub enum Float {
    Card(Key),
    /// A row's More menu: Profile, then the person's menu.
    Menu(Key),
    /// The profile pane's "…".
    Person { person: Key, seen: Seen, alt: bool },
    /// Details' "…".
    Conversation { conversation: Conversation, link: Option<String>, alt: bool },
    /// Asked once before a friend is removed or someone is blocked.
    Confirm(Key, RelateAction),
    /// Asked once before leaving a group.
    Leave(Conversation),
}

impl Float {
    fn person(&self) -> Option<&Key> {
        match self {
            Float::Card(person) | Float::Menu(person) | Float::Person { person, .. } | Float::Confirm(person, _) => Some(person),
            Float::Conversation { .. } | Float::Leave(_) => None,
        }
    }
}

/// Their wall clock now, and how far it is from ours: ("12:48 PM", "3 h behind you"). Offsets
/// are seconds east of UTC.
pub fn local_time_parts(now: i64, theirs: i32, ours: i32) -> (String, String) {
    let seconds = (now + i64::from(theirs)).rem_euclid(86_400);
    let (hour, minute) = (seconds / 3600, seconds % 3600 / 60);
    let (hour, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    let apart = (i64::from(theirs) - i64::from(ours)) / 60;
    let far = match (apart.abs() / 60, apart.abs() % 60) {
        (hours, 0) => format!("{hours} h"),
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    let where_ = match apart.signum() {
        0 => "same time as you".to_owned(),
        1 => format!("{far} ahead of you"),
        _ => format!("{far} behind you"),
    };
    (format!("{hour}:{minute:02} {half}"), where_)
}

/// This machine's offset from UTC now, in seconds.
pub(super) fn own_offset(now: i64) -> i32 {
    let seconds = now as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` / `localtime_s` write a full `tm` into the buffer we own.
    unsafe {
        #[cfg(unix)]
        libc::localtime_r(&seconds, &mut tm);
        #[cfg(windows)]
        libc::localtime_s(&mut tm, &seconds);
    }
    #[cfg(unix)]
    {
        tm.tm_gmtoff as i32
    }
    #[cfg(windows)]
    {
        utc_offset_seconds(seconds)
    }
}

#[cfg(windows)]
fn utc_offset_seconds(seconds: libc::time_t) -> i32 {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    // The libc crate does not expose these on Windows. They are in the CRT.
    extern "C" {
        fn mktime(tm: *mut libc::tm) -> libc::time_t;
        fn _mkgmtime(tm: *mut libc::tm) -> libc::time_t;
    }
    // SAFETY: `gmtime_s`, `localtime_s`, `mktime`, and `_mkgmtime` fill or read the buffers we own.
    unsafe {
        libc::localtime_s(&mut local, &seconds);
        libc::gmtime_s(&mut utc, &seconds);
        local.tm_isdst = -1;
        let local_sec = mktime(&mut local);
        let utc_sec = _mkgmtime(&mut utc);
        (local_sec - utc_sec) as i32
    }
}

pub struct Floating {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    open: Option<(Float, Point<Pixels>)>,
    /// Counts openings, so each one fades in.
    opened: usize,
    focus: Option<FocusHandle>,
    /// What the network said of the person whose card is up.
    profile: Option<Profile>,
    profile_asked: u64,
    /// Read from the shell once the card is up: the Space it was opened from, and their DM.
    place: Option<(Key, String)>,
    direct: Option<Conversation>,
}

impl Floating {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub) -> Floating {
        Floating {
            shell,
            hub,
            scene: Scene::default(),
            open: None,
            opened: 0,
            focus: None,
            profile: None,
            profile_asked: 0,
            place: None,
            direct: None,
        }
    }

    pub fn see(&mut self, scene: Scene, _: &mut Context<Self>) {
        self.scene = scene;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&mut self, float: Float, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(person) = float.person() {
            // Their presence is drawn, so it is watched for as long as this is up.
            self.hub.watch("profile", [person.clone()], cx);
        }
        if let Float::Card(person) | Float::Menu(person) = &float {
            self.learn_place(person.clone(), matches!(float, Float::Card(_)), cx);
        }
        self.open = Some((float, anchor));
        self.opened += 1;
        cx.notify();
    }

    /// What the shell holds about where they are seen, read once the shell's own update is
    /// over (cards open from inside it); then, for a card, the one profile request.
    fn learn_place(&mut self, person: Key, card: bool, cx: &mut Context<Self>) {
        self.profile_asked += 1;
        if self.profile.as_ref().is_some_and(|profile| profile.person.key != person) {
            self.profile = None;
        }
        self.place = None;
        self.direct = None;
        let (asked, shell, this) = (self.profile_asked, self.shell.clone(), cx.weak_entity());
        cx.spawn(async move |_, cx| {
            let held = shell.read_with(cx, |shell, _| {
                let open = shell.held_rows().next().cloned();
                (super::info::space_of(open.as_ref(), &person), existing_direct(shell.held_rows(), &person).cloned())
            });
            let Ok((place, direct)) = held else {
                return;
            };
            let space = place.as_ref().map(|(key, _)| key.clone());
            let weak = this.clone();
            let _ = this.update(cx, |this, cx| {
                if this.profile_asked != asked {
                    return;
                }
                this.place = place;
                this.direct = direct;
                cx.notify();
                if !card {
                    return;
                }
                super::ask_later(cx, Request::Profile { person, space }, move |answer, cx| {
                    let Ok(Response::Profile { profile }) = answer else {
                        return;
                    };
                    let _ = weak.update(cx, |this, cx| {
                        if this.profile_asked == asked {
                            this.hub.learn(std::slice::from_ref(&profile.person));
                            this.profile = Some(*profile);
                            cx.notify();
                        }
                    });
                });
            });
        })
        .detach();
    }

    /// Swaps what floats for something else in the same place.
    fn turn_to(&mut self, float: Float, cx: &mut Context<Self>) {
        let anchor = self.open.as_ref().map(|(_, anchor)| *anchor).unwrap_or_default();
        self.open = Some((float, anchor));
        self.opened += 1;
        cx.notify();
    }

    /// The shell is told after this update: a float may be closed from inside the shell's own.
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            self.hub.watch("profile", [], cx);
            let shell = self.shell.clone();
            cx.defer(move |cx| {
                let _ = shell.update(cx, |_, cx| cx.notify());
            });
            cx.notify();
        }
    }

    fn shell_do(&mut self, cx: &mut Context<Self>, then: impl FnOnce(&mut Shell, &mut Context<Shell>) + 'static) {
        self.close(cx);
        let shell = self.shell.clone();
        cx.defer(move |cx| {
            let _ = shell.update(cx, then);
        });
    }

    fn online(&self, person: &Key) -> bool {
        self.scene.links.iter().find(|(net, _)| *net == person.network).is_none_or(|(_, link)| *link == Link::Connected)
    }

    fn seen(&self, person: &Key) -> Seen {
        Seen {
            in_their_dm: self.scene.peer.as_ref() == Some(person),
            direct: self.direct.as_ref().map(|row| (row.call, row.muted)),
            online: self.online(person),
        }
    }

    /// The card: the pane's top third at the same width. It only ever grows downward.
    fn card(&self, person: &Person, window: &Window, cx: &mut Context<Self>) -> Div {
        let network = person.key.network;
        let profile = self.profile.as_ref().filter(|profile| profile.person.key == person.key);
        let seen = self.seen(&person.key);
        let calling = in_call(&self.hub, &self.scene, &person.key);
        let presence = if seen.online { shown_presence(&self.hub, person) } else { protocol::Presence::Unknown };
        let status = match self.hub.status_of(&person.key) {
            status if status.is_empty() => person.status.clone(),
            status => status,
        };
        let line = draw::presence_line_for(person, profile, presence, &status, calling);
        let actions = facts::actions(person, &seen, Surface::Card);
        let place = self.place.as_ref().map(|(_, name)| name.as_str());
        let ring = Ring::on(theme::overlay());
        let head = div()
            .relative()
            .h(px(16. + draw::AVATAR))
            .children(profile.and_then(|profile| profile.accent).map(|color| {
                div().absolute().top_0().left_0().right_0().h(px(draw::BAND)).bg(draw::band(color, theme::overlay()))
            }))
            .child(div().absolute().top(px(16.)).left(px(16.)).child(person_avatar(&self.hub, person, draw::AVATAR, &ring, calling, cx)));
        let mut top = div()
            .px(px(16.))
            .flex()
            .flex_col()
            .child(draw::name_line(person.name.clone(), draw::mark_of(person)).mt(px(12.)))
            .children(draw::soft_line(facts::handle_line(person, profile)))
            .children(draw::presence_row(&line, theme::overlay(), 2))
            .when(actions.message || actions.request || actions.call.is_some(), |top| {
                top.child(self.card_actions(person, &actions, cx).mt(px(16.)))
            });
        if let Some(heads_up) = facts::heads_up(person, profile, seen.online) {
            let words = facts::heads_up_words(&heads_up, &person.name, network, crate::view::now());
            top = top.child(draw::heads_up_line(&words).mt(px(12.)));
        }
        let mut body = div().px(px(16.)).flex().flex_col().gap(px(10.));
        let mut any = false;
        if let Some(profile) = profile.filter(|_| seen.online) {
            match facts::card_body(person, profile, place) {
                Some(CardBody::Role(role)) => {
                    any = true;
                    let spaces = self.spaces(cx);
                    let icon = self.place.as_ref().map(|(key, name)| {
                        let file = spaces.iter().find(|space| space.key == *key).and_then(|space| space.icon.clone());
                        let file = file.map(|file| self.hub.file(&key.account(), &file, cx));
                        Avatar::new(key, name, 16.).shape(Plate::Place).image(file.as_ref()).build()
                    });
                    body = body.child(draw::role(&role, icon, Some(3)));
                }
                Some(CardBody::About(bio)) => {
                    any = true;
                    body = body.child(div().type_ui().line_clamp(2).child(bio));
                }
                None => {}
            }
            if let Some(common) = facts::in_common(network, profile) {
                any = true;
                let spaces = self.spaces(cx);
                let account = person.key.account();
                let icons = common.names.iter().map(|name| {
                    let known = spaces.iter().find(|space| space.key.account() == account && space.name == *name);
                    let key = known.map(|space| space.key.clone()).unwrap_or_else(|| Key { id: name.clone(), ..person.key.clone() });
                    let icon = known.and_then(|space| space.icon.as_ref()).map(|icon| self.hub.file(&account, icon, cx));
                    Avatar::new(&key, name, 20.).shape(Plate::Place).image(icon.as_ref()).build()
                });
                let icons: Vec<Div> = icons.collect();
                body = body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .children(icons)
                        .child(div().ml(px(4.)).type_small().text_color(theme::text_soft()).child(common.words)),
                );
            }
        }
        let max = f32::from(window.viewport_size().height) - 16.;
        let (key, space, given) = (person.key.clone(), self.place.as_ref().map(|(key, _)| key.clone()), profile.cloned());
        let foot = div()
            .h(px(40.))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(theme::hairline())
            // CONTRACT: `panel-right` is not in the asset list; `panel-left` stands in.
            .child(button_with_icon("card-dock", ButtonKind::Ghost, "icons/panel-left.svg", "View profile").on_click(cx.listener(
                move |this, _, _, cx| {
                    let (key, space, given) = (key.clone(), space.clone(), given.clone());
                    this.shell_do(cx, move |shell, cx| shell.details_person_with(key, space, given, cx));
                },
            )))
            .child(div().pr(px(8.)).child(network_icon(network, 16.)));
        div()
            .w(px(CARD_WIDTH))
            .max_h(px(max))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_popover())
            .overflow_hidden()
            .font_family(theme::FONT)
            .type_ui()
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(head)
            .child(top)
            .when(any, |card| card.child(div().id("card-body").min_h_0().flex_shrink(1.).overflow_y_scroll().pt(px(12.)).child(body)))
            .child(div().h(px(12.)))
            .child(foot)
    }

    fn spaces(&self, cx: &mut Context<Self>) -> Vec<protocol::Space> {
        // Drawn outside the shell's update, so reading it here is safe.
        self.shell.upgrade().map(|shell| shell.read(cx).spaces.clone()).unwrap_or_default()
    }

    fn card_actions(&self, person: &Person, actions: &facts::Actions, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().gap(px(8.));
        if actions.request {
            let (accept, ignore) = (person.clone(), person.clone());
            row = row
                .child(button("card-accept", ButtonKind::Primary, "Accept").flex_1().on_click(cx.listener(move |this, _, _, cx| {
                    let person = accept.clone();
                    this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::Accept, cx));
                })))
                .child(button("card-ignore", ButtonKind::Secondary, "Ignore").flex_1().on_click(cx.listener(move |this, _, _, cx| {
                    let person = ignore.clone();
                    this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::Ignore, cx));
                })));
        }
        if actions.message {
            let message = person.clone();
            row = row.child(
                draw::action("card-message", ButtonKind::Primary, "icons/message-square.svg", "Message".into(), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let person = message.clone();
                        this.shell_do(cx, move |shell, cx| shell.open_person(&person, cx));
                    })),
            );
        }
        if let Some(call) = &actions.call {
            let (who, direct) = (person.clone(), self.direct.as_ref().map(|row| row.key.clone()));
            let handoff = matches!(call, Call::Handoff(_));
            let network = person.key.network;
            let icon = if handoff && network == protocol::Network::Slack { "icons/headphones.svg" } else { "icons/phone.svg" };
            let button = draw::action("card-call", ButtonKind::Secondary, icon, facts::call_label(call, network, actions.message), handoff);
            let button = match call {
                Call::Handoff(full) => button.tooltip(crate::gui::components::tip(full.clone(), None)),
                Call::Native => button,
            };
            row = row.child(button.on_click(cx.listener(move |this, _, _, cx| {
                let (who, direct) = (who.clone(), direct.clone());
                this.shell_do(cx, move |shell, cx| match (handoff, direct) {
                    (true, Some(direct)) => shell.call_handoff(direct, cx),
                    // No DM held yet: open it; its header hands the call off.
                    (true, None) => shell.open_person(&who, cx),
                    _ if !only_draws() => shell.call_person(&who, cx),
                    _ => {}
                });
            })));
        }
        row
    }

    /// One menu item, and what it does.
    fn item(&self, item: Item, person: Option<&Person>, conversation: Option<&Conversation>, link: Option<String>, cx: &mut Context<Self>) -> Div {
        let network = person.map(|person| person.key.network).or(conversation.map(|row| row.key.network));
        let open_words = format!("Open in {}", network.map_or("the app", |network| network.label()));
        match item {
            Item::Separator => menu_separator(),
            Item::Mute { muted } => {
                let direct = self.direct.as_ref().map(|row| row.key.clone());
                let (icon, words) = if muted { ("icons/bell.svg", "Unmute") } else { ("icons/bell-off.svg", "Mute") };
                div().child(menu_item("menu-mute", Some(icon), words, None).on_click(cx.listener(move |this, _, _, cx| {
                    this.close(cx);
                    let Some(conversation) = direct.clone().filter(|_| !only_draws()) else {
                        return;
                    };
                    let notify = if muted { Notify::All } else { Notify::Nothing };
                    super::ask_later(cx, Request::SetMute { conversation, notify }, |_, _| {});
                })))
            }
            Item::CopyHandle => {
                let handle = person.map(|person| person.handle.trim_start_matches('@').to_owned()).unwrap_or_default();
                div().child(menu_item("menu-copy", Some("icons/copy.svg"), "Copy username", None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(handle.clone()));
                    this.close(cx);
                })))
            }
            Item::CopyLink => {
                let link = link.unwrap_or_default();
                div().child(menu_item("menu-link", Some("icons/link.svg"), "Copy link", None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                    this.close(cx);
                })))
            }
            Item::CopyId => {
                let id = person.map(|person| person.key.id.clone()).or(conversation.map(|row| row.key.id.clone())).unwrap_or_default();
                div().child(menu_item("menu-id", Some("icons/copy.svg"), "Copy ID", Some("⌥")).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(id.clone()));
                    this.close(cx);
                })))
            }
            Item::OpenIn => {
                let link = link.unwrap_or_default();
                div().child(menu_item("menu-open", Some("icons/external-link.svg"), open_words, None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.open_url(&link);
                    this.close(cx);
                })))
            }
            Item::CloseDirect => {
                let direct = self.direct.as_ref().map(|row| row.key.clone());
                div().child(menu_item("menu-close", Some("icons/x.svg"), "Close conversation", None).on_click(cx.listener(move |this, _, _, cx| {
                    let direct = direct.clone();
                    this.shell_do(cx, move |shell, cx| {
                        if let Some(direct) = direct {
                            shell.close_direct(direct, cx);
                        }
                    });
                })))
            }
            Item::RemoveFriend => {
                let key = person.map(|person| person.key.clone());
                div().child(menu_item_danger("menu-remove", Some("icons/x.svg"), "Remove friend").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(key) = key.clone() {
                        this.turn_to(Float::Confirm(key, RelateAction::Remove), cx);
                    }
                })))
            }
            Item::Block => {
                let key = person.map(|person| person.key.clone());
                div().child(menu_item_danger("menu-block", Some("icons/circle-alert.svg"), "Block").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(key) = key.clone() {
                        this.turn_to(Float::Confirm(key, RelateAction::Block), cx);
                    }
                })))
            }
            Item::Leave => {
                let row = conversation.cloned();
                let words = if row.as_ref().is_some_and(|row| row.kind == protocol::ConversationKind::Group) { "Leave group" } else { "Leave" };
                div().child(menu_item_danger("menu-leave", Some("icons/log-out.svg"), words).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(row) = row.clone() {
                        this.turn_to(Float::Leave(row), cx);
                    }
                })))
            }
        }
    }

    fn person_menu(&self, person: &Person, seen: &Seen, alt: bool, with_profile: bool, cx: &mut Context<Self>) -> Div {
        let items = facts::person_menu(person, seen, alt, !with_profile);
        let link = (!person.link.is_empty()).then(|| person.link.clone());
        let profile = person.key.clone();
        let mut list = menu().w(px(220.));
        if with_profile {
            list = list.child(menu_item("menu-profile", Some("icons/user.svg"), "Profile", None).on_click(cx.listener(
                move |this, _, _, cx| {
                    let key = profile.clone();
                    this.open(Float::Card(key), this.open.as_ref().map(|(_, at)| *at).unwrap_or_default(), cx)
                },
            )));
            if !items.is_empty() {
                list = list.child(menu_separator());
            }
        }
        for item in items {
            list = list.child(self.item(item, Some(person), None, link.clone(), cx));
        }
        list
    }

    fn conversation_menu(&self, row: &Conversation, link: Option<String>, alt: bool, cx: &mut Context<Self>) -> Div {
        let mut list = menu().w(px(220.));
        for item in facts::conversation_menu(row, link.as_deref(), alt) {
            list = list.child(self.item(item, None, Some(row), link.clone(), cx));
        }
        list
    }

    /// The one question before something that cannot be taken back here. Nothing is sent until
    /// its second button.
    fn confirm(&self, act: Act, name: &str, network: protocol::Network, window: &Window, cx: &mut Context<Self>, then: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> Div {
        let (title, words, verb) = facts::confirm_words(act, name, network);
        let size = window.viewport_size();
        let then = std::rc::Rc::new(then);
        let panel = div()
            .w(px(CONFIRM_WIDTH))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(div().type_title().child(title))
            .child(modal_body(words))
            .child(
                modal_buttons()
                    .child(button("confirm-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                    .child(button("confirm-do", ButtonKind::Danger, verb).on_click(cx.listener(move |this, _, _, cx| {
                        let then = then.clone();
                        this.shell_do(cx, move |shell, cx| then(shell, cx));
                    }))),
            );
        div()
            .w(size.width)
            .h(size.height)
            .relative()
            .child(scrim().id("confirm-scrim").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(80.)).child(panel))
    }
}

impl Render for Floating {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((float, anchor)) = self.open.clone() else {
            return div();
        };
        // Someone seen only as a message's sender still floats: the id is all that is known.
        let person = float.person().map(|key| self.hub.person(key).unwrap_or_else(|| Person::new(key.clone(), key.id.clone())));
        let (surface, modal) = match (&float, person.as_ref()) {
            (Float::Card(_), Some(person)) => (self.card(person, window, cx), false),
            (Float::Menu(_), Some(person)) => {
                let seen = self.seen(&person.key);
                (self.person_menu(person, &seen, window.modifiers().alt, true, cx), false)
            }
            (Float::Person { seen, alt, .. }, Some(person)) => (self.person_menu(person, seen, *alt, false, cx), false),
            (Float::Conversation { conversation, link, alt }, _) => (self.conversation_menu(conversation, link.clone(), *alt, cx), false),
            (Float::Confirm(_, action), Some(person)) => {
                let (target, action) = (person.clone(), *action);
                let act = super::info::act_of(action);
                (self.confirm(act, &person.name, person.key.network, window, cx, move |shell, cx| shell.relate(&target, action, cx)), true)
            }
            (Float::Leave(row), _) => {
                let target = row.clone();
                (self.confirm(Act::Leave, &row.title, row.key.network, window, cx, move |shell, cx| shell.leave(&target, cx)), true)
            }
            _ => return div(),
        };
        // Escape goes to whatever has the keys, so the float takes them while it is up.
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        if !focus.is_focused(window) {
            window.focus(&focus, cx);
        }
        let surface = surface.track_focus(&focus).on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                this.close(cx);
            }
        }));
        if modal {
            return div().child(deferred(anchored().position(point(px(0.), px(0.))).child(fade_in(("float", self.opened), 160, surface))).with_priority(3));
        }
        let surface = surface.on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)));
        let offset = if matches!(float, Float::Card(_)) { point(px(8.), px(0.)) } else { point(px(2.), px(2.)) };
        div().child(
            deferred(anchored().position(anchor + offset).snap_to_window_with_margin(px(8.)).child(fade_in(("float", self.opened), 120, surface)))
                .with_priority(2),
        )
    }
}

#[cfg(feature = "gui-shot")]
impl Floating {
    /// A card's answer as a network would give it.
    pub fn stage_profile(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.hub.learn(std::slice::from_ref(&profile.person));
        self.profile_asked += 1;
        self.profile = Some(profile);
        cx.notify();
    }

    /// What View profile does.
    pub fn stage_dock(&mut self, cx: &mut Context<Self>) {
        let Some((Float::Card(key), _)) = self.open.clone() else {
            return;
        };
        let (space, given) = (self.place.as_ref().map(|(key, _)| key.clone()), self.profile.clone());
        self.shell_do(cx, move |shell, cx| shell.details_person_with(key, space, given, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2025-10-13 12:00 UTC.
    const NOON_UTC: i64 = 1_760_356_800;

    fn local_time(now: i64, theirs: i32, ours: i32) -> String {
        let (clock, apart) = local_time_parts(now, theirs, ours);
        format!("{clock} · {apart}")
    }

    #[test]
    fn their_clock_is_read_from_the_offset_and_set_against_ours() {
        assert_eq!(local_time(NOON_UTC + 48 * 60, 3600, 4 * 3600), "1:48 PM · 3 h behind you");
        assert_eq!(local_time(NOON_UTC, 3600, -4 * 3600), "1:00 PM · 5 h ahead of you");
        assert_eq!(local_time(NOON_UTC, -4 * 3600, -4 * 3600), "8:00 AM · same time as you");
        assert_eq!(local_time(NOON_UTC, 19_800, 0), "5:30 PM · 5 h 30 min ahead of you");
        assert_eq!(local_time(NOON_UTC, -12 * 3600 - 1800, -12 * 3600), "11:30 PM · 30 min behind you");
        assert_eq!(local_time(NOON_UTC, 12 * 3600, 0), "12:00 AM · 12 h ahead of you");
    }

    #[test]
    fn only_a_discord_friend_can_be_removed_and_nobody_is_blocked_twice() {
        let person = |network, relation| {
            let mut person = Person::new(Key { network, account: "1".into(), id: "p".into() }, "P");
            person.relation = relation;
            person
        };
        use protocol::{Network, Relation};
        assert_eq!(facts::offers(&person(Network::Discord, Relation::Friend)), (true, true));
        assert_eq!(facts::offers(&person(Network::Discord, Relation::None)), (false, true));
        assert_eq!(facts::offers(&person(Network::Discord, Relation::Blocked)), (false, false));
        assert_eq!(facts::offers(&person(Network::Telegram, Relation::Contact)), (false, true));
        assert_eq!(facts::offers(&person(Network::Slack, Relation::Contact)), (false, false));
    }
}
