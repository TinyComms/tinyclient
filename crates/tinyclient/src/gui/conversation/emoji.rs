//! The reaction picker is the composer's one picker on its Emoji tab alone
//! (`gui/picker`). What the conversation calls stays here: `Picker::new(custom, window, cx)` with
//! the conversation's own custom emoji, `PickerEvent::Picked`, `recents` for the toolbar's quick
//! slots and `used` when a reaction is sent. Recents are the open conversation's account's.

use crate::gui::composer::picker::{self, emoji as table};

pub use picker::{Picker, PickerEvent};

/// The picker's width, for placing it.
pub const WIDTH: f32 = picker::WIDTH;

/// The `count` emoji this account uses most, filled out with the defaults.
pub fn recents(count: usize) -> Vec<String> {
    table::filled(&table::recent(), count)
}

/// A reaction was sent with `emoji`.
pub fn used(emoji: &str) {
    table::used(emoji);
}
