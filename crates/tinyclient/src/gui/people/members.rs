//! The member list of the open conversation, in the right pane. Only the rows on screen are
//! served from a bounded page cache; missing visible ranges wait for scrolling
//! to settle and use one backend request at a time.

use super::presence::Ring;
use super::{in_call, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{button, empty_state, skeleton, ButtonKind};
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
use std::time::{Duration, Instant};

pub const SOURCE: &str = "members";
/// A member row: the desktop's 28, and a thumb's 44 on a phone.
fn row() -> f32 {
    if crate::gui::phone::active() { theme::row_height(theme::ROW_ONE) } else { theme::ROW_ONE }
}
const PAGE: u32 = 50;
/// Pages of members held; the ones farthest from the viewport go first.
const PAGES_KEPT: usize = 64;
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

/// One immediate cache query at a time; the newest viewport replaces obsolete queued work.
#[derive(Default)]
struct PageRequests {
    wanted: Vec<u32>,
    ready: HashSet<u32>,
    stale: HashSet<u32>,
    waiting: HashSet<u32>,
    failed: HashSet<u32>,
    attempts: HashMap<u32, u32>,
    active: Option<(u64, u32)>,
}

impl PageRequests {
    fn viewport(&mut self, first: u32, last: u32, total: u32) -> bool {
        // Discord already delivers 100-row windows around each 50-member read.
        // Speculative neighboring reads can displace those live windows and make
        // every roster update resubscribe back and forth without any scrolling.
        let wanted: Vec<u32> = (first..=last).filter(|page| page.saturating_mul(PAGE) < total).take(PAGES_KEPT).collect();
        if self.wanted == wanted { return false; }
        self.wanted = wanted;
        self.waiting.retain(|page| self.wanted.contains(page));
        self.failed.retain(|page| self.wanted.contains(page));
        self.attempts.retain(|page, _| self.wanted.contains(page));
        true
    }

    fn next(&mut self, epoch: u64) -> Option<u32> {
        if self.active.is_some() { return None; }
        // Fetch holes before refreshing retained rows, even during frequent roster edits.
        let eligible = |page: &u32| !self.failed.contains(page) && !self.waiting.contains(page);
        let page = self.wanted.iter().copied().find(|page| !self.ready.contains(page) && eligible(page))
            .or_else(|| self.wanted.iter().copied().find(|page| self.stale.contains(page) && eligible(page)))?;
        self.active = Some((epoch, page));
        Some(page)
    }

    fn pending(&mut self, epoch: u64, page: u32, current: u64) -> bool {
        if self.active != Some((epoch, page)) { return false; }
        self.active = None;
        if epoch != current { return false; }
        if self.wanted.contains(&page) { self.waiting.insert(page); }
        true
    }

    fn complete(&mut self, epoch: u64, page: u32, current: u64, success: bool) -> bool {
        if self.active != Some((epoch, page)) { return false; }
        self.active = None;
        if epoch != current { return false; }
        if success {
            self.ready.insert(page);
            self.stale.remove(&page);
            self.attempts.remove(&page);
        } else if self.wanted.contains(&page) {
            self.failed.insert(page);
            *self.attempts.entry(page).or_default() += 1;
        }
        true
    }

    fn retry_delay(&self, page: u32) -> Duration {
        let attempts = self.attempts.get(&page).copied().unwrap_or(1);
        Duration::from_secs(1 << attempts.min(5))
    }

    fn reset(&mut self) {
        // Reads are independent cache queries; navigation never waits for an old reply.
        self.active = None;
        self.wanted = vec![0];
        self.ready.clear();
        self.stale.clear();
        self.waiting.clear();
        self.failed.clear();
        self.attempts.clear();
    }
}

pub struct MemberList {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    conversation: Option<Key>,
    sections: Vec<MemberSection>,
    pages: HashMap<u32, Vec<Rc<Person>>>,
    requests: PageRequests,
    errors: HashMap<u32, String>,
    viewport_tick: u64,
    settled: bool,
    scroll_task: Option<gpui::Task<()>>,
    loaded: bool,
    waiting_since: Option<Instant>,
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
            requests: PageRequests::default(),
            errors: HashMap::new(),
            viewport_tick: 0,
            settled: true,
            scroll_task: None,
            loaded: false,
            waiting_since: None,
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
        if moved || self.requests.wanted.is_empty() {
            self.conversation = self.scene.open.clone();
            if moved { self.reset(); } else { self.requests.wanted = vec![0]; }
            // Declare the initial viewport before rows exist, so an unrelated
            // presence watch cannot unsubscribe the bootstrap window.
            self.hub.watch_members(self.conversation.as_ref().map(|_| (0, PAGE)), cx);
            self.pump(cx);
        }
    }

    pub fn pause(&mut self) {
        if self.requests.wanted.is_empty() { return; }
        self.epoch += 1;
        self.requests.active = None;
        self.requests.wanted.clear();
        self.requests.waiting.clear();
        self.requests.failed.clear();
        self.requests.attempts.clear();
        self.errors.clear();
        self.refused = None;
        self.waiting_since = None;
        self.viewport_tick += 1;
        self.scroll_task = None;
        self.settled = true;
    }

    fn reset(&mut self) {
        self.epoch += 1;
        self.sections.clear();
        self.pages.clear();
        self.requests.reset();
        self.errors.clear();
        self.viewport_tick += 1;
        self.scroll_task = None;
        self.settled = true;
        self.loaded = false;
        self.waiting_since = None;
        self.refused = None;
    }

    /// Rows of this conversation's list were added, removed or moved.
    pub fn members_changed(&mut self, conversation: &Key, cx: &mut Context<Self>) {
        if self.conversation.as_ref() == Some(conversation) {
            self.read_again(cx);
        }
    }

    /// Refreshes only the latest viewport against retained rows, one request at a time.
    fn read_again(&mut self, cx: &mut Context<Self>) {
        if !self.loaded && self.requests.waiting.is_empty() { return; }
        self.requests.waiting.clear();
        // Retain the cached rows; refresh only the latest viewport and its neighbors.
        self.requests.stale.extend(self.requests.ready.iter().copied());
        self.pump(cx);
    }

    fn viewport(&mut self, first: u32, last: u32, cx: &mut Context<Self>) {
        let total = self.sections.iter().map(|section| section.count).sum();
        if !self.requests.viewport(first, last, total) { return; }
        let errors = self.errors.len();
        self.errors.retain(|page, _| self.requests.failed.contains(page));
        if self.errors.len() != errors { cx.notify(); }
        self.viewport_tick += 1;
        let tick = self.viewport_tick;
        self.settled = false;
        self.scroll_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let _ = this.update(cx, |this, cx| {
                if this.viewport_tick == tick {
                    this.settled = true;
                    this.pump(cx);
                }
            });
        }));
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        if !self.settled { return; }
        let Some(conversation) = self.conversation.clone() else { return; };
        let Some(page) = self.requests.next(self.epoch) else { return; };
        let request = Request::Members { conversation, offset: page * PAGE, limit: PAGE };
        let (epoch, this) = (self.epoch, cx.weak_entity());
        super::ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                let pending = matches!(&answer, Ok(Response::Members { pending: true, .. }));
                let success = matches!(&answer, Ok(Response::Members { pending: false, .. }));
                let accepted = if pending { this.requests.pending(epoch, page, this.epoch) }
                    else { this.requests.complete(epoch, page, this.epoch, success) };
                if accepted {
                    this.landed(page, answer, cx);
                } else {
                    this.pump(cx);
                }
            });
        });
    }

    fn landed(&mut self, page: u32, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        if let Ok(Response::Members { pending: true, sections, .. }) = &answer {
            self.waiting_since.get_or_insert_with(Instant::now);
            if !sections.is_empty() { self.sections = sections.clone(); self.loaded = true; }
            // A cache miss never holds up the next viewport. SYNC notifications
            // normally wake it; a cheap cache poll also covers missed notifications.
            let epoch = self.epoch;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.epoch == epoch && this.requests.wanted.contains(&page) && this.requests.waiting.remove(&page) {
                        this.pump(cx);
                    }
                });
            }).detach();
            self.pump(cx);
            cx.notify();
            return;
        }
        let retryable = match &answer {
            Err(err) => crate::client::is_slow(err) || crate::client::is_gone(err),
            Ok(Response::Error { code: protocol::ErrorCode::Offline, .. }) => true,
            Ok(Response::Error { detail, .. }) => detail == "Discord did not send the member list.",
            _ => false,
        };
        match answer {
            Ok(Response::Members { sections, members, .. }) => {
                self.hub.learn(&members);
                self.sections = sections;
                self.pages.insert(page, members.into_iter().map(Rc::new).collect());
                self.refused = None;
                self.errors.remove(&page);
                while self.pages.len() > PAGES_KEPT {
                    let anchor = self.requests.wanted.first().copied().unwrap_or(page);
                    let farthest = self.pages.keys().copied().max_by_key(|held| (!self.requests.wanted.contains(held), held.abs_diff(anchor)));
                    let Some(farthest) = farthest else {
                        break;
                    };
                    self.pages.remove(&farthest);
                    self.requests.ready.remove(&farthest);
                    self.requests.stale.remove(&farthest);
                }
            }
            Ok(Response::Error { detail, .. }) => self.failed(page, detail),
            Ok(_) => self.failed(page, crate::client::OTHER_ANSWER.to_owned()),
            Err(err) => self.failed(page, crate::client::trouble_words("people", &err)),
        }
        if retryable && self.requests.failed.contains(&page) {
            // Transient gateway congestion must not leave a stationary viewport
            // blocked forever. One delayed attempt per failed read, never per render.
            let epoch = self.epoch;
            let delay = self.requests.retry_delay(page);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    if this.epoch == epoch && this.requests.wanted.contains(&page) && this.requests.failed.remove(&page) {
                        this.pump(cx);
                    }
                });
            }).detach();
        }
        if self.requests.wanted.iter().all(|page| self.requests.ready.contains(page)) || !self.errors.is_empty() {
            self.waiting_since = None;
        }
        self.loaded = true;
        // The counts live in the section headers, never beside the pane's title.
        self.pump(cx);
        cx.notify();
    }

    fn failed(&mut self, page: u32, detail: String) {
        if self.sections.is_empty() {
            self.refused = Some(detail.clone());
        }
        if self.requests.wanted.contains(&page) {
            self.errors.insert(page, detail);
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.requests.failed.clear();
        self.errors.clear();
        self.refused = None;
        self.loaded = !self.sections.is_empty();
        self.settled = true;
        self.pump(cx);
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
            self.viewport(first / PAGE, last / PAGE, cx);
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
        div().id(("member", ix)).w_full().h(px(row())).px(px(theme::INSET)).child(
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
        .h(px(row()))
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
        .h(px(row()))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().size(px(20.)).rounded(px(10.)).bg(theme::hover()))
        .child(skeleton(104., 10.))
}

impl Render for MemberList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = div().size_full().pt(px(4.)).flex().flex_col().when(
            self.waiting_since.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) && !self.requests.waiting.is_empty(),
            |body| body.child(div().flex_none().px(px(12.)).py(px(4.)).type_small().text_color(theme::text_soft())
                .child("Waiting for Discord to send these members…")),
        );
        if let Some(refused) = &self.refused {
            return body.child(empty_state("icons/users.svg", "Couldn’t load members", refused).px(px(16.))).child(button("retry-members", ButtonKind::Ghost, "Try again").on_click(cx.listener(|this, _, _, cx| this.retry(cx))));
        }
        if !self.loaded {
            return body.children((0..WAITING_ROWS).map(|_| waiting_row()));
        }
        let rows = slots(&self.sections);
        if rows == 0 {
            return body.child(empty_state("icons/users.svg", "Nobody here", "This conversation has no members to show.").px(px(16.)));
        }
        body.when(!self.errors.is_empty(), |body| body.child(
            div().flex_none().px(px(12.)).flex().items_center().gap(px(8.))
                .child(div().flex_1().type_small().text_color(theme::text_soft()).child("Some members couldn’t load"))
                .child(button("retry-member-pages", ButtonKind::Ghost, "Retry").on_click(cx.listener(|this, _, _, cx| this.retry(cx))))
        )).child(div().flex_1().min_h_0().child(uniform_list("members", rows, cx.processor(Self::rows)).track_scroll(&self.scroll).size_full()))
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
            self.requests.ready.insert(page as u32);
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
    fn a_missing_discord_window_does_not_hold_the_next_viewport_hostage() {
        let mut requests = PageRequests::default();
        requests.viewport(23, 24, 4000);
        assert_eq!(requests.next(1), Some(23));
        assert!(requests.pending(1, 23, 1));
        assert_eq!(requests.next(1), Some(24));
        assert!(requests.pending(1, 24, 1));
        assert_eq!(requests.next(1), None, "wait for delivery rather than spin on cache misses");
        requests.viewport(40, 41, 4000);
        assert_eq!(requests.next(1), Some(40), "no timeout or completion of old network reads needed");
        assert!(requests.pending(1, 40, 1));
        requests.waiting.clear(); // notification that the subscribed rows arrived
        assert_eq!(requests.next(1), Some(40));
        assert!(requests.complete(1, 40, 1, true));
    }

    #[test]
    fn busy_large_roster_refreshes_do_not_read_neighbors_that_displace_live_windows() {
        let mut requests = PageRequests::default();
        // Captured viewport: members 1197..1246; five groups, about 3900 people.
        // Pages 23 and 24 cover it. Reading neighbor 25 requests a third remote
        // window, evicting the window containing page 23 from the subscription.
        requests.viewport(23, 24, 3931);
        assert_eq!(requests.wanted, [23, 24]);
        for _ in 0..100 {
            requests.stale.extend(requests.ready.iter().copied());
            for page in [23, 24] {
                assert_eq!(requests.next(1), Some(page));
                assert!(requests.complete(1, page, 1, true));
            }
            assert_eq!(requests.next(1), None, "no requests for off-screen pages 22 or 25");
        }
    }

    #[test]
    fn fast_scrolling_coalesces_missing_pages_behind_one_active_request() {
        let mut requests = PageRequests::default();
        requests.viewport(0, 1, 10_000);
        assert_eq!(requests.next(1), Some(0));
        requests.viewport(20, 21, 10_000);
        requests.viewport(40, 41, 10_000);
        assert_eq!(requests.next(1), None, "never supersede an active Discord reply");
        assert!(requests.complete(1, 0, 1, true));
        assert_eq!(requests.next(1), Some(40), "discard intermediate scroll ranges");
        assert!(requests.complete(1, 40, 1, true));
        assert_eq!(requests.next(1), Some(41), "continue with the other visible page");
        assert!(requests.complete(1, 41, 1, true));
        assert_eq!(requests.next(1), None);
    }

    #[test]
    fn transient_failures_can_recover_without_another_scroll_and_back_off() {
        let mut requests = PageRequests::default();
        requests.viewport(40, 40, 10_000);
        for seconds in [2, 4, 8, 16, 32, 32] {
            assert_eq!(requests.next(1), Some(40));
            assert!(requests.complete(1, 40, 1, false));
            assert_eq!(requests.retry_delay(40), Duration::from_secs(seconds));
            assert!(requests.failed.remove(&40)); // delayed retry, same viewport
        }
        assert_eq!(requests.next(1), Some(40));
        assert!(requests.complete(1, 40, 1, true));
        assert!(requests.attempts.is_empty());
        assert_eq!(requests.next(1), None);
    }

    #[test]
    fn roster_updates_cannot_starve_missing_visible_pages() {
        let mut requests = PageRequests::default();
        requests.viewport(40, 42, 10_000);
        // Each window's arrival can coincide with a real presence/roster edit.
        // Already loaded rows must remain lower priority than the next hole.
        for page in [40, 41, 42] {
            assert_eq!(requests.next(1), Some(page));
            assert!(requests.complete(1, page, 1, true));
            requests.stale.extend(requests.ready.iter().copied());
        }
        assert!([40, 41, 42].iter().all(|page| requests.ready.contains(page)));
        assert_eq!(requests.next(1), Some(40), "refresh cached rows only after filling holes");
    }

    #[test]
    fn cached_pages_are_reused_and_failed_pages_do_not_create_retry_loops() {
        let mut requests = PageRequests::default();
        requests.viewport(2, 2, 250);
        requests.ready.extend([1, 2, 3]);
        assert_eq!(requests.next(1), None);
        requests.viewport(3, 3, 250);
        assert_eq!(requests.next(1), None, "scrolling to a cached page needs no speculative reads");
        requests.viewport(4, 4, 250);
        assert_eq!(requests.next(1), Some(4));
        assert!(requests.complete(1, 4, 1, false));
        assert_eq!(requests.next(1), None);
        assert!(requests.ready.contains(&2) && requests.ready.contains(&3), "failure preserves cached pages");
        requests.failed.clear(); // explicit Retry
        assert_eq!(requests.next(1), Some(4));
    }

    #[test]
    fn old_conversation_replies_cannot_fill_the_new_conversations_cache() {
        let mut requests = PageRequests::default();
        requests.reset();
        assert_eq!(requests.next(1), Some(0));
        requests.reset();
        assert_eq!(requests.next(2), Some(0), "navigation does not wait for the old query");
        assert!(!requests.complete(1, 0, 2, true));
        assert!(requests.ready.is_empty());
        assert_eq!(requests.next(2), None, "the new query remains active after the old reply");
        assert!(requests.complete(2, 0, 2, true));
        requests.stale.extend(requests.ready.iter().copied()); // refresh after a member update
        assert_eq!(requests.next(2), Some(0));
    }

    #[test]
    fn visible_requests_stay_inside_the_roster_and_the_cache_capacity() {
        let mut requests = PageRequests::default();
        requests.viewport(0, 0, 50);
        assert_eq!(requests.wanted, [0]);
        requests.viewport(2, 2, 150);
        assert_eq!(requests.wanted, [2]);
        requests.viewport(0, 100, 10_000);
        assert_eq!(requests.wanted.len(), PAGES_KEPT);
        assert_eq!(requests.wanted, (0..PAGES_KEPT as u32).collect::<Vec<_>>());
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
