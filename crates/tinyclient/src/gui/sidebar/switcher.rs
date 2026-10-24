//! The quick switcher (`cmd-K`, `design/polish/04-unified.md` §5): never blank. Nothing typed is
//! who needs me, then where I was. Typed letters are grouped into Conversations, People and
//! Spaces, each ranked, and what arrives later from the networks goes in below the selected row,
//! never above it. chatd finds the conversations (`Find`) and people (`People`); the shell ranks.

use super::rank;
use super::Mode;
use crate::client::ask;
use crate::gui::components::{fade_in, glyph, keycap, place::switcher_frame, row_badge, rollup_badge, Avatar, Shape};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::view::now;
use gpui::{
    div, prelude::*, px, Context, Div, Entity, Focusable, FontWeight, HighlightStyle, MouseButton, ScrollHandle, SharedString, Stateful,
    StyledText, Subscription, Window,
};
use protocol::{Badge, Conversation, ConversationKind, Key, Network, Person, Relation, Request, Response, Space, SEARCH_MIN_CHARS};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// Before anything is typed: who needs me, then where I was.
const NEEDS_YOU: usize = 5;
const RECENT: usize = 8;
/// Typed: the most of each group.
const CONVERSATIONS: usize = 6;
const PEOPLE: usize = 4;
const SPACES: usize = 3;
const ROW: f32 = 44.;
const HEADER: f32 = 28.;
/// Conversations asked of chatd for one query: several times what is shown, so ranking has
/// something to choose from.
const FOUND: u32 = 80;
/// The networks are asked for people once the typing has paused this long.
/// A ranked group: score, recency, row.
type Ranked = Vec<(u32, i64, Hit)>;

const PAUSE: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub enum Target {
    Conversation(Rc<Conversation>),
    Space(Key),
    Person(Box<Person>),
    /// The last row: search messages for what was typed.
    Search(String),
    /// "Asking Slack and Telegram…": not a place.
    Asking(String),
}

impl Target {
    /// What a row is, across answers: the selection follows it.
    fn identity(&self) -> String {
        match self {
            Target::Conversation(c) => format!("c/{:?}/{}/{}", c.key.network, c.key.account, c.key.id),
            Target::Space(key) => format!("s/{:?}/{}/{}", key.network, key.account, key.id),
            Target::Person(person) => format!("p/{:?}/{}/{}", person.key.network, person.key.account, person.key.id),
            Target::Search(_) => "search".into(),
            Target::Asking(_) => "asking".into(),
        }
    }
}

#[derive(Clone)]
pub struct Hit {
    pub title: String,
    /// Where it is: `Moss & Lichen › Projects`, `Harbor Labs · Direct message`, `Friend`.
    pub context: String,
    pub matched: Vec<Range<usize>>,
    pub target: Target,
    /// The header drawn above this row: `Needs you`, `Recent`, `Conversations`.
    pub section: Option<&'static str>,
    /// Words in place of a badge: `Previous`, `New message`.
    pub trailing: Option<&'static str>,
}

pub struct Switcher {
    pub input: Entity<TextInput>,
    conversations: Vec<Rc<Conversation>>,
    people: Vec<Person>,
    /// Recent places, newest first, as rows.
    recent: Vec<Rc<Conversation>>,
    pub hits: Vec<Hit>,
    pub selected: usize,
    scroll: ScrollHandle,
    /// chatd did not answer when the switcher opened.
    down: bool,
    /// The networks still being asked for people.
    asking: Vec<Network>,
    people_asked: u64,
    found_asked: u64,
    _subscriptions: [Subscription; 2],
}

/// What a conversation is, in words, when its place does not say it.
fn kind_word(c: &Conversation) -> &'static str {
    match c.kind {
        ConversationKind::Direct => "Direct message",
        ConversationKind::Group if c.meeting => "Meeting",
        ConversationKind::Group => "Group",
        ConversationKind::Channel | ConversationKind::Announcement => "Channel",
        ConversationKind::Voice => "Voice channel",
        ConversationKind::Forum => "Forum",
        ConversationKind::Thread => "Thread",
    }
}

/// Where a conversation is, for the row's second column: its place for a channel, its kind for
/// a DM, group or meeting, after the workspace on Slack and Teams.
pub fn context(c: &Conversation) -> String {
    let place: Vec<&str> = [c.space_title.as_str(), c.group_title.as_str()].into_iter().filter(|part| !part.is_empty()).collect();
    match (c.kind.is_direct(), place.first()) {
        // Only Slack and Teams keep DMs inside a workspace worth naming.
        (true, Some(space)) if matches!(c.key.network, Network::Slack | Network::Teams) => format!("{space} · {}", kind_word(c)),
        (true, _) => kind_word(c).to_owned(),
        (false, Some(_)) => place.join(" › "),
        (false, None) if matches!(c.key.network, Network::Telegram | Network::Signal | Network::Messages) => kind_word(c).to_owned(),
        (false, None) => c.key.network.label().to_owned(),
    }
}

fn space_word(network: Network) -> &'static str {
    match network {
        Network::Discord => "Server",
        Network::Slack => "Workspace",
        Network::Teams => "Team",
        Network::Telegram => "Folder",
        // Signal has no Spaces; this is never asked of it.
        Network::Signal | Network::Messages | Network::Tiny => "Group",
    }
}

/// A network named first narrows to it: `slack mara`.
fn narrowed(query: &str) -> (Option<Network>, &str) {
    let typed = query.trim_start();
    for network in Network::ALL {
        let word = network.label();
        if typed.len() > word.len() && typed[..word.len()].eq_ignore_ascii_case(word) && typed[word.len()..].starts_with(' ') {
            return (Some(network), typed[word.len()..].trim_start());
        }
    }
    (None, typed)
}

fn conversation_hit(c: &Rc<Conversation>, matched: Vec<Range<usize>>, section: Option<&'static str>) -> Hit {
    Hit { title: c.title.clone(), context: context(c), matched, target: Target::Conversation(c.clone()), section, trailing: None }
}

/// Nothing typed: who needs me (mentions, then unread DMs, newest first), then the places last
/// opened, the one before this first.
pub fn waiting_and_recent(conversations: &[Rc<Conversation>], recent: &[Rc<Conversation>], open: Option<&Key>) -> Vec<Hit> {
    let mut seen: std::collections::HashSet<&Key> = std::collections::HashSet::new();
    let mut waiting: Vec<&Rc<Conversation>> = conversations
        .iter()
        .filter(|c| seen.insert(&c.key))
        .filter(|c| !c.muted && !c.bot && matches!(c.badge(), Badge::Count(_)) && open != Some(&c.key))
        .collect();
    waiting.sort_by_key(|c| (c.mentions == 0, std::cmp::Reverse(c.updated)));
    waiting.truncate(NEEDS_YOU);
    let mut rows: Vec<Hit> = waiting.iter().enumerate().map(|(ix, c)| conversation_hit(c, Vec::new(), (ix == 0).then_some("Needs you"))).collect();
    let mut shown: Vec<&Key> = waiting.iter().map(|c| &c.key).collect();
    let mut first = true;
    for c in recent {
        if shown.contains(&&c.key) || open == Some(&c.key) {
            continue;
        }
        let mut hit = conversation_hit(c, Vec::new(), first.then_some("Recent"));
        if first {
            hit.trailing = Some("Previous");
        }
        first = false;
        shown.push(&c.key);
        rows.push(hit);
        if rows.len() >= waiting.len() + RECENT {
            break;
        }
    }
    rows
}

/// The rows for what is typed: Conversations, People and Spaces, each group ordered by rank and
/// the groups by their best hit, then the search row.
pub fn hits(query: &str, conversations: &[Rc<Conversation>], spaces: &[Space], people: &[Person], asking: &[Network], can_search: bool, now: i64) -> Vec<Hit> {
    let (network, typed) = narrowed(query.trim());
    let only_people = typed.starts_with('@');
    let only_channels = typed.starts_with('#');
    let words = typed.trim_start_matches(['#', '@']).to_owned();
    let mut seen: std::collections::HashSet<&Key> = std::collections::HashSet::new();
    let ours = |key: &Key| network.is_none_or(|network| key.network == network);
    let mut chats: Vec<(u32, i64, Hit)> = Vec::new();
    for c in conversations.iter().filter(|c| seen.insert(&c.key) && ours(&c.key)) {
        // `#` leaves out people; `@` leaves out everything else.
        if (only_channels && c.kind.is_direct()) || (only_people && !c.kind.is_direct()) || c.kind == ConversationKind::Thread {
            continue;
        }
        if let Some((score, matched)) = rank::matches(typed, &c.title) {
            let badge = c.badge();
            let lift = rank::boost(matches!(badge, Badge::Count(_)), badge != Badge::None, c.kind.is_direct(), c.updated, now);
            chats.push((score + lift, c.updated, conversation_hit(c, matched, None)));
        }
    }
    let mut places: Vec<(u32, i64, Hit)> = Vec::new();
    if !only_people && !only_channels {
        for space in spaces.iter().filter(|s| ours(&s.key)) {
            if let Some((score, matched)) = rank::matches(typed, &space.name) {
                let target = Target::Space(space.key.clone());
                let hit = Hit { title: space.name.clone(), context: space_word(space.key.network).to_owned(), matched, target, section: None, trailing: None };
                places.push((score + rank::boost(space.attention > 0, space.unread, false, 0, now), 0, hit));
            }
        }
    }
    let mut folk: Vec<(u32, i64, Hit)> = Vec::new();
    if !only_channels {
        for person in people.iter().filter(|p| !p.bot && ours(&p.key)) {
            // A person with a DM is found once, as the DM.
            let known = conversations.iter().any(|c| c.peer.as_deref() == Some(&person.key.id) && c.key.account == person.key.account && c.key.network == person.key.network);
            if known {
                continue;
            }
            let Some((score, matched)) = rank::matches(typed, &person.name) else {
                continue;
            };
            let relation = match person.relation {
                Relation::Friend => "Friend",
                Relation::Contact => "Contact",
                _ => person.key.network.label(),
            };
            let context = match person.handle.trim_start_matches('@') {
                "" => relation.to_owned(),
                handle => format!("{relation} · @{handle}"),
            };
            let hit = Hit { title: person.name.clone(), context, matched, target: Target::Person(Box::new(person.clone())), section: None, trailing: Some("New message") };
            folk.push((score, 0, hit));
        }
    }
    let rank = |group: &mut Vec<(u32, i64, Hit)>, most: usize| {
        group.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then_with(|| a.2.title.cmp(&b.2.title)));
        group.truncate(most);
    };
    rank(&mut chats, CONVERSATIONS);
    rank(&mut folk, PEOPLE);
    rank(&mut places, SPACES);
    let mut groups: Vec<(&'static str, Ranked)> = vec![("Conversations", chats), ("People", folk), ("Spaces", places)];
    if !asking.is_empty() && !only_channels {
        let names: Vec<&str> = asking.iter().map(|n| n.label()).collect();
        let words = match names.split_last() {
            Some((last, [])) => format!("Asking {last}…"),
            Some((last, rest)) => format!("Asking {} and {last}…", rest.join(", ")),
            None => String::new(),
        };
        let hit = Hit { title: words.clone(), context: String::new(), matched: Vec::new(), target: Target::Asking(words), section: None, trailing: None };
        groups[1].1.push((0, 0, hit));
    }
    groups.sort_by_key(|(_, group)| std::cmp::Reverse(group.iter().map(|(score, ..)| *score).max().unwrap_or(0)));
    let mut rows = Vec::new();
    for (label, group) in groups {
        for (ix, (_, _, mut hit)) in group.into_iter().enumerate() {
            hit.section = (ix == 0).then_some(label);
            rows.push(hit);
        }
    }
    if can_search && !words.is_empty() {
        let target = Target::Search(words.clone());
        rows.push(Hit { title: format!("Search messages for “{words}”"), context: String::new(), matched: Vec::new(), target, section: None, trailing: None });
    }
    rows
}

/// New rows from a later answer, with the ones down to the selected row kept where they were:
/// what arrives goes in below what the user is aiming at.
pub fn settle(old: &[Hit], selected: usize, new: Vec<Hit>) -> Vec<Hit> {
    if old.is_empty() || selected == 0 && old.first().map(|hit| hit.target.identity()) == new.first().map(|hit| hit.target.identity()) {
        return new;
    }
    let kept: Vec<Hit> = old.iter().take(selected + 1).cloned().collect();
    let held: std::collections::HashSet<String> = kept.iter().map(|hit| hit.target.identity()).collect();
    // A row still being asked about goes when its answer comes.
    let fresh_targets: std::collections::HashSet<String> = new.iter().map(|hit| hit.target.identity()).collect();
    let mut rows: Vec<Hit> = kept.into_iter().filter(|hit| fresh_targets.contains(&hit.target.identity()) || !matches!(hit.target, Target::Asking(_))).collect();
    rows.extend(new.into_iter().filter(|hit| !held.contains(&hit.target.identity())));
    rows
}

impl Shell {
    pub(in crate::gui) fn open_switcher(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_state.switcher.is_some() {
            self.close_switcher(window, cx);
            return;
        }
        // With nothing signed in there is nowhere to jump, and the sheet would not be drawn:
        // it must not take the keys from the first-launch card.
        if !self.snapshot.as_ref().is_some_and(|snapshot| !snapshot.signed_in().is_empty()) {
            return;
        }
        self.layout_state.menu = None;
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Jump to a conversation, a person or a server", cx);
            input.set_text(prefix, cx);
            input
        });
        let subscriptions = [
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Changed => this.requery(cx),
                InputEvent::Submit => this.pick(None, window, cx),
            }),
            cx.subscribe_in(&input, window, |this, _, event: &InputKey, window, cx| match event {
                InputKey::Escape => this.close_switcher(window, cx),
                InputKey::Up => this.move_pick(false, cx),
                InputKey::Down => this.move_pick(true, cx),
            }),
        ];
        window.focus(&input.focus_handle(cx), cx);
        // What the sidebar already holds can be matched before chatd answers.
        let held: Vec<Rc<Conversation>> =
            self.sidebar_state.held().cloned().chain(self.trees.values().flat_map(|tree| tree.conversations.iter().cloned())).collect();
        let recent = self.recent_places(&held);
        self.sidebar_state.switcher = Some(Switcher {
            input,
            conversations: held,
            people: Vec::new(),
            recent,
            hits: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            down: false,
            asking: Vec::new(),
            people_asked: 0,
            found_asked: 0,
            _subscriptions: subscriptions,
        });
        self.requery(cx);
    }

    /// The conversations opened before this one, newest first, as rows.
    fn recent_places(&self, held: &[Rc<Conversation>]) -> Vec<Rc<Conversation>> {
        let mut out: Vec<Rc<Conversation>> = Vec::new();
        for location in self.sidebar_state.history.behind() {
            let Some((key, title)) = &location.open else {
                continue;
            };
            if out.iter().any(|c| &c.key == key) {
                continue;
            }
            let row = held.iter().find(|c| &c.key == key).cloned().or_else(|| self.sidebar_state.find(&self.trees, key).map(Rc::new));
            out.push(row.unwrap_or_else(|| Rc::new(Conversation::new(key.clone(), ConversationKind::Channel, title.clone()))));
            if out.len() >= RECENT {
                break;
            }
        }
        out
    }

    /// The rows for what is typed now, from what chatd last found. `keep`: a later answer to
    /// the same letters, which must not move the rows above the selection.
    fn rehit(&mut self, keep: bool, cx: &mut Context<Self>) {
        let can_search = self.slots.search.is_some();
        let open = self.open.clone();
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        let query = switcher.input.read(cx).text().to_owned();
        let fresh = if narrowed(query.trim()).1.trim_start_matches(['#', '@']).is_empty() && !query.trim().starts_with('@') {
            waiting_and_recent(&switcher.conversations, &switcher.recent, open.as_ref())
        } else {
            hits(&query, &switcher.conversations, &self.spaces, &switcher.people, &switcher.asking, can_search, now())
        };
        if keep {
            switcher.hits = settle(&switcher.hits, switcher.selected, fresh);
            switcher.selected = switcher.selected.min(switcher.hits.len().saturating_sub(1));
        } else {
            switcher.hits = fresh;
            switcher.selected = 0;
        }
        cx.notify();
    }

    pub(in crate::gui) fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_state.switcher.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Matches again what is typed and asks chatd's store for conversations by that name at
    /// once; people are asked once the typing pauses, which is also when chatd asks networks.
    fn requery(&mut self, cx: &mut Context<Self>) {
        self.rehit(false, cx);
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        let query = switcher.input.read(cx).text().trim().to_owned();
        let name = narrowed(&query).1.trim_start_matches(['#', '@']).to_owned();
        switcher.found_asked += 1;
        switcher.people_asked += 1;
        switcher.asking.clear();
        let (found_asked, people_asked) = (switcher.found_asked, switcher.people_asked);
        let find = Request::Find { network: narrowed(&query).0, query: name.clone(), limit: FOUND };
        self.off_thread(cx, move || ask(&find), move |this, answer, cx| {
            let Some(switcher) = this.sidebar_state.switcher.as_mut().filter(|switcher| switcher.found_asked == found_asked) else {
                return;
            };
            match answer {
                Ok(Response::Conversations { conversations, .. }) if !conversations.is_empty() || !name_is_empty(&switcher.input, cx) => {
                    switcher.conversations = conversations.into_iter().map(Rc::new).collect();
                    switcher.down = false;
                }
                Ok(_) => switcher.down = false,
                Err(_) => switcher.down = true,
            }
            this.rehit(true, cx);
        });
        if name.chars().count() < SEARCH_MIN_CHARS || query.starts_with('#') {
            return;
        }
        let network = narrowed(&query).0;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PAUSE).await;
            let _ = this.update(cx, |this, cx| {
                let connected: Vec<Network> = this.snapshot.as_ref().map(|s| s.signed_in()).unwrap_or_default();
                let Some(switcher) = this.sidebar_state.switcher.as_mut().filter(|s| s.people_asked == people_asked) else {
                    return;
                };
                switcher.asking = connected.into_iter().filter(|n| network.is_none_or(|wanted| wanted == *n)).collect();
                this.rehit(true, cx);
                this.ask_people(name, network, people_asked, cx);
            });
        })
        .detach();
    }

    fn ask_people(&mut self, name: String, network: Option<Network>, asked: u64, cx: &mut Context<Self>) {
        let request = Request::People { network, relation: None, query: Some(name), offset: 0, limit: 20 };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            let (Ok(Response::People { people, .. }), Some(switcher)) = (answer, &mut this.sidebar_state.switcher) else {
                return;
            };
            if switcher.people_asked != asked {
                return;
            }
            switcher.people = people;
            this.rehit(true, cx);
        });
    }

    /// A network finished its people search: its "Asking…" goes and its answer is read.
    pub(in crate::gui) fn switcher_searched(&mut self, network: Network, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        if !switcher.asking.contains(&network) {
            return;
        }
        switcher.asking.retain(|n| *n != network);
        let query = switcher.input.read(cx).text().trim().to_owned();
        let (narrow, name) = narrowed(&query);
        let (name, asked) = (name.trim_start_matches(['#', '@']).to_owned(), switcher.people_asked);
        self.ask_people(name, narrow, asked, cx);
    }

    fn move_pick(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        let pickable = |hit: &Hit| !matches!(hit.target, Target::Asking(_));
        let next = if down {
            (switcher.selected + 1..switcher.hits.len()).find(|ix| pickable(&switcher.hits[*ix]))
        } else {
            (0..switcher.selected).rev().find(|ix| pickable(&switcher.hits[*ix]))
        };
        if let Some(next) = next {
            switcher.selected = next;
            switcher.scroll.scroll_to_item(next);
        }
        cx.notify();
    }

    /// Opens the selected row, or the one clicked.
    fn pick(&mut self, row: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(switcher) = &self.sidebar_state.switcher else {
            return;
        };
        let Some(hit) = switcher.hits.get(row.unwrap_or(switcher.selected)).cloned() else {
            return;
        };
        if matches!(hit.target, Target::Asking(_)) {
            return;
        }
        self.close_switcher(window, cx);
        match hit.target {
            Target::Conversation(c) => self.select(c.key.clone(), &c.title, cx),
            // Picking a Space is the one case where the sidebar follows.
            Target::Space(space) => self.go(Mode::Space(space), cx),
            // Their conversation if there is one; a new one only when there is not.
            Target::Person(person) => self.open_person(&person, cx),
            Target::Search(words) => {
                if let Some(search) = self.slots.search {
                    search(self, words, window, cx);
                }
            }
            Target::Asking(_) => {}
        }
    }

    fn hit_row(&self, ix: usize, hit: &Hit, selected: bool, marks: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let surface = theme::over(if selected { theme::selected() } else { gpui::rgba(0) }, theme::overlay());
        let leading = match &hit.target {
            Target::Conversation(c) => self.conversation_avatar(c, 28., surface, cx),
            Target::Space(key) => {
                let space = self.spaces.iter().find(|space| &space.key == key);
                let icon = space.and_then(|space| self.media_path(key, space.icon.as_ref()));
                Avatar::new(key, &hit.title, 28.).shape(Shape::Place).image_at(icon).build()
            }
            Target::Person(person) => Avatar::new(&person.key, &person.name, 28.).image_at(self.person_picture(&person.key, cx)).build(),
            Target::Search(_) => div().size(px(28.)).flex_none().flex().items_center().justify_center().child(glyph("icons/search.svg", 16., theme::text_soft())),
            Target::Asking(words) => {
                return div().id(("asking", ix)).h(px(28.)).px(px(10.)).flex().items_center().type_small().text_color(theme::text_soft()).child(words.clone());
            }
        };
        let network = match &hit.target {
            Target::Conversation(c) => Some(c.key.network),
            Target::Space(key) => Some(key.network),
            Target::Person(person) => Some(person.key.network),
            _ => None,
        };
        let trailing = match (&hit.target, hit.trailing) {
            (_, Some(words)) => Some(div().flex_none().type_small().text_color(theme::text_soft()).child(words)),
            (Target::Conversation(c), None) => row_badge(c),
            (Target::Space(key), None) => self.spaces.iter().find(|s| &s.key == key).and_then(|s| rollup_badge(s.attention, s.unread)),
            (Target::Search(_), None) => Some(keycap("⌘⇧F")),
            _ => None,
        };
        // The letters that matched are heavier: the order is the rank, and this is why.
        let lit = HighlightStyle { color: Some(theme::text().into()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
        let title = hit.title.trim_start_matches('#');
        let shift = hit.title.len() - title.len();
        let highlights: Vec<(Range<usize>, HighlightStyle)> =
            hit.matched.iter().filter(|range| range.start >= shift).map(|range| (range.start - shift..range.end - shift, lit)).collect();
        let channel = matches!(&hit.target, Target::Conversation(c) if !c.kind.is_direct() && c.key.network != Network::Telegram && c.key.network != Network::Teams);
        let shown_title = if channel { format!("#{title}") } else { title.to_owned() };
        let highlights = if channel { highlights.into_iter().map(|(r, s)| (r.start + 1..r.end + 1, s)).collect() } else { highlights };
        div()
            .id(("hit", ix))
            .h(px(ROW))
            .flex_none()
            .px(px(10.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::selected()))
            .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
            .on_click(cx.listener(move |this, _, window, cx| this.pick(Some(ix), window, cx)))
            .child(leading)
            .child(
                div()
                    .flex_none()
                    .max_w(px(240.))
                    .truncate()
                    .type_ui()
                    .text_color(if hit.matched.is_empty() { theme::text() } else { theme::text_soft() })
                    .child(StyledText::new(SharedString::from(shown_title)).with_highlights(highlights)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .children(network.filter(|_| marks && !hit.context.is_empty()).map(|network| super::chat::mark(network, 12., theme::text_soft())))
                    .child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(hit.context.clone())),
            )
            .children(trailing)
    }

    /// The switcher's sheet, over the window, or nothing when it is closed.
    pub(in crate::gui) fn switcher(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let switcher = self.sidebar_state.switcher.as_ref()?;
        let query = switcher.input.read(cx).text().trim().to_owned();
        let viewport = window.viewport_size();
        let frame = switcher_frame((f32::from(viewport.width), f32::from(viewport.height)), ROW);
        let hint = |keys: &'static str, words: &'static str| div().flex().items_center().gap(px(6.)).child(keycap(keys)).child(words);
        let nothing = switcher.hits.iter().all(|hit| matches!(hit.target, Target::Search(_) | Target::Asking(_)));
        let said = if switcher.down {
            Some("tinyclient isn't answering".to_owned())
        } else if nothing && !query.is_empty() {
            Some(format!("No one and nothing called “{query}”"))
        } else if switcher.hits.is_empty() {
            Some("Nothing waiting, and nowhere visited yet".to_owned())
        } else {
            None
        };
        let marks = self.marks_shown();
        // The list is as tall as whole rows that fit, so the last one on show is never cut.
        let room = frame.h - 52. - 32. - 10.;
        let mut rows = div().id("hits").p(px(4.)).pb(px(6.)).max_h(px(room.max(ROW * 2.))).overflow_y_scroll().track_scroll(&switcher.scroll).flex().flex_col();
        for (ix, hit) in switcher.hits.iter().enumerate() {
            let label = hit.section.map(|section| {
                div().h(px(HEADER)).px(px(10.)).pb(px(4.)).flex().items_end().type_micro().text_color(theme::text_soft()).child(section)
            });
            rows = rows.child(div().flex_none().children(label).child(self.hit_row(ix, hit, ix == switcher.selected, marks, cx)));
        }
        let sheet = div()
            .w(px(frame.w))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .occlude()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(52.))
                    .flex_none()
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(glyph("icons/search.svg", 16., theme::text_soft()))
                    .child(div().flex_1().min_w_0().text_size(px(16.)).line_height(px(22.)).child(switcher.input.clone()))
                    .child(keycap("esc")),
            )
            .children(said.map(|words| div().h(px(ROW)).flex_none().px(px(16.)).flex().items_center().type_ui().text_color(theme::text_soft()).child(words)))
            .child(rows)
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .border_t_1()
                    .border_color(theme::hairline())
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(hint("↑↓", "Move"))
                    .child(hint("↵", "Open"))
                    .child(hint("#", "Channels"))
                    .child(hint("@", "People")),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .pt(px(frame.y))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_switcher(window, cx)))
                .child(fade_in("switcher", 120, sheet)),
        )
    }
}

fn name_is_empty(input: &Entity<TextInput>, cx: &gpui::App) -> bool {
    narrowed(input.read(cx).text().trim()).1.trim_start_matches(['#', '@']).is_empty()
}

impl super::State {
    /// The conversation open before this one, which leads the switcher's recent places.
    #[allow(dead_code)]
    pub fn previous(&self) -> Option<Key> {
        self.history.behind().find_map(|location| location.open.as_ref().map(|(key, _)| key.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> Key {
        Key { network: Network::Discord, account: "me".into(), id: id.into() }
    }

    fn row(id: &str, kind: ConversationKind, updated: i64) -> Rc<Conversation> {
        let mut c = Conversation::new(key(id), kind, id);
        c.updated = updated;
        Rc::new(c)
    }

    fn titles(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|hit| format!("{}{}", hit.section.map_or(String::new(), |s| format!("[{s}] ")), hit.title)).collect()
    }

    fn person(id: &str) -> Person {
        Person { relation: Relation::Friend, ..Person::new(key(id), id) }
    }

    #[test]
    fn nothing_typed_lists_who_needs_me_mentions_first_then_where_i_was() {
        use ConversationKind::{Channel, Direct};
        let with = |c: Rc<Conversation>, unread, mentions| {
            let mut c = (*c).clone();
            (c.unread, c.mentions) = (unread, mentions);
            Rc::new(c)
        };
        let rows = vec![with(row("ines", Direct, 50), 2, 0), with(row("general", Channel, 40), 3, 1), row("old", Channel, 10), with(row("muted", Direct, 90), 2, 0)];
        let muted = {
            let mut m = (*rows[3]).clone();
            m.muted = true;
            Rc::new(m)
        };
        let rows = vec![rows[0].clone(), rows[1].clone(), rows[2].clone(), muted];
        let recent = vec![row("before", Channel, 0), row("ines", Direct, 0), row("older", Channel, 0)];
        let got = waiting_and_recent(&rows, &recent, None);
        assert_eq!(titles(&got), ["[Needs you] general", "ines", "[Recent] before", "older"]);
        assert_eq!(got[2].trailing, Some("Previous"));
    }

    #[test]
    fn typed_letters_group_conversations_people_and_spaces_and_end_with_search() {
        use ConversationKind::{Channel, Direct};
        let mut desmond = (*row("Desmond", Direct, 10)).clone();
        desmond.peer = Some("desmond".into());
        let rows = vec![row("design", Channel, 10), row("redesign", Channel, 10), Rc::new(desmond), row("general", Channel, 99)];
        let space = Space { key: key("s"), name: "Design Guild".into(), icon: None, position: 0, attention: 0, unread: false, folder: None, muted: false, hide_muted: false };
        let people = vec![person("desmond"), person("Desiree")];
        // Long after anything was said, so recency lifts nothing.
        let found = hits("des", &rows, std::slice::from_ref(&space), &people, &[], true, 10_000_000);
        // Desmond is a DM, so he is listed once, as the DM.
        assert_eq!(titles(&found), ["[Conversations] Desmond", "design", "redesign", "[People] Desiree", "[Spaces] Design Guild", "Search messages for “des”"]);
        // `#` leaves out people and Spaces; `@` leaves out channels.
        assert_eq!(titles(&hits("#des", &rows, std::slice::from_ref(&space), &people, &[], false, 100)), ["[Conversations] design", "redesign"]);
        assert_eq!(titles(&hits("@des", &rows, &[space], &people, &[], false, 100)), ["[Conversations] Desmond", "[People] Desiree"]);
        // A network named first narrows to it.
        assert_eq!(narrowed("slack mara"), (Some(Network::Slack), "mara"));
        assert!(hits("telegram des", &rows, &[], &people, &[], false, 100).is_empty());
    }

    #[test]
    fn later_answers_go_in_below_the_selected_row_never_above_it() {
        use ConversationKind::Channel;
        let rows = vec![row("design", Channel, 10), row("redesign", Channel, 10)];
        let first = hits("des", &rows, &[], &[], &[Network::Slack], false, 100);
        assert_eq!(titles(&first), ["[Conversations] design", "redesign", "[People] Asking Slack…"]);
        // The user moved to `redesign`; the network then finds a better person.
        let later = hits("des", &rows, &[], &[person("Des")], &[], false, 100);
        assert_eq!(titles(&later)[0], "[People] Des");
        let settled = settle(&first, 1, later);
        assert_eq!(titles(&settled), ["[Conversations] design", "redesign", "[People] Des"]);
    }

    #[test]
    fn a_row_says_where_it_is_so_two_channels_of_one_name_differ() {
        let mut moss = (*row("general", ConversationKind::Channel, 10)).clone();
        (moss.space_title, moss.group_title) = ("Moss & Lichen".into(), "Trail".into());
        assert_eq!(context(&moss), "Moss & Lichen › Trail");
        let mut slack = (*row("mara", ConversationKind::Direct, 0)).clone();
        slack.key.network = Network::Slack;
        slack.space_title = "Harbor Labs".into();
        assert_eq!(context(&slack), "Harbor Labs · Direct message");
        assert_eq!(context(&row("ines", ConversationKind::Direct, 0)), "Direct message");
    }

    #[test]
    fn a_group_or_meeting_is_named_for_what_it_is_not_a_direct_message() {
        let mut group = (*row("fernhill", ConversationKind::Group, 0)).clone();
        group.key.network = Network::Telegram;
        assert_eq!(context(&group), "Group");
        let mut teams = group.clone();
        (teams.key.network, teams.space_title) = (Network::Teams, "Northwind".into());
        assert_eq!(context(&teams), "Northwind · Group");
        teams.meeting = true;
        assert_eq!(context(&teams), "Northwind · Meeting");
        let mut channel = (*row("news", ConversationKind::Channel, 0)).clone();
        channel.key.network = Network::Telegram;
        assert_eq!(context(&channel), "Channel");
    }
}
