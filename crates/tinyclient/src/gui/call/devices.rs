//! The microphone and speaker menu above the dock's devices button (07 §7).
//!
//! Everything in it is the call's own: `Call::inputs` and `Call::outputs` are what the system
//! lists, `Call::input` and `Call::output` what is in use, `Call::level` the microphone's level.
//! chatd sends the level only between `Media(Meter { on: true })` and `Meter { on: false }`,
//! which the menu sends as it opens and closes. The shell runs no process to ask.
//!
//! A pick here changes this call only; Settings › Audio & video holds the defaults a call starts
//! with, and the picks are forgotten when the call ends. "Play test sound" plays the join cue on
//! the call's speaker; "Call settings…" opens that section.

use super::{in_call, Menu};
use crate::client::ask;
use crate::gui::app::prefs;
use crate::gui::components::{menu_item, menu_separator};
use crate::gui::settings;
use crate::gui::state::Shell;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, FontWeight, SharedString};
use protocol::settings::{INPUT, OUTPUT};
use protocol::{Call, MediaRequest, Request};
use std::time::{Duration, Instant};

/// Segments of the level meter.
const SEGMENTS: u8 = 12;

/// The row to mark: what was picked in this call, else the default from Settings, where `None`
/// is "System default".
fn chosen(picked: &Option<Option<String>>, default: Option<String>) -> Option<String> {
    match picked {
        Some(picked) => picked.clone(),
        None => default,
    }
}

/// How many of the meter's segments a level of 0 to 100 lights. Any sound at all lights one.
fn lit(level: Option<u8>) -> u8 {
    match level {
        None | Some(0) => 0,
        Some(level) => (u16::from(level.min(100)) * u16::from(SEGMENTS)).div_ceil(100) as u8,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Input,
    Output,
}

impl Shell {
    /// The popover that is open. Under `--shot`, the one the hook names.
    pub(super) fn call_menu(&self) -> Option<Menu> {
        #[cfg(feature = "gui-shot")]
        if let Some(menu) = super::shot::menu() {
            return Some(menu);
        }
        self.call_state.menu
    }

    /// Tells chatd the device menu opened or closed: levels flow only in between. Nothing is
    /// shown if it is refused; a call that just ended has no meter to turn off.
    fn call_meter(&self, on: bool, cx: &mut Context<Self>) {
        cx.background_executor()
            .spawn(async move {
                let _ = ask(&Request::Media(MediaRequest::Meter { on }));
            })
            .detach();
    }

    /// Closes whatever popover is open, however it came to close.
    pub(super) fn call_menu_set(&mut self, menu: Option<Menu>, cx: &mut Context<Self>) {
        let before = std::mem::replace(&mut self.call_state.menu, menu);
        if before == menu {
            return;
        }
        if before == Some(Menu::Devices) {
            self.call_meter(false, cx);
        }
        if menu == Some(Menu::Devices) {
            self.call_meter(true, cx);
        }
        cx.notify();
    }

    pub(super) fn call_menu_close(&mut self, cx: &mut Context<Self>) {
        if self.call_state.menu.is_some() {
            self.call_state.menu_closed = Some(Instant::now());
            self.call_menu_set(None, cx);
        }
    }

    /// Opens `menu`, or closes it when it is the one open.
    pub(super) fn call_menu_toggle(&mut self, menu: Menu, cx: &mut Context<Self>) {
        // The press that closed it from outside was on its own button: stay closed.
        let just_closed = self
            .call_state
            .menu_closed
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_millis(400));
        if self.call_state.menu == Some(menu) || just_closed {
            self.call_menu_set(None, cx);
        } else {
            self.call_menu_set(Some(menu), cx);
        }
    }

    /// `None` follows the system default.
    fn call_pick(&mut self, side: Side, device: Option<String>, cx: &mut Context<Self>) {
        match side {
            Side::Input => self.call_state.input = Some(device),
            Side::Output => self.call_state.output = Some(device),
        }
        let values = prefs::values();
        let input = chosen(&self.call_state.input, values.device(INPUT));
        let output = chosen(&self.call_state.output, values.device(OUTPUT));
        self.call_ask(Request::Media(MediaRequest::Devices { input, output }), cx);
        cx.notify();
    }

    fn call_device_row(
        &self,
        side: Side,
        ix: usize,
        device: Option<String>,
        detail: Option<String>,
        chosen: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label: SharedString = device.clone().unwrap_or_else(|| "System default".to_owned()).into();
        let group = if side == Side::Input { "call-input" } else { "call-output" };
        let radio = div().flex_none().size(px(14.)).rounded(px(7.)).flex().items_center().justify_center();
        let radio = if chosen {
            radio
                .bg(theme::accent())
                .child(div().size(px(6.)).rounded(px(3.)).bg(theme::on_accent()))
        } else {
            radio.border_1().border_color(theme::text_faint())
        };
        div()
            .id((group, ix))
            .h(px(30.))
            .flex_none()
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(if chosen { theme::text() } else { theme::text_soft() })
            .hover(|style| style.bg(theme::hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.call_pick(side, device.clone(), cx)))
            .child(radio)
            .child(div().flex_none().max_w(px(250.)).truncate().child(label))
            .children(detail.map(|name| {
                div().flex_1().min_w_0().truncate().text_color(theme::text_faint()).child(name)
            }))
    }

    fn call_device_section(&self, side: Side, call: &Call, cx: &mut Context<Self>) -> Div {
        let (title, picked, in_use, listed, key) = match side {
            Side::Input => ("MICROPHONE", &self.call_state.input, &call.input, &call.inputs, INPUT),
            Side::Output => ("SPEAKER", &self.call_state.output, &call.output, &call.outputs, OUTPUT),
        };
        let chosen = chosen(picked, prefs::values().device(key));
        // A device picked here that the system no longer lists still has its row, so the
        // choice can be seen and changed.
        let mut names = listed.clone();
        if let Some(name) = chosen.as_ref().filter(|name| !names.contains(name)) {
            names.push(name.clone());
        }
        // What "System default" is now: the device in use, while it is the one being followed.
        let following = (chosen.is_none() && !in_use.is_empty()).then(|| in_use.clone());
        let label = div()
            .h(px(24.))
            .flex_none()
            .px(px(8.))
            .flex()
            .items_center()
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_faint())
            .child(title);
        div()
            .flex()
            .flex_col()
            .child(label)
            .child(self.call_device_row(side, 0, None, following, chosen.is_none(), cx))
            .children(names.into_iter().enumerate().map(|(ix, name)| {
                let on = chosen.as_ref() == Some(&name);
                self.call_device_row(side, ix + 1, Some(name), None, on, cx).into_any_element()
            }))
    }

    /// Twelve segments and what they measure. It stays live while muted so the microphone can
    /// be tried, and says that nobody hears it. Before the call is up the microphone is not
    /// open, and the meter says so by staying dark.
    fn call_level(&self, call: &Call) -> Div {
        let lit = lit(call.level);
        let words = match (in_call(call), call.muted || call.deafened) {
            (false, _) => "Input level, once the call connects",
            (true, true) => "Muted: others can't hear you",
            (true, false) => "Input level",
        };
        let segment = |ix: u8| {
            div()
                .flex_none()
                .w(px(4.))
                .h(px(12.))
                .rounded(px(1.))
                .bg(if ix < lit { theme::positive() } else { theme::selected() })
        };
        div()
            .h(px(28.))
            .flex_none()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex_none().flex().gap(px(2.)).children((0..SEGMENTS).map(segment)))
            .child(div().min_w_0().truncate().text_size(px(11.5)).text_color(theme::text_soft()).child(words))
    }

    /// What the device menu holds: MICROPHONE with its level, then SPEAKER, "System default"
    /// first in each.
    pub(super) fn call_devices_menu(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        let test = menu_item("call-test-sound", Some("icons/volume-2.svg"), "Play test sound", None).on_click(cx.listener(|this, _, _, cx| {
            this.call_ask(Request::TestSound, cx);
        }));
        let settings = menu_item("call-settings", Some("icons/settings.svg"), "Call settings…", None).on_click(cx.listener(|this, _, _, cx| {
            this.call_menu_close(cx);
            settings::open(settings::Section::Audio, cx);
        }));
        div()
            .flex()
            .flex_col()
            .child(self.call_device_section(Side::Input, call, cx))
            .child(self.call_level(call))
            .child(div().flex_none().my(px(4.)).mx(px(8.)).h(px(1.)).bg(theme::hairline()))
            .child(self.call_device_section(Side::Output, call, cx))
            .child(menu_separator().mx(px(8.)))
            .child(test)
            .child(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_lights_one_segment_for_any_sound_and_all_twelve_at_the_top() {
        assert_eq!(lit(None), 0);
        assert_eq!(lit(Some(0)), 0);
        assert_eq!(lit(Some(1)), 1);
        assert_eq!(lit(Some(50)), 6);
        assert_eq!(lit(Some(62)), 8);
        assert_eq!(lit(Some(100)), 12);
        assert_eq!(lit(Some(250)), 12);
    }

    #[test]
    fn the_marked_row_is_what_was_picked_in_this_call_and_the_settings_default_otherwise() {
        let headset = Some("Studio headset".to_owned());
        assert_eq!(chosen(&None, None), None);
        assert_eq!(chosen(&None, headset.clone()), headset, "the default from Settings");
        assert_eq!(chosen(&Some(None), headset.clone()), None, "System default picked in this call");
        assert_eq!(chosen(&Some(Some("AirPods".into())), headset), Some("AirPods".into()));
    }
}
