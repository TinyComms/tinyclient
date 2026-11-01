//! The server's column, in the sidebar: the header with the server's name and menu, then the
//! channel list. Categories fold; each voice channel shows who is in its call underneath, with a
//! speaking ring and what is true of each seat in small glyphs. A click shows a channel, a
//! double click joins it. With the right to, a person is dragged onto another channel to move
//! them, and channels and categories are dragged into a new order.

use super::model::{self, Line};
use super::{Dragged, Float, Modal, Page};
use crate::gui::components::{glyph, list_row, network_icon, tip, Avatar, Shape as AvatarShape};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, MouseButton, MouseDownEvent, Render, SharedString, Stateful, Window};
use protocol::{Conversation, ConversationKind, Key, Network, TinyRoom, TinySeat};
use std::rc::Rc;

/// Text channels only. A direct message is a People chat, not a row in the server.
fn in_the_server(chats: &[Rc<Conversation>]) -> Vec<Rc<Conversation>> {
    chats.iter().filter(|chat| chat.kind == ConversationKind::Channel).cloned().collect()
}

/// Text channels above the voice rooms. The header is omitted when empty. Direct messages are
/// not channels of the server; they live in People.
fn text_section(mut list: Stateful<Div>, label: &str, chats: &[Rc<Conversation>], kind: ConversationKind, open: Option<&Key>, cx: &mut Context<Shell>) -> Stateful<Div> {
    let rows: Vec<&Rc<Conversation>> = chats.iter().filter(|chat| chat.kind == kind).collect();
    if rows.is_empty() {
        return list;
    }
    list = list.child(
        div()
            .px(px(theme::INSET + 2.))
            .pt(px(10.))
            .pb(px(2.))
            .text_size(px(11. + theme::text_step() as f32))
            .line_height(px(14. + theme::text_step() as f32))
            .font_weight(theme::SEMIBOLD)
            .text_color(theme::text_faint())
            .child(label.to_uppercase()),
    );
    for chat in rows {
        list = list.child(text_row(chat, open, cx));
    }
    list
}

/// A text channel or a direct message. Picking it opens the conversation pane.
fn text_row(chat: &Conversation, open: Option<&Key>, cx: &mut Context<Shell>) -> Stateful<Div> {
    let selected = open.is_some_and(|open| open == &chat.key);
    let icon = if chat.kind == ConversationKind::Direct { "icons/user.svg" } else { "icons/hash.svg" };
    let tone = if selected { theme::text() } else { theme::text_soft() };
    let content = div()
        .gap(px(8.))
        .child(glyph(icon, 16., tone))
        .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(chat.title.clone()));
    let key = chat.key.clone();
    let title = chat.title.clone();
    list_row(SharedString::from(format!("tiny-chat-{}", key.id)), theme::row_height(theme::ROW_ONE), selected, false, content)
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.select(key.clone(), &title, cx)))
}

/// What is drawn under the pointer while something is dragged.
pub(super) struct Ghost(pub(super) SharedString);

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.))
            .h(px(28.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_popover())
            .flex()
            .items_center()
            .font_family(theme::FONT)
            .type_ui()
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

impl Dragged {
    pub(super) fn label(&self) -> SharedString {
        match self {
            Dragged::Person { name, .. } => name.clone().into(),
            Dragged::Room { name, .. } => name.clone().into(),
            Dragged::Category { name, .. } => name.clone().into(),
            Dragged::Role { name, .. } => name.clone().into(),
        }
    }
}

/// The small glyphs after a seated person's name, in the order TeamSpeak draws them: what the
/// server did to them first, in `danger`, then what they chose, in `text_faint`.
pub(super) fn seat_glyphs(seat: &TinySeat) -> Vec<(&'static str, gpui::Rgba, &'static str)> {
    let mut out = Vec::new();
    if seat.priority {
        out.push(("icons/megaphone.svg", theme::accent_text(), "Priority speaker"));
    }
    if seat.server_deafened {
        out.push(("icons/headphone-off.svg", theme::danger(), "Server deafened"));
    } else if seat.deafened {
        out.push(("icons/headphone-off.svg", theme::text_faint(), "Deafened"));
    }
    if seat.server_muted {
        out.push(("icons/mic-off.svg", theme::danger(), "Server muted"));
    } else if seat.suppressed {
        out.push(("icons/mic-off.svg", theme::warning(), "Can't speak in this channel"));
    } else if seat.muted && !seat.deafened {
        out.push(("icons/mic-off.svg", theme::text_faint(), "Muted"));
    }
    if seat.sharing {
        out.push(("icons/monitor-up.svg", theme::accent(), "Sharing their screen"));
    }
    if seat.camera {
        out.push(("icons/video.svg", theme::accent(), "Camera on"));
    }
    out
}

impl Shell {
    /// The sidebar's title strip on a Tiny server: the server's icon and name, which open its
    /// menu.
    pub(in crate::gui) fn tiny_header(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let page = self.tiny_page_facts();
        let name = page.as_ref().map(|page| page.name.clone()).unwrap_or_else(|| "Tiny server".to_owned());
        // Before the server answers there is no icon to show: the network's mark stands in, as
        // the pane's splash wears it.
        let avatar = match page.as_ref() {
            Some(page) => Avatar::named(&name, 24.).shape(AvatarShape::Place).image_at(page.icon()).build().into_any_element(),
            None => div().size(px(24.)).flex_none().child(network_icon(Network::Tiny, 24.)).into_any_element(),
        };
        let open = self.tiny_state.float == Some(Float::Header);
        div()
            .id("tiny-header")
            .flex_1()
            .min_w_0()
            .h(px(36.))
            .ml(px(-8.))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .when(open, |header| header.bg(theme::selected()))
            .when(!open, |header| header.hover(|style| style.bg(theme::hover())))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                if this.tiny_state.float == Some(Float::Header) {
                    this.tiny_close_float(cx);
                } else {
                    this.tiny_open_float(Float::Header, cx);
                }
            }))
            .child(avatar)
            .child(div().flex_1().min_w_0().truncate().type_title().text_color(theme::text()).child(name))
            .child(glyph(if open { "icons/x.svg" } else { "icons/chevron-down.svg" }, 14., theme::text_soft()))
    }

    /// The channel list. Loading shows the rows it will hold as shapes; a failure says so in its
    /// place.
    pub(in crate::gui) fn tiny_directory(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = self.tiny_page_facts() else {
            let Some(detail) = self.tiny_state.error.clone() else {
                return self.tiny_loading_column();
            };
            let line = div().px(px(theme::PANE_PAD)).pt(px(12.)).whitespace_normal();
            return line.type_small().text_color(theme::danger()).child(detail).into_any_element();
        };
        let lines = model::lines(page.community, &self.tiny_state.folded, page.mine);
        let mut list = div().id("tiny-channels").size_full().overflow_y_scroll().pt(px(4.)).pb(px(12.)).flex().flex_col();
        let chats = self.tiny_chats();
        list = text_section(list, "Text channels", &chats, ConversationKind::Channel, self.open.as_ref(), cx);
        if page.community.rooms.is_empty() && chats.is_empty() {
            list = list.child(
                div()
                    .px(px(theme::PANE_PAD))
                    .pt(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().type_ui().text_color(theme::text()).child("No voice channels yet"))
                    .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(if page.may.make_room {
                        "Make one from the server menu above."
                    } else {
                        "Channels this server makes show up here."
                    })),
            );
        }
        for (ix, line) in lines.iter().enumerate() {
            let row = match line {
                Line::Category { id, name, folded, key, rooms } => self.category_row(&page, id.as_deref(), name, *folded, key, *rooms, ix == 0, cx),
                Line::Room { id } => match page.community.rooms.iter().find(|room| room.id == *id) {
                    Some(room) => self.room_row(&page, room, cx),
                    None => continue,
                },
                Line::Seat { room, user } => {
                    let found = page.community.rooms.iter().find(|r| r.id == *room).and_then(|r| r.participants.iter().find(|seat| seat.user_id == *user));
                    match found {
                        Some(seat) => self.seat_row(&page, room, seat, cx),
                        None => continue,
                    }
                }
            };
            list = list.child(row);
        }
        list.into_any_element()
    }

    /// Text channels in this server's space, in the tree's order. Direct messages are kept in
    /// the store for People and are not drawn here.
    fn tiny_chats(&self) -> Vec<Rc<Conversation>> {
        let Some(account) = self.tiny_state.account.as_ref() else {
            return Vec::new();
        };
        let Some(tree) = self.trees.get(&Key::new(account, "server")) else {
            return Vec::new();
        };
        in_the_server(&tree.conversations)
    }

    #[allow(clippy::too_many_arguments)]
    fn category_row(&self, page: &Page, id: Option<&str>, name: &str, folded: bool, key: &str, rooms: usize, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let fold_key = key.to_owned();
        let menu_key = key.to_owned();
        let label = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(px(11. + theme::text_step() as f32))
            .line_height(px(14. + theme::text_step() as f32))
            .font_weight(theme::SEMIBOLD)
            .child(name.to_uppercase());
        let mut row = div()
            .id(SharedString::from(format!("tiny-cat-{key}")))
            .group("tiny-cat")
            .flex_none()
            .w_full()
            .h(px(if first { theme::SECTION_FIRST } else { theme::SECTION + 6. }))
            .pl(px(theme::INSET + 2.))
            .pr(px(theme::INSET + 4.))
            .pb(px(4.))
            .flex()
            .items_end()
            .gap(px(4.))
            .cursor_pointer()
            .text_color(theme::text_faint())
            .hover(|style| style.text_color(theme::text_soft()))
            .on_click(cx.listener(move |this, _, _, cx| this.tiny_fold(fold_key.clone(), cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Category { key: menu_key.clone(), at: event.position }, cx)),
            )
            .child(div().pb(px(1.)).child(glyph(if folded { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" }, 12., theme::text_faint())))
            .child(label)
            .when(folded && rooms > 0, |row| row.child(div().type_micro().text_color(theme::text_faint()).child(rooms.to_string())));
        if page.may.make_room {
            let category = id.map(str::to_owned);
            row = row.child(
                div()
                    .id(SharedString::from(format!("tiny-cat-add-{key}")))
                    .size(px(18.))
                    .rounded(px(theme::R_CHIP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .invisible()
                    .group_hover("tiny-cat", |style| style.visible())
                    .hover(|style| style.bg(theme::hover()))
                    .tooltip(tip("Create channel", None))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.tiny_open_modal(super::Modal::NewRoom(category.clone()), window, cx);
                    }))
                    .child(glyph("icons/plus.svg", 14., theme::text_soft())),
            );
        }
        if let (Some(category_id), true) = (id, page.may.layout) {
            let dragged = Dragged::Category { category_id: category_id.to_owned(), name: name.to_owned() };
            let target = category_id.to_owned();
            row = row
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| Ghost(dragged.label())))
                .drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft()).rounded(px(theme::R_CONTROL)))
                .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| this.tiny_dropped_on_category(Some(target.clone()), dragged, cx)));
        } else if id.is_none() && page.may.layout && !page.community.categories.is_empty() {
            row = row
                .drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft()))
                .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| this.tiny_dropped_on_category(None, dragged, cx)));
        }
        row.into_any_element()
    }

    fn room_row(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.tiny_state.channel.as_deref() == Some(room.id.as_str());
        let mine = page.mine == Some(room.id.as_str());
        let empty = room.participants.is_empty();
        let tone = if mine || selected { theme::text() } else if empty { theme::text_soft() } else { theme::text() };
        let icon_tone = if mine { theme::positive() } else { theme::text_faint() };
        let count = (!empty || room.user_limit > 0).then(|| {
            div()
                .flex_none()
                .px(px(5.))
                .h(px(16.))
                .rounded(px(theme::R_CHIP))
                .when(model::full(room), |chip| chip.bg(theme::warning_soft()))
                .flex()
                .items_center()
                .type_micro()
                .text_color(if model::full(room) { theme::warning() } else { theme::text_faint() })
                .child(model::occupancy(room))
        });
        let content = div()
            .gap(px(8.))
            .child(glyph("icons/volume-2.svg", 16., icon_tone))
            .child(div().flex_1().min_w_0().truncate().type_ui().when(mine, |name| name.font_weight(theme::MEDIUM)).text_color(tone).child(room.name.clone()))
            .when(room.ptt_only, |row| row.child(glyph("icons/radio.svg", 12., theme::text_faint())))
            .children(count);
        let (pick, join, menu) = (room.id.clone(), room.id.clone(), room.id.clone());
        let joinable = page.mine != Some(room.id.as_str()) && page.community.allowed("community.calls.join");
        let mut row = list_row(SharedString::from(format!("tiny-room-{}", room.id)), theme::row_height(theme::ROW_ONE), selected, false, content)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if event.click_count >= 2 && joinable {
                        this.tiny_join(join.clone(), cx);
                    } else {
                        this.tiny_pick_channel(Some(pick.clone()), cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Channel { room_id: menu.clone(), at: event.position }, cx)),
            );
        if page.may.layout {
            row = row.on_drag(Dragged::Room { room_id: room.id.clone(), name: room.name.clone() }, |dragged, _, _, cx| cx.new(|_| Ghost(dragged.label())));
        }
        let target = room.id.clone();
        row.drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft()).rounded(px(theme::R_CONTROL)))
            .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| this.tiny_dropped_on_room(target.clone(), dragged, cx)))
            .into_any_element()
    }

    fn seat_row(&self, page: &Page, room_id: &str, seat: &TinySeat, cx: &mut Context<Self>) -> AnyElement {
        let name = model::name_by_id(page.community, &seat.user_id);
        let me = seat.user_id == page.me;
        // My mute and my ring as my call has them, before the server echoes them back.
        let call = self.tiny_call().filter(|call| call.conversation.id == room_id);
        let seat = &super::channel::as_heard(seat, me, call.as_ref());
        let avatar = super::super::call::ringed(Avatar::new(&page.key(&seat.user_id), &name, 20.).image_at(page.picture(&seat.user_id)).build(), 20., seat.speaking && !seat.server_muted);
        let glyphs = seat_glyphs(seat);
        let content = div()
            .pl(px(24.))
            .gap(px(8.))
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_ui()
                    .text_color(if seat.speaking { theme::text() } else { theme::text_soft() })
                    .child(name.clone()),
            )
            .when(me, |row| row.child(div().flex_none().type_micro().text_color(theme::text_faint()).child("You")))
            .child(div().flex_none().flex().items_center().gap(px(4.)).children(glyphs.into_iter().map(|(icon, color, words)| {
                div().id(SharedString::from(format!("tiny-seat-glyph-{}-{icon}", seat.user_id))).tooltip(tip(words, None)).child(glyph(icon, 14., color))
            })));
        let menu_id = seat.user_id.clone();
        let open_id = seat.user_id.clone();
        let mut row = list_row(SharedString::from(format!("tiny-seat-{room_id}-{}", seat.user_id)), theme::row_height(theme::ROW_ONE), false, false, content)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::Profile(open_id.clone()), window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Person { user_id: menu_id.clone(), at: event.position }, cx)),
            );
        if !model::person_menu(page.community, &seat.user_id).move_to.is_empty() {
            row = row.cursor_grab().on_drag(Dragged::Person { user_id: seat.user_id.clone(), name }, |dragged, _, _, cx| cx.new(|_| Ghost(dragged.label())));
        }
        row.into_any_element()
    }

    pub(super) fn tiny_dropped_on_room(&mut self, room_id: String, dragged: &Dragged, cx: &mut Context<Self>) {
        let Some(community) = self.tiny_state.community.as_ref() else {
            return;
        };
        match dragged {
            Dragged::Person { user_id, .. } => {
                if model::person_menu(community, user_id).move_to.iter().any(|(id, _)| *id == room_id) {
                    self.tiny_move(user_id.clone(), room_id, cx);
                }
            }
            Dragged::Room { room_id: moved, .. } if *moved != room_id => {
                let category = community.rooms.iter().find(|room| room.id == room_id).and_then(|room| room.category_id.clone());
                let category = category.filter(|id| community.categories.iter().any(|category| category.id == *id));
                let groups = model::move_room(model::layout_of(community), moved, category.as_deref(), Some(&room_id));
                self.tiny_layout(groups, cx);
            }
            _ => {}
        }
    }

    pub(super) fn tiny_dropped_on_category(&mut self, category: Option<String>, dragged: &Dragged, cx: &mut Context<Self>) {
        let Some(community) = self.tiny_state.community.as_ref() else {
            return;
        };
        match dragged {
            Dragged::Room { room_id, .. } => {
                let groups = model::move_room(model::layout_of(community), room_id, category.as_deref(), None);
                self.tiny_layout(groups, cx);
            }
            Dragged::Category { category_id, .. } => {
                if let Some(before) = category.filter(|before| before != category_id) {
                    let groups = model::move_category(model::layout_of(community), category_id, &before);
                    self.tiny_layout(groups, cx);
                }
            }
            Dragged::Person { .. } | Dragged::Role { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::AccountKey;
    use protocol::Network;

    #[test]
    fn direct_messages_are_not_rows_of_the_server() {
        let account = AccountKey { network: Network::Tiny, account: "host".into() };
        let channel = Rc::new(Conversation::new(Key::new(&account, "c"), ConversationKind::Channel, "general"));
        let direct = Rc::new(Conversation::new(Key::new(&account, "d"), ConversationKind::Direct, "Ada"));
        let voice = Rc::new(Conversation::new(Key::new(&account, "v"), ConversationKind::Voice, "Lobby"));
        let shown = in_the_server(&[channel, direct, voice]);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title, "general");
    }
}
