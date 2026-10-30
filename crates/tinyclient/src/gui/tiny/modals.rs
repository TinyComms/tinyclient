//! The modals: inviting people (and the code that comes of it, shown once), kicking and banning
//! with a reason, deleting a channel or a category, naming a channel or a category, someone's
//! profile, and signing out of the server; and Server Settings' confirmations: deleting a role,
//! kicking several people, unbanning, revoking invites, handing over the server and signing out
//! everywhere. The most dangerous ask for the server's name to be typed. Each is the system's modal (`components::modal`):
//! a title, what it does in words, Cancel and the action last.

use super::{model, Modal, Page};
use crate::gui::components::{button, button_disabled, field, glyph, modal, segment, segmented, ButtonKind};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Entity, Focusable, SharedString, Window};

fn words(text: impl Into<SharedString>) -> Div {
    div().mt(px(8.)).whitespace_normal().type_ui().text_color(theme::text_soft()).child(text.into())
}

fn label(text: &'static str) -> Div {
    div().mt(px(16.)).mb(px(6.)).type_micro().text_color(theme::text_faint()).child(text.to_uppercase())
}

fn step(number: usize, text: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex_none()
                .size(px(20.))
                .rounded_full()
                .bg(theme::accent_soft())
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::accent_text())
                .child(number.to_string()),
        )
        .child(div().type_ui().text_color(theme::text()).child(text))
}

impl Shell {
    fn input_box(&self, input: &Entity<TextInput>, window: &Window, cx: &mut Context<Self>) -> Div {
        let focused = input.focus_handle(cx).is_focused(window);
        field(focused).w_full().child(div().flex_1().min_w_0().child(input.clone()))
    }

    /// Cancel, then the action; the action is quiet while the request is out.
    fn buttons(&self, verb: &'static str, kind: ButtonKind, cx: &mut Context<Self>, act: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> Div {
        let action = if self.tiny_state.busy {
            button_disabled(verb).h(px(32.)).into_any_element()
        } else {
            button("tiny-modal-do", kind, verb).h(px(32.)).on_click(cx.listener(move |this, _, _, cx| act(this, cx))).into_any_element()
        };
        div()
            .mt(px(20.))
            .flex()
            .justify_end()
            .gap(px(8.))
            .child(button("tiny-modal-cancel", ButtonKind::Secondary, "Cancel").h(px(32.)).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
            .child(action)
    }

    fn modal_error(&self) -> Option<Div> {
        self.tiny_state.modal_error.clone().map(|detail| div().mt(px(12.)).whitespace_normal().type_small().text_color(theme::danger()).child(detail))
    }

    pub(super) fn tiny_modal(&self, page: &Page, modal_on_show: &Modal, window: &Window, cx: &mut Context<Self>) -> Div {
        match modal_on_show {
            Modal::Invite => self.invite_modal(page, window, cx),
            Modal::Code { paste, origin, terms, role } => self.code_modal(page, paste, origin, terms, role, cx),
            Modal::Kick(user_id) => {
                let name = model::name_by_id(page.community, user_id);
                let modern = page.community.access.is_some();
                let id = user_id.clone();
                modal(format!("Kick {name}?"))
                    .child(words(if modern {
                        format!("{name} is signed out of {} and leaves its calls. They can come back with a new invite.", page.name)
                    } else {
                        format!("Remove {name} from this server? They are signed out at once.")
                    }))
                    .when(modern, |panel| panel.child(label("Reason")).child(self.input_box(&self.tiny_state.reason_input, window, cx)))
                    .children(self.modal_error())
                    .child(self.buttons("Kick", ButtonKind::Danger, cx, move |this, cx| this.tiny_send_kick(id.clone(), cx)))
            }
            Modal::Ban(user_id) => {
                let name = model::name_by_id(page.community, user_id);
                let id = user_id.clone();
                let mut lengths = segmented();
                for (ix, (words, days)) in model::BANS.iter().enumerate() {
                    let days = *days;
                    lengths = lengths.child(segment(("tiny-ban-days", ix), *words, self.tiny_state.ban_days == days).on_click(cx.listener(move |this, _, _, cx| {
                        this.tiny_state.ban_days = days;
                        cx.notify();
                    })));
                }
                modal(format!("Ban {name}?"))
                    .child(words(format!("{name} is removed and can't come back: not with an invite, not by signing in again, and not through sign-in groups.")))
                    .child(label("Reason"))
                    .child(self.input_box(&self.tiny_state.reason_input, window, cx))
                    .child(label("For"))
                    .child(lengths)
                    .children(self.modal_error())
                    .child(self.buttons("Ban", ButtonKind::Danger, cx, move |this, cx| this.tiny_send_ban(id.clone(), cx)))
            }
            Modal::DeleteRoom(room_id) => {
                let name = page.community.rooms.iter().find(|room| room.id == *room_id).map(|room| room.name.clone()).unwrap_or_default();
                let id = room_id.clone();
                modal(format!("Delete {name}?"))
                    .child(words("Anyone in its call leaves it. This can't be undone."))
                    .children(self.modal_error())
                    .child(self.buttons("Delete channel", ButtonKind::Danger, cx, move |this, cx| this.tiny_delete_room(id.clone(), cx)))
            }
            Modal::DeleteCategory(category_id) => {
                let name = page.community.categories.iter().find(|category| category.id == *category_id).map(|category| category.name.clone()).unwrap_or_default();
                let id = category_id.clone();
                modal(format!("Delete {name}?"))
                    .child(words("Its channels stay, at the top of the list with no category."))
                    .children(self.modal_error())
                    .child(self.buttons("Delete category", ButtonKind::Danger, cx, move |this, cx| this.tiny_delete_category(id.clone(), cx)))
            }
            Modal::NewRoom(category) => {
                let place = category.as_ref().and_then(|id| page.community.categories.iter().find(|category| category.id == *id)).map(|category| category.name.clone());
                let category = category.clone();
                modal("Create voice channel")
                    .child(words(match place {
                        Some(place) => format!("In {place}. People join and leave it as they like."),
                        None => "People join and leave it as they like.".to_owned(),
                    }))
                    .child(label("Channel name"))
                    .child(self.input_box(&self.tiny_state.name_input, window, cx))
                    .children(self.modal_error())
                    .child(self.buttons("Create channel", ButtonKind::Primary, cx, move |this, cx| this.tiny_create_room(category.clone(), cx)))
            }
            Modal::RenameRoom(room_id) => {
                let id = room_id.clone();
                modal("Rename channel")
                    .child(label("Channel name"))
                    .child(self.input_box(&self.tiny_state.name_input, window, cx))
                    .children(self.modal_error())
                    .child(self.buttons("Save", ButtonKind::Primary, cx, move |this, cx| this.tiny_rename_room(id.clone(), cx)))
            }
            Modal::NewCategory => modal("Create category")
                .child(words("A category holds channels and folds away in the list."))
                .child(label("Category name"))
                .child(self.input_box(&self.tiny_state.name_input, window, cx))
                .children(self.modal_error())
                .child(self.buttons("Create category", ButtonKind::Primary, cx, |this, cx| this.tiny_create_category(cx))),
            Modal::Profile(user_id) => match page.community.member(user_id) {
                Some(member) => modal("Profile")
                    .w(px(400.))
                    .child(div().mt(px(16.)).child(self.tiny_person_card(page, member, true)))
                    .child(div().mt(px(20.)).flex().justify_end().child(button("tiny-profile-close", ButtonKind::Secondary, "Close").h(px(32.)).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))),
                None => modal("Profile").child(words("They are no longer on this server.")),
            },
            Modal::SignOut => modal(format!("Sign out of {}?", page.name))
                .child(words("Your sign-in here is forgotten on this computer, with everything tinyclient saved for it. You can join again with an invite or by signing in."))
                .child(self.buttons("Sign out", ButtonKind::Danger, cx, |this, cx| this.tiny_sign_out(cx))),
            Modal::DeleteRole(role_id) => {
                let role = page.community.roles.iter().find(|role| role.id == *role_id);
                let name = role.map(|role| role.name.clone()).unwrap_or_default();
                let count = role.map_or(0, |role| role.member_count);
                let id = role_id.clone();
                modal(format!("Delete {name}?"))
                    .child(words(match count {
                        0 => "Nobody has this role. This can't be undone.".to_owned(),
                        1 => "1 person has this role and loses it, with what it allowed them. This can't be undone.".to_owned(),
                        n => format!("{n} people have this role and lose it, with what it allowed them. This can't be undone."),
                    }))
                    .children(self.modal_error())
                    .child(self.buttons("Delete role", ButtonKind::Danger, cx, move |this, cx| this.tiny_delete_role(id.clone(), cx)))
            }
            Modal::KickMany(user_ids) => {
                let ids = user_ids.clone();
                let names: Vec<String> = user_ids.iter().map(|id| model::name_by_id(page.community, id)).collect();
                modal(format!("Kick {} people?", user_ids.len()))
                    .child(words(format!("{} are signed out of {} and leave its calls. They can come back with a new invite.", list_names(&names), page.name)))
                    .child(label("Reason"))
                    .child(self.input_box(&self.tiny_state.reason_input, window, cx))
                    .children(self.modal_error())
                    .child(self.buttons("Kick them", ButtonKind::Danger, cx, move |this, cx| this.tiny_send_kicks(ids.clone(), cx)))
            }
            Modal::Unban(user_id) => {
                let name = page.community.bans.iter().find(|ban| ban.user_id == *user_id).map(super::settings::banned_name).unwrap_or_else(|| "them".to_owned());
                let id = user_id.clone();
                modal(format!("Unban {name}?"))
                    .child(words(format!("{name} can join again with an invite, or through sign-in groups. They don't come back on their own.")))
                    .children(self.modal_error())
                    .child(self.buttons("Unban", ButtonKind::Primary, cx, move |this, cx| this.tiny_unban(id.clone(), cx)))
            }
            Modal::RevokeInvites { typed } => {
                let count = page.community.invites.iter().filter(|invite| super::settings::revocable(page.community, invite)).count();
                let panel = modal(format!("Revoke {count} invites?")).child(words("Every code still out there stops working at once. People who already joined stay."));
                if *typed {
                    self.tiny_typed_confirm(panel, page, "Revoke all", window, cx, |this, cx| this.tiny_revoke_all(cx))
                } else {
                    panel.children(self.modal_error()).child(self.buttons("Revoke all", ButtonKind::Danger, cx, |this, cx| this.tiny_revoke_all(cx)))
                }
            }
            Modal::Transfer(user_id) => {
                let name = model::name_by_id(page.community, user_id);
                let step_down = self.tiny_state.settings.as_ref().is_some_and(|overlay| overlay.danger.step_down);
                let panel = modal(format!("Make {name} an owner?")).child(words(if step_down {
                    format!("{name} can do everything on {}, including taking ownership from others. Then you stop being an owner, and only another owner can make you one again.", page.name)
                } else {
                    format!("{name} can do everything on {}, including taking ownership from others. You stay an owner too.", page.name)
                }));
                self.tiny_typed_confirm(panel, page, "Transfer ownership", window, cx, |this, cx| this.tiny_transfer(cx))
            }
            Modal::SignOutEverywhere => modal("Sign out everywhere?")
                .child(words(format!("Every session of yours on {} ends, on every computer, this one too. Sign in again with your provider when you want back in.", page.name)))
                .children(self.modal_error())
                .child(self.buttons("Sign out everywhere", ButtonKind::Danger, cx, |this, cx| this.tiny_sign_out_everywhere(cx))),
        }
    }

    /// A dangerous modal's end: the server's name to type, and the action quiet until it is.
    fn tiny_typed_confirm(&self, panel: Div, page: &Page, verb: &'static str, window: &Window, cx: &mut Context<Self>, act: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> Div {
        let confirm = &self.tiny_state.settings_fields.confirm;
        let ready = super::settings::confirmed(confirm.read(cx).text(), &page.name);
        let action = if self.tiny_state.busy || !ready {
            button_disabled(verb).h(px(32.)).into_any_element()
        } else {
            button("tiny-modal-do", ButtonKind::Danger, verb).h(px(32.)).on_click(cx.listener(move |this, _, _, cx| act(this, cx))).into_any_element()
        };
        panel
            .child(div().mt(px(16.)).mb(px(6.)).type_small().text_color(theme::text_soft()).child(format!("Type “{}” to go on.", page.name)))
            .child(self.input_box(confirm, window, cx))
            .children(self.modal_error())
            .child(
                div()
                    .mt(px(20.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("tiny-modal-cancel", ButtonKind::Secondary, "Cancel").h(px(32.)).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
                    .child(action),
            )
    }

    fn invite_modal(&self, page: &Page, window: &Window, cx: &mut Context<Self>) -> Div {
        let draft = &self.tiny_state.invite;
        let host = protocol::join_host(&page.community.origin);
        let mut panel = modal(format!("Invite people to {}", page.name))
            .w(px(480.))
            .child(words("They paste one code into their own tinyclient and sign in through their browser. The code holds the server, so nobody types an address."));
        if page.may.invite_options {
            if page.may.invite_roles {
                let assignable: Vec<_> = {
                    let mut roles: Vec<_> = page.community.roles.iter().filter(|role| !role.default && page.community.access.as_ref().is_some_and(|access| access.may_assign(role))).collect();
                    roles.sort_by_key(|role| std::cmp::Reverse(role.position));
                    roles
                };
                if !assignable.is_empty() {
                    let mut chips = div().flex().flex_wrap().gap(px(6.));
                    for (ix, role) in assignable.into_iter().enumerate() {
                        let on = draft.roles.contains(&role.id);
                        let id = role.id.clone();
                        let dot = role.color.as_deref().and_then(model::hex).map(gpui::rgb).unwrap_or(theme::text_faint());
                        chips = chips.child(
                            div()
                                .id(("tiny-invite-role", ix))
                                .h(px(28.))
                                .px(px(10.))
                                .rounded(px(14.))
                                .border_1()
                                .border_color(if on { theme::accent() } else { theme::border() })
                                .bg(if on { theme::accent_soft() } else { theme::hover() })
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .cursor_pointer()
                                .hover(|style| style.border_color(theme::border_strong()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let roles = &mut this.tiny_state.invite.roles;
                                    if let Some(at) = roles.iter().position(|role| *role == id) {
                                        roles.remove(at);
                                    } else {
                                        roles.push(id.clone());
                                    }
                                    cx.notify();
                                }))
                                .when(on, |chip| chip.child(glyph("icons/check.svg", 12., theme::accent_text())))
                                .when(!on, |chip| chip.child(div().size(px(8.)).rounded_full().bg(dot)))
                                .child(div().type_ui().text_color(theme::text()).child(role.name.clone())),
                        );
                    }
                    panel = panel.child(label("Roles they join with")).child(chips);
                }
            }
            let mut uses = segmented();
            for (ix, (words, value)) in model::USES.iter().enumerate() {
                let value = *value;
                uses = uses.child(segment(("tiny-invite-uses", ix), *words, draft.uses == value).on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_state.invite.uses = value;
                    cx.notify();
                })));
            }
            let mut expiry = segmented();
            for (ix, (words, value)) in model::EXPIRIES.iter().enumerate() {
                if value.is_none() && !page.may.invite_forever {
                    continue;
                }
                let value = *value;
                expiry = expiry.child(segment(("tiny-invite-expiry", ix), *words, draft.expiry == value).on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_state.invite.expiry = value;
                    cx.notify();
                })));
            }
            panel = panel
                .child(label("Label"))
                .child(self.input_box(&self.tiny_state.label_input, window, cx))
                .child(label("Max uses"))
                .child(uses)
                .child(label("Expires after"))
                .child(expiry);
        } else {
            let role = draft.legacy_role;
            panel = panel.child(label("They join as")).child(
                segmented()
                    .child(segment("tiny-invite-member", "Member", role != "admin").on_click(cx.listener(|this, _, _, cx| {
                        this.tiny_state.invite.legacy_role = "member";
                        cx.notify();
                    })))
                    .child(segment("tiny-invite-admin", "Admin", role == "admin").on_click(cx.listener(|this, _, _, cx| {
                        this.tiny_state.invite.legacy_role = "admin";
                        cx.notify();
                    }))),
            );
        }
        if !host.is_empty() {
            panel = panel.child(div().mt(px(16.)).flex().items_center().gap(px(6.)).child(glyph("icons/link.svg", 14., theme::text_faint())).child(div().type_small().text_color(theme::text_faint()).child(format!("The code opens {host}."))));
        }
        panel.children(self.modal_error()).child(self.buttons("Create invite", ButtonKind::Primary, cx, |this, cx| this.tiny_send_invite(cx)))
    }

    fn code_modal(&self, page: &Page, paste: &str, origin: &str, terms: &str, role: &str, cx: &mut Context<Self>) -> Div {
        let (opens, makes) = handoff_facts(if origin.is_empty() { &page.community.origin } else { origin }, role, !page.community.has(protocol::TINY_FEATURE_INVITE_OPTIONS));
        let copy = paste.to_owned();
        let copied = self.tiny_state.copied;
        modal("Give them this code")
            .w(px(480.))
            .child(words("Copy it and send it to them. They do the rest in their own tinyclient."))
            .child(
                div()
                    .mt(px(16.))
                    .p(px(16.))
                    .rounded(px(theme::R_CARD))
                    .bg(theme::raised())
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(step(1, "Open tinyclient"))
                    .child(step(2, "Choose Tiny server, or Join a server"))
                    .child(step(3, "Paste this code"))
                    .child(step(4, "Continue in the browser that opens")),
            )
            .child(label("Code"))
            .child(
                div()
                    .w_full()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(theme::R_CARD))
                    .bg(theme::field())
                    .border_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(div().flex_1().min_w_0().whitespace_normal().type_code().text_color(theme::text()).child(paste.to_owned()))
                    .child(
                        button("tiny-code-copy", if copied { ButtonKind::Secondary } else { ButtonKind::Primary }, if copied { "Copied" } else { "Copy" }).on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                            this.tiny_state.copied = true;
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .mt(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().whitespace_normal().type_small().text_color(theme::text()).child(opens))
                    .children(makes.map(|makes| div().type_small().text_color(theme::text_soft()).child(makes)))
                    .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(terms.to_owned())),
            )
            .child(div().mt(px(20.)).flex().justify_end().child(button("tiny-code-done", ButtonKind::Secondary, "Done").h(px(32.)).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)))))
    }
}

/// Names as a sentence lists them: `Ann`, `Ann and Bea`, `Ann, Bea and 3 others`.
fn list_names(names: &[String]) -> String {
    match names {
        [] => "They".to_owned(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} others", rest.len()),
    }
}

/// Where a code opens, and who it makes. The host is plain: they do not type it. A newer
/// server's invite carries roles instead of a word, so only an admin invite is said.
fn handoff_facts(origin: &str, role: &str, legacy: bool) -> (String, Option<&'static str>) {
    let host = protocol::join_host(origin);
    let opens = if host.is_empty() { "It opens this server. They don't type that.".to_owned() } else { format!("It opens {host}. They don't type that.") };
    let makes = match role.to_ascii_lowercase().as_str() {
        "admin" => Some("This code makes them an admin."),
        _ if legacy => Some("This code makes them a member."),
        _ => None,
    };
    (opens, makes)
}

/// A modal panel, centered over a scrim that closes it.
pub(super) fn layer(panel: Div, cx: &mut Context<Shell>) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(
            div()
                .id("tiny-modal-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(theme::scrim())
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .pb(px(60.))
                .child(div().id("tiny-modal").occlude().on_click(|_, _, cx| cx.stop_propagation()).child(crate::gui::components::fade_in("tiny-modal-fade", 160, panel))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{handoff_facts, list_names};

    #[test]
    fn several_names_read_as_a_sentence() {
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        assert_eq!(list_names(&names(&["Ann"])), "Ann");
        assert_eq!(list_names(&names(&["Ann", "Bea"])), "Ann and Bea");
        assert_eq!(list_names(&names(&["Ann", "Bea", "Cy", "Di", "Ed"])), "Ann, Bea and 3 others");
    }

    #[test]
    fn a_code_says_where_it_opens_and_who_it_makes() {
        let (opens, makes) = handoff_facts("https://private.tinyclient.io", "member", true);
        assert_eq!(opens, "It opens private.tinyclient.io. They don't type that.");
        assert_eq!(makes, Some("This code makes them a member."));
        let (opens, makes) = handoff_facts("http://127.0.0.1:8080", "admin", true);
        assert_eq!(opens, "It opens 127.0.0.1:8080. They don't type that.");
        assert_eq!(makes, Some("This code makes them an admin."));
        let (opens, makes) = handoff_facts("", "member", false);
        assert_eq!(opens, "It opens this server. They don't type that.");
        assert_eq!(makes, None, "a newer server's invite says its roles, not a word");
    }
}
