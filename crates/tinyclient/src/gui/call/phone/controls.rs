//! The bottom bar of the call screen: round buttons a thumb can hit, each with its word under
//! it, so a state never rests on color alone. The set is the desktop's (07 §2): mute, deafen,
//! camera and share on a Tiny room only, devices, Leave; plus what only a phone has, the camera's
//! flip and where the sound plays. A control a network cannot do is absent, as on the desktop;
//! one that cannot work now says why in its word.

use super::hooks::{self, Permission, Route};
use super::{in_call, pictures_network, round, sending, Look, Shell, Sheet, CONTROL_GAP};
use crate::gui::call::Subject;
use crate::gui::phone::tokens;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, MouseButton};
use protocol::settings::PUSH_TO_TALK;
use protocol::{Call, CallState, Content, ConversationKind, MediaRequest, Request};

/// The round buttons of the bar.
const SIZE: f32 = super::BUTTON;

/// Why the camera cannot be turned on here, when it cannot.
fn camera_unavailable() -> Option<&'static str> {
    // 06 §Pictures: a camera is sent from Windows, and from a phone once its platform does.
    let desktop_without = cfg!(any(target_os = "macos", target_os = "linux")) && !cfg!(any(target_os = "android", target_os = "ios"));
    desktop_without.then_some("A camera isn't available on this system yet. You can still share your screen.")
}

impl Shell {
    /// How many rows of buttons the bar has: two when the call carries pictures.
    pub(super) fn call_phone_control_rows(&self, call: &Call, _subject: &Subject) -> usize {
        if pictures_network(call) && in_call(call) {
            2
        } else {
            1
        }
    }

    /// Where the sound plays now: what was chosen, else the speaker for a call with my camera
    /// on, else the earpiece.
    fn call_phone_route(&self, call: &Call) -> Route {
        let fallback = if sending(call, Content::Camera) || call.conversation.network == protocol::Network::Tiny { Route::Speaker } else { Route::Earpiece };
        hooks::route(fallback)
    }

    pub(super) fn call_phone_controls(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Div {
        let up = in_call(call);
        // Reconnecting keeps the controls, dead; only the way out works.
        let live = call.state != CallState::Reconnecting;
        let off = call.muted || call.deafened;
        let push = crate::gui::app::prefs::values().on(PUSH_TO_TALK);
        let mute = if push && up && live {
            round("call-phone-talk", "icons/hand.svg", "Hold to talk", if call.speaking { Look::On } else { Look::Rest }, SIZE)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.call_ask(Request::CallTalk { held: true, priority: false }, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.call_ask(Request::CallTalk { held: false, priority: false }, cx)))
        } else {
            let (look, word) = match (live, off) {
                (false, _) => (Look::Unavailable, "Mute"),
                (true, true) => (Look::Off, "Unmute"),
                (true, false) => (Look::Rest, "Mute"),
            };
            round("call-phone-mute", if off { "icons/mic-off.svg" } else { "icons/mic.svg" }, word, look, SIZE)
                .when(live, |mute| mute.on_click(cx.listener(|this, _, _, cx| this.call_mute(cx))))
        };
        let route = self.call_phone_route(call);
        let routes = hooks::routes();
        let speaker = round(
            "call-phone-route",
            route.icon(),
            route.label(),
            match (live, route) {
                (false, _) => Look::Unavailable,
                (true, Route::Speaker) => Look::On,
                _ => Look::Rest,
            },
            SIZE,
        )
        .when(live, |button| {
            button.on_click(cx.listener(move |this, _, _, cx| {
                if routes.len() > 2 {
                    this.call_phone_open_sheet(Sheet::Devices, cx);
                } else {
                    hooks::choose_route(if route == Route::Speaker { Route::Earpiece } else { Route::Speaker });
                    cx.notify();
                }
            }))
        });
        let end_word = match call.state {
            CallState::RingingOut | CallState::Connecting => "Cancel",
            _ if subject.kind == ConversationKind::Direct => "End",
            _ => "Leave",
        };
        let end = round("call-phone-end", "icons/phone-off.svg", end_word, Look::End, SIZE).on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)));
        let deafen = round(
            "call-phone-deafen",
            if call.deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" },
            if call.deafened { "Undeafen" } else { "Deafen" },
            match (live, call.deafened) {
                (false, _) => Look::Unavailable,
                (true, true) => Look::Off,
                (true, false) => Look::Rest,
            },
            SIZE,
        )
        .when(live, |button| button.on_click(cx.listener(|this, _, _, cx| this.call_deafen(cx))));
        let more = round("call-phone-more", "icons/ellipsis.svg", "More", if live { Look::Rest } else { Look::Unavailable }, SIZE)
            .when(live, |button| button.on_click(cx.listener(|this, _, _, cx| this.call_phone_open_sheet(Sheet::More, cx))));

        let rows: Vec<Div> = if !up {
            vec![self.call_phone_row(vec![mute, speaker, end], true)]
        } else if pictures_network(call) {
            let camera_on = sending(call, Content::Camera);
            let share_on = sending(call, Content::Share);
            let denied = hooks::denied(Permission::Camera);
            let missing = camera_unavailable();
            let camera = if denied {
                round("call-phone-camera", "icons/video-off.svg", "Not allowed", Look::Unavailable, SIZE).on_click(cx.listener(|this, _, _, cx| this.call_phone_open_sheet(Sheet::Camera, cx)))
            } else if let Some(why) = missing {
                round("call-phone-camera", "icons/video-off.svg", "No camera", Look::Unavailable, SIZE)
                    .on_click(cx.listener(move |this, _, _, cx| this.toast(Some(("icons/video-off.svg", theme::text_soft())), why, cx)))
            } else {
                round("call-phone-camera", if camera_on { "icons/video.svg" } else { "icons/video-off.svg" }, if camera_on { "Camera" } else { "Camera off" }, if !live { Look::Unavailable } else if camera_on { Look::On } else { Look::Rest }, SIZE)
                    .when(live, |button| button.on_click(cx.listener(move |this, _, _, cx| this.call_ask(Request::Media(MediaRequest::Camera { on: !camera_on }), cx))))
            };
            let front = !self.call_state.phone.back_camera;
            let flip = round("call-phone-flip", "icons/switch-camera.svg", "Flip", if camera_on && live { Look::Rest } else { Look::Unavailable }, SIZE).when(camera_on && live, |button| {
                button.on_click(cx.listener(move |this, _, _, cx| {
                    this.call_state.phone.back_camera = front;
                    hooks::flip_camera(!front);
                    cx.notify();
                }))
            });
            let share = round(
                "call-phone-share",
                if share_on { "icons/monitor-off.svg" } else { "icons/monitor-up.svg" },
                if share_on { "Stop share" } else { "Share" },
                if !live { Look::Unavailable } else if share_on { Look::On } else { Look::Rest },
                SIZE,
            )
            .when(live, |button| button.on_click(cx.listener(move |this, _, _, cx| this.call_ask(Request::Media(MediaRequest::ShareSend { on: !share_on }), cx))));
            vec![self.call_phone_row(vec![camera, flip, share, speaker], false), self.call_phone_row(vec![mute, deafen, more, end], false)]
        } else {
            vec![self.call_phone_row(vec![mute, speaker, deafen, more, end], false)]
        };
        div()
            .flex_none()
            .w_full()
            .px(px(12.))
            .pt(px(16.))
            .pb(px(tokens::safe_bottom() + 10.))
            .rounded_t(px(28.))
            .bg(theme::raised())
            .border_t_1()
            .border_color(theme::hairline())
            .shadow(tokens::top_highlight())
            .flex()
            .flex_col()
            .gap(px(CONTROL_GAP))
            .children(rows)
    }

    /// One row of buttons, each taking an equal share of the width. A short row keeps them
    /// as far apart as a full one, centered.
    fn call_phone_row(&self, buttons: Vec<gpui::Stateful<Div>>, short: bool) -> Div {
        div()
            .w_full()
            .flex()
            .justify_center()
            .when(short, |row| row.px(px(40.)))
            .children(buttons)
    }
}
