//! The phone's own numbers and palette (`research_notes/mobile/design/10-visual-direction.md`,
//! "Paper Signal"). The desktop palette stays as it is: at 600 and wider none of the colour
//! values here are read, and `theme` only branches on [`super::active`].
//!
//! The palette is deliberately a step quieter than the desktop's: a near-black page, opaque
//! chrome one step up, raised surfaces for anything that floats, and hairlines that are lines
//! rather than borders. Amber is still the only accent, and it means "for you".

use gpui::{hsla, point, px, rgb, rgba, BoxShadow, Rgba, Styled};
use std::sync::atomic::{AtomicU32, Ordering};

/// The status bar. The pin has no `WindowInsets` getter (`20-implementation-map` §3.4), so the
/// phone form reserves the iOS value rather than reading it.
pub const SAFE_TOP: f32 = 44.;
pub const SAFE_BOTTOM: f32 = 34.;
/// Chrome heights: the nav strip and the tab bar. The tab bar carries the safe area below it.
pub const NAV: f32 = 52.;
pub const TAB: f32 = 56.;
/// A conversation row. 64, not 56: a thumb can find it and the two lines breathe.
pub const ROW: f32 = 64.;
pub const TOUCH: f32 = 44.;
/// The gutter every list row, header and message shares.
pub const GUTTER: f32 = 16.;

// A 200% text run multiplies the phone's type, not the desktop's. 1 by default.
static SCALE: AtomicU32 = AtomicU32::new(0);

pub fn text_scale() -> f32 {
    match SCALE.load(Ordering::Relaxed) {
        0 => 1.,
        bits => f32::from_bits(bits),
    }
}

/// Sets a text scale for the phone, clamped to something a screen can hold.
#[cfg_attr(not(any(test, feature = "gui-shot")), allow(dead_code))]
pub fn set_text_scale(scale: f32) -> f32 {
    let scale = if scale.is_finite() { scale.clamp(1., 3.) } else { 1. };
    SCALE.store(scale.to_bits(), Ordering::Relaxed);
    scale
}

/// A type size at the current phone scale.
pub fn scaled(size: f32) -> f32 {
    (size * text_scale() * 2.).round() / 2.
}

/// A row height at the current phone scale: rows grow with their text, so nothing clips.
pub fn row_height() -> f32 {
    ROW + (text_scale() - 1.) * 32.
}

pub fn tab_height() -> f32 {
    TAB
}

// Surfaces.
/// The page behind every phone screen.
pub fn page() -> Rgba {
    rgb(0x131418)
}
/// Nav strips, tab bars, sheets' headers: anything pinned.
pub fn surface() -> Rgba {
    rgb(0x191b1f)
}
/// Cards, fields, the composer card.
pub fn raised() -> Rgba {
    rgb(0x1f2227)
}

// Lines. 0.07 and 0.12: a hairline reads as a seam, a strong hairline as an edge.
pub fn hairline() -> Rgba {
    rgba(0xffffff12)
}
pub fn hairline_strong() -> Rgba {
    rgba(0xffffff1f)
}
/// The 1 pt light along the top of a bar: chrome catches the room, it does not glow.
pub fn highlight() -> Rgba {
    rgba(0xffffff0d)
}

// Text.
pub fn text() -> Rgba {
    rgb(0xf2f3f5)
}
/// 8.96:1 on the page; the preview and the row's second line.
pub fn text_soft() -> Rgba {
    rgb(0xaeb3bd)
}
/// 5.73:1 on the page; the lowest level that may still carry meaning.
pub fn text_faint() -> Rgba {
    rgb(0x8a909b)
}

// The one accent: amber means "for you" — a mention, the Needs me badge, the Inbox dot, and
// the rule beside a message of mine.
pub fn accent() -> Rgba {
    rgb(0xe9a23b)
}
pub fn accent_text() -> Rgba {
    rgb(0xffc56e)
}
pub fn on_accent() -> Rgba {
    rgb(0x1a1204)
}
/// 18% amber, for the wash under a mention and the edit mark.
pub fn accent_soft() -> Rgba {
    rgba(0xe9a23b2e)
}

/// The 6% white a press leaves. No colour shift, no scale.
pub fn pressed() -> Rgba {
    rgba(0xffffff0f)
}
pub fn hover() -> Rgba {
    rgba(0xffffff0a)
}
pub fn selected() -> Rgba {
    rgba(0xffffff14)
}

/// A sheet floats above the page: a real, soft shadow under a 12 pt top edge.
pub fn shadow_sheet() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: hsla(0., 0., 0., 0.45),
        offset: point(px(0.), px(8.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// The 1 pt light at the top of a bar, painted inside it.
pub fn top_highlight() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: highlight().into(),
        offset: point(px(0.), px(1.)),
        blur_radius: px(0.),
        spread_radius: px(0.),
        inset: true,
    }]
}

/// The phone's type scale: 17/24 body, 15/20 interface, 13/18 previews, 12/16 timestamps,
/// 11/14 tab labels. One point larger than the desktop at every step, because the eye is 30 cm
/// from the glass, not 60. Tab labels stay at 11 when the text scale rises: the icons and the
/// badge must not move, and a tab label may truncate.
pub trait PhoneType: Styled + Sized {
    fn phone_title(self) -> Self {
        self.text_size(px(scaled(17.))).line_height(px(scaled(22.))).font_weight(crate::gui::theme::SEMIBOLD)
    }
    fn phone_row(self) -> Self {
        self.text_size(px(scaled(15.))).line_height(px(scaled(20.)))
    }
    fn phone_preview(self) -> Self {
        self.text_size(px(scaled(13.))).line_height(px(scaled(18.)))
    }
    fn phone_stamp(self) -> Self {
        self.text_size(px(scaled(12.))).line_height(px(scaled(16.))).font_weight(crate::gui::theme::MEDIUM)
    }
    fn phone_tab(self) -> Self {
        self.text_size(px(11.)).line_height(px(14.)).font_weight(crate::gui::theme::MEDIUM)
    }
}

impl<T: Styled + Sized> PhoneType for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_bar_is_56_plus_the_safe_area() {
        assert_eq!(TAB + SAFE_BOTTOM, 90.);
        assert!(ROW >= TOUCH);
        assert!(NAV >= TOUCH);
    }

    #[test]
    fn a_row_grows_with_a_bigger_text_scale_and_never_shrinks() {
        set_text_scale(1.);
        assert_eq!(row_height(), 64.);
        set_text_scale(2.);
        assert_eq!(row_height(), 96.);
        set_text_scale(1.);
    }

    #[test]
    fn the_tab_cell_is_seven_icon_gap_indicator_gap_label_and_five() {
        // 7 pad + 22 icon + 3 + 2 indicator + 3 + 14 label + 5 pad = 56, nothing touching the rule.
        assert_eq!(7. + 22. + 3. + 2. + 3. + 14. + 5., TAB);
    }
}
