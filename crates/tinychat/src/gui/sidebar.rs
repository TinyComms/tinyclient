//! The sidebar: the same three parts in every mode (title strip, jump strip, one list). Inbox
//! is by recency across networks; a network's home is a tree of its Spaces; a Space is its
//! Groups and conversations in the network's own order; People is the people slice's view in
//! this frame.

pub mod history;
pub mod hold;
pub mod label;
pub mod pages;
pub mod rank;
pub mod switcher;
pub mod tree;
pub mod walk;

use super::components::{
    badge_dot, badge_mention, badge_quiet, banner, banner_action, empty_state, glyph, icon_button,
    keycap, list_row, network_icon, pane_title, rollup_badge, row_badge, row_tone, segment,
    segmented, skeleton, tip, Avatar, Shape, Tone,
};
use super::layout::{drags_window, Menu, TOP_BAR};
use super::people::presence;
use super::state::{Shell, Snapshot, SpaceTree, TROUBLE_SHOWN};
use super::theme::{self, Type};
use super::time;
use crate::client::ask;
use crate::view::now;
use gpui::{
    div, prelude::*, px, uniform_list, Context, Div, FocusHandle, MouseButton, ScrollStrategy,
    SharedString, Stateful, UniformListScrollHandle, Window,
};
use pages::Pages;
use history::History;
use protocol::{
    Badge, Conversation, ConversationKind, InboxCounts, InboxFilter, Key, Link,
    MediaFile, Network, Request, Response, Space, PAGE_LIMIT,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;
use tree::Row;

/// The name this view watches people under (see `people::Hub::watch`).
const WATCH: &str = "sidebar";
/// Drafts remembered ahead of their rows being read again.
const DRAFTS_HELD: usize = 256;
/// A network with this many Spaces or fewer shows them open.
const OPEN_SPACES: usize = 3;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Inbox,
    People,
    /// A network's home: its direct conversations, then its Spaces.
    Network(Network),
    Space(Key),
}

impl Mode {
    /// The network a mode is inside, for its banner.
    pub fn network(&self) -> Option<Network> {
        match self {
            Mode::Network(network) => Some(*network),
            Mode::Space(space) => Some(space.network),
            Mode::Inbox | Mode::People => None,
        }
    }
}

/// Somewhere the user has been.
#[derive(Clone, PartialEq, Eq)]
pub struct Location {
    mode: Mode,
    open: Option<(Key, String)>,
}

pub struct State {
    pub mode: Mode,
    pub filter: InboxFilter,
    /// The Inbox, as the pages near the viewport.
    pub inbox: Pages<Rc<Conversation>>,
    /// How many rows each filter holds, as the last Inbox answer said.
    pub counts: InboxCounts,
    /// Whether the Inbox's order is being held (the pointer is over it, or the keys are in it).
    pub hold: hold::Hold,
    inbox_scroll: UniformListScrollHandle,
    /// The direct conversations of the network home on show.
    pub directs: Vec<Rc<Conversation>>,
    directs_all: bool,
    pub spaces_loaded: bool,
    /// Spaces of a network home flipped from how they start (open when there are few).
    toggled: HashSet<Key>,
    /// The tree on show, flat. Rebuilt on collapse and when its data changes.
    pub rows: Rc<Vec<Row>>,
    tree_scroll: UniformListScrollHandle,
    /// The row the keyboard is on. Moving it opens nothing.
    pub cursor: Option<usize>,
    pub focus: FocusHandle,
    /// Spaces pinned to the rail, in its order.
    pub pins: Vec<Key>,
    history: History<Location>,
    /// Conversations the unread walk has opened since the user last opened one by hand.
    walked: Vec<Key>,
    /// The Space the unread walk is going into, and its direction, until its tree is read.
    walking: Option<(Key, bool)>,
    /// The sidebar's width as last drawn, for fitting a row's title.
    width: std::cell::Cell<f32>,
    /// Drafts typed since their rows were read: chatd announces no change for `SetDraft`.
    drafts: HashMap<Key, String>,
    pub switcher: Option<switcher::Switcher>,
}

impl State {
    pub fn new(cx: &mut Context<Shell>) -> State {
        State {
            mode: Mode::Inbox,
            filter: InboxFilter::All,
            inbox: Pages::default(),
            counts: InboxCounts::default(),
            hold: hold::Hold::default(),
            inbox_scroll: UniformListScrollHandle::new(),
            directs: Vec::new(),
            directs_all: false,
            spaces_loaded: false,
            toggled: HashSet::new(),
            rows: Rc::new(Vec::new()),
            tree_scroll: UniformListScrollHandle::new(),
            cursor: None,
            focus: cx.focus_handle(),
            pins: Vec::new(),
            history: History::default(),
            walked: Vec::new(),
            walking: None,
            width: std::cell::Cell::new(super::layout::SIDEBAR_WIDTH),
            drafts: HashMap::new(),
            switcher: None,
        }
    }

    /// A conversation's row, if any list on show holds it.
    pub fn find(&self, trees: &HashMap<Key, SpaceTree>, key: &Key) -> Option<Conversation> {
        let held = self.inbox.rows().map(|(_, row)| row).chain(&self.directs);
        let in_trees = trees.values().flat_map(|tree| &tree.conversations);
        held.chain(in_trees).find(|row| &row.key == key).map(|row| (**row).clone())
    }
}

/// The glyph of a conversation in a tree. A channel only invited people see shows a lock.
pub fn kind_glyph(kind: ConversationKind, private: bool) -> &'static str {
    match kind {
        ConversationKind::Channel if private => "icons/lock.svg",
        ConversationKind::Direct => "icons/user.svg",
        ConversationKind::Group => "icons/users.svg",
        ConversationKind::Channel => "icons/hash.svg",
        ConversationKind::Announcement => "icons/megaphone.svg",
        ConversationKind::Voice => "icons/volume-2.svg",
        ConversationKind::Forum => "icons/messages-square.svg",
        ConversationKind::Thread => "icons/corner-down-right.svg",
    }
}

fn row_id(prefix: &'static str, key: &Key) -> gpui::ElementId {
    // The id carries the network and account, so two of them may reuse a conversation id.
    (gpui::ElementId::from(prefix), format!("{}/{}/{}", key.network.label(), key.account, key.id)).into()
}

impl Shell {
    /// For the composer: what is typed and unsent in a conversation now, empty when nothing is.
    pub(super) fn draft_changed(&mut self, conversation: &Key, text: &str, cx: &mut Context<Self>) {
        let drafts = &mut self.sidebar_state.drafts;
        if drafts.len() >= DRAFTS_HELD && !drafts.contains_key(conversation) {
            drafts.clear();
        }
        if drafts.insert(conversation.clone(), text.trim().to_owned()).as_deref() != Some(text.trim()) {
            cx.notify();
        }
    }

    /// The draft a row shows: what the composer last said, else what chatd stored.
    fn draft_of<'a>(&'a self, c: &'a Conversation) -> &'a str {
        self.sidebar_state.drafts.get(&c.key).map_or(c.draft.as_str(), String::as_str)
    }

    // Where the sidebar is.

    /// Goes to a mode, remembering where the user was.
    pub(super) fn go(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.sidebar_state.mode == mode {
            // Clicking the lit place scrolls its list to the top.
            self.sidebar_state.inbox_scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.sidebar_state.tree_scroll.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
            return;
        }
        self.remember();
        self.set_mode(mode, cx);
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let state = &mut self.sidebar_state;
        state.mode = mode;
        state.cursor = None;
        state.directs.clear();
        state.directs_all = false;
        state.tree_scroll = UniformListScrollHandle::new();
        state.hold.reset();
        // The rows on show name who is watched again as they draw.
        self.people_state.hub().watch(WATCH, [], cx);
        self.show_mode(cx);
        self.load_directs(cx);
    }

    fn here(&self) -> Location {
        let open = self.snapshot.as_ref().and_then(|s| s.open.as_ref());
        Location {
            mode: self.sidebar_state.mode.clone(),
            open: open.map(|open| (open.key.clone(), open.title.clone())),
        }
    }

    /// Notes where the user is, before they go somewhere else.
    pub(super) fn remember(&mut self) {
        let here = self.here();
        self.sidebar_state.history.leave(here);
    }

    /// `cmd-[` and `cmd-]`: replays where the user has been.
    pub(super) fn travel(&mut self, back: bool, cx: &mut Context<Self>) {
        let here = self.here();
        let Some(there) = self.sidebar_state.history.travel(back, here) else {
            return;
        };
        if self.sidebar_state.mode != there.mode {
            self.set_mode(there.mode, cx);
        }
        match there.open {
            Some((key, title)) if self.open.as_ref() != Some(&key) => self.open_now(key, &title, cx),
            _ => cx.notify(),
        }
    }

    /// Whether back (or forward) leads anywhere: the strip's buttons dim when it does not.
    pub(super) fn can_travel(&self, back: bool) -> bool {
        self.sidebar_state.history.can(back)
    }

    /// `cmd-up`: a Space's network, a network's Inbox.
    pub(super) fn up_level(&mut self, cx: &mut Context<Self>) {
        match self.sidebar_state.mode.clone() {
            Mode::Space(space) => self.go(Mode::Network(space.network), cx),
            Mode::Network(_) | Mode::People => self.go(Mode::Inbox, cx),
            Mode::Inbox => {}
        }
    }

    fn space_open(&self, space: &Space) -> bool {
        let few = self.spaces.iter().filter(|s| s.key.network == space.key.network).count() <= OPEN_SPACES;
        few != self.sidebar_state.toggled.contains(&space.key)
    }

    /// Makes the trees held match what the mode shows: one Space, or the open ones of a network
    /// home. The rest are dropped; chatd holds them.
    pub(super) fn show_mode(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<Key> = match &self.sidebar_state.mode {
            Mode::Space(space) => vec![space.clone()],
            Mode::Network(network) => self
                .spaces
                .iter()
                .filter(|space| space.key.network == *network && self.space_open(space))
                .map(|space| space.key.clone())
                .collect(),
            Mode::Inbox | Mode::People => Vec::new(),
        };
        // One Space's live subscription at a time: the one on show, or in a home with several
        // open, the one the open conversation is in.
        let watched = match &self.sidebar_state.mode {
            Mode::Space(space) => Some(space),
            _ => wanted.iter().find(|space| self.open.as_ref().is_some_and(|open| open.account() == space.account())).or(wanted.first()),
        };
        self.people_state.hub().watch_space(watched, cx);
        self.trees.retain(|space, _| wanted.contains(space));
        for space in wanted {
            if !self.trees.contains_key(&space) {
                self.load_tree(space, cx);
            }
        }
        self.rebuild_rows(cx);
    }

    /// The tree on show as flat rows.
    pub(super) fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        let flat = |space: &Key, depth: u8| match self.trees.get(space).filter(|tree| tree.loaded) {
            Some(tree) => tree::flatten(space, &tree.groups, &tree.conversations, self.open.as_ref(), depth),
            None => vec![Row::Loading { depth }; 6],
        };
        let rows = match &state.mode {
            Mode::Space(space) => flat(space, 0),
            Mode::Network(network) => {
                let mut rows = Vec::new();
                if !state.directs.is_empty() {
                    let shown = if state.directs_all { state.directs.len() } else { tree::DIRECT_SHOWN };
                    rows.push(Row::Label("Direct messages"));
                    rows.extend(
                        state.directs.iter().take(shown).map(|c| Row::Conversation { conversation: c.clone(), depth: 0 }),
                    );
                    if state.directs.len() > shown {
                        rows.push(Row::ShowDirect { more: state.directs.len() - shown });
                    }
                }
                for (index, space) in self.spaces.iter().enumerate().filter(|(_, s)| s.key.network == *network) {
                    let open = self.space_open(space);
                    rows.push(Row::Space { index, open });
                    if open {
                        rows.extend(flat(&space.key, 1));
                    }
                }
                rows
            }
            Mode::Inbox | Mode::People => Vec::new(),
        };
        if *self.sidebar_state.rows != rows {
            self.sidebar_state.rows = Rc::new(rows);
        }
        self.walk_arrived(cx);
        cx.notify();
    }

    // What rows do.

    /// Collapses or opens a Group. chatd remembers it; the network never hears.
    fn collapse(&mut self, space: Key, group: String, collapsed: bool, cx: &mut Context<Self>) {
        if let Some(found) = self.trees.get_mut(&space).and_then(|tree| tree.groups.iter_mut().find(|g| g.id == group)) {
            found.collapsed = collapsed;
        }
        self.rebuild_rows(cx);
        let request = Request::Collapse { space, group, collapsed };
        self.off_thread(cx, move || ask(&request), |_, _, _| {});
    }

    pub(super) fn star(&mut self, conversation: Key, starred: bool, cx: &mut Context<Self>) {
        let request = Request::Star { conversation, starred };
        self.off_thread(cx, move || ask(&request), |this, _, cx| {
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    pub(super) fn pin(&mut self, space: Key, pinned: bool, cx: &mut Context<Self>) {
        let pins = &mut self.sidebar_state.pins;
        pins.retain(|pin| pin != &space);
        if pinned {
            pins.push(space);
        }
        self.save_prefs(cx);
        cx.notify();
    }

    pub(super) fn set_filter(&mut self, filter: InboxFilter, cx: &mut Context<Self>) {
        if self.sidebar_state.filter == filter {
            return;
        }
        let state = &mut self.sidebar_state;
        state.filter = filter;
        state.cursor = None;
        state.inbox.reset();
        state.inbox_scroll = UniformListScrollHandle::new();
        self.load_inbox(vec![0], cx);
        cx.notify();
    }

    /// What Return or a click does to the tree row at `ix`.
    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.sidebar_state.rows.get(ix).cloned() else {
            return;
        };
        match row {
            Row::Conversation { conversation, .. } => self.pick_row(&conversation, cx),
            Row::Space { index, .. } => {
                if let Some(space) = self.spaces.get(index) {
                    self.go(Mode::Space(space.key.clone()), cx);
                }
            }
            Row::Group { space, id, collapsed, .. } => self.collapse(space, id, !collapsed, cx),
            Row::ShowDirect { .. } => {
                self.sidebar_state.directs_all = true;
                self.rebuild_rows(cx);
            }
            Row::Occupant { occupant, .. } => {
                if let Some(person) = self.people_state.hub().person(&occupant.person) {
                    self.open_person(&person, cx);
                }
            }
            Row::Label(_) | Row::More { .. } | Row::Loading { .. } => {}
        }
    }

    fn toggle_space(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(space) = self.spaces.get(index) else {
            return;
        };
        let key = space.key.clone();
        let toggled = &mut self.sidebar_state.toggled;
        if !toggled.remove(&key) {
            toggled.insert(key);
        }
        self.show_mode(cx);
    }

    fn list_len(&self) -> usize {
        match self.sidebar_state.mode {
            Mode::Inbox => self.sidebar_state.inbox.total as usize,
            Mode::People => 0,
            Mode::Network(_) | Mode::Space(_) => self.sidebar_state.rows.len(),
        }
    }

    fn scroll_to(&mut self, ix: usize) {
        let state = &self.sidebar_state;
        let handle = if state.mode == Mode::Inbox { &state.inbox_scroll } else { &state.tree_scroll };
        handle.scroll_to_item(ix, ScrollStrategy::Nearest);
    }

    /// Arrow keys in the list: the cursor moves, nothing opens.
    pub(super) fn move_cursor(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = self.list_len();
        if len == 0 {
            return;
        }
        let state = &mut self.sidebar_state;
        let next = match (state.cursor, down) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(at), true) => (at + 1).min(len - 1),
            (Some(at), false) => at.saturating_sub(1),
        };
        state.cursor = Some(next);
        state.hold.key_moved(Instant::now());
        self.scroll_to(next);
        cx.notify();
    }

    /// Puts the cursor on the open conversation's row, or the first row, when it is nowhere.
    pub(super) fn cursor_to_open(&mut self) {
        let state = &self.sidebar_state;
        if state.cursor.is_some() {
            return;
        }
        let open = self.open.as_ref();
        let at = match state.mode {
            Mode::Inbox => open.and_then(|open| state.inbox.rows().find(|(_, row)| &row.key == open).map(|(ix, _)| ix)),
            _ => self.tree_at(),
        };
        self.sidebar_state.cursor = Some(at.unwrap_or(0));
    }

    /// Return on the cursor's row.
    pub(super) fn open_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.sidebar_state.cursor else {
            return;
        };
        if self.sidebar_state.mode == Mode::Inbox {
            if let Some(row) = self.sidebar_state.inbox.get(ix).cloned() {
                self.pick_row(&row, cx);
            }
        } else {
            self.activate(ix, cx);
        }
    }

    /// Opens the conversation at `ix` of the list on show, as a click on it would.
    #[cfg_attr(not(feature = "gui-shot"), allow(dead_code))]
    pub(super) fn open_listed(&mut self, ix: usize, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        let row = match state.mode {
            Mode::Inbox => state.inbox.get(ix).cloned(),
            _ => state.rows.iter().filter_map(|row| row.conversation()).nth(ix).cloned(),
        };
        if let Some(row) = row {
            self.select(row.key.clone(), &row.title, cx);
        }
    }

    /// Left and right on the cursor's row: collapse or open a Group or a Space.
    pub(super) fn fold_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        match state.cursor.and_then(|ix| state.rows.get(ix)).cloned() {
            Some(Row::Group { space, id, collapsed, .. }) if collapsed == open => self.collapse(space, id, !open, cx),
            Some(Row::Space { index, open: is_open }) if is_open != open => self.toggle_space(index, cx),
            _ => {}
        }
    }

    /// Opens a row the user chose by hand: the unread walk starts over from there.
    fn pick_row(&mut self, row: &Conversation, cx: &mut Context<Self>) {
        self.sidebar_state.walked.clear();
        self.select(row.key.clone(), &row.title, cx);
    }

    /// `alt-down` and `alt-up` open the next or previous row of the list on show.
    pub(super) fn walk(&mut self, down: bool, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        let found = if state.mode == Mode::Inbox {
            let at = self.open.as_ref().and_then(|open| state.inbox.rows().find(|(_, row)| &row.key == open)).map(|(ix, _)| ix);
            let mut rows = state.inbox.rows();
            let hit = match (at.or(state.cursor), down) {
                (None, _) => rows.next(),
                (Some(at), true) => rows.find(|(ix, _)| *ix > at),
                (Some(at), false) => rows.take_while(|(ix, _)| *ix < at).last(),
            };
            hit.map(|(ix, row)| (ix, row.clone()))
        } else {
            tree::walk(&state.rows, self.tree_at(), down, false).and_then(|ix| Some((ix, state.rows[ix].conversation()?.clone())))
        };
        if let Some((ix, row)) = found {
            self.sidebar_state.cursor = Some(ix);
            self.scroll_to(ix);
            self.pick_row(&row, cx);
        }
    }

    /// Where the open conversation (or else the cursor) is in the tree on show.
    fn tree_at(&self) -> Option<usize> {
        let state = &self.sidebar_state;
        let open = self.open.as_ref().and_then(|open| state.rows.iter().position(|row| row.conversation().is_some_and(|c| &c.key == open)));
        open.or(state.cursor)
    }

    /// The stops of the unread walk: the Inbox, then every Space in the rail's network order.
    fn walk_stops(&self) -> Vec<(Mode, bool)> {
        let counts = self.sidebar_state.counts;
        let inbox = (Mode::Inbox, counts.unread > 0 || counts.mentions > 0);
        let spaces = super::layout::NETWORK_ORDER.into_iter().flat_map(|network| {
            self.spaces.iter().filter(move |space| space.key.network == network)
        });
        std::iter::once(inbox)
            .chain(spaces.map(|space| (Mode::Space(space.key.clone()), space.attention > 0 || space.unread)))
            .collect()
    }

    /// `alt-shift-down` and `alt-shift-up`: the next or previous unread conversation. Past the
    /// end of the list on show it goes on into the next place with something unread, the
    /// sidebar following, until nothing unread is left.
    pub(super) fn walk_unread(&mut self, down: bool, cx: &mut Context<Self>) {
        self.sidebar_state.walking = None;
        match self.sidebar_state.mode.clone() {
            Mode::Inbox => self.walk_inbox(down, true, cx),
            Mode::Space(_) | Mode::Network(_) => {
                let rows = self.sidebar_state.rows.clone();
                match self.tree_unread(&rows, self.tree_at(), down) {
                    Some(ix) => self.walk_to_row(ix, cx),
                    None => self.walk_on(down, cx),
                }
            }
            Mode::People => self.walk_on(down, cx),
        }
    }

    /// The next unread row of a tree that this walk has not already opened.
    fn tree_unread(&self, rows: &[Row], from: Option<usize>, down: bool) -> Option<usize> {
        let mut at = from;
        loop {
            let ix = tree::walk(rows, at, down, true)?;
            let fresh = rows[ix].conversation().is_some_and(|c| !self.sidebar_state.walked.contains(&c.key));
            if fresh {
                return Some(ix);
            }
            at = Some(ix);
        }
    }

    fn walk_to_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.sidebar_state.rows.get(ix).and_then(|row| row.conversation()).cloned() else {
            return;
        };
        self.sidebar_state.cursor = Some(ix);
        self.scroll_to(ix);
        self.walk_open(&row, cx);
    }

    fn walk_open(&mut self, row: &Conversation, cx: &mut Context<Self>) {
        let walked = &mut self.sidebar_state.walked;
        if walked.len() >= PAGE_LIMIT as usize {
            walked.clear();
        }
        walked.push(row.key.clone());
        self.select(row.key.clone(), &row.title, cx);
    }

    /// The Inbox's unread rows are read from the store, not from the pages held: the next one
    /// may be far below the viewport. `onward`: with none left here, go on to the next stop.
    fn walk_inbox(&mut self, down: bool, onward: bool, cx: &mut Context<Self>) {
        let request = Request::Inbox { network: None, filter: InboxFilter::Unread, offset: 0, limit: PAGE_LIMIT };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            let Ok(Response::Conversations { conversations, .. }) = answer else {
                return;
            };
            let seen: Vec<bool> = conversations.iter().map(|c| this.sidebar_state.walked.contains(&c.key)).collect();
            let at = this.open.as_ref().and_then(|open| conversations.iter().position(|c| &c.key == open));
            match walk::step(&seen, at, down) {
                Some(ix) => {
                    if this.sidebar_state.mode != Mode::Inbox {
                        this.remember();
                        this.set_mode(Mode::Inbox, cx);
                    }
                    let held = this.sidebar_state.inbox.rows().find(|(_, row)| row.key == conversations[ix].key).map(|(at, _)| at);
                    this.sidebar_state.cursor = held;
                    if let Some(held) = held {
                        this.scroll_to(held);
                    }
                    this.walk_open(&conversations[ix], cx);
                }
                None if onward => this.walk_on(down, cx),
                None => {}
            }
        });
    }

    /// Nothing unread is left in the list on show: on to the next stop that has some.
    fn walk_on(&mut self, down: bool, cx: &mut Context<Self>) {
        let stops = self.walk_stops();
        let here = match &self.sidebar_state.mode {
            Mode::Space(_) => stops.iter().position(|(mode, _)| *mode == self.sidebar_state.mode).unwrap_or(0),
            // A network home walks on from the Space before its first, so that one comes next.
            Mode::Network(network) => stops
                .iter()
                .position(|(mode, _)| matches!(mode, Mode::Space(space) if space.network == *network))
                .map_or(0, |first| first.saturating_sub(usize::from(down))),
            Mode::Inbox | Mode::People => 0,
        };
        let unread: Vec<bool> = stops.iter().map(|(_, unread)| *unread).collect();
        // The nearest stop only: if it turns out to hold nothing new, the next key goes on.
        let Some(next) = walk::route(&unread, here, down).into_iter().find(|stop| stops[*stop].0 != self.sidebar_state.mode) else {
            return;
        };
        match stops[next].0.clone() {
            Mode::Space(space) => {
                self.remember();
                self.sidebar_state.walking = Some((space.clone(), down));
                self.set_mode(Mode::Space(space), cx);
            }
            _ => self.walk_inbox(down, false, cx),
        }
    }

    /// The tree the walk went into has been read: open its first (or last) unread row.
    fn walk_arrived(&mut self, cx: &mut Context<Self>) {
        let Some((space, down)) = self.sidebar_state.walking.clone() else {
            return;
        };
        if self.sidebar_state.mode != Mode::Space(space.clone()) {
            self.sidebar_state.walking = None;
            return;
        }
        if !self.trees.get(&space).is_some_and(|tree| tree.loaded) {
            return;
        }
        self.sidebar_state.walking = None;
        let rows = self.sidebar_state.rows.clone();
        if let Some(ix) = self.tree_unread(&rows, None, down) {
            self.walk_to_row(ix, cx);
        }
    }

    // Drawing.

    /// The avatar of a conversation: a person round with their presence, a group as two of
    /// its people, a channel as its glyph on its Space's tint.
    pub(super) fn conversation_avatar(&self, c: &Conversation, size: f32, selected: bool) -> Div {
        let picture = self.media_path(&c.key, c.avatar.as_ref());
        let surface = theme::row_surface(selected);
        match c.kind {
            ConversationKind::Direct => {
                let avatar = Avatar::new(&c.key, &c.title, size).image_at(picture).build();
                let shape = c.peer.as_ref().and_then(|peer| {
                    let person = Key { id: peer.clone(), ..c.key.clone() };
                    if self.in_call(&person) {
                        Some(presence::Shape::InCall)
                    } else {
                        presence::Shape::of(self.presence_of(&person))
                    }
                });
                avatar.children(shape.and_then(|shape| presence::dot(shape, size, &presence::Ring::on(surface))))
            }
            ConversationKind::Group if picture.is_none() && size >= 32. => super::components::pair(&c.key, &c.title, size, surface),
            ConversationKind::Group if picture.is_none() => Avatar::channel("icons/users.svg", size).shape(Shape::Person).build(),
            ConversationKind::Group => Avatar::new(&c.key, &c.title, size).image_at(picture).build(),
            kind if picture.is_none() => match &c.space {
                Some(space) => Avatar::channel_in(&Key { id: space.clone(), ..c.key.clone() }, kind_glyph(kind, c.private), size).build(),
                None => Avatar::channel(kind_glyph(kind, c.private), size).build(),
            },
            _ => Avatar::new(&c.key, &c.title, size).shape(Shape::Place).image_at(picture).build(),
        }
    }

    /// How wide `text` draws at the interface's sizes, for fitting a row's title.
    fn text_width(window: &mut Window, text: &str, size: f32, weight: gpui::FontWeight) -> f32 {
        // The font the rows are drawn in: the window's own, as the list inherits it.
        let font = gpui::Font { weight, ..window.text_style().font() };
        let run = gpui::TextRun { len: text.len(), font, color: gpui::Hsla::default(), background_color: None, underline: None, strikethrough: None };
        f32::from(window.text_system().shape_line(text.to_owned().into(), px(size), &[run], None).width)
    }

    /// The people whose presence these rows draw: the other side of each direct conversation.
    fn watch_peers<'a>(&self, rows: impl Iterator<Item = &'a Rc<Conversation>>, cx: &mut Context<Self>) {
        let peers = rows.filter_map(|c| Some(Key { id: c.peer.clone()?, ..c.key.clone() }));
        self.people_state.hub().watch(WATCH, peers, cx);
    }

    /// What stands in a row's badge slot: a send that failed there outranks what is unread.
    fn row_mark(&self, c: &Conversation) -> Option<Div> {
        if self.send_failed_in(&c.key) {
            let alert = glyph("icons/circle-alert.svg", 14., theme::danger());
            return Some(div().flex_none().size(px(16.)).flex().items_center().justify_center().child(alert));
        }
        row_badge(c)
    }

    /// The files of these rows that are not on disk and have not been asked for.
    fn missing<'a>(&self, rows: impl Iterator<Item = (&'a Key, Option<&'a MediaFile>)>) -> Vec<(Key, String)> {
        rows.filter_map(|(key, file)| {
            let file = file?;
            self.media_missing(key, file).then(|| (key.clone(), file.id.clone()))
        })
        .collect()
    }

    /// One row of the Inbox: two lines, 48 tall.
    fn inbox_row(&self, ix: usize, c: &Rc<Conversation>, focused: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.open.as_ref() == Some(&c.key);
        let tone = row_tone(c, selected, false);
        let typing = if c.kind.is_direct() { self.slots.typing.and_then(|typing| typing(self, &c.key)) } else { None };
        let draft = self.draft_of(c);
        let preview = match typing {
            Some(words) => div().truncate().text_color(theme::accent_text()).child(words),
            // Unread wins: the draft is still there when the conversation opens.
            None if !draft.is_empty() && c.badge() == Badge::None => div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(glyph("icons/pencil.svg", 12., theme::text_soft()))
                .child(div().flex_none().text_color(theme::text_soft()).child("Draft:"))
                .child(div().min_w_0().truncate().text_color(theme::text_faint()).child(draft.to_owned())),
            None => div().truncate().text_color(tone.preview).child(c.preview.clone()),
        };
        // The last pinned row draws the line that ends the pinned ones.
        let last_pin = c.starred && self.sidebar_state.inbox.get(ix + 1).is_some_and(|next| !next.starred);
        let several = self.snapshot.as_ref().is_some_and(|snapshot| snapshot.accounts_of(c.key.network) > 1);
        let stamp = time::stamp(now(), c.updated);
        let title = label::title(c);
        // The Space gives up width before the title does, so its share is set, not left to flex.
        let space = label::place(c, several).map(|space| {
            const SEPARATOR: &str = " › ";
            let fixed = 2. * theme::INSET + 16. + 32. + 10. + 14. + 6. + 6. + if c.starred { 18. } else { 0. };
            let room = self.sidebar_state.width.get()
                - fixed
                - Self::text_width(window, &stamp, 11., theme::MEDIUM)
                - Self::text_width(window, SEPARATOR, 13., tone.weight);
            let needs = (Self::text_width(window, space, 13., tone.weight).ceil(), Self::text_width(window, &title, 13., tone.weight).ceil());
            // Two points of slack: shaping a run alone and inside a row differ by a hair.
            let (width, _) = label::fit(needs.0, needs.1 + 2., room.floor());
            div()
                .flex_none()
                .flex()
                .child(div().w(px(width)).truncate().child(space.to_owned()))
                .child(div().flex_none().whitespace_nowrap().child(SEPARATOR))
        });
        let (menu_row, picked) = (c.clone(), c.clone());
        let content = div()
            .gap(px(10.))
            .child(self.conversation_avatar(c, 32., selected))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            // The Inbox mixes networks, so each row says which it is from.
                            .child(network_icon(c.key.network, 14.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .type_ui()
                                    .font_weight(tone.weight)
                                    .text_color(tone.title)
                                    .children(space)
                                    .child(div().flex_1().min_w_0().truncate().child(title)),
                            )
                            .when(c.starred, |line| line.child(glyph("icons/pin.svg", 12., tone.time)))
                            .child(div().flex_none().type_micro().text_color(tone.time).child(stamp)),
                    )
                    .child(
                        div()
                            .h(px(16.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(preview.flex_1().min_w_0().type_small())
                            .children(self.row_mark(c)),
                    ),
            );
        list_row(row_id("inbox", &c.key), theme::ROW_TWO, selected, focused, content)
            .relative()
            .when(last_pin, |row| {
                row.child(div().absolute().bottom_0().left(px(16.)).right(px(16.)).h(px(1.)).bg(theme::hairline()))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.pick_row(&picked, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.open_menu(Menu::Row(menu_row.clone(), event.position), cx);
                }),
            )
    }

    /// A placeholder in a row's final geometry, for a page still on its way.
    fn inbox_placeholder(ix: usize) -> Stateful<Div> {
        let widths = [120., 90., 140., 100., 130., 80.];
        let content = div()
            .gap(px(10.))
            .child(div().size(px(32.)).rounded(px(16.)).bg(theme::hover()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(skeleton(widths[ix % widths.len()], 10.))
                    .child(skeleton(widths[(ix + 2) % widths.len()] + 40., 8.)),
            );
        list_row(("inbox-wait", ix), theme::ROW_TWO, false, false, content).cursor_default()
    }

    /// One row of a tree: 28 tall, whatever it is.
    fn tree_row(&self, ix: usize, row: &Row, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        // A spacer rather than padding: the row sets its own.
        let indent = |depth: u8| div().flex_none().w(px(12. * f32::from(depth)));
        let label = |words: SharedString| div().flex_1().min_w_0().truncate().type_micro().text_color(theme::text_faint()).child(words);
        match row {
            Row::Label(words) => list_row(("label", ix), theme::ROW_ONE, false, false, div().child(label((*words).into()))).cursor_default(),
            Row::Loading { depth } => {
                let widths = [0.60, 0.45, 0.70, 0.50, 0.65, 0.40];
                let bar = div().h(px(8.)).w(gpui::relative(widths[ix % widths.len()])).rounded(px(theme::R_CHIP)).bg(theme::raised());
                list_row(("loading", ix), theme::ROW_ONE, false, false, div().child(indent(*depth)).child(bar)).cursor_default()
            }
            Row::ShowDirect { more } => {
                let content = div()
                    .gap(px(8.))
                    .child(glyph("icons/chevron-down.svg", 14., theme::text_faint()))
                    .child(div().type_ui().text_color(theme::text_faint()).child(format!("Show {more} more")));
                list_row("show-direct", theme::ROW_ONE, false, focused, content)
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(ix, cx)))
            }
            Row::More { words, depth } => {
                let content = div().child(indent(*depth)).child(div().pl(px(28.)).type_small().text_color(theme::text_faint()).child(words.clone()));
                list_row(("more", ix), theme::ROW_ONE, false, false, content).cursor_default()
            }
            Row::Occupant { occupant, depth } => {
                let content = div()
                    .child(indent(*depth))
                    .gap(px(8.))
                    .child(Avatar::new(&occupant.person, &occupant.name, 20.).image_at(self.person_picture(&occupant.person, cx)).build())
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child(occupant.name.clone()))
                    .when(occupant.streaming, |row| row.child(glyph("icons/monitor-up.svg", 14., theme::text_faint())))
                    .when(occupant.muted && !occupant.deafened, |row| row.child(glyph("icons/mic-off.svg", 14., theme::danger())))
                    .when(occupant.deafened, |row| row.child(glyph("icons/headphone-off.svg", 14., theme::danger())));
                list_row(row_id("occupant", &occupant.person), theme::ROW_ONE, false, false, content).cursor_default()
            }
            Row::Group { id, name, collapsed, rollup, depth, .. } => {
                let chevron = if *collapsed { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" };
                let lit = *collapsed && (rollup.attention > 0 || rollup.unread);
                let color = if lit { theme::text() } else { theme::text_faint() };
                let badge = match (rollup.attention, rollup.quiet, rollup.unread) {
                    (0, _, false) => None,
                    (0, _, true) => Some(badge_dot(6., theme::unread())),
                    (count, true, _) => Some(badge_quiet(count)),
                    (count, false, _) => Some(badge_mention(count)),
                };
                let content = div()
                    .child(indent(*depth))
                    .gap(px(4.))
                    .child(glyph(chevron, 14., color))
                    .child(div().flex_1().min_w_0().truncate().type_micro().text_color(color).child(name.clone()))
                    .children(badge);
                list_row((gpui::ElementId::from("group"), id.clone()), theme::ROW_ONE, false, focused, content)
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(ix, cx)))
            }
            Row::Space { index, open } => {
                let Some(space) = self.spaces.get(*index) else {
                    return list_row(("space-gone", ix), theme::ROW_ONE, false, false, div());
                };
                let chevron = if *open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" };
                let lit = space.attention > 0 || space.unread;
                let index = *index;
                let content = div()
                    .gap(px(6.))
                    .child(
                        // The chevron opens the Space in place; the name focuses the sidebar on it.
                        div()
                            .id(("space-fold", index))
                            .size(px(20.))
                            .ml(px(-4.))
                            .rounded(px(theme::R_CHIP))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme::selected()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_space(index, cx);
                            }))
                            .child(glyph(chevron, 14., theme::text_soft())),
                    )
                    .child(
                        Avatar::new(&space.key, &space.name, 20.)
                            .shape(Shape::Place)
                            .image_at(self.media_path(&space.key, space.icon.as_ref()))
                            .build(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_ui()
                            .font_weight(if lit { theme::SEMIBOLD } else { theme::MEDIUM })
                            .text_color(theme::text())
                            .child(space.name.clone()),
                    )
                    .children(rollup_badge(space.attention, space.unread));
                list_row(row_id("space", &space.key), theme::ROW_ONE, false, focused, content)
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(ix, cx)))
            }
            Row::Conversation { conversation: c, depth } => {
                let selected = self.open.as_ref() == Some(&c.key);
                let tone = row_tone(c, selected, true);
                let thread = c.kind == ConversationKind::Thread;
                let home = matches!(self.sidebar_state.mode, Mode::Network(_));
                let several = self.snapshot.as_ref().filter(|_| home && *depth == 0 && c.kind.is_direct());
                // In a home that holds several accounts, a direct row says whose it is.
                let whose = several.filter(|snapshot| snapshot.accounts_of(c.key.network) > 1).and_then(|snapshot| {
                    Some(snapshot.account(&c.key.account())?.name.clone())
                });
                let inside = c.occupants.as_ref().map(|inside| inside.total).filter(|total| *total > 0);
                let leading = match c.kind {
                    ConversationKind::Direct => self.conversation_avatar(c, 20., selected).into_any_element(),
                    kind => div()
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(glyph(kind_glyph(kind, c.private), if thread { 14. } else { 16. }, if c.muted { tone.title } else { theme::text_soft() }))
                        .into_any_element(),
                };
                let (picked, menu_row) = (c.clone(), c.clone());
                let content = div()
                    .child(indent(*depth))
                    .gap(px(6.))
                    .child(leading)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_ui()
                            .font_weight(tone.weight)
                            .text_color(tone.title)
                            .child(c.title.trim_start_matches('#').to_owned()),
                    )
                    .children(whose.map(|name| div().flex_none().max_w(px(96.)).truncate().type_small().text_color(theme::text_faint()).child(name)))
                    .children(inside.map(|total| div().flex_none().type_small().text_color(theme::text_faint()).child(total.to_string())))
                    .children(self.row_mark(c));
                list_row(row_id("tree", &c.key), theme::ROW_ONE, selected, focused, content)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_row(&picked, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.open_menu(Menu::Row(menu_row.clone(), event.position), cx);
                        }),
                    )
            }
        }
    }

    /// The Inbox's rows on screen. Seeing them is what asks for the pages around them.
    fn inbox_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let pages = self.sidebar_state.inbox.show(range.clone());
        if !pages.is_empty() {
            self.load_inbox(pages, cx);
        }
        let state = &self.sidebar_state;
        let visible = range.clone().filter_map(|ix| state.inbox.get(ix));
        let missing = self.missing(visible.clone().map(|row| (&row.key, row.avatar.as_ref())));
        self.watch_peers(visible, cx);
        self.want_media(missing, cx);
        // The shell's own focus is the list's too: arrows move the cursor when no field has them.
        let focused = self.sidebar_state.focus.is_focused(window) || self.focus.is_focused(window);
        let cursor = self.sidebar_state.cursor.filter(|_| focused);
        range
            .map(|ix| match self.sidebar_state.inbox.get(ix).cloned() {
                Some(row) => self.inbox_row(ix, &row, cursor == Some(ix), window, cx),
                None => Self::inbox_placeholder(ix),
            })
            .collect()
    }

    fn tree_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let rows = self.sidebar_state.rows.clone();
        let visible = rows.get(range.start.min(rows.len())..range.end.min(rows.len())).unwrap_or_default();
        let missing = self.missing(visible.iter().filter_map(|row| match row {
            Row::Conversation { conversation, .. } => Some((&conversation.key, conversation.avatar.as_ref())),
            Row::Space { index, .. } => self.spaces.get(*index).map(|space| (&space.key, space.icon.as_ref())),
            _ => None,
        }));
        self.want_media(missing, cx);
        self.watch_peers(visible.iter().filter_map(|row| row.conversation()), cx);
        // The shell's own focus is the list's too: arrows move the cursor when no field has them.
        let focused = self.sidebar_state.focus.is_focused(window) || self.focus.is_focused(window);
        let cursor = self.sidebar_state.cursor.filter(|_| focused);
        range.filter_map(|ix| Some(self.tree_row(ix, rows.get(ix)?, cursor == Some(ix), cx))).collect()
    }

    /// What a pane says while a link is not normal (`06-live-signals.md`, section 5). Healthy is
    /// silent, and a pane speaks only for the accounts it shows: a workspace's tree says nothing
    /// of another workspace.
    fn banners(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<Div> {
        let mode = &self.sidebar_state.mode;
        let mut banners = Vec::new();
        for account in &snapshot.accounts {
            let network = account.key.network;
            let (here, mixed) = match mode {
                Mode::Space(space) => (space.account() == account.key, false),
                Mode::Network(shown) => (*shown == network, false),
                Mode::Inbox => (true, true),
                Mode::People => (false, false),
            };
            if !here {
                continue;
            }
            let words = banner_words(network, snapshot.accounts_of(network) > 1, account, mixed, || {
                self.trouble.get(&account.key).map(|since| since.elapsed()).unwrap_or_default() >= TROUBLE_SHOWN
            });
            match words {
                Some((Tone::Danger, words)) => banners.push(banner(Tone::Danger, words).child(
                    banner_action(("sign-in-again", banners.len()), "Sign in again")
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx))),
                )),
                Some((tone, words)) => banners.push(banner(tone, words)),
                None => {}
            }
        }
        banners
    }

    fn title_strip(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Div {
        let strip = drags_window(div()).h(px(TOP_BAR)).flex_none().pl(px(theme::PANE_PAD)).pr(px(12.)).flex().items_center().gap(px(8.));
        let quiet = |button: Stateful<Div>| button.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        match &self.sidebar_state.mode {
            Mode::Inbox => {
                let filter = self.sidebar_state.filter;
                let choice = |id: &'static str, label: String, value: InboxFilter| {
                    quiet(segment(id, label, filter == value)).on_click(cx.listener(move |this, _, _, cx| this.set_filter(value, cx)))
                };
                // Each chip says how many rows it holds; the numbers go when there is no room.
                let counts = self.sidebar_state.counts;
                let roomy = self.sidebar_state.width.get() >= 300.;
                let counted = |label: &str, count: u32| match (roomy, count) {
                    (false, _) | (_, 0) => label.to_owned(),
                    (true, count) => format!("{label} {}", if count > 99 { "99+".to_owned() } else { count.to_string() }),
                };
                strip.child(pane_title("Inbox")).child(
                    segmented()
                        .child(choice("filter-all", "All".to_owned(), InboxFilter::All))
                        .child(choice("filter-unread", counted("Unread", counts.unread), InboxFilter::Unread))
                        .child(choice("filter-mentions", counted("Mentions", counts.mentions), InboxFilter::Mentions)),
                )
            }
            Mode::People => strip.child(pane_title("People")),
            Mode::Network(network) => {
                let network = *network;
                let _ = snapshot;
                strip.child(network_icon(network, 20.)).child(pane_title(network.label())).child(
                    quiet(icon_button("network-menu", "icons/ellipsis.svg", false))
                        .tooltip(tip("Accounts", None))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_menu(Menu::Network(network), cx))),
                )
            }
            Mode::Space(key) => {
                let space = self.spaces.iter().find(|space| &space.key == key);
                let name = space.map_or_else(String::new, |space| space.name.clone());
                let (up, menu_key) = (key.network, key.clone());
                strip
                    .pl(px(10.))
                    .child(
                        quiet(icon_button("space-up", "icons/chevron-left.svg", false))
                            .tooltip(tip(format!("Back to {}", up.label()), Some("⌘↑")))
                            .on_click(cx.listener(move |this, _, _, cx| this.go(Mode::Network(up), cx))),
                    )
                    .child(
                        Avatar::new(key, &name, 24.)
                            .shape(Shape::Place)
                            .image_at(space.and_then(|space| self.media_path(&space.key, space.icon.as_ref())))
                            .build(),
                    )
                    .child(pane_title(name))
                    .child(
                        quiet(icon_button("space-menu", "icons/ellipsis.svg", false))
                            .tooltip(tip("More", None))
                            .on_click(cx.listener(move |this, _, _, cx| this.open_menu(Menu::Space(menu_key.clone()), cx))),
                    )
            }
        }
    }

    /// The button drawn as a field that opens the switcher. It does not filter: one field means
    /// one thing.
    fn jump_strip(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(36.))
            .flex_none()
            .px(px(12.))
            .flex()
            .items_start()
            .gap(px(6.))
            .child(
                div()
                    .id("jump")
                    .flex_1()
                    .h(px(theme::CONTROL))
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::field())
                    .border_1()
                    .border_color(theme::border())
                    .hover(|style| style.border_color(theme::border_strong()))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| this.open_switcher("", window, cx)))
                    .child(glyph("icons/search.svg", 14., theme::text_faint()))
                    .child(div().flex_1().type_ui().text_color(theme::text_faint()).child("Jump to…"))
                    .child(keycap("⌘K")),
            )
            .child(
                icon_button("compose", "icons/square-pen.svg", false)
                    .tooltip(tip("New message", None))
                    .on_click(cx.listener(|this, _, window, cx| this.open_switcher("@", window, cx))),
            )
    }

    fn list(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.sidebar_state;
        let empty = |icon, title: &str, detail: &str| empty_state(icon, title, detail).px(px(24.)).into_any_element();
        match &state.mode {
            Mode::People => unreachable!("the People frame is drawn by its slot"),
            Mode::Inbox if state.inbox.fresh => uniform_list("inbox-wait", 6, |range: Range<usize>, _, _| {
                range.map(Self::inbox_placeholder).collect()
            })
            .size_full()
            .into_any_element(),
            Mode::Inbox if state.inbox.total == 0 => match state.filter {
                InboxFilter::All | InboxFilter::Directs => empty("icons/inbox.svg", "No conversations yet", "They appear here as soon as the network sends them."),
                InboxFilter::Unread => empty("icons/check.svg", "Nothing unread", "You have seen everything."),
                InboxFilter::Mentions => empty("icons/at-sign.svg", "No mentions", "Nobody is waiting on you."),
            },
            Mode::Inbox => div()
                .id("inbox-hold")
                .size_full()
                // Under the pointer the order holds; what arrived meanwhile is sorted in on leave.
                .on_hover(cx.listener(|this, over: &bool, _, cx| {
                    if this.sidebar_state.hold.hover(*over, Instant::now()) {
                        this.reorder_inbox(cx);
                    }
                }))
                .child(
                    uniform_list("inbox", state.inbox.total as usize, cx.processor(Self::inbox_rows))
                        .track_scroll(&state.inbox_scroll)
                        .size_full(),
                )
                .into_any_element(),
            Mode::Network(network) if state.rows.is_empty() && state.spaces_loaded => {
                let name = network.label();
                match snapshot.link(*network) {
                    Link::Offline => empty("icons/wifi-off.svg", &format!("{name} is offline"), "What was saved shows up here once it connects."),
                    _ => empty("icons/messages-square.svg", "Nothing here yet", &format!("Servers and workspaces you join in {name} show up here.")),
                }
            }
            Mode::Space(_) if state.rows.is_empty() => empty("icons/hash.svg", "No channels you can read", "Channels you are allowed to see show up here."),
            Mode::Network(_) | Mode::Space(_) => uniform_list("tree", state.rows.len(), cx.processor(Self::tree_rows))
                .track_scroll(&state.tree_scroll)
                .size_full()
                .into_any_element(),
        }
    }

    pub(super) fn sidebar(&self, snapshot: &Snapshot, width: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let people = self.sidebar_state.mode == Mode::People;
        let body = if people {
            let drawn = self.slots.people.and_then(|draw| draw(self, window, cx));
            drawn.unwrap_or_else(|| empty_state("icons/users.svg", "People", "Friends and contacts show up here.").px(px(24.)).into_any_element())
        } else {
            self.list(snapshot, cx)
        };
        let banners = self.banners(snapshot, cx);
        self.sidebar_state.width.set(width);
        div()
            .relative()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::hairline())
            .child(self.title_strip(snapshot, cx))
            .when(!people, |sidebar| sidebar.child(self.jump_strip(cx)))
            .when(!banners.is_empty(), |sidebar| {
                sidebar.child(div().flex_none().px(px(12.)).pb(px(8.)).flex().flex_col().gap(px(4.)).children(banners))
            })
            .child(
                div()
                    .id("sidebar-list")
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.sidebar_state.focus)
                    .key_context("Sidebar")
                    .child(body),
            )
            .children(self.slots.call_dock.and_then(|draw| draw(self, window, cx)))
    }
}

/// What a pane says about one account's link, and in which tone. `several`: its network has
/// more than one account, so the account is named. `mixed`: the pane lists every account (the
/// Inbox), where only what needs the user is said. `long` answers whether the link has been
/// down long enough to mention.
fn banner_words(
    network: Network,
    several: bool,
    status: &protocol::AccountStatus,
    mixed: bool,
    long: impl Fn() -> bool,
) -> Option<(Tone, String)> {
    let (name, account) = (network.label(), status.name.as_str());
    // The network's own words for what it wants done in its app.
    if !status.required.is_empty() {
        return Some((Tone::Attention, status.required.clone()));
    }
    match status.link {
        Link::Expired if several => Some((Tone::Danger, format!("{name} signed you out of {account}"))),
        Link::Expired => Some((Tone::Danger, format!("{name} signed you out"))),
        Link::Offline if !mixed && long() => Some(match (status.reconnecting, several) {
            (true, false) => (Tone::Warning, "Reconnecting…".to_owned()),
            (true, true) => (Tone::Warning, format!("Reconnecting to {account}…")),
            (false, false) => (Tone::Warning, format!("Can't reach {name}")),
            (false, true) => (Tone::Warning, format!("Can't reach {account}")),
        }),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use protocol::{AccountKey, AccountStatus, CallSupport, Presence};

    pub fn row(id: &str, kind: ConversationKind) -> Conversation {
        Conversation {
            key: Key { network: Network::Discord, account: "me".into(), id: id.into() },
            kind,
            title: id.into(),
            space: None,
            space_title: String::new(),
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
            call: CallSupport::None,
            occupants: None,
            read_only: false,
            private: false,
            draft: String::new(),
        }
    }

    fn status(link: Link, reconnecting: bool, required: &str) -> AccountStatus {
        AccountStatus {
            key: AccountKey { network: Network::Slack, account: "T1".into() },
            name: "Fernhill".into(),
            link,
            detail: String::new(),
            me: String::new(),
            attention: 0,
            unread: false,
            reconnecting,
            required: required.into(),
            avatar: None,
            presence: Presence::Unknown,
            status: String::new(),
        }
    }

    #[test]
    fn a_pane_says_what_is_wrong_with_a_link_in_the_specs_words_and_nothing_when_healthy() {
        let say = |status: &AccountStatus, several, mixed, long| {
            banner_words(Network::Slack, several, status, mixed, || long).map(|(_, words)| words)
        };
        assert_eq!(say(&status(Link::Connected, false, ""), false, false, true), None);
        assert_eq!(say(&status(Link::Expired, false, ""), false, true, false).as_deref(), Some("Slack signed you out"));
        assert_eq!(say(&status(Link::Expired, false, ""), true, false, false).as_deref(), Some("Slack signed you out of Fernhill"));
        // The first seconds of any connect say nothing; then it depends on whether chatd is trying.
        assert_eq!(say(&status(Link::Offline, true, ""), false, false, false), None);
        assert_eq!(say(&status(Link::Offline, true, ""), false, false, true).as_deref(), Some("Reconnecting…"));
        assert_eq!(say(&status(Link::Offline, true, ""), true, false, true).as_deref(), Some("Reconnecting to Fernhill…"));
        assert_eq!(say(&status(Link::Offline, false, ""), false, false, true).as_deref(), Some("Can't reach Slack"));
        // The Inbox mixes accounts: only what needs me is said there.
        assert_eq!(say(&status(Link::Offline, false, ""), false, true, true), None);
        let wanted = "Discord wants you to verify your account in its app.";
        assert_eq!(say(&status(Link::Connected, false, wanted), false, true, false).as_deref(), Some(wanted));
    }
}
