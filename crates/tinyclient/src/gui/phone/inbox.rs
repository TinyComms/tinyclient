//! Needs me and Inbox on a phone: the same rows, 64 pt tall, unread first inside each day
//! (`01-ia-navigation.md` Q2; `02-component-inventory.md` B1).

use crate::gui::components::{button, empty_state, empty_state_with, list_row, row_tone, ButtonKind};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::phone::{Route, Tab};
use crate::gui::sidebar::chat::row_time;
use crate::gui::sidebar::inbox::{self, Day};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use crate::view::now;
use gpui::{div, prelude::*, px, Context, Div, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Stateful, Window};
use protocol::{AccountKey, Conversation, ConversationKind, Network};
use std::rc::Rc;

/// A day heading on a phone: 20 above, 6 below, 12/16 uppercase `text_faint`. The first heading
/// in a list sits 8 from its top, so the list starts where the strip ends. The label is plain
/// caps: GPUI has no letter-spacing, and a hair space between letters read as noise.
pub fn phone_section(label: impl Into<SharedString>, first: bool) -> Div {
    let label: SharedString = label.into();
    div()
        .flex_none()
        .w_full()
        .pt(px(if first { 8. } else { 20. }))
        .pb(px(6.))
        .px(px(tokens::GUTTER))
        .child(
            div()
                .text_size(px(tokens::scaled(12.)))
                .line_height(px(tokens::scaled(16.)))
                .font_weight(theme::MEDIUM)
                .text_color(theme::text_faint())
                .child(label.to_uppercase()),
        )
}

/// "Needs me": an unread direct message, or anything that names me. A muted row counts only
/// when it names me. Thread replies are not a field the row carries yet, so they are not here.
pub fn needs(conversation: &Conversation) -> bool {
    conversation.mentions > 0 || (!conversation.muted && conversation.kind == ConversationKind::Direct && conversation.unread > 0)
}

/// How many conversations need me, the one number the chrome carries.
pub fn needing(rows: &[Rc<Conversation>]) -> u32 {
    rows.iter().filter(|row| needs(row)).count() as u32
}

/// The two lists in phone order: scope, then day groups, unread first inside each. A pinned
/// row keeps its day but leads it while it is unread (`01` §10 item 6).
pub fn sections(rows: &[Rc<Conversation>], needs_only: bool, scope: Option<&AccountKey>) -> Vec<(Day, Vec<Rc<Conversation>>)> {
    let mut picked: Vec<&Rc<Conversation>> = rows
        .iter()
        .filter(|row| scope.is_none_or(|scope| &row.key.account() == scope))
        .filter(|row| !needs_only || needs(row))
        .collect();
    let at = now();
    picked.sort_by(|a, b| {
        let day = inbox::days_ago(at, a.updated).cmp(&inbox::days_ago(at, b.updated));
        let unread = (b.badge() != protocol::Badge::None).cmp(&(a.badge() != protocol::Badge::None));
        day.then(unread).then(b.updated.cmp(&a.updated))
    });
    let mut out: Vec<(Day, Vec<Rc<Conversation>>)> = Vec::new();
    for row in picked {
        let day = Day::of(inbox::days_ago(at, row.updated));
        match out.last_mut() {
            Some((last, rows)) if *last == day => rows.push(Rc::clone(row)),
            _ => out.push((day, vec![Rc::clone(row)])),
        }
    }
    out
}

/// A pointer position as the two numbers the slop test wants.
pub fn at(point: Point<Pixels>) -> (f32, f32) {
    (f32::from(point.x), f32::from(point.y))
}

impl Shell {
    /// One of the two lists, whole: the nav strip whose title switches the scope, the rows, and
    /// the scope sheet over them.
    pub(super) fn phone_list(&self, snapshot: &Snapshot, needs_only: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let _ = window;
        let scope = self.phone_state.scope.as_ref();
        let label = Some(scope.map_or_else(
            || SharedString::from("All accounts"),
            |key| {
                let name = snapshot.account(key).map(|account| account.name.clone()).unwrap_or_default();
                SharedString::from(format!("{} · {name}", key.network.label()))
            },
        ));
        let words = if needs_only { "Needs me" } else { "Inbox" };
        let inbox = &self.sidebar_state.inbox;
        let groups = sections(inbox.all(), needs_only, scope);
        let mut list = div()
            .id("phone-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb(px(tokens::GUTTER))
            .flex()
            .flex_col()
            // A press that travels past the slop is a scroll: the rows check it on release.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| this.phone_state.press_move(at(event.position))))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.phone_state.press_cancel()));
        let mut ix = 0;
        for (first, (day, rows)) in groups.iter().enumerate().map(|(n, group)| (n == 0, group)) {
            list = list.child(phone_section(day.label(), first));
            for (last, row) in rows.iter().enumerate().map(|(n, row)| (n + 1 == rows.len(), row)) {
                list = list.child(self.phone_row(ix, row, last, cx));
                ix += 1;
            }
        }
        if ix == 0 {
            // Nothing is empty until the first answer has come: a list that has not been read
            // says it is loading, with a retry it can use at once, never "No conversations".
            list = list.child(if inbox.fresh {
                self.phone_loading(cx).into_any_element()
            } else if needs_only {
                empty_state_with(
                    "icons/check.svg",
                    "You're clear",
                    "Nothing needs you right now.",
                    button("phone-to-inbox", ButtonKind::Secondary, "Open Inbox").on_click(cx.listener(|this, _, _, cx| {
                        this.phone_state.tab = Tab::Inbox;
                        cx.notify();
                    })),
                )
                .into_any_element()
            } else if snapshot.signed_in().is_empty() {
                empty_state_with(
                    "icons/inbox.svg",
                    "No conversations yet",
                    "Sign in to a network to see your chats.",
                    self.phone_sign_in_button("phone-sign-in", "Sign in", snapshot, cx),
                )
                .into_any_element()
            } else {
                empty_state("icons/inbox.svg", "No conversations yet", "Your chats will appear here.").into_any_element()
            });
        }
        let body = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.phone_nav(words, label, cx))
            // The scroller clips its own rows: a row scrolled past the top must not paint over
            // the nav strip. At 200% a row is 96 pt and the overflow is visible for a frame.
            .child(div().flex_1().min_h_0().min_w_0().flex().flex_col().overflow_hidden().child(list));
        div().size_full().child(body)
    }

    /// The list before its first answer: a line that it is loading, and a retry that asks again.
    /// Shown for as long as the answer is missing, so there is never a timeout to hit or miss.
    pub(super) fn phone_loading(&self, cx: &mut Context<Self>) -> Div {
        empty_state_with(
            "icons/refresh-cw.svg",
            "Loading your chats",
            "This can take a moment. Still nothing? Try again.",
            button("phone-retry", ButtonKind::Secondary, "Try again").on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
        )
    }

    /// One conversation, 64 pt: avatar 40, title 15, one preview line 13, one time 12 and one
    /// count. Nothing else. `last` says it closes its section: the rule under a row is inset past
    /// the avatar, and there is none after the last one.
    ///
    /// A row opens on release, not on press: a finger that goes down and then scrolls the list
    /// opens nothing (`tokens::SLOP`).
    pub(super) fn phone_row(&self, ix: usize, conversation: &Rc<Conversation>, last: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let c = conversation;
        let tone = row_tone(c, false, false);
        let avatar = self.conversation_avatar(c, 40., theme::row_surface(false), cx);
        let several = self.snapshot.as_ref().is_some_and(|snapshot| snapshot.signed_in().len() > 1 || snapshot.accounts.len() > 1);
        let mark = several.then(|| crate::gui::sidebar::chat::mark(c.key.network, 14., theme::text_faint()));
        let time = row_time(now(), c.updated);
        let preview = one_line(&c.preview);
        let (count, hot) = if c.mentions > 0 {
            (c.mentions, true)
        } else if c.unread > 0 {
            (c.unread, false)
        } else {
            (0, false)
        };
        let title = match c.kind {
            ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Voice | ConversationKind::Forum
                if c.key.network != Network::Telegram && c.key.network != Network::Teams =>
            {
                format!("#{}", c.title.trim_start_matches('#'))
            }
            _ => c.title.clone(),
        };
        let content = div()
            .gap(px(12.))
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .children(mark)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .phone_row()
                                    .font_weight(tone.weight)
                                    .text_color(tone.title)
                                    .child(title),
                            )
                            .child(div().flex_none().phone_stamp().text_color(tone.time).child(time)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().truncate().phone_preview().text_color(tone.preview).child(preview))
                            .children((count > 0).then(|| {
                                let color = if hot { tokens::accent() } else { theme::text_soft() };
                                let words = if count > 99 { "99+".to_owned() } else { count.to_string() };
                                div().flex_none().phone_stamp().text_color(color).child(SharedString::from(words))
                            }))
                            // An unread with no count the network will name: a dot, not a made-up 1.
                            .children((count == 0 && c.badge() != protocol::Badge::None).then(|| {
                                div().flex_none().p(px(4.)).child(div().size(px(8.)).rounded(px(4.)).bg(theme::text_soft()))
                            })),
                    ),
            );
        let (key, title) = (c.key.clone(), c.title.clone());
        let (press_key, release_key) = (key.clone(), key.clone());
        list_row(("phone-row", ix), tokens::row_height(), false, false, content)
            .relative()
            // 1 pt at 5.5% from x = 76 to the edge: the seam is between rows, not around them.
            .children((!last).then(|| div().absolute().bottom_0().left(px(tokens::RULE_START)).right_0().h(px(1.)).bg(tokens::rule())))
            .active(|row| row.bg(tokens::pressed()))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, _| {
                this.phone_state.press_down(press_key.clone(), at(event.position));
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                if this.phone_state.press_up(&release_key, at(event.position)) {
                    this.select(key.clone(), &title, cx);
                    this.phone_state.push(Route::Conversation { key: key.clone(), title: title.clone() });
                    cx.notify();
                }
            }))
    }

}

/// One line of a preview: newlines and tabs become spaces, as the desktop sidebar's rows do.
fn one_line(text: &str) -> SharedString {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        }
    }
    SharedString::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key};

    fn row(kind: ConversationKind, unread: u32, mentions: u32, muted: bool) -> Conversation {
        let key = Key::new(&AccountKey { network: Network::Slack, account: "T1".into() }, "c");
        Conversation { kind, unread, mentions, muted, ..Conversation::new(key, kind, "Mara") }
    }

    #[test]
    fn needs_me_is_unread_directs_and_mentions_with_mutes_respected() {
        assert!(needs(&row(ConversationKind::Direct, 2, 0, false)));
        assert!(!needs(&row(ConversationKind::Direct, 2, 0, true)));
        assert!(needs(&row(ConversationKind::Channel, 0, 1, false)));
        assert!(needs(&row(ConversationKind::Channel, 0, 1, true)));
        assert!(!needs(&row(ConversationKind::Channel, 3, 0, false)));
    }

    #[test]
    fn a_preview_is_one_line_with_its_breaks_as_spaces() {
        assert_eq!(one_line("hi\n\n  there\tfriend").as_ref(), "hi there friend");
        assert_eq!(one_line("  \n ").as_ref(), "");
    }
}
