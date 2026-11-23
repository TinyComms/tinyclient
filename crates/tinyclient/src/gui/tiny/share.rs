//! Sharing and watching in a Tiny call. Share opens a picker of every screen and window, each with
//! a small picture of it (`mediad --screens`, read off the main thread), and shares the one picked;
//! a phone's share has the system's own picker, so it shares at once. A click on someone's stream
//! watches it and fills the stage with it, and the stage can have the whole window, until Escape.

use super::Modal;
use crate::gui::components::{button, button_disabled, glyph, modal, segment, segmented, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, img, prelude::*, px, Context, Div, ObjectFit, SharedString, Window};
use protocol::{MediaRequest, Request, ScreenSource, ShareQuality, SCREENS_LIMIT};

/// A picture in the picker, 16:9.
const THUMB_W: f32 = 184.;
const THUMB_H: f32 = 104.;

/// A desktop can loop back a screen or a window. A phone shares through the system picker and
/// has no mix of its own to send.
pub(in crate::gui) fn share_sound_offered() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux")) && !crate::gui::phone::active()
}

/// `Some` only where the switch is shown, so a phone's share stays the frame an older build sent.
pub(in crate::gui) fn share_sound_flag(on: bool) -> Option<bool> {
    share_sound_offered().then_some(on)
}

/// The picker's qualities, with what each costs in words.
const QUALITIES: [(ShareQuality, &str, &str); 3] = [
    (ShareQuality::Best, "Best", "Your screen as it is, up to 1440p at 60 fps. About 8 Mb/s."),
    (ShareQuality::Balanced, "Balanced", "1080p at 30 fps. About 4 Mb/s."),
    (ShareQuality::Low, "Low", "720p at 15 fps. About 1.5 Mb/s, for a slow connection."),
];

impl Shell {
    /// Share pressed while not sharing: the picker.
    pub(in crate::gui) fn tiny_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::gui::phone::active() {
            self.call_ask(Request::Media(MediaRequest::ShareSend { on: true, screen: None, quality: None, show_cursor: None, sound: None }), cx);
            return;
        }
        self.tiny_state.screens = None;
        self.tiny_state.screen_pick = None;
        self.tiny_state.screen_windows = false;
        self.tiny_open_modal(Modal::Share, window, cx);
        self.off_thread(cx, read_screens, |this, screens, cx| {
            this.tiny_state.screen_pick = screens.iter().find(|screen| screen.primary).or(screens.first()).map(|screen| screen.id.clone());
            this.tiny_state.screens = Some(screens);
            cx.notify();
        });
    }

    /// Shares the selected source from the visible tab.
    fn tiny_share_picked(&mut self, cx: &mut Context<Self>) {
        let Some(picked) = self.tiny_state.screen_pick.as_ref().and_then(|id| {
            self.tiny_state.screens.iter().flatten().find(|screen| screen.id == *id && screen.window == self.tiny_state.screen_windows)
        }).cloned() else {
            return;
        };
        let screen = Some(picked.id.clone());
        self.tiny_state.shared = Some(picked);
        self.close_modal(cx);
        let quality = Some(self.tiny_state.share_quality);
        let show_cursor = Some(self.tiny_state.share_cursor);
        let sound = share_sound_flag(self.tiny_state.share_sound);
        self.call_ask(Request::Media(MediaRequest::ShareSend { on: true, screen, quality, show_cursor, sound }), cx);
    }

    pub(in crate::gui) fn tiny_stop_share(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.shared = None;
        self.call_ask(Request::Media(MediaRequest::ShareSend { on: false, screen: None, quality: None, show_cursor: None, sound: None }), cx);
    }

    /// The picker: screens or windows, a picture and a name each, Cancel and Share.
    pub(super) fn tiny_share_modal(&self, cx: &mut Context<Self>) -> Div {
        let state = &self.tiny_state;
        let has_windows = state.screens.iter().flatten().any(|screen| screen.window);
        let windows = state.screen_windows && has_windows;
        let tabs = has_windows.then(|| {
            segmented()
                .mt(px(14.))
                .child(segment("tiny-share-screens", "Screens", !windows).on_click(cx.listener(|this, _, _, cx| {
                    if this.tiny_state.screen_windows {
                        this.tiny_state.screen_windows = false;
                        let displays = this.tiny_state.screens.iter().flatten().filter(|screen| !screen.window);
                        this.tiny_state.screen_pick = displays.clone().find(|screen| screen.primary).or_else(|| displays.into_iter().next()).map(|screen| screen.id.clone());
                        cx.notify();
                    }
                })))
                .child(segment("tiny-share-windows", "Windows", windows).on_click(cx.listener(|this, _, _, cx| {
                    if !this.tiny_state.screen_windows {
                        this.tiny_state.screen_windows = true;
                        this.tiny_state.screen_pick = None;
                        cx.notify();
                    }
                })))
        });
        let body = match &state.screens {
            None => div().h(px(THUMB_H + 40.)).flex().items_center().justify_center().type_ui().text_color(theme::text_soft()).child("Looking for screens and windows…").into_any_element(),
            Some(screens) if screens.is_empty() => {
                div().mt(px(12.)).whitespace_normal().type_ui().text_color(theme::text_soft()).child("No screens or windows are available to share.").into_any_element()
            }
            Some(screens) => div()
                .id("tiny-share-list")
                .mt(px(12.))
                .max_h(px(2. * (THUMB_H + 48.) + 12.))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .children(screens.iter().filter(|screen| screen.window == windows).map(|screen| self.tiny_share_card(screen, cx)))
                .into_any_element(),
        };
        // How much the share spends. Best is the screen as it is; the others for a slow link
        // or a busy machine.
        let quality = div()
            .mt(px(14.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().type_ui().text_color(theme::text_soft()).child("Quality"))
            .child(segmented().children(QUALITIES.iter().map(|&(quality, label, hint)| {
                segment(SharedString::from(format!("tiny-share-quality-{label}")), label, state.share_quality == quality)
                    .tooltip(crate::gui::components::tip(hint, None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tiny_state.share_quality = quality;
                        cx.notify();
                    }))
            })));
        // Whether viewers see my pointer, so they can follow what I point at. Shown unless they
        // are told otherwise; a platform that cannot draw it just keeps drawing none.
        let pointer = div()
            .mt(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().type_ui().text_color(theme::text_soft()).child("Pointer"))
            .child(segmented()
                .child(segment("tiny-share-pointer-shown", "Shown", state.share_cursor).on_click(cx.listener(|this, _, _, cx| {
                    this.tiny_state.share_cursor = true;
                    cx.notify();
                })))
                .child(segment("tiny-share-pointer-hidden", "Hidden", !state.share_cursor).on_click(cx.listener(|this, _, _, cx| {
                    this.tiny_state.share_cursor = false;
                    cx.notify();
                }))));
        // The screen's own sound. Hidden where there is no loopback. The call is left out of it.
        let sound = share_sound_offered().then(|| {
            div()
                .mt(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().type_ui().text_color(theme::text_soft()).child("Share sound"))
                .child(segmented()
                    .child(segment("tiny-share-sound-on", "On", state.share_sound).tooltip(crate::gui::components::tip("The sound of this screen or window. Your call is left out.", None)).on_click(cx.listener(|this, _, _, cx| {
                        this.tiny_state.share_sound = true;
                        cx.notify();
                    })))
                    .child(segment("tiny-share-sound-off", "Off", !state.share_sound).on_click(cx.listener(|this, _, _, cx| {
                        this.tiny_state.share_sound = false;
                        cx.notify();
                    }))))
        });
        let ready = state.screen_pick.is_some();
        let go = if ready && !self.tiny_state.busy {
            button("tiny-share-go", ButtonKind::Primary, "Share").h(px(32.)).on_click(cx.listener(|this, _, _, cx| this.tiny_share_picked(cx))).into_any_element()
        } else {
            button_disabled("Share").h(px(32.)).into_any_element()
        };
        modal("Share your screen")
            // Three cards a row, with room for the list's scroll bar.
            .w(px(3. * (THUMB_W + 16.) + 2. * 12. + 40. + 16.))
            .child(div().mt(px(6.)).whitespace_normal().type_ui().text_color(theme::text_soft()).child("Pick what everyone in the call can watch."))
            .children(tabs)
            .child(body)
            .child(quality)
            .child(pointer)
            .children(sound)
            .child(
                div()
                    .mt(px(20.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("tiny-share-cancel", ButtonKind::Secondary, "Cancel").h(px(32.)).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
                    .child(go),
            )
    }

    fn tiny_share_card(&self, screen: &ScreenSource, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let picked = self.tiny_state.screen_pick.as_deref() == Some(screen.id.as_str());
        let id = screen.id.clone();
        let picture = if screen.thumbnail.is_empty() {
            glyph(if screen.window { "icons/square.svg" } else { "icons/monitor-up.svg" }, 28., theme::text_faint()).into_any_element()
        } else {
            img(std::path::PathBuf::from(&screen.thumbnail)).size_full().object_fit(ObjectFit::Contain).into_any_element()
        };
        div()
            .id(SharedString::from(format!("tiny-share-{}", screen.id)))
            .w(px(THUMB_W + 16.))
            .p(px(6.))
            .rounded(px(10.))
            .border_2()
            .border_color(if picked { theme::accent() } else { theme::hairline() })
            .when(picked, |card| card.bg(theme::accent_wash()))
            .flex()
            .flex_col()
            .gap(px(6.))
            .cursor_pointer()
            .tab_index(0)
            .hover(|style| style.bg(theme::hover()))
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                this.tiny_state.screen_pick = Some(id.clone());
                // A double click shares at once.
                if event.click_count() > 1 {
                    this.tiny_share_picked(cx);
                }
                cx.notify();
            }))
            .child(div().w(px(THUMB_W)).h(px(THUMB_H)).rounded(px(6.)).bg(gpui::rgb(0x000000)).overflow_hidden().flex().items_center().justify_center().child(picture))
            .child(div().min_w_0().truncate().type_small().text_color(if picked { theme::text() } else { theme::text_soft() }).child(screen.name.clone()))
    }
}

/// `mediad --screens`, with its pictures in the support folder.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_sound_is_sent_only_where_the_switch_is_shown() {
        let offered = cfg!(any(windows, target_os = "macos", target_os = "linux")) && !crate::gui::phone::active();
        assert_eq!(share_sound_offered(), offered);
        if offered {
            assert_eq!(share_sound_flag(true), Some(true));
            assert_eq!(share_sound_flag(false), Some(false));
        } else {
            assert_eq!(share_sound_flag(true), None);
        }
    }
}

fn read_screens() -> Vec<ScreenSource> {
    let folder = protocol::support_dir().join("screens");
    let stdout = protocol::media_host::output(protocol::launch::no_console(&mut std::process::Command::new(crate::gui::settings::mediad_path())).arg("--screens").arg(&folder).env("TINYCLIENT_SHARE_PICKER_PID", std::process::id().to_string()).stderr(std::process::Stdio::null())).unwrap_or_default();
    serde_json::from_slice::<Vec<ScreenSource>>(&stdout).unwrap_or_default().into_iter().take(SCREENS_LIMIT).collect()
}
