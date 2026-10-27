//! The one palette, type scale, radii and shadows (`docs/design/01-system.md`). Components take
//! every value from here and nowhere else. Dark only until the light values have been checked.

use gpui::{hsla, point, px, rgb, rgba, BoxShadow, FontWeight, Hsla, Rgba, Styled};

// Surfaces. The rail is not a third stripe: it takes the sidebar's value.
pub fn rail() -> Rgba { rgb(0x121315) }
pub fn sidebar() -> Rgba { rgb(0x121315) }
pub fn canvas() -> Rgba { rgb(0x18191c) }
/// Cards, code, banners, image placeholders, the neutral avatar plate. Opaque.
pub fn raised() -> Rgba { rgb(0x1f2023) }
/// Anything that floats: menu, popover, modal, toast.
pub fn overlay() -> Rgba { rgb(0x25262a) }
pub fn tooltip() -> Rgba { rgb(0x2c2d32) }
pub fn scrim() -> Rgba { rgba(0x00000080) }

// Lines.
/// Pane and section dividers.
pub fn hairline() -> Rgba { rgba(0xffffff14) }
/// Control and overlay edges.
pub fn border() -> Rgba { rgba(0xffffff1f) }
/// Field hover, checkbox edge.
pub fn border_strong() -> Rgba { rgba(0xffffff33) }
/// The 1 pt edge of a floating surface.
pub fn overlay_edge() -> Rgba { rgba(0xffffff1a) }

// State fills. Alpha, so they sit on any surface; `over` gives the opaque result where a ring
// must match what is behind it.
pub fn hover() -> Rgba { rgba(0xffffff0f) }
pub fn selected() -> Rgba { rgba(0xffffff1f) }
pub fn pressed() -> Rgba { rgba(0xffffff29) }
/// A text field at rest.
pub fn field() -> Rgba { rgba(0xffffff0a) }
/// The lit segment of a segmented switch.
pub fn segment() -> Rgba { rgba(0xffffff24) }
/// Was the neutral count badge's fill; counts are unfilled now (`polish/01-system.md` §3).
#[allow(dead_code)]
pub fn count_fill() -> Rgba { rgba(0xffffff29) }
/// Removed from badges: a muted mention keeps the accent pill (C6).
#[allow(dead_code)]
pub fn quiet_fill() -> Rgba { rgba(0xffffff1f) }

// Text.
pub fn text() -> Rgba { rgb(0xedeef0) }
pub fn text_soft() -> Rgba { rgb(0xa8abb3) }
/// The lowest level that may carry meaning.
pub fn text_faint() -> Rgba { rgb(0x858993) }
pub fn text_disabled() -> Rgba { rgb(0x53565e) }

// Accent: "for you" or "this will act". Never plain unread.
pub fn accent() -> Rgba { rgb(0x2f6feb) }
pub fn accent_hover() -> Rgba { rgb(0x3a77ee) }
pub fn accent_pressed() -> Rgba { rgb(0x2861d3) }
/// Links, @mentions, the "New" label, the caret.
pub fn accent_text() -> Rgba { rgb(0x8ab4ff) }
pub fn focus() -> Rgba { accent_text() }
pub fn accent_soft() -> Rgba { rgba(0x2f6feb2e) }
/// A message that mentions me.
pub fn accent_wash() -> Rgba { rgba(0x2f6feb1a) }
/// The halo of a focused field.
pub fn accent_halo() -> Rgba { rgba(0x2f6feb59) }
/// Selected text in a field.
pub fn selection() -> Rgba { rgba(0x2f6feb73) }
pub fn on_accent() -> Rgba { rgb(0xffffff) }

// Semantic.
pub fn positive() -> Rgba { rgb(0x3fbf7a) }
pub fn positive_soft() -> Rgba { rgba(0x3fbf7a26) }
pub fn warning() -> Rgba { rgb(0xe0a63a) }
pub fn warning_soft() -> Rgba { rgba(0xe0a63a24) }
pub fn danger() -> Rgba { rgb(0xf0655d) }
pub fn danger_fill() -> Rgba { rgb(0xd13f3f) }
pub fn danger_soft() -> Rgba { rgba(0xf0655d26) }

// Aliases, so they can diverge later.
pub fn online() -> Rgba { positive() }
pub fn idle() -> Rgba { warning() }
pub fn dnd() -> Rgba { danger_fill() }
pub fn offline() -> Rgba { text_faint() }
pub fn mention() -> Rgba { accent() }
pub fn unread() -> Rgba { text() }

/// `fill` composited on an opaque `surface`: the color a ring needs to match a row's fill.
pub fn over(fill: Rgba, surface: Rgba) -> Rgba {
    let mix = |top: f32, bottom: f32| top * fill.a + bottom * (1. - fill.a);
    Rgba {
        r: mix(fill.r, surface.r),
        g: mix(fill.g, surface.g),
        b: mix(fill.b, surface.b),
        a: 1.,
    }
}

/// What is behind a row's content, for rings: the sidebar under the row's own fill.
pub fn row_surface(selected: bool) -> Rgba {
    if selected {
        over(self::selected(), sidebar())
    } else {
        sidebar()
    }
}

pub const FONT: &str = ".SystemUIFont";
pub const MONO: &str = "Menlo";

/// Weights: 400, 500 and 600 only.
pub const REGULAR: FontWeight = FontWeight::NORMAL;
pub const MEDIUM: FontWeight = FontWeight::MEDIUM;
pub const SEMIBOLD: FontWeight = FontWeight::SEMIBOLD;

// Radii. Nested: inner = outer minus the padding between them.
/// Chips, inline code, checkbox, segment.
pub const R_CHIP: f32 = 4.;
/// Every control and row.
pub const R_CONTROL: f32 = 6.;
/// Cards and media.
pub const R_CARD: f32 = 8.;
/// Floating containers and the composer.
pub const R_FLOAT: f32 = 10.;
pub const R_MODAL: f32 = 12.;

// Density.
/// Controls and one-line rows.
pub const CONTROL: f32 = 28.;
/// The app's mark where the window has nothing open, and on the first launch.
pub const BRAND_MARK: f32 = 56.;
/// Place rows: channels, servers, folders, menu items.
pub const ROW_ONE: f32 = 28.;
/// Person rows inside a pane: members, switcher, a menu's header. Avatar 24.
#[allow(dead_code)]
pub const ROW_MID: f32 = 36.;
/// Conversation rows: Inbox, chat lists, People. Avatar 32.
pub const ROW_TWO: f32 = 48.;
/// The title strip that starts every pane, level with the traffic lights.
pub const STRIP: f32 = 52.;
/// A list's section header; the first one in a list is `SECTION_FIRST`.
pub const SECTION: f32 = 28.;
pub const SECTION_FIRST: f32 = 24.;
/// A menu's own label, which stays smaller than a list's header.
pub const MENU_LABEL: f32 = 24.;
/// One-line banner under a strip.
pub const BANNER: f32 = 32.;
/// Every overlay keeps this far from each window edge.
pub const EDGE: f32 = 8.;

/// ⌘+ / ⌘−: −1 … +3 steps, 0 by default. Each step adds 1 to every type size and line height,
/// 2 to 28 and 36 rows and 4 to 48 rows; strips, icons, avatars, radii and spacing stay.
// One window, one value, read on every type call: a global is the cheapest honest home.
static TEXT_STEP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
pub const TEXT_STEPS: (i32, i32) = (-1, 3);

pub fn text_step() -> i32 {
    TEXT_STEP.load(std::sync::atomic::Ordering::Relaxed)
}

/// Sets the step, clamped to its range. Answers the step now in effect.
pub fn set_text_step(step: i32) -> i32 {
    let step = step.clamp(TEXT_STEPS.0, TEXT_STEPS.1);
    TEXT_STEP.store(step, std::sync::atomic::Ordering::Relaxed);
    step
}

/// Settings › Appearance › Density (A3): conversation rows 40 instead of 48.
static COMPACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_compact(compact: bool) {
    COMPACT.store(compact, std::sync::atomic::Ordering::Relaxed);
}

pub fn compact() -> bool {
    COMPACT.load(std::sync::atomic::Ordering::Relaxed)
}

/// A row height at the current text step and density: `ROW_ONE`, `ROW_MID` or `ROW_TWO` in,
/// points out.
pub fn row_height(base: f32) -> f32 {
    let per_step = if base >= ROW_TWO { 4. } else { 2. };
    let dense = if base >= ROW_TWO && compact() { 8. } else { 0. };
    base - dense + per_step * text_step() as f32
}

/// The avatar of a conversation row: 32, or 28 when compact.
pub fn row_avatar() -> f32 {
    if compact() {
        28.
    } else {
        32.
    }
}

fn sized(size: f32) -> gpui::Pixels {
    px(size + text_step() as f32)
}
/// Pane padding; lists are inset `INSET` from pane edges.
pub const PANE_PAD: f32 = 16.;
pub const INSET: f32 = 8.;

/// The six sizes, each with its line height. No fractional sizes.
pub trait Type: Styled + Sized {
    /// 20 / 26, 600: first run and sign-in headline.
    fn type_headline(self) -> Self {
        self.text_size(sized(20.)).line_height(sized(26.)).font_weight(SEMIBOLD)
    }
    /// 16 / 22, 600: pane, modal and empty-state titles.
    fn type_title(self) -> Self {
        self.text_size(sized(16.)).line_height(sized(22.)).font_weight(SEMIBOLD)
    }
    /// 14 / 20: message body and sender name, nothing else.
    fn type_body(self) -> Self {
        self.text_size(sized(14.)).line_height(sized(20.))
    }
    /// 13 / 18: the interface size.
    fn type_ui(self) -> Self {
        self.text_size(sized(13.)).line_height(sized(18.))
    }
    /// 12 / 16: previews, helper text, tooltips, shortcuts.
    fn type_small(self) -> Self {
        self.text_size(sized(12.)).line_height(sized(16.))
    }
    /// 11 / 14, 500: timestamps, section labels, badge numerals.
    fn type_micro(self) -> Self {
        self.text_size(sized(11.)).line_height(sized(14.)).font_weight(MEDIUM)
    }
    /// 13 / 20 monospace.
    fn type_code(self) -> Self {
        self.font_family(MONO).text_size(sized(13.)).line_height(sized(20.))
    }
}

impl<T: Styled + Sized> Type for T {}

fn shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: hsla(0., 0., 0., alpha),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
    }
}

pub fn shadow_tooltip() -> Vec<BoxShadow> {
    vec![shadow(2., 8., 0., 0.40)]
}

/// Popover, menu, toast.
pub fn shadow_popover() -> Vec<BoxShadow> {
    vec![shadow(0., 0., 1., 0.40), shadow(2., 6., 0., 0.30), shadow(12., 32., 0., 0.45)]
}

pub fn shadow_modal() -> Vec<BoxShadow> {
    vec![shadow(0., 0., 1., 0.40), shadow(4., 12., 0., 0.35), shadow(24., 64., 0., 0.55)]
}

/// The sidebar when it floats over the conversation in a narrow window.
pub fn shadow_pane() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: hsla(0., 0., 0., 0.6),
        offset: point(px(16.), px(0.)),
        blur_radius: px(48.),
        spread_radius: px(0.),
        inset: false,
    }]
}

fn ring(color: impl Into<Hsla>, spread: f32) -> BoxShadow {
    BoxShadow {
        color: color.into(),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(spread),
        inset: false,
    }
}

/// Keyboard focus on a control: 2 pt of `focus` outside the edge. GPUI has no outline, so it is a shadow with
/// no blur.
pub fn focus_ring() -> Vec<BoxShadow> {
    vec![ring(focus(), 2.)]
}

/// The ring on a filled control: a 2 pt gap of the surface first.
pub fn focus_ring_gap(surface: Rgba) -> Vec<BoxShadow> {
    vec![ring(surface, 2.), ring(focus(), 4.)]
}

/// Keyboard focus on a row or menu item: 2 pt inset, over any fill, because rows abut.
pub fn focus_ring_inset() -> Vec<BoxShadow> {
    vec![BoxShadow { inset: true, ..ring(focus(), 2.) }]
}

/// The 3 pt halo of a focused text field, which also takes a `focus` border.
pub fn field_halo() -> Vec<BoxShadow> {
    vec![ring(accent_halo(), 3.)]
}

/// The eight avatar tints. Flat, low chroma: fill and the letters on it.
pub fn tint(index: usize) -> (Hsla, Hsla) {
    const HUES: [f32; 8] = [8., 32., 95., 150., 185., 215., 265., 325.];
    let hue = HUES[index % HUES.len()] / 360.;
    (hsla(hue, 0.30, 0.22, 1.), hsla(hue, 0.70, 0.82, 1.))
}
