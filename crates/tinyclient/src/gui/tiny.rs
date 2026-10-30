//! The Tiny server window. Rooms, text channels, direct messages, and people are the sidebar.
//! Picking a channel or a direct message opens the conversation pane. The server pane is the
//! invite, the one person you picked, or one room. Join a server is always there: it opens the
//! same paste sheet, and a second server is kept beside this one. A control the account cannot
//! use is absent, never disabled.
//!
//! A room is drawn as a voice channel is: its name, then who is in its call, ringed while they
//! speak. On a server whose audio is up, joining is a call: the call dock at the foot of the
//! sidebar carries it, with mute, deafen, devices and Leave, as for a Discord voice channel. On
//! one without audio, joining takes a seat and nothing more, the room says so, and a strip at the
//! foot of the sidebar keeps the seat in sight. Changes arrive from chatd's live socket
//! (`Change::Tiny`, `Occupants`, `Presence`); nothing here polls.

use super::components::{button, button_disabled, field, glyph, list_row, pane_title, section_header, segment, segmented, Avatar, ButtonKind};
use super::input::{InputEvent, InputKey, TextInput};
use super::layout::{drags_window, TOP_BAR};
use super::sidebar::Mode;
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, Entity, Focusable, MouseButton, Window};
use protocol::{AccountKey, Change, Conversation, ConversationKind, Key, Link, Network, Presence, Request, Response};
use std::collections::HashSet;
use std::io;
use std::rc::Rc;

/// Shown when the server's token cannot be wrapped with this server's address.
const UNMADE: &str = "The invite could not be made into a code.";
/// Said wherever a seat is on a server without audio: the room is a call with no sound.
const NO_SOUND: &str = "This server has no audio turned on, so joining only takes a seat.";
/// Said in a room on a server with audio. tinyclient cancels no echo yet.
const HEADPHONES: &str = "Wear headphones: tinyclient doesn't cancel echo yet, so others would hear your speakers.";
const ROOM_NAME_SIZE: &str = "A room name is 1 to 80 characters.";

/// What the server window is holding. The invite token stays until Done and is never logged.
pub struct State {
    seen: bool,
    account: Option<AccountKey>,
    load_epoch: u64,
    action_epoch: u64,
    loading: bool,
    community: Option<protocol::TinyCommunity>,
    error: Option<String>,
    /// `"member"` or `"admin"`. Never owner.
    invite_role: String,
    /// The paste code, shown once. It carries the server. The raw token is not shown alone.
    token: Option<String>,
    /// The server packed into `token`.
    origin: Option<String>,
    copied: bool,
    busy: bool,
    confirm: Option<String>,
    /// A person on show. None is the invite.
    picked: Option<String>,
    /// A room on show, over the person or the invite.
    room: Option<String>,
    /// The room name field is open: for a new room (`None`), or to rename one.
    naming: Option<Option<String>>,
    room_name: Entity<TextInput>,
    /// The room the delete question is open for.
    confirm_room: Option<String>,
    /// A join, leave or remove-from-call is with the server. It has its own quiet.
    call_busy: bool,
    call_epoch: u64,
    /// What the server said no to, under the room.
    call_error: Option<String>,
}

impl State {
    pub(super) fn new(cx: &mut Context<Shell>) -> State {
        let room_name = cx.new(|cx| TextInput::new("Room name", cx));
        cx.subscribe(&room_name, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.save_room(cx),
            InputEvent::Changed => {
                this.tiny_state.error = None;
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&room_name, |this, _, event: &InputKey, cx| {
            if *event == InputKey::Escape {
                this.tiny_state.naming = None;
                cx.notify();
            }
        })
        .detach();
        State {
            seen: false,
            account: None,
            load_epoch: 0,
            action_epoch: 0,
            loading: false,
            community: None,
            error: None,
            invite_role: "member".into(),
            token: None,
            origin: None,
            copied: false,
            busy: false,
            confirm: None,
            picked: None,
            room: None,
            naming: None,
            room_name,
            confirm_room: None,
            call_busy: false,
            call_epoch: 0,
            call_error: None,
        }
    }

    /// Everything that belongs to one server's page, when another is shown.
    fn forget_page(&mut self) {
        self.community = None;
        self.token = None;
        self.origin = None;
        self.copied = false;
        self.confirm = None;
        self.picked = None;
        self.error = None;
        self.room = None;
        self.naming = None;
        self.confirm_room = None;
        self.call_error = None;
    }
}

struct Member {
    user_id: String,
    name: String,
    email: String,
    role: String,
    make_admin: bool,
    make_member: bool,
    make_owner: bool,
    remove: bool,
}

/// Someone in a room's call.
struct Seated {
    user_id: String,
    name: String,
    muted: bool,
    deafened: bool,
    /// Remove from call is drawn for them.
    kick: bool,
    speaking: bool,
}

struct Room {
    id: String,
    name: String,
    people: Vec<Seated>,
    /// I am in this room's call.
    mine: bool,
}

struct OpenInvite {
    id: String,
    role: String,
    expires: String,
}

struct Ready {
    error: Option<String>,
    token: Option<String>,
    origin: Option<String>,
    /// The signed-in server, before a code exists.
    home: String,
    copied: bool,
    invite_role: String,
    can_invite: bool,
    busy: bool,
    confirm: Option<String>,
    members: Vec<Member>,
    invites: Vec<OpenInvite>,
    can_revoke: bool,
    /// `None` when this account cannot read the audit.
    audit: Option<Vec<String>>,
    rooms: Vec<Room>,
    can_make_room: bool,
    can_rename_room: bool,
    can_delete_room: bool,
    /// The live socket is up: presence and seats are current.
    live: bool,
    online: HashSet<String>,
    me: Option<String>,
    naming: Option<Option<String>>,
    confirm_room: Option<String>,
    call_busy: bool,
    call_error: Option<String>,
    /// What stands in the way of Join, in words. `None` when a seat can be taken.
    join_blocked: Option<&'static str>,
    /// The server's audio is up: a join is a call with sound.
    audio: bool,
    /// The room whose call the call dock carries now, which takes the seat strip's place.
    docked: Option<String>,
}

/// What the pane shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shown<'a> {
    Invite,
    Person(&'a str),
    Room(&'a str),
    NewRoom,
}

enum Page {
    Loading,
    Failed(String),
    Ready(Ready),
}

/// The server on show. The one already open stays, including while it is still connecting.
/// Otherwise the first connected server, then any Tiny server that is still signed in.
fn preferred_tiny(accounts: &[(AccountKey, Link)], current: Option<&AccountKey>) -> Option<AccountKey> {
    let signed_in = |key: &AccountKey, link: Link| key.network == Network::Tiny && link != Link::SignedOut;
    if let Some(current) = current {
        if accounts.iter().any(|(key, link)| key == current && signed_in(key, *link)) {
            return Some(current.clone());
        }
    }
    accounts
        .iter()
        .find(|(key, link)| key.network == Network::Tiny && *link == Link::Connected)
        .or_else(|| accounts.iter().find(|(key, link)| signed_in(key, *link)))
        .map(|(key, _)| key.clone())
}

fn same_role(role: &str, name: &str) -> bool {
    role.eq_ignore_ascii_case(name)
}

fn known_role(role: &str) -> bool {
    same_role(role, "owner") || same_role(role, "admin") || same_role(role, "member")
}

/// A role button is drawn only when that change would be legal.
fn role_change_legal(caller: &str, target: &str, next: &str, owners: usize) -> bool {
    if same_role(target, next) || !known_role(next) {
        return false;
    }
    // The only owner cannot be demoted.
    if same_role(target, "owner") && owners <= 1 {
        return false;
    }
    // Changing an owner, or granting owner, is the owner's act alone.
    if same_role(target, "owner") || same_role(next, "owner") {
        return same_role(caller, "owner");
    }
    if same_role(caller, "owner") {
        return true;
    }
    // An admin may promote a member, and does not demote another admin.
    same_role(caller, "admin") && same_role(target, "member") && same_role(next, "admin")
}

fn remove_legal(caller: &str, target: &str, owners: usize) -> bool {
    if same_role(target, "owner") && owners <= 1 {
        return false;
    }
    same_role(caller, "owner") || (same_role(caller, "admin") && same_role(target, "member"))
}

fn can_make(caller: &str, target: &str, next: &str, owners: usize) -> bool {
    protocol::tiny_can(caller, "community.members.role") && role_change_legal(caller, target, next, owners)
}

fn can_remove(caller: &str, target: &str, owners: usize) -> bool {
    protocol::tiny_can(caller, "community.members.remove") && remove_legal(caller, target, owners)
}

/// Why Join cannot be pressed now.
fn join_blocked(live: bool, media: &str) -> Option<&'static str> {
    match media {
        _ if !live => Some("Tiny server isn't connected. Join once it is."),
        "down" => Some("This server's audio is down. Try again later."),
        _ => None,
    }
}

/// Remove from call: an owner, or an admin for anyone but an owner. Never yourself; Leave is.
fn can_kick(caller: &str, target: &str, mine: bool) -> bool {
    !mine && protocol::tiny_can(caller, "community.calls.kick") && (same_role(caller, "owner") || !same_role(target, "owner"))
}

fn role_word(role: &str) -> String {
    if same_role(role, "owner") {
        "Owner".to_owned()
    } else if same_role(role, "admin") {
        "Admin".to_owned()
    } else if same_role(role, "member") {
        "Member".to_owned()
    } else {
        role.to_owned()
    }
}

fn person_name(display: &str, email: &str) -> (String, String) {
    if !display.is_empty() {
        (display.to_owned(), email.to_owned())
    } else if !email.is_empty() {
        (email.to_owned(), String::new())
    } else {
        ("Someone".to_owned(), String::new())
    }
}

impl Shell {
    /// Loads the server when the window is actually showing it, and again the next time it is.
    pub(super) fn note_tiny_page(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_shown() else {
            self.tiny_state.seen = false;
            return;
        };
        if self.tiny_state.seen && self.tiny_state.account.as_ref() == Some(&account) {
            return;
        }
        self.tiny_state.seen = true;
        self.fetch(account, None, cx);
    }

    pub(super) fn tiny_open(&self) -> bool {
        self.tiny_state.seen
    }

    /// The server the page is showing, when one has been chosen.
    pub(super) fn tiny_account(&self) -> Option<&AccountKey> {
        self.tiny_state.account.as_ref()
    }

    fn tiny_shown(&self) -> Option<AccountKey> {
        if self.shown().empty_state().is_some() {
            return None;
        }
        if !matches!(self.sidebar_state.mode, Mode::Network(Network::Tiny)) {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.open.is_some() {
            return None;
        }
        let accounts: Vec<(AccountKey, Link)> = snapshot.accounts.iter().map(|status| (status.key.clone(), status.link)).collect();
        preferred_tiny(&accounts, self.tiny_state.account.as_ref())
    }

    /// Remembers which server the page shows. When the page is up, it loads that server now.
    pub(super) fn prefer_tiny(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        if self.tiny_state.account.as_ref() == Some(&account) && (self.tiny_state.community.is_some() || self.tiny_state.loading) {
            return;
        }
        let showing = matches!(self.sidebar_state.mode, Mode::Network(Network::Tiny)) && self.snapshot.as_ref().is_some_and(|snapshot| snapshot.open.is_none());
        if showing {
            self.tiny_state.seen = true;
            self.fetch(account, None, cx);
        } else {
            self.tiny_state.account = Some(account);
            self.tiny_state.seen = false;
            self.tiny_state.forget_page();
        }
    }

    pub(super) fn tiny_page(&self, people_beside: bool, window: &Window, cx: &mut Context<Self>) -> Div {
        let page = self.page();
        let me = self.tiny_state.account.as_ref().map(|key| key.account.as_str());
        let (title, body) = match &page {
            Page::Loading => ("Tiny server".to_owned(), note_line("Loading this server.", false).into_any_element()),
            Page::Failed(detail) => ("Tiny server".to_owned(), note_line(detail, true).into_any_element()),
            Page::Ready(ready) => {
                let shown = shown(&self.tiny_state, &ready);
                let title = match shown {
                    Shown::Person(id) => ready.members.iter().find(|member| member.user_id == id).map(|member| member.name.clone()),
                    Shown::Room(id) => ready.rooms.iter().find(|room| room.id == id).map(|room| room.name.clone()),
                    Shown::NewRoom => Some("New room".to_owned()),
                    Shown::Invite => None,
                }
                .unwrap_or_else(|| "Invite".to_owned());
                let input = self.tiny_state.room_name.clone();
                let focused = input.focus_handle(cx).is_focused(window);
                let chats = self.tiny_chats();
                let open = self.open.clone();
                (title, ready_view(&ready, shown, me, people_beside, (&input, focused), &chats, open.as_ref(), cx))
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                drags_window(div())
                    .h(px(TOP_BAR))
                    .flex_none()
                    .px(px(theme::PANE_PAD))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.strip_controls(window, cx))
                    .child(pane_title(title)),
            )
            .child(body)
    }

    /// The name of the Tiny room `key` names, for the call dock.
    pub(super) fn tiny_room(&self, key: &Key) -> Option<String> {
        let community = self.tiny_state.community.as_ref().filter(|_| self.tiny_state.account.as_ref() == Some(&key.account()))?;
        community.rooms.iter().find(|room| room.id == key.id).map(|room| room.name.clone())
    }

    fn page(&self) -> Page {
        let state = &self.tiny_state;
        let live = state.account.as_ref().is_some_and(|account| {
            self.snapshot.as_ref().is_some_and(|snapshot| snapshot.accounts.iter().any(|status| &status.key == account && status.link == Link::Connected))
        });
        if state.loading && state.community.is_none() {
            return Page::Loading;
        }
        let Some(community) = state.community.as_ref() else {
            return match state.error.clone() {
                Some(detail) => Page::Failed(detail),
                None => Page::Loading,
            };
        };
        let caller = community.role.as_str();
        let owners = community.members.iter().filter(|member| same_role(&member.role, "owner")).count();
        let can_invite = protocol::tiny_can(caller, "community.invites.create");
        let members = if protocol::tiny_can(caller, "community.members.read") {
            community
                .members
                .iter()
                .map(|member| {
                    let (name, email) = person_name(&member.display_name, &member.email);
                    let role = member.role.as_str();
                    Member {
                        user_id: member.user_id.clone(),
                        name,
                        email,
                        role: role_word(role),
                        make_admin: can_make(caller, role, "admin", owners),
                        make_member: can_make(caller, role, "member", owners),
                        make_owner: can_make(caller, role, "owner", owners),
                        remove: can_remove(caller, role, owners),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let invites = if protocol::tiny_can(caller, "community.invites.read") {
            community
                .invites
                .iter()
                .filter(|invite| invite.accepted_at.is_none())
                .map(|invite| OpenInvite { id: invite.id.clone(), role: role_word(&invite.role), expires: invite.expires_at.to_string() })
                .collect()
        } else {
            Vec::new()
        };
        let audit = protocol::tiny_can(caller, "community.audit.read").then(|| {
            community
                .audit
                .iter()
                .filter_map(|row| audit_line(&row.action, &row.detail, |id| members.iter().find(|member| member.user_id == id).map(|member| member.name.as_str())))
                .collect()
        });
        let me = state.account.as_ref().map(|account| account.account.clone());
        let role_of = |id: &str| community.members.iter().find(|member| member.user_id == id).map_or("member", |member| member.role.as_str());
        let name_of = |id: &str| members.iter().find(|member| member.user_id == id).map_or_else(|| "Someone".to_owned(), |member| member.name.clone());
        let rooms = community
            .rooms
            .iter()
            .map(|room| Room {
                id: room.id.clone(),
                name: room.name.clone(),
                people: room
                    .participants
                    .iter()
                    .map(|seat| Seated {
                        user_id: seat.user_id.clone(),
                        name: name_of(&seat.user_id),
                        muted: seat.muted,
                        deafened: seat.deafened,
                        kick: can_kick(caller, role_of(&seat.user_id), me.as_deref() == Some(seat.user_id.as_str())),
                        speaking: seat.speaking,
                    })
                    .collect(),
                mine: me.as_ref().is_some_and(|me| room.participants.iter().any(|seat| &seat.user_id == me)),
            })
            .collect();
        Page::Ready(Ready {
            rooms,
            can_make_room: protocol::tiny_can(caller, "community.rooms.create"),
            can_rename_room: protocol::tiny_can(caller, "community.rooms.rename"),
            can_delete_room: protocol::tiny_can(caller, "community.rooms.delete"),
            live,
            online: if live { community.online.iter().cloned().collect() } else { HashSet::new() },
            me,
            naming: state.naming.clone(),
            confirm_room: state.confirm_room.clone(),
            call_busy: state.call_busy,
            call_error: state.call_error.clone(),
            join_blocked: join_blocked(live, &community.media),
            audio: community.media == "up",
            docked: self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref()).filter(|call| state.account.as_ref() == Some(&call.conversation.account())).map(|call| call.conversation.id.clone()),
            error: state.error.clone().filter(|detail| !detail.is_empty()),
            token: state.token.clone(),
            origin: state.origin.clone(),
            home: community.origin.clone(),
            copied: state.copied,
            invite_role: state.invite_role.clone(),
            can_invite,
            busy: state.busy,
            confirm: state.confirm.clone(),
            members,
            invites,
            can_revoke: protocol::tiny_can(caller, "community.invites.revoke"),
            audit,
        })
    }

    /// `clear_busy` is the action whose button stays quiet until this read returns.
    fn fetch(&mut self, account: AccountKey, clear_busy: Option<u64>, cx: &mut Context<Self>) {
        let switched = self.tiny_state.account.as_ref() != Some(&account);
        if switched {
            self.tiny_state.forget_page();
            self.tiny_state.invite_role = "member".into();
            self.tiny_state.action_epoch += 1;
            self.tiny_state.busy = false;
            self.tiny_state.call_epoch += 1;
            self.tiny_state.call_busy = false;
        }
        self.tiny_state.account = Some(account.clone());
        if self.tiny_state.community.is_none() {
            self.tiny_state.loading = true;
            self.tiny_state.error = None;
        }
        self.tiny_state.load_epoch += 1;
        let epoch = self.tiny_state.load_epoch;
        self.off_thread(cx, move || ask(&Request::TinyCommunity { account }), move |this, answer, cx| {
            if this.tiny_state.load_epoch != epoch {
                return;
            }
            this.tiny_state.loading = false;
            if let Some(action) = clear_busy {
                if this.tiny_state.action_epoch == action {
                    this.tiny_state.busy = false;
                }
            }
            match answer {
                Ok(Response::TinyCommunity { community }) => {
                    let confirm = this.tiny_state.confirm.clone();
                    if confirm.as_ref().is_some_and(|id| community.members.iter().all(|member| &member.user_id != id)) {
                        this.tiny_state.confirm = None;
                    }
                    if this.tiny_state.picked.as_ref().is_some_and(|id| community.members.iter().all(|member| &member.user_id != id)) {
                        this.tiny_state.picked = None;
                    }
                    // A room deleted from elsewhere takes its pane and its questions with it.
                    let gone = |id: &String| community.rooms.iter().all(|room| &room.id != id);
                    if this.tiny_state.room.as_ref().is_some_and(gone) {
                        this.tiny_state.room = None;
                        this.tiny_state.call_error = None;
                    }
                    if this.tiny_state.confirm_room.as_ref().is_some_and(gone) {
                        this.tiny_state.confirm_room = None;
                    }
                    if matches!(&this.tiny_state.naming, Some(Some(id)) if gone(id)) {
                        this.tiny_state.naming = None;
                    }
                    // A code that could not be packed stays on screen. A good load would hide it.
                    let unpacked = this.tiny_state.token.is_none()
                        && this.tiny_state.error.as_deref() == Some(UNMADE);
                    if !unpacked {
                        this.tiny_state.error = None;
                    }
                    this.tiny_state.community = Some(community);
                }
                Ok(Response::Error { detail, .. }) => this.tiny_state.error = Some(detail),
                Ok(_) => this.tiny_state.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                Err(err) => this.tiny_state.error = Some(crate::client::trouble_words(&err)),
            }
            cx.notify();
        });
    }

    fn act(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_state.busy = true;
        self.tiny_state.error = None;
        self.tiny_state.action_epoch += 1;
        let epoch = self.tiny_state.action_epoch;
        cx.notify();
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| this.acted(epoch, account, answer, cx));
    }

    fn acted(&mut self, epoch: u64, account: AccountKey, answer: io::Result<Response>, cx: &mut Context<Self>) {
        if self.tiny_state.action_epoch != epoch {
            return;
        }
        if self.tiny_state.account.as_ref() != Some(&account) {
            self.tiny_state.busy = false;
            cx.notify();
            return;
        }
        match answer {
            Ok(Response::Ok) => {
                self.tiny_state.confirm = None;
                self.tiny_state.confirm_room = None;
                self.tiny_state.naming = None;
                self.tiny_state.error = None;
                self.fetch(account, Some(epoch), cx);
                cx.notify();
            }
            Ok(Response::TinyCode { code }) => {
                match protocol::pack_join(&code.origin, &code.token) {
                    Some(paste) => {
                        self.tiny_state.token = Some(paste);
                        self.tiny_state.origin = Some(code.origin);
                        self.tiny_state.copied = false;
                        self.tiny_state.error = None;
                    }
                    None => {
                        self.tiny_state.token = None;
                        self.tiny_state.origin = None;
                        self.tiny_state.copied = false;
                        self.tiny_state.error = Some(UNMADE.to_owned());
                    }
                }
                self.fetch(account, Some(epoch), cx);
                cx.notify();
            }
            Ok(Response::Error { detail, .. }) => {
                self.tiny_state.busy = false;
                self.tiny_state.error = Some(detail);
                cx.notify();
            }
            Ok(_) => {
                self.tiny_state.busy = false;
                self.tiny_state.error = Some(crate::client::OTHER_ANSWER.to_owned());
                cx.notify();
            }
            Err(err) => {
                self.tiny_state.busy = false;
                self.tiny_state.error = Some(crate::client::trouble_words(&err));
                cx.notify();
            }
        }
    }

    fn send_invite(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let role = if self.tiny_state.invite_role == "admin" { "admin" } else { "member" };
        self.act(Request::TinyInvite { account, role: role.to_owned() }, cx);
    }

    fn pick_role(&mut self, role: &'static str, cx: &mut Context<Self>) {
        if self.tiny_state.busy || (role != "member" && role != "admin") {
            return;
        }
        self.tiny_state.invite_role = role.to_owned();
        cx.notify();
    }

    fn send_role(&mut self, user_id: String, role: &'static str, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.act(Request::TinyRole { account, user_id, role: role.to_owned() }, cx);
    }

    fn ask_remove(&mut self, user_id: String, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        self.tiny_state.confirm = Some(user_id);
        cx.notify();
    }

    fn cancel_remove(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.confirm = None;
        cx.notify();
    }

    fn send_remove(&mut self, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_state.confirm = None;
        self.act(Request::TinyRemove { account, user_id }, cx);
    }

    fn send_revoke(&mut self, invite_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.act(Request::TinyRevoke { account, invite_id }, cx);
    }

    fn copy_code(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.tiny_state.token.clone() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(token));
        self.tiny_state.copied = true;
        cx.notify();
    }

    fn done_code(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.token = None;
        self.tiny_state.origin = None;
        self.tiny_state.copied = false;
        cx.notify();
    }

    fn pick_invite(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.picked = None;
        self.leave_room_pane();
        cx.notify();
    }

    fn pick_person(&mut self, user_id: String, cx: &mut Context<Self>) {
        self.tiny_state.picked = Some(user_id);
        self.leave_room_pane();
        cx.notify();
    }

    fn pick_room(&mut self, room_id: String, cx: &mut Context<Self>) {
        if self.tiny_state.room.as_ref() != Some(&room_id) {
            self.leave_room_pane();
        }
        self.tiny_state.room = Some(room_id);
        cx.notify();
    }

    /// The room's questions and words go with its pane. The name typed is dropped too.
    fn leave_room_pane(&mut self) {
        self.tiny_state.confirm = None;
        self.tiny_state.room = None;
        self.tiny_state.naming = None;
        self.tiny_state.confirm_room = None;
        self.tiny_state.call_error = None;
    }

    /// Opens the name field: for a new room, or to rename `room`.
    fn name_room(&mut self, room: Option<(String, String)>, window: &mut Window, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        let (id, name) = match room {
            Some((id, name)) => (Some(id), name),
            None => {
                self.leave_room_pane();
                (None, String::new())
            }
        };
        self.tiny_state.error = None;
        self.tiny_state.confirm_room = None;
        self.tiny_state.naming = Some(id);
        self.tiny_state.room_name.update(cx, |input, cx| input.set_text(&name, cx));
        window.focus(&self.tiny_state.room_name.focus_handle(cx), cx);
        cx.notify();
    }

    fn cancel_naming(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.naming = None;
        self.tiny_state.error = None;
        cx.notify();
    }

    fn save_room(&mut self, cx: &mut Context<Self>) {
        let Some(naming) = self.tiny_state.naming.clone() else {
            return;
        };
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let typed = self.tiny_state.room_name.read(cx).text().to_owned();
        let Some(name) = protocol::tiny_name(&typed).map(str::to_owned) else {
            self.tiny_state.error = Some(ROOM_NAME_SIZE.to_owned());
            cx.notify();
            return;
        };
        let request = match naming {
            None => Request::TinyRoomCreate { account, name },
            Some(room_id) => Request::TinyRoomRename { account, room_id, name },
        };
        self.act(request, cx);
    }

    fn ask_delete_room(&mut self, room_id: String, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        self.tiny_state.naming = None;
        self.tiny_state.confirm_room = Some(room_id);
        cx.notify();
    }

    fn cancel_delete_room(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.confirm_room = None;
        cx.notify();
    }

    fn send_delete_room(&mut self, room_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.act(Request::TinyRoomDelete { account, room_id }, cx);
    }

    fn join_room(&mut self, room_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.call(Request::TinyCallJoin { account, room_id }, cx);
    }

    fn leave_call(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.call(Request::TinyCallLeave { account }, cx);
    }

    fn kick(&mut self, room_id: String, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.call(Request::TinyCallKick { account, room_id, user_id }, cx);
    }

    /// A call request. Nothing is read again on success: the seats arrive as `Occupants`.
    fn call(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.tiny_state.call_busy {
            return;
        }
        self.tiny_state.call_busy = true;
        self.tiny_state.call_error = None;
        self.tiny_state.call_epoch += 1;
        let epoch = self.tiny_state.call_epoch;
        cx.notify();
        self.off_thread(cx, move || ask(&request), move |this, answer, cx| {
            if this.tiny_state.call_epoch != epoch {
                return;
            }
            this.tiny_state.call_busy = false;
            this.tiny_state.call_error = match answer {
                Ok(Response::Ok) => None,
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => Some(crate::client::OTHER_ANSWER.to_owned()),
                Err(err) => Some(crate::client::trouble_words(&err)),
            };
            cx.notify();
        });
    }

    /// What chatd's live socket says about the server on show. Seats and presence change the
    /// page where it is; anything else is read again.
    pub(super) fn tiny_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        match change {
            Change::Tiny { account: changed } if *changed == account => {
                // A page not on show reads again when it is shown.
                if self.tiny_state.seen && self.tiny_state.community.is_some() {
                    self.fetch(account, None, cx);
                }
            }
            Change::Occupants { conversation, occupants } if conversation.account() == account => {
                let Some(room) = self.tiny_state.community.as_mut().and_then(|community| community.rooms.iter_mut().find(|room| room.id == conversation.id)) else {
                    return;
                };
                room.participants = occupants
                    .people
                    .iter()
                    .map(|occupant| protocol::TinySeat { user_id: occupant.person.id.clone(), muted: occupant.muted, deafened: occupant.deafened, speaking: occupant.speaking })
                    .collect();
                cx.notify();
            }
            Change::Presence { person, presence, .. } if person.account() == account => {
                let Some(community) = self.tiny_state.community.as_mut() else {
                    return;
                };
                let online = *presence == Presence::Online;
                let known = community.online.iter().position(|id| *id == person.id);
                match (online, known) {
                    (true, None) => community.online.push(person.id.clone()),
                    (false, Some(at)) => {
                        community.online.remove(at);
                    }
                    _ => return,
                }
                cx.notify();
            }
            _ => {}
        }
    }

    /// The sidebar's list: the invite, channels, direct messages, rooms, then the people. The pane
    /// draws the same list only when the sidebar is not beside it.
    pub(super) fn tiny_directory(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page() {
            Page::Loading => join_column(directory_note("Loading this server.", false), cx),
            Page::Failed(detail) => join_column(directory_note(&detail, true), cx),
            Page::Ready(ready) => {
                let shown = shown(&self.tiny_state, &ready);
                let chats = self.tiny_chats();
                directory(&ready, shown, &chats, self.open.as_ref(), false, cx)
            }
        }
    }

    /// Text channels and direct messages in this server's space, in the tree's order.
    fn tiny_chats(&self) -> Vec<Rc<Conversation>> {
        let Some(account) = self.tiny_state.account.as_ref() else {
            return Vec::new();
        };
        let Some(tree) = self.trees.get(&Key::new(account, "server")) else {
            return Vec::new();
        };
        tree.conversations
            .iter()
            .filter(|chat| matches!(chat.kind, ConversationKind::Channel | ConversationKind::Direct))
            .cloned()
            .collect()
    }
}

/// The room or the form over everything; then the person picked, or the invite.
fn shown<'a>(state: &'a State, ready: &'a Ready) -> Shown<'a> {
    if state.naming == Some(None) && ready.can_make_room {
        return Shown::NewRoom;
    }
    if let Some(room) = state.room.as_deref().filter(|id| ready.rooms.iter().any(|room| room.id == *id)) {
        return Shown::Room(room);
    }
    match shown_id(state.picked.as_deref(), ready.can_invite, &ready.members) {
        Some(id) => Shown::Person(id),
        None => Shown::Invite,
    }
}

fn note_line(words: &str, bad: bool) -> Div {
    let line = div().flex_1().px(px(theme::PANE_PAD)).pt(px(8.));
    if bad {
        line.type_small().text_color(theme::danger()).child(words.to_owned())
    } else {
        line.type_ui().text_color(theme::text_soft()).child(words.to_owned())
    }
}

/// Who the pane shows. None is the invite. Someone who cannot invite lands on the first person.
fn shown_id<'a>(picked: Option<&'a str>, can_invite: bool, members: &'a [Member]) -> Option<&'a str> {
    if let Some(id) = picked {
        if members.iter().any(|member| member.user_id == id) {
            return Some(id);
        }
    }
    if !can_invite {
        return members.first().map(|member| member.user_id.as_str());
    }
    None
}

fn ready_view(ready: &Ready, shown: Shown, me: Option<&str>, people_beside: bool, name: (&Entity<TextInput>, bool), chats: &[Rc<Conversation>], open: Option<&Key>, cx: &mut Context<Shell>) -> AnyElement {
    let detail = match shown {
        Shown::Person(id) => match ready.members.iter().find(|member| member.user_id == id) {
            Some(member) => person_view(member, me == Some(member.user_id.as_str()), ready.confirm.as_deref(), cx),
            None => server_view(ready, cx),
        },
        Shown::Room(id) => match ready.rooms.iter().find(|room| room.id == id) {
            Some(room) => room_view(ready, room, name, cx),
            None => server_view(ready, cx),
        },
        Shown::NewRoom => new_room_view(ready, name, cx),
        Shown::Invite => server_view(ready, cx),
    };
    let column = div()
        .w_full()
        .pb(px(24.))
        .flex()
        .flex_col()
        .when(!people_beside, |column| column.child(directory(ready, shown, chats, open, true, cx)))
        .child(detail);
    div().id("tiny-page").flex_1().min_h_0().overflow_y_scroll().child(column).into_any_element()
}

fn directory_note(words: &str, bad: bool) -> AnyElement {
    let line = div().px(px(theme::PANE_PAD)).pt(px(12.));
    let line = if bad { line.type_small().text_color(theme::danger()) } else { line.type_ui().text_color(theme::text_soft()) };
    line.child(words.to_owned()).into_any_element()
}

fn join_row(cx: &mut Context<Shell>) -> gpui::Stateful<Div> {
    let content = div()
        .gap(px(8.))
        .child(glyph("icons/log-in.svg", 16., theme::text_soft()))
        .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child("Join a server"));
    list_row("tiny-join-row", theme::row_height(theme::ROW_ONE), false, false, content)
        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.sign_in(Network::Tiny, cx)))
}

fn join_column(note: AnyElement, cx: &mut Context<Shell>) -> AnyElement {
    div()
        .id("tiny-people")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(join_row(cx))
        .child(note)
        .into_any_element()
}

fn directory(ready: &Ready, shown: Shown, chats: &[Rc<Conversation>], open: Option<&Key>, in_pane: bool, cx: &mut Context<Shell>) -> AnyElement {
    let mut column = div().id(if in_pane { "tiny-people-pane" } else { "tiny-people" }).w_full().when(!in_pane, |column| column.flex_1().min_h_0().overflow_y_scroll()).flex().flex_col();
    if ready.can_invite {
        let selected = shown == Shown::Invite;
        let content = div()
            .gap(px(8.))
            .child(glyph("icons/plus.svg", 16., if selected { theme::text() } else { theme::text_soft() }))
            .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child("Invite someone"));
        column = column.child(
            list_row("tiny-invite-row", theme::row_height(theme::ROW_ONE), selected, false, content)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_invite(cx))),
        );
    }
    column = column.child(join_row(cx));
    column = chat_section(column, "Channels", chats, ConversationKind::Channel, open, cx);
    column = chat_section(column, "Direct messages", chats, ConversationKind::Direct, open, cx);
    if !ready.rooms.is_empty() || ready.can_make_room {
        column = column.child(section_header("Rooms", false));
        for (ix, room) in ready.rooms.iter().enumerate() {
            column = column.child(room_row(ix, room, shown == Shown::Room(&room.id), cx));
            for who in &room.people {
                column = column.child(seat_row(&room.id, who));
            }
        }
        if ready.can_make_room {
            let selected = shown == Shown::NewRoom;
            let content = div()
                .gap(px(8.))
                .child(div().size(px(20.)).flex_none().flex().items_center().justify_center().child(glyph("icons/plus.svg", 16., if selected { theme::text() } else { theme::text_faint() })))
                .child(div().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child("New room"));
            column = column.child(
                list_row("tiny-new-room-row", theme::row_height(theme::ROW_ONE), selected, false, content)
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.name_room(None, window, cx))),
            );
        }
    }
    if !ready.members.is_empty() {
        column = column.child(section_header("People", false));
        for (ix, member) in ready.members.iter().enumerate() {
            let selected = shown == Shown::Person(member.user_id.as_str());
            let key = Key { network: Network::Tiny, account: member.user_id.clone(), id: member.user_id.clone() };
            let mut avatar = Avatar::new(&key, &member.name, theme::row_avatar());
            // Presence only while the live socket is up: otherwise it is not known.
            if ready.live {
                let presence = if ready.online.contains(&member.user_id) { Presence::Online } else { Presence::Offline };
                avatar = avatar.presence(presence, theme::row_surface(selected));
            }
            let content = div()
                .gap(px(10.))
                .child(avatar.build())
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(member.name.clone()))
                        .child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(member.role.clone())),
                );
            let user_id = member.user_id.clone();
            column = column.child(
                list_row(("tiny-person", ix), theme::row_height(theme::ROW_TWO), selected, false, content)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.pick_person(user_id.clone(), cx))),
            );
        }
    }
    if in_pane {
        return column.into_any_element();
    }
    // The sidebar keeps the seat in sight, as the call dock does for a voice channel. A call
    // with audio is the dock's to show.
    let seated = ready.rooms.iter().find(|room| room.mine && ready.docked.as_deref() != Some(room.id.as_str()));
    div().size_full().flex().flex_col().child(column).children(seated.map(|room| seat_strip(room, ready.call_busy, cx))).into_any_element()
}

/// Channels, or direct messages, after Join and before Rooms. The header is omitted when empty.
fn chat_section(mut column: gpui::Stateful<Div>, label: &str, chats: &[Rc<Conversation>], kind: ConversationKind, open: Option<&Key>, cx: &mut Context<Shell>) -> gpui::Stateful<Div> {
    let rows: Vec<&Rc<Conversation>> = chats.iter().filter(|chat| chat.kind == kind).collect();
    if rows.is_empty() {
        return column;
    }
    column = column.child(section_header(label, false));
    for chat in rows {
        column = column.child(chat_row(chat, open, cx));
    }
    column
}

/// A text channel or a direct message. Picking it opens the conversation pane.
fn chat_row(chat: &Conversation, open: Option<&Key>, cx: &mut Context<Shell>) -> gpui::Stateful<Div> {
    let selected = open.is_some_and(|open| open == &chat.key);
    let icon = if chat.kind == ConversationKind::Direct { "icons/user.svg" } else { "icons/hash.svg" };
    let content = div()
        .gap(px(8.))
        .child(glyph(icon, 16., if selected { theme::text() } else { theme::text_soft() }))
        .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(chat.title.clone()));
    let key = chat.key.clone();
    let title = chat.title.clone();
    list_row(gpui::ElementId::from(format!("tiny-chat-{}", key.id)), theme::row_height(theme::ROW_ONE), selected, false, content)
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.select(key.clone(), &title, cx)))
}

/// A room, as a voice channel's row: 28, its glyph, its name, then how many are in it.
fn room_row(ix: usize, room: &Room, selected: bool, cx: &mut Context<Shell>) -> gpui::Stateful<Div> {
    let empty = room.people.is_empty();
    // An empty room rests at a muted row's weight, as an empty voice channel does.
    let tone = if empty && !selected { theme::text_faint() } else { theme::text() };
    let content = div()
        .gap(px(8.))
        .child(div().size(px(20.)).flex_none().flex().items_center().justify_center().child(glyph("icons/volume-2.svg", 16., if empty { theme::text_faint() } else { theme::text_soft() })))
        .child(div().min_w_0().truncate().type_ui().text_color(tone).child(room.name.clone()))
        .child(div().flex_1())
        .when(!empty, |row| {
            row.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(glyph("icons/volume-2.svg", 12., theme::text_faint()))
                    .child(div().type_micro().text_color(theme::text_faint()).child(room.people.len().to_string())),
            )
        });
    let room_id = room.id.clone();
    list_row(("tiny-room", ix), theme::row_height(theme::ROW_ONE), selected, false, content)
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.pick_room(room_id.clone(), cx)))
}

/// Someone in a room, under it, as a voice channel's occupant.
fn seat_row(room_id: &str, who: &Seated) -> gpui::Stateful<Div> {
    let key = Key { network: Network::Tiny, account: who.user_id.clone(), id: who.user_id.clone() };
    let content = div()
        .child(div().flex_none().w(px(8.)))
        .gap(px(8.))
        .child(super::call::ringed(Avatar::new(&key, &who.name, 20.).build(), 20., who.speaking))
        .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child(who.name.clone()))
        .children(quiet_glyph(who));
    list_row(gpui::ElementId::from(format!("tiny-seat-{room_id}-{}", who.user_id)), theme::row_height(theme::ROW_ONE), false, false, content).cursor_default()
}

/// Muted or deafened, as an occupant shows it. Deafened says both.
fn quiet_glyph(who: &Seated) -> Option<gpui::Svg> {
    if who.deafened {
        Some(glyph("icons/headphone-off.svg", 14., theme::danger()))
    } else if who.muted {
        Some(glyph("icons/mic-off.svg", 14., theme::danger()))
    } else {
        None
    }
}

/// Where I am sitting, at the foot of the sidebar, with Leave.
fn seat_strip(room: &Room, busy: bool, cx: &mut Context<Shell>) -> Div {
    let leave = if busy {
        button_disabled("Leave").into_any_element()
    } else {
        button("tiny-strip-leave", ButtonKind::Secondary, "Leave").on_click(cx.listener(|this, _, _, cx| this.leave_call(cx))).into_any_element()
    };
    let room_id = room.id.clone();
    div()
        .flex_none()
        .border_t_1()
        .border_color(theme::border())
        .px(px(theme::PANE_PAD))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(glyph("icons/volume-x.svg", 16., theme::text_soft()))
        .child(
            div()
                .id("tiny-strip-room")
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.pick_room(room_id.clone(), cx)))
                .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(room.name.clone()))
                .child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child("Seated, no sound yet")),
        )
        .child(leave)
}

/// One room: who is in it, Join or Leave, and for an owner or an admin, rename and delete.
fn room_view(ready: &Ready, room: &Room, name: (&Entity<TextInput>, bool), cx: &mut Context<Shell>) -> AnyElement {
    let renaming = matches!(&ready.naming, Some(Some(id)) if *id == room.id);
    let confirming = ready.confirm_room.as_deref() == Some(room.id.as_str());
    let count = match room.people.len() {
        0 => "Nobody is in this room.".to_owned(),
        1 => "1 person is in this room.".to_owned(),
        n => format!("{n} people are in this room."),
    };
    let join = if room.mine {
        let leave = if ready.call_busy {
            button_disabled("Leave").into_any_element()
        } else {
            button("tiny-leave", ButtonKind::Secondary, "Leave").on_click(cx.listener(|this, _, _, cx| this.leave_call(cx))).into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .p(px(16.))
                    .rounded(px(theme::R_CARD))
                    .bg(theme::raised())
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(glyph(if ready.audio { "icons/headphones.svg" } else { "icons/volume-x.svg" }, 16., theme::text_soft()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().type_ui().text_color(theme::text()).child(if ready.audio { "You're in this room." } else { "You're in this room, with no sound." }))
                            .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(if ready.audio { HEADPHONES } else { NO_SOUND })),
                    ),
            )
            .child(div().flex().child(leave))
            .into_any_element()
    } else {
        let blocked = ready.join_blocked;
        let button = match blocked {
            Some(_) => button_disabled("Join").into_any_element(),
            None if ready.call_busy => button_disabled("Join").into_any_element(),
            None => {
                let room_id = room.id.clone();
                button("tiny-join", ButtonKind::Primary, "Join").on_click(cx.listener(move |this, _, _, cx| this.join_room(room_id.clone(), cx))).into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child(blocked.unwrap_or(if ready.audio { HEADPHONES } else { NO_SOUND })))
            .child(div().flex().child(button))
            .into_any_element()
    };
    let mut column = div()
        .px(px(theme::PANE_PAD))
        .pt(px(20.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .max_w(px(440.))
        .child(div().type_ui().text_color(theme::text()).child(count))
        .child(join)
        .children(ready.call_error.as_ref().map(|detail| div().whitespace_normal().type_small().text_color(theme::danger()).child(detail.clone())))
        .children(ready.error.as_ref().map(|detail| div().whitespace_normal().type_small().text_color(theme::danger()).child(detail.clone())));
    let manage = ready.can_rename_room || ready.can_delete_room;
    if renaming {
        column = column.child(name_form(name, "Save", ready.busy, cx));
    } else if confirming {
        let room_id = room.id.clone();
        column = column.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(div().whitespace_normal().type_ui().text_color(theme::text()).child(format!("Delete {}? Anyone in it leaves the call.", room.name)))
                .child(button("tiny-room-delete-yes", ButtonKind::Danger, "Delete").on_click(cx.listener(move |this, _, _, cx| this.send_delete_room(room_id.clone(), cx))))
                .child(button("tiny-room-delete-no", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_delete_room(cx)))),
        );
    } else if manage && !ready.busy {
        let mut actions = div().flex().flex_wrap().gap(px(4.));
        if ready.can_rename_room {
            let (id, now) = (room.id.clone(), room.name.clone());
            actions = actions.child(
                button("tiny-room-rename", ButtonKind::Secondary, "Rename…").on_click(cx.listener(move |this, _, window, cx| this.name_room(Some((id.clone(), now.clone())), window, cx))),
            );
        }
        if ready.can_delete_room {
            let id = room.id.clone();
            actions = actions.child(button("tiny-room-delete", ButtonKind::Danger, "Delete room").on_click(cx.listener(move |this, _, _, cx| this.ask_delete_room(id.clone(), cx))));
        }
        column = column.child(actions);
    }
    let mut page = div().flex().flex_col().child(column);
    if !room.people.is_empty() {
        let room_id = room.id.clone();
        page = page.child(div().mt(px(8.)).flex().flex_col().child(section_header("In this room", false)).children(room.people.iter().map(|who| {
            let key = Key { network: Network::Tiny, account: who.user_id.clone(), id: who.user_id.clone() };
            let mine = ready.me.as_deref() == Some(who.user_id.as_str());
            let (room_id, user_id) = (room_id.clone(), who.user_id.clone());
            div()
                .px(px(theme::PANE_PAD))
                .h(px(theme::ROW_ONE))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(super::call::ringed(Avatar::new(&key, &who.name, 20.).build(), 20., who.speaking))
                .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(who.name.clone()))
                .when(mine, |row| row.child(div().type_small().text_color(theme::text_faint()).child("You")))
                .children(quiet_glyph(who))
                .child(div().flex_1())
                .when(who.kick && !ready.call_busy, |row| {
                    row.child(
                        button(gpui::ElementId::from(format!("tiny-kick-{user_id}")), ButtonKind::Ghost, "Remove from call")
                            .on_click(cx.listener(move |this, _, _, cx| this.kick(room_id.clone(), user_id.clone(), cx))),
                    )
                })
        })));
    }
    page.into_any_element()
}

fn new_room_view(ready: &Ready, name: (&Entity<TextInput>, bool), cx: &mut Context<Shell>) -> AnyElement {
    div()
        .px(px(theme::PANE_PAD))
        .pt(px(20.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .max_w(px(440.))
        .child(div().type_title().text_color(theme::text()).child("New room"))
        .child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child("A room is a voice channel. People join and leave it as they like."))
        .children(ready.error.as_ref().map(|detail| div().whitespace_normal().type_small().text_color(theme::danger()).child(detail.clone())))
        .child(name_form(name, "Create", ready.busy, cx))
        .into_any_element()
}

/// The room name field, its button, and Cancel.
fn name_form(name: (&Entity<TextInput>, bool), save: &'static str, busy: bool, cx: &mut Context<Shell>) -> Div {
    let (input, focused) = name;
    let save = if busy {
        button_disabled(save).into_any_element()
    } else {
        button("tiny-room-save", ButtonKind::Primary, save).on_click(cx.listener(|this, _, _, cx| this.save_room(cx))).into_any_element()
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(field(focused).w(px(220.)).child(input.clone()))
        .child(save)
        .child(button("tiny-room-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_naming(cx))))
}

fn server_view(ready: &Ready, cx: &mut Context<Shell>) -> AnyElement {
    if !ready.can_invite {
        return note_line("Nobody is on this server.", false).into_any_element();
    }
    let told = ready.origin.as_deref().filter(|origin| !origin.is_empty()).unwrap_or(ready.home.as_str());
    let head = div()
        .px(px(theme::PANE_PAD))
        .pt(px(20.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .max_w(px(440.))
        .children(ready.error.as_ref().map(|detail| div().whitespace_normal().type_small().text_color(theme::danger()).child(detail.clone())))
        .child(match ready.token.as_deref() {
            Some(paste) => handoff(paste, told, &ready.invite_role, ready.copied, cx).into_any_element(),
            None => invite_form(told, &ready.invite_role, ready.busy, cx).into_any_element(),
        });
    let mut column = div().flex().flex_col().child(head);
    if !ready.invites.is_empty() {
        column = column.child(invite_section(&ready.invites, ready.can_revoke && !ready.busy, cx));
    }
    if let Some(rows) = &ready.audit {
        if !rows.is_empty() {
            column = column.child(audit_section(rows));
        }
    }
    column.into_any_element()
}

fn person_view(member: &Member, mine: bool, confirm: Option<&str>, cx: &mut Context<Shell>) -> AnyElement {
    let confirming = confirm == Some(member.user_id.as_str());
    let actions = member.make_admin || member.make_member || member.make_owner || member.remove;
    let key = Key { network: Network::Tiny, account: member.user_id.clone(), id: member.user_id.clone() };
    div()
        .px(px(theme::PANE_PAD))
        .pt(px(24.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .child(Avatar::new(&key, &member.name, 64.).build())
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(member.name.clone()))
                        .child(div().type_ui().text_color(theme::text_soft()).child(member.role.clone()))
                        .when(!member.email.is_empty(), |column| {
                            column.child(div().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(member.email.clone()))
                        }),
                ),
        )
        .when(mine && member.role == "Owner" && !actions, |column| {
            column.child(div().max_w(px(420.)).whitespace_normal().type_ui().text_color(theme::text_soft()).child("You're the owner of this server."))
        })
        .when(!confirming && actions, |column| column.child(member_actions(member, cx)))
        .when(confirming, |column| column.child(confirm_remove(&member.name, &member.user_id, cx)))
        .into_any_element()
}

fn invite_form(origin: &str, role: &str, busy: bool, cx: &mut Context<Shell>) -> Div {
    let host = protocol::join_host(origin);
    div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(div().type_title().text_color(theme::text()).child("Invite someone"))
        .child(
            div()
                .whitespace_normal()
                .type_ui()
                .text_color(theme::text_soft())
                .child("They paste one code into their own tinyclient. The code has this server in it, so they never type an address. Then they sign in through their browser, with the account this server uses."),
        )
        .when(!host.is_empty(), |column| {
            column.child(div().whitespace_normal().type_ui().text_color(theme::text()).child(format!("It will open {host}.")))
        })
        .child(div().type_small().text_color(theme::text_soft()).child("They join as"))
        .child(invite_row(role, busy, cx))
}

fn handoff(paste: &str, origin: &str, role: &str, copied: bool, cx: &mut Context<Shell>) -> Div {
    let (opens, makes) = handoff_facts(origin, role);
    div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(div().type_title().text_color(theme::text()).child("Give them this code"))
        .child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child("Copy it and send it to them. They do the rest in their own tinyclient."))
        .child(
            div()
                .p(px(16.))
                .rounded(px(theme::R_CARD))
                .bg(theme::raised())
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(handoff_step(1, "Open tinyclient"))
                .child(handoff_step(2, "Choose Tiny server, or Join a server"))
                .child(handoff_step(3, "Paste this code"))
                .child(handoff_step(4, "Continue in the browser that opens")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().type_micro().text_color(theme::text_faint()).child("Code"))
                .child(
                    div()
                        .w_full()
                        .px(px(16.))
                        .py(px(12.))
                        .rounded(px(theme::R_CARD))
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::border())
                        .whitespace_normal()
                        .type_code()
                        .text_color(theme::text())
                        .child(paste.to_owned()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().whitespace_normal().type_ui().text_color(theme::text()).child(opens))
                .child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child(makes))
                .child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child("It works once, for 7 days, and will not be shown again.")),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(
                    button("tiny-copy", ButtonKind::Primary, if copied { "Copied" } else { "Copy code" })
                        .on_click(cx.listener(|this, _, _, cx| this.copy_code(cx))),
                )
                .child(button("tiny-done", ButtonKind::Secondary, "Done").on_click(cx.listener(|this, _, _, cx| this.done_code(cx)))),
        )
}

fn handoff_step(number: usize, words: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex_none()
                .size(px(18.))
                .rounded(px(9.))
                .bg(theme::selected())
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .line_height(px(14.))
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::text())
                .child(number.to_string()),
        )
        .child(div().type_ui().text_color(theme::text()).child(words))
}

/// Where the code opens, and who it makes. The host is plain: they do not type it.
fn handoff_facts(origin: &str, role: &str) -> (String, String) {
    let host = protocol::join_host(origin);
    let opens = if host.is_empty() {
        "It opens this server. They don't type that.".to_owned()
    } else {
        format!("It opens {host}. They don't type that.")
    };
    let place = role_place(role);
    let makes = if place.is_empty() {
        "This code makes them a member.".to_owned()
    } else {
        format!("This code makes them {place}.")
    };
    (opens, makes)
}

fn invite_row(role: &str, busy: bool, cx: &mut Context<Shell>) -> Div {
    let invite = if busy {
        button_disabled("Invite someone").into_any_element()
    } else {
        button("tiny-invite", ButtonKind::Primary, "Invite someone")
            .on_click(cx.listener(|this, _, _, cx| this.send_invite(cx)))
            .into_any_element()
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(
            segmented()
                .child(segment("tiny-as-member", "Member", role != "admin").on_click(cx.listener(|this, _, _, cx| this.pick_role("member", cx))))
                .child(segment("tiny-as-admin", "Admin", role == "admin").on_click(cx.listener(|this, _, _, cx| this.pick_role("admin", cx)))),
        )
        .child(invite)
}

fn member_actions(member: &Member, cx: &mut Context<Shell>) -> Div {
    let mut actions = div().flex_none().flex().flex_wrap().items_center().gap(px(4.));
    if member.make_admin {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-admin-{}", member.user_id), ButtonKind::Secondary, "Make admin")
                .on_click(cx.listener(move |this, _, _, cx| this.send_role(user_id.clone(), "admin", cx))),
        );
    }
    if member.make_member {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-member-{}", member.user_id), ButtonKind::Secondary, "Make member")
                .on_click(cx.listener(move |this, _, _, cx| this.send_role(user_id.clone(), "member", cx))),
        );
    }
    if member.make_owner {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-owner-{}", member.user_id), ButtonKind::Secondary, "Make owner")
                .on_click(cx.listener(move |this, _, _, cx| this.send_role(user_id.clone(), "owner", cx))),
        );
    }
    if member.remove {
        let user_id = member.user_id.clone();
        actions = actions.child(
            button(format!("tiny-remove-{}", member.user_id), ButtonKind::Danger, "Remove")
                .on_click(cx.listener(move |this, _, _, cx| this.ask_remove(user_id.clone(), cx))),
        );
    }
    actions
}

fn confirm_remove(name: &str, user_id: &str, cx: &mut Context<Shell>) -> Div {
    let remove_id = user_id.to_owned();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(div().whitespace_normal().type_ui().text_color(theme::text()).child(format!("Remove {name} from this server? They are signed out at once.")))
        .child(
            button(format!("tiny-remove-yes-{user_id}"), ButtonKind::Danger, "Remove")
                .on_click(cx.listener(move |this, _, _, cx| this.send_remove(remove_id.clone(), cx))),
        )
        .child(button(format!("tiny-remove-no-{user_id}"), ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_remove(cx))))
}

/// The invites nobody has used. Revoke is there for whoever may revoke, and quiet while busy.
fn invite_section(invites: &[OpenInvite], revoke: bool, cx: &mut Context<Shell>) -> Div {
    div().mt(px(8.)).flex().flex_col().child(section_header("Invites", false)).children(invites.iter().map(|invite| {
        let id = invite.id.clone();
        div()
            .px(px(theme::PANE_PAD))
            .h(px(theme::ROW_ONE))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().type_ui().text_color(theme::text()).child(invite.role.clone()))
            .child(div().flex_1().type_small().text_color(theme::text_soft()).child(format!("Expires {}", short_day(&invite.expires))))
            .when(revoke, |row| {
                row.child(
                    button(format!("tiny-revoke-{}", invite.id), ButtonKind::Ghost, "Revoke")
                        .on_click(cx.listener(move |this, _, _, cx| this.send_revoke(id.clone(), cx))),
                )
            })
    }))
}

fn audit_section(rows: &[String]) -> Div {
    div().mt(px(8.)).flex().flex_col().child(section_header("Activity", false)).children(rows.iter().map(|row| {
        div()
            .px(px(theme::PANE_PAD))
            .min_h(px(theme::ROW_ONE))
            .flex()
            .items_center()
            .min_w_0()
            .whitespace_normal()
            .type_ui()
            .text_color(theme::text_soft())
            .child(row.clone())
    }))
}

/// A day from `YYYY-MM-DD…`. Anything else is left as the server sent it.
fn short_day(stamp: &str) -> String {
    let bytes = stamp.as_bytes();
    if bytes.len() >= 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        let year = &stamp[0..4];
        let month: usize = stamp[5..7].parse().unwrap_or(0);
        let day: usize = stamp[8..10].parse().unwrap_or(0);
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return format!("{} {day}, {year}", MONTHS[month - 1]);
        }
    }
    stamp.to_owned()
}

fn role_place(role: &str) -> &'static str {
    if same_role(role, "admin") {
        "an admin"
    } else if same_role(role, "member") {
        "a member"
    } else if same_role(role, "owner") {
        "an owner"
    } else {
        ""
    }
}

/// What one audit row says. Machine actions stay off the screen.
fn audit_line<'a>(action: &str, detail: &str, name_of: impl Fn(&str) -> Option<&'a str>) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(detail).unwrap_or(serde_json::Value::Null);
    let role = parsed.get("role").and_then(|value| value.as_str()).unwrap_or("");
    let user_id = parsed.get("user_id").and_then(|value| value.as_str()).unwrap_or("");
    let who = name_of(user_id).filter(|name| !name.is_empty());
    let place = role_place(role);
    Some(match action {
        "bootstrap" => "This server was created.".to_owned(),
        "invite.create" => match place {
            "an admin" => "An invite for an admin was created.".to_owned(),
            "a member" => "An invite for a member was created.".to_owned(),
            "an owner" => "An invite for an owner was created.".to_owned(),
            _ => "An invite was created.".to_owned(),
        },
        "invite.revoke" => "An invite was revoked.".to_owned(),
        "invite.accept" => match place {
            "an admin" => "An admin joined.".to_owned(),
            "a member" => "A member joined.".to_owned(),
            "an owner" => "An owner joined.".to_owned(),
            _ => "Someone joined.".to_owned(),
        },
        "member.role" => match (who, place) {
            (Some(name), place) if !place.is_empty() => format!("{name} is now {place}."),
            (None, place) if !place.is_empty() => format!("Someone is now {place}."),
            _ => "A role changed.".to_owned(),
        },
        "member.remove" => match who {
            Some(name) => format!("{name} was removed."),
            None => "Someone was removed.".to_owned(),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_buttons_follow_who_may_change_whom() {
        let owners = 1;
        assert!(role_change_legal("owner", "member", "admin", owners));
        assert!(role_change_legal("owner", "member", "owner", owners));
        assert!(role_change_legal("admin", "member", "admin", owners));
        assert!(!role_change_legal("admin", "member", "owner", owners));
        assert!(!role_change_legal("admin", "admin", "member", owners));
        assert!(!role_change_legal("admin", "owner", "admin", owners));
        assert!(!role_change_legal("admin", "owner", "member", owners));
        assert!(!role_change_legal("owner", "owner", "admin", owners));
        assert!(!role_change_legal("owner", "owner", "member", owners));
        assert!(!role_change_legal("member", "member", "admin", owners));
        assert!(role_change_legal("owner", "owner", "admin", 2));
        assert!(role_change_legal("owner", "admin", "member", owners));
        assert!(!role_change_legal("owner", "admin", "admin", owners));
    }

    #[test]
    fn remove_spares_the_only_owner_and_an_admin_does_not_touch_one() {
        assert!(!remove_legal("owner", "owner", 1));
        assert!(remove_legal("owner", "owner", 2));
        assert!(remove_legal("owner", "admin", 1));
        assert!(remove_legal("owner", "member", 1));
        assert!(remove_legal("admin", "member", 1));
        assert!(!remove_legal("admin", "admin", 1));
        assert!(!remove_legal("admin", "owner", 2));
        assert!(!remove_legal("member", "member", 1));
    }

    fn one(id: &str) -> Member {
        Member {
            user_id: id.into(),
            name: id.into(),
            email: String::new(),
            role: "Member".into(),
            make_admin: false,
            make_member: false,
            make_owner: false,
            remove: false,
        }
    }

    fn tiny_key(account: &str) -> AccountKey {
        AccountKey { network: Network::Tiny, account: account.to_owned() }
    }

    #[test]
    fn the_open_server_stays_and_a_new_one_is_reachable() {
        let first = tiny_key("one");
        let second = tiny_key("two");
        let accounts = [(first.clone(), Link::Connected), (second.clone(), Link::Connected)];
        assert_eq!(preferred_tiny(&accounts, None).as_ref(), Some(&first));
        assert_eq!(preferred_tiny(&accounts, Some(&second)).as_ref(), Some(&second));
        let offline = [(first.clone(), Link::Offline), (second.clone(), Link::Connected)];
        assert_eq!(preferred_tiny(&offline, Some(&first)).as_ref(), Some(&first));
        assert_eq!(preferred_tiny(&offline, None).as_ref(), Some(&second));
        let gone = [(second.clone(), Link::Connected)];
        assert_eq!(preferred_tiny(&gone, Some(&first)).as_ref(), Some(&second));
        assert_eq!(preferred_tiny(&[(first.clone(), Link::SignedOut)], Some(&first)), None);
    }

    #[test]
    fn the_invite_is_on_show_until_a_person_is_picked_and_a_member_lands_on_someone() {
        let people = [one("ada"), one("bo")];
        assert_eq!(shown_id(None, true, &people), None);
        assert_eq!(shown_id(Some("bo"), true, &people), Some("bo"));
        assert_eq!(shown_id(Some("gone"), true, &people), None);
        assert_eq!(shown_id(None, false, &people), Some("ada"));
        assert_eq!(shown_id(Some("gone"), false, &people), Some("ada"));
    }

    #[test]
    fn an_audit_row_is_a_sentence_and_a_stamp_is_a_day() {
        assert_eq!(audit_line("bootstrap", "", |_| None).as_deref(), Some("This server was created."));
        assert_eq!(audit_line("invite.create", r#"{"role":"admin"}"#, |_| None).as_deref(), Some("An invite for an admin was created."));
        assert_eq!(audit_line("invite.create", r#"{"role":"member"}"#, |_| None).as_deref(), Some("An invite for a member was created."));
        assert_eq!(audit_line("invite.revoke", "{}", |_| None).as_deref(), Some("An invite was revoked."));
        assert_eq!(audit_line("invite.accept", r#"{"role":"member"}"#, |_| None).as_deref(), Some("A member joined."));
        assert_eq!(
            audit_line("member.role", r#"{"user_id":"a","role":"admin"}"#, |id| (id == "a").then_some("Ada")).as_deref(),
            Some("Ada is now an admin.")
        );
        assert_eq!(
            audit_line("member.role", r#"{"user_id":"a","role":"member"}"#, |id| (id == "a").then_some("Ada")).as_deref(),
            Some("Ada is now a member.")
        );
        assert_eq!(audit_line("member.remove", r#"{"user_id":"gone"}"#, |_| None).as_deref(), Some("Someone was removed."));
        assert_eq!(audit_line("member.remove", r#"{"user_id":"a"}"#, |id| (id == "a").then_some("Ada")).as_deref(), Some("Ada was removed."));
        assert_eq!(audit_line("nope", "membership", |_| None), None);
        assert_eq!(short_day("2026-11-01T00:00:00Z"), "Nov 1, 2026");
        assert_eq!(short_day("soon"), "soon");
    }

    #[test]
    fn remove_from_call_follows_the_servers_rule_and_join_says_what_stands_in_its_way() {
        assert!(can_kick("owner", "owner", false));
        assert!(can_kick("owner", "member", false));
        assert!(can_kick("admin", "admin", false));
        assert!(!can_kick("admin", "owner", false), "an admin may not remove an owner from a call");
        assert!(!can_kick("member", "member", false));
        assert!(!can_kick("owner", "owner", true), "Leave is for yourself");
        assert_eq!(join_blocked(true, "off"), None);
        assert_eq!(join_blocked(true, ""), None, "an older server has no audio to ask for");
        assert_eq!(join_blocked(true, "up"), None, "a server with audio takes a join with audio");
        assert!(join_blocked(true, "down").is_some_and(|words| words.contains("down")));
        assert!(join_blocked(false, "off").is_some_and(|words| words.contains("connected")));
    }

    #[test]
    fn a_code_says_where_it_opens_and_who_it_makes() {
        let (opens, makes) = handoff_facts("https://private.tinyclient.io", "member");
        assert_eq!(opens, "It opens private.tinyclient.io. They don't type that.");
        assert_eq!(makes, "This code makes them a member.");
        let (opens, makes) = handoff_facts("http://127.0.0.1:8080", "admin");
        assert_eq!(opens, "It opens 127.0.0.1:8080. They don't type that.");
        assert_eq!(makes, "This code makes them an admin.");
        let (opens, makes) = handoff_facts("", "nope");
        assert_eq!(opens, "It opens this server. They don't type that.");
        assert_eq!(makes, "This code makes them a member.");
    }
}
