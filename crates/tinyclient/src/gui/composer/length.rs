//! How long a draft is once mentions are in the network's form, and the line that says so.
//! A limit of 0 means this account does not count.

use super::mention::{self, Token};
use super::niceties;
use protocol::Network;

/// The text a send would put on the wire: mentions expanded, then emoticons when that setting is on.
pub fn wire_text(field: &str, tokens: &[Token], network: Network, emoticons: bool) -> String {
    let wire = mention::wire(field, tokens, network);
    let wire = wire.trim();
    match emoticons {
        true => niceties::emoticons(wire),
        false => wire.to_owned(),
    }
}

pub fn over(counted: usize, limit: u32) -> bool {
    limit > 0 && counted > limit as usize
}

/// Near the limit, or past it. None while the account does not count, or while more than 100 characters remain.
pub fn line(counted: usize, limit: u32) -> Option<String> {
    if limit == 0 {
        return None;
    }
    let limit = limit as usize;
    if counted > limit {
        Some(format!(
            "{counted} / {limit}. Press Return to send this as message.txt."
        ))
    } else if limit.saturating_sub(counted) <= 100 {
        Some(format!("{counted} / {limit}"))
    } else {
        None
    }
}

/// An edit has no file to fall back on.
pub fn edit_refusal(counted: usize, limit: u32) -> Option<String> {
    over(counted, limit).then(|| format!("{counted} / {limit}. An edit this long cannot be saved."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::composer::mention::Kind;

    fn person(range: std::ops::Range<usize>, label: &str, id: &str) -> Token {
        Token {
            range,
            label: label.into(),
            kind: Kind::Person,
            id: id.into(),
        }
    }

    #[test]
    fn the_count_is_the_wire_form_not_what_the_field_shows() {
        let field = "@Mara hello";
        let wire = wire_text(
            field,
            &[person(0..5, "@Mara", "1000")],
            Network::Discord,
            false,
        );
        assert_eq!(wire, "<@1000> hello");
        assert_ne!(wire.chars().count(), field.chars().count());
    }

    #[test]
    fn emoticons_count_when_the_send_would_convert_them() {
        assert_eq!(wire_text("hi :)", &[], Network::Discord, false), "hi :)");
        assert_eq!(wire_text("hi :)", &[], Network::Discord, true), "hi 🙂");
    }

    #[test]
    fn the_counter_appears_near_the_limit_and_offers_a_file_past_it() {
        assert_eq!(line(1899, 2000), None);
        assert_eq!(line(1900, 2000).as_deref(), Some("1900 / 2000"));
        assert_eq!(line(2000, 2000).as_deref(), Some("2000 / 2000"));
        assert_eq!(
            line(2001, 2000).as_deref(),
            Some("2001 / 2000. Press Return to send this as message.txt.")
        );
        assert_eq!(line(5000, 0), None);
        assert!(!over(2001, 0));
        assert!(!over(2000, 2000));
        assert!(over(2001, 2000));
        assert_eq!(
            edit_refusal(2001, 2000).as_deref(),
            Some("2001 / 2000. An edit this long cannot be saved.")
        );
        assert_eq!(edit_refusal(2000, 2000), None);
        assert_eq!(edit_refusal(2001, 0), None);
    }
}
