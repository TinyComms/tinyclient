//! The user panel at the foot of the server's column, as Discord has it: who I am here, how my
//! link is, my microphone and headphones, and the voice settings. While a call is up its second
//! line is the round trip, and a click on it shows the connection's numbers (ping, jitter,
//! loss, bitrate and the path the audio takes).

use super::Float;
use crate::gui::components::{glyph, tip, Avatar};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Rgba, SharedString, Stateful};
use protocol::{CallPath, CallStats, Presence};

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
        .when(off, |button| button.bg(theme::danger_soft()))
        .hover(|style| style.bg(theme::hover()))
        .active(|style| style.bg(theme::pressed()))
        .child(glyph(icon, 18., if off { theme::danger() } else { theme::text_soft() }))
}

/// How good the link is, from its round trip and loss: the colour of the panel's words.
pub(super) fn quality(stats: &CallStats) -> Rgba {
    if stats.rtt_ms == 0 && stats.loss_permille == 0 {
        theme::positive()
    } else if stats.rtt_ms > 250 || stats.loss_permille > 50 {
        theme::danger()
    } else if stats.rtt_ms > 120 || stats.loss_permille > 15 {
        theme::warning()
    } else {
        theme::positive()
    }
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
        let (line, color, clickable): (String, Rgba, bool) = match (call, room) {
            (Some(_), _) => match &self.tiny_state.stats {
                Some(stats) if stats.rtt_ms > 0 => (format!("Voice connected · {} ms", stats.rtt_ms), quality(stats), true),
                _ => ("Voice connected".to_owned(), theme::positive(), true),
            },
            (None, Some(room)) => (format!("Seated in {}", room.name), theme::text_soft(), false),
            (None, None) if page.live => ("Online".to_owned(), theme::text_soft(), false),
            (None, None) => ("Connecting…".to_owned(), theme::warning(), false),
        };
        let stats_open = self.tiny_state.float == Some(Float::Stats);
        let status = div()
            .id("tiny-panel-status")
            .min_w_0()
            .truncate()
            .type_small()
            .text_color(color)
            .child(line)
            .when(clickable, |status| {
                status
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .tooltip(tip("Connection details", None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if stats_open {
                            this.tiny_close_float(cx);
                        } else {
                            this.tiny_open_float(Float::Stats, cx);
                        }
                    }))
            });
        let mic = panel_button("tiny-panel-mic", if muted { "icons/mic-off.svg" } else { "icons/mic.svg" }, muted)
            .tooltip(tip(if muted { "Unmute" } else { "Mute" }, Some("⌘⇧M")))
            .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_mute(cx)));
        let ears = panel_button("tiny-panel-deafen", if deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" }, deafened)
            .tooltip(tip(if deafened { "Undeafen" } else { "Deafen" }, Some("⌘⇧D")))
            .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_deafen(cx)));
        let cog = panel_button("tiny-panel-settings", "icons/settings.svg", false)
            .tooltip(tip("Voice settings", None))
            .on_click(|_, _, cx| crate::gui::settings::open(crate::gui::settings::Section::Audio, cx));
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

    /// The connection's numbers, over the panel.
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
        let mut card = div().w(px(260.)).flex().flex_col().gap(px(2.)).child(
            div()
                .pb(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(glyph("icons/signal.svg", 16., self.tiny_state.stats.as_ref().map_or(theme::positive(), quality)))
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Voice connection")),
        );
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
    fn the_link_is_coloured_by_its_round_trip_and_loss() {
        let stats = |rtt_ms, loss_permille| CallStats { rtt_ms, loss_permille, ..CallStats::default() };
        assert_eq!(quality(&stats(40, 0)), theme::positive());
        assert_eq!(quality(&stats(180, 0)), theme::warning());
        assert_eq!(quality(&stats(40, 30)), theme::warning());
        assert_eq!(quality(&stats(400, 0)), theme::danger());
        assert_eq!(path_words(CallPath::Relay), "Relayed");
    }
}
