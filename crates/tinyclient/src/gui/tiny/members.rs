//! The member list beside the pane: online people grouped under the roles shown separately, in
//! the role's colour, then the rest online, then Offline; a search on top. Hovering someone
//! shows their card (roles, joined, last seen, and their email to those who manage people);
//! right-clicking opens the person menu. I am marked "You".

use super::{model, Float, Page};
use crate::gui::components::{field, glyph, Avatar};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Focusable, MouseButton, MouseDownEvent, SharedString, Window};
use protocol::TinyMember;

pub(super) const MEMBERS_WIDTH: f32 = 248.;

impl Shell {
    pub(super) fn tiny_members(&self, page: &Page, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let query = self.tiny_state.search_input.read(cx).text().to_owned();
        let groups = model::groups(page.community, page.live, &query);
        let focused = self.tiny_state.search_input.focus_handle(cx).is_focused(window);
        let mut list = div().id("tiny-members").flex_1().min_h_0().overflow_y_scroll().pb(px(12.)).flex().flex_col();
        if groups.is_empty() {
            list = list.child(div().px(px(theme::PANE_PAD)).pt(px(12.)).type_ui().text_color(theme::text_soft()).child(format!("Nobody called “{}”", query.trim())));
        }
        for group in &groups {
            let color = group.color.map(gpui::rgb).unwrap_or(theme::text_faint());
            list = list.child(
                div()
                    .flex_none()
                    .h(px(theme::SECTION + 4.))
                    .px(px(theme::PANE_PAD))
                    .pb(px(4.))
                    .flex()
                    .items_end()
                    .gap(px(6.))
                    .type_micro()
                    .text_color(theme::text_faint())
                    .when(group.color.is_some(), |header| header.child(div().mb(px(4.)).size(px(8.)).rounded_full().bg(color)))
                    .child(format!("{} — {}", group.title.to_uppercase(), group.people.len())),
            );
            for user_id in &group.people {
                if let Some(member) = page.community.member(user_id) {
                    list = list.child(self.tiny_member_row(page, member, group.offline, cx));
                }
            }
        }
        div()
            .w(px(MEMBERS_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .border_l_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .pt(px(12.))
                    .pb(px(4.))
                    .child(field(focused).child(glyph("icons/search.svg", 14., theme::text_faint())).child(div().flex_1().min_w_0().child(self.tiny_state.search_input.clone()))),
            )
            .child(list)
            .into_any_element()
    }

    fn tiny_member_row(&self, page: &Page, member: &TinyMember, offline: bool, cx: &mut Context<Self>) -> AnyElement {
        let name = model::name_of(member);
        let me = member.user_id == page.me;
        let mut avatar = Avatar::new(&page.key(&member.user_id), &name, 32.);
        if let Some(presence) = page.online(&member.user_id) {
            avatar = avatar.presence(presence, theme::sidebar());
        }
        let color = model::color_of(page.community, member).map(gpui::rgb).unwrap_or(theme::text());
        let seat = model::seat_of(page.community, &member.user_id);
        let second = match (&seat, &member.timeout_until) {
            (_, Some(_)) => Some(("Timed out".to_owned(), theme::warning())),
            (Some((room, _)), None) => Some((format!("In {}", room.name), theme::text_soft())),
            (None, None) if model::is_owner(member) && page.community.roles.is_empty() => None,
            _ => None,
        };
        let hovered = self.tiny_state.card.as_ref().is_some_and(|(id, _)| *id == member.user_id);
        let (hover_id, menu_id) = (member.user_id.clone(), member.user_id.clone());
        div()
            .id(SharedString::from(format!("tiny-member-{}", member.user_id)))
            .flex_none()
            .w_full()
            .h(px(theme::row_height(44.)))
            .px(px(theme::INSET))
            .child(
                div()
                    .size_full()
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(hovered, |row| row.bg(theme::hover()))
                    .when(offline, |row| row.opacity(0.45))
                    .child(avatar.build())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(div().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(color).child(name))
                                    .when(model::is_owner(member), |row| row.child(glyph("icons/crown.svg", 12., theme::warning())))
                                    .when(me, |row| row.child(super::channel::chip("You", theme::text_soft(), theme::hover()))),
                            )
                            .children(second.map(|(words, tone)| div().min_w_0().truncate().type_small().text_color(tone).child(words))),
                    ),
            )
            .on_hover(cx.listener(move |this, over: &bool, window, cx| {
                let y = f32::from(window.mouse_position().y);
                this.tiny_hover_card(&hover_id, *over, y, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Person { user_id: menu_id.clone(), at: event.position }, cx)),
            )
            .into_any_element()
    }

    /// The card beside a hovered member: who they are here.
    pub(super) fn tiny_person_card(&self, page: &Page, member: &TinyMember, wide: bool) -> Div {
        let name = model::name_of(member);
        let mut avatar = Avatar::new(&page.key(&member.user_id), &name, if wide { 72. } else { 56. });
        if let Some(presence) = page.online(&member.user_id) {
            avatar = avatar.presence(presence, theme::overlay());
        }
        let color = model::color_of(page.community, member).map(gpui::rgb).unwrap_or(theme::text());
        let roles = model::roles_of(page.community, member);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
        let joined = protocol::tiny_unix(&member.created_at).map(crate::gui::time::date);
        let online = page.online(&member.user_id) == Some(protocol::Presence::Online);
        let seen = if online {
            Some("Online now".to_owned())
        } else {
            member.last_seen_at.as_deref().and_then(protocol::tiny_unix).map(|at| format!("Last seen {}", model::relative(now, at)))
        };
        let timeout = member.timeout_until.as_deref().and_then(protocol::tiny_unix).filter(|until| *until > now).map(|until| format!("Timed out, ends {}", model::relative(now, until)));
        let fact = |label: &'static str, value: String| {
            div().flex().flex_col().gap(px(2.)).child(div().type_micro().text_color(theme::text_faint()).child(label.to_uppercase())).child(div().type_ui().text_color(theme::text()).child(value))
        };
        let legacy_role = page.community.roles.is_empty().then(|| model::role_word(&member.role));
        let mut card = div()
            .w(px(if wide { 340. } else { 280. }))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(avatar.build())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(div().min_w_0().truncate().type_title().text_color(color).child(name))
                                    .when(model::is_owner(member), |row| row.child(glyph("icons/crown.svg", 14., theme::warning()))),
                            )
                            .when(member.user_id == page.me, |column| column.child(div().type_small().text_color(theme::text_faint()).child("This is you")))
                            .when(page.may.see_email && !member.email.is_empty(), |column| {
                                column.child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(member.email.clone()))
                            }),
                    ),
            );
        if !roles.is_empty() || legacy_role.is_some() {
            let mut chips = div().flex().flex_wrap().gap(px(4.));
            for role in roles {
                let tone = role.color.as_deref().and_then(model::hex).map(gpui::rgb).unwrap_or(theme::text_faint());
                chips = chips.child(
                    div()
                        .h(px(22.))
                        .px(px(8.))
                        .rounded(px(11.))
                        .bg(theme::hover())
                        .border_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().size(px(8.)).rounded_full().bg(tone))
                        .child(div().type_small().text_color(theme::text()).child(role.name.clone())),
                );
            }
            if let Some(word) = legacy_role {
                chips = chips.child(super::channel::chip(word, theme::text(), theme::hover()));
            }
            card = card.child(div().flex().flex_col().gap(px(6.)).child(div().type_micro().text_color(theme::text_faint()).child("ROLES")).child(chips));
        }
        let mut facts = div().flex().gap(px(24.));
        if let Some(joined) = joined {
            facts = facts.child(fact("Joined", joined));
        }
        if let Some(seen) = seen {
            facts = facts.child(fact("Seen", seen));
        }
        card = card.child(facts);
        if let Some((room, _)) = model::seat_of(page.community, &member.user_id) {
            card = card.child(
                div().flex().items_center().gap(px(6.)).child(glyph("icons/volume-2.svg", 14., theme::positive())).child(div().type_small().text_color(theme::text_soft()).child(format!("In {}", room.name))),
            );
        }
        if let Some(timeout) = timeout {
            card = card.child(div().flex().items_center().gap(px(6.)).child(glyph("icons/timer.svg", 14., theme::warning())).child(div().type_small().text_color(theme::warning()).child(timeout)));
        }
        card
    }
}
