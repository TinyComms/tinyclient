//! The page before its server answers. The column keeps the rows the loaded list will hold, at
//! their own heights and insets, so what arrives lands where its shape already was; the pane is
//! the one splash in the window, the server's mark over its name. The mark breathes while it
//! waits, and nothing else moves: a screen with nothing on it yet has no other way to say it is
//! working (`docs/design/01-system.md` §3, Loading).

use crate::gui::components::{network_icon, skeleton};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Animation, AnimationExt, AnimationElement, AnyElement, Div, Img, Stateful};
use protocol::Network;
use std::time::Duration;

/// A breath of the mark: this long, and this deep.
const BREATH: Duration = Duration::from_millis(2400);
const DIM: f32 = 0.62;

/// `--shot` only: `TINYCLIENT_SHOT_TINY=loading` draws the page as it is before the server
/// answers, so the splash and the column's shapes can be looked at without waiting on a fetch.
pub(super) fn shot_loading() -> bool {
    #[cfg(feature = "gui-shot")]
    return std::env::var("TINYCLIENT_SHOT_TINY").is_ok_and(|name| name == "loading");
    #[cfg(not(feature = "gui-shot"))]
    false
}

impl Shell {
    /// The pane's half. A window too narrow for the sidebar keeps the column's shapes above it,
    /// as the loaded page keeps the list above the home or the channel.
    pub(in crate::gui) fn tiny_loading_pane(&self, beside: bool) -> AnyElement {
        if beside {
            return div().id("tiny-page").w_full().flex_1().min_h_0().flex().flex_col().justify_center().child(splash()).into_any_element();
        }
        div().id("tiny-page").w_full().flex_1().min_h_0().overflow_y_scroll().flex().flex_col().child(rows()).child(splash()).into_any_element()
    }

    /// The column's half: the list's own rows, as shapes.
    pub(in crate::gui) fn tiny_loading_column(&self) -> AnyElement {
        div()
            .id("tiny-channels")
            .size_full()
            .overflow_y_scroll()
            .pt(px(4.))
            .pb(px(12.))
            .flex()
            .flex_col()
            .child(rows())
            .into_any_element()
    }
}

/// The rows the loaded column will hold: text channels, then two categories of rooms, one of them
/// with a seat. Nothing here is a control, so nothing lights under the pointer.
fn rows() -> Div {
    let one = theme::row_height(theme::ROW_ONE);
    let mut list = div().w_full().flex().flex_col().child(label(44.));
    for (at, width) in [0.44, 0.30, 0.52, 0.36].into_iter().enumerate() {
        list = list.child(row(("tiny-loading-text", at), one, 16., width, false));
    }
    for (group, (rooms, seated)) in [(&[0.46, 0.32, 0.40][..], true), (&[0.38, 0.50][..], false)].into_iter().enumerate() {
        list = list.child(header(group, group == 0));
        for (at, width) in rooms.iter().copied().enumerate() {
            // The second room in a category carries its occupancy, as a room with people does.
            list = list.child(row(("tiny-loading-room", group * 8 + at), one, 16., width, at == 1));
        }
        if seated {
            list = list.child(seat_row(group));
        }
    }
    list
}

/// A section label's shape: as tall as the label takes, its own width in `hover`.
fn label(width: f32) -> Div {
    div().flex_none().w_full().px(px(theme::INSET + 2.)).pt(px(10.)).pb(px(2.)).flex().items_center().child(skeleton(width, 10.))
}

/// A category's shape: the 12 chevron, then the name in capitals.
fn header(ix: usize, first: bool) -> Div {
    let height = if first { theme::SECTION_FIRST } else { theme::SECTION + 6. };
    div()
        .flex_none()
        .w_full()
        .h(px(height))
        .pl(px(theme::INSET + 2.))
        .pr(px(theme::INSET + 4.))
        .pb(px(4.))
        .flex()
        .items_end()
        .gap(px(4.))
        .child(skeleton(12., 12.))
        .child(skeleton(if ix == 0 { 76. } else { 60. }, 10.))
}

/// A room's, or a text channel's, shape: the glyph's box, a name, and a count chip where one sits.
fn row(id: (&'static str, usize), height: f32, lead: f32, width: f32, count: bool) -> Stateful<Div> {
    let name = div().w(gpui::relative(width)).h(px(10.)).rounded(px(theme::R_CHIP)).bg(theme::raised());
    let content = div()
        .size_full()
        .px(px(8.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(skeleton(lead, lead))
        .child(div().flex_1().min_w_0().flex().flex_col().child(name))
        .when(count, |row| row.child(skeleton(22., 12.)));
    plain(id, height, content)
}

/// Someone seated in a room: indented 24, an avatar 20, their name.
fn seat_row(ix: usize) -> Stateful<Div> {
    let name = div().w(gpui::relative(0.30)).h(px(10.)).rounded(px(theme::R_CHIP)).bg(theme::raised());
    let content = div()
        .size_full()
        .px(px(8.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().flex_none().pl(px(24.)))
        .child(skeleton(20., 20.))
        .child(div().flex_1().min_w_0().child(name));
    plain(("tiny-loading-seat", ix), theme::row_height(theme::ROW_ONE), content)
}

/// A row at a list row's own geometry, without the hover fill a row that can be clicked has.
fn plain(id: (&'static str, usize), height: f32, content: Div) -> Stateful<Div> {
    div().id(id).w_full().h(px(height)).px(px(theme::INSET)).py(px(1.)).child(content)
}

/// The mark, the server's name, and what is on its way, in the first launch's own words' shape.
fn splash() -> Div {
    div()
        .w_full()
        .flex_none()
        .py(px(24.))
        .flex()
        .flex_col()
        .items_center()
        .child(mark())
        .child(div().mt(px(16.)).type_headline().text_color(theme::text()).child("Tiny server"))
        .child(
            div()
                .mt(px(6.))
                .max_w(px(320.))
                .text_center()
                .type_body()
                .text_color(theme::text_soft())
                .child("Getting this server's channels, people and rooms."),
        )
}

/// The mark breathes while the page is on its way: its opacity alone, full at the start of each
/// breath. GPUI draws it at that first frame, full, under Reduce Motion.
fn mark() -> AnimationElement<Img> {
    let breathe = |into: f32| DIM + (1. - DIM) * (0.5 + 0.5 * (into * std::f32::consts::TAU).cos());
    network_icon(Network::Tiny, theme::BRAND_MARK).with_animation(
        "tiny-loading-mark",
        Animation::new(BREATH).repeat_synced().with_easing(breathe),
        |mark, opacity| mark.opacity(opacity),
    )
}
