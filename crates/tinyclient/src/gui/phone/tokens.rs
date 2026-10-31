//! The phone's own numbers (`research_notes/mobile/design/10-visual-direction.md`, "Paper
//! Signal"). The desktop palette stays as it is: at 600 and wider nothing here is read.

use crate::gui::theme;
use gpui::{px, rgb, rgba, Rgba, Styled};

/// Chrome heights: a nav strip, the tab bar, and the safe area the tab bar carries.
pub const NAV: f32 = 52.;
pub const TAB: f32 = 49.;
/// The home indicator's room. The pin has no `WindowInsets` getter (`20-implementation-map`
/// §3.4), so the phone form reserves the iOS value rather than reading it.
pub const SAFE_BOTTOM: f32 = 34.;
pub const ROW: f32 = 56.;
pub const TOUCH: f32 = 44.;

/// The one accent: amber means "for you" — a mention, the Needs me badge, the Inbox dot, and
/// the rule beside a message of mine.
pub fn accent() -> Rgba {
    rgb(0xe9a23b)
}
pub fn accent_text() -> Rgba {
    rgb(0xffc56e)
}
pub fn on_accent() -> Rgba {
    rgb(0x1a1204)
}
pub fn accent_soft() -> Rgba {
    rgba(0xe9a23b2e)
}

/// Opaque chrome. One step above the page, as `surface` is in the token file.
pub fn surface() -> Rgba {
    theme::sidebar()
}

pub fn strip_rule() -> Rgba {
    theme::hairline()
}

/// The phone's type scale: 17/24 body, 15/20 interface, 13/18 previews, 12/16 timestamps,
/// 11/14 tab labels. One point larger than the desktop at every step, because the eye is 30 cm
/// from the glass, not 60.
pub trait PhoneType: Styled + Sized {
    fn phone_title(self) -> Self {
        self.text_size(px(17.)).line_height(px(22.)).font_weight(theme::SEMIBOLD)
    }
    fn phone_row(self) -> Self {
        self.text_size(px(15.)).line_height(px(20.))
    }
    fn phone_preview(self) -> Self {
        self.text_size(px(13.)).line_height(px(18.))
    }
    fn phone_stamp(self) -> Self {
        self.text_size(px(12.)).line_height(px(16.)).font_weight(theme::MEDIUM)
    }
    fn phone_tab(self) -> Self {
        self.text_size(px(11.)).line_height(px(14.)).font_weight(theme::MEDIUM)
    }
}

impl<T: Styled + Sized> PhoneType for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_bar_is_49_plus_the_safe_area() {
        assert_eq!(TAB + SAFE_BOTTOM, 83.);
        assert!(ROW >= TOUCH);
        assert!(NAV >= TOUCH);
    }
}
