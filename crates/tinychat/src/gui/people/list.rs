//! The People list in the sidebar: the lists I own, online first, then A to Z; requests;
//! and search, which filters what is held at once and asks the networks after a pause.

use super::presence::Ring;
use super::hub::Leaves;
use super::{can_call, in_call, person_avatar, shown_presence, status_line, Hub, Scene};
use crate::gui::components::{
    empty_state, glyph, icon_button, network_icon, search_field, segment, segmented, tip,
};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, Div, Entity, Focusable, MouseButton,
    ScrollStrategy, SharedString, Stateful, UniformListScrollHandle, WeakEntity, Window,
};
use protocol::{
    AccountKey, Key, Link, Network, Person, Presence, RelateAction, Relation, Request, Response, PAGE_LIMIT,
    SEARCH_MIN_CHARS,
};
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const SOURCE: &str = "people";
/// Every row is this tall, so the list only builds what is on screen.
const ROW: f32 = theme::ROW_TWO;
const FIRST_HEADER_LIFT: f32 = 20.;
/// The owned lists are held whole, up to this many people.
const HELD: u32 = 2000;
/// A network is asked once the typing has paused this long.
const PAUSE: Duration = Duration::from_millis(250);
const QUERY_ROWS: u32 = 25;
/// People move between Online and Everyone at most this often, so the list does not jump about.
const REGROUP: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Online,
    All,
    Pending,
    Blocked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Header(String),
    Person(Rc<Person>),
    /// One line of words: a search in flight, or one that found nobody.
    Note(String),
}

/// Lowercase, with the accents of Latin letters removed, so "maren" finds "Mårén".
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter_map(|ch| match ch {
            'à'..='å' | 'ā' | 'ă' | 'ą' => Some('a'),
            'ç' | 'ć' | 'č' => Some('c'),
            'è'..='ë' | 'ē' | 'ė' | 'ę' | 'ě' => Some('e'),
            'ì'..='ï' | 'ī' | 'į' | 'ı' => Some('i'),
            'ñ' | 'ń' | 'ň' => Some('n'),
            'ò'..='ö' | 'ø' | 'ō' | 'ő' => Some('o'),
            'ù'..='ü' | 'ū' | 'ů' | 'ű' => Some('u'),
            'ý' | 'ÿ' => Some('y'),
            'š' | 'ś' => Some('s'),
            'ž' | 'ź' | 'ż' => Some('z'),
            'ł' => Some('l'),
            'đ' | 'ď' => Some('d'),
            // Combining marks left by a decomposed spelling.
            '\u{300}'..='\u{36f}' => None,
            other => Some(other),
        })
        .collect()
}

/// What is typed, as it is compared: folded, and without the `@` a handle is typed with.
fn needle_of(typed: &str) -> String {
    fold(typed.trim().trim_start_matches('@'))
}

/// Anywhere in the name or the handle, not only at the start of one.
fn matches(person: &Person, needle: &str) -> bool {
    fold(&person.name).contains(needle) || fold(&person.handle).contains(needle)
}

/// A network said it finished searching for `query`. True when that was the search on screen,
/// and the account is no longer waited on.
pub fn search_ended(waiting: &mut Vec<AccountKey>, asked: Option<&str>, account: &AccountKey, query: &str) -> bool {
    if asked.map(str::trim) != Some(query.trim()) {
        return false;
    }
    let before = waiting.len();
    waiting.retain(|waited| waited != account);
    waiting.len() != before
}

/// The network agreed to an action: the person's row becomes what they now are, where it is.
pub fn apply_related(owned: &mut Vec<Rc<Person>>, blocked: &mut Vec<Rc<Person>>, person: &Key, action: RelateAction) {
    let Some(at) = owned.iter().position(|held| &held.key == person) else {
        return;
    };
    let now = Person { relation: action.leaves(owned[at].relation), ..(*owned[at]).clone() };
    match now.relation {
        Relation::None => {
            owned.remove(at);
        }
        Relation::Blocked => {
            owned.remove(at);
            blocked.push(Rc::new(now));
        }
        _ => owned[at] = Rc::new(now),
    }
}

/// Line two of a request's row: which way it went, and their handle.
fn request_line(person: &Person) -> String {
    let words = if person.relation == Relation::Incoming { "Wants to be friends" } else { "Request sent" };
    match person.handle.trim_start_matches('@') {
        "" => words.to_owned(),
        handle => format!("{words} · @{handle}"),
    }
}

fn by_name(a: &Rc<Person>, b: &Rc<Person>) -> std::cmp::Ordering {
    fold(&a.name).cmp(&fold(&b.name)).then_with(|| a.key.id.cmp(&b.key.id))
}

fn is_around(presence: Presence) -> bool {
    !matches!(presence, Presence::Offline | Presence::Unknown)
}

/// The chips a scope offers: requests and blocking exist where friends do.
pub fn chips(scope: Option<Network>, pending: usize) -> Vec<Chip> {
    match scope {
        Some(Network::Discord) => vec![Chip::Online, Chip::All, Chip::Pending, Chip::Blocked],
        None if pending > 0 => vec![Chip::Online, Chip::All, Chip::Pending],
        _ => vec![Chip::Online, Chip::All],
    }
}

/// The rows of the list for one chip, from the people held. `presence` is what is known of each
/// person now. Online means anything but offline and unknown.
pub fn sections(owned: &[Rc<Person>], chip: Chip, presence: &dyn Fn(&Person) -> Presence) -> Vec<Item> {
    let mut items = Vec::new();
    let mut section = |title: &str, mut people: Vec<Rc<Person>>| {
        if people.is_empty() {
            return;
        }
        people.sort_by(by_name);
        items.push(Item::Header(format!("{title} — {}", people.len())));
        items.extend(people.into_iter().map(Item::Person));
    };
    let of = |wanted: &[Relation]| -> Vec<Rc<Person>> {
        owned.iter().filter(|person| wanted.contains(&person.relation)).cloned().collect()
    };
    match chip {
        Chip::Online | Chip::All => {
            let (around, rest): (Vec<_>, Vec<_>) = of(&[Relation::Friend, Relation::Contact])
                .into_iter()
                .partition(|person| is_around(presence(person)));
            section("Online", around);
            if chip == Chip::All {
                section("Everyone", rest);
            }
        }
        Chip::Pending => {
            section("Incoming", of(&[Relation::Incoming]));
            section("Outgoing", of(&[Relation::Outgoing]));
        }
        Chip::Blocked => section("Blocked", of(&[Relation::Blocked])),
    }
    items
}

/// The rows while searching: what is held and matches, then each network's own answer under
/// its name, then a line for each network still being asked.
pub fn search_sections(
    owned: &[Rc<Person>],
    found: &[Rc<Person>],
    needle: &str,
    waiting: &[Network],
    asked: bool,
) -> Vec<Item> {
    let (typed, needle) = (needle.trim(), needle_of(needle));
    let mut items = Vec::new();
    let mut mine: Vec<Rc<Person>> = owned
        .iter()
        .filter(|person| person.relation != Relation::Blocked && matches(person, &needle))
        .cloned()
        .collect();
    mine.sort_by(by_name);
    let held: HashSet<_> = mine.iter().map(|person| person.key.clone()).collect();
    if !mine.is_empty() {
        items.push(Item::Header(format!("In your lists — {}", mine.len())));
        items.extend(mine.into_iter().map(Item::Person));
    }
    for network in Network::ALL {
        let mut theirs: Vec<Rc<Person>> = found
            .iter()
            .filter(|person| person.key.network == network && !held.contains(&person.key) && matches(person, &needle))
            .cloned()
            .collect();
        theirs.sort_by(by_name);
        let searching = waiting.contains(&network);
        if theirs.is_empty() && !searching {
            continue;
        }
        let label = network.label();
        let title = match network {
            Network::Slack | Network::Teams => format!("{label} directory"),
            Network::Discord | Network::Telegram => label.to_owned(),
        };
        if !theirs.is_empty() {
            items.push(Item::Header(format!("{title} — {}", theirs.len())));
            items.extend(theirs.into_iter().map(Item::Person));
        }
        if searching {
            items.push(Item::Note(format!("Searching {label} for “{typed}”…")));
        }
    }
    if asked && waiting.is_empty() && !items.is_empty() && !items.iter().any(|item| matches!(item, Item::Header(title) if !title.starts_with("In your lists"))) {
        items.push(Item::Note("No one else matches".to_owned()));
    }
    items
}

pub struct PeopleList {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    /// The network the list is narrowed to. `None` mixes them.
    scope: Option<Network>,
    chip: Chip,
    owned: Vec<Rc<Person>>,
    blocked: Vec<Rc<Person>>,
    loaded: bool,
    /// A newer read of the lists makes an older answer stale.
    epoch: u64,
    /// The lists changed while a search was up; they are read again when it is cleared.
    stale: bool,
    search: Entity<TextInput>,
    needle: String,
    /// What the networks were last asked, their answers, and who has yet to answer.
    asked: Option<String>,
    found: Vec<Rc<Person>>,
    waiting: Vec<AccountKey>,
    query_epoch: u64,
    /// People a `Relate` is out for: their buttons do nothing until it is answered.
    relating: HashSet<Key>,
    items: Rc<Vec<Item>>,
    /// The row the arrow keys are on.
    cursor: Option<usize>,
    /// The presence the sections were last built from, and when.
    presence_seen: u64,
    built: Instant,
    regroup_pending: bool,
    refused: Option<String>,
    scroll: UniformListScrollHandle,
}

impl PeopleList {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub, cx: &mut Context<Self>) -> PeopleList {
        let search = cx.new(|cx| TextInput::new("Search people", cx));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Changed => this.typed(cx),
            InputEvent::Submit => this.open_cursor(cx),
        })
        .detach();
        cx.subscribe(&search, |this, _, key: &InputKey, cx| match key {
            InputKey::Up => this.step(false, cx),
            InputKey::Down => this.step(true, cx),
            InputKey::Escape => this.search.update(cx, |search, cx| search.clear(cx)),
        })
        .detach();
        let mut list = PeopleList {
            shell,
            hub,
            scene: Scene::default(),
            scope: None,
            chip: Chip::All,
            owned: Vec::new(),
            blocked: Vec::new(),
            loaded: false,
            epoch: 0,
            stale: false,
            search,
            needle: String::new(),
            asked: None,
            found: Vec::new(),
            waiting: Vec::new(),
            query_epoch: 0,
            relating: HashSet::new(),
            items: Rc::new(Vec::new()),
            cursor: None,
            presence_seen: 0,
            built: Instant::now(),
            regroup_pending: false,
            refused: None,
            scroll: UniformListScrollHandle::new(),
        };
        list.load(cx);
        list
    }

    /// What the shell shows now. Presence moving regroups the list, but not more often than
    /// every two seconds.
    pub fn see(&mut self, scene: Scene, cx: &mut Context<Self>) {
        if self.scene != scene {
            let links_changed = self.scene.links != scene.links;
            let accounts_changed = self.scene.accounts != scene.accounts;
            self.scene = scene;
            // An account that dropped mid-search will not say it finished.
            if accounts_changed && !self.waiting.is_empty() {
                let before = self.waiting.len();
                let accounts = &self.scene.accounts;
                self.waiting.retain(|waited| accounts.iter().any(|own| &own.key == waited && own.link == Link::Connected));
                if self.waiting.len() != before {
                    self.rebuild();
                }
            }
            // A network that just signed in or out has another list.
            if links_changed {
                if self.scope.is_some_and(|scope| !self.networks().contains(&scope)) {
                    self.scope = None;
                }
                self.load(cx);
            }
        }
        if self.presence_seen == self.hub.presence_rev() || self.regroup_pending {
            return;
        }
        let wait = REGROUP.saturating_sub(self.built.elapsed());
        if wait.is_zero() {
            self.rebuild();
            return;
        }
        self.regroup_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.regroup_pending = false;
                this.rebuild();
                cx.notify();
            });
        })
        .detach();
    }

    fn networks(&self) -> Vec<Network> {
        self.scene.links.iter().filter(|(_, link)| *link != Link::SignedOut).map(|(network, _)| *network).collect()
    }

    fn in_scope(&self, network: Network) -> bool {
        self.scope.is_none_or(|scope| scope == network)
    }

    fn pending(&self) -> usize {
        self.owned.iter().filter(|person| matches!(person.relation, Relation::Incoming | Relation::Outgoing)).count()
    }

    fn build(&self) -> Vec<Item> {
        let hub = &self.hub;
        if !self.needle.is_empty() {
            let mut waiting: Vec<Network> = self.waiting.iter().map(|account| account.network).collect();
            waiting.dedup();
            return search_sections(&self.owned, &self.found, &self.needle, &waiting, self.asked.is_some());
        }
        let owned = if self.chip == Chip::Blocked { &self.blocked } else { &self.owned };
        sections(owned, self.chip, &|person| shown_presence(hub, person))
    }

    fn rebuild(&mut self) {
        if !chips(self.scope, self.pending()).contains(&self.chip) {
            self.chip = Chip::All;
        }
        self.items = Rc::new(self.build());
        self.presence_seen = self.hub.presence_rev();
        self.built = Instant::now();
        self.cursor = self.cursor.filter(|ix| matches!(self.items.get(*ix), Some(Item::Person(_))));
    }

    /// Reads the owned lists, a page at a time until they are held whole.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.stale = false;
        self.page(0, self.epoch, cx);
        let (epoch, this) = (self.epoch, cx.weak_entity());
        let blocked = Request::People {
            network: self.scope,
            relation: Some(Relation::Blocked),
            query: None,
            offset: 0,
            limit: PAGE_LIMIT,
        };
        super::ask_later(cx, blocked, move |answer, cx| {
            let Ok(Response::People { people, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.epoch == epoch {
                    this.blocked = people.into_iter().map(Rc::new).collect();
                    this.rebuild();
                    cx.notify();
                }
            });
        });
    }

    fn page(&mut self, offset: u32, epoch: u64, cx: &mut Context<Self>) {
        let request = Request::People { network: self.scope, relation: None, query: None, offset, limit: PAGE_LIMIT };
        let this = cx.weak_entity();
        super::ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                let Ok(Response::People { people, total }) = answer else {
                    this.loaded = true;
                    cx.notify();
                    return;
                };
                this.hub.learn(&people);
                if offset == 0 {
                    this.owned.clear();
                }
                let got = people.len() as u32;
                this.owned.extend(people.into_iter().map(Rc::new));
                this.loaded = true;
                this.rebuild();
                cx.notify();
                let held = this.owned.len() as u32;
                if got > 0 && held < total.min(HELD) {
                    this.page(held, epoch, cx);
                }
            });
        });
    }

    /// The owned lists or a search's results changed on this account.
    pub fn people_changed(&mut self, account: &AccountKey, cx: &mut Context<Self>) {
        if !self.in_scope(account.network) {
            return;
        }
        match self.asked.clone() {
            Some(query) => {
                self.stale = true;
                self.query(query, cx);
            }
            None => self.load(cx),
        }
    }

    /// A network finished looking for `query`, whether or not it found anyone: its line of
    /// "Searching…" comes down, and what it found is read.
    pub fn searched(&mut self, account: &AccountKey, query: &str, cx: &mut Context<Self>) {
        if !search_ended(&mut self.waiting, self.asked.as_deref(), account, query) {
            return;
        }
        if let Some(asked) = self.asked.clone() {
            self.query(asked, cx);
        }
        self.rebuild();
        cx.notify();
    }


    /// The network answered a `Relate`. When it agreed, the row changes where it is; the list
    /// is read again when chatd reports the change.
    pub fn related(&mut self, person: &Key, action: RelateAction, agreed: bool, cx: &mut Context<Self>) {
        self.relating.remove(person);
        if agreed {
            apply_related(&mut self.owned, &mut self.blocked, person, action);
            if matches!(action, RelateAction::Block) {
                self.found.retain(|found| &found.key != person);
            }
            self.rebuild();
        }
        cx.notify();
    }

    /// One `Relate` at a time for a person: a second click waits for the first answer.
    fn relate(&mut self, person: &Rc<Person>, action: RelateAction, cx: &mut Context<Self>) {
        if super::only_draws() || !self.relating.insert(person.key.clone()) {
            return;
        }
        self.refused = None;
        let _ = self.shell.update(cx, |shell, cx| shell.relate(person, action, cx));
        cx.notify();
    }

    fn typed(&mut self, cx: &mut Context<Self>) {
        self.needle = self.search.read(cx).text().trim().to_owned();
        self.cursor = None;
        self.refused = None;
        self.query_epoch += 1;
        if needle_of(&self.needle).chars().count() < SEARCH_MIN_CHARS {
            self.asked = None;
            self.found.clear();
            self.waiting.clear();
            if self.needle.is_empty() && self.stale {
                self.load(cx);
            }
        } else {
            let epoch = self.query_epoch;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(PAUSE).await;
                let _ = this.update(cx, |this, cx| {
                    if this.query_epoch == epoch {
                        this.ask_networks(cx);
                    }
                });
            })
            .detach();
        }
        self.rebuild();
        cx.notify();
    }

    /// The typing has paused: chatd asks each connected account in scope once, and each says
    /// when it has finished (`Change::Searched`).
    fn ask_networks(&mut self, cx: &mut Context<Self>) {
        let query = self.needle.clone();
        self.asked = Some(query.clone());
        self.waiting = self
            .scene
            .accounts
            .iter()
            .filter(|own| own.link == Link::Connected && self.in_scope(own.key.network))
            .map(|own| own.key.clone())
            .collect();
        self.query(query, cx);
        self.rebuild();
        cx.notify();
    }

    fn query(&mut self, query: String, cx: &mut Context<Self>) {
        let request = Request::People {
            network: self.scope,
            relation: None,
            query: Some(query),
            offset: 0,
            limit: QUERY_ROWS,
        };
        let (epoch, this) = (self.query_epoch, cx.weak_entity());
        super::ask_later(cx, request, move |answer, cx| {
            let Ok(Response::People { people, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.query_epoch == epoch {
                    this.hub.learn(&people);
                    this.found = people.into_iter().map(Rc::new).collect();
                    this.rebuild();
                    cx.notify();
                }
            });
        });
    }

    fn pick_chip(&mut self, chip: Chip, cx: &mut Context<Self>) {
        self.chip = chip;
        self.cursor = None;
        self.rebuild();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn pick_scope(&mut self, scope: Option<Network>, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        if !chips(scope, self.pending()).contains(&self.chip) {
            self.chip = Chip::All;
        }
        self.owned.clear();
        self.blocked.clear();
        self.found.clear();
        self.loaded = false;
        self.rebuild();
        self.load(cx);
        if self.asked.is_some() {
            self.ask_networks(cx);
        }
        cx.notify();
    }

    /// Down and Up from the search field walk the people, skipping headers.
    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let people = |ix: &usize| matches!(self.items.get(*ix), Some(Item::Person(_)));
        let next = match (self.cursor, down) {
            (None, _) => (0..self.items.len()).find(people),
            (Some(at), true) => (at + 1..self.items.len()).find(people).or(Some(at)),
            (Some(at), false) => (0..at).rev().find(people).or(Some(at)),
        };
        if let Some(ix) = next {
            self.cursor = Some(ix);
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn open_cursor(&mut self, cx: &mut Context<Self>) {
        let first = (0..self.items.len()).find(|ix| matches!(self.items.get(*ix), Some(Item::Person(_))));
        if let Some(Item::Person(person)) = self.cursor.or(first).and_then(|ix| self.items.get(ix)) {
            self.start(person.clone(), false, cx);
        }
    }

    /// A click on a row is Message.
    fn start(&mut self, person: Rc<Person>, call: bool, cx: &mut Context<Self>) {
        self.refused = None;
        let _ = self.shell.update(cx, |shell, cx| {
            if call {
                shell.call_person(&person, cx);
            } else {
                shell.open_person(&person, cx);
            }
        });
        cx.notify();
    }

    pub fn refused(&mut self, refused: Option<String>, cx: &mut Context<Self>) {
        self.refused = refused;
        cx.notify();
    }

    fn rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let items = self.items.clone();
        let visible = items[range.start.min(items.len())..range.end.min(items.len())].iter();
        self.hub.watch(
            SOURCE,
            visible.filter_map(|item| match item {
                Item::Person(person) if !person.bot => Some(person.key.clone()),
                _ => None,
            }),
            cx,
        );
        range
            .filter_map(|ix| Some((ix, items.get(ix)?)))
            .map(|(ix, item)| match item {
                Item::Header(title) => header(title).into_any_element(),
                Item::Note(words) => note(words).into_any_element(),
                Item::Person(person) => self.person_row(ix, person, cx).into_any_element(),
            })
            .collect()
    }

    fn person_row(&self, ix: usize, person: &Rc<Person>, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.scene.peer.as_ref() == Some(&person.key);
        let focused = self.cursor == Some(ix);
        let blocked = person.relation == Relation::Blocked;
        let ring = Ring::row(theme::sidebar(), selected, "person");
        let in_call = !person.bot && in_call(&self.hub, &self.scene, &person.key);
        let second = match person.relation {
            Relation::Incoming | Relation::Outgoing => request_line(person),
            Relation::Blocked => "Blocked".to_owned(),
            _ if in_call => "In a call".to_owned(),
            _ => status_line(&self.hub, person),
        };
        let (profile, shell) = (person.key.clone(), self.shell.clone());
        let avatar = div()
            .flex_none()
            .when(blocked, |avatar| avatar.opacity(0.45))
            .child(person_avatar(&self.hub, person, 32., &ring, in_call, cx))
            // The avatar opens the profile; the rest of the row is Message.
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                cx.stop_propagation();
                let _ = shell.update(cx, |shell, cx| shell.open_profile(profile.clone(), event.position, cx));
            });
        let title = div()
            .h(px(18.))
            .flex()
            .items_center()
            .gap(px(6.))
            .when(self.scope.is_none(), |title| title.child(network_icon(person.key.network, 14.)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .type_ui()
                    .text_color(if blocked { theme::text_faint() } else { theme::text() })
                    .child(person.name.clone()),
            )
            .when(person.bot, |title| title.child(bot_tag()));
        // The actions sit on a patch of the row's own color, which follows the row under the pointer.
        let rest = if selected { theme::over(theme::selected(), theme::sidebar()) } else { theme::sidebar() };
        let hovered = if selected { rest } else { theme::over(theme::hover(), theme::sidebar()) };
        let trailing = match person.relation {
            Relation::Incoming | Relation::Outgoing => self.request_actions(ix, person, cx),
            Relation::Blocked => div(),
            _ => self.hover_actions(ix, person, cx).invisible().group_hover("person", |style| style.visible()),
        };
        let target = person.clone();
        let (menu_of, menu_shell) = (person.key.clone(), self.shell.clone());
        div().id(("person", ix)).w_full().h(px(ROW)).px(px(theme::INSET)).py(px(1.)).child(
            div()
                .id(("person-row", ix))
                .group("person")
                .relative()
                .size_full()
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(10.))
                .cursor_pointer()
                .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
                .when(selected, |row| row.bg(theme::selected()))
                .when(focused, |row| row.shadow(theme::focus_ring()))
                .on_click(cx.listener(move |this, _, _, cx| this.start(target.clone(), false, cx)))
                // A right click anywhere on the row is its More menu, where the pointer is.
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    cx.stop_propagation();
                    let _ = menu_shell.update(cx, |shell, cx| shell.open_row_menu(menu_of.clone(), event.position, cx));
                })
                .child(avatar)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        // A request's buttons are always there, so its words end before them.
                        .pr(px(match person.relation {
                            Relation::Incoming => 64.,
                            Relation::Outgoing => 32.,
                            _ => 0.,
                        }))
                        .child(title)
                        .child(div().truncate().type_small().text_color(theme::text_soft()).child(second)),
                )
                // Over the end of the text, on the row's own color, so nothing reflows.
                .child(
                    trailing
                        .absolute()
                        .right(px(4.))
                        .top(px(9.))
                        .pl(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(rest)
                        .group_hover("person", move |style| style.bg(hovered))
                        .flex()
                        .items_center()
                        .gap(px(2.)),
                ),
        )
    }

    fn hover_actions(&self, ix: usize, person: &Rc<Person>, cx: &mut Context<Self>) -> Div {
        let stop = |button: Stateful<Div>| button.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let (message, call) = (person.clone(), person.clone());
        let (more, shell) = (person.key.clone(), self.shell.clone());
        div()
            .child(
                stop(icon_button(("message", ix), "icons/message-circle.svg", false))
                    .tooltip(tip("Message", None))
                    .on_click(cx.listener(move |this, _, _, cx| this.start(message.clone(), false, cx))),
            )
            .when(can_call(person.key.network) && !person.bot, |actions| {
                actions.child(
                    stop(icon_button(("call", ix), "icons/phone.svg", false))
                        .tooltip(tip("Call", None))
                        .on_click(cx.listener(move |this, _, _, cx| this.start(call.clone(), true, cx))),
                )
            })
            .child(
                icon_button(("more", ix), "icons/ellipsis.svg", false).tooltip(tip("More", None)).on_mouse_down(
                    MouseButton::Left,
                    move |event, _, cx| {
                        cx.stop_propagation();
                        let _ = shell.update(cx, |shell, cx| shell.open_row_menu(more.clone(), event.position, cx));
                    },
                ),
            )
    }

    /// A request's buttons are always shown, not only under the pointer. Each sends one
    /// `Relate`, and only when clicked.
    fn request_actions(&self, ix: usize, person: &Rc<Person>, cx: &mut Context<Self>) -> Div {
        let quiet = |button: Stateful<Div>| button.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let incoming = person.relation == Relation::Incoming;
        let (accept, other) = (person.clone(), person.clone());
        let decline = if incoming { RelateAction::Ignore } else { RelateAction::Cancel };
        div()
            .when(self.relating.contains(&person.key), |actions| actions.opacity(0.45))
            .when(incoming, |actions| {
                actions.child(
                    quiet(icon_button(("accept", ix), "icons/check.svg", false))
                        .bg(theme::positive_soft())
                        .tooltip(tip("Accept", None))
                        .on_click(cx.listener(move |this, _, _, cx| this.relate(&accept, RelateAction::Accept, cx))),
                )
            })
            .child(
                quiet(icon_button(("ignore", ix), "icons/x.svg", false))
                    .tooltip(tip(if incoming { "Ignore" } else { "Cancel request" }, None))
                    .on_click(cx.listener(move |this, _, _, cx| this.relate(&other, decline, cx))),
            )
    }

    fn empty(&self) -> Option<Div> {
        if !self.items.is_empty() || !self.loaded {
            return None;
        }
        let people = self.owned.iter().filter(|p| matches!(p.relation, Relation::Friend | Relation::Contact)).count();
        let (title, detail) = if !self.needle.is_empty() {
            (format!("No one matches “{}”", self.needle), "Checked your lists and the networks you are signed in to.".to_owned())
        } else {
            match (self.chip, self.scope) {
                (Chip::Online, _) if people > 0 => (
                    "Nobody is online".to_owned(),
                    format!("{people} {} in All.", if people == 1 { "person is" } else { "people are" }),
                ),
                (Chip::Pending, _) => ("No pending requests".to_owned(), "Requests you get or send show up here.".to_owned()),
                (Chip::Blocked, _) => ("Nobody is blocked".to_owned(), "People you block in Discord show up here.".to_owned()),
                (_, Some(Network::Discord)) => {
                    ("No friends on Discord yet".to_owned(), "Friends you add in Discord appear here.".to_owned())
                }
                (_, Some(Network::Telegram)) => ("No contacts yet".to_owned(), "Type a @username to find someone.".to_owned()),
                (_, Some(network)) => (
                    "No direct messages yet".to_owned(),
                    format!("Type a name to find anyone in {}.", self.workspace(network)),
                ),
                (_, None) => ("No people yet".to_owned(), "Friends and people you message appear here.".to_owned()),
            }
        };
        Some(empty_state("icons/users.svg", &title, &detail).px(px(24.)))
    }

    /// The workspace's own name when there is exactly one, else the network's.
    fn workspace(&self, network: Network) -> String {
        let mut names = self.scene.accounts.iter().filter(|own| own.key.network == network).map(|own| &own.name);
        match (names.next(), names.next()) {
            (Some(name), None) if !name.is_empty() => name.clone(),
            _ => network.label().to_owned(),
        }
    }

    fn chips_row(&self, cx: &mut Context<Self>) -> Div {
        let pending = self.pending();
        let mut switch = segmented();
        for chip in chips(self.scope, pending) {
            let label: SharedString = match chip {
                Chip::Online => "Online".into(),
                Chip::All => "All".into(),
                Chip::Pending if pending == 0 => "Pending".into(),
                Chip::Pending if self.scope.is_none() => format!("Requests {pending}").into(),
                Chip::Pending => format!("Pending {pending}").into(),
                Chip::Blocked => "Blocked".into(),
            };
            let id: &'static str = match chip {
                Chip::Online => "chip-online",
                Chip::All => "chip-all",
                Chip::Pending => "chip-pending",
                Chip::Blocked => "chip-blocked",
            };
            switch = switch.child(
                segment(id, label, self.chip == chip).on_click(cx.listener(move |this, _, _, cx| this.pick_chip(chip, cx))),
            );
        }
        div().flex_none().px(px(12.)).pb(px(4.)).flex().items_center().child(switch)
    }

    /// The networks the list can be narrowed to, beside the search field. Not drawn with one
    /// network signed in: there is nothing to narrow.
    fn scopes(&self, cx: &mut Context<Self>) -> Option<Div> {
        let networks = self.networks();
        if networks.len() < 2 {
            return None;
        }
        let scope = |id: &'static str, scope: Option<Network>, cx: &mut Context<Self>| {
            let on = self.scope == scope;
            div()
                .id(id)
                .size(px(theme::CONTROL))
                .flex_none()
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(on, |button| button.bg(theme::selected()))
                .when(!on, |button| button.opacity(0.55).hover(|style| style.bg(theme::hover()).opacity(1.)))
                .tooltip(tip(scope.map_or("All networks", Network::label), None))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_scope(scope, cx)))
                .child(match scope {
                    Some(network) => network_icon(network, 16.).into_any_element(),
                    None => glyph("icons/users.svg", 16., theme::text()).into_any_element(),
                })
        };
        let mut scopes = div().flex_none().flex().items_center().gap(px(2.)).child(scope("scope-all", None, cx));
        for network in networks {
            scopes = scopes.child(scope(network.label(), Some(network), cx));
        }
        Some(scopes)
    }

    #[cfg(feature = "gui-shot")]
    pub fn stage(&mut self, people: Vec<Person>, scope: Option<Network>, chip: Chip, needle: &str, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.query_epoch += 1;
        self.hub.learn(&people);
        self.scope = scope;
        self.chip = chip;
        let (found, owned): (Vec<_>, Vec<_>) = people
            .into_iter()
            .filter(|person| scope.is_none_or(|scope| person.key.network == scope))
            .map(Rc::new)
            .partition(|person| person.relation == Relation::None);
        self.blocked = owned.iter().filter(|p| p.relation == Relation::Blocked).cloned().collect();
        self.owned = owned.into_iter().filter(|p| p.relation != Relation::Blocked).collect();
        self.found = found;
        self.loaded = true;
        if !needle.is_empty() {
            self.search.update(cx, |search, cx| search.set_text(needle, cx));
            self.needle = needle.to_owned();
            self.asked = Some(needle.to_owned());
            self.waiting = vec![AccountKey { network: Network::Telegram, account: "demo".into() }];
        }
        self.rebuild();
        cx.notify();
    }
}

fn header(title: &str) -> Div {
    div()
        .w_full()
        .h(px(ROW))
        .px(px(16.))
        .pb(px(6.))
        .flex()
        .items_end()
        .type_micro()
        .text_color(theme::text_faint())
        .child(title.to_owned())
}

/// A line of words in a row's slot, set where a header's would be: it stands for one.
fn note(words: &str) -> Div {
    div()
        .w_full()
        .h(px(ROW))
        .px(px(16.))
        .pb(px(6.))
        .flex()
        .items_end()
        .type_small()
        .text_color(theme::text_faint())
        .child(div().truncate().child(words.to_owned()))
}

pub fn bot_tag() -> Div {
    div()
        .flex_none()
        .h(px(14.))
        .px(px(4.))
        .rounded(px(theme::R_CHIP))
        .bg(theme::hover())
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(theme::MEDIUM)
        .text_color(theme::text_soft())
        .child("Bot")
}

impl Render for PeopleList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.search.focus_handle(cx).is_focused(window);
        let down: Vec<&'static str> = self
            .scene
            .links
            .iter()
            .filter(|(network, link)| *link == Link::Offline && self.in_scope(*network))
            .map(|(network, _)| network.label())
            .collect();
        let offline = match down.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is offline. Presence is paused.")),
            [one, two] => Some(format!("{one} and {two} are offline. Presence is paused.")),
            _ => Some("Some networks are offline. Presence is paused.".to_owned()),
        };
        let clear = (!self.needle.is_empty()).then(|| {
            div()
                .id("search-clear")
                .size(px(20.))
                .flex_none()
                .rounded(px(theme::R_CHIP))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .on_click(cx.listener(|this, _, _, cx| this.search.update(cx, |search, cx| search.clear(cx))))
                .child(glyph("icons/x.svg", 14., theme::text_soft()))
        });
        let line = |words: String, color: gpui::Rgba| {
            div().flex_none().px(px(16.)).pb(px(4.)).type_small().text_color(color).child(words)
        };
        let body = match self.empty() {
            Some(empty) => empty.into_any_element(),
            // A header is a whole row with its words at the bottom; the first one needs no air
            // above it, so the list starts that much higher.
            None => div()
                .size_full()
                .overflow_hidden()
                .child(
                    uniform_list("people", self.items.len(), cx.processor(Self::rows))
                        .track_scroll(&self.scroll)
                        .absolute()
                        .top(px(-FIRST_HEADER_LIFT))
                        .left_0()
                        .right_0()
                        .bottom_0(),
                )
                .relative()
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .pb(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        search_field(focused)
                            .flex_1()
                            .min_w_0()
                            .line_height(px(18.))
                            .child(self.search.clone())
                            .children(clear),
                    )
                    .children(self.scopes(cx)),
            )
            .child(self.chips_row(cx))
            .children(offline.map(|words| line(words, theme::warning())))
            .children(self.refused.clone().map(|words| line(words, theme::danger())))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Key;

    fn person(name: &str, relation: Relation, presence: Presence) -> Rc<Person> {
        Rc::new(Person {
            link: String::new(),
            key: Key { network: Network::Discord, account: "1".into(), id: name.to_lowercase() },
            name: name.to_owned(),
            handle: name.to_lowercase().replace(' ', "."),
            avatar: None,
            bot: false,
            relation,
            presence,
            status: String::new(),
        })
    }

    fn names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Header(title) => format!("[{title}]"),
                Item::Person(person) => person.name.clone(),
                Item::Note(words) => format!("({words})"),
            })
            .collect()
    }

    fn friends() -> Vec<Rc<Person>> {
        vec![
            person("tobias", Relation::Friend, Presence::Idle),
            person("Mara", Relation::Friend, Presence::Offline),
            person("Ines", Relation::Friend, Presence::Online),
            person("esme", Relation::Contact, Presence::Unknown),
            person("Kofi", Relation::Friend, Presence::Dnd),
            person("Rafaela", Relation::Incoming, Presence::Unknown),
            person("Hana", Relation::Outgoing, Presence::Unknown),
        ]
    }

    #[test]
    fn online_comes_first_then_everyone_each_a_to_z() {
        let all = sections(&friends(), Chip::All, &|person| person.presence);
        assert_eq!(names(&all), ["[Online — 3]", "Ines", "Kofi", "tobias", "[Everyone — 2]", "esme", "Mara"]);
        let online = sections(&friends(), Chip::Online, &|person| person.presence);
        assert_eq!(names(&online), ["[Online — 3]", "Ines", "Kofi", "tobias"]);
    }

    #[test]
    fn requests_are_their_own_list_and_never_among_friends() {
        let pending = sections(&friends(), Chip::Pending, &|person| person.presence);
        assert_eq!(names(&pending), ["[Incoming — 1]", "Rafaela", "[Outgoing — 1]", "Hana"]);
        assert!(sections(&friends(), Chip::Blocked, &|person| person.presence).is_empty());
    }

    #[test]
    fn presence_known_now_moves_a_person_between_sections() {
        let all = sections(&friends(), Chip::All, &|person| {
            if person.name == "Mara" { Presence::Online } else { Presence::Offline }
        });
        assert_eq!(names(&all)[..2], ["[Online — 1]", "Mara"]);
    }

    #[test]
    fn chips_follow_what_the_network_has() {
        assert_eq!(chips(Some(Network::Discord), 0), [Chip::Online, Chip::All, Chip::Pending, Chip::Blocked]);
        assert_eq!(chips(Some(Network::Slack), 3), [Chip::Online, Chip::All]);
        assert_eq!(chips(None, 0), [Chip::Online, Chip::All]);
        assert_eq!(chips(None, 2), [Chip::Online, Chip::All, Chip::Pending]);
    }

    #[test]
    fn search_ignores_case_and_accents_and_reads_handles() {
        let mut held = friends();
        held.push(person("Mårten Öberg", Relation::Friend, Presence::Online));
        let found = search_sections(&held, &[], "marten", &[], false);
        assert_eq!(names(&found), ["[In your lists — 1]", "Mårten Öberg"]);
        let by_handle = search_sections(&held, &[], "MÅRTEN.ö", &[], false);
        assert_eq!(names(&by_handle), ["[In your lists — 1]", "Mårten Öberg"]);
    }

    #[test]
    fn what_is_typed_is_found_anywhere_in_a_name_or_handle() {
        let mut held = friends();
        held.push(person("Ines Calloway", Relation::Friend, Presence::Online));
        held.push(person("Žofia Kowalczyk", Relation::Contact, Presence::Unknown));
        let found = |typed: &str| names(&search_sections(&held, &[], typed, &[], false));
        // The middle of a word, the second word, the handle with its `@`, one letter.
        assert_eq!(found("allow"), ["[In your lists — 1]", "Ines Calloway"]);
        assert_eq!(found("  CALLO "), ["[In your lists — 1]", "Ines Calloway"]);
        assert_eq!(found("@ines.c"), ["[In your lists — 1]", "Ines Calloway"]);
        assert_eq!(found("walc"), ["[In your lists — 1]", "Žofia Kowalczyk"]);
        assert_eq!(found("zof"), ["[In your lists — 1]", "Žofia Kowalczyk"]);
        assert_eq!(found("ž"), ["[In your lists — 1]", "Žofia Kowalczyk"]);
        assert!(found("calloway ines").is_empty());
    }

    #[test]
    fn a_network_saying_it_searched_ends_its_line_and_only_for_the_query_on_screen() {
        let account = |network, id: &str| AccountKey { network, account: id.into() };
        let (north, river) = (account(Network::Slack, "T1"), account(Network::Slack, "T2"));
        let mut waiting = vec![north.clone(), river.clone(), account(Network::Telegram, "me")];
        // An answer to what was typed before is not an answer to this.
        assert!(!search_ended(&mut waiting, Some("mar"), &north, "ma"));
        assert!(!search_ended(&mut waiting, None, &north, "mar"));
        assert_eq!(waiting.len(), 3);

        assert!(search_ended(&mut waiting, Some("mar"), &north, "mar"));
        assert!(!search_ended(&mut waiting, Some("mar"), &north, "mar"));
        // Slack still has a workspace looking, so its line stays.
        let lines = |waiting: &[AccountKey]| {
            let networks: Vec<Network> = waiting.iter().map(|account| account.network).collect();
            names(&search_sections(&[], &[], "mar", &networks, true))
        };
        assert_eq!(lines(&waiting), ["(Searching Telegram for “mar”…)", "(Searching Slack for “mar”…)"]);
        assert!(search_ended(&mut waiting, Some("mar"), &river, "mar"));
        assert_eq!(lines(&waiting), ["(Searching Telegram for “mar”…)"]);
        assert!(search_ended(&mut waiting, Some("mar"), &account(Network::Telegram, "me"), "mar"));
        assert!(lines(&waiting).is_empty());
    }

    #[test]
    fn an_answered_request_changes_its_row_in_place() {
        let (mut owned, mut blocked) = (friends(), Vec::new());
        let key = |name: &str| person(name, Relation::None, Presence::Unknown).key.clone();
        apply_related(&mut owned, &mut blocked, &key("Rafaela"), RelateAction::Accept);
        assert_eq!(owned.iter().find(|p| p.name == "Rafaela").map(|p| p.relation), Some(Relation::Friend));
        apply_related(&mut owned, &mut blocked, &key("Hana"), RelateAction::Cancel);
        assert!(!owned.iter().any(|p| p.name == "Hana"));
        apply_related(&mut owned, &mut blocked, &key("Kofi"), RelateAction::Remove);
        assert!(!owned.iter().any(|p| p.name == "Kofi"));
        apply_related(&mut owned, &mut blocked, &key("Mara"), RelateAction::Block);
        assert!(!owned.iter().any(|p| p.name == "Mara"));
        assert_eq!(blocked.iter().map(|p| (p.name.as_str(), p.relation)).collect::<Vec<_>>(), [("Mara", Relation::Blocked)]);
        assert!(sections(&owned, Chip::Pending, &|p| p.presence).is_empty());

        let asks = person("Rafaela Quist", Relation::Incoming, Presence::Unknown);
        assert_eq!(request_line(&asks), "Wants to be friends · @rafaela.quist");
        let sent = Person { handle: String::new(), ..(*person("Hana", Relation::Outgoing, Presence::Unknown)).clone() };
        assert_eq!(request_line(&sent), "Request sent");
    }

    #[test]
    fn a_network_answer_gets_its_own_section_and_a_line_while_it_is_asked() {
        let held = friends();
        let slack = |name: &str| {
            let mut found = (*person(name, Relation::None, Presence::Unknown)).clone();
            found.key.network = Network::Slack;
            Rc::new(found)
        };
        let found = vec![slack("Marcus Feld"), slack("Mario Benedek"), person("Mara", Relation::Friend, Presence::Offline)];
        let rows = search_sections(&held, &found, "mar", &[Network::Telegram], true);
        assert_eq!(
            names(&rows),
            [
                "[In your lists — 1]",
                "Mara",
                "(Searching Telegram for “mar”…)",
                "[Slack directory — 2]",
                "Marcus Feld",
                "Mario Benedek",
            ]
        );
        let done = search_sections(&held, &[], "mar", &[], true);
        assert_eq!(names(&done), ["[In your lists — 1]", "Mara", "(No one else matches)"]);
        assert!(search_sections(&held, &[], "zzz", &[], true).is_empty());
    }
}
