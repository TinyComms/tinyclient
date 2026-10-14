//! Presence on an avatar's bottom-right corner. The shape carries the state and the color only
//! repeats it: a disc, a crescent, a disc with a bar, a hollow ring, a phone badge. Each is two
//! or three plain rounded divs, so nothing here needs a mask or an image.

use crate::gui::components::glyph;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Div, Rgba, SharedString};
use protocol::Presence;

/// What a person's corner shows. The contract's four states, plus being in a call, which the
/// shell knows from the call it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Online,
    Idle,
    Dnd,
    /// In a meeting, a call or a huddle: a solid red disc, apart from do-not-disturb's bar.
    Busy,
    Offline,
    InCall,
}

impl Shape {
    /// Unknown is not a shape: nothing is drawn, and it never borrows the hollow ring.
    pub fn of(presence: Presence) -> Option<Shape> {
        match presence {
            Presence::Unknown => None,
            Presence::Online => Some(Shape::Online),
            Presence::Idle => Some(Shape::Idle),
            Presence::Dnd => Some(Shape::Dnd),
            Presence::Busy => Some(Shape::Busy),
            Presence::Offline => Some(Shape::Offline),
        }
    }
}

/// The opaque color behind the avatar, which the ring takes so the dot looks cut out of it.
/// A row that fills on hover names its group and what the fill composites to.
#[derive(Clone)]
pub struct Ring {
    pub rest: Rgba,
    pub hover: Option<(SharedString, Rgba)>,
}

impl Ring {
    pub fn on(surface: Rgba) -> Ring {
        Ring { rest: surface, hover: None }
    }

    /// A list row on `surface`: `selected` at rest when it is the current row, `hover` under the
    /// pointer otherwise.
    pub fn row(surface: Rgba, selected: bool, group: impl Into<SharedString>) -> Ring {
        if selected {
            Ring::on(theme::over(theme::selected(), surface))
        } else {
            Ring {
                rest: surface,
                hover: Some((group.into(), theme::over(theme::hover(), surface))),
            }
        }
    }

    fn fill(&self, piece: Div) -> Div {
        let piece = piece.bg(self.rest);
        match &self.hover {
            Some((group, color)) => {
                let color = *color;
                piece.group_hover(group.clone(), move |style| style.bg(color))
            }
            None => piece,
        }
    }
}

/// Dot diameter, ring width, and the phone badge's diameter, for an avatar this many points
/// across. `None` below 20: the avatar is too small to carry a legible shape.
pub fn metrics(avatar: f32) -> Option<(f32, f32, f32)> {
    Some(match avatar as u32 {
        0..=19 => return None,
        20..=31 => (8., 2., 8.),
        32..=39 => (10., 2., 14.),
        40..=63 => (12., 2., 16.),
        64..=79 => (14., 2., 20.),
        _ => (18., 3., 24.),
    })
}

/// The ringed shape, placed on the corner of the `relative` box it is added to.
pub fn dot(shape: Shape, avatar: f32, ring: &Ring) -> Option<Div> {
    let (dot, width, badge) = metrics(avatar)?;
    // At 24 there is no room for a glyph: a call reads as online.
    let shape = if shape == Shape::InCall && avatar < 32. { Shape::Online } else { shape };
    let diameter = if shape == Shape::InCall { badge } else { dot };
    let disc = div().size(px(diameter)).rounded(px(diameter / 2.)).flex_none();
    let glyph_of = |shape| match shape {
        Shape::Online => disc.bg(theme::online()),
        Shape::Idle => {
            // A bite out of the top-left leaves a crescent.
            let bite = (diameter * 0.62).round();
            let inset = -(diameter * 0.08).round().max(1.);
            disc.bg(theme::idle()).relative().child(
                ring.fill(div().absolute().top(px(inset)).left(px(inset)).size(px(bite)).rounded(px(bite / 2.))),
            )
        }
        Shape::Dnd => {
            let bar = if diameter >= 14. { 3. } else { 2. };
            disc.bg(theme::dnd()).flex().items_center().justify_center().child(
                ring.fill(div().w(px((diameter * 0.6).round())).h(px(bar)).rounded(px(bar / 2.))),
            )
        }
        Shape::Busy => disc.bg(theme::dnd()),
        Shape::Offline => disc.border_2().border_color(theme::offline()),
        Shape::InCall => disc.bg(theme::online()).flex().items_center().justify_center().child(glyph(
            "icons/phone.svg",
            (diameter * 0.56).round(),
            theme::on_accent(),
        )),
    };
    let outer = diameter + 2. * width;
    // The ringed disc reaches 2 past the avatar's box, a call badge 4; on the large avatar of a
    // card it sits flush.
    let overhang = match (avatar >= 64., shape == Shape::InCall) {
        (true, _) => 0.,
        (false, true) => -4.,
        (false, false) => -2.,
    };
    Some(
        ring.fill(
            div()
                .absolute()
                .right(px(overhang))
                .bottom(px(overhang))
                .size(px(outer))
                .rounded(px(outer / 2.))
                .flex()
                .items_center()
                .justify_center(),
        )
        .child(glyph_of(shape)),
    )
}

/// The shape alone at `diameter`, for a line of text: the profile card and the status menu.
pub fn inline(shape: Shape, diameter: f32, surface: Rgba) -> Div {
    let ring = Ring::on(surface);
    let disc = div().size(px(diameter)).rounded(px(diameter / 2.)).flex_none();
    match shape {
        Shape::Online | Shape::InCall => disc.bg(theme::online()),
        Shape::Idle => {
            let bite = (diameter * 0.62).round();
            disc.bg(theme::idle()).relative().child(
                ring.fill(div().absolute().top(px(-1.)).left(px(-1.)).size(px(bite)).rounded(px(bite / 2.))),
            )
        }
        Shape::Dnd => disc.bg(theme::dnd()).flex().items_center().justify_center().child(
            ring.fill(div().w(px((diameter * 0.6).round())).h(px(2.)).rounded(px(1.))),
        ),
        Shape::Busy => disc.bg(theme::dnd()),
        Shape::Offline => disc.border_2().border_color(theme::offline()),
    }
}

/// The word that goes with the shape. Slack and Teams say "Away" where Discord says "Idle".
pub fn word(presence: Presence, network: protocol::Network) -> &'static str {
    match (presence, network) {
        (Presence::Online, _) => "Online",
        (Presence::Idle, protocol::Network::Discord) => "Idle",
        (Presence::Idle, _) => "Away",
        (Presence::Busy, _) => "Busy",
        (Presence::Dnd, _) => "Do not disturb",
        (Presence::Offline, _) => "Offline",
        (Presence::Unknown, _) => "",
    }
}

/// A status as one line: the emoji, then the words. A network that already starts the words with
/// the emoji (Slack's "🎧 synthwave") is not given it twice.
pub fn status_words(emoji: &str, text: &str) -> String {
    let (emoji, text) = (emoji.trim(), text.trim());
    if emoji.is_empty() || text.starts_with(emoji) {
        return text.to_owned();
    }
    format!("{emoji} {text}").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_avatar_size_has_its_dot_and_the_smallest_has_none() {
        assert_eq!(metrics(16.), None);
        assert_eq!(metrics(24.), Some((8., 2., 8.)));
        assert_eq!(metrics(32.), Some((10., 2., 14.)));
        assert_eq!(metrics(36.), Some((10., 2., 14.)));
        assert_eq!(metrics(40.), Some((12., 2., 16.)));
        assert_eq!(metrics(80.), Some((18., 3., 24.)));
    }

    #[test]
    fn unknown_is_no_shape_at_all() {
        assert_eq!(Shape::of(Presence::Unknown), None);
        assert_eq!(Shape::of(Presence::Offline), Some(Shape::Offline));
        assert_eq!(word(Presence::Idle, protocol::Network::Slack), "Away");
        assert_eq!(word(Presence::Idle, protocol::Network::Discord), "Idle");
    }

    #[test]
    fn a_status_emoji_is_drawn_once() {
        assert_eq!(status_words("🎧", "🎧 synthwave"), "🎧 synthwave");
        assert_eq!(status_words("🎧", "synthwave"), "🎧 synthwave");
        assert_eq!(status_words("🌴", ""), "🌴");
        assert_eq!(status_words("", "Back Monday"), "Back Monday");
    }
}
