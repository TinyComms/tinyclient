//! Members: everyone on the server as a table — avatar and name, their roles as chips, when they
//! joined and were last seen, and for those who manage people the identity they sign in with.
//! A search and a role filter on top. Tick people (only those this account outranks) to add or
//! take a role from all of them at once, or to kick them. A row's "…" (or a right-click) opens
//! the same person menu as anywhere else.

use super::kit::{self, cell, check_box, table_head, table_row};
use super::page_head;
use crate::gui::components::{button, glyph, icon_button, menu, menu_item_check, menu_label, tip, Avatar, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::{Float, Modal, Page as Facts};
use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, Context, Div, MouseButton, MouseDownEvent, SharedString, Window};
use protocol::{Request, TinyCommunity, TinyMember, TINY_FEATURE_ROLES};
use std::collections::BTreeSet;

/// Which of the page's lists is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Menu {
    Filter,
    Add,
    Remove,
}

/// Who is ticked, the role the table is filtered to, and the list open.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub selected: BTreeSet<String>,
    pub role: Option<String>,
    pub open: Option<Menu>,
}

/// The members the table lists for `query` and `role`: owners first, then by rank, then by
/// name. The query matches a name or an email.
pub(in crate::gui) fn listed<'a>(community: &'a TinyCommunity, query: &str, role: Option<&str>) -> Vec<&'a TinyMember> {
    let query = query.trim().to_lowercase();
    let mut members: Vec<&TinyMember> = community
        .members
        .iter()
        .filter(|member| query.is_empty() || model::name_of(member).to_lowercase().contains(&query) || member.email.to_lowercase().contains(&query))
        .filter(|member| role.is_none_or(|role| member.roles.iter().any(|id| id == role)))
        .collect();
    members.sort_by_key(|member| (!model::is_owner(member), std::cmp::Reverse(protocol::tiny_rank(&community.roles, &member.roles, false)), model::name_of(member).to_lowercase()));
    members
}

/// Who may be ticked: people this account outranks, when it may change roles or kick.
pub(in crate::gui) fn selectable(community: &TinyCommunity, member: &TinyMember) -> bool {
    community.outranks(member) && (community.allowed("community.members.role") || community.allowed("community.members.remove"))
}

/// Each ticked person whose roles change when `role_id` is added (or taken away), with all
/// their roles after. Those this account doesn't outrank are left alone.
pub(in crate::gui) fn bulk_roles(community: &TinyCommunity, selected: &BTreeSet<String>, role_id: &str, add: bool) -> Vec<(String, Vec<String>)> {
    selected
        .iter()
        .filter_map(|id| community.member(id))
        .filter(|member| community.outranks(member))
        .filter_map(|member| {
            let has = member.roles.iter().any(|id| id == role_id);
            match (add, has) {
                (true, false) => Some((member.user_id.clone(), member.roles.iter().cloned().chain([role_id.to_owned()]).collect())),
                (false, true) => Some((member.user_id.clone(), member.roles.iter().filter(|id| *id != role_id).cloned().collect())),
                _ => None,
            }
        })
        .collect()
}

impl Shell {
    fn tiny_members_draft(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Draft)) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            change(&mut overlay.members);
        }
        cx.notify();
    }

    fn tiny_members_bulk(&mut self, role_id: String, add: bool, cx: &mut Context<Self>) {
        let (Some(account), Some(community), Some(overlay)) = (self.tiny_state.account.clone(), self.tiny_state.community.as_ref(), self.tiny_state.settings.as_ref()) else {
            return;
        };
        let requests: Vec<Request> =
            bulk_roles(community, &overlay.members.selected, &role_id, add).into_iter().map(|(user_id, role_ids)| Request::TinyMemberRoles { account: account.clone(), user_id, role_ids }).collect();
        self.tiny_members_draft(cx, |draft| draft.open = None);
        self.act_all(requests, cx);
    }

    /// A fresh read: people who left are no longer ticked.
    pub(in crate::gui) fn tiny_members_refreshed(&mut self) {
        let (Some(community), Some(overlay)) = (self.tiny_state.community.as_ref(), self.tiny_state.settings.as_mut()) else {
            return;
        };
        overlay.members.selected.retain(|id| community.member(id).is_some_and(|member| community.outranks(member)));
        if overlay.members.role.as_ref().is_some_and(|role| community.roles.iter().all(|known| known.id != *role)) {
            overlay.members.role = None;
        }
    }
}

/// A list under a trigger, as the page's pickers drop one.
fn dropdown(list: Div, cx: &mut Context<Shell>) -> AnyElement {
    let list = list.on_mouse_down_out(cx.listener(|this, _, _, cx| this.tiny_members_draft(cx, |draft| draft.open = None)));
    deferred(anchored().snap_to_window().child(div().mt(px(4.)).child(list))).with_priority(2).into_any_element()
}

pub(in crate::gui::tiny) fn members(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.members;
    let fields = &shell.tiny_state.settings_fields;
    let query = fields.member_search.read(cx).text().to_owned();
    let roles_known = community.has(TINY_FEATURE_ROLES);
    let rows = listed(community, &query, draft.role.as_deref());
    let mut page = div().flex().flex_col().gap(px(20.)).child(page_head("Members", "Everyone on this server. Tick people to change their roles or remove them together."));

    // Search, filter, and how many.
    let filter_name = draft.role.as_ref().and_then(|id| community.roles.iter().find(|role| role.id == *id)).map_or("All roles".to_owned(), |role| role.name.clone());
    let mut tools = div().flex().items_center().gap(px(8.)).child(kit::search(&fields.member_search, 260., window, cx));
    if roles_known {
        let open = draft.open == Some(Menu::Filter);
        let mut filter = div().flex().flex_col().child(
            kit::picker("tiny-members-filter", "icons/shield.svg", filter_name, open, 180.)
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_members_draft(cx, |draft| draft.open = if open { None } else { Some(Menu::Filter) }))),
        );
        if open {
            let mut list = menu().w(px(220.)).child(menu_item_check("tiny-members-filter-all", draft.role.is_none(), "All roles", None).on_click(cx.listener(|this, _, _, cx| {
                this.tiny_members_draft(cx, |draft| {
                    draft.role = None;
                    draft.open = None;
                })
            })));
            for (ix, role) in super::roles::ordered(community).into_iter().filter(|role| !role.default).enumerate() {
                let id = role.id.clone();
                list = list.child(menu_item_check(("tiny-members-filter-role", ix), draft.role.as_deref() == Some(role.id.as_str()), role.name.clone(), None).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let id = id.clone();
                        this.tiny_members_draft(cx, |draft| {
                            draft.role = Some(id);
                            draft.open = None;
                        })
                    },
                )));
            }
            filter = filter.child(dropdown(list, cx));
        }
        tools = tools.child(filter);
    }
    tools = tools.child(div().flex_1()).child(div().type_small().text_color(theme::text_faint()).child(format!("{} of {}", rows.len(), community.members.len())));
    page = page.child(tools);

    // What can be done to everyone ticked.
    if !draft.selected.is_empty() {
        let mut bar = div()
            .h(px(44.))
            .px(px(12.))
            .rounded(px(theme::R_CARD))
            .bg(theme::accent_wash())
            .border_1()
            .border_color(theme::accent_soft())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(format!("{} selected", draft.selected.len())))
            .child(div().flex_1());
        let assignable: Vec<_> = super::roles::ordered(community).into_iter().filter(|role| !role.default && super::roles::editable(community, role)).collect();
        if roles_known && facts.may.manage_roles && !assignable.is_empty() {
            for (menu_kind, words) in [(Menu::Add, "Add role"), (Menu::Remove, "Remove role")] {
                let open = draft.open == Some(menu_kind);
                let mut wrap = div().flex().flex_col().child(
                    button(SharedString::from(format!("tiny-members-{words}")), ButtonKind::Secondary, words)
                        .on_click(cx.listener(move |this, _, _, cx| this.tiny_members_draft(cx, |draft| draft.open = if open { None } else { Some(menu_kind) }))),
                );
                if open {
                    let add = menu_kind == Menu::Add;
                    let mut list = menu().w(px(220.)).child(menu_label(if add { "Give everyone ticked" } else { "Take from everyone ticked" }));
                    for (ix, role) in assignable.iter().enumerate() {
                        let id = role.id.clone();
                        let dot = role.color.as_deref().and_then(model::hex).map(gpui::rgb).unwrap_or(theme::text_faint());
                        let changes = bulk_roles(community, &draft.selected, &role.id, add).len();
                        list = list.child(
                            div()
                                .id(("tiny-members-bulk-role", ix))
                                .h(px(30.))
                                .px(px(8.))
                                .rounded(px(theme::R_CONTROL))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::hover()))
                                .on_click(cx.listener(move |this, _, _, cx| this.tiny_members_bulk(id.clone(), add, cx)))
                                .child(div().size(px(8.)).rounded_full().bg(dot))
                                .child(div().flex_1().type_ui().text_color(theme::text()).child(role.name.clone()))
                                .child(div().type_small().text_color(theme::text_faint()).child(if changes == 0 { "no change".to_owned() } else { format!("{changes} change") })),
                        );
                    }
                    wrap = wrap.child(dropdown(list, cx));
                }
                bar = bar.child(wrap);
            }
        }
        if community.allowed("community.members.remove") {
            let ids: Vec<String> = draft.selected.iter().cloned().collect();
            bar = bar.child(button("tiny-members-kick", ButtonKind::Danger, "Kick").on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::KickMany(ids.clone()), window, cx))));
        }
        bar = bar.child(button("tiny-members-clear", ButtonKind::Ghost, "Clear").on_click(cx.listener(|this, _, _, cx| this.tiny_members_draft(cx, |draft| draft.selected.clear()))));
        page = page.child(bar);
    }

    // The table.
    let ticks: Vec<String> = rows.iter().filter(|member| selectable(community, member)).map(|member| member.user_id.clone()).collect();
    let all = !ticks.is_empty() && ticks.iter().all(|id| draft.selected.contains(id));
    let some = ticks.iter().any(|id| draft.selected.contains(id));
    let head_check = check_box(
        "tiny-members-all",
        if all {
            Some(true)
        } else if some {
            None
        } else {
            Some(false)
        },
        !ticks.is_empty(),
    )
    .when(!ticks.is_empty(), |check| {
        check.on_click(cx.listener(move |this, _, _, cx| {
            let ticks = ticks.clone();
            this.tiny_members_draft(cx, |draft| {
                if all {
                    ticks.iter().for_each(|id| {
                        draft.selected.remove(id);
                    });
                } else {
                    draft.selected.extend(ticks);
                }
            })
        }))
    });
    let mut table = kit::card().child(
        div()
            .flex()
            .items_center()
            .pl(px(12.))
            .child(head_check)
            .child(div().flex_1().child(table_head(&[("Name", None), ("Roles", Some(150.)), ("Joined", Some(84.)), ("Last seen", Some(84.)), ("", Some(28.))]).border_b_0())),
    );
    table = table.child(div().h(px(1.)).bg(theme::hairline()));
    let now = kit::now();
    for member in &rows {
        table = table.child(row(facts, member, draft.selected.contains(&member.user_id), now, cx));
    }
    if rows.is_empty() {
        table = table.child(div().py(px(28.)).flex().justify_center().type_ui().text_color(theme::text_soft()).child(if query.trim().is_empty() {
            "Nobody has this role.".to_owned()
        } else {
            format!("Nobody matches “{}”.", query.trim())
        }));
    }
    page = page.child(table);
    page.into_any_element()
}

fn row(facts: &Facts, member: &TinyMember, on: bool, now: i64, cx: &mut Context<Shell>) -> AnyElement {
    let community = facts.community;
    let name = model::name_of(member);
    let me = member.user_id == facts.me;
    let mut avatar = Avatar::new(&facts.key(&member.user_id), &name, 32.).image_at(facts.picture(&member.user_id));
    if let Some(presence) = facts.online(&member.user_id) {
        avatar = avatar.presence(presence, theme::raised());
    }
    let color = model::color_of(community, member).map(gpui::rgb).unwrap_or(theme::text());
    let can_tick = selectable(community, member);
    let id = member.user_id.clone();
    let tick = check_box(SharedString::from(format!("tiny-members-tick-{}", member.user_id)), Some(on), can_tick).when(can_tick, |check| {
        check.on_click(cx.listener(move |this, _, _, cx| {
            let id = id.clone();
            this.tiny_members_draft(cx, |draft| {
                if !draft.selected.remove(&id) {
                    draft.selected.insert(id);
                }
            })
        }))
    });
    // Who they sign in as, for those who manage people.
    let identity = (facts.may.see_email && !member.email.is_empty()).then(|| member.email.clone());
    let by_group = facts.may.see_email && member.joined_via.as_deref() == Some("groups");
    let mut roles = div().flex_none().w(px(150.)).flex().flex_wrap().gap(px(4.));
    let mine = model::roles_of(community, member);
    if mine.is_empty() {
        roles = roles.child(div().type_small().text_color(theme::text_faint()).child(if community.roles.is_empty() { model::role_word(&member.role) } else { "—" }));
    }
    for role in mine.iter().take(2) {
        roles = roles.child(kit::role_chip(role.name.clone(), role.color.as_deref().and_then(model::hex)));
    }
    if mine.len() > 2 {
        roles = roles.child(div().type_small().text_color(theme::text_faint()).child(format!("+{}", mine.len() - 2)));
    }
    let joined = protocol::tiny_unix(&member.created_at).map(crate::gui::time::date).unwrap_or_default();
    let online = facts.online(&member.user_id) == Some(protocol::Presence::Online);
    let seen = if online { "Online".to_owned() } else { member.last_seen_at.as_deref().and_then(protocol::tiny_unix).map(|at| model::relative(now, at)).unwrap_or_else(|| "—".to_owned()) };
    let timed_out = member.timeout_until.as_deref().and_then(protocol::tiny_unix).is_some_and(|until| until > now);
    let (menu_id, right_id) = (member.user_id.clone(), member.user_id.clone());
    table_row(SharedString::from(format!("tiny-members-row-{}", member.user_id)), on)
        .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Person { user_id: right_id.clone(), at: event.position }, cx)))
        .child(tick)
        .child(
            div().flex_1().min_w_0().flex().items_center().gap(px(10.)).child(avatar.build()).child(
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
                            .when(model::is_owner(member), |line| line.child(glyph("icons/crown.svg", 12., theme::warning())))
                            .when(me, |line| line.child(crate::gui::tiny::channel::chip("You", theme::text_soft(), theme::hover())))
                            .when(timed_out, |line| line.child(crate::gui::tiny::channel::chip("Timed out", theme::warning(), theme::warning_soft())))
                            .when(by_group, |line| line.child(crate::gui::tiny::channel::chip("Sign-in group", theme::accent_text(), theme::accent_soft()))),
                    )
                    .children(identity.map(|identity| div().min_w_0().truncate().type_small().text_color(theme::text_faint()).child(identity))),
            ),
        )
        .child(roles)
        .child(cell(84., joined, theme::text_soft()))
        .child(cell(84., seen, if online { theme::positive() } else { theme::text_soft() }))
        .child(icon_button(SharedString::from(format!("tiny-members-more-{}", member.user_id)), "icons/ellipsis.svg", false).tooltip(tip("More", None)).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.tiny_open_float(Float::Person { user_id: menu_id.clone(), at: event.position }, cx);
            }),
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{TinyAccess, TinyRole, TINY_ALL};

    fn community() -> TinyCommunity {
        let member = |id: &str, name: &str, roles: &[&str], owner: bool| TinyMember {
            user_id: id.into(),
            display_name: name.into(),
            email: format!("{id}@x.example"),
            owner,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            ..TinyMember::default()
        };
        let role = |id: &str, position: i32| TinyRole { id: id.into(), name: id.into(), position, ..TinyRole::default() };
        let mut community: TinyCommunity = serde_json::from_value(serde_json::json!({"role":"admin","members":[],"invites":[],"audit":[]})).unwrap();
        community.me = "me".into();
        community.roles = vec![role("admin", 3), role("mod", 2), role("raid", 1), TinyRole { default: true, ..role("everyone", 0) }];
        community.members = vec![
            member("zed", "Zed", &["raid"], false),
            member("me", "Me", &["mod"], false),
            member("boss", "Boss", &[], true),
            member("ann", "Ann", &["admin"], false),
            member("bea", "Bea", &[], false),
        ];
        community.access = Some(TinyAccess { owner: false, roles: vec!["mod".into()], permissions: TINY_ALL, rank: 2 });
        community
    }

    #[test]
    fn the_table_lists_owners_then_by_rank_and_filters_by_name_email_and_role() {
        let community = community();
        let names = |rows: Vec<&TinyMember>| rows.into_iter().map(|member| member.user_id.clone()).collect::<Vec<_>>();
        assert_eq!(names(listed(&community, "", None)), ["boss", "ann", "me", "zed", "bea"]);
        assert_eq!(names(listed(&community, "ZE", None)), ["zed"]);
        assert_eq!(names(listed(&community, "bea@x", None)), ["bea"]);
        assert_eq!(names(listed(&community, "", Some("raid"))), ["zed"]);
    }

    #[test]
    fn only_people_below_me_are_ticked_and_bulk_changes_skip_who_has_it_already() {
        let community = community();
        let tickable: Vec<_> = community.members.iter().filter(|member| selectable(&community, member)).map(|member| member.user_id.as_str()).collect();
        assert_eq!(tickable, ["zed", "bea"], "not me, not the owner, not someone above me");
        let selected: BTreeSet<String> = ["zed", "bea", "ann"].map(String::from).into();
        assert_eq!(bulk_roles(&community, &selected, "raid", true), [("bea".to_owned(), vec!["raid".to_owned()])]);
        assert_eq!(bulk_roles(&community, &selected, "raid", false), [("zed".to_owned(), Vec::<String>::new())]);
    }
}
