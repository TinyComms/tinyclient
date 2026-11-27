//! Everything the server page floats over the window, drawn from the window's root so it sits
//! above every pane: a stream in full screen, Server Settings, a menu with its submenu, the hover
//! card, the connection numbers, and a modal. A menu has a catcher behind it that closes it on a
//! click anywhere else, and that also carries a volume drag that wanders off the slider.

use super::members::MEMBERS_WIDTH;
use super::panel::PANEL;
use super::{Float, Page};
use crate::gui::components::place::anchor;
use crate::gui::components::{fade_in, placed, popover, Layer, Place};
use crate::gui::layout::RAIL_WIDTH;
use crate::gui::state::Shell;
use gpui::{div, prelude::*, px, AnyElement, Bounds, Context, MouseButton, MouseMoveEvent, Pixels, Window};
use std::cell::Cell;

thread_local! {
    static HEADER_BOUNDS: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
    static PANEL_BOUNDS: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
    static MEMBERS_BOUNDS: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
}

pub(super) fn note_header_bounds(bounds: Bounds<Pixels>) {
    HEADER_BOUNDS.with(|cell| cell.set(Some(bounds)));
}

pub(super) fn note_panel_bounds(bounds: Bounds<Pixels>) {
    PANEL_BOUNDS.with(|cell| cell.set(Some(bounds)));
}

pub(super) fn note_members_bounds(bounds: Bounds<Pixels>) {
    MEMBERS_BOUNDS.with(|cell| cell.set(Some(bounds)));
}

/// Desktop `tiny_settings_overlay` must never mount when phone mode is active (`tiny_settings_phone_page` owns it).
pub(in crate::gui) fn desktop_settings_overlay_shown(settings_open: bool, phone: bool) -> bool {
    settings_open && !phone
}

/// Vertical anchor height for Call health (`Float::Stats`): clears the compact call dock (`+112`)
/// whenever a Tiny voice call is active above the user panel.
pub(in crate::gui) fn stats_anchor_base(has_tiny_call: bool) -> f32 {
    PANEL + if has_tiny_call { 112. } else { 0. }
}

impl Shell {
    pub(in crate::gui) fn tiny_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = &self.tiny_state;
        if state.float.is_none() && state.modal.is_none() && state.settings.is_none() && state.card.is_none() && !state.setup.prompt {
            return None;
        }
        // Sharing and abuse reports work even while another chat is selected and
        // no Tiny community page is loaded.
        if matches!(state.modal, Some(super::Modal::Share)) {
            return Some(super::modals::layer(self.tiny_share_modal(cx), true, window, cx));
        }
        if let Some(super::Modal::Report { kind, label, message_text, category, include_evidence, .. }) = state.modal.clone() {
            return Some(super::modals::layer(
                self.tiny_report_modal(&kind, &label, message_text.as_deref(), category, include_evidence, window, cx),
                true,
                window,
                cx,
            ));
        }
        let page = self.tiny_page_facts()?;
        let mut layer = div().absolute().top_0().left_0().size_full();
        if desktop_settings_overlay_shown(state.settings.is_some(), crate::gui::phone::active()) {
            layer = layer.child(self.tiny_settings_overlay(&page, window, cx));
        }
        if let Some(float) = state.float.clone() {
            layer = layer.child(
                div()
                    .id("tiny-catcher")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.tiny_close_float(cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _, cx| this.tiny_close_float(cx)))
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.call_track(event, cx)))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.call_slide_end(cx))),
            );
            layer = layer.children(self.tiny_float(&page, &float, window, cx));
        } else if let Some((user_id, y)) = state.card.clone().filter(|_| state.modal.is_none() && state.settings.is_none()) {
            if let Some(member) = page.community.member(&user_id) {
                let wide = f32::from(window.viewport_size().width);
                let (col_x, col_w) = MEMBERS_BOUNDS
                    .with(|cell| cell.get())
                    .map(|b| (f32::from(b.origin.x), f32::from(b.size.width)))
                    .filter(|(_, w)| *w > 0.)
                    .unwrap_or((wide - MEMBERS_WIDTH, MEMBERS_WIDTH));
                let at = anchor(col_x + 8., y - 22., (col_w - 16.).max(40.), 44.);
                let card_max_w = (wide - 16.).max(200.);
                let card = fade_in("tiny-card", 120, popover().max_w(px(card_max_w)).p(px(16.)).child(self.tiny_person_card(&page, member, false)));
                layer = layer.child(placed("tiny-card-at", Place::Beside(at), Layer::Popover, window, card));
            }
        }
        // Passkey links are asked for from the first-run screen, so they are drawn over it.
        let over_setup = matches!(state.modal, Some(super::Modal::PasskeyLinks { .. }));
        let modal = state.modal.clone().map(|modal| super::modals::layer(self.tiny_modal(&page, &modal, window, cx), !modal.holds_secrets(), window, cx));
        let (under, over) = if over_setup { (None, modal) } else { (modal, None) };
        layer = layer.children(under);
        // Above everything else, until the owner answers or puts it off with Later or Escape.
        if state.setup.prompt {
            layer = layer.child(self.tiny_setup_overlay(&page, cx));
        }
        layer = layer.children(over);
        Some(layer.into_any_element())
    }

    fn tiny_float(&self, page: &Page, float: &Float, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let viewport = window.viewport_size();
        let wide = f32::from(viewport.width);
        let tall = f32::from(viewport.height);
        let live_sidebar = self.sidebar_state.width.get();
        let sidebar = if live_sidebar > 0. { live_sidebar } else { self.layout_state.sidebar_width };
        let rail = if crate::gui::phone::active() { 64. } else { RAIL_WIDTH };
        let live_off = if self.live_location_state.all_shares().is_empty() { 0. } else { crate::gui::theme::BANNER };
        let mut out = Vec::new();
        let menu_events = |menu: gpui::Div, cx: &mut Context<Self>| {
            menu.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.call_track(event, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.call_slide_end(cx)))
        };
        match float {
            Float::Header => {
                let at = HEADER_BOUNDS
                    .with(|cell| cell.get())
                    .filter(|b| f32::from(b.size.width) > 0.)
                    .unwrap_or_else(|| anchor(rail + 8., live_off + 8., (sidebar - 16.).max(180.), 36.));
                let menu = menu_events(self.tiny_header_menu(page, cx), cx);
                out.push(placed("tiny-header-menu", Place::Below(at), Layer::Menu, window, fade_in("tiny-header-fade", 120, menu)));
            }
            Float::Person { user_id, at } => {
                let menu = menu_events(self.tiny_person_menu(page, user_id, cx), cx);
                out.push(placed("tiny-person-menu", Place::Pointer(*at), Layer::Menu, window, fade_in("tiny-person-fade", 120, menu)));
                if let (Some(sub), Some(bounds)) = (self.tiny_state.sub, self.tiny_state.sub_anchor.get()) {
                    let menu = self.tiny_person_sub(page, user_id, sub, cx);
                    out.push(placed("tiny-person-sub", Place::Beside(bounds), Layer::Menu, window, menu));
                }
            }
            Float::Channel { room_id, at } => {
                let menu = self.tiny_channel_menu(page, room_id, cx);
                out.push(placed("tiny-channel-menu", Place::Pointer(*at), Layer::Menu, window, fade_in("tiny-channel-fade", 120, menu)));
            }
            Float::TextChannel { id, at } => {
                if let Some(menu) = self.tiny_text_channel_menu(page, id, cx) {
                    out.push(placed("tiny-text-channel-menu", Place::Pointer(*at), Layer::Menu, window, fade_in("tiny-text-channel-fade", 120, menu)));
                }
            }
            Float::Category { key, at } => {
                let menu = self.tiny_category_menu(page, key, cx);
                out.push(placed("tiny-category-menu", Place::Pointer(*at), Layer::Menu, window, fade_in("tiny-category-fade", 120, menu)));
            }
            Float::Stats => {
                // Clear the compact call dock when a Tiny voice call is active above the user panel.
                let has_call = self.tiny_call().is_some();
                let dock = if has_call { 112. } else { 0. };
                let at = PANEL_BOUNDS
                    .with(|cell| cell.get())
                    .filter(|b| f32::from(b.size.width) > 0.)
                    .map(|b| {
                        let y = (f32::from(b.origin.y) - dock).max(8.);
                        let h = f32::from(b.size.height) + dock;
                        anchor(f32::from(b.origin.x) + 8., y, (f32::from(b.size.width) - 16.).max(180.), h)
                    })
                    .unwrap_or_else(|| {
                        let base = stats_anchor_base(has_call);
                        anchor(rail + 8., (tall - base).max(8.), (sidebar - 16.).max(180.), base)
                    });
                let max_w = (wide - 16.).max(200.);
                let card = popover().max_w(px(max_w)).p(px(14.)).child(self.tiny_stats_card(cx));
                out.push(placed("tiny-stats", Place::Above(at), Layer::Popover, window, fade_in("tiny-stats-fade", 120, card)));
            }
        }
        out
    }
}
