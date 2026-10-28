//! One voice channel in the pane: its topic, how full it is, push-to-talk and bitrate in the top
//! bar, then the stage, one tile per person in the call, ringed while they speak and labelled
//! with what is true of them ("Server muted"). Right-click a tile for the person menu. Under the
//! stage, Join, or while I am in it the call's own controls and Leave. A channel nobody is in
//! says so, with Join and which devices it will use.

use super::{join_blocked, model, Float, Page};
use crate::gui::components::{button_disabled, button_with_icon, glyph, tip, Avatar, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, MouseButton, MouseDownEvent, SharedString, Stateful};
use protocol::{TinyRoom, TinySeat};

const TILE_W: f32 = 196.;
/// Said under the call while echo cancellation is switched off in Settings → Voice.
const HEADPHONES: &str = "Echo cancellation is off: wear headphones, or others hear themselves.";
const TILE_H: f32 = 148.;

/// A chip of words: `Push to talk`, `96 kb/s`.
pub(super) fn chip(words: impl Into<SharedString>, color: gpui::Rgba, fill: gpui::Rgba) -> Div {
    div().flex_none().h(px(20.)).px(px(6.)).rounded(px(theme::R_CHIP)).bg(fill).flex().items_center().type_micro().text_color(color).child(words.into())
}

/// The round button of the call bar.
fn bar_button(id: &'static str, icon: &'static str, off: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(44.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(if off { theme::danger_soft() } else { theme::selected() })
        .hover(|style| style.bg(theme::pressed()))
        .child(glyph(icon, 20., if off { theme::danger() } else { theme::text() }))
}

impl Shell {
    /// What the top bar says of a channel after its name.
    pub(super) fn tiny_channel_facts(&self, room: &TinyRoom) -> Div {
        let mut facts = div().flex().items_center().gap(px(6.)).min_w_0();
        if !room.topic.trim().is_empty() {
            facts = facts.child(div().flex_none().w(px(1.)).h(px(16.)).bg(theme::hairline())).child(
                div().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child(room.topic.clone()),
            );
        }
        facts = facts.child(chip(
            if room.user_limit > 0 { format!("{} of {}", room.participants.len(), room.user_limit) } else { format!("{} in call", room.participants.len()) },
            if model::full(room) { theme::warning() } else { theme::text_soft() },
            if model::full(room) { theme::warning_soft() } else { theme::hover() },
        ));
        if room.ptt_only {
            facts = facts.child(chip("Push to talk", theme::accent_text(), theme::accent_soft()));
        }
        if room.bitrate_kbps > 0 {
            facts = facts.child(chip(format!("{} kb/s", room.bitrate()), theme::text_soft(), theme::hover()));
        }
        facts
    }

    pub(super) fn tiny_channel(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let mine = page.mine == Some(room.id.as_str());
        let stage: AnyElement = if room.participants.is_empty() {
            self.tiny_empty_stage(page, room, cx)
        } else {
            div()
                .id("tiny-stage")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(24.))
                .flex()
                .flex_wrap()
                .content_center()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .children(room.participants.iter().map(|seat| self.tiny_tile(page, seat, cx)))
                .into_any_element()
        };
        let bar = if room.participants.is_empty() && !mine { None } else { Some(self.tiny_call_bar(page, room, mine, cx)) };
        div().size_full().flex().flex_col().child(stage).children(bar).into_any_element()
    }

    fn tiny_tile(&self, page: &Page, seat: &TinySeat, cx: &mut Context<Self>) -> AnyElement {
        let name = model::name_by_id(page.community, &seat.user_id);
        let speaking = seat.speaking && !seat.server_muted && !seat.suppressed;
        let me = seat.user_id == page.me;
        let color = page.community.member(&seat.user_id).and_then(|member| model::color_of(page.community, member)).map(gpui::rgb).unwrap_or(theme::text());
        let states = model::seat_states(seat);
        let glyphs = super::column::seat_glyphs(seat);
        let user_id = seat.user_id.clone();
        let (percent, local_mute) = self.tiny_state.gain(&seat.user_id);
        div()
            .id(SharedString::from(format!("tiny-tile-{}", seat.user_id)))
            .relative()
            .w(px(TILE_W))
            .h(px(TILE_H))
            .rounded(px(theme::R_CARD + 4.))
            .bg(theme::raised())
            .border_2()
            .border_color(if speaking { theme::positive() } else { theme::hairline() })
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Person { user_id: user_id.clone(), at: event.position }, cx)),
            )
            .child(Avatar::new(&page.key(&seat.user_id), &name, 64.).build())
            .child(
                div()
                    .max_w(px(TILE_W - 24.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w_0().truncate().type_ui().font_weight(theme::SEMIBOLD).text_color(color).child(name))
                    .when(me, |row| row.child(chip("You", theme::text_soft(), theme::hover()))),
            )
            .child(
                div()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .children(states.first().map(|words| {
                        let danger = words.starts_with("Server") || words.starts_with("Can't");
                        chip(*words, if danger { theme::danger() } else { theme::text_soft() }, if danger { theme::danger_soft() } else { theme::hover() })
                    }))
                    .when(local_mute && !me, |row| row.child(chip("Muted for you", theme::warning(), theme::warning_soft())))
                    .when(percent != 100 && !local_mute && !me, |row| row.child(chip(format!("{percent} %"), theme::text_soft(), theme::hover()))),
            )
            .child(div().absolute().top(px(8.)).right(px(8.)).flex().gap(px(4.)).children(glyphs.into_iter().map(|(icon, color, _)| glyph(icon, 14., color))))
            .into_any_element()
    }

    fn tiny_empty_stage(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let blocked = join_blocked(page.live, &page.community.media);
        let join: AnyElement = match blocked {
            _ if !page.community.allowed("community.calls.join") => div().into_any_element(),
            Some(_) => button_disabled("Join channel").h(px(32.)).into_any_element(),
            None if self.tiny_state.call_busy => button_disabled("Joining…").h(px(32.)).into_any_element(),
            None => {
                let room_id = room.id.clone();
                button_with_icon("tiny-join-empty", ButtonKind::Primary, "icons/volume-2.svg", "Join channel")
                    .h(px(32.))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            }
        };
        let devices = self.tiny_device_hint(page);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .px(px(24.))
            .child(div().size(px(56.)).rounded_full().bg(theme::raised()).flex().items_center().justify_center().child(glyph("icons/volume-2.svg", 24., theme::text_faint())))
            .child(div().type_title().text_color(theme::text()).child(format!("Nobody is in {} yet", room.name)))
            .child(div().max_w(px(380.)).text_center().whitespace_normal().type_ui().text_color(theme::text_soft()).child(blocked.map(str::to_owned).unwrap_or(devices)))
            .child(join)
            .children(self.tiny_state.call_error.clone().map(|detail| div().max_w(px(380.)).text_center().whitespace_normal().type_small().text_color(theme::danger()).child(detail)))
            .into_any_element()
    }

    /// Which microphone and speaker a join will use, as far as is known.
    fn tiny_device_hint(&self, page: &Page) -> String {
        if !page.audio && !page.community.media.is_empty() {
            return "This server has no audio turned on, so joining only takes a seat.".to_owned();
        }
        match self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref()).filter(|call| !call.input.is_empty()) {
            Some(call) => format!("You'll join with {} and hear through {}.", call.input, if call.output.is_empty() { "your speaker" } else { &call.output }),
            None => "You'll join with your default microphone and speaker. Change them in Settings › Audio & video.".to_owned(),
        }
    }

    /// Under the stage: Join for a channel I am not in, else mute, deafen, devices and Leave.
    fn tiny_call_bar(&self, page: &Page, room: &TinyRoom, mine: bool, cx: &mut Context<Self>) -> Div {
        let bar = div().flex_none().pb(px(20.)).pt(px(8.)).flex().flex_col().items_center().gap(px(8.));
        if !mine {
            let blocked = join_blocked(page.live, &page.community.media);
            let join: AnyElement = if !page.community.allowed("community.calls.join") {
                div().type_ui().text_color(theme::text_soft()).child("You can't join calls on this server.").into_any_element()
            } else if model::full(room) {
                button_disabled("Channel is full").h(px(36.)).into_any_element()
            } else if blocked.is_some() || self.tiny_state.call_busy {
                button_disabled("Join channel").h(px(36.)).into_any_element()
            } else {
                let room_id = room.id.clone();
                button_with_icon("tiny-join", ButtonKind::Primary, "icons/volume-2.svg", "Join channel")
                    .h(px(36.))
                    .px(px(18.))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            };
            return bar
                .child(join)
                .child(div().type_small().text_color(theme::text_faint()).child(blocked.map(str::to_owned).unwrap_or_else(|| self.tiny_device_hint(page))))
                .children(self.tiny_state.call_error.clone().map(|detail| div().type_small().text_color(theme::danger()).child(detail)));
        }
        let (muted, deafened) = self.tiny_quiet();
        let leave = if self.tiny_state.call_busy {
            button_disabled("Leave").h(px(44.)).px(px(20.)).rounded_full().into_any_element()
        } else {
            div()
                .id("tiny-leave")
                .h(px(44.))
                .px(px(20.))
                .rounded_full()
                .bg(theme::danger_fill())
                .hover(|style| style.bg(theme::danger()))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .tooltip(tip("Leave the call", Some("⌘⇧H")))
                .on_click(cx.listener(|this, _, _, cx| this.tiny_leave(cx)))
                .child(glyph("icons/phone-off.svg", 18., theme::on_accent()))
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::on_accent()).child("Leave"))
                .into_any_element()
        };
        let no_sound = !page.audio && !page.community.media.is_empty();
        bar.child(
            div()
                .p(px(6.))
                .rounded_full()
                .bg(theme::overlay())
                .border_1()
                .border_color(theme::overlay_edge())
                .shadow(theme::shadow_popover())
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    bar_button("tiny-bar-mute", if muted { "icons/mic-off.svg" } else { "icons/mic.svg" }, muted)
                        .tooltip(tip(if muted { "Unmute" } else { "Mute" }, Some("⌘⇧M")))
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_mute(cx))),
                )
                .child(
                    bar_button("tiny-bar-deafen", if deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" }, deafened)
                        .tooltip(tip(if deafened { "Undeafen" } else { "Deafen" }, Some("⌘⇧D")))
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_deafen(cx))),
                )
                .child(
                    bar_button("tiny-bar-devices", "icons/settings.svg", false)
                        .tooltip(tip("Microphone and speaker", None))
                        .on_click(|_, _, cx| crate::gui::settings::open(crate::gui::settings::Section::Audio, cx)),
                )
                .child(leave),
        )
        .when(no_sound, |bar| bar.child(div().type_small().text_color(theme::text_faint()).child("This server has no audio turned on, so you have a seat and no sound.")))
        // mediad cancels echo unless it was switched off (V10); only then does the call say to
        // wear headphones.
        .when(page.audio && !crate::gui::app::prefs::values().processing().echo_cancellation, |bar| bar.child(div().type_small().text_color(theme::text_faint()).child(HEADPHONES)))
        .children(self.tiny_state.call_error.clone().map(|detail| div().type_small().text_color(theme::danger()).child(detail)))
    }
}
