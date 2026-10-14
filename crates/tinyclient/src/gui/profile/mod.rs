//! What the profile pane, the floating card and the Details pane say, in one fixed order on
//! every network (`design/polish/05-profile-info.md`). `facts` decides what is shown from the
//! data alone; `draw` and `shared` draw it; `details` is a conversation's and a Space's.
//!
//! Declared from `people/info.rs`, which owns the pane entity.

pub(in crate::gui) mod details;
pub(in crate::gui) mod draw;
pub(in crate::gui) mod facts;
pub(in crate::gui) mod shared;
