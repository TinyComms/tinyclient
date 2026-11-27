//! The one palette, type scale, radii and shadows (`docs/design/01-system.md`). Components take
//! every value from here and nowhere else. Supports 8 curated themes (6 Dark + 2 Light), 8 curated
//! accents + Auto, System day/night pairing, and per-theme WCAG AAA High Contrast.

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

/// Curated built-in themes (Settings › Appearance › Theme, AT).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ThemeId {
    #[default]
    Graphite = 0, // Dark (default dark)
    Paper = 1,    // Light (default light)
    Midnight = 2, // Dark (OLED)
    Nord = 3,     // Dark (Arctic Slate)
    Mocha = 4,    // Dark (Catppuccin Mocha)
    Stone = 5,    // Light (Warm Stone / Sepia)
    Dusk = 6,     // Dark (Rosé Pine Dusk)
    Forest = 7,   // Dark (Deep Pine Slate)
}

impl ThemeId {
    pub const ALL: [ThemeId; 8] = [
        ThemeId::Graphite,
        ThemeId::Paper,
        ThemeId::Midnight,
        ThemeId::Nord,
        ThemeId::Mocha,
        ThemeId::Stone,
        ThemeId::Dusk,
        ThemeId::Forest,
    ];

    /// Visual picker order: Dark defaults, Dark curated, then Light.
    pub const PICKER_ORDER: [ThemeId; 8] = [
        ThemeId::Graphite,
        ThemeId::Midnight,
        ThemeId::Nord,
        ThemeId::Mocha,
        ThemeId::Dusk,
        ThemeId::Forest,
        ThemeId::Paper,
        ThemeId::Stone,
    ];

    pub const fn is_light(self) -> bool {
        matches!(self, ThemeId::Paper | ThemeId::Stone)
    }

    pub const fn is_dark(self) -> bool {
        !self.is_light()
    }

    pub const fn word(self) -> &'static str {
        match self {
            ThemeId::Graphite => "graphite",
            ThemeId::Paper => "paper",
            ThemeId::Midnight => "midnight",
            ThemeId::Nord => "nord",
            ThemeId::Mocha => "mocha",
            ThemeId::Stone => "stone",
            ThemeId::Dusk => "dusk",
            ThemeId::Forest => "forest",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ThemeId::Graphite => "Graphite",
            ThemeId::Paper => "Paper",
            ThemeId::Midnight => "Midnight",
            ThemeId::Nord => "Nord",
            ThemeId::Mocha => "Mocha",
            ThemeId::Stone => "Warm Stone",
            ThemeId::Dusk => "Dusk",
            ThemeId::Forest => "Forest",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "graphite" => Some(ThemeId::Graphite),
            "paper" => Some(ThemeId::Paper),
            "midnight" => Some(ThemeId::Midnight),
            "nord" => Some(ThemeId::Nord),
            "mocha" => Some(ThemeId::Mocha),
            "stone" => Some(ThemeId::Stone),
            "dusk" => Some(ThemeId::Dusk),
            "forest" => Some(ThemeId::Forest),
            _ => None,
        }
    }

    pub fn from_dark_word(word: &str) -> Option<Self> {
        Self::from_word(word).filter(|id| id.is_dark())
    }

    pub fn from_light_word(word: &str) -> Option<Self> {
        Self::from_word(word).filter(|id| id.is_light())
    }

    pub const fn default_accent(self) -> Accent {
        match self {
            ThemeId::Graphite | ThemeId::Paper | ThemeId::Midnight => Accent::Blue,
            ThemeId::Nord => Accent::Teal,
            ThemeId::Mocha => Accent::Violet,
            ThemeId::Stone => Accent::Amber,
            ThemeId::Dusk => Accent::Rose,
            ThemeId::Forest => Accent::Emerald,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => ThemeId::Paper,
            2 => ThemeId::Midnight,
            3 => ThemeId::Nord,
            4 => ThemeId::Mocha,
            5 => ThemeId::Stone,
            6 => ThemeId::Dusk,
            7 => ThemeId::Forest,
            _ => ThemeId::Graphite,
        }
    }
}

/// Curated accent colour (Settings › Appearance › Accent color, AC).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Accent {
    #[default]
    Auto = 0, // Follows the active theme's curated signature accent
    Blue = 1,
    Indigo = 2,
    Violet = 3,
    Emerald = 4,
    Amber = 5,
    Rose = 6,
    Teal = 7,
    Graphite = 8,
}

impl Accent {
    pub const ALL: [Accent; 9] = [
        Accent::Auto,
        Accent::Blue,
        Accent::Indigo,
        Accent::Violet,
        Accent::Emerald,
        Accent::Amber,
        Accent::Rose,
        Accent::Teal,
        Accent::Graphite,
    ];

    #[allow(dead_code)]
    pub const CURATED: [Accent; 8] = [
        Accent::Blue,
        Accent::Indigo,
        Accent::Violet,
        Accent::Emerald,
        Accent::Amber,
        Accent::Rose,
        Accent::Teal,
        Accent::Graphite,
    ];

    pub const fn word(self) -> &'static str {
        match self {
            Accent::Auto => "auto",
            Accent::Blue => "blue",
            Accent::Indigo => "indigo",
            Accent::Violet => "violet",
            Accent::Emerald => "emerald",
            Accent::Amber => "amber",
            Accent::Rose => "rose",
            Accent::Teal => "teal",
            Accent::Graphite => "graphite",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Accent::Auto => "Auto",
            Accent::Blue => "Blue",
            Accent::Indigo => "Indigo",
            Accent::Violet => "Violet",
            Accent::Emerald => "Emerald",
            Accent::Amber => "Amber",
            Accent::Rose => "Rose",
            Accent::Teal => "Teal",
            Accent::Graphite => "Graphite",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "auto" => Some(Accent::Auto),
            "blue" => Some(Accent::Blue),
            "indigo" => Some(Accent::Indigo),
            "violet" => Some(Accent::Violet),
            "emerald" => Some(Accent::Emerald),
            "amber" => Some(Accent::Amber),
            "rose" => Some(Accent::Rose),
            "teal" => Some(Accent::Teal),
            "graphite" => Some(Accent::Graphite),
            _ => None,
        }
    }

    /// Swatch dot fill for the Settings › Appearance › Accent color row.
    pub fn swatch(self) -> Rgba {
        let resolved = match self {
            Accent::Auto => active_theme().default_accent(),
            other => other,
        };
        rgb(match resolved {
            Accent::Auto | Accent::Blue => 0x2f6feb,
            Accent::Indigo => 0x5d5fe6,
            Accent::Violet => 0x7e52f0,
            Accent::Emerald => 0x1f8255,
            Accent::Amber => 0xd48d17,
            Accent::Rose => 0xcf3d65,
            Accent::Teal => 0x16808f,
            Accent::Graphite => 0x767d8e,
        })
    }

    /// Check/indicator ink painted inside this swatch dot.
    pub fn swatch_ink(self) -> Rgba {
        let fill = self.swatch();
        let white = rgb(0xffffff);
        let dark = rgb(0x121315);
        if contrast_ratio(white, fill) >= contrast_ratio(dark, fill) {
            white
        } else {
            dark
        }
    }

    fn curated_index(self) -> usize {
        match self {
            Accent::Auto | Accent::Blue => 0,
            Accent::Indigo => 1,
            Accent::Violet => 2,
            Accent::Emerald => 3,
            Accent::Amber => 4,
            Accent::Rose => 5,
            Accent::Teal => 6,
            Accent::Graphite => 7,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Accent::Blue,
            2 => Accent::Indigo,
            3 => Accent::Violet,
            4 => Accent::Emerald,
            5 => Accent::Amber,
            6 => Accent::Rose,
            7 => Accent::Teal,
            8 => Accent::Graphite,
            _ => Accent::Auto,
        }
    }
}

/// Resolved visual polarity + contrast mode.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
    DarkHighContrast,
    LightHighContrast,
}

#[allow(dead_code)]
impl Mode {
    pub fn is_light(self) -> bool {
        matches!(self, Mode::Light | Mode::LightHighContrast)
    }

    pub fn is_high_contrast(self) -> bool {
        matches!(self, Mode::DarkHighContrast | Mode::LightHighContrast)
    }
}

static APPEARANCE: AtomicU8 = AtomicU8::new(Appearance::System as u8);
static DARK_THEME: AtomicU8 = AtomicU8::new(ThemeId::Graphite as u8);
static LIGHT_THEME: AtomicU8 = AtomicU8::new(ThemeId::Paper as u8);
static ACCENT: AtomicU8 = AtomicU8::new(Accent::Auto as u8);
static USER_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);
/// True until the platform reports a light window appearance.
static SYSTEM_DARK: AtomicBool = AtomicBool::new(true);
static SYSTEM_HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);
/// Optional `--theme` CLI override: 0 = none, else `1 + (id as u8) * 2 + u8::from(hc)`.
static THEME_OVERRIDE: AtomicU8 = AtomicU8::new(0);
/// Optional `--accent` CLI override: 0 = none, else `accent as u8 + 1`.
static ACCENT_OVERRIDE: AtomicU8 = AtomicU8::new(0);

#[allow(dead_code)]
pub fn appearance() -> Appearance {
    Appearance::from_u8(APPEARANCE.load(Ordering::Relaxed))
}

pub fn set_appearance(choice: Appearance) {
    APPEARANCE.store(choice as u8, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn dark_theme() -> ThemeId {
    ThemeId::from_u8(DARK_THEME.load(Ordering::Relaxed))
}

pub fn set_dark_theme(id: ThemeId) {
    if id.is_dark() {
        DARK_THEME.store(id as u8, Ordering::Relaxed);
    }
}

#[allow(dead_code)]
pub fn light_theme() -> ThemeId {
    ThemeId::from_u8(LIGHT_THEME.load(Ordering::Relaxed))
}

pub fn set_light_theme(id: ThemeId) {
    if id.is_light() {
        LIGHT_THEME.store(id as u8, Ordering::Relaxed);
    }
}

pub fn accent_choice() -> Accent {
    Accent::from_u8(ACCENT.load(Ordering::Relaxed))
}

pub fn set_accent(accent: Accent) {
    ACCENT.store(accent as u8, Ordering::Relaxed);
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

pub fn system_is_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}

pub fn set_system_appearance(win: WindowAppearance) {
    let dark = matches!(win, WindowAppearance::Dark | WindowAppearance::VibrantDark);
    SYSTEM_DARK.store(dark, Ordering::Relaxed);
}

fn encode_override(id: ThemeId, hc: bool) -> u8 {
    1 + (id as u8) * 2 + u8::from(hc)
}

fn decode_override(code: u8) -> Option<(ThemeId, bool)> {
    if code == 0 {
        return None;
    }
    let idx = code - 1;
    let id = ThemeId::from_u8(idx / 2);
    let hc = (idx % 2) == 1;
    Some((id, hc))
}

/// Applies `--theme <id|dark|light|system>[-hc]` for `--shot` and testing.
pub fn set_override(word: Option<&str>) -> bool {
    let code = match word {
        None | Some("system") => 0,
        Some("dark") => encode_override(ThemeId::Graphite, false),
        Some("light") => encode_override(ThemeId::Paper, false),
        Some("dark-hc") | Some("dark-high-contrast") => encode_override(ThemeId::Graphite, true),
        Some("light-hc") | Some("light-high-contrast") => encode_override(ThemeId::Paper, true),
        Some(raw) => {
            if let Some(stem) = raw.strip_suffix("-high-contrast").or_else(|| raw.strip_suffix("-hc")) {
                let Some(id) = ThemeId::from_word(stem) else {
                    return false;
                };
                encode_override(id, true)
            } else {
                let Some(id) = ThemeId::from_word(raw) else {
                    return false;
                };
                encode_override(id, false)
            }
        }
    };
    THEME_OVERRIDE.store(code, Ordering::Relaxed);
    true
}

/// Applies `--accent <auto|blue|indigo|violet|emerald|amber|rose|teal|graphite>` for `--shot` and testing.
pub fn set_accent_override(word: Option<&str>) -> bool {
    let code = match word {
        None => 0,
        Some(raw) => match Accent::from_word(raw) {
            Some(accent) => accent as u8 + 1,
            None => return false,
        },
    };
    ACCENT_OVERRIDE.store(code, Ordering::Relaxed);
    true
}

/// The resolved [`ThemeId`] in effect for drawing.
pub fn active_theme() -> ThemeId {
    if let Some((id, _)) = decode_override(THEME_OVERRIDE.load(Ordering::Relaxed)) {
        return id;
    }
    let dark = match appearance() {
        Appearance::System => SYSTEM_DARK.load(Ordering::Relaxed),
        Appearance::Light => false,
        Appearance::Dark => true,
    };
    if dark {
        dark_theme()
    } else {
        light_theme()
    }
}

/// The resolved [`Accent`] in effect for drawing (never [`Accent::Auto`]).
pub fn active_accent() -> Accent {
    let choice = match ACCENT_OVERRIDE.load(Ordering::Relaxed) {
        0 => accent_choice(),
        code => Accent::from_u8(code - 1),
    };
    match choice {
        Accent::Auto => active_theme().default_accent(),
        other => other,
    }
}

/// The resolved visual polarity and contrast mode in effect for drawing.
#[allow(dead_code)]
pub fn mode() -> Mode {
    let light = active_theme().is_light();
    let hc = is_high_contrast();
    match (light, hc) {
        (false, false) => Mode::Dark,
        (true, false) => Mode::Light,
        (false, true) => Mode::DarkHighContrast,
        (true, true) => Mode::LightHighContrast,
    }
}

pub fn is_light() -> bool {
    active_theme().is_light()
}

pub fn is_high_contrast() -> bool {
    if let Some((_, hc)) = decode_override(THEME_OVERRIDE.load(Ordering::Relaxed)) {
        return hc;
    }
    USER_HIGH_CONTRAST.load(Ordering::Relaxed) || SYSTEM_HIGH_CONTRAST.load(Ordering::Relaxed)
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

/// Layer 1: Surface, neutral, control, and semantic base tokens for one curated theme.
#[derive(Clone, Copy, Debug)]
pub struct ThemeBase {
    pub rail: u32,
    pub sidebar: u32,
    pub canvas: u32,
    pub raised: u32,
    pub overlay: u32,
    pub tooltip: u32,
    pub scrim: u32,
    pub hairline: u32,
    pub border: u32,
    pub border_strong: u32,
    pub overlay_edge: u32,
    pub hover: u32,
    pub selected: u32,
    pub pressed: u32,
    pub field: u32,
    pub segment: u32,
    pub count_fill: u32,
    pub quiet_fill: u32,
    pub switch_off: u32,
    pub switch_knob: u32,
    pub code_inline: u32,
    pub float_hover: u32,
    pub spoiler: u32,
    pub text: u32,
    pub text_soft: u32,
    pub text_faint: u32,
    pub text_disabled: u32,
    pub scrollbar: u32,
    pub scrollbar_hover: u32,
    pub positive: u32,
    pub warning: u32,
    pub danger: u32,
    pub danger_fill: u32,
    pub slider_knob: u32,
}

/// Layer 2: Accent ramp for one `(Accent, polarity, high_contrast)` combination.
#[derive(Clone, Copy, Debug)]
pub struct AccentRamp {
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_pressed: u32,
    pub accent_text: u32,
    pub on_accent: u32,
}

/// Layer 3: Code syntax & diff highlight tokens verified against every theme's `raised`.
#[derive(Clone, Copy, Debug)]
pub struct SyntaxPalette {
    pub keyword: u32,
    pub string: u32,
    pub comment: u32,
    pub number: u32,
    pub type_: u32,
    pub function: u32,
    pub added: u32,
    pub removed: u32,
}

/// Standard-contrast `ThemeBase` table indexed by `ThemeId as usize`.
const BASE_STD: [ThemeBase; 8] = [
    // 0: Graphite (Default Dark)
    ThemeBase {
        rail: 0x121315,
        sidebar: 0x121315,
        canvas: 0x18191c,
        raised: 0x1f2023,
        overlay: 0x25262a,
        tooltip: 0x2c2d32,
        scrim: 0x00000080,
        hairline: 0xffffff14,
        border: 0xffffff24,
        border_strong: 0xffffff59,
        overlay_edge: 0xffffff1f,
        hover: 0xffffff0f,
        selected: 0xffffff1f,
        pressed: 0xffffff29,
        field: 0xffffff0a,
        segment: 0xffffff24,
        count_fill: 0xffffff29,
        quiet_fill: 0xffffff1f,
        switch_off: 0xffffff59,
        switch_knob: 0xffffff,
        code_inline: 0xffffff14,
        float_hover: 0x303136,
        spoiler: 0x3a3b42,
        text: 0xedeef0,
        text_soft: 0xa8abb3,
        text_faint: 0x9095a0,
        text_disabled: 0x53565e,
        scrollbar: 0xffffff33,
        scrollbar_hover: 0xffffff5c,
        positive: 0x3fbf7a,
        warning: 0xe0a63a,
        danger: 0xf0655d,
        danger_fill: 0xd13f3f,
        slider_knob: 0xf4f4f6,
    },
    // 1: Paper (Default Light)
    ThemeBase {
        rail: 0xf3f4f6,
        sidebar: 0xf3f4f6,
        canvas: 0xffffff,
        raised: 0xf4f5f7,
        overlay: 0xffffff,
        tooltip: 0xecedf0,
        scrim: 0x00000059,
        hairline: 0x00000014,
        border: 0x00000024,
        border_strong: 0x00000073,
        overlay_edge: 0x0000001f,
        hover: 0x0000000c,
        selected: 0x00000017,
        pressed: 0x00000021,
        field: 0x0000000a,
        segment: 0x0000001a,
        count_fill: 0x0000001f,
        quiet_fill: 0x00000017,
        switch_off: 0x0000006e,
        switch_knob: 0xffffff,
        code_inline: 0x0000000f,
        float_hover: 0xe4e6eb,
        spoiler: 0xd0d4dc,
        text: 0x141518,
        text_soft: 0x454954,
        text_faint: 0x5b606d,
        text_disabled: 0x8c919e,
        scrollbar: 0x00000033,
        scrollbar_hover: 0x0000005c,
        positive: 0x1a7f49,
        warning: 0x9a6200,
        danger: 0xc92a2a,
        danger_fill: 0xc92a2a,
        slider_knob: 0x2b2e36,
    },
    // 2: Midnight (OLED)
    ThemeBase {
        rail: 0x000000,
        sidebar: 0x000000,
        canvas: 0x07080a,
        raised: 0x111216,
        overlay: 0x181a20,
        tooltip: 0x20232b,
        scrim: 0x000000a6,
        hairline: 0xffffff1a,
        border: 0xffffff29,
        border_strong: 0xffffff59,
        overlay_edge: 0xffffff24,
        hover: 0xffffff12,
        selected: 0xffffff21,
        pressed: 0xffffff2e,
        field: 0xffffff0d,
        segment: 0xffffff29,
        count_fill: 0xffffff2e,
        quiet_fill: 0xffffff21,
        switch_off: 0xffffff59,
        switch_knob: 0xffffff,
        code_inline: 0xffffff1a,
        float_hover: 0x262932,
        spoiler: 0x2d313c,
        text: 0xf4f5f7,
        text_soft: 0xa8adb8,
        text_faint: 0x8c92a0,
        text_disabled: 0x525763,
        scrollbar: 0xffffff38,
        scrollbar_hover: 0xffffff66,
        positive: 0x3ecf82,
        warning: 0xe5ab3e,
        danger: 0xf56b62,
        danger_fill: 0xd13f3f,
        slider_knob: 0xf4f5f7,
    },
    // 3: Nord (Arctic Slate)
    ThemeBase {
        rail: 0x21262f,
        sidebar: 0x21262f,
        canvas: 0x272d38,
        raised: 0x2f3643,
        overlay: 0x363e4d,
        tooltip: 0x3d4657,
        scrim: 0x0f121899,
        hairline: 0xe5e9f017,
        border: 0xe5e9f029,
        border_strong: 0xe5e9f069,
        overlay_edge: 0xe5e9f024,
        hover: 0xe5e9f010,
        selected: 0xe5e9f01f,
        pressed: 0xe5e9f02b,
        field: 0x191d2459,
        segment: 0xe5e9f026,
        count_fill: 0xe5e9f02b,
        quiet_fill: 0xe5e9f01f,
        switch_off: 0xe5e9f069,
        switch_knob: 0xf4f6f9,
        code_inline: 0xe5e9f01a,
        float_hover: 0x434d60,
        spoiler: 0x4a556a,
        text: 0xeceff4,
        text_soft: 0xc3ccd9,
        text_faint: 0xadb8c9,
        text_disabled: 0x6b778c,
        scrollbar: 0xe5e9f036,
        scrollbar_hover: 0xe5e9f061,
        positive: 0x8fc48a,
        warning: 0xe8c77b,
        danger: 0xeb848e,
        danger_fill: 0xbf4b57,
        slider_knob: 0xeceff4,
    },
    // 4: Catppuccin Mocha
    ThemeBase {
        rail: 0x181825,
        sidebar: 0x181825,
        canvas: 0x1e1e2e,
        raised: 0x262638,
        overlay: 0x2e2f44,
        tooltip: 0x36374f,
        scrim: 0x0d0d1699,
        hairline: 0xcdd6f417,
        border: 0xcdd6f426,
        border_strong: 0xcdd6f46b,
        overlay_edge: 0xcdd6f421,
        hover: 0xcdd6f410,
        selected: 0xcdd6f41f,
        pressed: 0xcdd6f42b,
        field: 0x11111b59,
        segment: 0xcdd6f426,
        count_fill: 0xcdd6f42b,
        quiet_fill: 0xcdd6f41f,
        switch_off: 0xcdd6f46b,
        switch_knob: 0xf5f7ff,
        code_inline: 0xcdd6f41a,
        float_hover: 0x3c3e58,
        spoiler: 0x454764,
        text: 0xcdd6f4,
        text_soft: 0xb4befe,
        text_faint: 0xa2aad0,
        text_disabled: 0x666a85,
        scrollbar: 0xcdd6f436,
        scrollbar_hover: 0xcdd6f461,
        positive: 0x7dd38d,
        warning: 0xf2cd85,
        danger: 0xf38ba8,
        danger_fill: 0xc93b5c,
        slider_knob: 0xe6ebff,
    },
    // 5: Warm Stone (Sepia Light)
    ThemeBase {
        rail: 0xeeeae1,
        sidebar: 0xeeeae1,
        canvas: 0xf8f5ef,
        raised: 0xefebe2,
        overlay: 0xfcfaf6,
        tooltip: 0xe7e2d7,
        scrim: 0x1d191459,
        hairline: 0x29231a17,
        border: 0x29231a29,
        border_strong: 0x29231a82,
        overlay_edge: 0x29231a24,
        hover: 0x29231a0d,
        selected: 0x29231a1a,
        pressed: 0x29231a24,
        field: 0x29231a0c,
        segment: 0x29231a1c,
        count_fill: 0x29231a21,
        quiet_fill: 0x29231a1a,
        switch_off: 0x29231a82,
        switch_knob: 0xffffff,
        code_inline: 0x29231a12,
        float_hover: 0xe3ddd2,
        spoiler: 0xd8d1c4,
        text: 0x24201b,
        text_soft: 0x4c453c,
        text_faint: 0x5d554a,
        text_disabled: 0x8f877b,
        scrollbar: 0x29231a38,
        scrollbar_hover: 0x29231a66,
        positive: 0x1e7548,
        warning: 0x8c5600,
        danger: 0xb8302c,
        danger_fill: 0xb8302c,
        slider_knob: 0x332d26,
    },
    // 6: Dusk (Rosé Pine)
    ThemeBase {
        rail: 0x161420,
        sidebar: 0x161420,
        canvas: 0x1c1a28,
        raised: 0x242134,
        overlay: 0x2c2840,
        tooltip: 0x342f4a,
        scrim: 0x0b0a1299,
        hairline: 0xe0def417,
        border: 0xe0def426,
        border_strong: 0xe0def466,
        overlay_edge: 0xe0def421,
        hover: 0xe0def410,
        selected: 0xe0def41f,
        pressed: 0xe0def42b,
        field: 0x110f1a59,
        segment: 0xe0def426,
        count_fill: 0xe0def42b,
        quiet_fill: 0xe0def41f,
        switch_off: 0xe0def466,
        switch_knob: 0xf5f4fc,
        code_inline: 0xe0def41a,
        float_hover: 0x3b3654,
        spoiler: 0x443e61,
        text: 0xe6e4f6,
        text_soft: 0xb7b2d1,
        text_faint: 0xa49ec2,
        text_disabled: 0x655f80,
        scrollbar: 0xe0def436,
        scrollbar_hover: 0xe0def461,
        positive: 0x6cc2b3,
        warning: 0xf2c179,
        danger: 0xf27d9a,
        danger_fill: 0xc43b5e,
        slider_knob: 0xeeeaf8,
    },
    // 7: Forest (Deep Pine Slate)
    ThemeBase {
        rail: 0x101613,
        sidebar: 0x101613,
        canvas: 0x151d19,
        raised: 0x1c2621,
        overlay: 0x232f29,
        tooltip: 0x2b3932,
        scrim: 0x070b0999,
        hairline: 0xe2efe717,
        border: 0xe2efe726,
        border_strong: 0xe2efe75e,
        overlay_edge: 0xe2efe721,
        hover: 0xe2efe710,
        selected: 0xe2efe71f,
        pressed: 0xe2efe72b,
        field: 0x0b100d59,
        segment: 0xe2efe726,
        count_fill: 0xe2efe72b,
        quiet_fill: 0xe2efe71f,
        switch_off: 0xe2efe75e,
        switch_knob: 0xf2f7f4,
        code_inline: 0xe2efe71a,
        float_hover: 0x2f3e36,
        spoiler: 0x374940,
        text: 0xe8f0eb,
        text_soft: 0xa7bab0,
        text_faint: 0x95aba0,
        text_disabled: 0x576b61,
        scrollbar: 0xe2efe736,
        scrollbar_hover: 0xe2efe761,
        positive: 0x58cf8b,
        warning: 0xe5b85c,
        danger: 0xf07872,
        danger_fill: 0xc73e3e,
        slider_knob: 0xeef5f0,
    },
];

/// High-Contrast (WCAG AAA) `ThemeBase` table indexed by `ThemeId as usize`.
const BASE_HC: [ThemeBase; 8] = [
    // 0: Graphite HC
    ThemeBase {
        rail: 0x0a0a0c,
        sidebar: 0x0a0a0c,
        canvas: 0x0e0f12,
        raised: 0x17181c,
        overlay: 0x1c1d22,
        tooltip: 0x222328,
        scrim: 0x000000b3,
        hairline: 0xffffff47,
        border: 0xffffff80,
        border_strong: 0xffffffb3,
        overlay_edge: 0xffffff80,
        hover: 0xffffff1c,
        selected: 0xffffff30,
        pressed: 0xffffff40,
        field: 0xffffff16,
        segment: 0xffffff3b,
        count_fill: 0xffffff3b,
        quiet_fill: 0xffffff30,
        switch_off: 0xffffff80,
        switch_knob: 0xffffff,
        code_inline: 0xffffff24,
        float_hover: 0x28292f,
        spoiler: 0x34363d,
        text: 0xffffff,
        text_soft: 0xd8dce3,
        text_faint: 0xb8bdc8,
        text_disabled: 0x7c818d,
        scrollbar: 0xffffff73,
        scrollbar_hover: 0xffffffa6,
        positive: 0x5ae096,
        warning: 0xffc44d,
        danger: 0xff8078,
        danger_fill: 0xc93232,
        slider_knob: 0xffffff,
    },
    // 1: Paper HC
    ThemeBase {
        rail: 0xebedf0,
        sidebar: 0xebedf0,
        canvas: 0xffffff,
        raised: 0xf0f2f5,
        overlay: 0xffffff,
        tooltip: 0xe4e7ec,
        scrim: 0x00000080,
        hairline: 0x0000004d,
        border: 0x0000008c,
        border_strong: 0x000000b8,
        overlay_edge: 0x0000008c,
        hover: 0x00000014,
        selected: 0x00000024,
        pressed: 0x00000033,
        field: 0x00000012,
        segment: 0x00000029,
        count_fill: 0x0000002e,
        quiet_fill: 0x00000024,
        switch_off: 0x0000008c,
        switch_knob: 0xffffff,
        code_inline: 0x00000018,
        float_hover: 0xdce0e7,
        spoiler: 0xc5cad4,
        text: 0x000000,
        text_soft: 0x262930,
        text_faint: 0x3e424c,
        text_disabled: 0x686d78,
        scrollbar: 0x00000066,
        scrollbar_hover: 0x00000099,
        positive: 0x0f6636,
        warning: 0x7a4a00,
        danger: 0xa61b1b,
        danger_fill: 0xa61b1b,
        slider_knob: 0x141518,
    },
    // 2: Midnight HC
    ThemeBase {
        rail: 0x000000,
        sidebar: 0x000000,
        canvas: 0x000000,
        raised: 0x0d0e12,
        overlay: 0x14161c,
        tooltip: 0x1b1e26,
        scrim: 0x000000b3,
        hairline: 0xffffff47,
        border: 0xffffff80,
        border_strong: 0xffffffb3,
        overlay_edge: 0xffffff80,
        hover: 0xffffff1c,
        selected: 0xffffff30,
        pressed: 0xffffff40,
        field: 0xffffff16,
        segment: 0xffffff3b,
        count_fill: 0xffffff3b,
        quiet_fill: 0xffffff30,
        switch_off: 0xffffff80,
        switch_knob: 0xffffff,
        code_inline: 0xffffff24,
        float_hover: 0x222630,
        spoiler: 0x2b303c,
        text: 0xffffff,
        text_soft: 0xdce0e8,
        text_faint: 0xb8beca,
        text_disabled: 0x7a808d,
        scrollbar: 0xffffff73,
        scrollbar_hover: 0xffffffa6,
        positive: 0x5ae096,
        warning: 0xffc44d,
        danger: 0xff8078,
        danger_fill: 0xc93232,
        slider_knob: 0xffffff,
    },
    // 3: Nord HC
    ThemeBase {
        rail: 0x14181f,
        sidebar: 0x14181f,
        canvas: 0x191e27,
        raised: 0x212733,
        overlay: 0x272e3c,
        tooltip: 0x2d3545,
        scrim: 0x000000b3,
        hairline: 0xffffff47,
        border: 0xffffff80,
        border_strong: 0xffffffb3,
        overlay_edge: 0xffffff80,
        hover: 0xffffff1c,
        selected: 0xffffff30,
        pressed: 0xffffff40,
        field: 0xffffff16,
        segment: 0xffffff3b,
        count_fill: 0xffffff3b,
        quiet_fill: 0xffffff30,
        switch_off: 0xffffff80,
        switch_knob: 0xffffff,
        code_inline: 0xffffff24,
        float_hover: 0x353e50,
        spoiler: 0x3f4a5f,
        text: 0xffffff,
        text_soft: 0xdbe2ec,
        text_faint: 0xc5d0df,
        text_disabled: 0x8290a6,
        scrollbar: 0xffffff73,
        scrollbar_hover: 0xffffffa6,
        positive: 0x9ee098,
        warning: 0xfad98c,
        danger: 0xff9ca6,
        danger_fill: 0xbf3d4b,
        slider_knob: 0xffffff,
    },
    // 4: Mocha HC
    ThemeBase {
        rail: 0x10101a,
        sidebar: 0x10101a,
        canvas: 0x141421,
        raised: 0x1c1c2c,
        overlay: 0x232438,
        tooltip: 0x292a40,
        scrim: 0x000000b3,
        hairline: 0xffffff47,
        border: 0xffffff80,
        border_strong: 0xffffffb3,
        overlay_edge: 0xffffff80,
        hover: 0xffffff1c,
        selected: 0xffffff30,
        pressed: 0xffffff40,
        field: 0xffffff16,
        segment: 0xffffff3b,
        count_fill: 0xffffff3b,
        quiet_fill: 0xffffff30,
        switch_off: 0xffffff80,
        switch_knob: 0xffffff,
        code_inline: 0xffffff24,
        float_hover: 0x32334d,
        spoiler: 0x3c3d5b,
        text: 0xffffff,
        text_soft: 0xdbe2ff,
        text_faint: 0xc2caf0,
        text_disabled: 0x8388a8,
        scrollbar: 0xffffff73,
        scrollbar_hover: 0xffffffa6,
        positive: 0x8ee89f,
        warning: 0xffdb94,
        danger: 0xff9db8,
        danger_fill: 0xc43154,
        slider_knob: 0xffffff,
    },
    // 5: Stone HC
    ThemeBase {
        rail: 0xe8e2d5,
        sidebar: 0xe8e2d5,
        canvas: 0xfbf9f5,
        raised: 0xeee8dc,
        overlay: 0xffffff,
        tooltip: 0xe3dccd,
        scrim: 0x1d191480,
        hairline: 0x29231a59,
        border: 0x29231a99,
        border_strong: 0x29231ac2,
        overlay_edge: 0x29231a99,
        hover: 0x29231a17,
        selected: 0x29231a29,
        pressed: 0x29231a38,
        field: 0x29231a14,
        segment: 0x29231a2e,
        count_fill: 0x29231a33,
        quiet_fill: 0x29231a29,
        switch_off: 0x29231a99,
        switch_knob: 0xffffff,
        code_inline: 0x29231a1a,
        float_hover: 0xdcd4c5,
        spoiler: 0xcec5b4,
        text: 0x120f0b,
        text_soft: 0x2c2720,
        text_faint: 0x3f3930,
        text_disabled: 0x6b6357,
        scrollbar: 0x29231a73,
        scrollbar_hover: 0x29231aa6,
        positive: 0x115e35,
        warning: 0x6e4200,
        danger: 0x9c1c19,
        danger_fill: 0x9c1c19,
        slider_knob: 0x1a1612,
    },
    // 6: Dusk HC
    ThemeBase {
        rail: 0x0e0c16,
        sidebar: 0x0e0c16,
        canvas: 0x13111e,
        raised: 0x1b1829,
        overlay: 0x221f33,
        tooltip: 0x29253d,
        scrim: 0x000000b3,
        hairline: 0xffffff47,
        border: 0xffffff80,
        border_strong: 0xffffffb3,
        overlay_edge: 0xffffff80,
        hover: 0xffffff1c,
        selected: 0xffffff30,
        pressed: 0xffffff40,
        field: 0xffffff16,
        segment: 0xffffff3b,
        count_fill: 0xffffff3b,
        quiet_fill: 0xffffff30,
        switch_off: 0xffffff80,
        switch_knob: 0xffffff,
        code_inline: 0xffffff24,
        float_hover: 0x322d4a,
        spoiler: 0x3c3658,
        text: 0xffffff,
        text_soft: 0xe0dcf5,
        text_faint: 0xc4bee0,
        text_disabled: 0x857ea3,
        scrollbar: 0xffffff73,
        scrollbar_hover: 0xffffffa6,
        positive: 0x82dbcb,
        warning: 0xffd28c,
        danger: 0xff96b0,
        danger_fill: 0xc23257,
        slider_knob: 0xffffff,
    },
    // 7: Forest HC
    ThemeBase {
        rail: 0x090d0b,
        sidebar: 0x090d0b,
        canvas: 0x0d1410,
        raised: 0x141e19,
        overlay: 0x1a2620,
        tooltip: 0x202e27,
        scrim: 0x000000b3,
        hairline: 0xffffff47,
        border: 0xffffff80,
        border_strong: 0xffffffb3,
        overlay_edge: 0xffffff80,
        hover: 0xffffff1c,
        selected: 0xffffff30,
        pressed: 0xffffff40,
        field: 0xffffff16,
        segment: 0xffffff3b,
        count_fill: 0xffffff3b,
        quiet_fill: 0xffffff30,
        switch_off: 0xffffff80,
        switch_knob: 0xffffff,
        code_inline: 0xffffff24,
        float_hover: 0x283830,
        spoiler: 0x31443a,
        text: 0xffffff,
        text_soft: 0xd5e6dc,
        text_faint: 0xb9cfc2,
        text_disabled: 0x768c80,
        scrollbar: 0xffffff73,
        scrollbar_hover: 0xffffffa6,
        positive: 0x73e8a4,
        warning: 0xffcc70,
        danger: 0xff918a,
        danger_fill: 0xc73636,
        slider_knob: 0xffffff,
    },
];

/// Curated accent ramps: `[curated_index]` in order Blue, Indigo, Violet, Emerald, Amber, Rose, Teal, Graphite.
const ACCENT_DARK: [AccentRamp; 8] = [
    // Blue
    AccentRamp { accent: 0x2764db, accent_hover: 0x2f6feb, accent_pressed: 0x1f55bf, accent_text: 0x94bdff, on_accent: 0xffffff },
    // Indigo
    AccentRamp { accent: 0x5254de, accent_hover: 0x5d5fe6, accent_pressed: 0x4446c9, accent_text: 0xb3b5ff, on_accent: 0xffffff },
    // Violet
    AccentRamp { accent: 0x7346e8, accent_hover: 0x7e52f0, accent_pressed: 0x6237d4, accent_text: 0xcfb5ff, on_accent: 0xffffff },
    // Emerald
    AccentRamp { accent: 0x1a754c, accent_hover: 0x1f8255, accent_pressed: 0x14613f, accent_text: 0x74e5ac, on_accent: 0xffffff },
    // Amber
    AccentRamp { accent: 0xd48d17, accent_hover: 0xe09b26, accent_pressed: 0xba7a10, accent_text: 0xf7c565, on_accent: 0x121315 },
    // Rose
    AccentRamp { accent: 0xc2325a, accent_hover: 0xcf3d65, accent_pressed: 0xa8274b, accent_text: 0xffa6be, on_accent: 0xffffff },
    // Teal
    AccentRamp { accent: 0x127380, accent_hover: 0x16808f, accent_pressed: 0x0e5f6a, accent_text: 0x72dfe9, on_accent: 0xffffff },
    // Graphite
    AccentRamp { accent: 0xcbd0db, accent_hover: 0xdce0e8, accent_pressed: 0xb4bac7, accent_text: 0xd2d7e2, on_accent: 0x121315 },
];

const ACCENT_LIGHT: [AccentRamp; 8] = [
    // Blue
    AccentRamp { accent: 0x1d5bd6, accent_hover: 0x184ec0, accent_pressed: 0x1441a3, accent_text: 0x1852c9, on_accent: 0xffffff },
    // Indigo
    AccentRamp { accent: 0x4644c9, accent_hover: 0x3a38b5, accent_pressed: 0x2f2d9c, accent_text: 0x3d3bb8, on_accent: 0xffffff },
    // Violet
    AccentRamp { accent: 0x6d35d9, accent_hover: 0x5d29c2, accent_pressed: 0x4e1fa8, accent_text: 0x622cc7, on_accent: 0xffffff },
    // Emerald
    AccentRamp { accent: 0x15784a, accent_hover: 0x11663e, accent_pressed: 0x0d5433, accent_text: 0x126b41, on_accent: 0xffffff },
    // Amber
    AccentRamp { accent: 0x9c5c00, accent_hover: 0x874e00, accent_pressed: 0x704000, accent_text: 0x8c5200, on_accent: 0xffffff },
    // Rose
    AccentRamp { accent: 0xbf2450, accent_hover: 0xa61d44, accent_pressed: 0x8c1738, accent_text: 0xb01e48, on_accent: 0xffffff },
    // Teal
    AccentRamp { accent: 0x0f727e, accent_hover: 0x0b606b, accent_pressed: 0x084f58, accent_text: 0x0c6670, on_accent: 0xffffff },
    // Graphite
    AccentRamp { accent: 0x434854, accent_hover: 0x353944, accent_pressed: 0x282b34, accent_text: 0x393e4a, on_accent: 0xffffff },
];

const ACCENT_DARK_HC: [AccentRamp; 8] = [
    // Blue
    AccentRamp { accent: 0x7eb0ff, accent_hover: 0x99c2ff, accent_pressed: 0x669ef7, accent_text: 0xb5d4ff, on_accent: 0x0a0a0c },
    // Indigo
    AccentRamp { accent: 0xa2a5ff, accent_hover: 0xb8baff, accent_pressed: 0x9194fa, accent_text: 0xc9cbff, on_accent: 0x0a0a0c },
    // Violet
    AccentRamp { accent: 0xc1a1ff, accent_hover: 0xd2baff, accent_pressed: 0xae88fa, accent_text: 0xdec9ff, on_accent: 0x0a0a0c },
    // Emerald
    AccentRamp { accent: 0x5de09e, accent_hover: 0x7aebb2, accent_pressed: 0x47cf8b, accent_text: 0x8cf2bd, on_accent: 0x0a0a0c },
    // Amber
    AccentRamp { accent: 0xf2b035, accent_hover: 0xffc252, accent_pressed: 0xdc9c23, accent_text: 0xffd985, on_accent: 0x0a0a0c },
    // Rose
    AccentRamp { accent: 0xff94b0, accent_hover: 0xffadc3, accent_pressed: 0xf27999, accent_text: 0xffc2d2, on_accent: 0x0a0a0c },
    // Teal
    AccentRamp { accent: 0x59d8e6, accent_hover: 0x78e4f0, accent_pressed: 0x42c6d4, accent_text: 0x94edf5, on_accent: 0x0a0a0c },
    // Graphite
    AccentRamp { accent: 0xe4e7ee, accent_hover: 0xffffff, accent_pressed: 0xcbd0db, accent_text: 0xffffff, on_accent: 0x0a0a0c },
];

const ACCENT_LIGHT_HC: [AccentRamp; 8] = [
    // Blue
    AccentRamp { accent: 0x1243b0, accent_hover: 0x0e3691, accent_pressed: 0x0a2973, accent_text: 0x0f3b9e, on_accent: 0xffffff },
    // Indigo
    AccentRamp { accent: 0x3330a3, accent_hover: 0x28258a, accent_pressed: 0x1f1c73, accent_text: 0x2c2994, on_accent: 0xffffff },
    // Violet
    AccentRamp { accent: 0x4f1fa8, accent_hover: 0x421794, accent_pressed: 0x36117d, accent_text: 0x471a9c, on_accent: 0xffffff },
    // Emerald
    AccentRamp { accent: 0x0a5431, accent_hover: 0x074527, accent_pressed: 0x05361e, accent_text: 0x094d2c, on_accent: 0xffffff },
    // Amber
    AccentRamp { accent: 0x6e3d00, accent_hover: 0x5c3200, accent_pressed: 0x4a2800, accent_text: 0x663800, on_accent: 0xffffff },
    // Rose
    AccentRamp { accent: 0x911236, accent_hover: 0x7a0e2d, accent_pressed: 0x630a24, accent_text: 0x850e30, on_accent: 0xffffff },
    // Teal
    AccentRamp { accent: 0x07525c, accent_hover: 0x05434b, accent_pressed: 0x04353c, accent_text: 0x064a53, on_accent: 0xffffff },
    // Graphite
    AccentRamp { accent: 0x262930, accent_hover: 0x191b20, accent_pressed: 0x0d0e11, accent_text: 0x1e2026, on_accent: 0xffffff },
];

const SYNTAX_DARK: SyntaxPalette = SyntaxPalette {
    keyword: 0xd2a8ff,
    string: 0x9ee089,
    comment: 0x9da9b8,
    number: 0xffa67a,
    type_: 0xffd275,
    function: 0x8cb8ff,
    added: 0x7ee787,
    removed: 0xff857d,
};

const SYNTAX_LIGHT: SyntaxPalette = SyntaxPalette {
    keyword: 0x6f32d6,
    string: 0x196e35,
    comment: 0x555b66,
    number: 0xa83b0f,
    type_: 0x8c5300,
    function: 0x164cb8,
    added: 0x167342,
    removed: 0xbf2424,
};

const SYNTAX_DARK_HC: SyntaxPalette = SyntaxPalette {
    keyword: 0xe4c7ff,
    string: 0xbaf2a8,
    comment: 0xbdc8d6,
    number: 0xffbfa0,
    type_: 0xffe09c,
    function: 0xb0d0ff,
    added: 0x9ef2a6,
    removed: 0xffa39c,
};

const SYNTAX_LIGHT_HC: SyntaxPalette = SyntaxPalette {
    keyword: 0x4c1ba3,
    string: 0x0d5426,
    comment: 0x383d47,
    number: 0x802700,
    type_: 0x6b3d00,
    function: 0x0c368f,
    added: 0x0c542e,
    removed: 0x961414,
};

/// `ThemeBase` lookup for any curated theme and contrast tier.
pub fn base_for(id: ThemeId, hc: bool) -> &'static ThemeBase {
    if hc {
        &BASE_HC[id as usize]
    } else {
        &BASE_STD[id as usize]
    }
}

/// `AccentRamp` lookup for any accent, polarity, and contrast tier.
pub fn accent_ramp_for(accent: Accent, light: bool, hc: bool) -> &'static AccentRamp {
    let idx = accent.curated_index();
    match (light, hc) {
        (false, false) => &ACCENT_DARK[idx],
        (true, false) => &ACCENT_LIGHT[idx],
        (false, true) => &ACCENT_DARK_HC[idx],
        (true, true) => &ACCENT_LIGHT_HC[idx],
    }
}

/// `SyntaxPalette` lookup for any polarity and contrast tier.
pub fn syntax_for(light: bool, hc: bool) -> &'static SyntaxPalette {
    match (light, hc) {
        (false, false) => &SYNTAX_DARK,
        (true, false) => &SYNTAX_LIGHT,
        (false, true) => &SYNTAX_DARK_HC,
        (true, true) => &SYNTAX_LIGHT_HC,
    }
}

fn base() -> &'static ThemeBase {
    base_for(active_theme(), is_high_contrast())
}

fn accent_ramp() -> &'static AccentRamp {
    accent_ramp_for(active_accent(), is_light(), is_high_contrast())
}

pub fn syntax() -> &'static SyntaxPalette {
    syntax_for(is_light(), is_high_contrast())
}

fn with_alpha(rgb24: u32, alpha: u8) -> Rgba {
    rgba((rgb24 << 8) | u32::from(alpha))
}

// Surfaces. The rail is not a third stripe: it takes the sidebar's value.
pub fn rail() -> Rgba { rgb(base().rail) }
pub fn sidebar() -> Rgba { rgb(base().sidebar) }
pub fn canvas() -> Rgba { rgb(base().canvas) }
/// Cards, code, banners, image placeholders, the neutral avatar plate. Opaque.
pub fn raised() -> Rgba { rgb(base().raised) }
/// Anything that floats: menu, popover, modal, toast.
pub fn overlay() -> Rgba { rgb(base().overlay) }
pub fn tooltip() -> Rgba { rgb(base().tooltip) }
pub fn scrim() -> Rgba { rgba(base().scrim) }

// Lines.
/// Pane and section dividers.
pub fn hairline() -> Rgba { rgba(base().hairline) }
/// Control and overlay edges.
pub fn border() -> Rgba { rgba(base().border) }
/// Field hover, checkbox edge.
pub fn border_strong() -> Rgba { rgba(base().border_strong) }
/// The 1 pt edge of a floating surface.
pub fn overlay_edge() -> Rgba { rgba(base().overlay_edge) }

// State fills. Alpha, so they sit on any surface; `over` gives the opaque result where a ring
// must match what is behind it.
pub fn hover() -> Rgba { rgba(base().hover) }
pub fn selected() -> Rgba { rgba(base().selected) }
pub fn pressed() -> Rgba { rgba(base().pressed) }
/// A text field at rest.
pub fn field() -> Rgba { rgba(base().field) }
/// The lit segment of a segmented switch.
pub fn segment() -> Rgba { rgba(base().segment) }
/// A switch that is off, a slider's empty side, an unlit meter segment.
pub fn switch_off() -> Rgba { rgba(base().switch_off) }
/// A switch thumb when off (on uses `on_accent()`).
pub fn switch_knob() -> Rgba { rgb(base().switch_knob) }
/// Inline code and timestamp pill fill.
pub fn code_inline() -> Rgba { rgba(base().code_inline) }
/// Hover state for floating buttons over messages.
pub fn float_hover() -> Rgba { rgb(base().float_hover) }
/// Unrevealed spoiler cover fill.
pub fn spoiler() -> Rgba { rgb(base().spoiler) }
/// Was the neutral count badge's fill; counts are unfilled now (`polish/01-system.md` §3).
#[allow(dead_code)]
pub fn count_fill() -> Rgba { rgba(base().count_fill) }
/// Removed from badges: a muted mention keeps the accent pill (C6).
#[allow(dead_code)]
pub fn quiet_fill() -> Rgba { rgba(base().quiet_fill) }

// Text.
pub fn text() -> Rgba { rgb(base().text) }
pub fn text_soft() -> Rgba { rgb(base().text_soft) }
/// The lowest level that may carry meaning.
pub fn text_faint() -> Rgba { rgb(base().text_faint) }
pub fn text_disabled() -> Rgba { rgb(base().text_disabled) }

// Scrollbars stay visible while content overflows, without competing with text.
pub fn scrollbar() -> Rgba { rgba(base().scrollbar) }
pub fn scrollbar_hover() -> Rgba { rgba(base().scrollbar_hover) }

// Accent: "for you" or "this will act". Never plain unread.
pub fn accent() -> Rgba { rgb(accent_ramp().accent) }
pub fn accent_hover() -> Rgba { rgb(accent_ramp().accent_hover) }
pub fn accent_pressed() -> Rgba { rgb(accent_ramp().accent_pressed) }
/// Links, @mentions, the "New" label, the caret.
pub fn accent_text() -> Rgba { rgb(accent_ramp().accent_text) }
pub fn focus() -> Rgba { accent_text() }
pub fn accent_soft() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x2e,
        (true, false) => 0x24,
        (false, true) => 0x40,
        (true, true) => 0x2e,
    };
    with_alpha(accent_ramp().accent, alpha)
}
/// A message that mentions me.
pub fn accent_wash() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x1a,
        (true, false) => 0x14,
        (false, true) => 0x26,
        (true, true) => 0x1c,
    };
    with_alpha(accent_ramp().accent, alpha)
}
/// The halo of a focused field.
pub fn accent_halo() -> Rgba {
    let r = accent_ramp();
    match (is_light(), is_high_contrast()) {
        (false, false) => with_alpha(r.accent, 0x59),
        (true, false) => with_alpha(r.accent, 0x4d),
        (false, true) => with_alpha(r.accent_text, 0x80),
        (true, true) => with_alpha(r.accent, 0x66),
    }
}
/// Selected text in a field.
pub fn selection() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x73,
        (true, false) => 0x40,
        (false, true) => 0x59,
        (true, true) => 0x4d,
    };
    with_alpha(accent_ramp().accent, alpha)
}
pub fn on_accent() -> Rgba { rgb(accent_ramp().on_accent) }
/// A mention of me inside the message body.
pub fn mention_me() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x52,
        (true, false) => 0x2e,
        (false, true) => 0x47,
        (true, true) => 0x38,
    };
    with_alpha(accent_ramp().accent, alpha)
}
/// Text colour painted over [`mention_me`].
pub fn mention_me_text() -> Rgba {
    if is_light() {
        accent_text()
    } else {
        text()
    }
}
/// Tinted chip fill (e.g. diff backgrounds, burst reactions) respecting polarity and HC.
pub fn tinted_chip(base_rgb: u32, mine: bool) -> Rgba {
    let alpha = match (is_light(), is_high_contrast(), mine) {
        (false, false, true) => 0x33,
        (false, false, false) => 0x1a,
        (true, false, true) => 0x24,
        (true, false, false) => 0x14,
        (false, true, true) => 0x47,
        (false, true, false) => 0x29,
        (true, true, true) => 0x38,
        (true, true, false) => 0x21,
    };
    with_alpha(base_rgb & 0x00ff_ffff, alpha)
}

// Light-on-dark media overlays (viewer chrome, dark duration chips, starred scrim controls).
pub fn on_media() -> Rgba { rgb(0xffffff) }
pub fn on_media_soft() -> Rgba { rgba(0xffffffb8) }
pub fn media_chip() -> Rgba { rgba(0x0000008c) }
/// Dark letterbox stage behind video / camera frames.
pub fn media_stage() -> Rgba { rgb(0x0b0b0d) }
/// Darker badge/scrim fill over media.
pub fn media_dim() -> Rgba { rgba(0x000000a6) }
/// Hover fill for light-on-dark media controls.
pub fn media_hover() -> Rgba { rgba(0xffffff1a) }
/// Pressed fill for light-on-dark media controls.
pub fn media_pressed() -> Rgba { rgba(0xffffff29) }
/// Hairline edge on dark media chrome.
pub fn media_hairline() -> Rgba { rgba(0xffffff24) }
/// Gold favorite star over dark media chips.
pub fn media_star() -> Rgba { rgb(0xffc44d) }
/// Active accent colour guaranteed legible on dark media overlays (always luminous dark-HC ramp).
pub fn accent_on_media() -> Rgba {
    rgb(accent_ramp_for(active_accent(), false, true).accent_text)
}

/// Slider thumb fill, visible on both dark and light surfaces.
pub fn slider_knob() -> Rgba { rgb(base().slider_knob) }

// Semantic.
pub fn positive() -> Rgba { rgb(base().positive) }
pub fn positive_soft() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x26,
        (true, false) => 0x1f,
        (false, true) => 0x33,
        (true, true) => 0x26,
    };
    with_alpha(base().positive, alpha)
}
pub fn warning() -> Rgba { rgb(base().warning) }
pub fn warning_soft() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x24,
        (true, false) => 0x1f,
        (false, true) => 0x33,
        (true, true) => 0x26,
    };
    with_alpha(base().warning, alpha)
}
pub fn danger() -> Rgba { rgb(base().danger) }
pub fn danger_fill() -> Rgba { rgb(base().danger_fill) }
pub fn danger_soft() -> Rgba {
    let alpha = match (is_light(), is_high_contrast()) {
        (false, false) => 0x26,
        (true, false) => 0x1f,
        (false, true) => 0x33,
        (true, true) => 0x26,
    };
    with_alpha(base().danger, alpha)
}

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

/// Scales a dark-theme shadow alpha for the active polarity so light themes stay soft.
pub fn shadow_alpha(dark_alpha: f32) -> f32 {
    if is_light() {
        dark_alpha * 0.35
    } else {
        dark_alpha
    }
}

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

/// Selection ring around a circular swatch dot on `surface`.
pub fn swatch_ring(surface: Rgba) -> Vec<BoxShadow> {
    vec![ring(surface, 1.5), ring(text(), 3.5)]
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

/// Exhaustive WCAG contrast check over all 8 themes × 9 accents × {Standard, HC}, used by both
/// unit tests and the lab's `curated-themes` proof driver.
pub(crate) fn verify_all_wcag() -> Result<(), String> {
    for id in ThemeId::ALL {
        let light = id.is_light();
        for hc in [false, true] {
            let tier = if hc { "HC" } else { "STD" };
            let min_text = if hc { 7.0_f32 } else { 4.5_f32 };
            let b = base_for(id, hc);
            let surfaces = [
                ("rail", rgb(b.rail)),
                ("sidebar", rgb(b.sidebar)),
                ("canvas", rgb(b.canvas)),
                ("raised", rgb(b.raised)),
                ("overlay", rgb(b.overlay)),
                ("tooltip", rgb(b.tooltip)),
            ];
            for (bg_name, bg) in surfaces {
                for (tok, fg) in [("text", rgb(b.text)), ("text_soft", rgb(b.text_soft)), ("text_faint", rgb(b.text_faint))] {
                    let r = contrast_ratio(fg, bg);
                    if r < min_text {
                        return Err(format!("{} {tier} {tok} on {bg_name}: {r:.2} < {min_text}", id.word()));
                    }
                }
            }
            for (bg_name, bg) in [("canvas", rgb(b.canvas)), ("raised", rgb(b.raised))] {
                for (tok, fg) in [("positive", rgb(b.positive)), ("warning", rgb(b.warning)), ("danger", rgb(b.danger))] {
                    let r = contrast_ratio(fg, bg);
                    if r < 4.5 {
                        return Err(format!("{} {tier} {tok} on {bg_name}: {r:.2} < 4.5", id.word()));
                    }
                }
                let bs = contrast_ratio(over(rgba(b.border_strong), bg), bg);
                if bs < 3.0 {
                    return Err(format!("{} {tier} border_strong on {bg_name}: {bs:.2} < 3.0", id.word()));
                }
                let sw = contrast_ratio(over(rgba(b.switch_off), bg), bg);
                if sw < 3.0 {
                    return Err(format!("{} {tier} switch_off on {bg_name}: {sw:.2} < 3.0", id.word()));
                }
                if hc {
                    let bd = contrast_ratio(over(rgba(b.border), bg), bg);
                    if bd < 3.0 {
                        return Err(format!("{} {tier} border on {bg_name}: {bd:.2} < 3.0", id.word()));
                    }
                }
            }
            let df = contrast_ratio(rgb(0xffffff), rgb(b.danger_fill));
            if df < 4.5 {
                return Err(format!("{} {tier} white on danger_fill: {df:.2} < 4.5", id.word()));
            }
            for (bg_name, bg) in [("canvas", rgb(b.canvas)), ("raised", rgb(b.raised)), ("overlay", rgb(b.overlay))] {
                let sk = contrast_ratio(rgb(b.slider_knob), bg);
                if sk < 4.5 {
                    return Err(format!("{} {tier} slider_knob on {bg_name}: {sk:.2} < 4.5", id.word()));
                }
            }
            let syn = syntax_for(light, hc);
            let raised = rgb(b.raised);
            for (tok, fg) in [
                ("keyword", syn.keyword),
                ("string", syn.string),
                ("comment", syn.comment),
                ("number", syn.number),
                ("type_", syn.type_),
                ("function", syn.function),
                ("added", syn.added),
                ("removed", syn.removed),
            ] {
                let r = contrast_ratio(rgb(fg), raised);
                if r < min_text {
                    return Err(format!("{} {tier} syntax.{tok} on raised: {r:.2} < {min_text}", id.word()));
                }
            }
            for acc in Accent::ALL {
                let resolved = if acc == Accent::Auto { id.default_accent() } else { acc };
                let ramp = accent_ramp_for(resolved, light, hc);
                for (bg_name, bg) in surfaces {
                    let r = contrast_ratio(rgb(ramp.accent_text), bg);
                    if r < min_text {
                        return Err(format!("{} {tier} {} accent_text on {bg_name}: {r:.2} < {min_text}", id.word(), acc.word()));
                    }
                }
                for (state, fill) in [
                    ("accent", ramp.accent),
                    ("accent_hover", ramp.accent_hover),
                    ("accent_pressed", ramp.accent_pressed),
                ] {
                    let r = contrast_ratio(rgb(ramp.on_accent), rgb(fill));
                    if r < min_text {
                        return Err(format!("{} {tier} {} on_accent on {state}: {r:.2} < {min_text}", id.word(), acc.word()));
                    }
                }
                if hc && !light {
                    let ui = contrast_ratio(rgb(ramp.accent), raised);
                    if ui < 6.0 {
                        return Err(format!("{} {tier} {} accent UI contrast on raised: {ui:.2} < 6.0", id.word(), acc.word()));
                    }
                }
                let mention_alpha = match (light, hc) {
                    (false, false) => 0x52,
                    (true, false) => 0x2e,
                    (false, true) => 0x47,
                    (true, true) => 0x38,
                };
                let mention_bg = over(with_alpha(ramp.accent, mention_alpha), rgb(b.canvas));
                let mention_fg = if light { rgb(ramp.accent_text) } else { rgb(b.text) };
                let mr = contrast_ratio(mention_fg, mention_bg);
                let min_mention = if hc { 5.5 } else { 4.5 };
                if mr < min_mention {
                    return Err(format!("{} {tier} {} mention_me_text on mention_me: {mr:.2} < {min_mention}", id.word(), acc.word()));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_and_accent_combination_meets_wcag_aa_and_hc_meets_aaa() {
        if let Err(err) = verify_all_wcag() {
            panic!("{err}");
        }
    }

    #[test]
    fn dark_blue_accent_hover_meets_wcag_aa() {
        let ramp = accent_ramp_for(Accent::Blue, false, false);
        let ratio = contrast_ratio(rgb(ramp.on_accent), rgb(ramp.accent_hover));
        assert!(ratio >= 4.57, "Dark Blue accent_hover contrast {ratio:.2} < 4.57");
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
        let light_bg = rgb(base_for(ThemeId::Paper, false).canvas);
        let dark_bg = rgb(base_for(ThemeId::Graphite, false).canvas);
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
    fn appearance_theme_and_accent_words_round_trip_and_overrides_parse() {
        for a in [Appearance::System, Appearance::Light, Appearance::Dark] {
            assert_eq!(Appearance::from_word(a.word()), Some(a));
        }
        assert_eq!(Appearance::from_word("sepia"), None);
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::from_word(id.word()), Some(id));
            assert_eq!(ThemeId::from_dark_word(id.word()).is_some(), id.is_dark());
            assert_eq!(ThemeId::from_light_word(id.word()).is_some(), id.is_light());
        }
        for acc in Accent::ALL {
            assert_eq!(Accent::from_word(acc.word()), Some(acc));
        }
        assert!(set_override(Some("light")));
        assert_eq!((active_theme(), mode()), (ThemeId::Paper, Mode::Light));
        assert!(set_override(Some("light-hc")));
        assert_eq!((active_theme(), mode()), (ThemeId::Paper, Mode::LightHighContrast));
        assert!(set_override(Some("dark-hc")));
        assert_eq!((active_theme(), mode()), (ThemeId::Graphite, Mode::DarkHighContrast));
        assert!(set_override(Some("dark")));
        assert_eq!((active_theme(), mode()), (ThemeId::Graphite, Mode::Dark));
        for id in ThemeId::ALL {
            assert!(set_override(Some(id.word())));
            assert_eq!((active_theme(), is_high_contrast()), (id, false));
            assert!(set_override(Some(&format!("{}-hc", id.word()))));
            assert_eq!((active_theme(), is_high_contrast()), (id, true));
        }
        assert!(!set_override(Some("invalid")));
        assert!(!set_override(Some("invalid-hc")));
        assert!(set_override(None));

        assert!(set_override(Some("nord")));
        assert!(set_accent_override(Some("auto")));
        assert_eq!(active_accent(), Accent::Teal);
        assert!(set_accent_override(Some("rose")));
        assert_eq!(active_accent(), Accent::Rose);
        assert!(!set_accent_override(Some("neon")));
        assert!(set_accent_override(None));
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
        for acc in Accent::CURATED {
            let on_chip = rgb(accent_ramp_for(acc, false, true).accent_text);
            let on_stage = contrast_ratio(on_chip, media_stage());
            assert!(on_stage >= 10.0, "accent_on_media {} on media_stage: {on_stage:.2} < 10.0", acc.word());
            let on_white_chip = contrast_ratio(on_chip, chip_on_white);
            assert!(on_white_chip >= 3.0, "accent_on_media {} on media_chip(white): {on_white_chip:.2} < 3.0", acc.word());
        }
    }
}
