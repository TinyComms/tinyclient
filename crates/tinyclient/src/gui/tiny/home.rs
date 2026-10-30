//! The server's home: what the pane shows with no channel picked, and where the server opens.
//! Its icon, name and description; the welcome its admins wrote; who is in which call now, with
//! Join beside each; and who else is online.

use super::{model, Modal, Page};
use crate::gui::components::{button, button_disabled, button_with_icon, glyph, Avatar, ButtonKind, Shape};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, MouseButton, MouseDownEvent, SharedString};
use protocol::TinyRoom;

/// The home's column is at most this wide, so lines stay readable in a wide window.
const READABLE: f32 = 720.;

pub(super) fn card() -> Div {
    div().p(px(16.)).rounded(px(theme::R_CARD)).bg(theme::raised()).border_1().border_color(theme::hairline())
}

pub(super) fn heading(words: &'static str, count: Option<usize>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .type_micro()
        .text_color(theme::text_faint())
        .child(words.to_uppercase())
        .children(count.map(|count| div().text_color(theme::text_faint()).child(format!("— {count}"))))
}

impl Shell {
    pub(super) fn tiny_home(&self, page: &Page, cx: &mut Context<Self>) -> AnyElement {
        let server = page.community.server.as_ref();
        let description = server.map(|server| server.description.trim().to_owned()).filter(|words| !words.is_empty());
        let welcome = server.map(|server| server.welcome.trim().to_owned()).filter(|words| !words.is_empty());
        let in_voice: Vec<&TinyRoom> = page.community.rooms.iter().filter(|room| !room.participants.is_empty()).collect();
        let seated: usize = in_voice.iter().map(|room| room.participants.len()).sum();
        let online = if page.live { page.community.online.len() } else { page.community.members.len() };
        let facts = format!(
            "{} {} · {} online · {} in voice",
            page.community.members.len(),
            if page.community.members.len() == 1 { "member" } else { "members" },
            online,
            seated
        );
        let avatar = Avatar::named(&page.name, 72.).shape(Shape::Place).image_at(page.icon()).build();
        let mut actions = div().flex().flex_wrap().gap(px(8.));
        if page.may.invite {
            actions = actions.child(
                button_with_icon("tiny-home-invite", ButtonKind::Primary, "icons/user-plus.svg", "Invite people")
                    .on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::Invite, window, cx))),
            );
        }
        if let Some(default) = server.and_then(|server| server.default_room_id.as_ref()).and_then(|id| page.community.rooms.iter().find(|room| room.id == *id)) {
            if page.mine != Some(default.id.as_str()) && page.community.allowed("community.calls.join") {
                let room_id = default.id.clone();
                actions = actions.child(
                    button_with_icon("tiny-home-default", ButtonKind::Secondary, "icons/volume-2.svg", format!("Join {}", default.name))
                        .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx))),
                );
            }
        }
        let hero = div()
            .flex()
            .items_center()
            .gap(px(20.))
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().min_w_0().truncate().type_headline().text_color(theme::text()).child(page.name.clone()))
                    .children(description.map(|words| div().whitespace_normal().type_ui().text_color(theme::text_soft()).child(words)))
                    .child(div().type_small().text_color(theme::text_faint()).child(facts)),
            );
        let mut column = div().w_full().max_w(px(READABLE)).px(px(32.)).pt(px(32.)).pb(px(40.)).flex().flex_col().gap(px(24.)).child(hero).child(actions);
        if let Some(welcome) = welcome {
            column = column.child(
                card()
                    .flex()
                    .gap(px(12.))
                    .child(div().pt(px(1.)).child(glyph("icons/message-square.svg", 16., theme::accent_text())))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Welcome"))
                            .child(div().whitespace_normal().type_body().text_color(theme::text_soft()).child(welcome)),
                    ),
            );
        }
        column = column.child(self.tiny_voice_now(page, &in_voice, cx));
        column = column.child(self.tiny_online_now(page, cx));
        div().id("tiny-home").size_full().overflow_y_scroll().flex().justify_center().child(column).into_any_element()
    }

    /// Each call with someone in it: its name and count, the faces in it, Join.
    fn tiny_voice_now(&self, page: &Page, rooms: &[&TinyRoom], cx: &mut Context<Self>) -> Div {
        let mut section = div().flex().flex_col().gap(px(8.)).child(heading("In voice now", Some(rooms.iter().map(|room| room.participants.len()).sum())));
        if rooms.is_empty() {
            return section.child(
                card().type_ui().text_color(theme::text_soft()).whitespace_normal().child(if page.community.rooms.is_empty() {
                    "This server has no voice channels yet."
                } else {
                    "Nobody is talking right now. Join a channel and others will see you there."
                }),
            );
        }
        for room in rooms {
            let mine = page.mine == Some(room.id.as_str());
            let faces = div().flex().items_center().gap(px(6.)).children(room.participants.iter().take(8).map(|seat| {
                let name = model::name_by_id(page.community, &seat.user_id);
                crate::gui::call::ringed(Avatar::new(&page.key(&seat.user_id), &name, 28.).build(), 28., seat.speaking && !seat.server_muted)
            }));
            let more = room.participants.len().saturating_sub(8);
            let names: Vec<String> = room.participants.iter().take(3).map(|seat| model::name_by_id(page.community, &seat.user_id).split_whitespace().next().unwrap_or("").to_owned()).collect();
            let who = match room.participants.len() {
                1 => names[0].clone(),
                2 => format!("{} and {}", names[0], names[1]),
                3 => format!("{}, {} and {}", names[0], names[1], names[2]),
                n => format!("{}, {} and {} others", names[0], names[1], n - 2),
            };
            let join: AnyElement = if mine {
                div().px(px(8.)).type_small().text_color(theme::positive()).child("You're here").into_any_element()
            } else if model::full(room) {
                button_disabled("Full").into_any_element()
            } else if self.tiny_state.call_busy || !page.community.allowed("community.calls.join") {
                button_disabled("Join").into_any_element()
            } else {
                let room_id = room.id.clone();
                button(SharedString::from(format!("tiny-home-join-{}", room.id)), ButtonKind::Secondary, "Join")
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            };
            let room_id = room.id.clone();
            section = section.child(
                card()
                    .id(SharedString::from(format!("tiny-home-room-{}", room.id)))
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::border()))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| this.tiny_pick_channel(Some(room_id.clone()), cx)))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(glyph("icons/volume-2.svg", 16., if mine { theme::positive() } else { theme::text_soft() }))
                                    .child(div().min_w_0().truncate().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(room.name.clone()))
                                    .child(div().type_small().text_color(theme::text_faint()).child(model::occupancy(room))),
                            )
                            .child(faces.when(more > 0, |faces| faces.child(div().type_small().text_color(theme::text_faint()).child(format!("+{more}")))))
                            .child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(who)),
                    )
                    .child(join),
            );
        }
        section
    }

    /// Everyone online who is not in a call, as chips.
    fn tiny_online_now(&self, page: &Page, cx: &mut Context<Self>) -> Div {
        let seated = |id: &str| model::seat_of(page.community, id).is_some();
        let mut people: Vec<_> = page
            .community
            .members
            .iter()
            .filter(|member| !seated(&member.user_id) && (!page.live || page.community.online.contains(&member.user_id)))
            .collect();
        people.sort_by_key(|member| model::name_of(member).to_lowercase());
        let mut grid = div().flex().flex_wrap().gap(px(8.));
        for member in &people {
            let name = model::name_of(member);
            let color = model::color_of(page.community, member).map(gpui::rgb).unwrap_or(theme::text());
            let user_id = member.user_id.clone();
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("tiny-home-person-{}", member.user_id)))
                    .h(px(36.))
                    .pl(px(4.))
                    .pr(px(12.))
                    .rounded(px(18.))
                    .bg(theme::raised())
                    .border_1()
                    .border_color(theme::hairline())
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::border()))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(super::Float::Person { user_id: user_id.clone(), at: event.position }, cx)),
                    )
                    .child(Avatar::new(&page.key(&member.user_id), &name, 28.).build())
                    .child(div().type_ui().text_color(color).child(name))
                    .when(member.user_id == page.me, |chip| chip.child(div().type_micro().text_color(theme::text_faint()).child("You"))),
            );
        }
        let title = if page.live { "Online, not in voice" } else { "Members" };
        div().flex().flex_col().gap(px(8.)).child(heading(title, Some(people.len()))).child(if people.is_empty() {
            div().type_ui().text_color(theme::text_soft()).child("Everyone online is in a call.")
        } else {
            grid
        })
    }
}
