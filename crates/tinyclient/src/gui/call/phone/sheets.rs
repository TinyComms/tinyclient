//! What opens over the call screen: the people page (pushed, as the desktop's participant list
//! is on a phone), and bottom sheets for audio and devices (the desktop's device menu, 07 §7),
//! More, the call's protection, and a refused camera.

use super::hooks::{self, Permission};
use super::{in_call, sending, strip_button, Shell, Sheet};
use crate::gui::call::devices::{rows, Side};
use crate::gui::call::{ringed, Subject};
use crate::gui::components::{glyph, Avatar};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::phone::{sheet, sheet_title};
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, MouseButton, SharedString, Stateful};
use protocol::settings::{DeviceChoice, ECHO_CANCELLATION, GLOBAL, INPUT, NOISE_SUPPRESSION, PUSH_TO_TALK};
use protocol::{Call, Content, ConversationKind, Key, Request};

/// A row of a sheet: a glyph on its plate, the label and what it says now, then what is at its
/// right (a switch, a check, a chevron).
fn row(id: impl Into<SharedString>, icon: &'static str, label: impl Into<SharedString>, detail: Option<String>, trailing: Option<AnyElement>) -> Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .min_h(px(56.))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .active(|style| style.bg(theme::pressed()))
        .child(div().flex_none().size(px(36.)).rounded_full().bg(theme::hover()).flex().items_center().justify_center().child(glyph(icon, 18., theme::text())))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().phone_row().text_color(theme::text()).child(label.into()))
                .children(detail.map(|detail| div().truncate().phone_stamp().font_weight(theme::REGULAR).text_color(theme::text_faint()).child(detail))),
        )
        .children(trailing)
}

/// A switch, 46 x 28: on is green with the knob at the right.
fn switch(on: bool) -> AnyElement {
    div()
        .flex_none()
        .w(px(46.))
        .h(px(28.))
        .rounded_full()
        .p(px(3.))
        .bg(if on { theme::positive() } else { theme::selected() })
        .flex()
        .when(on, |track| track.justify_end())
        .child(div().size(px(22.)).rounded_full().bg(gpui::rgb(0xffffff)))
        .into_any_element()
}

/// A radio mark: filled when chosen.
fn radio(chosen: bool) -> AnyElement {
    let mark = div().flex_none().size(px(22.)).rounded_full().flex().items_center().justify_center();
    if chosen {
        mark.bg(theme::accent()).child(div().size(px(8.)).rounded_full().bg(theme::on_accent())).into_any_element()
    } else {
        mark.border_2().border_color(theme::text_faint()).into_any_element()
    }
}

fn chevron() -> AnyElement {
    glyph("icons/chevron-right.svg", 18., theme::text_faint()).into_any_element()
}

/// A section's label in a sheet.
fn label(words: &'static str) -> Div {
    div().h(px(32.)).px(px(10.)).flex().items_end().pb(px(6.)).phone_stamp().text_color(theme::text_faint()).child(words)
}

impl Shell {
    /// Stores a switch of Settings › Voice as Settings would, and tells chatd.
    fn call_phone_switch(&mut self, key: &'static str, on: bool, cx: &mut Context<Self>) {
        let value = Some(if on { "on" } else { "off" }.to_owned());
        let mut values = crate::gui::app::prefs::values();
        values.set(GLOBAL, key, value.clone());
        crate::gui::app::prefs::set_values(values);
        crate::gui::settings::redraw(cx);
        self.call_ask(Request::Set { scope: GLOBAL.to_owned(), key: key.to_owned(), value }, cx);
        cx.notify();
    }

    /// The sheet `which` over the call screen.
    pub(super) fn call_phone_sheet_view(&self, which: Sheet, call: &Call, subject: &Subject, tall: f32, cx: &mut Context<Self>) -> Div {
        let this = cx.weak_entity();
        let close = move |_: &gpui::MouseDownEvent, _: &mut gpui::Window, app: &mut gpui::App| {
            let _ = this.update(app, |shell, cx| shell.call_phone_close_sheet(cx));
        };
        let body: AnyElement = match which {
            Sheet::More => self.call_phone_more(call, subject, cx).into_any_element(),
            Sheet::Devices => self.call_phone_devices(call, cx).into_any_element(),
            Sheet::Security => div().child(sheet_title("Call security")).child(self.call_security_panel(call, cx)).into_any_element(),
            Sheet::Camera => self.call_phone_camera_denied(cx).into_any_element(),
        };
        sheet("call-phone-sheet", tall, body, close)
    }

    /// Everything that is not in the bar.
    fn call_phone_more(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Div {
        let values = crate::gui::app::prefs::values();
        let (push, noise, echo) = (values.on(PUSH_TO_TALK), values.on(NOISE_SUPPRESSION), values.on(ECHO_CANCELLATION));
        let group = subject.kind != ConversationKind::Direct && in_call(call);
        let route = hooks::route(if sending(call, Content::Camera) { hooks::Route::Speaker } else { hooks::Route::Earpiece });
        let mic = if call.input.is_empty() { "Microphone".to_owned() } else { call.input.clone() };
        let people = group.then(|| {
            row("call-phone-more-people", "icons/users.svg", "People", Some(format!("{} in the call", call.participants.len() + 1)), Some(chevron())).on_click(cx.listener(|this, _, _, cx| {
                this.call_phone_close_sheet(cx);
                this.call_state.phone.people = true;
                cx.notify();
            }))
        });
        let security = call.security.as_ref().map(|security| {
            let words = super::super::security::badge_words(super::super::security::badge(security));
            row("call-phone-more-security", "icons/lock.svg", "Call security", Some(words.to_owned()), Some(chevron())).on_click(cx.listener(|this, _, _, cx| this.call_phone_open_sheet(Sheet::Security, cx)))
        });
        div()
            .child(sheet_title(subject.title.clone()))
            .children(people)
            .child(
                row("call-phone-more-audio", "icons/settings.svg", "Audio and devices", Some(format!("{} · {mic}", route.label())), Some(chevron()))
                    .on_click(cx.listener(|this, _, _, cx| this.call_phone_open_sheet(Sheet::Devices, cx))),
            )
            .children(security)
            .child(label("VOICE"))
            .child(
                row("call-phone-more-ptt", "icons/hand.svg", "Push to talk", Some(if push { "Hold the button to talk".into() } else { "Your microphone opens when you speak".into() }), Some(switch(push)))
                    .on_click(cx.listener(move |this, _, _, cx| this.call_phone_switch(PUSH_TO_TALK, !push, cx))),
            )
            .child(
                row("call-phone-more-noise", "icons/audio-lines.svg", "Noise suppression", Some("Quiets fans, traffic and keyboards".into()), Some(switch(noise)))
                    .on_click(cx.listener(move |this, _, _, cx| this.call_phone_switch(NOISE_SUPPRESSION, !noise, cx))),
            )
            .child(
                row("call-phone-more-echo", "icons/volume-2.svg", "Echo cancellation", Some(if echo { "On: no headphones needed".into() } else { "Off: wear headphones".into() }), Some(switch(echo)))
                    .on_click(cx.listener(move |this, _, _, cx| this.call_phone_switch(ECHO_CANCELLATION, !echo, cx))),
            )
            .child(div().h(px(8.)))
            .child(row("call-phone-more-settings", "icons/settings.svg", "Call settings", None, Some(chevron())).on_click(cx.listener(|this, _, _, cx| {
                this.call_phone_close_sheet(cx);
                crate::gui::settings::open(crate::gui::settings::Section::Audio, cx);
            })))
    }

    /// Where the sound plays and which microphone hears me, with the microphone's level.
    fn call_phone_devices(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        let route = hooks::route(if sending(call, Content::Camera) || call.conversation.network == protocol::Network::Tiny { hooks::Route::Speaker } else { hooks::Route::Earpiece });
        let routes = hooks::routes().into_iter().map(|each| {
            row(format!("call-phone-route-{}", each.label()), each.icon(), each.label(), None, Some(radio(each == route))).on_click(cx.listener(move |_, _, _, cx| {
                hooks::choose_route(each);
                cx.notify();
            }))
        });
        let choice = crate::gui::app::prefs::values().device_choice(INPUT);
        let inputs: Vec<Stateful<Div>> = if call.inputs.is_empty() {
            vec![row("call-phone-input-phone", "icons/mic.svg", "This phone's microphone", None, Some(radio(true)))]
        } else {
            rows(&choice, &call.inputs, &call.input_id)
                .into_iter()
                .enumerate()
                .map(|(ix, each)| {
                    let pick: DeviceChoice = each.choice.clone();
                    let detail = each.detail.clone().or_else(|| each.is_virtual.then(|| "Virtual, only when picked".to_owned()));
                    row(format!("call-phone-input-{ix}"), "icons/mic.svg", each.label.clone(), detail, Some(radio(each.chosen)))
                        .on_click(cx.listener(move |this, _, _, cx| this.call_pick(Side::Input, pick.clone(), cx)))
                })
                .collect()
        };
        div()
            .child(sheet_title("Audio"))
            .child(label("PLAYS THROUGH"))
            .children(routes)
            .child(label("MICROPHONE"))
            .children(inputs)
            .child(div().px(px(2.)).py(px(4.)).child(self.call_level(call, Side::Input)))
            .child(div().h(px(8.)))
            .child(row("call-phone-test", "icons/volume-2.svg", "Test speakers", Some("Plays the join sound".into()), None).on_click(cx.listener(|this, _, _, cx| this.call_ask(Request::TestSound, cx))))
    }

    /// The camera was refused: the desktop's words for a refused microphone, for the camera.
    fn call_phone_camera_denied(&self, cx: &mut Context<Self>) -> Div {
        div()
            .px(px(12.))
            .pt(px(12.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .child(div().size(px(64.)).rounded_full().bg(theme::warning_soft()).flex().items_center().justify_center().child(glyph("icons/video-off.svg", 28., theme::warning())))
            .child(div().mt(px(6.)).text_center().text_size(px(tokens::scaled(20.))).line_height(px(tokens::scaled(26.))).font_weight(theme::SEMIBOLD).child("tinyclient can't use the camera"))
            .child(
                div()
                    .max_w(px(300.))
                    .text_center()
                    .whitespace_normal()
                    .phone_row()
                    .text_color(theme::text_soft())
                    .child("Allow camera access for tinyclient in Settings, then turn your camera on. Your voice keeps going meanwhile."),
            )
            .child(
                div()
                    .w_full()
                    .mt(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(super::screens::wide("call-phone-camera-settings", "Open Settings", true).on_click(cx.listener(|this, _, _, cx| {
                        hooks::open_system_settings(Permission::Camera);
                        this.call_phone_close_sheet(cx);
                    })))
                    .child(super::screens::wide("call-phone-camera-later", "Not now", false).on_click(cx.listener(|this, _, _, cx| this.call_phone_close_sheet(cx)))),
            )
    }

    /// Who is in the call, pushed over the screen: me first, then everyone by name, each with
    /// what they are doing in words and in glyphs.
    pub(super) fn call_phone_people(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Stateful<Div> {
        struct Who {
            key: Key,
            name: String,
            face: Option<std::path::PathBuf>,
            me: bool,
            speaking: bool,
            muted: bool,
            deafened: bool,
            camera: bool,
            sharing: bool,
        }
        let (my_name, my_face) = self.call_phone_me();
        let mut people = vec![Who {
            key: Key { id: "me".into(), ..call.conversation.clone() },
            name: my_name,
            face: my_face,
            me: true,
            speaking: call.speaking && !call.muted && !call.deafened,
            muted: call.muted || call.deafened,
            deafened: call.deafened,
            camera: sending(call, Content::Camera),
            sharing: sending(call, Content::Share),
        }];
        let mut others: Vec<Who> = call
            .participants
            .iter()
            .map(|who| Who {
                key: Key { id: who.id.clone(), ..call.conversation.clone() },
                name: who.name.clone(),
                face: self.call_phone_face(call, subject, &who.id),
                me: false,
                speaking: who.speaking,
                muted: who.muted || who.server_muted,
                deafened: who.deafened || who.server_deafened,
                camera: who.camera,
                sharing: who.sharing,
            })
            .collect();
        others.sort_by_key(|who| who.name.to_lowercase());
        people.extend(others);
        let count = people.len();
        let rows = people.into_iter().map(|who| {
            let state = if who.sharing {
                ("Sharing their screen", theme::accent_text())
            } else if who.speaking {
                ("Speaking", theme::positive())
            } else if who.deafened {
                ("Can't hear", theme::text_faint())
            } else if who.muted {
                ("Muted", theme::text_faint())
            } else if who.camera {
                ("Camera on", theme::text_faint())
            } else {
                ("In the call", theme::text_faint())
            };
            let name = if who.me { format!("{} (you)", who.name) } else { who.name.clone() };
            div()
                .min_h(px(64.))
                .px(px(tokens::GUTTER))
                .flex()
                .items_center()
                .gap(px(14.))
                .child(ringed(Avatar::new(&who.key, &who.name, 40.).image_at(who.face).build(), 40., who.speaking))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().phone_row().font_weight(theme::MEDIUM).child(name))
                        .child(div().truncate().phone_stamp().font_weight(theme::REGULAR).text_color(state.1).child(state.0)),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(10.))
                        .children(who.camera.then(|| glyph("icons/video.svg", 18., theme::accent_text())))
                        .children(who.sharing.then(|| glyph("icons/monitor-up.svg", 18., theme::accent_text())))
                        .children(who.deafened.then(|| glyph("icons/headphone-off.svg", 18., theme::text_faint())))
                        .children(who.muted.then(|| glyph("icons/mic-off.svg", 18., theme::danger()))),
                )
        });
        let place = subject.space.clone().map(|space| format!("{} · {space}", subject.title)).unwrap_or_else(|| subject.title.clone());
        div()
            .id("call-phone-people")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex_none().h(px(tokens::safe_top())))
            .child(
                div()
                    .flex_none()
                    .h(px(tokens::NAV))
                    .pl(px(4.))
                    .pr(px(tokens::GUTTER))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(strip_button("call-phone-people-back", "icons/chevron-left.svg").on_click(cx.listener(|this, _, _, cx| {
                        this.call_state.phone.people = false;
                        cx.notify();
                    })))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(div().flex_none().phone_title().child("People"))
                            .child(div().min_w_0().truncate().phone_preview().text_color(theme::text_faint()).child(format!("{count} · {place}"))),
                    ),
            )
            .child(div().id("call-phone-people-rows").flex_1().min_h_0().overflow_y_scroll().py(px(6.)).children(rows))
            .child(div().flex_none().h(px(tokens::safe_bottom())))
    }
}
