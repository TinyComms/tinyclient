//! Roles: the server's roles in their order, most senior first, with @everyone at the foot.
//! Drag one to change who outranks whom (`TinyRoleOrder`); pick one to edit it on the right:
//! its name, colour (a palette or a hex code), whether it shows separately in the member list,
//! and its permissions in five groups, each with a line saying what it lets people do.
//!
//! Only roles below this account's own rank can be changed or moved, and only permissions this
//! account holds itself can be given (owners hold them all). @everyone keeps its name and place
//! and can't be deleted; only its permissions change. Creating and deleting a role, and the
//! order, go to the server at once; the rest waits for Save.

use super::kit::{self, card, swatch, switch_row};
use super::{page_head, section};
use crate::gui::components::{button, button_disabled, button_with_icon, glyph, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::{Dragged, Modal, Page as Facts};
use gpui::{div, prelude::*, px, AnyElement, App, Context, SharedString, Window};
use protocol::*;

/// The permissions, grouped as the page shows them: a name and what it lets people do.
pub(in crate::gui) const GROUPS: [(&str, &[(u64, &str, &str)]); 5] = [
    (
        "General",
        &[
            (TINY_VIEW_CHANNEL, "View channels", "See the voice channels and who is sitting in them."),
            (TINY_MANAGE_CHANNELS, "Manage channels", "Create, rename, reorder and delete channels and categories."),
            (TINY_VIEW_AUDIT_LOG, "View audit log", "Read who changed what on the server, and why."),
        ],
    ),
    (
        "Membership",
        &[
            (TINY_CREATE_INVITES, "Create invites", "Make invite codes for new people, and revoke their own."),
            (TINY_MANAGE_INVITES, "Manage invites", "See and revoke everyone's invites."),
            (TINY_KICK_MEMBERS, "Kick members", "Remove people from the server. They can come back with a new invite."),
            (TINY_BAN_MEMBERS, "Ban members", "Remove people and keep them out, for a while or for good."),
            (TINY_TIMEOUT_MEMBERS, "Time out members", "Keep someone out of calls for a while."),
        ],
    ),
    (
        "Voice",
        &[
            (TINY_CONNECT, "Connect", "Join voice channels."),
            (TINY_SPEAK, "Speak", "Be heard in a call. Without it they can only listen."),
            (TINY_VOICE_ACTIVITY, "Use voice activity", "Talk without holding a key. Without it, push to talk only."),
            (TINY_VIDEO, "Video", "Turn their camera on in a call."),
            (TINY_SHARE_SCREEN, "Share screen", "Show their screen in a call."),
            (TINY_PRIORITY_SPEAKER, "Priority speaker", "Hold a key to be heard over everyone else."),
        ],
    ),
    (
        "Moderation",
        &[
            (TINY_MUTE_MEMBERS, "Mute members", "Silence someone in a call, for everyone."),
            (TINY_DEAFEN_MEMBERS, "Deafen members", "Stop someone hearing a call."),
            (TINY_MOVE_MEMBERS, "Move members", "Move people from one channel to another."),
            (TINY_DISCONNECT_MEMBERS, "Disconnect members", "Take someone out of a call."),
        ],
    ),
    (
        "Advanced",
        &[
            (TINY_MANAGE_ROLES, "Manage roles", "Create and edit roles below their own, and give them to people."),
            (TINY_MANAGE_SERVER, "Manage server", "Change the server's name, icon and welcome, and where people land."),
        ],
    ),
];

/// A permission's name and help, by its bit.
pub(in crate::gui) fn permission(bit: u64) -> (&'static str, &'static str) {
    GROUPS.iter().flat_map(|(_, rows)| rows.iter()).find(|(of, _, _)| *of == bit).map_or(("", ""), |(_, name, help)| (*name, *help))
}

/// The colours offered beside the hex field.
pub(in crate::gui) const PALETTE: [u32; 10] = [0x1ABC9C, 0x3FBF7A, 0x3B82F6, 0xA97CF8, 0xE91E63, 0xE5484D, 0xE67E22, 0xF1C40F, 0x95A5A6, 0x607D8B];

/// What has changed on the role being edited and not been saved: the switches. The name and
/// colour are in their fields. `None` is as the server has it.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    /// The role being edited.
    pub selected: Option<String>,
    pub hoist: Option<bool>,
    pub permissions: Option<u64>,
    /// A role was just asked for: the roles there were then, so the new one is picked when it
    /// arrives.
    pub creating: Option<Vec<String>>,
}

/// The roles most senior first, @everyone last.
pub(in crate::gui) fn ordered(community: &TinyCommunity) -> Vec<&TinyRole> {
    let mut roles: Vec<&TinyRole> = community.roles.iter().collect();
    roles.sort_by_key(|role| (role.default, std::cmp::Reverse(role.position)));
    roles
}

/// A colour as typed, as the server keeps it: `#RRGGBB` in capitals, or empty for none. `None`
/// when it is not a colour.
pub(in crate::gui) fn color_text(typed: &str) -> Option<String> {
    let digits = typed.trim().trim_start_matches('#');
    if digits.is_empty() {
        return Some(String::new());
    }
    (digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", digits.to_ascii_uppercase()))
}

/// The edit Save sends: only what differs from the server. @everyone keeps its name and colour.
pub(in crate::gui) fn edit(role: &TinyRole, draft: &Draft, name: &str, color: &str) -> TinyRoleEdit {
    let now_color = role.color.as_deref().map(str::to_ascii_uppercase).unwrap_or_default();
    let typed_color = color_text(color).unwrap_or_else(|| color.trim().to_owned());
    TinyRoleEdit {
        name: (!role.default && name.trim() != role.name).then(|| name.trim().to_owned()),
        color: (!role.default && typed_color != now_color).then_some(typed_color),
        permissions: draft.permissions.filter(|bits| *bits != role.permissions),
        hoist: draft.hoist.filter(|hoist| *hoist != role.hoist && !role.default),
    }
}

/// The order after dropping `moved` on `onto`: it takes that row's place, below it when it came
/// from above.
pub(in crate::gui) fn reorder(order: &[String], moved: &str, onto: &str) -> Vec<String> {
    let (Some(from), Some(to)) = (order.iter().position(|id| id == moved), order.iter().position(|id| id == onto)) else {
        return order.to_vec();
    };
    let mut ids = order.to_vec();
    let id = ids.remove(from);
    ids.insert(to, id);
    ids
}

/// Whether this account may change `role` (it is below its own rank).
pub(in crate::gui) fn editable(community: &TinyCommunity, role: &TinyRole) -> bool {
    community.access.as_ref().is_some_and(|access| access.may_assign(role)) && community.allowed("community.roles.manage")
}

/// Whether this account may give `bit`: only what it holds itself, unless it is an owner.
pub(in crate::gui) fn grantable(community: &TinyCommunity, bit: u64) -> bool {
    community.access.as_ref().is_some_and(|access| access.owner || access.has(bit))
}

impl Shell {
    fn tiny_roles_role(&self) -> Option<TinyRole> {
        let id = self.tiny_state.settings.as_ref()?.roles.selected.clone()?;
        self.tiny_state.community.as_ref()?.roles.iter().find(|role| role.id == id).cloned()
    }

    fn tiny_roles_edit(&self, cx: &App) -> Option<TinyRoleEdit> {
        let role = self.tiny_roles_role()?;
        let overlay = self.tiny_state.settings.as_ref()?;
        let fields = &self.tiny_state.settings_fields;
        Some(edit(&role, &overlay.roles, fields.role_name.read(cx).text(), fields.role_color.read(cx).text()))
    }

    pub(in crate::gui) fn tiny_roles_dirty(&self, cx: &App) -> bool {
        self.tiny_roles_edit(cx).is_some_and(|edit| edit != TinyRoleEdit::default())
    }

    pub(in crate::gui) fn tiny_roles_save(&mut self, cx: &mut Context<Self>) {
        let (Some(edit), Some(role), Some(account)) = (self.tiny_roles_edit(cx), self.tiny_roles_role(), self.tiny_state.account.clone()) else {
            return;
        };
        let refused = if edit.name.as_deref().is_some_and(|name| name.is_empty() || name.chars().count() > TINY_NAME_MAX) {
            Some("A role's name is 1 to 80 characters.")
        } else if edit.color.as_deref().is_some_and(|color| color_text(color).is_none()) {
            Some("A colour is six hex digits, as #3B82F6.")
        } else {
            None
        };
        if let Some(words) = refused {
            if let Some(overlay) = self.tiny_state.settings.as_mut() {
                overlay.error = Some(words.to_owned());
            }
            cx.notify();
            return;
        }
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.saving = true;
        }
        self.act(Request::TinyRoleUpdate { account, role_id: role.id, role: edit }, cx);
    }

    /// The fields back to the role as the server has it, and the switches with them.
    pub(in crate::gui) fn tiny_roles_reset(&mut self, cx: &mut Context<Self>) {
        let role = self.tiny_roles_role();
        let fields = &self.tiny_state.settings_fields;
        let (name, color) = (fields.role_name.clone(), fields.role_color.clone());
        let (name_now, color_now) = role.map(|role| (role.name, role.color.unwrap_or_default().to_ascii_uppercase())).unwrap_or_default();
        name.update(cx, |input, cx| input.set_text(&name_now, cx));
        color.update(cx, |input, cx| input.set_text(&color_now, cx));
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.roles.hoist = None;
            overlay.roles.permissions = None;
        }
    }

    pub(in crate::gui) fn tiny_roles_select(&mut self, role_id: Option<String>, cx: &mut Context<Self>) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.roles.selected = role_id;
            overlay.error = None;
        }
        self.tiny_roles_reset(cx);
        cx.notify();
    }

    /// A fresh read of the server: a role just made is picked, a role gone is let go of, and
    /// with nothing picked the most senior role this account may change is.
    pub(in crate::gui) fn tiny_roles_refreshed(&mut self, cx: &mut Context<Self>) {
        let (Some(community), Some(overlay)) = (self.tiny_state.community.as_ref(), self.tiny_state.settings.as_ref()) else {
            return;
        };
        let draft = &overlay.roles;
        let made = draft.creating.as_ref().and_then(|before| community.roles.iter().find(|role| !before.contains(&role.id)).map(|role| role.id.clone()));
        let kept = draft.selected.as_ref().filter(|id| community.roles.iter().any(|role| role.id == **id)).cloned();
        let first = || ordered(community).into_iter().find(|role| editable(community, role)).or_else(|| ordered(community).into_iter().next()).map(|role| role.id.clone());
        let pick = made.clone().or_else(|| kept.clone()).or_else(first);
        let changed = pick != draft.selected || made.is_some();
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            if made.is_some() {
                overlay.roles.creating = None;
            }
        }
        if changed {
            self.tiny_roles_select(pick, cx);
        }
    }

    fn tiny_roles_draft(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Draft)) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            change(&mut overlay.roles);
            overlay.saved = false;
        }
        cx.notify();
    }

    fn tiny_roles_create(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let before = self.tiny_state.community.as_ref().map(|community| community.roles.iter().map(|role| role.id.clone()).collect());
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.roles.creating = before;
        }
        self.act(Request::TinyRoleCreate { account, role: TinyRoleEdit { name: Some("New role".to_owned()), ..TinyRoleEdit::default() } }, cx);
    }

    /// A role dropped on another: the new order, drawn at once and sent whole.
    pub(in crate::gui) fn tiny_roles_dropped(&mut self, moved: &str, onto: &str, cx: &mut Context<Self>) {
        let (Some(account), Some(community)) = (self.tiny_state.account.clone(), self.tiny_state.community.as_mut()) else {
            return;
        };
        let movable = |id: &str| community.roles.iter().find(|role| role.id == id).is_some_and(|role| !role.default && editable(community, role));
        if moved == onto || !movable(moved) || !movable(onto) {
            return;
        }
        let order: Vec<String> = ordered(community).into_iter().filter(|role| !role.default).map(|role| role.id.clone()).collect();
        let role_ids = reorder(&order, moved, onto);
        let top = role_ids.len() as i32;
        for (ix, id) in role_ids.iter().enumerate() {
            if let Some(role) = community.roles.iter_mut().find(|role| role.id == *id) {
                role.position = top - ix as i32;
            }
        }
        self.act(Request::TinyRoleOrder { account, role_ids }, cx);
    }
}

pub(in crate::gui::tiny) fn roles(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.roles;
    let mut page = div().flex().flex_col().gap(px(24.)).child(page_head("Roles", "Who can do what. A role higher in the list outranks those below it: drag to change the order."));
    if !community.has(TINY_FEATURE_ROLES) {
        return page
            .child(kit::note("icons/info.svg", theme::text_faint(), "This server is older than roles: people are owners, admins or members. Update tinyserver for roles of your own."))
            .into_any_element();
    }

    // The list, most senior first.
    let mut list = div().w(px(232.)).flex_none().flex().flex_col().gap(px(2.));
    list = list.child(div().pb(px(8.)).child(if shell.tiny_state.busy {
        button_disabled("Create role").w_full().into_any_element()
    } else {
        button_with_icon("tiny-role-create", ButtonKind::Primary, "icons/plus.svg", "Create role").w_full().on_click(cx.listener(|this, _, _, cx| this.tiny_roles_create(cx))).into_any_element()
    }));
    for (ix, role) in ordered(community).into_iter().enumerate() {
        let on = draft.selected.as_deref() == Some(role.id.as_str());
        let mine = editable(community, role);
        let id = role.id.clone();
        let dot = role.color.as_deref().and_then(model::hex).map(gpui::rgb).unwrap_or(theme::text_faint());
        let mut row = div()
            .id(("tiny-role-row", ix))
            .h(px(36.))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .when(on, |row| row.bg(theme::selected()))
            .when(!on, |row| row.hover(|style| style.bg(theme::hover())))
            .on_click(cx.listener(move |this, _, _, cx| this.tiny_roles_select(Some(id.clone()), cx)))
            .child(div().w(px(14.)).flex_none().children((!role.default && mine).then(|| glyph("icons/grip-vertical.svg", 14., theme::text_faint()))))
            .child(div().flex_none().size(px(10.)).rounded_full().bg(dot))
            .child(div().flex_1().min_w_0().truncate().type_ui().text_color(if on { theme::text() } else { theme::text_soft() }).child(role.name.clone()))
            .when(!mine, |row| row.child(glyph("icons/lock.svg", 12., theme::text_faint())))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(glyph("icons/user.svg", 12., theme::text_faint()))
                    .child(div().type_small().text_color(theme::text_faint()).child(role.member_count.to_string())),
            );
        if !role.default && mine {
            let target = role.id.clone();
            row = row
                .on_drag(Dragged::Role { role_id: role.id.clone(), name: role.name.clone() }, |dragged, _, _, cx| cx.new(|_| crate::gui::tiny::column::Ghost(dragged.label())))
                .drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft()))
                .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| {
                    if let Dragged::Role { role_id, .. } = dragged {
                        this.tiny_roles_dropped(role_id, &target, cx);
                    }
                }));
        }
        if role.default {
            list = list.child(div().my(px(6.)).h(px(1.)).bg(theme::hairline()));
        }
        list = list.child(row);
    }
    list = list.child(div().pt(px(10.)).whitespace_normal().type_small().text_color(theme::text_faint()).child("You can change and move only the roles below your own."));

    let editor = match shell.tiny_roles_role() {
        Some(role) => editor(shell, facts, &role, window, cx).into_any_element(),
        None => kit::empty("icons/shield.svg", "Pick a role to edit it.").into_any_element(),
    };
    page = page.child(div().flex().items_start().gap(px(28.)).child(list).child(div().flex_1().min_w_0().child(editor)));
    page.into_any_element()
}

fn editor(shell: &Shell, facts: &Facts, role: &TinyRole, window: &mut Window, cx: &mut Context<Shell>) -> gpui::Div {
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div();
    };
    let draft = &overlay.roles;
    let fields = &shell.tiny_state.settings_fields;
    let mine = editable(community, role);
    let mut column = div().flex().flex_col().gap(px(24.));
    let typed_color = color_text(fields.role_color.read(cx).text()).unwrap_or_default();
    let dot = model::hex(&typed_color).map(gpui::rgb).unwrap_or(theme::text_faint());
    column = column.child(
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().size(px(14.)).rounded_full().bg(dot))
            .child(div().type_title().text_color(theme::text()).child(if role.default { role.name.clone() } else { fields.role_name.read(cx).text().to_owned() }))
            .child(kit::count(role.member_count as usize))
            .child(div().type_small().text_color(theme::text_faint()).child(if role.member_count == 1 { "member" } else { "members" })),
    );
    if role.default {
        column = column.child(kit::note("icons/users.svg", theme::text_faint(), "Everyone on the server has @everyone. What it allows, every member can do; the roles above add to it."));
    }
    if !mine {
        column = column.child(kit::note("icons/lock.svg", theme::warning(), "This role is at or above your own, so you can see it but not change it."));
    }
    if !role.default {
        column = column.child(if mine {
            section("Role name").child(kit::input(&fields.role_name, Some(360.), window, cx))
        } else {
            section("Role name").child(div().type_ui().text_color(theme::text()).child(role.name.clone()))
        });
        let mut swatches = div().flex().flex_wrap().items_center().gap(px(4.));
        let pick = |this: &mut Shell, text: String, cx: &mut Context<Shell>| {
            let color = this.tiny_state.settings_fields.role_color.clone();
            color.update(cx, |input, cx| input.set_text(&text, cx));
            if let Some(overlay) = this.tiny_state.settings.as_mut() {
                overlay.saved = false;
            }
            cx.notify();
        };
        swatches = swatches.child(swatch("tiny-role-color-none", None, typed_color.is_empty()).when(mine, |swatch| swatch.on_click(cx.listener(move |this, _, _, cx| pick(this, String::new(), cx)))));
        for (ix, color) in PALETTE.iter().enumerate() {
            let text = format!("#{color:06X}");
            let on = typed_color == text;
            swatches = swatches.child(swatch(("tiny-role-color", ix), Some(*color), on).when(mine, |swatch| swatch.on_click(cx.listener(move |this, _, _, cx| pick(this, text.clone(), cx)))));
        }
        let hex = mine.then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(kit::input(&fields.role_color, Some(120.), window, cx))
                .child(div().type_small().text_color(theme::text_faint()).child("or any colour as a hex code"))
        });
        column = column.child(
            section("Colour")
                .child(swatches)
                .children(hex)
                .child(div().type_small().text_color(theme::text_faint()).child("Names of people with this role are drawn in its colour, from their highest role that has one.")),
        );
        let hoist = draft.hoist.unwrap_or(role.hoist);
        column = column.child(
            card().child(
                switch_row("tiny-role-hoist", "Show separately in the member list", "People with this role get their own group, above everyone else online.", hoist, mine)
                    .border_b_0()
                    .when(mine, |row| row.on_click(cx.listener(move |this, _, _, cx| this.tiny_roles_draft(cx, |draft| draft.hoist = Some(!hoist))))),
            ),
        );
    }
    let bits = draft.permissions.unwrap_or(role.permissions);
    for (group, rows) in GROUPS {
        let mut box_ = card();
        for (ix, (bit, name, help)) in rows.iter().enumerate() {
            let bit = *bit;
            let on = bits & bit != 0;
            let can = mine && grantable(community, bit);
            box_ = box_.child(
                switch_row(SharedString::from(format!("tiny-role-perm-{group}-{ix}")), name, help, on, can)
                    .when(ix + 1 == rows.len(), |row| row.border_b_0())
                    .when(can, |row| row.on_click(cx.listener(move |this, _, _, cx| this.tiny_roles_draft(cx, |draft| draft.permissions = Some(bits ^ bit))))),
            );
        }
        column = column.child(section(group).child(box_));
    }
    if mine && !role.default {
        let id = role.id.clone();
        column = column.child(div().h(px(1.)).bg(theme::hairline())).child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_1().min_w_0().whitespace_normal().type_small().text_color(theme::text_soft()).child("Deleting a role takes it from everyone who has it."))
                .child(button("tiny-role-delete", ButtonKind::Danger, "Delete role").on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::DeleteRole(id.clone()), window, cx)))),
        );
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(id: &str, position: i32, default: bool) -> TinyRole {
        TinyRole { id: id.into(), name: id.into(), position, default, permissions: TINY_EVERYONE_DEFAULT, color: Some("#3b82f6".into()), ..TinyRole::default() }
    }

    #[test]
    fn every_permission_is_in_one_group_with_words() {
        let mut seen = 0u64;
        for (_, rows) in GROUPS {
            for (bit, name, help) in rows.iter() {
                assert_eq!(seen & bit, 0, "{name} is listed twice");
                assert!(!name.is_empty() && help.ends_with('.'));
                seen |= bit;
            }
        }
        assert_eq!(seen, TINY_ALL);
        assert_eq!(permission(TINY_SPEAK).0, "Speak");
    }

    #[test]
    fn a_role_edit_sends_only_what_changed_and_everyone_keeps_its_name() {
        let admin = role("Admin", 2, false);
        assert_eq!(edit(&admin, &Draft::default(), " Admin ", "#3B82F6"), TinyRoleEdit::default(), "the same colour in other case is no change");
        let draft = Draft { hoist: Some(true), permissions: Some(TINY_ALL), ..Draft::default() };
        let sent = edit(&admin, &draft, "Admins", "");
        assert_eq!(sent.name.as_deref(), Some("Admins"));
        assert_eq!(sent.color.as_deref(), Some(""), "no colour clears it");
        assert_eq!(sent.hoist, Some(true));
        assert_eq!(sent.permissions, Some(TINY_ALL));
        let everyone = role("@everyone", 0, true);
        let sent = edit(&everyone, &draft, "Everybody", "#FFFFFF");
        assert_eq!((sent.name, sent.color, sent.hoist), (None, None, None));
        assert_eq!(sent.permissions, Some(TINY_ALL));
        assert_eq!(color_text("abcdef").as_deref(), Some("#ABCDEF"));
        assert_eq!(color_text("#12345"), None);
        assert_eq!(edit(&admin, &Draft::default(), "Admin", "red").color.as_deref(), Some("red"), "a bad colour is sent to the check, not dropped");
    }

    #[test]
    fn a_dropped_role_takes_the_place_of_the_one_it_lands_on() {
        let order = ["a", "b", "c", "d"].map(String::from);
        assert_eq!(reorder(&order, "a", "c"), ["b", "c", "a", "d"]);
        assert_eq!(reorder(&order, "d", "b"), ["a", "d", "b", "c"]);
        assert_eq!(reorder(&order, "a", "x"), order, "an unknown row changes nothing");
    }

    #[test]
    fn only_roles_below_my_rank_and_bits_i_hold_can_be_changed() {
        let mut community: TinyCommunity = serde_json::from_value(serde_json::json!({"role":"admin","members":[],"invites":[],"audit":[]})).unwrap();
        community.capabilities.features = vec!["roles".into()];
        community.roles = vec![role("Owner-ish", 3, false), role("Mod", 1, false), role("everyone", 0, true)];
        community.access = Some(TinyAccess { owner: false, roles: vec![], permissions: TINY_MANAGE_ROLES | TINY_SPEAK, rank: 2 });
        assert!(!editable(&community, &community.roles[0].clone()));
        assert!(editable(&community, &community.roles[1].clone()));
        assert!(editable(&community, &community.roles[2].clone()), "@everyone is below everyone");
        assert!(grantable(&community, TINY_SPEAK) && !grantable(&community, TINY_BAN_MEMBERS));
        community.access = Some(TinyAccess { owner: true, ..TinyAccess::default() });
        assert!(editable(&community, &community.roles[0].clone()) && grantable(&community, TINY_BAN_MEMBERS));
        community.access = Some(TinyAccess { owner: false, roles: vec![], permissions: TINY_SPEAK, rank: 2 });
        assert!(!editable(&community, &community.roles[1].clone()), "without manage_roles nothing is editable");
        assert_eq!(ordered(&community).iter().map(|role| role.id.as_str()).collect::<Vec<_>>(), ["Owner-ish", "Mod", "everyone"]);
    }
}
