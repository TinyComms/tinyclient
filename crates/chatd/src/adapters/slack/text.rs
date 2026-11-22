//! Slack's two ways of writing a message, as the one text-and-ranges model: `blocks` (rich text
//! and Block Kit's text-bearing blocks) and mrkdwn, the older inline markup.

use super::emoji;
use protocol::{MediaFile, MentionKind, Span, Style};
use serde_json::{json, Value};

/// What a custom emoji name stands for.
pub enum Custom {
    /// An alias of a standard emoji.
    Unicode(String),
    File(MediaFile),
}

/// What the text needs looked up. The adapter answers from its cache and asks Slack only for
/// what it has never seen.
pub trait Names {
    fn me(&self) -> &str;
    fn user(&self, id: &str) -> String;
    /// The channel's title, `#name` included.
    fn channel(&self, id: &str) -> Option<String>;
    fn usergroup(&self, id: &str) -> Option<String>;
    /// Whether I belong to this user group.
    fn in_usergroup(&self, _id: &str) -> bool {
        false
    }
    fn custom(&self, name: &str) -> Option<Custom>;
    /// Whether this URL is served by Slack (or its image proxy) and can be attached inline.
    fn hosted(&self, _url: &str) -> bool {
        false
    }
}

pub struct Rich<'a> {
    names: &'a dyn Names,
    pub text: String,
    pub spans: Vec<Span>,
    /// The text names me.
    pub mentioned: bool,
    /// The text names everyone: `@here`, `@channel`, `@everyone`.
    pub broadcast: bool,
    /// Blocks tinyclient does not draw (buttons, inputs, video) were left out.
    pub undrawn: bool,
}

fn word_like(c: char) -> bool {
    c.is_alphanumeric()
}

impl<'a> Rich<'a> {
    pub fn new(names: &'a dyn Names) -> Rich<'a> {
        Rich {
            names,
            text: String::new(),
            spans: Vec::new(),
            mentioned: false,
            broadcast: false,
            undrawn: false,
        }
    }

    fn at(&self) -> u32 {
        self.text.len() as u32
    }

    fn span(&mut self, start: u32, style: Style) {
        let end = self.at();
        if end > start {
            self.spans.push(Span { start, end, style });
        }
    }

    /// Starts a new line unless the text is already at the start of one.
    pub fn line(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    /// Drops trailing blank space and whatever ranges it leaves empty.
    pub fn finish(&mut self) {
        let len = self.text.trim_end().len();
        self.text.truncate(len);
        let len = len as u32;
        self.spans.retain_mut(|span| {
            span.end = span.end.min(len);
            span.start < span.end
        });
    }

    fn user(&mut self, id: &str, label: Option<&str>) {
        let start = self.at();
        self.text.push('@');
        match label {
            Some(label) => self.text.push_str(label.trim_start_matches('@')),
            None => self.text.push_str(&self.names.user(id)),
        }
        self.mentioned |= id == self.names.me();
        let kind = MentionKind::User;
        self.span(start, Style::Mention { kind, id: id.to_owned() });
    }

    fn channel(&mut self, id: &str, label: Option<&str>) {
        let start = self.at();
        match label.filter(|label| !label.is_empty()) {
            Some(label) => {
                self.text.push('#');
                self.text.push_str(label);
            }
            None => match self.names.channel(id) {
                Some(title) => self.text.push_str(&title),
                // A private channel we are not in: Slack shows no name either.
                None => self.text.push_str("#private-channel"),
            },
        }
        let kind = MentionKind::Channel;
        self.span(start, Style::Mention { kind, id: id.to_owned() });
    }

    fn usergroup(&mut self, id: &str, label: Option<&str>) {
        let start = self.at();
        self.text.push('@');
        match label {
            Some(label) => self.text.push_str(label.trim_start_matches('@')),
            None => self.text.push_str(&self.names.usergroup(id).unwrap_or_else(|| "group".to_owned())),
        }
        self.mentioned |= self.names.in_usergroup(id);
        let kind = MentionKind::Role;
        self.span(start, Style::Mention { kind, id: id.to_owned() });
    }

    fn everyone(&mut self, range: &str) {
        let start = self.at();
        self.text.push('@');
        self.text.push_str(range);
        self.broadcast = true;
        let kind = MentionKind::Everyone;
        self.span(start, Style::Mention { kind, id: range.to_owned() });
    }

    pub fn link(&mut self, url: &str, label: Option<&str>) {
        let start = self.at();
        self.text.push_str(label.filter(|label| !label.is_empty()).unwrap_or(url));
        self.span(start, Style::Link { url: url.to_owned() });
    }

    /// A standard emoji as itself, a custom one as `:name:` with its picture, and a name nobody
    /// knows as it was written.
    fn emoji(&mut self, name: &str) {
        if let Some(standard) = emoji::standard(name) {
            self.text.push_str(&standard);
            return;
        }
        let start = self.at();
        match self.names.custom(name) {
            Some(Custom::Unicode(emoji)) => self.text.push_str(&emoji),
            Some(Custom::File(file)) => {
                self.text.push_str(&format!(":{name}:"));
                self.span(start, Style::Emoji { file });
            }
            None => self.text.push_str(&format!(":{name}:")),
        }
    }

    // Blocks.

    /// Every block that carries text. Returns false when none did.
    pub fn blocks(&mut self, blocks: &[Value]) -> bool {
        let before = self.text.len();
        for block in blocks {
            self.line();
            match block["type"].as_str().unwrap_or("") {
                "rich_text" => {
                    for element in block["elements"].as_array().into_iter().flatten() {
                        self.rich_block(element);
                    }
                }
                "section" => {
                    self.text_object(&block["text"]);
                    for field in block["fields"].as_array().into_iter().flatten() {
                        self.line();
                        self.text_object(field);
                    }
                }
                "header" => {
                    let start = self.at();
                    self.text_object(&block["text"]);
                    self.span(start, Style::Bold);
                }
                "context" => {
                    for (at, element) in block["elements"].as_array().into_iter().flatten().enumerate() {
                        if element["type"] == "image" {
                            continue;
                        }
                        if at > 0 && !self.text.ends_with(['\n', ' ']) {
                            self.text.push(' ');
                        }
                        self.text_object(element);
                    }
                }
                "image" => {
                    let url = block["image_url"]
                        .as_str()
                        .or_else(|| block["slack_file"]["url_private"].as_str())
                        .unwrap_or("");
                    if !url.is_empty() && self.names.hosted(url) {
                        // Attached inline by message.rs.
                    } else {
                        // The picture is at an address Slack does not serve, so it is named, not fetched.
                        let label = [&block["title"]["text"], &block["alt_text"]];
                        let label = label.into_iter().filter_map(Value::as_str).find(|label| !label.is_empty());
                        self.text.push_str(&format!("[image: {}]", label.unwrap_or("untitled")));
                    }
                }
                "video" => {
                    // Mapped to a Preview with thumbnail and link by message.rs.
                }
                "markdown" => {
                    if let Some(text) = block["text"].as_str() {
                        self.mrkdwn(text);
                    }
                }
                "table" => {
                    for row in block["rows"].as_array().into_iter().flatten() {
                        self.line();
                        for (ix, cell) in row.as_array().into_iter().flatten().enumerate() {
                            if ix > 0 {
                                self.text.push_str(" | ");
                            }
                            if cell["type"] == "rich_text" {
                                for el in cell["elements"].as_array().into_iter().flatten() {
                                    self.rich_block(el);
                                }
                            } else {
                                self.text_object(cell);
                            }
                        }
                    }
                }
                "divider" => {
                    self.text.push_str("───");
                }
                // A meeting card (`call`). Drawn as a card with a Join link, not as buttons.
                "call" => {}
                _ => self.undrawn = true,
            }
        }
        self.text.len() > before
    }

    /// A Block Kit text object: `mrkdwn` or `plain_text`.
    fn text_object(&mut self, object: &Value) {
        let Some(text) = object["text"].as_str() else {
            return;
        };
        if object["type"] == "plain_text" {
            self.text.push_str(text);
        } else {
            self.mrkdwn(text);
        }
    }

    fn rich_block(&mut self, element: &Value) {
        self.line();
        let children = element["elements"].as_array().map_or(&[][..], Vec::as_slice);
        match element["type"].as_str().unwrap_or("") {
            "rich_text_list" => {
                let style = Style::ListItem {
                    ordered: element["style"] == "ordered",
                    depth: element["indent"].as_u64().unwrap_or(0).min(8) as u8,
                };
                for item in children {
                    self.line();
                    let start = self.at();
                    self.inline(item["elements"].as_array().map_or(&[][..], Vec::as_slice));
                    self.span(start, style.clone());
                }
            }
            "rich_text_preformatted" => {
                let start = self.at();
                self.inline(children);
                let language = element["language"].as_str().unwrap_or("").to_owned();
                self.span(start, Style::CodeBlock { language });
            }
            "rich_text_quote" => {
                let start = self.at();
                self.inline(children);
                self.span(start, Style::Quote);
            }
            _ => self.inline(children),
        }
    }

    fn inline(&mut self, elements: &[Value]) {
        for element in elements {
            let start = self.at();
            match element["type"].as_str().unwrap_or("") {
                "text" => self.text.push_str(element["text"].as_str().unwrap_or("")),
                "link" => {
                    let url = element["url"].as_str().unwrap_or("");
                    self.link(url, element["text"].as_str());
                }
                "user" => self.user(element["user_id"].as_str().unwrap_or(""), None),
                "channel" => self.channel(element["channel_id"].as_str().unwrap_or(""), None),
                "usergroup" => self.usergroup(element["usergroup_id"].as_str().unwrap_or(""), None),
                "broadcast" => self.everyone(element["range"].as_str().unwrap_or("here")),
                "emoji" => {
                    let name = element["name"].as_str().unwrap_or("");
                    match element["unicode"].as_str().and_then(emoji::from_code_points) {
                        Some(emoji) => self.text.push_str(&emoji),
                        None => self.emoji(name),
                    }
                }
                "date" => self.text.push_str(element["fallback"].as_str().unwrap_or("")),
                "color" => self.text.push_str(element["value"].as_str().unwrap_or("")),
                _ => self.text.push_str(element["text"].as_str().unwrap_or("")),
            }
            let style = &element["style"];
            let styles = [
                ("bold", Style::Bold),
                ("italic", Style::Italic),
                ("strike", Style::Strike),
                ("underline", Style::Underline),
                ("code", Style::Code),
            ];
            for (name, style_of) in styles {
                if style[name] == true {
                    self.span(start, style_of);
                }
            }
        }
    }

    // mrkdwn.

    /// Slack's inline markup: `*bold*`, `_italic_`, `~strike~`, `` `code` ``, fenced code,
    /// `>` quotes, `<…>` references and `:emoji:`.
    pub fn mrkdwn(&mut self, text: &str) {
        for (at, part) in text.split("```").enumerate() {
            if at % 2 == 1 {
                self.line();
                let start = self.at();
                self.plain(part.trim_matches('\n'));
                self.span(start, Style::CodeBlock { language: String::new() });
                self.line();
                continue;
            }
            // The line break after a closing fence belongs to the fence.
            let part = if at > 0 { part.strip_prefix('\n').unwrap_or(part) } else { part };
            for (line_at, line) in part.split('\n').enumerate() {
                if line_at > 0 {
                    self.text.push('\n');
                }
                match line.strip_prefix("&gt;") {
                    Some(quoted) => {
                        let start = self.at();
                        self.styled(quoted.strip_prefix(' ').unwrap_or(quoted));
                        self.span(start, Style::Quote);
                    }
                    None => self.styled(line),
                }
            }
        }
    }

    /// Text with Slack's three escapes undone and nothing else read into it.
    fn plain(&mut self, text: &str) {
        self.text.push_str(&text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"));
    }

    /// One line of mrkdwn. Every character that starts something is ASCII, so the byte offsets
    /// this stops at are always character boundaries.
    fn styled(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let (mut at, mut kept) = (0, 0);
        while at < bytes.len() {
            let found = match bytes[at] {
                b'<' => line[at..].find('>').map(|close| (at + close + 1, 0u8)),
                b'`' => line[at + 1..].find('`').filter(|len| *len > 0).map(|len| (at + len + 2, b'`')),
                mark @ (b'*' | b'_' | b'~') => {
                    // A backslash keeps the mark, as in the shrug `¯\_(ツ)_/¯`.
                    if line[..at].ends_with('\\') {
                        None
                    } else {
                        closer(line, at, mark).map(|close| (close + 1, mark))
                    }
                }
                b':' => emoji_end(line, at).map(|close| (close + 1, b':')),
                _ => None,
            };
            let Some((next, what)) = found else {
                at += 1;
                continue;
            };
            self.plain(&line[kept..at]);
            let inner = &line[at + 1..next - 1];
            let start = self.at();
            match what {
                0 => self.reference(inner),
                b'`' => {
                    self.plain(inner);
                    self.span(start, Style::Code);
                }
                b':' => self.emoji(inner),
                mark => {
                    self.styled(inner);
                    let style = match mark {
                        b'*' => Style::Bold,
                        b'_' => Style::Italic,
                        _ => Style::Strike,
                    };
                    self.span(start, style);
                }
            }
            at = next;
            kept = next;
        }
        self.plain(&line[kept..]);
    }

    /// What stands between `<` and `>`: a person, a channel, a broadcast, a date or a link.
    fn reference(&mut self, inner: &str) {
        let (target, label) = match inner.split_once('|') {
            Some((target, label)) => (target, Some(label)),
            None => (inner, None),
        };
        match target.as_bytes().first() {
            Some(b'@') => self.user(&target[1..], label),
            Some(b'#') => self.channel(&target[1..], label),
            Some(b'!') => {
                let command = &target[1..];
                if let Some(group) = command.strip_prefix("subteam^") {
                    self.usergroup(group, label);
                } else if matches!(command, "here" | "channel" | "everyone" | "group") {
                    self.everyone(if command == "group" { "channel" } else { command });
                } else {
                    // `<!date^…|fallback>` and anything newer: the label is what to read.
                    self.plain(label.unwrap_or(command));
                }
            }
            _ => {
                let url = target.replace("&amp;", "&");
                let label = label.map(|label| label.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"));
                self.link(&url, label.as_deref());
            }
        }
    }
}

/// Where the style opened at `open` closes, if this really is a pair: it opens at the start of
/// a word and closes at the end of one, as Slack reads it.
fn closer(line: &str, open: usize, mark: u8) -> Option<usize> {
    let before = line[..open].chars().next_back();
    let first = line[open + 1..].chars().next()?;
    if before.is_some_and(word_like) || first.is_whitespace() || first as u32 == u32::from(mark) {
        return None;
    }
    let bytes = line.as_bytes();
    (open + 2..bytes.len()).find(|at| {
        bytes[*at] == mark
            && !line[..*at].ends_with('\\')
            && !line[..*at].ends_with(char::is_whitespace)
            && !line[at + 1..].chars().next().is_some_and(word_like)
    })
}

fn block_style(style: &Style) -> bool {
    matches!(
        style,
        Style::ListItem { .. } | Style::CodeBlock { .. } | Style::Quote
    )
}

/// Slack `rich_text` blocks for `text` and its spans. Empty when there is no formatting to send:
/// a plain message stays `text` alone. Reading the blocks back is the check that they match.
pub(super) fn rich_blocks(text: &str, spans: &[Span]) -> Vec<Value> {
    if text.is_empty() || spans.is_empty() {
        return Vec::new();
    }
    let elements = block_elements(text, spans);
    if elements.is_empty() {
        return Vec::new();
    }
    vec![json!({"type": "rich_text", "elements": elements})]
}

fn block_elements(text: &str, spans: &[Span]) -> Vec<Value> {
    let mut blocks: Vec<&Span> = spans
        .iter()
        .filter(|span| {
            block_style(&span.style)
                && text.is_char_boundary(span.start as usize)
                && text.is_char_boundary(span.end as usize)
                && span.start < span.end
                && (span.end as usize) <= text.len()
        })
        .collect();
    blocks.sort_by_key(|span| (span.start, span.end));
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut i = 0usize;
    let mut emitted = false;
    let mut ends_with_nl = false;
    while i < blocks.len() {
        let span = blocks[i];
        let start = span.start as usize;
        if start < at {
            i += 1;
            continue;
        }
        if let Some((from, to)) = gap_range(text, at, start, emitted, ends_with_nl) {
            push_section(&mut out, text, spans, from, to);
            emitted = true;
            ends_with_nl = text[from..to].ends_with('\n');
        }
        match &span.style {
            Style::ListItem { ordered, depth } => {
                let (ordered, depth) = (*ordered, *depth);
                let mut items = vec![(start, span.end as usize)];
                let mut end = span.end as usize;
                while let Some(next) = blocks.get(i + 1) {
                    let same = matches!(&next.style, Style::ListItem { ordered: next_ordered, depth: next_depth } if *next_ordered == ordered && *next_depth == depth);
                    let next_start = next.start as usize;
                    if same && text.get(end..next_start) == Some("\n") {
                        items.push((next_start, next.end as usize));
                        end = next.end as usize;
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(list(ordered, depth, text, spans, &items));
                at = end;
                emitted = true;
                ends_with_nl = text[start..end].ends_with('\n');
            }
            Style::CodeBlock { language } => {
                let end = span.end as usize;
                out.push(preformatted(text, spans, start, end, language));
                at = end;
                emitted = true;
                ends_with_nl = text[start..end].ends_with('\n');
            }
            Style::Quote => {
                let end = span.end as usize;
                out.push(json!({"type": "rich_text_quote", "elements": inline_elements(text, spans, start, end)}));
                at = end;
                emitted = true;
                ends_with_nl = text[start..end].ends_with('\n');
            }
            _ => {}
        }
        i += 1;
    }
    if let Some((from, to)) = gap_range(text, at, text.len(), emitted, ends_with_nl) {
        push_section(&mut out, text, spans, from, to);
    }
    out
}

/// Bytes of the gap that are content. One leading newline is the break the reader inserts
/// before a block when the text so far does not already end on one.
fn gap_range(
    text: &str,
    at: usize,
    start: usize,
    emitted: bool,
    ends_with_nl: bool,
) -> Option<(usize, usize)> {
    if at >= start {
        return None;
    }
    let mut from = at;
    if emitted && !ends_with_nl && text.as_bytes().get(from) == Some(&b'\n') {
        from += 1;
    }
    (from < start).then_some((from, start))
}

fn push_section(out: &mut Vec<Value>, text: &str, spans: &[Span], from: usize, to: usize) {
    let elements = inline_elements(text, spans, from, to);
    if !elements.is_empty() {
        out.push(json!({"type": "rich_text_section", "elements": elements}));
    }
}

fn list(ordered: bool, depth: u8, text: &str, spans: &[Span], items: &[(usize, usize)]) -> Value {
    let elements: Vec<Value> = items
        .iter()
        .map(|(start, end)| json!({"type": "rich_text_section", "elements": inline_elements(text, spans, *start, *end)}))
        .collect();
    let style = if ordered { "ordered" } else { "bullet" };
    let mut element = json!({"type": "rich_text_list", "style": style, "elements": elements});
    if depth > 0 {
        element["indent"] = json!(depth);
    }
    element
}

fn preformatted(text: &str, spans: &[Span], start: usize, end: usize, language: &str) -> Value {
    let mut element = json!({"type": "rich_text_preformatted", "elements": inline_elements(text, spans, start, end)});
    if !language.is_empty() {
        element["language"] = json!(language);
    }
    element
}

fn inline_elements(text: &str, spans: &[Span], from: usize, to: usize) -> Vec<Value> {
    if from >= to {
        return Vec::new();
    }
    let mut points = vec![from, to];
    for span in spans {
        if block_style(&span.style) {
            continue;
        }
        let start = span.start as usize;
        let end = span.end as usize;
        if (from..to).contains(&start) && text.is_char_boundary(start) {
            points.push(start);
        }
        if (from..to).contains(&end) && text.is_char_boundary(end) {
            points.push(end);
        }
    }
    points.sort_unstable();
    points.dedup();
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let Some(slice) = text.get(a..b).filter(|slice| !slice.is_empty()) else {
            continue;
        };
        let active: Vec<&Span> = spans
            .iter()
            .filter(|span| {
                !block_style(&span.style) && (span.start as usize) <= a && (span.end as usize) >= b
            })
            .collect();
        let exact: Vec<&Span> = active
            .iter()
            .copied()
            .filter(|span| span.start as usize == a && span.end as usize == b)
            .collect();
        out.push(element(slice, &active, &exact));
    }
    out
}

fn element(slice: &str, active: &[&Span], exact: &[&Span]) -> Value {
    let marked = |mut value: Value| {
        if let Some(style) = flags(active) {
            value["style"] = style;
        }
        value
    };
    if let Some(Style::Mention { kind, id }) = exact.iter().find_map(|span| match &span.style {
        style @ Style::Mention { .. } => Some(style),
        _ => None,
    }) {
        let value = match kind {
            MentionKind::User => json!({"type": "user", "user_id": id}),
            MentionKind::Channel => json!({"type": "channel", "channel_id": id}),
            MentionKind::Role => json!({"type": "usergroup", "usergroup_id": id}),
            MentionKind::Everyone => json!({"type": "broadcast", "range": id}),
        };
        return marked(value);
    }
    if let Some(Style::Link { url }) = exact.iter().find_map(|span| match &span.style {
        style @ Style::Link { .. } => Some(style),
        _ => None,
    }) {
        return marked(json!({"type": "link", "url": url, "text": slice}));
    }
    if let Some(Style::Emoji { .. }) = exact.iter().find_map(|span| match &span.style {
        style @ Style::Emoji { .. } => Some(style),
        _ => None,
    }) {
        let name = slice.trim_matches(':');
        if !name.is_empty() && !name.contains(':') {
            return marked(json!({"type": "emoji", "name": name}));
        }
    }
    if exact.is_empty() {
        if let Some(name) = emoji::name(slice) {
            if emoji::standard(&name).as_deref() == Some(slice) {
                return json!({"type": "emoji", "name": name});
            }
        }
    }
    marked(json!({"type": "text", "text": slice}))
}

fn flags(active: &[&Span]) -> Option<Value> {
    let mut style = serde_json::Map::new();
    for span in active {
        let key = match span.style {
            Style::Bold => "bold",
            Style::Italic => "italic",
            Style::Strike => "strike",
            Style::Underline => "underline",
            Style::Code => "code",
            _ => continue,
        };
        style.insert(key.to_owned(), Value::Bool(true));
    }
    (!style.is_empty()).then_some(Value::Object(style))
}

struct Ledger;

impl Names for Ledger {
    fn me(&self) -> &str {
        "UME"
    }
    fn user(&self, id: &str) -> String {
        format!("name-of-{id}")
    }
    fn channel(&self, id: &str) -> Option<String> {
        (id == "C9").then(|| "#platform".to_owned())
    }
    fn usergroup(&self, id: &str) -> Option<String> {
        (id == "S1").then(|| "oncall".to_owned())
    }
    fn custom(&self, name: &str) -> Option<Custom> {
        (name == "partyparrot").then(|| {
            Custom::File(protocol::MediaFile::new(
                "emoji:partyparrot",
                protocol::MediaKind::Emoji,
            ))
        })
    }
}

fn cover(text: &str, piece: &str, style: Style) -> Span {
    let start = text.find(piece).unwrap_or(0) as u32;
    Span {
        start,
        end: start + piece.len() as u32,
        style,
    }
}

fn round_trip(text: &str, spans: &[Span]) -> Result<(), String> {
    let blocks = rich_blocks(text, spans);
    if blocks.is_empty() {
        return Err("formatted text was sent as plain text".into());
    }
    let mut rich = Rich::new(&Ledger);
    if !rich.blocks(&blocks) {
        return Err("rich text blocks carried no text".into());
    }
    rich.finish();
    if rich.text != text {
        return Err(format!("sent {text:?} and read {:?}", rich.text));
    }
    if rich.spans != spans {
        return Err(format!("sent {spans:?} and read {:?}", rich.spans));
    }
    Ok(())
}

/// The lab's slack-rich-text driver. A build without this stays up and the driver fails.
/// The same blocks a send posts are what the reader turns back into text and spans.
pub(crate) fn prove_rich_text() -> Result<(), String> {
    let shaped = "Ship today 🚀 cc @name-of-UME @oncall #platform plan\nbuild\ndeploy\ncargo test\nlooks good";
    round_trip(
        shaped,
        &[
            cover(shaped, "today", Style::Bold),
            cover(shaped, "today", Style::Italic),
            cover(
                shaped,
                "@name-of-UME",
                Style::Mention {
                    kind: MentionKind::User,
                    id: "UME".into(),
                },
            ),
            cover(
                shaped,
                "@oncall",
                Style::Mention {
                    kind: MentionKind::Role,
                    id: "S1".into(),
                },
            ),
            cover(
                shaped,
                "#platform",
                Style::Mention {
                    kind: MentionKind::Channel,
                    id: "C9".into(),
                },
            ),
            cover(
                shaped,
                "plan",
                Style::Link {
                    url: "https://ex.com".into(),
                },
            ),
            cover(
                shaped,
                "build",
                Style::ListItem {
                    ordered: true,
                    depth: 1,
                },
            ),
            cover(
                shaped,
                "deploy",
                Style::ListItem {
                    ordered: true,
                    depth: 1,
                },
            ),
            cover(
                shaped,
                "cargo test",
                Style::CodeBlock {
                    language: "rust".into(),
                },
            ),
            cover(shaped, "looks good", Style::Quote),
        ],
    )?;
    let linked = "see the doc";
    round_trip(
        linked,
        &[cover(
            linked,
            "the doc",
            Style::Link {
                url: "https://ex.com/a?b=1&c=2".into(),
            },
        )],
    )?;
    let nested = "one\ntwo\nchild";
    round_trip(
        nested,
        &[
            cover(
                nested,
                "one",
                Style::ListItem {
                    ordered: false,
                    depth: 0,
                },
            ),
            cover(
                nested,
                "two",
                Style::ListItem {
                    ordered: false,
                    depth: 0,
                },
            ),
            cover(
                nested,
                "child",
                Style::ListItem {
                    ordered: false,
                    depth: 1,
                },
            ),
        ],
    )?;
    let custom = "wave :partyparrot:";
    round_trip(
        custom,
        &[cover(
            custom,
            ":partyparrot:",
            Style::Emoji {
                file: protocol::MediaFile::new("emoji:partyparrot", protocol::MediaKind::Emoji),
            },
        )],
    )?;
    Ok(())
}

/// The closing colon of an emoji name opened at `open`.
pub(super) fn emoji_end(line: &str, open: usize) -> Option<usize> {
    let rest = &line[open + 1..];
    let len = rest.find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "_+-'".contains(c)))?;
    let mut close = open + 1 + len;
    if len == 0 || line.as_bytes()[close] != b':' {
        return None;
    }
    // `:+1::skin-tone-3:` is one emoji.
    if let Some(tone) = line[close + 1..].strip_prefix(":skin-tone-") {
        if tone.as_bytes().get(1) == Some(&b':') && tone.as_bytes()[0].is_ascii_digit() {
            close += ":skin-tone-N:".len();
        }
    }
    Some(close)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;
    use serde_json::json;

    struct Book;

    impl Names for Book {
        fn me(&self) -> &str {
            "UME"
        }
        fn user(&self, id: &str) -> String {
            format!("name-of-{id}")
        }
        fn channel(&self, id: &str) -> Option<String> {
            (id == "C9").then(|| "#platform".to_owned())
        }
        fn usergroup(&self, id: &str) -> Option<String> {
            match id {
                "S1" => Some("oncall".to_owned()),
                "S2" => Some("design".to_owned()),
                _ => None,
            }
        }
        fn in_usergroup(&self, id: &str) -> bool {
            id == "S1"
        }
        fn custom(&self, name: &str) -> Option<Custom> {
            (name == "partyparrot").then(|| Custom::File(MediaFile::new("emoji:partyparrot", MediaKind::Emoji)))
        }
    }

    fn ranges(rich: &Rich) -> Vec<(String, Style)> {
        let cut = |span: &Span| rich.text[span.start as usize..span.end as usize].to_owned();
        rich.spans.iter().map(|span| (cut(span), span.style.clone())).collect()
    }

    #[test]
    fn a_backslash_keeps_the_following_mark() {
        let mut rich = Rich::new(&Book);
        rich.mrkdwn("hello ¯\\_(ツ)_/¯");
        rich.finish();
        assert_eq!(rich.text, "hello ¯\\_(ツ)_/¯");
        assert!(rich.spans.iter().all(|span| !matches!(span.style, Style::Italic)));
    }

    #[test]
    fn mrkdwn_becomes_text_with_ranges_and_no_markup_left() {
        let mut rich = Rich::new(&Book);
        rich.mrkdwn(
            "<@U1> see <https://ex.com/a?b=1&amp;c=2|the doc> in <#C9|platform> <!here> a &lt; b \
             *bold _both_* ~gone~ `x*y` :tada: :partyparrot: :nope: 2*3*4 snake_case_name <https://ex.com>",
        );
        rich.finish();
        assert_eq!(
            rich.text,
            "@name-of-U1 see the doc in #platform @here a < b bold both gone x*y 🎉 :partyparrot: :nope: \
             2*3*4 snake_case_name https://ex.com"
        );
        let found = ranges(&rich);
        let has = |text: &str, style: Style| found.contains(&(text.to_owned(), style));
        assert!(has("@name-of-U1", Style::Mention { kind: MentionKind::User, id: "U1".into() }));
        assert!(has("the doc", Style::Link { url: "https://ex.com/a?b=1&c=2".into() }));
        assert!(has("#platform", Style::Mention { kind: MentionKind::Channel, id: "C9".into() }));
        assert!(has("@here", Style::Mention { kind: MentionKind::Everyone, id: "here".into() }));
        assert!(has("bold both", Style::Bold));
        assert!(has("both", Style::Italic));
        assert!(has("gone", Style::Strike));
        assert!(has("x*y", Style::Code));
        assert!(found.iter().any(|(text, style)| text == ":partyparrot:" && matches!(style, Style::Emoji { .. })));
        assert!(rich.broadcast && !rich.mentioned);
        // Every range sits on character boundaries of the text it describes.
        for span in &rich.spans {
            assert!(rich.text.is_char_boundary(span.start as usize) && rich.text.is_char_boundary(span.end as usize));
        }
    }

    #[test]
    fn quotes_code_blocks_and_my_own_mention() {
        let mut rich = Rich::new(&Book);
        rich.mrkdwn("&gt; said <@UME>\nthen\n```let a = 1 &lt; 2;\n*not bold*```\nafter :+1::skin-tone-3:");
        rich.finish();
        assert_eq!(rich.text, "said @name-of-UME\nthen\nlet a = 1 < 2;\n*not bold*\nafter 👍🏼");
        let found = ranges(&rich);
        assert!(found.contains(&("said @name-of-UME".into(), Style::Quote)));
        assert!(found.contains(&("let a = 1 < 2;\n*not bold*".into(), Style::CodeBlock { language: String::new() })));
        assert!(rich.mentioned);
    }

    #[test]
    fn rich_text_blocks_keep_their_structure() {
        let blocks = json!([{"type": "rich_text", "elements": [
            {"type": "rich_text_section", "elements": [
                {"type": "text", "text": "Ship "},
                {"type": "text", "text": "today", "style": {"bold": true, "italic": true}},
                {"type": "text", "text": " "},
                {"type": "emoji", "name": "rocket", "unicode": "1f680"},
                {"type": "text", "text": " cc "},
                {"type": "user", "user_id": "UME"},
                {"type": "text", "text": " "},
                {"type": "usergroup", "usergroup_id": "S1"},
                {"type": "text", "text": " "},
                {"type": "link", "url": "https://ex.com", "text": "plan"},
                {"type": "text", "text": "\n"}
            ]},
            {"type": "rich_text_list", "style": "ordered", "indent": 1, "elements": [
                {"type": "rich_text_section", "elements": [{"type": "text", "text": "build"}]},
                {"type": "rich_text_section", "elements": [{"type": "text", "text": "deploy"}]}
            ]},
            {"type": "rich_text_preformatted", "elements": [{"type": "text", "text": "cargo test"}]},
            {"type": "rich_text_quote", "elements": [{"type": "text", "text": "looks good"}]}
        ]}]);
        let mut rich = Rich::new(&Book);
        assert!(rich.blocks(blocks.as_array().unwrap()));
        rich.finish();
        assert_eq!(
            rich.text,
            "Ship today 🚀 cc @name-of-UME @oncall plan\nbuild\ndeploy\ncargo test\nlooks good"
        );
        let found = ranges(&rich);
        let has = |text: &str, style: Style| found.contains(&(text.to_owned(), style));
        assert!(has("today", Style::Bold) && has("today", Style::Italic));
        assert!(has("@oncall", Style::Mention { kind: MentionKind::Role, id: "S1".into() }));
        assert!(has("plan", Style::Link { url: "https://ex.com".into() }));
        assert!(has("build", Style::ListItem { ordered: true, depth: 1 }));
        assert!(has("deploy", Style::ListItem { ordered: true, depth: 1 }));
        assert!(has("cargo test", Style::CodeBlock { language: String::new() }));
        assert!(has("looks good", Style::Quote));
        assert!(rich.mentioned && !rich.undrawn);
    }

    #[test]
    fn block_kit_text_is_read_and_controls_are_noted() {
        let blocks = json!([
            {"type": "header", "text": {"type": "plain_text", "text": "Deploy #42"}},
            {"type": "section", "text": {"type": "mrkdwn", "text": "*main* is green"},
             "fields": [{"type": "mrkdwn", "text": "Took: 4 min"}]},
            {"type": "actions", "elements": [{"type": "button", "text": {"type": "plain_text", "text": "Roll back"}}]}
        ]);
        let mut rich = Rich::new(&Book);
        assert!(rich.blocks(blocks.as_array().unwrap()));
        rich.finish();
        assert_eq!(rich.text, "Deploy #42\nmain is green\nTook: 4 min");
        assert!(rich.undrawn);
    }

    #[test]
    fn sending_rich_text_reads_back_as_the_same_text_and_spans() {
        super::prove_rich_text().unwrap();
    }

    #[test]
    fn usergroup_mention_marks_mentioned_only_for_groups_i_belong_to() {
        let mut mine = Rich::new(&Book);
        mine.mrkdwn("ping <!subteam^S1>");
        mine.finish();
        assert_eq!(mine.text, "ping @oncall");
        assert!(mine.mentioned, "S1 is my group");

        let mut other = Rich::new(&Book);
        other.mrkdwn("ping <!subteam^S2>");
        other.finish();
        assert_eq!(other.text, "ping @design");
        assert!(!other.mentioned, "S2 is not my group");

        let blocks = json!([{"type": "rich_text", "elements": [
            {"type": "rich_text_section", "elements": [{"type": "usergroup", "usergroup_id": "S1"}]}
        ]}]);
        let mut rich_mine = Rich::new(&Book);
        assert!(rich_mine.blocks(blocks.as_array().unwrap()));
        assert!(rich_mine.mentioned);

        let other_blocks = json!([{"type": "rich_text", "elements": [
            {"type": "rich_text_section", "elements": [{"type": "usergroup", "usergroup_id": "S2"}]}
        ]}]);
        let mut rich_other = Rich::new(&Book);
        assert!(rich_other.blocks(other_blocks.as_array().unwrap()));
        assert!(!rich_other.mentioned);
    }
}
