//! Sharing and watching in a Tiny call. Share opens a picker of every screen and window, each with
//! a small picture of it (`mediad --screens`, read off the main thread), and shares the one picked;
//! a phone's share has the system's own picker, so it shares at once. A click on someone's stream
//! watches it and fills the stage with it, and the stage can have the whole window, until Escape.

use super::channel::watching;
use super::stage::{self, Focus};
use super::Modal;
use crate::gui::components::{button, button_disabled, glyph, modal, segment, segmented, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, img, prelude::*, px, Context, Div, ObjectFit, SharedString, Window};
use protocol::{MediaRequest, Request, ScreenSource, ShareQuality, SCREENS_LIMIT};

/// A picture in the picker, 16:9.
const THUMB_W: f32 = 184.;
const THUMB_H: f32 = 104.;

/// The picker's qualities, with what each costs in words.
const QUALITIES: [(ShareQuality, &str, &str); 3] = [
    (ShareQuality::Best, "Best", "Your screen as it is, up to 1440p at 60 fps. About 8 Mb/s."),
    (ShareQuality::Balanced, "Balanced", "1080p at 30 fps. About 4 Mb/s."),
    (ShareQuality::Low, "Low", "720p at 15 fps. About 1.5 Mb/s, for a slow connection."),
];

impl Shell {
    /// Share pressed while not sharing: the picker.
    pub(super) fn tiny_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::gui::phone::active() {
            self.call_ask(Request::Media(MediaRequest::ShareSend { on: true, screen: None, quality: None }), cx);
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

    /// Shares what the picker has picked; the primary screen when it listed nothing.
    fn tiny_share_picked(&mut self, cx: &mut Context<Self>) {
        let picked = self.tiny_state.screen_pick.as_ref().and_then(|id| self.tiny_state.screens.iter().flatten().find(|screen| screen.id == *id)).cloned();
        let screen = picked.as_ref().map(|screen| screen.id.clone());
        self.tiny_state.shared = picked;
        self.close_modal(cx);
        let quality = Some(self.tiny_state.share_quality);
        self.call_ask(Request::Media(MediaRequest::ShareSend { on: true, screen, quality }), cx);
    }

    pub(super) fn tiny_stop_share(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.shared = None;
        self.call_ask(Request::Media(MediaRequest::ShareSend { on: false, screen: None, quality: None }), cx);
    }

    /// A click on a stream: watched, and filling the stage; when it already fills it, the grid.
    pub(super) fn tiny_open_stream(&mut self, user_id: &str, mine: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.tiny_keep_keys(window, cx);
        let focus = stage::toggle(self.tiny_state.focus.as_ref(), Focus::Stream(user_id.to_owned()));
        if focus.is_some() && !mine && !self.tiny_call().is_some_and(|call| watching(&call, user_id)) {
            self.call_ask(Request::Media(MediaRequest::ShareWatch { source: Some(user_id.to_owned()) }), cx);
        }
        self.tiny_state.focus = focus;
        cx.notify();
    }

    pub(super) fn tiny_stop_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tiny_keep_keys(window, cx);
        if matches!(self.tiny_state.focus, Some(Focus::Stream(_))) {
            self.tiny_state.focus = None;
        }
        self.call_ask(Request::Media(MediaRequest::ShareWatch { source: None }), cx);
    }

    /// The people under the focused stream, over it, or gone.
    pub(super) fn tiny_strip(&mut self, strip: stage::Strip, window: &mut Window, cx: &mut Context<Self>) {
        self.tiny_keep_keys(window, cx);
        self.tiny_state.strip = strip;
        cx.notify();
    }

    /// The focused stream has the whole window, which goes full screen if it was not.
    pub(super) fn tiny_full(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tiny_state.full.is_some() {
            return;
        }
        self.tiny_keep_keys(window, cx);
        let took = !window.is_fullscreen();
        if took {
            window.toggle_fullscreen();
        }
        self.tiny_state.full = Some(took);
        cx.notify();
    }

    /// Out of the stream's full screen, and the window back as it was. False when it was not.
    pub(in crate::gui) fn tiny_unfull(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(took) = self.tiny_state.full.take() else {
            return false;
        };
        self.tiny_keep_keys(window, cx);
        if took && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.notify();
        true
    }

    /// The stage's keys back on the window's root. A click that redraws the stage takes away the
    /// tile or button it focused, and Escape then finds no binding.
    pub(super) fn tiny_keep_keys(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
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
                    this.tiny_state.screen_windows = false;
                    cx.notify();
                })))
                .child(segment("tiny-share-windows", "Windows", windows).on_click(cx.listener(|this, _, _, cx| {
                    this.tiny_state.screen_windows = true;
                    cx.notify();
                })))
        });
        let body = match &state.screens {
            None => div().h(px(THUMB_H + 40.)).flex().items_center().justify_center().type_ui().text_color(theme::text_soft()).child("Looking for screens and windows…").into_any_element(),
            Some(screens) if screens.is_empty() => {
                div().mt(px(12.)).whitespace_normal().type_ui().text_color(theme::text_soft()).child("Your whole screen is shared. Everyone in the call can watch it.").into_any_element()
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
        let ready = state.screens.is_some();
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

/// `mediad --screens`, with its pictures in the support folder. Empty when mediad could not say,
/// and the share is then the primary screen.
fn read_screens() -> Vec<ScreenSource> {
    let folder = protocol::support_dir().join("screens");
    let stdout = protocol::media_host::output(protocol::launch::no_console(&mut std::process::Command::new(crate::gui::settings::mediad_path())).arg("--screens").arg(&folder).stderr(std::process::Stdio::null())).unwrap_or_default();
    serde_json::from_slice::<Vec<ScreenSource>>(&stdout).unwrap_or_default().into_iter().take(SCREENS_LIMIT).collect()
}
