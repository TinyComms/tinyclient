//! Drawing the Inbox (`design/polish/04-unified.md` §2) and holding its order.

use super::chat::{row_time, Look};
use super::inbox::{self, Item};
use super::{on_screen, respliced, Mode};
use crate::gui::components::{badge_dot, badge_mention, button, empty_state, empty_state_with, glyph, ButtonKind, list_row, section_header, skeleton, Avatar, Shape};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::view::now;
use gpui::{div, list, prelude::*, px, AnyElement, Context, Div, MouseButton, Stateful, Window};
use protocol::{Badge, Conversation, InboxCounts, InboxFilter, Network};
use std::rc::Rc;
use std::time::{Duration, Instant};

impl Shell {
    /// The Inbox's items again, from the rows held. The list keeps its place by the first
    /// visible row's key.
    pub(in crate::gui) fn rebuild_inbox(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.sidebar_state;
        let items = inbox::items(state.inbox.all(), state.filter, now(), state.apps_open, state.apps_all);
        if *state.inbox_items != items {
            let anchor = |item: &Item| item.anchor().map(|key| format!("{}/{}/{}", key.network.label(), key.account, key.id));
            respliced(&state.inbox_list, &state.inbox_items, &items, anchor);
            state.inbox_items = Rc::new(items);
            cx.notify();
        }
    }

    /// The whole Inbox as chatd has it now. While the list is held, rows change in place and
    /// the new order waits behind the pill.
    pub(in crate::gui) fn inbox_read(&mut self, epoch: u64, rows: Vec<Conversation>, total: u32, counts: InboxCounts, cx: &mut Context<Self>) {
        let fresh: Vec<Rc<Conversation>> = rows.into_iter().map(Rc::new).collect();
        let state = &mut self.sidebar_state;
        if state.filter == InboxFilter::All {
            state.last_seen = fresh.iter().find(|c| !c.starred && !c.bot).map(|c| (c.title.clone(), c.updated));
            // A muted mention is already in Unread; the rest wait behind the empty state's button.
            state.muted_news = fresh.iter().filter(|c| c.muted && c.unread > 0 && c.mentions == 0 && !c.bot).cloned().collect();
        }
        state.counts = counts;
        let held = !state.inbox.fresh && state.mode == Mode::Inbox && state.hold.holding(Instant::now());
        let applied = if held {
            let triage = matches!(state.filter, InboxFilter::Unread | InboxFilter::Mentions);
            let (kept, waiting) = inbox::hold(state.inbox.all(), &fresh, triage);
            let kept_total = kept.len() as u32;
            state.pill = waiting;
            state.waiting = Some(fresh);
            if waiting > 0 {
                state.hold.held_back();
            }
            let applied = state.inbox.fill(epoch, kept, kept_total);
            if let Some(wait) = state.hold.free_in(Instant::now()) {
                self.settle_later(wait, cx);
            }
            applied
        } else {
            state.pill = 0;
            state.waiting = None;
            state.inbox.fill(epoch, fresh, total)
        };
        if applied {
            self.rebuild_inbox(cx);
            cx.notify();
        }
    }

    /// The order waiting behind the pill goes in.
    fn apply_waiting(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.sidebar_state;
        state.pill = 0;
        if let Some(fresh) = state.waiting.take() {
            let total = fresh.len() as u32;
            let epoch = state.inbox.epoch();
            state.inbox.fill(epoch, fresh, total);
            self.rebuild_inbox(cx);
        }
        cx.notify();
    }

    /// Looks again once the hold may have ended.
    pub(super) fn settle_later(&mut self, wait: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait + Duration::from_millis(20)).await;
            let _ = this.update(cx, |this, cx| {
                if this.sidebar_state.hold.settle(Instant::now()) {
                    this.apply_waiting(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn activate_inbox(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.sidebar_state.inbox_items.get(ix).cloned() else {
            return;
        };
        match item {
            Item::Row { conversation, .. } | Item::App(conversation) => self.pick_row(&conversation, cx),
            Item::Apps(_) => {
                self.sidebar_state.apps_open = !self.sidebar_state.apps_open;
                self.rebuild_inbox(cx);
            }
            Item::AppsMore(_) => {
                self.sidebar_state.apps_all = true;
                self.rebuild_inbox(cx);
            }
            Item::Header { .. } | Item::End => {}
        }
    }

    fn inbox_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.sidebar_state.inbox_items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let focused = self.sidebar_state.cursor == Some(ix) && (self.sidebar_state.focus.is_focused(window) || self.focus.is_focused(window));
        let look = Look { mark: self.marks_shown(), context: true, time: true, ..Look::default() };
        match item {
            Item::Header { day, first } => section_header(day.label(), first).into_any_element(),
            Item::Row { conversation, .. } => self.chat_row("inbox", &conversation, look, focused, cx).into_any_element(),
            Item::Apps(apps) => self.apps_row(ix, &apps, focused, cx).into_any_element(),
            Item::App(app) => self.app_row(ix, &app, look.mark, focused, cx).into_any_element(),
            Item::AppsMore(more) => self
                .action_row(("apps-more", ix), Some("icons/chevron-down.svg"), format!("Show {more} more").into(), false, None, focused)
                .map(|row| self.tap(row, crate::gui::phone::token(&["inbox-item", &ix.to_string()]), None, move |this, cx| this.activate_inbox(ix, cx), cx))
                .into_any_element(),
            Item::End => self.end_line(cx).into_any_element(),
        }
    }

    /// The one Apps row: the two newest apps' faces, "Apps", who is in it, a dot, never a count.
    fn apps_row(&self, ix: usize, apps: &inbox::Apps, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let unread = apps.unread();
        let face = |c: &Conversation| Avatar::new(&c.key, &c.title, 21.).shape(Shape::Place).build();
        let mut faces = div().relative().size(px(32.)).flex_none();
        if let Some(back) = apps.members.get(1) {
            faces = faces.child(div().absolute().top_0().left_0().child(face(back)));
        }
        if let Some(front) = apps.members.first() {
            faces = faces.child(div().absolute().right(px(-2.)).bottom(px(-2.)).rounded(px(7.)).border_2().border_color(theme::sidebar()).child(face(front)));
        }
        let badge = match (apps.mentions(), unread) {
            (0, false) => None,
            (0, true) => Some(badge_dot(8., theme::unread())),
            (count, _) => Some(badge_mention(count)),
        };
        let chevron = if apps.open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" };
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
                            .h(theme::line(18.)).flex_none().overflow_hidden()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(div().type_ui().font_weight(if unread { theme::SEMIBOLD } else { theme::REGULAR }).text_color(theme::text()).child("Apps"))
                            .child(glyph(chevron, 12., theme::text_faint()))
                            .child(div().flex_1())
                            .child(div().type_micro().text_color(theme::text_faint()).child(row_time(now(), apps.newest()))),
                    )
                    .child(
                        div()
                            .h(theme::line(16.)).flex_none().overflow_hidden()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().truncate().type_small().text_color(if unread { theme::text() } else { theme::text_soft() }).child(super::chat::one_line(&apps.names()).into_owned()))
                            .children(badge),
                    ),
            );
        list_row(("apps", ix), theme::row_height(theme::ROW_TWO), false, focused, content)
            .map(|row| self.tap(row, crate::gui::phone::token(&["inbox-item", &ix.to_string()]), None, move |this, cx| this.activate_inbox(ix, cx), cx))
    }

    /// An app under the open Apps row, 28: its square, name, last line, time and dot.
    fn app_row(&self, ix: usize, app: &Rc<Conversation>, mark: bool, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.open.as_ref() == Some(&app.key);
        let unread = app.badge() != Badge::None;
        let faint = if selected { theme::text_soft() } else { theme::text_faint() };
        let picture = self.people_state.hub().avatar(&app.key.account(), app.avatar.as_ref(), cx).and_then(|file| file.path).map(std::path::PathBuf::from);
        let content = div()
            .gap(px(8.))
            .child(Avatar::new(&app.key, &app.title, 20.).shape(Shape::Place).image_at(picture).build())
            .children(mark.then(|| super::chat::mark(app.key.network, 12., faint)))
            .child(div().flex_none().max_w(px(110.)).truncate().type_ui().font_weight(if unread { theme::SEMIBOLD } else { theme::REGULAR }).text_color(theme::text()).child(super::chat::one_line(&app.title).into_owned()))
            // A4 off: the name and time only, as every other row.
            .child(div().flex_1().min_w_0().truncate().type_small().text_color(faint).when(crate::gui::app::prefs::choices().previews, |line| line.child(super::chat::one_line(&app.preview).into_owned())))
            .child(div().flex_none().type_micro().text_color(faint).child(row_time(now(), app.updated)))
            .children(super::super::components::row_badge(app));
        let picked = app.clone();
        list_row(("app", ix), theme::row_height(theme::ROW_ONE), selected, focused, content)
            .map(|row| self.tap(row, crate::gui::phone::token(&["row", picked.key.network.label(), &picked.key.account, &picked.key.id]), Some(picked.clone()), move |this, cx| this.pick_row(&picked, cx), cx))
    }

    /// "Older conversations are in Discord · Slack": each opens that network's home.
    fn end_line(&self, cx: &mut Context<Self>) -> Div {
        let networks: Vec<Network> = self.snapshot.as_ref().map(|snapshot| snapshot.signed_in()).unwrap_or_default();
        let mut line = div().w_full().pt(px(16.)).pb(px(24.)).px(px(theme::PANE_PAD)).flex().flex_wrap().items_center().gap(px(4.)).type_small().text_color(theme::text_faint()).child("Older conversations are in");
        let last = networks.len().saturating_sub(1);
        for (ix, network) in networks.into_iter().enumerate() {
            let link = div()
                .id(("older", network as usize))
                .cursor_pointer()
                .text_color(theme::text_soft())
                .hover(|style| style.text_color(theme::text()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.go(Mode::Network(network), cx)))
                .child(if last == 0 { format!("{} ›", network.label()) } else { network.label().to_owned() });
            line = line.child(link);
            if ix < last {
                line = line.child("·");
            }
        }
        line
    }

    /// A placeholder in a row's final geometry, for a list still on its way.
    fn inbox_placeholder(ix: usize) -> Div {
        let widths = [120., 90., 140., 100., 130., 80.];
        div().h(px(48.)).px(px(16.)).flex().items_center().gap(px(10.)).child(div().size(px(32.)).rounded(px(16.)).bg(theme::hover())).child(
            div().flex().flex_col().gap(px(8.)).child(skeleton(widths[ix % widths.len()], 10.)).child(skeleton(widths[(ix + 2) % widths.len()] + 40., 8.)),
        )
    }

    fn inbox_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.sidebar_state.filter {
            InboxFilter::Unread => {
                let last = self.sidebar_state.last_seen.as_ref().map(|(name, at)| format!("Last: {name}, {}", row_time(now(), *at)));
                let muted = self.sidebar_state.muted_news.len();
                if muted == 0 {
                    return empty_state("icons/check.svg", "You're all caught up", last.as_deref().unwrap_or("")).px(px(24.)).into_any_element();
                }
                if self.sidebar_state.muted_open {
                    return self.muted_news(cx).into_any_element();
                }
                let words = if muted == 1 { "1 muted conversation has new messages".to_owned() } else { format!("{muted} muted conversations have new messages") };
                let open = button("muted-news", ButtonKind::Secondary, words).h(px(28.)).on_click(cx.listener(|this, _, _, cx| {
                    this.sidebar_state.muted_open = true;
                    cx.notify();
                }));
                empty_state_with("icons/check.svg", "You're all caught up", last.as_deref().unwrap_or(""), open)
            }
            InboxFilter::Mentions => empty_state("icons/at-sign.svg", "No one's waiting on you", "Mentions from the last 30 days show here."),
            InboxFilter::All | InboxFilter::Directs => empty_state("icons/inbox.svg", "No conversations yet", "Sign in to a network to see your chats."),
        }
        .px(px(24.))
        .into_any_element()
    }

    /// The muted conversations with new messages, under a `Muted` header, once asked for.
    fn muted_news(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let look = Look { mark: self.marks_shown(), context: true, time: true, ..Look::default() };
        let rows: Vec<Rc<Conversation>> = self.sidebar_state.muted_news.clone();
        div()
            .id("muted-news-list")
            .size_full()
            .overflow_y_scroll()
            .child(section_header("Muted", true))
            .children(rows.iter().map(|c| self.chat_row("muted", c, look, false, cx)))
    }

    pub(super) fn inbox_list(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.sidebar_state;
        if state.inbox.fresh {
            return div().pt(px(4.)).children((0..6).map(Self::inbox_placeholder)).into_any_element();
        }
        if state.inbox_items.is_empty() {
            return self.inbox_empty(cx);
        }
        let items = state.inbox_items.clone();
        let visible = on_screen(&state.inbox_list, items.len(), |ix| items[ix].height());
        self.watch_rows(items[visible].iter().filter_map(Item::conversation), cx);
        let _ = window;
        let pill = (state.pill > 0).then(|| {
            div()
                .id("new-pill")
                .absolute()
                .top(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("new-pill-button")
                        .h(px(24.))
                        .px(px(10.))
                        .rounded(px(12.))
                        .bg(theme::overlay())
                        .shadow(theme::shadow_popover())
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.sidebar_state.hold.release();
                            this.apply_waiting(cx);
                            this.sidebar_state.inbox_list.scroll_to(gpui::ListOffset::default());
                        }))
                        .child(glyph("icons/chevron-up.svg", 12., theme::text()))
                        .child(div().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).child(format!("{} new", state.pill))),
                )
        });
        div()
            .id("inbox-hold")
            .relative()
            .size_full()
            // Under the pointer the order holds; what arrived meanwhile goes in a second after.
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                let now = Instant::now();
                this.sidebar_state.hold.hover(*over, now);
                if !*over {
                    if let Some(wait) = this.sidebar_state.hold.free_in(now) {
                        this.settle_later(wait, cx);
                    }
                }
            }))
            .child(list(state.inbox_list.clone(), cx.processor(|this: &mut Shell, ix: usize, window, cx| this.inbox_item(ix, window, cx))).size_full())
            .children(pill)
            .into_any_element()
    }
}
