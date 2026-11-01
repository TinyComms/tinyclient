//! The microphone and speaker menu above the dock's devices button (07 §7).
//!
//! Everything it lists is the call's own: `Call::inputs` and `Call::outputs` are what the system
//! lists, `Call::input_id` and `Call::output_id` what is in use, `Call::level` and
//! `Call::output_level` the two meters. chatd sends the levels only between
//! `Media(Meter { on: true })` and `Meter { on: false }`, which the menu sends as it opens and
//! closes. The shell runs no process to ask.
//!
//! A pick here is the user's choice of device, the same one Settings › Audio & video shows (V1,
//! V2): stored by chatd, which hands it to this call at once and to every call after, across
//! restarts. "Automatic" is never having chosen: the system default, unless that is a virtual
//! device. "System default" follows the system wherever it goes. Virtual devices (Krisp,
//! VB-Cable, ...) are listed apart and only used when picked. "Test speakers" plays the join cue
//! on the call's speaker; the input meter is the microphone test; "Call settings…" opens the
//! section.

use super::{in_call, Menu};
use crate::client::ask;
use crate::gui::app::prefs;
use crate::gui::components::{menu_item, menu_separator};
use crate::gui::settings;
use crate::gui::state::Shell;
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, FontWeight, SharedString};
use protocol::settings::{DeviceChoice, GLOBAL, INPUT, OUTPUT};
use protocol::{AudioDevice, Call, MediaRequest, Request};
use std::time::{Duration, Instant};

/// Segments of a level meter.
const SEGMENTS: u8 = 12;

/// How many of the meter's segments a level of 0 to 100 lights. Any sound at all lights one.
fn lit(level: Option<u8>) -> u8 {
    match level {
        None | Some(0) => 0,
        Some(level) => (u16::from(level.min(100)) * u16::from(SEGMENTS)).div_ceil(100) as u8,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Input,
    Output,
}

/// One row of a side of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) choice: DeviceChoice,
    pub(super) label: String,
    /// What the row stands for now, or why it is not there: faint, after the label.
    pub(super) detail: Option<String>,
    pub(super) chosen: bool,
    pub(super) is_virtual: bool,
}

/// A side's rows: Automatic, System default, the hardware, then the virtual devices, then the
/// device chosen if the system no longer lists it. `in_use` is the id of the device open now.
pub(super) fn rows(choice: &DeviceChoice, listed: &[AudioDevice], in_use: &str) -> Vec<Row> {
    let name_of = |pick: fn(&AudioDevice) -> bool| listed.iter().find(|d| pick(d)).map(|d| d.name.clone());
    let mut rows = vec![
        Row {
            choice: DeviceChoice::Auto,
            label: "Automatic".to_owned(),
            detail: name_of(|d| d.auto),
            chosen: *choice == DeviceChoice::Auto,
            is_virtual: false,
        },
        Row {
            choice: DeviceChoice::System,
            label: "System default".to_owned(),
            detail: name_of(|d| d.default),
            chosen: *choice == DeviceChoice::System,
            is_virtual: false,
        },
    ];
    let device_row = |device: &AudioDevice| {
        let chosen = choice.is(device, listed);
        // The device open now, when it is not the one chosen: what stands in for it.
        let standing_in = !chosen && device.id == in_use && matches!(choice, DeviceChoice::Device { .. });
        Row {
            choice: DeviceChoice::device(&device.name, &device.id),
            label: device.name.clone(),
            detail: standing_in.then(|| "in use for now".to_owned()),
            chosen,
            is_virtual: device.is_virtual,
        }
    };
    rows.extend(listed.iter().filter(|d| !d.is_virtual).map(device_row));
    rows.extend(listed.iter().filter(|d| d.is_virtual).map(device_row));
    if let DeviceChoice::Device { name, .. } = choice {
        if !listed.iter().any(|d| choice.is(d, listed)) {
            rows.push(Row { choice: choice.clone(), label: name.clone(), detail: Some("not connected".to_owned()), chosen: true, is_virtual: false });
        }
    }
    rows
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

    /// Stores the choice (V1, V2) as Settings would: chatd gives it to the call at once and
    /// keeps it for every call after.
    pub(super) fn call_pick(&mut self, side: Side, choice: DeviceChoice, cx: &mut Context<Self>) {
        let key = if side == Side::Input { INPUT } else { OUTPUT };
        let value = choice.value();
        let mut values = prefs::values();
        values.set(GLOBAL, key, value.clone());
        prefs::set_values(values);
        settings::redraw(cx);
        self.call_ask(Request::Set { scope: GLOBAL.to_owned(), key: key.to_owned(), value }, cx);
        cx.notify();
    }

    fn call_device_row(&self, side: Side, ix: usize, row: Row, cx: &mut Context<Self>) -> impl IntoElement {
        let group = if side == Side::Input { "call-input" } else { "call-output" };
        let radio = div().flex_none().size(px(14.)).rounded(px(7.)).flex().items_center().justify_center();
        let radio = if row.chosen {
            radio
                .bg(theme::accent())
                .child(div().size(px(6.)).rounded(px(3.)).bg(theme::on_accent()))
        } else {
            radio.border_1().border_color(theme::text_faint())
        };
        let tag = row.is_virtual.then(|| {
            div()
                .flex_none()
                .px(px(4.))
                .rounded(px(3.))
                .border_1()
                .border_color(theme::hairline())
                .text_size(px(10.))
                .text_color(theme::text_faint())
                .child("virtual")
        });
        let choice = row.choice.clone();
        div()
            .id((group, ix))
            .h(px(28.))
            .flex_none()
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(if row.chosen { theme::text() } else { theme::text_soft() })
            .hover(|style| style.bg(theme::hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.call_pick(side, choice.clone(), cx)))
            .child(radio)
            .child(div().flex_none().max_w(px(220.)).truncate().child(SharedString::from(row.label)))
            .children(tag)
            .children(row.detail.map(|detail| {
                div().flex_1().min_w_0().truncate().text_color(theme::text_faint()).child(detail)
            }))
    }

    fn call_device_section(&self, side: Side, call: &Call, cx: &mut Context<Self>) -> Div {
        let (title, listed, in_use, key) = match side {
            Side::Input => ("MICROPHONE", &call.inputs, &call.input_id, INPUT),
            Side::Output => ("SPEAKER", &call.outputs, &call.output_id, OUTPUT),
        };
        let choice = prefs::values().device_choice(key);
        let heading = |words: &'static str| {
            div()
                .h(px(22.))
                .flex_none()
                .px(px(8.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_faint())
                .child(words)
        };
        let mut section = div().flex().flex_col().child(heading(title));
        let mut virtual_heading = false;
        for (ix, row) in rows(&choice, listed, in_use).into_iter().enumerate() {
            if row.is_virtual && !virtual_heading {
                virtual_heading = true;
                section = section.child(heading("VIRTUAL · only when picked"));
            }
            section = section.child(self.call_device_row(side, ix, row, cx));
        }
        section
    }

    /// Twelve segments and what they measure. The microphone's stays live while muted so it can
    /// be tried, and says that nobody hears it. Before the call is up nothing is open, and the
    /// meters say so by staying dark.
    pub(super) fn call_level(&self, call: &Call, side: Side) -> Div {
        let (level, words) = match side {
            Side::Input => {
                let words = match (in_call(call), call.muted || call.deafened) {
                    (false, _) => "Microphone level, once the call connects",
                    (true, true) => "Muted: others can't hear you. Speak to test.",
                    (true, false) => "Speak to test your microphone",
                };
                (call.level, words)
            }
            Side::Output => {
                let words = if in_call(call) { "What you hear" } else { "Speaker level, once the call connects" };
                (call.output_level, words)
            }
        };
        let lit = lit(level);
        let segment = |ix: u8| {
            div()
                .flex_none()
                .w(px(4.))
                .h(px(12.))
                .rounded(px(1.))
                .bg(if ix < lit { theme::positive() } else { theme::selected() })
        };
        div()
            .h(px(26.))
            .flex_none()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex_none().flex().gap(px(2.)).children((0..SEGMENTS).map(segment)))
            .child(div().min_w_0().truncate().text_size(px(11.5)).text_color(theme::text_soft()).child(words))
    }

    /// Why a device in use is not the one chosen, or that one is silent, at the top of the menu.
    fn call_device_notice_row(&self, call: &Call) -> Option<Div> {
        let words = call.device_notice.clone()?;
        let color = if call.device_stuck { theme::warning() } else { theme::text_soft() };
        Some(
            div()
                .mx(px(4.))
                .mb(px(4.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::warning_soft())
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(color)
                .child(words),
        )
    }

    /// "Switch device", after the dock's line when a device stays silent: opens this menu.
    pub(super) fn call_switch_device(&self, call: &Call, live: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !live || !call.device_stuck {
            return None;
        }
        Some(
            div()
                .id("call-switch-device")
                .flex_none()
                .cursor_pointer()
                .underline()
                .text_color(theme::warning())
                .child("Switch device")
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_set(Some(Menu::Devices), cx);
                }))
                .into_any_element(),
        )
    }

    /// What the device menu holds: a notice when something is not as chosen, MICROPHONE with
    /// its level, SPEAKER with its level, then the speaker test and the settings.
    pub(super) fn call_devices_menu(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        let test = menu_item("call-test-sound", Some("icons/volume-2.svg"), "Test speakers", None).on_click(cx.listener(|this, _, _, cx| {
            this.call_ask(Request::TestSound, cx);
        }));
        let settings = menu_item("call-settings", Some("icons/settings.svg"), "Call settings…", None).on_click(cx.listener(|this, _, _, cx| {
            this.call_menu_close(cx);
            settings::open(settings::Section::Audio, cx);
        }));
        div()
            .flex()
            .flex_col()
            .children(self.call_device_notice_row(call))
            .child(self.call_device_section(Side::Input, call, cx))
            .child(self.call_level(call, Side::Input))
            .child(div().flex_none().my(px(4.)).mx(px(8.)).h(px(1.)).bg(theme::hairline()))
            .child(self.call_device_section(Side::Output, call, cx))
            .child(self.call_level(call, Side::Output))
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

    const HEADSET: &str = "Headset Earphone (CORSAIR HS55 Wireless Core Receiver)";

    fn listed() -> Vec<AudioDevice> {
        let device = |id: &str, name: &str| AudioDevice { id: id.into(), name: name.into(), ..AudioDevice::default() };
        let krisp = AudioDevice { is_virtual: true, default: true, ..device("krisp", "Krisp Speaker (Krisp Audio)") };
        let headset = AudioDevice { auto: true, ..device("hs55", HEADSET) };
        vec![krisp, headset, device("usb", "Speakers (2- USB Audio Device)")]
    }

    fn labels(rows: &[Row]) -> Vec<(&str, Option<&str>, bool)> {
        rows.iter().map(|row| (row.label.as_str(), row.detail.as_deref(), row.chosen)).collect()
    }

    #[test]
    fn automatic_and_system_default_say_what_they_mean_now_and_virtual_devices_come_last() {
        let rows = rows(&DeviceChoice::Auto, &listed(), "hs55");
        assert_eq!(
            labels(&rows),
            [
                ("Automatic", Some(HEADSET), true),
                ("System default", Some("Krisp Speaker (Krisp Audio)"), false),
                (HEADSET, None, false),
                ("Speakers (2- USB Audio Device)", None, false),
                ("Krisp Speaker (Krisp Audio)", None, false),
            ]
        );
        assert!(rows[4].is_virtual && rows[..4].iter().all(|row| !row.is_virtual));
        assert_eq!(rows[2].choice, DeviceChoice::device(HEADSET, "hs55"), "a pick is stored with the id");
    }

    #[test]
    fn the_chosen_device_is_marked_and_a_missing_one_keeps_its_row() {
        let headset = DeviceChoice::device(HEADSET, "hs55");
        let rows_now = rows(&headset, &listed(), "hs55");
        assert_eq!(rows_now.iter().filter(|row| row.chosen).map(|row| row.label.as_str()).collect::<Vec<_>>(), [HEADSET]);
        // Switched off: its row stays, marked, and what stands in says so.
        let without: Vec<AudioDevice> = listed().into_iter().filter(|d| d.id != "hs55").collect();
        let rows_gone = rows(&headset, &without, "usb");
        assert_eq!(rows_gone.last().map(|row| (row.label.as_str(), row.detail.as_deref(), row.chosen)), Some((HEADSET, Some("not connected"), true)));
        let usb = rows_gone.iter().find(|row| row.label.starts_with("Speakers")).unwrap();
        assert_eq!(usb.detail.as_deref(), Some("in use for now"));
    }

    #[test]
    fn a_name_saved_by_an_older_build_marks_its_device() {
        let legacy = DeviceChoice::Device { name: HEADSET.into(), id: None };
        let rows = rows(&legacy, &listed(), "hs55");
        assert!(rows.iter().any(|row| row.label == HEADSET && row.chosen));
        assert_eq!(rows.len(), 5, "no 'not connected' row for a device that is there");
    }
}
