//! The pieces every Settings page is built of (`docs/design/09-settings.md` §2 and §3): the
//! group card and its rows, the switch, the slider, the level meter and the reset slot. Pure
//! drawing; the window wires the clicks.

use palette::IntoColor;
use crate::gui::components::glyph;
use crate::gui::phone::{self, tokens::{self, PhoneType}};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Div, ElementId, Hsla, Rgba, SharedString, Stateful};

/// The content column.
pub const COLUMN: f32 = 480.;
/// The space kept left of every control that has a default, for its reset button.
pub const RESET_SLOT: f32 = 28.;
pub const TRACK: f32 = 200.;
pub const METER: f32 = 238.;
pub const SEGMENTS: usize = 20;

/// White 16 %: a switch that is off, a slider's empty side, an unlit segment.
pub fn switch_off() -> Rgba {
    gpui::rgba(0xffffff29)
}

fn knob() -> Rgba {
    gpui::rgb(0xffffff)
}

/// A row's side padding: 12 on the desktop, the phone's 16 gutter on a phone.
pub fn side() -> f32 {
    if phone::active() {
        tokens::GUTTER
    } else {
        12.
    }
}

/// The page's column: 480 on the desktop, the whole width on a phone.
pub fn column() -> Div {
    if phone::active() {
        div().w_full()
    } else {
        div().w(px(COLUMN))
    }
}

/// A group of rows: `raised`, radius 8, no border (a step, not a line). 12 on a phone.
pub fn card() -> Div {
    div().w_full().flex().flex_col().rounded(px(if phone::active() { 12. } else { theme::R_CARD })).bg(theme::raised())
}

/// The 1 pt line between two rows of a card, inset 12 on both sides (16 on a phone).
pub fn rule() -> Div {
    div().flex_none().h(px(1.)).mx(px(side())).bg(theme::hairline())
}

/// The label above a card.
pub fn group_label(words: impl Into<SharedString>) -> Div {
    let label = div().h(px(24.)).pl(px(side())).flex().items_end().pb(px(4.)).type_micro().text_color(theme::text_faint());
    label.when(phone::active(), |label| label.h(px(32.)).pb(px(6.)).text_size(px(tokens::scaled(12.)))).child(words.into())
}

/// The words under a card, 6 below it.
pub fn caption(words: impl Into<SharedString>) -> Div {
    let words: SharedString = words.into();
    div()
        .pt(px(6.))
        .px(px(side()))
        .max_w(px(456.))
        .type_small()
        .when(phone::active(), |caption| caption.pt(px(8.)).max_w(gpui::relative(1.)).phone_preview())
        .text_color(theme::text_faint())
        .child(crate::gui::shortcuts::label(&words))
}

/// A row: its label (and the line saying what the value does) on the left, the control on the
/// right. 40 high with one line, 52 with two, 2 more for each text step past +0, as the rows
/// of the main window grow. 6 above and below at least, so a label that wraps has room.
pub fn row(label: impl Into<SharedString>, second: Option<SharedString>, control: impl IntoElement) -> Div {
    if phone::active() {
        return phone_row(label.into(), second, control, false);
    }
    let step = theme::text_step().max(0) as f32 * 2.;
    let two = second.is_some();
    div()
        .min_h(px(if two { 52. } else { 40. } + step))
        .px(px(12.))
        .py(px(if two { 8. } else { 6. }))
        .flex()
        .items_center()
        .gap(px(16.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().text_color(theme::text()).child(label.into()))
                .children(second.map(|line| div().type_small().text_color(theme::text_soft()).child(line))),
        )
        .child(div().flex_none().flex().items_center().child(control))
}

/// A row whose control is wider than a phone's row leaves beside its label: a segmented
/// control, a slider, a field. On a phone the control goes under the label, the row's whole
/// width; on the desktop this is [`row`].
pub fn wide_row(label: impl Into<SharedString>, second: Option<SharedString>, control: impl IntoElement) -> Div {
    if phone::active() {
        return phone_row(label.into(), second, control, true);
    }
    row(label, second, control)
}

/// A row on a phone: 15 pt label and 13 pt line, 52 high with one line and 64 with two, so a
/// thumb lands on it. `stacked` puts the control under the words.
fn phone_row(label: SharedString, second: Option<SharedString>, control: impl IntoElement, stacked: bool) -> Div {
    let two = second.is_some();
    let words = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().phone_row().text_color(theme::text()).child(label))
        .children(second.map(|line| div().phone_preview().text_color(theme::text_soft()).child(line)));
    let row = div().min_h(px(if two { 64. } else { 52. })).px(px(tokens::GUTTER)).py(px(10.));
    if stacked {
        return row.flex().flex_col().gap(px(10.)).child(words).child(div().w_full().flex().items_center().child(control));
    }
    row.flex().items_center().gap(px(12.)).child(words).child(div().flex_none().flex().items_center().child(control))
}

/// A row's button sized for a thumb on a phone: 36 high, a pill. The desktop's own elsewhere.
pub fn touch<E: Styled>(button: E) -> E {
    if phone::active() {
        button.h(px(36.)).px(px(16.)).rounded(px(18.)).text_size(px(tokens::scaled(15.)))
    } else {
        button
    }
}

/// The reset slot: 28 kept whether or not the button shows, so controls stay in one column.
/// A phone keeps no empty slot: the control sits on the row's edge until it has a reset.
pub fn reset_slot(button: Option<Stateful<Div>>) -> Div {
    if phone::active() {
        return div().flex_none().flex().items_center().children(button.map(|button| button.mr(px(8.)).size(px(32.))));
    }
    div().flex_none().w(px(RESET_SLOT)).h(px(28.)).flex().items_center().children(button)
}

/// The reset button itself, shown while the value is not the default.
pub fn reset_button(id: impl Into<ElementId>, label: SharedString) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(24.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .tooltip(crate::gui::components::tip(label, None))
        .hover(|style| style.bg(theme::hover()))
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .child(glyph("icons/rotate-ccw.svg", 14., theme::text_faint()))
}

/// A switch: 32 × 18, the knob moving in the same frame. The caller makes the whole row its
/// click target.
pub fn switch(id: impl Into<ElementId>, label: &str, on: bool) -> Stateful<Div> {
    let track = if on { theme::accent() } else { switch_off() };
    let hover: Rgba = if on { theme::accent_hover() } else { theme::border_strong() };
    if phone::active() {
        // 48 × 28 under a thumb, the knob 24.
        return div()
            .id(id)
            .flex_none()
            .w(px(48.))
            .h(px(28.))
            .rounded(px(14.))
            .p(px(2.))
            .flex()
            .when(on, |track| track.justify_end())
            .bg(track)
            .cursor_pointer()
            .role(gpui::Role::Switch)
            .aria_label(SharedString::from(label.to_owned()))
            .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
            .child(div().size(px(24.)).rounded_full().bg(knob()));
    }
    div()
        .id(id)
        .flex_none()
        .w(px(32.))
        .h(px(18.))
        .rounded(px(9.))
        .p(px(2.))
        .flex()
        .when(on, |track| track.justify_end())
        .bg(track)
        .cursor_pointer()
        .tab_index(0)
        .role(gpui::Role::Switch)
        .aria_label(SharedString::from(label.to_owned()))
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .hover(move |style| style.bg(hover))
        .focus_visible(|style| style.shadow(theme::focus_ring_gap(theme::raised())))
        .child(div().size(px(14.)).rounded_full().bg(knob()))
}

/// Where `value` sits on a track `width` wide, from `min` to `max`.
pub fn position(value: u32, (min, max): (u32, u32), width: f32) -> f32 {
    if max <= min {
        return 0.;
    }
    (value.clamp(min, max) - min) as f32 / (max - min) as f32 * width
}

/// The value at `x` along a track `width` wide, snapped to `step`.
pub fn value_at(x: f32, (min, max): (u32, u32), step: u32, width: f32) -> u32 {
    let share = (x / width).clamp(0., 1.);
    let raw = min as f32 + share * (max - min) as f32;
    let step = step.max(1) as f32;
    ((raw / step).round() * step).clamp(min as f32, max as f32) as u32
}

/// A slider's track and thumb, `width` wide; `default` puts the taller tick under its stop.
pub fn slider_track(value: u32, range: (u32, u32), stops: Option<u32>, default: Option<u32>, width: f32) -> Div {
    let at = position(value, range, width);
    let mut track = div()
        .relative()
        .w(px(width))
        .h(px(28.))
        .child(div().absolute().top(px(12.)).left_0().w(px(width)).h(px(4.)).rounded(px(2.)).bg(switch_off()))
        .child(div().absolute().top(px(12.)).left_0().w(px(at)).h(px(4.)).rounded(px(2.)).bg(theme::accent()));
    if let Some(stops) = stops {
        for stop in 0..stops {
            let value = range.0 + stop;
            let x = position(value, range, width);
            let main = default == Some(value);
            track = track.child(
                div()
                    .absolute()
                    .top(px(20.))
                    .left(px(x - 1.))
                    .w(px(2.))
                    .h(px(if main { 8. } else { 6. }))
                    .bg(if main { theme::text_faint() } else { theme::border_strong() }),
            );
        }
    } else if let Some(default) = default {
        let x = position(default, range, width);
        track = track.child(div().absolute().top(px(20.)).left(px(x - 1.)).w(px(2.)).h(px(8.)).bg(theme::text_faint()));
    }
    track.child(
        div()
            .absolute()
            .top(px(6.))
            .left(px(at - 8.))
            .size(px(16.))
            .rounded_full()
            .bg(knob())
            .border_1()
            .border_color(gpui::rgba(0x0000001f))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.3).into(),
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
                inset: false,
            }]),
    )
}

/// How many of the meter's segments a level lights.
pub fn lit(level: Option<u8>) -> usize {
    match level {
        None | Some(0) => 0,
        Some(level) => (usize::from(level.min(100)) * SEGMENTS).div_ceil(100),
    }
}

/// The level meter: 20 segments, lit `text_faint` under the sensitivity line and `positive`
/// from it, so it shows what would be sent. The line is drawn whether or not a test runs.
pub fn meter(level: Option<u8>, gate: u32) -> Div {
    let lit = lit(level);
    let line = position(gate, (0, 100), METER);
    let first_sent = (gate as usize * SEGMENTS).div_ceil(100);
    let segment = |ix: usize| {
        let color: Hsla = if ix >= lit {
            switch_off().into_color()
        } else if ix >= first_sent {
            theme::positive().into_color()
        } else {
            theme::text_faint().into_color()
        };
        div().flex_none().w(px(10.)).h(px(8.)).rounded(px(2.)).bg(color)
    };
    div()
        .relative()
        .w(px(METER))
        .h(px(14.))
        .flex()
        .items_center()
        .gap(px(2.))
        .children((0..SEGMENTS).map(segment))
        .child(div().absolute().top_0().left(px(line - 1.)).w(px(2.)).h(px(14.)).bg(theme::text()))
}

/// The value beside a slider: right-aligned in a box as wide as the meter row's Test button and
/// its gap, so every value ends on the column's edge and V6's track sits under the meter.
pub const VALUE_BOX: f32 = 56.;
/// Between a track's end and its value: the thumb overhangs the track by 8, and 8 stay clear.
pub const VALUE_GAP: f32 = 16.;

pub fn slider_value(words: impl Into<SharedString>) -> Div {
    div().w(px(VALUE_BOX)).flex().justify_end().type_ui().text_color(theme::text_soft()).child(words.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slider_maps_its_track_to_values_and_back_and_snaps_to_stops() {
        assert_eq!(position(50, (0, 100), 200.), 100.);
        assert_eq!(position(250, (0, 100), 200.), 200.);
        assert_eq!(value_at(100., (0, 100), 1, 200.), 50);
        assert_eq!(value_at(-30., (0, 100), 1, 200.), 0);
        assert_eq!(value_at(999., (0, 100), 1, 200.), 100);
        // Five stops from 0 to 4: a third of the way is the nearer stop.
        assert_eq!(value_at(66., (0, 4), 1, 200.), 1);
        assert_eq!(value_at(76., (0, 4), 1, 200.), 2);
    }

    #[test]
    fn the_meter_lights_one_segment_for_any_sound_and_all_twenty_at_the_top() {
        assert_eq!(lit(None), 0);
        assert_eq!(lit(Some(1)), 1);
        assert_eq!(lit(Some(50)), 10);
        assert_eq!(lit(Some(100)), 20);
    }
}
