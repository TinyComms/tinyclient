//! The pane on a Tiny server: one top bar across it, then the home or the channel on show, and
//! the member list at the right while it is open and the window has room for it.

use super::members::MEMBERS_WIDTH;
use crate::gui::chrome::{self, End};
use crate::gui::components::{glyph, icon_button, pane_title, tip};
use crate::gui::layout::{drags_window, RAIL_WIDTH, TOP_BAR};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};

/// The narrowest the home or channel may get before the member list gives way.
const MAIN_MIN: f32 = 440.;

impl Shell {
    /// `beside`: the sidebar is in the row with the pane. Without it (a narrow window), the
    /// channel list is drawn at the pane's left so the server can still be walked.
    pub(in crate::gui) fn tiny_page(&self, beside: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(page) = self.tiny_page_facts() else {
            let words = self.tiny_state.error.clone().unwrap_or_else(|| "Loading this server.".to_owned());
            let bad = self.tiny_state.error.is_some();
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(drags_window(div()).h(px(TOP_BAR)).flex_none().px(px(theme::PANE_PAD)).flex().items_center().gap(px(8.)).child(self.strip_controls(window, cx)).child(pane_title("Tiny server")))
                .child(div().px(px(theme::PANE_PAD)).pt(px(8.)).whitespace_normal().map(|line| if bad { line.type_small().text_color(theme::danger()) } else { line.type_ui().text_color(theme::text_soft()) }).child(words));
        };
        let room = self.tiny_state.channel.as_ref().and_then(|id| page.community.rooms.iter().find(|room| room.id == *id));
        let width = f32::from(window.viewport_size().width) - RAIL_WIDTH - if beside { self.layout_state.sidebar_width } else { 0. };
        let members = self.tiny_state.members_open && width - MEMBERS_WIDTH - if beside { 0. } else { 260. } >= MAIN_MIN;

        let title = match room {
            Some(room) => div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id("tiny-crumb-home")
                        .flex_none()
                        .h(px(28.))
                        .px(px(6.))
                        .ml(px(-6.))
                        .rounded(px(theme::R_CONTROL))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::hover()))
                        .tooltip(tip("Server home", None))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_pick_channel(None, cx)))
                        .child(div().max_w(px(160.)).truncate().type_ui().text_color(theme::text_soft()).child(page.name.clone())),
                )
                .child(glyph("icons/chevron-right.svg", 14., theme::text_faint()))
                .child(glyph("icons/volume-2.svg", 18., if page.mine == Some(room.id.as_str()) { theme::positive() } else { theme::text_soft() }))
                .child(div().flex_none().max_w(px(240.)).truncate().type_title().text_color(theme::text()).child(room.name.clone()))
                .child(self.tiny_channel_facts(room))
                .children(self.tiny_channel_security(room, cx)),
            None => div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(glyph("icons/server.svg", 18., theme::text_soft()))
                .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(page.name.clone()))
                .child(div().flex_none().type_ui().text_color(theme::text_faint()).child("Home")),
        };
        let toggle = icon_button("tiny-members-toggle", "icons/users.svg", members)
            .tooltip(tip(if members { "Hide members" } else { "Show members" }, None))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_members(cx)));
        let bar = drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .pl(px(theme::PANE_PAD))
            .pr(px(chrome::end_pad(End::Conversation, 12.)))
            .border_b_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(self.strip_controls(window, cx))
            .child(title)
            .child(toggle);
        let main = match room {
            Some(room) => self.tiny_channel(&page, room, cx),
            None => self.tiny_home(&page, cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(!beside, |row| row.child(div().w(px(260.)).h_full().flex_none().flex().flex_col().bg(theme::sidebar()).border_r_1().border_color(theme::hairline()).child(self.tiny_directory(cx))))
                    .child(div().flex_1().min_w_0().h_full().child(main))
                    .when(members, |row| row.child(self.tiny_members(&page, window, cx))),
            )
    }
}
