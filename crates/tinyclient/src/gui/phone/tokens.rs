//! The phone's own numbers and palette (`research_notes/mobile/design/10-visual-direction.md`,
//! "Paper Signal"). The desktop palette stays as it is: at 600 and wider none of the colour
//! values here are read, and `theme` only branches on [`super::active`].
//!
//! The palette is deliberately a step quieter than the desktop's: a near-black page, opaque
//! chrome one step up, raised surfaces for anything that floats, and hairlines that are lines
//! rather than borders. Amber is still the only accent, and it means "for you".

use gpui::{hsla, point, px, rgb, rgba, BoxShadow, Rgba, Styled};
use std::sync::atomic::{AtomicU32, Ordering};

/// The fallback status bar and home indicator: the iOS values. The platform feeds the real ones
/// through [`set_insets`]; until it does, these stand in (see there for what is still owed).
pub const SAFE_TOP: f32 = 44.;
pub const SAFE_BOTTOM: f32 = 34.;
/// How far a press may travel before it is a scroll, not a tap. A thumb wobbles about 4 pt.
pub const SLOP: f32 = 10.;
/// A sheet may take this share of the window's height, and no more: the list behind stays visible.
pub const SHEET_CAP: f32 = 0.6;
/// Chrome heights: the nav strip and the tab bar. The tab bar carries the safe area below it.
pub const NAV: f32 = 52.;
/// 8 above the icon, 22 of it, 10 between it and the label, 14 of label, 2 under: 56.
pub const TAB: f32 = 56.;
/// A conversation row. 64, not 56: a thumb can find it and the two lines breathe.
pub const ROW: f32 = 64.;
pub const TOUCH: f32 = 44.;
/// The gutter every list row, header and message shares.
pub const GUTTER: f32 = 16.;
/// Where a row's rule starts: past the 40 pt avatar and its 16 + 12 of air.
pub const RULE_START: f32 = 76.;

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

/// The tallest a sheet may be in a window `window_height` tall.
pub fn sheet_cap(window_height: f32) -> f32 {
    window_height * SHEET_CAP
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

/// A row height for a desktop step and a phone run. Rows grow with their text, so nothing clips.
/// The step adds 4 a level, as the desktop's 48-point rows do.
pub fn row_height_for(step: i32, scale: f32) -> f32 {
    ROW + 4. * step as f32 + (scale - 1.) * 32.
}

/// A row height now: 64 at 100%, 96 at 200%, and the desktop step on top.
pub fn row_height() -> f32 {
    row_height_for(crate::gui::theme::text_step(), text_scale())
}

/// A sheet's one-line choice: 56 at 100%, 68 at 200%, so a 40 pt line of type keeps its air.
pub fn sheet_row() -> f32 {
    56. + (text_scale() - 1.) * 12.
}

/// Lines a tab label may take: one at 100%, two once the run is above it, so "Needs me" wraps
/// at 200% instead of truncating.
pub fn tab_lines(scale: f32) -> u32 {
    if scale > 1. {
        2
    } else {
        1
    }
}

/// The tab cell's height: 56 with one 14 pt line of label under the icon, growing by the label's
/// extra lines. The icon and its badge stay where they are at every scale.
pub fn tab_height_for(lines: u32, label_line: f32) -> f32 {
    TAB - 14. + lines as f32 * label_line
}

pub fn tab_height() -> f32 {
    tab_height_for(tab_lines(text_scale()), scaled(14.))
}

// Surfaces. A ladder, not one flat black: the page is the floor, chrome sits one step above
// it, and anything that floats (a card, a field, a sheet) one above that.
/// The page behind every phone screen.
pub fn page() -> Rgba {
    rgb(0x14151a)
}
/// Nav strips, tab bars, the composer's dock: anything pinned.
pub fn surface() -> Rgba {
    rgb(0x1b1d22)
}
/// Cards, fields, the composer card, sheets.
pub fn raised() -> Rgba {
    rgb(0x23262c)
}

// Lines. 0.07 and 0.12: a hairline reads as a seam, a strong hairline as an edge. A row's rule
// is quieter than a seam (0.055) and starts past the avatar, so the column of faces stays whole.
/// Chrome edges and section dividers.
pub fn hairline() -> Rgba {
    rgba(0xffffff12)
}
/// Between two rows of a list: 1 pt at 5.5%, inset to x = 76.
pub fn rule() -> Rgba {
    rgba(0xffffff0e)
}
/// The edge of a floating surface: a sheet's top, a field's border.
pub fn hairline_strong() -> Rgba {
    rgba(0xffffff1f)
}
/// The 1 pt light along the top of a bar: chrome catches the room, it does not glow.
pub fn highlight() -> Rgba {
    rgba(0xffffff0f)
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
/// 18% amber, for the edit mark.
pub fn accent_soft() -> Rgba {
    rgba(0xe9a23b2e)
}
/// The accent under a hover, on the phone. The desktop's own is in `theme`.
pub fn accent_hover() -> Rgba {
    rgb(0xf2b45c)
}
/// The accent under a press.
pub fn accent_pressed() -> Rgba {
    rgb(0xd08e2c)
}
/// 10% amber under a message that mentions me.
pub fn accent_wash() -> Rgba {
    rgba(0xe9a23b1a)
}
/// The 35% amber halo of a focused field.
pub fn accent_halo() -> Rgba {
    rgba(0xe9a23b59)
}
/// Selected text in a field: 45% amber.
pub fn selection() -> Rgba {
    rgba(0xe9a23b73)
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

/// A ring painted outside a shape: what separates a badge from the icon under it.
pub fn ring(color: Rgba, width: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: color.into(),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(width),
        inset: false,
    }]
}

/// The phone's type scale: 17/24 body, 15/20 interface, 13/18 previews, 12/16 timestamps,
/// 11/14 tab labels. One point larger than the desktop at every step, because the eye is 30 cm
/// from the glass, not 60. Every size goes through [`scaled`], so the desktop step and the
/// phone's run both reach it.
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
    /// Tab labels: 11/14 at 100%, and they scale with the run. The icon and badge do not.
    fn phone_tab(self) -> Self {
        self.text_size(px(scaled(11.))).line_height(px(scaled(14.))).font_weight(crate::gui::theme::MEDIUM)
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
    fn a_row_grows_with_the_phone_run_and_the_desktop_step_and_never_shrinks() {
        assert_eq!(row_height_for(0, 1.), 64.);
        assert_eq!(row_height_for(0, 2.), 96.);
        assert_eq!(row_height_for(2, 1.), 72.);
        assert_eq!(row_height_for(-1, 1.), 60.);
        assert!(row_height_for(0, 1.5) > row_height_for(0, 1.));
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
    fn the_tab_cell_is_56_at_one_line_and_grows_only_with_extra_lines() {
        assert_eq!(tab_height_for(1, 14.), TAB);
        assert_eq!(tab_height_for(2, 14.), 70.);
        assert_eq!(tab_lines(1.), 1);
        assert_eq!(tab_lines(2.), 2);
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
    fn a_sheet_takes_sixty_percent_of_the_window_and_no_more() {
        assert!((sheet_cap(800.) - 480.).abs() < 1e-3);
        assert!(sheet_cap(640.) < 640.);
    }

    #[test]
    fn the_bottom_padding_drops_while_the_keyboard_is_up() {
        assert_eq!(bottom_padding(SAFE_BOTTOM, 0.), SAFE_BOTTOM);
        assert_eq!(bottom_padding(SAFE_BOTTOM, 300.), 0.);
        assert_eq!(bottom_padding(0., 0.), 0.);
    }

    #[test]
    fn the_surfaces_are_a_ladder_with_the_page_lowest() {
        let lum = |c: Rgba| c.r + c.g + c.b;
        assert!(lum(page()) < lum(surface()));
        assert!(lum(surface()) < lum(raised()));
        // 6% white for a press, 5.5% for a row's rule: the rule is the quieter of the two.
        assert!(rule().a < pressed().a);
    }
}
