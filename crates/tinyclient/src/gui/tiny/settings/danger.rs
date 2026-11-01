//! Danger zone, for owners: hand the server to someone else (make them an owner, then, if
//! chosen, stop being one), and revoke every open invite. Each asks for the server's name to be
//! typed before it goes.

use super::kit::{self, card, check_box};
use super::{page_head, section};
use crate::gui::components::{button, menu, menu_item_check, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::{Modal, Page as Facts};
use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, Context, Div, Window};
use protocol::{Request, TinyCommunity, TinyMember};

/// Who ownership goes to, and whether this account stops being an owner after.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub heir: Option<String>,
    pub picking: bool,
    pub step_down: bool,
}

/// Who ownership can go to: members who are not owners already, by name.
pub(in crate::gui) fn heirs(community: &TinyCommunity) -> Vec<&TinyMember> {
    let mut members: Vec<&TinyMember> = community.members.iter().filter(|member| member.user_id != community.me && !model::is_owner(member)).collect();
    members.sort_by_key(|member| model::name_of(member).to_lowercase());
    members
}

/// The requests a transfer sends, in order: the heir made an owner first, so the server always
/// has one.
pub(in crate::gui) fn transfer(account: &protocol::AccountKey, me: &str, heir: &str, step_down: bool) -> Vec<Request> {
    let mut requests = vec![Request::TinyMemberOwner { account: account.clone(), user_id: heir.to_owned(), owner: true }];
    if step_down {
        requests.push(Request::TinyMemberOwner { account: account.clone(), user_id: me.to_owned(), owner: false });
    }
    requests
}

/// Whether what was typed is the server's name, as the confirmation asks.
pub(in crate::gui) fn confirmed(typed: &str, name: &str) -> bool {
    !name.trim().is_empty() && typed.trim() == name.trim()
}

impl Shell {
    fn tiny_danger_draft(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Draft)) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            change(&mut overlay.danger);
        }
        cx.notify();
    }

    pub(in crate::gui) fn tiny_transfer(&mut self, cx: &mut Context<Self>) {
        let (Some(account), Some(overlay)) = (self.tiny_state.account.clone(), self.tiny_state.settings.as_ref()) else {
            return;
        };
        let Some(heir) = overlay.danger.heir.clone() else {
            return;
        };
        let me = self.tiny_state.community.as_ref().map(|community| community.me.clone()).unwrap_or_default();
        let requests = transfer(&account, &me, &heir, overlay.danger.step_down);
        self.act_all(requests, cx);
    }

    /// A fresh read: an heir who left, or became an owner, is no longer picked.
    pub(in crate::gui) fn tiny_danger_refreshed(&mut self) {
        let (Some(community), Some(overlay)) = (self.tiny_state.community.as_ref(), self.tiny_state.settings.as_mut()) else {
            return;
        };
        if overlay.danger.heir.as_ref().is_some_and(|id| heirs(community).iter().all(|member| member.user_id != *id)) {
            overlay.danger.heir = None;
        }
    }
}

fn zone(title: &'static str, words: &'static str, action: impl IntoElement) -> Div {
    div()
        .px(px(16.))
        .py(px(14.))
        .flex()
        .items_center()
        .gap(px(16.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(title))
                .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(words)),
        )
        .child(action)
}

pub(in crate::gui::tiny) fn danger(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = window;
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.danger;
    let mut page = div().flex().flex_col().gap(px(28.)).child(page_head("Danger zone", "Changes that are hard to take back. Each asks you to type the server's name first."));

    // Transfer ownership.
    let heir_name = draft.heir.as_ref().map(|id| model::name_by_id(community, id));
    let mut picker = div().flex().flex_col().child(
        kit::picker("tiny-danger-heir", "icons/user.svg", heir_name.clone().unwrap_or_else(|| "Choose a member".to_owned()), draft.picking, 260.)
            .on_click(cx.listener(|this, _, _, cx| this.tiny_danger_draft(cx, |draft| draft.picking = !draft.picking))),
    );
    if draft.picking {
        let mut list = menu().id("tiny-danger-heirs").overflow_y_scroll().max_h(px(320.)).w(px(260.));
        for (ix, member) in heirs(community).into_iter().enumerate() {
            let id = member.user_id.clone();
            list = list.child(menu_item_check(("tiny-danger-heir-item", ix), draft.heir.as_deref() == Some(member.user_id.as_str()), model::name_of(member), None).on_click(cx.listener(
                move |this, _, _, cx| {
                    let id = id.clone();
                    this.tiny_danger_draft(cx, |draft| {
                        draft.heir = Some(id);
                        draft.picking = false;
                    })
                },
            )));
        }
        let list = list.on_mouse_down_out(cx.listener(|this, _, _, cx| this.tiny_danger_draft(cx, |draft| draft.picking = false)));
        picker = picker.child(deferred(anchored().snap_to_window().child(div().mt(px(4.)).child(list))).priority(2));
    }
    let step_down = draft.step_down;
    let transfer_button = match &draft.heir {
        Some(heir) => {
            let heir = heir.clone();
            button("tiny-danger-transfer", ButtonKind::Danger, "Transfer ownership…")
                .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::Transfer(heir.clone()), window, cx)))
                .into_any_element()
        }
        None => crate::gui::components::button_disabled("Transfer ownership…").into_any_element(),
    };
    page = page.child(
        section("Ownership").child(
            card().border_color(theme::danger_soft()).child(
                div()
                    .px(px(16.))
                    .py(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Transfer ownership"))
                    .child(
                        div()
                            .whitespace_normal()
                            .type_small()
                            .text_color(theme::text_soft())
                            .child("Make someone else an owner. Owners can do everything, including making and unmaking other owners. A server always keeps at least one."),
                    )
                    .child(picker)
                    .child(
                        div()
                            .id("tiny-danger-step-down")
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.tiny_danger_draft(cx, |draft| draft.step_down = !step_down)))
                            .child(check_box("tiny-danger-step-down-box", Some(step_down), true))
                            .child(div().type_ui().text_color(theme::text()).child("Then stop being an owner myself")),
                    )
                    .child(div().flex().child(transfer_button)),
            ),
        ),
    );

    // Invites.
    let open = community.invites.len();
    page = page.child(section("Invites").child(card().border_color(theme::danger_soft()).child(zone(
        "Revoke every invite",
        "Every open invite stops working at once, whoever made it. People already on the server stay.",
        if open == 0 {
            crate::gui::components::button_disabled("No invites open").into_any_element()
        } else {
            button("tiny-danger-revoke", ButtonKind::Danger, format!("Revoke all {open}…"))
                .on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::RevokeInvites { typed: true }, window, cx)))
                .into_any_element()
        },
    ))));
    page.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Network};

    #[test]
    fn ownership_goes_to_the_heir_first_and_the_confirmation_is_the_name() {
        let account = AccountKey { network: Network::Tiny, account: "me".into() };
        let sent = transfer(&account, "me", "leila", true);
        assert!(
            matches!(&sent[..], [Request::TinyMemberOwner { user_id: first, owner: true, .. }, Request::TinyMemberOwner { user_id: second, owner: false, .. }] if first == "leila" && second == "me")
        );
        assert_eq!(transfer(&account, "me", "leila", false).len(), 1);
        assert!(confirmed(" Night Shift ", "Night Shift"));
        assert!(!confirmed("night shift", "Night Shift") && !confirmed("", ""));
        let community: TinyCommunity = serde_json::from_value(serde_json::json!({"role":"owner","members":[{"user_id":"me"},{"user_id":"o","owner":true},{"user_id":"b","display_name":"Bea"},{"user_id":"a","display_name":"Ann"}],"invites":[],"audit":[],"me":"me"})).unwrap();
        assert_eq!(heirs(&community).iter().map(|member| member.user_id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }
}
