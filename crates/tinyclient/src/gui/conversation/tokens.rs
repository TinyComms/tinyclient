//! Colors and sizes only the conversation view uses. Shared tokens come from `theme`; these are
//! the ones `docs/design/03-conversation.md` adds, mapped onto the values of `01-system.md`.

use crate::gui::theme;
use gpui::{rgb, rgba, Rgba};

/// Links, mentions and "N replies": the accent adjusted for body-text contrast.
pub fn link() -> Rgba { theme::accent_text() }
pub fn row_hover() -> Rgba {
    if theme::is_light() { rgba(0x00000008) } else { rgba(0xffffff08) }
}
/// A mention of me inside the text.
pub fn mention_me() -> Rgba {
    if theme::is_light() { rgba(0x1d5bd62e) } else { rgba(0x2f6feb52) }
}
pub fn border_strong() -> Rgba { theme::border_strong() }
pub fn code_inline() -> Rgba {
    if theme::is_light() { rgba(0x0000000f) } else { rgba(0xffffff14) }
}
/// Opaque, so a picture's box hides the row tint behind it.
pub fn media_fill() -> Rgba { theme::raised() }
pub fn card() -> Rgba { theme::raised() }
pub fn code_bg() -> Rgba { theme::raised() }
pub fn code_keyword() -> Rgba { if theme::is_light() { rgb(0x7c3aed) } else { rgb(0xc792ea) } }
pub fn code_string() -> Rgba { if theme::is_light() { rgb(0x1e7e34) } else { rgb(0xc3e88d) } }
pub fn code_comment() -> Rgba { if theme::is_light() { rgb(0x5b606d) } else { rgb(0x7f8c98) } }
pub fn code_number() -> Rgba { if theme::is_light() { rgb(0xb84314) } else { rgb(0xf78c6c) } }
pub fn code_type() -> Rgba { if theme::is_light() { rgb(0x9a5b00) } else { rgb(0xffcb6b) } }
pub fn code_function() -> Rgba { if theme::is_light() { rgb(0x1852c9) } else { rgb(0x82aaff) } }
pub fn code_added() -> Rgba { if theme::is_light() { rgb(0x1a7f49) } else { rgb(0x7ee787) } }
pub fn code_added_bg() -> Rgba { if theme::is_light() { rgba(0x1a7f4924) } else { rgba(0x3fb95033) } }
pub fn code_removed() -> Rgba { if theme::is_light() { rgb(0xc92a2a) } else { rgb(0xff7b72) } }
pub fn code_removed_bg() -> Rgba { if theme::is_light() { rgba(0xc92a2a24) } else { rgba(0xf8514933) } }

/// ANSI SGR colours. `0` is the default, so it has no paint of its own.
pub fn code_ansi(code: u8) -> Option<Rgba> {
    let (index, bright) = match code {
        30..=37 => (code - 30, false),
        90..=97 => (code - 90, true),
        40..=47 => (code - 40, false),
        100..=107 => (code - 100, true),
        _ => return None,
    };
    if theme::is_light() {
        const DIM: [u32; 8] = [0x141518, 0xc92a2a, 0x1a7f49, 0x9a6200, 0x1852c9, 0x7c3aed, 0x0e7490, 0x5b606d];
        const BRIGHT: [u32; 8] = [0x454954, 0xd93838, 0x1e8f53, 0xb07200, 0x2563eb, 0x9333ea, 0x0891b2, 0x6b7280];
        return Some(rgb(if bright { BRIGHT[index as usize] } else { DIM[index as usize] }));
    }
    const DIM: [u32; 8] = [0x4a4a4a, 0xf07178, 0xc3e88d, 0xffcb6b, 0x82aaff, 0xc792ea, 0x89ddff, 0xeeffff];
    const BRIGHT: [u32; 8] = [0x6b6b6b, 0xff8b92, 0xd0f0a0, 0xffe082, 0x9cc4ff, 0xd4b0ff, 0xb3e5ff, 0xffffff];
    Some(rgb(if bright { BRIGHT[index as usize] } else { DIM[index as usize] }))
}
pub fn float_hover() -> Rgba { if theme::is_light() { rgb(0xe4e6eb) } else { rgb(0x303136) } }
pub fn spoiler() -> Rgba { if theme::is_light() { rgb(0xd0d4dc) } else { rgb(0x3a3b42) } }
/// Labels over media.
pub fn badge() -> Rgba { rgba(0x000000a6) }
pub fn scrim() -> Rgba { if theme::is_light() { rgba(0x000000c0) } else { rgba(0x0b0b0df0) } }
pub fn selection() -> Rgba { theme::selection() }

/// Code's face. Menlo is on Apple systems only: Android has its own mono face (`theme::MONO`),
/// and without it a code span fell back to the proportional system face there.
// TODO(design): the desktops off Apple likely want `theme::MONO` too; left as drawn so their
// screenshots stay put.
#[cfg(target_os = "android")]
pub const MONO: &str = crate::gui::theme::MONO;
#[cfg(not(target_os = "android"))]
pub const MONO: &str = "Menlo";

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
