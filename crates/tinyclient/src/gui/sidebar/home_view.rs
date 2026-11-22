//! Reading and drawing the network homes and the lists they drill into.

use super::chat::Look;
use super::home::{self, Act, Drill, Item};
use super::{joins_from_list, on_screen, respliced, Mode};
use crate::client::ask;
use crate::gui::components::{badge_dot, empty_state, glyph, icon_button, list_row, rollup_badge, section_header, Avatar, Shape};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme::{self, Type};
use crate::view::now;
use gpui::{div, list, prelude::*, px, AnyElement, Context, Div, MouseButton, Stateful, Window};
use protocol::{Conversation, ConversationKind, Folder, Key, Network, Request, Response, Shelf, PAGE_LIMIT};
use std::rc::Rc;

/// The most rows a home holds; chatd's order puts the ones that matter first.
const HOME_HELD: u32 = 500;

/// What one read of a home brought back.
#[derive(Default)]
struct Read {
    chats: Vec<Conversation>,
    requests: u32,
    in_voice: Vec<Conversation>,
    folders: Vec<Folder>,
    around: Option<(usize, u32)>,
    archive: Vec<Conversation>,
    archive_total: u32,
    drilled: Vec<Conversation>,
    unreads: Vec<protocol::UnreadGroup>,
    calls: Vec<protocol::CallLogEntry>,
    calls_next: Option<String>,
    calls_more: bool,
}

fn conversations(request: &Request) -> std::io::Result<(Vec<Conversation>, u32)> {
    match ask(request)? {
        Response::Conversations { conversations, total, .. } => Ok((conversations, total)),
        // A signed-out refusal must not look like an empty forum or thread list.
        Response::Error { detail, .. } => Err(std::io::Error::new(std::io::ErrorKind::Other, detail)),
        _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, crate::client::OTHER_ANSWER)),
    }
}

/// A whole home list, page by page, up to [`HOME_HELD`].
fn whole(network: Network, folder: Option<String>, shelf: Shelf) -> std::io::Result<(Vec<Conversation>, u32)> {
    let mut all = Vec::new();
    loop {
        let request = Request::Home { network, account: None, folder: folder.clone(), shelf, offset: all.len() as u32, limit: PAGE_LIMIT };
        let (page, total) = conversations(&request)?;
        let last = page.is_empty();
        all.extend(page);
        if last || all.len() as u32 >= total.min(HOME_HELD) {
            return Ok((all, total));
        }
    }
}

impl Shell {
    /// Reads what the home on show draws. Asking is also what makes chatd bring it up to date.
    pub(in crate::gui) fn load_home(&mut self, cx: &mut Context<Self>) {
        let network = match &self.sidebar_state.mode {
            Mode::Network(network) => *network,
            Mode::Space(space) if space.network == Network::Discord => Network::Discord,
            _ => return,
        };
        // Keys only: the read runs off this thread.
        enum DrillLoad {
            Unreads(Option<Key>),
            Threads(Key),
            Drafts,
            Archive,
            Calls,
            Requests,
            Topics(Key),
        }
        let drill = self.sidebar_state.drill.as_ref().map(|drill| match drill {
            Drill::Unreads(space) => DrillLoad::Unreads(space.clone()),
            Drill::Threads(space) => DrillLoad::Threads(space.clone()),
            Drill::Drafts(_) => DrillLoad::Drafts,
            Drill::Archive => DrillLoad::Archive,
            Drill::Calls => DrillLoad::Calls,
            Drill::Requests => DrillLoad::Requests,
            Drill::Topics(forum) => DrillLoad::Topics(forum.key.clone()),
        });
        let tg_account = self
            .snapshot
            .as_ref()
            .and_then(|s| s.accounts.iter().find(|a| a.key.network == Network::Telegram && a.link == protocol::Link::Connected))
            .map(|a| a.key.clone());
        let calls_missed = self.sidebar_state.home.calls_missed;
        let folder = self.sidebar_state.home.folder.clone();
        self.sidebar_state.home.epoch += 1;
        let epoch = self.sidebar_state.home.epoch;
        let read = move || -> std::io::Result<Read> {
            let mut read = Read::default();
            match drill {
                Some(DrillLoad::Unreads(space)) => {
                    let net = space.as_ref().map(|s| s.network).or(Some(network));
                    if let Response::AllUnreads { groups } = ask(&Request::AllUnreads { network: net, space, limit: 15 })? {
                        read.unreads = groups;
                    }
                }
                Some(DrillLoad::Calls) => {
                    if let Some(account) = tg_account {
                        if let Response::CallHistory { entries, next, more } =
                            ask(&Request::CallHistory { account, missed: calls_missed, before: None, limit: 30 })?
                        {
                            read.calls = entries;
                            read.calls_next = next;
                            read.calls_more = more;
                        }
                    }
                }
                Some(DrillLoad::Threads(space)) => read.drilled = conversations(&Request::Threads { space, offset: 0, limit: PAGE_LIMIT })?.0,
                Some(DrillLoad::Topics(forum)) => read.drilled = conversations(&Request::Topics { forum, offset: 0, limit: PAGE_LIMIT, landing: false })?.0,
                Some(DrillLoad::Archive) => read.drilled = whole(network, None, Shelf::Archive)?.0,
                Some(DrillLoad::Requests) => read.drilled = whole(network, None, Shelf::Requests)?.0,
                Some(DrillLoad::Drafts) => read.drilled = whole(network, None, Shelf::Main)?.0,
                None => {}
            }
            match network {
                Network::Discord => {
                    read.chats = whole(network, None, Shelf::Main)?.0;
                    let requests = Request::Home { network, account: None, folder: None, shelf: Shelf::Requests, offset: 0, limit: 1 };
                    read.requests = conversations(&requests)?.1;
                    read.in_voice = conversations(&Request::InVoice { network: Some(network) })?.0;
                    if let Response::Folders { folders } = ask(&Request::Folders { network })? {
                        read.folders = folders;
                    }
                    if let Response::Around { requests, online, in_call, .. } = ask(&Request::Around { network: Some(network) })? {
                        read.around = Some((online.len() + in_call.len(), requests));
                    }
                }
                Network::Telegram => {
                    read.chats = whole(network, folder.clone(), Shelf::Main)?.0;
                    let archive = Request::Home { network, account: None, folder: None, shelf: Shelf::Archive, offset: 0, limit: 3 };
                    (read.archive, read.archive_total) = conversations(&archive)?;
                    if let Response::Folders { folders } = ask(&Request::Folders { network })? {
                        read.folders = folders;
                    }
                }
                Network::Teams | Network::Signal | Network::Messages => read.chats = whole(network, None, Shelf::Main)?.0,
                Network::Slack | Network::Tiny => {}
            }
            Ok(read)
        };
        self.off_thread(cx, read, move |this, answer, cx| {
            let home = &mut this.sidebar_state.home;
            let (Ok(read), true) = (answer, home.epoch == epoch && home.network == Some(network)) else {
                return;
            };
            let rows = |rows: Vec<Conversation>| rows.into_iter().filter(|c| !c.hidden).map(Rc::new).collect::<Vec<_>>();
            home.chats = rows(read.chats);
            home.requests = read.requests;
            home.in_voice = rows(read.in_voice);
            home.folders = read.folders;
            if let Some((online, requests)) = read.around {
                (home.online, home.friend_requests) = (online, requests);
            }
            home.archive = rows(read.archive);
            home.archive_total = read.archive_total;
            home.drilled = rows(read.drilled);
            if matches!(this.sidebar_state.drill, Some(Drill::Unreads(_))) {
                home.unreads = read.unreads.into_iter().map(Rc::new).collect();
            }
            if matches!(this.sidebar_state.drill, Some(Drill::Calls)) {
                home.calls = read.calls;
                home.calls_next = read.calls_next;
                home.calls_more = read.calls_more;
            }
            home.loaded = true;
            home.loaded_once = true;
            this.rebuild_home(cx);
        });
    }

    /// Counts the friend requests waiting, for People's dot on the rail.
    pub(in crate::gui) fn load_requests(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || ask(&Request::Around { network: None }), |this, answer, cx| {
            if let Ok(Response::Around { requests, .. }) = answer {
                if this.sidebar_state.friend_requests != requests {
                    this.sidebar_state.friend_requests = requests;
                    cx.notify();
                }
            }
        });
    }

    /// Goes into a list of the home: Threads, Archive, Message requests, a forum's topics.
    pub(super) fn drill(&mut self, drill: Drill, cx: &mut Context<Self>) {
        let state = &mut self.sidebar_state;
        state.enter_drill(drill);
        state.cursor = None;
        state.home.drilled.clear();
        state.home.unreads.clear();
        state.home.loaded = false;
        self.rebuild_rows(cx);
        self.load_home(cx);
    }

    /// The Slack workspace's All Unreads row: unread channels and DMs in this workspace (#606).
    pub(super) fn unreads_row(&self, space: &Key) -> Option<super::tree::Row> {
        let tree = self.trees.get(space)?;
        let unreads: Vec<&Rc<Conversation>> = tree
            .conversations
            .iter()
            .filter(|c| c.kind != ConversationKind::Thread && !c.hidden && !c.archived && c.unread > 0 && (!c.muted || c.mentions > 0))
            .collect();
        Some(super::tree::Row::Unreads {
            space: space.clone(),
            count: unreads.len() as u32,
            mention: unreads.iter().any(|c| c.mentions > 0),
        })
    }

    /// The Slack workspace's Threads row: followed threads with new replies, from its tree.
    pub(super) fn threads_row(&self, space: &Key) -> Option<super::tree::Row> {
        let tree = self.trees.get(space)?;
        let news: Vec<&Rc<Conversation>> = tree.conversations.iter().filter(|c| c.kind == ConversationKind::Thread && c.followed && c.unread > 0).collect();
        Some(super::tree::Row::Threads { space: space.clone(), count: news.len() as u32, mention: news.iter().any(|c| c.mentions > 0) })
    }

    /// The Slack workspace's Drafts row: shown when any conversation in the workspace has a draft.
    pub(super) fn drafts_row(&self, space: &Key) -> Option<super::tree::Row> {
        let tree = self.trees.get(space)?;
        let count = tree.conversations.iter().filter(|c| !c.draft.trim().is_empty()).count() as u32;
        (count > 0).then(|| super::tree::Row::Drafts { space: space.clone(), count })
    }

    /// The home's items again, from what is held.
    pub(super) fn rebuild_home(&mut self, cx: &mut Context<Self>) {
        let state = &self.sidebar_state;
        let items: Vec<Item> = match (&state.mode, &state.drill) {
            (Mode::Network(_), Some(Drill::Unreads(_))) => {
                let mut out = Vec::new();
                if !state.home.unreads.is_empty() {
                    out.push(Item::Action {
                        act: Act::MarkAllUnreadsRead,
                        icon: "icons/check-check.svg",
                        words: "Mark all as read".into(),
                        after: format!("{} unread", state.home.unreads.len()),
                        dot: None,
                    });
                }
                for group in &state.home.unreads {
                    out.push(Item::UnreadGroupHeader(group.clone()));
                    for msg in &group.messages {
                        out.push(Item::UnreadMessage {
                            conversation: group.conversation.key.clone(),
                            title: group.conversation.title.clone(),
                            message: Rc::new(msg.clone()),
                        });
                    }
                }
                out
            }
            (Mode::Network(_), Some(Drill::Requests)) => {
                let mut out: Vec<Item> = state.home.drilled.iter().filter(|c| !c.spam).cloned().map(Item::Chat).collect();
                let spam: Vec<Item> = state.home.drilled.iter().filter(|c| c.spam).cloned().map(Item::Chat).collect();
                if !spam.is_empty() {
                    out.push(Item::Header { label: "Spam", first: out.is_empty() });
                    out.extend(spam);
                }
                out
            }
            (Mode::Network(_), Some(Drill::Calls)) => {
                let mut out = Vec::new();
                let filter_words = if state.home.calls_missed { "Showing missed · show all" } else { "Showing all · filter missed" };
                out.push(Item::Action {
                    act: Act::CallsMissedToggle,
                    icon: "icons/phone-missed.svg",
                    words: filter_words.into(),
                    after: String::new(),
                    dot: None,
                });
                out.extend(home::collapse_call_entries(&state.home.calls).into_iter().map(Item::Call));
                if state.home.calls_more {
                    out.push(Item::Action {
                        act: Act::CallsMore,
                        icon: "icons/chevron-down.svg",
                        words: "Load more calls".into(),
                        after: String::new(),
                        dot: None,
                    });
                }
                out
            }
            (Mode::Network(_), Some(Drill::Drafts(space))) => self
                .trees
                .get(space)
                .map(|tree| {
                    tree.conversations
                        .iter()
                        .filter(|c| !c.draft.trim().is_empty())
                        .cloned()
                        .map(Item::Chat)
                        .collect()
                })
                .unwrap_or_default(),
            (Mode::Network(_), Some(_)) => state.home.drilled.iter().cloned().map(Item::Chat).collect(),
            (Mode::Network(Network::Discord), None) | (Mode::Space(Key { network: Network::Discord, .. }), None) => {
                let spaces: Vec<protocol::Space> = self.spaces.iter().filter(|s| s.key.network == Network::Discord).cloned().collect();
                let open_space = match &state.mode {
                    Mode::Space(space) => Some(space.id.clone()),
                    _ => self.snapshot.as_ref().and_then(|s| s.open.as_ref()).and_then(|open| open.space.clone()),
                };
                let items = home::discord(&state.home, &spaces, open_space.as_deref(), state.home_unread);
                // The servers' indexes are into the shell's whole list of Spaces.
                items
                    .into_iter()
                    .map(|item| match item {
                        Item::Space { index, child, open } => {
                            let at = self.spaces.iter().position(|s| s.key == spaces[index].key).unwrap_or(index);
                            Item::Space { index: at, child, open }
                        }
                        other => other,
                    })
                    .collect()
            }
            (Mode::Network(Network::Telegram), None) => home::telegram(&state.home),
            (Mode::Network(Network::Signal | Network::Messages), None) => home::signal(&state.home),
            (Mode::Network(Network::Teams), None) => {
                let channels = |team: &Key| -> Vec<Rc<Conversation>> {
                    self.trees.get(team).map(|tree| tree.conversations.iter().filter(|c| c.kind != ConversationKind::Thread).cloned().collect()).unwrap_or_default()
                };
                home::teams(&state.home, &self.spaces, &channels, self.open.as_ref(), now())
            }
            _ => Vec::new(),
        };
        let news = if self.sidebar_state.mode.network() == Some(Network::Discord) && self.sidebar_state.drill.is_none() {
            let state = &self.sidebar_state;
            let spaces: Vec<protocol::Space> = self.spaces.iter().filter(|s| s.key.network == Network::Discord).cloned().collect();
            home::discord(&state.home, &spaces, None, true).iter().filter(|item| matches!(item, Item::Chat(_) | Item::Space { .. } | Item::Folder { .. })).count() as u32
        } else {
            0
        };
        let state = &mut self.sidebar_state;
        state.home_news = news;
        if *state.home_items != items {
            respliced(&state.home_list, &state.home_items, &items, Item::anchor, |item| item.height());
            state.home_items = Rc::new(items);
            cx.notify();
        }
    }

    pub(super) fn activate_home(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.sidebar_state.home_items.get(ix).cloned() else {
            return;
        };
        let home = &mut self.sidebar_state.home;
        match item {
            Item::Chat(c) | Item::Voice(c) | Item::Channel(c) => return self.pick_row(&c, cx),
            Item::UnreadGroupHeader(group) => return self.pick_row(&group.conversation, cx),
            Item::UnreadMessage { conversation, title, message } => {
                let msg_id = message.id.clone();
                let pane = self.pane(cx);
                if self.open.as_ref() == Some(&conversation) {
                    pane.update(cx, |pane, cx| pane.jump_to(&msg_id, cx));
                } else {
                    pane.update(cx, |pane, _| pane.land_next(msg_id));
                    self.select(conversation, &title, cx);
                }
                return;
            }
            Item::Call(group) => {
                let key = group.latest.conversation.clone();
                let msg_id = group.latest.message_id.clone();
                let title = group.latest.name.clone();
                let pane = self.pane(cx);
                if self.open.as_ref() == Some(&key) {
                    pane.update(cx, |pane, cx| pane.jump_to(&msg_id, cx));
                } else {
                    pane.update(cx, |pane, _| pane.land_next(msg_id));
                    self.select(key, &title, cx);
                }
                return;
            }
            Item::Header { .. } => return,
            Item::Action { act, .. } => match act {
                Act::Friends => {
                    self.go(Mode::People, cx);
                    self.people_scope(Some(Network::Discord), cx);
                    return;
                }
                Act::Requests => {
                    if matches!(self.sidebar_state.mode, Mode::Space(_)) {
                        self.go(Mode::Network(Network::Discord), cx);
                    }
                    return self.drill(Drill::Requests, cx);
                },
                Act::Archive => return self.drill(Drill::Archive, cx),
                Act::Calls => return self.drill(Drill::Calls, cx),
                Act::CallsMissedToggle => {
                    home.calls_missed = !home.calls_missed;
                    home.calls.clear();
                    home.calls_next = None;
                    home.calls_more = false;
                    home.loaded = false;
                    self.rebuild_home(cx);
                    self.load_home(cx);
                    return;
                }
                Act::CallsMore => {
                    let before = home.calls_next.clone();
                    let missed = home.calls_missed;
                    let account = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.accounts.iter().find(|a| a.key.network == Network::Telegram && a.link == protocol::Link::Connected))
                        .map(|a| a.key.clone());
                    if let Some(account) = account {
                        let epoch = home.epoch;
                        self.off_thread(
                            cx,
                            move || ask(&Request::CallHistory { account, missed, before, limit: 30 }),
                            move |this, answer, cx| {
                                let home = &mut this.sidebar_state.home;
                                if home.epoch != epoch {
                                    return;
                                }
                                if let Ok(Response::CallHistory { entries, next, more }) = answer {
                                    home.calls.extend(entries);
                                    home.calls_next = next;
                                    home.calls_more = more;
                                    this.rebuild_home(cx);
                                }
                            },
                        );
                    }
                    return;
                }
                Act::MoreDms => home.dms_all = true,
                Act::MoreVoice => home.voice_all = true,
                Act::MoreChats => home.chats_all = true,
                Act::Meetings => home.meetings_open = !home.meetings_open,
                Act::MarkAllUnreadsRead => {
                    let space = match &self.sidebar_state.drill {
                        Some(Drill::Unreads(space)) => space.clone(),
                        _ => None,
                    };
                    let network = space.as_ref().map(|s| s.network).or(self.sidebar_state.home.network);
                    self.sidebar_state.home.unreads.clear();
                    self.rebuild_home(cx);
                    self.off_thread(cx, move || ask(&Request::MarkAllRead { network, space, folder: None }), |_, _, _| {});
                    return;
                }
                Act::Hidden(id) => {
                    if !home.hidden_shown.remove(&id) {
                        home.hidden_shown.insert(id);
                    }
                }
            },
            Item::Folder { id, .. } => {
                if !home.opened.remove(&id) {
                    home.opened.insert(id);
                }
            }
            Item::Space { index, open: Some(_), .. } => {
                // A team opens in place; its channels are read as it opens.
                if let Some(team) = self.spaces.get(index) {
                    let id = team.key.id.clone();
                    if !home.opened.remove(&id) {
                        home.opened.insert(id);
                    }
                }
                self.show_mode(cx);
                return;
            }
            Item::Space { index, .. } => {
                if let Some(space) = self.spaces.get(index) {
                    self.go(Mode::Space(space.key.clone()), cx);
                }
                return;
            }
        }
        self.rebuild_home(cx);
    }

    fn home_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.sidebar_state.home_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let focused = self.sidebar_state.listed() && self.sidebar_state.cursor == Some(ix) && (self.sidebar_state.focus.is_focused(window) || self.focus.is_focused(window));
        let network = self.sidebar_state.home.network;
        let click = |row: Stateful<Div>, cx: &mut Context<Self>| self.tap(row, crate::gui::phone::token(&["home-item", &ix.to_string()]), None, move |this, cx| this.activate_home(ix, cx), cx);
        match item {
            Item::Header { label, first } => section_header(label, first).into_any_element(),
            Item::Chat(c) => {
                let look = match network {
                    Some(Network::Discord) if self.sidebar_state.drill.is_none() => Look { status: true, ..Look::default() },
                    // The Telegram home says the network's own pins on its rows; a list drilled
                    // into (the archive, a forum's topics) is drawn plain.
                    Some(Network::Telegram) if self.sidebar_state.drill.is_none() => Look { time: true, context: false, pin: true, ..Look::default() },
                    _ => Look { time: true, context: false, ..Look::default() },
                };
                self.chat_row("home", &c, look, focused, cx).into_any_element()
            }
            Item::Voice(c) => self.voice_row(&c, focused, cx).into_any_element(),
            Item::Call(group) => {
                let entry = &group.latest;
                let missed = entry.note.outcome == protocol::CallOutcome::Missed && !entry.outgoing;
                let icon_path = if missed {
                    "icons/phone-missed.svg"
                } else if entry.note.video {
                    "icons/video.svg"
                } else if entry.outgoing {
                    "icons/phone-outgoing.svg"
                } else {
                    "icons/phone-incoming.svg"
                };
                let title = if group.count > 1 { format!("{} ({})", entry.name, group.count) } else { entry.name.clone() };
                let subtitle = match (entry.note.outcome, entry.outgoing, entry.note.seconds) {
                    (protocol::CallOutcome::Missed, false, _) => "Missed".to_owned(),
                    (protocol::CallOutcome::Declined, _, _) => "Declined".to_owned(),
                    (protocol::CallOutcome::Cancelled, _, _) => "Cancelled".to_owned(),
                    (protocol::CallOutcome::Busy, _, _) => "Line busy".to_owned(),
                    (_, true, Some(s)) if s > 0 => format!("Outgoing · {}s", s),
                    (_, false, Some(s)) if s > 0 => format!("Incoming · {}s", s),
                    (_, true, _) => "Outgoing".to_owned(),
                    (_, false, _) => "Incoming".to_owned(),
                };
                let conv_key = entry.conversation.clone();
                let call_support = self
                    .sidebar_state
                    .home
                    .chats
                    .iter()
                    .find(|c| c.key == conv_key)
                    .map_or(protocol::CallSupport::Handoff, |c| c.call);
                let call_btn = (!matches!(call_support, protocol::CallSupport::None | protocol::CallSupport::Restricted)).then(|| {
                    let key = conv_key.clone();
                    let handoff = call_support == protocol::CallSupport::Handoff;
                    icon_button(("call-log-dial", ix), "icons/phone.svg", false).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if handoff {
                                this.call_handoff(key.clone(), cx);
                            } else {
                                this.call_start(key.clone(), cx);
                            }
                        }),
                    )
                });
                let del_ids = group.message_ids.clone();
                let del_account = conv_key.account();
                let del_btn = icon_button(("call-log-del", ix), "icons/trash-2.svg", false)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let ids = del_ids.clone();
                        let account = del_account.clone();
                        this.sidebar_state.home.calls.retain(|c| !ids.contains(&c.message_id));
                        this.rebuild_home(cx);
                        this.off_thread(cx, move || ask(&Request::DeleteCallEntries { account, message_ids: ids, revoke: false }), |_, _, _| {});
                    }));
                let fg = if missed { theme::danger() } else { theme::text() };
                let content = div()
                    .gap(px(8.))
                    .child(glyph(icon_path, 16., if missed { theme::danger() } else { theme::text_soft() }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().type_ui().text_color(fg).truncate().child(title))
                            .child(div().type_small().text_color(theme::text_faint()).truncate().child(subtitle)),
                    )
                    .children(call_btn)
                    .child(del_btn);
                click(list_row(("home-call", ix), theme::row_height(48.), false, focused, content), cx).into_any_element()
            }
            Item::Action { act: Act::Meetings, icon, words, after, .. } => {
                // The subjects use the row that remains after the label. The chevron says the fold is open.
                let open = self.sidebar_state.home.meetings_open;
                let chevron = if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" };
                let content = div()
                    .gap(px(8.))
                    .child(div().size(px(20.)).flex_none().flex().items_center().justify_center().child(glyph(icon, 16., theme::text_soft())))
                    .child(div().flex_none().type_ui().text_color(theme::text_soft()).child(words))
                    .child(div().flex_1().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(after))
                    .child(glyph(chevron, 12., theme::text_faint()));
                click(list_row(("home-action", ix), theme::row_height(theme::ROW_ONE), false, focused, content), cx).into_any_element()
            }
            Item::Action { act, icon, words, after, dot } => {
                // Folds and drills keep their words quiet; a number or dot says what is inside.
                let lit = act == Act::Friends && self.sidebar_state.home.friend_requests > 0;
                let trailing = match (&act, dot) {
                    (Act::Friends, _) if self.sidebar_state.home.friend_requests > 0 => {
                        Some(div().flex().items_center().gap(px(8.)).child(faint_words(after.clone())).child(crate::gui::components::badge_count_unread(self.sidebar_state.home.friend_requests)))
                    }
                    (_, Some(mention)) => Some(div().flex().items_center().gap(px(8.)).child(faint_words(after.clone())).child(badge_dot(8., if mention { theme::mention() } else { theme::unread() }))),
                    _ if after.is_empty() => None,
                    _ => Some(faint_words(after.clone())),
                };
                // Message requests is a mail glyph and a count. Archive is the row that drills.
                let chevron = matches!(act, Act::Archive).then(|| glyph("icons/chevron-right.svg", 12., theme::text_faint()));
                let trailing = match (trailing, chevron) {
                    (Some(trailing), Some(chevron)) => Some(div().flex().items_center().gap(px(6.)).child(trailing).child(chevron)),
                    (None, chevron) => chevron.map(|chevron| div().child(chevron)),
                    (trailing, None) => trailing,
                };
                click(self.action_row(("home-action", ix), Some(icon), words.into(), lit, trailing, focused), cx).into_any_element()
            }
            Item::Space { index, child, open } => click(self.space_row(ix, index, child, open, focused), cx).into_any_element(),
            Item::Folder { id, name, color, open, lit, mention } => {
                let color = color.map_or(theme::text_faint(), gpui::rgb);
                let chevron = if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" };
                let folder_key = self
                    .sidebar_state
                    .home
                    .folders
                    .iter()
                    .find(|f| f.key.id == id && (f.unread > 0 || f.mentions > 0))
                    .map(|f| f.key.clone());
                let content = div()
                    .gap(px(8.))
                    .child(div().size(px(20.)).flex_none().flex().items_center().child(div().w(px(3.)).h(px(16.)).rounded(px(1.5)).bg(color)))
                    .child(div().min_w_0().truncate().type_ui().font_weight(if lit { theme::SEMIBOLD } else { theme::REGULAR }).text_color(if lit { theme::text() } else { theme::text_soft() }).child(name))
                    .child(glyph(chevron, 12., theme::text_faint()))
                    .child(div().flex_1())
                    .children(lit.then(|| badge_dot(8., if mention { theme::mention() } else { theme::unread() })));
                let mut row = click(list_row(("folder", ix), theme::row_height(theme::ROW_ONE), false, focused, content), cx);
                if let Some(folder_key) = folder_key {
                    row = row.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_menu(super::Menu::Folder(folder_key.clone(), event.position), cx);
                        }),
                    );
                }
                row.into_any_element()
            }
            Item::Channel(c) => self.place_row(&c, 1, focused, cx).into_any_element(),
            Item::UnreadGroupHeader(group) => {
                let conv = &group.conversation;
                let label = if conv.kind == ConversationKind::Channel && !conv.title.starts_with('#') {
                    format!("#{}", conv.title)
                } else {
                    conv.title.clone()
                };
                let count = group.total_unread;
                let has_mention = conv.mentions > 0;
                let mark_key = conv.key.clone();
                let mark_through = group.messages.last().map(|m| m.id.clone()).unwrap_or_default();
                let mark_btn = div()
                    .id(("unread-group-mark", ix))
                    .flex_none()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(theme::R_CONTROL))
                    .type_micro()
                    .text_color(theme::text_faint())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = mark_key.clone();
                        let message_id = mark_through.clone();
                        this.sidebar_state.home.unreads.retain(|g| g.conversation.key != key);
                        this.rebuild_home(cx);
                        this.off_thread(cx, move || ask(&Request::MarkRead { conversation: key, message_id }), |_, _, _| {});
                    }))
                    .child("Mark read");
                let badge = if has_mention {
                    crate::gui::components::badge_mention(conv.mentions)
                } else {
                    crate::gui::components::badge_count_unread(count)
                };
                let content = div()
                    .gap(px(6.))
                    .child(div().flex_1().min_w_0().truncate().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(label))
                    .child(badge)
                    .child(mark_btn);
                click(list_row(("unread-group", ix), theme::row_height(32.), false, focused, content), cx).into_any_element()
            }
            Item::UnreadMessage { message, .. } => {
                let sender = if message.sender.name.is_empty() { "Someone".to_owned() } else { message.sender.name.clone() };
                let snippet = if message.text.trim().is_empty() {
                    if !message.attachments.is_empty() { "[attachment]".to_owned() } else { "New message".to_owned() }
                } else {
                    message.text.replace('\n', " ")
                };
                let content = div()
                    .pl(px(8.))
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .child(div().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).truncate().child(sender))
                    .child(div().type_small().text_color(theme::text_soft()).truncate().child(snippet));
                click(list_row(("unread-msg", ix), theme::row_height(44.), false, focused, content), cx).into_any_element()
            }
        }
    }

    /// A server or a team, 28: its icon, its name, a dot for what is inside. A team opens in
    /// place with a chevron; a server opens its tree.
    fn space_row(&self, ix: usize, index: usize, child: Option<u32>, open: Option<bool>, focused: bool) -> Stateful<Div> {
        let Some(space) = self.spaces.get(index) else {
            return list_row(("space-gone", ix), theme::row_height(theme::ROW_ONE), false, false, div());
        };
        let lit = space.attention > 0 || space.unread;
        let here = if let Mode::Space(selected) = &self.sidebar_state.mode { selected == &space.key } else { self.open.as_ref().is_some_and(|open| self.snapshot.as_ref().and_then(|s| s.open.as_ref()).is_some_and(|row| row.key == *open && row.space.as_deref() == Some(space.key.id.as_str()))) };
        let icon = self.media_path(&space.key, space.icon.as_ref());
        let color = if space.muted { theme::text_faint() } else if lit { theme::text() } else { theme::text_soft() };
        // A team's chevron sits in the tree's 16 slot, so its name lines up with its channels'.
        let chevron = open.map(|open| {
            div().w(px(16.)).mr(px(-8.)).flex_none().child(glyph(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }, 12., theme::text_faint()))
        });
        let content = div()
            .gap(px(8.))
            .children(child.map(|_| div().w(px(12.)).flex_none()))
            .children(chevron)
            .child(Avatar::new(&space.key, &space.name, 20.).shape(Shape::Place).image_at(icon).build())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_ui()
                    .font_weight(if lit && !space.muted { theme::SEMIBOLD } else if open.is_some() { theme::MEDIUM } else { theme::REGULAR })
                    .text_color(color)
                    .child(space.name.clone()),
            )
            .children(rollup_badge(space.attention, space.unread && !space.muted).filter(|_| open != Some(true)));
        let spine = child.map(|color| {
            let color = if color == 0 { theme::text_faint() } else { theme::role_color(color) };
            div().absolute().left(px(16.)).top_0().bottom_0().w(px(3.)).bg(color).opacity(0.7)
        });
        list_row(super::row_id("space", &space.key), theme::row_height(theme::ROW_ONE), here, focused, content)
            .tooltip(crate::gui::components::tip(space.name.clone(), None))
            .relative()
            .children(spine)
    }

    /// The people in a voice row. Their photos come from the person keys, not the channel key.
    fn voice_faces(&self, c: &Conversation, surface: gpui::Rgba, cx: &mut Context<Self>) -> Div {
        let people = c.occupants.as_ref().map(|inside| inside.people.as_slice()).unwrap_or(&[]);
        let size = 32.;
        match people {
            [] => self.conversation_avatar(c, size, surface, cx),
            [one] => Avatar::new(&one.person, &one.name, size).image_at(self.person_picture(&one.person, cx)).build(),
            [back, front, ..] => {
                let small = (size * 0.66).round();
                let face = |person: &protocol::Occupant, cx: &mut Context<Self>| {
                    Avatar::new(&person.person, &person.name, small).image_at(self.person_picture(&person.person, cx)).build()
                };
                div()
                    .relative()
                    .size(px(size))
                    .flex_none()
                    .child(div().absolute().top_0().left_0().child(face(back, cx)))
                    .child(
                        div()
                            .absolute()
                            .right(px(-2.))
                            .bottom(px(-2.))
                            .rounded(px(small / 2. + 2.))
                            .border_2()
                            .border_color(surface)
                            .child(face(front, cx)),
                    )
            }
        }
    }

    /// Friends in a voice channel or a DM call: their faces, where, and who.
    fn voice_row(&self, c: &Rc<Conversation>, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.open.as_ref() == Some(&c.key);
        let surface = theme::row_surface(selected);
        let people: Vec<&str> = c.occupants.as_ref().map(|inside| inside.people.iter().map(|p| p.name.as_str()).collect()).unwrap_or_default();
        let total = c.occupants.as_ref().map_or(0, |inside| inside.total as usize);
        let faces = self.voice_faces(c, surface, cx);
        let firsts: Vec<&str> = people.iter().take(3).map(|name| name.split_whitespace().next().unwrap_or(name)).collect();
        let who = match total.saturating_sub(firsts.len()) {
            0 => firsts.join(", "),
            more => format!("{} +{more}", firsts.join(", ")),
        };
        let (title, glyph_name) = if c.kind.is_direct() { ("Call in your DMs".to_owned(), "icons/phone.svg") } else { (c.title.trim_start_matches('#').to_owned(), "icons/volume-2.svg") };
        let ours = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.call.as_ref())
            .map_or(false, |call| {
                call.conversation == c.key
                    && !matches!(call.state, protocol::CallState::Failed { .. })
            });
        let join = joins_from_list(c) && !ours;
        let content = div()
            .gap(px(10.))
            .child(faces)
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
                            .child(glyph(glyph_name, 14., theme::accent_text()))
                            .child(div().flex_none().max_w(px(150.)).truncate().type_ui().text_color(theme::text()).child(title))
                            .children((!c.space_title.is_empty()).then(|| div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text_faint()).child(c.space_title.clone()))),
                    )
                    .child(div().h(px(16.)).truncate().type_small().text_color(theme::text_soft()).child(who)),
            )
            .children(join.then(|| self.join_pill(c, cx)))
            .when(join, |row| row.group("voice"));
        let picked = c.clone();
        list_row(super::row_id("voice", &c.key), theme::row_height(theme::ROW_TWO), selected, focused, content)
            .map(|row| {
                if crate::gui::phone::active() {
                    let token = crate::gui::phone::token(&["row", picked.key.network.label(), &picked.key.account, &picked.key.id]);
                    let menu = Some(picked.clone());
                    return self.tap(row, token, menu, move |this, cx| this.pick_row(&picked, cx), cx);
                }
                row.on_mouse_down(
                    MouseButton::Left,
                    // A voice channel joins on a double-click here too; a single click only opens.
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        if join && event.click_count == 2 {
                            this.call_start(picked.key.clone(), cx);
                        } else {
                            this.pick_row(&picked, cx);
                        }
                    }),
                )
            })
    }

    /// Telegram's folder tabs under the jump strip: All, then each folder with its count.
    pub(super) fn folder_tabs(&self, cx: &mut Context<Self>) -> Option<Div> {
        let state = &self.sidebar_state;
        if state.mode != Mode::Network(Network::Telegram) || state.drill.is_some() || state.home.folders.is_empty() {
            return None;
        }
        let chosen = state.home.folder.clone();
        let tab = |id: Option<String>, name: String, unread: u32, mentions: u32, cx: &mut Context<Self>| {
            let on = chosen == id;
            let count = match (mentions, unread) {
                (0, 0) => None,
                (0, unread) => Some(crate::gui::components::badge_count(unread)),
                (mentions, _) => Some(crate::gui::components::badge_mention(mentions)),
            };
            let key = id.clone().unwrap_or_default();
            div()
                .id(gpui::SharedString::from(format!("tab-{key}")))
                .flex_none()
                .h(px(32.))
                .px(px(8.))
                .relative()
                .flex()
                .items_center()
                .gap(px(5.))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    this.sidebar_state.home.folder = id.clone();
                    this.sidebar_state.cursor = None;
                    this.load_home(cx);
                }))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(if on { theme::text() } else { theme::text_soft() }).child(name))
                .children(count)
                .children(on.then(|| div().absolute().bottom_0().left(px(8.)).right(px(8.)).h(px(2.)).rounded(px(1.)).bg(theme::text())))
        };
        let scroll = state.folder_tabs_scroll.clone();
        // Make the strip grow to its content inside a clipping wrapper; arrows are always visible
        // when content overflows the sidebar, and hidden once it fits.
        let mut tabs = div()
            .id("folder-tabs")
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(32.))
            .px(px(8.))
            .overflow_x_scroll()
            .track_scroll(&scroll);
        tabs = tabs.child(tab(None, "All".into(), 0, 0, cx));
        let folders = state.home.folders.clone();
        for folder in folders {
            tabs = tabs.child(tab(Some(folder.key.id.clone()), folder.name.clone(), folder.unread, folder.mentions, cx));
        }

        let max = scroll.max_offset().x;
        let can_left = scroll.offset().x < px(0.);
        let can_right = scroll.offset().x > -max;
        // Show arrows once the scrollable range is non-zero. max_offset updates on the second layout
        // pass, so this naturally resolves from "hidden" to "visible" once the strip is measured.
        let wide = max > px(0.);

        let arrow = |icon: &'static str, enabled: bool, direction: i32, cx: &mut Context<Shell>| {
            let id = if direction < 0 { "folder-tabs-scroll-left" } else { "folder-tabs-scroll-right" };
            icon_button(id, icon, false)
                .size(px(24.))
                .when(!enabled, |b| b.opacity(0.35).cursor_default())
                .when(enabled, |b| {
                    b.on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                        let scroll = this.sidebar_state.folder_tabs_scroll.clone();
                        let current = scroll.offset().x;
                        let new_x = (current - px(120. * direction as f32)).clamp(-max, px(0.));
                        scroll.set_offset(gpui::point(new_x, px(0.)));
                        cx.notify();
                    }))
                })
        };

        // The clipping wrapper is the same width as the sidebar; any scrollable content peeks out
        // and the arrows appear. Wrap in w_full + overflow_hidden so the strip never paints into
        // the conversation.
        Some(
            div()
                .flex()
                .w_full()
                .flex_none()
                .items_center()
                .mb(px(4.))
                .children(wide.then(|| arrow("icons/chevron-left.svg", can_left, -1, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(tabs),
                )
                .children(wide.then(|| arrow("icons/chevron-right.svg", can_right, 1, cx))),
        )
    }

    fn home_empty(&self) -> Div {
        let state = &self.sidebar_state;
        let (icon, title, detail): (&'static str, String, String) = match (&state.home.network, &state.drill) {
            (_, Some(Drill::Unreads(_))) => ("icons/check.svg", "All caught up".into(), "Unread messages from your channels and DMs show here.".into()),
            (_, Some(Drill::Threads(_))) => ("icons/messages-square.svg", "No new replies".into(), "Threads you follow show here when someone replies.".into()),
            (_, Some(Drill::Drafts(_))) => ("icons/pencil.svg", "No drafts".into(), "Messages you start typing in this workspace appear here.".into()),
            (_, Some(Drill::Archive)) => ("icons/inbox.svg", "Nothing archived".into(), "Chats you archive in Telegram show up here.".into()),
            (_, Some(Drill::Calls)) => ("icons/phone.svg", "No recent calls".into(), "Your Telegram calls show up here.".into()),
            (_, Some(Drill::Requests)) => ("icons/mail.svg", "No message requests".into(), "Messages from people you don't know wait here.".into()),
            (_, Some(Drill::Topics(_))) => ("icons/messages-square.svg", "No topics yet".into(), "Topics of this group show up here.".into()),
            (Some(Network::Discord), None) if state.home_unread => ("icons/check.svg", "Nothing unread".into(), String::new()),
            (Some(Network::Discord), None) => ("icons/hash.svg", "No servers yet".into(), "Servers you join in Discord show up here.".into()),
            (Some(Network::Telegram), None) => match state.home.folder.as_ref().and_then(|id| state.home.folders.iter().find(|f| &f.key.id == id)) {
                Some(folder) => ("icons/list.svg", format!("Nothing in {}", folder.name), "Chats you add to this folder in Telegram show up here.".into()),
                None => ("icons/message-circle.svg", "No chats yet".into(), "Type a @username to find someone.".into()),
            },
            (Some(Network::Teams), None) => ("icons/message-circle.svg", "No chats yet".into(), "Type a name to start one.".into()),
            // A newly linked device is sent no history: chats appear as messages arrive.
            (Some(Network::Signal), None) => ("icons/message-circle.svg", "No chats yet".into(), "New messages show up here as they arrive.".into()),
            (Some(Network::Messages), None) => ("icons/message-circle.svg", "No chats yet".into(), "Texts show up here while your phone is online.".into()),
            _ => ("icons/message-circle.svg", "Nothing here yet".into(), String::new()),
        };
        empty_state(icon, &title, &detail).px(px(24.))
    }

    pub(super) fn home_list(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = (snapshot, window);
        let state = &self.sidebar_state;
        if state.home_items.is_empty() {
            if !state.home.loaded {
                let network = state.home.network.map_or("", Network::label);
                let words = if state.drill.is_some() { "Getting the list…".to_owned() } else { format!("Getting your {network} chats…") };
                return div().px(px(theme::PANE_PAD)).pt(px(8.)).type_small().text_color(theme::text_faint()).child(words).into_any_element();
            }
            return self.home_empty().into_any_element();
        }
        let items = state.home_items.clone();
        let visible = on_screen(&state.home_list, items.len(), |ix| items[ix].height());
        self.watch_rows(items[visible].iter().filter_map(Item::conversation), cx);
        crate::gui::components::scroll_frame("home-scroll-frame", state.home_list.clone(), list(state.home_list.clone(), cx.processor(|this: &mut Shell, ix: usize, window, cx| this.home_item(ix, window, cx))).size_full()).keyboard(false).into_any_element()
    }
}

fn faint_words(words: String) -> Div {
    div().flex_none().max_w(px(140.)).truncate().type_small().text_color(theme::text_faint()).child(words)
}
