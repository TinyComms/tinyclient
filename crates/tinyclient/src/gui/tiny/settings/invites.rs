//! Invites: the invites still open, with who they are for, who made them ("Server setup" for the
//! owner invite the server printed when it was set up), the roles they give, how often they have
//! been used and when they run out. Create invite opens the same modal as the header menu.
//! A code is shown once, when it is made: the server keeps no way to show it again, and the page
//! says so. Revoke one, or all of them (asked first).

use super::kit::{self, cell, table_head, table_row};
use super::page_head;
use crate::gui::components::{button, button_with_icon, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::{Modal, Page as Facts};
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString, Window};
use protocol::{Request, TinyCommunity, TinyInvite, TINY_MANAGE_INVITES};

/// Who made an invite, in words: the server itself for the owner invite printed at setup.
pub(in crate::gui) fn creator(community: &TinyCommunity, invite: &TinyInvite) -> String {
    if invite.created_by.is_empty() {
        return "Server setup".to_owned();
    }
    if !invite.created_by_name.trim().is_empty() {
        return invite.created_by_name.clone();
    }
    model::name_by_id(community, &invite.created_by)
}

/// Whether this account may revoke `invite`: anyone's with manage_invites, its own with
/// create_invites. On an older server, as its role allows.
pub(in crate::gui) fn revocable(community: &TinyCommunity, invite: &TinyInvite) -> bool {
    if !community.allowed("community.invites.revoke") {
        return false;
    }
    match &community.access {
        Some(access) => access.owner || access.has(TINY_MANAGE_INVITES) || invite.created_by == community.me,
        None => true,
    }
}

/// `3 / 10`, `0 / 1`, or `3` with no limit.
pub(in crate::gui) fn uses(invite: &TinyInvite) -> String {
    match invite.max_uses {
        Some(max) => format!("{} / {max}", invite.uses),
        None => format!("{} / ∞", invite.uses),
    }
}

impl Shell {
    pub(in crate::gui) fn tiny_revoke(&mut self, invite_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.act(Request::TinyRevoke { account, invite_id }, cx);
    }

    /// Every invite this account may revoke, one request each.
    pub(in crate::gui) fn tiny_revoke_all(&mut self, cx: &mut Context<Self>) {
        let (Some(account), Some(community)) = (self.tiny_state.account.clone(), self.tiny_state.community.as_ref()) else {
            return;
        };
        let requests = community.invites.iter().filter(|invite| revocable(community, invite)).map(|invite| Request::TinyRevoke { account: account.clone(), invite_id: invite.id.clone() }).collect();
        self.act_all(requests, cx);
    }
}

pub(in crate::gui::tiny) fn invites(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = window;
    let community = facts.community;
    let mut page = div().flex().flex_col().gap(px(20.)).child(page_head("Invites", "The invites still open. Anyone with a code can join until it runs out or is revoked."));
    page = page.child(kit::note(
        "icons/info.svg",
        theme::text_faint(),
        "A code is shown once, when it is made, and can't be shown again. If someone lost theirs, make a new invite and revoke the old one.",
    ));
    let any = community.invites.iter().any(|invite| revocable(community, invite));
    let mut tools = div().flex().items_center().gap(px(8.));
    if facts.may.invite {
        tools = tools.child(
            button_with_icon("tiny-invites-create", ButtonKind::Primary, "icons/user-plus.svg", "Create invite")
                .on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::Invite, window, cx))),
        );
    }
    tools = tools.child(div().flex_1()).child(div().type_small().text_color(theme::text_faint()).child(match community.invites.len() {
        1 => "1 invite open".to_owned(),
        n => format!("{n} invites open"),
    }));
    if any && !shell.tiny_state.busy {
        tools = tools.child(
            button("tiny-invites-revoke-all", ButtonKind::Danger, "Revoke all").on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::RevokeInvites { typed: false }, window, cx))),
        );
    }
    page = page.child(tools);
    if community.invites.is_empty() {
        return page.child(kit::empty("icons/user-plus.svg", "No invites are open.")).into_any_element();
    }
    let mut table = kit::card().child(table_head(&[("Invite", None), ("Made by", Some(120.)), ("Roles", Some(110.)), ("Uses", Some(52.)), ("Expires", Some(86.)), ("", Some(64.))]));
    let now = kit::now();
    for (ix, invite) in community.invites.iter().enumerate() {
        let label = if invite.label.trim().is_empty() { "No label".to_owned() } else { invite.label.clone() };
        let mut roles = div().flex_none().w(px(110.)).flex().flex_wrap().gap(px(4.));
        let named: Vec<_> = invite.role_ids.iter().filter_map(|id| community.roles.iter().find(|role| role.id == *id)).collect();
        if named.is_empty() {
            roles = roles.child(div().type_small().text_color(theme::text_soft()).child(model::role_word(&invite.role)));
        }
        for role in named {
            roles = roles.child(kit::role_chip(role.name.clone(), role.color.as_deref().and_then(model::hex)));
        }
        let expires =
            protocol::tiny_unix(&invite.expires_at).map(|at| model::relative(now, at)).unwrap_or_else(|| if invite.expires_at.is_empty() { "Never".to_owned() } else { invite.expires_at.clone() });
        let id = invite.id.clone();
        let revoke =
            revocable(community, invite).then(|| button(("tiny-invites-revoke", ix), ButtonKind::Ghost, "Revoke").on_click(cx.listener(move |this, _, _, cx| this.tiny_revoke(id.clone(), cx))));
        table = table.child(
            table_row(SharedString::from(format!("tiny-invites-row-{}", invite.id)), false)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(if invite.label.trim().is_empty() { theme::text_soft() } else { theme::text() }).child(label))
                        .when(invite.accepted_at.is_some() && invite.uses > 0, |column| {
                            column.child(
                                div()
                                    .type_small()
                                    .text_color(theme::text_faint())
                                    .child("Last used ".to_owned() + &invite.accepted_at.as_deref().and_then(protocol::tiny_unix).map(|at| model::relative(now, at)).unwrap_or_default()),
                            )
                        }),
                )
                .child(cell(120., creator(community, invite), if invite.created_by.is_empty() { theme::text_faint() } else { theme::text_soft() }))
                .child(roles)
                .child(cell(52., uses(invite), theme::text_soft()))
                .child(cell(86., expires, theme::text_soft()))
                .child(div().flex_none().w(px(64.)).flex().justify_end().children(revoke)),
        );
    }
    page.child(table).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{TinyAccess, TINY_CREATE_INVITES};

    #[test]
    fn an_invite_says_who_made_it_and_who_may_revoke_it() {
        let mut community: TinyCommunity = serde_json::from_value(serde_json::json!({"role":"admin","members":[{"user_id":"m","display_name":"Mara"}],"invites":[],"audit":[]})).unwrap();
        community.me = "me".into();
        let setup = TinyInvite { id: "1".into(), ..TinyInvite::default() };
        let mara = TinyInvite { id: "2".into(), created_by: "m".into(), ..TinyInvite::default() };
        let mine = TinyInvite { id: "3".into(), created_by: "me".into(), created_by_name: "Robin".into(), max_uses: Some(5), uses: 2, ..TinyInvite::default() };
        assert_eq!(creator(&community, &setup), "Server setup");
        assert_eq!(creator(&community, &mara), "Mara");
        assert_eq!(creator(&community, &mine), "Robin");
        assert_eq!(uses(&mine), "2 / 5");
        assert_eq!(uses(&mara), "0 / ∞");
        assert!(revocable(&community, &mara), "an older server's admin revokes any");
        community.capabilities.features = vec!["invite_options".into()];
        community.access = Some(TinyAccess { permissions: TINY_CREATE_INVITES, ..TinyAccess::default() });
        assert!(revocable(&community, &mine) && !revocable(&community, &mara), "only my own without manage_invites");
        community.access = Some(TinyAccess { permissions: TINY_MANAGE_INVITES, ..TinyAccess::default() });
        assert!(revocable(&community, &mara) && revocable(&community, &setup));
    }
}
