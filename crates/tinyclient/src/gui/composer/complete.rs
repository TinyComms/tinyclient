//! The completion list over the composer: people after `@`, channels after `#`. Rows come from
//! chatd's store; nothing here asks a network. The list is a value the composer holds while a
//! sigil is being completed and drops when it is not.

use super::mention::{rank, Kind, Trigger};
use crate::gui::components::{menu, Avatar};
use crate::gui::people::presence::{self, Ring, Shape};
use crate::gui::people::Hub;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, App, Div, MouseButton, MouseDownEvent, Window};
use protocol::{AccountKey, Conversation, ConversationKind, Key, Person, Presence, Request};

/// Rows shown. The answer is asked for no more than this.
pub const ROWS: usize = 8;
const WIDTH: f32 = 340.;
const ROW: f32 = 40.;
const FACE: f32 = 26.;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: Kind,
    pub key: Key,
    pub name: String,
    pub handle: String,
    /// The quieter words after the name.
    pub second: String,
    /// The person, for their picture and presence.
    pub person: Option<Person>,
}

impl Row {
    /// What completing the row puts in the field.
    pub fn label(&self) -> String {
        match self.kind {
            Kind::Channel => format!("#{}", self.name),
            Kind::Person | Kind::Role => format!("@{}", self.name),
            Kind::Emoji => self.name.clone(),
        }
    }

    fn of_person(person: Person) -> Row {
        let handle = person.handle.trim_start_matches('@').to_owned();
        let second = if handle.is_empty() { person.status.clone() } else { handle.clone() };
        Row { kind: Kind::Person, key: person.key.clone(), name: person.name.clone(), handle, second, person: Some(person) }
    }
}

pub struct List {
    pub trigger: Trigger,
    pub rows: Vec<Row>,
    pub selected: usize,
}

impl List {
    /// Up and Down, wrapping.
    pub fn step(&mut self, down: bool) {
        let count = self.rows.len();
        if count > 0 {
            self.selected = if down { (self.selected + 1) % count } else { (self.selected + count - 1) % count };
        }
    }

    /// The rows that still answer what is typed now, in the order they were shown: a keystroke
    /// narrows the list in the same frame, and the store's answer replaces it after.
    pub fn narrowed(&self, trigger: &Trigger) -> Vec<Row> {
        if trigger.kind != self.trigger.kind || trigger.start != self.trigger.start {
            return Vec::new();
        }
        self.rows.iter().filter(|row| rank(&row.name, &row.handle, &trigger.query).is_some()).cloned().collect()
    }
}

/// What to ask the store for a trigger. `None`: this place has no such list.
pub fn request(trigger: &Trigger, conversation: &Key, space: Option<&str>) -> Option<Request> {
    let network = Some(conversation.network);
    match trigger.kind {
        Kind::Emoji => None,
        Kind::Person | Kind::Role => Some(Request::People {
            network,
            relation: None,
            query: (!trigger.query.is_empty()).then(|| trigger.query.clone()),
            offset: 0,
            limit: ROWS as u32,
        }),
        // Channels of this Space only, so a direct conversation outside any Space has none.
        Kind::Channel => space.map(|_| Request::Find { network, query: trigger.query.clone(), limit: protocol::PAGE_LIMIT }),
    }
}

/// People for a query, best match first. `known` are people the shell already holds (those
/// typing here); they lead, and nobody is listed twice or from another account.
pub fn people(known: Vec<Person>, found: Vec<Person>, account: &AccountKey, query: &str) -> Vec<Row> {
    let mut rows: Vec<(u8, Row)> = Vec::new();
    for person in known.into_iter().chain(found) {
        let row = Row::of_person(person);
        let Some(score) = rank(&row.name, &row.handle, query) else {
            continue;
        };
        if row.key.account() == *account && !row.name.is_empty() && !rows.iter().any(|(_, seen)| seen.key == row.key) {
            rows.push((score, row));
        }
    }
    // Stable: equal matches keep the order they came in.
    rows.sort_by_key(|(score, _)| *score);
    rows.into_iter().map(|(_, row)| row).take(ROWS).collect()
}

/// The channels of one Space that answer a query: those starting with it, then the rest, each
/// in the store's order (most recent first).
pub fn channels(found: Vec<Conversation>, account: &AccountKey, space: &str, query: &str) -> Vec<Row> {
    let mut rows: Vec<(u8, Row)> = found
        .into_iter()
        .filter(|c| c.key.account() == *account && c.space.as_deref() == Some(space))
        .filter(|c| !c.kind.is_direct() && c.kind != ConversationKind::Thread)
        .filter_map(|c| {
            let name = c.title.trim_start_matches('#').to_owned();
            let score = rank(&name, "", query)?;
            Some((score, Row { kind: Kind::Channel, key: c.key, name, handle: String::new(), second: c.space_title, person: None }))
        })
        .collect();
    rows.sort_by_key(|(score, _)| *score);
    rows.into_iter().map(|(_, row)| row).take(ROWS).collect()
}

/// The popover: 340 wide, 40 pt rows, the selected one filled. `pick` is told the row clicked.
pub fn draw(list: &List, hub: &Hub, pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static, cx: &mut App) -> Div {
    let ring = Ring::on(theme::overlay());
    menu().w(px(WIDTH)).max_w(px(WIDTH)).children(list.rows.iter().enumerate().map(|(ix, row)| {
        let face = match &row.person {
            Some(person) => {
                let picture = hub.avatar(&person.key.account(), person.avatar.as_ref(), cx);
                let shown = match hub.presence_of(&person.key) {
                    Presence::Unknown => person.presence,
                    known => known,
                };
                let shape = Shape::of(shown).filter(|_| !person.bot);
                Avatar::new(&person.key, &person.name, FACE)
                    .image(picture.as_ref())
                    .build()
                    .children(shape.and_then(|shape| presence::dot(shape, FACE, &ring)))
            }
            None => Avatar::channel("icons/hash.svg", FACE).build(),
        };
        let pick = pick.clone();
        div()
            .id(("complete", ix))
            .h(px(ROW))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(ix == list.selected, |row| row.bg(theme::selected()))
            .when(ix != list.selected, |row| row.hover(|style| style.bg(theme::hover())))
            // On the press, and kept from the field: the caret stays where it is.
            .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                window.prevent_default();
                pick(ix, window, cx);
            })
            .child(face)
            .child(div().flex_none().max_w(px(180.)).truncate().type_ui().font_weight(theme::SEMIBOLD).child(row.name.clone()))
            .child(div().flex_1().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(row.second.clone()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Network, Relation};

    fn account() -> AccountKey {
        AccountKey { network: Network::Discord, account: "me".into() }
    }

    fn person(account: &str, name: &str, handle: &str) -> Person {
        Person {
            key: Key { network: Network::Discord, account: account.into(), id: name.to_lowercase() },
            name: name.into(),
            handle: handle.into(),
            avatar: None,
            bot: false,
            relation: Relation::None,
            presence: Presence::Unknown,
            status: String::new(),
            link: String::new(),
            emoji: String::new(), activity: None, title: String::new(), utc_offset: None, mark: None,
        }
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn people_are_ranked_listed_once_and_only_from_this_account() {
        let found = vec![
            person("me", "Ingrid Vandeveer", "iv"),
            person("me", "Nadia Devlin", "ndevlin"),
            person("me", "Dev Patel", "devp"),
            person("other", "Devon Reyes", "devon"),
            person("me", "Mara Okafor", "mara"),
        ];
        let known = vec![person("me", "Nadia Devlin", "ndevlin")];
        let rows = people(known, found.clone(), &account(), "dev");
        assert_eq!(names(&rows), ["Dev Patel", "Nadia Devlin", "Ingrid Vandeveer"]);
        assert_eq!(rows[0].label(), "@Dev Patel");
        assert_eq!(rows[0].second, "devp");
        // With nothing typed, everyone, those already here first, and never more than fit.
        let many: Vec<Person> = (0..20).map(|n| person("me", &format!("Person {n}"), "")).collect();
        assert_eq!(people(Vec::new(), many, &account(), "").len(), ROWS);
        assert_eq!(names(&people(vec![person("me", "Mara Okafor", "mara")], found, &account(), ""))[0], "Mara Okafor");
    }

    fn channel(space: Option<&str>, title: &str, kind: ConversationKind) -> Conversation {
        Conversation {
            key: Key { network: Network::Discord, account: "me".into(), id: title.into() },
            kind,
            title: title.into(),
            space: space.map(str::to_owned),
            space_title: "Northwind".into(),
            group: None,
            group_title: String::new(),
            topic: String::new(),
            parent: None,
            root: None,
            position: 0,
            avatar: None,
            peer: None,
            unread: 0,
            mentions: 0,
            muted: false,
            starred: false,
            preview: String::new(),
            updated: 0,
            last_read: None,
            call: protocol::CallSupport::None,
            occupants: None,
            limit: 0,
            read_only: false,
            private: false,
            draft: String::new(),
            folders: Vec::new(), archived: false, pin: None, hidden: false, followed: false, bot: false, meeting: false, special: Default::default(), mark: None, opted_out: false, quiet: false, preview_from: String::new(), preview_mine: false, seen: false, delivered: false, sending: false,
        }
    }

    #[test]
    fn channels_come_from_the_open_space_only() {
        let found = vec![
            channel(Some("s1"), "data-platform", ConversationKind::Channel),
            channel(Some("s1"), "#platform", ConversationKind::Channel),
            channel(Some("s2"), "platform-eu", ConversationKind::Channel),
            channel(None, "Plato", ConversationKind::Direct),
            channel(Some("s1"), "platform thread", ConversationKind::Thread),
            channel(Some("s1"), "random", ConversationKind::Channel),
        ];
        let rows = channels(found, &account(), "s1", "plat");
        assert_eq!(names(&rows), ["platform", "data-platform"]);
        assert_eq!(rows[0].label(), "#platform");
        assert_eq!(rows[0].second, "Northwind");
    }

    #[test]
    fn a_place_without_a_space_has_no_channel_list() {
        let conversation = Key { network: Network::Discord, account: "me".into(), id: "dm".into() };
        let hash = Trigger { kind: Kind::Channel, start: 0, query: "pla".into() };
        assert_eq!(request(&hash, &conversation, None), None);
        assert!(matches!(request(&hash, &conversation, Some("s1")), Some(Request::Find { .. })));
        let at = Trigger { kind: Kind::Person, start: 0, query: String::new() };
        // Nothing typed yet asks for the owned lists, not for a search.
        assert!(matches!(request(&at, &conversation, None), Some(Request::People { query: None, limit: 8, .. })));
    }

    #[test]
    fn the_selection_wraps_and_a_keystroke_narrows_what_is_shown() {
        let rows = people(Vec::new(), vec![person("me", "Dev Patel", "devp"), person("me", "Devon Reyes", "devon")], &account(), "de");
        let trigger = Trigger { kind: Kind::Person, start: 4, query: "de".into() };
        let mut list = List { trigger: trigger.clone(), rows, selected: 0 };
        list.step(false);
        assert_eq!(list.selected, 1);
        list.step(true);
        assert_eq!(list.selected, 0);
        let longer = Trigger { query: "devo".into(), ..trigger.clone() };
        assert_eq!(names(&list.narrowed(&longer)), ["Devon Reyes"]);
        // Another sigil is another list.
        assert!(list.narrowed(&Trigger { start: 9, ..trigger }).is_empty());
    }
}
