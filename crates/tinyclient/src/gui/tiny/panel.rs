//! The user panel at the foot of the server's column, as Discord has it: who I am here, how my
//! link is, my microphone and headphones, and the voice settings. While a call is up its second
//! line is the state of the call with signal bars and the round trip, and a click on it opens
//! Call health: the link graded in words, what to do about it, and the numbers (ping, jitter,
//! loss, bitrate and the path the audio takes), with a minute of ping and receive on a graph.

use super::Float;
use crate::gui::call::health::{self, ChartSample, Grade, Health};
use crate::gui::components::{glyph, segment, segmented, tip, Avatar};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    canvas, div, fill, point, prelude::*, px, size, Bounds, Context, Div, PathBuilder, Rgba, SharedString, Stateful,
};
use protocol::{CallPath, CallState, Presence};

pub(super) const PANEL: f32 = 52.;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum StatsTab {
    #[default]
    Connection,
    Video,
}

/// One of the panel's three buttons. `off` draws it struck through in `danger`.
fn panel_button(id: &'static str, icon: &'static str, off: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(32.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .when(off, |button| button.bg(theme::danger_soft()))
        .hover(|style| style.bg(theme::hover()))
        .active(|style| style.bg(theme::pressed()))
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .child(glyph(icon, 18., if off { theme::danger() } else { theme::text_soft() }))
}

/// How good the link is, from its round trip, loss and jitter: the colour of the panel's words.
/// Not measured yet reads as connected.
pub(super) fn quality(health: &Health) -> Rgba {
    match health.grade() {
        Grade::Unknown => theme::positive(),
        grade => grade.color(),
    }
}

/// The panel's second line while this server has a call, and whether a click on it opens
/// Call health. `None` when the call is not one to speak of (failed: the dock says so).
fn call_line(state: &CallState, health: Option<&Health>, securing: bool) -> Option<(String, Rgba)> {
    Some(match state {
        CallState::Joined if securing => ("Securing…".to_owned(), theme::warning()),
        CallState::Connecting | CallState::RingingOut => ("Connecting…".to_owned(), theme::warning()),
        CallState::Reconnecting => ("Reconnecting…".to_owned(), theme::warning()),
        CallState::Joined => match health.and_then(|health| Some((health, health.rtt_ms?))) {
            Some((health, ms)) => (format!("Voice connected · {ms} ms"), quality(health)),
            None => ("Voice connected".to_owned(), theme::positive()),
        },
        CallState::Failed { .. } | CallState::RingingIn { .. } => return None,
    })
}

pub(super) fn path_words(path: CallPath) -> &'static str {
    match path {
        CallPath::Udp => "Direct (UDP)",
        CallPath::Tcp => "Direct (TCP)",
        CallPath::Relay => "Relayed",
        CallPath::Unknown => "Not known yet",
    }
}

fn path_pill(path: CallPath) -> &'static str {
    match path {
        CallPath::Udp => "Direct · UDP",
        CallPath::Tcp => "Direct · TCP",
        CallPath::Relay => "Relayed",
        CallPath::Unknown => "Not known yet",
    }
}

fn grade_badge(grade: Grade) -> &'static str {
    match grade {
        Grade::Unknown => "…",
        Grade::Poor => "Poor",
        Grade::Fair => "Fair",
        Grade::Good => "Good",
        Grade::Great => "Excellent",
    }
}

fn meta_row(label: &'static str, value: impl IntoElement) -> Div {
    div()
        .h(px(26.))
        .flex()
        .items_center()
        .child(div().flex_1().type_ui().text_color(theme::text_soft()).child(label))
        .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(value))
}

fn kpi(label: &'static str, value: String, unit: &'static str, color: Rgba) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .px(px(8.))
        .py(px(8.))
        .rounded(px(theme::R_CONTROL))
        .border_1()
        .border_color(theme::border())
        .bg(theme::field())
        .child(div().type_micro().font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child(label))
        .child(
            div()
                .mt(px(2.))
                .flex()
                .items_baseline()
                .gap(px(2.))
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(color).child(value))
                .child(div().type_small().font_weight(theme::MEDIUM).text_color(theme::text_soft()).child(unit)),
        )
}

/// The selected tab's measured history, with current values in the footer. Video send and
/// receive share one bitrate scale; connection RTT and receive retain their own scales.
fn health_chart(mut samples: Vec<ChartSample>, now_rtt: Option<u32>, now_recv: u32, video: bool) -> Div {
    // One chart slot, with only the selected tab's series. Video's two rates share a scale.
    if video {
        for sample in &mut samples {
            let (sent, received) = sample.video_kbps.unwrap_or_default();
            sample.rtt_ms = sent;
            sample.recv_kbps = received;
        }
    }
    let foot = match (video, now_rtt) {
        (true, Some(sent)) => format!("↑ {sent} · ↓ {now_recv} kb/s"),
        (true, None) => "—".to_owned(),
        (false, Some(rtt)) => format!("{rtt} ms · {now_recv} kb/s"),
        (false, None) => format!("— · {now_recv} kb/s"),
    };
    let chart = canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            if samples.is_empty() {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let origin_x = f32::from(bounds.origin.x);
            let origin_y = f32::from(bounds.origin.y);
            for frac in [0.25_f32, 0.5, 0.75] {
                let y = origin_y + height * frac;
                window.paint_quad(fill(Bounds::new(point(px(origin_x), px(y)), size(px(width), px(1.))), theme::hairline()));
            }
            let last_at = samples.last().map(|sample| sample.at).unwrap_or_else(std::time::Instant::now);
            let max_rtt = samples.iter().map(|sample| sample.rtt_ms).max().unwrap_or(1).max(1) as f32;
            let max_recv = samples.iter().map(|sample| sample.recv_kbps).max().unwrap_or(1).max(1) as f32;
            let max_video = max_rtt.max(max_recv);
            let x_at = |sample: &ChartSample| origin_x + width * (1. - last_at.saturating_duration_since(sample.at).as_secs_f32() / 60.).clamp(0., 1.);
            let ping_y = |rtt: u32| {
                let t = (rtt as f32 / (if video { max_video } else { max_rtt } * 1.2)).clamp(0., 1.);
                origin_y + height * if video { 0.06 + 0.86 * (1. - t) } else { 0.42 + 0.5 * (1. - t) }
            };
            let recv_y = |kbps: u32| {
                let t = (kbps as f32 / (if video { max_video } else { max_recv } * 1.2)).clamp(0., 1.);
                origin_y + height * if video { 0.06 + 0.86 * (1. - t) } else { 0.06 + 0.3 * (1. - t) }
            };

            let mut area = PathBuilder::fill();
            area.move_to(point(px(x_at(&samples[0])), px(origin_y + height)));
            for sample in &samples {
                area.line_to(point(px(x_at(sample)), px(ping_y(sample.rtt_ms))));
            }
            area.line_to(point(px(x_at(samples.last().unwrap())), px(origin_y + height)));
            area.close();
            if let Ok(path) = area.build() {
                window.paint_path(path, theme::positive_soft());
            }

            let mut ping = PathBuilder::stroke(px(2.));
            for (index, sample) in samples.iter().enumerate() {
                let at = point(px(x_at(sample)), px(ping_y(sample.rtt_ms)));
                if index == 0 {
                    ping.move_to(at);
                } else {
                    ping.line_to(at);
                }
            }
            if let Ok(path) = ping.build() {
                window.paint_path(path, theme::positive());
            }

            let mut recv = PathBuilder::stroke(px(1.75)).dash_array(&[px(3.), px(3.)]);
            for (index, sample) in samples.iter().enumerate() {
                let at = point(px(x_at(sample)), px(recv_y(sample.recv_kbps)));
                if index == 0 {
                    recv.move_to(at);
                } else {
                    recv.line_to(at);
                }
            }
            if let Ok(path) = recv.build() {
                window.paint_path(path, theme::accent());
            }

            if let Some(last) = samples.last() {
                let cx = x_at(last);
                let cy = ping_y(last.rtt_ms);
                window.paint_quad(fill(
                    Bounds::new(point(px(cx - 5.), px(cy - 5.)), size(px(10.), px(10.))),
                    theme::positive_soft(),
                ));
                window.paint_quad(fill(
                    Bounds::new(point(px(cx - 2.5), px(cy - 2.5)), size(px(5.), px(5.))),
                    theme::positive(),
                ));
            }
        },
    )
    .h(px(92.))
    .w_full();

    div()
        .mb(px(10.))
        .p(px(10.))
        .rounded(px(theme::R_CONTROL))
        .border_1()
        .border_color(theme::border())
        .bg(theme::field())
        .child(
            div()
                .mb(px(6.))
                .flex()
                .items_center()
                .justify_between()
                .child(div().type_micro().font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child("Last 60s"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .child(div().size(px(7.)).rounded_full().bg(theme::positive()))
                                .child(div().type_micro().text_color(theme::text_soft()).child(if video { "Send" } else { "RTT" })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .child(div().size(px(7.)).rounded_full().bg(theme::accent()))
                                .child(div().type_micro().text_color(theme::text_soft()).child("Recv")),
                        ),
                ),
        )
        .child(chart)
        .child(
            div()
                .mt(px(4.))
                .flex()
                .justify_between()
                .type_micro()
                .text_color(theme::text_soft())
                .child("−60s")
                .child(foot),
        )
}

impl Shell {
    /// The panel, at the very foot of the sidebar on a Tiny server.
    pub(in crate::gui) fn tiny_user_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        let page = self.tiny_page_facts()?;
        let me = page.community.member(page.me);
        let name = me.map(super::model::name_of).unwrap_or_else(|| "You".to_owned());
        let mut avatar = Avatar::new(&page.key(page.me), &name, 32.).image_at(page.picture(page.me));
        if page.live {
            avatar = avatar.presence(Presence::Online, theme::rail());
        }
        let (muted, deafened) = self.tiny_quiet();
        let call = self.tiny_call();
        let room = page.mine.and_then(|id| page.community.rooms.iter().find(|room| room.id == id));
        let health = self.tiny_health();
        let in_call = call.as_ref().and_then(|call| call_line(&call.state, health.as_ref(), crate::gui::call::securing(call)));
        let grade = health.map_or(Grade::Unknown, |health| health.grade());
        let joined = call.as_ref().is_some_and(|call| call.state == CallState::Joined);
        let (line, color, clickable): (String, Rgba, bool) = match (in_call, room) {
            (Some((line, color)), _) => (line, color, true),
            (None, Some(room)) => (format!("Seated in {}", room.name), theme::text_soft(), false),
            (None, None) if page.live => ("Online".to_owned(), theme::text_soft(), false),
            (None, None) => ("Connecting…".to_owned(), theme::warning(), false),
        };
        let status = div()
            .id("tiny-panel-status")
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(5.))
            .type_small()
            .text_color(color)
            .when(joined, |status| status.child(health::bars(grade, 10.)))
            .child(div().min_w_0().truncate().child(line))
            .when(clickable, |status| {
                status
                    .cursor_pointer()
                    .tab_index(0)
                    .hover(|style| style.underline())
                    .focus_visible(|style| style.underline())
                    .tooltip(tip("Call health", None))
                    .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_health(cx)))
            });
        let mic = panel_button("tiny-panel-mic", if muted { "icons/mic-off.svg" } else { "icons/mic.svg" }, muted)
            .tooltip(tip(if muted { "Unmute" } else { "Mute" }, Some("⌘⇧M")))
            .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_mute(cx)));
        let ears = panel_button("tiny-panel-deafen", if deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" }, deafened)
            .tooltip(tip(if deafened { "Undeafen" } else { "Deafen" }, Some("⌘⇧D")))
            .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_deafen(cx)));
        let cog = panel_button("tiny-panel-settings", "icons/settings.svg", false)
            .tooltip(tip("Voice settings", Some("⌘,")))
            .on_click(|_, _, cx| crate::gui::settings::open(crate::gui::settings::Section::Voice, cx));
        Some(
            div()
                .flex_none()
                .h(px(PANEL))
                .px(px(8.))
                .border_t_1()
                .border_color(theme::hairline())
                .bg(theme::rail())
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id("tiny-my-avatar")
                        .flex_none()
                        .cursor_pointer()
                        .tooltip(tip("Change your picture", None))
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_pick_avatar(cx)))
                        .child(avatar.build()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().min_w_0().truncate().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(name))
                        .child(status),
                )
                .child(div().flex_none().flex().items_center().gap(px(2.)).child(mic).child(ears).child(cog)),
        )
    }

    /// Opens Call health over the panel, or closes it: from the dock's signal bars and the
    /// stage's ping chip as from the panel's own line.
    pub(in crate::gui) fn tiny_toggle_health(&mut self, cx: &mut Context<Self>) {
        if self.tiny_state.float == Some(Float::Stats) {
            self.tiny_close_float(cx);
        } else {
            self.tiny_open_float(Float::Stats, cx);
        }
    }

    /// How this server's call is doing, while it has one.
    pub(in crate::gui) fn tiny_health(&self) -> Option<Health> {
        let before = self.tiny_state.stats_before.as_ref();
        self.tiny_state.stats.as_ref().map(|stats| self.tiny_state.steady.applied(Health::from_polls(before, stats)))
    }

    /// One compact view at a time, with the same chart area in both tabs.
    pub(super) fn tiny_stats_card(&self, cx: &mut Context<Self>) -> Div {
        let health = self.tiny_health().unwrap_or_default();
        let grade = health.grade();
        let hints = health.hints();
        let details = hints.iter().map(|hint| hint.words.as_str()).collect::<Vec<_>>().join("\n");
        let video_tab = self.tiny_state.stats_tab == StatsTab::Video;
        let mut card = div().w(px(320.)).flex().flex_col()
            .child(div().pb(px(10.)).flex().items_center().justify_between().gap(px(8.))
                .child(div().flex().items_center().gap(px(8.))
                    .child(health::bars(grade, 14.))
                    .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Call health"))
                    .child(div().id("call-health-info").flex().items_center()
                        .tooltip(tip(details, None))
                        .child(glyph("icons/info.svg", 13., theme::text_faint()))))
                .child(div().px(px(8.)).py(px(3.)).rounded_full()
                    .bg(match grade {
                        Grade::Great | Grade::Good => theme::positive_soft(),
                        Grade::Fair => theme::warning_soft(),
                        Grade::Poor => theme::danger_soft(),
                        Grade::Unknown => theme::field(),
                    }).type_micro().font_weight(theme::SEMIBOLD).text_color(grade.color()).child(grade_badge(grade))))
            .child(segmented().mb(px(10.))
                .child(segment("call-health-connection", "Connection", !video_tab).flex_1().justify_center()
                    .tab_index(0).focus_visible(|style| style.shadow(theme::focus_ring()))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified() && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.tiny_state.stats_tab = StatsTab::Connection;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tiny_state.stats_tab = StatsTab::Connection;
                        cx.notify();
                    })))
                .child(segment("call-health-video", "Video", video_tab).flex_1().justify_center()
                    .tab_index(0).focus_visible(|style| style.shadow(theme::focus_ring()))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified() && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.tiny_state.stats_tab = StatsTab::Video;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tiny_state.stats_tab = StatsTab::Video;
                        cx.notify();
                    }))));
        // A real problem stays visible whichever tab is selected; its full explanation is on hover.
        if let Some(hint) = hints.first().filter(|hint| hint.level == health::Level::Bad || hint.level == health::Level::Warn) {
            card = card.child(div().id("call-health-warning").mb(px(8.)).truncate().type_small()
                .text_color(hint.level.color()).tooltip(tip(hint.words.clone(), None)).child(hint.words.clone()));
        }
        let Some(stats) = &self.tiny_state.stats else {
            return card.child(div().h(px(260.)).flex().items_center().justify_center().type_small()
                .text_color(theme::text_soft()).child(self.tiny_state.stats_note.clone().unwrap_or_else(|| "Measuring…".to_owned())));
        };
        let samples: Vec<_> = self.tiny_state.stats_chart.iter().copied().collect();
        let row = |label, value: String, unit, measured, help| {
            kpi(label, value, unit, if measured { theme::positive() } else { theme::text_soft() })
                .id(SharedString::from(format!("call-health-kpi-{label}"))).tooltip(tip(help, None))
        };
        if video_tab {
            let Some(video) = &stats.video else {
                return card.child(div().h(px(260.)).flex().flex_col().items_center().justify_center().gap(px(10.))
                    .child(glyph("icons/monitor-up.svg", 26., theme::text_faint()))
                    .child(div().type_small().text_color(theme::text_soft()).child("No video active")));
            };
            let incoming = video.receive_expected;
            card = card.child(health_chart(samples, Some(video.send_kbps), video.receive_kbps, true))
                .child(div().mb(px(10.)).flex().gap(px(8.))
                    .child(row("Sending", video.send_fps.to_string(), "fps", true,
                        "Original video frames emitted after pacing, across screen sharing and camera layers. Retransmissions are not new frames; TCP may still buffer output."))
                    .child(row("Receiving", if incoming { video.receive_fps.to_string() } else { "—".to_owned() }, "fps", incoming,
                        "Complete incoming video frames per second. No value while you are not watching."))
                    .child(row("Rendered", if incoming { video.rendered_fps.to_string() } else { "—".to_owned() }, "fps", incoming,
                        "Completed video images prepared for this window, not monitor refreshes.")));
            if incoming {
                card = card.child(meta_row("Resolution", if video.width > 0 { format!("{} × {}", video.width, video.height) } else { "Waiting…".to_owned() }))
                    .child(meta_row("Last image", video.frame_age_ms.map_or_else(|| "None yet".to_owned(), |age| format!("{age} ms ago"))));
            } else {
                card = card.child(meta_row("Playback", "Not watching")).child(meta_row("Codec", "H.264"));
            }
        } else {
            let rtt = (stats.rtt_ms > 0).then_some(stats.rtt_ms);
            let jitter = stats.link_measured.then_some(stats.jitter_ms);
            let loss = stats.link_measured.then_some(stats.loss_permille);
            card = card.child(health_chart(samples, rtt, stats.receive_kbps, false))
                .child(div().mb(px(10.)).flex().gap(px(8.))
                    .child(row("RTT", rtt.map_or_else(|| "—".to_owned(), |ms| ms.to_string()), "ms", rtt.is_some(),
                        "Round-trip time to the call server, not one-way playback delay. Numeric values are the current sample; signal bars smooth recent measurements."))
                    .child(row("Jitter", jitter.map_or_else(|| "—".to_owned(), |ms| ms.to_string()), "ms", jitter.is_some(),
                        "Variation in packet arrival timing. No value when traffic is insufficient to measure it."))
                    .child(row("Loss", loss.map_or_else(|| "—".to_owned(), |n| format!("{:.1}", f32::from(n) / 10.)), "%", loss.is_some(),
                        "Measured packet loss on your connection, after repair.")))
                .child(meta_row("Traffic", format!("↑ {} · ↓ {} kb/s", stats.send_kbps, stats.receive_kbps))
                    .id("call-health-traffic").tooltip(tip("Total sending and receiving for the whole call, including video and transport overhead.", None)))
                .child(meta_row("Transport", format!("{}{}", path_pill(stats.path), if stats.codec.is_empty() { String::new() } else { format!(" · {}", stats.codec.to_uppercase()) })));
        }
        card
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::CallStats;

    #[test]
    fn the_panel_says_what_the_call_is_doing_not_just_that_there_is_one() {
        let good = Health::from_stats(&CallStats { rtt_ms: 38, jitter_ms: 3, ..CallStats::default() });
        assert_eq!(call_line(&CallState::Joined, Some(&good), false).map(|line| line.0), Some("Voice connected · 38 ms".to_owned()));
        assert_eq!(call_line(&CallState::Joined, None, false).map(|line| line.0), Some("Voice connected".to_owned()));
        assert_eq!(call_line(&CallState::Joined, Some(&good), true).map(|line| line.0), Some("Securing…".to_owned()), "keys not ready: nothing is heard yet");
        assert_eq!(call_line(&CallState::Connecting, Some(&good), false), Some(("Connecting…".to_owned(), theme::warning())));
        assert_eq!(call_line(&CallState::Reconnecting, Some(&good), false), Some(("Reconnecting…".to_owned(), theme::warning())));
        let failed = CallState::Failed { reason: protocol::CallFailure::Lost, detail: String::new() };
        assert_eq!(call_line(&failed, Some(&good), false), None, "a failed call is the dock's to tell, not 'Voice connected'");
    }

    #[test]
    fn the_link_is_coloured_by_its_round_trip_and_loss() {
        let stats = |rtt_ms, loss_permille| Health::from_stats(&CallStats { rtt_ms, loss_permille, ..CallStats::default() });
        assert_eq!(quality(&stats(40, 0)), theme::positive());
        assert_eq!(quality(&stats(250, 0)), theme::warning());
        assert_eq!(quality(&stats(40, 40)), theme::warning());
        assert_eq!(quality(&stats(400, 0)), theme::danger());
        assert_eq!(path_words(CallPath::Relay), "Relayed");
        assert_eq!(path_pill(CallPath::Udp), "Direct · UDP");
        assert_eq!(grade_badge(Grade::Great), "Excellent");
    }
}
