//! The reaction picker is the composer's one picker on its Emoji tab alone
//! (`gui/picker`). What the conversation calls stays here: `Picker::new` with the
//! conversation's own custom emoji and its reaction rules, `PickerEvent::Picked`, `quick` for
//! the toolbar's and menu's slots and `used` when a reaction is sent. Recents are the open
//! conversation's account's.

use crate::gui::composer::picker::{self, emoji as table};

pub use picker::{Picker, PickerEvent};

/// The picker's width, for placing it.
pub const WIDTH: f32 = picker::WIDTH;
/// The picker's preferred height. A short window shrinks it and the rest scrolls.
pub const HEIGHT: f32 = picker::HEIGHT;

/// The quick reactions for a message: the most used, kept to what the conversation's rules
/// let go through on it (`table::quick`).
pub fn quick(rules: Option<&protocol::ReactionRules>, reactions: &[protocol::Reaction], count: usize) -> Vec<String> {
    table::quick(rules, &table::recent(), reactions, count)
}

/// How an emoji is drawn (`table::display`).
pub fn display(emoji: &str) -> String {
    table::display(emoji)
}

/// A reaction was sent with `emoji`.
pub fn used(emoji: &str) {
    table::used(emoji);
}
