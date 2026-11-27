//! Discord administration overlays: channel/category permission overwrites (#397),
//! server audit log (#394), and AutoMod rules & safety setup (#395).

use super::components::{button, button_disabled, button_with_icon, glyph, icon_button, segment, segmented, ButtonKind, Layer};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{deferred, div, prelude::*, px, AnyElement, Context, Div, Entity, MouseButton, SharedString, Stateful};
use protocol::{
    effective_permissions, known_permission_mask, merge_overwrite_bits, would_lock_self_out,
    AuditEntry, AutomodAction, AutomodRule, AutomodTrigger, ChannelOverwrite, ChannelPermissionsInfo, Key,
    OverwriteTargetKind, PermissionSection, Request, Response, SpaceSafety, DISCORD_PERMISSIONS, PERM_VIEW_CHANNEL,
};

/// Action-type filters for the audit log page. The adapter still labels every type.
const AUDIT_FILTERS: &[(u32, &str)] = &[
    (20, "Kicks"),
    (22, "Bans"),
    (24, "Members"),
    (11, "Channels"),
    (31, "Roles"),
    (72, "Messages"),
    (140, "AutoMod"),
];

const VERIFY_LEVELS: &[(u8, &str)] = &[(0, "None"), (1, "Low"), (2, "Medium"), (3, "High"), (4, "Highest")];
const EXPLICIT_FILTERS: &[(u8, &str)] = &[(0, "Don't scan"), (1, "No roles"), (2, "Everyone")];
const PAUSE_CHOICES: &[(i64, &str)] = &[(0, "Off"), (3600, "1 hour"), (86_400, "24 hours")];
const AUTOMOD_TRIGGERS: &[(u8, &str)] = &[(1, "Keywords"), (3, "Spam"), (4, "Presets"), (5, "Mentions"), (6, "Profile")];
const PRESET_LABELS: &[&str] = &["Profanity", "Sexual content", "Slurs"];

#[derive(Default)]
pub struct State {
    pub perms: Option<PermsModal>,
    pub audit: Option<AuditModal>,
    pub safety: Option<SafetyModal>,
}

struct AuditModal {
    space: Key,
    name: String,
    entries: Vec<AuditEntry>,
    more: bool,
    loading: bool,
    error: Option<String>,
    person: Entity<TextInput>,
    action: Option<u32>,
    expanded: Option<String>,
    generation: u64,
}

struct SafetyModal {
    space: Key,
    name: String,
    rules: Vec<AutomodRule>,
    selected: Option<usize>,
    creating: bool,
    loaded: bool,
    loading: bool,
    busy: bool,
    error: Option<String>,
    generation: u64,
    draft_enabled: bool,
    draft_trigger: u8,
    draft_presets: [bool; 3],
    draft_raid: bool,
    draft_block: bool,
    draft_alert: bool,
    draft_timeout: bool,
    draft_quarantine: bool,
    draft_read_only: bool,
    draft_id: String,
    verification: u8,
    explicit: u8,
    invites_for: i64,
    dms_for: i64,
    name_input: Entity<TextInput>,
    words: Entity<TextInput>,
    patterns: Entity<TextInput>,
    allow: Entity<TextInput>,
    block_message: Entity<TextInput>,
    alert_channel: Entity<TextInput>,
    timeout_seconds: Entity<TextInput>,
    exempt_roles: Entity<TextInput>,
    exempt_channels: Entity<TextInput>,
    mention_limit: Entity<TextInput>,
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
        let perms = self.discord_admin_state.perms.take().is_some();
        let audit = self.discord_admin_state.audit.take().is_some();
        let safety = self.discord_admin_state.safety.take().is_some();
        if perms || audit || safety {
            cx.notify();
            return true;
        }
        false
    }

    /// Opens the Discord channel/category permission overwrites modal (#397).
    pub(in crate::gui) fn open_channel_permissions(&mut self, conversation: Key, cx: &mut Context<Self>) {
        self.discord_admin_state.audit = None;
        self.discord_admin_state.safety = None;
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

    /// Opens a Discord server's audit log (#394). Shown only when the account may view it.
    pub(in crate::gui) fn open_audit_log(&mut self, space: Key, name: String, cx: &mut Context<Self>) {
        self.discord_admin_state.perms = None;
        self.discord_admin_state.safety = None;
        let person = cx.new(|cx| TextInput::new("Filter by user ID", cx));
        self.discord_admin_state.audit = Some(AuditModal {
            space,
            name,
            entries: Vec::new(),
            more: false,
            loading: false,
            error: None,
            person,
            action: None,
            expanded: None,
            generation: 0,
        });
        cx.notify();
        self.reload_audit_log(false, cx);
    }

    fn reload_audit_log(&mut self, older: bool, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.audit.as_mut() else {
            return;
        };
        if modal.loading {
            return;
        }
        let person_field = modal.person.clone();
        let person_text = person_field.read(cx).text().trim().to_owned();
        let person = if person_text.is_empty() { None } else { Some(person_text) };
        let action = modal.action;
        let before = if older { modal.entries.last().map(|entry| entry.id.clone()) } else { None };
        if older && before.is_none() {
            return;
        }
        let space = modal.space.clone();
        modal.generation = modal.generation.wrapping_add(1);
        let generation = modal.generation;
        if !older {
            modal.entries.clear();
            modal.more = false;
            modal.expanded = None;
        }
        modal.loading = true;
        modal.error = None;
        cx.notify();
        let req = Request::AuditLog { space: space.clone(), person, action, before, limit: 50 };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin_state.audit.as_mut() else {
                    return;
                };
                if modal.space != space || modal.generation != generation {
                    return;
                }
                modal.loading = false;
                match answer {
                    Ok(Response::AuditLog { entries, more }) => {
                        for entry in entries {
                            if modal.entries.iter().any(|have| have.id == entry.id) {
                                continue;
                            }
                            modal.entries.push(entry);
                        }
                        modal.more = more;
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not read the audit log.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::gui) fn discord_admin_layer(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.channel_permissions_modal(window, cx)
            .or_else(|| self.audit_log_modal(window, cx))
            .or_else(|| self.safety_modal(window, cx))
    }

    fn audit_log_modal(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.discord_admin_state.audit.as_ref()?;
        let viewport = window.viewport_size();
        let max_w = (f32::from(viewport.width) - 32.).max(200.);
        let max_h = (f32::from(viewport.height) - 32.).max(200.);
        let title = format!("Audit log · {}", modal.name);
        let selected = modal.action;
        let mut filters = segmented().flex_wrap().child(
            segment("audit-action-any", "Any action", selected.is_none()).on_click(cx.listener(|this, _, _, cx| {
                // A click during a load must not light a chip the in-flight page does not match.
                let ready = this.discord_admin_state.audit.as_mut().is_some_and(|modal| {
                    if modal.loading {
                        false
                    } else {
                        modal.action = None;
                        true
                    }
                });
                if ready {
                    this.reload_audit_log(false, cx);
                }
            })),
        );
        for (code, label) in AUDIT_FILTERS {
            let code = *code;
            filters = filters.child(
                segment(SharedString::from(format!("audit-action-{code}")), *label, selected == Some(code)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let ready = this.discord_admin_state.audit.as_mut().is_some_and(|modal| {
                            if modal.loading {
                                false
                            } else {
                                modal.action = Some(code);
                                true
                            }
                        });
                        if ready {
                            this.reload_audit_log(false, cx);
                        }
                    },
                )),
            );
        }

        let rows: Vec<_> = modal
            .entries
            .iter()
            .enumerate()
            .map(|(ix, entry)| {
                let open = modal.expanded.as_deref() == Some(entry.id.as_str());
                let id = entry.id.clone();
                let when = if entry.time > 0 {
                    format!("{} {}", super::time::date(entry.time), super::time::clock(entry.time))
                } else {
                    String::new()
                };
                let changes: Vec<String> = entry.changes.iter().map(audit_change_line).collect();
                div()
                    .id(("audit-row", ix))
                    .px(px(8.))
                    .py(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .cursor_pointer()
                    .hover(|row| row.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(m) = this.discord_admin_state.audit.as_mut() {
                            m.expanded = if m.expanded.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                            cx.notify();
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().type_small().text_color(theme::text()).child(entry.summary.clone()))
                    .child(
                        div()
                            .type_micro()
                            .text_color(theme::text_faint())
                            .child(format!("{} · {}{}", entry.action_label, entry.action_type, if when.is_empty() { String::new() } else { format!(" · {when}") })),
                    )
                    .when(!entry.reason.is_empty(), |row| {
                        row.child(div().type_small().text_color(theme::text_soft()).child(format!("Reason: {}", entry.reason)))
                    })
                    .when(open, |row| {
                        row.children(changes.into_iter().map(|line| div().type_micro().text_color(theme::text_soft()).child(line)))
                            .when(entry.changes.is_empty(), |row| row.child(div().type_micro().text_color(theme::text_faint()).child("No field changes.")))
                    })
            })
            .collect();

        let body = if modal.loading && modal.entries.is_empty() {
            div().py(px(24.)).type_small().text_color(theme::text_soft()).child("Loading the audit log…")
        } else if modal.entries.is_empty() {
            div()
                .py(px(24.))
                .type_small()
                .text_color(theme::text_soft())
                .child(if modal.action.is_some() || !modal.person.read(cx).text().trim().is_empty() {
                    "Nothing matches these filters."
                } else {
                    "Nothing has been recorded."
                })
        } else {
            div().flex().flex_col().gap(px(4.)).children(rows)
        };

        let loading = modal.loading;
        let more = modal.more && !modal.entries.is_empty();
        let person_focused = gpui::Focusable::focus_handle(modal.person.read(cx), cx).is_focused(window);
        let card = div()
            .id("discord-audit-card")
            .w(px(640.0_f32.min(max_w)))
            .max_w_full()
            .max_h(px(640.0_f32.min(max_h)))
            .p(px(16.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().truncate().type_title().text_color(theme::text()).child(title))
                    .child(
                        icon_button("discord-audit-close", "icons/x.svg", false)
                            .aria_label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discord_admin_state.audit = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        super::components::field(person_focused)
                            .flex_1()
                            .min_w_0()
                            .child(div().flex_1().min_w_0().child(modal.person.clone())),
                    )
                    .child(button("discord-audit-apply", ButtonKind::Secondary, "Apply").on_click(cx.listener(|this, _, _, cx| this.reload_audit_log(false, cx)))),
            )
            .child(div().flex().child(filters))
            .children(modal.error.as_ref().map(|error| div().type_small().text_color(theme::danger()).child(error.clone())))
            .child(div().id("discord-audit-list").flex_1().min_h_0().max_h(px(360.)).overflow_y_scroll().flex().flex_col().gap(px(4.)).child(body).when(more, |list| {
                list.child(
                    div().flex().justify_center().py(px(8.)).child(if loading {
                        button_disabled("Loading…").into_any_element()
                    } else {
                        button("discord-audit-more", ButtonKind::Secondary, "Load older entries")
                            .on_click(cx.listener(|this, _, _, cx| this.reload_audit_log(true, cx)))
                            .into_any_element()
                    }),
                )
            }));

        Some(
            deferred(
                div()
                    .id("discord-audit-backdrop")
                    .absolute()
                    .inset_0()
                    .p(px(16.))
                    .bg(theme::scrim())
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.discord_admin_state.audit = None;
                        cx.notify();
                    }))
                    .child(card),
            )
            .priority(Layer::Modal as usize)
            .into_any_element(),
        )
    }

    fn channel_permissions_modal(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.discord_admin_state.perms.as_ref()?;
        let viewport = window.viewport_size();
        let wide = f32::from(viewport.width);
        let max_w = (wide - 32.).max(200.);
        let max_h = (f32::from(viewport.height) - 32.).max(200.);
        let stacked = wide < 600.;
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
                        .bg(if is_sel { theme::selected() } else { theme::canvas() })
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
                    let pill = |id: SharedString, label: &'static str, active: bool, active_bg: gpui::Rgba, active_fg: gpui::Rgba| {
                        div()
                            .id(id)
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(if active { active_bg } else { theme::raised() })
                            .type_micro()
                            .text_color(if active { active_fg } else { theme::text_soft() })
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
                                        pill(SharedString::from(format!("perm-deny-{ix}")), "Deny", is_deny, theme::danger(), theme::on_accent())
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
                                        pill(SharedString::from(format!("perm-inh-{ix}")), "Inherit", is_inherit, theme::selected(), theme::text())
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
                                        pill(SharedString::from(format!("perm-allow-{ix}")), "Allow", is_allow, theme::accent(), theme::on_accent())
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
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .when(stacked, |box_| box_.flex_col())
                        .gap(px(12.))
                        .child(
                            div()
                                .id("perm-targets-col")
                                .when(!stacked, |col| col.w(px(200.)).flex_none())
                                .when(stacked, |col| col.w_full().max_h(px(176.)).flex_none())
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
                                            super::components::field(gpui::Focusable::focus_handle(modal.add_member.read(cx), cx).is_focused(window))
                                                .child(div().flex_1().min_w_0().child(modal.add_member.clone())),
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
                                .min_h_0()
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
            .max_w(px(max_w))
            .h(px(580.0_f32.min(max_h)))
            .max_h(px(max_h))
            .p(px(16.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().truncate().type_title().text_color(theme::text()).child(title))
                    .child(
                        icon_button("discord-perms-close", "icons/x.svg", false)
                            .aria_label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discord_admin_state.perms = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .children(modal.error.as_ref().map(|e| div().flex_none().type_small().text_color(theme::danger()).child(e.clone())))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("discord-perms-done", ButtonKind::Ghost, "Close").on_click(cx.listener(|this, _, _, cx| {
                        this.discord_admin_state.perms = None;
                        cx.notify();
                    })))
                    .child(if modal.busy {
                        button_disabled("Saving…").into_any_element()
                    } else if can_save {
                        button_with_icon("discord-perms-save", ButtonKind::Primary, "icons/check.svg", "Save overwrite")
                            .on_click(cx.listener(|this, _, _, cx| this.save_selected_overwrite(cx)))
                            .into_any_element()
                    } else {
                        button_disabled("Save overwrite").into_any_element()
                    }),
            );

        Some(
            deferred(
                div()
                    .id("discord-perms-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(theme::scrim())
                    .occlude()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.discord_admin_state.perms = None;
                        cx.notify();
                    }))
                    .child(card),
            )
            .priority(Layer::Modal as usize)
            .into_any_element(),
        )
    }

    /// Opens a Discord server's Safety setup (#395). Shown only with Manage Server.
    pub(in crate::gui) fn open_safety(&mut self, space: Key, name: String, cx: &mut Context<Self>) {
        self.discord_admin_state.perms = None;
        self.discord_admin_state.audit = None;
        let name_input = cx.new(|cx| TextInput::new("Rule name", cx));
        let words = cx.new(|cx| TextInput::new("Keywords, separated by commas", cx));
        let patterns = cx.new(|cx| TextInput::new("Patterns, separated by commas", cx));
        let allow = cx.new(|cx| TextInput::new("Allowed words, separated by commas", cx));
        let block_message = cx.new(|cx| TextInput::new("Message shown when a post is blocked", cx));
        let alert_channel = cx.new(|cx| TextInput::new("Alert channel ID", cx));
        let timeout_seconds = cx.new(|cx| TextInput::new("Timeout seconds", cx));
        let exempt_roles = cx.new(|cx| TextInput::new("Exempt role IDs", cx));
        let exempt_channels = cx.new(|cx| TextInput::new("Exempt channel IDs", cx));
        let mention_limit = cx.new(|cx| TextInput::new("Mention limit", cx));
        self.discord_admin_state.safety = Some(SafetyModal {
            space: space.clone(),
            name,
            rules: Vec::new(),
            selected: None,
            creating: false,
            loaded: false,
            loading: true,
            busy: false,
            error: None,
            generation: 1,
            draft_enabled: true,
            draft_trigger: 1,
            draft_presets: [false; 3],
            draft_raid: false,
            draft_block: true,
            draft_alert: false,
            draft_timeout: false,
            draft_quarantine: false,
            draft_read_only: false,
            draft_id: String::new(),
            verification: 0,
            explicit: 0,
            invites_for: 0,
            dms_for: 0,
            name_input,
            words,
            patterns,
            allow,
            block_message,
            alert_channel,
            timeout_seconds,
            exempt_roles,
            exempt_channels,
            mention_limit,
        });
        cx.notify();
        self.reload_safety(cx);
    }

    fn reload_safety(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.safety.as_ref() else { return };
        let space = modal.space.clone();
        let generation = modal.generation;
        if let Some(modal) = self.discord_admin_state.safety.as_mut() {
            modal.loading = true;
            modal.error = None;
        }
        cx.notify();
        let req = Request::AutomodRules { space: space.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin_state.safety.as_mut() else { return };
                if modal.space != space || modal.generation != generation {
                    return;
                }
                modal.loading = false;
                match answer {
                    Ok(Response::AutomodRules { rules, safety }) => {
                        let now = crate::view::now();
                        modal.verification = safety.verification_level.min(4);
                        modal.explicit = safety.explicit_filter.min(2);
                        modal.invites_for = pause_choice(safety.invites_paused_until, now);
                        modal.dms_for = pause_choice(safety.dms_paused_until, now);
                        modal.rules = rules;
                        modal.loaded = true;
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not read safety setup.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_automod_rule(&mut self, index: usize, cx: &mut Context<Self>) {
        let rule = self.discord_admin_state.safety.as_ref().and_then(|modal| modal.rules.get(index).cloned());
        let Some(rule) = rule else { return };
        self.apply_rule_draft(Some(index), rule, cx);
    }

    fn new_automod_rule(&mut self, cx: &mut Context<Self>) {
        self.apply_rule_draft(None, blank_rule(), cx);
    }

    fn apply_rule_draft(&mut self, index: Option<usize>, rule: AutomodRule, cx: &mut Context<Self>) {
        let inputs = {
            let Some(modal) = self.discord_admin_state.safety.as_ref() else { return };
            (
                modal.name_input.clone(),
                modal.words.clone(),
                modal.patterns.clone(),
                modal.allow.clone(),
                modal.block_message.clone(),
                modal.alert_channel.clone(),
                modal.timeout_seconds.clone(),
                modal.exempt_roles.clone(),
                modal.exempt_channels.clone(),
                modal.mention_limit.clone(),
            )
        };
        let (words, patterns, allow, mention, raid) = rule_words(&rule.trigger);
        let presets = rule_presets(&rule.trigger);
        let (block, block_message, alert, alert_channel, timeout, timeout_seconds, quarantine) = rule_actions(&rule);
        inputs.0.update(cx, |input, cx| input.set_text(&rule.name, cx));
        inputs.1.update(cx, |input, cx| input.set_text(&words, cx));
        inputs.2.update(cx, |input, cx| input.set_text(&patterns, cx));
        inputs.3.update(cx, |input, cx| input.set_text(&allow, cx));
        inputs.4.update(cx, |input, cx| input.set_text(&block_message, cx));
        inputs.5.update(cx, |input, cx| input.set_text(&alert_channel, cx));
        inputs.6.update(cx, |input, cx| input.set_text(&timeout_seconds, cx));
        inputs.7.update(cx, |input, cx| input.set_text(&join_list(&rule.exempt_roles), cx));
        inputs.8.update(cx, |input, cx| input.set_text(&join_list(&rule.exempt_channels), cx));
        inputs.9.update(cx, |input, cx| input.set_text(&mention, cx));
        let Some(modal) = self.discord_admin_state.safety.as_mut() else { return };
        modal.selected = index;
        modal.creating = rule.id.is_empty();
        modal.draft_id = rule.id;
        modal.draft_read_only = rule.read_only;
        modal.draft_enabled = rule.enabled;
        modal.draft_trigger = trigger_code_of(&rule.trigger);
        modal.draft_presets = presets;
        modal.draft_raid = raid;
        modal.draft_block = block;
        modal.draft_alert = alert;
        modal.draft_timeout = timeout;
        modal.draft_quarantine = quarantine;
        modal.error = None;
        cx.notify();
    }

    fn save_selected_rule(&mut self, cx: &mut Context<Self>) {
        let prepared = {
            let Some(modal) = self.discord_admin_state.safety.as_ref() else { return };
            if modal.busy || modal.loading || !modal.loaded || modal.draft_read_only {
                return;
            }
            let words = modal.words.read(cx).text().to_owned();
            let patterns = modal.patterns.read(cx).text().to_owned();
            let allow = modal.allow.read(cx).text().to_owned();
            let mention = modal.mention_limit.read(cx).text().to_owned();
            let block_message = modal.block_message.read(cx).text().to_owned();
            let alert_channel = modal.alert_channel.read(cx).text().to_owned();
            let timeout_seconds = modal.timeout_seconds.read(cx).text().to_owned();
            let exempt_roles = modal.exempt_roles.read(cx).text().to_owned();
            let exempt_channels = modal.exempt_channels.read(cx).text().to_owned();
            let rule = AutomodRule {
                id: modal.draft_id.clone(),
                name: modal.name_input.read(cx).text().trim().to_owned(),
                enabled: modal.draft_enabled,
                event_type: if modal.draft_trigger == 6 { 2 } else { 1 },
                trigger: trigger_from(modal.draft_trigger, &words, &patterns, &allow, modal.draft_presets, &mention, modal.draft_raid),
                actions: actions_from(
                    modal.draft_block && modal.draft_trigger != 6,
                    &block_message,
                    modal.draft_alert,
                    &alert_channel,
                    modal.draft_timeout && (modal.draft_trigger == 1 || modal.draft_trigger == 5),
                    &timeout_seconds,
                    modal.draft_quarantine && modal.draft_trigger == 6,
                ),
                exempt_roles: split_list(&exempt_roles),
                exempt_channels: split_list(&exempt_channels),
                read_only: false,
                raw: String::new(),
            };
            Some((modal.space.clone(), rule))
        };
        let Some((space, rule)) = prepared else { return };
        if let AutomodTrigger::Keyword { keywords, regex, .. } | AutomodTrigger::Profile { keywords, regex, .. } = &rule.trigger {
            if keywords.is_empty() && regex.is_empty() {
                if let Some(modal) = self.discord_admin_state.safety.as_mut() {
                    modal.error = Some("A keyword or profile rule needs a keyword or a pattern.".to_owned());
                }
                cx.notify();
                return;
            }
        }
        if let Some(modal) = self.discord_admin_state.safety.as_mut() {
            modal.busy = true;
            modal.error = None;
        }
        cx.notify();
        let req = Request::SaveAutomodRule { space: space.clone(), rule };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin_state.safety.as_mut() else { return };
                if modal.space != space {
                    return;
                }
                modal.busy = false;
                match answer {
                    Ok(Response::AutomodRule { rule }) => {
                        modal.creating = false;
                        modal.draft_id = rule.id.clone();
                        modal.draft_read_only = rule.read_only;
                        if let Some(ix) = modal.rules.iter().position(|have| !rule.id.is_empty() && have.id == rule.id) {
                            modal.rules[ix] = rule;
                            modal.selected = Some(ix);
                        } else if !rule.id.is_empty() {
                            modal.rules.push(rule);
                            modal.selected = Some(modal.rules.len() - 1);
                        }
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not save that rule.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_selected_rule(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.safety.as_ref() else { return };
        if modal.busy || modal.loading || !modal.loaded || modal.draft_id.is_empty() {
            return;
        }
        let space = modal.space.clone();
        let id = modal.draft_id.clone();
        if let Some(modal) = self.discord_admin_state.safety.as_mut() {
            modal.busy = true;
            modal.error = None;
        }
        cx.notify();
        let req = Request::DeleteAutomodRule { space: space.clone(), id: id.clone() };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin_state.safety.as_mut() else { return };
                if modal.space != space {
                    return;
                }
                modal.busy = false;
                match answer {
                    Ok(Response::Ok) => {
                        modal.rules.retain(|rule| rule.id != id);
                        modal.selected = None;
                        modal.creating = false;
                        modal.draft_id.clear();
                        modal.draft_read_only = false;
                    }
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not delete that rule.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_safety_levels(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.discord_admin_state.safety.as_ref() else { return };
        if modal.busy || modal.loading || !modal.loaded {
            return;
        }
        let now = crate::view::now();
        let space = modal.space.clone();
        let safety = SpaceSafety {
            verification_level: modal.verification,
            explicit_filter: modal.explicit,
            invites_paused_until: if modal.invites_for == 0 { 0 } else { now + modal.invites_for },
            dms_paused_until: if modal.dms_for == 0 { 0 } else { now + modal.dms_for },
        };
        if let Some(modal) = self.discord_admin_state.safety.as_mut() {
            modal.busy = true;
            modal.error = None;
        }
        cx.notify();
        let req = Request::SaveSpaceSafety { space: space.clone(), safety };
        cx.spawn(async move |shell, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(modal) = this.discord_admin_state.safety.as_mut() else { return };
                if modal.space != space {
                    return;
                }
                modal.busy = false;
                match answer {
                    Ok(Response::Ok) => {}
                    Ok(Response::Error { detail, .. }) => modal.error = Some(detail),
                    Ok(_) => modal.error = Some("Could not save safety setup.".to_owned()),
                    Err(err) => modal.error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn safety_modal(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.discord_admin_state.safety.as_ref()?;
        let viewport = window.viewport_size();
        let max_w = (f32::from(viewport.width) - 32.).max(200.);
        let max_h = (f32::from(viewport.height) - 32.).max(200.);
        let title = format!("Safety setup · {}", modal.name);
        let loaded = modal.loaded;
        let loading = modal.loading;
        let busy = modal.busy;
        let show_editor = !modal.draft_read_only && (modal.creating || modal.selected.is_some());
        let read_only = modal.draft_read_only;
        let can_timeout = modal.draft_trigger == 1 || modal.draft_trigger == 5;

        let mut verification = segmented().flex_wrap();
        for (level, label) in VERIFY_LEVELS {
            let level = *level;
            verification = verification.child(
                segment(SharedString::from(format!("safety-verify-{level}")), *label, modal.verification == level).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(m) = this.discord_admin_state.safety.as_mut() {
                            if !m.busy && !m.loading && m.loaded {
                                m.verification = level;
                                cx.notify();
                            }
                        }
                    },
                )),
            );
        }
        let mut explicit = segmented().flex_wrap();
        for (level, label) in EXPLICIT_FILTERS {
            let level = *level;
            explicit = explicit.child(
                segment(SharedString::from(format!("safety-explicit-{level}")), *label, modal.explicit == level).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(m) = this.discord_admin_state.safety.as_mut() {
                            if !m.busy && !m.loading && m.loaded {
                                m.explicit = level;
                                cx.notify();
                            }
                        }
                    },
                )),
            );
        }
        let mut invites = segmented().flex_wrap();
        let mut dms = segmented().flex_wrap();
        for (seconds, label) in PAUSE_CHOICES {
            let seconds = *seconds;
            invites = invites.child(
                segment(SharedString::from(format!("safety-invites-{seconds}")), *label, modal.invites_for == seconds).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(m) = this.discord_admin_state.safety.as_mut() {
                            if !m.busy && !m.loading && m.loaded {
                                m.invites_for = seconds;
                                cx.notify();
                            }
                        }
                    }),
                ),
            );
            dms = dms.child(
                segment(SharedString::from(format!("safety-dms-{seconds}")), *label, modal.dms_for == seconds).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(m) = this.discord_admin_state.safety.as_mut() {
                            if !m.busy && !m.loading && m.loaded {
                                m.dms_for = seconds;
                                cx.notify();
                            }
                        }
                    },
                )),
            );
        }

        let rows: Vec<_> = modal
            .rules
            .iter()
            .enumerate()
            .map(|(ix, rule)| {
                let selected_row = modal.selected == Some(ix);
                let name = if rule.name.is_empty() { "Untitled rule".to_owned() } else { rule.name.clone() };
                let detail = format!(
                    "{}{}{}",
                    trigger_name(&rule.trigger),
                    if rule.enabled { "" } else { " · Off" },
                    if rule.read_only { " · Read-only" } else { "" }
                );
                div()
                    .id(("safety-rule", ix))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(if selected_row { theme::selected() } else { theme::canvas() })
                    .hover(|row| row.bg(theme::hover()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.select_automod_rule(ix, cx)))
                    .child(div().type_small().text_color(theme::text()).child(name))
                    .child(div().type_micro().text_color(theme::text_faint()).child(detail))
            })
            .collect();

        let mut triggers = segmented().flex_wrap();
        let trigger_label = AUTOMOD_TRIGGERS.iter().find(|pair| pair.0 == modal.draft_trigger).map(|pair| pair.1).unwrap_or("Trigger");
        if modal.creating {
            for (code, label) in AUTOMOD_TRIGGERS {
                let code = *code;
                triggers = triggers.child(
                    segment(SharedString::from(format!("safety-trigger-{code}")), *label, modal.draft_trigger == code).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(m) = this.discord_admin_state.safety.as_mut() {
                                if m.busy || m.loading || m.draft_read_only || !m.creating {
                                    return;
                                }
                                m.draft_trigger = code;
                                if code != 1 && code != 5 {
                                    m.draft_timeout = false;
                                }
                                if code == 6 {
                                    m.draft_block = false;
                                } else {
                                    m.draft_quarantine = false;
                                }
                                cx.notify();
                            }
                        },
                    )),
                );
            }
        }
        let mut presets = segmented().flex_wrap();
        for (ix, label) in PRESET_LABELS.iter().enumerate() {
            let on = modal.draft_presets[ix];
            presets = presets.child(
                segment(SharedString::from(format!("safety-preset-{ix}")), *label, on).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(m) = this.discord_admin_state.safety.as_mut() {
                        if m.busy || m.loading || m.draft_read_only {
                            return;
                        }
                        m.draft_presets[ix] = !m.draft_presets[ix];
                        cx.notify();
                    }
                })),
            );
        }

        let editor = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().type_small().text_color(theme::text()).child(if modal.creating { "New rule" } else { "Edit rule" }))
            .child(field_box("Name", modal.name_input.clone(), window, cx))
            .child(toggle_line(cx, "safety-enabled", "Enabled", modal.draft_enabled, |m| &mut m.draft_enabled))
            .when(modal.creating, |col| col.child(div().flex().child(triggers)))
            .when(!modal.creating, |col| col.child(div().type_small().text_color(theme::text_faint()).child(trigger_label)))
            .when(modal.draft_trigger == 1 || modal.draft_trigger == 6, |col| {
                col.child(field_box("Keywords", modal.words.clone(), window, cx))
                    .child(field_box("Patterns", modal.patterns.clone(), window, cx))
                    .child(field_box("Allow list", modal.allow.clone(), window, cx))
            })
            .when(modal.draft_trigger == 4, |col| {
                col.child(div().flex().child(presets))
                    .child(field_box("Allow list", modal.allow.clone(), window, cx))
            })
            .when(modal.draft_trigger == 5, |col| {
                col.child(field_box("Mention limit", modal.mention_limit.clone(), window, cx))
                    .child(toggle_line(cx, "safety-raid", "Mention raid protection", modal.draft_raid, |m| &mut m.draft_raid))
            })
            .child(div().type_micro().text_color(theme::text_faint()).child("Actions"))
            .when(modal.draft_trigger != 6, |col| {
                col.child(toggle_line(cx, "safety-block", "Block message", modal.draft_block, |m| &mut m.draft_block)).when(modal.draft_block, |col| {
                    col.child(field_box("Block message", modal.block_message.clone(), window, cx))
                        .child(div().type_micro().text_color(theme::text_faint()).child("Shown to the member. Not a command."))
                })
            })
            .child(toggle_line(cx, "safety-alert", "Alert a channel", modal.draft_alert, |m| &mut m.draft_alert))
            .when(modal.draft_alert, |col| col.child(field_box("Alert channel", modal.alert_channel.clone(), window, cx)))
            .when(can_timeout, |col| col.child(toggle_line(cx, "safety-timeout", "Time the member out", modal.draft_timeout, |m| &mut m.draft_timeout)))
            .when(can_timeout && modal.draft_timeout, |col| col.child(field_box("Timeout seconds", modal.timeout_seconds.clone(), window, cx)))
            .when(modal.draft_trigger == 6, |col| col.child(toggle_line(cx, "safety-quarantine", "Block member", modal.draft_quarantine, |m| &mut m.draft_quarantine)))
            .child(field_box("Exempt roles", modal.exempt_roles.clone(), window, cx))
            .child(field_box("Exempt channels", modal.exempt_channels.clone(), window, cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(8.))
                    .when(!modal.draft_id.is_empty(), |row| {
                        row.child(if busy {
                            button_disabled("Deleting…").into_any_element()
                        } else {
                            button("safety-delete-rule", ButtonKind::Danger, "Delete rule")
                                .on_click(cx.listener(|this, _, _, cx| this.delete_selected_rule(cx)))
                                .into_any_element()
                        })
                    })
                    .child(if busy {
                        button_disabled("Saving…").into_any_element()
                    } else {
                        button("safety-save-rule", ButtonKind::Primary, "Save rule")
                            .on_click(cx.listener(|this, _, _, cx| this.save_selected_rule(cx)))
                            .into_any_element()
                    }),
            );

        let card = div()
            .id("discord-safety-card")
            .w(px(720.))
            .max_w(px(max_w))
            .max_h(px(720.0_f32.min(max_h)))
            .p(px(16.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().truncate().type_title().text_color(theme::text()).child(title))
                    .child(
                        icon_button("discord-safety-close", "icons/x.svg", false)
                            .aria_label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.discord_admin_state.safety = None;
                                cx.notify();
                            })),
                    ),
            )
            .children(modal.error.as_ref().map(|error| div().type_small().text_color(theme::danger()).child(error.clone())))
            .child(
                div()
                    .id("discord-admin-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .when(loading && !loaded, |col| col.child(div().py(px(12.)).type_small().text_color(theme::text_soft()).child("Loading safety setup…")))
                    .child(div().type_small().text_color(theme::text()).child("Verification"))
                    .child(div().flex().child(verification))
                    .child(div().type_small().text_color(theme::text()).child("Explicit content"))
                    .child(div().flex().child(explicit))
                    .child(div().type_small().text_color(theme::text()).child("Pause invites"))
                    .child(div().flex().child(invites))
                    .child(div().type_small().text_color(theme::text()).child("Pause direct messages"))
                    .child(div().flex().child(dms))
                    .child(
                        div().flex().justify_end().child(if busy {
                            button_disabled("Saving…").into_any_element()
                        } else if loaded && !loading {
                            button("safety-save-levels", ButtonKind::Primary, "Save safety")
                                .on_click(cx.listener(|this, _, _, cx| this.save_safety_levels(cx)))
                                .into_any_element()
                        } else {
                            button_disabled("Save safety").into_any_element()
                        }),
                    )
                    .child(div().type_small().text_color(theme::text()).child("AutoMod rules"))
                    .when(loaded, |col| {
                        col.child(button("safety-new-rule", ButtonKind::Secondary, "New rule").on_click(cx.listener(|this, _, _, cx| this.new_automod_rule(cx))))
                    })
                    .when(loaded && modal.rules.is_empty(), |col| col.child(div().type_small().text_color(theme::text_soft()).child("No AutoMod rules yet.")))
                    .children(rows)
                    .when(read_only, |col| {
                        col.child(
                            div()
                                .type_small()
                                .text_color(theme::text_soft())
                                .child("This rule uses a setup tinyclient cannot edit, so it is left unchanged."),
                        )
                    })
                    .when(show_editor, |col| col.child(editor))
                    .when(loaded && !show_editor && !read_only, |col| {
                        col.child(div().type_small().text_color(theme::text_faint()).child("Select a rule, or add one."))
                    }),
            );

        Some(
            deferred(
                div()
                    .id("discord-safety-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(theme::scrim())
                    .occlude()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.discord_admin_state.safety = None;
                        cx.notify();
                    }))
                    .child(card),
            )
            .priority(Layer::Modal as usize)
            .into_any_element(),
        )
    }
}

fn toggle_line(
    cx: &mut Context<Shell>,
    id: &'static str,
    label: &'static str,
    on: bool,
    slot: fn(&mut SafetyModal) -> &mut bool,
) -> Stateful<Div> {
    let kind = if on { ButtonKind::Primary } else { ButtonKind::Ghost };
    button(id, kind, label).on_click(cx.listener(move |this, _, _, cx| {
        if let Some(modal) = this.discord_admin_state.safety.as_mut() {
            if modal.busy || modal.loading || modal.draft_read_only {
                return;
            }
            let flag = slot(modal);
            *flag = !*flag;
            cx.notify();
        }
    }))
}

fn field_box(label: &str, input: Entity<TextInput>, window: &gpui::Window, cx: &Context<Shell>) -> Div {
    let focused = gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window);
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().type_micro().text_color(theme::text_faint()).child(label.to_owned()))
        .child(super::components::field(focused).child(div().flex_1().min_w_0().child(input)))
}

fn blank_rule() -> AutomodRule {
    AutomodRule {
        enabled: true,
        event_type: 1,
        trigger: AutomodTrigger::Keyword { keywords: Vec::new(), regex: Vec::new(), allow: Vec::new() },
        actions: vec![AutomodAction::Block { message: String::new() }],
        ..AutomodRule::default()
    }
}

fn trigger_name(trigger: &AutomodTrigger) -> String {
    match trigger {
        AutomodTrigger::Keyword { .. } => "Keywords".to_owned(),
        AutomodTrigger::Spam => "Spam".to_owned(),
        AutomodTrigger::Preset { .. } => "Presets".to_owned(),
        AutomodTrigger::Mention { .. } => "Mentions".to_owned(),
        AutomodTrigger::Profile { .. } => "Profile".to_owned(),
        AutomodTrigger::Other { code, .. } => format!("Trigger {code}"),
    }
}

fn trigger_code_of(trigger: &AutomodTrigger) -> u8 {
    match trigger {
        AutomodTrigger::Keyword { .. } => 1,
        AutomodTrigger::Spam => 3,
        AutomodTrigger::Preset { .. } => 4,
        AutomodTrigger::Mention { .. } => 5,
        AutomodTrigger::Profile { .. } => 6,
        AutomodTrigger::Other { code, .. } => *code,
    }
}

fn rule_words(trigger: &AutomodTrigger) -> (String, String, String, String, bool) {
    match trigger {
        AutomodTrigger::Keyword { keywords, regex, allow } | AutomodTrigger::Profile { keywords, regex, allow } => {
            (join_list(keywords), join_list(regex), join_list(allow), "5".to_owned(), false)
        }
        AutomodTrigger::Preset { allow, .. } => (String::new(), String::new(), join_list(allow), "5".to_owned(), false),
        AutomodTrigger::Mention { limit, raid_protection } => {
            let mention = if *limit == 0 { "5".to_owned() } else { limit.to_string() };
            (String::new(), String::new(), String::new(), mention, *raid_protection)
        }
        _ => (String::new(), String::new(), String::new(), "5".to_owned(), false),
    }
}

fn rule_presets(trigger: &AutomodTrigger) -> [bool; 3] {
    let mut on = [false; 3];
    if let AutomodTrigger::Preset { presets, .. } = trigger {
        for code in presets {
            if (1..=3).contains(code) {
                on[*code as usize - 1] = true;
            }
        }
    }
    on
}

fn rule_actions(rule: &AutomodRule) -> (bool, String, bool, String, bool, String, bool) {
    let mut block = false;
    let mut block_message = String::new();
    let mut alert = false;
    let mut alert_channel = String::new();
    let mut timeout = false;
    let mut timeout_seconds = "60".to_owned();
    let mut quarantine = false;
    for action in &rule.actions {
        match action {
            AutomodAction::Block { message } => {
                block = true;
                block_message = message.clone();
            }
            AutomodAction::Alert { channel_id } => {
                alert = true;
                alert_channel = channel_id.clone();
            }
            AutomodAction::Timeout { seconds } => {
                timeout = true;
                timeout_seconds = seconds.to_string();
            }
            AutomodAction::Quarantine => quarantine = true,
            AutomodAction::Other { .. } => {}
        }
    }
    (block, block_message, alert, alert_channel, timeout, timeout_seconds, quarantine)
}

fn trigger_from(kind: u8, words: &str, patterns: &str, allow: &str, presets: [bool; 3], mention: &str, raid: bool) -> AutomodTrigger {
    match kind {
        3 => AutomodTrigger::Spam,
        4 => AutomodTrigger::Preset {
            presets: presets.iter().enumerate().filter(|(_, on)| **on).map(|(ix, _)| ix as u8 + 1).collect(),
            allow: split_list(allow),
        },
        5 => AutomodTrigger::Mention { limit: mention.trim().parse().unwrap_or(0), raid_protection: raid },
        6 => AutomodTrigger::Profile { keywords: split_list(words), regex: split_list(patterns), allow: split_list(allow) },
        _ => AutomodTrigger::Keyword { keywords: split_list(words), regex: split_list(patterns), allow: split_list(allow) },
    }
}

fn actions_from(block: bool, message: &str, alert: bool, channel: &str, timeout: bool, seconds: &str, quarantine: bool) -> Vec<AutomodAction> {
    let mut actions = Vec::new();
    if block {
        actions.push(AutomodAction::Block { message: message.trim().to_owned() });
    }
    if alert {
        actions.push(AutomodAction::Alert { channel_id: channel.trim().to_owned() });
    }
    if timeout {
        actions.push(AutomodAction::Timeout { seconds: seconds.trim().parse().unwrap_or(0) });
    }
    if quarantine {
        actions.push(AutomodAction::Quarantine);
    }
    actions
}

fn split_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c == '\n').map(str::trim).filter(|part| !part.is_empty()).map(str::to_owned).collect()
}

fn join_list(items: &[String]) -> String {
    items.join(", ")
}

fn pause_choice(until: i64, now: i64) -> i64 {
    if until <= now {
        0
    } else if until - now > 6 * 3600 {
        24 * 3600
    } else {
        3600
    }
}

fn audit_change_line(change: &protocol::AuditChange) -> String {
    match (&change.old, &change.new) {
        (Some(old), Some(new)) => format!("{}: {old} → {new}", change.key),
        (None, Some(new)) => format!("{}: {new}", change.key),
        (Some(old), None) => format!("{} removed ({old})", change.key),
        (None, None) => change.key.clone(),
    }
}
