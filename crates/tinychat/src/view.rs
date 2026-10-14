//! What a row says, shared by the window and the text output so both read the same.

use protocol::{Badge, Conversation};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// How long ago, in the one unit that fits: `now`, `5m`, `3h`, `2d`.
pub fn age(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    match seconds {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `None` when there is nothing unread, so the row draws no badge at all.
pub fn unread_badge(unread: u32) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(unread.to_string()),
        _ => Some("99+".to_owned()),
    }
}

/// What a conversation's row says beside its preview in the text output. The window draws a
/// dot for a channel with unread messages and no mention; as text it is its count.
pub fn row_badge(conversation: &Conversation) -> Option<String> {
    match conversation.badge() {
        Badge::None => None,
        Badge::Dot => unread_badge(conversation.unread),
        Badge::Count(count) => unread_badge(count),
    }
}
