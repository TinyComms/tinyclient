//! The quick switcher (`cmd-K`): a few letters of any conversation, Space or person, then Return.
//! chatd finds the conversations (`Find`, from its store, per keystroke); the shell ranks what
//! comes back and frees it on close. No tree is read and no network is asked.

use super::rank;
use super::Mode;
use crate::client::ask;
use crate::gui::components::{
    fade_in, glyph, keycap, network_icon, rollup_badge, row_badge, scrim, Avatar, Shape,
};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::view::now;
use gpui::{
    div, prelude::*, px, Context, Div, Entity, Focusable, FontWeight, HighlightStyle, MouseButton,
    ScrollHandle, SharedString, Stateful, StyledText, Subscription, Window,
};
use protocol::{Badge, Conversation, Key, Person, Relation, Request, Response, Space};
use std::ops::Range;
use std::rc::Rc;

/// Rows shown at most.
const SHOWN: usize = 20;
/// Places listed before anything is typed, after the few where someone waits.
const RECENT: usize = 8;
const WAITING: usize = 4;
/// The rows on show at once; the rest scroll.
const VISIBLE: f32 = 9.;
const ROW: f32 = 44.;
/// Conversations asked of chatd for one query: several times what is shown, so ranking has
/// something to choose from.
const FOUND: u32 = 80;

#[derive(Clone)]
pub enum Target {
    Conversation(Rc<Conversation>),
    Space(Key),
    Person(Box<Person>),
    /// The last row: search messages for what was typed.
    Search(String),
}

#[derive(Clone)]
pub struct Hit {
    pub title: String,
    /// Where it is: `Space › Group`, `Friend`, `Server`.
    pub context: String,
    pub matched: Vec<Range<usize>>,
    pub target: Target,
    /// A label drawn above this row when nothing is typed: `Waiting`, `Recent`.
    pub section: Option<&'static str>,
}

pub struct Switcher {
    pub input: Entity<TextInput>,
    conversations: Vec<Rc<Conversation>>,
    people: Vec<Person>,
    pub hits: Vec<Hit>,
    pub selected: usize,
    scroll: ScrollHandle,
    /// chatd did not answer when the switcher opened.
    down: bool,
    people_asked: u64,
    found_asked: u64,
    _subscriptions: [Subscription; 2],
}

/// Where a conversation is, for the row's second column: `Space › Group`.
pub fn context(c: &Conversation) -> String {
    let place: Vec<&str> = [c.space_title.as_str(), c.group_title.as_str()].into_iter().filter(|part| !part.is_empty()).collect();
    match (place.is_empty(), c.kind.is_direct()) {
        (false, _) => place.join(" › "),
        (true, true) => "Direct message".to_owned(),
        (true, false) => c.key.network.label().to_owned(),
    }
}

/// The rows for what is typed, best first.
pub fn hits(
    query: &str,
    conversations: &[Rc<Conversation>],
    spaces: &[Space],
    people: &[Person],
    previous: Option<&Key>,
    can_search: bool,
    now: i64,
) -> Vec<Hit> {
    let conversation = |c: &Rc<Conversation>, matched, section| Hit {
        title: c.title.clone(),
        context: context(c),
        matched,
        target: Target::Conversation(c.clone()),
        section,
    };
    // The same conversation can come from the Inbox and from its Space's tree.
    let mut seen: std::collections::HashSet<&Key> = std::collections::HashSet::new();
    let conversations: Vec<&Rc<Conversation>> = conversations.iter().filter(|c| seen.insert(&c.key)).collect();
    let typed = query.trim();
    if typed.trim_start_matches(['#', '@']).is_empty() && !typed.starts_with('@') {
        // Nothing typed: who is waiting, then where I have been. The first recent place is the
        // one before this, so `cmd-K, Return` goes back and forth between two.
        let mut waiting: Vec<&Rc<Conversation>> =
            conversations.iter().copied().filter(|c| matches!(c.badge(), Badge::Count(_)) && !c.muted).collect();
        waiting.sort_by_key(|c| std::cmp::Reverse(c.updated));
        waiting.truncate(WAITING);
        let mut recent: Vec<&Rc<Conversation>> =
            conversations.iter().copied().filter(|c| !waiting.iter().any(|w| w.key == c.key)).collect();
        recent.sort_by_key(|c| (previous != Some(&c.key), std::cmp::Reverse(c.updated)));
        recent.truncate(RECENT);
        let mut rows: Vec<Hit> = Vec::new();
        for (ix, c) in waiting.into_iter().enumerate() {
            rows.push(conversation(c, Vec::new(), (ix == 0).then_some("Waiting")));
        }
        for (ix, c) in recent.into_iter().enumerate() {
            rows.push(conversation(c, Vec::new(), (ix == 0).then_some("Recent")));
        }
        return rows;
    }

    let only_people = typed.starts_with('@');
    let only_channels = typed.starts_with('#');
    let mut scored: Vec<(u32, i64, Hit)> = Vec::new();
    {
        for c in conversations.iter().copied() {
            // `#` leaves out people; `@` leaves out everything else.
            if (only_channels && c.kind.is_direct()) || (only_people && !c.kind.is_direct()) {
                continue;
            }
            if let Some((score, matched)) = rank::matches(typed, &c.title) {
                let badge = c.badge();
                let lift = rank::boost(matches!(badge, Badge::Count(_)), badge != Badge::None, c.kind.is_direct(), c.updated, now);
                scored.push((score + lift, c.updated, conversation(c, matched, None)));
            }
        }
    }
    if !only_people && !only_channels {
        for space in spaces {
            if let Some((score, matched)) = rank::matches(typed, &space.name) {
                let hit = Hit {
                    title: space.name.clone(),
                    context: space.key.network.label().to_owned(),
                    matched,
                    target: Target::Space(space.key.clone()),
                    section: None,
                };
                scored.push((score + rank::boost(space.attention > 0, space.unread, false, 0, now), 0, hit));
            }
        }
    }
    if !only_channels {
        for person in people {
            // Someone I already have a conversation with is found as that conversation.
            let known = conversations.iter().any(|c| {
                c.peer.as_deref() == Some(&person.key.id) && c.key.account == person.key.account && c.key.network == person.key.network
            });
            if known {
                continue;
            }
            let Some((score, matched)) = rank::matches(typed, &person.name) else {
                continue;
            };
            let context = match person.relation {
                Relation::Friend => "Friend",
                Relation::Contact => "Contact",
                _ => "New message",
            };
            let hit = Hit {
                title: person.name.clone(),
                context: context.to_owned(),
                matched,
                target: Target::Person(Box::new(person.clone())),
                section: None,
            };
            scored.push((score, 0, hit));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then_with(|| a.2.title.cmp(&b.2.title)));
    let mut rows: Vec<Hit> = scored.into_iter().take(SHOWN - 1).map(|(_, _, hit)| hit).collect();
    let words = typed.trim_start_matches(['#', '@']).to_owned();
    if can_search && !words.is_empty() {
        rows.push(Hit {
            title: format!("Search messages for “{words}”"),
            context: String::new(),
            matched: Vec::new(),
            target: Target::Search(words),
            section: None,
        });
    }
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
        let held: Vec<Rc<Conversation>> = self
            .sidebar_state
            .inbox
            .rows()
            .map(|(_, row)| row.clone())
            .chain(self.trees.values().flat_map(|tree| tree.conversations.iter().cloned()))
            .collect();
        self.sidebar_state.switcher = Some(Switcher {
            input,
            conversations: held,
            people: Vec::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            down: false,
            people_asked: 0,
            found_asked: 0,
            _subscriptions: subscriptions,
        });
        self.requery(cx);
    }

    /// The rows for what is typed now, from what chatd last found.
    fn rehit(&mut self, keep: bool, cx: &mut Context<Self>) {
        let previous = self.sidebar_state.previous();
        let can_search = self.slots.search.is_some();
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        let query = switcher.input.read(cx).text().to_owned();
        let picked = switcher.selected;
        switcher.hits = hits(&query, &switcher.conversations, &self.spaces, &switcher.people, previous.as_ref(), can_search, now());
        // With no match the search row is the one Return takes.
        switcher.selected = if keep { picked.min(switcher.hits.len().saturating_sub(1)) } else { 0 };
        cx.notify();
    }

    pub(in crate::gui) fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_state.switcher.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Matches again what is typed, and asks chatd's store for conversations and people by
    /// that name. The newest answer wins; rows already shown stay until it lands.
    fn requery(&mut self, cx: &mut Context<Self>) {
        self.rehit(false, cx);
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        let query = switcher.input.read(cx).text().trim().to_owned();
        let name = query.trim_start_matches(['#', '@']).to_owned();
        switcher.found_asked += 1;
        switcher.people_asked += 1;
        let (found_asked, people_asked) = (switcher.found_asked, switcher.people_asked);
        let find = Request::Find { network: None, query: name.clone(), limit: FOUND };
        self.off_thread(cx, move || ask(&find), move |this, answer, cx| {
            let Some(switcher) = this.sidebar_state.switcher.as_mut().filter(|switcher| switcher.found_asked == found_asked) else {
                return;
            };
            match answer {
                Ok(Response::Conversations { conversations, .. }) => {
                    // Rows the sidebar holds are fresher than nothing while this was out, but
                    // the store's answer is the whole truth for this query.
                    switcher.conversations = conversations.into_iter().map(Rc::new).collect();
                    switcher.down = false;
                }
                _ => switcher.down = true,
            }
            this.rehit(true, cx);
        });
        if name.is_empty() || query.starts_with('#') {
            return;
        }
        let request = Request::People { network: None, relation: None, query: Some(name), offset: 0, limit: SHOWN as u32 };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            let (Ok(Response::People { people, .. }), Some(switcher)) = (answer, &mut this.sidebar_state.switcher) else {
                return;
            };
            if switcher.people_asked != people_asked || switcher.people == people {
                return;
            }
            switcher.people = people;
            this.rehit(true, cx);
        });
    }

    fn move_pick(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.sidebar_state.switcher else {
            return;
        };
        let last = switcher.hits.len().saturating_sub(1);
        switcher.selected = if down { (switcher.selected + 1).min(last) } else { switcher.selected.saturating_sub(1) };
        switcher.scroll.scroll_to_item(switcher.selected);
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
        self.close_switcher(window, cx);
        match hit.target {
            Target::Conversation(c) => self.select(c.key.clone(), &c.title, cx),
            // Picking a Space is the one case where the sidebar follows.
            Target::Space(space) => self.go(Mode::Space(space), cx),
            // Their conversation if there is one; a new one only when there is not.
            Target::Person(person) => self.open_person(&person, cx),
            Target::Search(words) => {
                if let Some(search) = self.slots.search {
                    search(self, words, cx);
                }
            }
        }
    }

    fn hit_row(&self, ix: usize, hit: &Hit, selected: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let leading = match &hit.target {
            Target::Conversation(c) => self.conversation_avatar(c, 28., selected),
            Target::Space(key) => {
                let space = self.spaces.iter().find(|space| &space.key == key);
                Avatar::new(key, &hit.title, 28.)
                    .shape(Shape::Place)
                    .image_at(space.and_then(|space| self.media_path(key, space.icon.as_ref())))
                    .build()
            }
            Target::Person(person) => Avatar::new(&person.key, &person.name, 28.)
                .image_at(self.media_path(&person.key, person.avatar.as_ref()))
                .build(),
            Target::Search(_) => div()
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(glyph("icons/search.svg", 16., theme::text_soft())),
        };
        let network = match &hit.target {
            Target::Conversation(c) => Some(c.key.network),
            Target::Space(key) => Some(key.network),
            Target::Person(person) => Some(person.key.network),
            Target::Search(_) => None,
        };
        let trailing = match &hit.target {
            Target::Conversation(c) => row_badge(c),
            Target::Space(key) => self.spaces.iter().find(|s| &s.key == key).and_then(|s| rollup_badge(s.attention, s.unread)),
            Target::Person(_) => None,
            Target::Search(_) => Some(keycap("⌘⇧F")),
        };
        // The letters that matched are heavier: the order is the rank, and this is why.
        let lit = HighlightStyle { color: Some(theme::text().into()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
        let title = hit.title.trim_start_matches('#');
        let shift = hit.title.len() - title.len();
        let highlights: Vec<(Range<usize>, HighlightStyle)> = hit
            .matched
            .iter()
            .filter(|range| range.start >= shift)
            .map(|range| (range.start - shift..range.end - shift, lit))
            .collect();
        div()
            .id(("hit", ix))
            .h(px(ROW))
            .flex_none()
            .px(px(10.))
            .rounded(px(theme::R_CARD))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::selected()))
            .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
            .on_click(cx.listener(move |this, _, window, cx| this.pick(Some(ix), window, cx)))
            .child(leading)
            .children(network.map(|network| network_icon(network, 14.)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .type_ui()
                    .text_color(if selected || hit.matched.is_empty() { theme::text() } else { theme::text_soft() })
                    .child(StyledText::new(SharedString::from(title.to_owned())).with_highlights(highlights)),
            )
            .child(div().flex_1().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(hit.context.clone()))
            .children(trailing)
    }

    /// The switcher's sheet over a flat scrim, or nothing when it is closed.
    pub(in crate::gui) fn switcher(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let switcher = self.sidebar_state.switcher.as_ref()?;
        let query = switcher.input.read(cx).text().trim().to_owned();
        let width = (f32::from(window.viewport_size().width) - 48.).min(600.);
        let hint = |keys: &'static str, words: &'static str| {
            div().flex().items_center().gap(px(6.)).child(keycap(keys)).child(words)
        };
        let nothing = switcher.hits.iter().all(|hit| matches!(hit.target, Target::Search(_)));
        let said = if switcher.down {
            Some("chatd isn't running".to_owned())
        } else if nothing && !query.is_empty() {
            Some(format!("Nothing called “{query}”"))
        } else {
            None
        };
        // One child per hit, its label included, so the scroll handle's index is the hit's.
        // The list is as tall as whole rows that fit above the footer, so the last one on show
        // is never cut by it.
        let said_height = if said.is_some() { ROW } else { 0. };
        let room = f32::from(window.viewport_size().height) - 96. - 52. - 32. - 24. - said_height - 8.;
        let fitting = (room / ROW).floor().clamp(2., VISIBLE);
        let mut rows = div()
            .id("hits")
            .p(px(4.))
            .max_h(px(fitting * ROW + 8.))
            .overflow_y_scroll()
            .track_scroll(&switcher.scroll)
            .flex()
            .flex_col();
        for (ix, hit) in switcher.hits.iter().enumerate() {
            let label = hit.section.map(|section| {
                div().h(px(24.)).px(px(8.)).flex().items_center().type_micro().text_color(theme::text_soft()).child(section)
            });
            rows = rows.child(div().flex_none().children(label).child(self.hit_row(ix, hit, ix == switcher.selected, cx)));
        }
        let sheet = div()
            .w(px(width))
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
            .children(said.map(|words| {
                div().h(px(ROW)).flex_none().px(px(16.)).flex().items_center().type_ui().text_color(theme::text_soft()).child(words)
            }))
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
            scrim()
                .flex()
                .flex_col()
                .items_center()
                .pt(px(96.))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_switcher(window, cx)))
                .child(fade_in("switcher", 120, sheet)),
        )
    }
}

impl super::State {
    /// The conversation open before this one, which leads the switcher's recent places.
    pub fn previous(&self) -> Option<Key> {
        self.history.behind().find_map(|location| location.open.as_ref().map(|(key, _)| key.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CallSupport, ConversationKind, Network};

    fn key(id: &str) -> Key {
        Key { network: Network::Discord, account: "me".into(), id: id.into() }
    }

    fn row(id: &str, kind: ConversationKind, updated: i64) -> Rc<Conversation> {
        Rc::new(Conversation {
            root: None,
            space_title: String::new(),
            topic: String::new(),
            parent: None,
            private: false,
            read_only: false,
            draft: String::new(),
            group_title: String::new(),
            occupants: None,
            key: key(id),
            kind,
            title: id.into(),
            space: None,
            group: None,
            position: 0,
            avatar: None,
            peer: None,
            unread: 0,
            mentions: 0,
            muted: false,
            starred: false,
            preview: String::new(),
            updated,
            last_read: None,
            call: CallSupport::None,
        })
    }

    fn titles(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|hit| format!("{}{}", hit.section.map_or(String::new(), |s| format!("[{s}] ")), hit.title)).collect()
    }

    #[test]
    fn nothing_typed_lists_who_waits_then_recent_places_led_by_the_previous_one() {
        use ConversationKind::{Channel, Direct};
        let mut waiting = (*row("ines", Direct, 50)).clone();
        waiting.unread = 2;
        let rows = vec![row("old", Channel, 10), row("newest", Channel, 90), Rc::new(waiting), row("before", Channel, 20)];
        let found = hits("", &rows, &[], &[], Some(&key("before")), true, 100);
        assert_eq!(titles(&found), ["[Waiting] ines", "[Recent] before", "newest", "old"]);
    }

    #[test]
    fn typed_letters_rank_names_and_end_with_the_search_row() {
        use ConversationKind::{Channel, Direct};
        let mut mentioned = (*row("redesign", Channel, 10)).clone();
        (mentioned.unread, mentioned.mentions) = (3, 1);
        let rows = vec![Rc::new(mentioned), row("design", Channel, 10), row("#design-crit", Channel, 10), row("Desmond", Direct, 10), row("general", Channel, 99)];
        let space = Space { key: key("s"), name: "Design Guild".into(), icon: None, position: 0, attention: 0, unread: false };

        let found = hits("des", &rows, std::slice::from_ref(&space), &[], None, true, 100);
        // An exact prefix wins; a mention lifts a weaker match but not past a better one.
        assert_eq!(
            titles(&found),
            ["Desmond", "design", "Design Guild", "#design-crit", "redesign", "Search messages for “des”"]
        );
        // `#` leaves out people and Spaces; `@` leaves out channels.
        assert_eq!(titles(&hits("#des", &rows, std::slice::from_ref(&space), &[], None, false, 100)), ["design", "#design-crit", "redesign"]);
        assert_eq!(titles(&hits("@des", &rows, &[space], &[], None, false, 100)), ["Desmond"]);
        // No match at all: only the search row is left, so Return still does something.
        assert_eq!(titles(&hits("zzz", &rows, &[], &[], None, true, 100)), ["Search messages for “zzz”"]);
    }

    #[test]
    fn a_row_says_which_space_it_is_in_so_two_channels_of_one_name_differ() {
        let mut moss = (*row("general", ConversationKind::Channel, 10)).clone();
        (moss.space_title, moss.group_title) = ("Moss & Lichen".into(), "Trail".into());
        let mut guild = (*row("general", ConversationKind::Channel, 10)).clone();
        guild.key.id = "general-2".into();
        guild.space_title = "Design Guild".into();
        let found = hits("gen", &[Rc::new(moss), Rc::new(guild)], &[], &[], None, false, 100);
        let places: Vec<&str> = found.iter().map(|hit| hit.context.as_str()).collect();
        assert_eq!(places, ["Moss & Lichen › Trail", "Design Guild"]);
        assert_eq!(context(&row("ines", ConversationKind::Direct, 0)), "Direct message");
    }

    #[test]
    fn a_conversation_read_from_two_lists_is_one_row() {
        let rows = vec![row("design", ConversationKind::Channel, 10), row("design", ConversationKind::Channel, 10)];
        assert_eq!(hits("des", &rows, &[], &[], None, false, 100).len(), 1);
    }
}
