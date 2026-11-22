//! The sidebar: the same three parts in every mode (title strip, jump strip, one list). The
//! Inbox is by recency across networks, in day groups; a network's home is its own list
//! (Discord's DMs and servers, Slack's workspace, Telegram's chats, Teams' chats and teams); a
//! Space is its groups and channels in the network's own order; People is its own view in
//! this frame.

pub mod chat;
pub mod history;
pub mod hold;
pub mod home;
mod home_view;
pub mod inbox;
mod inbox_view;
pub mod pages;
pub mod rank;
#[cfg(feature = "gui-shot")]
mod shot;
pub mod switcher;
pub mod tree;
pub mod walk;

use super::components::{
    badge_dot, badge_mention, banner, banner_action, button, empty_state, glyph, icon_button, keycap, list_row,
    network_icon, pane_title, rollup_badge, row_badge, row_tone, segment, segmented, skeleton, tip, Avatar, ButtonKind,
    Shape, Tone,
};
use super::chrome::{self, End};
use super::layout::{drags_window, Menu};
use super::state::{Shell, Snapshot, SpaceTree, TROUBLE_SHOWN};
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{
    div, prelude::*, px, uniform_list, Context, Div, Entity, FocusHandle, FontWeight, ListAlignment, ListState,
    MouseButton, ScrollStrategy, SharedString, Stateful, UniformListScrollHandle, Window,
};
use history::History;
use pages::Pages;
use protocol::{
    AccountKey, Badge, CallSupport, Conversation, ConversationKind, InboxCounts, InboxFilter, Key, Link, Network,
    Occupants, Request, Response, PAGE_LIMIT,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;
use tree::Row;

/// The name this view watches people and rows under (see `people::Hub::watch`).
const WATCH: &str = "sidebar";
/// Drafts remembered ahead of their rows being read again.
const DRAFTS_HELD: usize = 256;

/// The id to ask before on the next mention page. `None` when this page is the last.
/// A page that was filtered down to nothing still moves on, using the raw page's last id.
fn mention_cursor(items: &[protocol::MentionHit], more: bool, cursor: Option<String>) -> Option<String> {
    if !more {
        return None;
    }
    cursor.filter(|id| !id.is_empty()).or_else(|| items.last().map(|item| item.message_id.clone()))
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Inbox,
    People,
    /// A network's home.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    mode: Mode,
    open: Option<(Key, String)>,
    tiny_channel: Option<String>,
}

pub struct FolderEdit {
    pub key: Key,
    pub name: gpui::Entity<crate::gui::input::TextInput>,
}

pub struct CreateChannelModal {
    pub space: Key,
    pub group: Option<String>,
    pub name: gpui::Entity<crate::gui::input::TextInput>,
    pub description: gpui::Entity<crate::gui::input::TextInput>,
    pub username: gpui::Entity<crate::gui::input::TextInput>,
    pub kind: ConversationKind,
    pub private: bool,
}

pub struct ForumPostModal {
    pub forum: Key,
    pub title: gpui::Entity<crate::gui::input::TextInput>,
    pub text: gpui::Entity<crate::gui::input::TextInput>,
    pub tags: Vec<protocol::ForumTag>,
    pub picked: Vec<String>,
    pub files: Vec<protocol::OutgoingFile>,
    pub require_tag: bool,
    pub media: bool,
    pub guidelines: String,
    pub error: Option<String>,
    pub busy: bool,
}

pub struct State {
    pub mode: Mode,
    pub filter: InboxFilter,
    /// The Inbox, held whole, in the order drawn.
    pub inbox: Pages<Rc<Conversation>>,
    /// How many rows each filter holds, as the last Inbox answer said.
    pub counts: InboxCounts,
    /// Whether the Inbox's order is being held (the pointer is over it, or the keys are in it).
    pub hold: hold::Hold,
    inbox_list: ListState,
    inbox_items: Rc<Vec<inbox::Item>>,
    apps_open: bool,
    apps_all: bool,
    /// The order that came in while the Inbox was held, and how many rows have news in it.
    waiting: Option<Vec<Rc<Conversation>>>,
    pill: usize,
    /// The newest conversation of the whole Inbox, for "You're all caught up".
    last_seen: Option<(String, i64)>,
    /// Muted conversations with new messages, as the last All answer had them: "You're all
    /// caught up" offers them, and lists them when asked.
    muted_news: Vec<Rc<Conversation>>,
    muted_open: bool,
    /// Network activity feed (mentions, thread replies, reactions, apps) shown under Activity.
    pub(super) activity_filter: Option<protocol::ActivityItemKind>,
    pub(super) activity_items: Vec<protocol::ActivityItem>,
    pub(super) activity_cursor: Option<(AccountKey, String)>,
    pub(super) activity_loading: bool,
    /// Activity rows whose invite or sign-in code the user chose to reveal in Streamer mode.
    activity_revealed: HashSet<String>,
    activity_epoch: u64,
    /// Recent mentions fetched from the network on demand (`Request::Mentions`, Discord).
    pub(super) mentions_items: Vec<protocol::MentionHit>,
    /// One cursor per account that still has another page.
    pub(super) mentions_more: Vec<(AccountKey, String)>,
    pub(super) mentions_loading: bool,
    pub(super) mentions_everyone: bool,
    pub(super) mentions_roles: bool,
    mentions_epoch: u64,
    /// The network home on show: what chatd said, and the user's folds.
    pub home: home::Home,
    saved_homes: HashMap<Network, home::Home>,
    home_list: ListState,
    home_items: Rc<Vec<home::Item>>,
    /// The home's Unread segment, and how many rows it holds.
    home_unread: bool,
    home_news: u32,
    /// A list the home drilled into (Threads, Archive, Message requests, a forum's topics).
    pub drill: Option<home::Drill>,
    /// User navigation, independent of background home reads and restored home epochs.
    navigation: u64,
    /// The Slack workspace on show, and whether its menu is open.
    pub slack: Option<Key>,
    slack_menu: bool,
    pub spaces_loaded: bool,
    /// Groups the user opened or closed against how they start, and what they revealed.
    opened: HashSet<String>,
    closed: HashSet<String>,
    quiet_shown: HashSet<String>,
    threads_shown: HashSet<Key>,
    muted_shown: HashSet<Key>,
    opted_shown: HashSet<Key>,
    /// The tree on show, flat. Rebuilt on collapse and when its data changes.
    pub rows: Rc<Vec<Row>>,
    tree_scroll: UniformListScrollHandle,
    folder_tabs_scroll: gpui::ScrollHandle,
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
    /// Friend requests waiting, for People's dot on the rail.
    pub friend_requests: u32,
    /// A read of the home is on its way.
    pub home_pending: bool,
    /// A read of the Inbox is on its way, coalesced across a burst of changes.
    pub inbox_pending: bool,
    /// A read of Spaces is on its way, coalesced the same way.
    pub spaces_pending: bool,
    pub switcher: Option<switcher::Switcher>,
    pub create_channel: Option<CreateChannelModal>,
    pub create_invite: Option<CreateInviteModal>,
    /// Rename of a Discord server folder.
    pub folder_edit: Option<FolderEdit>,
    pub forum_post: Option<ForumPostModal>,
    pub join_server: Option<JoinServerModal>,
    pub create_server: Option<CreateServerModal>,
    /// Cached scheduled & active events per Discord Space.
    pub space_events: HashMap<Key, Vec<protocol::SpaceEvent>>,
    /// Scheduled events modal when open for a Space.
    pub events_modal: Option<EventsModal>,
    /// The search field on Discord's Discover list.
    discover_input: Option<Entity<crate::gui::input::TextInput>>,
    /// A server's rules and onboarding, opened from the composer or the conversation header.
    pub server_gate: Option<ServerGate>,
    /// Confirmation modal before leaving a Space (`(space_key, space_name)`).
    pub leave_space: Option<(Key, String)>,
    /// Discord "Browse channels" modal with follow/unfollow checkboxes.
    pub browse_channels: Option<BrowseChannelsModal>,
    /// Discord "Create category" modal.
    pub create_category: Option<CreateCategoryModal>,
    /// Discord "Edit channel" / "Rename category" modal.
    pub edit_channel: Option<EditChannelModal>,
    /// Discord bulk channel/category reorder modal.
    pub move_channels: Option<MoveChannelsModal>,
    /// Confirmation modal requiring typing the channel/category name before deleting.
    pub delete_channel: Option<DeleteChannelModal>,
}

pub struct CreateCategoryModal {
    pub space: Key,
    pub name: Entity<crate::gui::input::TextInput>,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct EditChannelModal {
    pub target: Key,
    pub space: Key,
    pub is_category: bool,
    pub kind: ConversationKind,
    pub name: Entity<crate::gui::input::TextInput>,
    pub topic: Entity<crate::gui::input::TextInput>,
    pub slow_mode: Entity<crate::gui::input::TextInput>,
    pub user_limit: Entity<crate::gui::input::TextInput>,
    pub nsfw: bool,
    pub parent_id: Option<String>,
    pub initial_parent_id: Option<String>,
    pub categories: Vec<(String, String)>,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct MoveChannelItem {
    pub id: String,
    pub name: String,
    pub is_category: bool,
    pub parent_id: Option<String>,
}

pub struct MoveChannelsModal {
    pub space: Key,
    pub items: Vec<MoveChannelItem>,
    pub categories: Vec<(String, String)>,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct DeleteChannelModal {
    pub target: Key,
    pub name: String,
    pub is_category: bool,
    pub confirm: Entity<crate::gui::input::TextInput>,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct BrowseChannelsModal {
    pub space: Key,
    pub space_name: String,
    pub filter: Entity<crate::gui::input::TextInput>,
    pub loading: bool,
    pub error: Option<String>,
    pub channels: Vec<protocol::BrowsedChannel>,
}

#[derive(Clone, Debug)]
pub enum CreateInviteTarget {
    Space(Key, String),
    Channel(Key, String),
    Friend(AccountKey),
}

pub struct CreateInviteModal {
    pub target: CreateInviteTarget,
    pub max_age: u32,
    pub max_uses: u32,
    pub temporary: bool,
    pub url: Option<String>,
    pub error: Option<String>,
    pub busy: bool,
    pub sent: HashSet<Key>,
    pub epoch: u64,
}
pub struct EventsModal {
    pub space: Key,
    pub error: Option<String>,
    pub busy_event: Option<String>,
}

const DISCORD_APP: &str = "https://discord.com/app";

pub struct ServerGate {
    pub space: Key,
    pub loaded: bool,
    pub gate: Option<protocol::SpaceGate>,
    pub agreed: Vec<bool>,
    pub choice: Vec<Option<u32>>,
    pub texts: Vec<Option<Entity<crate::gui::input::TextInput>>>,
    pub picked: Vec<String>,
    pub error: Option<String>,
    pub busy: bool,
}

pub struct JoinServerModal {
    pub account: AccountKey,
    pub input: Entity<crate::gui::input::TextInput>,
    pub preview: Option<protocol::InvitePreview>,
    pub error: Option<String>,
    pub busy: bool,
}

pub struct CreateServerModal {
    pub account: AccountKey,
    pub name: Entity<crate::gui::input::TextInput>,
    pub icon_path: Option<String>,
    pub template: String,
    pub error: Option<String>,
    pub busy: bool,
}

fn list_state() -> ListState {
    // One row ahead of the viewport. Scrolling already redraws this view, and that
    // render is what updates who is watched. A content change that keeps the same
    // rows does not splice, so a measured height stays measured (`respliced`).
    ListState::new(0, ListAlignment::Top, px(48.))
}

impl State {
    /// The chosen Telegram chip belongs to its home even while another network is shown.
    pub fn telegram_folder(&self) -> Option<String> {
        let home = if self.home.network == Some(Network::Telegram) {
            Some(&self.home)
        } else {
            self.saved_homes.get(&Network::Telegram)
        };
        home.and_then(|home| home.folder.clone())
    }

    fn switch_mode(&mut self, mode: Mode) {
        self.navigation_changed();
        let home_network = match &mode {
            Mode::Space(space) if space.network == Network::Discord => Some(Network::Discord),
            Mode::Network(network) => Some(*network),
            _ => None,
        };
        self.home.switch_network(home_network, &mut self.saved_homes);
        self.mode = mode;
        self.drill = None;
    }

    pub(super) fn navigation_changed(&mut self) {
        self.navigation = self.navigation.wrapping_add(1);
    }

    fn enter_drill(&mut self, drill: home::Drill) {
        self.navigation_changed();
        self.drill = Some(drill);
    }

    fn leave_drill(&mut self) -> bool {
        self.navigation_changed();
        self.drill.take().is_some()
    }

    fn landing_topic<'a>(&self, navigation: u64, forum: &Key, conversations: &'a [Conversation]) -> Option<&'a Conversation> {
        if self.navigation != navigation || !matches!(&self.drill, Some(home::Drill::Topics(current)) if &current.key == forum) {
            return None;
        }
        conversations.first()
    }

    /// The Telegram folder chip last chosen, from the window's own memory: the home on show
    /// and the one waiting in reserve both start there.
    pub fn remember_telegram_folder(&mut self, folder: Option<String>) {
        self.home.folder = folder.clone();
        let saved = self.saved_homes.entry(Network::Telegram).or_insert_with(|| home::Home::for_network(Network::Telegram));
        saved.folder = folder;
    }

    pub fn new(cx: &mut Context<Shell>) -> State {
        #[cfg(feature = "gui-shot")]
        shot::schedule(cx);
        Self::with_focus(cx.focus_handle())
    }

    fn with_focus(focus: FocusHandle) -> State {
        State {
            mode: Mode::Inbox,
            filter: InboxFilter::All,
            inbox: Pages::default(),
            counts: InboxCounts::default(),
            hold: hold::Hold::default(),
            inbox_list: list_state(),
            inbox_items: Rc::new(Vec::new()),
            apps_open: false,
            apps_all: false,
            waiting: None,
            pill: 0,
            last_seen: None,
            muted_news: Vec::new(),
            muted_open: false,
            activity_filter: None,
            activity_items: Vec::new(),
            activity_cursor: None,
            activity_loading: false,
            activity_revealed: HashSet::new(),
            activity_epoch: 0,
            mentions_items: Vec::new(),
            mentions_more: Vec::new(),
            mentions_loading: false,
            mentions_everyone: true,
            mentions_roles: true,
            mentions_epoch: 0,
            home: home::Home::default(),
            saved_homes: HashMap::new(),
            home_list: list_state(),
            home_items: Rc::new(Vec::new()),
            home_unread: false,
            home_news: 0,
            drill: None,
            navigation: 0,
            slack: None,
            slack_menu: false,
            spaces_loaded: false,
            opened: HashSet::new(),
            closed: HashSet::new(),
            quiet_shown: HashSet::new(),
            threads_shown: HashSet::new(),
            muted_shown: HashSet::new(),
            opted_shown: HashSet::new(),
            rows: Rc::new(Vec::new()),
            tree_scroll: UniformListScrollHandle::new(),
            folder_tabs_scroll: gpui::ScrollHandle::new(),
            cursor: None,
            focus,
            pins: Vec::new(),
            history: History::default(),
            walked: Vec::new(),
            walking: None,
            width: std::cell::Cell::new(super::layout::SIDEBAR_WIDTH),
            drafts: HashMap::new(),
            friend_requests: 0,
            home_pending: false,
            inbox_pending: false,
            spaces_pending: false,
            switcher: None,
            create_channel: None,
            create_invite: None,
            folder_edit: None,
            forum_post: None,
            join_server: None,
            create_server: None,
            space_events: HashMap::new(),
            events_modal: None,
            server_gate: None,
            leave_space: None,
            browse_channels: None,
            create_category: None,
            edit_channel: None,
            move_channels: None,
            delete_channel: None,
            discover_input: None,
        }
    }

    /// Drops rows whose account is signed out. True when the Inbox changed, and when a home list did.
    pub fn without_signed_out(&mut self, keep: &HashSet<AccountKey>) -> (bool, bool) {
        let visible = |key: &Key| keep.contains(&key.account());
        let kept: Vec<_> = self.inbox.all().iter().filter(|row| visible(&row.key)).cloned().collect();
        let inbox_changed = kept.len() != self.inbox.all().len();
        if inbox_changed {
            let epoch = self.inbox.epoch();
            let total = kept.len() as u32;
            self.inbox.fill(epoch, kept, total);
            self.muted_news.retain(|row| visible(&row.key));
            if let Some(waiting) = self.waiting.as_mut() {
                waiting.retain(|row| visible(&row.key));
            }
        }
        let home = &mut self.home;
        let before = home.chats.len() + home.drilled.len() + home.in_voice.len() + home.archive.len();
        home.chats.retain(|row| visible(&row.key));
        home.drilled.retain(|row| visible(&row.key));
        home.in_voice.retain(|row| visible(&row.key));
        home.archive.retain(|row| visible(&row.key));
        let mut home_changed = home.chats.len() + home.drilled.len() + home.in_voice.len() + home.archive.len() != before;
        for saved in self.saved_homes.values_mut() {
            let before = saved.chats.len() + saved.drilled.len() + saved.in_voice.len() + saved.archive.len();
            saved.chats.retain(|row| visible(&row.key));
            saved.drilled.retain(|row| visible(&row.key));
            saved.in_voice.retain(|row| visible(&row.key));
            saved.archive.retain(|row| visible(&row.key));
            home_changed |= saved.chats.len() + saved.drilled.len() + saved.in_voice.len() + saved.archive.len() != before;
        }
        (inbox_changed, home_changed)
    }

    /// Every row the sidebar holds, wherever it is drawn.
    pub fn held(&self) -> impl Iterator<Item = &Rc<Conversation>> {
        let home = &self.home;
        self.inbox.all().iter().chain(&home.chats).chain(&home.drilled).chain(&home.in_voice).chain(&home.archive)
    }

    /// A conversation's row, if any list on show holds it.
    pub fn find(&self, trees: &HashMap<Key, SpaceTree>, key: &Key) -> Option<Conversation> {
        let in_trees = trees.values().flat_map(|tree| &tree.conversations);
        self.held().chain(in_trees).find(|row| &row.key == key).map(|row| (**row).clone())
    }

    /// The list on show is a GPUI `list` (the Inbox, a home) rather than a tree.
    fn listed(&self) -> bool {
        match &self.mode {
            Mode::Inbox => true,
            Mode::Network(Network::Slack) => self.drill.is_some(),
            Mode::Network(_) => true,
            Mode::People | Mode::Space(_) => false,
        }
    }
}

/// The glyph of a conversation in a tree. A channel only invited people see shows a lock.
pub fn kind_glyph(kind: ConversationKind, private: bool, call: CallSupport) -> &'static str {
    match kind {
        ConversationKind::Channel | ConversationKind::Voice if private => "icons/lock.svg",
        ConversationKind::Direct => "icons/user.svg",
        ConversationKind::Group => "icons/users.svg",
        ConversationKind::Channel => "icons/hash.svg",
        ConversationKind::Announcement => "icons/megaphone.svg",
        // A stage is a voice row handed to Discord (`02` §4): radio, not speaker.
        ConversationKind::Voice if call == CallSupport::Handoff => "icons/radio.svg",
        ConversationKind::Voice => "icons/volume-2.svg",
        ConversationKind::Forum => "icons/messages-square.svg",
        ConversationKind::Thread => "icons/corner-down-right.svg",
    }
}

/// A voice row the server list lets you join from: a native voice channel, never a stage,
/// which opens in Discord instead (07 §1).
pub fn joins_from_list(c: &Conversation) -> bool {
    c.kind == ConversationKind::Voice && c.call == CallSupport::Native
}

fn row_id(prefix: &'static str, key: &Key) -> gpui::ElementId {
    // The id carries the network and account, so two of them may reuse a conversation id.
    (gpui::ElementId::from(prefix), format!("{}/{}/{}", key.network.label(), key.account, key.id)).into()
}

/// The rows of a mixed-height list that are on screen, from its scroll position and the known
/// height of each item: what is watched and fetched for.
pub(in crate::gui) fn on_screen(list: &ListState, count: usize, height: impl Fn(usize) -> f32) -> Range<usize> {
    let top = list.logical_scroll_top();
    let tall = f32::from(list.viewport_bounds().size.height);
    // Before the first layout the list has no bounds: a window's worth.
    let tall = if tall <= 0. { 900. } else { tall };
    let start = top.item_ix.min(count);
    let mut y = -f32::from(top.offset_in_item);
    let mut end = start;
    while end < count && y < tall {
        y += height(end);
        end += 1;
    }
    start..end
}

/// The slice of `old` to replace so the list matches `new`. `None` when the sequences
/// are the same: the caller keeps every measured height.
pub(in crate::gui) fn splice_range<T: PartialEq>(old: &[T], new: &[T]) -> Option<(Range<usize>, usize)> {
    if old == new {
        return None;
    }
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix && suffix < new.len() - prefix && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    Some((prefix..old.len() - suffix, new.len() - suffix - prefix))
}

/// Replaces a list's items, keeping the first visible item where it is when it is still there.
/// Same anchors and heights do not splice, so GPUI keeps the heights it already measured.
pub(in crate::gui) fn respliced<T>(list: &ListState, old: &[T], new: &[T], anchor: impl Fn(&T) -> Option<String>, height: impl Fn(&T) -> f32) {
    let old_sig: Vec<_> = old.iter().map(|item| (anchor(item), height(item).to_bits())).collect();
    let new_sig: Vec<_> = new.iter().map(|item| (anchor(item), height(item).to_bits())).collect();
    let Some((range, count)) = splice_range(&old_sig, &new_sig) else {
        return;
    };
    let top = list.logical_scroll_top();
    // A list at its top stays at its top: what is new there is what the user should see.
    if top.item_ix == 0 && top.offset_in_item <= px(0.) {
        list.splice(range, count);
        list.scroll_to(gpui::ListOffset::default());
        return;
    }
    let kept = old.get(top.item_ix).and_then(&anchor).and_then(|key| new.iter().position(|item| anchor(item).as_ref() == Some(&key)));
    list.splice(range, count);
    if let Some(ix) = kept {
        list.scroll_to(gpui::ListOffset { item_ix: ix, offset_in_item: top.offset_in_item });
    } else if top.item_ix > 0 {
        list.scroll_to(gpui::ListOffset { item_ix: top.item_ix.min(new.len()), offset_in_item: px(0.) });
    }
}

impl Shell {
    /// For the composer: what is typed and unsent in a conversation now, empty when nothing is.
    pub(super) fn draft_changed(&mut self, conversation: &Key, text: &str, cx: &mut Context<Self>) {
        let drafts = &mut self.sidebar_state.drafts;
        if drafts.len() >= DRAFTS_HELD && !drafts.contains_key(conversation) {
            drafts.clear();
        }
        if drafts.insert(conversation.clone(), text.trim().to_owned()).as_deref() != Some(text.trim()) {
            if self.sidebar_state.mode == Mode::Network(Network::Slack)
                || matches!(self.sidebar_state.drill, Some(home::Drill::Drafts(_)))
            {
                self.rebuild_rows(cx);
            } else {
                cx.notify();
            }
        }
    }

    /// The draft a row shows: what the composer last said, else what chatd stored.
    pub(super) fn draft_of<'a>(&'a self, c: &'a Conversation) -> &'a str {
        self.sidebar_state.drafts.get(&c.key).map_or(c.draft.as_str(), String::as_str)
    }

    // Where the sidebar is.

    /// Goes to a mode, remembering where the user was.
    pub(super) fn go(&mut self, mode: Mode, cx: &mut Context<Self>) {
        // Text channels and direct messages open in the conversation pane. A chat left open
        // from another network would hide the server page, so it is closed on the way in.
        if matches!(mode, Mode::Network(Network::Tiny)) {
            self.open = None;
            if let Some(snapshot) = &mut self.snapshot {
                snapshot.open = None;
            }
            // The open conversation may already be a window with no composer.
            self.sync_popout_composers(cx);
        }
        if self.sidebar_state.mode == mode {
            let state = &mut self.sidebar_state;
            // Clicking the lit place leaves a drill and scrolls its list to the top.
            if state.leave_drill() {
                self.rebuild_rows(cx);
            }
            let state = &self.sidebar_state;
            state.inbox_list.scroll_to(gpui::ListOffset::default());
            if !matches!(&state.mode, Mode::Space(space) if space.network == Network::Discord) {
                state.home_list.scroll_to(gpui::ListOffset::default());
            }
            state.tree_scroll.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
            return;
        }
        self.remember();
        self.set_mode(mode, cx);
    }

    /// Show this conversation in its network home and scroll to the row. Slack stays on the
    /// workspace home: Space mode drops the Threads row and the All / Unread segments.
    pub(super) fn reveal_in_sidebar(&mut self, conversation: &Conversation, cx: &mut Context<Self>) {
        if conversation.key.network == Network::Slack {
            if let Some(id) = conversation.space.clone() {
                self.sidebar_state.slack = Some(Key { id, ..conversation.key.clone() });
            }
            if matches!(self.sidebar_state.mode, Mode::Network(Network::Slack)) {
                self.show_mode(cx);
            } else {
                self.go(Mode::Network(Network::Slack), cx);
            }
        } else if let Some(id) = conversation.space.clone() {
            self.go(Mode::Space(Key { id, ..conversation.key.clone() }), cx);
        }
        let key = conversation.key.clone();
        if let Some(ix) = self.sidebar_state.rows.iter().position(|row| row.conversation().is_some_and(|c| c.key == key)) {
            self.sidebar_state.cursor = Some(ix);
            self.scroll_to(ix);
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let state = &mut self.sidebar_state;
        if state.mode == Mode::Inbox && state.hold.release() {
            if let Some(fresh) = state.waiting.take() {
                let total = fresh.len() as u32;
                let epoch = state.inbox.epoch();
                state.inbox.fill(epoch, fresh, total);
            }
        }
        let keep_discord = state.mode.network() == Some(Network::Discord) && mode.network() == Some(Network::Discord);
        state.switch_mode(mode);
        state.cursor = None;
        state.slack_menu = false;
        if !keep_discord {
            state.home_unread = false;
        }
        state.apps_open = false;
        state.apps_all = false;
        state.pill = 0;
        state.tree_scroll = UniformListScrollHandle::new();
        state.hold.reset();
        // The rows on show name who is watched again as they draw.
        let hub = self.people_state.hub();
        hub.watch(WATCH, [], cx);
        hub.watch_rows(WATCH, [], cx);
        self.show_mode(cx);
        self.rebuild_inbox(cx);
        // Navigation reads immediately; only background event bursts are debounced.
        self.load_home(cx);
    }

    fn here(&self) -> Location {
        let open = self.snapshot.as_ref().and_then(|s| s.open.as_ref());
        Location {
            mode: self.sidebar_state.mode.clone(),
            open: open.map(|open| (open.key.clone(), open.title.clone())),
            tiny_channel: self.tiny_state.channel(),
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
        let returning_to_tiny = matches!(there.mode, Mode::Network(Network::Tiny));
        if self.sidebar_state.mode != there.mode {
            self.set_mode(there.mode, cx);
        }
        if returning_to_tiny {
            if there.open.is_none() {
                self.open = None;
                if let Some(snapshot) = &mut self.snapshot {
                    snapshot.open = None;
                }
                self.sync_popout_composers(cx);
            }
            self.tiny_pick_channel(there.tiny_channel, cx);
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

    /// `cmd-up`: out of a drill, a Space's network, a network's Inbox.
    pub(super) fn up_level(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.leave_drill() {
            self.rebuild_rows(cx);
            return;
        }
        match self.sidebar_state.mode.clone() {
            Mode::Space(space) if space.network == Network::Slack => self.go(Mode::Network(Network::Slack), cx),
            Mode::Space(space) => self.go(Mode::Network(space.network), cx),
            Mode::Network(_) | Mode::People => self.go(Mode::Inbox, cx),
            Mode::Inbox => {}
        }
    }

    /// The Slack workspace the home shows: the one last used, else the first.
    pub(super) fn slack_space(&self) -> Option<Key> {
        let slack: Vec<&protocol::Space> = self.spaces.iter().filter(|s| s.key.network == Network::Slack).collect();
        let kept = self.sidebar_state.slack.as_ref().filter(|key| slack.iter().any(|s| &s.key == *key));
        kept.cloned().or_else(|| slack.first().map(|s| s.key.clone()))
    }

    /// Makes the trees held match what the mode shows: one Space, Slack's workspace, or the
    /// teams of the Teams home that are open or need me. The rest are dropped; chatd holds them.
    pub(super) fn show_mode(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<Key> = match &self.sidebar_state.mode {
            Mode::Space(space) => vec![space.clone()],
            Mode::Network(Network::Slack) => self.slack_space().into_iter().collect(),
            Mode::Network(Network::Teams) => self
                .spaces
                .iter()
                .filter(|s| s.key.network == Network::Teams)
                .filter(|s| s.attention > 0 || s.unread || self.sidebar_state.home.opened.contains(&s.key.id) || self.open.as_ref().is_some_and(|open| open.account() == s.key.account() && self.trees.get(&s.key).is_some_and(|t| t.conversations.iter().any(|c| &c.key == open))))
                .map(|s| s.key.clone())
                .collect(),
            Mode::Network(Network::Tiny) => self.spaces.iter().filter(|space| space.key.network == Network::Tiny).map(|space| space.key.clone()).collect(),
            Mode::Network(_) | Mode::Inbox | Mode::People => Vec::new(),
        };
        // One Space's live subscription at a time: the one on show.
        let watched = match &self.sidebar_state.mode {
            Mode::Space(space) => Some(space.clone()),
            Mode::Network(Network::Slack) => wanted.first().cloned(),
            _ => None,
        };
        self.people_state.hub().watch_space(watched.as_ref(), cx);
        if let Mode::Space(space) = &self.sidebar_state.mode {
            if space.network == Network::Discord {
                self.load_space_events(space.clone(), cx);
            }
        }
        self.trees.retain(|space, _| wanted.contains(space));
        for space in wanted {
            if !self.trees.contains_key(&space) {
                self.load_tree(space, cx);
            }
        }
        self.rebuild_rows(cx);
    }

    /// The tree on show as flat rows, or the home's list.
    pub(super) fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        let flat = |space: &Key, depth: u8| match self.trees.get(space).filter(|tree| tree.loaded) {
            Some(tree) => {
                let hide_muted = self.spaces.iter().any(|s| &s.key == space && s.hide_muted);
                let rules = tree::Rules {
                    network: Some(space.network),
                    hide_muted,
                    muted_shown: state.muted_shown.contains(space),
                    opted_out_shown: state.opted_shown.contains(space),
                    opened: Some(&state.opened),
                    closed: Some(&state.closed),
                    quiet_shown: Some(&state.quiet_shown),
                    threads_shown: Some(&state.threads_shown),
                    unread: state.home_unread && space.network == Network::Slack,
                };
                tree::flatten(space, &tree.groups, &tree.conversations, self.open.as_ref(), depth, &rules)
            }
            None => vec![Row::Loading { depth }; 6],
        };
        let rows = match &state.mode {
            Mode::Space(space) => flat(space, 0),
            Mode::Network(Network::Slack) => match self.slack_space() {
                Some(space) => {
                    let unreads = self.unreads_row(&space);
                    let threads = self.threads_row(&space);
                    let drafts = self.drafts_row(&space);
                    unreads.into_iter().chain(threads).chain(drafts).chain(flat(&space, 0)).collect()
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        if *self.sidebar_state.rows != rows {
            self.sidebar_state.rows = Rc::new(rows);
        }
        self.rebuild_home(cx);
        self.walk_arrived(cx);
        cx.notify();
    }

    // What rows do.

    /// Collapses or opens a Group. chatd remembers it; the network never hears.
    fn collapse(&mut self, space: Key, group: String, collapsed: bool, cx: &mut Context<Self>) {
        let state = &mut self.sidebar_state;
        if collapsed {
            state.opened.remove(&group);
            state.closed.insert(group.clone());
        } else {
            state.closed.remove(&group);
            state.opened.insert(group.clone());
        }
        if group == tree::APPS_GROUP {
            self.rebuild_rows(cx);
            return;
        }
        if let Some(found) = self.trees.get_mut(&space).and_then(|tree| tree.groups.iter_mut().find(|g| g.id == group)) {
            found.collapsed = collapsed;
        }
        self.rebuild_rows(cx);
        if super::people::only_draws() {
            return;
        }
        let request = Request::Collapse { space, group, collapsed };
        self.off_thread(cx, move || ask(&request), |_, _, _| {});
    }

    /// The newest stored message becomes the first unread one. An open pane keeps its divider
    /// and does not immediately read the messages already on screen.
    pub(super) fn mark_conversation_unread(&mut self, conversation: Key, cx: &mut Context<Self>) {
        if self.open.as_ref() == Some(&conversation) {
            self.hold_open_unread(cx);
        }
        let request = Request::MarkUnread { conversation, message_id: String::new() };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            if let Some(words) = refused(&answer) {
                this.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx);
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    pub(super) fn mark_conversation_read(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let request = Request::MarkRead { conversation, message_id: String::new() };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            if let Some(words) = refused(&answer) {
                this.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx);
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    pub(super) fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        let request = Request::MarkAllRead { network: None, space: None, folder: None };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            match answer {
                Ok(Response::Ok) => {
                    // The Unread page never lists muted rows, so the catch-up note would keep
                    // naming conversations this just read.
                    this.sidebar_state.muted_news.clear();
                    this.sidebar_state.muted_open = false;
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Marked as read", cx);
                }
                Ok(Response::Error { detail, .. }) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx),
                Err(err) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), crate::client::trouble_words("inbox", &err), cx),
                _ => {}
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    /// Everything with a badge in one Space is read, here and where the network keeps a read
    /// state. Direct messages and other Spaces are left alone.
    pub(super) fn mark_space_read(&mut self, space: Key, cx: &mut Context<Self>) {
        let request = Request::MarkAllRead { network: Some(space.network), space: Some(space), folder: None };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            match answer {
                Ok(Response::Ok) => {
                    this.sidebar_state.muted_news.clear();
                    this.sidebar_state.muted_open = false;
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Marked as read", cx);
                }
                Ok(Response::Error { detail, .. }) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx),
                Err(err) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), crate::client::trouble_words("inbox", &err), cx),
                _ => {}
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    /// Everything with a badge in one folder (e.g. a Discord server folder) is read.
    pub(super) fn mark_folder_read(&mut self, folder: Key, cx: &mut Context<Self>) {
        let request = Request::MarkAllRead { network: Some(folder.network), space: None, folder: Some(folder) };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            match answer {
                Ok(Response::Ok) => {
                    this.sidebar_state.muted_news.clear();
                    this.sidebar_state.muted_open = false;
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Marked as read", cx);
                }
                Ok(Response::Error { detail, .. }) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx),
                Err(err) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), crate::client::trouble_words("inbox", &err), cx),
                _ => {}
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    /// Discord lets this account move and rename server folders.
    pub(super) fn manages_space_folders(&self, account: &AccountKey) -> bool {
        self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.accounts.iter().any(|row| row.key == *account && row.link == Link::Connected && row.can.manage_space_folders)
        })
    }

    fn known_discord_folder(&self, folder: &Key) -> bool {
        self.sidebar_state.home.folders.iter().any(|known| &known.key == folder)
    }

    /// Moves `dragged` into `folder` (the end of that folder). Same account only.
    fn drop_discord_on_folder(&mut self, folder: Key, dragged: &home_view::MovedSpace, cx: &mut Context<Self>) {
        if dragged.key.account() != folder.account() || !self.manages_space_folders(&folder.account()) || !self.known_discord_folder(&folder) {
            return;
        }
        self.send_move_space(
            Request::MoveSpace { space: dragged.key.clone(), before: None, folder: Some(folder.id.clone()), into_space: None },
            cx,
        );
    }

    /// Drops `dragged` onto a server row: into that server's folder, ahead of it, or onto the
    /// server itself to make a folder of the two.
    fn drop_discord_space(&mut self, target: Key, dragged: &home_view::MovedSpace, cx: &mut Context<Self>) {
        if dragged.key == target || dragged.key.account() != target.account() || !self.manages_space_folders(&target.account()) {
            return;
        }
        let account = target.account();
        let folder_id = self.spaces.iter().find(|space| space.key == target).and_then(|space| space.folder.clone());
        let grouped = folder_id.as_ref().is_some_and(|id| {
            self.sidebar_state.home.folders.iter().any(|folder| {
                folder.key.account() == account && folder.key.id == *id && folder.spaces.len() > 1
            })
        });
        let request = if grouped {
            Request::MoveSpace {
                space: dragged.key.clone(),
                before: Some(target.id.clone()),
                folder: folder_id,
                into_space: None,
            }
        } else {
            Request::MoveSpace {
                space: dragged.key.clone(),
                before: None,
                folder: None,
                into_space: Some(target.id.clone()),
            }
        };
        self.send_move_space(request, cx);
    }

    fn send_move_space(&mut self, request: Request, cx: &mut Context<Self>) {
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            if let Some(words) = refused(&answer) {
                this.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx);
                return;
            }
            this.load_home(cx);
        });
    }

    /// Renames or recolors a Discord server folder. `name` absent leaves it; empty clears it.
    /// `color` absent leaves it; `Some(None)` clears it.
    pub(super) fn set_discord_folder(&mut self, folder: Key, name: Option<String>, color: Option<Option<u32>>, cx: &mut Context<Self>) {
        if !self.manages_space_folders(&folder.account()) || !self.known_discord_folder(&folder) {
            return;
        }
        let request = Request::SetFolder { folder, name, color };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            if let Some(words) = refused(&answer) {
                this.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx);
                return;
            }
            this.load_home(cx);
        });
    }

    pub(super) fn open_folder_rename(&mut self, folder: Key, cx: &mut Context<Self>) {
        if !self.manages_space_folders(&folder.account()) || !self.known_discord_folder(&folder) {
            return;
        }
        let current = self.sidebar_state.home.folders.iter().find(|known| known.key == folder).map(|known| known.name.clone()).unwrap_or_default();
        let name = cx.new(|cx| {
            let mut input = crate::gui::input::TextInput::new("Folder name", cx);
            if !current.is_empty() {
                input.set_text(&current, cx);
            }
            input
        });
        self.sidebar_state.folder_edit = Some(FolderEdit { key: folder, name });
        cx.notify();
    }

    fn submit_folder_rename(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.sidebar_state.folder_edit.take() else {
            return;
        };
        let name = edit.name.read(cx).text().trim().to_owned();
        cx.notify();
        self.set_discord_folder(edit.key, Some(name), None, cx);
    }

    /// Removes a direct or group conversation from this Inbox. The network keeps the messages.
    pub(super) fn forget_conversation(&mut self, conversation: Conversation, cx: &mut Context<Self>) {
        let title = conversation.title.clone();
        let network = conversation.key.network;
        let request = Request::Forget { conversation: conversation.key };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            match answer {
                Ok(Response::Ok) => {
                    let words = match network {
                        Network::Discord => format!("{title} was deleted from your inbox. A new message brings it back. Discord still has the messages."),
                        _ => format!("{title} was deleted from your inbox. A new message brings it back."),
                    };
                    this.toast(Some(("icons/trash-2.svg", theme::text_soft())), words, cx);
                }
                Ok(Response::Error { detail, .. }) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx),
                Err(err) => this.toast(Some(("icons/circle-alert.svg", theme::danger())), crate::client::trouble_words("inbox", &err), cx),
                _ => {}
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    pub(super) fn star(&mut self, conversation: Key, starred: bool, cx: &mut Context<Self>) {
        let request = Request::Star { conversation, starred };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            if let Some(words) = refused(&answer) {
                this.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx);
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
        });
    }

    /// Pins/unpins, archives/unarchives, or files a conversation in a folder on the network.
    pub(super) fn file_conversation(
        &mut self,
        conversation: Key,
        pinned: Option<bool>,
        archived: Option<bool>,
        folder_add: Option<String>,
        folder_remove: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let request = Request::File { conversation, pinned, archived, folder_add, folder_remove };
        self.off_thread(cx, move || ask(&request), |this, answer, cx| {
            if let Some(words) = refused(&answer) {
                this.toast(Some(("icons/circle-alert.svg", theme::danger())), words, cx);
            }
            let pages = this.sidebar_state.inbox.stale();
            this.load_inbox(pages, cx);
            if this.sidebar_state.home.network.is_some() {
                this.load_home(cx);
            }
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
        state.waiting = None;
        state.pill = 0;
        state.muted_open = false;
        state.hold.reset();
        state.inbox.reset();
        self.rebuild_inbox(cx);
        self.load_inbox(vec![0], cx);
        if filter == InboxFilter::Activity {
            self.load_activity_feed(cx);
        } else if filter == InboxFilter::Mentions {
            self.load_mentions_history(cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_mentions_everyone(&mut self, cx: &mut Context<Self>) {
        self.sidebar_state.mentions_everyone = !self.sidebar_state.mentions_everyone;
        self.load_mentions_history(cx);
        cx.notify();
    }

    pub(super) fn toggle_mentions_roles(&mut self, cx: &mut Context<Self>) {
        self.sidebar_state.mentions_roles = !self.sidebar_state.mentions_roles;
        self.load_mentions_history(cx);
        cx.notify();
    }

    pub(super) fn load_mentions_history(&mut self, cx: &mut Context<Self>) {
        let accounts: Vec<AccountKey> = self
            .snapshot
            .as_ref()
            .map(|s| {
                s.accounts
                    .iter()
                    .filter(|a| a.can.mentions_history && a.link == Link::Connected)
                    .map(|a| a.key.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.sidebar_state.mentions_epoch += 1;
        let epoch = self.sidebar_state.mentions_epoch;
        self.sidebar_state.mentions_items.clear();
        self.sidebar_state.mentions_more.clear();
        if accounts.is_empty() {
            self.sidebar_state.mentions_loading = false;
            return;
        }
        self.sidebar_state.mentions_loading = true;
        let everyone = self.sidebar_state.mentions_everyone;
        let roles = self.sidebar_state.mentions_roles;
        cx.spawn(async move |this, cx| {
            let (merged, more) = cx
                .background_executor()
                .spawn(async move {
                    let mut merged = Vec::new();
                    let mut more = Vec::new();
                    for account in accounts {
                        let req = Request::Mentions {
                            account: account.clone(),
                            before: None,
                            everyone,
                            roles,
                            space: None,
                            limit: 25,
                        };
                        if let Ok(Response::Mentions { items, more: has_more, cursor }) = ask(&req) {
                            if let Some(before) = mention_cursor(&items, has_more, cursor) {
                                more.push((account, before));
                            }
                            merged.extend(items);
                        }
                    }
                    merged.sort_by_key(|item| std::cmp::Reverse(item.sent));
                    (merged, more)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.sidebar_state.mentions_epoch != epoch {
                    return;
                }
                this.sidebar_state.mentions_loading = false;
                this.sidebar_state.mentions_items = merged;
                this.sidebar_state.mentions_more = more;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_more_mentions(&mut self, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.sidebar_state.mentions_more);
        if pending.is_empty() {
            return;
        }
        let epoch = self.sidebar_state.mentions_epoch;
        let everyone = self.sidebar_state.mentions_everyone;
        let roles = self.sidebar_state.mentions_roles;
        self.sidebar_state.mentions_loading = true;
        cx.spawn(async move |this, cx| {
            let (extra, next) = cx
                .background_executor()
                .spawn(async move {
                    let mut extra = Vec::new();
                    let mut next = Vec::new();
                    for (account, before) in pending {
                        let req = Request::Mentions {
                            account: account.clone(),
                            before: Some(before.clone()),
                            everyone,
                            roles,
                            space: None,
                            limit: 25,
                        };
                        match ask(&req) {
                            Ok(Response::Mentions { items, more, cursor }) => {
                                if let Some(before) = mention_cursor(&items, more, cursor) {
                                    next.push((account, before));
                                }
                                extra.extend(items);
                            }
                            _ => next.push((account, before)),
                        }
                    }
                    (extra, next)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.sidebar_state.mentions_epoch != epoch {
                    return;
                }
                this.sidebar_state.mentions_loading = false;
                this.sidebar_state.mentions_items.extend(extra);
                this.sidebar_state.mentions_items.sort_by_key(|item| std::cmp::Reverse(item.sent));
                this.sidebar_state.mentions_more = next;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn open_mention_hit(&mut self, hit: protocol::MentionHit, cx: &mut Context<Self>) {
        let title = if !hit.title.is_empty() {
            hit.title.clone()
        } else {
            self.sidebar_state
                .inbox
                .all()
                .iter()
                .find(|c| c.key == hit.conversation)
                .map(|c| c.title.clone())
                .unwrap_or_else(|| hit.conversation.id.clone())
        };
        self.open_resolved_link(hit.conversation, Some(hit.message_id), &title, cx);
        cx.notify();
    }

    pub(super) fn dismiss_mention_hit(&mut self, account: AccountKey, message_id: String, cx: &mut Context<Self>) {
        let req = Request::DismissMention { account: account.clone(), message_id: message_id.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| {
                if matches!(answer, Ok(Response::Ok)) {
                    this.sidebar_state.mentions_items.retain(|hit| {
                        hit.conversation.account() != account || hit.message_id != message_id
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn set_activity_filter(&mut self, kind: Option<protocol::ActivityItemKind>, cx: &mut Context<Self>) {
        if self.sidebar_state.activity_filter == kind {
            return;
        }
        self.sidebar_state.activity_filter = kind;
        self.load_activity_feed(cx);
        cx.notify();
    }

    pub(super) fn load_activity_feed(&mut self, cx: &mut Context<Self>) {
        let accounts: Vec<AccountKey> = self
            .snapshot
            .as_ref()
            .map(|s| {
                s.accounts
                    .iter()
                    .filter(|a| a.can.activity_feed && a.link == Link::Connected)
                    .map(|a| a.key.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.sidebar_state.activity_epoch += 1;
        let epoch = self.sidebar_state.activity_epoch;
        self.sidebar_state.activity_items.clear();
        self.sidebar_state.activity_cursor = None;
        if accounts.is_empty() {
            self.sidebar_state.activity_loading = false;
            return;
        }
        self.sidebar_state.activity_loading = true;
        let kind = self.sidebar_state.activity_filter;
        cx.spawn(async move |this, cx| {
            let (merged, next_cursor) = cx
                .background_executor()
                .spawn(async move {
                    let mut merged = Vec::new();
                    let mut next_cursor = None;
                    for account in accounts {
                        let req = Request::ActivityFeed { account: account.clone(), kind, cursor: None, limit: 30 };
                        if let Ok(Response::ActivityFeed { items, cursor }) = ask(&req) {
                            merged.extend(items);
                            if next_cursor.is_none() {
                                if let Some(c) = cursor {
                                    next_cursor = Some((account, c));
                                }
                            }
                        }
                    }
                    merged.sort_by_key(|item| std::cmp::Reverse(item.time));
                    (merged, next_cursor)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.sidebar_state.activity_epoch != epoch {
                    return;
                }
                this.sidebar_state.activity_loading = false;
                this.sidebar_state.activity_items = merged;
                this.sidebar_state.activity_cursor = next_cursor;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_more_activity(&mut self, cx: &mut Context<Self>) {
        let Some((account, cursor)) = self.sidebar_state.activity_cursor.take() else {
            return;
        };
        let epoch = self.sidebar_state.activity_epoch;
        let kind = self.sidebar_state.activity_filter;
        self.sidebar_state.activity_loading = true;
        cx.spawn(async move |this, cx| {
            let req = Request::ActivityFeed { account: account.clone(), kind, cursor: Some(cursor), limit: 30 };
            let got = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.sidebar_state.activity_epoch != epoch {
                    return;
                }
                this.sidebar_state.activity_loading = false;
                if let Ok(Response::ActivityFeed { items, cursor }) = got {
                    this.sidebar_state.activity_items.extend(items);
                    this.sidebar_state.activity_cursor = cursor.map(|c| (account, c));
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn open_activity_item(&mut self, item: protocol::ActivityItem, cx: &mut Context<Self>) {
        if item.unread {
            for existing in &mut self.sidebar_state.activity_items {
                if existing.id == item.id {
                    existing.unread = false;
                }
            }
            let req = Request::ActivityMarkRead {
                account: item.conversation.account(),
                item_id: Some(item.id.clone()),
            };
            cx.background_executor().spawn(async move { let _ = ask(&req); }).detach();
        }
        let message_id = (!item.message_id.is_empty()).then_some(item.message_id);
        let title = self
            .sidebar_state
            .inbox
            .all()
            .iter()
            .find(|c| c.key == item.conversation)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| item.conversation.id.clone());
        self.open_resolved_link(item.conversation, message_id, &title, cx);
        cx.notify();
    }

    /// What Return or a click does to the tree row at `ix`.
    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.sidebar_state.rows.get(ix).cloned() else {
            return;
        };
        match row {
            Row::Conversation { conversation, .. } => self.pick_row(&conversation, cx),
            Row::Group { space, id, collapsed, .. } => self.collapse(space, id, !collapsed, cx),
            Row::Unreads { space, .. } => self.drill(home::Drill::Unreads(Some(space)), cx),
            Row::Threads { space, .. } => self.drill(home::Drill::Threads(space), cx),
            Row::Drafts { space, .. } => self.drill(home::Drill::Drafts(space), cx),
            Row::Fold { reveal, .. } => {
                let space = match &self.sidebar_state.mode {
                    Mode::Space(space) => Some(space.clone()),
                    _ => self.slack_space(),
                };
                let state = &mut self.sidebar_state;
                match (reveal, space) {
                    (tree::Reveal::Muted, Some(space)) => {
                        state.muted_shown.insert(space);
                    }
                    (tree::Reveal::OptedOut, Some(space)) => {
                        if space.network == Network::Discord {
                            self.open_browse_channels(space, cx);
                            return;
                        }
                        state.opted_shown.insert(space);
                    }
                    (tree::Reveal::Threads(channel), _) => {
                        if !state.threads_shown.remove(&channel) {
                            state.threads_shown.insert(channel);
                        }
                    }
                    (tree::Reveal::Quiet(group), _) => {
                        state.quiet_shown.insert(group);
                    }
                    _ => return,
                }
                self.rebuild_rows(cx);
            }
            Row::Occupant { occupant, .. } => {
                if let Some(person) = self.people_state.hub().person(&occupant.person) {
                    self.open_person(&person, cx);
                }
            }
            Row::More { .. } | Row::Loading { .. } => {}
        }
    }

    fn list_len(&self) -> usize {
        let state = &self.sidebar_state;
        match state.mode {
            Mode::Inbox => state.inbox_items.len(),
            Mode::People => 0,
            _ if state.listed() => state.home_items.len(),
            _ => state.rows.len(),
        }
    }

    /// Whether the keyboard may stop on the item at `ix` of the list on show.
    fn stoppable(&self, ix: usize) -> bool {
        let state = &self.sidebar_state;
        match state.mode {
            Mode::Inbox => state.inbox_items.get(ix).is_some_and(|item| !matches!(item, inbox::Item::Header { .. } | inbox::Item::End)),
            _ if state.listed() => state.home_items.get(ix).is_some_and(|item| !matches!(item, home::Item::Header { .. })),
            _ => state.rows.get(ix).is_some_and(|row| !matches!(row, Row::Loading { .. } | Row::More { .. } | Row::Occupant { .. })),
        }
    }

    fn scroll_to(&mut self, ix: usize) {
        let state = &self.sidebar_state;
        match state.mode {
            Mode::Inbox => state.inbox_list.scroll_to_reveal_item(ix),
            _ if state.listed() => state.home_list.scroll_to_reveal_item(ix),
            _ => state.tree_scroll.scroll_to_item(ix, ScrollStrategy::Nearest),
        }
    }

    /// Arrow keys in the list: the cursor moves, nothing opens.
    pub(super) fn move_cursor(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = self.list_len();
        if len == 0 {
            return;
        }
        let from = self.sidebar_state.cursor;
        let mut steps: Box<dyn Iterator<Item = usize>> = match (from, down) {
            (None, true) => Box::new(0..len),
            (None, false) => Box::new((0..len).rev()),
            (Some(at), true) => Box::new(at + 1..len),
            (Some(at), false) => Box::new((0..at).rev()),
        };
        let Some(next) = steps.find(|ix| self.stoppable(*ix)).or(from) else {
            return;
        };
        self.sidebar_state.cursor = Some(next);
        self.sidebar_state.hold.key_moved(Instant::now());
        self.settle_later(super::sidebar::hold::KEY_HOLD, cx);
        self.scroll_to(next);
        cx.notify();
    }

    /// The conversation of the list item at `ix`, in whichever list is on show.
    fn listed_at(&self, ix: usize) -> Option<Rc<Conversation>> {
        let state = &self.sidebar_state;
        match state.mode {
            Mode::Inbox => state.inbox_items.get(ix)?.conversation().cloned(),
            _ if state.listed() => state.home_items.get(ix)?.conversation().cloned(),
            _ => state.rows.get(ix)?.conversation().cloned(),
        }
    }

    /// Where the open conversation (or else the cursor) is in the list on show.
    fn list_at(&self) -> Option<usize> {
        let open = self.open.as_ref();
        let at = open.and_then(|open| (0..self.list_len()).find(|ix| self.listed_at(*ix).is_some_and(|c| &c.key == open)));
        at.or(self.sidebar_state.cursor)
    }

    /// Puts the cursor on the open conversation's row, or the first row, when it is nowhere.
    pub(super) fn cursor_to_open(&mut self) {
        if self.sidebar_state.cursor.is_some() {
            return;
        }
        let at = self.list_at().or_else(|| (0..self.list_len()).find(|ix| self.stoppable(*ix)));
        self.sidebar_state.cursor = Some(at.unwrap_or(0));
    }

    /// Return on the cursor's row.
    pub(super) fn open_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.sidebar_state.cursor else {
            return;
        };
        let state = &self.sidebar_state;
        match state.mode {
            Mode::Inbox => self.activate_inbox(ix, cx),
            _ if state.listed() => self.activate_home(ix, window, cx),
            _ => self.activate(ix, cx),
        }
    }

    /// Opens the conversation at `ix` of the list on show, as a click on it would.
    #[cfg_attr(not(feature = "gui-shot"), allow(dead_code))]
    pub(super) fn open_listed(&mut self, ix: usize, cx: &mut Context<Self>) {
        let rows: Vec<Rc<Conversation>> = (0..self.list_len()).filter_map(|at| self.listed_at(at)).collect();
        if let Some(row) = rows.get(ix) {
            self.select(row.key.clone(), &row.title, cx);
        }
    }

    /// Left and right on the cursor's row: collapse or open a group.
    pub(super) fn fold_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        if state.listed() || state.mode == Mode::Inbox {
            return;
        }
        if let Some(Row::Group { space, id, collapsed, .. }) = state.cursor.and_then(|ix| state.rows.get(ix)).cloned() {
            if collapsed == open {
                self.collapse(space, id, !open, cx);
            }
        }
    }

    /// Opens a row the user chose by hand: the unread walk starts over from there.
    /// A row's press. On the desktop it acts at once, as every list row does. On a phone it acts
    /// on release, when the finger lifts where it went down (a drag past the slop is a scroll),
    /// and resting on a row with a `menu` opens that menu instead.
    pub(in crate::gui) fn tap(
        &self,
        row: Stateful<Div>,
        token: u64,
        menu: Option<Rc<Conversation>>,
        act: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        if !super::phone::active() {
            return row.on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| act(this, window, cx)));
        }
        // A finger's long press: the timer below never starts for one, as its mouse-down only
        // comes when it lifts.
        let held = menu.clone().map(|row| {
            super::phone::long_press(cx.listener(move |this, event: &gpui::LongPressEvent, _, cx| {
                this.phone_state.press_cancel();
                this.open_menu(Menu::Row(row.clone(), event.start_position), cx);
            }))
        });
        row.relative().children(held).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                let menu = menu.clone().map(|row| Menu::Row(row, event.position));
                this.phone_press(token, super::phone::at(event.position), menu, cx);
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, event: &gpui::MouseUpEvent, window, cx| {
                if this.phone_state.press_up(token, super::phone::at(event.position)) {
                    act(this, window, cx);
                }
            }),
        )
    }

    fn pick_row(&mut self, row: &Conversation, cx: &mut Context<Self>) {
        self.sidebar_state.walked.clear();
        if row.key.network == Network::Discord && row.kind.is_direct() && matches!(&self.sidebar_state.mode, Mode::Space(space) if space.network == Network::Discord) {
            self.go(Mode::Network(Network::Discord), cx);
        }
        if row.kind == ConversationKind::Forum && row.key.network == Network::Discord {
            // The main pane is the post list. The sidebar still drills into the posts.
            self.drill(home::Drill::Topics(Rc::new(row.clone())), cx);
            self.select(row.key.clone(), &row.title, cx);
            return;
        }
        if row.kind == ConversationKind::Forum && row.key.network == Network::Telegram {
            self.drill(home::Drill::Topics(Rc::new(row.clone())), cx);
            // The topics drill reads what the store holds; opening the forum asks once for
            // the landing. Telegram lists its topics only on demand, and lands on the newest
            // unread topic, else General (`03` §2). The network catching up behind a later
            // open cannot loop a change back into a request.
            let forum = row.key.clone();
            // Only this visit may choose the landing. Opening another forum (including this
            // same one again), picking a topic, or leaving the drill supersedes its reply.
            let navigation = self.sidebar_state.navigation;
            let requested_forum = forum.clone();
            let landing = move || ask(&Request::Topics {
                forum,
                offset: 0,
                limit: 1,
                landing: true,
                sort: protocol::TopicSort::Activity,
                tag: None,
                query: None,
            });
            self.off_thread(cx, landing, move |this, answer, cx| {
                let Ok(Response::Conversations { conversations, .. }) = answer else {
                    return;
                };
                if let Some(topic) = this.sidebar_state.landing_topic(navigation, &requested_forum, &conversations) {
                    this.select(topic.key.clone(), &topic.title, cx);
                }
            });
            return;
        }
        self.select(row.key.clone(), &row.title, cx);
    }

    /// `alt-down` and `alt-up` open the next or previous row of the list on show.
    pub(super) fn walk(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = self.list_len();
        let at = self.list_at();
        let mut steps: Box<dyn Iterator<Item = usize>> = match (at, down) {
            (None, _) => Box::new(0..len),
            (Some(at), true) => Box::new(at + 1..len),
            (Some(at), false) => Box::new((0..at).rev()),
        };
        let found = steps.find_map(|ix| self.listed_at(ix).map(|row| (ix, row)));
        if let Some((ix, row)) = found {
            self.sidebar_state.cursor = Some(ix);
            self.scroll_to(ix);
            self.pick_row(&row, cx);
        }
    }

    /// The stops of the unread walk: the Inbox, then every Space in the rail's network order.
    fn walk_stops(&self) -> Vec<(Mode, bool)> {
        let counts = self.sidebar_state.counts;
        let inbox = (Mode::Inbox, counts.unread > 0 || counts.mentions > 0);
        let spaces = super::layout::NETWORK_ORDER.into_iter().flat_map(|network| self.spaces.iter().filter(move |space| space.key.network == network));
        std::iter::once(inbox).chain(spaces.map(|space| (Mode::Space(space.key.clone()), space.attention > 0 || space.unread))).collect()
    }

    /// `alt-shift-down` and `alt-shift-up`: the next or previous unread conversation. Past the
    /// end of the list on show it goes on into the next place with something unread, the
    /// sidebar following, until nothing unread is left.
    pub(super) fn walk_unread(&mut self, down: bool, cx: &mut Context<Self>) {
        self.sidebar_state.walking = None;
        match self.sidebar_state.mode.clone() {
            Mode::Inbox => self.walk_inbox(down, true, cx),
            Mode::Space(_) | Mode::Network(Network::Slack) => {
                let rows = self.sidebar_state.rows.clone();
                match self.tree_unread(&rows, self.list_at(), down) {
                    Some(ix) => self.walk_to_row(ix, cx),
                    None => self.walk_on(down, cx),
                }
            }
            Mode::Network(_) | Mode::People => self.walk_on(down, cx),
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

    /// The Inbox's unread rows are read from the store, not from what is drawn: the next one
    /// may be anywhere. `onward`: with none left here, go on to the next stop.
    fn walk_inbox(&mut self, down: bool, onward: bool, cx: &mut Context<Self>) {
        let request = Request::Inbox { network: None, filter: InboxFilter::Unread, offset: 0, limit: PAGE_LIMIT };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            let Ok(Response::Conversations { conversations, .. }) = answer else {
                return;
            };
            let seen: Vec<bool> = conversations.iter().map(|c| this.sidebar_state.walked.contains(&c.key) || c.bot).collect();
            let at = this.open.as_ref().and_then(|open| conversations.iter().position(|c| &c.key == open));
            match walk::step(&seen, at, down) {
                Some(ix) => {
                    if this.sidebar_state.mode != Mode::Inbox {
                        this.remember();
                        this.set_mode(Mode::Inbox, cx);
                    }
                    let key = &conversations[ix].key;
                    let held = (0..this.sidebar_state.inbox_items.len()).find(|at| this.listed_at(*at).is_some_and(|c| &c.key == key));
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

    /// The people whose presence these rows draw, and the rows themselves for their previews.
    fn watch_rows<'a>(&self, rows: impl Iterator<Item = &'a Rc<Conversation>> + Clone, cx: &mut Context<Self>) {
        let hub = self.people_state.hub();
        let home = &self.sidebar_state.home_items;
        let visible = on_screen(&self.sidebar_state.home_list, home.len(), |ix| home[ix].height());
        let extra = home[visible].iter().filter_map(home::Item::conversation).filter(|_| matches!(&self.sidebar_state.mode, Mode::Space(space) if space.network == Network::Discord));
        let rows = rows.cloned().chain(extra.cloned());
        let peers = rows.clone().filter_map(|c| Some(Key { id: c.peer.clone()?, ..c.key.clone() }));
        hub.watch(WATCH, peers, cx);
        hub.watch_rows(WATCH, rows.map(|c| c.key.clone()), cx);
    }

    /// One row of a tree: 28 tall, whatever it is.
    fn tree_row(&self, ix: usize, row: &Row, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        // A spacer rather than padding: the row sets its own.
        // 16 a level, less the row's gap, which follows it; none at the top, so a top-level
        // row starts on the 16 pt text edge (C10) with the Threads row and the headers.
        let indent = |depth: u8, gap: f32| (depth > 0).then(|| div().flex_none().w(px(16. * f32::from(depth) - gap)));
        let height = theme::row_height(theme::ROW_ONE);
        match row {
            Row::Loading { depth } => {
                let widths = [0.60, 0.45, 0.70, 0.50, 0.65, 0.40];
                let bar = div().h(px(8.)).w(gpui::relative(widths[ix % widths.len()])).rounded(px(theme::R_CHIP)).bg(theme::raised());
                list_row(("loading", ix), height, false, false, div().children(indent(*depth, 8.)).gap(px(8.)).child(skeleton(20., 20.)).child(bar)).cursor_default()
            }
            Row::Unreads { count, mention, .. } => {
                let trailing = match (*count, *mention) {
                    (0, _) => None,
                    (count, true) => Some(badge_mention(count)),
                    (count, false) => Some(super::components::badge_count_unread(count)),
                };
                self.action_row(("unreads", ix), Some("icons/inbox.svg"), "Unreads".into(), *count > 0, trailing, focused)
                    .map(|row| self.tap(row, super::phone::token(&["tree", &ix.to_string()]), None, move |this, _, cx| this.activate(ix, cx), cx))
            }
            Row::Threads { count, mention, .. } => {
                let trailing = match (*count, *mention) {
                    (0, _) => None,
                    (count, true) => Some(badge_mention(count)),
                    (count, false) => Some(super::components::badge_count_unread(count)),
                };
                self.action_row(("threads", ix), Some("icons/messages-square.svg"), "Threads".into(), *count > 0, trailing, focused)
                    .map(|row| self.tap(row, super::phone::token(&["tree", &ix.to_string()]), None, move |this, _, cx| this.activate(ix, cx), cx))
            }
            Row::Drafts { count, .. } => {
                let trailing = (*count > 0).then(|| super::components::badge_count_unread(*count));
                self.action_row(("drafts", ix), Some("icons/pencil.svg"), "Drafts & sent".into(), false, trailing, focused)
                    .map(|row| self.tap(row, super::phone::token(&["tree", &ix.to_string()]), None, move |this, _, cx| this.activate(ix, cx), cx))
            }
            Row::Fold { words, depth, .. } => {
                let content = div()
                    .children(indent(*depth, 8.))
                    .gap(px(8.))
                    .child(div().size(px(20.)).flex_none().flex().items_center().justify_center().child(glyph("icons/ellipsis.svg", 14., theme::text_faint())))
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text_faint()).child(words.clone()));
                list_row(("fold", ix), height, false, focused, content)
                    .map(|row| self.tap(row, super::phone::token(&["tree", &ix.to_string()]), None, move |this, _, cx| this.activate(ix, cx), cx))
            }
            Row::More { words, depth } => {
                let content = div().children(indent(*depth, 0.)).child(div().pl(px(28.)).type_small().text_color(theme::text_faint()).child(words.clone()));
                list_row(("more", ix), height, false, false, content).cursor_default()
            }
            Row::Occupant { occupant, depth } => {
                let face = occupant
                    .avatar
                    .as_ref()
                    .and_then(|file| self.people_state.hub().file(&occupant.person.account(), file, cx).path.map(std::path::PathBuf::from))
                    .or_else(|| self.person_picture(&occupant.person, cx));
                let content = div()
                    .children(indent(*depth, 8.))
                    .gap(px(8.))
                    .child(Avatar::new(&occupant.person, &occupant.name, 20.).image_at(face).build())
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child(occupant.name.clone()))
                    .when(occupant.streaming, |row| row.child(glyph("icons/monitor-up.svg", 14., theme::text_soft())))
                    .when(occupant.camera, |row| row.child(glyph("icons/video.svg", 14., theme::text_soft())))
                    .when(occupant.server_muted, |row| row.child(glyph("icons/mic-off.svg", 14., theme::danger())))
                    .when(!occupant.server_muted && occupant.suppressed, |row| row.child(glyph("icons/mic-off.svg", 14., theme::warning())))
                    .when(!occupant.server_muted && !occupant.suppressed && occupant.muted && !occupant.deafened && !occupant.server_deafened, |row| {
                        row.child(glyph("icons/mic-off.svg", 14., theme::text_faint()))
                    })
                    .when(occupant.server_deafened, |row| row.child(glyph("icons/headphone-off.svg", 14., theme::danger())))
                    .when(!occupant.server_deafened && occupant.deafened, |row| row.child(glyph("icons/headphone-off.svg", 14., theme::text_faint())));
                list_row(row_id("occupant", &occupant.person), height, false, false, content)
                    .map(|row| self.tap(row, super::phone::token(&["tree", &ix.to_string()]), None, move |this, _, cx| this.activate(ix, cx), cx))
            }
            Row::Group { space, id, name, collapsed, muted, rollup, depth, .. } => {
                let lit = *collapsed && (rollup.attention > 0 || rollup.unread);
                let dot = rollup_badge(rollup.attention, rollup.unread).filter(|_| *collapsed);
                let chevron = if *collapsed { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" };
                let group = SharedString::from(format!("group-{id}"));
                let can_create = self.spaces.iter().any(|s| &s.key == space && s.can_create_channels);
                let add_space = space.clone();
                let add_group = id.clone();
                let menu_space = space.clone();
                let menu_id = id.clone();
                let menu_name = name.clone();
                let menu_muted = *muted;
                let add_btn = can_create.then(|| {
                    div()
                        .id((gpui::ElementId::from("group-add"), id.clone()))
                        .size(px(18.))
                        .rounded(px(theme::R_CONTROL))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(tip("Create channel…", None))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_create_channel(add_space.clone(), Some(add_group.clone()), cx);
                        }))
                        .child(glyph("icons/plus.svg", 12., theme::text_faint()))
                });
                let content = div()
                    .group(group.clone())
                    .children(indent(*depth, 4.))
                    .gap(px(4.))
                    .child(div().min_w_0().truncate().type_micro().text_color(if lit { theme::text() } else { theme::text_faint() }).child(name.clone()))
                    .child(glyph(chevron, 12., theme::text_faint()).when(!*collapsed, |chevron| chevron.invisible().group_hover(group, |style| style.visible())))
                    .child(div().flex_1())
                    .children(add_btn)
                    .children(dot);
                list_row((gpui::ElementId::from("group"), id.clone()), height, false, focused, content)
                    .map(|row| self.tap(row, super::phone::token(&["tree", &ix.to_string()]), None, move |this, _, cx| this.activate(ix, cx), cx))
                    .when(space.network == Network::Discord, |row| {
                        row.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                this.open_menu(
                                    Menu::Group {
                                        space: menu_space.clone(),
                                        id: menu_id.clone(),
                                        name: menu_name.clone(),
                                        muted: menu_muted,
                                        at: event.position,
                                    },
                                    cx,
                                );
                            }),
                        )
                    })
            }
            Row::Conversation { conversation: c, depth } => self.place_row(c, *depth, focused, cx),
        }
    }

    /// The hover Join pill of a voice row: the header pill's look, shrunk to the row (07 §1).
    /// Invisible until the row is hovered, so a plain view of the list stays quiet.
    pub(super) fn join_pill(&self, conversation: &Conversation, cx: &mut Context<Self>) -> Stateful<Div> {
        let key = conversation.key.clone();
        let title = conversation.title.clone();
        let group_call = conversation.kind == ConversationKind::Group;
        let hover_group = if group_call { "group-call" } else { "voice" };
        let icon = if group_call { "icons/phone.svg" } else { "icons/volume-2.svg" };
        div()
            .id(row_id("join", &key))
            .flex_none()
            .h(px(20.))
            .px(px(8.))
            .rounded(px(10.))
            .border_1()
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .bg(theme::raised())
            .border_color(theme::hairline())
            .text_color(theme::text())
            .hover(|style| style.bg(theme::selected()))
            .tooltip(tip(
                format!("Join {}. People there will see you arrive.", title),
                None,
            ))
            // A phone has no hover: there the pill is always on the row, at a thumb's size.
            .map(|pill| {
                if super::phone::active() {
                    pill.h(px(30.)).px(px(12.)).rounded(px(15.)).text_size(px(13.))
                } else {
                    pill.invisible().group_hover(hover_group, |style| style.visible())
                }
            })
            // The press belongs to the pill: the row's open must not fire with it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
            .child(glyph(icon, 12., theme::text()))
            .child("Join")
    }

    /// A conversation in a tree, 28: glyph or avatar 20, the name, then what is going on.
    fn place_row(&self, c: &Rc<Conversation>, depth: u8, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.open.as_ref() == Some(&c.key);
        let tone = row_tone(c, selected, true);
        let surface = theme::row_surface(selected);
        let inside = c.occupants.as_ref().filter(|row| row.total > 0);
        // A stage is Discord's: its row counts who is on stage, not who is in it.
        let stage = c.key.network == Network::Discord && c.kind == ConversationKind::Voice && c.call == CallSupport::Handoff;
        let live = stage.then(|| inside.and_then(stage_line)).flatten();
        // An empty voice channel rests at a muted row's weight, and a stage with nobody on
        // stage reads as one (`02` §4).
        let empty_voice = empty_voice_row(c.kind, stage, selected, inside.is_some(), live.is_some());
        let title_color = if empty_voice { theme::text_faint() } else { tone.title };
        let leading = match c.kind {
            ConversationKind::Direct | ConversationKind::Group => self.conversation_avatar(c, 20., surface, cx).into_any_element(),
            kind => {
                let icon_color = if c.muted || empty_voice { theme::text_faint() } else { theme::text_soft() };
                let icon = if c.key.network == Network::Teams && kind == ConversationKind::Channel && !c.private { None } else { Some(kind_glyph(kind, c.private, c.call)) };
                div().size(px(20.)).flex_none().flex().items_center().justify_center().children(icon.map(|icon| glyph(icon, if kind == ConversationKind::Thread { 14. } else { 16. }, icon_color))).into_any_element()
            }
        };
        let status = c
            .peer
            .as_ref()
            .filter(|_| c.kind == ConversationKind::Direct && c.key.network == Network::Slack)
            .and_then(|peer| self.people_state.hub().person(&Key { id: peer.clone(), ..c.key.clone() }))
            .map(|person| person.emoji)
            .filter(|emoji| !emoji.is_empty());
        // The `4/12` is a native voice channel. A stage trails "Live · n speaking" instead.
        let count = if stage {
            None
        } else {
            inside.and_then(|row| voice_count(row.total, if c.call == CallSupport::Native { c.limit } else { 0 }))
        };
        let draft = (!self.draft_of(c).is_empty() && !selected).then(|| glyph("icons/pencil.svg", 14., theme::text_faint()));
        let (picked, menu_row) = (c.clone(), c.clone());
        let ours = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.call.as_ref())
            .map_or(false, |call| {
                call.conversation == c.key
                    && !matches!(call.state, protocol::CallState::Failed { .. })
            });
        let voice_join = joins_from_list(c) && !ours;
        let group_join = c.kind == ConversationKind::Group && c.call == CallSupport::Native && inside.is_some() && !ours;
        let show_join = voice_join || group_join;
        let hover_group = if c.kind == ConversationKind::Group { "group-call" } else { "voice" };
        let count_icon = if c.kind == ConversationKind::Group { "icons/phone.svg" } else { "icons/volume-2.svg" };
        let content = div()
            // 16 a level less the gap after it (none at the top), as the tree's other rows.
            .children((depth > 0).then(|| div().flex_none().w(px(16. * f32::from(depth) - 8.))))
            .gap(px(8.))
            .child(leading)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .type_ui()
                    .font_weight(tone.weight)
                    .text_color(title_color)
                    .child(tree_title(c)),
            )
            .children(status.map(|emoji| div().flex_none().type_small().child(emoji)))
            .children(chat::name_mark(c).filter(|_| c.kind.is_direct()))
            .child(div().flex_1())
            .children(show_join.then(|| self.join_pill(c, cx)))
            .children(draft)
            .children(count.map(|(count, soft)| div().flex_none().flex().items_center().gap(px(2.)).child(glyph(count_icon, 12., theme::text_faint())).child(div().type_micro().text_color(if soft { theme::text_soft() } else { theme::text_faint() }).child(count))))
            .children(live.map(|line| div().flex_none().type_micro().text_color(theme::text_faint()).child(line)))
            .children(row_badge(c))
            .when(show_join, |row| row.group(hover_group));
        list_row(row_id("tree", &c.key), theme::row_height(theme::ROW_ONE), selected, focused, content)
            .map(|row| {
                if super::phone::active() {
                    let token = super::phone::token(&["place", picked.key.network.label(), &picked.key.account, &picked.key.id]);
                    let menu = Some(picked.clone());
                    // A voice channel is a place to be, not to read: a tap joins it, as the
                    // desktop's double-click does, and the call screen comes up. Once I am in
                    // it, a tap brings that screen back. Its chat is the call screen's.
                    let voice = picked.kind == ConversationKind::Voice;
                    return self.tap(
                        row,
                        token,
                        menu,
                        move |this, _, cx| {
                            if voice && ours {
                                this.call_phone_restore(cx);
                            } else if voice_join {
                                this.call_start(picked.key.clone(), cx);
                            } else {
                                this.pick_row(&picked, cx);
                            }
                        },
                        cx,
                    );
                }
                row.on_mouse_down(
                    MouseButton::Left,
                    // A voice row joins on a double-click; a single click only opens its chat.
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        if crate::gui::conversation::modifier_click(event.click_count, event.modifiers) {
                            this.pop_out_conversation(picked.key.clone(), cx);
                            return;
                        }
                        // The first press of that double-click must not open the row.
                        if !crate::gui::conversation::press_opens_row(crate::gui::conversation::modifier_held(event.modifiers)) {
                            return;
                        }
                        if voice_join && event.click_count == 2 {
                            this.call_start(picked.key.clone(), cx);
                        } else {
                            this.pick_row(&picked, cx);
                        }
                    }),
                )
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.open_menu(Menu::Row(menu_row.clone(), event.position), cx);
                }),
            )
    }

    fn tree_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let rows = self.sidebar_state.rows.clone();
        let visible = rows.get(range.start.min(rows.len())..range.end.min(rows.len())).unwrap_or_default();
        self.watch_rows(visible.iter().filter_map(|row| row.conversation()), cx);
        // The shell's own focus is the list's too: arrows move the cursor when no field has them.
        let focused = self.sidebar_state.focus.is_focused(window) || self.focus.is_focused(window);
        let cursor = self.sidebar_state.cursor.filter(|_| focused);
        range.filter_map(|ix| Some(self.tree_row(ix, rows.get(ix)?, cursor == Some(ix), cx))).collect()
    }

    /// What a pane says while a link is not normal. Healthy is silent, and a pane speaks only
    /// for the accounts it shows: a workspace's tree says nothing of another workspace. The
    /// window's own banner (chatd, the keychain) comes first; then the most urgent account's.
    fn banners(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<Div> {
        if let Some(window) = self.window_banner(cx) {
            return vec![window];
        }
        let mode = &self.sidebar_state.mode;
        let mut found: Vec<(AccountKey, Tone, String)> = Vec::new();
        for account in &snapshot.accounts {
            let network = account.key.network;
            let here = match mode {
                Mode::Space(space) => space.account() == account.key,
                Mode::Network(Network::Slack) => self.slack_space().is_none_or(|space| space.account() == account.key),
                Mode::Network(shown) => *shown == network,
                Mode::Inbox => true,
                Mode::People => false,
            };
            if !here {
                continue;
            }
            let words = banner_words(network, snapshot.accounts_of(network) > 1, account, *mode == Mode::Inbox, || {
                self.trouble.get(&account.key).map(|since| since.elapsed()).unwrap_or_default() >= TROUBLE_SHOWN
            });
            if let Some((tone, words)) = words {
                found.push((account.key.clone(), tone, words));
            }
        }
        // One per pane: the most urgent, not the first account's.
        found.sort_by_key(|(_, tone, _)| urgency(*tone));
        found
            .into_iter()
            .take(1)
            .map(|(account, tone, words)| {
                let network = account.network;
                match tone {
                Tone::Danger => banner(tone, words).child(
                    banner_action("sign-in-again", "Sign in").on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx))),
                ),
                Tone::Attention if slack_can_continue(network, &words) => {
                    let id = account.account.clone();
                    banner(tone, words).child(
                        banner_action("continue-slack", "Continue in tinyclient").on_click(cx.listener(move |this, _, _, cx| {
                            this.continue_slack(id.clone(), cx);
                        })),
                    )
                }
                Tone::Attention if network == Network::Slack => banner(tone, words),
                Tone::Attention => banner(tone, words).child(
                    banner_action("open-network", format!("Open {}", network.label())).on_click(cx.listener(move |this, _, _, cx| {
                        if network == Network::Tiny { this.tiny_reconnect(account.clone(), cx); }
                        else { cx.open_url(network_app(network)); }
                    })),
                ),
                tone => banner(tone, words),
            }})
            .collect()
    }

    /// `All · Unread n · Mentions n`: counts in faint numerals after the label, hidden at 0
    /// or when `show_counts` is false (narrow strip).
    fn segments(
        &self,
        choices: &[(&'static str, &'static str, u32, bool)],
        show_counts: bool,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Shell, usize, &mut Context<Shell>) + Clone + 'static,
    ) -> Div {
        let mut out = segmented();
        for (ix, (id, label, count, on)) in choices.iter().copied().enumerate() {
            let pick = pick.clone();
            let counted = (count > 0 && show_counts).then(|| {
                let words = if count > 99 { "99+".to_owned() } else { count.to_string() };
                div().ml(px(5.)).text_size(px(11.)).font_weight(theme::MEDIUM).text_color(if on { theme::text() } else { theme::text_faint() }).child(words)
            });
            let mention_tip = (label == "Mentions" && count > 0).then(|| {
                if count == 1 { "1 conversation mentions you".to_owned() } else { format!("{count} conversations mention you") }
            });
            out = out.child(
                segment(id, label, on)
                    .children(counted)
                    .when_some(mention_tip, |segment, words| segment.tooltip(tip(words, None)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| pick(this, ix, cx))),
            );
        }
        out
    }

    fn title_strip(&self, mode: &Mode, cx: &mut Context<Self>) -> Div {
        let strip = drags_window(div()).h(px(theme::STRIP)).flex_none().pl(px(theme::PANE_PAD)).pr(px(chrome::end_pad(End::List, 12.))).flex().items_center().gap(px(8.));
        let quiet = |button: Stateful<Div>| button.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let back = |id: &'static str, words: String, cx: &mut Context<Self>| {
            quiet(icon_button(id, "icons/chevron-left.svg", false))
                .tooltip(tip(words, Some("⌘↑")))
                .on_click(cx.listener(|this, _, _, cx| this.up_level(cx)))
        };
        if let Some(drill) = &self.sidebar_state.drill {
            let new_post = match drill {
                home::Drill::Topics(forum) if matches!(forum.key.network, Network::Discord | Network::Telegram) && !forum.read_only => {
                    let forum = Rc::clone(forum);
                    Some(
                        quiet(icon_button("new-post", "icons/plus.svg", false))
                            .tooltip(tip("New post", None))
                            .on_click(cx.listener(move |this, _, _, cx| this.begin_forum_post(&forum, cx))),
                    )
                }
                _ => None,
            };
            let title = match drill {
                home::Drill::Unreads(_) => "All unreads".to_owned(),
                home::Drill::Threads(_) => "Threads".to_owned(),
                home::Drill::Drafts(_) => "Drafts & sent".to_owned(),
                home::Drill::Archive => "Archive".to_owned(),
                home::Drill::Calls => "Calls".to_owned(),
                home::Drill::Requests => "Message requests".to_owned(),
                home::Drill::Topics(forum) => forum.title.clone(),
                home::Drill::Discover => "Discover".to_owned(),
            };
            let field = matches!(drill, home::Drill::Discover).then(|| self.sidebar_state.discover_input.clone()).flatten();
            let place = match drill {
                home::Drill::Unreads(Some(space)) | home::Drill::Threads(space) | home::Drill::Drafts(space) => {
                    self.spaces.iter().find(|s| &s.key == space).map(|s| s.name.clone())
                }
                _ => None,
            };
            return strip
                .pl(px(10.))
                .child(back("drill-up", "Back".into(), cx))
                .child(pane_title(title))
                .children(place.map(|place| div().flex_none().max_w(px(120.)).truncate().type_small().text_color(theme::text_faint()).child(place)))
                .children(new_post)
                .when_some(field, |strip, input| {
                    strip.child(
                        div()
                            .w(px(180.))
                            .flex_none()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::canvas())
                            .border_1()
                            .border_color(theme::border())
                            .child(input),
                    )
                });
        }
        match mode {
            Mode::Inbox => {
                let filter = self.sidebar_state.filter;
                let counts = self.sidebar_state.counts;
                let choices = [
                    ("filter-all", "All", 0, filter == InboxFilter::All),
                    ("filter-unread", "Unread", counts.unread, filter == InboxFilter::Unread),
                    ("filter-mentions", "Mentions", counts.mentions, filter == InboxFilter::Mentions),
                    ("filter-activity", "Activity", counts.activity, filter == InboxFilter::Activity),
                ];
                let pick = |this: &mut Shell, ix: usize, cx: &mut Context<Shell>| {
                    this.set_filter([InboxFilter::All, InboxFilter::Unread, InboxFilter::Mentions, InboxFilter::Activity][ix], cx);
                };
                let has_unread = counts.unread > 0 || counts.mentions > 0;
                let fit = inbox_strip_fit(
                    self.sidebar_state.width.get(),
                    super::phone::active(),
                    if super::phone::active() { super::phone::tokens::text_scale() } else { 1. },
                    has_unread,
                );
                // Fixed label: never use pane_title (flex_1 + truncate) for "Inbox".
                let title = div().flex_none().type_title().text_color(theme::text()).child("Inbox");
                let mark = fit.mark_all.then(|| {
                    quiet(icon_button("mark-all", "icons/check-check.svg", false))
                        .tooltip(tip("Mark all as read", None))
                        .on_click(cx.listener(|this, _, _, cx| this.mark_all_read(cx)))
                });
                let segments = self.segments(&choices, fit.counts, cx, pick);
                if fit.stacked {
                    return div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .child(strip.child(title).child(div().flex_1()).children(mark))
                        .child(div().flex_none().px(px(theme::PANE_PAD)).pb(px(8.)).flex().child(segments));
                }
                strip.child(title).child(segments).child(div().flex_1()).children(mark)
            }
            Mode::People => strip.child(pane_title("People")),
            Mode::Network(Network::Slack) => self.slack_title(strip, cx),
            // A Tiny server's strip is its header: its icon and name, which open its menu.
            Mode::Network(Network::Tiny) if self.snapshot.as_ref().is_some_and(|snapshot| snapshot.link(Network::Tiny) == Link::Connected) => {
                strip.child(self.tiny_header(cx))
            }
            Mode::Network(network) => {
                let network = *network;
                let strip = strip.child(network_icon(network, 20.));
                match network {
                    Network::Tiny if self.snapshot.as_ref().is_some_and(|snapshot| snapshot.accounts_of(Network::Tiny) > 1) => {
                        let name = self
                            .tiny_account()
                            .and_then(|key| self.snapshot.as_ref().and_then(|snapshot| snapshot.account(key).map(|account| account.name.clone())))
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| network.label().to_owned());
                        strip.child(
                            div()
                                .id("network-name")
                                .flex_1()
                                .min_w_0()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, _, cx| this.open_menu(Menu::Network(network), cx)))
                                .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(name))
                                .child(glyph("icons/chevron-down.svg", 12., theme::text_faint())),
                        )
                    }
                    Network::Discord => {
                        let unread = self.sidebar_state.home_unread;
                        let choices = [("home-all", "All", 0, !unread), ("home-unread", "Unread", self.sidebar_state.home_news, unread)];
                        let strip = strip.child(
                            div()
                                .id("network-name")
                                .flex_1()
                                .min_w_0()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, _, cx| this.open_menu(Menu::Network(network), cx)))
                                .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(network.label()))
                                .child(glyph("icons/chevron-down.svg", 12., theme::text_faint())),
                        );
                        strip.child(self.segments(&choices, self.sidebar_state.width.get() >= 300., cx, |this, ix, cx| this.set_home_unread(ix == 1, cx)))
                    }
                    _ => strip.child(pane_title(network.label())).child(
                        quiet(icon_button("network-menu", "icons/ellipsis.svg", false))
                            .tooltip(tip("Accounts", None))
                            .on_click(cx.listener(move |this, _, _, cx| this.open_menu(Menu::Network(network), cx))),
                    ),
                }
            }
            Mode::Space(key) => {
                let space = self.spaces.iter().find(|space| &space.key == key);
                let name = space.map_or_else(String::new, |space| space.name.clone());
                let menu_key = key.clone();
                let icon = space.and_then(|space| self.media_path(&space.key, space.icon.as_ref()));
                strip
                    .pl(px(10.))
                    .child(back("space-up", format!("Back to {}", key.network.label()), cx))
                    .child(Avatar::new(key, &name, 20.).shape(Shape::Place).image_at(icon).build())
                    .child(
                        div()
                            .id("space-menu")
                            .flex_1()
                            .min_w_0()
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| this.open_menu(Menu::Space(menu_key.clone()), cx)))
                            .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(name))
                            .child(glyph("icons/chevron-down.svg", 12., theme::text_faint())),
                    )
            }
        }
    }

    /// The Slack home's strip: the workspace is the title and its switcher (`02` §5).
    fn slack_title(&self, strip: Div, cx: &mut Context<Self>) -> Div {
        let Some(key) = self.slack_space() else {
            return strip.child(network_icon(Network::Slack, 20.)).child(pane_title("Slack"));
        };
        let space = self.spaces.iter().find(|s| s.key == key);
        let name = space.map_or_else(|| "Slack".to_owned(), |s| s.name.clone());
        let icon = space.and_then(|space| self.media_path(&space.key, space.icon.as_ref()));
        let others: Vec<&protocol::Space> = self.spaces.iter().filter(|s| s.key.network == Network::Slack && s.key != key).collect();
        // The filled pill is for mentions alone (C6); a Space's `attention` also counts its DMs.
        let mentions = self.slack_mentions(&others);
        let attention = others.iter().map(|s| s.attention).sum::<u32>();
        let elsewhere = match (mentions, attention, others.iter().any(|s| s.unread)) {
            (0, 0, false) => None,
            (0, 0, true) => Some(badge_dot(8., theme::unread())),
            (0, count, _) => Some(super::components::badge_count_unread(count)),
            (count, _, _) => Some(badge_mention(count)),
        };
        let unread = self.sidebar_state.home_unread;
        let news = self.trees.get(&key).map_or(0, |tree| tree.conversations.iter().filter(|c| c.badge() != Badge::None).count() as u32);
        let choices = [("slack-all", "All", 0, !unread), ("slack-unread", "Unread", news, unread)];
        strip
            .child(
                div()
                    .id("slack-workspace")
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.slack_menu = !this.sidebar_state.slack_menu;
                        cx.notify();
                    }))
                    .child(Avatar::new(&key, &name, 20.).shape(Shape::Place).image_at(icon).build())
                    .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(name))
                    .child(glyph("icons/chevron-down.svg", 12., theme::text_faint()))
                    .children(elsewhere),
            )
            .child(self.segments(&choices, self.sidebar_state.width.get() >= 300., cx, |this, ix, cx| this.set_home_unread(ix == 1, cx)))
    }

    /// The mentions of me in these workspaces, from the rows the shell holds: their trees where
    /// read, and the Inbox. Each conversation once.
    fn slack_mentions(&self, spaces: &[&protocol::Space]) -> u32 {
        let inside = |c: &&Rc<Conversation>| spaces.iter().any(|s| s.key.account == c.key.account && c.space.as_deref().is_none_or(|id| id == s.key.id));
        let trees = spaces.iter().filter_map(|s| self.trees.get(&s.key)).flat_map(|tree| tree.conversations.iter());
        let mut seen = HashSet::new();
        trees.chain(self.sidebar_state.inbox.all().iter()).filter(inside).filter(|c| seen.insert(c.key.clone())).map(|c| c.mentions).sum()
    }

    /// The Slack header's menu: the workspaces, then signing out of this one.
    // CONTRACT: status, pause notifications, away and "Open in Slack" have no request behind
    // them in the protocol, so the menu holds the workspaces and sign-out only.
    fn slack_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.sidebar_state.slack_menu {
            return None;
        }
        let current = self.slack_space();
        let snapshot = self.snapshot.as_ref()?;
        let phone = super::phone::active();
        let mut menu = if phone { div().w_full().flex().flex_col() } else { super::components::menu().w(px(280.)) };
        let workspaces: Vec<&protocol::Space> = self.spaces.iter().filter(|s| s.key.network == Network::Slack).collect();
        if workspaces.len() > 1 {
            menu = menu.child(super::components::menu_label("Workspaces"));
            for (ix, space) in workspaces.iter().enumerate() {
                let key = space.key.clone();
                let link = snapshot.account_link(&space.key);
                let words = (link != Link::Connected).then(|| super::components::link_word(link));
                let row = div()
                    .id(("workspace", ix))
                    .h(px(if phone { theme::row_height(theme::ROW_ONE) } else { 28. }))
                    .px(px(8.))
                    .mx(px(4.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .when(current.as_ref() == Some(&space.key), |row| row.bg(theme::selected()))
                    .when(!phone, |row| row.hover(|style| style.bg(theme::hover())))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sidebar_state.slack = Some(key.clone());
                        this.sidebar_state.slack_menu = false;
                        this.show_mode(cx);
                    }))
                    .child(Avatar::new(&space.key, &space.name, 20.).shape(Shape::Place).image_at(self.media_path(&space.key, space.icon.as_ref())).build())
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(space.name.clone()))
                    .children(words.map(|words| div().type_small().text_color(theme::text_soft()).child(words)))
                    .children(rollup_badge(space.attention, space.unread));
                menu = menu.child(row);
            }
            menu = menu.child(super::components::menu_separator());
        }
        let account = current.as_ref().map(Key::account);
        let is_grid = account.as_ref().is_some_and(|a| a.account.starts_with('E'));
        if is_grid {
            if let Some(org_id) = account.as_ref().map(|a| a.account.clone()) {
                let browse_url = format!("https://app.slack.com/client/{org_id}");
                menu = menu.child(
                    super::components::menu_item("slack-browse-workspaces", None, "Browse workspaces", None).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.sidebar_state.slack_menu = false;
                            cx.open_url(&browse_url);
                            cx.notify();
                        }),
                    ),
                );
            }
        }
        let name = if is_grid {
            account
                .as_ref()
                .and_then(|acc| snapshot.accounts.iter().find(|a| a.key == *acc))
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Slack".to_owned())
        } else {
            current
                .as_ref()
                .and_then(|key| self.spaces.iter().find(|s| &s.key == key))
                .map_or_else(String::new, |s| s.name.clone())
        };
        if let Some(space_key) = current.clone() {
            let team_id = space_key.id.clone();
            let info_key = space_key.clone();
            menu = menu.child(
                super::components::menu_item("slack-workspace-info", Some("icons/info.svg"), "Workspace info", None).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.sidebar_state.slack_menu = false;
                        this.details_space(info_key.clone(), cx);
                    }),
                ),
            );
            menu = menu.child(super::components::menu_separator());
            menu = menu.child(super::components::menu_label("Workspace settings · Opens in your browser"));
            for (ix, &(label, rel)) in super::people::SLACK_WORKSPACE_LINKS.iter().enumerate() {
                let url = super::people::slack_workspace_url("", &team_id, rel);
                menu = menu.child(
                    super::components::menu_item(("slack-admin-link", ix), Some("icons/external-link.svg"), label, None)
                        .tooltip(tip(format!("{label} · Opens in your browser"), None))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.sidebar_state.slack_menu = false;
                            this.open_external_link(&url, window, cx);
                        })),
                );
            }
            menu = menu.child(super::components::menu_separator());
        }
        menu = menu.child(super::components::menu_item_danger("slack-sign-out", Some("icons/log-out.svg"), format!("Sign out of {name}")).on_click(cx.listener(move |this, _, _, cx| {
            this.sidebar_state.slack_menu = false;
            if let Some(account) = account.clone() {
                this.sign_out(account, false, cx);
            }
        })));
        if phone {
            // A phone's menu is a sheet over the whole screen, drawn by `phone_slack_sheet`.
            return Some(menu);
        }
        Some(
            div()
                .absolute()
                .top(px(theme::STRIP - 6.))
                .left(px(12.))
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.sidebar_state.slack_menu = false;
                    cx.notify();
                }))
                .child(menu),
        )
    }

    /// Closes the Slack header's menu. False when it was not open.
    pub(in crate::gui) fn close_slack_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was = std::mem::take(&mut self.sidebar_state.slack_menu);
        if was {
            cx.notify();
        }
        was
    }

    /// Moves a Slack conversation into another sidebar section (`Request::SectionMoveChannel`).
    pub(in crate::gui) fn move_to_section(
        &mut self,
        space: Key,
        conversation: Key,
        from_section: Option<String>,
        to_section: String,
        cx: &mut Context<Self>,
    ) {
        let request = Request::SectionMoveChannel {
            space,
            conversation,
            from_section,
            to_section,
        };
        self.off_thread(
            cx,
            move || crate::client::ask(&request),
            |this, answer, cx| {
                if let Some(why) = refused(&answer) {
                    this.toast(None, why, cx);
                }
            },
        );
    }

    /// The Slack header's menu on a phone: the same rows as a bottom sheet over the screen.
    pub(in crate::gui) fn phone_slack_sheet(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let body = self.slack_menu(cx)?;
        let tall = f32::from(window.viewport_size().height);
        let close = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
            this.sidebar_state.slack_menu = false;
            cx.notify();
        });
        Some(super::phone::sheet("phone-slack-menu", tall, body, close))
    }

    fn set_home_unread(&mut self, unread: bool, cx: &mut Context<Self>) {
        self.sidebar_state.home_unread = unread;
        self.sidebar_state.cursor = None;
        self.rebuild_rows(cx);
    }

    /// The jump strip: a field that opens the switcher, and the compose button. People draws
    /// its own search here instead.
    fn jump_strip(&self, cx: &mut Context<Self>) -> Div {
        // A thumb's field on a phone: 36 at least, and it grows with the text run.
        let phone = super::phone::active();
        let field = if phone { f32::from(theme::line(18.)).max(20.) + 16. } else { theme::CONTROL };
        div()
            .h(px(if phone { field + 12. } else { 44. }))
            .flex_none()
            .px(px(theme::PANE_PAD))
            .pb(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .id("jump")
                    .flex_1()
                    .h(px(field))
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
                    .when(!super::phone::active(), |field| field.child(keycap("⌘K"))),
            )
            .child(
                icon_button("compose", "icons/square-pen.svg", false)
                    .when(phone, |button| button.size(px(field)))
                    .tooltip(tip("New message", Some("⌘N")))
                    .on_click(cx.listener(|this, _, window, cx| this.open_switcher("@", window, cx))),
            )
    }

    fn list(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.sidebar_state;
        let empty = |icon, title: &str, detail: &str| empty_state(icon, title, detail).px(px(24.)).into_any_element();
        match &state.mode {
            Mode::People => div().into_any_element(),
            Mode::Inbox => self.inbox_list(window, cx),
            // Channels, direct messages, rooms, and people. The pane is the invite, a person, or a room.
            Mode::Network(Network::Tiny) => match snapshot.link(Network::Tiny) {
                Link::Connected => self.tiny_directory(cx),
                Link::Offline => empty("icons/wifi-off.svg", "Tiny server is offline", "It shows up here once it connects."),
                _ => empty("icons/users.svg", "Tiny server", "Sign in to see who's here."),
            },
            _ if state.listed() => self.home_list(snapshot, window, cx),
            Mode::Network(network) if state.rows.is_empty() && state.spaces_loaded => {
                let name = network.label();
                match snapshot.link(*network) {
                    Link::Offline => empty("icons/wifi-off.svg", &format!("{name} is offline"), "What was saved shows up here once it connects."),
                    _ => empty("icons/hash.svg", "You're not in any channels yet", &format!("Channels you join in {name} show up here.")),
                }
            }
            Mode::Space(_) if state.rows.is_empty() => empty("icons/hash.svg", "No channels you can read", "Channels you are allowed to see show up here."),
            _ => crate::gui::components::scroll_frame("tree-scroll-frame", state.tree_scroll.clone(), uniform_list("tree", state.rows.len(), cx.processor(Self::tree_rows))
                .track_scroll(&state.tree_scroll)
                .size_full()).keyboard(false)
                .into_any_element(),
        }
    }

    pub(super) fn open_browse_channels(&mut self, space: Key, cx: &mut Context<Self>) {
        let space_name = self
            .spaces
            .iter()
            .find(|s| s.key == space)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Server".to_owned());
        let filter = cx.new(|cx| crate::gui::input::TextInput::new("Filter channels…", cx));
        let initial: Vec<protocol::BrowsedChannel> = self
            .trees
            .get(&space)
            .map(|tree| {
                tree.conversations
                    .iter()
                    .filter(|c| c.kind != ConversationKind::Thread)
                    .map(|c| protocol::BrowsedChannel {
                        key: c.key.clone(),
                        name: c.title.clone(),
                        topic: c.topic.clone(),
                        members: 0,
                        joined: !c.opted_out,
                        private: c.private,
                        archived: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.sidebar_state.browse_channels = Some(BrowseChannelsModal {
            space: space.clone(),
            space_name,
            filter,
            loading: true,
            error: None,
            channels: initial,
        });
        cx.notify();
        let req = Request::BrowseChannels {
            space: space.clone(),
            query: String::new(),
            cursor: None,
            limit: 200,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.sidebar_state.browse_channels.as_mut().filter(|m| m.space == space) else {
                    return;
                };
                modal.loading = false;
                match answer {
                    Ok(Response::BrowsedChannels { channels, .. }) => {
                        modal.channels = channels;
                        modal.error = None;
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not load channels.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn toggle_follow_channel(&mut self, conversation: Key, on: bool, cx: &mut Context<Self>) {
        if let Some(modal) = self.sidebar_state.browse_channels.as_mut() {
            if let Some(ch) = modal.channels.iter_mut().find(|c| c.key == conversation) {
                ch.joined = on;
            }
        }
        cx.notify();
        let revert_key = conversation.clone();
        let req = Request::FollowChannel { conversation, on };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {}
                Ok(Response::Error { detail, .. }) => {
                    if let Some(modal) = this.sidebar_state.browse_channels.as_mut() {
                        if let Some(ch) = modal.channels.iter_mut().find(|c| c.key == revert_key) {
                            ch.joined = !on;
                        }
                    }
                    this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                    cx.notify();
                }
                _ => {
                    if let Some(modal) = this.sidebar_state.browse_channels.as_mut() {
                        if let Some(ch) = modal.channels.iter_mut().find(|c| c.key == revert_key) {
                            ch.joined = !on;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn set_space_view(
        &mut self,
        space: Key,
        show_all: Option<bool>,
        hide_muted: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let req = Request::SetSpaceView { space, show_all, hide_muted };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Ok(Response::Error { detail, .. }) = answer {
                    this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                } else if let Err(err) = answer {
                    this.toast(Some(("icons/circle-alert.svg", theme::danger())), err.to_string(), cx);
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn browse_channels_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.browse_channels.as_ref()?;
        let space_row = self.spaces.iter().find(|s| s.key == modal.space);
        let opt_in_on = space_row.is_some_and(|s| s.opt_in_channels);
        let hide_muted = space_row.is_some_and(|s| s.hide_muted);
        let needle = modal.filter.read(cx).text().trim().to_lowercase();
        let tree = self.trees.get(&modal.space);

        // Group viewable channels by their category in tree order.
        let mut group_order: Vec<(Option<String>, String)> = vec![(None, "Channels".to_owned())];
        if let Some(tree) = tree {
            let mut groups: Vec<&protocol::Group> = tree.groups.iter().collect();
            groups.sort_by_key(|g| g.position);
            for g in groups {
                group_order.push((Some(g.id.clone()), g.name.clone()));
            }
        }
        let group_of = |key: &Key| -> Option<String> {
            tree.and_then(|t| t.conversations.iter().find(|c| &c.key == key).and_then(|c| c.group.clone()))
        };

        let mut list = div()
            .id("browse-channels-list")
            .flex_1()
            .min_h(px(120.))
            .max_h(px(340.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(4.));

        let mut any_shown = false;
        for (cat_id, cat_name) in &group_order {
            let matching: Vec<&protocol::BrowsedChannel> = modal
                .channels
                .iter()
                .filter(|ch| {
                    let in_cat = group_of(&ch.key) == *cat_id
                        || (cat_id.is_none()
                            && group_of(&ch.key)
                                .as_ref()
                                .is_none_or(|gid| !group_order.iter().any(|(id, _)| id.as_deref() == Some(gid.as_str()))));
                    let matches_query = needle.is_empty()
                        || ch.name.to_lowercase().contains(&needle)
                        || ch.topic.to_lowercase().contains(&needle);
                    in_cat && matches_query
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            any_shown = true;
            if group_order.len() > 1 {
                list = list.child(
                    div()
                        .pt(px(6.))
                        .pb(px(2.))
                        .type_micro()
                        .text_color(theme::text_faint())
                        .child(cat_name.clone()),
                );
            }
            for (ix, ch) in matching.into_iter().enumerate() {
                let key = ch.key.clone();
                let following = ch.joined;
                let row_key = SharedString::from(format!("browse-ch-{}-{ix}", ch.key.id));
                list = list.child(
                    div()
                        .id(row_key)
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(theme::R_CONTROL))
                        .hover(|s| s.bg(theme::hover()))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(glyph(
                            if ch.private { "icons/lock.svg" } else { "icons/hash.svg" },
                            14.,
                            theme::text_soft(),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(ch.name.clone()))
                                .when(!ch.topic.is_empty(), |col| {
                                    col.child(div().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(ch.topic.clone()))
                                }),
                        )
                        .child(
                            button(
                                SharedString::from(format!("browse-toggle-{}", ch.key.id)),
                                if following { ButtonKind::Secondary } else { ButtonKind::Primary },
                                if following { "Following" } else { "Follow" },
                            )
                            .h(px(26.))
                            .px(px(10.))
                            .type_small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_follow_channel(key.clone(), !following, cx);
                            })),
                        ),
                );
            }
        }
        if !any_shown {
            let msg = if modal.loading { "Loading channels…" } else { "No matching channels." };
            list = list.child(div().py(px(20.)).type_small().text_color(theme::text_faint()).child(msg));
        }

        let space_all = modal.space.clone();
        let space_muted = modal.space.clone();
        let card = div()
            .id("browse-channels-card")
            .w(px(480.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_title().text_color(theme::text()).child(format!("Browse channels · {}", modal.space_name)))
                    .child(icon_button("browse-channels-close", "icons/x.svg", false).on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.browse_channels = None;
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        button(
                            "browse-show-all",
                            if !opt_in_on { ButtonKind::Primary } else { ButtonKind::Secondary },
                            if !opt_in_on { "Showing all channels" } else { "Show all channels" },
                        )
                        .h(px(26.))
                        .px(px(10.))
                        .type_small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_space_view(space_all.clone(), Some(opt_in_on), None, cx);
                        })),
                    )
                    .child(
                        button(
                            "browse-hide-muted",
                            if hide_muted { ButtonKind::Primary } else { ButtonKind::Secondary },
                            if hide_muted { "Muted hidden" } else { "Hide muted channels" },
                        )
                        .h(px(26.))
                        .px(px(10.))
                        .type_small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_space_view(space_muted.clone(), None, Some(!hide_muted), cx);
                        })),
                    ),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(modal.filter.clone()),
            )
            .children(modal.error.as_ref().map(|err| div().type_small().text_color(theme::danger()).child(err.clone())))
            .child(list);

        Some(
            div()
                .id("browse-channels-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.browse_channels = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    /// Open the "Leave server" confirmation modal for a Space.
    pub(super) fn open_leave_space(&mut self, space: Key, name: String, cx: &mut Context<Self>) {
        self.sidebar_state.leave_space = Some((space, name));
        cx.notify();
    }

    fn confirm_leave_space(&mut self, cx: &mut Context<Self>) {
        let Some((space, name)) = self.sidebar_state.leave_space.take() else {
            return;
        };
        cx.notify();
        let network = space.network;
        let req = Request::LeaveSpace { space: space.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.sidebar_state.pins.retain(|pin| pin != &space);
                    this.go(Mode::Network(network), cx);
                    this.toast(Some(("icons/check.svg", theme::text_soft())), format!("Left {name}"), cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Ok(_) => this.toast(None, format!("Could not leave {name}."), cx),
                Err(err) => this.toast(None, err.to_string(), cx),
            });
        })
        .detach();
    }

    pub(in crate::gui) fn leave_space_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let (space, name) = self.sidebar_state.leave_space.as_ref()?;
        let is_teams = space.network == Network::Teams;
        let leave_btn = if is_teams { "Leave team" } else { "Leave server" };
        let body_copy = if is_teams {
            "You will lose access to this team's channels unless you rejoin or are added again."
        } else {
            "You will lose access to its channels and need a new invite to rejoin."
        };
        let card = div()
            .id("leave-space-card")
            .w(px(360.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child(format!("Leave {name}?")))
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(body_copy),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("leave-space-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.leave_space = None;
                        cx.notify();
                    })))
                    .child(button("leave-space-confirm", ButtonKind::Danger, leave_btn).on_click(
                        cx.listener(|this, _, _, cx| this.confirm_leave_space(cx)),
                    )),
            );
        Some(
            div()
                .id("leave-space-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.leave_space = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(in crate::gui) fn open_create_invite(&mut self, target: CreateInviteTarget, cx: &mut Context<Self>) {
        self.sidebar_state.create_invite = Some(CreateInviteModal {
            target,
            max_age: 604_800,
            max_uses: 0,
            temporary: false,
            url: None,
            error: None,
            busy: false,
            sent: HashSet::new(),
            epoch: 0,
        });
        self.generate_invite(cx);
    }

    fn generate_invite(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.create_invite.as_mut() else {
            return;
        };
        modal.epoch = modal.epoch.wrapping_add(1);
        modal.busy = true;
        modal.error = None;
        modal.sent.clear();
        let epoch = modal.epoch;
        let req = match &modal.target {
            CreateInviteTarget::Channel(key, _) => Request::CreateInviteLink {
                conversation: Some(key.clone()),
                space: None,
                account: None,
                max_age: modal.max_age,
                max_uses: modal.max_uses,
                temporary: modal.temporary,
            },
            CreateInviteTarget::Space(key, _) => Request::CreateInviteLink {
                conversation: None,
                space: Some(key.clone()),
                account: None,
                max_age: modal.max_age,
                max_uses: modal.max_uses,
                temporary: modal.temporary,
            },
            CreateInviteTarget::Friend(account) => Request::CreateInviteLink {
                conversation: None,
                space: None,
                account: Some(account.clone()),
                max_age: 0,
                max_uses: 0,
                temporary: false,
            },
        };
        cx.notify();
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.sidebar_state.create_invite.as_mut() else {
                    return;
                };
                if modal.epoch != epoch {
                    return;
                }
                modal.busy = false;
                match answer {
                    Ok(Response::Link { url }) => {
                        modal.url = Some(url);
                        modal.error = None;
                    }
                    Ok(Response::Error { detail, .. }) => {
                        modal.url = None;
                        modal.error = Some(detail);
                    }
                    Ok(_) => {
                        modal.url = None;
                        modal.error = Some("Could not create invite link.".to_owned());
                    }
                    Err(err) => {
                        modal.url = None;
                        modal.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send_invite_to(&mut self, conversation: Key, title: String, cx: &mut Context<Self>) {
        let Some(url) = self.sidebar_state.create_invite.as_ref().and_then(|m| m.url.clone()) else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let req = Request::Send {
            conversation: conversation.clone(),
            client_id: format!("invite-{now}"),
            text: url,
            reply_to: None,
            with: protocol::Outgoing::default(),
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Error { detail, .. }) => {
                    this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                }
                Err(err) => {
                    this.toast(Some(("icons/circle-alert.svg", theme::danger())), err.to_string(), cx);
                }
                Ok(_) => {
                    if let Some(modal) = this.sidebar_state.create_invite.as_mut() {
                        modal.sent.insert(conversation);
                    }
                    this.toast(Some(("icons/check.svg", theme::text_soft())), format!("Sent invite to {title}"), cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn create_invite_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.create_invite.as_ref()?;
        let (heading, is_friend) = match &modal.target {
            CreateInviteTarget::Space(_, name) => (format!("Invite people to {name}"), false),
            CreateInviteTarget::Channel(_, name) => (format!("Invite people to #{}", name.trim_start_matches('#')), false),
            CreateInviteTarget::Friend(_) => ("Create friend invite".to_owned(), true),
        };

        let link_text = if modal.busy && modal.url.is_none() {
            "Creating invite…".to_owned()
        } else {
            modal.url.clone().unwrap_or_else(|| "No invite link yet".to_owned())
        };
        let copy_url = modal.url.clone();

        let mut card = div()
            .id("create-invite-card")
            .w(px(420.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child(heading))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::canvas())
                            .border_1()
                            .border_color(theme::border())
                            .type_code()
                            .text_color(if modal.url.is_some() { theme::text() } else { theme::text_soft() })
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(link_text),
                    )
                    .child(
                        button("create-invite-copy", ButtonKind::Primary, "Copy").when_some(copy_url, |b, url| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                                this.toast(Some(("icons/check.svg", theme::text_soft())), "Invite link copied", cx);
                            }))
                        }),
                    ),
            );

        if is_friend {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .type_small()
                            .text_color(theme::text_soft())
                            .child("Anyone with this link can add you as a Discord friend."),
                    )
                    .child(
                        button("create-invite-refresh", ButtonKind::Secondary, if modal.busy { "Creating…" } else { "New link" })
                            .when(!modal.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.generate_invite(cx)))),
                    ),
            );
        } else {
            let ages: &[(u32, &str)] = &[
                (1_800, "30m"),
                (3_600, "1h"),
                (21_600, "6h"),
                (43_200, "12h"),
                (86_400, "1d"),
                (604_800, "7d"),
                (0, "Never"),
            ];
            let mut age_row = div().flex().flex_wrap().items_center().gap(px(4.));
            for &(secs, label) in ages {
                let selected = modal.max_age == secs;
                age_row = age_row.child(
                    div()
                        .id(("invite-age", secs as usize))
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(theme::R_CONTROL))
                        .cursor_pointer()
                        .type_small()
                        .bg(if selected { theme::accent_soft() } else { theme::canvas() })
                        .text_color(if selected { theme::accent_text() } else { theme::text_soft() })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(m) = this.sidebar_state.create_invite.as_mut() {
                                if m.max_age != secs {
                                    m.max_age = secs;
                                    this.generate_invite(cx);
                                }
                            }
                        })),
                );
            }

            let uses: &[(u32, &str)] = &[
                (0, "Unlimited"),
                (1, "1"),
                (5, "5"),
                (10, "10"),
                (25, "25"),
                (50, "50"),
                (100, "100"),
            ];
            let mut uses_row = div().flex().flex_wrap().items_center().gap(px(4.));
            for &(limit, label) in uses {
                let selected = modal.max_uses == limit;
                uses_row = uses_row.child(
                    div()
                        .id(("invite-uses", limit as usize))
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(theme::R_CONTROL))
                        .cursor_pointer()
                        .type_small()
                        .bg(if selected { theme::accent_soft() } else { theme::canvas() })
                        .text_color(if selected { theme::accent_text() } else { theme::text_soft() })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(m) = this.sidebar_state.create_invite.as_mut() {
                                if m.max_uses != limit {
                                    m.max_uses = limit;
                                    this.generate_invite(cx);
                                }
                            }
                        })),
                );
            }

            let temp = modal.temporary;
            card = card
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().type_small().text_color(theme::text_soft()).child("Expire after"))
                        .child(age_row),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().type_small().text_color(theme::text_soft()).child("Max uses"))
                        .child(uses_row),
                )
                .child(
                    div()
                        .id("invite-temporary")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(m) = this.sidebar_state.create_invite.as_mut() {
                                m.temporary = !m.temporary;
                                this.generate_invite(cx);
                            }
                        }))
                        .child(
                            div()
                                .w(px(14.))
                                .h(px(14.))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(if temp { theme::accent() } else { theme::border() })
                                .bg(if temp { theme::accent() } else { theme::canvas() })
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(temp, |box_el| box_el.child(glyph("icons/check.svg", 10., theme::on_accent()))),
                        )
                        .child(div().type_small().text_color(theme::text()).child("Grant temporary membership")),
                );
        }

        if modal.url.is_some() {
            let mut seen = HashSet::new();
            let mut targets: Vec<(Key, String)> = Vec::new();
            for row in self.held_rows() {
                if matches!(row.kind, ConversationKind::Direct | ConversationKind::Group | ConversationKind::Channel)
                    && seen.insert(row.key.clone())
                {
                    targets.push((row.key.clone(), row.title.clone()));
                    if targets.len() >= 5 {
                        break;
                    }
                }
            }
            if !targets.is_empty() {
                let mut list = div().flex().flex_col().gap(px(4.));
                for (ix, (key, title)) in targets.into_iter().enumerate() {
                    let already = modal.sent.contains(&key);
                    let send_key = key.clone();
                    let send_title = title.clone();
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::canvas())
                            .child(div().type_small().text_color(theme::text()).truncate().child(title))
                            .child(
                                button(
                                    ("invite-send-to", ix),
                                    if already { ButtonKind::Ghost } else { ButtonKind::Secondary },
                                    if already { "Sent" } else { "Send" },
                                )
                                .when(!already, |b| {
                                    b.on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_invite_to(send_key.clone(), send_title.clone(), cx);
                                    }))
                                }),
                            ),
                    );
                }
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().type_small().text_color(theme::text_soft()).child("Send to…"))
                        .child(list),
                );
            }
        }

        card = card
            .children(modal.error.as_ref().map(|err| div().type_small().text_color(theme::danger()).child(err.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("create-invite-close", ButtonKind::Secondary, "Done").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.sidebar_state.create_invite = None;
                            cx.notify();
                        },
                    ))),
            );

        Some(
            div()
                .id("create-invite-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.create_invite = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_join_server(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        self.open_join_invite(account, None, cx);
    }

    pub(in crate::gui) fn open_join_invite(&mut self, account: AccountKey, link: Option<String>, cx: &mut Context<Self>) {
        let placeholder = if account.network == Network::Telegram {
            "t.me/+… or invite link"
        } else if account.network == Network::Teams {
            "Team join code or link"
        } else {
            "discord.gg/… or invite code"
        };
        let prefill = link.clone();
        let input = cx.new(|cx| {
            let mut field = crate::gui::input::TextInput::new(placeholder, cx);
            if let Some(ref text) = prefill {
                field.set_text(text, cx);
            }
            field
        });
        self.sidebar_state.join_server = Some(JoinServerModal {
            account,
            input,
            preview: None,
            error: None,
            busy: false,
        });
        if link.is_some() {
            self.preview_join_server(cx);
        } else {
            cx.notify();
        }
    }

    /// Opens the Telegram invite join sheet for a clicked `t.me/+…` or `t.me/joinchat/…` link.
    /// Returns `true` if a connected Telegram account was found to preview the invite.
    pub(in crate::gui) fn open_telegram_invite(
        &mut self,
        preferred: Option<AccountKey>,
        url: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let account = preferred
            .filter(|k| {
                self.snapshot
                    .as_ref()
                    .and_then(|s| s.account(k))
                    .is_some_and(|a| a.link == Link::Connected)
            })
            .or_else(|| {
                self.snapshot.as_ref().and_then(|s| {
                    s.accounts
                        .iter()
                        .find(|a| a.key.network == Network::Telegram && a.link == Link::Connected)
                        .map(|a| a.key.clone())
                })
            });
        let Some(account) = account else {
            return false;
        };
        self.open_join_invite(account, Some(url), cx);
        true
    }

    fn preview_join_server(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.join_server.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let code = modal.input.read(cx).text().trim().to_owned();
        if code.is_empty() {
            modal.error = Some("Enter an invite link or code.".to_owned());
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let account = modal.account.clone();
        cx.notify();
        let req = Request::InvitePreview { account, code, event_id: None };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(modal) = this.sidebar_state.join_server.as_mut() {
                    modal.busy = false;
                    match answer {
                        Ok(Response::InvitePreview { preview }) => {
                            modal.preview = Some(preview);
                            modal.error = None;
                        }
                        Ok(Response::Error { detail, .. }) => {
                            modal.preview = None;
                            modal.error = Some(detail);
                        }
                        Ok(_) => modal.error = Some("Could not load invite preview.".to_owned()),
                        Err(err) => modal.error = Some(err.to_string()),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn submit_join_server(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.join_server.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let code = modal.input.read(cx).text().trim().to_owned();
        if code.is_empty() {
            modal.error = Some("Enter an invite link or code.".to_owned());
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let account = modal.account.clone();
        cx.notify();
        let is_teams = account.network == Network::Teams;
        let noun = if is_teams { "team" } else { "server" };
        let req = Request::InviteJoin { account, code };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::InviteJoined { space, conversation, pending }) => {
                    this.sidebar_state.join_server = None;
                    if pending {
                        this.toast(
                            Some(("icons/check.svg", theme::text_soft())),
                            "Join request sent — waiting for admin approval",
                            cx,
                        );
                    } else if let Some(conv) = conversation {
                        this.open_now(conv, "", cx);
                        this.toast(Some(("icons/check.svg", theme::text_soft())), "Joined chat", cx);
                    } else if let Some(space) = space {
                        this.go(Mode::Space(space), cx);
                        this.toast(Some(("icons/check.svg", theme::text_soft())), format!("Joined {noun}"), cx);
                    } else {
                        this.toast(Some(("icons/check.svg", theme::text_soft())), "Joined", cx);
                    }
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(modal) = this.sidebar_state.join_server.as_mut() {
                        modal.busy = false;
                        modal.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(modal) = this.sidebar_state.join_server.as_mut() {
                        modal.busy = false;
                        modal.error = Some(format!("Could not join {noun}."));
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(modal) = this.sidebar_state.join_server.as_mut() {
                        modal.busy = false;
                        modal.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn join_server_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.join_server.as_ref()?;
        let is_teams = modal.account.network == Network::Teams;
        let modal_title = if is_teams { "Join a team" } else { "Join a server" };
        let ready = !modal.input.read(cx).text().trim().is_empty() && !modal.busy;
        let preview_box = modal.preview.as_ref().map(|p| {
            let mut meta = Vec::new();
            if let Some(online) = p.online {
                meta.push(format!("{online} online"));
            }
            if let Some(members) = p.members {
                meta.push(format!("{members} members"));
            }
            div()
                .p(px(10.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::canvas())
                .border_1()
                .border_color(theme::border())
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(p.name.clone()))
                .when(!p.description.is_empty(), |box_el| {
                    box_el.child(div().type_small().text_color(theme::text_soft()).child(p.description.clone()))
                })
                .when(!meta.is_empty(), |box_el| {
                    box_el.child(div().type_small().text_color(theme::text_soft()).child(meta.join(" · ")))
                })
        });
        let is_telegram = modal.account.network == Network::Telegram;
        let request_needed = modal.preview.as_ref().is_some_and(|p| p.request_needed);
        let title = if is_telegram { "Join Telegram chat" } else { modal_title };
        let join_label = if modal.busy {
            if request_needed { "Requesting…" } else { "Joining…" }
        } else if request_needed {
            "Request to join"
        } else {
            "Join"
        };
        let card = div()
            .id("join-server-card")
            .w(px(380.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child(title))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(modal.input.clone()),
            )
            .children(preview_box)
            .children(modal.error.as_ref().map(|err| div().type_small().text_color(theme::danger()).child(err.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("join-server-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.join_server = None;
                        cx.notify();
                    })))
                    .child(
                        button("join-server-preview", ButtonKind::Secondary, "Preview").when(ready, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.preview_join_server(cx)))
                        }),
                    )
                    .child(
                        button(
                            "join-server-submit",
                            ButtonKind::Primary,
                            join_label,
                        )
                        .when(ready, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.submit_join_server(cx)))
                        }),
                    ),
            );
        Some(
            div()
                .id("join-server-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.join_server = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_create_server(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        let name = cx.new(|cx| crate::gui::input::TextInput::new("Server name", cx));
        self.sidebar_state.create_server = Some(CreateServerModal {
            account,
            name,
            icon_path: None,
            template: String::new(),
            error: None,
            busy: false,
        });
        cx.notify();
    }

    fn pick_create_server_icon(&mut self, cx: &mut Context<Self>) {
        let asked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose server picture".into()),
        });
        cx.spawn(async move |shell, cx| {
            if let Ok(Ok(Some(paths))) = asked.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = shell.update(cx, |this, cx| {
                        if let Some(modal) = this.sidebar_state.create_server.as_mut() {
                            modal.icon_path = Some(path.to_string_lossy().into_owned());
                            modal.error = None;
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    fn submit_create_server(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.create_server.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let name = modal.name.read(cx).text().trim().to_owned();
        if !(2..=100).contains(&name.chars().count()) {
            modal.error = Some("Server name must be between 2 and 100 characters.".to_owned());
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let account = modal.account.clone();
        let icon = modal.icon_path.clone();
        let template = (!modal.template.is_empty()).then(|| modal.template.clone());
        cx.notify();
        let req = Request::CreateSpace { account, name, icon, template };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::SpaceCreated { space, .. }) => {
                    this.sidebar_state.create_server = None;
                    this.go(Mode::Space(space), cx);
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Server created", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(modal) = this.sidebar_state.create_server.as_mut() {
                        modal.busy = false;
                        modal.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(modal) = this.sidebar_state.create_server.as_mut() {
                        modal.busy = false;
                        modal.error = Some("Could not create server.".to_owned());
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(modal) = this.sidebar_state.create_server.as_mut() {
                        modal.busy = false;
                        modal.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn create_server_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.create_server.as_ref()?;
        let trimmed = modal.name.read(cx).text().trim().chars().count();
        let ready = (2..=100).contains(&trimmed) && !modal.busy;
        let icon_label = modal
            .icon_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let templates = [
            ("", "Blank"),
            ("gaming", "Gaming"),
            ("study", "Study Group"),
            ("friends", "Friends"),
            ("community", "Community"),
        ];
        let mut template_bar = div().flex().flex_wrap().gap(px(6.));
        for (ix, (id, label)) in templates.into_iter().enumerate() {
            let selected = modal.template == id;
            let value = id.to_owned();
            template_bar = template_bar.child(
                segment(("create-server-tpl", ix), label, selected)
                    .px(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(modal) = this.sidebar_state.create_server.as_mut() {
                            modal.template = value.clone();
                            cx.notify();
                        }
                    })),
            );
        }
        let icon_row = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                button("create-server-icon", ButtonKind::Secondary, "Choose picture…").on_click(
                    cx.listener(|this, _, _, cx| this.pick_create_server_icon(cx)),
                ),
            )
            .children(icon_label.map(|name| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(name)
            }))
            .when(modal.icon_path.is_some(), |row| {
                row.child(button("create-server-icon-clear", ButtonKind::Ghost, "Clear").on_click(
                    cx.listener(|this, _, _, cx| {
                        if let Some(modal) = this.sidebar_state.create_server.as_mut() {
                            modal.icon_path = None;
                            cx.notify();
                        }
                    }),
                ))
            });
        let card = div()
            .id("create-server-card")
            .w(px(400.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child("Create a server"))
            .child(div().type_small().text_color(theme::text_soft()).child("Name"))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(modal.name.clone()),
            )
            .child(div().type_small().text_color(theme::text_soft()).child("Picture (optional)"))
            .child(icon_row)
            .child(div().type_small().text_color(theme::text_soft()).child("Template"))
            .child(template_bar)
            .children(modal.error.as_ref().map(|err| div().type_small().text_color(theme::danger()).child(err.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("create-server-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.create_server = None;
                        cx.notify();
                    })))
                    .child(
                        button(
                            "create-server-submit",
                            ButtonKind::Primary,
                            if modal.busy { "Creating…" } else { "Create" },
                        )
                        .when(ready, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.submit_create_server(cx)))
                        }),
                    ),
            );
        Some(
            div()
                .id("create-server-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.create_server = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(in crate::gui) fn load_space_events(&mut self, space: Key, cx: &mut Context<Self>) {
        let req = Request::Events { space: space.clone() };
        self.off_thread(cx, move || ask(&req), move |this, answer, cx| {
            if let Ok(Response::Events { space, events }) = answer {
                this.sidebar_state.space_events.insert(space, events);
                cx.notify();
            }
        });
    }

    pub(in crate::gui) fn open_space_events(&mut self, space: Key, cx: &mut Context<Self>) {
        self.sidebar_state.events_modal = Some(EventsModal {
            space: space.clone(),
            error: None,
            busy_event: None,
        });
        self.load_space_events(space, cx);
        cx.notify();
    }

    fn toggle_event_interest(&mut self, space: Key, event: Key, on: bool, cx: &mut Context<Self>) {
        if let Some(modal) = self.sidebar_state.events_modal.as_mut() {
            if modal.busy_event.is_some() {
                return;
            }
            modal.busy_event = Some(event.id.clone());
            modal.error = None;
        }
        cx.notify();
        let req = Request::EventInterest {
            space: space.clone(),
            event: event.clone(),
            on,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(modal) = this.sidebar_state.events_modal.as_mut() {
                    modal.busy_event = None;
                }
                match answer {
                    Ok(Response::Ok) => {
                        if let Some(list) = this.sidebar_state.space_events.get_mut(&space) {
                            if let Some(ev) = list.iter_mut().find(|e| e.key == event) {
                                if ev.interested != on {
                                    ev.interested = on;
                                    ev.count = if on { ev.count.saturating_add(1) } else { ev.count.saturating_sub(1) };
                                }
                            }
                        }
                    }
                    Ok(Response::Error { detail, .. }) => {
                        if let Some(modal) = this.sidebar_state.events_modal.as_mut() {
                            modal.error = Some(detail);
                        }
                    }
                    Err(err) => {
                        if let Some(modal) = this.sidebar_state.events_modal.as_mut() {
                            modal.error = Some(err.to_string());
                        }
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn join_space_event(&mut self, event: &protocol::SpaceEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.place_kind {
            protocol::EventPlaceKind::Voice => {
                if let Some(conv) = &event.conversation {
                    self.call_start(conv.clone(), cx);
                }
            }
            protocol::EventPlaceKind::Stage => {
                if let Some(conv) = &event.conversation {
                    self.call_handoff(conv.clone(), cx);
                }
            }
            protocol::EventPlaceKind::External => {
                if !event.place.is_empty() {
                    self.open_external_url(&event.place, window, cx);
                }
            }
        }
    }

    fn space_events_strip(&self, cx: &mut Context<Self>) -> Option<Div> {
        let Mode::Space(space) = &self.sidebar_state.mode else {
            return None;
        };
        if space.network != Network::Discord {
            return None;
        }
        let events = self.sidebar_state.space_events.get(space)?;
        if events.is_empty() {
            return None;
        }
        let live = events.iter().find(|e| e.status == protocol::EventStatus::Active).cloned();
        let count = events.len();
        let open_key = space.clone();
        let mut strip = div()
            .flex_none()
            .px(px(theme::INSET))
            .pb(px(6.))
            .flex()
            .flex_col()
            .gap(px(4.));
        if let Some(active) = live {
            let can_join = active.conversation.is_some()
                || (active.place_kind == protocol::EventPlaceKind::External
                    && protocol::link::Destination::parse(&active.place).is_some());
            let join_event = active.clone();
            let place = active.place.clone();
            strip = strip.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().size(px(8.)).flex_none().rounded(px(4.)).bg(theme::online()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().min_w_0().truncate().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(active.name))
                            .when(!place.is_empty(), |col| {
                                col.child(div().min_w_0().truncate().type_micro().text_color(theme::text_soft()).child(place))
                            }),
                    )
                    .when(can_join, |row| {
                        row.child(
                            button("space-live-event-join", ButtonKind::Primary, "Join")
                                .h(px(22.))
                                .px(px(8.))
                                .type_small()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.join_space_event(&join_event, window, cx);
                                })),
                        )
                    }),
            );
        }
        strip = strip.child(
            div()
                .id("space-events-row")
                .h(px(28.))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .flex()
                .items_center()
                .gap(px(8.))
                .on_click(cx.listener(move |this, _, _, cx| this.open_space_events(open_key.clone(), cx)))
                .child(glyph("icons/calendar.svg", 15., theme::text_soft()))
                .child(div().flex_1().type_ui().text_color(theme::text()).child("Events"))
                .child(
                    div()
                        .type_small()
                        .text_color(theme::text_soft())
                        .child(count.to_string()),
                ),
        );
        Some(strip)
    }

    pub(in crate::gui) fn events_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.events_modal.as_ref()?;
        let space = modal.space.clone();
        let space_name = self
            .spaces
            .iter()
            .find(|s| s.key == space)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Server".to_owned());
        let events = self.sidebar_state.space_events.get(&space).cloned().unwrap_or_default();
        let now = crate::view::now();
        let mut list = div()
            .id("space-events-list")
            .flex()
            .flex_col()
            .gap(px(10.))
            .max_h(px(420.))
            .overflow_y_scroll();
        if events.is_empty() {
            list = list.child(div().py(px(16.)).type_ui().text_color(theme::text_soft()).child("No upcoming events."));
        } else {
            for (ix, ev) in events.into_iter().enumerate() {
                let is_live = ev.status == protocol::EventStatus::Active;
                let when_line = if is_live {
                    "Live now".to_owned()
                } else if ev.start > 0 {
                    format!("{} · {}", super::time::day(now, ev.start), super::time::clock(ev.start))
                } else {
                    "Scheduled".to_owned()
                };
                let cover = ev
                    .cover
                    .as_ref()
                    .and_then(|file| self.people_state.hub().file(&ev.key.account(), file, cx).path)
                    .map(std::path::PathBuf::from);
                let interested = ev.interested;
                let busy = modal.busy_event.as_deref() == Some(ev.key.id.as_str());
                let toggle_space = space.clone();
                let toggle_event = ev.key.clone();
                let can_join = is_live
                    && (ev.conversation.is_some()
                        || (ev.place_kind == protocol::EventPlaceKind::External
                            && protocol::link::Destination::parse(&ev.place).is_some()));
                let join_event = ev.clone();
                let count_text = if ev.count == 1 {
                    "1 interested".to_owned()
                } else {
                    format!("{} interested", ev.count)
                };
                let mut item = div()
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .flex()
                    .flex_col();
                if let Some(path) = cover {
                    item = item.child(
                        div()
                            .w_full()
                            .h(px(112.))
                            .overflow_hidden()
                            .bg(theme::raised())
                            .child(gpui::img(path).size_full().object_fit(gpui::ObjectFit::Cover)),
                    );
                }
                let mut inner = div()
                    .p(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .when(is_live, |row| row.child(div().size(px(8.)).rounded(px(4.)).bg(theme::online())))
                            .child(div().type_small().text_color(if is_live { theme::online() } else { theme::text_soft() }).child(when_line)),
                    )
                    .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(ev.name.clone()));
                if !ev.description.trim().is_empty() {
                    inner = inner.child(div().type_small().text_color(theme::text_soft()).line_clamp(3).child(ev.description.trim().to_owned()));
                }
                if !ev.place.is_empty() {
                    let place_icon = match ev.place_kind {
                        protocol::EventPlaceKind::Voice => "icons/volume-2.svg",
                        protocol::EventPlaceKind::Stage => "icons/radio.svg",
                        protocol::EventPlaceKind::External => "icons/external-link.svg",
                    };
                    inner = inner.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .type_small()
                            .text_color(theme::text_soft())
                            .child(glyph(place_icon, 13., theme::text_faint()))
                            .child(div().min_w_0().truncate().child(ev.place.clone())),
                    );
                }
                inner = inner.child(
                    div()
                        .mt(px(4.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(div().type_small().text_color(theme::text_faint()).child(count_text))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    button(
                                        ("event-interest", ix),
                                        if interested { ButtonKind::Primary } else { ButtonKind::Secondary },
                                        if interested { "Interested" } else { "Mark interested" },
                                    )
                                    .h(px(26.))
                                    .px(px(10.))
                                    .type_small()
                                    .when(!busy, |b| {
                                        b.on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_event_interest(
                                                toggle_space.clone(),
                                                toggle_event.clone(),
                                                !interested,
                                                cx,
                                            );
                                        }))
                                    }),
                                )
                                .when(can_join, |bar| {
                                    bar.child(
                                        button(("event-join", ix), ButtonKind::Primary, "Join")
                                            .h(px(26.))
                                            .px(px(10.))
                                            .type_small()
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.sidebar_state.events_modal = None;
                                                this.join_space_event(&join_event, window, cx);
                                                cx.notify();
                                            })),
                                    )
                                }),
                        ),
                );
                item = item.child(inner);
                list = list.child(item);
            }
        }
        let card = div()
            .id("space-events-card")
            .w(px(440.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_title().text_color(theme::text()).child(format!("{space_name} · Events")))
                    .child(icon_button("space-events-close", "icons/x.svg", false).on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.events_modal = None;
                        cx.notify();
                    }))),
            )
            .children(modal.error.as_ref().map(|err| div().type_small().text_color(theme::danger()).child(err.clone())))
            .child(list);
        Some(
            div()
                .id("space-events-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.events_modal = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_server_gate(&mut self, space: Key, cx: &mut Context<Self>) {
        if space.network != Network::Discord {
            return;
        }
        self.sidebar_state.server_gate = Some(ServerGate {
            space: space.clone(),
            loaded: false,
            gate: None,
            agreed: Vec::new(),
            choice: Vec::new(),
            texts: Vec::new(),
            picked: Vec::new(),
            error: None,
            busy: true,
        });
        cx.notify();
        self.off_thread(cx, move || ask(&Request::SpaceGate { space }), |this, answer, cx| {
            this.install_gate_answer(answer, cx);
        });
    }

    fn install_gate_answer(&mut self, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        {
            let Some(modal) = self.sidebar_state.server_gate.as_mut() else {
                return;
            };
            modal.busy = false;
            modal.loaded = true;
        }
        match answer {
            Ok(Response::SpaceGate { gate, .. }) => self.install_gate(gate, cx),
            Ok(Response::Error { detail, .. }) => {
                if let Some(modal) = self.sidebar_state.server_gate.as_mut() {
                    modal.error = Some(detail);
                }
            }
            Ok(_) => {
                if let Some(modal) = self.sidebar_state.server_gate.as_mut() {
                    modal.error = Some("Could not load this server's setup.".to_owned());
                }
            }
            Err(err) => {
                if let Some(modal) = self.sidebar_state.server_gate.as_mut() {
                    modal.error = Some(err.to_string());
                }
            }
        }
        cx.notify();
    }

    fn install_gate(&mut self, mut gate: protocol::SpaceGate, cx: &mut Context<Self>) {
        if gate.prompts.is_empty() {
            if let Some(old) = self.sidebar_state.server_gate.as_ref().and_then(|modal| modal.gate.as_ref()) {
                gate.prompts = old.prompts.clone();
            }
        }
        let same = self.sidebar_state.server_gate.as_ref().is_some_and(|modal| {
            modal.gate.as_ref().is_some_and(|old| {
                old.fields.len() == gate.fields.len() && old.fields.iter().zip(&gate.fields).all(|(left, right)| left.kind == right.kind)
            })
        });
        let texts = if same {
            None
        } else {
            Some(
                gate.fields
                    .iter()
                    .map(|field| {
                        matches!(field.kind.as_str(), "TEXT_INPUT" | "PARAGRAPH").then(|| {
                            let hint = if field.kind == "PARAGRAPH" { "Write a paragraph" } else { "Write an answer" };
                            cx.new(|cx| crate::gui::input::TextInput::new(hint, cx))
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let Some(modal) = self.sidebar_state.server_gate.as_mut() else {
            return;
        };
        if let Some(texts) = texts {
            modal.agreed = vec![false; gate.fields.len()];
            modal.choice = vec![None; gate.fields.len()];
            modal.texts = texts;
        }
        modal.gate = Some(gate);
        modal.loaded = true;
        modal.busy = false;
        modal.error = None;
    }

    fn accept_server_rules(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.server_gate.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let Some(gate) = modal.gate.as_ref() else {
            return;
        };
        if gate.fields.is_empty() {
            return;
        }
        let answers = gate
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| match field.kind.as_str() {
                "TERMS" => {
                    if modal.agreed.get(index).copied().unwrap_or(false) {
                        "true".to_owned()
                    } else {
                        "false".to_owned()
                    }
                }
                "MULTIPLE_CHOICE" => modal.choice.get(index).and_then(|choice| *choice).map(|choice| choice.to_string()).unwrap_or_default(),
                "TEXT_INPUT" | "PARAGRAPH" => modal
                    .texts
                    .get(index)
                    .and_then(|input| input.as_ref())
                    .map(|input| input.read(cx).text().to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            })
            .collect::<Vec<_>>();
        let space = modal.space.clone();
        modal.busy = true;
        modal.error = None;
        cx.notify();
        self.off_thread(cx, move || ask(&Request::AcceptRules { space, answers }), |this, answer, cx| {
            match answer {
                Ok(Response::SpaceGate { gate, .. }) => {
                    let pending = gate.pending;
                    this.install_gate(gate, cx);
                    if !pending {
                        this.toast(Some(("icons/check.svg", theme::text_soft())), "Rules accepted", cx);
                    }
                }
                other => this.install_gate_answer(other, cx),
            }
            cx.notify();
        });
    }

    fn submit_server_onboarding(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.server_gate.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let space = modal.space.clone();
        let options = modal.picked.clone();
        modal.busy = true;
        modal.error = None;
        cx.notify();
        self.off_thread(cx, move || ask(&Request::SubmitOnboarding { space, options }), |this, answer, cx| {
            let finished = matches!(answer, Ok(Response::Ok));
            if let Some(modal) = this.sidebar_state.server_gate.as_mut() {
                modal.busy = false;
                match answer {
                    Ok(Response::Ok) => modal.error = None,
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not finish setup.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
            }
            if finished {
                this.toast(Some(("icons/check.svg", theme::text_soft())), "Setup finished", cx);
            }
            cx.notify();
        });
    }

    pub(in crate::gui) fn server_gate_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.server_gate.as_ref()?;
        let gate = modal.gate.clone();
        let busy = modal.busy;
        let loaded = modal.loaded;
        let error = modal.error.clone();
        let agreed = modal.agreed.clone();
        let choice = modal.choice.clone();
        let texts = modal.texts.clone();
        let picked = modal.picked.clone();
        let body = match &gate {
            None if !loaded => div().type_ui().text_color(theme::text_soft()).child("Loading server setup…"),
            None => div().type_ui().text_color(theme::text_soft()).child("Nothing to finish on this server."),
            Some(gate) if gate.fields.is_empty() && gate.prompts.is_empty() && gate.description.is_empty() && gate.reason.is_empty() => {
                div().type_ui().text_color(theme::text_soft()).child("Nothing to finish on this server.")
            }
            Some(gate) => {
                let mut column = div().flex().flex_col().gap(px(8.));
                if !gate.reason.is_empty() {
                    column = column.child(div().type_ui().text_color(theme::text()).child(gate.reason.clone()));
                }
                if !gate.description.is_empty() {
                    column = column.child(div().type_small().text_color(theme::text_soft()).child(gate.description.clone()));
                }
                if gate.manual {
                    column = column.child(div().type_small().text_color(theme::text_soft()).child("A moderator still has to approve you."));
                }
                for (index, field) in gate.fields.iter().take(5).enumerate() {
                    let mut row = div().flex().flex_col().gap(px(4.));
                    if !field.label.is_empty() {
                        row = row.child(div().type_ui().text_color(theme::text()).child(field.label.clone()));
                    }
                    if !field.values.is_empty() {
                        row = row.child(div().type_small().text_color(theme::text_soft()).child(field.values.join("\n")));
                    }
                    match field.kind.as_str() {
                        "TERMS" => {
                            let on = agreed.get(index).copied().unwrap_or(false);
                            row = row.child(
                                button(SharedString::from(format!("gate-terms-{index}")), ButtonKind::Secondary, if on { "Agreed" } else { "Agree" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(modal) = this.sidebar_state.server_gate.as_mut() {
                                            if let Some(flag) = modal.agreed.get_mut(index) {
                                                *flag = !*flag;
                                            }
                                        }
                                        cx.notify();
                                    })),
                            );
                        }
                        "MULTIPLE_CHOICE" => {
                            let chosen = choice.get(index).copied().flatten();
                            for (choice_index, label) in field.choices.iter().enumerate() {
                                let on = chosen == Some(choice_index as u32);
                                let words = if on { format!("Selected: {label}") } else { label.clone() };
                                row = row.child(
                                    button(SharedString::from(format!("gate-choice-{index}-{choice_index}")), ButtonKind::Secondary, words).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            if let Some(modal) = this.sidebar_state.server_gate.as_mut() {
                                                if let Some(slot) = modal.choice.get_mut(index) {
                                                    *slot = Some(choice_index as u32);
                                                }
                                            }
                                            cx.notify();
                                        }),
                                    ),
                                );
                            }
                        }
                        "TEXT_INPUT" | "PARAGRAPH" => {
                            if let Some(input) = texts.get(index).and_then(|input| input.clone()) {
                                row = row.child(
                                    div()
                                        .px(px(8.))
                                        .py(px(6.))
                                        .rounded(px(theme::R_CONTROL))
                                        .bg(theme::canvas())
                                        .border_1()
                                        .border_color(theme::border())
                                        .child(input),
                                );
                            }
                        }
                        _ => {}
                    }
                    column = column.child(row);
                }
                for prompt in &gate.prompts {
                    let mut block = div().flex().flex_col().gap(px(4.));
                    if !prompt.label.is_empty() {
                        block = block.child(div().type_ui().text_color(theme::text()).child(prompt.label.clone()));
                    }
                    for option in &prompt.options {
                        let on = picked.iter().any(|id| id == &option.id);
                        let id = option.id.clone();
                        let words = if on { format!("Selected: {}", option.label) } else { option.label.clone() };
                        block = block.child(
                            button(SharedString::from(format!("gate-option-{}", option.id)), ButtonKind::Secondary, words).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(modal) = this.sidebar_state.server_gate.as_mut() {
                                        if let Some(place) = modal.picked.iter().position(|kept| kept == &id) {
                                            modal.picked.remove(place);
                                        } else {
                                            modal.picked.push(id.clone());
                                        }
                                    }
                                    cx.notify();
                                }),
                            ),
                        );
                    }
                    column = column.child(block);
                }
                column
            }
        };
        let has_fields = gate.as_ref().is_some_and(|gate| !gate.fields.is_empty());
        let has_prompts = gate.as_ref().is_some_and(|gate| !gate.prompts.is_empty());
        let external = gate.as_ref().is_some_and(|gate| gate.external == DISCORD_APP);
        let card = div()
            .id("server-gate-card")
            .w(px(420.))
            .max_w_full()
            .max_h(px(560.))
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .overflow_y_scroll()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child("Server setup"))
            .child(body)
            .children(error.map(|err| div().type_small().text_color(theme::danger()).child(err)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .flex_wrap()
                    .child(button("server-gate-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.server_gate = None;
                        cx.notify();
                    })))
                    .when(external, |row| {
                        row.child(button("server-gate-discord", ButtonKind::Secondary, "Open Discord").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.open_external_link(DISCORD_APP, window, cx);
                            },
                        )))
                    })
                    .when(has_fields, |row| {
                        row.child(
                            button("server-gate-accept", ButtonKind::Primary, if busy { "Sending…" } else { "Accept the rules" }).when(!busy, |button| {
                                button.on_click(cx.listener(|this, _, _, cx| this.accept_server_rules(cx)))
                            }),
                        )
                    })
                    .when(has_prompts, |row| {
                        row.child(
                            button("server-gate-finish", ButtonKind::Primary, if busy { "Sending…" } else { "Finish setup" }).when(!busy, |button| {
                                button.on_click(cx.listener(|this, _, _, cx| this.submit_server_onboarding(cx)))
                            }),
                        )
                    }),
            );
        Some(
            div()
                .id("server-gate-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x0000_0066))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.server_gate = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_create_channel(&mut self, space: Key, group: Option<String>, cx: &mut Context<Self>) {
        let placeholder = if space.network == Network::Telegram { "Channel name" } else { "channel-name" };
        let name = cx.new(|cx| crate::gui::input::TextInput::new(placeholder, cx));
        let description = cx.new(|cx| crate::gui::input::TextInput::new("Description (optional)", cx));
        let username = cx.new(|cx| crate::gui::input::TextInput::new("@public_link (optional)", cx));
        self.sidebar_state.create_channel = Some(CreateChannelModal {
            space,
            group,
            name,
            description,
            username,
            kind: ConversationKind::Channel,
            private: false,
        });
        cx.notify();
    }

    fn submit_create_channel(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.create_channel.take() else {
            return;
        };
        let name = modal.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.sidebar_state.create_channel = Some(modal);
            return;
        }
        let description = {
            let d = modal.description.read(cx).text().trim().to_owned();
            (!d.is_empty()).then_some(d)
        };
        let username = if modal.private {
            None
        } else {
            let u = modal.username.read(cx).text().trim().to_owned();
            (!u.is_empty()).then_some(u)
        };
        cx.notify();
        let req = Request::CreateChannel {
            space: modal.space,
            name,
            kind: modal.kind,
            private: modal.private,
            group: modal.group,
            description,
            username,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Conversation { conversation }) => {
                    this.open_now(conversation.key, &conversation.title, cx);
                }
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                _ => {}
            });
        })
        .detach();
    }

    /// Asks the forum's posting rules, then opens the shared New post form. A network that
    /// cannot describe a forum yet (Telegram, until it answers) still gets the empty form.
    fn begin_forum_post(&mut self, forum: &Conversation, cx: &mut Context<Self>) {
        if forum.read_only || !matches!(forum.key.network, Network::Discord | Network::Telegram) {
            return;
        }
        let key = forum.key.clone();
        cx.spawn(async move |shell, cx| {
            let asked = key.clone();
            let req = Request::ForumInfo { forum: key };
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::ForumInfo { info }) => this.show_forum_post(info, cx),
                Ok(Response::Error { code: protocol::ErrorCode::Unsupported, .. }) => this.show_forum_post(
                    protocol::ForumInfo {
                        forum: asked,
                        guidelines: String::new(),
                        tags: Vec::new(),
                        require_tag: false,
                        media: false,
                        default_slowmode: 0,
                    },
                    cx,
                ),
                Ok(Response::Error { detail, .. }) => this.toast(None, detail, cx),
                Err(err) => this.toast(None, err.to_string(), cx),
                _ => {}
            });
        })
        .detach();
    }

    fn show_forum_post(&mut self, info: protocol::ForumInfo, cx: &mut Context<Self>) {
        let title = cx.new(|cx| crate::gui::input::TextInput::new("Title", cx));
        let text = cx.new(|cx| crate::gui::input::TextInput::multiline("Message", cx));
        self.sidebar_state.forum_post = Some(ForumPostModal {
            forum: info.forum,
            title,
            text,
            tags: info.tags,
            picked: Vec::new(),
            files: Vec::new(),
            require_tag: info.require_tag,
            media: info.media,
            guidelines: info.guidelines,
            error: None,
            busy: false,
        });
        cx.notify();
    }

    fn toggle_forum_tag(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.forum_post.as_mut() else { return };
        if modal.busy {
            return;
        }
        if let Some(at) = modal.picked.iter().position(|picked| picked == id) {
            modal.picked.remove(at);
        } else {
            modal.picked.push(id.to_owned());
        }
        modal.error = None;
        cx.notify();
    }

    fn attach_forum_files(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_state.forum_post.as_ref().is_some_and(|modal| modal.busy) {
            return;
        }
        let asked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = asked.await {
                let _ = this.update(cx, |this, cx| {
                    let Some(modal) = this.sidebar_state.forum_post.as_mut() else { return };
                    for path in paths {
                        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file").to_owned();
                        modal.files.push(protocol::OutgoingFile {
                            path: path.to_string_lossy().into_owned(),
                            name,
                            ..Default::default()
                        });
                    }
                    modal.error = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn remove_forum_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.forum_post.as_mut() else { return };
        if modal.busy || index >= modal.files.len() {
            return;
        }
        modal.files.remove(index);
        modal.error = None;
        cx.notify();
    }

    fn submit_forum_post(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.forum_post.as_mut() else { return };
        if modal.busy {
            return;
        }
        let title = modal.title.read(cx).text().trim().to_owned();
        let text = modal.text.read(cx).text().trim().to_owned();
        if title.is_empty() {
            modal.error = Some("A post needs a title.".into());
            cx.notify();
            return;
        }
        if modal.require_tag && modal.picked.is_empty() {
            modal.error = Some("This forum requires a tag.".into());
            cx.notify();
            return;
        }
        if modal.media && modal.files.is_empty() {
            modal.error = Some("A media channel needs a file.".into());
            cx.notify();
            return;
        }
        if text.is_empty() && modal.files.is_empty() {
            modal.error = Some("Write a message or attach a file.".into());
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let forum = modal.forum.clone();
        let tags = modal.picked.clone();
        let files = modal.files.clone();
        cx.notify();
        let req = Request::ForumPost {
            forum,
            title,
            text,
            tags,
            with: protocol::Outgoing { files, ..Default::default() },
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Conversation { conversation }) => {
                    this.sidebar_state.forum_post = None;
                    this.open_now(conversation.key, &conversation.title, cx);
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(modal) = this.sidebar_state.forum_post.as_mut() {
                        modal.busy = false;
                        modal.error = Some(detail.clone());
                    }
                    this.toast(None, detail, cx);
                }
                _ => {
                    if let Some(modal) = this.sidebar_state.forum_post.as_mut() {
                        modal.busy = false;
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn create_channel_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.create_channel.as_ref()?;
        let ready = !modal.name.read(cx).text().trim().is_empty();
        let is_discord = modal.space.network == Network::Discord;
        let kind_label = match modal.kind {
            ConversationKind::Voice => "Kind: Voice",
            ConversationKind::Announcement => "Kind: Announcement",
            ConversationKind::Forum => "Kind: Forum",
            _ => "Kind: Text",
        };
        let is_telegram = modal.space.network == Network::Telegram;
        let is_private = modal.private;
        let field_wrap = |input: Entity<crate::gui::input::TextInput>| {
            div()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::canvas())
                .border_1()
                .border_color(theme::border())
                .type_ui()
                .child(input)
        };
        let is_teams = modal.space.network == Network::Teams;
        let card = div()
            .id("create-channel-card")
            .w(px(360.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child(if is_telegram { "New channel" } else { "Create channel" }))
            .child(field_wrap(modal.name.clone()))
            .when(is_telegram, |card| card.child(field_wrap(modal.description.clone())))
            .when(is_telegram && !is_private, |card| card.child(field_wrap(modal.username.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .when(modal.space.network != Network::Slack && !is_telegram && !is_teams, |row| {
                        row.child(
                            div()
                                .id("create-chan-kind")
                                .type_small()
                                .text_color(theme::accent_text())
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(m) = this.sidebar_state.create_channel.as_mut() {
                                        m.kind = if is_discord {
                                            match m.kind {
                                                ConversationKind::Channel => ConversationKind::Voice,
                                                ConversationKind::Voice => ConversationKind::Announcement,
                                                ConversationKind::Announcement => ConversationKind::Forum,
                                                _ => ConversationKind::Channel,
                                            }
                                        } else if m.kind == ConversationKind::Voice {
                                            ConversationKind::Channel
                                        } else {
                                            ConversationKind::Voice
                                        };
                                        cx.notify();
                                    }
                                }))
                                .child(kind_label),
                        )
                    })
                    .child(
                        div()
                            .id("create-chan-private")
                            .type_small()
                            .text_color(if is_private { theme::accent_text() } else { theme::text_faint() })
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(m) = this.sidebar_state.create_channel.as_mut() {
                                    m.private = !m.private;
                                    cx.notify();
                                }
                            }))
                            .child(if is_private { "☑ Private channel" } else { "☐ Private channel" }),
                    ),
            )
            .when(is_teams, |card| {
                card.child(
                    div()
                        .type_micro()
                        .text_color(theme::text_faint())
                        .child("Creates a channel in this Microsoft Teams team under your account; your organization's policy may restrict this."),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        crate::gui::components::button("create-chan-cancel", crate::gui::components::ButtonKind::Ghost, "Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_state.create_channel = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        crate::gui::components::button(
                            "create-chan-submit",
                            if ready { crate::gui::components::ButtonKind::Primary } else { crate::gui::components::ButtonKind::Ghost },
                            "Create",
                        )
                        .when(ready, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_create_channel(cx)))),
                    ),
            );
        Some(
            div()
                .id("create-channel-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.create_channel = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_create_category(&mut self, space: Key, cx: &mut Context<Self>) {
        let name = cx.new(|cx| crate::gui::input::TextInput::new("Category name", cx));
        self.sidebar_state.create_category = Some(CreateCategoryModal {
            space,
            name,
            busy: false,
            error: None,
        });
        cx.notify();
    }

    fn submit_create_category(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.create_category.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let name = modal.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            modal.error = Some("Enter a category name.".to_owned());
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let space = modal.space.clone();
        cx.notify();
        let req = Request::CreateCategory { space: space.clone(), name };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Group { .. }) => {
                    this.sidebar_state.create_category = None;
                    this.load_tree(space, cx);
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Category created", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.sidebar_state.create_category.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(m) = this.sidebar_state.create_category.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not create category.".to_owned());
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = this.sidebar_state.create_category.as_mut() {
                        m.busy = false;
                        m.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn create_category_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.create_category.as_ref()?;
        let ready = !modal.name.read(cx).text().trim().is_empty() && !modal.busy;
        let card = div()
            .id("create-category-card")
            .w(px(360.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child("Create category"))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(modal.name.clone()),
            )
            .children(modal.error.as_ref().map(|e| div().type_small().text_color(theme::danger()).child(e.clone())))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("create-cat-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.create_category = None;
                        cx.notify();
                    })))
                    .child(
                        button(
                            "create-cat-submit",
                            if ready { ButtonKind::Primary } else { ButtonKind::Ghost },
                            if modal.busy { "Creating…" } else { "Create" },
                        )
                        .when(ready, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_create_category(cx)))),
                    ),
            );
        Some(
            div()
                .id("create-category-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.create_category = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_edit_channel(&mut self, conversation: &Conversation, cx: &mut Context<Self>) {
        let space = match &conversation.space {
            Some(id) => Key { id: id.clone(), ..conversation.key.clone() },
            None => return,
        };
        let categories: Vec<(String, String)> = self
            .trees
            .get(&space)
            .map(|t| t.groups.iter().map(|g| (g.id.clone(), g.name.clone())).collect())
            .unwrap_or_default();
        let initial_name = conversation.title.trim_start_matches('#').to_owned();
        let initial_topic = conversation.topic.clone();
        let initial_limit = if conversation.limit > 0 { conversation.limit.to_string() } else { String::new() };
        let name = cx.new(|cx| {
            let mut input = crate::gui::input::TextInput::new("Channel name", cx);
            input.set_text(&initial_name, cx);
            input
        });
        let topic = cx.new(|cx| {
            let mut input = crate::gui::input::TextInput::new("Topic (optional)", cx);
            input.set_text(&initial_topic, cx);
            input
        });
        let slow_mode = cx.new(|cx| crate::gui::input::TextInput::new("Slow mode seconds (0 = off)", cx));
        let user_limit = cx.new(|cx| {
            let mut input = crate::gui::input::TextInput::new("User limit (0 = unlimited)", cx);
            input.set_text(&initial_limit, cx);
            input
        });
        self.sidebar_state.edit_channel = Some(EditChannelModal {
            target: conversation.key.clone(),
            space,
            is_category: false,
            kind: conversation.kind,
            name,
            topic,
            slow_mode,
            user_limit,
            nsfw: false,
            parent_id: conversation.group.clone(),
            initial_parent_id: conversation.group.clone(),
            categories,
            busy: false,
            error: None,
        });
        cx.notify();
    }

    pub(super) fn open_edit_category(&mut self, space: Key, id: String, current_name: String, cx: &mut Context<Self>) {
        let target = Key { id, ..space.clone() };
        let name = cx.new(|cx| {
            let mut input = crate::gui::input::TextInput::new("Category name", cx);
            input.set_text(&current_name, cx);
            input
        });
        let topic = cx.new(|cx| crate::gui::input::TextInput::new("", cx));
        let slow_mode = cx.new(|cx| crate::gui::input::TextInput::new("", cx));
        let user_limit = cx.new(|cx| crate::gui::input::TextInput::new("", cx));
        self.sidebar_state.edit_channel = Some(EditChannelModal {
            target,
            space,
            is_category: true,
            kind: ConversationKind::Channel,
            name,
            topic,
            slow_mode,
            user_limit,
            nsfw: false,
            parent_id: None,
            initial_parent_id: None,
            categories: Vec::new(),
            busy: false,
            error: None,
        });
        cx.notify();
    }

    fn submit_edit_channel(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.edit_channel.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let name_val = modal.name.read(cx).text().trim().to_owned();
        if name_val.is_empty() {
            modal.error = Some("Name cannot be empty.".to_owned());
            cx.notify();
            return;
        }
        let is_category = modal.is_category;
        let is_voice = modal.kind == ConversationKind::Voice;
        let topic_val = if is_category || is_voice {
            None
        } else {
            Some(modal.topic.read(cx).text().trim().to_owned())
        };
        let slow_str = modal.slow_mode.read(cx).text().trim().to_owned();
        let rate_limit_per_user = if is_category || slow_str.is_empty() {
            None
        } else {
            match slow_str.parse::<u32>() {
                Ok(n) if n <= 21_600 => Some(n),
                _ => {
                    modal.error = Some("Slow mode must be a number of seconds (0–21600).".to_owned());
                    cx.notify();
                    return;
                }
            }
        };
        let limit_str = modal.user_limit.read(cx).text().trim().to_owned();
        let user_limit = if !is_voice || is_category || limit_str.is_empty() {
            None
        } else {
            match limit_str.parse::<u32>() {
                Ok(n) if n <= 99 => Some(n),
                _ => {
                    modal.error = Some("User limit must be between 0 and 99.".to_owned());
                    cx.notify();
                    return;
                }
            }
        };
        let nsfw = if is_category || is_voice { None } else { Some(modal.nsfw) };
        let parent_id = if !is_category && modal.parent_id != modal.initial_parent_id {
            Some(modal.parent_id.clone())
        } else {
            None
        };
        modal.busy = true;
        modal.error = None;
        let target = modal.target.clone();
        let space = modal.space.clone();
        cx.notify();
        let req = Request::EditChannel {
            conversation: target,
            name: Some(name_val),
            topic: topic_val,
            rate_limit_per_user,
            nsfw,
            user_limit,
            parent_id,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.sidebar_state.edit_channel = None;
                    this.load_tree(space, cx);
                    this.toast(
                        Some(("icons/check.svg", theme::text_soft())),
                        if is_category { "Category updated" } else { "Channel updated" },
                        cx,
                    );
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.sidebar_state.edit_channel.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(m) = this.sidebar_state.edit_channel.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not save changes.".to_owned());
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = this.sidebar_state.edit_channel.as_mut() {
                        m.busy = false;
                        m.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn edit_channel_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.edit_channel.as_ref()?;
        let ready = !modal.name.read(cx).text().trim().is_empty() && !modal.busy;
        let is_category = modal.is_category;
        let is_voice = modal.kind == ConversationKind::Voice;
        let nsfw = modal.nsfw;
        let parent_label = match modal.parent_id.as_deref() {
            Some(pid) => modal
                .categories
                .iter()
                .find(|(id, _)| id == pid)
                .map(|(_, name)| format!("Category: {name}"))
                .unwrap_or_else(|| "Category: None".to_owned()),
            None => "Category: None".to_owned(),
        };
        let field = |label: &'static str, input: Entity<crate::gui::input::TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().type_small().text_color(theme::text_soft()).child(label))
                .child(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::canvas())
                        .border_1()
                        .border_color(theme::border())
                        .type_ui()
                        .child(input),
                )
        };
        let card = div()
            .id("edit-channel-card")
            .w(px(400.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child(if is_category { "Rename category" } else { "Edit channel" }))
            .child(field("Name", modal.name.clone()))
            .when(!is_category && !is_voice, |card| card.child(field("Topic", modal.topic.clone())))
            .when(!is_category, |card| card.child(field("Slow mode (seconds)", modal.slow_mode.clone())))
            .when(!is_category && is_voice, |card| card.child(field("User limit", modal.user_limit.clone())))
            .when(!is_category, |card| {
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .when(!modal.categories.is_empty(), |row| {
                            row.child(
                                div()
                                    .id("edit-chan-parent")
                                    .type_small()
                                    .text_color(theme::accent_text())
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(m) = this.sidebar_state.edit_channel.as_mut() {
                                            let next = match m.parent_id.as_deref() {
                                                None => m.categories.first().map(|(id, _)| id.clone()),
                                                Some(cur) => {
                                                    let pos = m.categories.iter().position(|(id, _)| id == cur);
                                                    match pos {
                                                        Some(i) if i + 1 < m.categories.len() => Some(m.categories[i + 1].0.clone()),
                                                        _ => None,
                                                    }
                                                }
                                            };
                                            m.parent_id = next;
                                            cx.notify();
                                        }
                                    }))
                                    .child(parent_label),
                            )
                        })
                        .when(!is_voice, |row| {
                            row.child(
                                div()
                                    .id("edit-chan-nsfw")
                                    .type_small()
                                    .text_color(if nsfw { theme::accent_text() } else { theme::text_faint() })
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(m) = this.sidebar_state.edit_channel.as_mut() {
                                            m.nsfw = !m.nsfw;
                                            cx.notify();
                                        }
                                    }))
                                    .child(if nsfw { "☑ Age-restricted (NSFW)" } else { "☐ Age-restricted (NSFW)" }),
                            )
                        }),
                )
            })
            .children(modal.error.as_ref().map(|e| div().type_small().text_color(theme::danger()).child(e.clone())))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("edit-chan-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.edit_channel = None;
                        cx.notify();
                    })))
                    .child(
                        button(
                            "edit-chan-save",
                            if ready { ButtonKind::Primary } else { ButtonKind::Ghost },
                            if modal.busy { "Saving…" } else { "Save" },
                        )
                        .when(ready, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_edit_channel(cx)))),
                    ),
            );
        Some(
            div()
                .id("edit-channel-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.edit_channel = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_move_channels(&mut self, space: Key, cx: &mut Context<Self>) {
        let Some(tree) = self.trees.get(&space) else {
            self.load_tree(space, cx);
            return;
        };
        let categories: Vec<(String, String)> = tree.groups.iter().map(|g| (g.id.clone(), g.name.clone())).collect();
        let mut items = Vec::new();
        for group in &tree.groups {
            items.push(MoveChannelItem {
                id: group.id.clone(),
                name: group.name.clone(),
                is_category: true,
                parent_id: None,
            });
        }
        for conv in &tree.conversations {
            if conv.kind == ConversationKind::Thread {
                continue;
            }
            items.push(MoveChannelItem {
                id: conv.key.id.clone(),
                name: conv.title.trim_start_matches('#').to_owned(),
                is_category: false,
                parent_id: conv.group.clone(),
            });
        }
        self.sidebar_state.move_channels = Some(MoveChannelsModal {
            space,
            items,
            categories,
            busy: false,
            error: None,
        });
        cx.notify();
    }

    fn submit_move_channels(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.move_channels.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        modal.busy = true;
        modal.error = None;
        let space = modal.space.clone();
        let mut cat_pos = 0i32;
        let mut ch_pos = 0i32;
        let order: Vec<protocol::ChannelPosition> = modal
            .items
            .iter()
            .map(|item| {
                if item.is_category {
                    let pos = cat_pos;
                    cat_pos += 1;
                    protocol::ChannelPosition {
                        id: item.id.clone(),
                        position: pos,
                        parent_id: None,
                        lock_permissions: None,
                    }
                } else {
                    let pos = ch_pos;
                    ch_pos += 1;
                    protocol::ChannelPosition {
                        id: item.id.clone(),
                        position: pos,
                        parent_id: Some(item.parent_id.clone()),
                        lock_permissions: None,
                    }
                }
            })
            .collect();
        cx.notify();
        let req = Request::MoveChannels { space: space.clone(), order };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.sidebar_state.move_channels = None;
                    this.load_tree(space, cx);
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Channel order saved", cx);
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.sidebar_state.move_channels.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(m) = this.sidebar_state.move_channels.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not save channel order.".to_owned());
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = this.sidebar_state.move_channels.as_mut() {
                        m.busy = false;
                        m.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn move_channels_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.move_channels.as_ref()?;
        let categories = modal.categories.clone();
        let len = modal.items.len();
        let rows: Vec<_> = modal
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let parent_name = item.parent_id.as_deref().and_then(|pid| {
                    categories.iter().find(|(id, _)| id == pid).map(|(_, name)| name.clone())
                });
                let prefix = if item.is_category { "Category" } else { "#" };
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().type_small().text_color(theme::text_faint()).child(prefix))
                            .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(item.name.clone())),
                    )
                    .when(!item.is_category && !categories.is_empty(), |row| {
                        let cat_label = parent_name.unwrap_or_else(|| "No category".to_owned());
                        row.child(
                            div()
                                .id(("move-cat", ix))
                                .type_small()
                                .text_color(theme::accent_text())
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(m) = this.sidebar_state.move_channels.as_mut() {
                                        if let Some(entry) = m.items.get_mut(ix) {
                                            entry.parent_id = match entry.parent_id.as_deref() {
                                                None => m.categories.first().map(|(id, _)| id.clone()),
                                                Some(cur) => {
                                                    let pos = m.categories.iter().position(|(id, _)| id == cur);
                                                    match pos {
                                                        Some(i) if i + 1 < m.categories.len() => Some(m.categories[i + 1].0.clone()),
                                                        _ => None,
                                                    }
                                                }
                                            };
                                            cx.notify();
                                        }
                                    }
                                }))
                                .child(cat_label),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                icon_button(("move-up", ix), "icons/chevron-up.svg", false)
                                    .size(px(24.))
                                    .when(ix > 0, |btn| {
                                        btn.on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(m) = this.sidebar_state.move_channels.as_mut() {
                                                if ix > 0 && ix < m.items.len() {
                                                    m.items.swap(ix - 1, ix);
                                                    cx.notify();
                                                }
                                            }
                                        }))
                                    }),
                            )
                            .child(
                                icon_button(("move-down", ix), "icons/chevron-down.svg", false)
                                    .size(px(24.))
                                    .when(ix + 1 < len, |btn| {
                                        btn.on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(m) = this.sidebar_state.move_channels.as_mut() {
                                                if ix + 1 < m.items.len() {
                                                    m.items.swap(ix, ix + 1);
                                                    cx.notify();
                                                }
                                            }
                                        }))
                                    }),
                            ),
                    )
            })
            .collect();
        let card = div()
            .id("move-channels-card")
            .w(px(480.))
            .max_w_full()
            .max_h(px(560.))
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child("Reorder channels"))
            .child(
                div()
                    .id("move-channels-list")
                    .flex_1()
                    .min_h(px(160.))
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(rows),
            )
            .children(modal.error.as_ref().map(|e| div().type_small().text_color(theme::danger()).child(e.clone())))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("move-ch-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.move_channels = None;
                        cx.notify();
                    })))
                    .child(
                        button(
                            "move-ch-save",
                            if !modal.busy { ButtonKind::Primary } else { ButtonKind::Ghost },
                            if modal.busy { "Saving…" } else { "Save order" },
                        )
                        .when(!modal.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_move_channels(cx)))),
                    ),
            );
        Some(
            div()
                .id("move-channels-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.move_channels = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(super) fn open_delete_channel(&mut self, target: Key, name: String, is_category: bool, cx: &mut Context<Self>) {
        let clean = name.trim_start_matches('#').to_owned();
        let placeholder = format!("Type \"{clean}\" to confirm");
        let confirm = cx.new(|cx| crate::gui::input::TextInput::new(placeholder, cx));
        self.sidebar_state.delete_channel = Some(DeleteChannelModal {
            target,
            name: clean,
            is_category,
            confirm,
            busy: false,
            error: None,
        });
        cx.notify();
    }

    fn submit_delete_channel(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.sidebar_state.delete_channel.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let typed = modal.confirm.read(cx).text().trim();
        if typed != modal.name {
            modal.error = Some(format!("Type \"{}\" exactly to confirm deletion.", modal.name));
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let target = modal.target.clone();
        let is_category = modal.is_category;
        cx.notify();
        let req = Request::DeleteChannel { conversation: target.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.sidebar_state.delete_channel = None;
                    if this.open.as_ref() == Some(&target) {
                        this.open = None;
                    }
                    this.toast(
                        Some(("icons/trash-2.svg", theme::text_soft())),
                        if is_category { "Category deleted" } else { "Channel deleted" },
                        cx,
                    );
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.sidebar_state.delete_channel.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(m) = this.sidebar_state.delete_channel.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not delete.".to_owned());
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = this.sidebar_state.delete_channel.as_mut() {
                        m.busy = false;
                        m.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn delete_channel_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.delete_channel.as_ref()?;
        let matches_name = modal.confirm.read(cx).text().trim() == modal.name && !modal.busy;
        let kind_word = if modal.is_category { "category" } else { "channel" };
        let card = div()
            .id("delete-channel-card")
            .w(px(400.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child(format!("Delete {kind_word} {}?", modal.name)))
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(format!("This permanently deletes the {kind_word}. Type \"{}\" to confirm.", modal.name)),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(modal.confirm.clone()),
            )
            .children(modal.error.as_ref().map(|e| div().type_small().text_color(theme::danger()).child(e.clone())))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("delete-ch-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.delete_channel = None;
                        cx.notify();
                    })))
                    .child(
                        button(
                            "delete-ch-confirm",
                            if matches_name { ButtonKind::Danger } else { ButtonKind::Ghost },
                            if modal.busy { "Deleting…" } else { "Delete" },
                        )
                        .when(matches_name, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit_delete_channel(cx)))),
                    ),
            );
        Some(
            div()
                .id("delete-channel-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.delete_channel = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(in crate::gui) fn folder_edit_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.folder_edit.as_ref()?;
        let card = div()
            .id("folder-edit-card")
            .w(px(360.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child("Rename folder"))
            .child(div().type_small().text_color(theme::text_faint()).child("An empty name clears the folder name."))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(modal.name.clone()),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        crate::gui::components::button("folder-edit-cancel", crate::gui::components::ButtonKind::Ghost, "Cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.sidebar_state.folder_edit = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        crate::gui::components::button("folder-edit-save", crate::gui::components::ButtonKind::Primary, "Save")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_folder_rename(cx))),
                    ),
            );
        Some(
            div()
                .id("folder-edit-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.folder_edit = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    pub(in crate::gui) fn forum_post_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.sidebar_state.forum_post.as_ref()?;
        let title = modal.title.clone();
        let text = modal.text.clone();
        let guidelines = modal.guidelines.clone();
        let tags = modal.tags.clone();
        let picked = modal.picked.clone();
        let files = modal.files.clone();
        let require_tag = modal.require_tag;
        let media = modal.media;
        let error = modal.error.clone();
        let busy = modal.busy;
        let title_text = title.read(cx).text().trim().to_owned();
        let body_text = text.read(cx).text().trim().to_owned();
        let ready = !busy
            && !title_text.is_empty()
            && (!require_tag || !picked.is_empty())
            && (!media || !files.is_empty())
            && !(body_text.is_empty() && files.is_empty());
        let mut chips = div().flex().flex_wrap().gap(px(6.));
        for (ix, tag) in tags.iter().enumerate() {
            let on = picked.iter().any(|id| id == &tag.id);
            let id = tag.id.clone();
            let label = if tag.emoji.is_empty() { tag.name.clone() } else { format!("{} {}", tag.emoji, tag.name) };
            chips = chips.child(
                div()
                    .id(("forum-tag", ix))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(if on { theme::accent() } else { theme::canvas() })
                    .border_1()
                    .border_color(if on { theme::accent() } else { theme::border() })
                    .type_small()
                    .text_color(if on { theme::on_accent() } else { theme::text() })
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_forum_tag(&id, cx)))
                    .child(label),
            );
        }
        let mut attached = div().flex().flex_col().gap(px(4.));
        for (index, file) in files.iter().enumerate() {
            let name = file.name.clone();
            attached = attached.child(
                div()
                    .id(("forum-file", index))
                    .type_small()
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_forum_file(index, cx)))
                    .child(format!("{name}  ×")),
            );
        }
        let card = div()
            .id("forum-post-card")
            .w(px(420.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_title().text_color(theme::text()).child("New post"))
            .when(!guidelines.is_empty(), |card| {
                card.child(div().type_small().text_color(theme::text_soft()).child(guidelines))
            })
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(title),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .h(px(96.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .border_1()
                    .border_color(theme::border())
                    .type_ui()
                    .child(text),
            )
            .child(chips)
            .child(attached)
            .children(error.map(|err| div().type_small().text_color(theme::danger()).child(err)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        button("forum-post-attach", ButtonKind::Ghost, "Attach")
                            .when(!busy, |button| button.on_click(cx.listener(|this, _, _, cx| this.attach_forum_files(cx)))),
                    )
                    .child(
                        button("forum-post-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_state.forum_post = None;
                            cx.notify();
                        })),
                    )
                    .child(
                        button("forum-post-submit", if ready { ButtonKind::Primary } else { ButtonKind::Ghost }, if busy { "Posting…" } else { "Post" })
                            .when(ready, |button| button.on_click(cx.listener(|this, _, _, cx| this.submit_forum_post(cx)))),
                    ),
            );
        Some(
            div()
                .id("forum-post-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.sidebar_state.forum_post = None;
                    cx.notify();
                }))
                .child(card),
        )
    }

    /// The persistent Discord parent column, using the same home list and scroll state.
    pub(super) fn discord_overview(&self, snapshot: &Snapshot, width: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.sidebar_state.width.set(width);
        div().w(px(width)).h_full().flex_none().bg(theme::sidebar()).border_r_1().border_color(theme::hairline()).child(
            div().id("discord-overview").size_full().flex().flex_col()
                .child(self.title_strip(&Mode::Network(Network::Discord), cx))
                .child(self.jump_strip(cx))
                .child(div().flex_1().min_h_0().child(self.home_list(snapshot, window, cx)))
        )
    }

    /// Keep row selection on arrows; paging keys scroll the list that already has focus.
    pub(super) fn page_sidebar(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.in_lists(window) || self.sidebar_state.switcher.is_some() || self.layout_state.menu.is_some() { return; }
        let state = &self.sidebar_state;
        let target: crate::gui::components::scroll::ScrollTarget = match &state.mode {
            Mode::Inbox if !state.inbox.fresh => state.inbox_list.clone().into(),
            Mode::Inbox | Mode::People | Mode::Network(Network::Tiny) => return,
            _ if state.listed() => state.home_list.clone().into(),
            Mode::Space(_) | Mode::Network(_) if !state.rows.is_empty() => state.tree_scroll.clone().into(),
            _ => return,
        };
        if target.page_key(event) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn sidebar(&self, snapshot: &Snapshot, width: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let people = self.sidebar_state.mode == Mode::People;
        let tiny = matches!(self.sidebar_state.mode, Mode::Network(Network::Tiny));
        if !matches!(&self.sidebar_state.mode, Mode::Space(space) if space.network == Network::Discord) {
            self.sidebar_state.width.set(width);
        }
        let body = if people {
            let drawn = self.slots.people.and_then(|draw| draw(self, window, cx));
            drawn.unwrap_or_else(|| empty_state("icons/users.svg", "People", "Friends and contacts show up here.").px(px(24.)).into_any_element())
        } else {
            self.list(snapshot, window, cx)
        };
        let banners = self.banners(snapshot, cx);
        let tabs = self.folder_tabs(cx);
        let phone = super::phone::active();
        div()
            .relative()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            // A phone's sidebar is the screen's right edge: no hairline, and it keeps clear of
            // the status bar and the home indicator.
            .when(!phone, |sidebar| sidebar.border_r_1().border_color(theme::hairline()))
            .when(phone, |sidebar| sidebar.pt(px(super::phone::tokens::safe_top())).pb(px(super::phone::tokens::safe_bottom())))
            .child(self.title_strip(&self.sidebar_state.mode, cx))
            .when(!people && !tiny, |sidebar| sidebar.child(self.jump_strip(cx)))
            .children(tabs)
            .when(!banners.is_empty(), |sidebar| sidebar.child(div().flex_none().px(px(theme::INSET)).pb(px(8.)).flex().flex_col().gap(px(4.)).children(banners)))
            .children(self.space_events_strip(cx))
            .child(
                div()
                    .id("sidebar-list")
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.sidebar_state.focus)
                    .key_context("Sidebar")
                    // A press that travels past the slop is a scroll: the rows check it on release.
                    .when(phone, |list| {
                        list.on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, _| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                this.phone_state.press_move(super::phone::at(event.position));
                            }
                        }))
                    })
                    .child(body),
            )
            .children(super::playing::draw(self, window, cx))
            .children(self.slots.call_dock.and_then(|draw| draw(self, window, cx)))
            .children(tiny.then(|| self.tiny_user_panel(cx)).flatten())
            .when(!phone, |sidebar| sidebar.children(self.slack_menu(cx)))
    }
}

/// An occupied voice row's trailing count: `4/12` when the channel has a user limit, else just
/// `4` (`02` §4), and whether the number is drawn `text_soft`. `None` for an empty channel.
fn voice_count(total: u32, limit: u32) -> Option<(String, bool)> {
    (total > 0).then(|| {
        let limited = limit > 0;
        (if limited { format!("{total}/{limit}") } else { total.to_string() }, limited)
    })
}

/// A stage channel's trailing words: only who is on stage counts, the audience does not.
/// Nobody on stage reads as an empty channel, so nothing (`02` §4).
fn stage_line(occupants: &Occupants) -> Option<String> {
    let speaking = occupants.people.iter().filter(|occupant| occupant.speaking).count();
    (speaking > 0).then(|| format!("Live · {speaking} speaking"))
}

/// Whether a voice row draws its title faint: an empty voice channel, or a stage with
/// nobody on stage. An audience-only stage is therefore faint, even though its people
/// are still listed.
fn empty_voice_row(kind: ConversationKind, stage: bool, selected: bool, has_occupants: bool, has_live: bool) -> bool {
    kind == ConversationKind::Voice && !selected && (!has_occupants || (stage && !has_live))
}

/// The count a row trails: a normal voice channel shows how many are in it; a stage
/// shows none, since its trailing words are the live count instead.
fn row_count(stage: bool, inside: Option<&Occupants>) -> Option<String> {
    if stage { None } else { inside.map(|row| row.total.to_string()) }
}

/// A tree row's name: a channel without its `#`; a Slack group DM by first names, "Mara,
/// Dev, +2", which would otherwise truncate to nothing.
fn tree_title(c: &Conversation) -> String {
    let title = c.title.trim_start_matches('#');
    if c.kind != ConversationKind::Group || c.key.network != Network::Slack {
        return title.to_owned();
    }
    let names: Vec<&str> = title.split(',').map(|name| name.split_whitespace().next().unwrap_or("")).filter(|name| !name.is_empty()).collect();
    match names.len() {
        0..=3 => names.join(", "),
        n => format!("{}, +{}", names[..2].join(", "), n - 2),
    }
}

/// How urgent an account's banner is, lowest first: signed out, then the network wanting
/// something done in its app, then a link that is reconnecting.
fn urgency(tone: Tone) -> u8 {
    match tone {
        Tone::Danger => 0,
        Tone::Attention => 1,
        Tone::Warning => 2,
        Tone::Info | Tone::Keychain => 3,
    }
}

/// A Slack demand can be finished here. A workspace that blocks this browser is words only.
fn slack_can_continue(network: Network, words: &str) -> bool {
    network == Network::Slack && !words.contains("from this browser")
}

/// Where the network's own app is, for what only it can do (verify an account, solve a captcha).
fn network_app(network: Network) -> &'static str {
    match network {
        Network::Discord => "https://discord.com/app",
        Network::Slack => "https://app.slack.com/client",
        Network::Telegram => "https://web.telegram.org/",
        Network::Teams => "https://teams.microsoft.com/",
        // Signal has no web app: what only it can do is done on the phone.
        Network::Signal => "https://signal.org/download/",
        Network::Messages => "https://messages.google.com/web/",
        // Nothing only a Tiny server's own app can do is asked from here.
        Network::Tiny => "",
    }
}

/// What a pane says about one account's link, and in which tone. `several`: its network has
/// more than one account, so the account is named. `mixed`: the pane lists every account (the
/// Inbox), where only what needs the user is said. `long` answers whether the link has been
/// down long enough to mention.
fn banner_words(network: Network, several: bool, status: &protocol::AccountStatus, mixed: bool, long: impl Fn() -> bool) -> Option<(Tone, String)> {
    let (name, account) = (network.label(), status.name.as_str());
    // The network's own words for what it wants done in its app.
    if !status.required.is_empty() {
        return Some((Tone::Attention, status.required.clone()));
    }
    match status.link {
        Link::Expired if several => Some((Tone::Danger, format!("{name} signed you out of {account}"))),
        Link::Expired => Some((Tone::Danger, format!("{name} signed you out"))),
        Link::Offline if long() => Some(match (status.reconnecting, several || mixed) {
            (true, false) => (Tone::Warning, "Reconnecting…".to_owned()),
            (true, true) => (Tone::Warning, format!("{name} is reconnecting")),
            (false, false) => (Tone::Warning, format!("Can't reach {name}")),
            (false, true) => (Tone::Warning, format!("Can't reach {name} · {account}")),
        }),
        _ => None,
    }
}

/// Why a sidebar action did not happen, in words. `Ok` is silence.
fn refused(answer: &std::io::Result<Response>) -> Option<String> {
    match answer {
        Ok(Response::Ok) => None,
        Ok(Response::Error { detail, .. }) => Some(detail.clone()),
        Err(err) => Some(crate::client::trouble_words("inbox", err)),
        _ => Some(crate::client::OTHER_ANSWER.to_owned()),
    }
}

/// How the Inbox title strip fits at this sidebar width
/// (`docs/superpowers/specs/2026-11-02-inbox-strip-fit-design.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InboxStripFit {
    stacked: bool,
    counts: bool,
    mark_all: bool,
}

/// Squeeze ladder for the Inbox strip: fixed title never truncates; counts, mark-all, then stack.
fn inbox_strip_fit(width: f32, phone: bool, text_scale: f32, has_unread: bool) -> InboxStripFit {
    // Phone large type always stacks. Desktop stacks below 280 so All/Unread/Mentions stay whole.
    let stacked = (phone && text_scale > 1.25) || (!phone && width < 280.);
    // Phone keeps its ≥310 one-line count rule. Desktop: counts at ≥360 one-line, or ≥270 when stacked.
    let counts = if phone {
        !stacked && width >= 310.
    } else if stacked {
        width >= 270.
    } else {
        width >= 360.
    };
    // Mark-all stays out of the phone strip; on desktop it yields before the title or filters.
    let mark_all = has_unread && !phone && (stacked || width >= 300.);
    InboxStripFit { stacked, counts, mark_all }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use protocol::{AccountKey, AccountStatus, Presence};

    #[test]
    fn a_slack_demand_can_continue_and_a_blocked_browser_cannot() {
        let demand = "Slack wants two-factor authentication set up. Continue in tinyclient for this workspace.";
        assert!(slack_can_continue(Network::Slack, demand));
        assert!(!slack_can_continue(Network::Slack, "This workspace does not allow signing in from this browser."));
        assert!(!slack_can_continue(Network::Discord, demand));
    }

    #[cfg(feature = "gui-shot")]
    #[gpui::test]
    fn telegram_folder_survives_saving_after_leaving_its_network(cx: &mut gpui::TestAppContext) {
        use crate::gui::app::prefs::{self, Prefs};
        let mut state = State::with_focus(cx.update(|cx| cx.focus_handle()));
        state.remember_telegram_folder(Some("2".into()));
        assert_eq!(state.telegram_folder().as_deref(), Some("2"), "the startup preference survives before a home is shown");
        for mode in [Mode::Network(Network::Telegram), Mode::Network(Network::Discord), Mode::Inbox] {
            state.switch_mode(mode);
            let saved = Prefs { telegram_folder: state.telegram_folder(), ..Prefs::default() };
            let restored = prefs::parse(&prefs::write(&saved));
            assert_eq!(restored.telegram_folder.as_deref(), Some("2"), "folder lost in {:?}", state.mode);
            let mut relaunched = State::with_focus(cx.update(|cx| cx.focus_handle()));
            relaunched.remember_telegram_folder(restored.telegram_folder);
            relaunched.switch_mode(Mode::Network(Network::Telegram));
            assert_eq!(relaunched.home.folder.as_deref(), Some("2"));
        }
        state.switch_mode(Mode::Network(Network::Telegram));
        state.home.folder = None;
        let saved = Prefs { telegram_folder: state.telegram_folder(), ..Prefs::default() };
        assert_eq!(prefs::parse(&prefs::write(&saved)).telegram_folder, None, "choosing All replaces the saved chip");
        state.switch_mode(Mode::Inbox);
        assert_eq!(state.telegram_folder(), None, "All is remembered after leaving Telegram too");
    }

    #[cfg(feature = "gui-shot")]
    fn forum(id: &str) -> Conversation {
        Conversation::new(Key { network: Network::Telegram, account: "me".into(), id: id.into() }, ConversationKind::Forum, id)
    }

    #[cfg(feature = "gui-shot")]
    #[gpui::test]
    fn telegram_landing_only_opens_for_the_latest_forum_visit(cx: &mut gpui::TestAppContext) {
        let mut state = State::with_focus(cx.update(|cx| cx.focus_handle()));
        state.switch_mode(Mode::Network(Network::Telegram));
        let first = forum("first");
        let second = forum("second");
        let first_topics = [Conversation::new(Key { id: "first/topic/1".into(), ..first.key.clone() }, ConversationKind::Thread, "General")];
        let topics = [Conversation::new(Key { id: "second/topic/1".into(), ..second.key.clone() }, ConversationKind::Thread, "General")];
        state.enter_drill(home::Drill::Topics(Rc::new(first.clone())));
        let first_visit = state.navigation;
        state.enter_drill(home::Drill::Topics(Rc::new(second.clone())));
        let second_visit = state.navigation;
        assert_eq!(state.landing_topic(second_visit, &second.key, &topics).map(|c| c.key.id.as_str()), Some("second/topic/1"));
        assert!(state.landing_topic(first_visit, &first.key, &first_topics).is_none(), "an older forum reply must not replace the new landing");
        state.enter_drill(home::Drill::Topics(Rc::new(second.clone())));
        let repeated_visit = state.navigation;
        assert!(state.landing_topic(second_visit, &second.key, &topics).is_none(), "reopening the same forum supersedes its first request");
        assert!(state.landing_topic(repeated_visit, &second.key, &topics).is_some());
        assert!(state.landing_topic(repeated_visit, &second.key, &[]).is_none());
    }

    #[cfg(feature = "gui-shot")]
    #[gpui::test]
    fn telegram_landing_does_not_override_navigation_away_from_the_drill(cx: &mut gpui::TestAppContext) {
        let mut state = State::with_focus(cx.update(|cx| cx.focus_handle()));
        state.switch_mode(Mode::Network(Network::Telegram));
        let forum = forum("forum");
        let topics = [Conversation::new(Key { id: "forum/topic/1".into(), ..forum.key.clone() }, ConversationKind::Thread, "General")];
        for destination in [Mode::Inbox, Mode::Network(Network::Discord)] {
            state.enter_drill(home::Drill::Topics(Rc::new(forum.clone())));
            let request = state.navigation;
            state.switch_mode(destination);
            assert!(state.landing_topic(request, &forum.key, &topics).is_none(), "a forum must not reopen after changing modes");
            state.switch_mode(Mode::Network(Network::Telegram));
        }
        state.enter_drill(home::Drill::Topics(Rc::new(forum.clone())));
        let request = state.navigation;
        assert!(state.leave_drill());
        assert!(state.landing_topic(request, &forum.key, &topics).is_none(), "leaving the drill cancels its landing");
        state.enter_drill(home::Drill::Topics(Rc::new(forum.clone())));
        let request = state.navigation;
        state.enter_drill(home::Drill::Archive);
        assert!(state.landing_topic(request, &forum.key, &topics).is_none(), "another drill supersedes the landing");
    }

    #[cfg(feature = "gui-shot")]
    #[gpui::test]
    fn telegram_landing_respects_an_explicit_conversation_choice(cx: &mut gpui::TestAppContext) {
        let mut state = State::with_focus(cx.update(|cx| cx.focus_handle()));
        state.switch_mode(Mode::Network(Network::Telegram));
        let forum = forum("forum");
        let topics = [Conversation::new(Key { id: "forum/topic/1".into(), ..forum.key.clone() }, ConversationKind::Thread, "General")];
        state.enter_drill(home::Drill::Topics(Rc::new(forum.clone())));
        let request = state.navigation;
        state.home.epoch += 1;
        assert!(state.landing_topic(request, &forum.key, &topics).is_some(), "a background home refresh is not navigation");
        state.navigation_changed();
        assert!(state.landing_topic(request, &forum.key, &topics).is_none(), "a chat or topic picked by hand wins, even when the drill stays open");
    }

    pub fn row(id: &str, kind: ConversationKind) -> Conversation {
        Conversation::new(Key { network: Network::Discord, account: "me".into(), id: id.into() }, kind, id)
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
            status_emoji: String::new(),
            status_until: None,
            can: Default::default(),
            send_as: Vec::new(),
        }
    }

    #[test]
    fn a_sidebar_action_says_when_it_was_refused_or_never_arrived() {
        assert_eq!(refused(&Ok(Response::Ok)), None);
        let detail = refused(&Ok(Response::Error {
            code: protocol::ErrorCode::Internal,
            detail: "no".into(),
        }));
        assert_eq!(detail.as_deref(), Some("no"));
        assert!(refused(&Err(std::io::Error::other("down"))).is_some());
        assert_eq!(
            refused(&Ok(Response::Pong)).as_deref(),
            Some(crate::client::OTHER_ANSWER)
        );
    }

    #[test]
    fn a_pane_says_what_is_wrong_with_a_link_in_the_specs_words_and_nothing_when_healthy() {
        let say = |status: &AccountStatus, several, mixed, long| banner_words(Network::Slack, several, status, mixed, || long).map(|(_, words)| words);
        assert_eq!(say(&status(Link::Connected, false, ""), false, false, true), None);
        assert_eq!(say(&status(Link::Expired, false, ""), false, true, false).as_deref(), Some("Slack signed you out"));
        assert_eq!(say(&status(Link::Expired, false, ""), true, false, false).as_deref(), Some("Slack signed you out of Fernhill"));
        // The first seconds of any connect say nothing; then it depends on whether chatd is trying.
        assert_eq!(say(&status(Link::Offline, true, ""), false, false, false), None);
        assert_eq!(say(&status(Link::Offline, true, ""), false, false, true).as_deref(), Some("Reconnecting…"));
        assert_eq!(say(&status(Link::Offline, false, ""), false, false, true).as_deref(), Some("Can't reach Slack"));
        // The Inbox mixes networks, so it names the one that is late.
        assert_eq!(say(&status(Link::Offline, true, ""), false, true, true).as_deref(), Some("Slack is reconnecting"));
        let wanted = "Discord wants you to verify your account in its app.";
        assert_eq!(say(&status(Link::Connected, false, wanted), false, true, false).as_deref(), Some(wanted));
    }

    #[test]
    fn an_occupied_voice_row_counts_4_of_12_when_limited_and_bare_without() {
        assert_eq!(voice_count(4, 12), Some(("4/12".to_owned(), true)));
        assert_eq!(voice_count(4, 0), Some(("4".to_owned(), false)));
        // An empty channel has nothing trailing, limit or not.
        assert_eq!(voice_count(0, 12), None);
    }

    #[test]
    fn the_most_urgent_account_speaks_signed_out_then_needs_you_then_reconnecting() {
        let mut tones = vec![Tone::Warning, Tone::Attention, Tone::Danger];
        tones.sort_by_key(|tone| urgency(*tone));
        assert!(tones == [Tone::Danger, Tone::Attention, Tone::Warning]);
    }

    #[test]
    fn a_mixed_list_knows_which_rows_are_on_screen_from_its_heights() {
        let list = ListState::new(10, ListAlignment::Top, px(0.));
        // No layout yet: a window's worth from the top.
        assert_eq!(on_screen(&list, 10, |_| 48.), 0..10);
        assert_eq!(on_screen(&list, 40, |_| 48.), 0..19);
        list.scroll_to(gpui::ListOffset { item_ix: 5, offset_in_item: px(10.) });
        assert_eq!(on_screen(&list, 40, |_| 100.).start, 5);
    }

    #[test]
    fn a_preview_keeps_its_place_and_only_a_move_splices() {
        assert_eq!(splice_range(&["a", "b", "c"], &["a", "b", "c"]), None);
        assert_eq!(splice_range(&["a", "b"], &["a", "b", "c"]), Some((2..2, 1)));
        assert_eq!(splice_range(&["a", "b", "c"], &["a", "c"]), Some((1..2, 0)));
        assert_eq!(splice_range(&["a"], &["b", "a"]), Some((0..0, 1)));
        let list = ListState::new(3, ListAlignment::Top, px(0.));
        list.scroll_to(gpui::ListOffset { item_ix: 2, offset_in_item: px(4.) });
        let old = ["a", "b", "c"];
        respliced(&list, &old, &old, |item| Some((*item).to_owned()), |_| 48.);
        let top = list.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item, list.item_count()), (2, px(4.), 3));
    }

    #[test]
    fn a_stage_row_reads_as_a_radio_and_a_voice_channel_as_a_speaker() {
        let stage = Conversation { call: CallSupport::Handoff, ..row("stage", ConversationKind::Voice) };
        let voice = row("voice", ConversationKind::Voice);
        assert_eq!(kind_glyph(stage.kind, stage.private, stage.call), "icons/radio.svg");
        assert_eq!(kind_glyph(voice.kind, voice.private, voice.call), "icons/volume-2.svg");
        assert_eq!(kind_glyph(ConversationKind::Channel, false, CallSupport::None), "icons/hash.svg");
    }

    #[test]
    fn a_native_voice_row_joins_from_the_list_and_nothing_else_does() {
        let voice = |call| {
            let mut c = row("lounge", ConversationKind::Voice);
            c.call = call;
            c
        };
        assert!(joins_from_list(&voice(CallSupport::Native)));
        // A stage opens in Discord instead, and an empty channel's pill cannot join either.
        assert!(!joins_from_list(&voice(CallSupport::Handoff)));
        assert!(!joins_from_list(&voice(CallSupport::None)));
        // A text channel's click only opens, and a DM call joins from its header.
        assert!(!joins_from_list(&row("general", ConversationKind::Channel)));
        assert!(!joins_from_list(&row("mara", ConversationKind::Direct)));
    }

    fn on_stage(speaking: &[bool]) -> Occupants {
        let person = row("2001", ConversationKind::Voice).key;
        Occupants {
            people: speaking
                .iter()
                .map(|live| protocol::Occupant { person: person.clone(), name: "Mara O".into(), avatar: None, muted: false, deafened: false, server_muted: false, server_deafened: false, suppressed: false, streaming: false, camera: false, speaking: *live })
                .collect(),
            total: speaking.len() as u32,
        }
    }

    #[test]
    fn a_stage_row_says_who_is_live_and_an_audience_only_one_reads_as_empty() {
        assert_eq!(stage_line(&on_stage(&[false, true, true])).as_deref(), Some("Live · 2 speaking"));
        assert_eq!(stage_line(&on_stage(&[true])).as_deref(), Some("Live · 1 speaking"));
        // Only the audience: the design's empty stage, so no words trail.
        assert_eq!(stage_line(&on_stage(&[false, false])), None);
        assert_eq!(stage_line(&Occupants::default()), None);
    }

    #[test]
    fn a_stage_row_carries_no_count_and_faints_when_only_the_audience_is_present() {
        let occupants = on_stage(&[false, false]);
        let inside = Some(&occupants);
        // A stage never trails a total: its trailing words are the live count.
        assert_eq!(row_count(true, inside), None);
        assert_eq!(row_count(true, None), None);
        // A normal voice channel still trails its total.
        assert_eq!(row_count(false, inside), Some("2".to_owned()));
        assert_eq!(row_count(false, None), None);

        // An audience-only stage reads as empty, so its title is faint.
        assert!(empty_voice_row(ConversationKind::Voice, true, false, true, false));
        // A stage with someone on it is not faint.
        assert!(!empty_voice_row(ConversationKind::Voice, true, false, true, true));
        // An empty voice channel is faint too.
        assert!(empty_voice_row(ConversationKind::Voice, false, false, false, false));
        // A selected row is never faint, even when empty.
        assert!(!empty_voice_row(ConversationKind::Voice, true, true, true, false));
        // Non-voice rows are untouched.
        assert!(!empty_voice_row(ConversationKind::Channel, true, false, true, false));
    }

    #[test]
    fn inbox_strip_fit_squeezes_by_width_without_truncating_the_title() {
        let full = inbox_strip_fit(400., false, 1., true);
        assert_eq!(full, InboxStripFit { stacked: false, counts: true, mark_all: true });

        // Default 316: counts drop so Inbox + segments + mark-all fit on one line.
        let mid = inbox_strip_fit(316., false, 1., true);
        assert_eq!(mid, InboxStripFit { stacked: false, counts: false, mark_all: true });

        let lean = inbox_strip_fit(290., false, 1., true);
        assert_eq!(lean, InboxStripFit { stacked: false, counts: false, mark_all: false });

        // Minimum sidebar: stack; mark-all on the title row; counts when the second row has room.
        let narrow = inbox_strip_fit(260., false, 1., true);
        assert_eq!(narrow, InboxStripFit { stacked: true, counts: false, mark_all: true });
        let stacked_roomy = inbox_strip_fit(270., false, 1., true);
        assert_eq!(stacked_roomy, InboxStripFit { stacked: true, counts: true, mark_all: true });

        assert!(!inbox_strip_fit(400., false, 1., false).mark_all);

        // Phone: no mark-all; large type stacks; baseline keeps the ≥310 count rule.
        assert_eq!(inbox_strip_fit(326., true, 1.3, true), InboxStripFit { stacked: true, counts: false, mark_all: false });
        assert_eq!(inbox_strip_fit(326., true, 1., true), InboxStripFit { stacked: false, counts: true, mark_all: false });
        assert_eq!(inbox_strip_fit(296., true, 1., true), InboxStripFit { stacked: false, counts: false, mark_all: false });
    }

    #[test]
    fn location_round_trips_tiny_channel_in_history() {
        let mut history = history::History::default();
        let inbox = Location { mode: Mode::Inbox, open: Some((Key { network: Network::Tiny, account: "a".into(), id: "dm-1".into() }, "Bea".into())), tiny_channel: None };
        let room = Location { mode: Mode::Network(Network::Tiny), open: None, tiny_channel: Some("lounge".into()) };
        history.leave(inbox.clone());
        assert_eq!(history.travel(true, room.clone()), Some(inbox.clone()));
        assert_eq!(history.travel(false, inbox), Some(room));
    }
}
