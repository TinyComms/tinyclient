//! What floats over the window for people: the profile card (the pane's top third, 300 wide),
//! the "…" menus of a person and a conversation, and the one confirm before a block, an
//! unfriending or a leave. One at a time; Escape or a click outside closes it.

use super::info::profile::draw;
use super::info::profile::facts::{self, Act, Call, CardBody, Item, Seen, Surface};
use super::presence::Ring;
use super::{existing_direct, in_call, only_draws, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::place::{placed, Layer, Place};
use crate::gui::components::{
    button, button_with_icon, fade_in, menu, menu_item, menu_item_danger, menu_item_more, menu_label, menu_separator,
    modal_body, modal_buttons, network_icon, scrim, search_field, sensitive_cover, Avatar, ButtonKind, Shape as Plate,
};
use crate::gui::input::{InputEvent, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    anchored, canvas, deferred, div, point, prelude::*, px, AnyElement, Bounds, ClipboardItem, Context, Div, Entity,
    FocusHandle, Focusable, KeyDownEvent, MouseDownEvent, Pixels, Point, WeakEntity, Window,
};
use protocol::{AccountKey, AppCommand, CommandKind, Conversation, Key, Link, Network, Notify, Person, Profile, RelateAction, Request, Response, SpaceRole};
use std::cell::Cell;
use std::rc::Rc;

const CARD_WIDTH: f32 = 300.;
const CONFIRM_WIDTH: f32 = 360.;

#[derive(Clone, PartialEq)]
pub enum Float {
    Card(Key),
    /// A row's More menu: Profile, then the person's menu.
    Menu(Key),
    /// The profile pane's "…".
    Person { person: Key, seen: Seen, alt: bool },
    /// Details' "…".
    Conversation { conversation: Conversation, link: Option<String>, alt: bool },
    /// Asked once before a friend is removed or someone is blocked.
    Confirm(Key, RelateAction),
    /// One-line modal to set or clear a friend's private nickname.
    FriendNickname(Key),
    /// Asked once before leaving a group (`silent` suppresses Discord's leave notice).
    Leave { conversation: Conversation, silent: bool },
    /// Asked once before clearing history or deleting a chat on the network.
    ClearHistory { conversation: Conversation, delete_chat: bool, revoke: bool },
    /// Confirm sheet before reporting a message, person, or conversation to the network.
    Report {
        target: protocol::ReportTarget,
        name: String,
        link: String,
        reason: protocol::ReportReason,
        block: bool,
    },
}

impl Float {
    fn person(&self) -> Option<&Key> {
        match self {
            Float::Card(person)
            | Float::Menu(person)
            | Float::Person { person, .. }
            | Float::Confirm(person, _)
            | Float::FriendNickname(person) => Some(person),
            Float::Conversation { .. } | Float::Leave { .. } | Float::ClearHistory { .. } | Float::Report { .. } => None,
        }
    }
}

/// Their wall clock now, and how far it is from ours: ("12:48 PM", "3 h behind you"). Offsets
/// are seconds east of UTC.
pub fn local_time_parts(now: i64, theirs: i32, ours: i32) -> (String, String) {
    let seconds = (now + i64::from(theirs)).rem_euclid(86_400);
    let (hour, minute) = (seconds / 3600, seconds % 3600 / 60);
    let (hour, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    let apart = (i64::from(theirs) - i64::from(ours)) / 60;
    let far = match (apart.abs() / 60, apart.abs() % 60) {
        (hours, 0) => format!("{hours} h"),
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    let where_ = match apart.signum() {
        0 => "same time as you".to_owned(),
        1 => format!("{far} ahead of you"),
        _ => format!("{far} behind you"),
    };
    (format!("{hour}:{minute:02} {half}"), where_)
}

/// This machine's offset from UTC now, in seconds.
pub(super) fn own_offset(now: i64) -> i32 {
    let seconds = now as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` / `localtime_s` write a full `tm` into the buffer we own.
    unsafe {
        #[cfg(unix)]
        libc::localtime_r(&seconds, &mut tm);
        #[cfg(windows)]
        libc::localtime_s(&mut tm, &seconds);
    }
    #[cfg(unix)]
    {
        tm.tm_gmtoff as i32
    }
    #[cfg(windows)]
    {
        utc_offset_seconds(seconds)
    }
}

#[cfg(windows)]
fn utc_offset_seconds(seconds: libc::time_t) -> i32 {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    // The libc crate does not expose these on Windows. They are in the CRT.
    extern "C" {
        fn mktime(tm: *mut libc::tm) -> libc::time_t;
        fn _mkgmtime(tm: *mut libc::tm) -> libc::time_t;
    }
    // SAFETY: `gmtime_s`, `localtime_s`, `mktime`, and `_mkgmtime` fill or read the buffers we own.
    unsafe {
        libc::localtime_s(&mut local, &seconds);
        libc::gmtime_s(&mut utc, &seconds);
        local.tm_isdst = -1;
        let local_sec = mktime(&mut local);
        let utc_sec = _mkgmtime(&mut utc);
        (local_sec - utc_sec) as i32
    }
}

pub struct Floating {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    open: Option<(Float, Point<Pixels>)>,
    /// Counts openings, so each one fades in.
    opened: usize,
    focus: Option<FocusHandle>,
    /// What the network said of the person whose card is up.
    profile: Option<Profile>,
    profile_asked: u64,
    /// Read from the shell once the card is up: the Space it was opened from, and their DM.
    place: Option<(Key, String)>,
    direct: Option<Conversation>,
    /// One-line input for setting a friend's private nickname (`Float::FriendNickname`).
    nick_input: Option<Entity<TextInput>>,
    nick_error: Option<String>,
    /// Editable private note on the person whose card is open (`Capabilities::person_notes`).
    note_input: Option<(Key, Entity<TextInput>)>,
    note_baseline: String,
    note_saving: bool,
    note_saved: bool,
    note_error: Option<String>,
    /// Roles of the Discord server this card was opened from, when we may assign them.
    space_roles: Vec<SpaceRole>,
    roles_loaded: bool,
    roles_error: Option<String>,
    manage_roles: bool,
    /// The Apps submenu of a person menu is open.
    apps: bool,
    apps_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    apps_menu: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The open card's bio was revealed in Streamer mode. A new card hides it again.
    bio_revealed: bool,
}

impl Floating {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub) -> Floating {
        Floating {
            shell,
            hub,
            scene: Scene::default(),
            open: None,
            opened: 0,
            focus: None,
            profile: None,
            profile_asked: 0,
            place: None,
            direct: None,
            nick_input: None,
            nick_error: None,
            note_input: None,
            note_baseline: String::new(),
            note_saving: false,
            note_saved: false,
            note_error: None,
            space_roles: Vec::new(),
            roles_loaded: false,
            roles_error: None,
            manage_roles: false,
            apps: false,
            apps_anchor: Rc::default(),
            apps_menu: Rc::default(),
            bio_revealed: false,
        }
    }

    pub fn see(&mut self, scene: Scene, _: &mut Context<Self>) {
        self.scene = scene;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The person whose card is up, when the float is a profile card.
    pub fn card_person(&self) -> Option<&Key> {
        match self.open.as_ref() {
            Some((Float::Card(person), _)) => Some(person),
            _ => None,
        }
    }

    pub fn open(&mut self, float: Float, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        let next = match &float {
            Float::Card(person) => Some(person),
            _ => None,
        };
        self.release_note(next, cx);
        // On a phone a person's card is their Profile page, never a popover over the screen.
        if let (true, Float::Card(person)) = (crate::gui::phone::active(), &float) {
            let person = person.clone();
            return self.shell_do(cx, move |shell, cx| shell.phone_profile(person, cx));
        }
        if let Some(person) = float.person() {
            // Their presence is drawn, so it is watched for as long as this is up.
            self.hub.watch("profile", [person.clone()], cx);
        }
        if let Float::Card(person) | Float::Menu(person) = &float {
            self.learn_place(person.clone(), matches!(float, Float::Card(_)), cx);
        }
        self.apps = false;
        self.apps_anchor.set(None);
        self.apps_menu.set(None);
        self.bio_revealed = false;
        self.open = Some((float, anchor));
        self.opened += 1;
        cx.notify();
    }

    /// What the shell holds about where they are seen, read once the shell's own update is
    /// over (cards open from inside it); then, for a card, the one profile request.
    fn learn_place(&mut self, person: Key, card: bool, cx: &mut Context<Self>) {
        self.profile_asked += 1;
        if self.profile.as_ref().is_some_and(|profile| profile.person.key != person) {
            self.profile = None;
        }
        self.place = None;
        self.direct = None;
        self.space_roles.clear();
        self.roles_loaded = false;
        self.roles_error = None;
        self.manage_roles = false;
        let (asked, shell, this) = (self.profile_asked, self.shell.clone(), cx.weak_entity());
        cx.spawn(async move |_, cx| {
            let held = shell.read_with(cx, |shell, _| {
                let open = shell.held_rows().next().cloned();
                let place = super::info::space_of(open.as_ref(), &person);
                let direct = existing_direct(shell.held_rows(), &person).cloned();
                let manage = place.as_ref().is_some_and(|(key, _)| {
                    shell.spaces.iter().any(|space| space.key == *key && space.key.network == Network::Discord && space.rights.manage_roles)
                });
                (place, direct, manage)
            });
            let Ok((place, direct, manage)) = held else {
                return;
            };
            let space = place.as_ref().map(|(key, _)| key.clone());
            let weak = this.clone();
            let _ = this.update(cx, |this, cx| {
                if this.profile_asked != asked {
                    return;
                }
                this.place = place;
                this.direct = direct;
                this.manage_roles = manage;
                cx.notify();
                if !card {
                    return;
                }
                if manage {
                    if let Some(space_key) = space.clone() {
                        let weak_roles = weak.clone();
                        super::ask_later(cx, Request::Roles { space: space_key }, move |answer, cx| {
                            let _ = weak_roles.update(cx, |this, cx| {
                                if this.profile_asked != asked {
                                    return;
                                }
                                this.roles_loaded = true;
                                match answer {
                                    Ok(Response::Roles { roles, .. }) => this.space_roles = roles,
                                    Ok(Response::Error { detail, .. }) => this.roles_error = Some(detail),
                                    Err(err) => this.roles_error = Some(err.to_string()),
                                    _ => {}
                                }
                                cx.notify();
                            });
                        });
                    }
                }
                super::ask_later(cx, Request::Profile { person, space }, move |answer, cx| {
                    let Ok(Response::Profile { profile }) = answer else {
                        return;
                    };
                    let _ = weak.update(cx, |this, cx| {
                        if this.profile_asked == asked {
                            this.hub.learn(std::slice::from_ref(&profile.person));
                            this.sync_note_input(&profile.person.key, &profile.note, cx);
                            if !profile.mutual_people.is_empty() {
                                this.hub.learn(&profile.mutual_people);
                            }
                            this.profile = Some(*profile);
                            cx.notify();
                        }
                    });
                });
            });
        })
        .detach();
    }

    fn can_note(&self, person: &Key) -> bool {
        self.scene.accounts.iter().any(|own| own.key == person.account() && own.can.person_notes)
    }

    fn sync_note_input(&mut self, person: &Key, note: &str, cx: &mut Context<Self>) {
        if !self.can_note(person) {
            self.note_input = None;
            return;
        }
        let same = self.note_input.as_ref().is_some_and(|(k, _)| k == person);
        if same {
            let current = self.note_input.as_ref().map(|(_, e)| e.read(cx).text().to_owned()).unwrap_or_default();
            if current == self.note_baseline {
                if let Some((_, input)) = &self.note_input {
                    input.update(cx, |i, cx| i.set_text(note, cx));
                }
                self.note_baseline = note.to_owned();
            }
            return;
        }
        let input = cx.new(|cx| {
            let mut field = TextInput::new("Add a note…", cx);
            field.set_text(note, cx);
            field
        });
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Changed => {
                this.note_saved = false;
                this.note_error = None;
                cx.notify();
            }
            InputEvent::Submit => this.save_note(cx),
        })
        .detach();
        self.note_input = Some((person.clone(), input));
        self.note_baseline = note.to_owned();
        self.note_saving = false;
        self.note_saved = false;
        self.note_error = None;
    }

    /// Saves a dirty note. `force` sends even when a save is already in flight, so switching
    /// people does not drop the latest text.
    fn write_note(&mut self, cx: &mut Context<Self>, force: bool) {
        let Some((person, input)) = self.note_input.clone() else {
            return;
        };
        let text = input.read(cx).text().to_owned();
        if text == self.note_baseline || only_draws() || (!force && self.note_saving) {
            return;
        }
        if text.chars().count() > 256 {
            self.note_error = Some("Notes can be at most 256 characters.".to_owned());
            cx.notify();
            return;
        }
        self.note_saving = true;
        self.note_saved = false;
        self.note_error = None;
        cx.notify();
        let this = cx.weak_entity();
        let saved_text = text.clone();
        super::ask_later(cx, Request::SetNote { person: person.clone(), text }, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                let still = this.note_input.as_ref().is_some_and(|(key, _)| key == &person);
                let profile_hit = this.profile.as_ref().is_some_and(|profile| profile.person.key == person);
                if still {
                    this.note_saving = false;
                    match &answer {
                        Ok(Response::Ok) => {
                            this.note_baseline = saved_text.clone();
                            this.note_saved = true;
                            this.note_error = None;
                        }
                        Ok(Response::Error { detail, .. }) => this.note_error = Some(detail.clone()),
                        Ok(_) => this.note_error = Some("Could not save note.".to_owned()),
                        Err(err) => this.note_error = Some(err.to_string()),
                    }
                }
                if profile_hit {
                    if let Ok(Response::Ok) = &answer {
                        if let Some(profile) = this.profile.as_mut() {
                            profile.note = saved_text;
                        }
                    }
                }
                if still || profile_hit {
                    cx.notify();
                }
            });
        });
    }

    fn save_note(&mut self, cx: &mut Context<Self>) {
        self.write_note(cx, false);
    }

    /// Sends the latest note for whoever is open, then forgets the editor when `next` is someone else.
    fn release_note(&mut self, next: Option<&Key>, cx: &mut Context<Self>) {
        let current = self.note_input.as_ref().map(|(key, _)| key);
        if current == next {
            return;
        }
        if current.is_some() {
            self.write_note(cx, true);
            self.note_input = None;
            self.note_baseline.clear();
            self.note_saving = false;
            self.note_saved = false;
            self.note_error = None;
        }
    }

    /// Re-fetches the open card's profile when `Change::People` signals a live update (e.g. `USER_NOTE_UPDATE`).
    pub fn refresh_profile(&mut self, account: &AccountKey, cx: &mut Context<Self>) {
        let Some((Float::Card(person), _)) = self.open.as_ref() else {
            return;
        };
        if person.account() != *account || self.note_saving {
            return;
        }
        let person = person.clone();
        let space = self.place.as_ref().map(|(key, _)| key.clone());
        let asked = self.profile_asked;
        let weak = cx.weak_entity();
        super::ask_later(cx, Request::Profile { person, space }, move |answer, cx| {
            let Ok(Response::Profile { profile }) = answer else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                if this.profile_asked == asked {
                    this.sync_note_input(&profile.person.key, &profile.note, cx);
                    this.profile = Some(*profile);
                    cx.notify();
                }
            });
        });
    }

    /// Swaps what floats for something else in the same place.
    fn turn_to(&mut self, float: Float, cx: &mut Context<Self>) {
        let next = match &float {
            Float::Card(person) => Some(person),
            _ => None,
        };
        self.release_note(next, cx);
        let anchor = self.open.as_ref().map(|(_, anchor)| *anchor).unwrap_or_default();
        self.bio_revealed = false;
        self.open = Some((float, anchor));
        self.opened += 1;
        cx.notify();
    }

    /// The shell is told after this update: a float may be closed from inside the shell's own.
    fn close(&mut self, cx: &mut Context<Self>) {
        self.release_note(None, cx);
        if self.open.take().is_some() {
            self.hub.watch("profile", [], cx);
            let shell = self.shell.clone();
            cx.defer(move |cx| {
                let _ = shell.update(cx, |_, cx| cx.notify());
            });
            cx.notify();
        }
    }

    fn shell_do(&mut self, cx: &mut Context<Self>, then: impl FnOnce(&mut Shell, &mut Context<Shell>) + 'static) {
        self.close(cx);
        let shell = self.shell.clone();
        cx.defer(move |cx| {
            let _ = shell.update(cx, then);
        });
    }

    fn online(&self, person: &Key) -> bool {
        self.scene.links.iter().find(|(net, _)| *net == person.network).is_none_or(|(_, link)| *link == Link::Connected)
    }

    fn seen(&self, person: &Key) -> Seen {
        Seen {
            native_calls: super::can_call(&self.scene, person),
            in_their_dm: self.scene.peer.as_ref() == Some(person),
            direct: self.direct.as_ref().map(|row| (row.call, row.muted)),
            online: self.online(person),
            close_direct: super::scene_can_close_direct(&self.scene, person),
            can_block: self.scene.blocks_accounts.contains(&person.account()),
            can_friend_nickname: self.scene.accounts.iter().any(|own| own.key == person.account() && own.can.friend_nickname),
        }
    }

    /// The card: the pane's top third at the same width. It only ever grows downward.
    fn card(&self, person: &Person, window: &Window, cx: &mut Context<Self>) -> Div {
        let network = person.key.network;
        let profile = self.profile.as_ref().filter(|profile| profile.person.key == person.key);
        let seen = self.seen(&person.key);
        let calling = in_call(&self.hub, &self.scene, &person.key);
        let presence = if seen.online { shown_presence(&self.hub, person) } else { protocol::Presence::Unknown };
        let status = match self.hub.status_of(&person.key) {
            status if status.is_empty() => person.status.clone(),
            status => status,
        };
        let line = draw::presence_line_for(person, profile, presence, &status, calling);
        let actions = facts::actions(person, &seen, Surface::Card);
        let place = self.place.as_ref().map(|(_, name)| name.as_str());
        let ring = Ring::on(theme::overlay());
        let shown_person = profile.map(|p| &p.person).unwrap_or(person);
        let banner_path = profile
            .and_then(|p| p.banner.as_ref())
            .and_then(|file| self.hub.file(&person.key.account(), file, cx).path);
        let head = div()
            .relative()
            .h(px(16. + draw::AVATAR))
            .children(draw::banner_band(banner_path, profile.and_then(|p| p.accent), theme::overlay()))
            .child(div().absolute().top(px(16.)).left(px(16.)).child(person_avatar(&self.hub, shown_person, draw::AVATAR, &ring, calling, cx)));
        let badges: Vec<(String, Option<String>)> = profile
            .map(|p| {
                p.badges
                    .iter()
                    .map(|b| {
                        let path = b.icon.as_ref().and_then(|file| self.hub.file(&person.key.account(), file, cx).path);
                        (b.name.clone(), path)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mark = draw::mark_of(person).or_else(|| {
            profile
                .map(|p| p.server_tag.as_str())
                .filter(|t| !t.is_empty())
                .map(|t| draw::tag(t.to_owned(), false))
        });
        let mut top = div()
            .px(px(16.))
            .flex()
            .flex_col()
            .child(draw::name_line(person.name.clone(), mark).mt(px(12.)))
            .children(profile.filter(|p| p.server_profile).and_then(|_| draw::soft_line("Server profile".to_owned())))
            .children(draw::soft_line(facts::handle_line(person, profile)))
            .children(draw::badge_strip("card-badge", &badges))
            .children(draw::presence_row(&line, theme::overlay(), 2))
            .when(actions.message || actions.request || actions.call.is_some(), |top| {
                top.child(self.card_actions(person, &actions, cx).mt(px(16.)))
            });
        let is_self = self.scene.accounts.iter().any(|own| own.key == person.key.account() && own.me == person.key.id);
        let can_edit_self = is_self
            && self
                .scene
                .accounts
                .iter()
                .any(|own| own.key == person.key.account() && own.can.can_edit_profile());
        if can_edit_self {
            top = top.child(
                button("card-edit-profile", ButtonKind::Secondary, "Edit profile")
                    .w_full()
                    .mt(px(12.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close(cx);
                        crate::gui::settings::open(crate::gui::settings::Section::Accounts, cx);
                    })),
            );
        }
        if let Some((action, label)) = facts::friendship_action(person).filter(|_| !is_self) {
            let who = person.clone();
            let friendship = button("card-friendship", ButtonKind::Secondary, label).w_full();
            let friendship = if seen.online {
                friendship.on_click(cx.listener(move |this, _, _, cx| {
                    let who = who.clone();
                    this.shell_do(cx, move |shell, cx| shell.relate(&who, action, cx));
                }))
            } else {
                friendship.opacity(0.4).cursor_default()
                    .tooltip(crate::gui::components::tip(format!("{} is offline", person.key.network.label()), None))
            };
            if person.relation == protocol::Relation::Outgoing {
                top = top.children(draw::soft_line("Request sent".to_owned()));
            }
            top = top.child(friendship.mt(px(12.)));
        }
        if let Some(heads_up) = facts::heads_up(person, profile, seen.online) {
            let words = facts::heads_up_words(&heads_up, &person.name, network, crate::view::now());
            top = top.child(draw::heads_up_line(&words).mt(px(12.)));
        }
        let mut body = div().px(px(16.)).flex().flex_col().gap(px(10.));
        let mut any = false;
        if !person.bot && person.activity.is_some() && seen.online {
            let now = crate::view::now();
            let ours = own_offset(now);
            for (ix, fact) in facts::right_now(person, profile, &line, now, ours)
                .into_iter()
                .filter(|f| f.glyph != "icons/clock.svg")
                .enumerate()
            {
                any = true;
                let opens = fact.opens.clone();
                let url = fact.url.clone().filter(|_| opens.is_none());
                let title = fact.words.clone();
                let art_path = fact
                    .art
                    .as_ref()
                    .and_then(|file| self.hub.file(&person.key.account(), file, cx).path);
                let row = match art_path {
                    Some(path) => draw::fact_inline_art(("card-now", ix), path, fact.words, fact.soft),
                    None => draw::fact_inline(("card-now", ix), fact.glyph, fact.words, fact.soft),
                };
                body = body.child(
                    row.when_some(opens, |row, key| {
                        row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(move |this, _, _, cx| {
                            let (key, title) = (key.clone(), title.clone());
                            this.shell_do(cx, move |shell, cx| shell.select(key, &title, cx));
                        }))
                    })
                    .when_some(url, |row, url| {
                        row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(move |this, _, window, cx| {
                            this.close(cx);
                            let _ = this.shell.update(cx, |shell, cx| shell.open_external_link(&url, window, cx));
                        }))
                    }),
                );
            }
        }
        if let Some(profile) = profile.filter(|_| seen.online) {
            match facts::card_body(person, profile, place) {
                Some(CardBody::Role(role)) if !self.manage_roles => {
                    any = true;
                    let spaces = self.spaces(cx);
                    let icon = self.place.as_ref().map(|(key, name)| {
                        let file = spaces.iter().find(|space| space.key == *key).and_then(|space| space.icon.clone());
                        let file = file.map(|file| self.hub.file(&key.account(), &file, cx));
                        Avatar::new(key, name, 16.).shape(Plate::Place).image(file.as_ref()).build()
                    });
                    body = body.child(draw::role(&role, icon, Some(3)));
                }
                Some(CardBody::Role(_)) => {}
                Some(CardBody::About(bio)) => {
                    any = true;
                    let hide = crate::gui::streamer::is_active()
                        && crate::gui::streamer::contains_sensitive(&bio)
                        && !self.bio_revealed;
                    body = if hide {
                        body.child(sensitive_cover("card-reveal-bio", cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.bio_revealed = true;
                            cx.notify();
                        })))
                    } else {
                        body.child(div().type_ui().line_clamp(2).child(bio))
                    };
                }
                None => {}
            }
            if !profile.office.is_empty() {
                any = true;
                body = body.child(draw::fact_inline("card-office", "icons/building-2.svg", profile.office.clone(), String::new()));
            }
            let ours = own_offset(crate::view::now());
            for (ix, fact) in facts::right_now(person, Some(profile), &line, crate::view::now(), ours).into_iter().enumerate() {
                if fact.glyph == "icons/clock.svg" {
                    any = true;
                    body = body.child(draw::fact_inline(("card-time", ix), fact.glyph, fact.words, fact.soft));
                }
            }
            if let Some(manager) = profile.manager.as_ref() {
                any = true;
                let mgr_key = manager.key.clone();
                let space = self.place.as_ref().map(|(key, _)| key.clone());
                let label = format!("Reports to {}", manager.name);
                body = body.child(
                    draw::fact_inline("card-manager", "icons/user.svg", label, String::new())
                        .cursor_pointer()
                        .hover(|row| row.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let (mgr_key, space) = (mgr_key.clone(), space.clone());
                            this.shell_do(cx, move |shell, cx| shell.details_person_with(mgr_key, space, None, cx));
                        })),
                );
            }
            if self.manage_roles {
                any = true;
                body = body.child(self.member_roles(person, profile, cx));
            }
            let friend_count = profile.mutual_friends.max(profile.mutual_people.len() as u32);
            if friend_count > 0 {
                any = true;
                let (key, space, given) = (person.key.clone(), self.place.as_ref().map(|(key, _)| key.clone()), Some(profile.clone()));
                let ring = Ring::on(theme::overlay());
                let faces: Vec<Div> = profile
                    .mutual_people
                    .iter()
                    .take(3)
                    .map(|friend| person_avatar(&self.hub, friend, 18., &ring, false, cx))
                    .collect();
                body = body.child(
                    div()
                        .id("card-mutual-friends")
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .rounded(px(theme::R_CONTROL))
                        .hover(|r| r.bg(theme::hover()))
                        .children(faces)
                        .child(div().ml(px(4.)).type_small().text_color(theme::text_soft()).child(facts::mutual_friends_words(friend_count)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let (key, space, given) = (key.clone(), space.clone(), given.clone());
                            this.shell_do(cx, move |shell, cx| shell.details_person_with(key, space, given, cx));
                        })),
                );
            }
            if let Some(common) = facts::in_common(network, profile) {
                any = true;
                let spaces = self.spaces(cx);
                let account = person.key.account();
                let mut row = div().flex().items_center().gap(px(4.));
                if !profile.mutual_space_items.is_empty() {
                    for (ix, item) in profile.mutual_space_items.iter().take(facts::SPACES_SHOWN).enumerate() {
                        let known = spaces.iter().find(|space| space.key == item.key);
                        let icon = known.and_then(|space| space.icon.as_ref()).map(|icon| self.hub.file(&account, icon, cx));
                        let tip_text = if item.nickname.is_empty() {
                            item.name.clone()
                        } else {
                            format!("{} ({})", item.name, item.nickname)
                        };
                        let key = item.key.clone();
                        row = row.child(
                            div()
                                .id(("card-mutual-space", ix))
                                .cursor_pointer()
                                .rounded(px(4.))
                                .tooltip(crate::gui::components::tip(tip_text, None))
                                .child(Avatar::new(&item.key, &item.name, 20.).shape(Plate::Place).image(icon.as_ref()).build())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let key = key.clone();
                                    this.shell_do(cx, move |shell, cx| shell.go(crate::gui::sidebar::Mode::Space(key), cx));
                                })),
                        );
                    }
                } else {
                    for (ix, name) in common.names.iter().enumerate() {
                        let known = spaces.iter().find(|space| space.key.account() == account && space.name == *name);
                        let key = known.map(|space| space.key.clone()).unwrap_or_else(|| Key { id: name.clone(), ..person.key.clone() });
                        let icon = known.and_then(|space| space.icon.as_ref()).map(|icon| self.hub.file(&account, icon, cx));
                        let clickable = known.is_some();
                        let click_key = key.clone();
                        row = row.child(
                            div()
                                .id(("card-mutual-space", ix))
                                .when(clickable, |el| {
                                    el.cursor_pointer()
                                        .tooltip(crate::gui::components::tip(name.clone(), None))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let key = click_key.clone();
                                            this.shell_do(cx, move |shell, cx| shell.go(crate::gui::sidebar::Mode::Space(key), cx));
                                        }))
                                })
                                .child(Avatar::new(&key, name, 20.).shape(Plate::Place).image(icon.as_ref()).build()),
                        );
                    }
                }
                body = body.child(row.child(div().ml(px(4.)).type_small().text_color(theme::text_soft()).child(common.words)));
            }
            if !profile.connections.is_empty() {
                any = true;
                let mut conns = div().flex().flex_col();
                for (ix, conn) in profile.connections.iter().take(4).enumerate() {
                    let url = conn.url.clone();
                    let row = draw::connection_row(("card-conn", ix), conn).when(!url.is_empty(), |row| {
                        row.on_click(cx.listener(move |this, _, window, cx| {
                            let url = url.clone();
                            let _ = this.shell.update(cx, |shell, cx| shell.open_web_link(&url, window, cx));
                        }))
                    });
                    conns = conns.child(row);
                }
                body = body.child(conns);
            }
            if let Some((_, input)) = self.note_input.as_ref().filter(|(k, _)| *k == person.key) {
                any = true;
                let focused = input.focus_handle(cx).is_focused(window);
                let dirty = input.read(cx).text() != self.note_baseline;
                let status = if self.note_saving {
                    Some("Saving…")
                } else if self.note_saved && !dirty {
                    Some("Saved")
                } else {
                    None
                };
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().type_micro().text_color(theme::text_faint()).child("Note · only you see this"))
                                .children(status.map(|s| div().type_micro().text_color(theme::text_faint()).child(s))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(search_field(focused).flex_1().min_w_0().line_height(px(18.)).child(input.clone()))
                                .when(dirty && !self.note_saving, |row| {
                                    row.child(
                                        button("card-note-save", ButtonKind::Secondary, "Save")
                                            .h(px(24.))
                                            .px(px(8.))
                                            .type_small()
                                            .on_click(cx.listener(|this, _, _, cx| this.save_note(cx))),
                                    )
                                }),
                        )
                        .children(self.note_error.as_ref().map(|err| div().type_micro().text_color(theme::danger()).child(err.clone()))),
                );
            }
        } else if self.manage_roles && seen.online {
            any = true;
            body = body.child(div().type_small().text_color(theme::text_soft()).child("Loading roles…"));
        }
        let max = f32::from(window.viewport_size().height) - 16.;
        let (key, space, given) = (person.key.clone(), self.place.as_ref().map(|(key, _)| key.clone()), profile.cloned());
        let foot = div()
            .h(px(40.))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(theme::hairline())
            // CONTRACT: `panel-right` is not in the asset list; `panel-left` stands in.
            .child(button_with_icon("card-dock", ButtonKind::Ghost, "icons/panel-left.svg", "View profile").on_click(cx.listener(
                move |this, _, _, cx| {
                    let (key, space, given) = (key.clone(), space.clone(), given.clone());
                    this.shell_do(cx, move |shell, cx| shell.details_person_with(key, space, given, cx));
                },
            )))
            .child(div().pr(px(8.)).child(network_icon(network, 16.)));
        div()
            .w(px(CARD_WIDTH))
            .max_h(px(max))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_popover())
            .overflow_hidden()
            .font_family(theme::FONT)
            .type_ui()
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(head)
            .child(top)
            .when(any, |card| card.child(div().id("card-body").min_h_0().flex_shrink(1.).overflow_y_scroll().pt(px(12.)).child(body)))
            .child(div().h(px(12.)))
            .child(foot)
    }

    /// Checkboxes for roles we may give or take. A picture URL is never fetched or linked:
    /// a colour swatch and a unicode emoji are the only marks drawn beside the name.
    fn member_roles(&self, person: &Person, profile: &Profile, cx: &mut Context<Self>) -> Div {
        if !self.roles_loaded && self.roles_error.is_none() {
            return div().type_small().text_color(theme::text_soft()).child("Loading roles…");
        }
        let space_id = self.place.as_ref().map(|(key, _)| key.id.as_str()).unwrap_or("");
        let mut col = div().flex().flex_col().gap(px(4.)).child(div().type_small().text_color(theme::text_soft()).child("Roles"));
        for (ix, role) in self.space_roles.iter().enumerate() {
            if role.id == space_id {
                continue;
            }
            let held = profile.role_ids.iter().any(|id| id == &role.id);
            if !role.assignable && !held {
                continue;
            }
            let on = held;
            let mut row = div()
                .id(("card-role", ix))
                .h(px(28.))
                .px(px(6.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap(px(6.));
            if role.assignable {
                let who = person.key.clone();
                let role_id = role.id.clone();
                row = row.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_member_role(who.clone(), role_id.clone(), cx);
                }));
            } else {
                row = row.opacity(0.55).cursor_default();
            }
            row = row
                .child(crate::gui::components::glyph(
                    if on { "icons/square-check.svg" } else { "icons/square.svg" },
                    14.,
                    if on { theme::accent() } else { theme::text_soft() },
                ))
                .when(role.color != 0, |row| row.child(div().w(px(10.)).h(px(10.)).rounded(px(4.)).bg(gpui::rgb(role.color))))
                .when(!role.icon.is_empty() && !role.icon.contains("://"), |row| {
                    row.child(div().type_small().child(role.icon.clone()))
                })
                .child(div().min_w_0().truncate().type_small().text_color(theme::text()).child(role.name.clone()));
            col = col.child(row);
        }
        if let Some(error) = &self.roles_error {
            col = col.child(div().type_small().text_color(theme::danger()).child(error.clone()));
        }
        col
    }

    /// The full desired set, including roles we cannot change, so the server does not treat them as removed.
    fn toggle_member_role(&mut self, person: Key, role_id: String, cx: &mut Context<Self>) {
        if !self.roles_loaded {
            self.roles_error = Some("Loading roles…".to_owned());
            cx.notify();
            return;
        }
        let Some(held) = self.profile.as_ref().filter(|profile| profile.person.key == person).map(|profile| profile.role_ids.clone()) else {
            self.roles_error = Some("Loading roles…".to_owned());
            cx.notify();
            return;
        };
        let Some(space) = self.place.as_ref().map(|(key, _)| key.clone()) else {
            return;
        };
        let mut ids = held;
        if ids.iter().any(|id| id == &role_id) {
            ids.retain(|id| id != &role_id);
        } else {
            ids.push(role_id);
        }
        self.roles_error = None;
        let asked = self.profile_asked;
        let weak = cx.weak_entity();
        super::ask_later(cx, Request::SetMemberRoles { space, person: person.clone(), roles: ids.clone() }, move |answer, cx| {
            let _ = weak.update(cx, |this, cx| {
                if this.profile_asked != asked {
                    return;
                }
                match answer {
                    Ok(Response::Ok) => {
                        let names: Vec<String> = ids
                            .iter()
                            .filter_map(|id| this.space_roles.iter().find(|role| role.id == *id).map(|role| role.name.clone()))
                            .take(protocol::PROFILE_ROLES)
                            .collect();
                        if let Some(profile) = this.profile.as_mut().filter(|profile| profile.person.key == person) {
                            profile.role_ids = ids;
                            profile.roles = names;
                        }
                    }
                    Ok(Response::Error { detail, .. }) => this.roles_error = Some(detail),
                    Err(err) => this.roles_error = Some(err.to_string()),
                    _ => {}
                }
                cx.notify();
            });
        });
    }

    fn spaces(&self, cx: &mut Context<Self>) -> Vec<protocol::Space> {
        // Drawn outside the shell's update, so reading it here is safe.
        self.shell.upgrade().map(|shell| shell.read(cx).spaces.clone()).unwrap_or_default()
    }

    fn card_actions(&self, person: &Person, actions: &facts::Actions, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().gap(px(8.));
        if actions.request {
            let (accept, ignore) = (person.clone(), person.clone());
            row = row
                .child(button("card-accept", ButtonKind::Primary, "Accept").flex_1().on_click(cx.listener(move |this, _, _, cx| {
                    let person = accept.clone();
                    this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::Accept, cx));
                })))
                .child(button("card-ignore", ButtonKind::Secondary, "Ignore").flex_1().on_click(cx.listener(move |this, _, _, cx| {
                    let person = ignore.clone();
                    this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::Ignore, cx));
                })));
        }
        if actions.message {
            let message = person.clone();
            row = row.child(
                draw::action("card-message", ButtonKind::Primary, "icons/message-square.svg", "Message".into(), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let person = message.clone();
                        this.shell_do(cx, move |shell, cx| shell.open_person(&person, cx));
                    })),
            );
        }
        if let Some(call) = &actions.call {
            let (who, direct) = (person.clone(), self.direct.as_ref().map(|row| row.key.clone()));
            let handoff = matches!(call, Call::Handoff(_));
            let restricted = matches!(call, Call::Restricted);
            let network = person.key.network;
            let icon = if restricted {
                "icons/phone-off.svg"
            } else if handoff && network == protocol::Network::Slack {
                "icons/headphones.svg"
            } else {
                "icons/phone.svg"
            };
            let button = draw::action("card-call", ButtonKind::Secondary, icon, facts::call_label(call, network, actions.message), handoff);
            row = row.child(match call {
                Call::Restricted => button
                    .opacity(0.4)
                    .cursor_default()
                    .tooltip(crate::gui::components::tip(format!("{} restricts calls", person.name), None)),
                Call::Handoff(full) => button
                    .tooltip(crate::gui::components::tip(full.clone(), None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (who, direct) = (who.clone(), direct.clone());
                        this.shell_do(cx, move |shell, cx| match direct {
                            Some(direct) => shell.call_handoff(direct, cx),
                            // No DM held yet: open it; its header hands the call off.
                            None => shell.open_person(&who, cx),
                        });
                    })),
                Call::Native => button.on_click(cx.listener(move |this, _, _, cx| {
                    let who = who.clone();
                    this.shell_do(cx, move |shell, cx| {
                        if !only_draws() {
                            shell.call_person(&who, cx);
                        }
                    });
                })),
            });
        }
        row
    }

    /// One menu item, and what it does.
    fn item(&self, item: Item, person: Option<&Person>, conversation: Option<&Conversation>, link: Option<String>, cx: &mut Context<Self>) -> Div {
        let network = person.map(|person| person.key.network).or(conversation.map(|row| row.key.network));
        let open_words = format!("Open in {}", network.map_or("the app", |network| network.label()));
        match item {
            Item::Separator => menu_separator(),
            Item::Mute { muted } => {
                let direct = self.direct.as_ref().map(|row| row.key.clone());
                let (icon, words) = if muted { ("icons/bell.svg", "Unmute") } else { ("icons/bell-off.svg", "Mute") };
                div().child(menu_item("menu-mute", Some(icon), words, None).on_click(cx.listener(move |this, _, _, cx| {
                    this.close(cx);
                    let Some(conversation) = direct.clone().filter(|_| !only_draws()) else {
                        return;
                    };
                    let notify = if muted { Notify::All } else { Notify::Nothing };
                    if conversation.network == protocol::Network::Tiny {
                        super::info::profile::details::set_local_notify(&conversation, notify, cx);
                    } else {
                        super::ask_later(cx, Request::SetMute { conversation, notify, until: None }, |_, _| {});
                    }
                })))
            }
            Item::CopyHandle => {
                let handle = person.map(|person| person.handle.trim_start_matches('@').to_owned()).unwrap_or_default();
                div().child(menu_item("menu-copy", Some("icons/copy.svg"), "Copy username", None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(handle.clone()));
                    this.close(cx);
                })))
            }
            Item::CopyLink => {
                let link = link.unwrap_or_default();
                div().child(menu_item("menu-link", Some("icons/link.svg"), "Copy link", None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                    this.close(cx);
                })))
            }
            Item::CopyHuddleLink => {
                let huddle = conversation.and_then(facts::huddle_link).unwrap_or_default();
                div().child(menu_item("menu-huddle-link", Some("icons/link.svg"), "Copy huddle link", None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(huddle.clone()));
                    this.close(cx);
                })))
            }
            Item::CopyId => {
                let id = person.map(|person| person.key.id.clone()).or(conversation.map(|row| row.key.id.clone())).unwrap_or_default();
                div().child(menu_item("menu-id", Some("icons/copy.svg"), "Copy ID", Some("⌥")).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(id.clone()));
                    this.close(cx);
                })))
            }
            Item::OpenIn => {
                let link = link.unwrap_or_default();
                div().child(menu_item("menu-open", Some("icons/external-link.svg"), open_words, None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.open_url(&link);
                    this.close(cx);
                })))
            }
            Item::CloseDirect => {
                let direct = self.direct.as_ref().map(|row| row.key.clone());
                div().child(menu_item("menu-close", Some("icons/x.svg"), "Close conversation", None).on_click(cx.listener(move |this, _, _, cx| {
                    let direct = direct.clone();
                    this.shell_do(cx, move |shell, cx| {
                        if let Some(direct) = direct {
                            shell.close_direct(direct, cx);
                        }
                    });
                })))
            }
            Item::FriendNickname => {
                let key = person.map(|person| person.key.clone());
                let current = person.map(|person| person.name.clone()).unwrap_or_default();
                div().child(menu_item("menu-friend-nickname", Some("icons/pencil.svg"), "Set nickname…", None).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(key) = key.clone() {
                            let initial = current.clone();
                            let input = cx.new(|cx| {
                                let mut input = TextInput::new("Friend nickname", cx);
                                input.set_text(&initial, cx);
                                input
                            });
                            let person_key = key.clone();
                            cx.subscribe(&input, move |this, _, event: &InputEvent, cx| {
                                if matches!(event, InputEvent::Submit) {
                                    this.save_friend_nickname(person_key.clone(), false, cx);
                                }
                            })
                            .detach();
                            this.nick_input = Some(input);
                            this.nick_error = None;
                            this.turn_to(Float::FriendNickname(key), cx);
                        }
                    }),
                ))
            }
            Item::RemoveFriend => {
                let key = person.map(|person| person.key.clone());
                div().child(menu_item_danger("menu-remove", Some("icons/x.svg"), "Remove friend").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(key) = key.clone() {
                        this.turn_to(Float::Confirm(key, RelateAction::Remove), cx);
                    }
                })))
            }
            Item::IgnoreUser => {
                let person = person.cloned();
                div().child(menu_item("menu-ignore-user", Some("icons/eye-off.svg"), "Ignore", None).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(person) = person.clone() {
                        this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::IgnoreUser, cx));
                    }
                    this.close(cx);
                })))
            }
            Item::UnignoreUser => {
                let person = person.cloned();
                div().child(menu_item("menu-unignore-user", Some("icons/eye.svg"), "Stop ignoring", None).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(person) = person.clone() {
                        this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::UnignoreUser, cx));
                    }
                    this.close(cx);
                })))
            }
            Item::Block => {
                let key = person.map(|person| person.key.clone());
                div().child(menu_item_danger("menu-block", Some("icons/circle-alert.svg"), "Block").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(key) = key.clone() {
                        this.turn_to(Float::Confirm(key, RelateAction::Block), cx);
                    }
                })))
            }
            Item::Unblock => {
                let person = person.cloned();
                div().child(menu_item("menu-unblock", Some("icons/circle-alert.svg"), "Unblock", None).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(person) = person.clone() {
                        this.shell_do(cx, move |shell, cx| shell.relate(&person, RelateAction::Unblock, cx));
                    }
                    this.close(cx);
                })))
            }
            Item::Report => {
                let target = person.map(|p| (p.key.clone(), p.name.clone(), p.link.clone()));
                div().child(menu_item_danger("menu-report", Some("icons/triangle-alert.svg"), "Report…").on_click(cx.listener(move |this, _, _, cx| {
                    if let Some((key, name, link)) = target.clone() {
                        this.turn_to(
                            Float::Report {
                                target: protocol::ReportTarget::Person(key),
                                name,
                                link,
                                reason: protocol::ReportReason::Spam,
                                block: false,
                            },
                            cx,
                        );
                    }
                })))
            }
            Item::StartVideoChat => {
                let row = conversation.cloned();
                let words = if row.as_ref().is_some_and(|r| r.kind == protocol::ConversationKind::Channel) {
                    "Start live stream…"
                } else {
                    "Start video chat…"
                };
                div().child(menu_item("menu-start-video-chat", Some("icons/phone.svg"), words, None).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(row) = row.clone() {
                            this.shell_do(cx, move |shell, cx| {
                                shell.telegram_open_start_confirm(row.key.clone(), cx);
                            });
                        }
                    }),
                ))
            }
            Item::Leave => {
                let row = conversation.cloned();
                let words = if row.as_ref().is_some_and(|row| row.kind == protocol::ConversationKind::Group) { "Leave group" } else { "Leave" };
                div().child(menu_item_danger("menu-leave", Some("icons/log-out.svg"), words).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(row) = row.clone() {
                        this.turn_to(Float::Leave { conversation: row, silent: false }, cx);
                    }
                })))
            }
        }
    }

    /// Discord user commands for the open conversation, when this person is on that account.
    fn user_app_commands(&self, person: &Person, cx: &Context<Self>) -> Option<(Key, Vec<AppCommand>)> {
        let shell = self.shell.upgrade()?;
        let shell = shell.read(cx);
        let open = shell.snapshot.as_ref()?.open.as_ref()?;
        if open.key.network != protocol::Network::Discord || open.key.account() != person.key.account() {
            return None;
        }
        let mut commands = shell.user_commands.get(&open.key).cloned().unwrap_or_default();
        commands.retain(|command| command.kind == CommandKind::User && !command.application_id.is_empty());
        commands.sort_by(|a, b| {
            a.app_name
                .to_ascii_lowercase()
                .cmp(&b.app_name.to_ascii_lowercase())
                .then(a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
        });
        Some((open.key.clone(), commands))
    }

    /// The row is measured while painting, so the submenu is placed on the next frame.
    fn show_apps(&mut self, cx: &mut Context<Self>) {
        if self.apps {
            return;
        }
        self.apps = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    fn apps_flyout(&self, person: &Person, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.apps {
            return None;
        }
        let anchor = self.apps_anchor.get()?;
        let (channel, commands) = self.user_app_commands(person, cx)?;
        if commands.is_empty() {
            return None;
        }
        let mark = self.apps_menu.clone();
        let mut list = menu().w(px(220.)).relative().child(
            canvas(move |bounds, _, _| mark.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full(),
        );
        let mut last = String::new();
        let target = person.key.id.clone();
        for (ix, command) in commands.iter().enumerate() {
            if !command.app_name.is_empty() && command.app_name != last {
                if !last.is_empty() {
                    list = list.child(menu_separator());
                }
                list = list.child(menu_label(command.app_name.clone()));
                last = command.app_name.clone();
            }
            let (chosen, channel, target) = (command.clone(), channel.clone(), target.clone());
            list = list.child(menu_item(("person-app", ix), None, command.name.clone(), None).on_click(cx.listener(
                move |this, _, _, cx| {
                    let (chosen, channel, target) = (chosen.clone(), channel.clone(), target.clone());
                    this.shell_do(cx, move |shell, cx| shell.run_app_command(channel, chosen, target, cx));
                },
            )));
        }
        Some(placed("person-apps", Place::Beside(anchor), Layer::Menu, window, list))
    }

    fn person_menu(&self, person: &Person, seen: &Seen, alt: bool, with_profile: bool, cx: &mut Context<Self>) -> Div {
        let items = facts::person_menu(person, seen, alt, !with_profile);
        let link = (!person.link.is_empty()).then(|| person.link.clone());
        let profile = person.key.clone();
        let mut list = menu().w(px(220.));
        if with_profile {
            list = list.child(menu_item("menu-profile", Some("icons/user.svg"), "Profile", None).on_click(cx.listener(
                move |this, _, _, cx| {
                    let key = profile.clone();
                    this.open(Float::Card(key), this.open.as_ref().map(|(_, at)| *at).unwrap_or_default(), cx)
                },
            )));
            if !items.is_empty() {
                list = list.child(menu_separator());
            }
        }
        for item in items {
            list = list.child(self.item(item, Some(person), None, link.clone(), cx));
        }
        if self.user_app_commands(person, cx).is_some_and(|(_, commands)| !commands.is_empty()) {
            let open = self.apps;
            let mut row = menu_item_more("menu-apps", Some("icons/bot.svg"), "Apps", open)
                .on_hover(cx.listener(|this, over: &bool, _, cx| {
                    if *over {
                        this.show_apps(cx);
                    }
                }))
                .on_click(cx.listener(|this, _, _, cx| this.show_apps(cx)));
            if open {
                let anchor = self.apps_anchor.clone();
                row = row.relative().child(
                    canvas(move |bounds, _, _| anchor.set(Some(bounds)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                );
            }
            list = list.child(menu_separator()).child(row);
        }
        list
    }

    fn conversation_menu(&self, row: &Conversation, link: Option<String>, alt: bool, cx: &mut Context<Self>) -> Div {
        let mut list = menu().w(px(220.));
        for item in facts::conversation_menu(row, link.as_deref(), alt) {
            list = list.child(self.item(item, None, Some(row), link.clone(), cx));
        }
        list
    }

    /// The one question before something that cannot be taken back here. Nothing is sent until
    /// its second button.
    fn confirm(&self, act: Act, name: &str, network: protocol::Network, window: &Window, cx: &mut Context<Self>, then: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> Div {
        let (title, words, verb) = facts::confirm_words(act, name, network);
        let size = window.viewport_size();
        let then = std::rc::Rc::new(then);
        // A phone keeps 16 pt of screen on each side of it.
        let wide = if crate::gui::phone::active() { CONFIRM_WIDTH.min(f32::from(size.width) - 32.) } else { CONFIRM_WIDTH };
        let panel = div()
            .w(px(wide))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(div().type_title().child(title))
            .child(modal_body(words))
            .child(
                modal_buttons()
                    .child(button("confirm-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                    .child(button("confirm-do", ButtonKind::Danger, verb).on_click(cx.listener(move |this, _, _, cx| {
                        let then = then.clone();
                        this.shell_do(cx, move |shell, cx| then(shell, cx));
                    }))),
            );
        div()
            .w(size.width)
            .h(size.height)
            .relative()
            .child(scrim().id("confirm-scrim").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(80.)).child(panel))
    }

    fn confirm_leave(&self, row: &Conversation, silent: bool, window: &Window, cx: &mut Context<Self>) -> Div {
        let (title, words, verb) = facts::confirm_words(Act::Leave, &row.title, row.key.network);
        let size = window.viewport_size();
        let wide = if crate::gui::phone::active() { CONFIRM_WIDTH.min(f32::from(size.width) - 32.) } else { CONFIRM_WIDTH };
        let can_silent = row.key.network == protocol::Network::Discord && row.kind == protocol::ConversationKind::Group;
        let toggle_row = row.clone();
        let target = row.clone();
        let mut panel = div()
            .w(px(wide))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(div().type_title().child(title))
            .child(modal_body(words));
        if can_silent {
            panel = panel.child(
                div()
                    .id("confirm-leave-silent")
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(theme::text())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.turn_to(
                            Float::Leave { conversation: toggle_row.clone(), silent: !silent },
                            cx,
                        );
                    }))
                    .child(
                        div()
                            .w(px(14.))
                            .h(px(14.))
                            .flex_none()
                            .rounded(px(3.))
                            .border_1()
                            .border_color(if silent { theme::accent() } else { theme::border() })
                            .bg(if silent { theme::accent() } else { theme::canvas() })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(silent, |box_el| {
                                box_el.child(crate::gui::components::glyph("icons/check.svg", 10., theme::on_accent()))
                            }),
                    )
                    .child("Leave without telling the group"),
            );
        }
        panel = panel.child(
            modal_buttons()
                .child(button("confirm-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                .child(button("confirm-do", ButtonKind::Danger, verb).on_click(cx.listener(move |this, _, _, cx| {
                    let target = target.clone();
                    this.shell_do(cx, move |shell, cx| shell.leave(&target, silent, cx));
                }))),
        );
        div()
            .w(size.width)
            .h(size.height)
            .relative()
            .child(scrim().id("confirm-scrim").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(80.)).child(panel))
    }

    fn confirm_clear_history(&self, row: &Conversation, delete_chat: bool, revoke: bool, window: &Window, cx: &mut Context<Self>) -> Div {
        let title = if delete_chat { format!("Delete chat with {}?", row.title) } else { format!("Clear history with {}?", row.title) };
        let words = if delete_chat {
            format!("This permanently deletes your copy of the chat with {} on {} and removes it from tinyclient.", row.title, row.key.network.label())
        } else {
            format!("This permanently clears your message history in {} on {}.", row.title, row.key.network.label())
        };
        let verb = if delete_chat { "Delete chat" } else { "Clear history" };
        let size = window.viewport_size();
        let wide = if crate::gui::phone::active() { CONFIRM_WIDTH.min(f32::from(size.width) - 32.) } else { CONFIRM_WIDTH };
        let can_revoke = row.kind == protocol::ConversationKind::Direct;
        let toggle_row = row.clone();
        let target = row.clone();
        let mut panel = div()
            .w(px(wide))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(div().type_title().child(title))
            .child(modal_body(words));
        if can_revoke {
            panel = panel.child(
                div()
                    .id("confirm-revoke")
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(theme::text())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.turn_to(
                            Float::ClearHistory { conversation: toggle_row.clone(), delete_chat, revoke: !revoke },
                            cx,
                        );
                    }))
                    .child(crate::gui::components::glyph(if revoke { "icons/square-check.svg" } else { "icons/square.svg" }, 16., if revoke { theme::danger() } else { theme::text_soft() }))
                    .child(format!("Also delete for {} (cannot be undone)", row.title)),
            );
        }
        panel = panel.child(
            modal_buttons()
                .child(button("confirm-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                .child(button("confirm-do", ButtonKind::Danger, verb).on_click(cx.listener(move |this, _, _, cx| {
                    let target = target.clone();
                    this.shell_do(cx, move |shell, cx| shell.clear_history(&target, delete_chat, revoke, cx));
                }))),
        );
        div()
            .w(size.width)
            .h(size.height)
            .relative()
            .child(scrim().id("confirm-scrim").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(80.)).child(panel))
    }

    fn confirm_report(
        &self,
        target: protocol::ReportTarget,
        name: String,
        link: String,
        reason: protocol::ReportReason,
        block: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let network = target.account().network.label();
        let size = window.viewport_size();
        let wide = if crate::gui::phone::active() { CONFIRM_WIDTH.min(f32::from(size.width) - 32.) } else { CONFIRM_WIDTH };
        let mut reasons = div().mt(px(12.)).flex().flex_wrap().gap(px(6.));
        for (ix, candidate) in protocol::ReportReason::ALL.into_iter().enumerate() {
            let active = candidate == reason;
            let t = target.clone();
            let n = name.clone();
            let l = link.clone();
            reasons = reasons.child(
                div()
                    .id(("report-reason", ix))
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(px(theme::R_CHIP))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .type_small()
                    .bg(if active { theme::accent_soft() } else { theme::hover() })
                    .border_1()
                    .border_color(if active { theme::accent() } else { theme::border() })
                    .text_color(if active { theme::accent_text() } else { theme::text() })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.turn_to(
                            Float::Report { target: t.clone(), name: n.clone(), link: l.clone(), reason: candidate, block },
                            cx,
                        );
                    }))
                    .child(candidate.label()),
            );
        }
        let toggle_target = target.clone();
        let toggle_name = name.clone();
        let toggle_link = link.clone();
        let block_row = div()
            .id("report-also-block")
            .mt(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(theme::text())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.turn_to(
                    Float::Report {
                        target: toggle_target.clone(),
                        name: toggle_name.clone(),
                        link: toggle_link.clone(),
                        reason,
                        block: !block,
                    },
                    cx,
                );
            }))
            .child(crate::gui::components::glyph(
                if block { "icons/square-check.svg" } else { "icons/square.svg" },
                16.,
                if block { theme::danger() } else { theme::text_soft() },
            ))
            .child(format!("Also block {name}"));

        let send_target = target.clone();
        let fallback_link = link.clone();
        let panel = div()
            .w(px(wide))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(div().type_title().child(format!("Report {name} to {network}?")))
            .child(modal_body(format!("Choose a reason. If {network} does not accept reports from here, tinyclient will open it in {network}.")))
            .child(reasons)
            .child(block_row)
            .child(
                modal_buttons()
                    .child(button("report-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                    .child(button("report-send", ButtonKind::Danger, "Report").on_click(cx.listener(move |this, _, _, cx| {
                        let req = Request::Report {
                            target: send_target.clone(),
                            reason,
                            note: String::new(),
                            block,
                        };
                        let fallback = fallback_link.clone();
                        this.close(cx);
                        super::ask_later(cx, req, move |answer, cx| {
                            if matches!(answer, Ok(Response::Error { code: protocol::ErrorCode::Unsupported, .. })) && !fallback.is_empty() {
                                cx.open_url(&fallback);
                            }
                        });
                    }))),
            );
        div()
            .w(size.width)
            .h(size.height)
            .relative()
            .child(scrim().id("report-scrim").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(80.)).child(panel))
    }

    fn save_friend_nickname(&mut self, person: Key, clear: bool, cx: &mut Context<Self>) {
        if only_draws() {
            self.close(cx);
            return;
        }
        let nickname = if clear {
            None
        } else {
            self.nick_input
                .as_ref()
                .map(|input| input.read(cx).text().trim().to_owned())
                .filter(|s| !s.is_empty())
        };
        let req = Request::SetFriendNickname { person, nickname };
        self.nick_error = None;
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { crate::client::ask(&req) }).await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => this.close(cx),
                Ok(Response::Error { detail, .. }) => {
                    this.nick_error = Some(detail);
                    cx.notify();
                }
                Ok(_) => {
                    this.nick_error = Some("Could not save nickname.".to_owned());
                    cx.notify();
                }
                Err(err) => {
                    this.nick_error = Some(err.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_friend_nickname(&self, person: &Person, window: &Window, cx: &mut Context<Self>) -> Div {
        let size = window.viewport_size();
        let key = person.key.clone();
        let key_clear = key.clone();
        let focused = self.nick_input.as_ref().is_some_and(|i| i.focus_handle(cx).is_focused(window));
        let wide = if crate::gui::phone::active() { CONFIRM_WIDTH.min(f32::from(size.width) - 32.) } else { CONFIRM_WIDTH };
        let panel = div()
            .w(px(wide))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .flex()
            .flex_col()
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Friend nickname"))
            .child(
                div()
                    .mt(px(4.))
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(format!("Only you see this nickname for {} (@{}).", person.name, person.handle)),
            )
            .children(self.nick_input.clone().map(|input| {
                div()
                    .mt(px(12.))
                    .child(search_field(focused).w_full().line_height(px(18.)).child(input))
            }))
            .children(
                self.nick_error
                    .as_ref()
                    .map(|err| div().mt(px(6.)).type_small().text_color(theme::danger()).child(err.clone())),
            )
            .child(
                modal_buttons()
                    .child(button("nick-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
                    .child(button("nick-clear", ButtonKind::Secondary, "Clear").on_click(cx.listener(move |this, _, _, cx| {
                        this.save_friend_nickname(key_clear.clone(), true, cx);
                    })))
                    .child(button("nick-save", ButtonKind::Primary, "Save").on_click(cx.listener(move |this, _, _, cx| {
                        this.save_friend_nickname(key.clone(), false, cx);
                    }))),
            );
        div()
            .w(size.width)
            .h(size.height)
            .relative()
            .child(scrim().id("nick-scrim").on_click(cx.listener(|this, _, _, cx| this.close(cx))))
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(80.)).child(panel))
    }
}

impl Render for Floating {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((float, anchor)) = self.open.clone() else {
            return div();
        };
        // Someone seen only as a message's sender still floats: the id is all that is known.
        let person = float.person().map(|key| self.hub.person(key).unwrap_or_else(|| Person::new(key.clone(), key.id.clone())));
        let (surface, modal) = match (&float, person.as_ref()) {
            (Float::Card(_), Some(person)) => (self.card(person, window, cx), false),
            (Float::Menu(_), Some(person)) => {
                let seen = self.seen(&person.key);
                (self.person_menu(person, &seen, window.modifiers().alt, true, cx), false)
            }
            (Float::Person { seen, alt, .. }, Some(person)) => (self.person_menu(person, seen, *alt, false, cx), false),
            (Float::Conversation { conversation, link, alt }, _) => (self.conversation_menu(conversation, link.clone(), *alt, cx), false),
            (Float::Confirm(_, action), Some(person)) => {
                let (target, action) = (person.clone(), *action);
                let act = super::info::act_of(action);
                (self.confirm(act, &person.name, person.key.network, window, cx, move |shell, cx| shell.relate(&target, action, cx)), true)
            }
            (Float::FriendNickname(_), Some(person)) => (self.confirm_friend_nickname(person, window, cx), true),
            (Float::Leave { conversation, silent }, _) => {
                (self.confirm_leave(conversation, *silent, window, cx), true)
            }
            (Float::ClearHistory { conversation, delete_chat, revoke }, _) => {
                (self.confirm_clear_history(conversation, *delete_chat, *revoke, window, cx), true)
            }
            (Float::Report { target, name, link, reason, block }, _) => {
                (self.confirm_report(target.clone(), name.clone(), link.clone(), *reason, *block, window, cx), true)
            }
            _ => return div(),
        };
        // Escape goes to whatever has the keys, so the float takes them while it is up,
        // without stealing focus away from an inline input (like the note or nickname field).
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        let surface = surface.track_focus(&focus).on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                if this.apps {
                    this.apps = false;
                    this.apps_anchor.set(None);
                    this.apps_menu.set(None);
                    cx.notify();
                    return;
                }
                this.close(cx);
            }
        }));
        if modal {
            return div().child(deferred(anchored().position(point(px(0.), px(0.))).child(fade_in(("float", self.opened), 160, surface))).priority(3));
        }
        let (row_at, menu_at) = (self.apps_anchor.clone(), self.apps_menu.clone());
        let surface = surface.on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            let at = event.position;
            let on_apps = row_at.get().is_some_and(|bounds| bounds.contains(&at)) || menu_at.get().is_some_and(|bounds| bounds.contains(&at));
            if !on_apps {
                this.close(cx);
            }
        }));
        let offset = if matches!(float, Float::Card(_)) { point(px(8.), px(0.)) } else { point(px(2.), px(2.)) };
        let flyout = person.as_ref().and_then(|person| self.apps_flyout(person, window, cx));
        div()
            .child(
                deferred(anchored().position(anchor + offset).snap_to_window_with_margin(px(8.)).child(fade_in(("float", self.opened), 120, surface)))
                    .priority(2),
            )
            .children(flyout)
    }
}

#[cfg(feature = "gui-shot")]
impl Floating {
    /// A card's answer as a network would give it.
    pub fn stage_profile(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.hub.learn(std::slice::from_ref(&profile.person));
        self.profile_asked += 1;
        self.profile = Some(profile);
        cx.notify();
    }

    /// What View profile does.
    pub fn stage_dock(&mut self, cx: &mut Context<Self>) {
        let Some((Float::Card(key), _)) = self.open.clone() else {
            return;
        };
        let (space, given) = (self.place.as_ref().map(|(key, _)| key.clone()), self.profile.clone());
        self.shell_do(cx, move |shell, cx| shell.details_person_with(key, space, given, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2025-10-13 12:00 UTC.
    const NOON_UTC: i64 = 1_760_356_800;

    fn local_time(now: i64, theirs: i32, ours: i32) -> String {
        let (clock, apart) = local_time_parts(now, theirs, ours);
        format!("{clock} · {apart}")
    }

    #[test]
    fn their_clock_is_read_from_the_offset_and_set_against_ours() {
        assert_eq!(local_time(NOON_UTC + 48 * 60, 3600, 4 * 3600), "1:48 PM · 3 h behind you");
        assert_eq!(local_time(NOON_UTC, 3600, -4 * 3600), "1:00 PM · 5 h ahead of you");
        assert_eq!(local_time(NOON_UTC, -4 * 3600, -4 * 3600), "8:00 AM · same time as you");
        assert_eq!(local_time(NOON_UTC, 19_800, 0), "5:30 PM · 5 h 30 min ahead of you");
        assert_eq!(local_time(NOON_UTC, -12 * 3600 - 1800, -12 * 3600), "11:30 PM · 30 min behind you");
        assert_eq!(local_time(NOON_UTC, 12 * 3600, 0), "12:00 AM · 12 h ahead of you");
    }

    #[test]
    fn only_a_discord_friend_can_be_removed_and_nobody_is_blocked_twice() {
        let person = |network, relation| {
            let mut person = Person::new(Key { network, account: "1".into(), id: "p".into() }, "P");
            person.relation = relation;
            person
        };
        use protocol::{Network, Relation};
        assert_eq!(facts::offers(&person(Network::Discord, Relation::Friend)), (true, true));
        assert_eq!(facts::offers(&person(Network::Discord, Relation::None)), (false, true));
        assert_eq!(facts::offers(&person(Network::Discord, Relation::Blocked)), (false, false));
        assert_eq!(facts::offers(&person(Network::Telegram, Relation::Contact)), (false, true));
        assert_eq!(facts::offers(&person(Network::Slack, Relation::Contact)), (false, false));
    }
}
