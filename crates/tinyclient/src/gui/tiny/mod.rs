//! A Tiny server, laid out as Discord lays out a server and with voice controls as TeamSpeak has
//! them (`docs/design/10-tiny-server.md`).
//!
//! - The sidebar is the server's column (`column`): a header with the server's name, icon and
//!   menu, the channel list in folding categories with who sits in each call, and the user
//!   panel at its foot (`panel`), under the call dock while a call is up.
//! - The pane is the server's home (`home`) with no channel picked, else the channel (`channel`):
//!   its header and a stage of tiles. The member list (`members`) sits beside it and toggles.
//! - Whatever floats is drawn by `tiny_layer`: the person, channel and header menus (`menus`),
//!   the modals (`modals`), the hover card, the connection popover, and Server Settings
//!   (`settings`), a full-window overlay.
//!
//! What this account may do decides what is drawn (`model`): a control the account cannot use is
//! absent, never disabled. On a server older than roles and capabilities, every check falls back
//! to the saved role and only what that server knows is offered. Changes arrive from chatd's live
//! socket (`Change::Tiny`, `Occupants`, `Presence`); nothing here polls, except the connection
//! numbers while a call is up.

mod channel;
pub(super) mod column;
mod home;
mod layer;
mod members;
mod menus;
mod modals;
pub(super) mod model;
mod pane;
mod panel;
pub(super) mod settings;

use super::input::{InputEvent, InputKey, TextInput};
use super::sidebar::Mode;
use super::state::Shell;
use crate::client::ask;
use gpui::{prelude::*, Bounds, ClipboardItem, Context, Entity, Focusable, Pixels, Point, Task, Window};
use protocol::{AccountKey, CallStats, Change, Key, Link, Network, Presence, Request, Response, TinyCommunity};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::rc::Rc;
use std::time::Duration;

/// Shown when the server's token cannot be wrapped with this server's address.
const UNMADE: &str = "The invite could not be made into a code.";
const ROOM_NAME_SIZE: &str = "A name is 1 to 80 characters.";
/// How often the connection's numbers are read while a call is up.
const STATS_EVERY: Duration = Duration::from_secs(1);

/// What floats over the window: one at a time.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Float {
    /// The server header's menu.
    Header,
    Person { user_id: String, at: Point<Pixels> },
    Channel { room_id: String, at: Point<Pixels> },
    /// A category's header; `key` is its id, or empty for the rooms in none.
    Category { key: String, at: Point<Pixels> },
    /// The connection's numbers, over the user panel.
    Stats,
}

/// A person menu's submenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sub {
    Move,
    Timeout,
    Roles,
}

/// The modal on show.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Modal {
    Invite,
    /// The code made, shown once: the paste, the server it opens, and its terms in words.
    Code { paste: String, origin: String, terms: String, role: String },
    Kick(String),
    Ban(String),
    DeleteRoom(String),
    DeleteCategory(String),
    /// A new channel, in this category.
    NewRoom(Option<String>),
    RenameRoom(String),
    NewCategory,
    Profile(String),
    SignOut,
    /// Server Settings' confirmations.
    DeleteRole(String),
    KickMany(Vec<String>),
    Unban(String),
    /// Every invite this account may revoke; `typed` asks for the server's name first.
    RevokeInvites {
        typed: bool,
    },
    /// Ownership to this member, the server's name typed first.
    Transfer(String),
    SignOutEverywhere,
}

/// The invite being made.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct InviteDraft {
    pub roles: Vec<String>,
    pub uses: Option<u32>,
    pub expiry: Option<u32>,
    /// On a server without invite options: `member` or `admin`.
    pub legacy_role: &'static str,
}

impl Default for InviteDraft {
    fn default() -> InviteDraft {
        InviteDraft { roles: Vec::new(), uses: Some(1), expiry: Some(604_800), legacy_role: "member" }
    }
}

/// Something dragged in the channel list.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Dragged {
    Person {
        user_id: String,
        name: String,
    },
    Room {
        room_id: String,
        name: String,
    },
    Category {
        category_id: String,
        name: String,
    },
    /// A role in Server Settings › Roles.
    Role {
        role_id: String,
        name: String,
    },
}

/// What the server page holds. An invite's token stays until Done and is never logged.
pub struct State {
    seen: bool,
    account: Option<AccountKey>,
    load_epoch: u64,
    action_epoch: u64,
    loading: bool,
    community: Option<TinyCommunity>,
    error: Option<String>,
    busy: bool,
    /// The action whose buttons stay quiet until a read started after it returns. Any such read
    /// will do: a live update's read can overtake the action's own and drop its answer.
    clears_busy: Option<u64>,
    /// A join or leave is with the server. It has its own quiet.
    call_busy: bool,
    call_epoch: u64,
    /// What the server said no to, under the channel.
    call_error: Option<String>,

    /// The channel on show. None is the server's home.
    channel: Option<String>,
    /// The person drawn large on the stage, pinned by a click on their tile.
    focus: Option<String>,
    /// Categories folded, by key ([`model::LOOSE`] for the rooms in none).
    folded: HashSet<String>,
    members_open: bool,

    float: Option<Float>,
    sub: Option<Sub>,
    /// Where the submenu's item was drawn, so the submenu opens beside it.
    sub_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    modal: Option<Modal>,
    modal_error: Option<String>,
    /// The member list's hover card: whose, and how far down the window the row is.
    card: Option<(String, f32)>,
    invite: InviteDraft,
    ban_days: Option<u32>,
    copied: bool,

    /// Each person's volume and mute for me as last set here, by server and user, until chatd's
    /// settings (which keep it, `CallGain`) are read again with it.
    gains: Gains,
    /// Mute and deafen chosen while not in a call, for the next one.
    preset: (bool, bool),
    /// The call the presets were last sent to.
    applied: Option<Key>,
    /// The volume slider's track, as last drawn, and whose volume is being dragged.
    slider: Rc<Cell<Option<Bounds<Pixels>>>>,
    sliding: Option<String>,

    stats: Option<CallStats>,
    /// Why there are no numbers, when mediad said.
    stats_note: Option<String>,
    stats_task: Option<Task<()>>,

    name_input: Entity<TextInput>,
    label_input: Entity<TextInput>,
    reason_input: Entity<TextInput>,
    search_input: Entity<TextInput>,

    /// Server Settings, while open.
    settings: Option<settings::Overlay>,
    settings_fields: settings::Fields,
}

impl State {
    pub(super) fn new(cx: &mut Context<Shell>) -> State {
        let name_input = cx.new(|cx| TextInput::new("Name", cx));
        let label_input = cx.new(|cx| TextInput::new("Who it's for (optional)", cx));
        let reason_input = cx.new(|cx| TextInput::new("Reason (optional, goes in the audit log)", cx));
        let search_input = cx.new(|cx| TextInput::new("Search members", cx));
        cx.subscribe(&name_input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.submit_name(cx),
            InputEvent::Changed => {
                this.tiny_state.modal_error = None;
                cx.notify();
            }
        })
        .detach();
        for input in [&name_input, &label_input, &reason_input] {
            cx.subscribe(input, |this, _, event: &InputKey, cx| {
                if *event == InputKey::Escape {
                    this.close_modal(cx);
                }
            })
            .detach();
        }
        cx.subscribe(&search_input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let settings_fields = settings::Fields::new(cx);
        State {
            seen: false,
            account: None,
            load_epoch: 0,
            action_epoch: 0,
            loading: false,
            community: None,
            error: None,
            busy: false,
            clears_busy: None,
            call_busy: false,
            call_epoch: 0,
            call_error: None,
            channel: None,
            focus: None,
            folded: HashSet::new(),
            members_open: true,
            float: None,
            sub: None,
            sub_anchor: Rc::default(),
            modal: None,
            modal_error: None,
            card: None,
            invite: InviteDraft::default(),
            ban_days: Some(7),
            copied: false,
            gains: HashMap::new(),
            preset: (false, false),
            applied: None,
            slider: Rc::default(),
            sliding: None,
            stats: None,
            stats_note: None,
            stats_task: None,
            name_input,
            label_input,
            reason_input,
            search_input,
            settings: None,
            settings_fields,
        }
    }

    /// Everything that belongs to one server's page, when another is shown.
    fn forget_page(&mut self) {
        self.community = None;
        self.error = None;
        self.channel = None;
        self.call_error = None;
        self.float = None;
        self.sub = None;
        self.modal = None;
        self.modal_error = None;
        self.card = None;
        self.settings = None;
    }

    /// The volume and mute for me kept for someone, else 100 % and heard: what was just set
    /// here, else what chatd keeps for this account.
    pub(super) fn gain(&self, user_id: &str) -> (u16, bool) {
        let Some(account) = self.account.as_ref() else {
            return (100, false);
        };
        if let Some(set) = self.gains.get(&(account.account.clone(), user_id.to_owned())) {
            return *set;
        }
        kept_gain(&crate::gui::app::prefs::values().gains(account), user_id)
    }

    pub(super) fn members_open(&self) -> bool {
        self.members_open
    }

    pub(super) fn set_members_open(&mut self, open: bool) {
        self.members_open = open;
    }
}

type Gains = HashMap<(String, String), (u16, bool)>;

/// Someone's volume and mute among what chatd keeps, else 100 % and heard.
fn kept_gain(kept: &[(String, u16, bool)], user_id: &str) -> (u16, bool) {
    kept.iter().find(|(id, _, _)| id == user_id).map_or((100, false), |(_, percent, muted)| (*percent, *muted))
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

/// Why Join cannot be pressed now.
fn join_blocked(live: bool, media: &str) -> Option<&'static str> {
    match media {
        _ if !live => Some("Tiny server isn't connected. Join once it is."),
        "down" => Some("This server's audio is down. Try again later."),
        _ => None,
    }
}

/// The key of a Tiny person, for their avatar's tint and picture.
fn person_key(account: &AccountKey, user_id: &str) -> Key {
    Key { network: Network::Tiny, account: account.account.clone(), id: user_id.to_owned() }
}

/// The page as the panes use it: the community, and the facts every pane asks.
pub(super) struct Page<'a> {
    pub community: &'a TinyCommunity,
    pub account: &'a AccountKey,
    pub may: model::May,
    /// The live socket is up: presence and seats are current.
    pub live: bool,
    pub me: &'a str,
    /// The room whose call I am in, if any.
    pub mine: Option<&'a str>,
    /// The server's audio is up: a join is a call with sound.
    pub audio: bool,
    pub name: String,
}

impl Page<'_> {
    pub fn online(&self, user_id: &str) -> Option<Presence> {
        self.live.then(|| if self.community.online.iter().any(|id| id == user_id) { Presence::Online } else { Presence::Offline })
    }

    pub fn key(&self, user_id: &str) -> Key {
        person_key(self.account, user_id)
    }

    pub fn icon(&self) -> Option<std::path::PathBuf> {
        self.community.server.as_ref().map(|server| server.icon_path.clone()).filter(|path| !path.is_empty()).map(std::path::PathBuf::from)
    }
}

impl Shell {
    /// Loads the server when the window is actually showing it, and again the next time it is.
    /// Each frame, too: a Tiny call that just began gets its volumes and presets, and the
    /// connection's numbers are read while one is up.
    pub(super) fn note_tiny_page(&mut self, cx: &mut Context<Self>) {
        self.note_tiny_call(cx);
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
    pub(in crate::gui) fn prefer_tiny(&mut self, account: AccountKey, cx: &mut Context<Self>) {
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

    /// The name of the Tiny room `key` names, and its server's, for the call dock.
    pub(super) fn tiny_room(&self, key: &Key) -> Option<(String, String)> {
        let community = self.tiny_state.community.as_ref().filter(|_| self.tiny_state.account.as_ref() == Some(&key.account()))?;
        let room = community.rooms.iter().find(|room| room.id == key.id)?;
        let account = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&key.account()).map(|status| status.name.clone())).unwrap_or_default();
        Some((room.name.clone(), model::server_name(community, &account)))
    }

    /// The Tiny server's column is on show with this call's server: its user panel holds mute
    /// and deafen, so the dock above it need only say where the call is.
    pub(in crate::gui) fn tiny_column_has(&self, key: &Key) -> bool {
        matches!(self.sidebar_state.mode, Mode::Network(Network::Tiny)) && self.tiny_state.account.as_ref() == Some(&key.account()) && self.tiny_state.community.is_some()
    }

    fn tiny_live(&self) -> bool {
        self.tiny_state.account.as_ref().is_some_and(|account| {
            self.snapshot.as_ref().is_some_and(|snapshot| snapshot.accounts.iter().any(|status| &status.key == account && status.link == Link::Connected))
        })
    }

    /// The Tiny call on show, if this server has one, in whatever state: its room.
    pub(super) fn tiny_call(&self) -> Option<protocol::Call> {
        let account = self.tiny_state.account.as_ref()?;
        self.call_shown(self.snapshot.as_ref()?).filter(|call| call.conversation.account() == *account)
    }

    /// Where a made-up call of `--shot` goes: the room I sit in, else the first.
    #[cfg(feature = "gui-shot")]
    pub(super) fn tiny_shot_room(&self) -> Option<Key> {
        let page = self.tiny_page_facts()?;
        let room = page.mine.map(str::to_owned).or_else(|| page.community.rooms.first().map(|room| room.id.clone()))?;
        Some(Key { network: Network::Tiny, account: page.account.account.clone(), id: room })
    }

    /// The page, when it has loaded.
    pub(super) fn tiny_page_facts(&self) -> Option<Page<'_>> {
        let community = self.tiny_state.community.as_ref()?;
        let account = self.tiny_state.account.as_ref()?;
        let me = if community.me.is_empty() { account.account.as_str() } else { community.me.as_str() };
        let mine = community.rooms.iter().find(|room| room.participants.iter().any(|seat| seat.user_id == me)).map(|room| room.id.as_str());
        let account_name = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(account).map(|status| status.name.clone())).unwrap_or_default();
        Some(Page {
            community,
            account,
            may: model::may(community),
            live: self.tiny_live(),
            me,
            mine,
            audio: community.media == "up",
            name: model::server_name(community, &account_name),
        })
    }

    /// `clear_busy` is the action whose button stays quiet until this read, or a later one, returns.
    fn fetch(&mut self, account: AccountKey, clear_busy: Option<u64>, cx: &mut Context<Self>) {
        let switched = self.tiny_state.account.as_ref() != Some(&account);
        if switched {
            self.tiny_state.forget_page();
            self.tiny_state.invite = InviteDraft::default();
            self.tiny_state.action_epoch += 1;
            self.tiny_state.busy = false;
            self.tiny_state.clears_busy = None;
            self.tiny_state.call_epoch += 1;
            self.tiny_state.call_busy = false;
        }
        self.tiny_state.account = Some(account.clone());
        if self.tiny_state.community.is_none() {
            self.tiny_state.loading = true;
            self.tiny_state.error = None;
        }
        if clear_busy.is_some() {
            self.tiny_state.clears_busy = clear_busy;
        }
        self.tiny_state.load_epoch += 1;
        let epoch = self.tiny_state.load_epoch;
        self.off_thread(cx, move || ask(&Request::TinyCommunity { account }), move |this, answer, cx| {
            if this.tiny_state.load_epoch != epoch {
                return;
            }
            this.tiny_state.loading = false;
            if let Some(action) = this.tiny_state.clears_busy.take() {
                if this.tiny_state.action_epoch == action {
                    this.tiny_state.busy = false;
                }
            }
            match answer {
                Ok(Response::TinyCommunity { community }) => {
                    this.tiny_state.error = None;
                    this.tiny_loaded(community, cx);
                }
                Ok(Response::Error { detail, .. }) => this.tiny_state.error = Some(detail),
                Ok(_) => this.tiny_state.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                Err(err) => this.tiny_state.error = Some(crate::client::trouble_words(&err)),
            }
            cx.notify();
        });
    }

    /// A fresh read of the page. What it no longer has goes from the panes and menus with it.
    fn tiny_loaded(&mut self, community: TinyCommunity, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state;
        let room_gone = |id: &String| community.rooms.iter().all(|room| &room.id != id);
        let person_gone = |id: &String| community.members.iter().all(|member| &member.user_id != id);
        if state.channel.as_ref().is_some_and(room_gone) {
            state.channel = None;
            state.call_error = None;
        }
        let stale = match &state.float {
            Some(Float::Person { user_id, .. }) => person_gone(user_id),
            Some(Float::Channel { room_id, .. }) => room_gone(room_id),
            _ => false,
        };
        if stale {
            state.float = None;
            state.sub = None;
        }
        let stale = match &state.modal {
            Some(Modal::Kick(id) | Modal::Ban(id) | Modal::Profile(id)) => person_gone(id),
            Some(Modal::DeleteRoom(id) | Modal::RenameRoom(id)) => room_gone(id),
            _ => false,
        };
        if stale {
            state.modal = None;
        }
        if state.card.as_ref().is_some_and(|(id, _)| person_gone(id)) {
            state.card = None;
        }
        let first = state.community.is_none();
        state.community = Some(community);
        if first {
            self.tiny_settings_loaded(cx);
        }
        self.tiny_settings_refreshed(cx);
    }

    /// Sends one request whose answer is `Ok` or a code, then reads the page again.
    fn act(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.tiny_state.busy {
            return;
        }
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_state.busy = true;
        self.tiny_state.modal_error = None;
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
        let refused = match answer {
            Ok(Response::Ok) => {
                // A modal that asked for this is done; the code stays until Done.
                if !matches!(self.tiny_state.modal, Some(Modal::Code { .. }) | Some(Modal::Invite)) {
                    self.tiny_state.modal = None;
                }
                self.tiny_settings_saved(cx);
                self.fetch(account, Some(epoch), cx);
                None
            }
            Ok(Response::TinyCode { code }) => {
                self.tiny_state.modal = Some(match protocol::pack_join(&code.origin, &code.token) {
                    Some(paste) => {
                        let draft = &self.tiny_state.invite;
                        let legacy = self.tiny_state.community.as_ref().is_some_and(|community| !community.has(protocol::TINY_FEATURE_INVITE_OPTIONS));
                        let terms = if legacy { model::invite_terms(Some(1), Some(604_800)) } else { model::invite_terms(draft.uses, draft.expiry) };
                        Modal::Code { paste, origin: code.origin, terms, role: code.role }
                    }
                    None => {
                        self.tiny_state.modal_error = Some(UNMADE.to_owned());
                        Modal::Invite
                    }
                });
                self.tiny_state.copied = false;
                self.fetch(account, Some(epoch), cx);
                None
            }
            Ok(Response::Error { detail, .. }) => Some(detail),
            Ok(_) => Some(crate::client::OTHER_ANSWER.to_owned()),
            Err(err) => Some(crate::client::trouble_words(&err)),
        };
        if let Some(detail) = refused {
            self.tiny_state.busy = false;
            // A save refused is said in the unsaved-changes bar, a modal's in the modal, anything
            // else in a toast.
            let saving = self.tiny_state.settings.as_ref().is_some_and(|overlay| overlay.saving);
            self.tiny_settings_failed(&detail);
            if self.tiny_state.modal.is_some() {
                self.tiny_state.modal_error = Some(detail);
            } else if !saving {
                self.toast(Some(("icons/circle-alert.svg", super::theme::danger())), detail, cx);
            }
        }
        cx.notify();
    }

    /// Several requests, one after another, as one action: the first refusal stops the rest and is
    /// said as `act` says one. The page is read again either way.
    fn act_all(&mut self, requests: Vec<Request>, cx: &mut Context<Self>) {
        if requests.is_empty() {
            self.tiny_settings_saved(cx);
            cx.notify();
            return;
        }
        if self.tiny_state.busy {
            return;
        }
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_state.busy = true;
        self.tiny_state.modal_error = None;
        self.tiny_state.action_epoch += 1;
        let epoch = self.tiny_state.action_epoch;
        cx.notify();
        self.off_thread(
            cx,
            move || {
                for request in &requests {
                    match ask(request) {
                        Ok(Response::Ok) => {}
                        other => return other,
                    }
                }
                Ok(Response::Ok)
            },
            move |this, answer, cx| {
                let failed = !matches!(answer, Ok(Response::Ok));
                this.acted(epoch, account.clone(), answer, cx);
                // Some may have gone through before the refusal: read what the server holds now.
                if failed && this.tiny_state.action_epoch == epoch {
                    this.fetch(account, None, cx);
                }
            },
        );
    }

    /// Ends every session of this account on the server, then forgets it here, as signing out does.
    pub(super) fn tiny_sign_out_everywhere(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        if self.tiny_state.busy {
            return;
        }
        self.tiny_state.busy = true;
        self.tiny_state.modal_error = None;
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinySignOutEverywhere { account }),
            |this, answer, cx| {
                this.tiny_state.busy = false;
                match answer {
                    Ok(Response::Error { detail, .. }) => this.tiny_state.modal_error = Some(detail),
                    Ok(_) => {
                        this.tiny_state.modal = None;
                        this.tiny_state.settings = None;
                    }
                    Err(err) => this.tiny_state.modal_error = Some(crate::client::trouble_words(&err)),
                }
                cx.notify();
            },
        );
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

    fn with_account(&self) -> Option<AccountKey> {
        self.tiny_state.account.clone()
    }

    pub(super) fn tiny_join(&mut self, room_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.tiny_state.channel = Some(room_id.clone());
        self.call_asked();
        self.call(Request::TinyCallJoin { account, room_id }, cx);
    }

    pub(super) fn tiny_leave(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.call(Request::TinyCallLeave { account }, cx);
    }

    /// Mute, from the user panel or the channel's bar: the call's own while one is up, else
    /// what the next call starts with.
    pub(super) fn tiny_toggle_mute(&mut self, cx: &mut Context<Self>) {
        if self.tiny_call().is_some() {
            self.call_mute(cx);
            return;
        }
        let (muted, deafened) = self.tiny_state.preset;
        // Unmuting while deafened undeafens too, as a call does.
        self.tiny_state.preset = if muted || deafened { (false, false) } else { (true, false) };
        cx.notify();
    }

    pub(super) fn tiny_toggle_deafen(&mut self, cx: &mut Context<Self>) {
        if self.tiny_call().is_some() {
            self.call_deafen(cx);
            return;
        }
        let deafened = self.tiny_state.preset.1;
        self.tiny_state.preset = if deafened { (false, false) } else { (true, true) };
        cx.notify();
    }

    /// Mute and deafen as the panel draws them.
    pub(super) fn tiny_quiet(&self) -> (bool, bool) {
        match self.tiny_call() {
            Some(call) => (call.muted || call.deafened, call.deafened),
            None => self.tiny_state.preset,
        }
    }

    /// A call that just began on this server gets the mute chosen before it (everyone's volume
    /// it has from chatd); the connection's numbers are read about once a second while it lasts.
    fn note_tiny_call(&mut self, cx: &mut Context<Self>) {
        // A call that failed or only rings has no connection to measure.
        let live = |call: &protocol::Call| !matches!(call.state, protocol::CallState::Failed { .. } | protocol::CallState::RingingIn { .. });
        let call = self.tiny_call().filter(live).map(|call| call.conversation.clone());
        if call.is_none() {
            self.tiny_state.applied = None;
            if self.tiny_state.stats_task.take().is_some() {
                self.tiny_state.stats = None;
                self.tiny_state.stats_note = None;
            }
            return;
        }
        if self.tiny_state.applied == call {
            return;
        }
        self.tiny_state.applied = call;
        #[cfg(feature = "gui-shot")]
        if let Some(stats) = crate::gui::call::shot_stats() {
            self.tiny_state.stats = Some(stats);
            return;
        }
        let (muted, deafened) = std::mem::take(&mut self.tiny_state.preset);
        if muted || deafened {
            self.off_thread(cx, move || ask(&Request::CallSet { muted, deafened }), |_, _, _| {});
        }
        self.tiny_state.stats_task = Some(cx.spawn(async move |this, cx| loop {
            let answer = cx.background_executor().spawn(async { ask(&Request::CallStats) }).await;
            let going = this.update(cx, |this, cx| {
                match answer {
                    Ok(Response::CallStats { stats }) => {
                        this.tiny_state.stats = Some(stats);
                        this.tiny_state.stats_note = None;
                    }
                    // Before media flows mediad says why there is nothing to measure yet.
                    Ok(Response::Error { detail, .. }) => {
                        this.tiny_state.stats = None;
                        this.tiny_state.stats_note = Some(detail);
                    }
                    _ => {}
                }
                cx.notify();
            });
            if going.is_err() {
                return;
            }
            cx.background_executor().timer(STATS_EVERY).await;
        }));
    }

    /// One person's volume and mute for me. With `send`, chatd keeps it for this account and
    /// applies it to the call at once (`CallGain`); without, it is only drawn (a slider being
    /// dragged).
    pub(super) fn tiny_set_gain(&mut self, user_id: &str, percent: u16, muted: bool, send: bool, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let percent = percent.min(protocol::GAIN_MAX);
        self.tiny_state.gains.insert((account.account.clone(), user_id.to_owned()), (percent, muted));
        if send {
            let request = Request::CallGain { account, user_id: user_id.to_owned(), percent, muted };
            self.off_thread(cx, move || ask(&request), |this, answer, cx| {
                if let Ok(Response::Error { detail, .. }) = answer {
                    this.toast(Some(("icons/circle-alert.svg", super::theme::danger())), detail, cx);
                }
            });
        }
        cx.notify();
    }

    pub(super) fn tiny_copy(&mut self, text: String, words: &'static str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast(Some(("icons/check.svg", super::theme::text_soft())), words, cx);
    }

    pub(in crate::gui) fn tiny_pick_channel(&mut self, room_id: Option<String>, cx: &mut Context<Self>) {
        if self.tiny_state.channel != room_id {
            self.tiny_state.call_error = None;
            self.tiny_state.focus = None;
        }
        self.tiny_state.channel = room_id;
        cx.notify();
    }

    pub(super) fn tiny_fold(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.tiny_state.folded.remove(&key) {
            self.tiny_state.folded.insert(key);
        }
        cx.notify();
    }

    pub(super) fn tiny_toggle_members(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.members_open = !self.tiny_state.members_open;
        self.tiny_state.card = None;
        self.save_prefs(cx);
        cx.notify();
    }

    pub(super) fn tiny_open_float(&mut self, float: Float, cx: &mut Context<Self>) {
        self.tiny_state.float = Some(float);
        self.tiny_state.sub = None;
        self.tiny_state.card = None;
        self.tiny_state.sliding = None;
        cx.notify();
    }

    pub(super) fn tiny_close_float(&mut self, cx: &mut Context<Self>) {
        // A volume being dragged is kept where it was let go.
        if let Some(user_id) = self.tiny_state.sliding.take() {
            let (percent, muted) = self.tiny_state.gain(&user_id);
            self.tiny_set_gain(&user_id, percent, muted, true, cx);
        }
        self.tiny_state.float = None;
        self.tiny_state.sub = None;
        cx.notify();
    }

    /// The pointer came onto a member's row, or left it. Leaving one row for the next may be
    /// told after arriving, so only the card of the row left is put away.
    pub(super) fn tiny_hover_card(&mut self, user_id: &str, over: bool, y: f32, cx: &mut Context<Self>) {
        let card = &mut self.tiny_state.card;
        if over && self.tiny_state.float.is_none() {
            *card = Some((user_id.to_owned(), y));
        } else if !over && card.as_ref().is_some_and(|(shown, _)| shown == user_id) {
            *card = None;
        } else {
            return;
        }
        cx.notify();
    }

    /// Escape, from the window's keys: the top layer here closes first. False when nothing here
    /// was open.
    pub(super) fn tiny_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.tiny_state.sub.is_some() {
            self.tiny_state.sub = None;
        } else if self.tiny_state.float.is_some() {
            self.tiny_close_float(cx);
        } else if self.tiny_state.modal.is_some() {
            self.close_modal(cx);
        } else if self.tiny_state.settings.is_some() {
            self.tiny_close_settings(cx);
        } else if self.tiny_state.card.is_some() {
            self.tiny_state.card = None;
        } else {
            return false;
        }
        cx.notify();
        true
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
                // Occupants carry only mute and deafen; what the server says of a seat beyond that
                // (server mute, priority) stays as the last read of the page had it.
                let before = std::mem::take(&mut room.participants);
                room.participants = occupants
                    .people
                    .iter()
                    .map(|occupant| {
                        let was = before.iter().find(|seat| seat.user_id == occupant.person.id).cloned().unwrap_or_default();
                        protocol::TinySeat { user_id: occupant.person.id.clone(), muted: occupant.muted, deafened: occupant.deafened, speaking: occupant.speaking, ..was }
                    })
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

    // What the menus and modals send.

    pub(super) fn tiny_moderate(&mut self, user_id: String, muted: Option<bool>, deafened: Option<bool>, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let Some(room_id) = self.tiny_state.community.as_ref().and_then(|community| model::seat_of(community, &user_id)).map(|(room, _)| room.id.clone()) else {
            return;
        };
        self.act(Request::TinyCallModerate { account, room_id, user_id, server_muted: muted, server_deafened: deafened }, cx);
    }

    pub(super) fn tiny_move(&mut self, user_id: String, room_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.act(Request::TinyCallMove { account, user_id, room_id }, cx);
    }

    pub(super) fn tiny_disconnect(&mut self, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let Some(room_id) = self.tiny_state.community.as_ref().and_then(|community| model::seat_of(community, &user_id)).map(|(room, _)| room.id.clone()) else {
            return;
        };
        self.act(Request::TinyCallKick { account, room_id, user_id }, cx);
    }

    /// Times someone out for `secs` from now, or with `None` ends their timeout.
    pub(super) fn tiny_timeout(&mut self, user_id: String, secs: Option<i64>, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
        let until = secs.map(|secs| protocol::tiny_time(now + secs));
        self.act(Request::TinyTimeout { account, user_id, until, reason: String::new() }, cx);
    }

    pub(super) fn tiny_toggle_role(&mut self, user_id: String, role_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let Some(member) = self.tiny_state.community.as_ref().and_then(|community| community.member(&user_id)) else {
            return;
        };
        let mut role_ids = member.roles.clone();
        if let Some(at) = role_ids.iter().position(|id| *id == role_id) {
            role_ids.remove(at);
        } else {
            role_ids.push(role_id);
        }
        self.act(Request::TinyMemberRoles { account, user_id, role_ids }, cx);
    }

    pub(super) fn tiny_legacy_role(&mut self, user_id: String, role: &'static str, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.act(Request::TinyRole { account, user_id, role: role.to_owned() }, cx);
    }

    pub(super) fn tiny_layout(&mut self, groups: Vec<protocol::TinyLayoutGroup>, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        // Drawn in the new order at once; the server's answer reads the page again.
        if let Some(community) = self.tiny_state.community.as_mut() {
            model::apply_layout(community, &groups);
        }
        self.act(Request::TinyLayout { account, groups }, cx);
    }

    pub(super) fn tiny_sign_out(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.tiny_state.modal = None;
        self.sign_out(account, cx);
    }

    /// The name field's Return: whichever modal holds it.
    fn submit_name(&mut self, cx: &mut Context<Self>) {
        match self.tiny_state.modal.clone() {
            Some(Modal::NewRoom(category)) => self.tiny_create_room(category, cx),
            Some(Modal::RenameRoom(room_id)) => self.tiny_rename_room(room_id, cx),
            Some(Modal::NewCategory) => self.tiny_create_category(cx),
            _ => {}
        }
    }

    fn typed_name(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let typed = self.tiny_state.name_input.read(cx).text().to_owned();
        let name = protocol::tiny_name(&typed).map(str::to_owned);
        if name.is_none() {
            self.tiny_state.modal_error = Some(ROOM_NAME_SIZE.to_owned());
            cx.notify();
        }
        name
    }

    pub(super) fn tiny_create_room(&mut self, category: Option<String>, cx: &mut Context<Self>) {
        let (Some(account), Some(name)) = (self.with_account(), self.typed_name(cx)) else {
            return;
        };
        let modern = self.tiny_state.community.as_ref().is_some_and(|community| community.has(protocol::TINY_FEATURE_CATEGORIES));
        let request = if modern {
            Request::TinyRoomCreateWith { account, room: protocol::TinyRoomEdit { name: Some(name), category_id: category, ..Default::default() } }
        } else {
            Request::TinyRoomCreate { account, name }
        };
        self.act(request, cx);
    }

    pub(super) fn tiny_rename_room(&mut self, room_id: String, cx: &mut Context<Self>) {
        let (Some(account), Some(name)) = (self.with_account(), self.typed_name(cx)) else {
            return;
        };
        self.act(Request::TinyRoomRename { account, room_id, name }, cx);
    }

    pub(super) fn tiny_create_category(&mut self, cx: &mut Context<Self>) {
        let (Some(account), Some(name)) = (self.with_account(), self.typed_name(cx)) else {
            return;
        };
        self.act(Request::TinyCategoryCreate { account, name }, cx);
    }

    pub(super) fn tiny_open_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        self.tiny_state.float = None;
        self.tiny_state.sub = None;
        self.tiny_state.card = None;
        self.tiny_state.modal_error = None;
        self.tiny_state.copied = false;
        let focus = match &modal {
            Modal::NewRoom(_) | Modal::NewCategory => {
                self.tiny_state.name_input.update(cx, |input, cx| input.set_text("", cx));
                Some(self.tiny_state.name_input.clone())
            }
            Modal::RenameRoom(room_id) => {
                let now = self.tiny_state.community.as_ref().and_then(|community| community.rooms.iter().find(|room| room.id == *room_id)).map(|room| room.name.clone()).unwrap_or_default();
                self.tiny_state.name_input.update(cx, |input, cx| input.set_text(&now, cx));
                Some(self.tiny_state.name_input.clone())
            }
            Modal::Transfer(_) | Modal::RevokeInvites { typed: true } => {
                let confirm = self.tiny_state.settings_fields.confirm.clone();
                confirm.update(cx, |input, cx| input.set_text("", cx));
                Some(confirm)
            }
            Modal::KickMany(_) => {
                self.tiny_state.reason_input.update(cx, |input, cx| input.set_text("", cx));
                Some(self.tiny_state.reason_input.clone())
            }
            Modal::Kick(_) | Modal::Ban(_) => {
                self.tiny_state.reason_input.update(cx, |input, cx| input.set_text("", cx));
                self.tiny_state.ban_days = Some(7);
                Some(self.tiny_state.reason_input.clone())
            }
            Modal::Invite => {
                self.tiny_state.label_input.update(cx, |input, cx| input.set_text("", cx));
                self.tiny_state.invite = InviteDraft::default();
                None
            }
            _ => None,
        };
        if let Some(input) = focus {
            window.focus(&input.focus_handle(cx), cx);
        }
        self.tiny_state.modal = Some(modal);
        cx.notify();
    }

    pub(super) fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.modal = None;
        self.tiny_state.modal_error = None;
        cx.notify();
    }

    pub(super) fn tiny_send_invite(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let options = self.tiny_state.community.as_ref().is_some_and(|community| community.has(protocol::TINY_FEATURE_INVITE_OPTIONS));
        let request = if options {
            let draft = &self.tiny_state.invite;
            let label = self.tiny_state.label_input.read(cx).text().trim().to_owned();
            Request::TinyInviteWith {
                account,
                options: protocol::TinyInviteOptions { role_ids: draft.roles.clone(), label, max_uses: draft.uses, expires_in_secs: draft.expiry, default_expiry: false },
            }
        } else {
            Request::TinyInvite { account, role: self.tiny_state.invite.legacy_role.to_owned() }
        };
        self.act(request, cx);
    }

    pub(super) fn tiny_send_kick(&mut self, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let modern = self.tiny_state.community.as_ref().is_some_and(|community| community.access.is_some());
        let reason = self.tiny_state.reason_input.read(cx).text().trim().to_owned();
        let request = if modern { Request::TinyKick { account, user_id, reason } } else { Request::TinyRemove { account, user_id } };
        self.act(request, cx);
    }

    /// Kicks everyone listed, with one reason for all of them.
    pub(super) fn tiny_send_kicks(&mut self, user_ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let reason = self.tiny_state.reason_input.read(cx).text().trim().to_owned();
        let requests = user_ids.into_iter().map(|user_id| Request::TinyKick { account: account.clone(), user_id, reason: reason.clone() }).collect();
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.members.selected.clear();
        }
        self.act_all(requests, cx);
    }

    pub(super) fn tiny_delete_role(&mut self, role_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.act(Request::TinyRoleDelete { account, role_id }, cx);
    }

    pub(super) fn tiny_send_ban(&mut self, user_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        let reason = self.tiny_state.reason_input.read(cx).text().trim().to_owned();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
        let expires_at = self.tiny_state.ban_days.map(|days| protocol::tiny_time(now + i64::from(days) * 86_400));
        self.act(Request::TinyBan { account, user_id, reason, expires_at }, cx);
    }

    pub(super) fn tiny_delete_room(&mut self, room_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        if self.tiny_state.channel.as_ref() == Some(&room_id) {
            self.tiny_state.channel = None;
        }
        self.act(Request::TinyRoomDelete { account, room_id }, cx);
    }

    pub(super) fn tiny_delete_category(&mut self, category_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.with_account() else {
            return;
        };
        self.act(Request::TinyCategoryDelete { account, category_id }, cx);
    }
}

#[cfg(feature = "gui-shot")]
impl Shell {
    /// `--tiny-*`: one state of the server page, opened without clicking, for a screenshot.
    pub(super) fn tiny_shot(&mut self, flag: fn(&str) -> Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.tiny_state.community.clone() else {
            return;
        };
        let rooms: Vec<String> = model::lines(&community, &HashSet::new(), None)
            .into_iter()
            .filter_map(|line| match line {
                model::Line::Room { id } => Some(id),
                _ => None,
            })
            .collect();
        let seats: Vec<String> = community.rooms.iter().flat_map(|room| room.participants.iter().map(|seat| seat.user_id.clone())).filter(|id| *id != community.me).collect();
        let at = Point::new(gpui::px(420.), gpui::px(220.));
        if let Some(ix) = flag("--tiny-room").and_then(|n| n.parse::<usize>().ok()) {
            self.tiny_state.channel = rooms.get(ix).cloned();
        }
        if let Some(ix) = flag("--tiny-focus").and_then(|n| n.parse::<usize>().ok()) {
            let room = self.tiny_state.channel.as_ref().and_then(|id| community.rooms.iter().find(|room| room.id == *id));
            self.tiny_state.focus = room.and_then(|room| room.participants.get(ix)).map(|seat| seat.user_id.clone());
        }
        match flag("--tiny-menu").as_deref() {
            Some("header") => self.tiny_open_float(Float::Header, cx),
            Some("stats") => self.tiny_open_float(Float::Stats, cx),
            Some("channel") => {
                if let Some(room_id) = self.tiny_state.channel.clone().or_else(|| rooms.first().cloned()) {
                    self.tiny_open_float(Float::Channel { room_id, at }, cx);
                }
            }
            Some(person) if person.starts_with("person") => {
                let mut parts = person.split(':').skip(1);
                let ix = parts.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
                if let Some(user_id) = seats.get(ix).cloned() {
                    self.tiny_open_float(Float::Person { user_id, at }, cx);
                    if parts.next() == Some("sub") {
                        self.tiny_state.sub = Some(Sub::Move);
                    }
                }
            }
            _ => {}
        }
        let target = seats.first().cloned().unwrap_or_default();
        match flag("--tiny-modal").as_deref() {
            Some("invite") => self.tiny_open_modal(Modal::Invite, window, cx),
            Some("code") => {
                let paste = protocol::pack_join(&community.origin, "Nf3kQ8xLw2pZ7rT1").unwrap_or_default();
                self.tiny_open_modal(Modal::Code { paste, origin: community.origin.clone(), terms: model::invite_terms(Some(5), Some(86_400)), role: "member".into() }, window, cx);
            }
            Some("kick") => self.tiny_open_modal(Modal::Kick(target), window, cx),
            Some("ban") => self.tiny_open_modal(Modal::Ban(target), window, cx),
            Some("profile") => self.tiny_open_modal(Modal::Profile(target), window, cx),
            _ => {}
        }
        if std::env::args().any(|arg| arg == "--tiny-owner") {
            if let Some(community) = self.tiny_state.community.as_mut() {
                community.access = Some(protocol::TinyAccess { owner: true, ..community.access.clone().unwrap_or_default() });
                if let Some(me) = community.members.iter_mut().find(|member| member.user_id == community.me) {
                    me.owner = true;
                }
            }
        }
        if std::env::args().any(|arg| arg == "--tiny-settings") {
            let asked = flag("--tiny-settings").unwrap_or_default();
            let page = settings::Page::ALL.into_iter().find(|page| page.label().to_lowercase().starts_with(&asked)).filter(|_| !asked.is_empty() && asked != "dirty");
            self.tiny_open_settings(page.unwrap_or(settings::Page::Overview), None, window, cx);
            if asked == "dirty" {
                let name = self.tiny_state.settings_fields.name.clone();
                name.update(cx, |input, cx| input.set_text("Night Shift HQ", cx));
            }
            if let Some(ix) = flag("--tiny-pick").and_then(|n| n.parse::<usize>().ok()) {
                let roles: Vec<String> = settings::roles_in_order(&community);
                match self.tiny_state.settings.as_ref().map(|overlay| overlay.page) {
                    Some(settings::Page::Roles) => self.tiny_roles_select(roles.get(ix).cloned(), cx),
                    Some(settings::Page::Channels) => self.tiny_channels_select(rooms.get(ix).cloned().map(settings::Sel::Room), cx),
                    Some(settings::Page::Members) => {
                        if let Some(overlay) = self.tiny_state.settings.as_mut() {
                            overlay.members.selected = community.members.iter().filter(|member| settings::tickable(&community, member)).take(ix).map(|member| member.user_id.clone()).collect();
                        }
                    }
                    _ => {}
                }
            }
            if flag("--tiny-confirm").as_deref() == Some("transfer") {
                let heir = community.members.iter().find(|member| member.user_id != community.me && !model::is_owner(member)).map(|member| member.user_id.clone());
                if let (Some(heir), Some(overlay)) = (heir, self.tiny_state.settings.as_mut()) {
                    overlay.danger.heir = Some(heir.clone());
                    overlay.danger.step_down = true;
                    self.tiny_open_modal(Modal::Transfer(heir), window, cx);
                    let confirm = self.tiny_state.settings_fields.confirm.clone();
                    confirm.update(cx, |input, cx| input.set_text("Night Shift", cx));
                }
            }
            if let Some(down) = flag("--tiny-scroll").and_then(|n| n.parse::<f32>().ok()) {
                if let Some(overlay) = self.tiny_state.settings.as_ref() {
                    overlay.scroll.set_offset(Point::new(gpui::px(0.), gpui::px(-down)));
                }
            }
            if let Some(role) = flag("--tiny-override") {
                if let Some(overlay) = self.tiny_state.settings.as_mut() {
                    overlay.channels.role = community.roles.iter().find(|known| known.name.eq_ignore_ascii_case(&role)).map(|known| known.id.clone());
                }
            }
        }
        if let Some(ix) = flag("--tiny-card").and_then(|n| n.parse::<usize>().ok()) {
            let groups = model::groups(&community, self.tiny_live(), "");
            if let Some(user_id) = groups.iter().flat_map(|group| group.people.iter()).nth(ix).cloned() {
                self.tiny_state.card = Some((user_id, 200.));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn join_says_what_stands_in_its_way() {
        assert_eq!(join_blocked(true, "off"), None);
        assert_eq!(join_blocked(true, ""), None, "an older server has no audio to ask for");
        assert_eq!(join_blocked(true, "up"), None, "a server with audio takes a join with audio");
        assert!(join_blocked(true, "down").is_some_and(|words| words.contains("down")));
        assert!(join_blocked(false, "off").is_some_and(|words| words.contains("connected")));
    }

    #[test]
    fn a_volume_is_what_chatd_keeps_else_the_default() {
        let kept = [("a".to_owned(), 150u16, false), ("b".to_owned(), 100u16, true)];
        assert_eq!(kept_gain(&kept, "a"), (150, false));
        assert_eq!(kept_gain(&kept, "b"), (100, true));
        assert_eq!(kept_gain(&kept, "c"), (100, false));
    }
}
