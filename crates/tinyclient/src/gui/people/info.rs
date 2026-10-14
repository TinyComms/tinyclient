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
use crate::gui::components::{button, glyph, icon_button, tip, ButtonKind};
use crate::gui::layout::{RightKind, RightPane};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, App, ClipboardItem, Context, Div, Entity, MouseButton, WeakEntity, Window};
use profile::draw;
use profile::facts::{self, Act, Call, Seen, Surface};
use profile::shared::{self, Shared};
use protocol::{
    Contact, ContactKind, Conversation, ConversationInfo, Key, Link, Network, Notify, Person, Profile, RelateAction,
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
    /// What the user set here, drawn before the row says so.
    notify_set: Option<(Key, Notify)>,
    /// The content width, from the pane's.
    width: f32,
    /// The shell's Spaces, for In common and Details' icons.
    spaces: Vec<Space>,
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
            notify_set: None,
            width: 268.,
            spaces: Vec::new(),
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
        let subject = Subject::Person { key: person.clone(), space: space.clone() };
        // Their presence is drawn, so it is watched for as long as the pane shows them.
        self.hub.watch("info", [person.clone()], cx);
        let same = self.subject.as_ref() == Some(&subject);
        self.subject = Some(subject);
        self.row = direct;
        if !same {
            self.phones_shown = false;
            self.about_open = false;
        }
        let entity = cx.entity();
        self.shared.show(self.row.as_ref().map(|row| row.key.clone()), &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
        if let Some(profile) = given {
            self.profile = Answer::Got(profile);
            cx.notify();
            return;
        }
        // The same person again keeps the answer; chatd holds it five minutes in any case.
        let held = self.profile.got().is_some_and(|profile| profile.person.key == person);
        if same && held {
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
                        Answer::Got(*profile)
                    }
                    _ => Answer::Failed,
                };
                cx.notify();
            });
        });
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
        self.hub.watch("info", [], cx);
        let same = self.subject.as_ref() == Some(&Subject::Conversation(key.clone()));
        self.subject = Some(Subject::Conversation(key.clone()));
        self.row = row.or_else(|| self.row.clone().filter(|_| same));
        let entity = cx.entity();
        self.shared.show(Some(key.clone()), &entity, cx, |pane: &mut InfoPane| &mut pane.shared);
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

    fn ask_members(&mut self, conversation: Key, asked: u64, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let request = Request::Members { conversation, offset: 0, limit: facts::INLINE_MEMBERS };
        super::ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Members { members, .. }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.asked == asked {
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
        self.notify_set = Some((conversation.clone(), notify));
        cx.notify();
        if only_draws() {
            return;
        }
        let shell = self.shell.clone();
        super::ask_later(cx, Request::SetMute { conversation, notify }, move |answer, cx| {
            if let Ok(Response::Error { detail, .. }) = answer {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toast(Some(("icons/circle-alert.svg", theme::danger())), format!("Can't change notifications: {detail}"), cx)
                });
            }
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
            in_their_dm: self.scene.peer.as_ref() == Some(&person.key),
            direct: self.row.as_ref().map(|row| (row.call, self.notify_of(row, None) == Notify::Nothing)),
            online: self.online(person.key.network),
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
        let actions = facts::actions(person, &seen, Surface::Pane);

        // "…" alone never opens a row of its own at the left: it sits at the head's right.
        let lone = actions.more && !actions.message && !actions.request && actions.call.is_none();
        let more = actions.more.then(|| self.more_button(person, &seen));
        let (more_in_head, more_in_row) = if lone { (more, None) } else { (None, more) };
        let head = self.head(person, accent, calling, more_in_head, cx);
        let mut top = div()
            .px(px(theme::PANE_PAD))
            .flex()
            .flex_col()
            .child(draw::name_line(person.name.clone(), draw::mark_of(person)).mt(px(12.)))
            .children(draw::soft_line(facts::handle_line(person, profile)))
            .children(draw::presence_row(&line, theme::sidebar(), 2));
        if !lone && (actions.more || actions.message || actions.request || actions.call.is_some()) {
            top = top.child(self.person_actions(person, &actions, &seen, more_in_row, cx).mt(px(16.)));
        }
        if actions.notifications {
            if let Some(row) = self.row.clone() {
                let on = self.notify_of(&row, None) != Notify::Nothing;
                let key = row.key.clone();
                top = top.child(
                    draw::notifications_row(draw::switch("info-notify", on).on_click(cx.listener(move |this, _, _, cx| {
                        this.set_notify(key.clone(), if on { Notify::Nothing } else { Notify::All }, cx)
                    })))
                    .mt(px(12.)),
                );
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
                let title = fact.words.clone();
                sections = sections.child(
                    draw::fact_inline(("info-now", ix), fact.glyph, fact.words, fact.soft).when_some(opens, |row, key| {
                        row.cursor_pointer().hover(|row| row.bg(theme::hover())).on_click(cx.listener(move |this, _, _, cx| {
                            let (key, title) = (key.clone(), title.clone());
                            this.with_shell(cx, move |shell, cx| shell.select(key, &title, cx));
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
            let shown = facts::sections(person, profile, place, crate::view::now(), ours);
            if shown.about {
                sections = sections.child(draw::label("About", !any));
                any = true;
                sections = sections.child(self.about(profile, cx));
            }
            if shown.contact {
                let contacts = facts::contacts(person, profile);
                sections = sections.child(draw::label("Contact", !any));
                any = true;
                for (ix, contact) in contacts.into_iter().enumerate() {
                    sections = sections.child(self.contact_row(ix, contact, cx));
                }
            }
            if let Some(common) = facts::in_common(network, profile) {
                sections = sections.child(draw::label("In common", !any));
                any = true;
                sections = sections.child(self.in_common(person, &common, cx));
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

    fn head(&self, person: &Person, accent: Option<u32>, calling: bool, more: Option<AnyElement>, cx: &mut Context<Self>) -> Div {
        let ring = Ring::on(theme::sidebar());
        // The band's slot is kept with or without a colour, so nothing below moves.
        div()
            .relative()
            .h(px(16. + draw::AVATAR))
            .children(accent.map(|color| div().absolute().top_0().left_0().right_0().h(px(draw::BAND)).bg(draw::band(color, theme::sidebar()))))
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
            let enabled = seen.online;
            let (who, direct) = (person.clone(), self.row.as_ref().map(|row| row.key.clone()));
            let network = person.key.network;
            let icon = if matches!(call, Call::Handoff(_)) && network == Network::Slack { "icons/headphones.svg" } else { "icons/phone.svg" };
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
            } else {
                button
                    .opacity(0.4)
                    .cursor_default()
                    .tooltip(tip(format!("{} is offline", person.key.network.label()), None))
            });
        }
        row.children(more)
    }

    fn about(&self, profile: &Profile, cx: &mut Context<Self>) -> Div {
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
            .when(!profile.note.trim().is_empty(), |about| {
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

    fn in_common(&self, person: &Person, common: &facts::InCommon, cx: &mut Context<Self>) -> Div {
        let spaces = &self.spaces;
        let account = person.key.account();
        let mut rows = div().flex().flex_col();
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
        shared::draw(&self.shared, network, &self.hub, self.width, hooks, cx)
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
        div().size_full().child(div().id("info-scroll").size_full().overflow_y_scroll().children(body))
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
        let info = self.info_entity(cx);
        info.update(cx, |info, cx| info.show_conversation(conversation, row, cx));
        self.open_details("Details", cx);
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
    pub(in crate::gui) fn leave(&mut self, conversation: &Conversation, cx: &mut Context<Self>) {
        self.conversation_act(Request::Leave { conversation: conversation.key.clone() }, format!("Can't leave {}", conversation.title), cx);
    }

    /// Closes a DM as the network's own ×: it leaves the list until a new message.
    pub(in crate::gui) fn close_direct(&mut self, conversation: Key, cx: &mut Context<Self>) {
        self.conversation_act(Request::CloseDirect { conversation }, "Can't close this conversation".to_owned(), cx);
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
                Err(err) => Some(crate::client::trouble_words(&err)),
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
                    let (_, block) = facts::offers(&person);
                    return block.then(|| Float::Confirm(key.clone(), RelateAction::Block));
                }
                Some(Float::Person { person: key.clone(), seen, alt: false })
            }
            Subject::Conversation(_) => {
                let row = self.row.clone()?;
                if confirm {
                    return facts::can_leave(&row).then_some(Float::Leave(row));
                }
                let link = facts::conversation_link(&row, self.info.got());
                Some(Float::Conversation { conversation: row, link, alt: false })
            }
            Subject::Space(_) => None,
        }
    }
}
