//! The call header's security indicator: the one slot where a call says how it is protected
//! (end-to-end encryption, a verified server). Every call header asks this file, so the
//! indicator is drawn the same everywhere it appears:
//!
//! - the dock's title row, at its right, before the avatar stack (`dock.rs`),
//! - the compact "Voice connected" bar of a Tiny server's column (`dock.rs`),
//! - a Tiny channel's top bar, after its chips, while I am in that channel's call
//!   (`tiny/pane.rs`).
//!
//! Each place leaves room for a 24 × 24 element and draws nothing when this is `None`.

use crate::gui::state::Shell;
use gpui::{AnyElement, Context};
use protocol::Call;

/// Where the indicator is drawn, should it look different in one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Place {
    Dock,
    TinyBar,
    TopBar,
}

impl Shell {
    /// What the call header shows about the call's protection, or nothing.
    // CONTRACT: the end-to-end encryption work fills this in (a lock badge that opens the
    // security panel). Keep it at most 24 × 24 and give it its own tooltip.
    pub(in crate::gui) fn call_security(&self, _call: &Call, _place: Place, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}
