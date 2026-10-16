//! Mentions in the field. What the user reads is `@Mara Okafor` and `#platform`; each completed
//! one is a token, a range of the text with the id it names. At send the tokens become what the
//! network takes. All of it is plain functions over text, so it is tested without a window.

use protocol::{MentionKind, Message, Network, Style};
use std::ops::Range;

/// The longest run after `@` or `#` that is still looked up.
const QUERY: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Person,
    Channel,
    Role,
    /// A network's own emoji, `:name:`; its id is the network's, `a:` first when animated.
    Emoji,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    /// What the range read when it was completed, sigil included.
    pub label: String,
    pub kind: Kind,
    pub id: String,
}

/// The text as the network takes it: every token that still reads as its label is replaced by
/// the network's own form. A token whose text was edited is ordinary text.
pub fn wire(text: &str, tokens: &[Token], network: Network) -> String {
    let mut live: Vec<&Token> = tokens.iter().filter(|token| text.get(token.range.clone()) == Some(token.label.as_str())).collect();
    live.sort_by_key(|token| token.range.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for token in live {
        if token.range.start < at {
            continue;
        }
        out.push_str(&text[at..token.range.start]);
        match (network, token.kind) {
            (Network::Discord, Kind::Person) => out.push_str(&format!("<@{}>", token.id)),
            (Network::Discord, Kind::Role) => out.push_str(&format!("<@&{}>", token.id)),
            (Network::Discord, Kind::Channel) => out.push_str(&format!("<#{}>", token.id)),
            (Network::Slack, Kind::Person) => out.push_str(&format!("<@{}>", token.id)),
            (Network::Slack, Kind::Role) => out.push_str(&format!("<!subteam^{}>", token.id)),
            (Network::Slack, Kind::Channel) => out.push_str(&format!("<#{}>", token.id)),
            (Network::Discord, Kind::Emoji) => {
                let name = token.label.trim_matches(':');
                match token.id.strip_prefix("a:") {
                    Some(id) => out.push_str(&format!("<a:{name}:{id}>")),
                    None => out.push_str(&format!("<:{name}:{}>", token.id)),
                }
            }
            // Slack takes the name as typed.
            (Network::Slack, Kind::Emoji) => out.push_str(&token.label),
            // No adapter yet says what these take, so the name goes as it reads.
            (Network::Telegram | Network::Teams | Network::Signal | Network::Messages, _) => out.push_str(&token.label),
        }
        at = token.range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Moves the tokens to where they are after an edit turned `old` into `new`. A token the edit
/// touched is dropped: it is text again.
pub fn shift(tokens: &mut Vec<Token>, old: &str, new: &str) {
    if tokens.is_empty() {
        return;
    }
    let mut prefix = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let most = old.len().min(new.len()) - prefix;
    let mut suffix = old.bytes().rev().zip(new.bytes().rev()).take(most).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let changed_end = old.len() - suffix;
    tokens.retain_mut(|token| {
        if token.range.start >= changed_end && token.range.start >= prefix {
            let start = token.range.start + new.len() - old.len();
            token.range = start..start + token.label.len();
        } else if token.range.end > prefix {
            return false;
        }
        new.get(token.range.clone()) == Some(token.label.as_str())
    });
}

/// A completion being typed: the sigil at `start`, and what follows it up to the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: Kind,
    pub start: usize,
    pub query: String,
}

/// The `@` or `#` the caret is completing: the nearest one on the caret's line that begins a
/// word and is not already a token.
pub fn trigger(text: &str, caret: usize, tokens: &[Token]) -> Option<Trigger> {
    let before = text.get(..caret)?;
    let line = before.rfind('\n').map_or(0, |ix| ix + 1);
    let (start, sigil) = before[line..]
        .char_indices()
        .rev()
        .map(|(ix, ch)| (line + ix, ch))
        .find(|(ix, ch)| matches!(ch, '@' | '#') && before[..*ix].chars().next_back().is_none_or(char::is_whitespace))?;
    let query = &before[start + 1..];
    let taken = tokens.iter().any(|token| token.range.start <= start && start < token.range.end);
    if taken || query.starts_with(char::is_whitespace) || query.chars().count() > QUERY {
        return None;
    }
    let kind = if sigil == '@' { Kind::Person } else { Kind::Channel };
    Some(Trigger { kind, start, query: query.to_owned() })
}

/// How well a name answers what was typed, best first: the name starts with it, the handle
/// does, a later word does, it is somewhere inside. `None`: no match.
pub fn rank(name: &str, handle: &str, query: &str) -> Option<u8> {
    let (name, handle, query) = (name.to_lowercase(), handle.trim_start_matches('@').to_lowercase(), query.to_lowercase());
    if name.starts_with(&query) {
        Some(0)
    } else if handle.starts_with(&query) {
        Some(1)
    } else if name.split_whitespace().any(|word| word.starts_with(&query)) {
        Some(2)
    } else if name.contains(&query) || handle.contains(&query) {
        Some(3)
    } else {
        None
    }
}

/// A sent message as it would have been typed, for editing it: the inline markup put back
/// around what it styles, and each mention a token again, so saving does not turn a mention
/// into its name.
// CONTRACT: a message's text has no markup left and no request returns what was typed. Bold,
// italic, strike and code are rebuilt here; code blocks, quotes, lists, links with a label
// and custom emoji come back as their plain text, so saving an edit of those flattens them.
pub fn source(message: &Message) -> (String, Vec<Token>) {
    let text = &message.text;
    let on_char = |at: u32| text.is_char_boundary(at as usize).then_some(at as usize);
    // (offset, closes, marker): at one offset, what closes goes before what opens.
    let mut marks: Vec<(usize, bool, &str)> = Vec::new();
    let mut mentions = Vec::new();
    for span in &message.spans {
        let (Some(start), Some(end)) = (on_char(span.start), on_char(span.end)) else {
            continue;
        };
        if start >= end || end > text.len() {
            continue;
        }
        let marker = match &span.style {
            Style::Bold => "**",
            Style::Italic => "_",
            Style::Strike => "~~",
            Style::Code => "`",
            Style::Mention { kind, id } => {
                let kind = match kind {
                    MentionKind::User => Kind::Person,
                    MentionKind::Channel => Kind::Channel,
                    MentionKind::Role => Kind::Role,
                    MentionKind::Everyone => continue,
                };
                mentions.push((start..end, kind, id.clone()));
                continue;
            }
            _ => continue,
        };
        marks.push((start, false, marker));
        marks.push((end, true, marker));
    }
    marks.sort_by_key(|(offset, closes, _)| (*offset, !*closes));
    let mut out = String::with_capacity(text.len() + marks.len() * 2);
    let mut at = 0;
    for (offset, _, marker) in &marks {
        out.push_str(&text[at..*offset]);
        out.push_str(marker);
        at = *offset;
    }
    out.push_str(&text[at..]);
    let moved = |offset: usize| -> usize { marks.iter().filter(|(at, ..)| *at <= offset).map(|(.., marker)| marker.len()).sum() };
    let tokens = mentions
        .into_iter()
        .map(|(range, kind, id)| {
            let start = range.start + moved(range.start);
            Token { range: start..start + range.len(), label: text[range].to_owned(), kind, id }
        })
        .filter(|token| out.get(token.range.clone()) == Some(token.label.as_str()))
        .collect();
    (out, tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Span;

    fn token(text: &str, label: &str, kind: Kind, id: &str) -> Token {
        let start = text.find(label).expect("label in text");
        Token { range: start..start + label.len(), label: label.to_owned(), kind, id: id.to_owned() }
    }

    #[test]
    fn tokens_become_what_each_network_takes() {
        let text = "ask @Mara Okafor in #platform, cc @admins";
        let tokens = [
            token(text, "@Mara Okafor", Kind::Person, "77"),
            token(text, "#platform", Kind::Channel, "5"),
            token(text, "@admins", Kind::Role, "3"),
        ];
        assert_eq!(wire(text, &tokens, Network::Discord), "ask <@77> in <#5>, cc <@&3>");
        assert_eq!(wire(text, &tokens, Network::Slack), "ask <@77> in <#5>, cc <!subteam^3>");
        assert_eq!(wire(text, &tokens, Network::Teams), text);
        assert_eq!(wire("no tokens <here>", &[], Network::Discord), "no tokens <here>");
        let text = "nice :mossparrot: :party:";
        let emoji = [token(text, ":mossparrot:", Kind::Emoji, "11"), token(text, ":party:", Kind::Emoji, "a:12")];
        assert_eq!(wire(text, &emoji, Network::Discord), "nice <:mossparrot:11> <a:party:12>");
        assert_eq!(wire(text, &emoji, Network::Slack), text);
    }

    #[test]
    fn a_token_whose_text_was_edited_is_text_again() {
        let text = "hi @Mara Okafor and @Dev";
        let stale = Token { range: 3..15, label: "@Mara Okafur".into(), kind: Kind::Person, id: "77".into() };
        let past = Token { range: 20..99, label: "@Dev".into(), kind: Kind::Person, id: "8".into() };
        assert_eq!(wire(text, &[stale, past, token(text, "@Dev", Kind::Person, "8")], Network::Discord), "hi @Mara Okafor and <@8>");
    }

    #[test]
    fn tokens_follow_the_text_around_an_edit() {
        let old = "hi @Mara Okafor, see #platform";
        let mut tokens = vec![token(old, "@Mara Okafor", Kind::Person, "77"), token(old, "#platform", Kind::Channel, "5")];
        // Typing before both moves both.
        let new = "oh hi @Mara Okafor, see #platform";
        shift(&mut tokens, old, new);
        assert_eq!(tokens.iter().map(|t| &new[t.range.clone()]).collect::<Vec<_>>(), ["@Mara Okafor", "#platform"]);
        // Typing right after a token leaves it whole; so does typing right before one.
        let after = "oh hi @Mara Okafor!, see #platform";
        shift(&mut tokens, new, after);
        let before = "oh hi @Mara Okafor!, see é#platform";
        shift(&mut tokens, after, before);
        assert_eq!(tokens.iter().map(|t| &before[t.range.clone()]).collect::<Vec<_>>(), ["@Mara Okafor", "#platform"]);
        // A Backspace inside the first makes it text; the second still moves.
        let cut = "oh hi @Mara Okaor!, see é#platform";
        shift(&mut tokens, before, cut);
        assert_eq!(tokens.len(), 1);
        assert_eq!(&cut[tokens[0].range.clone()], "#platform");
        assert_eq!(wire(cut, &tokens, Network::Discord), "oh hi @Mara Okaor!, see é<#5>");
        // Selecting everything and typing leaves none.
        shift(&mut tokens, cut, "x");
        assert!(tokens.is_empty());
    }

    #[test]
    fn a_repeated_letter_does_not_hide_an_edit_inside_a_token() {
        let old = "@aaa";
        let mut tokens = vec![token(old, "@aaa", Kind::Person, "1")];
        shift(&mut tokens, old, "@aa");
        assert!(tokens.is_empty());
    }

    #[test]
    fn a_sigil_opens_a_completion_only_at_a_word_start() {
        let at = |text: &str| trigger(text, text.len(), &[]);
        assert_eq!(at("@"), Some(Trigger { kind: Kind::Person, start: 0, query: String::new() }));
        assert_eq!(at("look, @de"), Some(Trigger { kind: Kind::Person, start: 6, query: "de".into() }));
        assert_eq!(at("see #pla"), Some(Trigger { kind: Kind::Channel, start: 4, query: "pla".into() }));
        assert_eq!(at("line one\n@mara ok").map(|found| found.query), Some("mara ok".into()));
        assert_eq!(at("mail mara@example.com"), None);
        assert_eq!(at("issue#12"), None);
        assert_eq!(at("at @ noon"), None);
        assert_eq!(at("@mara\nnext line"), None);
        assert_eq!(at(&format!("@{}", "x".repeat(QUERY + 1))), None);
        // The caret decides, not the end of the text.
        assert_eq!(trigger("@de and more", 3, &[]).map(|found| found.query), Some("de".into()));
        // A completed mention does not open the list again.
        let text = "@Mara Okafor ok";
        assert_eq!(trigger(text, text.len(), &[token(text, "@Mara Okafor", Kind::Person, "77")]), None);
    }

    #[test]
    fn names_rank_by_where_the_typed_letters_fall() {
        assert_eq!(rank("Dev Patel", "devp", "de"), Some(0));
        assert_eq!(rank("Nadia Devlin", "ndevlin", "nd"), Some(1));
        assert_eq!(rank("Nadia Devlin", "nadia", "dev"), Some(2));
        assert_eq!(rank("Ingrid Vandeveer", "iv", "dev"), Some(3));
        assert_eq!(rank("Mara Okafor", "@mara", "dev"), None);
        assert_eq!(rank("Mara Okafor", "", ""), Some(0));
        assert_eq!(rank("Mara Okafor", "", "MARA O"), Some(0));
    }

    #[test]
    fn a_sent_message_is_edited_as_it_would_have_been_typed() {
        let mut message = Message::plain("1", "me", "hold 20 minutes, @Dev owns the switch in #ops", 0, true);
        let span = |part: &str, style: Style| {
            let start = message.text.find(part).expect("part") as u32;
            Span { start, end: start + part.len() as u32, style }
        };
        message.spans = vec![
            span("hold 20 minutes", Style::Bold),
            span("20", Style::Italic),
            span("@Dev", Style::Mention { kind: MentionKind::User, id: "8".into() }),
            span("the switch", Style::Code),
            span("#ops", Style::Mention { kind: MentionKind::Channel, id: "5".into() }),
        ];
        let (text, tokens) = source(&message);
        assert_eq!(text, "**hold _20_ minutes**, @Dev owns `the switch` in #ops");
        assert_eq!(wire(&text, &tokens, Network::Discord), "**hold _20_ minutes**, <@8> owns `the switch` in <#5>");

        // A mention that is itself bold stays inside the markers.
        let mut bold = Message::plain("2", "me", "cc @Dev", 0, true);
        bold.spans = vec![
            Span { start: 3, end: 7, style: Style::Bold },
            Span { start: 3, end: 7, style: Style::Mention { kind: MentionKind::User, id: "8".into() } },
            // A range off a character boundary or past the end is ignored, not a panic.
            Span { start: 2, end: 99, style: Style::Bold },
        ];
        let (text, tokens) = source(&bold);
        assert_eq!(text, "cc **@Dev**");
        assert_eq!(wire(&text, &tokens, Network::Discord), "cc **<@8>**");
    }
}
