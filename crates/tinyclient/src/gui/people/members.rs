//! The member list of the open conversation, in the right pane. Only the rows on screen are
//! ever asked for: the sections and their counts say how tall the list is, and each visible
//! stretch is one `Members` page.

use super::presence::Ring;
use super::{in_call, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{empty_state, skeleton};
use super::info::Subject;
use crate::gui::pane::Layer;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, Div, MouseButton, Stateful,
    UniformListScrollHandle, WeakEntity, Window,
};
use protocol::{Key, MemberSection, Person, Presence, Request, Response};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

pub const SOURCE: &str = "members";
const ROW: f32 = theme::ROW_ONE;
const PAGE: u32 = 50;
/// Pages of members held; the ones farthest from the viewport go first.
const PAGES_KEPT: usize = 8;
/// Placeholder rows before the first answer says how many there are.
const WAITING_ROWS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// A section's header, by its index.
    Header(usize),
    /// A member, by their place in the whole list.
    Member(u32),
}

/// Rows the list has: a header and its people for every section that holds anyone.
pub fn slots(sections: &[MemberSection]) -> usize {
    sections.iter().filter(|section| section.count > 0).map(|section| 1 + section.count as usize).sum()
}

/// What row `ix` of the list is. Offsets are a running sum of the counts, so no row is measured.
pub fn slot(sections: &[MemberSection], ix: usize) -> Option<Slot> {
    let (mut row, mut member) = (0usize, 0u32);
    for (index, section) in sections.iter().enumerate().filter(|(_, section)| section.count > 0) {
        if ix == row {
            return Some(Slot::Header(index));
        }
        let inside = ix - row - 1;
        if inside < section.count as usize {
            return Some(Slot::Member(member + inside as u32));
        }
        row += 1 + section.count as usize;
        member += section.count;
    }
    None
}

pub struct MemberList {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    conversation: Option<Key>,
    sections: Vec<MemberSection>,
    pages: HashMap<u32, Vec<Rc<Person>>>,
    asked: HashSet<u32>,
    loaded: bool,
    refused: Option<String>,
    epoch: u64,
    scroll: UniformListScrollHandle,
}

impl MemberList {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub, _: &mut Context<Self>) -> MemberList {
        MemberList {
            shell,
            hub,
            scene: Scene::default(),
            conversation: None,
            sections: Vec::new(),
            pages: HashMap::new(),
            asked: HashSet::new(),
            loaded: false,
            refused: None,
            epoch: 0,
            scroll: UniformListScrollHandle::new(),
        }
    }

    // Read by `people.rs`'s old members toggle, which the right pane replaced.
    #[allow(dead_code)]
    pub fn total(&self) -> Option<u32> {
        self.loaded.then(|| self.sections.iter().map(|section| section.count).sum())
    }

    pub fn see(&mut self, scene: Scene, cx: &mut Context<Self>) {
        let moved = self.conversation != scene.open;
        self.scene = scene;
        if moved {
            self.conversation = self.scene.open.clone();
            self.reset();
            self.want(0, cx);
        }
    }

    fn reset(&mut self) {
        self.epoch += 1;
        self.sections.clear();
        self.pages.clear();
        self.asked.clear();
        self.loaded = false;
        self.refused = None;
    }

    /// Rows of this conversation's list were added, removed or moved.
    pub fn members_changed(&mut self, conversation: &Key, cx: &mut Context<Self>) {
        if self.conversation.as_ref() == Some(conversation) {
            self.read_again(cx);
        }
    }

    /// Asks again for the pages held, which are the ones around the viewport. The rows stay
    /// as they are until the answers land.
    fn read_again(&mut self, cx: &mut Context<Self>) {
        if !self.loaded {
            return;
        }
        self.epoch += 1;
        let pages: Vec<u32> = self.asked.drain().collect();
        for page in pages {
            self.want(page, cx);
        }
    }

    fn want(&mut self, page: u32, cx: &mut Context<Self>) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        if !self.asked.insert(page) {
            return;
        }
        let request = Request::Members { conversation, offset: page * PAGE, limit: PAGE };
        let (epoch, this) = (self.epoch, cx.weak_entity());
        super::ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.epoch == epoch {
                    this.landed(page, answer, cx);
                }
            });
        });
    }

    fn landed(&mut self, page: u32, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        match answer {
            Ok(Response::Members { sections, members, .. }) => {
                self.hub.learn(&members);
                self.sections = sections;
                self.pages.insert(page, members.into_iter().map(Rc::new).collect());
                self.refused = None;
                while self.pages.len() > PAGES_KEPT {
                    let farthest = self.pages.keys().copied().max_by_key(|held| held.abs_diff(page));
                    let Some(farthest) = farthest else {
                        break;
                    };
                    self.pages.remove(&farthest);
                    self.asked.remove(&farthest);
                }
            }
            Ok(Response::Error { detail, .. }) => self.refused = Some(detail),
            Ok(_) => self.refused = Some(crate::client::OTHER_ANSWER.to_owned()),
            Err(err) => self.refused = Some(crate::client::trouble_words(&err)),
        }
        self.loaded = true;
        // The counts live in the section headers, never beside the pane's title.
        cx.notify();
    }

    fn member(&self, offset: u32) -> Option<&Rc<Person>> {
        self.pages.get(&(offset / PAGE))?.get((offset % PAGE) as usize)
    }

    fn rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let slots: Vec<(usize, Slot)> = range.filter_map(|ix| Some((ix, slot(&self.sections, ix)?))).collect();
        let offsets: Vec<u32> = slots
            .iter()
            .filter_map(|(_, slot)| match slot {
                Slot::Member(offset) => Some(*offset),
                Slot::Header(_) => None,
            })
            .collect();
        if let (Some(first), Some(last)) = (offsets.first().copied(), offsets.last().copied()) {
            self.hub.watch_members(Some((first, last - first + 1)), cx);
            for page in first / PAGE..=last / PAGE {
                self.want(page, cx);
            }
        }
        let on_screen = offsets.iter().filter_map(|offset| self.member(*offset)).filter(|person| !person.bot);
        self.hub.watch(SOURCE, on_screen.map(|person| person.key.clone()).collect::<Vec<_>>(), cx);
        slots
            .into_iter()
            .map(|(ix, slot)| match slot {
                Slot::Header(section) => {
                    let section = &self.sections[section];
                    header(&format!("{} — {}", section.name, section.count)).into_any_element()
                }
                Slot::Member(offset) => match self.member(offset).cloned() {
                    Some(person) => self.row(ix, &person, cx).into_any_element(),
                    None => waiting_row().into_any_element(),
                },
            })
            .collect()
    }

    fn row(&self, ix: usize, person: &Rc<Person>, cx: &mut Context<Self>) -> Stateful<Div> {
        let away = shown_presence(&self.hub, person) == Presence::Offline;
        let ring = Ring::row(theme::sidebar(), false, "member");
        let in_call = !person.bot && in_call(&self.hub, &self.scene, &person.key);
        let (profile, shell) = (person.key.clone(), self.shell.clone());
        let (menu_of, menu_shell) = (person.key.clone(), self.shell.clone());
        div().id(("member", ix)).w_full().h(px(ROW)).px(px(theme::INSET)).child(
            div()
                .group("member")
                .size_full()
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                    let _ = shell.update(cx, |shell, cx| shell.open_profile(profile.clone(), event.position, cx));
                })
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    let _ = menu_shell.update(cx, |shell, cx| shell.open_row_menu(menu_of.clone(), event.position, cx));
                })
                .child(
                    div()
                        .flex_none()
                        .when(away, |avatar| avatar.opacity(0.45))
                        .child(person_avatar(&self.hub, person, 20., &ring, in_call, cx)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .type_ui()
                        .text_color(if away { theme::text_faint() } else { theme::text_soft() })
                        .child(person.name.clone()),
                )
                .when(person.bot, |row| row.child(super::list::bot_tag())),
        )
    }
}

fn header(title: &str) -> Div {
    div()
        .w_full()
        .h(px(ROW))
        .px(px(16.))
        .pb(px(4.))
        .flex()
        .items_end()
        .type_micro()
        .text_color(theme::text_faint())
        .child(div().truncate().child(title.to_owned()))
}

/// A row inside the visible range that has not arrived: its final geometry, standing still.
fn waiting_row() -> Div {
    div()
        .w_full()
        .h(px(ROW))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().size(px(20.)).rounded(px(10.)).bg(theme::hover()))
        .child(skeleton(104., 10.))
}

impl Render for MemberList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = div().size_full().pt(px(4.));
        if let Some(refused) = &self.refused {
            return body.child(empty_state("icons/users.svg", "No member list", refused).px(px(16.)));
        }
        if !self.loaded {
            return body.children((0..WAITING_ROWS).map(|_| waiting_row()));
        }
        let rows = slots(&self.sections);
        if rows == 0 {
            return body.child(empty_state("icons/users.svg", "Nobody here", "This conversation has no members to show.").px(px(16.)));
        }
        body.child(uniform_list("members", rows, cx.processor(Self::rows)).track_scroll(&self.scroll).size_full())
    }
}

#[cfg(feature = "gui-shot")]
impl MemberList {
    pub fn stage(&mut self, conversation: Key, sections: Vec<(&str, Vec<Person>)>, cx: &mut Context<Self>) {
        self.conversation = Some(conversation);
        self.reset();
        let mut all = Vec::new();
        for (name, people) in sections {
            self.sections.push(MemberSection { name: name.to_owned(), count: people.len() as u32 });
            all.extend(people);
        }
        self.hub.learn(&all);
        for (page, people) in all.chunks(PAGE as usize).enumerate() {
            self.pages.insert(page as u32, people.iter().cloned().map(Rc::new).collect());
            self.asked.insert(page as u32);
        }
        self.loaded = true;
        cx.notify();
    }
}

// The right pane draws these. It lives in `gui/pane.rs` and `conversation/side.rs`, and reaches
// the People views only through these methods.
impl Shell {
    /// The member list of the open conversation, as the pane's body.
    pub(in crate::gui) fn pane_members(&self, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
        super::draw_members(self, window, cx)
    }

    /// Slice D's Details, for a person or a conversation, as the pane's body.
    pub(in crate::gui) fn pane_details(&self, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
        super::info::draw_details(self, window, cx)
    }

    /// Puts a person or a conversation in the Details body, unless it is already there.
    /// `space`: where a person was picked from, for their name and roles there.
    pub(in crate::gui) fn pane_show(&mut self, layer: &Layer, space: Option<Key>, cx: &mut Context<Shell>) {
        if self.pane_subject(cx).as_ref() == Some(layer) {
            return;
        }
        match layer {
            Layer::Profile(person) => self.details_person(person.clone(), space, cx),
            Layer::Details(conversation) => self.details_conversation(conversation.clone(), cx),
            _ => {}
        }
    }

    /// What the Details body shows: another view may have put it there.
    pub(in crate::gui) fn pane_subject(&self, cx: &Context<Shell>) -> Option<Layer> {
        let info = self.people_state.info.get()?.read(cx);
        match info.subject()? {
            Subject::Person { key, .. } => Some(Layer::Profile(key.clone())),
            Subject::Conversation(key) => Some(Layer::Details(key.clone())),
            Subject::Space(key) => Some(Layer::Space(key.clone())),
        }
    }

    /// The floating profile card is up, for the Escape ladder.
    pub(in crate::gui) fn pane_card_open(&self, cx: &gpui::App) -> bool {
        self.people_state.floating.get().is_some_and(|floating| floating.read(cx).is_open())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections(counts: &[u32]) -> Vec<MemberSection> {
        counts.iter().map(|count| MemberSection { name: "Role".into(), count: *count }).collect()
    }

    #[test]
    fn rows_are_a_running_sum_of_the_section_counts() {
        let list = sections(&[2, 0, 3]);
        assert_eq!(slots(&list), 7);
        let all: Vec<Slot> = (0..7).map(|ix| slot(&list, ix).unwrap()).collect();
        assert_eq!(
            all,
            [
                Slot::Header(0),
                Slot::Member(0),
                Slot::Member(1),
                // The empty section has no header and takes no row.
                Slot::Header(2),
                Slot::Member(2),
                Slot::Member(3),
                Slot::Member(4),
            ]
        );
        assert_eq!(slot(&list, 7), None);
        assert_eq!(slots(&[]), 0);
    }

    #[test]
    fn a_row_far_down_a_large_server_is_found_without_walking_its_members() {
        let list = sections(&[40_000, 150_000]);
        assert_eq!(slot(&list, 40_001), Some(Slot::Header(1)));
        assert_eq!(slot(&list, 190_001), Some(Slot::Member(189_999)));
    }
}
