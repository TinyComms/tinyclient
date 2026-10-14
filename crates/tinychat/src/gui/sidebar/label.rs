//! What a conversation's row is called in a list that mixes Spaces: `Space › #channel`, so two
//! channels with one name can be told apart. When the line is too narrow the Space gives up
//! width first: the channel's own name is what the eye is looking for.

use protocol::{Conversation, ConversationKind};

/// The least a Space's name is cut to before the title starts to give way: room for a few
/// letters and the ellipsis.
pub const SPACE_MIN: f32 = 28.;

/// The Space a row names before its title. A channel always says where it is. A direct
/// conversation says so only when its network has more than one account signed in (`several`):
/// then the same person can be in two workspaces.
pub fn place(conversation: &Conversation, several: bool) -> Option<&str> {
    let named = !conversation.space_title.is_empty();
    (named && (!conversation.kind.is_direct() || several)).then_some(conversation.space_title.as_str())
}

/// The title as a mixed list shows it: a channel with its `#`.
pub fn title(conversation: &Conversation) -> String {
    let name = conversation.title.trim_start_matches('#');
    match conversation.kind {
        ConversationKind::Direct | ConversationKind::Group | ConversationKind::Thread => name.to_owned(),
        _ => format!("#{name}"),
    }
}

/// The widths the Space's name and the title get in `room`, given what each needs in full.
/// The Space is cut first, down to [`SPACE_MIN`]; only then the title.
pub fn fit(space: f32, title: f32, room: f32) -> (f32, f32) {
    if space + title <= room {
        return (space, title);
    }
    let least = space.min(SPACE_MIN);
    let space_gets = (room - title).max(least).min(space);
    (space_gets, title.min((room - space_gets).max(0.)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_space_is_cut_before_the_title() {
        // Room for both: nothing is cut.
        assert_eq!(fit(80., 60., 200.), (80., 60.));
        // A little short: only the Space gives.
        assert_eq!(fit(80., 60., 120.), (60., 60.));
        // Very short: the Space is at its least, and now the title gives.
        assert_eq!(fit(80., 60., 70.), (28., 42.));
        // A Space with a tiny name is never stretched to the minimum.
        assert_eq!(fit(10., 100., 60.), (10., 50.));
        assert_eq!(fit(80., 60., 0.), (28., 0.));
    }

    #[test]
    fn a_channel_says_where_it_is_and_a_person_only_when_it_is_ambiguous() {
        let mut row = crate::gui::sidebar::tests::row("general", ConversationKind::Channel);
        assert_eq!((place(&row, false), title(&row)), (None, "#general".to_owned()));
        row.space_title = "Moss & Lichen".into();
        assert_eq!(place(&row, false), Some("Moss & Lichen"));
        row.kind = ConversationKind::Direct;
        row.title = "Mara Okafor".into();
        assert_eq!((place(&row, false), place(&row, true), title(&row)), (None, Some("Moss & Lichen"), "Mara Okafor".to_owned()));
    }
}
