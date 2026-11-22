//! Always-on-top picture-in-picture window for an active call (`#267`).
//!
//! Draws directly from `Shell`'s existing `call::State` feeds (`feed`, `self_feed`, `screen_feed`)
//! without starting a second decoder or `Feed`. While this window is open, the main window's stage
//! and call picture box show a placeholder ("Playing in a separate window · Bring back") so the
//! same frame is never drawn twice.

use super::PopoutSnapshot;
use crate::gui::app::prefs;
use crate::gui::components::{glyph, tip, Avatar};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    div, img, point, prelude::*, px, size, AnyElement, App, Bounds, Context, FontWeight,
    MouseButton, ObjectFit, Subscription, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowKind, WindowOptions,
};
use protocol::{Call, CallState, Content, Direction, MediaRequest, PictureKind, Request};

/// Default 16:9 pop-out window size.
pub const DEFAULT_SIZE: (f32, f32) = (368., 207.);
/// Minimum 16:9 pop-out window size (resizable from 240 px wide).
pub const MIN_SIZE: (f32, f32) = (240., 135.);
/// Inset from the bottom-right corner of the primary display when no saved frame is available.
const EDGE_MARGIN: f32 = 24.;

/// Window kind for the call pop-out.
///
/// Windows, macOS, and X11 support always-on-top via `WindowKind::PopUp`. On Wayland a client
/// cannot request always-on-top, so we fall back to an ordinary small `WindowKind::Normal` window.
pub fn popout_window_kind(is_wayland: bool) -> WindowKind {
    if is_wayland {
        WindowKind::Normal
    } else {
        WindowKind::PopUp
    }
}

fn running_on_wayland() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Computes the initial `(x, y, width, height)` for the pop-out window: restores `saved` when it
/// is finite and at least half visible on one of `displays`, otherwise anchors in the bottom-right
/// corner of the primary display at [`DEFAULT_SIZE`].
pub fn popout_bounds(
    saved: Option<(f32, f32, f32, f32)>,
    displays: &[(f32, f32, f32, f32)],
) -> (f32, f32, f32, f32) {
    if let Some((x, y, w, h)) = saved {
        if x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() {
            let clamped = (x, y, w.max(MIN_SIZE.0), h.max(MIN_SIZE.1));
            if displays.is_empty() || prefs::frame_visible(clamped, displays) {
                return clamped;
            }
        }
    }
    if let Some(&(dx, dy, dw, dh)) = displays.first() {
        let w = DEFAULT_SIZE.0.min(dw.max(MIN_SIZE.0));
        let h = DEFAULT_SIZE.1.min(dh.max(MIN_SIZE.1));
        let x = (dx + dw - w - EDGE_MARGIN).max(dx);
        let y = (dy + dh - h - EDGE_MARGIN).max(dy);
        return (x, y, w, h);
    }
    (100., 100., DEFAULT_SIZE.0, DEFAULT_SIZE.1)
}

/// Decides whether losing focus on the main window should automatically pop out the call video.
pub fn should_auto_popout(
    setting_on: bool,
    main_window_active: bool,
    already_open: bool,
    call: Option<&Call>,
) -> bool {
    if !setting_on || main_window_active || already_open {
        return false;
    }
    let Some(call) = call.filter(|c| c.state == CallState::Joined) else {
        return false;
    };
    let watching_share = call
        .picture
        .as_ref()
        .is_some_and(|p| !p.path.is_empty() && p.kind == PictureKind::Share)
        || call
            .video
            .iter()
            .any(|v| v.direction == Direction::Decode && v.content == Content::Share);
    watching_share
}

/// Picks which participant's picture or tile the pop-out window should show:
/// 1. The focused stream if it matches the active decoded picture.
/// 2. Otherwise the active decoded picture's owner.
/// 3. Otherwise the active speaking participant (or first participant).
///
/// Returns `(user_id, is_share)`.
pub fn preferred_frame_owner(call: &Call, focused_stream: Option<&str>) -> Option<(String, bool)> {
    if let Some(picture) = call.picture.as_ref().filter(|p| !p.path.is_empty()) {
        if focused_stream.is_none_or(|f| f == picture.user_id) {
            return Some((picture.user_id.clone(), picture.kind == PictureKind::Share));
        }
        return Some((picture.user_id.clone(), picture.kind == PictureKind::Share));
    }
    if let Some(foc) = focused_stream {
        if let Some(p) = call.participants.iter().find(|p| p.id == foc) {
            return Some((p.id.clone(), p.sharing));
        }
    }
    if let Some(speaker) = call.participants.iter().find(|p| p.speaking) {
        return Some((speaker.id.clone(), speaker.sharing));
    }
    call.participants
        .first()
        .map(|p| (p.id.clone(), p.sharing))
}

pub struct CallPopout {
    shell: WeakEntity<Shell>,
    hovered: bool,
    _bounds: Subscription,
}

impl CallPopout {
    fn new(shell: WeakEntity<Shell>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let save_shell = shell.clone();
        let bounds = cx.observe_window_bounds(window, move |_, window, cx| {
            let b = window.bounds();
            let frame = (
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width).max(MIN_SIZE.0),
                f32::from(b.size.height).max(MIN_SIZE.1),
            );
            let _ = save_shell.update(cx, |shell, cx| {
                shell.call_state.popout_frame = Some(frame);
                shell.save_prefs(cx);
            });
        });
        Self {
            shell,
            hovered: false,
            _bounds: bounds,
        }
    }

    fn close_back(&self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.call_state.popout = None;
            cx.notify();
        });
        window.remove_window();
    }
}

/// Opens the call pop-out window. Returns `None` if the platform could not open it.
pub fn open(
    shell: WeakEntity<Shell>,
    saved: Option<(f32, f32, f32, f32)>,
    cx: &mut App,
) -> Option<WindowHandle<CallPopout>> {
    let displays: Vec<(f32, f32, f32, f32)> = cx
        .displays()
        .iter()
        .map(|display| {
            let b = display.bounds();
            (
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            )
        })
        .collect();
    let (x, y, w, h) = popout_bounds(saved, &displays);
    let bounds = Bounds {
        origin: point(px(x), px(y)),
        size: size(px(w), px(h)),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        titlebar: None,
        kind: popout_window_kind(running_on_wayland()),
        is_resizable: true,
        is_minimizable: false,
        is_movable: true,
        focus: true,
        show: true,
        app_id: Some(crate::gui::APP_ID.into()),
        ..Default::default()
    };
    let close_shell = shell.clone();
    match cx.open_window(options, move |window, cx| {
        window.on_window_should_close(cx, move |_, cx| {
            let _ = close_shell.update(cx, |shell, cx| {
                shell.call_state.popout = None;
                cx.notify();
            });
            true
        });
        cx.new(|cx| CallPopout::new(shell, window, cx))
    }) {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!("tinyclient: call pop-out window could not open: {err}");
            None
        }
    }
}

fn control_pill(
    id: &'static str,
    icon: &'static str,
    fill: gpui::Rgba,
    color: gpui::Rgba,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(30.))
        .rounded_full()
        .bg(fill)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .tooltip(tip(label, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph(icon, 14., color))
}

impl Render for CallPopout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data: Option<PopoutSnapshot> = self
            .shell
            .upgrade()
            .and_then(|shell| shell.read(cx).call_popout_snapshot());

        let Some(data) = data else {
            // The call ended while the pop-out was still alive; dismiss cleanly.
            window.remove_window();
            return div().size_full().bg(gpui::rgb(0x0b0b0d)).into_any_element();
        };

        let body: AnyElement = if let Some(frame) = data.main_frame {
            img(frame)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        } else {
            let ring = if data.speaker_speaking {
                theme::positive()
            } else {
                theme::hairline()
            };
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child(
                    div()
                        .p(px(3.))
                        .rounded_full()
                        .border_2()
                        .border_color(ring)
                        .child(Avatar::named(&data.speaker_name, 52.).build()),
                )
                .child(
                    div()
                        .type_small()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(data.speaker_name.clone()),
                )
                .into_any_element()
        };

        let pip = data.pip_frame.map(|frame| {
            div()
                .absolute()
                .bottom(px(10.))
                .right(px(10.))
                .w(px(72.))
                .h(px(40.))
                .rounded(px(6.))
                .overflow_hidden()
                .border_1()
                .border_color(theme::border())
                .bg(gpui::rgb(0x0b0b0d))
                .child(img(frame).size_full().object_fit(ObjectFit::Contain))
        });

        let show_chrome = self.hovered;
        let shell_mute = self.shell.clone();
        let shell_cam = self.shell.clone();
        let shell_share = self.shell.clone();
        let shell_leave = self.shell.clone();
        let camera_on = data.camera_on;
        let share_on = data.share_on;
        let muted = data.muted;

        let top_bar = show_chrome.then(|| {
            div()
                .absolute()
                .top(px(8.))
                .left(px(8.))
                .right(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .child(
                    div()
                        .min_w_0()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(theme::overlay())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .children(data.live_badge.then(|| {
                            div()
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(theme::danger())
                                .text_color(theme::on_accent())
                                .text_size(px(9.5))
                                .font_weight(FontWeight::BOLD)
                                .child("LIVE")
                        }))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .type_micro()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(data.title.clone()),
                        ),
                )
                .child(
                    div()
                        .id("call-popout-back")
                        .flex_none()
                        .h(px(26.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .bg(theme::overlay())
                        .text_color(theme::text())
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::pressed()))
                        .type_micro()
                        .font_weight(FontWeight::MEDIUM)
                        .tooltip(tip("Back to window", Some("⇧⌘O")))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, window, cx| this.close_back(window, cx)))
                        .child(glyph("icons/picture-in-picture-2.svg", 13., theme::text()))
                        .child("Back to window"),
                )
        });

        let controls = show_chrome.then(|| {
            let mute_btn = control_pill(
                "call-popout-mute",
                if muted { "icons/mic-off.svg" } else { "icons/mic.svg" },
                if muted { theme::danger_soft() } else { theme::overlay() },
                if muted { theme::danger() } else { theme::text() },
                if muted { "Unmute" } else { "Mute" },
            )
            .on_click(move |_, _, cx| {
                let _ = shell_mute.update(cx, |shell, cx| shell.call_mute(cx));
            });

            let cam_btn = data.can_pictures.then(|| {
                control_pill(
                    "call-popout-camera",
                    if camera_on { "icons/video.svg" } else { "icons/video-off.svg" },
                    if camera_on { theme::accent_soft() } else { theme::overlay() },
                    if camera_on { theme::accent() } else { theme::text() },
                    if camera_on { "Turn camera off" } else { "Turn camera on" },
                )
                .on_click(move |_, _, cx| {
                    let _ = shell_cam.update(cx, |shell, cx| {
                        shell.call_ask(Request::Media(MediaRequest::Camera { on: !camera_on }), cx);
                    });
                })
            });

            let share_btn = data.can_pictures.then(|| {
                control_pill(
                    "call-popout-share",
                    if share_on { "icons/monitor-off.svg" } else { "icons/monitor-up.svg" },
                    if share_on { theme::accent_soft() } else { theme::overlay() },
                    if share_on { theme::accent() } else { theme::text() },
                    if share_on { "Stop sharing" } else { "Share screen" },
                )
                .on_click(move |_, _, cx| {
                    let _ = shell_share.update(cx, |shell, cx| {
                        if share_on {
                            shell.tiny_stop_share(cx);
                        } else {
                            shell.call_ask(
                                Request::Media(MediaRequest::ShareSend {
                                    on: true,
                                    screen: None,
                                    quality: None,
                                    show_cursor: None,
                                }),
                                cx,
                            );
                        }
                    });
                })
            });

            let leave_btn = control_pill(
                "call-popout-leave",
                "icons/phone-off.svg",
                theme::danger_fill(),
                theme::on_accent(),
                "Leave call",
            )
            .on_click(move |_, _, cx| {
                let _ = shell_leave.update(cx, |shell, cx| shell.call_leave(cx));
            });

            div()
                .absolute()
                .bottom(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px(px(8.))
                        .py(px(5.))
                        .rounded_full()
                        .bg(theme::overlay())
                        .border_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(mute_btn)
                        .children(cam_btn)
                        .children(share_btn)
                        .child(leave_btn),
                )
        });

        div()
            .id("call-popout-root")
            .relative()
            .size_full()
            .bg(gpui::rgb(0x0b0b0d))
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.hovered != *hovered {
                    this.hovered = *hovered;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                window.start_window_move();
            })
            .child(body)
            .children(pip)
            .children(top_bar)
            .children(controls)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CallPicture, Participant, VideoStream};

    fn sample_call() -> Call {
        let mut call: Call = serde_json::from_value(serde_json::json!({
            "conversation": { "network": "tiny", "account": "srv", "id": "dm-1" },
            "state": "joined", "participants": [], "muted": false, "deafened": false,
            "joined": 100, "engine_linked": true, "video": []
        }))
        .unwrap();
        call.participants = vec![
            Participant { id: "u1".into(), name: "Mara".into(), camera: true, sharing: true, ..Participant::default() },
            Participant { id: "u2".into(), name: "Dev".into(), speaking: true, ..Participant::default() },
        ];
        call
    }

    #[test]
    fn popout_bounds_restores_visible_frame_and_clamps_minimum_size() {
        let displays = [(0., 0., 1920., 1080.)];
        assert_eq!(popout_bounds(Some((100., 120., 400., 225.)), &displays), (100., 120., 400., 225.));
        assert_eq!(popout_bounds(Some((100., 120., 100., 60.)), &displays), (100., 120., MIN_SIZE.0, MIN_SIZE.1));
        // Off-screen saved position falls back to primary display bottom-right.
        let fallback = popout_bounds(Some((5000., 5000., 368., 207.)), &displays);
        assert_eq!(fallback, (1920. - DEFAULT_SIZE.0 - EDGE_MARGIN, 1080. - DEFAULT_SIZE.1 - EDGE_MARGIN, DEFAULT_SIZE.0, DEFAULT_SIZE.1));
    }

    #[test]
    fn wayland_falls_back_to_normal_window_while_others_use_always_on_top_popup() {
        assert_eq!(popout_window_kind(true), WindowKind::Normal);
        assert_eq!(popout_window_kind(false), WindowKind::PopUp);
    }

    #[test]
    fn auto_popout_triggers_only_when_enabled_blurred_and_watching_a_share() {
        let mut call = sample_call();
        assert!(!should_auto_popout(false, false, false, Some(&call)));
        assert!(!should_auto_popout(true, true, false, Some(&call)));
        assert!(!should_auto_popout(true, false, false, Some(&call)), "not watching a share yet");

        call.video.push(VideoStream { direction: Direction::Decode, content: Content::Share, source: "u1".into() });
        assert!(should_auto_popout(true, false, false, Some(&call)));
        assert!(!should_auto_popout(true, false, true, Some(&call)), "already popped out");

        call.video.clear();
        call.picture = Some(CallPicture {
            path: "/tmp/frame.jpg".into(),
            width: 1280,
            height: 720,
            generation: 1,
            user_id: "u1".into(),
            kind: PictureKind::Share,
        });
        assert!(should_auto_popout(true, false, false, Some(&call)));
    }

    #[test]
    fn preferred_frame_owner_prefers_active_picture_then_active_speaker() {
        let mut call = sample_call();
        assert_eq!(preferred_frame_owner(&call, None), Some(("u2".into(), false)), "active speaker when no picture");
        call.picture = Some(CallPicture {
            path: "/tmp/share.jpg".into(),
            width: 1280,
            height: 720,
            generation: 2,
            user_id: "u1".into(),
            kind: PictureKind::Share,
        });
        assert_eq!(preferred_frame_owner(&call, None), Some(("u1".into(), true)));
    }
}
