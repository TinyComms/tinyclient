//! Channels: the categories and voice channels in one tree, in the server's order. Drag a channel
//! onto another to put it above, onto a category to put it at that category's end; drag a
//! category onto another to put it above (`TinyLayout`, sent at once). Create a channel or a
//! category; pick one to edit it on the right.
//!
//! A channel's editor: name, topic, how many people it holds, its bitrate (16 to 128 kb/s),
//! whether video is on, whether push to talk is required, and per role what it may do here:
//! allow, inherit (from the role's permissions) or deny, for each permission a channel can
//! change. Beside each, what someone with only that role ends up with here. A category's: its
//! name. Delete asks first. Opened from a channel's "Edit channel", that channel is picked.
//!
//! On a server older than categories, a channel has a name and nothing else to edit.

use super::kit::{self, card, switch_row, tri_cell, Tri};
use super::{page_head, section};
use crate::gui::components::{button, button_disabled, button_with_icon, glyph, icon_button, tip, ButtonKind};
use crate::gui::settings::controls::{slider_track, value_at};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model::{self, Line};
use crate::gui::tiny::{Dragged, Modal, Page as Facts};
use gpui::{canvas, div, prelude::*, px, AnyElement, App, Bounds, Context, Div, MouseButton, MouseDownEvent, Pixels, SharedString, Window};
use protocol::*;
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

/// The permissions a channel's override can change, in the order the editor lists them.
pub(in crate::gui) const SCOPED: [u64; 12] = [
    TINY_VIEW_CHANNEL,
    TINY_CONNECT,
    TINY_SPEAK,
    TINY_VOICE_ACTIVITY,
    TINY_VIDEO,
    TINY_SHARE_SCREEN,
    TINY_PRIORITY_SPEAKER,
    TINY_MUTE_MEMBERS,
    TINY_DEAFEN_MEMBERS,
    TINY_MOVE_MEMBERS,
    TINY_DISCONNECT_MEMBERS,
    TINY_MANAGE_CHANNELS,
];

/// The most people a channel can be limited to here.
const LIMIT_MAX: u32 = 99;
const TRACK: f32 = 280.;

/// What is picked in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Sel {
    Room(String),
    Category(String),
}

/// What has changed on the picked channel and not been saved; the name and topic are in their
/// fields. `None` is as the server has it.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub sel: Option<Sel>,
    pub limit: Option<u32>,
    pub bitrate: Option<u32>,
    pub video: Option<bool>,
    pub ptt: Option<bool>,
    /// Overrides changed here, by role: allow and deny.
    pub overrides: BTreeMap<String, (u64, u64)>,
    /// The role whose overrides are shown.
    pub role: Option<String>,
    /// The bitrate track as last drawn, and whether its thumb is held.
    pub track: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub sliding: bool,
}

/// One permission's state in an override.
pub(in crate::gui) fn tri_of((allow, deny): (u64, u64), bit: u64) -> Tri {
    if allow & bit != 0 {
        Tri::Allow
    } else if deny & bit != 0 {
        Tri::Deny
    } else {
        Tri::Inherit
    }
}

/// An override with one permission set to `tri`, the others as they were.
pub(in crate::gui) fn set_tri((allow, deny): (u64, u64), bit: u64, tri: Tri) -> (u64, u64) {
    let (allow, deny) = (allow & !bit, deny & !bit);
    match tri {
        Tri::Allow => (allow | bit, deny),
        Tri::Deny => (allow, deny | bit),
        Tri::Inherit => (allow, deny),
    }
}

/// A role's override in a room: as changed here, else as the server has it, else none.
pub(in crate::gui) fn override_of(room: &TinyRoom, draft: &Draft, role_id: &str) -> (u64, u64) {
    draft.overrides.get(role_id).copied().unwrap_or_else(|| room.overrides.iter().find(|over| over.role_id == role_id).map_or((0, 0), |over| (over.allow, over.deny)))
}

/// The room's overrides with the changes made here.
pub(in crate::gui) fn overrides_with(room: &TinyRoom, draft: &Draft) -> Vec<TinyOverride> {
    let mut all: Vec<TinyOverride> = room.overrides.iter().filter(|over| !draft.overrides.contains_key(&over.role_id)).cloned().collect();
    all.extend(draft.overrides.iter().map(|(role_id, (allow, deny))| TinyOverride { role_id: role_id.clone(), allow: *allow, deny: *deny }));
    all
}

/// What Save sends for the overrides: per role, the new bits, or `None` to clear one that
/// allows and denies nothing. Unchanged roles are left out.
pub(in crate::gui) fn override_changes(room: &TinyRoom, draft: &Draft) -> Vec<(String, Option<(u64, u64)>)> {
    draft
        .overrides
        .iter()
        .filter_map(|(role_id, bits)| {
            let now = room.overrides.iter().find(|over| over.role_id == *role_id).map(|over| (over.allow, over.deny));
            let bits = (bits.0 & TINY_CHANNEL_SCOPED, bits.1 & TINY_CHANNEL_SCOPED);
            match (now, bits) {
                (None, (0, 0)) => None,
                (Some(now), bits) if now == bits => None,
                (Some(_), (0, 0)) => Some((role_id.clone(), None)),
                (_, bits) => Some((role_id.clone(), Some(bits))),
            }
        })
        .collect()
}

/// The room edit Save sends: only what differs from the server.
pub(in crate::gui) fn room_edit(room: &TinyRoom, draft: &Draft, name: &str, topic: &str) -> TinyRoomEdit {
    TinyRoomEdit {
        name: (name.trim() != room.name).then(|| name.trim().to_owned()),
        topic: (topic.trim() != room.topic).then(|| topic.trim().to_owned()),
        user_limit: draft.limit.filter(|limit| *limit != room.user_limit),
        bitrate_kbps: draft.bitrate.filter(|kbps| *kbps != room.bitrate()),
        ptt_only: draft.ptt.filter(|ptt| *ptt != room.ptt_only),
        video_enabled: draft.video.filter(|video| *video != room.video_enabled),
        ..TinyRoomEdit::default()
    }
}

/// What someone holding only `role_id` (and @everyone) may do in the room, overrides included.
pub(in crate::gui) fn effective(community: &TinyCommunity, role_id: &str, overrides: &[TinyOverride]) -> u64 {
    let only: Vec<String> = community.roles.iter().filter(|role| role.id == role_id && !role.default).map(|role| role.id.clone()).collect();
    tiny_room_permissions(&community.roles, &only, false, overrides)
}

impl Shell {
    fn tiny_channels_room(&self) -> Option<TinyRoom> {
        let Some(Sel::Room(id)) = self.tiny_state.settings.as_ref()?.channels.sel.clone() else {
            return None;
        };
        self.tiny_state.community.as_ref()?.rooms.iter().find(|room| room.id == id).cloned()
    }

    fn tiny_channels_category(&self) -> Option<TinyCategory> {
        let Some(Sel::Category(id)) = self.tiny_state.settings.as_ref()?.channels.sel.clone() else {
            return None;
        };
        self.tiny_state.community.as_ref()?.categories.iter().find(|category| category.id == id).cloned()
    }

    /// The requests Save would send.
    fn tiny_channels_requests(&self, cx: &App) -> Vec<Request> {
        let (Some(account), Some(community), Some(overlay)) = (self.tiny_state.account.clone(), self.tiny_state.community.as_ref(), self.tiny_state.settings.as_ref()) else {
            return Vec::new();
        };
        let fields = &self.tiny_state.settings_fields;
        let draft = &overlay.channels;
        let mut out = Vec::new();
        if let Some(category) = self.tiny_channels_category() {
            let name = fields.category_name.read(cx).text().trim().to_owned();
            if name != category.name {
                out.push(Request::TinyCategoryRename { account, category_id: category.id, name });
            }
            return out;
        }
        let Some(room) = self.tiny_channels_room() else {
            return out;
        };
        let edit = room_edit(&room, draft, fields.room_name.read(cx).text(), fields.room_topic.read(cx).text());
        if community.has(TINY_FEATURE_CATEGORIES) {
            if edit != TinyRoomEdit::default() {
                out.push(Request::TinyRoomUpdate { account: account.clone(), room_id: room.id.clone(), room: edit });
            }
        } else if let Some(name) = edit.name {
            out.push(Request::TinyRoomRename { account: account.clone(), room_id: room.id.clone(), name });
        }
        for (role_id, bits) in override_changes(&room, draft) {
            out.push(match bits {
                Some((allow, deny)) => Request::TinyOverrideSet { account: account.clone(), room_id: room.id.clone(), role_id, allow, deny },
                None => Request::TinyOverrideClear { account: account.clone(), room_id: room.id.clone(), role_id },
            });
        }
        out
    }

    pub(in crate::gui) fn tiny_channels_dirty(&self, cx: &App) -> bool {
        !self.tiny_channels_requests(cx).is_empty()
    }

    pub(in crate::gui) fn tiny_channels_save(&mut self, cx: &mut Context<Self>) {
        let requests = self.tiny_channels_requests(cx);
        let named = |name: &str| name.is_empty() || name.chars().count() > TINY_NAME_MAX;
        let bad_name = requests.iter().any(|request| match request {
            Request::TinyCategoryRename { name, .. } | Request::TinyRoomRename { name, .. } => named(name),
            Request::TinyRoomUpdate { room, .. } => room.name.as_deref().is_some_and(named),
            _ => false,
        });
        if bad_name {
            if let Some(overlay) = self.tiny_state.settings.as_mut() {
                overlay.error = Some("A name is 1 to 80 characters.".to_owned());
            }
            cx.notify();
            return;
        }
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.saving = true;
        }
        self.act_all(requests, cx);
    }

    /// The fields back to the picked channel or category, and the draft emptied but for what is
    /// picked.
    pub(in crate::gui) fn tiny_channels_reset(&mut self, cx: &mut Context<Self>) {
        let room = self.tiny_channels_room();
        let category = self.tiny_channels_category();
        let fields = &self.tiny_state.settings_fields;
        let (name, topic, category_name) = (fields.room_name.clone(), fields.room_topic.clone(), fields.category_name.clone());
        let (room_name, room_topic) = room.map(|room| (room.name, room.topic)).unwrap_or_default();
        name.update(cx, |input, cx| input.set_text(&room_name, cx));
        topic.update(cx, |input, cx| input.set_text(&room_topic, cx));
        let category_now = category.map(|category| category.name).unwrap_or_default();
        category_name.update(cx, |input, cx| input.set_text(&category_now, cx));
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            let draft = &mut overlay.channels;
            draft.limit = None;
            draft.bitrate = None;
            draft.video = None;
            draft.ptt = None;
            draft.overrides.clear();
            draft.sliding = false;
        }
    }

    pub(in crate::gui) fn tiny_channels_select(&mut self, sel: Option<Sel>, cx: &mut Context<Self>) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.channels.sel = sel;
            overlay.error = None;
        }
        self.tiny_channels_reset(cx);
        cx.notify();
    }

    /// A fresh read: what was picked and is gone is let go of, and with nothing picked the
    /// channel the page was opened for, else the first.
    pub(in crate::gui) fn tiny_channels_refreshed(&mut self, cx: &mut Context<Self>) {
        let (Some(community), Some(overlay)) = (self.tiny_state.community.as_ref(), self.tiny_state.settings.as_ref()) else {
            return;
        };
        let exists = |sel: &Sel| match sel {
            Sel::Room(id) => community.rooms.iter().any(|room| room.id == *id),
            Sel::Category(id) => community.categories.iter().any(|category| category.id == *id),
        };
        let wanted = overlay.channel.clone().map(Sel::Room).filter(|sel| exists(sel));
        let kept = overlay.channels.sel.clone().filter(|sel| exists(sel));
        let first = || model::lines(community, &HashSet::new(), None).into_iter().find_map(|line| if let Line::Room { id } = line { Some(Sel::Room(id)) } else { None });
        let pick = kept.clone().or(wanted).or_else(first);
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.channel = None;
        }
        if pick != kept || kept.is_none() {
            self.tiny_channels_select(pick, cx);
        }
    }

    fn tiny_channels_draft(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Draft)) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            change(&mut overlay.channels);
            overlay.saved = false;
        }
        cx.notify();
    }

    /// The bitrate's thumb follows the pointer while it is held.
    pub(in crate::gui) fn tiny_channels_slide(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(overlay) = self.tiny_state.settings.as_mut().filter(|overlay| overlay.channels.sliding) else {
            return;
        };
        let Some(bounds) = overlay.channels.track.get() else {
            return;
        };
        let kbps = value_at(x - f32::from(bounds.origin.x), (TINY_BITRATE_MIN, TINY_BITRATE_MAX), 8, f32::from(bounds.size.width));
        overlay.channels.bitrate = Some(kbps);
        overlay.saved = false;
        cx.notify();
    }

    pub(in crate::gui) fn tiny_channels_slide_end(&mut self) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.channels.sliding = false;
        }
    }
}

pub(in crate::gui::tiny) fn channels(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.channels;
    let mut page = div()
        .flex()
        .flex_col()
        .gap(px(24.))
        .child(page_head("Channels", if facts.may.layout { "Voice channels and the categories that hold them. Drag to change the order." } else { "The server's voice channels." }));
    let mut tools = div().flex().items_center().gap(px(8.));
    if facts.may.make_room {
        let category = match &draft.sel {
            Some(Sel::Category(id)) => Some(id.clone()),
            Some(Sel::Room(id)) => community.rooms.iter().find(|room| room.id == *id).and_then(|room| room.category_id.clone()),
            None => None,
        };
        tools = tools.child(
            button_with_icon("tiny-channels-new-room", ButtonKind::Primary, "icons/plus.svg", "Create channel")
                .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::NewRoom(category.clone()), window, cx))),
        );
    }
    if facts.may.categories {
        tools = tools.child(
            button_with_icon("tiny-channels-new-category", ButtonKind::Secondary, "icons/folder-plus.svg", "Create category")
                .on_click(cx.listener(|this, _, window, cx| this.tiny_open_modal(Modal::NewCategory, window, cx))),
        );
    }
    page = page.child(tools);
    let editor = match &draft.sel {
        Some(Sel::Room(_)) => shell.tiny_channels_room().map(|room| room_editor(shell, facts, &room, window, cx).into_any_element()),
        Some(Sel::Category(_)) => shell.tiny_channels_category().map(|category| category_editor(shell, facts, &category, window, cx).into_any_element()),
        None => None,
    }
    .unwrap_or_else(|| kit::empty("icons/volume-2.svg", "Pick a channel or a category to edit it.").into_any_element());
    page.child(div().flex().items_start().gap(px(24.)).child(tree(facts, draft, cx)).child(div().flex_1().min_w_0().child(editor))).into_any_element()
}

/// The categories and rooms, in order, each a drag handle and a drop target where the layout may
/// change.
fn tree(facts: &Facts, draft: &Draft, cx: &mut Context<Shell>) -> Div {
    let community = facts.community;
    let mut tree = div().w(px(232.)).flex_none().flex().flex_col().gap(px(1.));
    let lines = model::lines(community, &HashSet::new(), None);
    let headed_loose = !community.categories.is_empty() && lines.iter().any(|line| matches!(line, Line::Category { id: None, .. }));
    if !community.categories.is_empty() && !headed_loose {
        // The rooms in no category still take a drop.
        tree = tree.child(loose_header(facts, cx));
    }
    for line in lines {
        match line {
            Line::Category { id: None, .. } => tree = tree.child(loose_header(facts, cx)),
            Line::Category { id: Some(id), name, rooms, .. } => {
                let on = draft.sel == Some(Sel::Category(id.clone()));
                let pick = id.clone();
                let mut row = div()
                    .id(SharedString::from(format!("tiny-channels-cat-{id}")))
                    .mt(px(10.))
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .when(on, |row| row.bg(theme::selected()))
                    .when(!on, |row| row.hover(|style| style.bg(theme::hover())))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_channels_select(Some(Sel::Category(pick.clone())), cx)))
                    .when(facts.may.layout, |row| row.child(glyph("icons/grip-vertical.svg", 12., theme::text_faint())))
                    .child(div().flex_1().min_w_0().truncate().type_micro().text_color(if on { theme::text() } else { theme::text_soft() }).child(name.to_uppercase()))
                    .child(div().type_micro().text_color(theme::text_faint()).child(rooms.to_string()));
                if facts.may.layout {
                    let target = id.clone();
                    row = row
                        .on_drag(Dragged::Category { category_id: id.clone(), name: name.clone() }, |dragged, _, _, cx| cx.new(|_| crate::gui::tiny::column::Ghost(dragged.label())))
                        .drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft()))
                        .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| this.tiny_dropped_on_category(Some(target.clone()), dragged, cx)));
                }
                tree = tree.child(row);
            }
            Line::Room { id } => {
                let Some(room) = community.rooms.iter().find(|room| room.id == id) else {
                    continue;
                };
                let on = draft.sel == Some(Sel::Room(id.clone()));
                let pick = id.clone();
                let mut row = div()
                    .id(SharedString::from(format!("tiny-channels-room-{id}")))
                    .h(px(32.))
                    .pl(px(if community.categories.is_empty() { 8. } else { 16. }))
                    .pr(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .when(on, |row| row.bg(theme::selected()))
                    .when(!on, |row| row.hover(|style| style.bg(theme::hover())))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_channels_select(Some(Sel::Room(pick.clone())), cx)))
                    .child(glyph("icons/volume-2.svg", 14., theme::text_faint()))
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(if on { theme::text() } else { theme::text_soft() }).child(room.name.clone()))
                    .when(room.ptt_only, |row| row.child(glyph("icons/radio.svg", 12., theme::text_faint())))
                    .when(!room.overrides.is_empty(), |row| row.child(glyph("icons/lock.svg", 12., theme::text_faint())));
                if facts.may.layout {
                    let target = id.clone();
                    row = row
                        .on_drag(Dragged::Room { room_id: id.clone(), name: room.name.clone() }, |dragged, _, _, cx| cx.new(|_| crate::gui::tiny::column::Ghost(dragged.label())))
                        .drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft()))
                        .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| this.tiny_dropped_on_room(target.clone(), dragged, cx)));
                }
                tree = tree.child(row);
            }
            Line::Seat { .. } => {}
        }
    }
    if community.rooms.is_empty() {
        tree = tree.child(div().px(px(8.)).py(px(8.)).type_small().text_color(theme::text_faint()).child("No channels yet."));
    }
    tree
}

/// The head of the rooms in no category: a place to drop a room out of its category.
fn loose_header(facts: &Facts, cx: &mut Context<Shell>) -> Stateful {
    let mut row = div().id("tiny-channels-loose").h(px(26.)).px(px(8.)).rounded(px(theme::R_CONTROL)).flex().items_center().type_micro().text_color(theme::text_faint()).child("NO CATEGORY");
    if facts.may.layout {
        row = row.drag_over::<Dragged>(|style, _, _, _| style.bg(theme::accent_soft())).on_drop(cx.listener(|this, dragged: &Dragged, _, cx| this.tiny_dropped_on_category(None, dragged, cx)));
    }
    row
}

type Stateful = gpui::Stateful<Div>;

fn category_editor(shell: &Shell, facts: &Facts, category: &TinyCategory, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    let fields = &shell.tiny_state.settings_fields;
    let rooms = facts.community.rooms.iter().filter(|room| room.category_id.as_deref() == Some(category.id.as_str())).count();
    let mut column = div().flex().flex_col().gap(px(24.)).child(
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(glyph("icons/list.svg", 16., theme::text_faint()))
            .child(div().type_title().text_color(theme::text()).child(category.name.clone()))
            .child(kit::count(rooms)),
    );
    column = column.child(if facts.may.categories {
        section("Category name").child(kit::input(&fields.category_name, Some(360.), window, cx))
    } else {
        section("Category name").child(div().type_ui().child(category.name.clone()))
    });
    if facts.may.categories {
        let id = category.id.clone();
        column = column.child(div().h(px(1.)).bg(theme::hairline())).child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_1().min_w_0().whitespace_normal().type_small().text_color(theme::text_soft()).child("Deleting a category keeps its channels; they move to the top, in no category."))
                .child(
                    button("tiny-channels-delete-category", ButtonKind::Danger, "Delete category")
                        .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::DeleteCategory(id.clone()), window, cx))),
                ),
        );
    }
    column
}

fn room_editor(shell: &Shell, facts: &Facts, room: &TinyRoom, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    let community = facts.community;
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div();
    };
    let draft = &overlay.channels;
    let fields = &shell.tiny_state.settings_fields;
    let modern = community.has(TINY_FEATURE_CATEGORIES);
    let edits = modern && community.allowed("community.rooms.edit");
    let renames = facts.may.edit_room;
    let mut column = div().flex().flex_col().gap(px(24.)).child(
        div().flex().items_center().gap(px(10.)).child(glyph("icons/volume-2.svg", 16., theme::text_faint())).child(div().type_title().text_color(theme::text()).child(room.name.clone())).child(
            div().type_small().text_color(theme::text_faint()).child(match room.participants.len() {
                0 => "Nobody in it now".to_owned(),
                1 => "1 person in it now".to_owned(),
                n => format!("{n} people in it now"),
            }),
        ),
    );
    column = column.child(if renames {
        section("Channel name").child(kit::input(&fields.room_name, Some(360.), window, cx))
    } else {
        section("Channel name").child(div().type_ui().child(room.name.clone()))
    });
    if !modern {
        column = column.child(kit::note(
            "icons/info.svg",
            theme::text_faint(),
            "This server is older than channel settings: a channel has a name and nothing more. Update tinyserver for topics, limits, bitrate and permissions.",
        ));
    } else {
        column = column.child(section("Topic").child(if edits {
            kit::input(&fields.room_topic, None, window, cx)
        } else {
            div().type_ui().text_color(theme::text_soft()).child(if room.topic.is_empty() { "No topic".to_owned() } else { room.topic.clone() })
        }));
        // How many may sit in it.
        let limit = draft.limit.unwrap_or(room.user_limit);
        let step = |id: &'static str, icon: &'static str, to: u32, enabled: bool| {
            icon_button(id, icon, false)
                .when(!enabled, |button| button.opacity(0.35))
                .when(enabled, |button| button.on_click(cx.listener(move |this, _, _, cx| this.tiny_channels_draft(cx, |draft| draft.limit = Some(to)))))
        };
        let stepper = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(step("tiny-channels-limit-down", "icons/minus.svg", limit.saturating_sub(1), edits && limit > 0))
            .child(
                div()
                    .w(px(84.))
                    .h(px(32.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::field())
                    .border_1()
                    .border_color(theme::border())
                    .flex()
                    .items_center()
                    .justify_center()
                    .type_ui()
                    .font_weight(theme::MEDIUM)
                    .text_color(theme::text())
                    .child(if limit == 0 { "No limit".to_owned() } else { limit.to_string() }),
            )
            .child(step("tiny-channels-limit-up", "icons/plus.svg", (limit + 1).min(LIMIT_MAX), edits && limit < LIMIT_MAX))
            .when(edits && limit > 0, |row| {
                row.child(button("tiny-channels-limit-off", ButtonKind::Ghost, "No limit").on_click(cx.listener(|this, _, _, cx| this.tiny_channels_draft(cx, |draft| draft.limit = Some(0)))))
            });
        column = column
            .child(section("User limit").child(div().type_small().text_color(theme::text_soft()).child("Once this many are in, Join says it's full. Moving someone in ignores it.")).child(stepper));
        // Bitrate.
        let kbps = draft.bitrate.unwrap_or(room.bitrate());
        let track_at = draft.track.clone();
        let mut track = div()
            .id("tiny-channels-bitrate")
            .relative()
            .child(slider_track(kbps, (TINY_BITRATE_MIN, TINY_BITRATE_MAX), None, Some(TINY_BITRATE_DEFAULT), TRACK))
            .child(canvas(move |bounds, _, _| track_at.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        if edits {
            track = track.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if let Some(overlay) = this.tiny_state.settings.as_mut() {
                        overlay.channels.sliding = true;
                    }
                    this.tiny_channels_slide(f32::from(event.position.x), cx);
                }),
            );
        }
        let quality = match kbps {
            0..=31 => "Speech, on a poor connection",
            32..=63 => "Clear speech",
            64..=95 => "Voice, the default",
            _ => "Music",
        };
        column = column.child(
            section("Bitrate").child(div().type_small().text_color(theme::text_soft()).child("Higher sounds better and takes more of everyone's connection.")).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(track)
                    .child(div().w(px(72.)).type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(format!("{kbps} kb/s")))
                    .child(div().type_small().text_color(theme::text_faint()).child(quality)),
            ),
        );
        let video = draft.video.unwrap_or(room.video_enabled);
        let ptt = draft.ptt.unwrap_or(room.ptt_only);
        column = column.child(
            section("Voice").child(
                card()
                    .child(
                        switch_row("tiny-channels-video", "Video", "People may turn their camera on here.", video, edits)
                            .when(edits, |row| row.on_click(cx.listener(move |this, _, _, cx| this.tiny_channels_draft(cx, |draft| draft.video = Some(!video))))),
                    )
                    .child(
                        switch_row("tiny-channels-ptt", "Push to talk required", "Everyone here holds a key to talk, whatever their voice activity permission.", ptt, edits)
                            .border_b_0()
                            .when(edits, |row| row.on_click(cx.listener(move |this, _, _, cx| this.tiny_channels_draft(cx, |draft| draft.ptt = Some(!ptt))))),
                    ),
            ),
        );
        if community.has(TINY_FEATURE_ROLES) {
            column = column.child(overrides(facts, room, draft, cx));
        }
    }
    if facts.may.delete_room {
        let id = room.id.clone();
        column = column.child(div().h(px(1.)).bg(theme::hairline())).child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_1().min_w_0().whitespace_normal().type_small().text_color(theme::text_soft()).child("Deleting a channel takes everyone in its call out of it."))
                .child(if shell.tiny_state.busy {
                    button_disabled("Delete channel").into_any_element()
                } else {
                    button("tiny-channels-delete-room", ButtonKind::Danger, "Delete channel")
                        .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_modal(Modal::DeleteRoom(id.clone()), window, cx)))
                        .into_any_element()
                }),
        );
    }
    column
}

/// Per role, allow / inherit / deny for each permission a channel can change.
fn overrides(facts: &Facts, room: &TinyRoom, draft: &Draft, cx: &mut Context<Shell>) -> Div {
    let community = facts.community;
    let may = community.allowed("community.rooms.overrides");
    let roles = super::roles::ordered(community);
    let role = draft.role.as_ref().and_then(|id| roles.iter().find(|role| role.id == *id).copied()).or_else(|| roles.iter().find(|role| role.default).copied()).or_else(|| roles.first().copied());
    let Some(role) = role else {
        return div();
    };
    let mut chips = div().flex().flex_wrap().gap(px(6.));
    for (ix, each) in roles.iter().enumerate() {
        let on = each.id == role.id;
        let has = override_of(room, draft, &each.id) != (0, 0);
        let dot = each.color.as_deref().and_then(model::hex).map(gpui::rgb).unwrap_or(theme::text_faint());
        let id = each.id.clone();
        chips = chips.child(
            div()
                .id(("tiny-channels-role", ix))
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
                    let id = id.clone();
                    this.tiny_channels_draft(cx, |draft| draft.role = Some(id));
                }))
                .child(div().size(px(8.)).rounded_full().bg(dot))
                .child(div().type_ui().text_color(theme::text()).child(each.name.clone()))
                .when(has, |chip| chip.child(div().size(px(6.)).rounded_full().bg(theme::accent_text()))),
        );
    }
    let editable = may && super::roles::editable(community, role);
    let bits = override_of(room, draft, &role.id);
    let after = effective(community, &role.id, &overrides_with(room, draft));
    let mut table = card();
    for (ix, bit) in SCOPED.iter().enumerate() {
        let bit = *bit;
        let (name, help) = super::roles::permission(bit);
        let state = tri_of(bits, bit);
        let mut control = div().flex_none().p(px(2.)).rounded(px(theme::R_CONTROL)).bg(theme::field()).border_1().border_color(theme::border()).flex().gap(px(2.));
        for (cell_ix, tri) in [Tri::Deny, Tri::Inherit, Tri::Allow].into_iter().enumerate() {
            let role_id = role.id.clone();
            let room_bits = bits;
            let words = match tri {
                Tri::Deny => "Deny",
                Tri::Inherit => "Inherit from the role",
                Tri::Allow => "Allow",
            };
            control = control.child(tri_cell(SharedString::from(format!("tiny-channels-tri-{ix}-{cell_ix}")), tri, state == tri, editable).tooltip(tip(words, None)).when(
                editable && state != tri,
                |cell| {
                    cell.on_click(cx.listener(move |this, _, _, cx| {
                        let role_id = role_id.clone();
                        this.tiny_channels_draft(cx, |draft| {
                            draft.overrides.insert(role_id, set_tri(room_bits, bit, tri));
                        })
                    }))
                },
            ));
        }
        let on = after & bit != 0;
        table = table.child(
            div()
                .min_h(px(56.))
                .px(px(14.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(14.))
                .when(ix + 1 < SCOPED.len(), |row| row.border_b_1().border_color(theme::hairline()))
                .when(!editable, |row| row.opacity(0.6))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(name))
                        .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(help)),
                )
                .child(div().w(px(60.)).flex_none().flex().justify_end().type_small().text_color(if on { theme::positive() } else { theme::text_faint() }).child(if on { "Can" } else { "Can't" }))
                .child(control),
        );
    }
    section("Permissions")
        .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(
            "For each role, what it may do in this channel. Inherit keeps the role's own permission; allow and deny override it here. Beside each: what someone with only that role ends up with.",
        ))
        .child(chips)
        .when(!editable, |section| {
            section.child(div().type_small().text_color(theme::warning()).child(if may {
                "This role is at or above your own, so its permissions here are shown, not changed."
            } else {
                "Changing these takes Manage roles and Manage channels."
            }))
        })
        .child(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_cell_is_allow_deny_or_inherit_and_setting_one_leaves_the_rest() {
        let bits = (TINY_SPEAK, TINY_VIDEO);
        assert_eq!(tri_of(bits, TINY_SPEAK), Tri::Allow);
        assert_eq!(tri_of(bits, TINY_VIDEO), Tri::Deny);
        assert_eq!(tri_of(bits, TINY_CONNECT), Tri::Inherit);
        assert_eq!(set_tri(bits, TINY_SPEAK, Tri::Deny), (0, TINY_VIDEO | TINY_SPEAK));
        assert_eq!(set_tri(bits, TINY_VIDEO, Tri::Inherit), (TINY_SPEAK, 0));
        assert_eq!(set_tri(bits, TINY_CONNECT, Tri::Allow), (TINY_SPEAK | TINY_CONNECT, TINY_VIDEO));
        assert_eq!(SCOPED.iter().fold(0, |all, bit| all | bit), TINY_CHANNEL_SCOPED);
    }

    #[test]
    fn save_sets_changed_overrides_and_clears_emptied_ones() {
        let room = TinyRoom {
            overrides: vec![TinyOverride { role_id: "a".into(), allow: TINY_SPEAK, deny: 0 }, TinyOverride { role_id: "b".into(), allow: 0, deny: TINY_VIDEO }],
            ..TinyRoom::new("r", "Lounge")
        };
        let mut draft = Draft::default();
        draft.overrides.insert("a".into(), (TINY_SPEAK, 0));
        draft.overrides.insert("b".into(), (0, 0));
        draft.overrides.insert("c".into(), (0, TINY_CONNECT));
        draft.overrides.insert("d".into(), (0, 0));
        assert_eq!(override_changes(&room, &draft), [("b".to_owned(), None), ("c".to_owned(), Some((0, TINY_CONNECT)))]);
        assert_eq!(override_of(&room, &Draft::default(), "a"), (TINY_SPEAK, 0));
        assert_eq!(override_of(&room, &draft, "c"), (0, TINY_CONNECT));
        let all = overrides_with(&room, &draft);
        assert!(all.iter().any(|over| over.role_id == "c" && over.deny == TINY_CONNECT) && all.iter().all(|over| over.role_id != "b" || over.deny == 0));
    }

    #[test]
    fn a_room_edit_sends_only_what_changed() {
        let room = TinyRoom { topic: "Hi".into(), user_limit: 8, bitrate_kbps: 0, ..TinyRoom::new("r", "Lounge") };
        assert_eq!(room_edit(&room, &Draft { bitrate: Some(TINY_BITRATE_DEFAULT), limit: Some(8), ..Draft::default() }, "Lounge ", "Hi"), TinyRoomEdit::default());
        let sent = room_edit(&room, &Draft { bitrate: Some(96), limit: Some(0), ptt: Some(true), video: Some(false), ..Draft::default() }, "Den", "");
        assert_eq!(sent.name.as_deref(), Some("Den"));
        assert_eq!(sent.topic.as_deref(), Some(""));
        assert_eq!((sent.user_limit, sent.bitrate_kbps, sent.ptt_only, sent.video_enabled), (Some(0), Some(96), Some(true), Some(false)));
    }

    #[test]
    fn what_a_role_ends_up_with_follows_the_overrides() {
        let mut community: TinyCommunity = serde_json::from_value(serde_json::json!({"role":"admin","members":[],"invites":[],"audit":[]})).unwrap();
        community.roles = vec![
            TinyRole { id: "everyone".into(), default: true, permissions: TINY_EVERYONE_DEFAULT, ..TinyRole::default() },
            TinyRole { id: "mod".into(), position: 1, permissions: TINY_MUTE_MEMBERS, ..TinyRole::default() },
        ];
        let overrides = [TinyOverride { role_id: "everyone".into(), allow: 0, deny: TINY_SPEAK }, TinyOverride { role_id: "mod".into(), allow: TINY_SPEAK, deny: 0 }];
        assert_eq!(effective(&community, "everyone", &overrides) & TINY_SPEAK, 0);
        assert_eq!(effective(&community, "mod", &overrides) & (TINY_SPEAK | TINY_MUTE_MEMBERS), TINY_SPEAK | TINY_MUTE_MEMBERS);
    }
}
