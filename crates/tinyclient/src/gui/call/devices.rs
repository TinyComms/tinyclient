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
use crate::gui::components::{button_disabled, button_with_icon, glyph, menu_item, menu_separator, ButtonKind};
use crate::gui::settings;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, img, prelude::*, px, AnyElement, Context, Div, FontWeight, ObjectFit, SharedString};
use protocol::settings::{DeviceChoice, CAMERA, GLOBAL, INPUT, OUTPUT};
use protocol::{AudioDevice, Call, CallState, CameraSource, MediaRequest, Request};
use std::time::{Duration, Instant};

/// Segments of a level meter.
const SEGMENTS: u8 = 12;

/// Maximum scroll height for a single device list inside a focused picker popover.
const LIST_MAX_H: f32 = 250.;

/// How many of the meter's segments a level of 0 to 100 lights. Any sound at all lights one.
fn lit(level: Option<u8>) -> u8 {
    match level {
        None | Some(0) => 0,
        Some(level) => (u16::from(level.min(100)) * u16::from(SEGMENTS)).div_ceil(100) as u8,
    }
}

/// Which focused device picker a device notice refers to, when a device is stuck or clicked.
pub(super) fn stuck_device_menu(notice: &str) -> Menu {
    let lower = notice.to_ascii_lowercase();
    if lower.contains("speaker") || lower.contains("output") || lower.contains("headphone") || lower.contains("earphone") {
        Menu::Output { from_hub: false }
    } else if lower.contains("microphone") || lower.contains("mic") || lower.contains("input") {
        Menu::Input { from_hub: false }
    } else {
        Menu::Devices
    }
}

/// Whether `notice` should be shown inside a focused `side` picker.
fn notice_applies_to(notice: &str, side: Side) -> bool {
    match (side, stuck_device_menu(notice)) {
        (Side::Input, Menu::Output { .. }) => false,
        (Side::Output, Menu::Input { .. }) => false,
        _ => true,
    }
}

/// One-line active device summary for the compact Call Options hub, plus whether it is in a
/// disconnected/warning state.
pub(super) fn active_device_summary(
    choice: &DeviceChoice,
    listed: &[AudioDevice],
    in_use: &str,
) -> (String, bool) {
    if let DeviceChoice::Device { name, .. } = choice {
        if !listed.iter().any(|d| choice.is(d, listed)) {
            return (format!("{name} · not connected"), true);
        }
    }
    if !in_use.is_empty() {
        if let Some(device) = listed.iter().find(|d| d.id == in_use) {
            return (device.name.clone(), false);
        }
    }
    match choice {
        DeviceChoice::Auto => (
            listed
                .iter()
                .find(|d| d.auto)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "Automatic".to_owned()),
            false,
        ),
        DeviceChoice::System => (
            listed
                .iter()
                .find(|d| d.default)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "System default".to_owned()),
            false,
        ),
        DeviceChoice::Device { name, .. } => (name.clone(), false),
    }
}

/// One-line active camera summary for the compact Call Options hub, plus whether it is disconnected.
pub(super) fn active_camera_summary(
    choice: &DeviceChoice,
    listed: &[CameraSource],
    in_use: &str,
) -> (String, bool) {
    if let DeviceChoice::Device { name, .. } = choice {
        if !listed.iter().any(|cam| camera_matches(choice, cam)) {
            return (format!("{name} · not connected"), true);
        }
    }
    if !in_use.is_empty() {
        if let Some(cam) = listed.iter().find(|c| c.id == in_use) {
            return (cam.name.clone(), false);
        }
    }
    match choice {
        DeviceChoice::Auto | DeviceChoice::System => (
            listed
                .first()
                .map(|cam| cam.name.clone())
                .unwrap_or_else(|| "System default".to_owned()),
            false,
        ),
        DeviceChoice::Device { name, .. } => (name.clone(), false),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Input,
    Output,
    Camera,
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

/// Whether `choice` picks `camera` among `listed`.
fn camera_matches(choice: &DeviceChoice, camera: &CameraSource) -> bool {
    match choice {
        DeviceChoice::Device { id: Some(id), .. } => camera.id == *id,
        DeviceChoice::Device { name, id: None } => camera.name == *name,
        DeviceChoice::Auto | DeviceChoice::System => false,
    }
}

/// Camera radio rows for the device menu and camera preview popover: empty when `listed` is empty
/// (no CAMERA section drawn); otherwise "System default" first, then each listed camera, then a
/// disconnected chosen camera if it is missing from `listed`.
pub(super) fn camera_rows(choice: &DeviceChoice, listed: &[CameraSource], in_use: &str) -> Vec<Row> {
    if listed.is_empty() {
        return Vec::new();
    }
    let default_selected = matches!(choice, DeviceChoice::Auto | DeviceChoice::System);
    let mut out = vec![Row {
        choice: DeviceChoice::Auto,
        label: "System default".to_owned(),
        detail: listed.first().map(|cam| cam.name.clone()),
        chosen: default_selected,
        is_virtual: false,
    }];
    let mut matched = false;
    for camera in listed {
        let chosen = camera_matches(choice, camera);
        if chosen {
            matched = true;
        }
        let standing_in = !chosen && !in_use.is_empty() && camera.id == in_use && !default_selected;
        out.push(Row {
            choice: DeviceChoice::device(&camera.name, &camera.id),
            label: camera.name.clone(),
            detail: standing_in.then(|| "in use for now".to_owned()),
            chosen,
            is_virtual: false,
        });
    }
    if let DeviceChoice::Device { name, .. } = choice {
        if !matched {
            out.push(Row {
                choice: choice.clone(),
                label: name.clone(),
                detail: Some("not connected".to_owned()),
                chosen: true,
                is_virtual: false,
            });
        }
    }
    out
}

/// True when clicking Camera (while off) should open the live-preview popover before sending:
/// on first use in the session, or whenever more than one camera is available.
pub(super) fn should_preview_camera(cameras: &[CameraSource], used_before: bool) -> bool {
    !used_before || cameras.len() > 1
}

impl Shell {
    /// The popover that is open. Under `--shot`, the one the hook names.
    pub(super) fn call_menu(&self) -> Option<Menu> {
        #[cfg(feature = "gui-shot")]
        if let Some(menu) = super::shot::menu() {
            return Some(menu);
        }
        self.call_state.menu.clone()
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

    fn call_camera_preview_media(&self, on: bool, cx: &mut Context<Self>) {
        cx.background_executor()
            .spawn(async move {
                let _ = ask(&Request::Media(MediaRequest::CameraPreview { on }));
            })
            .detach();
    }

    /// Closes whatever popover is open, however it came to close.
    pub(super) fn call_menu_set(&mut self, menu: Option<Menu>, cx: &mut Context<Self>) {
        if self.call_state.sliding.is_some() && menu.is_none() {
            self.call_slide_end(cx);
        }
        let before = std::mem::replace(&mut self.call_state.menu, menu.clone());
        if before == menu {
            return;
        }
        let metered = |m: &Option<Menu>| m.as_ref().is_some_and(Menu::metered);
        if metered(&before) && !metered(&menu) {
            self.call_meter(false, cx);
        }
        if !metered(&before) && metered(&menu) {
            self.call_meter(true, cx);
        }
        if before == Some(Menu::CameraPreview) && menu != Some(Menu::CameraPreview) {
            self.call_camera_preview_media(false, cx);
        }
        if before != Some(Menu::CameraPreview) && menu == Some(Menu::CameraPreview) {
            self.call_camera_preview_media(true, cx);
        }
        if before == Some(Menu::Security) && menu != Some(Menu::Security) {
            self.call_state.security_more = false;
        }
        cx.notify();
    }

    pub(in crate::gui) fn call_menu_close(&mut self, cx: &mut Context<Self>) {
        if self.call_state.menu.is_some() {
            self.call_state.menu_closed = Some(Instant::now());
            self.call_menu_set(None, cx);
        }
    }

    /// Opens `menu`, or closes it when the same menu kind is already open.
    pub(super) fn call_menu_toggle(&mut self, menu: Menu, cx: &mut Context<Self>) {
        // The press that closed it from outside was on its own button: stay closed.
        let just_closed = self
            .call_state
            .menu_closed
            .take()
            // Only long enough to absorb the button's own click after mouse_down_out closed the
            // popover. 400ms also ate a deliberate reopen after dismissing, so the lock felt dead.
            .is_some_and(|at| at.elapsed() < Duration::from_millis(50));
        if self.call_state.menu.as_ref().is_some_and(|open| open.same_kind(&menu)) || just_closed {
            self.call_menu_set(None, cx);
        } else {
            self.call_menu_set(Some(menu), cx);
        }
    }

    /// Right-click shortcut on a microphone button (including the Tiny user panel): opens the
    /// focused Microphone picker when a call is active, else opens Audio settings.
    pub(in crate::gui) fn call_toggle_input_menu(&mut self, cx: &mut Context<Self>) {
        let has_call = self.snapshot.as_ref().and_then(|s| self.call_shown(s)).is_some();
        if has_call {
            self.call_menu_toggle(Menu::Input { from_hub: false }, cx);
        } else {
            settings::open(settings::Section::Audio, cx);
        }
    }

    /// Right-click shortcut on a speaker/deafen button (including the Tiny user panel): opens the
    /// focused Speaker picker when a call is active, else opens Audio settings.
    pub(in crate::gui) fn call_toggle_output_menu(&mut self, cx: &mut Context<Self>) {
        let has_call = self.snapshot.as_ref().and_then(|s| self.call_shown(s)).is_some();
        if has_call {
            self.call_menu_toggle(Menu::Output { from_hub: false }, cx);
        } else {
            settings::open(settings::Section::Audio, cx);
        }
    }

    /// Right-click or chevron click on the Camera button: opens the live preview picker when the
    /// camera is off and cameras exist, or the compact Camera switcher while the camera is live.
    pub(super) fn call_camera_picker_toggle(&mut self, call: &Call, camera_on: bool, cx: &mut Context<Self>) {
        if !camera_on && !call.cameras.is_empty() {
            self.call_menu_toggle(Menu::CameraPreview, cx);
        } else {
            self.call_menu_toggle(Menu::Camera { from_hub: false }, cx);
        }
    }

    /// Maps any device/camera/options `Menu` variant to its `(width, body)` popover content, shared
    /// between the sidebar dock and the call stage control bar.
    pub(super) fn call_device_popover_body(
        &self,
        call: &Call,
        menu: &Menu,
        cx: &mut Context<Self>,
    ) -> Option<(f32, Div)> {
        let joined = call.state == CallState::Joined;
        match menu {
            Menu::Devices => Some((300., self.call_devices_menu(call, cx).p(px(6.)))),
            Menu::Input { from_hub } => Some((320., self.call_input_menu(call, *from_hub, cx).p(px(6.)))),
            Menu::Output { from_hub } => Some((320., self.call_output_menu(call, *from_hub, cx).p(px(6.)))),
            Menu::Camera { from_hub } => Some((300., self.call_camera_menu(call, *from_hub, cx).p(px(6.)))),
            Menu::CameraPreview if joined => Some((284., self.call_camera_preview_menu(call, cx).p(px(8.)))),
            _ => None,
        }
    }

    /// Stores the choice (V1, V2, V22) as Settings would: chatd gives it to the call at once and
    /// keeps it for every call after.
    pub(super) fn call_pick(&mut self, side: Side, choice: DeviceChoice, cx: &mut Context<Self>) {
        let key = match side {
            Side::Input => INPUT,
            Side::Output => OUTPUT,
            Side::Camera => CAMERA,
        };
        let value = choice.value();
        let mut values = prefs::values();
        values.set(GLOBAL, key, value.clone());
        prefs::set_values(values);
        settings::redraw(cx);
        self.call_ask(Request::Set { scope: GLOBAL.to_owned(), key: key.to_owned(), value }, cx);
        cx.notify();
    }

    /// Handles clicking the Camera button in the dock or call view bar.
    pub(super) fn call_camera_clicked(&mut self, call: &Call, camera_on: bool, cx: &mut Context<Self>) {
        if camera_on {
            self.call_menu_set(None, cx);
            self.call_ask(Request::Media(MediaRequest::Camera { on: false }), cx);
            return;
        }
        if should_preview_camera(&call.cameras, self.call_state.camera_used) {
            self.call_menu_toggle(Menu::CameraPreview, cx);
        } else {
            self.call_state.camera_used = true;
            self.call_ask(Request::Media(MediaRequest::Camera { on: true }), cx);
        }
    }

    fn call_device_row(&self, side: Side, ix: usize, row: Row, cx: &mut Context<Self>) -> impl IntoElement {
        let group = match side {
            Side::Input => "call-input",
            Side::Output => "call-output",
            Side::Camera => "call-camera",
        };
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
                .px(px(5.))
                .py(px(1.))
                .rounded(px(4.))
                .border_1()
                .border_color(theme::hairline())
                .type_micro()
                .text_color(theme::text_faint())
                .child("virtual")
        });
        let is_meta = matches!(row.choice, DeviceChoice::Auto | DeviceChoice::System);
        let choice = row.choice.clone();
        let base = div()
            .id((group, ix))
            .flex_none()
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .type_ui()
            .text_color(if row.chosen { theme::text() } else { theme::text_soft() })
            .hover(|style| style.bg(theme::hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.call_pick(side, choice.clone(), cx);
            }));

        if is_meta {
            if let Some(detail) = row.detail {
                return base
                    .min_h(px(38.))
                    .py(px(4.))
                    .child(radio)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .type_ui()
                                    .when(row.chosen, |d| d.font_weight(FontWeight::MEDIUM))
                                    .child(SharedString::from(row.label)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .type_micro()
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme::text_faint())
                                    .child(detail),
                            ),
                    );
            }
        }

        let status_pill = row.detail.map(|detail| {
            let (label, fg, bg) = if detail == "not connected" {
                ("not connected", theme::warning(), theme::warning_soft())
            } else if detail == "in use for now" {
                ("in use", theme::accent_text(), theme::accent_soft())
            } else {
                ("", theme::text_faint(), theme::hover())
            };
            let text: SharedString = if label.is_empty() { detail.into() } else { label.into() };
            div()
                .flex_none()
                .px(px(5.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(bg)
                .type_micro()
                .text_color(fg)
                .child(text)
        });

        base.h(px(30.))
            .child(radio)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(row.chosen, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(SharedString::from(row.label)),
            )
            .children(tag)
            .children(status_pill)
    }

    fn call_picker_header(&self, title: &'static str, from_hub: bool, cx: &mut Context<Self>) -> Div {
        let back = from_hub.then(|| {
            div()
                .id("call-device-back")
                .flex_none()
                .size(px(22.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(glyph("icons/arrow-left.svg", 13., theme::text_soft()))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_set(Some(Menu::Devices), cx);
                }))
        });
        div()
            .h(px(26.))
            .flex_none()
            .px(px(6.))
            .mb(px(2.))
            .flex()
            .items_center()
            .gap(px(6.))
            .children(back)
            .child(
                div()
                    .type_small()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_soft())
                    .child(title),
            )
    }

    fn call_device_section(&self, side: Side, call: &Call, cx: &mut Context<Self>) -> Div {
        let (scroll_id, listed, in_use, key) = match side {
            Side::Input => ("call-input-list", &call.inputs, &call.input_id, INPUT),
            Side::Output => ("call-output-list", &call.outputs, &call.output_id, OUTPUT),
            Side::Camera => unreachable!("camera section uses call_camera_menu"),
        };
        let choice = prefs::values().device_choice(key);
        let heading = |words: &'static str| {
            div()
                .h(px(20.))
                .flex_none()
                .px(px(8.))
                .mt(px(4.))
                .flex()
                .items_center()
                .type_micro()
                .text_color(theme::text_faint())
                .child(words)
        };
        let mut list = div()
            .id(scroll_id)
            .max_h(px(LIST_MAX_H))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(1.));
        let mut virtual_heading = false;
        for (ix, row) in rows(&choice, listed, in_use).into_iter().enumerate() {
            if row.is_virtual && !virtual_heading {
                virtual_heading = true;
                list = list.child(heading("VIRTUAL · only when picked"));
            }
            list = list.child(self.call_device_row(side, ix, row, cx));
        }
        div().flex().flex_col().child(list)
    }

    /// Focused Microphone picker (opened via right-click/chevron on Mic, or from the hub).
    pub(super) fn call_input_menu(&self, call: &Call, from_hub: bool, cx: &mut Context<Self>) -> Div {
        let notice = call
            .device_notice
            .as_deref()
            .filter(|words| notice_applies_to(words, Side::Input))
            .and_then(|_| self.call_device_notice_row(call));
        let settings = menu_item("call-input-settings", Some("icons/settings.svg"), "Audio settings…", None)
            .on_click(cx.listener(|this, _, _, cx| {
                this.call_menu_close(cx);
                settings::open(settings::Section::Audio, cx);
            }));
        div()
            .flex()
            .flex_col()
            .child(self.call_picker_header("MICROPHONE", from_hub, cx))
            .children(notice)
            .child(self.call_device_section(Side::Input, call, cx))
            .child(div().flex_none().mt(px(2.)).child(self.call_level(call, Side::Input)))
            .child(menu_separator().mx(px(8.)))
            .child(settings)
    }

    /// Focused Speaker picker (opened via right-click/chevron on Headphones, or from the hub).
    pub(super) fn call_output_menu(&self, call: &Call, from_hub: bool, cx: &mut Context<Self>) -> Div {
        let notice = call
            .device_notice
            .as_deref()
            .filter(|words| notice_applies_to(words, Side::Output))
            .and_then(|_| self.call_device_notice_row(call));
        let test = menu_item("call-output-test", Some("icons/volume-2.svg"), "Test speakers", None)
            .on_click(cx.listener(|this, _, _, cx| {
                this.call_ask(Request::TestSound, cx);
            }));
        let settings = menu_item("call-output-settings", Some("icons/settings.svg"), "Audio settings…", None)
            .on_click(cx.listener(|this, _, _, cx| {
                this.call_menu_close(cx);
                settings::open(settings::Section::Audio, cx);
            }));
        div()
            .flex()
            .flex_col()
            .child(self.call_picker_header("SPEAKER", from_hub, cx))
            .children(notice)
            .child(self.call_device_section(Side::Output, call, cx))
            .child(div().flex_none().mt(px(2.)).child(self.call_level(call, Side::Output)))
            .child(menu_separator().mx(px(8.)))
            .child(test)
            .child(settings)
    }

    /// Focused Camera picker (opened via right-click/chevron on live Camera, or from the hub).
    pub(super) fn call_camera_menu(&self, call: &Call, from_hub: bool, cx: &mut Context<Self>) -> Div {
        let choice = prefs::values().device_choice(CAMERA);
        let items = camera_rows(&choice, &call.cameras, &call.camera_id);
        let list = if items.is_empty() {
            div()
                .px(px(8.))
                .py(px(8.))
                .type_small()
                .text_color(theme::text_faint())
                .child("No cameras detected")
        } else {
            let mut scroll = div()
                .id("call-camera-list")
                .max_h(px(LIST_MAX_H))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(1.));
            for (ix, row) in items.into_iter().enumerate() {
                scroll = scroll.child(self.call_device_row(Side::Camera, ix, row, cx));
            }
            div().flex().flex_col().child(scroll)
        };
        let settings = menu_item("call-camera-settings", Some("icons/settings.svg"), "Video settings…", None)
            .on_click(cx.listener(|this, _, _, cx| {
                this.call_menu_close(cx);
                settings::open(settings::Section::Audio, cx);
            }));
        div()
            .flex()
            .flex_col()
            .child(self.call_picker_header("CAMERA", from_hub, cx))
            .child(list)
            .child(menu_separator().mx(px(8.)))
            .child(settings)
    }

    fn call_device_summary_row(
        &self,
        id: &'static str,
        icon: &'static str,
        title: &'static str,
        summary: String,
        warning: bool,
        target: Menu,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(42.))
            .flex_none()
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.call_menu_set(Some(target.clone()), cx);
            }))
            .child(glyph(icon, 16., if warning { theme::warning() } else { theme::text_soft() }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .type_ui()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .type_micro()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(if warning { theme::warning() } else { theme::text_faint() })
                            .child(summary),
                    ),
            )
            .child(glyph("icons/chevron-right.svg", 14., theme::text_faint()))
    }

    /// Small popover above the Camera button: live preview of the chosen camera, camera list, and
    /// "Turn on camera".
    pub(super) fn call_camera_preview_menu(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        let has_cameras = !call.cameras.is_empty();
        let preview = if !has_cameras {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(glyph("icons/video-off.svg", 22., theme::on_media_soft()))
                .child(div().type_small().text_color(theme::on_media_soft()).child("No camera found"))
                .into_any_element()
        } else {
            match self.call_self_frame(Some(call), false) {
                Some(frame) => img(frame)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .child(glyph("icons/video.svg", 22., theme::on_media_soft()))
                    .child(div().type_small().text_color(theme::on_media_soft()).child("Starting camera preview…"))
                    .into_any_element(),
            }
        };
        let choice = prefs::values().device_choice(CAMERA);
        let items = camera_rows(&choice, &call.cameras, &call.camera_id);
        let mut list = div()
            .id("call-camera-preview-list")
            .max_h(px(220.))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        for (ix, row) in items.into_iter().enumerate() {
            list = list.child(self.call_device_row(Side::Camera, ix, row, cx));
        }
        let go = if has_cameras {
            button_with_icon("call-camera-preview-on", ButtonKind::Primary, "icons/video.svg", "Turn on camera")
                .h(px(32.))
                .w_full()
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_state.camera_used = true;
                    this.call_menu_set(None, cx);
                    this.call_ask(Request::Media(MediaRequest::Camera { on: true }), cx);
                }))
                .into_any_element()
        } else {
            button_disabled("Turn on camera")
                .h(px(32.))
                .w_full()
                .into_any_element()
        };
        div().child(
            div()
                .id("call-camera-preview-menu")
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .w_full()
                        .h(px(150.))
                        .rounded(px(8.))
                        .bg(theme::media_stage())
                        .border_1()
                        .border_color(theme::hairline())
                        .overflow_hidden()
                        .child(preview),
                )
                .when(has_cameras, |box_div| box_div.child(list))
                .child(go),
        )
    }

    /// Twelve segments and what they measure. The microphone's stays live while muted so it can
    /// be tried, and says that nobody hears it. Before the call is up nothing is open, and the
    /// meters say so by staying dark.
    pub(super) fn call_level(&self, call: &Call, side: Side) -> Div {
        let (level, words) = match side {
            Side::Input => {
                let words = match (in_call(call), call.muted || call.deafened) {
                    (false, _) => "Microphone level, once the call connects",
                    (true, true) => "Muted · speak to test",
                    (true, false) => "Speak to test your microphone",
                };
                (call.level, words)
            }
            Side::Output => {
                let words = if in_call(call) { "What you hear" } else { "Speaker level, once the call connects" };
                (call.output_level, words)
            }
            Side::Camera => unreachable!("camera has no audio meter"),
        };
        let lit = lit(level);
        let gate = prefs::values().number(protocol::settings::SENSITIVITY) as u8;
        let first_sent = (u16::from(gate.min(100)) * u16::from(SEGMENTS)).div_ceil(100) as u8;
        let clip_from = (u16::from(95u8) * u16::from(SEGMENTS)).div_ceil(100) as u8;
        let segment = |ix: u8| {
            let color = if ix >= lit {
                theme::selected()
            } else if side == Side::Input && ix >= clip_from {
                theme::danger()
            } else if side == Side::Input && ix < first_sent {
                theme::warning()
            } else {
                theme::positive()
            };
            div().flex_none().w(px(4.)).h(px(12.)).rounded(px(1.)).bg(color)
        };
        div()
            .h(px(26.))
            .flex_none()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex_none().flex().gap(px(2.)).children((0..SEGMENTS).map(segment)))
            .child(div().min_w_0().truncate().type_small().text_color(theme::text_soft()).child(words))
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
                .type_small()
                .text_color(color)
                .child(words),
        )
    }

    /// "Switch device", after the dock's line when a device stays silent: opens the relevant picker.
    pub(super) fn call_switch_device(&self, call: &Call, live: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !live || !call.device_stuck {
            return None;
        }
        let target = call
            .device_notice
            .as_deref()
            .map(stuck_device_menu)
            .unwrap_or(Menu::Devices);
        Some(
            div()
                .id("call-switch-device")
                .flex_none()
                .cursor_pointer()
                .underline()
                .text_color(theme::warning())
                .child("Switch device")
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_set(Some(target.clone()), cx);
                }))
                .into_any_element(),
        )
    }

    /// Compact Call Options hub: active device summary rows (each drills into its focused picker),
    /// 1:1 peer volume when applicable, speaker test, Discord voice actions, and Call settings.
    pub(super) fn call_devices_menu(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        let values = prefs::values();
        let (mic_summary, mic_warn) =
            active_device_summary(&values.device_choice(INPUT), &call.inputs, &call.input_id);
        let (spk_summary, spk_warn) =
            active_device_summary(&values.device_choice(OUTPUT), &call.outputs, &call.output_id);
        let cam_row = (!call.cameras.is_empty()).then(|| {
            let (cam_summary, cam_warn) =
                active_camera_summary(&values.device_choice(CAMERA), &call.cameras, &call.camera_id);
            self.call_device_summary_row(
                "call-hub-camera",
                "icons/video.svg",
                "Camera",
                cam_summary,
                cam_warn,
                Menu::Camera { from_hub: true },
                cx,
            )
        });
        let row = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref())
            .filter(|row| row.key == call.conversation).cloned()
            .or_else(|| self.sidebar_state.find(&self.trees, &call.conversation));
        let region = (call.conversation.network == protocol::Network::Discord
            && call.state == protocol::CallState::Joined
            && row.as_ref().is_some_and(|row| row.kind.is_direct())).then(|| {
            let key = call.conversation.clone();
            menu_item("call-change-region", Some("icons/info.svg"), "Change region…", None)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.call_menu_close(cx);
                    this.open_discord_voice_settings(key.clone(), true, cx);
                }))
        });
        let edit_status = row.filter(|row| {
            super::super::sidebar::can_set_discord_voice_status(row, call.state == protocol::CallState::Joined)
        }).map(|row| {
            menu_item("call-set-voice-status", Some("icons/pencil.svg"), "Set channel status…", None)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.call_menu_close(cx);
                    this.open_voice_status(&row, cx);
                }))
        });
        let test = menu_item("call-test-sound", Some("icons/volume-2.svg"), "Test speakers", None).on_click(cx.listener(|this, _, _, cx| {
            this.call_ask(Request::TestSound, cx);
        }));
        let settings = menu_item("call-settings", Some("icons/settings.svg"), "Call settings…", None).on_click(cx.listener(|this, _, _, cx| {
            this.call_menu_close(cx);
            settings::open(settings::Section::Audio, cx);
        }));
        let peer_volume = (call.participants.len() == 1).then(|| {
            let peer = &call.participants[0];
            div()
                .flex()
                .flex_col()
                .child(menu_separator().mx(px(8.)))
                .child(crate::gui::components::menu_label(format!("Volume · {}", peer.name)))
                .child(self.call_volume_controls(call.conversation.account(), &peer.id, cx))
        });
        div()
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| this.call_track(event, cx)))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.call_slide_end(cx)))
            .children(self.call_device_notice_row(call))
            .child(self.call_device_summary_row(
                "call-hub-input",
                "icons/mic.svg",
                "Microphone",
                mic_summary,
                mic_warn,
                Menu::Input { from_hub: true },
                cx,
            ))
            .child(self.call_device_summary_row(
                "call-hub-output",
                "icons/headphones.svg",
                "Speaker",
                spk_summary,
                spk_warn,
                Menu::Output { from_hub: true },
                cx,
            ))
            .children(cam_row)
            .children(peer_volume)
            .child(menu_separator().mx(px(8.)))
            .child(test)
            .children(edit_status)
            .children(region)
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

    #[test]
    fn camera_rows_are_empty_without_cameras_and_list_system_default_first_when_present() {
        assert!(camera_rows(&DeviceChoice::Auto, &[], "").is_empty());
        let cams = vec![
            CameraSource { id: "cam-a".into(), name: "Built-in Camera".into(), thumbnail: String::new() },
            CameraSource { id: "cam-b".into(), name: "Desk USB Camera".into(), thumbnail: String::new() },
        ];
        let default_rows = camera_rows(&DeviceChoice::Auto, &cams, "cam-a");
        assert_eq!(
            labels(&default_rows),
            [
                ("System default", Some("Built-in Camera"), true),
                ("Built-in Camera", None, false),
                ("Desk USB Camera", None, false),
            ]
        );
        let picked = DeviceChoice::device("Desk USB Camera", "cam-b");
        let picked_rows = camera_rows(&picked, &cams, "cam-b");
        assert_eq!(
            picked_rows.iter().filter(|row| row.chosen).map(|row| row.label.as_str()).collect::<Vec<_>>(),
            ["Desk USB Camera"]
        );
        // Unplugged camera stays marked as not connected while the stand-in says so.
        let unplugged = camera_rows(&picked, &cams[..1], "cam-a");
        assert_eq!(
            unplugged.last().map(|r| (r.label.as_str(), r.detail.as_deref(), r.chosen)),
            Some(("Desk USB Camera", Some("not connected"), true))
        );
        assert_eq!(unplugged[1].detail.as_deref(), Some("in use for now"));
    }

    #[test]
    fn camera_preview_opens_on_first_use_or_multiple_cameras_and_skips_with_one_used_camera() {
        let one = vec![CameraSource { id: "cam-a".into(), name: "Built-in".into(), thumbnail: String::new() }];
        let two = vec![
            one[0].clone(),
            CameraSource { id: "cam-b".into(), name: "External".into(), thumbnail: String::new() },
        ];
        assert!(should_preview_camera(&one, false), "first use previews before sending");
        assert!(!should_preview_camera(&one, true), "single camera after first use turns on at once");
        assert!(should_preview_camera(&two, true), "multiple cameras always offer the picker and preview");
    }

    #[test]
    fn active_device_summary_flags_disconnected_and_resolves_in_use_or_auto() {
        assert_eq!(active_device_summary(&DeviceChoice::Auto, &listed(), "hs55"), (HEADSET.to_owned(), false));
        assert_eq!(
            active_device_summary(&DeviceChoice::System, &listed(), ""),
            ("Krisp Speaker (Krisp Audio)".to_owned(), false)
        );
        let picked = DeviceChoice::device("Microphone (HyperX SoloCast 2)", "hyperx");
        assert_eq!(
            active_device_summary(&picked, &listed(), "usb"),
            ("Microphone (HyperX SoloCast 2) · not connected".to_owned(), true)
        );
    }

    #[test]
    fn active_camera_summary_flags_disconnected_and_resolves_active() {
        let cams = vec![
            CameraSource { id: "cam-a".into(), name: "Lyra-CAM_36HW".into(), thumbnail: String::new() },
            CameraSource { id: "cam-b".into(), name: "Pixel 10 Pro XL".into(), thumbnail: String::new() },
        ];
        assert_eq!(active_camera_summary(&DeviceChoice::Auto, &cams, ""), ("Lyra-CAM_36HW".to_owned(), false));
        assert_eq!(
            active_camera_summary(&DeviceChoice::device("Pixel 10 Pro XL", "cam-b"), &cams, "cam-b"),
            ("Pixel 10 Pro XL".to_owned(), false)
        );
        assert_eq!(
            active_camera_summary(&DeviceChoice::device("Missing Cam", "gone"), &cams, "cam-a"),
            ("Missing Cam · not connected".to_owned(), true)
        );
    }

    #[test]
    fn stuck_device_menu_routes_microphone_and_speaker_notices_to_focused_pickers() {
        assert_eq!(
            stuck_device_menu("Microphone (HyperX SoloCast 2) isn't connected. Using fallback."),
            Menu::Input { from_hub: false }
        );
        assert_eq!(
            stuck_device_menu("Speaker (Headset Earphone) is silent."),
            Menu::Output { from_hub: false }
        );
        assert_eq!(stuck_device_menu("Device switched."), Menu::Devices);
        assert!(notice_applies_to("Microphone isn't connected", Side::Input));
        assert!(!notice_applies_to("Microphone isn't connected", Side::Output));
        assert!(notice_applies_to("Speaker isn't connected", Side::Output));
        assert!(!notice_applies_to("Speaker isn't connected", Side::Input));
    }

    #[test]
    fn menu_metered_and_same_kind_cover_focused_device_pickers() {
        assert!(Menu::Input { from_hub: false }.metered());
        assert!(Menu::Output { from_hub: true }.metered());
        assert!(Menu::Devices.metered());
        assert!(!Menu::Camera { from_hub: false }.metered());
        assert!(Menu::Input { from_hub: false }.same_kind(&Menu::Input { from_hub: true }));
        assert!(Menu::Output { from_hub: false }.same_kind(&Menu::Output { from_hub: true }));
        assert!(!Menu::Input { from_hub: false }.same_kind(&Menu::Output { from_hub: false }));
    }
}
