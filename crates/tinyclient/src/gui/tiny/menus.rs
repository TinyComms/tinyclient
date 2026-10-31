//! The menus: the server header's, a channel's, a category's, and the person menu, which opens
//! wherever a person is drawn (a seat in the column, a tile, the member list, the home). The
//! person menu holds what is mine to choose (their volume for me, 0 to 200 %, and Mute for me),
//! then moderation, then membership. What this account may not do to them is absent (`model`).

use super::model::{self, PersonMenu};
use super::{Modal, Page, Sub};
use crate::gui::components::{
    glyph, menu, menu_item, menu_item_check, menu_item_danger, menu_item_more, menu_label, menu_separator, slider, Avatar, NO_GLYPH,
};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{canvas, div, prelude::*, px, Context, Div, MouseButton, MouseDownEvent, SharedString, Stateful};
use protocol::Network;

const SLIDER: f32 = 132.;

/// Keeps a submenu's anchor up to date: the bounds of the item that opens it.
fn anchored(item: Stateful<Div>, anchor: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>) -> Stateful<Div> {
    item.relative().child(canvas(move |bounds, _, _| anchor.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
}

impl Shell {
    /// An item that closes any submenu when the pointer reaches it.
    fn plain(&self, item: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        item.on_hover(cx.listener(|this, over: &bool, _, cx| {
            if *over && this.tiny_state.sub.is_some() {
                this.tiny_state.sub = None;
                cx.notify();
            }
        }))
    }

    fn more(&self, id: &'static str, icon: &'static str, label: &'static str, sub: Sub, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.tiny_state.sub == Some(sub);
        let item = menu_item_more(id, Some(icon), label, open).on_hover(cx.listener(move |this, over: &bool, _, cx| {
            if *over && this.tiny_state.sub != Some(sub) {
                this.tiny_state.sub = Some(sub);
                cx.notify();
            }
        }));
        let item = item.on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_state.sub = Some(sub);
            cx.notify();
        }));
        if open {
            anchored(item, self.tiny_state.sub_anchor.clone())
        } else {
            item
        }
    }

    pub(super) fn tiny_header_menu(&self, page: &Page, cx: &mut Context<Self>) -> Div {
        let may = page.may;
        let mut items = menu().w(px(240.));
        if may.invite {
            items = items.child(self.plain(menu_item("tiny-menu-invite", Some("icons/user-plus.svg"), "Invite people", None), cx).on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::Invite, window, cx))));
        }
        items = items.child(
            self.plain(menu_item("tiny-menu-settings", Some("icons/settings.svg"), "Server settings", None), cx)
                .on_click(cx.listener(|this, _, window, cx| this.tiny_open_settings(super::settings::Page::Overview, None, window, cx))),
        );
        if may.make_room || may.categories {
            items = items.child(menu_separator());
        }
        if may.make_room {
            items = items.child(self.plain(menu_item("tiny-menu-channel", Some("icons/plus.svg"), "Create channel", None), cx).on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::NewRoom(None), window, cx))));
        }
        if may.categories {
            items = items.child(self.plain(menu_item("tiny-menu-category", Some("icons/folder-plus.svg"), "Create category", None), cx).on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::NewCategory, window, cx))));
        }
        let origin = page.community.origin.clone();
        items = items.child(menu_separator()).child(self.plain(menu_item("tiny-menu-copy", Some("icons/link.svg"), "Copy server address", None), cx).on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_close_float(cx);
            this.tiny_copy(origin.clone(), "Server address copied", cx);
        })));
        // Other servers this window is signed in to, and joining another.
        let snapshot = self.snapshot.as_ref();
        let others: Vec<_> = snapshot.map(|snapshot| snapshot.accounts.iter().filter(|account| account.key.network == Network::Tiny).cloned().collect()).unwrap_or_default();
        if others.len() > 1 {
            items = items.child(menu_separator()).child(menu_label("Servers"));
            for (ix, account) in others.into_iter().enumerate() {
                let on = account.key == *page.account;
                let key = account.key.clone();
                let name = if account.name.is_empty() { protocol::join_host(&account.key.account) } else { account.name.clone() };
                items = items.child(self.plain(menu_item_check(("tiny-menu-server", ix), on, name, None), cx).on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_close_float(cx);
                    this.prefer_tiny(key.clone(), cx);
                })));
            }
        }
        items
            .child(menu_separator())
            .child(self.plain(menu_item("tiny-menu-join", Some("icons/log-in.svg"), "Join another server", None), cx).on_click(cx.listener(|this, _, _, cx| {
                this.tiny_close_float(cx);
                this.sign_in(Network::Tiny, cx);
            })))
            .child(self.plain(menu_item_danger("tiny-menu-sign-out", Some("icons/log-out.svg"), "Sign out of this server…"), cx).on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::SignOut, window, cx))))
    }

    pub(super) fn tiny_person_menu(&self, page: &Page, user_id: &str, cx: &mut Context<Self>) -> Div {
        let Some(member) = page.community.member(user_id) else {
            return menu();
        };
        let facts: PersonMenu = model::person_menu(page.community, user_id);
        let name = model::name_of(member);
        let color = model::color_of(page.community, member).map(gpui::rgb).unwrap_or(theme::text());
        let mut items = menu().w(px(260.)).child(
            div()
                .h(px(44.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(Avatar::new(&page.key(user_id), &name, 28.).image_at(page.picture(user_id)).build())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().min_w_0().truncate().type_ui().font_weight(theme::SEMIBOLD).text_color(color).child(name.clone()))
                        .child(div().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(if facts.me {
                            "You".to_owned()
                        } else {
                            model::roles_of(page.community, member).first().map(|role| role.name.clone()).unwrap_or_else(|| model::role_word(&member.role).to_owned())
                        })),
                ),
        );
        let id = user_id.to_owned();
        items = items.child(menu_separator()).child(self.plain(menu_item("tiny-person-profile", Some("icons/user.svg"), "Profile", None), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::Profile(id.clone()), window, cx))));
        if facts.me {
            items = items.child(self.plain(menu_item("tiny-person-picture", Some("icons/upload.svg"), "Change picture", None), cx).on_click(cx.listener(|this, _, _, cx| {
                this.tiny_close_float(cx);
                this.tiny_pick_avatar(cx);
            })));
        } else {
            let message_id = user_id.to_owned();
            items = items.child(self.plain(menu_item("tiny-person-message", Some("icons/message-square.svg"), "Message", None), cx).on_click(cx.listener(move |this, _, _, cx| this.tiny_message(message_id.clone(), cx))));
        }
        if facts.local {
            let (percent, muted) = self.tiny_state.gain(user_id);
            let slide_id = user_id.to_owned();
            let bounds = self.tiny_state.slider.clone();
            let track = div()
                .id("tiny-volume")
                .relative()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.tiny_state.sliding = Some(slide_id.clone());
                        this.tiny_slide(f32::from(event.position.x), cx);
                    }),
                )
                .child(slider(f32::from(percent) / f32::from(protocol::GAIN_MAX), SLIDER, Some(0.5)))
                .child(canvas(move |at, _, _| bounds.set(Some(at)), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
            let mute_id = user_id.to_owned();
            items = items
                .child(menu_separator())
                .child(menu_label("For you"))
                .child(
                    div()
                        .h(px(32.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(glyph(if muted || percent == 0 { "icons/volume-x.svg" } else { "icons/volume-2.svg" }, 16., theme::text_soft()))
                        .child(track)
                        .child(div().flex_1().flex().justify_end().type_small().text_color(theme::text_soft()).child(format!("{percent} %"))),
                )
                .child(self.plain(menu_item_check("tiny-person-mute-local", muted, "Mute for me", None), cx).on_click(cx.listener(move |this, _, _, cx| {
                    let (percent, muted) = this.tiny_state.gain(&mute_id);
                    this.tiny_set_gain(&mute_id, percent, !muted, true, cx);
                })));
        }
        if facts.moderates() {
            items = items.child(menu_separator()).child(menu_label("Moderation"));
            if let Some(on) = facts.server_mute {
                let id = user_id.to_owned();
                items = items.child(self.plain(menu_item_check("tiny-person-server-mute", on, "Server mute", None), cx).on_click(cx.listener(move |this, _, _, cx| this.tiny_moderate(id.clone(), Some(!on), None, cx))));
            }
            if let Some(on) = facts.server_deafen {
                let id = user_id.to_owned();
                items = items.child(self.plain(menu_item_check("tiny-person-server-deafen", on, "Server deafen", None), cx).on_click(cx.listener(move |this, _, _, cx| this.tiny_moderate(id.clone(), None, Some(!on), cx))));
            }
            if !facts.move_to.is_empty() {
                items = items.child(self.more("tiny-person-move", "icons/arrow-right-left.svg", "Move to", Sub::Move, cx));
            }
            if facts.disconnect {
                let id = user_id.to_owned();
                items = items.child(self.plain(menu_item("tiny-person-disconnect", Some("icons/phone-off.svg"), "Disconnect from call", None), cx).on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_close_float(cx);
                    this.tiny_disconnect(id.clone(), cx);
                })));
            }
            if facts.timeout {
                items = items.child(self.more("tiny-person-timeout", "icons/timer.svg", if facts.timed_out { "Timed out" } else { "Timeout" }, Sub::Timeout, cx));
            }
        }
        if facts.manages() {
            items = items.child(menu_separator());
            if !facts.roles.is_empty() || !facts.legacy_roles.is_empty() {
                items = items.child(self.more("tiny-person-roles", "icons/shield.svg", "Roles", Sub::Roles, cx));
            }
            if facts.kick {
                let id = user_id.to_owned();
                items = items.child(self.plain(menu_item_danger("tiny-person-kick", Some("icons/user-x.svg"), "Kick…"), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::Kick(id.clone()), window, cx))));
            }
            if facts.ban {
                let id = user_id.to_owned();
                items = items.child(self.plain(menu_item_danger("tiny-person-ban", Some("icons/ban.svg"), "Ban…"), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::Ban(id.clone()), window, cx))));
            }
        }
        let copy = user_id.to_owned();
        items.child(menu_separator()).child(self.plain(menu_item("tiny-person-copy", Some("icons/copy.svg"), "Copy user ID", None), cx).on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_close_float(cx);
            this.tiny_copy(copy.clone(), "User ID copied", cx);
        })))
    }

    /// The submenu beside the person menu's item.
    pub(super) fn tiny_person_sub(&self, page: &Page, user_id: &str, sub: Sub, cx: &mut Context<Self>) -> Div {
        let facts = model::person_menu(page.community, user_id);
        let mut items = menu().min_w(px(200.));
        match sub {
            Sub::Move => {
                for (ix, (room_id, name)) in facts.move_to.iter().enumerate() {
                    let (user, room) = (user_id.to_owned(), room_id.clone());
                    let count = page.community.rooms.iter().find(|r| r.id == *room_id).map(model::occupancy).unwrap_or_default();
                    items = items.child(
                        menu_item(("tiny-move", ix), Some("icons/volume-2.svg"), name.clone(), None)
                            .child(div().type_small().text_color(theme::text_faint()).child(count))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.tiny_close_float(cx);
                                this.tiny_move(user.clone(), room.clone(), cx);
                            })),
                    );
                }
            }
            Sub::Timeout => {
                for (ix, (words, secs)) in model::TIMEOUTS.iter().enumerate() {
                    let (user, secs) = (user_id.to_owned(), *secs);
                    items = items.child(menu_item(("tiny-timeout", ix), Some(NO_GLYPH), *words, None).on_click(cx.listener(move |this, _, _, cx| {
                        this.tiny_close_float(cx);
                        this.tiny_timeout(user.clone(), Some(secs), cx);
                    })));
                }
                if facts.timed_out {
                    let user = user_id.to_owned();
                    items = items.child(menu_separator()).child(menu_item("tiny-timeout-end", Some("icons/x.svg"), "Remove timeout", None).on_click(cx.listener(move |this, _, _, cx| {
                        this.tiny_close_float(cx);
                        this.tiny_timeout(user.clone(), None, cx);
                    })));
                }
            }
            Sub::Roles => {
                for (ix, role) in facts.roles.iter().enumerate() {
                    let (user, role_id) = (user_id.to_owned(), role.id.clone());
                    let dot = role.color.map(gpui::rgb).unwrap_or(theme::text_faint());
                    items = items.child(
                        menu_item_check(("tiny-role", ix), role.on, role.name.clone(), None)
                            .child(div().size(px(10.)).rounded_full().bg(dot))
                            .on_click(cx.listener(move |this, _, _, cx| this.tiny_toggle_role(user.clone(), role_id.clone(), cx))),
                    );
                }
                for (ix, (role, on)) in facts.legacy_roles.iter().enumerate() {
                    let (user, role, on) = (user_id.to_owned(), *role, *on);
                    items = items.child(menu_item_check(("tiny-legacy-role", ix), on, model::role_word(role), None).on_click(cx.listener(move |this, _, _, cx| {
                        if !on {
                            this.tiny_close_float(cx);
                            this.tiny_legacy_role(user.clone(), role, cx);
                        }
                    })));
                }
            }
        }
        items
    }

    pub(super) fn tiny_channel_menu(&self, page: &Page, room_id: &str, cx: &mut Context<Self>) -> Div {
        let Some(room) = page.community.rooms.iter().find(|room| room.id == room_id) else {
            return menu();
        };
        let may = page.may;
        let mine = page.mine == Some(room_id);
        let mut items = menu().w(px(220.)).child(menu_label(room.name.clone()));
        if mine {
            items = items.child(self.plain(menu_item("tiny-channel-leave", Some("icons/phone-off.svg"), "Leave call", None), cx).on_click(cx.listener(|this, _, _, cx| {
                this.tiny_close_float(cx);
                this.tiny_leave(cx);
            })));
        } else if page.community.allowed("community.calls.join") {
            let id = room.id.clone();
            items = items.child(self.plain(menu_item("tiny-channel-join", Some("icons/volume-2.svg"), "Join", None), cx).on_click(cx.listener(move |this, _, _, cx| {
                this.tiny_close_float(cx);
                this.tiny_join(id.clone(), cx);
            })));
        }
        let modern = page.community.has(protocol::TINY_FEATURE_CATEGORIES);
        if may.edit_room {
            let id = room.id.clone();
            items = if modern {
                items.child(self.plain(menu_item("tiny-channel-edit", Some("icons/pencil.svg"), "Edit channel", None), cx).on_click(cx.listener(move |this, _, window, cx| {
                    this.tiny_open_settings(super::settings::Page::Channels, Some(id.clone()), window, cx);
                })))
            } else {
                items.child(self.plain(menu_item("tiny-channel-rename", Some("icons/pencil.svg"), "Rename channel…", None), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::RenameRoom(id.clone()), window, cx))))
            };
        }
        let link = format!("{}/rooms/{}", page.community.origin.trim_end_matches('/'), room.id);
        items = items.child(self.plain(menu_item("tiny-channel-copy", Some("icons/link.svg"), "Copy channel link", None), cx).on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_close_float(cx);
            this.tiny_copy(link.clone(), "Channel link copied", cx);
        })));
        if may.delete_room {
            let id = room.id.clone();
            items = items.child(menu_separator()).child(self.plain(menu_item_danger("tiny-channel-delete", Some("icons/trash-2.svg"), "Delete channel…"), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::DeleteRoom(id.clone()), window, cx))));
        }
        items
    }

    pub(super) fn tiny_category_menu(&self, page: &Page, key: &str, cx: &mut Context<Self>) -> Div {
        let may = page.may;
        let category = page.community.categories.iter().find(|category| category.id == key);
        let title: SharedString = category.map(|category| category.name.clone()).unwrap_or_else(|| "Voice channels".to_owned()).into();
        let folded = self.tiny_state.folded.contains(key);
        let fold = key.to_owned();
        let mut items = menu().w(px(220.)).child(menu_label(title)).child(
            self.plain(menu_item("tiny-category-fold", Some(if folded { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }), if folded { "Expand" } else { "Collapse" }, None), cx).on_click(cx.listener(move |this, _, _, cx| {
                this.tiny_close_float(cx);
                this.tiny_fold(fold.clone(), cx);
            })),
        );
        if may.make_room {
            let id = category.map(|category| category.id.clone());
            items = items.child(self.plain(menu_item("tiny-category-channel", Some("icons/plus.svg"), "Create channel", None), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::NewRoom(id.clone()), window, cx))));
        }
        if let (Some(category), true) = (category, may.categories) {
            items = items.child(self.plain(menu_item("tiny-category-edit", Some("icons/pencil.svg"), "Edit categories", None), cx).on_click(cx.listener(|this, _, window, cx| this.tiny_open_settings(super::settings::Page::Channels, None, window, cx))));
            let id = category.id.clone();
            items = items.child(menu_separator()).child(self.plain(menu_item_danger("tiny-category-delete", Some("icons/trash-2.svg"), "Delete category…"), cx).on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::DeleteCategory(id.clone()), window, cx))));
        }
        items
    }

    /// The volume slider follows the pointer while its button is down.
    pub(super) fn tiny_slide(&mut self, x: f32, cx: &mut Context<Self>) {
        let (Some(user_id), Some(bounds)) = (self.tiny_state.sliding.clone(), self.tiny_state.slider.get()) else {
            return;
        };
        let at = crate::gui::components::slider_value(x, f32::from(bounds.origin.x), f32::from(bounds.size.width));
        // It snaps to 100 % near the middle, where most people want to come back to.
        let mut percent = (at * f32::from(protocol::GAIN_MAX)).round() as u16;
        if percent.abs_diff(100) <= 4 {
            percent = 100;
        }
        let muted = self.tiny_state.gain(&user_id).1;
        self.tiny_set_gain(&user_id, percent, muted, false, cx);
    }

    /// The pointer let go of the slider: the volume is kept and sent.
    pub(super) fn tiny_slide_end(&mut self, cx: &mut Context<Self>) {
        if let Some(user_id) = self.tiny_state.sliding.take() {
            let (percent, muted) = self.tiny_state.gain(&user_id);
            self.tiny_set_gain(&user_id, percent, muted, true, cx);
        }
    }
}
