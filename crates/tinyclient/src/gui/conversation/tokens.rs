//! Colors and sizes only the conversation view uses. Shared tokens come from `theme`; these are
//! the ones `docs/design/03-conversation.md` adds, mapped onto the values of `01-system.md`.

use crate::gui::theme;
use gpui::{rgb, rgba, Rgba};

/// Links, mentions and "N replies": the accent adjusted for body-text contrast.
pub fn link() -> Rgba { theme::accent_text() }
pub fn row_hover() -> Rgba { theme::hover() }
/// A mention of me inside the text.
pub fn mention_me() -> Rgba { theme::mention_me() }
pub fn border_strong() -> Rgba { theme::border_strong() }
pub fn code_inline() -> Rgba { theme::code_inline() }
/// Opaque, so a picture's box hides the row tint behind it.
pub fn media_fill() -> Rgba { theme::raised() }
pub fn card() -> Rgba { theme::raised() }
pub fn code_bg() -> Rgba { theme::raised() }
pub fn code_keyword() -> Rgba { rgb(theme::syntax().keyword) }
pub fn code_string() -> Rgba { rgb(theme::syntax().string) }
pub fn code_comment() -> Rgba { rgb(theme::syntax().comment) }
pub fn code_number() -> Rgba { rgb(theme::syntax().number) }
pub fn code_type() -> Rgba { rgb(theme::syntax().type_) }
pub fn code_function() -> Rgba { rgb(theme::syntax().function) }
pub fn code_added() -> Rgba { rgb(theme::syntax().added) }
pub fn code_added_bg() -> Rgba { theme::tinted_chip(theme::syntax().added, true) }
pub fn code_removed() -> Rgba { rgb(theme::syntax().removed) }
pub fn code_removed_bg() -> Rgba { theme::tinted_chip(theme::syntax().removed, true) }

/// ANSI SGR colours. `0` is the default, so it has no paint of its own.
pub fn code_ansi(code: u8) -> Option<Rgba> {
    let (index, bright) = match code {
        30..=37 => (code - 30, false),
        90..=97 => (code - 90, true),
        40..=47 => (code - 40, false),
        100..=107 => (code - 100, true),
        _ => return None,
    };
    let s = theme::syntax();
    let cyan = theme::accent_ramp_for(theme::Accent::Teal, theme::is_light(), theme::is_high_contrast()).accent_text;
    let color = match index {
        0 => if theme::is_light() { if bright { theme::text_soft() } else { theme::text() } } else { if bright { theme::text_faint() } else { theme::text_disabled() } },
        1 => rgb(s.removed),
        2 => rgb(s.string),
        3 => rgb(s.type_),
        4 => rgb(s.function),
        5 => rgb(s.keyword),
        6 => rgb(cyan),
        _ => if theme::is_light() { theme::text_faint() } else { theme::text() },
    };
    Some(color)
}
pub fn float_hover() -> Rgba { theme::float_hover() }
pub fn spoiler() -> Rgba { theme::spoiler() }
/// Labels over media.
pub fn badge() -> Rgba { theme::media_dim() }
pub fn scrim() -> Rgba { if theme::is_light() { rgba(0x000000c0) } else { rgba(0x0b0b0df0) } }
pub fn selection() -> Rgba { theme::selection() }

/// Code's face: platform monospace across macOS, Windows, Linux, and mobile.
pub const MONO: &str = crate::gui::theme::MONO;

/// The widest a line of text, a code block or a card may be.
pub const MEASURE: f32 = 620.;

/// The sizes that differ between the conversation and the thread pane.
#[derive(Clone, Copy, PartialEq)]
pub struct Metrics {
    pub inset: f32,
    pub avatar: f32,
    pub gap: f32,
    /// The widest a picture or a grid may be.
    pub media: f32,
}

impl Metrics {
    pub const FULL: Metrics = Metrics { inset: 20., avatar: 36., gap: 12., media: 400. };
    pub const DENSE: Metrics = Metrics { inset: 16., avatar: 28., gap: 10., media: 310. };
    /// A phone conversation: 16 from the edge, the desktop's 36 pt avatar 10 from the words, and
    /// a picture as wide as the column (the column, not this, is what limits it).
    pub const PHONE: Metrics = Metrics { inset: 16., avatar: 36., gap: 10., media: 400. };

    /// Where the body starts, from the row's left edge.
    pub fn gutter(self) -> f32 {
        self.inset + self.avatar + self.gap
    }
}
