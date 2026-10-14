//! People: who they are, whether they are around, and who is typing.
//!
//! # For the other views
//!
//! This is the one sender of `Watch`. A view that draws people says who is on screen, and looks
//! up what chatd says about them as it draws. Everything goes through the [`Hub`], which every
//! view reaches as `shell.people_state.hub()` (a cheap handle; keep a clone if you are an entity).
//!
//! - `hub.watch("my-view", keys, cx)`: the people this view draws now, replacing what it said
//!   before. Call it when the visible rows change (a `uniform_list` range callback is the place),
//!   and with no keys when the view goes away. The union of all views is sent to chatd, debounced.
//! - `hub.watch_conversation(Some(&key), cx)`: the open conversation. The composer does this.
//! - `hub.watch_space(Some(&key), cx)`: the Space whose tree is on show. The sidebar does this.
//! - `hub.set_occupants(&channel, &occupants)` and `hub.in_voice(&person)`: who is in voice.
//! - `hub.presence_of(&person)` and `hub.status_of(&person)`: at render. `Unknown` draws nothing.
//! - `hub.typists_in(&conversation)`: who is typing there now, already timed out.
//! - `hub.avatar(&account, file, cx)`: the same `MediaFile` with its path once it is on disk;
//!   it asks `Fetch` for it otherwise and repaints when it lands. Only for rows on screen.
//! - `hub.learn(&people)`: tell the hub about people an answer carried, so a key is enough later.
//!
//! Drawing: `presence::dot(shape, avatar_size, &Ring)` is the corner shape for any avatar box
//! (`Ring::row` for list rows, `Ring::on` for a flat surface); `Shell::person_avatar` is the
//! whole avatar of a person.
//!
//! Hooks on `Shell`: `open_profile(key, anchor, cx)`, `open_person(person, cx)` (their
//! conversation, from what is held, then the store, then `StartDirect`), `start_direct(person,
//! cx)` (the same), `toggle_members(cx)`, `own_presence()`, `presence_of(key)`, `in_call(key)`,
//! `header_status(conversation)`, `person_picture(key, cx)`. The layout's slots are
//! `draw_people`, `draw_own_menu`, `presence_slot` and `typing_slot`.

mod card;
mod hub;
mod info;
mod list;
mod members;
pub mod presence;
#[cfg(feature = "gui-shot")]
pub(super) mod shot;
mod status;

pub use hub::{ask_later, Hub};

use super::components::Avatar;
use super::composer::typing_preview;
use super::layout::{RightKind, RightPane};
use super::sidebar::Mode;
use super::state::Shell;
use card::{Float, Floating};
use gpui::{prelude::*, AnyElement, Context, Div, Entity, Pixels, Point, SharedString, Window};
use info::InfoPane;
use list::PeopleList;
use members::MemberList;
use presence::{Ring, Shape};
use protocol::{
    AccountKey, Change, Conversation, ConversationKind, InboxFilter, Key, Link, MediaFile, Network, Person,
    Presence, RelateAction, Request, Response, PAGE_LIMIT,
};
use status::StatusMenu;
use std::cell::{Cell, OnceCell};
use std::path::PathBuf;

/// What the People views keep between frames. The views are made when first drawn.
#[derive(Default)]
pub struct State {
    hub: Hub,
    attached: Cell<bool>,
    list: OnceCell<Entity<PeopleList>>,
    members: OnceCell<Entity<MemberList>>,
    info: OnceCell<Entity<InfoPane>>,
    floating: OnceCell<Entity<Floating>>,
    status: OnceCell<Entity<StatusMenu>>,
}

/// One signed-in account, and how others see us on it.
#[derive(Clone, PartialEq)]
pub(in crate::gui) struct Own {
    pub key: AccountKey,
    pub name: String,
    pub link: Link,
    /// Our own person id on it.
    pub me: String,
    pub presence: Presence,
    pub status: String,
    pub avatar: Option<MediaFile>,
}

/// What the views need from the shell each time they draw.
#[derive(Clone, Default, PartialEq)]
pub(in crate::gui) struct Scene {
    /// The open conversation, and the person on the other side of it.
    pub open: Option<Key>,
    pub peer: Option<Key>,
    pub open_kind: Option<ConversationKind>,
    pub links: Vec<(Network, Link)>,
    pub accounts: Vec<Own>,
    /// The people in the call that is up.
    pub in_call: Vec<Key>,
}

/// True when this run only draws a state for `--shot`: nothing that changes the user's state on
/// a network may be sent from it.
pub(in crate::gui) fn only_draws() -> bool {
    #[cfg(feature = "gui-shot")]
    {
        static SHOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SHOT.get_or_init(|| std::env::args().any(|arg| arg == "--shot") || std::env::var_os("TINYCHAT_S6").is_some())
    }
    #[cfg(not(feature = "gui-shot"))]
    false
}

/// In the call we are in, or in a voice channel chatd told us about.
fn in_call(hub: &Hub, scene: &Scene, person: &Key) -> bool {
    scene.in_call.contains(person) || hub.in_voice(person)
}

/// The direct conversation with `person` among rows already read, on the same account.
fn existing_direct<'a>(rows: impl IntoIterator<Item = &'a Conversation>, person: &Key) -> Option<&'a Conversation> {
    rows.into_iter().find(|row| {
        row.kind == ConversationKind::Direct
            && row.peer.as_deref() == Some(person.id.as_str())
            && row.key.network == person.network
            && row.key.account == person.account
    })
}

/// The line under a direct conversation's title: the word for their presence, then what they
/// say. A call takes the place of both while it lasts. `None` when nothing is known.
fn header_line(word: &str, status: &str, in_call: bool) -> Option<String> {
    match (in_call, word.is_empty(), status.is_empty()) {
        (true, ..) => Some("In a call".to_owned()),
        (false, true, true) => None,
        (false, true, false) => Some(status.to_owned()),
        (false, false, true) => Some(word.to_owned()),
        (false, false, false) => Some(format!("{word} · {status}")),
    }
}

/// Whether tinychat can ring this person itself.
fn can_call(network: Network) -> bool {
    network == Network::Discord
}

impl State {
    pub fn hub(&self) -> Hub {
        self.hub.clone()
    }

    /// Lets the hub repaint the window. The first hook to run does it.
    pub fn attach(&self, cx: &mut Context<Shell>) {
        if self.attached.replace(true) {
            return;
        }
        // By id, not through the entity: the hub is often told from inside the shell's own update.
        let shell = cx.entity_id();
        self.hub.on_change(move |cx| cx.notify(shell));
        #[cfg(feature = "gui-shot")]
        {
            let shell = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = shell.update(cx, |shell, cx| shell.stage(cx));
            });
        }
    }
}

/// The layout's People slot: the list under the sidebar's title strip.
pub fn draw_people(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> Option<AnyElement> {
    let state = &shell.people_state;
    state.attach(cx);
    let scene = shell.scene();
    let hub = state.hub();
    let list = state
        .list
        .get_or_init(|| {
            let weak = cx.weak_entity();
            cx.new(|cx| PeopleList::new(weak, hub, cx))
        })
        .clone();
    list.update(cx, |list, cx| list.see(scene, cx));
    #[cfg(feature = "gui-shot")]
    shot::stage_people(&list, cx);
    shell.watch_peer(cx);
    // With no conversation open there is no composer to hold the card, so the list does.
    let floating = shell.snapshot.as_ref().is_none_or(|snapshot| snapshot.open.is_none());
    Some(
        gpui::div()
            .size_full()
            .child(list)
            .children(floating.then(|| shell.floating(cx)).flatten())
            .into_any_element(),
    )
}

/// The layout's own-avatar slot: every account's link, our presence on it, and setting both.
pub fn draw_own_menu(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> Option<AnyElement> {
    let state = &shell.people_state;
    state.attach(cx);
    let (scene, hub) = (shell.scene(), state.hub());
    let menu = state
        .status
        .get_or_init(|| {
            let weak = cx.weak_entity();
            cx.new(|cx| StatusMenu::new(weak, hub, cx))
        })
        .clone();
    menu.update(cx, |menu, cx| menu.see(scene, cx));
    #[cfg(feature = "gui-shot")]
    shot::stage_status(&menu, cx);
    Some(menu.into_any_element())
}

/// The layout's presence slot, for the avatars of direct rows.
pub fn presence_slot(shell: &Shell, person: &Key) -> Presence {
    shell.people_state.hub.presence_of(person)
}

/// The layout's typing slot: what a direct row says while someone is typing in it.
pub fn typing_slot(shell: &Shell, conversation: &Key) -> Option<SharedString> {
    let typists = shell.people_state.hub.typists_in(conversation);
    let names: Vec<&str> = typists.iter().map(|typist| typist.name.as_str()).collect();
    // CONTRACT: the slot is handed a key, not the row, so a group cannot be told from a direct
    // conversation here; two typists can only be a group.
    typing_preview(&names, names.len() > 1).map(SharedString::from)
}

fn draw_members(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let state = &shell.people_state;
    state.attach(cx);
    let scene = shell.scene();
    let hub = state.hub();
    let members = state
        .members
        .get_or_init(|| {
            let weak = cx.weak_entity();
            cx.new(|cx| MemberList::new(weak, hub, cx))
        })
        .clone();
    members.update(cx, |members, cx| members.see(scene, cx));
    #[cfg(feature = "gui-shot")]
    shot::stage_members(&members, shell.open.clone(), cx);
    members.into_any_element()
}

fn draw_info(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    shell.people_state.attach(cx);
    let scene = shell.scene();
    let info = shell.info_entity(cx);
    info.update(cx, |info, cx| info.see(scene, cx));
    info.into_any_element()
}

impl Shell {
    fn info_entity(&self, cx: &mut Context<Self>) -> Entity<InfoPane> {
        let hub = self.people_state.hub();
        self.people_state
            .info
            .get_or_init(|| {
                let weak = cx.weak_entity();
                cx.new(|_| InfoPane::new(weak, hub))
            })
            .clone()
    }

    /// Puts a person in the right pane: who they are, what the network says of them, and what
    /// was shared in the conversation with them. Replaces whatever the pane held.
    pub(super) fn show_info(&mut self, person: Key, cx: &mut Context<Self>) {
        self.people_state.attach(cx);
        let info = self.info_entity(cx);
        info.update(cx, |info, cx| info.show(person, cx));
        if self.right_kind() != Some(RightKind::Info) {
            self.open_right(RightPane { kind: RightKind::Info, title: "Profile".into(), count: None, body: draw_info }, cx);
        }
        cx.notify();
    }

    /// The header's button in a direct conversation: the other person, in the right pane.
    pub(super) fn toggle_info(&mut self, cx: &mut Context<Self>) {
        let peer = self.scene().peer;
        let shown = self.people_state.info.get().and_then(|info| info.read(cx).person().cloned());
        match peer {
            Some(peer) if self.right_kind() != Some(RightKind::Info) || shown.as_ref() != Some(&peer) => {
                self.show_info(peer, cx);
            }
            _ => self.close_right(cx),
        }
    }

    fn scene(&self) -> Scene {
        let Some(snapshot) = &self.snapshot else {
            return Scene::default();
        };
        let open = snapshot.open.as_ref();
        Scene {
            open: open.map(|open| open.key.clone()),
            peer: open.and_then(peer_of),
            open_kind: open.map(|open| open.kind),
            links: snapshot.networks.iter().map(|status| (status.network, status.link)).collect(),
            accounts: snapshot
                .accounts
                .iter()
                .map(|account| Own {
                    key: account.key.clone(),
                    name: account.name.clone(),
                    link: account.link,
                    me: account.me.clone(),
                    presence: account.presence,
                    status: account.status.clone(),
                    avatar: account.avatar.clone(),
                })
                .collect(),
            in_call: snapshot
                .call
                .iter()
                .filter(|call| call.state == protocol::CallState::Joined)
                .flat_map(|call| call.participants.iter().map(|person| Key { id: person.id.clone(), ..call.conversation.clone() }))
                .collect(),
        }
    }

    /// Presence, typing and the owned lists changing. Called for every change chatd reports.
    pub(super) fn people_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        self.people_state.attach(cx);
        self.people_state.hub.changed(change, cx);
        let state = &self.people_state;
        match change {
            Change::People { account } => {
                if let Some(list) = state.list.get() {
                    list.update(cx, |list, cx| list.people_changed(account, cx));
                }
            }
            Change::Members { conversation } => {
                if let Some(members) = state.members.get() {
                    members.update(cx, |members, cx| members.members_changed(conversation, cx));
                }
            }
            Change::Searched { account, query } => {
                if let Some(list) = state.list.get() {
                    list.update(cx, |list, cx| list.searched(account, query, cx));
                }
            }
            _ => {}
        }
        self.sweep(cx);
    }

    /// The rows the shell holds now, wherever they are drawn.
    fn held_rows(&self) -> impl Iterator<Item = &Conversation> {
        let open = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref());
        let lists = self.sidebar_state.inbox.rows().map(|(_, row)| row).chain(&self.sidebar_state.directs);
        let trees = self.trees.values().flat_map(|tree| &tree.conversations);
        open.into_iter().chain(lists.chain(trees).map(|row| &**row))
    }

    /// The open conversation's other person is drawn in its header, so they are watched.
    fn watch_peer(&self, cx: &mut Context<Self>) {
        let peer = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).and_then(peer_of);
        self.people_state.hub.watch("header", peer, cx);
    }

    /// What is known of a person's presence now. `Unknown` draws nothing.
    pub(super) fn presence_of(&self, person: &Key) -> Presence {
        self.people_state.hub.presence_of(person)
    }

    /// In the call we are in, or in a voice channel chatd reported.
    pub(super) fn in_call(&self, person: &Key) -> bool {
        let joined = self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref()).filter(|call| call.state == protocol::CallState::Joined);
        joined.is_some_and(|call| {
            call.conversation.account() == person.account() && call.participants.iter().any(|who| who.id == person.id)
        }) || self.people_state.hub.in_voice(person)
    }

    /// The line under the title of a direct conversation: `Online · heads down until 4`.
    /// `None` for anything that is not one, and when nothing is known of the other person.
    #[allow(dead_code)]
    pub(super) fn header_status(&self, conversation: &Key) -> Option<SharedString> {
        let row = self.held_rows().find(|row| &row.key == conversation)?;
        if row.kind != ConversationKind::Direct {
            return None;
        }
        let peer = peer_of(row)?;
        let hub = &self.people_state.hub;
        let known = hub.person(&peer);
        let presence = match hub.presence_of(&peer) {
            Presence::Unknown => known.as_ref().map_or(Presence::Unknown, |person| person.presence),
            seen => seen,
        };
        let status = match hub.status_of(&peer) {
            status if status.is_empty() => known.map(|person| person.status).unwrap_or_default(),
            status => status,
        };
        header_line(presence::word(presence, peer.network), &status, self.in_call(&peer)).map(SharedString::from)
    }

    /// Where a person's picture is on disk. When it is not there yet it is asked for, and the
    /// window repaints as it lands. Only for what is on screen.
    pub(super) fn person_picture(&self, person: &Key, cx: &mut Context<Self>) -> Option<PathBuf> {
        let hub = &self.people_state.hub;
        let file = hub.person(person).and_then(|known| known.avatar).or_else(|| {
            // We are a person too, and no list names us.
            let accounts = &self.snapshot.as_ref()?.accounts;
            accounts.iter().find(|own| own.key == person.account() && own.me == person.id)?.avatar.clone()
        })?;
        hub.file(&person.account(), &file, cx).path.map(PathBuf::from)
    }

    /// Lets go of what a list watched once it has left the screen.
    pub(super) fn sweep(&self, cx: &mut Context<Self>) {
        let hub = &self.people_state.hub;
        if self.sidebar_state.mode != Mode::People {
            hub.watch(list::SOURCE, [], cx);
        }
        if self.right_kind() != Some(RightKind::Members) {
            hub.watch(members::SOURCE, [], cx);
            hub.watch_members(None, cx);
        }
        if self.right_kind() != Some(RightKind::Info) {
            if let Some(info) = self.people_state.info.get().filter(|info| info.read(cx).person().is_some()) {
                info.clone().update(cx, |info, cx| info.hide(cx));
            }
        }
    }

    /// The profile card and the row menu, over everything. Drawn by one host a frame: the
    /// composer when a conversation is open, the People list when none is.
    pub(super) fn floating(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.watch_peer(cx);
        let floating = self.people_state.floating.get()?.clone();
        let scene = self.scene();
        floating.update(cx, |floating, cx| floating.see(scene, cx));
        floating.read(cx).is_open().then(|| floating.into_any_element())
    }

    fn floating_entity(&self, cx: &mut Context<Self>) -> Entity<Floating> {
        let hub = self.people_state.hub();
        self.people_state
            .floating
            .get_or_init(|| {
                let weak = cx.weak_entity();
                cx.new(|_| Floating::new(weak, hub))
            })
            .clone()
    }

    /// Opens the profile card of a person beside `anchor`: a click's position, or an avatar's
    /// corner. Escape or a click outside closes it.
    pub(super) fn open_profile(&mut self, person: Key, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        // With the side pane on people (a member list, or someone already), a person clicked
        // goes there and stays while the conversation is read. A thread is not given up for it.
        if matches!(self.right_kind(), Some(RightKind::Members | RightKind::Info)) {
            return self.show_info(person, cx);
        }
        self.people_state.attach(cx);
        let floating = self.floating_entity(cx);
        floating.update(cx, |floating, cx| floating.open(Float::Card(person), anchor, cx));
        cx.notify();
    }

    fn open_row_menu(&mut self, person: Key, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        let floating = self.floating_entity(cx);
        floating.update(cx, |floating, cx| floating.open(Float::Menu(person), anchor, cx));
        cx.notify();
    }

    /// Opens the conversation with a person: at once when a list on show holds it, else from
    /// the store, and only when there is none does chatd make one. Nothing reaches the other
    /// person until a message is sent.
    pub(super) fn open_person(&mut self, person: &Person, cx: &mut Context<Self>) {
        self.open_person_then(person, false, cx);
    }

    /// The same as [`Shell::open_person`].
    #[allow(dead_code)]
    pub(super) fn start_direct(&mut self, person: &Person, cx: &mut Context<Self>) {
        self.open_person_then(person, false, cx);
    }

    /// The same, and then rings them.
    fn call_person(&mut self, person: &Person, cx: &mut Context<Self>) {
        self.open_person_then(person, true, cx);
    }

    fn open_person_then(&mut self, person: &Person, call: bool, cx: &mut Context<Self>) {
        if let Some(row) = existing_direct(self.held_rows(), &person.key).cloned() {
            self.show_direct(row, call, cx);
            return;
        }
        let (key, name) = (person.key.clone(), person.name.clone());
        let read = move || -> std::io::Result<Response> {
            let stored = Request::Inbox {
                network: Some(key.network),
                filter: InboxFilter::Directs,
                offset: 0,
                limit: PAGE_LIMIT,
            };
            if let Response::Conversations { conversations, .. } = crate::client::ask(&stored)? {
                if let Some(row) = existing_direct(&conversations, &key) {
                    return Ok(Response::Conversation { conversation: row.clone() });
                }
            }
            if only_draws() {
                return Ok(Response::Ok);
            }
            crate::client::ask(&Request::StartDirect { person: key })
        };
        self.off_thread(cx, read, move |shell, answer, cx| {
            let refused = match answer {
                Ok(Response::Conversation { conversation }) => {
                    shell.show_direct(conversation, call, cx);
                    None
                }
                Ok(Response::Ok) => None,
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(other) => Some(format!("unexpected {other:?}")),
                Err(err) => Some(err.to_string()),
            };
            shell.people_note(refused.map(|why| format!("Can't open {name}: {why}")), cx);
        });
    }

    fn show_direct(&mut self, row: Conversation, call: bool, cx: &mut Context<Self>) {
        let key = row.key.clone();
        self.select(key.clone(), &row.title, cx);
        // The row is in hand, so the header need not wait for it to be read.
        if let Some(snapshot) = self.snapshot.as_mut().filter(|_| self.open.as_ref() == Some(&key)) {
            if snapshot.open.as_ref().is_none_or(|open| open.key != key || open.peer.is_none()) {
                snapshot.open = Some(row);
            }
        }
        if call && !only_draws() {
            self.call_start(key, cx);
        }
        cx.notify();
    }

    /// What went wrong with something the user did to a person: the People list's one line
    /// when it is on show, else a toast.
    fn people_note(&mut self, words: Option<String>, cx: &mut Context<Self>) {
        match self.people_state.list.get().filter(|_| self.sidebar_state.mode == Mode::People) {
            Some(list) => list.update(cx, |list, cx| list.refused(words, cx)),
            None => {
                if let Some(words) = words {
                    self.toast(Some(("icons/circle-alert.svg", super::theme::danger())), words, cx);
                }
            }
        }
    }

    /// Answers, withdraws or ends a friendship, or blocks: only ever from a click. The row
    /// changes in place when the network has agreed.
    fn relate(&mut self, person: &Person, action: RelateAction, cx: &mut Context<Self>) {
        if only_draws() {
            return;
        }
        // The list may be what called this, so it is only touched once the answer is back.
        let list = self.people_state.list.get().cloned();
        let (key, name, hub) = (person.key.clone(), person.name.clone(), self.people_state.hub());
        let request = Request::Relate { person: key.clone(), action };
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(err.to_string()),
            };
            if refused.is_none() {
                hub.related(&key, action);
            }
            if let Some(list) = list {
                list.update(cx, |list, cx| list.related(&key, action, refused.is_none(), cx));
            }
            let words = refused.map(|why| format!("{}: {why}", relate_failed(action, &name)));
            let _ = this.update(cx, |shell, cx| shell.people_note(words, cx));
        });
    }

    /// Shows or hides the member list of the open conversation in the right pane.
    pub(super) fn toggle_members(&mut self, cx: &mut Context<Self>) {
        if self.right_kind() == Some(RightKind::Members) {
            self.close_right(cx);
            return;
        }
        let count = self.people_state.members.get().and_then(|members| members.read(cx).total());
        self.open_right(RightPane { kind: RightKind::Members, title: "Members".into(), count, body: draw_members }, cx);
    }

    /// Our own presence, for the rail's avatar: the most available among the connected accounts.
    pub(super) fn own_presence(&self) -> Presence {
        let accounts = self.snapshot.as_ref().map(|snapshot| snapshot.accounts.as_slice()).unwrap_or_default();
        status::most_available(accounts.iter().filter(|own| own.link == Link::Connected).map(|own| own.presence))
    }

    /// A person's avatar with their picture and presence. `ring` is the opaque color behind it.
    #[allow(dead_code)]
    pub(super) fn person_avatar(&self, person: &Person, size: f32, ring: &Ring, cx: &mut Context<Self>) -> Div {
        person_avatar(&self.people_state.hub, person, size, ring, self.in_call(&person.key), cx)
    }
}

fn relate_failed(action: RelateAction, name: &str) -> String {
    match action {
        RelateAction::Accept => format!("Can't accept {name}"),
        RelateAction::Ignore => format!("Can't ignore {name}"),
        RelateAction::Cancel => format!("Can't cancel the request to {name}"),
        RelateAction::Remove => format!("Can't remove {name}"),
        RelateAction::Block => format!("Can't block {name}"),
    }
}

/// The other person of a direct conversation.
fn peer_of(conversation: &Conversation) -> Option<Key> {
    let peer = conversation.peer.as_ref()?;
    Some(Key { id: peer.clone(), ..conversation.key.clone() })
}

/// The avatar of a person: the picture when it is on disk, the tint until then, and the
/// presence shape on its corner. Bots never carry one.
fn person_avatar(hub: &Hub, person: &Person, size: f32, ring: &Ring, in_call: bool, cx: &mut gpui::App) -> Div {
    let picture = hub.avatar(&person.key.account(), person.avatar.as_ref(), cx);
    let avatar = Avatar::new(&person.key, &person.name, size).image(picture.as_ref()).build();
    let shape = match (person.bot, in_call) {
        (true, _) => None,
        (false, true) => Some(Shape::InCall),
        (false, false) => Shape::of(shown_presence(hub, person)),
    };
    avatar.children(shape.and_then(|shape| presence::dot(shape, size, ring)))
}

/// What is known now beats what the list said when it was read.
fn shown_presence(hub: &Hub, person: &Person) -> Presence {
    match hub.presence_of(&person.key) {
        Presence::Unknown => person.presence,
        known => known,
    }
}

/// Line two of a person's row: their status text, else the word for their presence, else the
/// handle.
fn status_line(hub: &Hub, person: &Person) -> String {
    let status = hub.status_of(&person.key);
    let status = if status.is_empty() { person.status.clone() } else { status };
    if !status.is_empty() {
        return status;
    }
    match shown_presence(hub, person) {
        Presence::Unknown if person.handle.is_empty() => String::new(),
        Presence::Unknown => format!("@{}", person.handle.trim_start_matches('@')),
        known => presence::word(known, person.key.network).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(account: &str, id: &str) -> Key {
        Key { network: Network::Discord, account: account.into(), id: id.into() }
    }

    fn row(account: &str, id: &str, kind: ConversationKind, peer: Option<&str>) -> Conversation {
        Conversation {
            key: key(account, id),
            kind,
            title: id.to_owned(),
            space: None,
            space_title: String::new(),
            group: None,
            group_title: String::new(),
            topic: String::new(),
            parent: None,
            root: None,
            position: 0,
            avatar: None,
            peer: peer.map(str::to_owned),
            unread: 0,
            mentions: 0,
            muted: false,
            starred: false,
            preview: String::new(),
            updated: 0,
            last_read: None,
            call: protocol::CallSupport::None,
            occupants: None,
            read_only: false,
            private: false,
            draft: String::new(),
        }
    }

    #[test]
    fn a_person_opens_the_direct_conversation_already_held_on_their_account() {
        use ConversationKind::{Direct, Group};
        let rows = [
            row("1", "group", Group, Some("ines")),
            row("2", "other-account", Direct, Some("ines")),
            row("1", "dm-kofi", Direct, Some("kofi")),
            row("1", "dm-ines", Direct, Some("ines")),
            row("1", "channel", ConversationKind::Channel, None),
        ];
        assert_eq!(existing_direct(&rows, &key("1", "ines")).map(|row| row.key.id.as_str()), Some("dm-ines"));
        assert_eq!(existing_direct(&rows, &key("2", "ines")).map(|row| row.key.id.as_str()), Some("other-account"));
        // Nobody to open: this is when `StartDirect` is asked.
        assert!(existing_direct(&rows, &key("1", "mara")).is_none());
        let slack = Key { network: Network::Slack, ..key("1", "ines") };
        assert!(existing_direct(&rows, &slack).is_none());
    }

    #[test]
    fn the_header_says_the_word_then_the_status_and_a_call_replaces_both() {
        assert_eq!(header_line("Online", "heads down until 4", false).as_deref(), Some("Online · heads down until 4"));
        assert_eq!(header_line("Away", "", false).as_deref(), Some("Away"));
        assert_eq!(header_line("", "On holiday", false).as_deref(), Some("On holiday"));
        assert_eq!(header_line("", "", false), None);
        assert_eq!(header_line("Do not disturb", "Presenting", true).as_deref(), Some("In a call"));
    }
}
