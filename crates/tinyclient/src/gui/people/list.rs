//! The People list in the sidebar (`design/polish/04-unified.md` §3). Across networks it answers
//! "who could I talk to right now": a Requests line, In a call, Online and Recent (`Around`),
//! each A to Z, never bots. Scoped to Discord it is Friends, with its chips. Search filters my
//! people at once and asks the networks after a pause, each answering under its own header.

use super::hub::Leaves;
use super::presence::Ring;
use super::{can_call, in_call, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{badge_count_unread, empty_state, glyph, icon_button, search_field, section_header, segment, segmented, tip};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::sidebar::{on_screen, Mode};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Div, Entity, Focusable, ListAlignment, ListState, MouseButton, SharedString,
    Stateful, WeakEntity, Window,
};
use protocol::{
    AccountKey, ActivityKind, Key, Link, Network, Person, Presence, RelateAction, Relation, Request, Response, PAGE_LIMIT,
    SEARCH_MIN_CHARS,
};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const SOURCE: &str = "people";
/// The owned lists are held whole, up to this many people.
const HELD: u32 = 2000;
/// A network is asked once the typing has paused this long.
const PAUSE: Duration = Duration::from_millis(300);
const QUERY_ROWS: u32 = 25;
/// Each network's answer shows at most this many.
const PER_NETWORK: usize = 8;
/// People move between sections at most this often, so the list does not jump about.
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
    Header { title: String, count: Option<usize>, first: bool },
    Person(Rc<Person>),
    /// One line of words: a network still being asked, nobody around.
    Note(String),
    /// "2 friend requests", which opens Discord's Pending.
    Requests(u32),
    /// "All Discord friends · Telegram contacts": the full lists, in each network's home.
    Links(Vec<Network>),
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Item::Header { first: true, .. } => theme::SECTION_FIRST,
            Item::Header { .. } | Item::Note(_) | Item::Requests(_) => theme::row_height(theme::ROW_ONE),
            Item::Person(_) => theme::row_height(theme::ROW_TWO),
            Item::Links(_) => 56.,
        }
    }

    fn anchor(&self) -> Option<String> {
        match self {
            Item::Person(person) => Some(format!("{:?}/{}/{}", person.key.network, person.key.account, person.key.id)),
            Item::Header { title, .. } => Some(title.clone()),
            _ => None,
        }
    }
}

/// Who is around, as `Around` answers.
#[derive(Default, Clone)]
pub struct Around {
    pub requests: u32,
    pub in_call: Vec<Rc<Person>>,
    pub online: Vec<Rc<Person>>,
    pub recent: Vec<Rc<Person>>,
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

/// A meeting is not a call (`04` §3): someone in a Teams meeting or presenting is listed under
/// Online, their line two saying so; In a call is for voice, calls and huddles.
pub fn meetings_are_online(in_call: Vec<Person>, mut online: Vec<Person>) -> (Vec<Person>, Vec<Person>) {
    let meeting = |person: &Person| person.activity.as_ref().is_some_and(|a| matches!(a.kind, ActivityKind::InMeeting | ActivityKind::Presenting));
    let (moved, calls): (Vec<Person>, Vec<Person>) = in_call.into_iter().partition(meeting);
    online.extend(moved);
    (calls, online)
}

/// Line two of a person's row, first that exists (`04` §3): where they are talking, their own
/// status, what they are doing, their title. Never "Online": the dot says it.
pub fn around_line(person: &Person, status: &str, in_call: bool, title: bool) -> String {
    if let Some(activity) = &person.activity {
        let place = |name: &str| if activity.place.is_empty() { name.to_owned() } else { format!("{name} · {}", activity.place) };
        match activity.kind {
            ActivityKind::InVoice if !activity.name.is_empty() => return place(&activity.name),
            ActivityKind::Huddle => return "In a huddle".into(),
            ActivityKind::InMeeting => return "In a meeting".into(),
            ActivityKind::Presenting => return "Presenting".into(),
            _ => {}
        }
    }
    if in_call {
        return "In a call".into();
    }
    let said = super::presence::status_words(&person.emoji, status);
    if !said.is_empty() {
        return said;
    }
    if let Some(activity) = &person.activity {
        let verb = match activity.kind {
            ActivityKind::Playing => "Playing",
            ActivityKind::Listening => "Listening to",
            ActivityKind::Watching => "Watching",
            ActivityKind::Streaming => "Streaming",
            ActivityKind::Competing => "Competing in",
            _ => "In",
        };
        return format!("{verb} {}", activity.name).trim().to_owned();
    }
    if title && !person.title.is_empty() {
        return person.title.clone();
    }
    String::new()
}

/// The chips a scope offers: requests and blocking exist where friends do.
pub fn chips(scope: Option<Network>, pending: usize) -> Vec<Chip> {
    match scope {
        Some(Network::Discord) => vec![Chip::Online, Chip::All, Chip::Pending, Chip::Blocked],
        None if pending > 0 => vec![Chip::Online, Chip::All, Chip::Pending],
        _ => vec![Chip::Online, Chip::All],
    }
}

fn header(items: &mut Vec<Item>, title: &str, count: Option<usize>) {
    items.push(Item::Header { title: title.to_owned(), count, first: items.is_empty() });
}

/// Who is around across networks: the Requests line, In a call, Online, Recent, then links to
/// the full lists. Nobody around says so and leaves Recent.
pub fn around_sections(around: &Around, networks: &[Network]) -> Vec<Item> {
    let mut items = Vec::new();
    if around.requests > 0 {
        items.push(Item::Requests(around.requests));
    }
    let section = |items: &mut Vec<Item>, title: &str, people: &[Rc<Person>]| {
        let mut people: Vec<Rc<Person>> = people.iter().filter(|p| !p.bot).cloned().collect();
        if people.is_empty() {
            return;
        }
        people.sort_by(by_name);
        header(items, title, Some(people.len()));
        items.extend(people.into_iter().map(Item::Person));
    };
    if around.in_call.is_empty() && around.online.is_empty() {
        items.push(Item::Note("Nobody's around right now".into()));
    }
    section(&mut items, "In a call", &around.in_call);
    section(&mut items, "Online", &around.online);
    section(&mut items, "Recent", &around.recent);
    if !networks.is_empty() {
        items.push(Item::Links(networks.to_vec()));
    }
    items
}

/// The rows of a scoped list for one chip (Discord's Friends). `presence` is what is known of
/// each person now.
pub fn sections(owned: &[Rc<Person>], chip: Chip, presence: &dyn Fn(&Person) -> Presence) -> Vec<Item> {
    let mut items = Vec::new();
    let mut section = |title: &str, mut people: Vec<Rc<Person>>| {
        if people.is_empty() {
            return;
        }
        people.sort_by(by_name);
        header(&mut items, title, Some(people.len()));
        items.extend(people.into_iter().map(Item::Person));
    };
    let of = |wanted: &[Relation]| -> Vec<Rc<Person>> { owned.iter().filter(|person| !person.bot && wanted.contains(&person.relation)).cloned().collect() };
    match chip {
        Chip::Online | Chip::All => {
            let (around, rest): (Vec<_>, Vec<_>) = of(&[Relation::Friend, Relation::Contact]).into_iter().partition(|person| presence(person).is_around());
            section("Online", around);
            if chip == Chip::All {
                section("Offline", rest);
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

/// The rows while searching: my people that match, then each network's own answer under its
/// name (at most eight, people already shown removed), then a line for each network still
/// being asked. `place` names an account's header: `Slack · Harbor Labs`.
pub fn search_sections(owned: &[Rc<Person>], found: &[Rc<Person>], needle: &str, waiting: &[Network], place: &dyn Fn(&AccountKey) -> String) -> Vec<Item> {
    let (typed, needle) = (needle.trim(), needle_of(needle));
    let mut items = Vec::new();
    let mut held: HashSet<Key> = HashSet::new();
    let mut mine: Vec<Rc<Person>> =
        owned.iter().filter(|person| !person.bot && person.relation != Relation::Blocked && matches(person, &needle) && held.insert(person.key.clone())).cloned().collect();
    mine.sort_by(by_name);
    if !mine.is_empty() {
        header(&mut items, "Your people", Some(mine.len()));
        items.extend(mine.into_iter().map(Item::Person));
    }
    let mut accounts: Vec<AccountKey> = Vec::new();
    for person in found {
        if !accounts.contains(&person.key.account()) {
            accounts.push(person.key.account());
        }
    }
    accounts.sort_by_key(|account| Network::ALL.iter().position(|n| *n == account.network));
    for account in accounts {
        let mut theirs: Vec<Rc<Person>> =
            found.iter().filter(|p| p.key.account() == account && !p.bot && !held.contains(&p.key) && matches(p, &needle)).cloned().collect();
        theirs.sort_by(by_name);
        theirs.truncate(PER_NETWORK);
        if !theirs.is_empty() {
            header(&mut items, &place(&account), None);
            items.extend(theirs.into_iter().map(Item::Person));
        }
    }
    for network in Network::ALL.into_iter().filter(|n| waiting.contains(n)) {
        items.push(Item::Note(format!("Asking {} for “{typed}”…", network.label())));
    }
    items
}

pub struct PeopleList {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    /// The network the list is narrowed to. `None` is who is around, across networks.
    scope: Option<Network>,
    chip: Chip,
    owned: Vec<Rc<Person>>,
    blocked: Vec<Rc<Person>>,
    around: Around,
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
    list: ListState,
    /// The row the arrow keys are on.
    cursor: Option<usize>,
    /// The presence the sections were last built from, and when.
    presence_seen: u64,
    built: Instant,
    regroup_pending: bool,
    refused: Option<String>,
}

impl PeopleList {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub, cx: &mut Context<Self>) -> PeopleList {
        let search = cx.new(|cx| TextInput::new("Find people", cx));
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
        let list = ListState::new(0, ListAlignment::Top, px(200.));
        let me = cx.entity_id();
        list.set_scroll_handler(move |_, _, cx| cx.notify(me));
        let mut people = PeopleList {
            shell,
            hub,
            scene: Scene::default(),
            scope: None,
            chip: Chip::Online,
            owned: Vec::new(),
            blocked: Vec::new(),
            around: Around::default(),
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
            list,
            cursor: None,
            presence_seen: 0,
            built: Instant::now(),
            regroup_pending: false,
            refused: None,
        };
        people.load(cx);
        people
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
        self.regroup_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.regroup_pending = false;
                // Who is around is chatd's to say: ask again rather than guess from dots.
                if this.scope.is_none() {
                    this.load_around(cx);
                }
                this.rebuild();
                cx.notify();
            });
        })
        .detach();
    }

    fn networks(&self) -> Vec<Network> {
        let signed_in: Vec<Network> = self.scene.links.iter().filter(|(_, link)| *link != Link::SignedOut).map(|(network, _)| *network).collect();
        crate::gui::layout::NETWORK_ORDER.into_iter().filter(|n| signed_in.contains(n)).collect()
    }

    fn in_scope(&self, network: Network) -> bool {
        self.scope.is_none_or(|scope| scope == network)
    }

    fn pending(&self) -> usize {
        self.owned.iter().filter(|person| matches!(person.relation, Relation::Incoming | Relation::Outgoing)).count()
    }

    /// `Slack · Harbor Labs`, or the network alone when it has one account that is a person.
    fn place(&self, account: &AccountKey) -> String {
        let several = self.scene.accounts.iter().filter(|own| own.key.network == account.network).count() > 1;
        let name = self.scene.accounts.iter().find(|own| &own.key == account).map(|own| own.name.clone()).unwrap_or_default();
        match account.network {
            Network::Slack | Network::Teams if !name.is_empty() => format!("{} · {name}", account.network.label()),
            _ if several && !name.is_empty() => format!("{} · {name}", account.network.label()),
            network => network.label().to_owned(),
        }
    }

    fn build(&self) -> Vec<Item> {
        if !self.needle.is_empty() {
            let mut waiting: Vec<Network> = self.waiting.iter().map(|account| account.network).collect();
            waiting.dedup();
            let mine: Vec<Rc<Person>> = self.owned.iter().chain(&self.around.in_call).chain(&self.around.online).chain(&self.around.recent).cloned().collect();
            return search_sections(&mine, &self.found, &self.needle, &waiting, &|account| self.place(account));
        }
        match self.scope {
            None => around_sections(&self.around, &self.networks()),
            Some(_) => {
                let owned = if self.chip == Chip::Blocked { &self.blocked } else { &self.owned };
                let hub = &self.hub;
                sections(owned, self.chip, &|person| shown_presence(hub, person))
            }
        }
    }

    fn rebuild(&mut self) {
        if !chips(self.scope, self.pending()).contains(&self.chip) {
            self.chip = Chip::Online;
        }
        let items = self.build();
        if *self.items != items {
            let old = self.items.clone();
            crate::gui::sidebar::respliced(&self.list, &old, &items, Item::anchor);
            self.items = Rc::new(items);
        }
        self.presence_seen = self.hub.presence_rev();
        self.built = Instant::now();
        self.cursor = self.cursor.filter(|ix| matches!(self.items.get(*ix), Some(Item::Person(_))));
    }

    /// Reads who is around, and the owned lists a page at a time until they are held whole.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.stale = false;
        self.page(0, self.epoch, cx);
        self.load_around(cx);
        let (epoch, this) = (self.epoch, cx.weak_entity());
        let blocked = Request::People { network: self.scope, relation: Some(Relation::Blocked), query: None, offset: 0, limit: PAGE_LIMIT };
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

    fn load_around(&mut self, cx: &mut Context<Self>) {
        let (epoch, this) = (self.epoch, cx.weak_entity());
        super::ask_later(cx, Request::Around { network: self.scope }, move |answer, cx| {
            let Ok(Response::Around { requests, in_call, online, recent }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                for people in [&in_call, &online, &recent] {
                    this.hub.learn(people);
                }
                let held = |people: Vec<Person>| people.into_iter().map(Rc::new).collect();
                let (in_call, online) = meetings_are_online(in_call, online);
                this.around = Around { requests, in_call: held(in_call), online: held(online), recent: held(recent) };
                this.loaded = true;
                this.rebuild();
                cx.notify();
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
                this.owned.extend(people.into_iter().filter(|p| !p.bot).map(Rc::new));
                this.loaded = true;
                this.rebuild();
                cx.notify();
                let held = this.owned.len() as u32;
                if got > 0 && offset + got < total.min(HELD) {
                    this.page(offset + got, epoch, cx);
                }
                let _ = held;
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
    /// "Asking…" comes down, and what it found is read.
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
        self.waiting = self.scene.accounts.iter().filter(|own| own.link == Link::Connected && self.in_scope(own.key.network)).map(|own| own.key.clone()).collect();
        self.query(query, cx);
        self.rebuild();
        cx.notify();
    }

    fn query(&mut self, query: String, cx: &mut Context<Self>) {
        let request = Request::People { network: self.scope, relation: None, query: Some(query), offset: 0, limit: QUERY_ROWS };
        let (epoch, this) = (self.query_epoch, cx.weak_entity());
        super::ask_later(cx, request, move |answer, cx| {
            let Ok(Response::People { people, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.query_epoch == epoch {
                    this.hub.learn(&people);
                    this.found = people.into_iter().filter(|p| !p.bot).map(Rc::new).collect();
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
        self.list.scroll_to(gpui::ListOffset::default());
        cx.notify();
    }

    /// Narrows the list to one network's own (Discord's Friends), or back to who is around.
    pub fn pick_scope(&mut self, scope: Option<Network>, chip: Option<Chip>, cx: &mut Context<Self>) {
        if let Some(chip) = chip {
            self.chip = chip;
        }
        if self.scope == scope {
            self.rebuild();
            cx.notify();
            return;
        }
        self.scope = scope;
        self.owned.clear();
        self.blocked.clear();
        self.found.clear();
        self.around = Around::default();
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
            self.list.scroll_to_reveal_item(ix);
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

    fn item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        match item {
            Item::Header { title, count, first } => {
                let count = count.map(|count| div().ml(px(4.)).child(count.to_string()));
                section_header(title, first).children(count).into_any_element()
            }
            Item::Note(words) => div()
                .h(px(theme::row_height(theme::ROW_ONE)))
                .px(px(theme::PANE_PAD))
                .flex()
                .items_center()
                .type_small()
                .text_color(theme::text_soft())
                .child(div().truncate().child(words))
                .into_any_element(),
            Item::Requests(count) => {
                let words = if count == 1 { "1 friend request".to_owned() } else { format!("{count} friend requests") };
                div()
                    .id("requests")
                    .w_full()
                    .h(px(theme::row_height(theme::ROW_ONE)))
                    .px(px(theme::INSET))
                    .py(px(1.))
                    .child(
                        div()
                            .size_full()
                            .px(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::hover()))
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_scope(Some(Network::Discord), Some(Chip::Pending), cx)))
                            .child(div().size(px(20.)).flex().items_center().justify_center().child(glyph("icons/users.svg", 16., theme::text_soft())))
                            .child(div().flex_1().type_ui().text_color(theme::text()).child(words))
                            .children(self.marks().then(|| crate::gui::sidebar::chat::mark(Network::Discord, 14., theme::text_faint())))
                            .child(glyph("icons/chevron-right.svg", 12., theme::text_faint())),
                    )
                    .into_any_element()
            }
            Item::Links(networks) => self.links(&networks, cx).into_any_element(),
            Item::Person(person) => self.person_row(ix, &person, cx).into_any_element(),
        }
    }

    fn marks(&self) -> bool {
        self.scope.is_none() && self.networks().len() >= 2
    }

    /// The full lists live in each network's home.
    fn links(&self, networks: &[Network], cx: &mut Context<Self>) -> Div {
        let mut line = div().w_full().pt(px(16.)).pb(px(24.)).px(px(theme::PANE_PAD)).flex().flex_wrap().items_center().gap(px(4.)).type_small().text_color(theme::text_faint());
        let last = networks.len().saturating_sub(1);
        for (ix, network) in networks.iter().copied().enumerate() {
            let words = match network {
                Network::Discord => "All Discord friends",
                Network::Telegram => "Telegram contacts",
                Network::Slack => "Slack",
                Network::Teams => "Teams",
                Network::Signal => "Signal contacts",
            };
            let shell = self.shell.clone();
            let link = div()
                .id(("people-link", network as usize))
                .cursor_pointer()
                .text_color(theme::text_soft())
                .hover(|style| style.text_color(theme::text()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    if network == Network::Discord {
                        this.pick_scope(Some(Network::Discord), Some(Chip::All), cx);
                    } else {
                        let _ = shell.update(cx, |shell, cx| shell.go(Mode::Network(network), cx));
                    }
                }))
                .child(words);
            line = line.child(link);
            if ix < last {
                line = line.child("·");
            }
        }
        line
    }

    fn person_row(&self, ix: usize, person: &Rc<Person>, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.scene.peer.as_ref() == Some(&person.key);
        let focused = self.cursor == Some(ix);
        let blocked = person.relation == Relation::Blocked;
        let ring = Ring::row(theme::sidebar(), selected, "person");
        let talking = person.activity.as_ref().is_some_and(|a| matches!(a.kind, ActivityKind::InVoice | ActivityKind::InCall | ActivityKind::Huddle));
        let in_call = !person.bot && (talking || in_call(&self.hub, &self.scene, &person.key));
        let several = self.scene.accounts.iter().filter(|own| own.key.network == person.key.network).count() > 1;
        let status = self.hub.status_of(&person.key);
        let status = if status.is_empty() { person.status.clone() } else { status };
        let second = match person.relation {
            Relation::Incoming | Relation::Outgoing => request_line(person),
            Relation::Blocked => "Blocked".to_owned(),
            _ => around_line(person, &status, in_call, several || matches!(person.key.network, Network::Teams)),
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
            .children(self.marks().then(|| crate::gui::sidebar::chat::mark(person.key.network, 14., if selected { theme::text_soft() } else { theme::text_faint() })))
            .child(div().min_w_0().truncate().type_ui().text_color(if blocked { theme::text_faint() } else { theme::text() }).child(person.name.clone()));
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
        div().id(("person", ix)).w_full().h(px(theme::row_height(theme::ROW_TWO))).px(px(theme::INSET)).py(px(1.)).child(
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
                .when(focused, |row| row.shadow(theme::focus_ring_inset()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.start(target.clone(), false, cx)))
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
                        .when(!second.is_empty(), |text| text.child(div().truncate().type_small().text_color(theme::text_soft()).child(second))),
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
        let (title, detail) = if !self.needle.is_empty() {
            let networks = self.scene.accounts.iter().filter(|own| own.link == Link::Connected).count();
            let checked = match networks {
                0 => "Checked your people.".to_owned(),
                1 => "Checked your people and 1 network.".to_owned(),
                n => format!("Checked your people and {n} networks."),
            };
            (format!("No one called “{}”", self.needle), checked)
        } else {
            match (self.chip, self.scope) {
                (Chip::Pending, _) => ("No pending requests".to_owned(), "Requests you get or send show up here.".to_owned()),
                (Chip::Blocked, _) => ("Nobody is blocked".to_owned(), "People you block in Discord show up here.".to_owned()),
                (Chip::Online, Some(Network::Discord)) => ("No friends online".to_owned(), "Friends who come online show up here.".to_owned()),
                (_, Some(Network::Discord)) => ("No friends on Discord yet".to_owned(), "Friends you add in Discord appear here.".to_owned()),
                (_, Some(Network::Telegram)) => ("No contacts yet".to_owned(), "Type a @username to find someone.".to_owned()),
                (_, Some(_)) => ("No direct messages yet".to_owned(), "Type a name to find anyone.".to_owned()),
                (_, None) => ("Nobody's around right now".to_owned(), "People you talk to show up here.".to_owned()),
            }
        };
        Some(empty_state("icons/users.svg", &title, &detail).px(px(24.)))
    }

    /// The chips of a scoped list, after its way back.
    fn scoped_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let scope = self.scope?;
        let pending = self.pending();
        let mut switch = segmented();
        for chip in chips(Some(scope), pending) {
            let label: SharedString = match chip {
                Chip::Online => "Online".into(),
                Chip::All => "All".into(),
                Chip::Pending => "Pending".into(),
                Chip::Blocked => "Blocked".into(),
            };
            let id: &'static str = match chip {
                Chip::Online => "chip-online",
                Chip::All => "chip-all",
                Chip::Pending => "chip-pending",
                Chip::Blocked => "chip-blocked",
            };
            let incoming = self.owned.iter().filter(|p| p.relation == Relation::Incoming).count() as u32;
            let count = (chip == Chip::Pending && incoming > 0).then(|| div().ml(px(5.)).child(badge_count_unread(incoming)));
            switch = switch.child(segment(id, label, self.chip == chip).children(count).on_click(cx.listener(move |this, _, _, cx| this.pick_chip(chip, cx))));
        }
        let word = if scope == Network::Discord { "Friends" } else { "Contacts" };
        Some(
            div()
                .flex_none()
                .px(px(10.))
                .pb(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    icon_button("people-back", "icons/chevron-left.svg", false)
                        .tooltip(tip("Everyone around", None))
                        .on_click(cx.listener(|this, _, _, cx| this.pick_scope(None, None, cx))),
                )
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(word)),
        )
        .map(|bar| div().flex_none().child(bar).child(div().px(px(theme::PANE_PAD)).pb(px(8.)).flex().child(switch)))
    }

    /// Types into the search field, for `--people-search`.
    #[cfg(feature = "gui-shot")]
    pub fn type_search(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_text(text, cx));
        self.typed(cx);
    }

    #[cfg(feature = "gui-shot")]
    pub fn stage(&mut self, people: Vec<Person>, scope: Option<Network>, chip: Chip, needle: &str, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.query_epoch += 1;
        self.hub.learn(&people);
        self.scope = scope;
        self.chip = chip;
        let (found, owned): (Vec<_>, Vec<_>) =
            people.into_iter().filter(|person| scope.is_none_or(|scope| person.key.network == scope)).map(Rc::new).partition(|person| person.relation == Relation::None);
        self.blocked = owned.iter().filter(|p| p.relation == Relation::Blocked).cloned().collect();
        self.owned = owned.into_iter().filter(|p| p.relation != Relation::Blocked).collect();
        let (around, rest): (Vec<_>, Vec<_>) = self.owned.iter().filter(|p| !p.bot).cloned().partition(|p| p.presence.is_around());
        self.around = Around { requests: self.pending() as u32, in_call: Vec::new(), online: around, recent: rest.into_iter().take(6).collect() };
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

pub fn bot_tag() -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(4.))
        .rounded(px(theme::R_CHIP))
        .bg(theme::quiet_fill())
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(theme::MEDIUM)
        .text_color(theme::text_soft())
        .child("App")
}

impl Render for PeopleList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.search.focus_handle(cx).is_focused(window);
        let items = self.items.clone();
        let visible = on_screen(&self.list, items.len(), |ix| items[ix].height());
        self.hub.watch(
            SOURCE,
            items[visible].iter().filter_map(|item| match item {
                Item::Person(person) if !person.bot => Some(person.key.clone()),
                _ => None,
            }),
            cx,
        );
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
        let line = |words: String, color: gpui::Rgba| div().flex_none().px(px(theme::PANE_PAD)).pb(px(4.)).type_small().text_color(color).child(words);
        let body = match self.empty() {
            Some(empty) => empty.into_any_element(),
            None => list(self.list.clone(), cx.processor(|this: &mut PeopleList, ix: usize, window, cx| this.item(ix, window, cx))).size_full().into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .px(px(theme::PANE_PAD))
                    .pb(px(8.))
                    .flex()
                    .items_center()
                    .child(search_field(focused).flex_1().min_w_0().line_height(px(18.)).child(self.search.clone()).children(clear)),
            )
            .children(self.scoped_bar(cx))
            .children(self.refused.clone().map(|words| line(words, theme::danger())))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Activity;

    fn person(name: &str, relation: Relation, presence: Presence) -> Rc<Person> {
        Rc::new(Person {
            handle: name.to_lowercase().replace(' ', "."),
            relation,
            presence,
            ..Person::new(Key { network: Network::Discord, account: "1".into(), id: name.to_lowercase() }, name)
        })
    }

    fn names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Header { title, count: Some(count), .. } => format!("[{title} {count}]"),
                Item::Header { title, .. } => format!("[{title}]"),
                Item::Person(person) => person.name.clone(),
                Item::Note(words) => format!("({words})"),
                Item::Requests(count) => format!("<{count} requests>"),
                Item::Links(networks) => format!("<links {}>", networks.len()),
            })
            .collect()
    }

    fn friends() -> Vec<Rc<Person>> {
        vec![
            person("tobias", Relation::Friend, Presence::Idle),
            person("Mara", Relation::Friend, Presence::Offline),
            person("Ines", Relation::Friend, Presence::Online),
            person("esme", Relation::Contact, Presence::Unknown),
            person("Kofi", Relation::Friend, Presence::Busy),
            person("Rafaela", Relation::Incoming, Presence::Unknown),
            person("Hana", Relation::Outgoing, Presence::Unknown),
        ]
    }

    #[test]
    fn around_reads_requests_in_a_call_online_recent_and_never_a_bot() {
        let bot = Rc::new(Person { bot: true, ..(*person("Lichen Bot", Relation::Contact, Presence::Online)).clone() });
        let around = Around {
            requests: 2,
            in_call: vec![person("Haruto", Relation::Friend, Presence::Online)],
            online: vec![person("tobias", Relation::Friend, Presence::Idle), bot, person("Ines", Relation::Friend, Presence::Online)],
            recent: vec![person("Mara", Relation::Friend, Presence::Offline)],
        };
        let got = names(&around_sections(&around, &[Network::Discord, Network::Slack]));
        assert_eq!(got, ["<2 requests>", "[In a call 1]", "Haruto", "[Online 2]", "Ines", "tobias", "[Recent 1]", "Mara", "<links 2>"]);
        let nobody = Around { recent: around.recent.clone(), ..Around::default() };
        assert_eq!(names(&around_sections(&nobody, &[])), ["(Nobody's around right now)", "[Recent 1]", "Mara"]);
    }

    #[test]
    fn a_meeting_or_a_presentation_is_online_not_in_a_call() {
        let doing = |name: &str, kind: ActivityKind| Person {
            activity: Some(Box::new(Activity { kind, name: String::new(), place: String::new(), conversation: None })),
            ..(*person(name, Relation::Contact, Presence::Busy)).clone()
        };
        let in_call = vec![doing("Ana", ActivityKind::InMeeting), doing("Farah", ActivityKind::Presenting), doing("Odalys", ActivityKind::InCall)];
        let (calls, online) = meetings_are_online(in_call, vec![(*person("Ines", Relation::Friend, Presence::Online)).clone()]);
        assert_eq!(calls.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Odalys"]);
        assert_eq!(online.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Ines", "Ana", "Farah"]);
        assert_eq!(around_line(&online[1], "", false, false), "In a meeting");
    }

    #[test]
    fn line_two_says_where_they_talk_then_their_status_then_what_they_do_never_online() {
        let mut haruto = (*person("Haruto", Relation::Friend, Presence::Online)).clone();
        assert_eq!(around_line(&haruto, "", false, false), "");
        haruto.activity = Some(Box::new(Activity { kind: ActivityKind::Playing, name: "Outer Ridge".into(), place: String::new(), conversation: None }));
        assert_eq!(around_line(&haruto, "", false, false), "Playing Outer Ridge");
        haruto.emoji = "🌴".into();
        assert_eq!(around_line(&haruto, "Back Monday", false, false), "🌴 Back Monday");
        haruto.activity = Some(Box::new(Activity { kind: ActivityKind::InVoice, name: "Campfire".into(), place: "Moss & Lichen".into(), conversation: None }));
        assert_eq!(around_line(&haruto, "Back Monday", false, false), "Campfire · Moss & Lichen");
        let titled = Person { title: "Designer".into(), ..(*person("Dev", Relation::Contact, Presence::Online)).clone() };
        assert_eq!((around_line(&titled, "", false, true), around_line(&titled, "", false, false)), ("Designer".to_owned(), String::new()));
    }

    #[test]
    fn friends_scoped_read_online_then_offline_and_requests_apart() {
        let all = sections(&friends(), Chip::All, &|person| person.presence);
        assert_eq!(names(&all), ["[Online 3]", "Ines", "Kofi", "tobias", "[Offline 2]", "esme", "Mara"]);
        let pending = sections(&friends(), Chip::Pending, &|person| person.presence);
        assert_eq!(names(&pending), ["[Incoming 1]", "Rafaela", "[Outgoing 1]", "Hana"]);
        assert!(sections(&friends(), Chip::Blocked, &|person| person.presence).is_empty());
        assert_eq!(chips(Some(Network::Discord), 0), [Chip::Online, Chip::All, Chip::Pending, Chip::Blocked]);
        assert_eq!(chips(Some(Network::Slack), 3), [Chip::Online, Chip::All]);
    }

    #[test]
    fn search_finds_mine_at_once_then_each_network_under_its_own_header() {
        let mut held = friends();
        held.push(person("Mårten Öberg", Relation::Friend, Presence::Online));
        let place = |account: &AccountKey| format!("{} · ws", account.network.label());
        assert_eq!(names(&search_sections(&held, &[], "marten", &[], &place)), ["[Your people 1]", "Mårten Öberg"]);
        assert_eq!(names(&search_sections(&held, &[], "@MÅRTEN.ö", &[], &place)), ["[Your people 1]", "Mårten Öberg"]);
        let slack = |name: &str| {
            let mut found = (*person(name, Relation::None, Presence::Unknown)).clone();
            found.key.network = Network::Slack;
            Rc::new(found)
        };
        let bot = Rc::new(Person { bot: true, ..(*slack("Marvin Bot")).clone() });
        let found = vec![slack("Marcus Feld"), bot, person("Mara", Relation::Friend, Presence::Offline)];
        let rows = search_sections(&held, &found, "mar", &[Network::Telegram], &place);
        assert_eq!(names(&rows), ["[Your people 2]", "Mara", "Mårten Öberg", "[Slack · ws]", "Marcus Feld", "(Asking Telegram for “mar”…)"]);
        assert!(search_sections(&held, &[], "zzz", &[], &place).is_empty());
    }

    #[test]
    fn a_network_saying_it_searched_ends_its_line_and_only_for_the_query_on_screen() {
        let account = |network, id: &str| AccountKey { network, account: id.into() };
        let (north, river) = (account(Network::Slack, "T1"), account(Network::Slack, "T2"));
        let mut waiting = vec![north.clone(), river.clone()];
        assert!(!search_ended(&mut waiting, Some("mar"), &north, "ma"));
        assert!(!search_ended(&mut waiting, None, &north, "mar"));
        assert!(search_ended(&mut waiting, Some("mar"), &north, "mar"));
        assert!(!search_ended(&mut waiting, Some("mar"), &north, "mar"));
        assert_eq!(waiting, [river]);
    }

    #[test]
    fn an_answered_request_changes_its_row_in_place() {
        let (mut owned, mut blocked) = (friends(), Vec::new());
        let key = |name: &str| person(name, Relation::None, Presence::Unknown).key.clone();
        apply_related(&mut owned, &mut blocked, &key("Rafaela"), RelateAction::Accept);
        assert_eq!(owned.iter().find(|p| p.name == "Rafaela").map(|p| p.relation), Some(Relation::Friend));
        apply_related(&mut owned, &mut blocked, &key("Mara"), RelateAction::Block);
        assert_eq!(blocked.iter().map(|p| (p.name.as_str(), p.relation)).collect::<Vec<_>>(), [("Mara", Relation::Blocked)]);
        let asks = person("Rafaela Quist", Relation::Incoming, Presence::Unknown);
        assert_eq!(request_line(&asks), "Wants to be friends · @rafaela.quist");
    }
}
