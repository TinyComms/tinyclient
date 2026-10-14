//! A message's text and spans as what is drawn: blocks top to bottom, each a run of styled
//! text. No drawing here; `text.rs` turns a block into an element.

use protocol::{MediaFile, MentionKind, Span, Style};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Stands in for a custom emoji whose image is on disk. Two spaces that together are a little
/// wider than the 18 px image painted over them.
pub const EMOJI_SLOT: &str = "\u{2003}\u{2004}";
/// The room either side of inline code and mentions, inside their background.
const PAD: &str = "\u{2005}";

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(Inline),
    Quote(Inline),
    Item { ordered: bool, depth: u8, number: u32, text: Inline },
    Code { language: String, text: String },
}

/// Styled text. `runs` cover `text` end to end.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Inline {
    pub text: String,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Run {
    /// Bytes of [`Inline::text`].
    pub len: usize,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    pub mention: Option<(MentionKind, String)>,
    /// Which spoiler this is part of: the span's start, which names it within the message.
    pub spoiler: Option<u32>,
    /// A custom emoji. With its image on disk the text is [`EMOJI_SLOT`]; without, `:name:`.
    pub emoji: Option<MediaFile>,
    /// "(edited)", after the last word.
    pub mark: bool,
    /// What the run stands for when it is copied: the `:name:` of an emoji drawn as its image.
    pub alt: Option<String>,
}

impl Run {
    fn same_style(&self, other: &Run) -> bool {
        Run { len: 0, ..self.clone() } == Run { len: 0, ..other.clone() }
    }

    /// Drawn on a background that the padding belongs to.
    fn boxed(&self) -> bool {
        self.code || self.mention.is_some()
    }
}

impl Inline {
    /// The byte range of each run, in order.
    pub fn ranges(&self) -> impl Iterator<Item = (Range<usize>, &Run)> {
        self.runs.iter().scan(0, |at, run| {
            let range = *at..*at + run.len;
            *at = range.end;
            Some((range, run))
        })
    }

    /// Adds the edited mark after the last word, so it never takes a line of its own.
    pub fn mark_edited(&mut self) {
        const MARK: &str = " (edited)";
        self.text.push_str(MARK);
        self.runs.push(Run { len: MARK.len(), mark: true, ..Run::default() });
    }
}

/// The nearest character boundary at or before `at`, so a span that is off by a byte cannot
/// split a character.
fn floor(text: &str, at: u32) -> usize {
    let mut at = (at as usize).min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn is_block(style: &Style) -> bool {
    matches!(style, Style::CodeBlock { .. } | Style::Quote | Style::ListItem { .. })
}

/// The styled runs of `text[range]`.
fn inline(text: &str, spans: &[Span], range: Range<usize>) -> Inline {
    let within = |span: &&Span| {
        !is_block(&span.style) && floor(text, span.start) < range.end && floor(text, span.end) > range.start
    };
    let spans: Vec<&Span> = spans.iter().filter(within).collect();
    let mut cuts = vec![range.start, range.end];
    for span in &spans {
        cuts.push(floor(text, span.start).clamp(range.start, range.end));
        cuts.push(floor(text, span.end).clamp(range.start, range.end));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut pieces: Vec<(String, Run)> = Vec::new();
    for cut in cuts.windows(2) {
        let (start, end) = (cut[0], cut[1]);
        let mut run = Run::default();
        let mut shown = &text[start..end];
        for span in spans.iter().filter(|span| floor(text, span.start) <= start && floor(text, span.end) >= end) {
            match &span.style {
                Style::Bold => run.bold = true,
                Style::Italic => run.italic = true,
                Style::Underline => run.underline = true,
                Style::Strike => run.strike = true,
                Style::Code => run.code = true,
                Style::Spoiler => run.spoiler = Some(span.start),
                Style::Link { url } => run.link = Some(url.clone()),
                Style::Mention { kind, id } => run.mention = Some((*kind, id.clone())),
                Style::Emoji { file } => {
                    if file.path.is_some() {
                        run.alt = Some(text[start..end].to_owned());
                        shown = EMOJI_SLOT;
                    }
                    run.emoji = Some(file.clone());
                }
                Style::CodeBlock { .. } | Style::Quote | Style::ListItem { .. } => {}
            }
        }
        match pieces.last_mut() {
            Some((before, last)) if last.same_style(&run) && run.emoji.is_none() => before.push_str(shown),
            _ => pieces.push((shown.to_owned(), run)),
        }
    }

    let mut out = Inline::default();
    for (ix, (shown, run)) in pieces.iter().enumerate() {
        let opens = run.boxed() && !ix.checked_sub(1).is_some_and(|ix| pieces[ix].1.same_box(run));
        let closes = run.boxed() && !pieces.get(ix + 1).is_some_and(|(_, next)| next.same_box(run));
        let before = if opens { PAD } else { "" };
        let after = if closes { PAD } else { "" };
        out.text.extend([before, shown.as_str(), after]);
        out.runs.push(Run { len: before.len() + shown.len() + after.len(), ..run.clone() });
    }
    out
}

impl Run {
    fn same_box(&self, other: &Run) -> bool {
        self.code == other.code && self.mention == other.mention
    }
}

/// Blank lines at either end of a paragraph belong to the blocks around it.
fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let slice = &text[range.clone()];
    let start = range.start + (slice.len() - slice.trim_start_matches(['\n', '\r']).len());
    let end = range.end - (slice.len() - slice.trim_end_matches(['\n', '\r', ' ']).len());
    start..end.max(start)
}

pub fn blocks(text: &str, spans: &[Span]) -> Vec<Block> {
    let mut whole: Vec<&Span> = spans.iter().filter(|span| is_block(&span.style)).collect();
    whole.sort_by_key(|span| span.start);

    let mut out = Vec::new();
    let mut at = 0;
    // The number the next ordered item gets, per depth.
    let mut numbers = [0u32; 3];
    let paragraph = |out: &mut Vec<Block>, range: Range<usize>| {
        let range = trimmed(text, range);
        if !range.is_empty() {
            out.push(Block::Text(inline(text, spans, range)));
        }
    };
    for span in whole {
        let (start, end) = (floor(text, span.start), floor(text, span.end));
        // A block inside another (a quote in a quote) is already drawn by the outer one.
        if start < at || end <= start {
            continue;
        }
        if !trimmed(text, at..start).is_empty() {
            numbers = [0; 3];
        }
        paragraph(&mut out, at..start);
        let body = trimmed(text, start..end);
        match &span.style {
            Style::CodeBlock { language } => {
                numbers = [0; 3];
                let code = text[start..end].trim_matches(['\n', '\r']);
                out.push(Block::Code { language: language.clone(), text: code.to_owned() });
            }
            Style::Quote => {
                numbers = [0; 3];
                // Consecutive quoted lines are one quote under one bar.
                let more = inline(text, spans, body);
                match out.last_mut() {
                    Some(Block::Quote(quote)) => {
                        quote.text.push('\n');
                        quote.runs.push(Run { len: 1, ..Run::default() });
                        quote.text.push_str(&more.text);
                        quote.runs.extend(more.runs);
                    }
                    _ => out.push(Block::Quote(more)),
                }
            }
            Style::ListItem { ordered, depth } => {
                // Three levels are drawn; deeper ones sit on the third.
                let depth = (*depth).min(2);
                numbers[depth as usize] += 1;
                for deeper in &mut numbers[depth as usize + 1..] {
                    *deeper = 0;
                }
                out.push(Block::Item {
                    ordered: *ordered,
                    depth,
                    number: numbers[depth as usize],
                    text: inline(text, spans, body),
                });
            }
            _ => {}
        }
        at = end;
    }
    paragraph(&mut out, at..text.len());
    out
}

/// Only the room a boxed run is given: a visual line holding nothing else has nothing to box.
pub fn is_padding(shown: &str) -> bool {
    shown.chars().all(|c| c.is_whitespace())
}

/// A place in a message's text: which block, and the byte of its shown text.
pub type Place = (usize, usize);

impl Inline {
    /// What `range` of the shown text is as plain text: a custom emoji is its `:name:`, a link
    /// whose words differ from its address is the address, and the padding and the edited mark,
    /// which were never written, are left out.
    pub fn plain(&self, range: Range<usize>) -> String {
        let mut out = String::new();
        let mut linked: Option<&str> = None;
        for (at, run) in self.ranges() {
            let (start, end) = (at.start.max(range.start), at.end.min(range.end));
            if start >= end {
                continue;
            }
            if run.mark {
                continue;
            }
            // A link whose words are not its address is written as the address, once.
            let address = run.link.as_deref().filter(|url| !self.link_reads_as(url));
            if let Some(url) = address {
                if linked != address {
                    out.push_str(url);
                }
                linked = address;
                continue;
            }
            linked = None;
            match &run.alt {
                Some(name) => out.push_str(name),
                None => out.extend(self.text[start..end].chars().filter(|c| !PAD.contains(*c))),
            }
        }
        out
    }

    /// The words of the link to `url` are the address itself, give or take its scheme.
    fn link_reads_as(&self, url: &str) -> bool {
        let words: String = self
            .ranges()
            .filter(|(_, run)| run.link.as_deref() == Some(url))
            .map(|(range, _)| &self.text[range])
            .collect();
        let bare = |text: &str| text.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_owned();
        bare(&words) == bare(url)
    }
}

/// The plain text between two places of a message, in either order, block by block: what
/// Cmd-C puts on the clipboard. Code between them comes whole, with its newlines.
pub fn plain(blocks: &[Block], a: Place, b: Place) -> String {
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    let mut lines = Vec::new();
    for (ix, block) in blocks.iter().enumerate().filter(|(ix, _)| (from.0..=to.0).contains(ix)) {
        let part = |inline: &Inline| {
            let start = if ix == from.0 { from.1.min(inline.text.len()) } else { 0 };
            let end = if ix == to.0 { to.1.min(inline.text.len()) } else { inline.text.len() };
            (start == 0, inline.plain(start..end.max(start)))
        };
        lines.push(match block {
            Block::Text(inline) => part(inline).1,
            Block::Quote(inline) => match part(inline) {
                (true, text) => text.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n"),
                (false, text) => text,
            },
            Block::Item { ordered, number, text, .. } => match part(text) {
                (true, text) if *ordered => format!("{number}. {text}"),
                (true, text) => format!("• {text}"),
                (false, text) => text,
            },
            Block::Code { text, .. } => text.clone(),
        });
    }
    lines.retain(|line| !line.is_empty());
    lines.join("\n")
}

/// One thing in a message that is only emoji.
#[derive(Debug, Clone, PartialEq)]
pub enum Jumbo {
    Unicode(String),
    Custom { file: MediaFile, name: String },
}

fn is_emoji(grapheme: &str) -> bool {
    let Some(first) = grapheme.chars().next() else {
        return false;
    };
    let pictograph = matches!(first as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0x2190..=0x21FF
        | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139);
    // A digit or symbol becomes an emoji only with the presentation selector or a keycap.
    pictograph || grapheme.contains(['\u{FE0F}', '\u{20E3}'])
}

/// A message that is nothing but one to three emoji is drawn large. `None` for anything else.
pub fn jumbo(text: &str, spans: &[Span]) -> Option<Vec<Jumbo>> {
    if spans.iter().any(|span| !matches!(span.style, Style::Emoji { .. })) {
        return None;
    }
    let mut custom: Vec<(Range<usize>, &MediaFile)> = spans
        .iter()
        .filter_map(|span| match &span.style {
            Style::Emoji { file } => Some((floor(text, span.start)..floor(text, span.end), file)),
            _ => None,
        })
        .collect();
    custom.sort_by_key(|(range, _)| range.start);

    let mut found = Vec::new();
    let mut at = 0;
    let unicode = |found: &mut Vec<Jumbo>, plain: &str| {
        for grapheme in plain.graphemes(true).filter(|grapheme| !grapheme.trim().is_empty()) {
            if !is_emoji(grapheme) {
                return false;
            }
            found.push(Jumbo::Unicode(grapheme.to_owned()));
        }
        true
    };
    for (range, file) in custom {
        if range.start < at || !unicode(&mut found, &text[at..range.start]) {
            return None;
        }
        found.push(Jumbo::Custom { file: file.clone(), name: text[range.clone()].to_owned() });
        at = range.end;
    }
    if !unicode(&mut found, &text[at..]) {
        return None;
    }
    (1..=3).contains(&found.len()).then_some(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;

    fn span(text: &str, part: &str, style: Style) -> Span {
        let start = text.find(part).unwrap_or_else(|| panic!("{part:?} is not in {text:?}"));
        Span { start: start as u32, end: (start + part.len()) as u32, style }
    }

    fn only(text: &str, spans: &[Span]) -> Inline {
        match blocks(text, spans).as_slice() {
            [Block::Text(inline)] => inline.clone(),
            other => panic!("not one paragraph: {other:?}"),
        }
    }

    /// Each run as its text and the styles on it.
    fn runs(inline: &Inline) -> Vec<(&str, String)> {
        inline
            .ranges()
            .map(|(range, run)| {
                let mut styles = String::new();
                for (on, letter) in [
                    (run.bold, 'b'),
                    (run.italic, 'i'),
                    (run.strike, 's'),
                    (run.underline, 'u'),
                    (run.code, 'c'),
                    (run.link.is_some(), 'l'),
                    (run.mention.is_some(), 'm'),
                    (run.spoiler.is_some(), 'x'),
                    (run.emoji.is_some(), 'e'),
                ] {
                    if on {
                        styles.push(letter);
                    }
                }
                (&inline.text[range], styles)
            })
            .collect()
    }

    #[test]
    fn overlapping_styles_split_into_runs_on_character_boundaries() {
        let text = "Grüße aus Zürich, 日本語 too 🎉 done";
        let spans = [
            span(text, "Grüße aus Zürich", Style::Bold),
            span(text, "Zürich, 日本語", Style::Italic),
            span(text, "🎉", Style::Strike),
        ];
        let inline = only(text, &spans);
        assert_eq!(inline.text, text);
        assert_eq!(
            runs(&inline),
            [
                ("Grüße aus ", "b".to_owned()),
                ("Zürich", "bi".to_owned()),
                (", 日本語", "i".to_owned()),
                (" too ", String::new()),
                ("🎉", "s".to_owned()),
                (" done", String::new()),
            ]
        );
        assert_eq!(inline.runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    }

    #[test]
    fn a_span_that_ends_inside_a_character_is_pulled_back_to_its_start() {
        let text = "né bold";
        // One byte into the two-byte é.
        let spans = [Span { start: 0, end: 2, style: Style::Bold }];
        assert_eq!(runs(&only(text, &spans)), [("n", "b".to_owned()), ("é bold", String::new())]);
    }

    #[test]
    fn code_and_mentions_get_room_inside_their_background() {
        let text = "ask @Zoë about `cfg`";
        let spans = [
            span(text, "@Zoë", Style::Mention { kind: MentionKind::User, id: "U1".into() }),
            span(text, "cfg", Style::Code),
        ];
        let inline = only(text, &spans);
        assert_eq!(inline.text, format!("ask {PAD}@Zoë{PAD} about `{PAD}cfg{PAD}`"));
        let boxed: Vec<&str> = runs(&inline).iter().filter(|(_, s)| !s.is_empty()).map(|(t, _)| *t).collect();
        assert_eq!(boxed, [format!("{PAD}@Zoë{PAD}"), format!("{PAD}cfg{PAD}")]);
    }

    #[test]
    fn a_custom_emoji_is_a_slot_once_its_image_is_on_disk_and_its_name_until_then() {
        let text = "ship it :parrot: now";
        let mut file = MediaFile::new("e1", MediaKind::Emoji);
        let waiting = only(text, &[span(text, ":parrot:", Style::Emoji { file: file.clone() })]);
        assert_eq!(waiting.text, text);
        assert_eq!(runs(&waiting)[1], (":parrot:", "e".to_owned()));

        file.path = Some("/tmp/parrot.png".into());
        let landed = only(text, &[span(text, ":parrot:", Style::Emoji { file })]);
        assert_eq!(landed.text, format!("ship it {EMOJI_SLOT} now"));
        assert_eq!(runs(&landed)[1], (EMOJI_SLOT, "e".to_owned()));
    }

    #[test]
    fn a_selection_copies_as_what_was_written_not_what_was_drawn() {
        let text = "ask @Zoë about `cfg` at docs :parrot: ok\n```\nlet x = 1;\n```\n> quoted\nlast line";
        let mut file = MediaFile::new("e1", MediaKind::Emoji);
        file.path = Some("/tmp/parrot.png".into());
        let spans = [
            span(text, "@Zoë", Style::Mention { kind: MentionKind::User, id: "U1".into() }),
            span(text, "cfg", Style::Code),
            span(text, "docs", Style::Link { url: "https://example.org/docs".into() }),
            span(text, ":parrot:", Style::Emoji { file }),
            span(text, "```\nlet x = 1;\n```", Style::CodeBlock { language: String::new() }),
            span(text, "> quoted", Style::Quote),
        ];
        let mut found = blocks(text, &spans);
        if let Some(Block::Text(last)) = found.last_mut() {
            last.mark_edited();
        }
        let end = |block: usize| match &found[block] {
            Block::Text(inline) | Block::Quote(inline) | Block::Item { text: inline, .. } => inline.text.len(),
            Block::Code { text, .. } => text.len(),
        };
        // Everything: mention by name, the emoji by its name, the link by its address, the code
        // whole, and neither padding nor "(edited)".
        let all = plain(&found, (0, 0), (3, end(3)));
        assert_eq!(
            all,
            "ask @Zoë about `cfg` at https://example.org/docs :parrot: ok\n```\nlet x = 1;\n```\n> > quoted\nlast line"
        );
        // Dragged backwards, and from the middle of the first block.
        let first = match &found[0] {
            Block::Text(inline) => inline.text.find("about").unwrap(),
            _ => unreachable!(),
        };
        assert!(plain(&found, (3, 4), (0, first)).starts_with("about `cfg`"));
        assert!(plain(&found, (3, 4), (0, first)).ends_with("\nlast"));
        // A link written as its own address is not said twice.
        let bare = "see https://example.org/";
        let one = only(bare, &[span(bare, "https://example.org/", Style::Link { url: "https://example.org".into() })]);
        assert_eq!(one.plain(0..one.text.len()), bare);
        assert!(is_padding(PAD) && !is_padding("a"));
    }

    #[test]
    fn block_spans_split_the_message_and_ordered_items_count_per_level() {
        let text = "Plan:\n> first line\n> second\n1. one\n2. two\n   a. inner\n3. three\n```\nlet x = 1;\n```\nDone ✅";
        let item = |part: &str, depth: u8| span(text, part, Style::ListItem { ordered: true, depth });
        let spans = [
            span(text, "> first line", Style::Quote),
            span(text, "> second", Style::Quote),
            item("1. one", 0),
            item("2. two", 0),
            item("   a. inner", 1),
            item("3. three", 0),
            span(text, "```\nlet x = 1;\n```", Style::CodeBlock { language: "rust".into() }),
        ];
        let found = blocks(text, &spans);
        let shape: Vec<String> = found
            .iter()
            .map(|block| match block {
                Block::Text(inline) => format!("text {:?}", inline.text),
                Block::Quote(inline) => format!("quote {:?}", inline.text),
                Block::Item { depth, number, text, .. } => format!("item {depth}.{number} {:?}", text.text),
                Block::Code { language, text } => format!("code {language} {text:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "text \"Plan:\"",
                "quote \"> first line\\n> second\"",
                "item 0.1 \"1. one\"",
                "item 0.2 \"2. two\"",
                "item 1.1 \"   a. inner\"",
                "item 0.3 \"3. three\"",
                "code rust \"```\\nlet x = 1;\\n```\"",
                "text \"Done ✅\"",
            ]
        );
    }

    #[test]
    fn one_to_three_emoji_alone_are_jumbo_and_nothing_else_is() {
        let unicode = |text: &str| jumbo(text, &[]).map(|found| found.len());
        assert_eq!(unicode("🎉"), Some(1));
        assert_eq!(unicode("👍🏽 🇩🇪 👨‍👩‍👧"), Some(3));
        assert_eq!(unicode("❤️"), Some(1));
        assert_eq!(unicode("🎉🎉🎉🎉"), None);
        assert_eq!(unicode("ok 🎉"), None);
        assert_eq!(unicode("123"), None);
        assert_eq!(unicode(""), None);

        let text = ":parrot: 🎉";
        let file = MediaFile::new("e1", MediaKind::Emoji);
        let spans = [span(text, ":parrot:", Style::Emoji { file: file.clone() })];
        assert_eq!(
            jumbo(text, &spans),
            Some(vec![Jumbo::Custom { file, name: ":parrot:".into() }, Jumbo::Unicode("🎉".into())])
        );
        assert_eq!(jumbo("🎉", &[span("🎉", "🎉", Style::Bold)]), None);
    }
}
