//! Voice & media: how the server's audio is, read-only. Whether its media is up (from its
//! `/health`), the limits it holds each channel to, what has to be reachable for people to
//! connect (UDP or TCP 50000), and, while this account is in a call here, the connection's own
//! numbers.

use super::kit::{self, card, cell, table_head, table_row};
use super::{page_head, section};
use crate::gui::components::glyph;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::model;
use crate::gui::tiny::Page as Facts;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Rgba, SharedString, Window};
use protocol::{CallPath, TINY_BITRATE_DEFAULT, TINY_BITRATE_MAX, TINY_BITRATE_MIN};

/// The media port the server listens on, over UDP and over TCP.
pub(in crate::gui) const MEDIA_PORT: u16 = 50_000;

/// What `/health` said of the server's audio, in words and a colour.
pub(in crate::gui) fn health(media: &str) -> (&'static str, &'static str, Rgba) {
    match media {
        "up" => ("Audio is up", "Joining a channel connects with sound.", theme::positive()),
        "down" => ("Audio is down", "The server's media service isn't answering. People get a seat and no sound until it is back.", theme::danger()),
        "off" => ("Audio is off", "This server runs without media: people take seats in channels, with no sound.", theme::warning()),
        _ => ("Not known", "This server doesn't report its audio. It may be older than voice.", theme::text_faint()),
    }
}

/// What the path of my own call says about reaching the server.
pub(in crate::gui) fn reach(path: CallPath) -> Option<&'static str> {
    match path {
        CallPath::Udp => Some("Your call reaches it over UDP, the best way."),
        CallPath::Tcp => Some("Your call reaches it over TCP: UDP is blocked somewhere between you and the server, so sound may lag a little."),
        CallPath::Relay => Some("Your call goes through a relay: neither UDP nor TCP reach the server directly from here."),
        CallPath::Unknown => None,
    }
}

fn fact(label: &'static str, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().type_micro().text_color(theme::text_faint()).child(label.to_uppercase()))
        .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(value.into()))
}

pub(in crate::gui::tiny) fn voice(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = (window, cx);
    let community = facts.community;
    let mut page = div().flex().flex_col().gap(px(28.)).child(page_head("Voice & media", "How this server's audio is doing, and what it asks of a connection."));

    let (title, words, tone) = health(&community.media);
    let host = protocol::join_host(&community.origin);
    page = page.child(
        section("Server health").child(
            card()
                .p(px(16.))
                .gap(px(16.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().size(px(36.)).rounded_full().bg(theme::hover()).flex().items_center().justify_center().child(glyph("icons/signal.svg", 18., tone)))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .child(div().size(px(8.)).rounded_full().bg(tone))
                                        .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(title)),
                                )
                                .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(words)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(32.))
                        .child(fact("Address", if host.is_empty() { "—".to_owned() } else { host.clone() }))
                        .child(fact("tinyserver", if community.capabilities.version.is_empty() { "Older version".to_owned() } else { community.capabilities.version.clone() }))
                        .child(fact("In voice now", community.rooms.iter().map(|room| room.participants.len()).sum::<usize>().to_string())),
                ),
        ),
    );

    // Reachability.
    let path = shell.tiny_state.stats.as_ref().map(|stats| stats.path).and_then(reach);
    page = page.child(
        section("Reaching the server").child(
            card()
                .p(px(16.))
                .gap(px(10.))
                .child(
                    div()
                        .whitespace_normal()
                        .type_ui()
                        .text_color(theme::text())
                        .child(format!("People's calls connect to {} on port {MEDIA_PORT}: over UDP when they can, else over TCP.", if host.is_empty() { "the server".to_owned() } else { host })),
                )
                .child(
                    div()
                        .whitespace_normal()
                        .type_small()
                        .text_color(theme::text_soft())
                        .child(format!("If people get a seat but hear nothing, open UDP and TCP {MEDIA_PORT} to the server in its firewall and on its router.")),
                )
                .children(path.map(|words| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(glyph("icons/check.svg", 14., theme::positive()))
                        .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(words))
                })),
        ),
    );

    // Limits.
    let mut rows = kit::card().child(table_head(&[("Channel", None), ("People", Some(80.)), ("Bitrate", Some(80.)), ("Talk", Some(110.))]));
    let mut ordered: Vec<_> = model::lines(community, &Default::default(), None)
        .into_iter()
        .filter_map(|line| match line {
            model::Line::Room { id } => community.rooms.iter().find(|room| room.id == id),
            _ => None,
        })
        .collect();
    ordered.dedup_by(|a, b| a.id == b.id);
    for room in ordered {
        rows = rows.child(
            table_row(SharedString::from(format!("tiny-voice-room-{}", room.id)), false)
                .min_h(px(40.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(glyph("icons/volume-2.svg", 14., theme::text_faint()))
                        .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(room.name.clone())),
                )
                .child(cell(80., if room.user_limit == 0 { "No limit".to_owned() } else { format!("Up to {}", room.user_limit) }, theme::text_soft()))
                .child(cell(80., format!("{} kb/s", room.bitrate()), theme::text_soft()))
                .child(cell(110., if room.ptt_only { "Push to talk" } else { "Voice activity" }, theme::text_soft())),
        );
    }
    page = page.child(
        section("Limits")
            .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(format!(
                "Each channel's own, set on the Channels page. Bitrate goes from {TINY_BITRATE_MIN} to {TINY_BITRATE_MAX} kb/s ({TINY_BITRATE_DEFAULT} unless set). A channel with no limit takes as many as the server allows."
            )))
            .child(rows),
    );

    // My own call.
    page = page.child(section("Your connection").child(if shell.tiny_call().is_some() {
        card().p(px(16.)).child(shell.tiny_stats_card())
    } else {
        kit::note("icons/info.svg", theme::text_faint(), "Join a channel here to see your own connection: ping, jitter, loss, bitrate and the path your audio takes.")
    }));
    page.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_and_reach_say_what_they_know() {
        assert_eq!(health("up").0, "Audio is up");
        assert_eq!(health("down").2, theme::danger());
        assert_eq!(health("").0, "Not known");
        assert!(reach(CallPath::Tcp).is_some_and(|words| words.contains("UDP is blocked")));
        assert_eq!(reach(CallPath::Unknown), None);
    }
}
