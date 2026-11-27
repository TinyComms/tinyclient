//! The pane on a Tiny server: one top bar across it, then the home or the channel on show, and
//! the member list at the right while it is open and the window has room for it.

use super::members::MEMBERS_WIDTH;
use super::Modal;
use crate::gui::chrome::{self, End};
use crate::gui::components::{glyph, icon_button, pane_title, tip};
use crate::gui::layout::{drags_window, RAIL_WIDTH, TOP_BAR};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Window};

/// The narrowest the home or channel may get before the member list gives way.
const MAIN_MIN: f32 = 440.;

/// Whether the inline 260 px directory column should be drawn inside `tiny_page`.
/// Never forced when the user explicitly hid the sidebar or when the pane is narrower than 580 px.
pub(in crate::gui) fn inline_directory_shown(beside: bool, phone: bool, sidebar_hidden: bool, width: f32) -> bool {
    !beside && !phone && !sidebar_hidden && width >= 580.
}

impl Shell {
    /// `beside`: the sidebar is in the row with the pane. Without it (a narrow window), the
    /// channel list is drawn at the pane's left when there is room and the sidebar wasn't hidden.
    pub(in crate::gui) fn tiny_page(&self, beside: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(page) = self.tiny_page_facts() else {
            let strip = div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    drags_window(div())
                        .h(px(TOP_BAR))
                        .flex_none()
                        .overflow_hidden()
                        .pl(px(theme::PANE_PAD))
                        .pr(px(chrome::end_pad(End::Conversation, 12.)))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .when(!crate::gui::phone::active(), |bar| bar.child(self.strip_controls(window, cx)))
                        .child(pane_title("Tiny server")),
                );
            let Some(detail) = self.tiny_state.error.clone() else {
                return strip.child(self.tiny_loading_pane(beside));
            };
            return strip.child(
                div()
                    .px(px(theme::PANE_PAD))
                    .pt(px(8.))
                    .whitespace_normal()
                    .map(|line| line.type_small().text_color(theme::danger()))
                    .child(detail),
            );
        };
        // A phone walks the server from its own sidebar, so the page is one column.
        let phone = crate::gui::phone::active();
        let room = self.tiny_state.channel.as_ref().and_then(|id| page.community.rooms.iter().find(|room| room.id == *id));
        let width = f32::from(window.viewport_size().width) - RAIL_WIDTH - if beside { self.layout_state.sidebar_width } else { 0. };
        let inline_directory = inline_directory_shown(beside, phone, self.layout_state.sidebar_hidden, width);
        let members = self.tiny_state.members_open && width - MEMBERS_WIDTH - if inline_directory { 260. } else { 0. } >= MAIN_MIN;
        let end_pad = chrome::end_pad(End::Conversation, 12.);
        let header_usable = (width - end_pad).max(0.);

        let title = match room {
            Some(room) => {
                // The sidebar beside the pane already names the server, so the crumb to it is a
                // small icon; only a pane without its sidebar spells the name out.
                let home = if beside {
                    icon_button("tiny-crumb-home", "icons/server.svg", false).size(px(28.)).tooltip(tip(format!("Back to {} home", page.name), None))
                } else {
                    div()
                        .id("tiny-crumb-home")
                        .flex_initial()
                        .min_w(px(48.))
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
                        .child(div().min_w_0().max_w(px(160.)).truncate().type_ui().text_color(theme::text_soft()).child(page.name.clone()))
                };
                let room_security = self
                    .tiny_call()
                    .filter(|call| call.conversation.id == room.id && call.state == protocol::CallState::Joined)
                    .and_then(|call| self.call_security_in(&call, crate::gui::call::security::Place::TopBar, Some(window), cx));
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        home.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| this.tiny_pick_channel(None, cx))),
                    )
                    .child(div().flex_none().child(glyph("icons/chevron-right.svg", 14., theme::text_faint())))
                    .child(div().flex_none().child(glyph("icons/volume-2.svg", 18., if page.mine == Some(room.id.as_str()) { theme::positive() } else { theme::text_soft() })))
                    .child(div().flex_initial().min_w(px(64.)).max_w(px(240.)).truncate().type_title().text_color(theme::text()).child(room.name.clone()))
                    .when(header_usable >= 560., |row| row.child(self.tiny_channel_facts(&page, room, cx)))
                    .children(room_security)
            }
            None => div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_none().child(glyph("icons/server.svg", 18., theme::text_soft())))
                .child(div().min_w_0().truncate().type_title().text_color(theme::text()).child(page.name.clone()))
                .child(div().flex_none().type_ui().text_color(theme::text_faint()).child("Home")),
        };
        let toggle = icon_button("tiny-members-toggle", "icons/users.svg", members)
            .tooltip(tip(if members { "Hide members" } else { "Show members" }, None))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_members(cx)));
        let invite = page.may.invite.then(|| {
            icon_button("tiny-invite", "icons/user-plus.svg", false)
                .tooltip(tip("Invite people", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::Invite, window, cx)))
        });
        let copy_link = room.map(|room| {
            let link = protocol::room_link(&page.community.origin, &room.id)
                .unwrap_or_else(|| format!("tinyclient://room?server={}&id={}", protocol::join_host(&page.community.origin), room.id));
            icon_button("tiny-copy-channel-link", "icons/link.svg", false)
                .tooltip(tip("Copy channel link", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_copy(link.clone(), "Channel link copied", cx)))
        });
        // The window's own buttons sit over this strip's right end: a hairline keeps Members off them.
        let window_buttons = end_pad > 12.;
        let bar = drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .overflow_hidden()
            .pl(px(theme::PANE_PAD))
            .pr(px(end_pad))
            .border_b_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            // A phone has no sidebar to hide and goes back with its own gesture.
            .when(!phone, |bar| bar.child(self.strip_controls(window, cx)))
            .child(title)
            .child(div().flex_none().flex().items_center().gap(px(4.)).children(copy_link).children(invite).child(toggle))
            .when(window_buttons, |bar| bar.child(div().flex_none().w(px(1.)).h(px(16.)).bg(theme::hairline())));
        let room_popover = room.and_then(|room| {
            let call = self.tiny_call().filter(|call| call.conversation.id == room.id)?;
            self.call_view_popover_layer(&call, window, cx)
        });
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
                    .when(inline_directory, |row| row.child(div().w(px(260.)).h_full().flex_none().flex().flex_col().bg(theme::sidebar()).border_r_1().border_color(theme::hairline()).child(self.tiny_directory(cx))))
                    .child(div().flex_1().min_w_0().h_full().child(main))
                    .when(members, |row| row.child(self.tiny_members(&page, window, cx))),
            )
            .children(room_popover)
    }
}
