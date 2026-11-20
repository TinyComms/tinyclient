//! Signal body ranges and the composer's markdown, as the message model draws them.
//!
//! Ranges from Signal are UTF-16 code units on the original body, where a mention is one
//! U+FFFC. Substituting the name moves every later offset, so styles are applied on the way
//! through that substitution. The composer types `**bold**`, `_italic_`, `~~strike~~`,
//! `` `code` `` and `||spoiler||`; those become ranges on the plain text that is sent.

use protocol::{MentionKind, Span, Style};

/// A style Signal's `BodyRange.Style` can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Strike,
    Monospace,
    Spoiler,
}

impl Mark {
    fn drawn(self) -> Style {
        match self {
            Mark::Bold => Style::Bold,
            Mark::Italic => Style::Italic,
            Mark::Strike => Style::Strike,
            Mark::Monospace => Style::Code,
            Mark::Spoiler => Style::Spoiler,
        }
    }

    fn of(style: &Style) -> Option<Mark> {
        match style {
            Style::Bold => Some(Mark::Bold),
            Style::Italic => Some(Mark::Italic),
            Style::Strike => Some(Mark::Strike),
            Style::Code => Some(Mark::Monospace),
            Style::Spoiler => Some(Mark::Spoiler),
            _ => None,
        }
    }
}

/// One range on a body. `start` and `length` are UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    Style {
        start: u32,
        length: u32,
        style: Mark,
    },
    /// A mention. `label` is what replaces the U+FFFC (`@Grace`), already resolved.
    Mention {
        start: u32,
        aci: String,
        label: String,
    },
}

/// Words after mention substitution, and the spans that draw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Styled {
    pub text: String,
    pub spans: Vec<Span>,
    pub mentions_me: bool,
}

/// Applies `ranges` to `body`. Mention placeholders are replaced before style offsets are read,
/// and a range that falls off the text is dropped. A style that starts inside a surrogate pair
/// is rounded down onto that character.
pub fn apply(body: &str, ranges: &[Range], me: &str) -> Styled {
    let mut mentions: Vec<&Range> = ranges
        .iter()
        .filter(|range| matches!(range, Range::Mention { .. }))
        .collect();
    mentions.sort_by_key(|range| match range {
        Range::Mention { start, .. } => *start,
        Range::Style { .. } => 0,
    });
    let mut next_mention = 0;
    let mut text = String::with_capacity(body.len());
    // `map[utf16]` is the byte offset in `text` of that original code unit. Both halves of a
    // surrogate point at the character. The last entry is the end of `text`.
    let mut map: Vec<u32> = Vec::new();
    let mut mentions_me = false;
    let mut mention_spans: Vec<(u32, u32, String)> = Vec::new();
    for ch in body.chars() {
        let at = text.len() as u32;
        if ch == '\u{FFFC}' {
            let (label, aci) = match mentions.get(next_mention) {
                Some(Range::Mention { aci, label, .. }) => {
                    next_mention += 1;
                    mentions_me |= aci == me;
                    let label = if label.is_empty() {
                        "@Signal user"
                    } else {
                        label.as_str()
                    };
                    (label, aci.as_str())
                }
                _ => ("@Signal user", ""),
            };
            text.push_str(label);
            map.push(at);
            if !aci.is_empty() {
                mention_spans.push((at, text.len() as u32, aci.to_owned()));
            }
        } else {
            text.push(ch);
            for _ in 0..ch.len_utf16() {
                map.push(at);
            }
        }
    }
    map.push(text.len() as u32);
    let mut spans: Vec<Span> = mention_spans
        .into_iter()
        .map(|(start, end, aci)| Span {
            start,
            end,
            style: Style::Mention {
                kind: MentionKind::User,
                id: aci,
            },
        })
        .collect();
    for range in ranges {
        let Range::Style {
            start,
            length,
            style,
        } = range
        else {
            continue;
        };
        let Some(end_at) = start.checked_add(*length) else {
            continue;
        };
        if *length == 0 || (*start as usize) >= map.len() || (end_at as usize) >= map.len() {
            continue;
        }
        let (byte_start, byte_end) = (map[*start as usize], map[end_at as usize]);
        if byte_start >= byte_end || (byte_end as usize) > text.len() {
            continue;
        }
        if !text.is_char_boundary(byte_start as usize) || !text.is_char_boundary(byte_end as usize)
        {
            continue;
        }
        spans.push(Span {
            start: byte_start,
            end: byte_end,
            style: style.drawn(),
        });
    }
    Styled {
        text,
        spans,
        mentions_me,
    }
}

/// The composer's markdown as plain text and UTF-16 style ranges on that plain text.
pub fn parse(source: &str) -> (String, Vec<Range>) {
    let mut built = Built {
        text: String::with_capacity(source.len()),
        ranges: Vec::new(),
    };
    inline(&mut built, source);
    (built.text, built.ranges)
}

/// Byte spans for `ranges` on `text`, which has no mention placeholders left.
pub fn spans(text: &str, ranges: &[Range]) -> Vec<Span> {
    apply(text, ranges, "").spans
}

/// UTF-16 style ranges for spans on `text`, for a quote or an edit sent back to Signal.
pub fn marks(text: &str, spans: &[Span]) -> Vec<(u32, u32, Mark)> {
    let mut out = Vec::new();
    for span in spans {
        let Some(mark) = Mark::of(&span.style) else {
            continue;
        };
        let (Ok(start), Ok(end)) = (usize::try_from(span.start), usize::try_from(span.end)) else {
            continue;
        };
        if start >= end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        let from = utf16_len(&text[..start]);
        let length = utf16_len(&text[start..end]);
        if length > 0 {
            out.push((from, length, mark));
        }
    }
    out
}

struct Built {
    text: String,
    ranges: Vec<Range>,
}

const PAIRS: [(&str, Mark); 3] = [
    ("**", Mark::Bold),
    ("~~", Mark::Strike),
    ("||", Mark::Spoiler),
];

fn inline(built: &mut Built, source: &str) {
    let mut at = 0;
    while at < source.len() {
        let rest = &source[at..];
        if let Some(after) = rest.strip_prefix('\\') {
            if let Some(escaped) = after.chars().next().filter(char::is_ascii_punctuation) {
                built.text.push(escaped);
                at += 1 + escaped.len_utf8();
                continue;
            }
        }
        if let Some((delimiter, mark)) = PAIRS
            .iter()
            .find(|(delimiter, _)| rest.starts_with(delimiter))
        {
            if let Some(close) = closing(rest, delimiter.len(), delimiter) {
                let inner = &rest[delimiter.len()..close];
                if fits(inner) {
                    let start = utf16_len(&built.text);
                    inline(built, inner);
                    let end = utf16_len(&built.text);
                    if end > start {
                        built.ranges.push(Range::Style {
                            start,
                            length: end - start,
                            style: *mark,
                        });
                    }
                    at += close + delimiter.len();
                    continue;
                }
            }
        }
        let first = rest.chars().next().unwrap_or(' ');
        if first == '`' {
            if let Some(close) = closing(rest, 1, "`") {
                let inner = &rest[1..close];
                if !inner.is_empty() {
                    let start = utf16_len(&built.text);
                    built.text.push_str(inner);
                    let end = utf16_len(&built.text);
                    built.ranges.push(Range::Style {
                        start,
                        length: end - start,
                        style: Mark::Monospace,
                    });
                    at += close + 1;
                    continue;
                }
            }
        }
        if matches!(first, '*' | '_') {
            let after_word = source[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let delimiter = if first == '*' { "*" } else { "_" };
            if !(first == '_' && after_word) {
                if let Some(close) = closing(rest, 1, delimiter) {
                    let inner = &rest[1..close];
                    let word_follows = rest[close + 1..]
                        .chars()
                        .next()
                        .is_some_and(char::is_alphanumeric);
                    if fits(inner) && !(first == '_' && word_follows) {
                        let start = utf16_len(&built.text);
                        inline(built, inner);
                        let end = utf16_len(&built.text);
                        if end > start {
                            built.ranges.push(Range::Style {
                                start,
                                length: end - start,
                                style: Mark::Italic,
                            });
                        }
                        at += close + 1;
                        continue;
                    }
                }
            }
        }
        built.text.push(first);
        at += first.len_utf8();
    }
}

/// A marker with nothing in it, or with space against the marker, is not a style.
fn fits(inner: &str) -> bool {
    !inner.is_empty()
        && !inner.starts_with(char::is_whitespace)
        && !inner.ends_with(char::is_whitespace)
}

fn closing(text: &str, open: usize, delimiter: &str) -> Option<usize> {
    let mut at = open;
    while at < text.len() {
        if text[at..].starts_with('\\') {
            let next = text[at + 1..].chars().next()?;
            at += 1 + next.len_utf8();
            continue;
        }
        if text[at..].starts_with(delimiter) {
            return Some(at);
        }
        at += text[at..].chars().next()?.len_utf8();
    }
    None
}

fn utf16_len(text: &str) -> u32 {
    text.chars().map(|ch| ch.len_utf16() as u32).sum()
}

/// What the lab runs. Styles, a mention and an emoji survive together, a spoiler stays out of
/// a notification preview, and markdown becomes ranges rather than asterisks.
pub fn prove() -> Result<(), String> {
    let me = "a1a1a1a1-0000-4000-8000-000000000001";
    let grace = "b2b2b2b2-0000-4000-8000-000000000002";
    // "Hi \u{FFFC}, \u{1F44D} secret" — the thumbs-up is two UTF-16 units.
    let body = "Hi \u{FFFC}, \u{1F44D} secret";
    let ranges = vec![
        Range::Mention {
            start: 3,
            aci: grace.into(),
            label: "@Grace".into(),
        },
        Range::Style {
            start: 4,
            length: 11,
            style: Mark::Bold,
        },
        Range::Style {
            start: 9,
            length: 6,
            style: Mark::Spoiler,
        },
    ];
    let styled = apply(body, &ranges, me);
    if styled.text != "Hi @Grace, \u{1F44D} secret" {
        return Err(format!("mention substitution, got {:?}", styled.text));
    }
    let covers = |style: &Style, words: &str| {
        styled.spans.iter().any(|span| {
            &span.style == style
                && styled.text.get(span.start as usize..span.end as usize) == Some(words)
        })
    };
    if !covers(&Style::Bold, ", \u{1F44D} secret") {
        return Err(format!(
            "bold should cover the comma through the spoiler, spans {:?}",
            styled.spans
        ));
    }
    if !covers(&Style::Spoiler, "secret") {
        return Err(format!(
            "spoiler should cover secret, spans {:?}",
            styled.spans
        ));
    }
    if !styled.spans.iter().any(|span| {
        matches!(&span.style, Style::Mention { kind: MentionKind::User, id } if id == grace)
            && styled.text.get(span.start as usize..span.end as usize) == Some("@Grace")
    }) {
        return Err("the mention was not a span".into());
    }
    let (plain, sent) = parse("**bold** ||secret|| \u{1F44D}");
    if plain != "bold secret \u{1F44D}" {
        return Err(format!("markdown left marks in {plain:?}"));
    }
    let drawn = spans(&plain, &sent);
    let has = |style: Style, words: &str| {
        drawn.iter().any(|span| {
            span.style == style && plain.get(span.start as usize..span.end as usize) == Some(words)
        })
    };
    if !has(Style::Bold, "bold") || !has(Style::Spoiler, "secret") {
        return Err(format!("sent ranges did not draw, {sent:?}"));
    }
    let back = marks(&plain, &drawn);
    if !back.contains(&(0, 4, Mark::Bold)) || !back.contains(&(5, 6, Mark::Spoiler)) {
        return Err(format!("ranges to send were {back:?}"));
    }
    let mut message = protocol::Message::plain("1", "Grace", plain, 0, false);
    message.spans = drawn;
    let preview = crate::store::preview_text(&message);
    if preview.contains("secret") {
        return Err(format!(
            "notification preview revealed the spoiler: {preview}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_overlap_a_mention_and_an_emoji_and_a_spoiler_stays_hidden() {
        prove().unwrap();
    }

    #[test]
    fn markdown_marks_do_not_style_the_middle_of_a_word() {
        let (plain, ranges) = parse("a_b_c and *italic*");
        assert_eq!(plain, "a_b_c and italic");
        let drawn = spans(&plain, &ranges);
        assert!(drawn.iter().any(|span| span.style == Style::Italic
            && plain.get(span.start as usize..span.end as usize) == Some("italic")));
        assert!(!drawn
            .iter()
            .any(|span| plain.get(span.start as usize..span.end as usize) == Some("b")));
    }
}
