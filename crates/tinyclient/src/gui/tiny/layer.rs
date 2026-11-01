//! Everything the server page floats over the window, drawn from the window's root so it sits
//! above every pane: Server Settings, a menu with its submenu, the hover card, the connection
//! numbers, and a modal. A menu has a catcher behind it that closes it on a click anywhere else,
//! and that also carries a volume drag that wanders off the slider.

use super::members::MEMBERS_WIDTH;
use super::panel::PANEL;
use super::{Float, Page};
use crate::gui::components::place::anchor;
use crate::gui::components::{fade_in, placed, popover, Layer, Place};
use crate::gui::layout::RAIL_WIDTH;
use crate::gui::state::Shell;
use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, MouseMoveEvent, Window};

impl Shell {
    pub(in crate::gui) fn tiny_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = &self.tiny_state;
        if state.float.is_none() && state.modal.is_none() && state.settings.is_none() && state.card.is_none() && !state.setup.prompt {
            return None;
        }
        let page = self.tiny_page_facts()?;
        let mut layer = div().absolute().top_0().left_0().size_full();
        if state.settings.is_some() {
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
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.tiny_track(event, cx)))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.tiny_slide_end(cx))),
            );
            layer = layer.children(self.tiny_float(&page, &float, window, cx));
        } else if let Some((user_id, y)) = state.card.clone().filter(|_| state.modal.is_none() && state.settings.is_none()) {
            if let Some(member) = page.community.member(&user_id) {
                let wide = f32::from(window.viewport_size().width);
                let at = anchor(wide - MEMBERS_WIDTH + 8., y - 22., MEMBERS_WIDTH - 16., 44.);
                let card = fade_in("tiny-card", 120, popover().p(px(16.)).child(self.tiny_person_card(&page, member, false)));
                layer = layer.child(placed("tiny-card-at", Place::Beside(at), Layer::Popover, window, card));
            }
        }
        if let Some(modal) = state.modal.clone() {
            let panel = self.tiny_modal(&page, &modal, window, cx);
            layer = layer.child(super::modals::layer(panel, cx));
        }
        // Above everything else, until the owner answers or puts it off with Later or Escape.
        if state.setup.prompt {
            layer = layer.child(self.tiny_setup_overlay(&page, cx));
        }
        Some(layer.into_any_element())
    }

    fn tiny_track(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.tiny_state.sliding.is_none() {
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            self.tiny_slide(f32::from(event.position.x), cx);
        } else {
            self.tiny_slide_end(cx);
        }
    }

    fn tiny_float(&self, page: &Page, float: &Float, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let tall = f32::from(window.viewport_size().height);
        let sidebar = self.layout_state.sidebar_width;
        let mut out = Vec::new();
        let menu_events = |menu: gpui::Div, cx: &mut Context<Self>| {
            menu.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.tiny_track(event, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.tiny_slide_end(cx)))
        };
        match float {
            Float::Header => {
                let at = anchor(RAIL_WIDTH + 8., 8., sidebar - 16., 36.);
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
            Float::Category { key, at } => {
                let menu = self.tiny_category_menu(page, key, cx);
                out.push(placed("tiny-category-menu", Place::Pointer(*at), Layer::Menu, window, fade_in("tiny-category-fade", 120, menu)));
            }
            Float::Stats => {
                let at = anchor(RAIL_WIDTH + 8., tall - PANEL, sidebar - 16., PANEL);
                let card = popover().p(px(14.)).child(self.tiny_stats_card());
                out.push(placed("tiny-stats", Place::Above(at), Layer::Popover, window, fade_in("tiny-stats-fade", 120, card)));
            }
        }
        out
    }
}
