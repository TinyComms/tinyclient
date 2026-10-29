//! The user panel at the foot of the server's column, as Discord has it: who I am here, how my
//! link is, my microphone and headphones, and the voice settings. While a call is up its second
//! line is the state of the call with signal bars and the round trip, and a click on it opens
//! Call health: the link graded in words, what to do about it, and the numbers (ping, jitter,
//! loss, bitrate and the path the audio takes).

use super::Float;
use crate::gui::call::health::{self, Grade, Health};
use crate::gui::components::{glyph, tip, Avatar};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Rgba, SharedString, Stateful};
use protocol::{CallPath, CallState, CallStats, Presence};

pub(super) const PANEL: f32 = 52.;

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
pub(super) fn quality(stats: &CallStats) -> Rgba {
    match Health::from_stats(stats).grade() {
        Grade::Unknown => theme::positive(),
        grade => grade.color(),
    }
}

/// The panel's second line while this server has a call, and whether a click on it opens
/// Call health. `None` when the call is not one to speak of (failed: the dock says so).
fn call_line(state: &CallState, stats: Option<&CallStats>) -> Option<(String, Rgba)> {
    Some(match state {
        CallState::Connecting | CallState::RingingOut => ("Connecting…".to_owned(), theme::warning()),
        CallState::Reconnecting => ("Reconnecting…".to_owned(), theme::warning()),
        CallState::Joined => match stats {
            Some(stats) if stats.rtt_ms > 0 => (format!("Voice connected · {} ms", stats.rtt_ms), quality(stats)),
            _ => ("Voice connected".to_owned(), theme::positive()),
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

impl Shell {
    /// The panel, at the very foot of the sidebar on a Tiny server.
    pub(in crate::gui) fn tiny_user_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        let page = self.tiny_page_facts()?;
        let me = page.community.member(page.me);
        let name = me.map(super::model::name_of).unwrap_or_else(|| "You".to_owned());
        let mut avatar = Avatar::new(&page.key(page.me), &name, 32.);
        if page.live {
            avatar = avatar.presence(Presence::Online, theme::rail());
        }
        let (muted, deafened) = self.tiny_quiet();
        let call = self.tiny_call();
        let room = page.mine.and_then(|id| page.community.rooms.iter().find(|room| room.id == id));
        let in_call = call.as_ref().and_then(|call| call_line(&call.state, self.tiny_state.stats.as_ref()));
        let grade = self.tiny_state.stats.as_ref().map_or(Grade::Unknown, |stats| Health::from_stats(stats).grade());
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
                .child(avatar.build())
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
        self.tiny_state.stats.as_ref().map(Health::from_stats)
    }

    /// Call health, over the panel: the link graded, what to do about it in words, then the
    /// connection's numbers.
    pub(super) fn tiny_stats_card(&self) -> Div {
        let row = |label: &'static str, value: String| {
            div()
                .h(px(24.))
                .flex()
                .items_center()
                .child(div().flex_1().type_ui().text_color(theme::text_soft()).child(label))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(value))
        };
        let room = self.tiny_page_facts().and_then(|page| page.mine.and_then(|id| page.community.rooms.iter().find(|room| room.id == id)).map(|room| (room.name.clone(), room.bitrate())));
        let health = self.tiny_health().unwrap_or_default();
        let grade = health.grade();
        let mut card = div().w(px(288.)).flex().flex_col().gap(px(2.)).child(
            div()
                .pb(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(health::bars(grade, 14.))
                .child(div().flex_1().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Call health"))
                .child(div().type_small().text_color(grade.color()).child(grade.words())),
        );
        let hints = health.hints();
        if !hints.is_empty() {
            card = card.child(
                div().pb(px(8.)).mb(px(4.)).border_b_1().border_color(theme::hairline()).flex().flex_col().gap(px(6.)).children(hints.into_iter().take(3).map(|hint| {
                    div()
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .child(div().pt(px(2.)).child(glyph(hint.level.icon(), 13., hint.level.color())))
                        .child(div().flex_1().min_w_0().whitespace_normal().type_small().text_color(theme::text()).child(hint.words))
                })),
            );
        }
        if let Some((name, kbps)) = &room {
            card = card.child(row("Channel", name.clone())).child(row("Channel bitrate", format!("{kbps} kb/s")));
        }
        match &self.tiny_state.stats {
            Some(stats) => {
                card = card
                    .child(row("Ping", format!("{} ms", stats.rtt_ms)))
                    .child(row("Jitter", format!("{} ms", stats.jitter_ms)))
                    .child(row("Packet loss", format!("{:.1} %", f32::from(stats.loss_permille) / 10.)))
                    .child(row("Sending", format!("{} kb/s", stats.send_kbps)))
                    .child(row("Receiving", format!("{} kb/s", stats.receive_kbps)))
                    .child(row("Path", path_words(stats.path).to_owned()))
                    .when(!stats.codec.is_empty(), |card| card.child(row("Codec", stats.codec.clone())));
            }
            None => {
                let words: SharedString = self.tiny_state.stats_note.clone().unwrap_or_else(|| "Measuring…".to_owned()).into();
                card = card.child(div().pt(px(4.)).whitespace_normal().type_small().text_color(theme::text_soft()).child(words));
            }
        }
        card
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_says_what_the_call_is_doing_not_just_that_there_is_one() {
        let good = CallStats { rtt_ms: 38, jitter_ms: 3, ..CallStats::default() };
        assert_eq!(call_line(&CallState::Joined, Some(&good)).map(|line| line.0), Some("Voice connected · 38 ms".to_owned()));
        assert_eq!(call_line(&CallState::Joined, None).map(|line| line.0), Some("Voice connected".to_owned()));
        assert_eq!(call_line(&CallState::Connecting, Some(&good)), Some(("Connecting…".to_owned(), theme::warning())));
        assert_eq!(call_line(&CallState::Reconnecting, Some(&good)), Some(("Reconnecting…".to_owned(), theme::warning())));
        let failed = CallState::Failed { reason: protocol::CallFailure::Lost, detail: String::new() };
        assert_eq!(call_line(&failed, Some(&good)), None, "a failed call is the dock's to tell, not 'Voice connected'");
    }

    #[test]
    fn the_link_is_coloured_by_its_round_trip_and_loss() {
        let stats = |rtt_ms, loss_permille| CallStats { rtt_ms, loss_permille, ..CallStats::default() };
        assert_eq!(quality(&stats(40, 0)), theme::positive());
        assert_eq!(quality(&stats(180, 0)), theme::warning());
        assert_eq!(quality(&stats(40, 30)), theme::warning());
        assert_eq!(quality(&stats(400, 0)), theme::danger());
        assert_eq!(path_words(CallPath::Relay), "Relayed");
    }
}
