//! The right pane's Details: a person ("Profile"), a conversation or a Space ("Details"), in the
//! one fixed order of `design/polish/05-profile-info.md`. Everything above the first hairline
//! is drawn from the row in the first frame; the one answer only extends the pane downward.
//!
//! For the pane frame (slice C): `Shell::details_person`, `Shell::details_conversation`,
//! `Shell::details_space` open it, and `draw_details` is the body the frame draws.

#[path = "../profile/mod.rs"]
pub(in crate::gui) mod profile;

use super::card::Float;
use super::presence::Ring;
use super::{existing_direct, in_call, only_draws, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{button, glyph, icon_button, search_field, tip, ButtonKind};
use crate::gui::input::{InputEvent, TextInput};
use crate::gui::layout::{RightKind, RightPane};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, App, ClipboardItem, Context, Div, Entity, Focusable, MouseButton, WeakEntity, Window};
use profile::draw;
use profile::facts::{self, Act, Call, Seen, Surface};
use profile::shared::{self, Shared};
use protocol::{
    AccountKey, Contact, ContactKind, Conversation, ConversationInfo, Key, Link, Network, Notify, Person, Profile, RelateAction,
    Request, Response, Space, SpaceInfo,
};
use std::time::{Duration, Instant};

/// What the pane is about.
#[derive(Clone, PartialEq)]
pub(in crate::gui) enum Subject {
    /// A person, and the Space they were opened from (its key and name), for their nickname,
    /// roles and join date there.
    Person { key: Key, space: Option<(Key, String)> },
    Conversation(Key),
    Space(Key),
}

/// An answer, or why there is none.
#[derive(Clone, Default, PartialEq)]
enum Answer<T> {
    #[default]
    Waiting,
    Got(T),
    Failed,
}

impl<T> Answer<T> {
    fn got(&self) -> Option<&T> {
        match self {
            Answer::Got(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::gui) enum ManageKind {
    Rename,
    Topic,
    Description,
    AddMembers,
    Nickname,
}

pub struct InfoPane {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    subject: Option<Subject>,
    profile: Answer<Profile>,
    /// The conversation in Details, or the DM with the person in Profile.
    row: Option<Conversation>,
    info: Answer<ConversationInfo>,
    space: Option<Space>,
    space_info: Answer<SpaceInfo>,
    /// A small group's people, listed inline.
    members: Vec<Person>,
    asked: u64,
    asked_at: Instant,
    shared: Shared,
    /// Show was clicked: the phone numbers read in full until the pane shows someone else.
    phones_shown: bool,
    about_open: bool,
    /// Mutual friends list expanded in the "In common" section.
    friends_open: bool,
    /// What the user set here, drawn before the row says so.
    notify_set: Option<(Key, Notify)>,
    /// What the user set for a Space's notifications, drawn before SpaceInfo reloads.
    space_notify_set: Option<(Key, Notify)>,
    /// Inline conversation management input (rename, topic, add people).
    manage: Option<(Key, ManageKind, Entity<crate::gui::input::TextInput>)>,
    /// Pending confirmation to remove a member: (conversation, person_id, person_name).
    pending_remove: Option<(Key, String, String)>,
    /// The content width, from the pane's.
    width: f32,
    /// The shell's Spaces, for In common and Details' icons.
    spaces: Vec<Space>,
    /// Editable private note on the person on show (`Capabilities::person_notes`).
    note_input: Option<(Key, Entity<TextInput>)>,
    note_baseline: String,
    note_saving: bool,
    note_saved: bool,
    note_error: Option<String>,
    /// True when the current conversation's age gate was already passed this session.
    age_gate_passed: bool,
}

/// The Space a person is seen from, when the open conversation is in one: what the network
/// says of them there. A DM has none.
pub(super) fn space_of(open: Option<&Conversation>, person: &Key) -> Option<(Key, String)> {
    let open = open.filter(|open| open.key.account() == person.account())?;
    let id = open.space.clone()?;
    if open.kind.is_direct() || !matches!(person.network, Network::Discord | Network::Telegram) {
        return None;
    }
    Some((Key { id, ..open.key.clone() }, open.space_title.clone()))
}

/// The override to keep after a notification change. A refusal puts the previous one back.
fn kept_notify(
    previous: Option<(Key, Notify)>,
    conversation: &Key,
    chosen: Notify,
    accepted: bool,
) -> Option<(Key, Notify)> {
    if accepted {
        Some((conversation.clone(), chosen))
    } else {
        previous
    }
}

impl InfoPane {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub) -> InfoPane {
        InfoPane {
            shell,
            hub,
            scene: Scene::default(),
            subject: None,
            profile: Answer::Waiting,
            row: None,
            info: Answer::Waiting,
            space: None,
            space_info: Answer::Waiting,
            members: Vec::new(),
            asked: 0,
            asked_at: Instant::now(),
            shared: Shared::default(),
            phones_shown: false,
            about_open: false,
            friends_open: false,
            notify_set: None,
            space_notify_set: None,
            manage: None,
            pending_remove: None,
            width: 268.,
            spaces: Vec::new(),
            note_input: None,
            note_baseline: String::new(),
            note_saving: false,
            note_saved: false,
            note_error: None,
            age_gate_passed: false,
        }
    }

    /// The person on show, if the pane is about one.
    pub fn person(&self) -> Option<&Key> {
        match &self.subject {
            Some(Subject::Person { key, .. }) => Some(key),
            _ => None,
        }
    }

    pub(in crate::gui) fn subject(&self) -> Option<&Subject> {
        self.subject.as_ref()
    }

    pub fn see(&mut self, scene: Scene, cx: &mut Context<Self>) {
        if scene != self.scene {
            self.scene = scene;
            cx.notify();
        }
    }

    fn set_width(&mut self, pane: f32) {
        self.width = (pane - 2. * theme::PANE_PAD).max(200.);
    }

    /// From the People hooks: a person, with the Space inferred from the open conversation.
    /// Those hooks run inside the shell's own update, so what the shell holds is read a moment
    /// later; the head is drawn at once.
    pub fn show(&mut self, person: Key, cx: &mut Context<Self>) {
        self.release_note(Some(&person), cx);
        let subject = Subject::Person { key: person.clone(), space: None };
        if self.subject.as_ref().and_then(|subject| match subject {
            Subject::Person { key, .. } => Some(key),
            _ => None,
        }) != Some(&person)
        {
            self.subject = Some(subject);
            self.profile = Answer::Waiting;
            self.row = None;
        }
        cx.notify();
        let (shell, this) = (self.shell.clone(), cx.weak_entity());
        cx.spawn(async move |_, cx| {
            let held = shell.read_with(cx, |shell, _| {
                let open = shell.held_rows().next().cloned();
                (space_of(open.as_ref(), &person), shell.direct_with(&person))
            });
            let Ok((space, direct)) = held else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.person() == Some(&person) {
                    this.show_person(person, space, None, direct, cx);
                }
            });
        })
        .detach();
    }

    /// Puts a person in the pane. `given` is an answer already in hand (the card's), so
    /// nothing is asked twice; `direct` is the DM with them, when one is held.
    pub(in crate::gui) fn show_person(
        &mut self,
        person: Key,
        space: Option<(Key, String)>,
        given: Option<Profile>,
        direct: Option<Conversation>,
        cx: &mut Context<Self>,
    ) {
        self.release_note(Some(&person), cx);
        let subject = Subject::Person { key: person.clone(), space: space.clone() };
        // Their presence is drawn, so it is watched for as long as the pane shows them.
        self.hub.watch("info", [person.clone()], cx);
        let same = self.subject.as_ref() == Some(&subject);
        self.subject = Some(subject);
        self.row = direct;
        if !same {
            self.phones_shown = false;
            self.about_open = false;
            self.friends_open = false;
        }
        let entity = cx.entity();
        self.shared.show(self.row.as_ref().map(|row| row.key.clone()), &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
        if let Some(profile) = given {
            self.sync_note_input(&profile.person.key, &profile.note, cx);
            self.profile = Answer::Got(profile);
            cx.notify();
            return;
        }
        // The same person again keeps the answer; chatd holds it five minutes in any case.
        let held = self.profile.got().is_some_and(|profile| profile.person.key == person);
        if same && held {
            if let Some(profile) = self.profile.got().cloned() {
                self.sync_note_input(&profile.person.key, &profile.note, cx);
            }
            cx.notify();
            return;
        }
        self.profile = Answer::Waiting;
        self.ask_profile(person, space.map(|(key, _)| key), cx);
    }

    fn ask_profile(&mut self, person: Key, space: Option<Key>, cx: &mut Context<Self>) {
        let asked = self.begin(cx);
        let this = cx.weak_entity();
        super::ask_later(cx, Request::Profile { person, space }, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.asked != asked {
                    return;
                }
                this.profile = match answer {
                    Ok(Response::Profile { profile }) => {
                        this.hub.learn(std::slice::from_ref(&profile.person));
                        this.sync_note_input(&profile.person.key, &profile.note, cx);
                        if !profile.mutual_people.is_empty() {
                            this.hub.learn(&profile.mutual_people);
                        }
                        Answer::Got(*profile)
                    }
                    _ => Answer::Failed,
                };
                cx.notify();
            });
        });
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
    /// the subject does not drop the latest text.
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
                let profile_hit = matches!(&this.profile, Answer::Got(profile) if profile.person.key == person);
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
                    if let (Ok(Response::Ok), Answer::Got(profile)) = (&answer, &mut this.profile) {
                        profile.note = saved_text;
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

    /// Sends the latest note, then forgets the editor when `next` is not that person.
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

    /// Re-fetches the open person's profile when `Change::People` signals a live update (e.g. `USER_NOTE_UPDATE`).
    pub fn refresh_profile(&mut self, account: &AccountKey, cx: &mut Context<Self>) {
        let Some(Subject::Person { key, space }) = self.subject.clone() else {
            return;
        };
        if key.account() != *account || self.note_saving {
            return;
        }
        self.ask_profile(key, space.map(|(k, _)| k), cx);
    }

    /// A new subject: counts the ask, and repaints at 150 ms (skeletons) and 1 s (Shared).
    fn begin(&mut self, cx: &mut Context<Self>) -> u64 {
        self.asked += 1;
        self.asked_at = Instant::now();
        let (asked, this) = (self.asked, cx.weak_entity());
        cx.spawn(async move |_, cx| {
            for wait in [150, 850] {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                let still = this.update(cx, |this, cx| {
                    if this.asked == asked {
                        cx.notify();
                    }
                    this.asked == asked
                });
                if !matches!(still, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        asked
    }

    /// Puts a conversation's details in the pane. `row` is drawn at once; the answer adds the rest.
    pub(in crate::gui) fn show_conversation(&mut self, key: Key, row: Option<Conversation>, cx: &mut Context<Self>) {
        self.release_note(None, cx);
        self.hub.watch("info", [], cx);
        let same = self.subject.as_ref() == Some(&Subject::Conversation(key.clone()));
        self.subject = Some(Subject::Conversation(key.clone()));
        self.row = row.or_else(|| self.row.clone().filter(|_| same));
        let gated = self.row.as_ref().is_some_and(|r| r.age_restricted) && !self.age_gate_passed;
        let entity = cx.entity();
        self.shared.show((!gated).then(|| key.clone()), &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
        if same && matches!(self.info, Answer::Got(_)) {
            cx.notify();
            return;
        }
        self.info = Answer::Waiting;
        self.members.clear();
        let asked = self.begin(cx);
        let this = cx.weak_entity();
        super::ask_later(cx, Request::ConversationInfo { conversation: key.clone() }, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.asked != asked {
                    return;
                }
                this.info = match answer {
                    Ok(Response::ConversationInfo { info }) => {
                        this.row = Some(info.conversation.clone());
                        if info.conversation.age_restricted && !this.age_gate_passed {
                            let entity = cx.entity();
                            this.shared.show(None, &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
                        }
                        let small = matches!(facts::members_shown(&info.conversation, Some(&info)), facts::MembersShown::Inline);
                        if small {
                            this.ask_members(key.clone(), asked, cx);
                        }
                        Answer::Got(*info)
                    }
                    _ => Answer::Failed,
                };
                cx.notify();
            });
        });
    }

    pub(in crate::gui) fn unlock_age_gate(&mut self, key: &Key, cx: &mut Context<Self>) {
        if self.subject.as_ref() == Some(&Subject::Conversation(key.clone())) {
            self.age_gate_passed = true;
            let entity = cx.entity();
            self.shared.show(Some(key.clone()), &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
            cx.notify();
        }
    }

    fn ask_members(&mut self, conversation: Key, asked: u64, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let request = Request::Members { conversation: conversation.clone(), offset: 0, limit: facts::INLINE_MEMBERS };
        super::ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Members { members, pending, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.asked == asked {
                    if pending {
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                            let _ = this.update(cx, |this, cx| {
                                if this.asked == asked { this.ask_members(conversation, asked, cx); }
                            });
                        }).detach();
                        return;
                    }
                    this.hub.learn(&members);
                    this.hub.watch("info", members.iter().map(|person| person.key.clone()).collect::<Vec<_>>(), cx);
                    this.members = members;
                    cx.notify();
                }
            });
        });
    }

    /// Puts a Space's own details in the pane.
    pub(in crate::gui) fn show_space(&mut self, space: Key, known: Option<Space>, cx: &mut Context<Self>) {
        self.release_note(None, cx);
        self.hub.watch("info", [], cx);
        let same = self.subject.as_ref() == Some(&Subject::Space(space.clone()));
        self.subject = Some(Subject::Space(space.clone()));
        self.space = known.or_else(|| self.space.clone().filter(|_| same));
        if same && matches!(self.space_info, Answer::Got(_)) {
            cx.notify();
            return;
        }
        self.space_info = Answer::Waiting;
        let asked = self.begin(cx);
        let this = cx.weak_entity();
        super::ask_later(cx, Request::SpaceInfo { space }, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.asked == asked {
                    this.space_info = match answer {
                        Ok(Response::SpaceInfo { info }) => {
                            this.space = Some(info.space.clone());
                            Answer::Got(*info)
                        }
                        _ => Answer::Failed,
                    };
                    cx.notify();
                }
            });
        });
    }

    /// The pane was closed: nobody in it is drawn any more.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.release_note(None, cx);
        self.hub.watch("info", [], cx);
        self.subject = None;
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        match self.subject.clone() {
            Some(Subject::Person { key, space }) => {
                self.profile = Answer::Waiting;
                self.ask_profile(key, space.map(|(key, _)| key), cx);
            }
            Some(Subject::Conversation(key)) => {
                self.info = Answer::Waiting;
                self.subject = None;
                let row = self.row.clone();
                self.show_conversation(key, row, cx);
            }
            Some(Subject::Space(key)) => {
                self.space_info = Answer::Waiting;
                self.subject = None;
                let known = self.space.clone();
                self.show_space(key, known, cx);
            }
            None => {}
        }
    }

    fn online(&self, network: Network) -> bool {
        self.scene.links.iter().find(|(net, _)| *net == network).is_none_or(|(_, link)| *link == Link::Connected)
    }

    fn waited(&self, millis: u64) -> bool {
        self.asked_at.elapsed() >= Duration::from_millis(millis)
    }

    /// What the row says of notifications, or what the user just set here.
    fn notify_of(&self, row: &Conversation, told: Option<Notify>) -> Notify {
        match &self.notify_set {
            Some((key, notify)) if *key == row.key => *notify,
            _ => told.unwrap_or(if row.muted { Notify::Nothing } else { Notify::All }),
        }
    }

    fn set_notify(&mut self, conversation: Key, notify: Notify, cx: &mut Context<Self>) {
        let previous = self.notify_set.clone();
        self.notify_set = Some((conversation.clone(), notify));
        cx.notify();
        if only_draws() {
            return;
        }
        let pane = cx.weak_entity();
        let shell = self.shell.clone();
        let saved = conversation.clone();
        super::ask_later(cx, Request::SetMute { conversation, notify, until: None }, move |answer, cx| {
            let accepted = matches!(&answer, Ok(Response::Ok));
            let _ = pane.update(cx, |this, cx| {
                let still = this.notify_set.as_ref().is_some_and(|(key, set)| key == &saved && *set == notify);
                if still {
                    this.notify_set = kept_notify(previous, &saved, notify, accepted);
                    cx.notify();
                }
            });
            if accepted {
                return;
            }
            let detail = match answer {
                Ok(Response::Error { detail, .. }) => detail,
                Err(err) => crate::client::trouble_words("save", &err),
                _ => crate::client::OTHER_ANSWER.to_owned(),
            };
            let _ = shell.update(cx, |shell, cx| {
                shell.toast(Some(("icons/circle-alert.svg", theme::danger())), format!("Can't change notifications: {detail}"), cx);
            });
        });
    }

    fn with_shell(&self, cx: &mut App, then: impl FnOnce(&mut Shell, &mut Context<Shell>)) {
        let _ = self.shell.update(cx, then);
    }

    fn copy(&self, words: String, what: &'static str, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(words));
        self.with_shell(cx, |shell, cx| shell.toast(Some(("icons/check.svg", theme::text_soft())), format!("{what} copied"), cx));
    }

    fn seen(&self, person: &Person) -> Seen {
        Seen {
            native_calls: super::can_call(&self.scene, &person.key),
            in_their_dm: self.scene.peer.as_ref() == Some(&person.key),
            direct: self.row.as_ref().map(|row| (row.call, self.notify_of(row, None) == Notify::Nothing)),
            online: self.online(person.key.network),
            close_direct: super::scene_can_close_direct(&self.scene, &person.key),
            can_block: self.scene.blocks_accounts.contains(&person.key.account()),
            can_friend_nickname: self.scene.accounts.iter().any(|own| own.key == person.key.account() && own.can.friend_nickname),
        }
    }

    // The person.

    fn person_body(&self, person: &Person, space: Option<&(Key, String)>, window: &Window, cx: &mut Context<Self>) -> Div {
        let network = person.key.network;
        let profile = self.profile.got().filter(|profile| profile.person.key == person.key);
        let seen = self.seen(person);
        let online = seen.online;
        let calling = in_call(&self.hub, &self.scene, &person.key);
        let presence = if online { shown_presence(&self.hub, person) } else { protocol::Presence::Unknown };
        let status = match self.hub.status_of(&person.key) {
            status if status.is_empty() => person.status.clone(),
            status => status,
        };
        let line = draw::presence_line_for(person, profile, presence, &status, calling);
        let place = space.map(|(_, name)| name.as_str());
        let accent = profile.and_then(|profile| profile.accent);
        let banner_path = profile
            .and_then(|p| p.banner.as_ref())
            .and_then(|file| self.hub.file(&person.key.account(), file, cx).path);
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
        let actions = facts::actions(person, &seen, Surface::Pane);

        // "…" alone never opens a row of its own at the left: it sits at the head's right.
        let lone = actions.more && !actions.message && !actions.request && actions.call.is_none();
        let more = actions.more.then(|| self.more_button(person, &seen));
        let (more_in_head, more_in_row) = if lone { (more, None) } else { (None, more) };
        let shown_person = profile.map(|p| &p.person).unwrap_or(person);
        let head = self.head(shown_person, banner_path, accent, calling, more_in_head, cx);
        let mut top = div()
            .px(px(theme::PANE_PAD))
            .flex()
            .flex_col()
            .child(draw::name_line(person.name.clone(), mark).mt(px(12.)))
            .children(profile.filter(|p| p.server_profile).and_then(|_| draw::soft_line("Server profile".to_owned())))
            .children(draw::soft_line(facts::handle_line(person, profile)))
            .children(draw::badge_strip("info-badge", &badges))
            .children(draw::presence_row(&line, theme::sidebar(), 2));
        if !lone && (actions.more || actions.message || actions.request || actions.call.is_some()) {
            top = top.child(self.person_actions(person, &actions, &seen, more_in_row, cx).mt(px(16.)));
        }
        let is_self = self.scene.accounts.iter().any(|own| own.key == person.key.account() && own.me == person.key.id);
        let can_edit_self = is_self
            && self
                .scene
                .accounts
                .iter()
                .any(|own| own.key == person.key.account() && own.can.can_edit_profile());
        if can_edit_self {
            top = top.child(
                button("info-edit-profile", ButtonKind::Secondary, "Edit profile")
                    .w_full()
                    .mt(px(12.))
                    .on_click(|_, _, cx| {
                        crate::gui::settings::open(crate::gui::settings::Section::Accounts, cx);
                    }),
            );
        }
        if let Some((action, label)) = facts::friendship_action(person).filter(|_| !is_self) {
            let who = person.clone();
            let friendship = button("info-friendship", ButtonKind::Secondary, label).w_full();
            let friendship = if seen.online {
                friendship.on_click(cx.listener(move |this, _, _, cx| {
                    let who = who.clone();
                    this.with_shell(cx, move |shell, cx| shell.relate(&who, action, cx));
                }))
            } else {
                friendship.opacity(0.4).cursor_default()
                    .tooltip(tip(format!("{} is offline", person.key.network.label()), None))
            };
            if person.relation == protocol::Relation::Outgoing {
                top = top.children(draw::soft_line("Request sent".to_owned()));
            }
            top = top.child(friendship.mt(px(12.)));
        }
        if actions.notifications {
            if let Some(row) = self.row.clone() {
                let local = row.key.network == Network::Tiny;
                let on = if local {
                    profile::details::local_notify_shown(&row) != Notify::Nothing
                } else {
                    self.notify_of(&row, None) != Notify::Nothing
                };
                let key = row.key.clone();
                top = top.child(
                    draw::notifications_row(draw::switch("info-notify", on).on_click(cx.listener(move |this, _, _, cx| {
                        let to = if on { Notify::Nothing } else { Notify::All };
                        if local {
                            profile::details::set_local_notify(&key, to, cx);
                            cx.notify();
                        } else {
                            this.set_notify(key.clone(), to, cx);
                        }
                    })))
                    .mt(px(12.)),
                );
                if local {
                    top = top.child(div().type_small().text_color(theme::text_faint()).child("Kept on this device."));
                }
            }
        }
        if network == Network::Discord && online && person.relation == protocol::Relation::Friend {
            if let Some(row) = self.row.as_ref().filter(|r| r.kind == protocol::ConversationKind::Direct) {
                let dm_key = row.key.clone();
                top = top.child(
                    div().mt(px(8.)).child(
                        button("info-dm-add-people", ButtonKind::Secondary, "Add people…")
                            .h(px(26.))
                            .px(px(8.))
                            .type_small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_manage(
                                    dm_key.clone(),
                                    ManageKind::AddMembers,
                                    "",
                                    "Friend IDs (comma-separated)",
                                    cx,
                                );
                            })),
                    ),
                );
                if let Some((mkey, ManageKind::AddMembers, input)) = self.managing() {
                    if mkey == &row.key {
                        top = top.child(
                            div()
                                .mt(px(8.))
                                .p(px(8.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(theme::raised())
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .type_micro()
                                        .text_color(theme::text_faint())
                                        .child("Only Discord friends can be added (creates a group DM, up to 10 people)."),
                                )
                                .child(input.clone())
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap(px(6.))
                                        .child(
                                            button("dm-manage-cancel", ButtonKind::Ghost, "Cancel")
                                                .h(px(24.))
                                                .px(px(8.))
                                                .type_small()
                                                .on_click(cx.listener(|this, _, _, cx| this.cancel_manage(cx))),
                                        )
                                        .child(
                                            button("dm-manage-save", ButtonKind::Primary, "Create group")
                                                .h(px(24.))
                                                .px(px(10.))
                                                .type_small()
                                                .on_click(cx.listener(|this, _, _, cx| this.submit_manage(cx))),
                                        ),
                                ),
                        );
                    }
                }
            }
        }
        if let Some(heads_up) = facts::heads_up(person, profile, online) {
            let words = facts::heads_up_words(&heads_up, &person.name, network, crate::view::now());
            // CONTRACT: `RelateAction` has no Unblock, so the Blocked banner offers none.
            top = top.child(draw::heads_up_banner(&words, None).mt(px(12.)));
        }

        let mut body = div().flex().flex_col().pb(px(theme::PANE_PAD)).child(head).child(top);
        let answered = match (&self.profile, online) {
            (_, false) => None,
            (Answer::Got(profile), _) if profile.person.key == person.key => Some(profile),
            _ => None,
        };
        let mut sections = div().px(px(theme::PANE_PAD)).flex().flex_col();
        let mut any = false;
        if let Some(profile) = answered {
            let ours = super::card::own_offset(crate::view::now());
            for (ix, fact) in facts::right_now(person, Some(profile), &line, crate::view::now(), ours).into_iter().enumerate() {
                any = true;
                let opens = fact.opens.clone();
                let url = fact.url.clone().filter(|_| opens.is_none());
                let title = fact.words.clone();
                let art_path = fact
                    .art
                    .as_ref()
                    .and_then(|file| self.hub.file(&person.key.account(), file, cx).path);
                let row = match art_path {
                    Some(path) => draw::fact_inline_art(("info-now", ix), path, fact.words, fact.soft),
                    None => draw::fact_inline(("info-now", ix), fact.glyph, fact.words, fact.soft),
                };
                sections = sections.child(
                    row.when_some(opens, |row, key| {
                        row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(move |this, _, _, cx| {
                            let (key, title) = (key.clone(), title.clone());
                            this.with_shell(cx, move |shell, cx| shell.select(key, &title, cx));
                        }))
                    })
                    .when_some(url, |row, url| {
                        row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(move |this, _, window, cx| {
                            let url = url.clone();
                            this.with_shell(cx, move |shell, cx| shell.open_external_link(&url, window, cx));
                        }))
                    }),
                );
            }
            if let Some(role) = facts::role(person, profile, place) {
                sections = sections.child(draw::label("Role", !any));
                any = true;
                let icon = space.map(|(key, name)| {
                    let file = self.spaces.iter().find(|known| known.key == *key).and_then(|known| known.icon.clone());
                    let file = file.map(|file| self.hub.file(&key.account(), &file, cx));
                    crate::gui::components::Avatar::new(key, name, 16.).shape(crate::gui::components::Shape::Place).image(file.as_ref()).build()
                });
                sections = sections.child(draw::role(&role, icon, None));
            }
            if !profile.office.is_empty() {
                if !any {
                    sections = sections.child(draw::label("Role", true));
                }
                any = true;
                sections = sections.child(draw::fact_inline("info-office", "icons/building-2.svg", profile.office.clone(), String::new()));
            }
            if profile.manager.is_some() || !profile.reports.is_empty() {
                sections = sections.child(draw::label("Organization", !any));
                any = true;
                let mut org = div().flex().flex_col().gap(px(4.));
                if let Some(manager) = profile.manager.as_ref() {
                    let mgr_key = manager.key.clone();
                    let label = if manager.title.is_empty() {
                        format!("Reports to {}", manager.name)
                    } else {
                        format!("Reports to {} · {}", manager.name, manager.title)
                    };
                    org = org.child(
                        draw::fact_inline("info-manager", "icons/user.svg", label, String::new())
                            .cursor_pointer()
                            .hover(|row| row.bg(theme::hover()))
                            .on_click(cx.listener(move |this, _, _, cx| this.show(mgr_key.clone(), cx))),
                    );
                }
                if !profile.reports.is_empty() {
                    org = org.child(
                        div()
                            .pt(px(4.))
                            .type_small()
                            .text_color(theme::text_faint())
                            .child(format!("Direct reports ({})", profile.reports.len())),
                    );
                    for (rix, report) in profile.reports.iter().enumerate() {
                        let rep_key = report.key.clone();
                        let label = if report.title.is_empty() {
                            report.name.clone()
                        } else {
                            format!("{} · {}", report.name, report.title)
                        };
                        org = org.child(
                            draw::fact_inline(("info-report", rix), "icons/users.svg", label, String::new())
                                .cursor_pointer()
                                .hover(|row| row.bg(theme::hover()))
                                .on_click(cx.listener(move |this, _, _, cx| this.show(rep_key.clone(), cx))),
                        );
                    }
                }
                sections = sections.child(org);
            }
            let shown = facts::sections(person, profile, place, crate::view::now(), ours);
            let editable_note = self.note_input.as_ref().filter(|(k, _)| *k == person.key);
            if shown.about && (editable_note.is_none() || !profile.bio.trim().is_empty()) {
                sections = sections.child(draw::label("About", !any));
                any = true;
                sections = sections.child(self.about(profile, editable_note.is_none(), cx));
            }
            if let Some((_, input)) = editable_note {
                sections = sections.child(draw::label("Note", !any));
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
                sections = sections.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().type_micro().text_color(theme::text_faint()).child("Only you see this"))
                                .children(status.map(|s| div().type_micro().text_color(theme::text_faint()).child(s))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(search_field(focused).flex_1().min_w_0().line_height(px(18.)).child(input.clone()))
                                .when(dirty && !self.note_saving, |row| {
                                    row.child(
                                        button("info-note-save", ButtonKind::Secondary, "Save")
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
            if shown.contact {
                let contacts = facts::contacts(person, profile);
                sections = sections.child(draw::label("Contact", !any));
                any = true;
                for (ix, contact) in contacts.into_iter().enumerate() {
                    sections = sections.child(self.contact_row(ix, contact, cx));
                }
            }
            let common = facts::in_common(network, profile);
            let has_mutual_friends = profile.mutual_friends > 0 || !profile.mutual_people.is_empty();
            if common.is_some() || has_mutual_friends {
                sections = sections.child(draw::label("In common", !any));
                any = true;
                sections = sections.child(self.in_common(person, profile, common.as_ref(), cx));
            }
            if !profile.connections.is_empty() {
                sections = sections.child(draw::label("Connections", !any));
                any = true;
                for (ix, conn) in profile.connections.iter().enumerate() {
                    let url = conn.url.clone();
                    let row = draw::connection_row(("info-conn", ix), conn).when(!url.is_empty(), |row| {
                        row.on_click(cx.listener(move |this, _, window, cx| {
                            let url = url.clone();
                            let _ = this.shell.update(cx, |shell, cx| shell.open_web_link(&url, window, cx));
                        }))
                    });
                    sections = sections.child(row);
                }
            }
            let safety_digits = profile
                .fields
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Safety number"))
                .map(|(_, v)| v.clone());
            let safety_status = profile
                .fields
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Safety status"))
                .map(|(_, v)| v.clone());
            let extra_fields: Vec<(String, String)> = profile
                .fields
                .iter()
                .filter(|(k, v)| {
                    !v.trim().is_empty()
                        && !k.eq_ignore_ascii_case("Safety number")
                        && !k.eq_ignore_ascii_case("Safety status")
                        && !k.eq_ignore_ascii_case("About emoji")
                })
                .cloned()
                .collect();
            if !extra_fields.is_empty() {
                sections = sections.child(draw::label("Details", !any));
                any = true;
                for (ix, (label, value)) in extra_fields.into_iter().enumerate() {
                    sections = sections.child(draw::fact(("info-field", ix), "icons/info.svg", label, value));
                }
            }
            if let Some(digits) = safety_digits.filter(|d| !d.trim().is_empty() && !is_self) {
                sections = sections.child(draw::label("Safety number", !any));
                any = true;
                sections = sections.child(self.safety_section(person, digits, safety_status.as_deref().unwrap_or(""), online, cx));
            }
        }
        let failed = matches!(self.profile, Answer::Failed) && online;
        let waiting = matches!(self.profile, Answer::Waiting) && online;
        if any {
            body = body.child(draw::rule().mt(px(16.))).child(sections.pt(px(4.)));
        } else if failed {
            body = body.child(draw::rule().mt(px(16.))).child(self.failed_line(cx));
        } else if waiting && self.waited(150) {
            body = body.child(draw::rule().mt(px(16.))).child(div().px(px(theme::PANE_PAD)).pt(px(16.)).child(draw::skeleton()));
        }
        // Shared waits for the answer (or a second), so it appends rather than inserts.
        let shared_ready = !waiting || self.waited(1000);
        if let (Some(_), true) = (self.row.as_ref(), shared_ready) {
            body = body.child(draw::rule().mt(px(16.))).child(self.shared_part(network, window, cx).mt(px(8.)));
        }
        let foot = answered.map(|profile| facts::foot(person, profile, place)).unwrap_or_default();
        let link = (!person.link.is_empty()).then(|| person.link.clone());
        if !waiting || !foot.is_empty() {
            body = body.child(
                div()
                    .px(px(theme::PANE_PAD))
                    .pt(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .children(draw::foot_line(foot))
                    .children(link.map(|url| draw::open_in("info-open", network).on_click(move |_, _, cx| cx.open_url(&url)))),
            );
        }
        body
    }

    fn head(&self, person: &Person, banner_path: Option<String>, accent: Option<u32>, calling: bool, more: Option<AnyElement>, cx: &mut Context<Self>) -> Div {
        let ring = Ring::on(theme::sidebar());
        let banner_path = banner.and_then(|file| self.hub.file(&person.key.account(), file, cx).path);
        // The band's slot is kept with or without a colour, so nothing below moves.
        div()
            .relative()
            .h(px(16. + draw::AVATAR))
            .children(draw::banner_band(banner_path, accent, theme::sidebar()))
            .child(
                div()
                    .absolute()
                    .top(px(16.))
                    .left(px(theme::PANE_PAD))
                    .child(person_avatar(&self.hub, person, draw::AVATAR, &ring, calling, cx)),
            )
            .children(more.map(draw::head_more))
    }

    /// The person's "…".
    fn more_button(&self, person: &Person, seen: &Seen) -> AnyElement {
        let (key, seen) = (person.key.clone(), seen.clone());
        let shell = self.shell.clone();
        icon_button("info-more", "icons/ellipsis.svg", false)
            .tooltip(tip("More", None))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                cx.stop_propagation();
                let alt = window.modifiers().alt;
                let float = Float::Person { person: key.clone(), seen: seen.clone(), alt };
                let _ = shell.update(cx, |shell, cx| shell.open_float(float, event.position, cx));
            })
            .into_any_element()
    }

    fn person_actions(&self, person: &Person, actions: &facts::Actions, seen: &Seen, more: Option<AnyElement>, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().items_center().gap(px(8.));
        if actions.request {
            let (accept, ignore) = (person.clone(), person.clone());
            row = row
                .child(
                    button("info-accept", ButtonKind::Primary, "Accept").flex_1().on_click(cx.listener(move |this, _, _, cx| {
                        let person = accept.clone();
                        this.with_shell(cx, move |shell, cx| shell.relate(&person, RelateAction::Accept, cx));
                    })),
                )
                .child(
                    button("info-ignore", ButtonKind::Secondary, "Ignore").flex_1().on_click(cx.listener(move |this, _, _, cx| {
                        let person = ignore.clone();
                        this.with_shell(cx, move |shell, cx| shell.relate(&person, RelateAction::Ignore, cx));
                    })),
                );
        }
        if actions.message {
            let message = person.clone();
            row = row.child(
                draw::action("info-message", ButtonKind::Primary, "icons/message-square.svg", "Message".into(), false).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let person = message.clone();
                        this.with_shell(cx, move |shell, cx| shell.open_person(&person, cx));
                    }),
                ),
            );
        }
        if let Some(call) = &actions.call {
            let restricted = matches!(call, Call::Restricted);
            let enabled = seen.online && !restricted;
            let (who, direct) = (person.clone(), self.row.as_ref().map(|row| row.key.clone()));
            let network = person.key.network;
            let icon = if restricted {
                "icons/phone-off.svg"
            } else if matches!(call, Call::Handoff(_)) && network == Network::Slack {
                "icons/headphones.svg"
            } else {
                "icons/phone.svg"
            };
            let words = facts::call_label(call, network, actions.message);
            let button = draw::action("info-call", ButtonKind::Secondary, icon, words, matches!(call, Call::Handoff(_)));
            let button = match call {
                Call::Handoff(full) if enabled => button.tooltip(tip(full.clone(), None)),
                _ => button,
            };
            let handoff = matches!(call, Call::Handoff(_));
            row = row.child(if enabled {
                button.on_click(cx.listener(move |this, _, _, cx| {
                    let (who, direct) = (who.clone(), direct.clone());
                    this.with_shell(cx, move |shell, cx| match (handoff, direct) {
                        (true, Some(direct)) => shell.call_handoff(direct, cx),
                        // No DM held yet: open it; its header hands the call off.
                        (true, None) => shell.open_person(&who, cx),
                        _ if !only_draws() => shell.call_person(&who, cx),
                        _ => {}
                    });
                }))
            } else if restricted {
                button
                    .opacity(0.4)
                    .cursor_default()
                    .tooltip(tip(format!("{} restricts calls", person.name), None))
            } else {
                button
                    .opacity(0.4)
                    .cursor_default()
                    .tooltip(tip(format!("{} is offline", person.key.network.label()), None))
            });
        }
        row.children(more)
    }

    fn about(&self, profile: &Profile, show_note: bool, cx: &mut Context<Self>) -> Div {
        let bio = profile.bio.trim().to_owned();
        let long = bio.chars().count() > 180 || bio.lines().count() > 4;
        let folded = long && !self.about_open;
        div()
            .flex()
            .flex_col()
            .when(!bio.is_empty(), |about| {
                about.child(div().type_ui().when(folded, |text| text.line_clamp(4)).child(bio)).when(long, |about| {
                    about.child(
                        div()
                            .id("info-more-bio")
                            .type_ui()
                            .text_color(theme::accent_text())
                            .cursor_pointer()
                            .child(if folded { "more" } else { "less" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.about_open = !this.about_open;
                                cx.notify();
                            })),
                    )
                })
            })
            .when(show_note && !profile.note.trim().is_empty(), |about| {
                about
                    .child(div().mt(px(8.)).type_micro().text_color(theme::text_faint()).child("Note · only you see this"))
                    .child(div().type_ui().text_color(theme::text_soft()).child(profile.note.trim().to_owned()))
            })
    }

    fn contact_row(&self, ix: usize, contact: Contact, cx: &mut Context<Self>) -> impl IntoElement {
        let phone = contact.kind == ContactKind::Phone || contact.masked;
        let masked = phone && !self.phones_shown;
        let icon = match contact.kind {
            ContactKind::Email => "icons/mail.svg",
            ContactKind::Phone => "icons/phone.svg",
            ContactKind::Other => "icons/link.svg",
        };
        let what = match contact.kind {
            ContactKind::Email => "Email",
            ContactKind::Phone => "Phone number",
            ContactKind::Other => "Contact",
        };
        let words = facts::contact_words(&contact, self.phones_shown);
        let value = contact.value.clone();
        div()
            .id(("info-contact", ix))
            .h(px(theme::ROW_ONE))
            .mx(px(-8.))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .type_ui()
            .cursor_pointer()
            .hover(|row| row.bg(theme::hover()))
            .child(glyph(icon, 16., theme::text_soft()))
            .child(div().flex_1().min_w_0().truncate().child(words))
            .when(masked, |row| {
                row.child(
                    button(("info-show", ix), ButtonKind::Ghost, "Show").h(px(24.)).px(px(6.)).type_small().on_click(cx.listener(
                        |this, _, _, cx| {
                            cx.stop_propagation();
                            this.phones_shown = true;
                            cx.notify();
                        },
                    )),
                )
            })
            // A masked number is not copied: the click that shows it is the one that reveals it.
            .on_click(cx.listener(move |this, _, _, cx| {
                if masked {
                    this.phones_shown = true;
                    cx.notify();
                } else {
                    this.copy(value.clone(), what, cx);
                }
            }))
    }

    fn in_common(&self, person: &Person, profile: &Profile, common: Option<&facts::InCommon>, cx: &mut Context<Self>) -> Div {
        let spaces = &self.spaces;
        let account = person.key.account();
        let mut rows = div().flex().flex_col();
        let friend_count = profile.mutual_friends.max(profile.mutual_people.len() as u32);
        if friend_count > 0 {
            let expandable = !profile.mutual_people.is_empty();
            let ring = Ring::on(theme::sidebar());
            let faces: Vec<Div> = profile
                .mutual_people
                .iter()
                .take(3)
                .map(|friend| person_avatar(&self.hub, friend, 18., &ring, false, cx))
                .collect();
            let toggle = div()
                .id("info-mutual-friends")
                .h(px(theme::ROW_ONE))
                .mx(px(-8.))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(8.))
                .type_ui()
                .when(faces.is_empty(), |row| row.child(glyph("icons/users.svg", 16., theme::text_soft())))
                .when(!faces.is_empty(), |row| row.child(div().flex().items_center().gap(px(2.)).children(faces)))
                .child(div().flex_1().min_w_0().truncate().child(facts::mutual_friends_words(friend_count)))
                .when(expandable, |row| {
                    let icon = if self.friends_open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" };
                    row.cursor_pointer()
                        .hover(|r| r.bg(theme::hover()))
                        .child(glyph(icon, 14., theme::text_faint()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.friends_open = !this.friends_open;
                            cx.notify();
                        }))
                });
            rows = rows.child(toggle);
            if self.friends_open && expandable {
                for (ix, friend) in profile.mutual_people.iter().enumerate() {
                    let key = friend.key.clone();
                    let ring = Ring::row(theme::sidebar(), false, "mutual-friend");
                    rows = rows.child(
                        div()
                            .id(("info-mutual-friend", ix))
                            .group("mutual-friend")
                            .h(px(theme::ROW_ONE))
                            .mx(px(-8.))
                            .pl(px(20.))
                            .pr(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .type_ui()
                            .cursor_pointer()
                            .hover(|r| r.bg(theme::hover()))
                            .child(person_avatar(&self.hub, friend, 20., &ring, false, cx))
                            .child(div().flex_1().min_w_0().truncate().child(friend.name.clone()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let key = key.clone();
                                this.with_shell(cx, move |shell, cx| shell.details_person(key, None, cx));
                            })),
                    );
                }
            }
        }
        if !profile.mutual_space_items.is_empty() {
            for (ix, item) in profile.mutual_space_items.iter().take(facts::SPACES_SHOWN).enumerate() {
                let known = spaces.iter().find(|space| space.key == item.key);
                let icon = known.and_then(|space| space.icon.as_ref()).map(|icon| self.hub.file(&account, icon, cx));
                let key = item.key.clone();
                let mut row = draw::place_row(("info-space", ix), &item.key, &item.name, icon.as_ref());
                if !item.nickname.is_empty() {
                    row = row.child(
                        div()
                            .flex_none()
                            .max_w(px(100.))
                            .truncate()
                            .type_small()
                            .text_color(theme::text_faint())
                            .child(item.nickname.clone()),
                    );
                }
                rows = rows.child(row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let key = key.clone();
                        this.with_shell(cx, move |shell, cx| shell.go(crate::gui::sidebar::Mode::Space(key), cx));
                    },
                )));
            }
            if let Some(common) = common.filter(|c| c.more > 0) {
                rows = rows.child(
                    div().h(px(theme::ROW_ONE)).flex().items_center().type_ui().text_color(theme::text_soft()).child(format!("{} more", common.more)),
                );
            }
            return rows;
        }
        let Some(common) = common else {
            return rows;
        };
        if common.names.is_empty() {
            // CONTRACT: no request names the groups behind `mutual_total`, so the count stays a line.
            return rows.child(draw::fact("info-common", "icons/users.svg", common.words.clone(), ""));
        }
        for (ix, name) in common.names.iter().enumerate() {
            let known = spaces.iter().find(|space| space.key.account() == account && space.name == *name);
            let key = known.map(|space| space.key.clone()).unwrap_or_else(|| Key { id: name.clone(), ..person.key.clone() });
            let icon = known.and_then(|space| space.icon.as_ref()).map(|icon| self.hub.file(&account, icon, cx));
            let row = draw::place_row(("info-space", ix), &key, name, icon.as_ref());
            rows = rows.child(match known {
                Some(_) => row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    this.with_shell(cx, move |shell, cx| shell.go(crate::gui::sidebar::Mode::Space(key), cx));
                })),
                None => row,
            });
        }
        if common.more > 0 {
            rows = rows.child(
                div().h(px(theme::ROW_ONE)).flex().items_center().type_ui().text_color(theme::text_soft()).child(format!("{} more", common.more)),
            );
        }
        rows
    }

    fn safety_section(&self, person: &Person, digits: String, status: &str, online: bool, cx: &mut Context<Self>) -> Div {
        let copy_digits = digits.clone();
        let changed = status.to_ascii_lowercase().contains("changed");
        let verified = status.eq_ignore_ascii_case("verified");
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .id("info-safety-digits")
                    .p(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::raised())
                    .type_small()
                    .text_color(theme::text())
                    .cursor_pointer()
                    .hover(|row| row.bg(theme::hover()))
                    .child(digits)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy(copy_digits.clone(), "Safety number", cx);
                    })),
            );
        if !status.is_empty() {
            let color = if changed { theme::danger() } else { theme::text_soft() };
            block = block.child(div().type_small().text_color(color).child(status.to_owned()));
        }
        if person.key.network == Network::Signal && online {
            let (label, prefix) = if changed {
                ("Approve new safety number", "\u{1}signal-safety-approve\u{1}")
            } else if verified {
                ("Clear verification", "\u{1}signal-safety-unverify\u{1}")
            } else {
                ("Mark as verified", "\u{1}signal-safety-verify\u{1}")
            };
            let cmd = format!("{prefix}{}", person.key.id);
            let account = person.key.account();
            block = block.child(
                button("info-safety-action", ButtonKind::Secondary, label)
                    .w_full()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let pane = cx.weak_entity();
                        let shell = this.shell.clone();
                        let request = Request::Handoff {
                            conversation: Key::new(&account, cmd.clone()),
                        };
                        super::ask_later(cx, request, move |answer, cx| {
                            if matches!(answer, Ok(Response::Link { .. })) {
                                let _ = pane.update(cx, |this, cx| this.retry(cx));
                            } else if let Ok(Response::Error { detail, .. }) = answer {
                                let _ = shell.update(cx, |shell, cx| {
                                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                                });
                            }
                        });
                    })),
            );
        }
        block
    }

    fn failed_line(&self, cx: &mut Context<Self>) -> Div {
        div()
            .px(px(theme::PANE_PAD))
            .pt(px(12.))
            .flex()
            .items_center()
            .justify_between()
            .type_small()
            .text_color(theme::text_soft())
            .child("Couldn't load the rest")
            .child(
                button("info-retry", ButtonKind::Ghost, "Retry")
                    .h(px(24.))
                    .px(px(6.))
                    .type_small()
                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
            )
    }

    /// The pane draws "Open in" at its foot: a person with a link, or a conversation whose
    /// network has a foot and a link.
    fn foot_opens(&self, network: Network) -> bool {
        match &self.subject {
            Some(Subject::Person { key, .. }) => self.hub.person(key).is_some_and(|person| !person.link.is_empty()),
            _ => facts::foot_opens(network) && self.row.as_ref().and_then(|row| facts::conversation_link(row, self.info.got())).is_some(),
        }
    }

    fn shared_part(&self, network: Network, _: &Window, cx: &mut Context<Self>) -> Div {
        if self.row.as_ref().is_some_and(|row| row.age_restricted) && !self.age_gate_passed {
            return div()
                .px(px(theme::PANE_PAD))
                .py(px(12.))
                .type_small()
                .text_color(theme::text_faint())
                .child("Confirm the age warning to view shared media.");
        }
        let entity = cx.entity();
        let (pick_to, search_to) = (entity.clone(), entity.clone());
        let link = self.row.as_ref().and_then(|row| facts::conversation_link(row, self.info.got()));
        let hooks = shared::Hooks {
            pick: Box::new(move |tab, _, cx| {
                let entity = pick_to.clone();
                pick_to.update(cx, |pane, cx| pane.shared.pick(tab, &entity, cx, |pane: &mut InfoPane| &mut pane.shared));
            }),
            search: Box::new(move |_, cx| {
                let entity = search_to.clone();
                search_to.update(cx, |pane, cx| pane.shared.ask(true, &entity, cx, |pane: &mut InfoPane| &mut pane.shared));
            }),
            // The pane's foot says "Open in" whenever it can; Shared does not say it again.
            open_in: link.filter(|_| !self.foot_opens(network)).map(|url| Box::new(move |_: &mut Window, cx: &mut App| cx.open_url(&url)) as shared::Hook),
        };
        let has_pins = self
            .row
            .as_ref()
            .is_some_and(|row| self.scene.pins_accounts.contains(&row.key.account()));
        shared::draw(&self.shared, network, has_pins, &self.hub, self.width, hooks, cx)
    }
}

impl Render for InfoPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.subject.clone() {
            None => None,
            Some(Subject::Person { key, space }) => {
                let profiled = self.profile.got().filter(|profile| profile.person.key == key).map(|profile| profile.person.clone());
                // Someone seen only as a message's sender is still a profile: the id is all that is known.
                let person = self.hub.person(&key).or(profiled).unwrap_or_else(|| Person::new(key.clone(), key.id.clone()));
                Some(self.person_body(&person, space.as_ref(), window, cx))
            }
            Some(Subject::Conversation(_)) => self.row.clone().map(|row| profile::details::conversation(self, &row, window, cx)),
            Some(Subject::Space(key)) => Some(profile::details::space(self, &key, cx)),
        };
        div().size_full().child(crate::gui::components::scroll::scroll_page("info-scroll-frame", div().id("info-scroll").size_full().overflow_y_scroll().children(body)))
    }
}

/// What `details` reads of the pane: the hub, the scene, the answers, the Space and the
/// inline members.
type Parts<'a> = (&'a Hub, &'a Scene, Option<&'a ConversationInfo>, Option<&'a SpaceInfo>, Option<&'a Space>, &'a [Person]);

impl InfoPane {
    pub(in crate::gui) fn parts(&self) -> Parts<'_> {
        (&self.hub, &self.scene, self.info.got(), self.space_info.got(), self.space.as_ref(), &self.members)
    }

    pub(in crate::gui) fn waiting(&self) -> (bool, bool, bool) {
        let waiting = match self.subject {
            Some(Subject::Space(_)) => matches!(self.space_info, Answer::Waiting),
            _ => matches!(self.info, Answer::Waiting),
        };
        let failed = match self.subject {
            Some(Subject::Space(_)) => matches!(self.space_info, Answer::Failed),
            _ => matches!(self.info, Answer::Failed),
        };
        (waiting, failed, self.waited(150))
    }

    pub(in crate::gui) fn notify_shown(&self, row: &Conversation) -> Notify {
        self.notify_of(row, self.info.got().map(|info| info.notify))
    }

    pub(in crate::gui) fn notify_to(&mut self, conversation: Key, notify: Notify, cx: &mut Context<Self>) {
        self.set_notify(conversation, notify, cx);
    }

    pub(in crate::gui) fn space_notify_shown(&self, space: &Key, told: Notify) -> Notify {
        match &self.space_notify_set {
            Some((key, notify)) if key == space => *notify,
            _ => told,
        }
    }

    pub(in crate::gui) fn space_notify_to(&mut self, space: Key, notify: Notify, cx: &mut Context<Self>) {
        let previous = self.space_notify_set.clone();
        self.space_notify_set = Some((space.clone(), notify));
        cx.notify();
        if only_draws() {
            return;
        }
        let pane = cx.weak_entity();
        let shell = self.shell.clone();
        let saved = space.clone();
        super::ask_later(
            cx,
            Request::SetSpaceNotify { space, notify, until: None, category: None },
            move |answer, cx| {
                let accepted = matches!(&answer, Ok(Response::Ok));
                let _ = pane.update(cx, |this, cx| {
                    let still = this.space_notify_set.as_ref().is_some_and(|(key, set)| key == &saved && *set == notify);
                    if still {
                        this.space_notify_set = kept_notify(previous, &saved, notify, accepted);
                        if accepted {
                            if let Answer::Got(info) = &mut this.space_info {
                                if info.space.key == saved {
                                    info.notify = notify;
                                }
                            }
                        }
                        cx.notify();
                    }
                });
                if accepted {
                    return;
                }
                let detail = match answer {
                    Ok(Response::Error { detail, .. }) => detail,
                    Err(err) => crate::client::trouble_words("save", &err),
                    _ => crate::client::OTHER_ANSWER.to_owned(),
                };
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), format!("Can't change notifications: {detail}"), cx);
                });
            },
        );
    }

    pub(in crate::gui) fn shared_of(&self, network: Network, window: &Window, cx: &mut Context<Self>) -> Div {
        self.shared_part(network, window, cx)
    }

    pub(in crate::gui) fn failed_of(&self, cx: &mut Context<Self>) -> Div {
        self.failed_line(cx)
    }

    pub(in crate::gui) fn shell_do(&self, cx: &mut App, then: impl FnOnce(&mut Shell, &mut Context<Shell>)) {
        self.with_shell(cx, then);
    }

    pub(in crate::gui) fn online_of(&self, network: Network) -> bool {
        self.online(network)
    }

    pub(in crate::gui) fn shell_weak(&self) -> WeakEntity<Shell> {
        self.shell.clone()
    }

    pub(in crate::gui) fn managing(&self) -> Option<&(Key, ManageKind, Entity<crate::gui::input::TextInput>)> {
        self.manage.as_ref()
    }

    pub(in crate::gui) fn pending_remove(&self) -> Option<&(Key, String, String)> {
        self.pending_remove.as_ref()
    }

    pub(in crate::gui) fn start_manage(
        &mut self,
        conversation: Key,
        kind: ManageKind,
        initial: &str,
        placeholder: &'static str,
        cx: &mut Context<Self>,
    ) {
        let initial = initial.to_owned();
        let input = cx.new(|cx| {
            let mut field = crate::gui::input::TextInput::new(placeholder, cx);
            if !initial.is_empty() {
                field.set_text(&initial, cx);
            }
            field
        });
        self.manage = Some((conversation, kind, input));
        cx.notify();
    }

    pub(in crate::gui) fn cancel_manage(&mut self, cx: &mut Context<Self>) {
        self.manage = None;
        cx.notify();
    }

    pub(in crate::gui) fn submit_manage(&mut self, cx: &mut Context<Self>) {
        let Some((conversation, kind, input)) = self.manage.take() else {
            return;
        };
        let value = input.read(cx).text().trim().to_owned();
        let request = match kind {
            ManageKind::Rename if !value.is_empty() => Request::ConversationUpdate {
                conversation,
                title: Some(value),
                topic: None,
                description: None,
            },
            ManageKind::Topic => Request::ConversationUpdate {
                conversation,
                title: None,
                topic: Some(value),
                description: None,
            },
            ManageKind::Description => Request::ConversationUpdate {
                conversation,
                title: None,
                topic: None,
                description: Some(value),
            },
            ManageKind::AddMembers => {
                let people: Vec<String> = value
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                if people.is_empty() {
                    cx.notify();
                    return;
                }
                if let Some(row) = self.row.as_ref().filter(|r| {
                    r.key == conversation
                        && r.kind == protocol::ConversationKind::Direct
                        && r.key.network == Network::Discord
                }) {
                    let account = row.key.account();
                    let mut keys: Vec<Key> = Vec::new();
                    if let Some(peer) = row.peer.as_deref().filter(|p| !p.is_empty()) {
                        keys.push(Key::new(&account, peer));
                    }
                    for id in people {
                        let k = Key::new(&account, id);
                        if !keys.contains(&k) {
                            keys.push(k);
                        }
                    }
                    let shell = self.shell.clone();
                    super::ask_later(cx, Request::StartGroup { account, people: keys, title: None }, move |answer, cx| {
                        match answer {
                            Ok(Response::Conversation { conversation }) => {
                                let _ = shell.update(cx, |shell, cx| {
                                    shell.select(conversation.key, &conversation.title, cx);
                                });
                            }
                            Ok(Response::Error { detail, .. }) => {
                                let _ = shell.update(cx, |shell, cx| {
                                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                                });
                            }
                            _ => {}
                        }
                    });
                    cx.notify();
                    return;
                }
                Request::MembersAdd { conversation, people }
            }
            ManageKind::Nickname => {
                let space = conversation.clone();
                let req = Request::SetNickname {
                    space: space.clone(),
                    nickname: (!value.is_empty()).then_some(value),
                };
                let this = cx.weak_entity();
                super::ask_later(cx, req, move |answer, cx| {
                    if matches!(answer, Ok(Response::Ok)) {
                        let _ = this.update(cx, |this, cx| {
                            if this.subject == Some(Subject::Space(space.clone())) {
                                this.space_info = Answer::Waiting;
                                let known = this.space.clone();
                                this.show_space(space, known, cx);
                            }
                        });
                    }
                });
                cx.notify();
                return;
            }
            _ => {
                cx.notify();
                return;
            }
        };
        let shell = self.shell.clone();
        super::ask_later(cx, request, move |answer, cx| {
            if let Ok(Response::Error { detail, .. }) = answer {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                });
            }
        });
        cx.notify();
    }

    pub(in crate::gui) fn ask_remove_member(&mut self, conversation: Key, person_id: String, person_name: String, cx: &mut Context<Self>) {
        self.pending_remove = Some((conversation, person_id, person_name));
        cx.notify();
    }

    pub(in crate::gui) fn cancel_remove_member(&mut self, cx: &mut Context<Self>) {
        self.pending_remove = None;
        cx.notify();
    }

    pub(in crate::gui) fn confirm_remove_member(&mut self, cx: &mut Context<Self>) {
        let Some((conversation, person, _)) = self.pending_remove.take() else {
            return;
        };
        let shell = self.shell.clone();
        super::ask_later(cx, Request::MembersRemove { conversation, person }, move |answer, cx| {
            if let Ok(Response::Error { detail, .. }) = answer {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                });
            }
        });
        cx.notify();
    }

    pub(in crate::gui) fn pick_conversation_picture(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let asked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose group picture".into()),
        });
        let shell = self.shell.clone();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(paths))) = asked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let req = Request::ConversationPicture {
                conversation,
                path: path.to_string_lossy().into_owned(),
            };
            let answer = cx.background_executor().spawn(async move { crate::client::ask(&req) }).await;
            if let Ok(Response::Error { detail, .. }) = answer {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                });
            }
        })
        .detach();
    }

    pub(in crate::gui) fn clear_conversation_picture(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let shell = self.shell.clone();
        super::ask_later(cx, Request::ConversationPicture { conversation, path: String::new() }, move |answer, cx| {
            if let Ok(Response::Error { detail, .. }) = answer {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                });
            }
        });
    }

    pub(in crate::gui) fn transfer_owner(&mut self, conversation: Key, person: String, cx: &mut Context<Self>) {
        let shell = self.shell.clone();
        super::ask_later(cx, Request::TransferOwner { conversation, person }, move |answer, cx| {
            if let Ok(Response::Error { detail, .. }) = answer {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                });
            }
        });
    }
}

/// The body the pane frame draws for Details, whichever subject it holds.
pub(in crate::gui) fn draw_details(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    shell.people_state.attach(cx);
    let scene = shell.scene();
    let width = shell.layout_state.right_width;
    let info = shell.details_entity(cx);
    let spaces = &shell.spaces;
    info.update(cx, |info, cx| {
        info.set_width(width);
        if info.spaces != *spaces {
            info.spaces = spaces.clone();
        }
        info.see(scene, cx);
    });
    info.into_any_element()
}

impl Shell {
    pub(in crate::gui) fn details_entity(&self, cx: &mut Context<Self>) -> Entity<InfoPane> {
        self.info_entity(cx)
    }

    fn open_details(&mut self, title: &'static str, cx: &mut Context<Self>) {
        let same = self.layout_state.right.as_ref().is_some_and(|pane| pane.kind == RightKind::Info && pane.title.as_ref() == title);
        if !same {
            self.open_right(RightPane { kind: RightKind::Info, title: title.into(), count: None, body: draw_details }, cx);
        }
        cx.notify();
    }

    /// A person in the pane ("Profile"). `space`: the Space it was opened from (its tree or
    /// member list), for their nickname, roles and join date there; `None` from a DM.
    pub(in crate::gui) fn details_person(&mut self, person: Key, space: Option<Key>, cx: &mut Context<Self>) {
        self.details_person_with(person, space, None, cx);
    }

    /// The same, with an answer already in hand (the card's "View profile"): no second request.
    pub(in crate::gui) fn details_person_with(&mut self, person: Key, space: Option<Key>, given: Option<Profile>, cx: &mut Context<Self>) {
        self.people_state.attach(cx);
        let space = space.map(|key| {
            let name = self.spaces.iter().find(|known| known.key == key).map(|known| known.name.clone()).unwrap_or_default();
            (key, name)
        });
        let direct = self.direct_with(&person);
        let info = self.info_entity(cx);
        info.update(cx, |info, cx| info.show_person(person, space, given, direct, cx));
        self.open_details("Profile", cx);
    }

    /// A conversation's details: a group DM, a channel, a Telegram group or channel. A direct
    /// conversation shows the other person instead.
    pub(in crate::gui) fn details_conversation(&mut self, conversation: Key, cx: &mut Context<Self>) {
        self.people_state.attach(cx);
        let row = self.held_rows().find(|row| row.key == conversation).cloned();
        if let Some(peer) = row.as_ref().filter(|row| row.kind == protocol::ConversationKind::Direct).and_then(super::peer_of) {
            return self.details_person(peer, None, cx);
        }
        let passed = self.age_gate_passed(&conversation);
        let info = self.info_entity(cx);
        info.update(cx, |info, cx| {
            info.age_gate_passed = passed;
            info.show_conversation(conversation, row, cx);
        });
        self.open_details("Details", cx);
    }

    pub(in crate::gui) fn info_age_gate_passed(&mut self, key: &Key, cx: &mut Context<Self>) {
        let info = self.info_entity(cx);
        info.update(cx, |info, cx| info.unlock_age_gate(key, cx));
    }

    /// A Space's own details, opened from its name above its tree.
    /// The hook for the Space's name above its tree (`sidebar.rs`, `layout.rs`'s Space menu):
    /// a click there calls this.
    // CONTRACT: the sidebar owns that click; until it calls this, only staging does.
    #[cfg_attr(not(feature = "gui-shot"), allow(dead_code))]
    pub(in crate::gui) fn details_space(&mut self, space: Key, cx: &mut Context<Self>) {
        self.people_state.attach(cx);
        let known = self.spaces.iter().find(|known| known.key == space).cloned();
        let info = self.info_entity(cx);
        info.update(cx, |info, cx| info.show_space(space, known, cx));
        self.open_details("Details", cx);
    }

    /// Opens a floating menu or confirm of the People views at `anchor`.
    pub(in crate::gui) fn open_float(&mut self, float: Float, anchor: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.people_state.attach(cx);
        let floating = self.floating_entity(cx);
        floating.update(cx, |floating, cx| floating.open(float, anchor, cx));
        cx.notify();
    }

    /// Leaves a group, after its one confirm. The row goes when the network agrees.
    pub(in crate::gui) fn leave(&mut self, conversation: &Conversation, silent: bool, cx: &mut Context<Self>) {
        self.conversation_act(
            Request::Leave { conversation: conversation.key.clone(), silent },
            format!("Can't leave {}", conversation.title),
            cx,
        );
    }

    /// Where the network's own app opens a row, and whether the user can leave it: what a row's
    /// sheet offers on a phone.
    pub(in crate::gui) fn row_reach(&self, row: &Conversation) -> (Option<String>, bool) {
        (facts::conversation_link(row, None), facts::can_leave(row))
    }

    /// Asks once, then leaves: the same confirm Details' "…" asks, from a row's sheet.
    pub(in crate::gui) fn confirm_leave(&mut self, conversation: Conversation, cx: &mut Context<Self>) {
        self.open_float(Float::Leave { conversation, silent: false }, gpui::Point::default(), cx);
    }

    /// A row's notifications, from its menu: everything, or nothing. The row changes when the
    /// network says so (or immediately in local settings for Tiny).
    pub(in crate::gui) fn set_row_mute(&mut self, conversation: Key, mute: bool, cx: &mut Context<Self>) {
        self.set_row_mute_until(conversation, mute, None, cx);
    }

    /// Mute or unmute a conversation, optionally until `until` (Unix seconds; `None` is endless).
    pub(in crate::gui) fn set_row_mute_until(&mut self, conversation: Key, mute: bool, until: Option<i64>, cx: &mut Context<Self>) {
        let notify = if mute { Notify::Nothing } else { Notify::All };
        if conversation.network == Network::Tiny {
            profile::details::set_local_notify(&conversation, notify, cx);
            cx.notify();
            return;
        }
        self.conversation_act(Request::SetMute { conversation, notify, until }, "Can't change notifications".to_owned(), cx);
    }

    /// Sets a Space's (or category's) notifications or timed mute from a menu.
    pub(in crate::gui) fn set_space_notify(
        &mut self,
        space: Key,
        notify: Notify,
        until: Option<i64>,
        category: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.conversation_act(
            Request::SetSpaceNotify { space, notify, until, category },
            "Can't change notifications".to_owned(),
            cx,
        );
    }

    /// Closes a DM as the network's own ×: it leaves the list until a new message.
    pub(in crate::gui) fn close_direct(&mut self, conversation: Key, cx: &mut Context<Self>) {
        self.conversation_act(Request::CloseDirect { conversation }, "Can't close this conversation".to_owned(), cx);
    }

    /// Asks once before permanently clearing a chat's history or deleting the chat on the network.
    pub(in crate::gui) fn confirm_clear_history(&mut self, conversation: Conversation, delete_chat: bool, cx: &mut Context<Self>) {
        self.open_float(Float::ClearHistory { conversation, delete_chat, revoke: false }, gpui::Point::default(), cx);
    }

    pub(in crate::gui) fn clear_history(&mut self, conversation: &Conversation, delete_chat: bool, revoke: bool, cx: &mut Context<Self>) {
        let failed = if delete_chat {
            format!("Can't delete {}", conversation.title)
        } else {
            format!("Can't clear {}", conversation.title)
        };
        self.conversation_act(
            Request::ClearHistory { conversation: conversation.key.clone(), delete_chat, revoke },
            failed,
            cx,
        );
    }

    fn conversation_act(&mut self, request: Request, failed: String, cx: &mut Context<Self>) {
        if only_draws() {
            return;
        }
        let this = cx.weak_entity();
        super::ask_later(cx, request, move |answer, cx| {
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(crate::client::trouble_words("people", &err)),
            };
            if let Some(why) = refused {
                let _ = this.update(cx, |shell, cx| shell.toast(Some(("icons/circle-alert.svg", theme::danger())), format!("{failed}: {why}"), cx));
            }
        });
    }

    /// The DM with a person, among the rows held, with how it calls and whether it is muted.
    pub(in crate::gui) fn direct_with(&self, person: &Key) -> Option<Conversation> {
        existing_direct(self.held_rows(), person).cloned()
    }
}

/// Which confirm a destructive item asks, and of whom.
pub(in crate::gui) fn act_of(action: RelateAction) -> Act {
    match action {
        RelateAction::Block => Act::Block,
        _ => Act::RemoveFriend,
    }
}

#[cfg(feature = "gui-shot")]
impl InfoPane {
    pub fn stage_tab(&mut self, tab: shared::Tab, cx: &mut Context<Self>) {
        let entity = cx.entity();
        self.shared.pick(tab, &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
    }

    pub fn stage_search(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity();
        self.shared.ask(true, &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
    }

    pub fn stage_phones(&mut self, cx: &mut Context<Self>) {
        self.phones_shown = true;
        cx.notify();
    }

    /// The "…" menu of what is on show, or its destructive item's confirm.
    pub fn stage_float(&self, confirm: bool) -> Option<Float> {
        match self.subject.as_ref()? {
            Subject::Person { key, .. } => {
                let person = self.hub.person(key)?;
                let seen = self.seen(&person);
                if confirm {
                    let can_block = self.scene.blocks_accounts.contains(&person.key.account());
                    let (_, block) = facts::offers_with(&person, can_block);
                    return block.then(|| Float::Confirm(key.clone(), RelateAction::Block));
                }
                Some(Float::Person { person: key.clone(), seen, alt: false })
            }
            Subject::Conversation(_) => {
                let row = self.row.clone()?;
                if confirm {
                    return facts::can_leave(&row).then_some(Float::Leave { conversation: row, silent: false });
                }
                let link = facts::conversation_link(&row, self.info.got());
                Some(Float::Conversation { conversation: row, link, alt: false })
            }
            Subject::Space(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key {
            network: Network::Slack,
            account: "T".into(),
            id: "C".into(),
        }
    }

    #[test]
    fn a_notification_change_keeps_the_choice_only_when_it_was_saved() {
        let previous = Some((key(), Notify::All));
        assert_eq!(
            kept_notify(previous.clone(), &key(), Notify::Nothing, true),
            Some((key(), Notify::Nothing))
        );
        assert_eq!(
            kept_notify(previous.clone(), &key(), Notify::Nothing, false),
            previous
        );
        assert_eq!(kept_notify(None, &key(), Notify::Mentions, false), None);
    }
}
