//! Colors and sizes only the conversation view uses. Shared tokens come from `theme`; these are
//! the ones `docs/design/03-conversation.md` adds, mapped onto the values of `01-system.md`.

use gpui::{rgb, rgba, Rgba};

/// Links, mentions and "N replies": the accent lightened for body-text contrast.
pub fn link() -> Rgba { rgb(0x8ab4ff) }
pub fn row_hover() -> Rgba { rgba(0xffffff08) }
/// A mention of me inside the text: accent at 32%.
pub fn mention_me() -> Rgba { rgba(0x2f6feb52) }
pub fn border_strong() -> Rgba { rgba(0xffffff33) }
pub fn code_inline() -> Rgba { rgba(0xffffff14) }
/// Opaque, so a picture's box hides the row tint behind it.
pub fn media_fill() -> Rgba { rgb(0x1f2023) }
pub fn card() -> Rgba { rgb(0x1f2023) }
pub fn code_bg() -> Rgba { rgb(0x1f2023) }
pub fn float_hover() -> Rgba { rgb(0x303136) }
pub fn spoiler() -> Rgba { rgb(0x3a3b42) }
/// Labels over media.
pub fn badge() -> Rgba { rgba(0x000000a6) }
pub fn scrim() -> Rgba { rgba(0x0b0b0df0) }
pub fn selection() -> Rgba { rgba(0x2f6feb73) }

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
