//! The phone's own numbers: the platform's insets, the touch slop, the strip and target
//! heights, and the text run a platform font scale sets. Colour comes from `theme` alone; the
//! phone draws the desktop's palette.

use gpui::{hsla, point, px, rgba, BoxShadow, Styled};
use std::sync::atomic::{AtomicU32, Ordering};

/// The fallback status bar and home indicator: the iOS values. The platform feeds the real ones
/// through [`set_insets`]; until it does, these stand in (see there for what is still owed).
pub const SAFE_TOP: f32 = 44.;
pub const SAFE_BOTTOM: f32 = 34.;
/// How far a press may travel before it is a scroll, not a tap. A thumb wobbles about 4 pt.
pub const SLOP: f32 = 10.;
/// The strip that heads a pushed page, and the smallest thing a thumb is asked to hit.
pub const NAV: f32 = 52.;
pub const TOUCH: f32 = 44.;
/// The gutter every list row, header and message shares.
pub const GUTTER: f32 = 16.;

// Insets. What the platform still owes: iOS `safeAreaInsets` and the keyboard frame, and on
// Android the status and navigation bar insets and whether the IME is visible. It calls
// `set_insets` whenever any of them change; nothing else in the phone reads a constant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Insets {
    /// The status bar, in points.
    pub top: f32,
    /// The home indicator or the navigation bar, in points.
    pub bottom: f32,
    /// The soft keyboard's height in points; 0 while it is down.
    pub keyboard: f32,
}

static INSETS: std::sync::Mutex<Option<Insets>> = std::sync::Mutex::new(None);

/// The one place the platform feeds real window insets into. Call it whenever they change; the
/// phone reads them on every frame through [`safe_top`], [`safe_bottom`] and [`keyboard_open`].
// Nothing calls it until a platform shell does (see the note above), so a build without one says
// nothing about it.
#[allow(dead_code)] // Nothing feeds it until a platform shell does.
pub fn set_insets(insets: Insets) {
    if let Ok(mut held) = INSETS.lock() {
        *held = Some(insets);
    }
}

fn insets() -> Option<Insets> {
    INSETS.lock().ok().and_then(|held| *held)
}

/// The status bar's height now: the platform's when it has said, else [`SAFE_TOP`].
pub fn safe_top() -> f32 {
    insets().map_or(SAFE_TOP, |insets| insets.top)
}

/// The home indicator's height now: the platform's when it has said, else [`SAFE_BOTTOM`].
pub fn safe_bottom() -> f32 {
    insets().map_or(SAFE_BOTTOM, |insets| insets.bottom)
}

/// The soft keyboard's height now: 0 while it is down, or while the platform has not said.
pub fn keyboard() -> f32 {
    insets().map_or(0., |insets| insets.keyboard)
}

/// The bottom padding the composer keeps. While the keyboard is up the home indicator is behind
/// it, so the composer sits on the keyboard's edge instead of reserving room it cannot show.
pub fn bottom_padding(safe_bottom: f32, keyboard: f32) -> f32 {
    if keyboard > 0. {
        0.
    } else {
        safe_bottom
    }
}

/// Whether a press that began at `from` and is now at `to` is still a tap.
pub fn within_slop(from: (f32, f32), to: (f32, f32)) -> bool {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    dx * dx + dy * dy <= SLOP * SLOP
}

// A 200% text run multiplies the phone's type, not the desktop's. 1 by default. The platform's
// font scale is fed through `set_text_scale`; the desktop's own step (`theme::text_step`) is read
// directly, so a step chosen in Settings reaches the phone too.
static SCALE: AtomicU32 = AtomicU32::new(0);

pub fn text_scale() -> f32 {
    match SCALE.load(Ordering::Relaxed) {
        0 => 1.,
        bits => f32::from_bits(bits),
    }
}

/// Sets the phone's text run, clamped to something a screen can hold. The platform calls this
/// when its font scale changes (Android `fontScale`, iOS `preferredContentSizeCategory`); the
/// screenshot harness calls it for `--text-scale`.
#[allow(dead_code)] // Called by the screenshot harness and by the platform's font-scale hook.
pub fn set_text_scale(scale: f32) -> f32 {
    let scale = if scale.is_finite() { scale.clamp(1., 3.) } else { 1. };
    SCALE.store(scale.to_bits(), Ordering::Relaxed);
    scale
}

/// A type size for a desktop step and a phone run: each step adds a point, then the run
/// multiplies, rounded to a half point.
pub fn type_size(size: f32, step: i32, scale: f32) -> f32 {
    ((size + step as f32) * scale * 2.).round() / 2.
}

/// A type size at the current phone run and the current desktop step.
pub fn scaled(size: f32) -> f32 {
    type_size(size, crate::gui::theme::text_step(), text_scale())
}

/// A sheet floats above the page: a real, soft shadow cast upward from its 12 pt top edge.
pub fn shadow_sheet() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: hsla(0., 0., 0., 0.45).into(),
        offset: point(px(0.), px(-8.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// The 1 pt light at the top of a bar, painted inside it.
pub fn top_highlight() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba(0xffffff0f).into(),
        offset: point(px(0.), px(1.)),
        blur_radius: px(0.),
        spread_radius: px(0.),
        inset: true,
    }]
}

/// The phone's type for its own strips and sheets: 17/22 titles, 13/18 lines, 12/16 stamps.
/// Every size goes through [`scaled`], so the desktop step and the phone's run both reach it.
pub trait PhoneType: Styled + Sized {
    fn phone_title(self) -> Self {
        self.text_size(px(scaled(17.))).line_height(px(scaled(22.))).font_weight(crate::gui::theme::SEMIBOLD)
    }
    /// 15/20: a sheet row's label.
    fn phone_row(self) -> Self {
        self.text_size(px(scaled(15.))).line_height(px(scaled(20.)))
    }
    fn phone_preview(self) -> Self {
        self.text_size(px(scaled(13.))).line_height(px(scaled(18.)))
    }
    fn phone_stamp(self) -> Self {
        self.text_size(px(scaled(12.))).line_height(px(scaled(16.))).font_weight(crate::gui::theme::MEDIUM)
    }
}

impl<T: Styled + Sized> PhoneType for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumb_can_hit_the_strip() {
        assert!(NAV >= TOUCH);
    }

    #[test]
    fn a_type_size_takes_the_desktop_step_then_the_run_and_rounds_to_half_points() {
        assert_eq!(type_size(15., 0, 1.), 15.);
        // One desktop step adds a point; the run doubles what is left.
        assert_eq!(type_size(15., 1, 1.), 16.);
        assert_eq!(type_size(15., 0, 2.), 30.);
        assert_eq!(type_size(11., 0, 1.2), 13.);
        assert_eq!(type_size(11., -1, 1.), 10.);
    }

    #[test]
    fn a_press_stays_a_tap_within_the_slop_and_becomes_a_scroll_past_it() {
        assert!(within_slop((10., 10.), (10., 10.)));
        assert!(within_slop((10., 10.), (16., 16.)));
        assert!(within_slop((0., 0.), (SLOP, 0.)));
        assert!(!within_slop((0., 0.), (SLOP + 0.5, 0.)));
        assert!(!within_slop((0., 0.), (0., 40.)));
    }

    #[test]
    fn the_bottom_padding_drops_while_the_keyboard_is_up() {
        assert_eq!(bottom_padding(SAFE_BOTTOM, 0.), SAFE_BOTTOM);
        assert_eq!(bottom_padding(SAFE_BOTTOM, 300.), 0.);
        assert_eq!(bottom_padding(0., 0.), 0.);
    }

}
