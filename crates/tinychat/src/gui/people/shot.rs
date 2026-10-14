//! States for `--shot` that no flag reaches: `TINYCHAT_S6=people,profile,...` puts made-up
//! people on screen so the People views can be looked at beside the mockups. Compiled only
//! with `gui-shot`. Staging only draws: nothing here, and nothing clicked in a shot run, is
//! sent to a network.
//!
//! The states: `people`, `people-discord`, `people-online`, `people-pending` (or
//! `people-requests`), `people-search`, `people-empty`, `members`, `own`, `own-status` (the own
//! menu with an account open and its status field), `profile`, `profile-dnd`, `profile-body`
//! (the card with what `Profile` answers), `rowmenu`, `cardmenu`, `confirm`, `confirm-block`,
//! `sheet`, and `open0` / `open1`.

use super::card::Float;
use super::list::{Chip, PeopleList};
use super::members::MemberList;
use super::presence::{self, Ring, Shape};
use super::status::StatusMenu;
use crate::gui::components::Avatar;
use crate::gui::composer::Composer;
use crate::gui::layout::Menu;
use crate::gui::sidebar::Mode;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{deferred, div, point, prelude::*, px, AnyElement, Context, Entity, Rgba};
use protocol::{ConversationKind, Key, Link, Network, Occupant, Occupants, Person, Presence, Profile, RelateAction, Relation};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn wants(what: &str) -> bool {
    std::env::var("TINYCHAT_S6").is_ok_and(|states| states.split(',').any(|state| state == what))
}

fn once(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

fn person(network: Network, name: &str, relation: Relation, presence: Presence, status: &str) -> Person {
    Person {
        link: String::new(),
        key: Key { network, account: "demo".into(), id: name.to_lowercase().replace(' ', "-") },
        name: name.to_owned(),
        handle: name.to_lowercase().replace(' ', "."),
        avatar: None,
        bot: name.ends_with("bot"),
        relation,
        presence,
        status: status.to_owned(),
    }
}

/// Twenty-five friends with every presence, a few Slack contacts, requests, and directory hits.
pub fn cast() -> Vec<Person> {
    use Presence::{Dnd, Idle, Offline, Online, Unknown};
    let friends: [(&str, Presence, &str); 25] = [
        ("Ines Calloway", Online, "shipping the tile renderer"),
        ("Kofi Mensah-Boyd", Online, ""),
        ("Lior Ashby", Dnd, "heads down until 4"),
        ("Mara Okafor", Online, ""),
        ("Noor Haddad", Idle, ""),
        ("Petra Lindqvist", Online, "back from Lisbon"),
        ("Tobias Renn", Idle, ""),
        ("Bram Oyelaran", Offline, ""),
        ("Devon Reyes", Offline, "Out of office until Monday"),
        ("Esme Varga", Offline, ""),
        ("Yuki Tanabe", Dnd, "Presenting"),
        ("Ada Brooks", Offline, ""),
        ("Caleb Stone", Offline, ""),
        ("Dalia Haim", Offline, ""),
        ("Felix Moreau", Online, "pairing"),
        ("Greta Olsen", Offline, ""),
        ("Hugo Baptiste", Offline, ""),
        ("Iris Nakamura", Idle, "lunch"),
        ("Jonas Weber", Offline, ""),
        ("Kira Volkova", Offline, ""),
        ("Leo Santoro", Offline, ""),
        ("Mina Park", Online, ""),
        ("Nils Holm", Offline, ""),
        ("Omar Rahim", Offline, ""),
        ("Pia Lund", Offline, ""),
    ];
    let mut cast: Vec<Person> = friends
        .into_iter()
        .map(|(name, presence, status)| person(Network::Discord, name, Relation::Friend, presence, status))
        .collect();
    cast.extend([
        person(Network::Slack, "Dev Patel", Relation::Contact, Online, ""),
        person(Network::Slack, "Mara Okafor", Relation::Contact, Online, "In a huddle"),
        person(Network::Slack, "deploybot", Relation::Contact, Unknown, ""),
        person(Network::Discord, "Rafaela Quist", Relation::Incoming, Unknown, ""),
        person(Network::Discord, "Otis Brandvold", Relation::Incoming, Unknown, ""),
        person(Network::Discord, "Hana Whitcombe", Relation::Outgoing, Unknown, ""),
        person(Network::Discord, "Vex Tolland", Relation::Blocked, Unknown, ""),
        person(Network::Slack, "Marcus Feld", Relation::None, Unknown, "Staff Engineer, Payments"),
        person(Network::Slack, "Marguerite Sol", Relation::None, Unknown, "Design Lead"),
        person(Network::Slack, "Mario Benedek", Relation::None, Unknown, "Support"),
    ]);
    cast
}

fn named(name: &str) -> Key {
    cast().into_iter().find(|person| person.name == name).map(|person| person.key).expect("in the cast")
}

/// The composer's states: what is typed, who is typing, why it was refused.
pub fn stage_composer(composer: &Entity<Composer>, cx: &mut Context<Shell>) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if !once(&DONE) {
        return;
    }
    let lines = |count: usize| {
        let all = [
            "Rollout plan for tonight:",
            "- canary at 22:00, **hold 20 minutes**",
            "- then 25%, 50%, 100% if error rate stays under `0.1%`",
            "- _Dev_ owns the rollback switch, ~~not~~ me",
            "> nobody deploys on Friday",
        ];
        (0..count).map(|ix| if ix < all.len() { all[ix].to_owned() } else { format!("line {}", ix + 1) }).collect::<Vec<_>>().join("\n")
    };
    let text = if wants("lines5") {
        Some(lines(5))
    } else if wants("lines12") {
        Some(lines(12))
    } else if wants("line1") {
        Some("Back in ten".to_owned())
    } else {
        None
    };
    let typists = if wants("typing1") {
        Some(vec!["Ines"])
    } else if wants("typing2") {
        Some(vec!["Ines", "Tomasz"])
    } else if wants("typing3") {
        Some(vec!["Ines", "Tomasz", "Odile"])
    } else {
        None
    };
    let typists = typists.map(|names| names.into_iter().map(str::to_owned).collect());
    composer.update(cx, |composer, cx| composer.stage(text, typists, wants("refused"), cx));
}

/// The link and kind the composer is drawn for, when a state asks for another.
pub fn locked(link: Link, kind: ConversationKind) -> (Link, ConversationKind) {
    if wants("signedout") {
        (Link::SignedOut, kind)
    } else if wants("expired") {
        (Link::Expired, kind)
    } else if wants("forum") {
        (link, ConversationKind::Forum)
    } else {
        (link, kind)
    }
}

pub fn stage_people(list: &Entity<PeopleList>, cx: &mut Context<Shell>) {
    static DONE: AtomicBool = AtomicBool::new(false);
    let any = ["people", "people-discord", "people-online", "people-pending", "people-requests", "people-search", "people-empty"];
    let Some(state) = any.into_iter().find(|state| wants(state)) else {
        return;
    };
    if !once(&DONE) {
        return;
    }
    let (scope, chip, needle) = match state {
        "people-discord" => (Some(Network::Discord), Chip::All, ""),
        "people-online" => (Some(Network::Discord), Chip::Online, ""),
        "people-pending" | "people-requests" => (Some(Network::Discord), Chip::Pending, ""),
        "people-search" => (None, Chip::All, "mar"),
        "people-empty" => (Some(Network::Slack), Chip::All, ""),
        _ => (None, Chip::All, ""),
    };
    let people = if state == "people-empty" { Vec::new() } else { cast() };
    list.update(cx, |list, cx| list.stage(people, scope, chip, needle, cx));
}

pub fn stage_status(menu: &Entity<StatusMenu>, cx: &mut Context<Shell>) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if wants("own-status") && once(&DONE) {
        menu.update(cx, |menu, cx| menu.stage("heads down until 4", cx));
    }
}

pub fn stage_members(members: &Entity<MemberList>, open: Option<Key>, cx: &mut Context<Shell>) {
    static DONE: AtomicBool = AtomicBool::new(false);
    let Some(open) = open.filter(|_| wants("members")) else {
        return;
    };
    if !once(&DONE) {
        return;
    }
    let cast = cast();
    let friends: Vec<Person> = cast.iter().filter(|p| p.relation == Relation::Friend).cloned().collect();
    let around = |on: bool| -> Vec<Person> {
        friends.iter().filter(|p| (p.presence != Presence::Offline) == on).cloned().collect()
    };
    let sections = vec![("Maintainers", around(true)[..3].to_vec()), ("Online", around(true)[3..].to_vec()), ("Offline", around(false))];
    members.update(cx, |members, cx| members.stage(open, sections, cx));
}

impl Shell {
    /// What needs the shell itself changed: the sidebar's mode, a pane, a popover.
    pub(super) fn stage(&mut self, cx: &mut Context<Self>) {
        static DONE: AtomicBool = AtomicBool::new(false);
        if std::env::var_os("TINYCHAT_S6").is_none() || !once(&DONE) {
            return;
        }
        let hub = self.people_state.hub();
        hub.learn(&cast());
        // Kofi is in a voice channel, so his corner is the call badge wherever he is drawn.
        let kofi = Occupant { person: named("Kofi Mensah-Boyd"), name: "Kofi".into(), muted: false, deafened: false, streaming: false };
        let voice = Key { network: Network::Discord, account: "demo".into(), id: "voice".into() };
        hub.set_occupants(&voice, &Occupants { people: vec![kofi], total: 1 });
        if wants("open0") {
            self.open_listed(0, cx);
        }
        if wants("open1") {
            self.open_listed(1, cx);
        }
        if std::env::var("TINYCHAT_S6").is_ok_and(|states| states.split(',').any(|state| state.starts_with("people"))) {
            self.go(Mode::People, cx);
        }
        if wants("members") {
            self.toggle_members(cx);
        }
        if wants("own") || wants("own-status") {
            self.open_menu(Menu::Own, cx);
        }
        if wants("profile-body") {
            let ines = named("Ines Calloway");
            self.open_profile(ines.clone(), point(px(120.), px(120.)), cx);
            let person = Person { link: "https://discord.com/users/1".into(), ..hub.person(&ines).expect("in the cast") };
            let profile = Profile {
                mutual_friends: 0,
                since: None,
                accent: None,
                banner: None,
                fields: Vec::new(),
                person,
                bio: "Rendering and text layout. Previously maps, before that a letterpress shop in Porto.".into(),
                mutual_spaces: ["Riverbend", "Moss & Lichen", "Tile Makers", "Letterpress", "Night Buses", "Porto Tech", "gpui"]
                    .map(str::to_owned)
                    .to_vec(),
                time_zone: "Europe/Lisbon".into(),
                utc_offset: Some(3600),
            };
            let floating = self.floating_entity(cx);
            floating.update(cx, |floating, cx| floating.stage_profile(profile, cx));
        }
        if wants("cardmenu") {
            let floating = self.floating_entity(cx);
            floating.update(cx, |floating, cx| floating.open(Float::More(named("Lior Ashby")), point(px(300.), px(250.)), cx));
        }
        for (state, action) in [("confirm", RelateAction::Remove), ("confirm-block", RelateAction::Block)] {
            if wants(state) {
                let float = Float::Confirm(named("Lior Ashby"), action);
                let floating = self.floating_entity(cx);
                floating.update(cx, |floating, cx| floating.open(float, point(px(300.), px(250.)), cx));
            }
        }
        if wants("profile") {
            self.open_profile(named("Ines Calloway"), point(px(120.), px(170.)), cx);
        }
        if wants("profile-dnd") {
            self.open_profile(named("Lior Ashby"), point(px(520.), px(120.)), cx);
        }
        if wants("rowmenu") {
            self.open_row_menu(named("Lior Ashby"), point(px(300.), px(250.)), cx);
        }
        cx.notify();
    }

    pub(in crate::gui) fn stage_composer(&self, composer: &Entity<Composer>, cx: &mut Context<Self>) {
        stage_composer(composer, cx);
    }
}

/// Every presence shape at every avatar size on every surface it sits on, drawn over the
/// window, for looking at beside the mockup's sheet.
pub fn sheet() -> Option<AnyElement> {
    if !wants("sheet") {
        return None;
    }
    let surfaces: [(&str, Rgba, Option<Rgba>); 5] = [
        ("Row at rest", theme::sidebar(), None),
        ("Row hovered", theme::sidebar(), Some(theme::over(theme::hover(), theme::sidebar()))),
        ("Row selected", theme::sidebar(), Some(theme::over(theme::selected(), theme::sidebar()))),
        ("Conversation", theme::canvas(), None),
        ("Popover", theme::overlay(), None),
    ];
    let shapes = [
        ("Online", Some(Shape::Online)),
        ("Idle", Some(Shape::Idle)),
        ("Do not disturb", Some(Shape::Dnd)),
        ("Offline", Some(Shape::Offline)),
        ("In a call", Some(Shape::InCall)),
        ("Unknown", None),
    ];
    let key = |name: &str| Key { network: Network::Discord, account: "demo".into(), id: name.to_owned() };
    let mut sheet = div()
        .w(px(1120.))
        .h(px(720.))
        .bg(theme::sidebar())
        .p(px(24.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .type_small()
        .text_color(theme::text_soft())
        .child(
            div().flex().pl(px(140.)).children(shapes.iter().map(|(word, _)| div().w(px(160.)).child(*word))),
        );
    for (label, surface, fill) in surfaces {
        let behind = fill.unwrap_or(surface);
        let ring = Ring::on(behind);
        sheet = sheet.child(
            div()
                .h(px(72.))
                .flex()
                .items_center()
                .bg(surface)
                .child(div().w(px(140.)).child(label))
                .children(shapes.iter().map(|(_, shape)| {
                    div()
                        .w(px(160.))
                        .h(px(64.))
                        .px(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(behind)
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .children([40., 32., 24., 20.].into_iter().map(|size| {
                            let avatar = Avatar::new(&key(&format!("{label}{size}")), "Ines Calloway", size).build();
                            avatar.children(shape.and_then(|shape| presence::dot(shape, size, &ring)))
                        }))
                }).collect::<Vec<_>>()),
        );
    }
    let card = Ring::on(theme::overlay());
    sheet = sheet.child(div().h(px(104.)).px(px(140.)).bg(theme::overlay()).flex().items_center().gap(px(72.)).children(
        shapes.iter().map(|(_, shape)| {
            Avatar::new(&key("card"), "Ines Calloway", 80.).build().children(shape.and_then(|shape| presence::dot(shape, 80., &card)))
        }),
    ));
    let big = |shape| presence::inline(shape, 40., theme::sidebar());
    sheet = sheet.child(
        div()
            .mt(px(8.))
            .flex()
            .items_center()
            .gap(px(40.))
            .child("Enlarged 4x")
            .children([Shape::Online, Shape::Idle, Shape::Dnd, Shape::Offline].map(big)),
    );
    Some(deferred(gpui::anchored().position(point(px(0.), px(0.))).child(sheet)).with_priority(3).into_any_element())
}
