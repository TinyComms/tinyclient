//! What the shell knows, and how it stays current. It reads what is on screen once, then holds
//! one `Wait` open; each change chatd reports names the one thing to read again. Every socket
//! call runs off the main thread.

use super::layout::{self, Slots};
use super::sidebar::{self, pages::PAGE, Mode};
use super::{call, composer, conversation, people};
use crate::client::ask;
use gpui::{Context, FocusHandle, Subscription, Window};
use protocol::{
    AccountKey, AccountStatus, Call, Change, Conversation, ErrorCode, Group, InboxFilter, Key, Link,
    MediaFile, Network, NetworkStatus, Occupants, Page, Request, Response, Space, PAGE_LIMIT,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How soon to look for chatd again when it is not answering.
const RETRY: Duration = Duration::from_secs(2);
/// A tree page, as large as chatd gives.
const TREE_PAGE: u32 = 500;
/// The most rows of one Space the shell holds.
// CONTRACT: `Tree` pages by conversation, in position order across Groups, and says nothing of
// how many rows each Group holds. A list with headers and collapsed Groups cannot be laid out
// from a window of that, so one Space is read whole, up to this many rows.
const TREE_LIMIT: u32 = 4000;
/// Paths of files that landed after their row was read, kept until this many.
const MEDIA_KEPT: usize = 2000;
/// A link that has been down this long says so: the first seconds of any connect say nothing.
pub const TROUBLE_SHOWN: Duration = Duration::from_secs(5);

/// What a sign-in that did not finish says under its row (`08-first-run-settings.md`, section
/// 2). `None` when the user closed the window: the screen is exactly as before.
// CONTRACT: `ErrorCode::SignIn` covers both "the user closed the window" and every other
// failure, so the two are told apart by chatd's words.
pub fn sign_in_words(network: Network, code: ErrorCode, detail: &str) -> Option<String> {
    let name = match network {
        Network::Teams => "Microsoft Teams",
        other => other.label(),
    };
    match code {
        ErrorCode::SignIn if detail.contains("was closed") => None,
        ErrorCode::SignIn if detail.contains("stopped before") => {
            Some("The sign-in window closed unexpectedly. Try again.".to_owned())
        }
        ErrorCode::SignInUnreachable => Some(format!("Couldn't reach {name}. Check your connection and try again.")),
        ErrorCode::SignInBlocked => Some(format!(
            "Your organization doesn't allow this app. {name} stays in the {name} app for this account."
        )),
        _ => Some(detail.to_owned()),
    }
}

/// The links, accounts and call, as chatd last said.
pub struct Snapshot {
    pub networks: Vec<NetworkStatus>,
    pub accounts: Vec<AccountStatus>,
    pub call: Option<Call>,
    /// The open conversation's row, when one is open and still exists.
    pub open: Option<Conversation>,
}

impl Snapshot {
    pub fn link(&self, network: Network) -> Link {
        self.networks
            .iter()
            .find(|status| status.network == network)
            .map_or(Link::SignedOut, |status| status.link)
    }

    /// The link of the account a conversation belongs to.
    pub fn account_link(&self, conversation: &Key) -> Link {
        let account = conversation.account();
        let known = self.accounts.iter().find(|status| status.key == account);
        known.map_or_else(|| self.link(conversation.network), |status| status.link)
    }

    pub fn account(&self, account: &AccountKey) -> Option<&AccountStatus> {
        self.accounts.iter().find(|status| &status.key == account)
    }

    /// How many accounts of a network are signed in: more than one, and rows say which.
    pub fn accounts_of(&self, network: Network) -> usize {
        self.accounts.iter().filter(|status| status.key.network == network).count()
    }

    /// The number on a rail button: what needs me on one network, or on all of them.
    pub fn attention(&self, network: Option<Network>) -> u32 {
        self.of(network).map(|account| account.attention).sum()
    }

    /// Something unmuted is unread that has no number.
    pub fn unread(&self, network: Option<Network>) -> bool {
        self.of(network).any(|account| account.unread)
    }

    fn of(&self, network: Option<Network>) -> impl Iterator<Item = &AccountStatus> {
        self.accounts
            .iter()
            .filter(move |account| network.is_none_or(|network| account.key.network == network))
    }

    /// The networks with a session, in the rail's order.
    pub fn signed_in(&self) -> Vec<Network> {
        layout::NETWORK_ORDER
            .into_iter()
            .filter(|network| self.link(*network) != Link::SignedOut)
            .collect()
    }
}

/// One Space's tree, read whole (see `TREE_LIMIT`).
#[derive(Default)]
pub struct SpaceTree {
    pub groups: Vec<Group>,
    pub conversations: Vec<Rc<Conversation>>,
    /// False until the first answer: the rows are placeholders.
    pub loaded: bool,
    epoch: u64,
}

fn unexpected(response: Response) -> io::Error {
    io::Error::other(format!("unexpected {response:?}"))
}

/// What a batch of changes asks to be read again.
#[derive(Default, Debug, PartialEq)]
struct Dirty {
    status: bool,
    spaces: bool,
    inbox: bool,
    trees: HashSet<Key>,
    rows: Vec<Key>,
}

impl Dirty {
    /// One change names one thing to read. What is ephemeral, or another view's, names nothing.
    fn note(&mut self, change: &Change) {
        match change {
            Change::Status => self.status = true,
            Change::Conversation { conversation } => {
                self.inbox = true;
                // A row's unread state is also its Space's.
                self.spaces = true;
                if !self.rows.contains(conversation) {
                    self.rows.push(conversation.clone());
                }
            }
            Change::Spaces { .. } => self.spaces = true,
            Change::Tree { space } => {
                self.trees.insert(space.clone());
            }
            Change::Messages { .. }
            | Change::People { .. }
            | Change::Media { .. }
            | Change::Typing { .. }
            | Change::Presence { .. }
            | Change::Members { .. }
            | Change::Occupants { .. }
            | Change::Searched { .. }
            | Change::SignIn { .. }
            | Change::Call { .. }
            | Change::Upload { .. } => {}
            Change::Folders { .. } => self.spaces = true,
        }
    }
}

pub struct Shell {
    pub(super) snapshot: Option<Snapshot>,
    /// Why there is no snapshot.
    pub(super) down: Option<String>,
    /// What chatd said about the last send or call that failed.
    pub(super) refused: Option<String>,
    /// The network whose sign-in window is open.
    pub(super) signing_in: Option<Network>,
    /// What chatd said about the last sign-in that did not finish.
    pub(super) sign_in_failed: Option<(Network, String)>,
    pub(super) open: Option<Key>,
    /// The window is the one the user is working in. Only then is what is open read.
    pub(super) active: bool,

    /// Every Space of every network, in the networks' order. Small by nature.
    pub(super) spaces: Vec<Space>,
    /// The trees on show: the Space the sidebar is in, or those open in a network home.
    pub(super) trees: HashMap<Key, SpaceTree>,
    /// Files that landed after the row naming them was read.
    media: HashMap<(AccountKey, String), PathBuf>,
    media_asked: HashSet<(AccountKey, String)>,
    /// When each account's link was first seen down, for the banner's threshold.
    pub(super) trouble: HashMap<AccountKey, Instant>,
    /// The window's top left corner in screen points, as last drawn: where sign-in opens.
    pub(super) window_at: Cell<Option<(i32, i32)>>,

    /// Keys go to the shell when nothing inside has focus.
    pub(super) focus: FocusHandle,
    /// What the other slices draw into this slice's frames.
    pub(super) slots: Slots,
    pub(super) layout_state: layout::State,
    pub(super) sidebar_state: sidebar::State,
    // Each view's own state lives in its own file. They are only held here.
    #[allow(dead_code)]
    pub(super) conversation_state: conversation::State,
    #[allow(dead_code)]
    pub(super) people_state: people::State,
    #[allow(dead_code)]
    pub(super) composer_state: composer::State,
    #[allow(dead_code)]
    pub(super) call_state: call::State,
    _subscriptions: [Subscription; 1],
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Shell {
        cx.spawn(async move |this, cx| loop {
            let status = cx.background_executor().spawn(async { ask(&Request::Status) }).await;
            let mut since = match status {
                Ok(Response::Status { networks, accounts, call, seq, .. }) => {
                    let applied = this.update(cx, |this, cx| {
                        this.apply_status(networks, accounts, call, cx);
                        this.reload_all(cx);
                    });
                    if applied.is_err() {
                        return;
                    }
                    seq
                }
                other => {
                    let why = match other {
                        Ok(response) => unexpected(response).to_string(),
                        Err(err) => err.to_string(),
                    };
                    let told = this.update(cx, |this, cx| {
                        this.snapshot = None;
                        this.down = Some(why);
                        cx.notify();
                    });
                    if told.is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY).await;
                    continue;
                }
            };
            // chatd holds each request until something changes, so an idle shell does not wake.
            loop {
                let wait = async move { ask(&Request::Wait { since }) };
                let Ok(Response::Changes { seq, reset, changes }) = cx.background_executor().spawn(wait).await
                else {
                    break;
                };
                since = seq;
                let applied = this.update(cx, |this, cx| {
                    if reset {
                        this.load_status(cx);
                        this.reload_all(cx);
                        this.conversation_reset(cx);
                    } else {
                        this.changed(&changes, cx);
                    }
                });
                if applied.is_err() {
                    return;
                }
            }
        })
        .detach();

        let subscriptions = [cx.observe_window_activation(window, |this, window, cx| {
            this.active = window.is_window_active();
            cx.notify();
        })];
        let mut shell = Shell {
            snapshot: None,
            down: None,
            refused: None,
            signing_in: None,
            sign_in_failed: None,
            open: None,
            active: window.is_window_active(),
            spaces: Vec::new(),
            trees: HashMap::new(),
            media: HashMap::new(),
            media_asked: HashSet::new(),
            trouble: HashMap::new(),
            window_at: Cell::new(None),
            focus: cx.focus_handle(),
            // The other slices' views, placed in this slice's frames.
            slots: Slots {
                people: Some(people::draw_people),
                own_menu: Some(people::draw_own_menu),
                presence: Some(people::presence_slot),
                typing: Some(people::typing_slot),
                call_dock: Some(call::dock),
                search: None,
            },
            layout_state: layout::State::default(),
            sidebar_state: sidebar::State::new(cx),
            conversation_state: conversation::State::default(),
            people_state: people::State::default(),
            composer_state: composer::State::default(),
            call_state: call::State::default(),
            _subscriptions: subscriptions,
        };
        shell.load_prefs(cx);
        shell
    }

    /// Runs `work` off the main thread and hands its answer back to the shell.
    pub(super) fn off_thread<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        then: impl FnOnce(&mut Shell, R, &mut Context<Shell>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| then(this, answer, cx));
        })
        .detach();
    }

    fn apply_status(
        &mut self,
        networks: Vec<NetworkStatus>,
        accounts: Vec<AccountStatus>,
        call: Option<Call>,
        cx: &mut Context<Self>,
    ) {
        let open = self.snapshot.take().and_then(|snapshot| snapshot.open);
        let links_before: Vec<(AccountKey, Link)> =
            self.snapshot.iter().flat_map(|s| s.accounts.iter().map(|a| (a.key.clone(), a.link))).collect();
        // By account: a network counts as connected while one of its workspaces is down.
        self.trouble.retain(|key, _| accounts.iter().any(|account| &account.key == key && account.link == Link::Offline));
        for account in accounts.iter().filter(|account| account.link == Link::Offline) {
            if let std::collections::hash_map::Entry::Vacant(first) = self.trouble.entry(account.key.clone()) {
                first.insert(Instant::now());
                // One wake at the threshold; nothing counts down before it.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(TROUBLE_SHOWN).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
        }
        let links_changed = links_before != accounts.iter().map(|a| (a.key.clone(), a.link)).collect::<Vec<_>>();
        self.snapshot = Some(Snapshot { networks, accounts, call, open });
        self.down = None;
        if links_changed {
            self.composer_links_changed(cx);
        }
        // A network that signed out takes its place in the sidebar with it.
        if let Some(network) = self.sidebar_state.mode.network() {
            if self.snapshot.as_ref().is_some_and(|s| s.link(network) == Link::SignedOut) {
                self.sidebar_state.mode = Mode::Inbox;
            }
        }
        cx.notify();
    }

    pub(super) fn load_status(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || ask(&Request::Status), |this, answer, cx| {
            if let Ok(Response::Status { networks, accounts, call, .. }) = answer {
                this.apply_status(networks, accounts, call, cx);
            }
        });
    }

    /// Reads again everything that is on screen: at start, and when chatd no longer knows what
    /// changed.
    pub(super) fn reload_all(&mut self, cx: &mut Context<Self>) {
        let dirty = Dirty {
            status: false,
            spaces: true,
            inbox: true,
            trees: self.trees.keys().cloned().collect(),
            rows: self.open.iter().cloned().collect(),
        };
        self.reload(dirty, cx);
        self.load_directs(cx);
    }

    /// What chatd reported since the last `Wait`. Stored things are read again, each by its own
    /// scope; the call is applied in place; then every view is offered each change and takes
    /// the ones it draws.
    fn changed(&mut self, changes: &[Change], cx: &mut Context<Self>) {
        let mut dirty = Dirty::default();
        for change in changes {
            dirty.note(change);
            match change {
                Change::Media { account, file } => self.media_landed(account, file, cx),
                Change::Call { call } => {
                    if let Some(snapshot) = &mut self.snapshot {
                        snapshot.call = call.clone();
                        cx.notify();
                    }
                }
                Change::Occupants { conversation, occupants } => self.occupants_changed(conversation, occupants, cx),
                _ => {}
            }
            self.composer_changed(change, cx);
            self.conversation_changed(change, cx);
            self.people_changed(change, cx);
            self.call_changed(change, cx);
        }
        self.reload(dirty, cx);
    }

    fn reload(&mut self, dirty: Dirty, cx: &mut Context<Self>) {
        if dirty.status {
            self.load_status(cx);
        }
        if dirty.spaces {
            self.load_spaces(cx);
        }
        if dirty.inbox {
            self.inbox_arrived(&dirty.rows, cx);
        }
        for space in dirty.trees {
            if self.trees.contains_key(&space) {
                self.load_tree(space, cx);
            }
        }
        let mut directs = false;
        for key in dirty.rows {
            let in_tree = self.trees.values().any(|tree| tree.conversations.iter().any(|c| c.key == key));
            directs |= !in_tree && matches!(self.sidebar_state.mode, Mode::Network(network) if network == key.network);
            if in_tree || self.open.as_ref() == Some(&key) {
                self.load_row(key, cx);
            }
        }
        if directs {
            self.load_directs(cx);
        }
    }

    /// Who is in a voice channel changed: the row carries it, so the row is changed where it is.
    fn occupants_changed(&mut self, channel: &Key, occupants: &Occupants, cx: &mut Context<Self>) {
        let inside = (occupants.total > 0).then(|| occupants.clone());
        let mut changed = false;
        for tree in self.trees.values_mut() {
            if let Some(row) = tree.conversations.iter_mut().find(|c| &c.key == channel && c.occupants != inside) {
                Rc::make_mut(row).occupants = inside.clone();
                changed = true;
            }
        }
        if changed {
            self.rebuild_rows(cx);
        }
    }

    /// Rows of the Inbox changed. Its order is held while the user is aiming at it: then the
    /// rows change where they are, and the list is read again in its new order once let go.
    fn inbox_arrived(&mut self, rows: &[Key], cx: &mut Context<Self>) {
        match self.sidebar_state.hold.arrived(Instant::now()) {
            sidebar::hold::Arrival::Reorder => self.reorder_inbox(cx),
            sidebar::hold::Arrival::InPlace(later) => {
                let held: Vec<Key> =
                    rows.iter().filter(|key| self.sidebar_state.inbox.rows().any(|(_, row)| &row.key == *key)).cloned().collect();
                for key in held {
                    self.load_row(key, cx);
                }
                if let Some(later) = later {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(later).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.sidebar_state.hold.settle(Instant::now()) {
                                this.reorder_inbox(cx);
                            }
                        });
                    })
                    .detach();
                }
            }
        }
    }

    pub(super) fn reorder_inbox(&mut self, cx: &mut Context<Self>) {
        let pages = self.sidebar_state.inbox.stale();
        self.load_inbox(pages, cx);
    }

    /// Asks for pages of the Inbox as it is filtered now.
    pub(super) fn load_inbox(&mut self, pages: Vec<u32>, cx: &mut Context<Self>) {
        let filter = self.sidebar_state.filter;
        let epoch = self.sidebar_state.inbox.epoch();
        for page in pages {
            let request = Request::Inbox { network: None, filter, offset: page * PAGE, limit: PAGE };
            self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
                if let Ok(Response::Conversations { conversations, total, counts }) = answer {
                    let rows = conversations.into_iter().map(Rc::new).collect();
                    this.sidebar_state.counts = counts;
                    #[cfg(feature = "gui-shot")]
                    let first = this.sidebar_state.inbox.fresh;
                    if this.sidebar_state.inbox.fill(epoch, page, rows, total) {
                        #[cfg(feature = "gui-shot")]
                        if let (true, Some(launched)) = (first, super::LAUNCHED.get()) {
                            // The rows are in hand here and drawn on the next frame.
                            eprintln!("tinychat: first list after {} ms", launched.elapsed().as_millis());
                        }
                        cx.notify();
                    }
                }
            });
        }
    }

    pub(super) fn load_spaces(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || ask(&Request::Spaces { network: None }), |this, answer, cx| {
            if let Ok(Response::Spaces { spaces }) = answer {
                this.spaces = spaces;
                this.sidebar_state.spaces_loaded = true;
                this.show_mode(cx);
            }
        });
    }

    /// Reads one Space's tree whole. Asking is also what makes chatd bring it up to date.
    pub(super) fn load_tree(&mut self, space: Key, cx: &mut Context<Self>) {
        let tree = self.trees.entry(space.clone()).or_default();
        tree.epoch += 1;
        let epoch = tree.epoch;
        let wanted = space.clone();
        let read = move || -> io::Result<(Vec<Group>, Vec<Conversation>)> {
            let mut all = Vec::new();
            loop {
                let request = Request::Tree { space: wanted.clone(), offset: all.len() as u32, limit: TREE_PAGE, peek: false };
                match ask(&request)? {
                    Response::Tree { groups, conversations, total, .. } => {
                        let last = conversations.is_empty();
                        all.extend(conversations);
                        if last || all.len() as u32 >= total.min(TREE_LIMIT) {
                            return Ok((groups, all));
                        }
                    }
                    other => return Err(unexpected(other)),
                }
            }
        };
        self.off_thread(cx, read, move |this, answer, cx| {
            let (Ok((groups, conversations)), Some(tree)) = (answer, this.trees.get_mut(&space)) else {
                return;
            };
            if tree.epoch != epoch {
                return;
            }
            tree.groups = groups;
            tree.conversations = conversations.into_iter().map(Rc::new).collect();
            tree.loaded = true;
            this.rebuild_rows(cx);
        });
    }

    /// The direct conversations of the network whose home is on show: those that belong to no
    /// Space. A Space that keeps its own lists them in its tree.
    pub(super) fn load_directs(&mut self, cx: &mut Context<Self>) {
        let Mode::Network(network) = self.sidebar_state.mode else {
            return;
        };
        let request = Request::Inbox {
            network: Some(network),
            filter: InboxFilter::Directs,
            offset: 0,
            limit: PAGE_LIMIT,
        };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            if let (Ok(Response::Conversations { conversations, .. }), Mode::Network(now)) =
                (answer, &this.sidebar_state.mode)
            {
                if *now == network {
                    this.sidebar_state.directs = conversations
                        .into_iter()
                        .filter(|c| c.space.is_none())
                        .map(Rc::new)
                        .collect();
                    this.rebuild_rows(cx);
                }
            }
        });
    }

    /// One row changed: the open conversation's, or one in a tree, which never reorders.
    fn load_row(&mut self, key: Key, cx: &mut Context<Self>) {
        let request = Request::Conversation { conversation: key.clone() };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            let Ok(Response::Conversation { conversation }) = answer else {
                return;
            };
            if this.open.as_ref() == Some(&key) {
                if let Some(snapshot) = &mut this.snapshot {
                    snapshot.open = Some(conversation.clone());
                }
            }
            let fresh = Rc::new(conversation.clone());
            this.sidebar_state.inbox.replace(|row| row.key == key, &fresh);
            if let Some(row) = this.sidebar_state.directs.iter_mut().find(|c| c.key == key) {
                *row = fresh;
            }
            let mut in_tree = false;
            for tree in this.trees.values_mut() {
                if let Some(row) = tree.conversations.iter_mut().find(|c| c.key == key) {
                    *row = Rc::new(conversation.clone());
                    in_tree = true;
                }
            }
            if in_tree {
                this.rebuild_rows(cx);
            }
            cx.notify();
        });
    }

    /// Reads one conversation's row again wherever it is drawn: after something local changed
    /// it (a discarded send, a draft) that chatd announces to nobody.
    pub(super) fn refresh_row(&mut self, key: Key, cx: &mut Context<Self>) {
        let pages = self.sidebar_state.inbox.stale();
        self.load_inbox(pages, cx);
        self.load_directs(cx);
        self.load_row(key, cx);
    }

    /// Our own picture on an account, for our pending rows and the rail.
    pub(super) fn own_avatar(&self, account: &AccountKey) -> Option<PathBuf> {
        let status = self.snapshot.as_ref()?.accounts.iter().find(|status| &status.key == account)?;
        let key = Key::new(account, status.me.clone());
        self.media_path(&key, status.avatar.as_ref())
    }

    /// Opens a conversation in the conversation region. The sidebar does not follow.
    pub(super) fn select(&mut self, key: Key, title: &str, cx: &mut Context<Self>) {
        if self.open.as_ref() == Some(&key) {
            return;
        }
        self.remember();
        self.open_now(key, title, cx);
    }

    /// Opens without adding to history: what back and forward do.
    pub(super) fn open_now(&mut self, key: Key, title: &str, cx: &mut Context<Self>) {
        let row = self.sidebar_state.find(&self.trees, &key);
        let known = row.is_some();
        if let Some(snapshot) = &mut self.snapshot {
            // Until its row is read, a conversation no list holds is its key and title.
            snapshot.open = row.or_else(|| Some(Self::bare_row(&key, title)));
        }
        if !known {
            self.load_row(key.clone(), cx);
        }
        self.open = Some(key);
        self.refused = None;
        // A floating sidebar closes on pick.
        self.layout_state.floating = false;
        self.rebuild_rows(cx);
    }

    /// What stands for a conversation's row before chatd has answered with it.
    fn bare_row(key: &Key, title: &str) -> Conversation {
        Conversation::new(key.clone(), protocol::ConversationKind::Channel, title)
    }

    /// `shift-esc`: the open conversation is read up to its newest message.
    pub(super) fn mark_open_read(&mut self, cx: &mut Context<Self>) {
        // A screenshot or a driven test never reads anything on a network.
        if cfg!(feature = "gui-shot") {
            return;
        }
        let Some(conversation) = self.open.clone() else {
            return;
        };
        let read = move || -> io::Result<()> {
            let newest = Request::Messages { conversation: conversation.clone(), page: Page::Newest, limit: 1 };
            if let Response::Messages { messages, .. } = ask(&newest)? {
                if let Some(last) = messages.last() {
                    ask(&Request::MarkRead { conversation, message_id: last.id.clone() })?;
                }
            }
            Ok(())
        };
        self.off_thread(cx, read, |_, _, _| {});
    }

    // Media.

    /// Where a file is on disk: as its row said, or as it landed since.
    pub(super) fn media_path(&self, account: &Key, file: Option<&MediaFile>) -> Option<PathBuf> {
        let file = file?;
        match &file.path {
            Some(path) => Some(PathBuf::from(path)),
            None => self.media.get(&(account.account(), file.id.clone())).cloned(),
        }
    }

    /// The file is not on disk and has not been asked for.
    pub(super) fn media_missing(&self, account: &Key, file: &MediaFile) -> bool {
        let named = (account.account(), file.id.clone());
        file.path.is_none() && !self.media.contains_key(&named) && !self.media_asked.contains(&named)
    }

    /// Asks for the pictures of rows that are on screen and not on disk. Asked once each.
    pub(super) fn want_media(&mut self, files: Vec<(Key, String)>, cx: &mut Context<Self>) {
        let mut wanted: HashMap<AccountKey, Vec<String>> = HashMap::new();
        for (key, id) in files {
            if self.media_asked.insert((key.account(), id.clone())) {
                wanted.entry(key.account()).or_default().push(id);
            }
        }
        for (account, ids) in wanted {
            let request = Request::Fetch { retry: false, size: None, account: account.clone(), ids };
            self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
                if let Ok(Response::Media { files }) = answer {
                    for file in &files {
                        this.media_landed(&account, file, cx);
                    }
                }
            });
        }
    }

    fn media_landed(&mut self, account: &AccountKey, file: &MediaFile, cx: &mut Context<Self>) {
        let Some(path) = &file.path else {
            return;
        };
        if self.media.len() >= MEDIA_KEPT {
            self.media.clear();
            self.media_asked.clear();
        }
        self.media.insert((account.clone(), file.id.clone()), PathBuf::from(path));
        cx.notify();
    }

    // Sessions.

    /// Opens the network's sign-in window. chatd answers when the user has finished with it.
    pub(super) fn sign_in(&mut self, network: Network, cx: &mut Context<Self>) {
        if super::signin::open(network, cx) { return; }
        if self.signing_in.is_some() {
            return;
        }
        self.signing_in = Some(network);
        self.sign_in_failed = None;
        cx.notify();
        let request = Request::SignIn { at: self.window_at.get(), network };
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            this.signing_in = None;
            this.sign_in_failed = match answer {
                Ok(Response::Error { code, detail }) => sign_in_words(network, code, &detail).map(|words| (network, words)),
                Ok(_) => None,
                Err(err) => Some((network, err.to_string())),
            };
            if this.sign_in_failed.is_none() {
                this.layout_state.menu = None;
            }
            this.load_status(cx);
            this.reload_all(cx);
            cx.notify();
        });
    }

    /// Forgets one account's session and everything stored for it.
    pub(super) fn sign_out(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.account() == account) {
            self.open = None;
            if let Some(snapshot) = &mut self.snapshot {
                snapshot.open = None;
            }
        }
        let request = Request::SignOut { account };
        self.off_thread(cx, move || ask(&request), |this, _, cx| {
            this.load_status(cx);
            this.reload_all(cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{MediaKind, Presence};

    fn key(id: &str) -> Key {
        Key { network: Network::Discord, account: "me".into(), id: id.into() }
    }

    #[test]
    fn each_change_names_only_what_it_touched() {
        let account = key("").account();
        let mut dirty = Dirty::default();
        for change in [
            Change::Conversation { conversation: key("a") },
            Change::Conversation { conversation: key("a") },
            Change::Tree { space: key("server") },
        ] {
            dirty.note(&change);
        }
        // A row changing moves the Inbox and its Space's badge; it is read once however often
        // it was reported. The links were not touched, so `Status` is not asked.
        let expected = Dirty {
            status: false,
            spaces: true,
            inbox: true,
            trees: HashSet::from([key("server")]),
            rows: vec![key("a")],
        };
        assert_eq!(dirty, expected);

        assert_eq!(sign_in_words(Network::Slack, ErrorCode::SignIn, "The sign-in window was closed."), None);
        assert_eq!(
            sign_in_words(Network::Discord, ErrorCode::SignInUnreachable, "x").as_deref(),
            Some("Couldn't reach Discord. Check your connection and try again.")
        );
        assert_eq!(
            sign_in_words(Network::Slack, ErrorCode::SignIn, "The sign-in window stopped before it finished.").as_deref(),
            Some("The sign-in window closed unexpectedly. Try again.")
        );
        assert!(sign_in_words(Network::Slack, ErrorCode::SignInBlocked, "x").unwrap().starts_with("Your organization doesn't allow this app."));

        let mut links = Dirty::default();
        links.note(&Change::Status);
        assert_eq!(links, Dirty { status: true, ..Dirty::default() });
        let mut spaces = Dirty::default();
        spaces.note(&Change::Spaces { account: account.clone() });
        assert_eq!(spaces, Dirty { spaces: true, ..Dirty::default() });

        // Typing, presence, the call, another view's messages and people, a file landing: no
        // list is read again for any of them.
        let mut quiet = Dirty::default();
        for change in [
            Change::Typing { conversation: key("a"), typists: Vec::new() },
            Change::Presence {
                person: key("p"),
                presence: Presence::Idle,
                status: String::new(),
                emoji: String::new(),
                activity: None,
            },
            Change::Call { call: None },
            Change::Messages { conversation: key("a") },
            Change::People { account: account.clone() },
            Change::Media { account, file: MediaFile::new("f", MediaKind::Image) },
        ] {
            dirty.note(&change);
            quiet.note(&change);
        }
        assert_eq!(quiet, Dirty::default());
        assert_eq!(dirty, expected);
    }
}
