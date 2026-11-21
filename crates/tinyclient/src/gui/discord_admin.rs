//! Discord administration overlays: channel/category permission overwrites (#397),
//! server audit log (#394), and AutoMod rules & safety setup (#395).

use super::components::{button, button_with_icon, glyph, icon_button, ButtonKind};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{div, prelude::*, px, Context, Entity, MouseButton, SharedString, Stateful, Div};
use protocol::{
    effective_permissions, known_permission_mask, merge_overwrite_bits, would_lock_self_out,
    ChannelOverwrite, ChannelPermissionsInfo, Key, OverwriteTargetKind, PermissionSection, Request,
    Response, DISCORD_PERMISSIONS, PERM_VIEW_CHANNEL,
};

#[derive(Default)]
pub struct State {
    pub perms: Option<PermsModal>,
}

pub struct PermsModal {
    pub conversation: Key,
    pub info: Option<ChannelPermissionsInfo>,
    pub selected: Option<(String, OverwriteTargetKind)>,
    /// Draft `(allow, deny)` bits for the currently selected overwrite target.
    pub draft_allow: u64,
    pub draft_deny: u64,
    pub dirty: bool,
    pub add_member: Entity<TextInput>,
    pub loading: bool,
    pub busy: bool,
    pub error: Option<String>,
}

impl PermsModal {
    fn select_target(&mut self, id: String, kind: OverwriteTargetKind) {
        let (allow, deny) = self
            .info
            .as_ref()
            .and_then(|i| i.overwrites.iter().find(|o| o.id == id && o.kind == kind))
            .map(|o| (o.allow, o.deny))
            .unwrap_or((0, 0));
        self.selected = Some((id, kind));
        self.draft_allow = allow;
        self.draft_deny = deny;
        self.dirty = false;
        self.error = None;
    }

    fn apply_info(&mut self, info: ChannelPermissionsInfo) {
        let keep = self.selected.clone().filter(|(id, kind)| {
            info.overwrites.iter().any(|o| &o.id == id && o.kind == *kind)
                || (*kind == OverwriteTargetKind::Role && *id == info.everyone_role_id)
        });
        let target = keep.unwrap_or_else(|| (info.everyone_role_id.clone(), OverwriteTargetKind::Role));
        self.info = Some(info);
        self.loading = false;
        self.busy = false;
        self.select_target(target.0, target.1);
    }
}

impl Shell {
    pub(super) fn close_discord_admin(&mut self, cx: &mut Context<Self>) -> bool {
        if self.discord_admin_state.perms.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    /// Opens the Discord channel/category permission overwrites modal (#397).
    pub(in crate::gui) fn open_channel_permissions(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let add_member = cx.new(|cx| TextInput::new("Member user ID…", cx));
        self.discord_admin_state.perms = Some(PermsModal {
            conversation: conversation.clone(),
            info: None,
            selected: None,
            draft_allow: 0,
            draft_deny: 0,
            dirty: false,
            add_member,
            loading: true,
            busy: false,
            error: None,
        });
        cx.notify();
        self.reload_channel_permissions(conversation, cx);
    }

    fn reload_channel_permissions(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let req = Request::ChannelPermissions { conversation: conversation.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                if let Some(modal) = this.discord_admin_state.perms.as_mut() {
                    if modal.conversation != conversation {
                        return;
                    }
                    modal.loading = false;
                    modal.busy = false;
                    match answer {
                        Ok(Response::ChannelPermissions { info }) => modal.apply_info(*info),
                        Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                        Ok(_) => modal.error = Some("Could not load channel permissions.".to_owned()),
                        Err(err) => modal.error = Some(err.to_string()),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_selected_overwrite(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.perms.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let Some(info) = modal.info.as_ref() else {
            return;
        };
        let Some((target_id, target_kind)) = modal.selected.clone() else {
            return;
        };
        let existing = info
            .overwrites
            .iter()
            .find(|o| o.id == target_id && o.kind == target_kind);
        let (allow, deny) = merge_overwrite_bits(
            existing.map_or(0, |o| o.allow),
            existing.map_or(0, |o| o.deny),
            known_permission_mask(),
            modal.draft_allow,
            modal.draft_deny,
        );
        if would_lock_self_out(
            info.is_owner,
            info.guild_permissions,
            &info.me_id,
            &info.everyone_role_id,
            &info.my_roles,
            &info.overwrites,
            &target_id,
            target_kind,
            Some((allow, deny)),
        ) {
            modal.error = Some("Refused: this change would lock you out of viewing or managing permissions here.".to_owned());
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let conversation = modal.conversation.clone();
        cx.notify();
        let req = Request::SetOverwrite {
            conversation: conversation.clone(),
            target_id,
            target_kind,
            allow,
            deny,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Permissions saved", cx);
                    this.reload_channel_permissions(conversation, cx);
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not save permissions.".to_owned());
                    }
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some(err.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_private_channel(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.perms.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let Some(info) = modal.info.as_ref() else {
            return;
        };
        let everyone = info.everyone_role_id.clone();
        let make_private = !info.private;
        let existing = info
            .overwrites
            .iter()
            .find(|o| o.id == everyone && o.kind == OverwriteTargetKind::Role);
        let (cur_allow, cur_deny) = existing.map_or((0, 0), |o| (o.allow, o.deny));
        let (new_allow, new_deny) = if make_private {
            (cur_allow & !PERM_VIEW_CHANNEL, cur_deny | PERM_VIEW_CHANNEL)
        } else {
            (cur_allow, cur_deny & !PERM_VIEW_CHANNEL)
        };
        if would_lock_self_out(
            info.is_owner,
            info.guild_permissions,
            &info.me_id,
            &everyone,
            &info.my_roles,
            &info.overwrites,
            &everyone,
            OverwriteTargetKind::Role,
            Some((new_allow, new_deny)),
        ) {
            modal.error = Some(
                "Add yourself or one of your roles with View Channel allowed before making this channel private."
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        modal.busy = true;
        modal.error = None;
        let conversation = modal.conversation.clone();
        cx.notify();
        let req = Request::SetOverwrite {
            conversation: conversation.clone(),
            target_id: everyone,
            target_kind: OverwriteTargetKind::Role,
            allow: new_allow,
            deny: new_deny,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => this.reload_channel_permissions(conversation, cx),
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                _ => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not update channel visibility.".to_owned());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn delete_channel_overwrite(&mut self, target_id: String, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.perms.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let conversation = modal.conversation.clone();
        modal.busy = true;
        modal.error = None;
        cx.notify();
        let req = Request::DeleteOverwrite {
            conversation: conversation.clone(),
            target_id,
        };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.toast(Some(("icons/trash-2.svg", theme::text_soft())), "Overwrite removed", cx);
                    this.reload_channel_permissions(conversation, cx);
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                _ => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not remove overwrite.".to_owned());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn sync_permissions_with_category(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.perms.as_mut() else {
            return;
        };
        if modal.busy {
            return;
        }
        let conversation = modal.conversation.clone();
        modal.busy = true;
        modal.error = None;
        cx.notify();
        let req = Request::SyncChannelPermissions { conversation: conversation.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Synced with category", cx);
                    this.reload_channel_permissions(conversation, cx);
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some(detail);
                    }
                    cx.notify();
                }
                _ => {
                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                        m.busy = false;
                        m.error = Some("Could not sync permissions.".to_owned());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(in crate::gui) fn discord_admin_layer(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        self.channel_permissions_modal(cx)
    }

    fn channel_permissions_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let modal = self.discord_admin_state.perms.as_ref()?;
        let title = modal
            .info
            .as_ref()
            .map(|i| {
                if i.is_category {
                    format!("Permissions · {}", i.name)
                } else {
                    format!("Permissions · #{}", i.name)
                }
            })
            .unwrap_or_else(|| "Channel permissions".to_owned());

        let body = if modal.loading {
            div().py(px(24.)).type_small().text_color(theme::text_soft()).child("Loading permissions…")
        } else if let Some(info) = modal.info.as_ref() {
            let everyone = info.everyone_role_id.clone();
            let selected = modal.selected.clone();
            let mut targets: Vec<ChannelOverwrite> = info.overwrites.clone();
            if !targets.iter().any(|o| o.id == everyone && o.kind == OverwriteTargetKind::Role) {
                targets.insert(
                    0,
                    ChannelOverwrite {
                        id: everyone.clone(),
                        kind: OverwriteTargetKind::Role,
                        name: "@everyone".to_owned(),
                        allow: 0,
                        deny: 0,
                    },
                );
            }
            if let Some((sel_id, sel_kind)) = &selected {
                if !targets.iter().any(|o| &o.id == sel_id && o.kind == *sel_kind) {
                    let name = if *sel_kind == OverwriteTargetKind::Role {
                        info.roles
                            .iter()
                            .find(|r| &r.id == sel_id)
                            .map(|r| r.name.clone())
                            .unwrap_or_else(|| format!("Role {sel_id}"))
                    } else {
                        format!("Member {sel_id}")
                    };
                    targets.push(ChannelOverwrite {
                        id: sel_id.clone(),
                        kind: *sel_kind,
                        name,
                        allow: modal.draft_allow,
                        deny: modal.draft_deny,
                    });
                }
            }

            let target_rows: Vec<_> = targets
                .iter()
                .enumerate()
                .map(|(ix, ov)| {
                    let is_sel = selected.as_ref().is_some_and(|(id, k)| id == &ov.id && *k == ov.kind);
                    let is_everyone = ov.id == everyone && ov.kind == OverwriteTargetKind::Role;
                    let has_stored = info.overwrites.iter().any(|o| o.id == ov.id && o.kind == ov.kind);
                    let click_id = ov.id.clone();
                    let click_kind = ov.kind;
                    let del_id = ov.id.clone();
                    let icon = if ov.kind == OverwriteTargetKind::Member { "icons/user.svg" } else { "icons/shield.svg" };
                    div()
                        .id(("perm-target", ix))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(if is_sel { theme::active() } else { theme::canvas() })
                        .hover(|s| s.bg(theme::hover()))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(m) = this.discord_admin_state.perms.as_mut() {
                                m.select_target(click_id.clone(), click_kind);
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(glyph(icon, 14., theme::text_soft()))
                                .child(div().min_w_0().truncate().type_small().text_color(theme::text()).child(ov.name.clone())),
                        )
                        .when(!is_everyone && has_stored, |row| {
                            row.child(
                                icon_button(("perm-target-del", ix), "icons/x.svg", false)
                                    .size(px(20.))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_channel_overwrite(del_id.clone(), cx);
                                    })),
                            )
                        })
                })
                .collect();

            let addable_roles: Vec<_> = info
                .roles
                .iter()
                .filter(|r| r.id != everyone && !targets.iter().any(|t| t.id == r.id && t.kind == OverwriteTargetKind::Role))
                .take(8)
                .enumerate()
                .map(|(ix, r)| {
                    let rid = r.id.clone();
                    div()
                        .id(("perm-add-role", ix))
                        .px(px(6.))
                        .py(px(3.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::raised())
                        .hover(|s| s.bg(theme::hover()))
                        .cursor_pointer()
                        .type_micro()
                        .text_color(theme::accent_text())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(m) = this.discord_admin_state.perms.as_mut() {
                                m.select_target(rid.clone(), OverwriteTargetKind::Role);
                                m.dirty = true;
                                cx.notify();
                            }
                        }))
                        .child(format!("+ {}", r.name))
                })
                .collect();

            let draft_allow = modal.draft_allow;
            let draft_deny = modal.draft_deny;
            let effective_self = effective_permissions(
                info.is_owner,
                info.guild_permissions,
                &info.me_id,
                &everyone,
                &info.my_roles,
                &info.overwrites,
            );

            let mut perm_sections = div().flex().flex_col().gap(px(10.));
            for section in PermissionSection::ALL {
                let mut rows = div().flex().flex_col().gap(px(4.));
                for (ix, def) in DISCORD_PERMISSIONS.iter().enumerate().filter(|(_, p)| p.section == section) {
                    let bit = def.bit;
                    let is_deny = draft_deny & bit != 0;
                    let is_allow = !is_deny && (draft_allow & bit != 0);
                    let is_inherit = !is_deny && !is_allow;
                    let eff = effective_self & bit != 0;
                    let pill = |id: SharedString, label: &'static str, active: bool, active_bg: gpui::Rgba| {
                        div()
                            .id(id)
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(if active { active_bg } else { theme::raised() })
                            .type_micro()
                            .text_color(if active { theme::text() } else { theme::text_soft() })
                            .cursor_pointer()
                            .child(label)
                    };
                    rows = rows.child(
                        div()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::canvas())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
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
                                            .child(div().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).child(def.label))
                                            .child(
                                                div()
                                                    .type_micro()
                                                    .text_color(if eff { theme::text_faint() } else { theme::danger() })
                                                    .child(if eff { "You: allowed" } else { "You: denied" }),
                                            ),
                                    )
                                    .child(div().type_micro().text_color(theme::text_faint()).child(def.description)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(
                                        pill(SharedString::from(format!("perm-deny-{ix}")), "Deny", is_deny, theme::danger())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(m) = this.discord_admin_state.perms.as_mut() {
                                                    m.draft_allow &= !bit;
                                                    m.draft_deny |= bit;
                                                    m.dirty = true;
                                                    cx.notify();
                                                }
                                            })),
                                    )
                                    .child(
                                        pill(SharedString::from(format!("perm-inh-{ix}")), "Inherit", is_inherit, theme::active())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(m) = this.discord_admin_state.perms.as_mut() {
                                                    m.draft_allow &= !bit;
                                                    m.draft_deny &= !bit;
                                                    m.dirty = true;
                                                    cx.notify();
                                                }
                                            })),
                                    )
                                    .child(
                                        pill(SharedString::from(format!("perm-allow-{ix}")), "Allow", is_allow, theme::accent())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(m) = this.discord_admin_state.perms.as_mut() {
                                                    m.draft_deny &= !bit;
                                                    m.draft_allow |= bit;
                                                    m.dirty = true;
                                                    cx.notify();
                                                }
                                            })),
                                    ),
                            ),
                    );
                }
                perm_sections = perm_sections.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child(section.label()))
                        .child(rows),
                );
            }

            let sync_row = info.synced_with_category.map(|synced| {
                let cat_label = info.parent_name.as_deref().unwrap_or("parent category");
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::canvas())
                    .child(
                        div()
                            .type_small()
                            .text_color(theme::text_soft())
                            .child(if synced {
                                format!("Permissions synced with {cat_label}")
                            } else {
                                format!("Permissions not synced with {cat_label}")
                            }),
                    )
                    .when(!synced, |row| {
                        row.child(
                            button("perm-sync-cat", ButtonKind::Secondary, "Sync with category")
                                .when(!modal.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.sync_permissions_with_category(cx)))),
                        )
                    })
            });

            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::canvas())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child("Private channel"))
                                .child(div().type_micro().text_color(theme::text_faint()).child("Only selected roles and members can view this channel.")),
                        )
                        .child(
                            button(
                                "perm-private-toggle",
                                if info.private { ButtonKind::Primary } else { ButtonKind::Secondary },
                                if info.private { "Private: On" } else { "Private: Off" },
                            )
                            .when(!modal.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.toggle_private_channel(cx)))),
                        ),
                )
                .children(sync_row)
                .child(
                    div()
                        .flex()
                        .gap(px(12.))
                        .h(px(360.))
                        .child(
                            div()
                                .id("perm-targets-col")
                                .w(px(200.))
                                .flex_none()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(div().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child("Roles & members"))
                                .children(target_rows)
                                .when(!addable_roles.is_empty(), |col| {
                                    col.child(div().mt(px(6.)).type_micro().text_color(theme::text_faint()).child("Add role"))
                                        .child(div().flex().flex_wrap().gap(px(4.)).children(addable_roles))
                                })
                                .child(
                                    div()
                                        .mt(px(6.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(div().type_micro().text_color(theme::text_faint()).child("Add member by ID"))
                                        .child(
                                            div()
                                                .px(px(6.))
                                                .py(px(4.))
                                                .rounded(px(theme::R_CONTROL))
                                                .bg(theme::canvas())
                                                .border_1()
                                                .border_color(theme::border())
                                                .type_small()
                                                .child(modal.add_member.clone()),
                                        )
                                        .child(
                                            button("perm-add-member", ButtonKind::Secondary, "Add member").on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    if let Some(m) = this.discord_admin_state.perms.as_mut() {
                                                        let uid = m.add_member.read(cx).text().trim().to_owned();
                                                        if !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()) {
                                                            m.add_member.update(cx, |i, cx| i.clear(cx));
                                                            m.select_target(uid, OverwriteTargetKind::Member);
                                                            m.dirty = true;
                                                        } else {
                                                            m.error = Some("Enter a numeric Discord user ID.".to_owned());
                                                        }
                                                        cx.notify();
                                                    }
                                                }),
                                            ),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id("perm-matrix-col")
                                .flex_1()
                                .min_w_0()
                                .overflow_y_scroll()
                                .pr(px(4.))
                                .child(perm_sections),
                        ),
                )
        } else {
            div().py(px(12.)).type_small().text_color(theme::text_soft()).child("Permissions unavailable.")
        };

        let can_save = modal.dirty && !modal.busy && modal.selected.is_some();
        let card = div()
            .id("discord-perms-card")
            .w(px(760.))
            .max_w_full()
            .p(px(16.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_title().text_color(theme::text()).child(title))
                    .child(icon_button("discord-perms-close", "icons/x.svg", false).size(px(24.)).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.discord_admin_state.perms = None;
                            cx.notify();
                        }),
                    )),
            )
            .child(body)
            .children(modal.error.as_ref().map(|e| div().type_small().text_color(theme::danger()).child(e.clone())))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("discord-perms-done", ButtonKind::Ghost, "Close").on_click(cx.listener(|this, _, _, cx| {
                        this.discord_admin_state.perms = None;
                        cx.notify();
                    })))
                    .child(
                        button_with_icon(
                            "discord-perms-save",
                            if can_save { ButtonKind::Primary } else { ButtonKind::Ghost },
                            "icons/check.svg",
                            if modal.busy { "Saving…" } else { "Save overwrite" },
                        )
                        .when(can_save, |b| b.on_click(cx.listener(|this, _, _, cx| this.save_selected_overwrite(cx)))),
                    ),
            );

        Some(
            div()
                .id("discord-perms-backdrop")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.discord_admin_state.perms = None;
                    cx.notify();
                }))
                .child(card),
        )
    }
}
