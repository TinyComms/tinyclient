//! The one palette, type scale, radii and shadows (`docs/design/01-system.md`). Components take
//! every value from here and nowhere else. Supports Light, Dark, System, and High Contrast modes.

use gpui::{hsla, point, px, rgb, rgba, BoxShadow, FontWeight, Hsla, Rgba, Styled, WindowAppearance};
use palette::IntoColor;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::phone::tokens as phone_tokens;

fn phone() -> bool {
    super::phone::active()
}

/// Settings › Appearance › Appearance (A1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Appearance {
    #[default]
    System = 0,
    Light = 1,
    Dark = 2,
}

impl Appearance {
    pub fn word(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "system" => Some(Appearance::System),
            "light" => Some(Appearance::Light),
            "dark" => Some(Appearance::Dark),
            _ => None,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Appearance::Light,
            2 => Appearance::Dark,
            _ => Appearance::System,
        }
    }
}

/// Resolved visual mode: light or dark, standard or high contrast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
    DarkHighContrast,
    LightHighContrast,
}

impl Mode {
    pub fn is_light(self) -> bool {
        matches!(self, Mode::Light | Mode::LightHighContrast)
    }

    pub fn is_high_contrast(self) -> bool {
        matches!(self, Mode::DarkHighContrast | Mode::LightHighContrast)
    }
}

static APPEARANCE: AtomicU8 = AtomicU8::new(Appearance::System as u8);
static USER_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);
/// True until the platform reports a light window appearance.
static SYSTEM_DARK: AtomicBool = AtomicBool::new(true);
static SYSTEM_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);
/// Optional `--theme` CLI override (0 = none, 1 = Dark, 2 = Light, 3 = DarkHC, 4 = LightHC).
static THEME_OVERRIDE: AtomicU8 = AtomicU8::new(0);

pub fn appearance() -> Appearance {
    Appearance::from_u8(APPEARANCE.load(Ordering::Relaxed))
}

pub fn set_appearance(choice: Appearance) {
    APPEARANCE.store(choice as u8, Ordering::Relaxed);
}

pub fn set_high_contrast(on: bool) {
    USER_HIGH_CONTRAST.store(on, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn user_high_contrast() -> bool {
    USER_HIGH_CONTRAST.load(Ordering::Relaxed)
}

pub fn system_high_contrast() -> bool {
    SYSTEM_HIGH_CONTRAST.load(Ordering::Relaxed)
}

#[allow(dead_code)]
pub fn set_system_high_contrast(on: bool) {
    SYSTEM_HIGH_CONTRAST.store(on, Ordering::Relaxed);
}

pub fn set_system_appearance(win: WindowAppearance) {
    let dark = matches!(win, WindowAppearance::Dark | WindowAppearance::VibrantDark);
    SYSTEM_DARK.store(dark, Ordering::Relaxed);
}

/// Applies `--theme <light|dark|light-hc|dark-hc|system>` for `--shot` and testing.
pub fn set_override(word: Option<&str>) -> bool {
    let code = match word {
        None | Some("system") => 0,
        Some("dark") => 1,
        Some("light") => 2,
        Some("dark-hc") | Some("dark-high-contrast") => 3,
        Some("light-hc") | Some("light-high-contrast") => 4,
        _ => return false,
    };
    THEME_OVERRIDE.store(code, Ordering::Relaxed);
    true
}

/// The resolved theme mode in effect for drawing.
pub fn mode() -> Mode {
    match THEME_OVERRIDE.load(Ordering::Relaxed) {
        1 => return Mode::Dark,
        2 => return Mode::Light,
        3 => return Mode::DarkHighContrast,
        4 => return Mode::LightHighContrast,
        _ => {}
    }
    let dark = match appearance() {
        Appearance::System => SYSTEM_DARK.load(Ordering::Relaxed),
        Appearance::Light => false,
        Appearance::Dark => true,
    };
    let hc = USER_HIGH_CONTRAST.load(Ordering::Relaxed) || SYSTEM_HIGH_CONTRAST.load(Ordering::Relaxed);
    match (dark, hc) {
        (true, false) => Mode::Dark,
        (false, false) => Mode::Light,
        (true, true) => Mode::DarkHighContrast,
        (false, true) => Mode::LightHighContrast,
    }
}

pub fn is_light() -> bool {
    mode().is_light()
}

pub fn is_high_contrast() -> bool {
    mode().is_high_contrast()
}

/// Reads the OS Increase Contrast / High Contrast preference once at launch, off the main thread.
pub fn read_system_contrast() {
    let _ = std::thread::Builder::new().name("system-contrast".into()).stack_size(64 * 1024).spawn(|| {
        let high = detect_system_high_contrast();
        SYSTEM_HIGH_CONTRAST.store(high, Ordering::Relaxed);
    });
}

#[cfg(target_os = "macos")]
fn detect_system_high_contrast() -> bool {
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.apple.universalaccess", "increaseContrast"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

#[cfg(windows)]
fn detect_system_high_contrast() -> bool {
    const SPI_GETHIGHCONTRAST: u32 = 0x0042;
    const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;
    #[repr(C)]
    struct HighContrastW {
        cb_size: u32,
        dw_flags: u32,
        lpsz_default_scheme: *mut u16,
    }
    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(ui_action: u32, ui_param: u32, pv_param: *mut std::ffi::c_void, f_win_ini: u32) -> i32;
    }
    let mut hc = HighContrastW {
        cb_size: std::mem::size_of::<HighContrastW>() as u32,
        dw_flags: 0,
        lpsz_default_scheme: std::ptr::null_mut(),
    };
    // SAFETY: valid initialized HighContrastW struct with cb_size set.
    let ok = unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cb_size, (&mut hc as *mut HighContrastW).cast(), 0) };
    ok != 0 && (hc.dw_flags & HCF_HIGHCONTRASTON) != 0
}

#[cfg(target_os = "linux")]
fn detect_system_high_contrast() -> bool {
    if std::env::var("GTK_THEME").ok().is_some_and(|t| t.to_ascii_lowercase().contains("highcontrast")) {
        return true;
    }
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.a11y.interface", "high-contrast"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "true")
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn detect_system_high_contrast() -> bool {
    false
}

/// Complete colour palette for one [`Mode`].
#[derive(Clone, Copy)]
struct Palette {
    rail: u32,
    sidebar: u32,
    canvas: u32,
    raised: u32,
    overlay: u32,
    tooltip: u32,
    scrim: u32,
    hairline: u32,
    border: u32,
    border_strong: u32,
    overlay_edge: u32,
    hover: u32,
    selected: u32,
    pressed: u32,
    field: u32,
    segment: u32,
    count_fill: u32,
    quiet_fill: u32,
    text: u32,
    text_soft: u32,
    text_faint: u32,
    text_disabled: u32,
    scrollbar: u32,
    scrollbar_hover: u32,
    accent: u32,
    accent_hover: u32,
    accent_pressed: u32,
    accent_text: u32,
    accent_soft: u32,
    accent_wash: u32,
    accent_halo: u32,
    selection: u32,
    on_accent: u32,
    positive: u32,
    positive_soft: u32,
    warning: u32,
    warning_soft: u32,
    danger: u32,
    danger_fill: u32,
    danger_soft: u32,
    slider_knob: u32,
}

const DARK: Palette = Palette {
    rail: 0x121315,
    sidebar: 0x121315,
    canvas: 0x18191c,
    raised: 0x1f2023,
    overlay: 0x25262a,
    tooltip: 0x2c2d32,
    scrim: 0x00000080,
    hairline: 0xffffff14,
    border: 0xffffff1f,
    border_strong: 0xffffff33,
    overlay_edge: 0xffffff1a,
    hover: 0xffffff0f,
    selected: 0xffffff1f,
    pressed: 0xffffff29,
    field: 0xffffff0a,
    segment: 0xffffff24,
    count_fill: 0xffffff29,
    quiet_fill: 0xffffff1f,
    text: 0xedeef0,
    text_soft: 0xa8abb3,
    text_faint: 0x9095a0,
    text_disabled: 0x53565e,
    scrollbar: 0xffffff33,
    scrollbar_hover: 0xffffff5c,
    accent: 0x2f6feb,
    accent_hover: 0x3a77ee,
    accent_pressed: 0x2861d3,
    accent_text: 0x8ab4ff,
    accent_soft: 0x2f6feb2e,
    accent_wash: 0x2f6feb1a,
    accent_halo: 0x2f6feb59,
    selection: 0x2f6feb73,
    on_accent: 0xffffff,
    positive: 0x3fbf7a,
    positive_soft: 0x3fbf7a26,
    warning: 0xe0a63a,
    warning_soft: 0xe0a63a24,
    danger: 0xf0655d,
    danger_fill: 0xd13f3f,
    danger_soft: 0xf0655d26,
    slider_knob: 0xf4f4f6,
};

const LIGHT: Palette = Palette {
    rail: 0xf3f4f6,
    sidebar: 0xf3f4f6,
    canvas: 0xffffff,
    raised: 0xf4f5f7,
    overlay: 0xffffff,
    tooltip: 0xecedf0,
    scrim: 0x00000059,
    hairline: 0x00000014,
    border: 0x00000024,
    border_strong: 0x00000040,
    overlay_edge: 0x0000001f,
    hover: 0x0000000c,
    selected: 0x00000017,
    pressed: 0x00000021,
    field: 0x0000000a,
    segment: 0x0000001a,
    count_fill: 0x0000001f,
    quiet_fill: 0x00000017,
    text: 0x141518,
    text_soft: 0x454954,
    text_faint: 0x5b606d,
    text_disabled: 0x8c919e,
    scrollbar: 0x00000033,
    scrollbar_hover: 0x0000005c,
    accent: 0x1d5bd6,
    accent_hover: 0x184ec0,
    accent_pressed: 0x1441a3,
    accent_text: 0x1852c9,
    accent_soft: 0x1d5bd624,
    accent_wash: 0x1d5bd614,
    accent_halo: 0x1d5bd64d,
    selection: 0x1d5bd640,
    on_accent: 0xffffff,
    positive: 0x1a7f49,
    positive_soft: 0x1a7f491f,
    warning: 0x9a6200,
    warning_soft: 0x9a62001f,
    danger: 0xc92a2a,
    danger_fill: 0xc92a2a,
    danger_soft: 0xc92a2a1f,
    slider_knob: 0x2b2e36,
};

const DARK_HC: Palette = Palette {
    rail: 0x0a0a0c,
    sidebar: 0x0a0a0c,
    canvas: 0x0e0f12,
    raised: 0x17181c,
    overlay: 0x1c1d22,
    tooltip: 0x222328,
    scrim: 0x000000a6,
    hairline: 0xffffff40,
    border: 0xffffff66,
    border_strong: 0xffffff99,
    overlay_edge: 0xffffff66,
    hover: 0xffffff1a,
    selected: 0xffffff2e,
    pressed: 0xffffff3d,
    field: 0xffffff14,
    segment: 0xffffff38,
    count_fill: 0xffffff38,
    quiet_fill: 0xffffff2e,
    text: 0xffffff,
    text_soft: 0xd8dce3,
    text_faint: 0xb5bac5,
    text_disabled: 0x7c818d,
    scrollbar: 0xffffff66,
    scrollbar_hover: 0xffffff99,
    accent: 0x2463eb,
    accent_hover: 0x3b76f6,
    accent_pressed: 0x1d4ed8,
    accent_text: 0xa6c8ff,
    accent_soft: 0x4c8dff40,
    accent_wash: 0x4c8dff26,
    accent_halo: 0x8ab4ff80,
    selection: 0x4c8dff73,
    on_accent: 0xffffff,
    positive: 0x5ae096,
    positive_soft: 0x5ae09633,
    warning: 0xffc44d,
    warning_soft: 0xffc44d33,
    danger: 0xff8078,
    danger_fill: 0xd13f3f,
    danger_soft: 0xff807833,
    slider_knob: 0xffffff,
};

const LIGHT_HC: Palette = Palette {
    rail: 0xebedf0,
    sidebar: 0xebedf0,
    canvas: 0xffffff,
    raised: 0xf0f2f5,
    overlay: 0xffffff,
    tooltip: 0xe4e7ec,
    scrim: 0x00000080,
    hairline: 0x0000004d,
    border: 0x00000073,
    border_strong: 0x000000a6,
    overlay_edge: 0x00000073,
    hover: 0x00000014,
    selected: 0x00000024,
    pressed: 0x00000033,
    field: 0x00000012,
    segment: 0x00000029,
    count_fill: 0x0000002e,
    quiet_fill: 0x00000024,
    text: 0x000000,
    text_soft: 0x262930,
    text_faint: 0x3e424c,
    text_disabled: 0x686d78,
    scrollbar: 0x00000066,
    scrollbar_hover: 0x00000099,
    accent: 0x1243b0,
    accent_hover: 0x0e3691,
    accent_pressed: 0x0a2973,
    accent_text: 0x0f3b9e,
    accent_soft: 0x1243b02e,
    accent_wash: 0x1243b01c,
    accent_halo: 0x1243b066,
    selection: 0x1243b04d,
    on_accent: 0xffffff,
    positive: 0x0f6636,
    positive_soft: 0x0f663626,
    warning: 0x7a4a00,
    warning_soft: 0x7a4a0026,
    danger: 0xa61b1b,
    danger_fill: 0xa61b1b,
    danger_soft: 0xa61b1b26,
    slider_knob: 0x141518,
};

fn palette_for(m: Mode) -> &'static Palette {
    match m {
        Mode::Dark => &DARK,
        Mode::Light => &LIGHT,
        Mode::DarkHighContrast => &DARK_HC,
        Mode::LightHighContrast => &LIGHT_HC,
    }
}

fn pal() -> &'static Palette {
    palette_for(mode())
}

// Surfaces. The rail is not a third stripe: it takes the sidebar's value.
pub fn rail() -> Rgba { rgb(pal().rail) }
pub fn sidebar() -> Rgba { rgb(pal().sidebar) }
pub fn canvas() -> Rgba { rgb(pal().canvas) }
/// Cards, code, banners, image placeholders, the neutral avatar plate. Opaque.
pub fn raised() -> Rgba { rgb(pal().raised) }
/// Anything that floats: menu, popover, modal, toast.
pub fn overlay() -> Rgba { rgb(pal().overlay) }
pub fn tooltip() -> Rgba { rgb(pal().tooltip) }
pub fn scrim() -> Rgba { rgba(pal().scrim) }

// Lines.
/// Pane and section dividers.
pub fn hairline() -> Rgba { rgba(pal().hairline) }
/// Control and overlay edges.
pub fn border() -> Rgba { rgba(pal().border) }
/// Field hover, checkbox edge.
pub fn border_strong() -> Rgba { rgba(pal().border_strong) }
/// The 1 pt edge of a floating surface.
pub fn overlay_edge() -> Rgba { rgba(pal().overlay_edge) }

// State fills. Alpha, so they sit on any surface; `over` gives the opaque result where a ring
// must match what is behind it.
pub fn hover() -> Rgba { rgba(pal().hover) }
pub fn selected() -> Rgba { rgba(pal().selected) }
pub fn pressed() -> Rgba { rgba(pal().pressed) }
/// A text field at rest.
pub fn field() -> Rgba { rgba(pal().field) }
/// The lit segment of a segmented switch.
pub fn segment() -> Rgba { rgba(pal().segment) }
/// Was the neutral count badge's fill; counts are unfilled now (`polish/01-system.md` §3).
#[allow(dead_code)]
pub fn count_fill() -> Rgba { rgba(pal().count_fill) }
/// Removed from badges: a muted mention keeps the accent pill (C6).
#[allow(dead_code)]
pub fn quiet_fill() -> Rgba { rgba(pal().quiet_fill) }

// Text.
pub fn text() -> Rgba { rgb(pal().text) }
pub fn text_soft() -> Rgba { rgb(pal().text_soft) }
/// The lowest level that may carry meaning.
pub fn text_faint() -> Rgba { rgb(pal().text_faint) }
pub fn text_disabled() -> Rgba { rgb(pal().text_disabled) }

// Scrollbars stay visible while content overflows, without competing with text.
pub fn scrollbar() -> Rgba { rgba(pal().scrollbar) }
pub fn scrollbar_hover() -> Rgba { rgba(pal().scrollbar_hover) }

// Accent: "for you" or "this will act". Never plain unread.
pub fn accent() -> Rgba { rgb(pal().accent) }
pub fn accent_hover() -> Rgba { rgb(pal().accent_hover) }
pub fn accent_pressed() -> Rgba { rgb(pal().accent_pressed) }
/// Links, @mentions, the "New" label, the caret.
pub fn accent_text() -> Rgba { rgb(pal().accent_text) }
pub fn focus() -> Rgba { accent_text() }
pub fn accent_soft() -> Rgba { rgba(pal().accent_soft) }
/// A message that mentions me.
pub fn accent_wash() -> Rgba { rgba(pal().accent_wash) }
/// The halo of a focused field.
pub fn accent_halo() -> Rgba { rgba(pal().accent_halo) }
/// Selected text in a field.
pub fn selection() -> Rgba { rgba(pal().selection) }
pub fn on_accent() -> Rgba { rgb(pal().on_accent) }

// Light-on-dark media overlays (viewer chrome, dark duration chips, starred scrim controls).
pub fn on_media() -> Rgba { rgb(0xffffff) }
pub fn on_media_soft() -> Rgba { rgba(0xffffffb8) }
pub fn media_chip() -> Rgba { rgba(0x0000008c) }

/// Slider thumb fill, visible on both dark and light surfaces.
pub fn slider_knob() -> Rgba { rgb(pal().slider_knob) }

// Semantic.
pub fn positive() -> Rgba { rgb(pal().positive) }
pub fn positive_soft() -> Rgba { rgba(pal().positive_soft) }
pub fn warning() -> Rgba { rgb(pal().warning) }
pub fn warning_soft() -> Rgba { rgba(pal().warning_soft) }
pub fn danger() -> Rgba { rgb(pal().danger) }
pub fn danger_fill() -> Rgba { rgb(pal().danger_fill) }
pub fn danger_soft() -> Rgba { rgba(pal().danger_soft) }

// Aliases, so they can diverge later.
pub fn online() -> Rgba { positive() }
pub fn idle() -> Rgba { warning() }
pub fn dnd() -> Rgba { danger_fill() }
pub fn offline() -> Rgba { text_faint() }
pub fn mention() -> Rgba { accent() }
pub fn unread() -> Rgba { text() }

/// WCAG 2.1 relative luminance of an opaque sRGB colour.
pub fn relative_luminance(c: Rgba) -> f32 {
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.red) + 0.7152 * lin(c.green) + 0.0722 * lin(c.blue)
}

/// WCAG 2.1 contrast ratio between two opaque colours (1.0 to 21.0).
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Adjusts a network role colour (e.g. from Discord or Tiny) so it meets a WCAG contrast floor
/// against the active canvas (4.5:1 standard, 7.0:1 high contrast). Darkens pastel/bright roles
/// on a light canvas and lightens very dark roles on a dark canvas, preserving hue.
pub fn role_color(color: u32) -> Rgba {
    role_color_on(color, canvas(), if is_high_contrast() { 7.0 } else { 4.5 })
}

pub fn role_color_on(color: u32, surface: Rgba, min_contrast: f32) -> Rgba {
    let raw = rgb(color);
    if contrast_ratio(raw, surface) >= min_contrast {
        return raw;
    }
    let light_surface = relative_luminance(surface) > 0.5;
    let target = if light_surface { rgb(0x000000) } else { rgb(0xffffff) };
    // Binary search blend factor toward black (on light) or white (on dark) to preserve hue.
    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    for _ in 0..14 {
        let mid = (lo + hi) * 0.5;
        let cand = Rgba {
            color: palette::rgb::Rgb::new(
                raw.red * (1.0 - mid) + target.red * mid,
                raw.green * (1.0 - mid) + target.green * mid,
                raw.blue * (1.0 - mid) + target.blue * mid,
            ),
            alpha: 1.0,
        };
        if contrast_ratio(cand, surface) >= min_contrast {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Rgba {
        color: palette::rgb::Rgb::new(
            raw.red * (1.0 - hi) + target.red * hi,
            raw.green * (1.0 - hi) + target.green * hi,
            raw.blue * (1.0 - hi) + target.blue * hi,
        ),
        alpha: 1.0,
    }
}

/// `fill` composited on an opaque `surface`: the color a ring needs to match a row's fill.
pub fn over(fill: Rgba, surface: Rgba) -> Rgba {
    let mix = |top: f32, bottom: f32| top * fill.alpha + bottom * (1. - fill.alpha);
    Rgba {
        color: palette::rgb::Rgb::new(
            mix(fill.red, surface.red),
            mix(fill.green, surface.green),
            mix(fill.blue, surface.blue),
        ),
        alpha: 1.,
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

// The faces per platform. `.SystemUIFont` is the system face wherever GPUI maps it (macOS, iOS,
// Windows, and the desktop Linux builds that already use it). Android has no such alias, and
// fontique there does not resolve the generic `sans-serif`: the run fell through gpui_ce_mobile's
// script fallback list to Noto Sans CJK, whose Latin is Source Sans with a full-width-looking
// word gap. Roboto is the family gpui_ce_mobile names as the system font, present on every
// Android release.
#[cfg(not(target_os = "android"))]
pub const FONT: &str = ".SystemUIFont";
#[cfg(target_os = "android")]
pub const FONT: &str = "Roboto";

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub const MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas";
// Android: the generic `monospace` does not resolve either (see FONT); Droid Sans Mono is the
// mono face in /system/fonts.
#[cfg(target_os = "android")]
pub const MONO: &str = "Droid Sans Mono";
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "windows", target_os = "android")))]
pub const MONO: &str = "monospace";

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
    if phone() {
        // A thumb's row: every row at least 44, a two-line row 56, which hold the phone's
        // baseline type (`phone::tokens::BASELINE`); both grow with the platform's text run so
        // nothing in them clips.
        let (floor, lines) = if base >= ROW_TWO { (56., 34.) } else { (44., 18.) };
        return floor + per_step * text_step() as f32 + (phone_tokens::type_factor() - phone_tokens::BASELINE) * lines;
    }
    let dense = if base >= ROW_TWO && compact() { 8. } else { 0. };
    base - dense + per_step * text_step() as f32
}

/// A line box for type of this line height, at the current step and, on a phone, its run: what
/// a row's line holds so its text never clips.
pub fn line(height: f32) -> gpui::Pixels {
    sized(height)
}

/// The avatar of a conversation row: 32, or 28 when compact.
pub fn row_avatar() -> f32 {
    if compact() {
        28.
    } else {
        32.
    }
}

pub fn sized(size: f32) -> gpui::Pixels {
    let base = size + text_step() as f32;
    if phone() {
        px((base * phone_tokens::type_factor() * 2.).round() / 2.)
    } else {
        px(base)
    }
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
        color: hsla(0., 0., 0., alpha).into(),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
    }
}

pub fn shadow_tooltip() -> Vec<BoxShadow> {
    let k = if is_light() { 0.4 } else { 1.0 };
    vec![shadow(2., 8., 0., 0.40 * k)]
}

/// Popover, menu, toast.
pub fn shadow_popover() -> Vec<BoxShadow> {
    let k = if is_light() { 0.35 } else { 1.0 };
    vec![shadow(0., 0., 1., 0.40 * k), shadow(2., 6., 0., 0.30 * k), shadow(12., 32., 0., 0.45 * k)]
}

pub fn shadow_modal() -> Vec<BoxShadow> {
    let k = if is_light() { 0.35 } else { 1.0 };
    vec![shadow(0., 0., 1., 0.40 * k), shadow(4., 12., 0., 0.35 * k), shadow(24., 64., 0., 0.55 * k)]
}

/// The sidebar when it floats over the conversation in a narrow window.
pub fn shadow_pane() -> Vec<BoxShadow> {
    let alpha = if is_light() { 0.22 } else { 0.6 };
    vec![BoxShadow {
        color: hsla(0., 0., 0., alpha).into(),
        offset: point(px(16.), px(0.)),
        blur_radius: px(48.),
        spread_radius: px(0.),
        inset: false,
    }]
}

fn ring(color: impl IntoColor<Hsla>, spread: f32) -> BoxShadow {
    BoxShadow {
        color: color.into_color().into(),
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
    if is_light() {
        (hsla(hue, 0.45, 0.88, 1.), hsla(hue, 0.65, 0.28, 1.))
    } else {
        (hsla(hue, 0.30, 0.22, 1.), hsla(hue, 0.70, 0.82, 1.))
    }
}

/// A color the network packs itself (a Telegram topic's `icon_color`): its hue drawn as the
/// tile's fill, with foreground contrast guaranteed to meet WCAG AA (>= 4.5:1).
pub fn tint_pack(color: u32) -> (Hsla, Hsla) {
    use palette::IntoColor;
    let (r, g, b) = ((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff);
    let (top, low) = (
        [r, g, b].iter().max().copied().unwrap(),
        [r, g, b].iter().min().copied().unwrap(),
    );
    let range = (top - low).max(1) as f32 / 255.;
    let seat = match top {
        top if top == r => (g as f32 - b as f32) / range / 255. / 6.,
        top if top == g => (2. + (b as f32 - r as f32) / range / 255.) / 6.,
        _ => (4. + (r as f32 - g as f32) / range / 255.) / 6.,
    };
    let hue = (seat + 1.).fract();
    let sat = 0.56;
    let base_l = 0.55;
    let white = rgb(0xffffff);
    let dark = rgb(0x141414);
    let fill_rgb: Rgba = hsla(hue, sat, base_l, 1.).into_color();
    if contrast_ratio(white, fill_rgb) >= 4.5 {
        return (hsla(hue, sat, base_l, 1.), hsla(0., 0., 1., 1.));
    }
    if contrast_ratio(dark, fill_rgb) >= 4.5 {
        return (hsla(hue, sat, base_l, 1.), hsla(0., 0., 0.08, 1.));
    }
    // Mid-luminance transition zone: darken the tile slightly so white ink reaches >= 4.5:1.
    let (mut lo, mut hi) = (0.20_f32, base_l);
    for _ in 0..16 {
        let mid = (lo + hi) * 0.5;
        let mid_rgb: Rgba = hsla(hue, sat, mid, 1.).into_color();
        if contrast_ratio(white, mid_rgb) >= 4.5 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (hsla(hue, sat, lo, 1.), hsla(0., 0., 1., 1.))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_meets_wcag_aa_and_high_contrast_meets_aaa() {
        for (name, m, min_text, min_faint) in [
            ("Dark", Mode::Dark, 4.5_f32, 4.5_f32),
            ("Light", Mode::Light, 4.5_f32, 4.5_f32),
            ("DarkHC", Mode::DarkHighContrast, 7.0_f32, 7.0_f32),
            ("LightHC", Mode::LightHighContrast, 7.0_f32, 7.0_f32),
        ] {
            let p = palette_for(m);
            let canvas = rgb(p.canvas);
            let raised = rgb(p.raised);
            let sidebar = rgb(p.sidebar);
            let overlay = rgb(p.overlay);
            let tooltip = rgb(p.tooltip);
            for (bg_name, bg) in [("canvas", canvas), ("raised", raised), ("sidebar", sidebar), ("overlay", overlay), ("tooltip", tooltip)] {
                let c_text = contrast_ratio(rgb(p.text), bg);
                let c_soft = contrast_ratio(rgb(p.text_soft), bg);
                let c_faint = contrast_ratio(rgb(p.text_faint), bg);
                let c_accent_text = contrast_ratio(rgb(p.accent_text), bg);
                assert!(c_text >= min_text, "{name} text on {bg_name}: {c_text:.2} < {min_text}");
                assert!(c_soft >= min_text, "{name} text_soft on {bg_name}: {c_soft:.2} < {min_text}");
                assert!(c_faint >= min_faint, "{name} text_faint on {bg_name}: {c_faint:.2} < {min_faint}");
                assert!(c_accent_text >= 4.5, "{name} accent_text on {bg_name}: {c_accent_text:.2} < 4.5");
            }
            let c_on_accent = contrast_ratio(rgb(p.on_accent), rgb(p.accent));
            assert!(c_on_accent >= 4.5, "{name} on_accent on accent: {c_on_accent:.2} < 4.5");
        }
    }

    #[test]
    fn tint_pack_meets_wcag_aa_across_telegram_topics_and_hues() {
        use palette::IntoColor;
        for color in [0x6fb9f0, 0xffd67e, 0xcb86db, 0x8eee98, 0xff93b2, 0xfb6f5f, 0x00ffff, 0xffff00, 0x2f6feb] {
            let (bg, fg) = tint_pack(color);
            let bg_rgb: Rgba = bg.into_color();
            let fg_rgb: Rgba = fg.into_color();
            let ratio = contrast_ratio(fg_rgb, bg_rgb);
            assert!(ratio >= 4.5, "tint_pack({color:#06x}) contrast {ratio:.2} < 4.5");
        }
    }

    #[test]
    fn role_colors_enforce_contrast_floor_on_light_and_dark_surfaces() {
        let light_bg = rgb(LIGHT.canvas);
        let dark_bg = rgb(DARK.canvas);
        // Bright yellow (common Discord role) has terrible contrast on white without adjustment.
        assert!(contrast_ratio(rgb(0xffe066), light_bg) < 2.0);
        let adjusted_light = role_color_on(0xffe066, light_bg, 4.5);
        assert!(contrast_ratio(adjusted_light, light_bg) >= 4.5);
        // Very dark navy has terrible contrast on dark canvas without adjustment.
        assert!(contrast_ratio(rgb(0x1a1a3a), dark_bg) < 2.0);
        let adjusted_dark = role_color_on(0x1a1a3a, dark_bg, 4.5);
        assert!(contrast_ratio(adjusted_dark, dark_bg) >= 4.5);
    }

    #[test]
    fn appearance_word_round_trips_and_override_parses() {
        for a in [Appearance::System, Appearance::Light, Appearance::Dark] {
            assert_eq!(Appearance::from_word(a.word()), Some(a));
        }
        assert_eq!(Appearance::from_word("sepia"), None);
        assert!(set_override(Some("light")));
        assert_eq!(mode(), Mode::Light);
        assert!(set_override(Some("light-hc")));
        assert_eq!(mode(), Mode::LightHighContrast);
        assert!(set_override(Some("dark-hc")));
        assert_eq!(mode(), Mode::DarkHighContrast);
        assert!(set_override(Some("dark")));
        assert_eq!(mode(), Mode::Dark);
        assert!(!set_override(Some("invalid")));
        assert!(set_override(None));
    }

    #[test]
    fn media_overlay_and_slider_knob_tokens_hold_contrast_in_light_and_dark() {
        let dark_scrim = rgb(0x000000);
        assert!(contrast_ratio(on_media(), dark_scrim) >= 15.0);
        let soft_on_dark = over(on_media_soft(), dark_scrim);
        assert!(contrast_ratio(soft_on_dark, dark_scrim) >= 7.0);
        let chip_on_white = over(media_chip(), rgb(0xffffff));
        assert!(contrast_ratio(on_media(), chip_on_white) >= 4.5);

        for (name, m) in [
            ("Dark", Mode::Dark),
            ("Light", Mode::Light),
            ("DarkHC", Mode::DarkHighContrast),
            ("LightHC", Mode::LightHighContrast),
        ] {
            let p = palette_for(m);
            let knob = rgb(p.slider_knob);
            for (bg_name, bg) in [
                ("canvas", rgb(p.canvas)),
                ("raised", rgb(p.raised)),
                ("overlay", rgb(p.overlay)),
            ] {
                let ratio = contrast_ratio(knob, bg);
                assert!(ratio >= 4.5, "{name} slider_knob on {bg_name}: {ratio:.2} < 4.5");
            }
        }
    }
}
