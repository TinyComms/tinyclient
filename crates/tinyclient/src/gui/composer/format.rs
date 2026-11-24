//! Discord draft formatting. The markers stay in the text, so the caret's byte offsets do not
//! move when the field paints them. Shortcuts and the selection bar share [`apply`]; a pasted
//! http(s) address over a selection becomes a masked link ([`mask_link`]).
//!
//! What is bold, code, or a spoiler follows the same delimiter rules as the Discord adapter's
//! inline reader. Markers are recorded beside the words they wrap, and a later pass flattens
//! the overlaps into the ranges the field paints.

use std::ops::Range;

/// One formatting action. Italic inserts `*`. A typed `_word_` still counts as italic when the
/// caret is inside it, so the same shortcut can take it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Kind {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Spoiler,
    Quote,
}

/// How a slice of the draft is drawn. A marker slice is only dimmed. The other flags stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) struct Look {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub spoiler: bool,
    pub link: bool,
    pub quote: bool,
    pub marker: bool,
}

impl Look {
    fn plain(range: Range<usize>) -> Look {
        Look {
            range,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            code: false,
            spoiler: false,
            link: false,
            quote: false,
            marker: false,
        }
    }

    fn styled(&self) -> bool {
        self.bold
            || self.italic
            || self.underline
            || self.strike
            || self.code
            || self.spoiler
            || self.link
            || self.quote
            || self.marker
    }
}

/// The marker each shortcut inserts. Kept next to the mention rebuilder, which only knows the
/// four styles a received message can hand back.
pub(in crate::gui) fn discord_markers() -> &'static [(&'static str, &'static str)] {
    &[
        ("bold", "**"),
        ("italic", "*"),
        ("underline", "__"),
        ("strike", "~~"),
        ("code", "`"),
        ("spoiler", "||"),
        ("quote", "> "),
    ]
}

/// A hard line that itself opens a quote. `>>> ` quotes the rest of the draft too;
/// [`line_quoted`] is that wider check.
pub(in crate::gui) fn quote_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("> ") || trimmed.starts_with(">>> ")
}

/// Whether the hard line that begins at `at` is quoted, including the lines under a `>>> `.
pub(in crate::gui) fn line_quoted(text: &str, at: usize) -> bool {
    let mut quoting = false;
    let mut start = 0usize;
    for line in text.split('\n') {
        if at == start {
            return quoting || quote_line(line);
        }
        if !quoting && line.trim_start().starts_with(">>> ") {
            quoting = true;
        }
        start += line.len() + 1;
        if start > text.len() {
            break;
        }
    }
    false
}

/// Wraps `selection`, or takes the same wrapping off. The returned range is what should stay
/// selected: the inside of a new pair, or the words after a pair comes off.
pub(in crate::gui) fn apply(
    text: &str,
    selection: Range<usize>,
    kind: Kind,
) -> (String, Range<usize>) {
    apply_with(text, selection, kind, scan_inline, wrap_marker)
}

fn apply_with(
    text: &str,
    selection: Range<usize>,
    kind: Kind,
    inline: Inline,
    mark: fn(Kind) -> &'static str,
) -> (String, Range<usize>) {
    let sel = snap(text, selection);
    if kind == Kind::Quote {
        return apply_quote(text, sel);
    }
    let pairs = analyze(text, inline).1;
    if let Some(pair) = covering(&pairs, kind, &sel) {
        return unwrap(text, &pair, &sel);
    }
    if kind == Kind::Code && inside_fence(&pairs, &sel) {
        return (text.to_owned(), sel);
    }
    // An empty `**|**` is bold, not a place to drop a single star. The same for `__|__`.
    if sel.is_empty()
        && pairs
            .iter()
            .any(|pair| pair.kind != kind && pair.inner.is_empty() && pair.open.end == sel.start)
    {
        return (text.to_owned(), sel);
    }
    wrap(text, &sel, kind, mark)
}

/// `[label](url)` when `url` is an http(s) address and the selection is not empty.
pub(in crate::gui) fn mask_link(
    text: &str,
    selection: Range<usize>,
    url: &str,
) -> Option<(String, Range<usize>)> {
    let sel = snap(text, selection);
    if sel.is_empty() {
        return None;
    }
    let url = url.trim();
    if url.is_empty() || url.contains(char::is_whitespace) || url.contains(')') {
        return None;
    }
    let https = url.starts_with("https://") && url.len() > "https://".len();
    let http = url.starts_with("http://") && url.len() > "http://".len();
    if !https && !http {
        return None;
    }
    let label = &text[sel.clone()];
    let wrapped = format!("[{label}]({url})");
    let mut next = String::with_capacity(text.len() - label.len() + wrapped.len());
    next.push_str(&text[..sel.start]);
    next.push_str(&wrapped);
    next.push_str(&text[sel.end..]);
    let start = sel.start + 1;
    Some((next, start..start + label.len()))
}

/// Non-overlapping styled slices of `text`. Plain words are left out.
pub(in crate::gui) fn paint(text: &str) -> Vec<Look> {
    let (hits, _) = analyze(text, scan_inline);
    flatten(&hits, text.len())
}

/// The lab driver. A build without these checks never reaches it, so the window stays open
/// and the scenario fails.
pub(in crate::gui) fn prove() -> Result<(), &'static str> {
    let markers = discord_markers();
    if markers
        .iter()
        .find(|(name, _)| *name == "bold")
        .map(|(_, mark)| *mark)
        != Some("**")
    {
        return Err("bold is not wrapped in **");
    }
    if markers
        .iter()
        .find(|(name, _)| *name == "spoiler")
        .map(|(_, mark)| *mark)
        != Some("||")
    {
        return Err("a spoiler is not wrapped in ||");
    }
    let (bold, inner) = apply("hi", 0..2, Kind::Bold);
    if bold != "**hi**" || inner != (2..4) {
        return Err("bold did not wrap the selection");
    }
    let (back, _) = apply(&bold, inner, Kind::Bold);
    if back != "hi" {
        return Err("bold did not come back off");
    }
    let (fenced, code) = apply("alpha\nbeta", 0..10, Kind::Code);
    if fenced != "```\nalpha\nbeta\n```" || code != (4..14) {
        return Err("a newline did not become a code fence");
    }
    if apply(&fenced, code, Kind::Code).0 != "alpha\nbeta" {
        return Err("the fence did not come back off");
    }
    let looks = paint("**hi** `x` ||s||");
    if !looks.iter().any(|look| look.bold && !look.marker) {
        return Err("bold words are not bold");
    }
    if !looks.iter().any(|look| look.code && !look.marker) {
        return Err("inline code is not code");
    }
    if !looks.iter().any(|look| look.spoiler && !look.marker) {
        return Err("a spoiler is not marked");
    }
    if !looks.iter().any(|look| look.marker) {
        return Err("the markers are not kept to be dimmed");
    }
    match mask_link("hello", 0..5, "https://example.com") {
        Some((linked, _)) if linked == "[hello](https://example.com)" => {}
        _ => return Err("a url pasted over a selection did not become [text](url)"),
    }
    if mask_link("hello", 0..0, "https://example.com").is_some() {
        return Err("an empty selection became a link");
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct Pair {
    kind: Kind,
    fence: bool,
    open: Range<usize>,
    close: Range<usize>,
    inner: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
enum Style {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Spoiler,
    Link,
    Quote,
    Marker,
}

struct Hit {
    range: Range<usize>,
    style: Style,
}

fn snap(text: &str, range: Range<usize>) -> Range<usize> {
    let end = boundary(text, range.end);
    let start = boundary(text, range.start.min(end));
    start..end
}

fn boundary(text: &str, mut at: usize) -> usize {
    if at > text.len() {
        at = text.len();
    }
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn wrap_marker(kind: Kind) -> &'static str {
    match kind {
        Kind::Bold => "**",
        Kind::Italic => "*",
        Kind::Underline => "__",
        Kind::Strike => "~~",
        Kind::Code => "`",
        Kind::Spoiler => "||",
        Kind::Quote => "> ",
    }
}

fn wrap(
    text: &str,
    sel: &Range<usize>,
    kind: Kind,
    mark: fn(Kind) -> &'static str,
) -> (String, Range<usize>) {
    let (open, close) = if kind == Kind::Code && text[sel.clone()].contains('\n') {
        ("```\n", "\n```")
    } else {
        let mark = mark(kind);
        (mark, mark)
    };
    let inner = &text[sel.clone()];
    let mut next = String::with_capacity(text.len() + open.len() + close.len());
    next.push_str(&text[..sel.start]);
    next.push_str(open);
    next.push_str(inner);
    next.push_str(close);
    next.push_str(&text[sel.end..]);
    let start = sel.start + open.len();
    (next, start..start + inner.len())
}

fn unwrap(text: &str, pair: &Pair, sel: &Range<usize>) -> (String, Range<usize>) {
    let mut next = String::with_capacity(text.len());
    next.push_str(&text[..pair.open.start]);
    next.push_str(&text[pair.open.end..pair.close.start]);
    next.push_str(&text[pair.close.end..]);
    let map = |point: usize| -> usize {
        let open_len = pair.open.end - pair.open.start;
        let close_len = pair.close.end - pair.close.start;
        if point >= pair.close.end {
            point - open_len - close_len
        } else if point >= pair.close.start {
            pair.open.start + (pair.close.start - pair.open.end)
        } else if point >= pair.open.end {
            pair.open.start + (point - pair.open.end)
        } else if point > pair.open.start {
            pair.open.start
        } else {
            point
        }
    };
    let start = map(sel.start);
    let end = map(sel.end);
    (next, start.min(end)..start.max(end))
}

fn covering(pairs: &[Pair], kind: Kind, sel: &Range<usize>) -> Option<Pair> {
    let mut best: Option<Pair> = None;
    for pair in pairs.iter().filter(|pair| pair.kind == kind) {
        let hit = if sel.is_empty() {
            pair.open.start < sel.start && sel.start < pair.close.end
        } else {
            pair.inner == *sel || (pair.open.start..pair.close.end) == *sel
        };
        if !hit {
            continue;
        }
        let span = pair.close.end - pair.open.start;
        let tighter = best
            .as_ref()
            .is_none_or(|kept| span < kept.close.end - kept.open.start);
        if tighter {
            best = Some(pair.clone());
        }
    }
    best
}

fn inside_fence(pairs: &[Pair], sel: &Range<usize>) -> bool {
    pairs
        .iter()
        .any(|pair| pair.fence && sel.start >= pair.open.start && sel.end <= pair.close.end)
}

fn apply_quote(text: &str, sel: Range<usize>) -> (String, Range<usize>) {
    let span_start = text[..sel.start].rfind('\n').map_or(0, |at| at + 1);
    let probe = if sel.is_empty() {
        sel.start
    } else {
        sel.end.saturating_sub(1).max(sel.start)
    };
    let span_end = if probe >= text.len() {
        text.len()
    } else {
        text[probe..].find('\n').map_or(text.len(), |at| probe + at)
    };
    let block = &text[span_start..span_end];
    let lines: Vec<&str> = if block.is_empty() && span_start == span_end {
        vec![""]
    } else {
        block.split('\n').collect()
    };
    let quoted = |line: &str| line.starts_with("> ") || line.starts_with(">>> ");
    let strip = !lines.is_empty() && lines.iter().all(|line| quoted(line));
    let mut next_block = String::new();
    let mut at_new = vec![0usize; block.len() + 1];
    let mut old_i = 0usize;
    let mut inserted_at = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let have = if line.starts_with(">>> ") {
            4
        } else if line.starts_with("> ") {
            2
        } else {
            0
        };
        let (add, drop) = if strip {
            (0, have)
        } else if have > 0 {
            (0, 0)
        } else {
            (2, 0)
        };
        if add == 2 {
            next_block.push_str("> ");
            inserted_at.push(old_i);
        }
        let new_start = next_block.len();
        for byte in 0..line.len() {
            at_new[old_i + byte] = if byte < drop {
                new_start
            } else {
                new_start + byte - drop
            };
        }
        next_block.push_str(&line[drop..]);
        at_new[old_i + line.len()] = next_block.len();
        old_i += line.len();
        if index + 1 < lines.len() {
            at_new[old_i] = next_block.len();
            next_block.push('\n');
            old_i += 1;
        }
    }
    if at_new.len() == block.len() + 1 && old_i == block.len() {
        at_new[block.len()] = next_block.len();
    }
    let delta = next_block.len() as isize - block.len() as isize;
    let map = |point: usize| -> usize {
        if point < span_start {
            return point;
        }
        if point > span_end {
            return (point as isize + delta) as usize;
        }
        let rel = point - span_start;
        let mut mapped = span_start + at_new.get(rel).copied().unwrap_or(next_block.len());
        let column = rel == 0 || block.as_bytes().get(rel.wrapping_sub(1)) == Some(&b'\n');
        if point == sel.start
            && !sel.is_empty()
            && column
            && inserted_at.contains(&rel)
            && mapped >= span_start + 2
        {
            mapped -= 2;
        }
        mapped
    };
    let mut next = String::with_capacity(text.len() + 8);
    next.push_str(&text[..span_start]);
    next.push_str(&next_block);
    next.push_str(&text[span_end..]);
    let start = map(sel.start).min(next.len());
    let end = map(sel.end).min(next.len());
    (next, start..end)
}

/// Reads the markers inside one stretch of a line. Discord and Slack spell them differently.
type Inline = fn(&str, usize, usize, &mut Vec<Hit>, &mut Vec<Pair>);

fn analyze(text: &str, inline: Inline) -> (Vec<Hit>, Vec<Pair>) {
    let mut hits = Vec::new();
    let mut pairs = Vec::new();
    let fences = fence_pairs(text);
    for fence in &fences {
        pairs.push(fence.clone());
        hits.push(Hit {
            range: fence.open.clone(),
            style: Style::Marker,
        });
        hits.push(Hit {
            range: fence.close.clone(),
            style: Style::Marker,
        });
        if fence.inner.start < fence.inner.end {
            hits.push(Hit {
                range: fence.inner.clone(),
                style: Style::Code,
            });
        }
    }
    let mut at = 0usize;
    let mut quoting = false;
    while at < text.len() {
        if let Some(fence) = fences
            .iter()
            .find(|fence| at >= fence.open.start && at < fence.close.end)
        {
            at = fence.close.end;
            quoting = false;
            continue;
        }
        let fence_at = fences
            .iter()
            .map(|fence| fence.open.start)
            .filter(|start| *start > at)
            .min();
        let newline = text[at..].find('\n').map(|index| at + index);
        let end = match (newline, fence_at) {
            (Some(line), Some(fence)) => line.min(fence),
            (Some(line), None) => line,
            (None, Some(fence)) => fence,
            (None, None) => text.len(),
        };
        if end == at {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let line = &text[at..end];
        let whole =
            newline == Some(end) || (fence_at.is_none() && end == text.len()) || newline.is_none();
        if whole {
            scan_line(text, at, end, quoting, inline, &mut hits, &mut pairs);
            if !quoting && line.trim_start().starts_with(">>> ") {
                quoting = true;
            }
        } else {
            inline(text, at, end, &mut hits, &mut pairs);
        }
        at = if newline == Some(end) { end + 1 } else { end };
    }
    (hits, pairs)
}

fn scan_line(
    text: &str,
    start: usize,
    end: usize,
    quoting: bool,
    inline: Inline,
    hits: &mut Vec<Hit>,
    pairs: &mut Vec<Pair>,
) {
    let line = &text[start..end];
    let indent = line.len() - line.trim_start().len();
    let trimmed = &line[indent..];
    let marker = if !quoting && trimmed.starts_with(">>> ") {
        Some(indent..indent + 4)
    } else if !quoting && trimmed.starts_with("> ") {
        Some(indent..indent + 2)
    } else {
        None
    };
    let marked = marker.map(|range| (start + range.start)..(start + range.end));
    let body = marked.as_ref().map_or(start, |range| range.end);
    if let Some(range) = marked {
        hits.push(Hit {
            range,
            style: Style::Marker,
        });
    }
    if (quoting || body != start) && body < end {
        hits.push(Hit {
            range: body..end,
            style: Style::Quote,
        });
    } else if quoting && body == end {
        // A blank line under `>>> ` is still part of the quote. Nothing to paint.
    }
    inline(text, body, end, hits, pairs);
}

fn scan_inline(text: &str, from: usize, to: usize, hits: &mut Vec<Hit>, pairs: &mut Vec<Pair>) {
    const PAIRS: [(&str, Kind); 5] = [
        ("**", Kind::Bold),
        ("__", Kind::Underline),
        ("~~", Kind::Strike),
        ("||", Kind::Spoiler),
        ("``", Kind::Code),
    ];
    let mut at = from;
    while at < to {
        let rest = &text[at..to];
        let Some(first) = rest.chars().next() else {
            break;
        };
        let after_word = text[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        if first == '\\' {
            if let Some(escaped) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                hits.push(Hit {
                    range: at..at + 1,
                    style: Style::Marker,
                });
                at += 1 + escaped.len_utf8();
                continue;
            }
        }
        let mut matched = false;
        for (delimiter, kind) in PAIRS {
            if !rest.starts_with(delimiter) {
                continue;
            }
            if let Some(close) = closing(text, at + delimiter.len(), to, delimiter) {
                push_pair(text, at, close, delimiter.len(), kind, hits, pairs);
                at = close + delimiter.len();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if first == '`' {
            if let Some(close) = closing(text, at + 1, to, "`") {
                push_pair(text, at, close, 1, Kind::Code, hits, pairs);
                at = close + 1;
                continue;
            }
        }
        if (first == '*' || first == '_') && !(first == '_' && after_word) {
            let delimiter = if first == '*' { "*" } else { "_" };
            let close = closing(text, at + 1, to, delimiter).filter(|close| {
                let inner = &text[at + 1..*close];
                let follows = text
                    .get(close + 1..to)
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(char::is_alphanumeric);
                !inner.starts_with(char::is_whitespace)
                    && !inner.ends_with(char::is_whitespace)
                    && !(first == '_' && follows)
            });
            if let Some(close) = close {
                push_pair(text, at, close, 1, Kind::Italic, hits, pairs);
                at = close + 1;
                continue;
            }
        }
        if first == '[' {
            if let Some((label_end, taken)) = masked(text, at, to) {
                hits.push(Hit {
                    range: at..at + 1,
                    style: Style::Marker,
                });
                hits.push(Hit {
                    range: label_end..taken,
                    style: Style::Marker,
                });
                if at + 1 < label_end {
                    hits.push(Hit {
                        range: at + 1..label_end,
                        style: Style::Link,
                    });
                    scan_inline(text, at + 1, label_end, hits, pairs);
                }
                at = taken;
                continue;
            }
        }
        if first == 'h'
            && !after_word
            && (rest.starts_with("https://") || rest.starts_with("http://"))
        {
            let end = bare_link(rest);
            if end > "https://".len() {
                hits.push(Hit {
                    range: at..at + end,
                    style: Style::Link,
                });
                at += end;
                continue;
            }
        }
        at += first.len_utf8();
    }
}

fn style_for(kind: Kind) -> Style {
    match kind {
        Kind::Bold => Style::Bold,
        Kind::Italic => Style::Italic,
        Kind::Underline => Style::Underline,
        Kind::Strike => Style::Strike,
        Kind::Code => Style::Code,
        Kind::Spoiler => Style::Spoiler,
        Kind::Quote => Style::Quote,
    }
}

fn push_pair(
    text: &str,
    open_at: usize,
    close_at: usize,
    len: usize,
    kind: Kind,
    hits: &mut Vec<Hit>,
    pairs: &mut Vec<Pair>,
) {
    let style = style_for(kind);
    let open = open_at..open_at + len;
    let close = close_at..close_at + len;
    let inner = open.end..close.start;
    pairs.push(Pair {
        kind,
        fence: false,
        open: open.clone(),
        close: close.clone(),
        inner: inner.clone(),
    });
    hits.push(Hit {
        range: open,
        style: Style::Marker,
    });
    hits.push(Hit {
        range: close,
        style: Style::Marker,
    });
    if inner.start < inner.end {
        hits.push(Hit {
            range: inner.clone(),
            style,
        });
        if kind != Kind::Code {
            scan_inline(text, inner.start, inner.end, hits, pairs);
        }
    }
}

fn closing(text: &str, from: usize, end: usize, delimiter: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < end {
        if bytes.get(at) == Some(&b'\\') {
            at += 1;
            if at >= end {
                break;
            }
            at += text[at..end].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if text[at..end].starts_with(delimiter) {
            return Some(at);
        }
        at += text[at..end].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn masked(text: &str, at: usize, end: usize) -> Option<(usize, usize)> {
    let label_end = closing(text, at + 1, end, "](")?;
    if label_end <= at + 1 {
        return None;
    }
    let target_from = label_end + 2;
    if target_from > end {
        return None;
    }
    let rel = text[target_from..end].find(')')?;
    let raw = &text[target_from..target_from + rel];
    let url = raw.trim_matches(|ch| ch == '<' || ch == '>');
    let url = url.split_whitespace().next()?;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    Some((label_end, target_from + rel + 1))
}

fn bare_link(text: &str) -> usize {
    let mut end = text
        .find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '|'))
        .unwrap_or(text.len());
    while let Some(last) = text[..end].chars().next_back() {
        let unmatched = last == ')' && !text[..end].contains('(');
        if matches!(
            last,
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_' | '~'
        ) || unmatched
        {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn fence_pairs(text: &str) -> Vec<Pair> {
    let mut pairs = Vec::new();
    let mut from = 0usize;
    while let Some(open_at) = find_token(text, from, "```") {
        let after = open_at + 3;
        let Some(close_at) = find_token(text, after, "```") else {
            break;
        };
        let open_end = match text[after..close_at].find('\n') {
            Some(index) => after + index + 1,
            None => after,
        };
        let close_start =
            if close_at > open_end && text.as_bytes().get(close_at - 1) == Some(&b'\n') {
                close_at - 1
            } else {
                close_at
            };
        if open_end <= close_start {
            pairs.push(Pair {
                kind: Kind::Code,
                fence: true,
                open: open_at..open_end,
                close: close_start..close_at + 3,
                inner: open_end..close_start,
            });
        }
        from = close_at + 3;
    }
    pairs
}

fn find_token(text: &str, from: usize, token: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at + token.len() <= bytes.len() {
        if bytes[at] == b'\\' {
            at += 1;
            at += text[at..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if text[at..].starts_with(token) {
            return Some(at);
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn flatten(hits: &[Hit], len: usize) -> Vec<Look> {
    let mut cuts = vec![0usize, len];
    for hit in hits {
        if hit.range.start < hit.range.end && hit.range.end <= len {
            cuts.push(hit.range.start);
            cuts.push(hit.range.end);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut looks = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let mut look = Look::plain(start..end);
        for hit in hits {
            if hit.range.start <= start && end <= hit.range.end {
                match hit.style {
                    Style::Bold => look.bold = true,
                    Style::Italic => look.italic = true,
                    Style::Underline => look.underline = true,
                    Style::Strike => look.strike = true,
                    Style::Code => look.code = true,
                    Style::Spoiler => look.spoiler = true,
                    Style::Link => look.link = true,
                    Style::Quote => look.quote = true,
                    Style::Marker => look.marker = true,
                }
            }
        }
        if look.marker {
            look = Look {
                range: start..end,
                marker: true,
                ..Look::plain(start..end)
            };
        }
        if look.styled() {
            looks.push(look);
        }
    }
    looks
}

/// Slack draft formatting. Slack writes `*bold*`, `_italic_`, `~strike~`, `` `code` `` and `> `
/// quotes. The markers stay in the field like Discord's, then [`fold`] turns them into style
/// spans on send, so a message with a mention keeps its formatting too.
pub(in crate::gui) mod slack {
    use super::*;

    fn mark(kind: Kind) -> &'static str {
        match kind {
            Kind::Bold => "*",
            Kind::Italic => "_",
            Kind::Strike => "~",
            Kind::Code => "`",
            Kind::Quote => "> ",
            Kind::Underline | Kind::Spoiler => "",
        }
    }

    pub(in crate::gui) fn apply(
        text: &str,
        selection: Range<usize>,
        kind: Kind,
    ) -> (String, Range<usize>) {
        if matches!(kind, Kind::Underline | Kind::Spoiler) {
            return (text.to_owned(), snap(text, selection));
        }
        let sel = snap(text, selection);
        // `*|*` is not a pair Slack reads, so the empty case is undone by hand.
        if sel.is_empty() && kind != Kind::Quote {
            let m = mark(kind);
            if text[..sel.start].ends_with(m) && text[sel.start..].starts_with(m) {
                let pair = Pair {
                    kind,
                    fence: false,
                    open: sel.start - m.len()..sel.start,
                    close: sel.start..sel.start + m.len(),
                    inner: sel.start..sel.start,
                };
                return unwrap(text, &pair, &sel);
            }
        }
        apply_with(text, sel, kind, scan_inline, mark)
    }

    pub(in crate::gui) fn paint(text: &str) -> Vec<Look> {
        flatten(&analyze(text, scan_inline).0, text.len())
    }

    /// Takes the markers out of `text` and returns the styles they asked for, with `spans`
    /// (mentions, emoji) moved to the shorter text.
    pub(in crate::gui) fn fold(
        text: String,
        spans: Vec<protocol::Span>,
    ) -> (String, Vec<protocol::Span>) {
        let (hits, pairs) = analyze(&text, scan_inline);
        let mut cut: Vec<Range<usize>> = hits
            .iter()
            .filter(|hit| matches!(hit.style, Style::Marker) && hit.range.start < hit.range.end)
            .map(|hit| hit.range.clone())
            .collect();
        if cut.is_empty() {
            return (text, spans);
        }
        cut.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in cut {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        let map = |point: usize| -> u32 {
            let mut gone = 0;
            for range in &merged {
                if point >= range.end {
                    gone += range.end - range.start;
                } else if point > range.start {
                    gone += point - range.start;
                }
            }
            (point - gone) as u32
        };
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for range in &merged {
            out.push_str(&text[at..range.start]);
            at = range.end;
        }
        out.push_str(&text[at..]);
        let mut folded: Vec<protocol::Span> = spans
            .into_iter()
            .map(|span| protocol::Span {
                start: map(span.start as usize),
                end: map(span.end as usize),
                style: span.style,
            })
            .collect();
        for hit in &hits {
            let style = match hit.style {
                Style::Bold => protocol::Style::Bold,
                Style::Italic => protocol::Style::Italic,
                Style::Strike => protocol::Style::Strike,
                Style::Quote => protocol::Style::Quote,
                Style::Code if pairs.iter().any(|pair| pair.fence && pair.inner == hit.range) => {
                    protocol::Style::CodeBlock { language: String::new() }
                }
                Style::Code => protocol::Style::Code,
                _ => continue,
            };
            let (start, end) = (map(hit.range.start), map(hit.range.end));
            if start < end {
                folded.push(protocol::Span { start, end, style });
            }
        }
        (out, folded)
    }

    fn scan_inline(text: &str, from: usize, to: usize, hits: &mut Vec<Hit>, pairs: &mut Vec<Pair>) {
        let mut at = from;
        while at < to {
            let Some(first) = text[at..to].chars().next() else {
                break;
            };
            let kind = match first {
                '*' => Some(Kind::Bold),
                '_' => Some(Kind::Italic),
                '~' => Some(Kind::Strike),
                '`' => Some(Kind::Code),
                _ => None,
            };
            let opens = text[..at]
                .chars()
                .next_back()
                .is_none_or(|before| !before.is_alphanumeric());
            if let (Some(kind), true) = (kind, opens) {
                if let Some(close) = closer(text, at, to, first) {
                    push(text, at, close, kind, hits, pairs);
                    at = close + 1;
                    continue;
                }
            }
            at += first.len_utf8();
        }
    }

    /// The matching mark on the same line. Words must touch it, and a letter right after it
    /// means it was not a closer. Code takes anything.
    fn closer(text: &str, at: usize, to: usize, mark: char) -> Option<usize> {
        let rel = text[at + 1..to].find(mark)?;
        let inner = &text[at + 1..at + 1 + rel];
        if inner.is_empty() {
            return None;
        }
        if mark == '`' {
            return Some(at + 1 + rel);
        }
        let close = at + 1 + rel;
        let follows = text[close + 1..to].chars().next().is_some_and(char::is_alphanumeric);
        let spaced = inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace);
        (!spaced && !follows).then_some(close)
    }

    fn push(
        text: &str,
        open_at: usize,
        close_at: usize,
        kind: Kind,
        hits: &mut Vec<Hit>,
        pairs: &mut Vec<Pair>,
    ) {
        let open = open_at..open_at + 1;
        let close = close_at..close_at + 1;
        let inner = open.end..close.start;
        pairs.push(Pair {
            kind,
            fence: false,
            open: open.clone(),
            close: close.clone(),
            inner: inner.clone(),
        });
        hits.push(Hit { range: open, style: Style::Marker });
        hits.push(Hit { range: close, style: Style::Marker });
        hits.push(Hit { range: inner.clone(), style: style_for(kind) });
        if kind != Kind::Code {
            scan_inline(text, inner.start, inner.end, hits, pairs);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn marks_toggle_and_the_empty_caret_round_trips() {
            for (kind, m) in [(Kind::Bold, "*"), (Kind::Italic, "_"), (Kind::Strike, "~"), (Kind::Code, "`")] {
                let (text, sel) = apply("hi", 0..2, kind);
                assert_eq!(text, format!("{m}hi{m}"));
                assert_eq!(apply(&text, sel, kind).0, "hi");
                let (text, sel) = apply("hi", 2..2, kind);
                assert_eq!((text.as_str(), sel.clone()), (format!("hi{m}{m}").as_str(), 3..3));
                assert_eq!(apply(&text, sel, kind), ("hi".to_owned(), 2..2));
            }
        }

        #[test]
        fn underline_and_spoiler_do_nothing() {
            assert_eq!(apply("hi", 0..2, Kind::Underline).0, "hi");
            assert_eq!(apply("hi", 0..2, Kind::Spoiler).0, "hi");
        }

        #[test]
        fn a_newline_becomes_a_fence() {
            let (text, _) = apply("a\nb", 0..3, Kind::Code);
            assert_eq!(text, "```\na\nb\n```");
        }

        #[test]
        fn paint_dims_markers_and_leaves_mid_word_marks_alone() {
            let looks = paint("*hi* a_b_c ~x~");
            assert!(looks.iter().any(|look| look.bold && !look.marker && look.range == (1..3)));
            assert!(looks.iter().any(|look| look.strike && !look.marker));
            assert!(!looks.iter().any(|look| look.italic));
            assert!(paint("2 * 3 * 4").is_empty());
        }

        #[test]
        fn fold_strips_markers_and_moves_other_spans() {
            let mention = protocol::Span {
                start: 10,
                end: 14,
                style: protocol::Style::Mention {
                    kind: protocol::MentionKind::User,
                    id: "U1".into(),
                },
            };
            let (text, spans) = fold("*hi* _yo_ @Bob".to_owned(), vec![mention]);
            assert_eq!(text, "hi yo @Bob");
            let find = |style: protocol::Style| spans.iter().find(|s| s.style == style).cloned();
            let bold = find(protocol::Style::Bold).unwrap();
            assert_eq!((bold.start, bold.end), (0, 2));
            let italic = find(protocol::Style::Italic).unwrap();
            assert_eq!((italic.start, italic.end), (3, 5));
            let moved = spans.iter().find(|s| matches!(s.style, protocol::Style::Mention { .. })).unwrap();
            assert_eq!(&text[moved.start as usize..moved.end as usize], "@Bob");
        }

        #[test]
        fn fold_reads_quotes_and_fences() {
            let (text, spans) = fold("> quoted\n```\ncode\n```".to_owned(), Vec::new());
            assert_eq!(text, "quoted\ncode");
            assert!(spans.iter().any(|s| s.style == protocol::Style::Quote && (s.start, s.end) == (0, 6)));
            assert!(spans.iter().any(|s| matches!(s.style, protocol::Style::CodeBlock { .. })));
        }

        #[test]
        fn fold_leaves_plain_text_alone() {
            assert_eq!(fold("a * b".to_owned(), Vec::new()), ("a * b".to_owned(), Vec::new()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_wraps_and_toggles() {
        let (text, sel) = apply("say hi", 4..6, Kind::Bold);
        assert_eq!(text, "say **hi**");
        assert_eq!(sel, 6..8);
        let (back, sel) = apply(&text, sel, Kind::Bold);
        assert_eq!(back, "say hi");
        assert_eq!(sel, 4..6);
    }

    #[test]
    fn empty_bold_round_trips_at_the_caret() {
        let (text, sel) = apply("hi", 2..2, Kind::Bold);
        assert_eq!((text.as_str(), sel.clone()), ("hi****", 4..4));
        let (back, sel) = apply(&text, sel, Kind::Bold);
        assert_eq!((back.as_str(), sel), ("hi", 2..2));
    }

    #[test]
    fn italic_uses_a_single_star_and_does_not_eat_bold() {
        let (text, sel) = apply("hi", 2..2, Kind::Italic);
        assert_eq!((text.as_str(), sel.clone()), ("hi**", 3..3));
        assert_eq!(apply(&text, sel, Kind::Italic).0, "hi");
        let (bold, sel) = apply("hi", 2..2, Kind::Bold);
        assert_eq!(apply(&bold, sel, Kind::Italic).0, bold);
    }

    #[test]
    fn underline_strike_spoiler_and_inline_code_toggle() {
        for (kind, mark) in [
            (Kind::Underline, "__"),
            (Kind::Strike, "~~"),
            (Kind::Spoiler, "||"),
            (Kind::Code, "`"),
        ] {
            let (text, sel) = apply("hi", 0..2, kind);
            assert_eq!(text, format!("{mark}hi{mark}"));
            assert_eq!(apply(&text, sel, kind).0, "hi");
        }
    }

    #[test]
    fn a_newline_becomes_a_fence_and_toggles_off() {
        let (text, sel) = apply("alpha\nbeta", 0..10, Kind::Code);
        assert_eq!(text, "```\nalpha\nbeta\n```");
        assert_eq!(sel, 4..14);
        let (back, sel) = apply(&text, sel, Kind::Code);
        assert_eq!(back, "alpha\nbeta");
        assert_eq!(sel, 0..10);
    }

    #[test]
    fn code_inside_a_fence_does_not_add_backticks() {
        let fenced = "```\nalpha\n```";
        assert_eq!(apply(fenced, 4..6, Kind::Code).0, fenced);
    }

    #[test]
    fn quote_toggles_and_keeps_a_caret_on_the_same_character() {
        let (text, sel) = apply("hello", 0..5, Kind::Quote);
        assert_eq!(text, "> hello");
        assert_eq!(sel, 0..7);
        assert_eq!(apply(&text, sel, Kind::Quote).0, "hello");
        let (text, sel) = apply("hello", 0..0, Kind::Quote);
        assert_eq!(text, "> hello");
        assert_eq!(sel, 2..2);
        assert_eq!(apply(&text, sel, Kind::Quote), ("hello".to_owned(), 0..0));
    }

    #[test]
    fn quote_shifts_a_middle_selection() {
        let (text, sel) = apply("hello", 1..4, Kind::Quote);
        assert_eq!(text, "> hello");
        assert_eq!(&text[sel], "ell");
    }

    #[test]
    fn mask_link_wraps_http_and_rejects_the_rest() {
        let (text, sel) = mask_link("hello", 0..5, "https://example.com").unwrap();
        assert_eq!(text, "[hello](https://example.com)");
        assert_eq!(&text[sel], "hello");
        assert!(mask_link("hello", 0..0, "https://example.com").is_none());
        assert!(mask_link("hello", 0..5, "https://example.com extra").is_none());
        assert!(mask_link("hello", 0..5, "https://exa)mple.com").is_none());
        assert!(mask_link("hello", 0..5, "note").is_none());
        assert!(mask_link("hello", 0..5, "https://").is_none());
    }

    #[test]
    fn paint_dims_markers_and_styles_the_inside() {
        let looks = paint("**hi** `x` ||s||");
        assert!(looks
            .iter()
            .any(|look| look.range == (2..4) && look.bold && !look.marker));
        assert!(looks.iter().any(|look| look.marker && look.range == (0..2)));
        assert!(looks.iter().any(|look| look.code && !look.marker));
        assert!(looks.iter().any(|look| look.spoiler && !look.marker));
    }

    #[test]
    fn underscore_inside_a_word_is_not_italic() {
        let looks = paint("a_b_c *b*");
        assert!(!looks.iter().any(|look| look.italic && look.range == (2..3)));
        assert!(looks.iter().any(|look| look.italic && !look.marker));
    }

    #[test]
    fn nested_bold_holds_italic() {
        let looks = paint("**a *b* c**");
        assert!(looks
            .iter()
            .any(|look| look.bold && look.italic && !look.marker));
    }

    #[test]
    fn a_fence_is_code_and_not_bold() {
        let looks = paint("```\n**a**\n```");
        assert!(looks
            .iter()
            .any(|look| look.code && !look.marker && look.range.contains(&5)));
        assert!(!looks.iter().any(|look| look.bold));
    }

    #[test]
    fn links_and_quotes_and_escapes() {
        let looks = paint("[hi](https://example.com) and https://example.com");
        assert!(looks
            .iter()
            .any(|look| look.link && !look.marker && look.range == (1..3)));
        assert!(looks.iter().any(|look| look.link && look.range.end > 20));
        let looks = paint("> **hi**");
        assert!(looks.iter().any(|look| look.quote && look.bold));
        assert!(quote_line("> hi") && !quote_line("hi"));
        let text = ">>> stay\nplain";
        assert!(line_quoted(text, 0) && line_quoted(text, text.find("plain").unwrap()));
        let looks = paint(r"\*not italic\*");
        assert!(!looks.iter().any(|look| look.italic));
        assert!(looks.iter().any(|look| look.marker));
    }

    #[test]
    fn markers_table_names_each_kind() {
        let names: Vec<_> = discord_markers().iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "bold",
                "italic",
                "underline",
                "strike",
                "code",
                "spoiler",
                "quote"
            ]
        );
    }
}
