//! A Teams message body, which is HTML, as plain text and styled ranges. The markup is read
//! token by token and never rendered: an element this does not know contributes its text and
//! nothing else, and no address in it is ever fetched but Teams' own and the GIFs its Giphy
//! button puts in.

use protocol::{MediaFile, MentionKind, Span, Style};

const MENTION: &str = "http://schema.skype.com/Mention";
const REPLY: &str = "http://schema.skype.com/Reply";
const EMOJI: &str = "http://schema.skype.com/Emoji";
const CUSTOM_EMOJI: &str = "http://schema.skype.com/CustomEmoji";
const GIPHY: &str = "http://schema.skype.com/Giphy";

/// What the text cannot say for itself, answered by the adapter.
pub(super) trait Lookup {
    /// Who or what the mention with this `itemid` names, from the message's `properties.mentions`.
    fn mention(&self, item: &str) -> Option<(MentionKind, String)>;
    /// True for an address Teams itself serves pictures from.
    fn hosted(&self, url: &str) -> bool;
    /// A custom emoji's picture, registered for fetching.
    fn emoji(&self, id: &str, url: &str) -> Option<MediaFile>;
}

/// A picture pasted into a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Image {
    pub src: String,
    pub width: u32,
    pub height: u32,
    pub name: String,
}

/// The message a chat reply quotes at its top.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Quote {
    pub id: String,
    pub mri: String,
    pub name: String,
    pub preview: String,
}

#[derive(Debug, Default)]
pub(super) struct Parsed {
    pub text: String,
    pub spans: Vec<Span>,
    pub images: Vec<Image>,
    /// GIFs from Teams' Giphy button, on Giphy's own host.
    pub gifs: Vec<Image>,
    pub quote: Option<Quote>,
    /// It holds a card or another object that is not text.
    pub card: bool,
    /// A mention in it named us.
    pub mentioned: bool,
}

enum Token<'a> {
    Text(&'a str),
    Open {
        name: String,
        attrs: Vec<(String, String)>,
        /// `<br/>`: nothing follows inside it.
        closed: bool,
    },
    Close(String),
}

/// Appends `text` with its character references read.
fn decode(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let named = tail.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &tail[..end];
            let found = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                _ => {
                    let digits = name.strip_prefix('#')?;
                    let code = match digits.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => digits.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((found, end))
        });
        match named {
            Some((found, end)) => {
                out.push(found);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
}

fn decoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    decode(text, &mut out);
    out
}

/// `name="value"` pairs of a tag's inside, names in lower case.
fn attributes(mut rest: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return found;
        }
        let end = rest.find(|c: char| c.is_whitespace() || c == '=' || c == '/').unwrap_or(rest.len());
        let name = rest[..end].to_ascii_lowercase();
        rest = rest[end..].trim_start();
        let Some(after) = rest.strip_prefix('=') else {
            found.push((name, String::new()));
            continue;
        };
        let after = after.trim_start();
        let (value, tail) = match after.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let inner = &after[1..];
                let end = inner.find(quote).unwrap_or(inner.len());
                (&inner[..end], inner.get(end + 1..).unwrap_or(""))
            }
            _ => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        found.push((name, decoded(value)));
        rest = tail;
    }
}

/// The end of the tag that starts at the `<` opening `text`, quotes respected.
fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (None, '>') => return Some(at),
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if open == c => quote = None,
            _ => {}
        }
    }
    None
}

fn next(rest: &str) -> Option<(Token<'_>, &str)> {
    if rest.is_empty() {
        return None;
    }
    if let Some(comment) = rest.strip_prefix("<!--") {
        let end = comment.find("-->").map_or(comment.len(), |end| end + 3);
        return Some((Token::Text(""), &comment[end..]));
    }
    let tag = rest.strip_prefix('<').filter(|inside| inside.starts_with(|c: char| c == '/' || c.is_ascii_alphabetic()));
    if let Some(end) = tag.and_then(|_| tag_end(rest)) {
        let (inside, after) = (&rest[1..end], &rest[end + 1..]);
        if let Some(name) = inside.strip_prefix('/') {
            return Some((Token::Close(name.trim().to_ascii_lowercase()), after));
        }
        let cut = inside.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(inside.len());
        let token = Token::Open {
            name: inside[..cut].to_ascii_lowercase(),
            attrs: attributes(&inside[cut..]),
            closed: inside.ends_with('/'),
        };
        return Some((token, after));
    }
    // A `<` that opens no tag is a character like any other.
    let end = rest.char_indices().skip(1).find(|(_, c)| *c == '<').map_or(rest.len(), |(end, _)| end);
    Some((Token::Text(&rest[..end]), &rest[end..]))
}

/// Where text goes while a quoted reply's header is being read.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Name,
    Preview,
    Nowhere,
}

struct Element {
    tag: String,
    start: usize,
    /// Spans recorded before this element opened.
    spans: usize,
    style: Option<Style>,
    mention: bool,
    field: Option<Field>,
    /// Ends a quoted reply, or something whose inside is not shown.
    hides: bool,
    block: bool,
}

struct Builder<'a> {
    lookup: &'a dyn Lookup,
    out: Parsed,
    open: Vec<Element>,
    /// A line break is owed before the next thing said.
    due: bool,
    /// Inside `pre`: spaces and line ends are kept.
    verbatim: u32,
    /// Inside something not shown.
    hidden: u32,
    quote: Option<Quote>,
}

const BLOCKS: &[&str] = &[
    "p", "div", "ul", "ol", "li", "blockquote", "pre", "codeblock", "table", "tr", "hr", "h1", "h2",
    "h3", "h4", "h5", "h6",
];
/// Elements that never have an inside.
const VOID: &[&str] = &["br", "img", "hr", "input", "meta", "link"];

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> &'a str {
    attrs.iter().find(|(known, _)| known == name).map_or("", |(_, value)| value.as_str())
}

/// A length from `width="250"`, or from `style="width:250px"` when the attribute is missing.
fn pixels(attrs: &[(String, String)], name: &str) -> u32 {
    let number = |text: &str| {
        let digits: String = text.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        digits.parse::<f64>().ok().filter(|size| *size >= 1.).map(|size| size.min(65_535.) as u32)
    };
    let styled = || {
        attr(attrs, "style").split(';').find_map(|rule| {
            let (property, value) = rule.split_once(':')?;
            (property.trim().eq_ignore_ascii_case(name)).then(|| number(value)).flatten()
        })
    };
    number(attr(attrs, name)).or_else(styled).unwrap_or(0)
}

/// An address a link may carry: the web, mail, or a Teams deep link.
/// An address on Giphy's own host, over HTTPS.
pub(super) fn giphy(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#', ':']).next().unwrap_or("").to_ascii_lowercase();
    host == "giphy.com" || host.ends_with(".giphy.com")
}

fn linkable(url: &str) -> bool {
    ["https://", "http://", "mailto:", "msteams:"].iter().any(|scheme| {
        url.get(..scheme.len()).is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

impl Builder<'_> {
    fn at(&self) -> usize {
        self.out.text.len()
    }

    fn line(&mut self) {
        self.due = true;
    }

    /// Puts the owed line break in, unless the text is already at the start of a line.
    fn settle(&mut self) {
        if std::mem::take(&mut self.due) && !self.out.text.is_empty() && !self.out.text.ends_with('\n') {
            self.out.text.push('\n');
        }
    }

    fn say(&mut self, text: &str) {
        if let Some(quote) = &mut self.quote {
            match self.open.iter().rev().find_map(|element| element.field) {
                Some(Field::Name) => quote.name.push_str(text),
                Some(Field::Preview) => quote.preview.push_str(text),
                _ => {}
            }
            return;
        }
        if self.hidden > 0 {
            return;
        }
        if self.verbatim > 0 {
            self.settle();
            self.out.text.extend(text.chars().filter(|c| *c != '\r'));
            return;
        }
        for c in text.chars() {
            if c.is_ascii_whitespace() {
                let text = &self.out.text;
                if !(self.due || text.is_empty() || text.ends_with([' ', '\n'])) {
                    self.out.text.push(' ');
                }
            } else {
                self.settle();
                // A non-breaking space is one the writer meant.
                self.out.text.push(if c == '\u{a0}' { ' ' } else { c });
            }
        }
    }

    fn span(&mut self, start: usize, style: Style) {
        let end = self.out.text.trim_end().len();
        if end > start {
            self.out.spans.push(Span {
                start: start as u32,
                end: end as u32,
                style,
            });
        }
    }

    fn image(&mut self, attrs: &[(String, String)]) {
        let (kind, src, alt) = (attr(attrs, "itemtype"), attr(attrs, "src"), attr(attrs, "alt"));
        if kind == EMOJI {
            self.say(alt);
            return;
        }
        if kind == CUSTOM_EMOJI {
            let name = format!(":{}:", alt.trim_matches(':'));
            let file = self.lookup.emoji(attr(attrs, "itemid"), src).filter(|_| self.quote.is_none() && self.hidden == 0);
            self.settle();
            let start = self.at();
            self.say(&name);
            if let Some(file) = file {
                self.span(start, Style::Emoji { file });
            }
            return;
        }
        if self.quote.is_some() || self.hidden > 0 {
            return;
        }
        if kind == GIPHY && giphy(src) {
            // Teams' own client loads these from Giphy too, so fetching one says no more than
            // reading the message there does.
            self.out.gifs.push(Image {
                src: src.to_owned(),
                width: pixels(attrs, "width"),
                height: pixels(attrs, "height"),
                name: alt.to_owned(),
            });
            return;
        }
        if self.lookup.hosted(src) {
            self.out.images.push(Image {
                src: src.to_owned(),
                width: pixels(attrs, "width"),
                height: pixels(attrs, "height"),
                name: alt.to_owned(),
            });
            return;
        }
        // A picture from anywhere else is a link: nothing outside Teams is fetched for a
        // message.
        self.settle();
        let start = self.at();
        self.say(if alt.trim().is_empty() { "Image" } else { alt });
        if linkable(src) {
            self.span(start, Style::Link { url: src.to_owned() });
        }
    }

    fn list_item(&self) -> Style {
        let lists = || self.open.iter().filter(|element| matches!(element.tag.as_str(), "ul" | "ol"));
        Style::ListItem {
            ordered: lists().next_back().is_some_and(|list| list.tag == "ol"),
            depth: lists().count().saturating_sub(1).min(8) as u8,
        }
    }

    fn opened(&mut self, name: String, attrs: &[(String, String)], closed: bool) {
        let block = BLOCKS.contains(&name.as_str());
        if block {
            self.line();
        }
        let kind = attr(attrs, "itemtype");
        match name.as_str() {
            "br" if self.quote.is_none() && self.hidden == 0 => {
                self.due = false;
                self.out.text.push('\n');
            }
            "img" => self.image(attrs),
            // A cell after the first on its row.
            "td" | "th" if !(self.due || self.out.text.is_empty() || self.out.text.ends_with('\n')) => self.say("\u{a0}|\u{a0}"),
            _ => {}
        }
        if closed || VOID.contains(&name.as_str()) {
            return;
        }
        let mut element = Element {
            start: 0,
            spans: 0,
            style: None,
            mention: false,
            field: None,
            hides: false,
            block,
            tag: name,
        };
        if self.quote.is_some() {
            element.field = match attr(attrs, "itemprop") {
                "mri" => {
                    if let Some(quote) = &mut self.quote {
                        quote.mri = attr(attrs, "itemid").to_owned();
                    }
                    Some(Field::Name)
                }
                "preview" => Some(Field::Preview),
                "time" => Some(Field::Nowhere),
                _ => None,
            };
            self.open.push(element);
            return;
        }
        let in_pre = self.verbatim > 0;
        element.style = match element.tag.as_str() {
            "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(Style::Bold),
            "i" | "em" => Some(Style::Italic),
            "u" => Some(Style::Underline),
            "s" | "strike" | "del" => Some(Style::Strike),
            "code" if !in_pre => Some(Style::Code),
            "pre" | "codeblock" => {
                self.verbatim += 1;
                let class = attr(attrs, "class");
                let language = class.split_whitespace().next().unwrap_or("");
                let language = language.strip_prefix("language-").unwrap_or(language);
                (!in_pre).then(|| Style::CodeBlock {
                    language: language.to_ascii_lowercase(),
                })
            }
            "blockquote" if kind == REPLY => {
                self.quote = Some(Quote {
                    id: attr(attrs, "itemid").to_owned(),
                    ..Quote::default()
                });
                element.hides = true;
                None
            }
            "blockquote" => Some(Style::Quote),
            "li" => Some(self.list_item()),
            "a" => Some(attr(attrs, "href")).filter(|url| linkable(url)).map(|url| Style::Link { url: url.to_owned() }),
            "span" | "at" if kind == MENTION || element.tag == "at" => {
                element.mention = true;
                let item = if element.tag == "at" { attr(attrs, "id") } else { attr(attrs, "itemid") };
                self.lookup.mention(item).map(|(kind, id)| Style::Mention { kind, id })
            }
            "uriobject" | "swift" | "script" | "style" | "title" | "head" => {
                self.out.card |= matches!(element.tag.as_str(), "uriobject" | "swift");
                self.hidden += 1;
                element.hides = true;
                None
            }
            _ => None,
        };
        if element.style.is_some() || element.mention {
            self.settle();
        }
        element.start = self.at();
        element.spans = self.out.spans.len();
        self.open.push(element);
    }

    fn closed(&mut self, name: &str) {
        // An end tag with nothing open to end is noise.
        let Some(depth) = self.open.iter().rposition(|element| element.tag == name) else {
            return;
        };
        while self.open.len() > depth {
            let Some(element) = self.open.pop() else {
                return;
            };
            if element.hides {
                match self.quote.take() {
                    Some(quote) if element.tag == "blockquote" => self.out.quote = Some(quote),
                    quote => {
                        self.quote = quote;
                        self.hidden = self.hidden.saturating_sub(1);
                    }
                }
            }
            if matches!(element.tag.as_str(), "pre" | "codeblock") {
                self.verbatim = self.verbatim.saturating_sub(1);
            }
            if element.mention {
                self.mentioned(&element);
            }
            if let Some(style) = element.style {
                self.span(element.start, style);
            }
            if element.block {
                self.line();
            }
        }
    }

    /// A mention reads `@Name`. Teams writes the name alone, so the sign is put in front and
    /// everything recorded inside the mention moves over by it.
    fn mentioned(&mut self, element: &Element) {
        let start = element.start;
        if self.out.text.len() <= start || self.out.text[start..].starts_with('@') {
            return;
        }
        self.out.text.insert(start, '@');
        for span in &mut self.out.spans[element.spans..] {
            span.start += 1;
            span.end += 1;
        }
    }

    fn finish(mut self, me: &str) -> Parsed {
        while let Some(element) = self.open.last() {
            let tag = element.tag.clone();
            self.closed(&tag);
        }
        let end = self.out.text.trim_end().len();
        self.out.text.truncate(end);
        for span in &mut self.out.spans {
            span.end = span.end.min(end as u32);
        }
        self.out.spans.retain(|span| span.start < span.end);
        let names_me = |span: &Span| match &span.style {
            Style::Mention { kind, id } => *kind == MentionKind::Everyone || (!me.is_empty() && id == me),
            _ => false,
        };
        self.out.mentioned = self.out.spans.iter().any(names_me);
        if let Some(quote) = &mut self.out.quote {
            quote.name = quote.name.trim().to_owned();
            quote.preview = quote.preview.trim().to_owned();
        }
        self.out
    }
}

/// Reads a message body. `me` is our own id, to know a mention of us.
pub(super) fn parse(html: &str, lookup: &dyn Lookup, me: &str) -> Parsed {
    let mut builder = Builder {
        lookup,
        out: Parsed::default(),
        open: Vec::new(),
        due: false,
        verbatim: 0,
        hidden: 0,
        quote: None,
    };
    let mut rest = html;
    let mut text = String::new();
    while let Some((token, after)) = next(rest) {
        rest = after;
        match token {
            Token::Text(raw) => {
                text.clear();
                decode(raw, &mut text);
                builder.say(&text);
            }
            Token::Open { name, attrs, closed } => builder.opened(name, &attrs, closed),
            Token::Close(name) => builder.closed(&name),
        }
    }
    builder.finish(me)
}

/// An outgoing mention parsed from the composer's wire markers (`<@id|Name>`, `<@&id|Name>`, `<#id|Name>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutMention {
    pub itemid: usize,
    pub mri: String,
    pub kind: &'static str,
    pub display_name: String,
}

/// Serializes outgoing mentions as the JSON string Teams expects in `properties.mentions`.
pub(super) fn mentions_property(mentions: &[OutMention]) -> String {
    let entries: Vec<serde_json::Value> = mentions
        .iter()
        .map(|mention| {
            serde_json::json!({
                "@type": MENTION,
                "itemid": mention.itemid,
                "mri": mention.mri,
                "mentionType": mention.kind,
                "displayName": mention.display_name,
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

struct ComposedLookup<'a>(&'a [OutMention]);

impl Lookup for ComposedLookup<'_> {
    fn mention(&self, item: &str) -> Option<(MentionKind, String)> {
        let itemid: usize = item.parse().ok()?;
        let found = self.0.iter().find(|mention| mention.itemid == itemid)?;
        let kind = match found.kind {
            "person" | "bot" => MentionKind::User,
            "tag" => MentionKind::Role,
            _ => MentionKind::Everyone,
        };
        Some((kind, found.mri.clone()))
    }

    fn hosted(&self, _: &str) -> bool {
        false
    }

    fn emoji(&self, _: &str, _: &str) -> Option<MediaFile> {
        None
    }
}

/// The text and spans a sent message shows. The same reading as Teams gives the HTML we post.
pub(super) fn styled(text: &str) -> (String, Vec<Span>) {
    let (html, mentions) = compose_rich(text);
    let parsed = parse(&html, &ComposedLookup(&mentions), "");
    (parsed.text, parsed.spans)
}

/// Bold, a list, a code block and a link, read back from the HTML `compose` posts.
pub(super) fn prove_markdown() -> Result<(), String> {
    let (text, spans) = styled("**done**\n- first\n- second\n```rs\nlet a = 1;\n```\n[plan](https://example.com/a)");
    if text != "done\nfirst\nsecond\nlet a = 1;\nplan" {
        return Err(format!("text {text:?}"));
    }
    let has = |needle: &str, style: Style| {
        spans.iter().any(|span| text.get(span.start as usize..span.end as usize) == Some(needle) && span.style == style)
    };
    if !has("done", Style::Bold) {
        return Err("bold".to_owned());
    }
    let item = Style::ListItem { ordered: false, depth: 0 };
    if !has("first", item.clone()) || !has("second", item) {
        return Err("list".to_owned());
    }
    if !has("let a = 1;", Style::CodeBlock { language: "rs".to_owned() }) {
        return Err("code".to_owned());
    }
    if !has("plan", Style::Link { url: "https://example.com/a".to_owned() }) {
        return Err("link".to_owned());
    }
    Ok(())
}

struct Item {
    depth: u8,
    ordered: bool,
    body: String,
}

enum Block {
    Para(String),
    Quote(String),
    List(Vec<Item>),
    Code { language: String, body: String },
}

fn normalized(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "")
}

/// Escapes text for HTML, with line ends as breaks. Not markdown: a quote preview is already plain.
pub(super) fn escape_plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_text(&mut out, &normalized(text), true);
    out
}

fn escape_text(out: &mut String, text: &str, breaks: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' if breaks => out.push_str("<br>"),
            '\n' => out.push('\n'),
            c => out.push(c),
        }
    }
}

fn closing(text: &str, from: usize, delimiter: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] == b'\\' {
            at += 1;
            at += text[at..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if text[at..].starts_with(delimiter) {
            return (at > from).then_some(at);
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// A lone `_`, not half of `__`. `__word__` is not underline and not italic.
fn closing_underscore(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] == b'\\' {
            at += 1;
            at += text[at..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        let single = text[at..].starts_with('_') && !text[at..].starts_with("__") && !text[..at].ends_with('_');
        if single {
            return (at > from).then_some(at);
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn edged(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_whitespace) || text.chars().next_back().is_some_and(char::is_whitespace)
}

fn take_wrap(out: &mut String, text: &str, delimiter: &str, open: &str, close: &str, mentions: &mut Vec<OutMention>) -> Option<usize> {
    if !text.starts_with(delimiter) {
        return None;
    }
    let close_at = closing(text, delimiter.len(), delimiter)?;
    let inner = &text[delimiter.len()..close_at];
    if inner.is_empty() || edged(inner) {
        return None;
    }
    out.push_str(open);
    inline_into(out, inner, mentions);
    out.push_str(close);
    Some(close_at + delimiter.len())
}

fn take_code(out: &mut String, text: &str) -> Option<usize> {
    let delimiter = if text.starts_with("``") { "``" } else if text.starts_with('`') { "`" } else { return None };
    let close_at = closing(text, delimiter.len(), delimiter)?;
    let inner = &text[delimiter.len()..close_at];
    if inner.contains('\n') {
        return None;
    }
    out.push_str("<code>");
    escape_text(out, inner, false);
    out.push_str("</code>");
    Some(close_at + delimiter.len())
}

fn allowed_href(url: &str) -> bool {
    let schemes = ["https://", "http://", "mailto:"];
    let scheme = schemes.iter().any(|scheme| {
        url.get(..scheme.len()).is_some_and(|start| start.eq_ignore_ascii_case(scheme)) && url.len() > scheme.len()
    });
    scheme && !url.chars().any(|c| c.is_ascii_whitespace() || matches!(c, '"' | '<' | '>' | '\u{0}'))
}

fn take_link(out: &mut String, text: &str, mentions: &mut Vec<OutMention>) -> Option<usize> {
    if !text.starts_with('[') {
        return None;
    }
    let label_end = closing(text, 1, "](")?;
    let target = &text[label_end + 2..];
    let target_end = target.find(')')?;
    let url = target[..target_end].trim();
    if !allowed_href(url) {
        return None;
    }
    let label = &text[1..label_end];
    if label.is_empty() || label.contains('\n') {
        return None;
    }
    out.push_str("<a href=\"");
    escape_text(out, url, false);
    out.push_str("\">");
    inline_into(out, label, mentions);
    out.push_str("</a>");
    Some(label_end + 2 + target_end + 1)
}

fn bare_end(text: &str) -> usize {
    let mut end = text.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"')).unwrap_or(text.len());
    while let Some(last) = text[..end].chars().next_back() {
        let unmatched = last == ')' && !text[..end].contains('(');
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'') || unmatched {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn take_autolink(out: &mut String, text: &str, after_word: bool) -> Option<usize> {
    if after_word {
        return None;
    }
    let schemes = ["https://", "http://", "mailto:"];
    let scheme = schemes.iter().find(|scheme| {
        text.get(..scheme.len()).is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })?;
    let end = bare_end(text);
    if end <= scheme.len() || !allowed_href(&text[..end]) {
        return None;
    }
    let url = &text[..end];
    out.push_str("<a href=\"");
    escape_text(out, url, false);
    out.push_str("\">");
    escape_text(out, url, false);
    out.push_str("</a>");
    Some(end)
}

fn take_mention(out: &mut String, text: &str, mentions: &mut Vec<OutMention>) -> Option<usize> {
    let (prefix_len, kind_hint) = if text.starts_with("<@&") {
        (3, "tag")
    } else if text.starts_with("<@") {
        (2, "person")
    } else if text.starts_with("<#") {
        (2, "channel")
    } else {
        return None;
    };
    let close_at = text[prefix_len..].find('>')? + prefix_len;
    let inside = &text[prefix_len..close_at];
    if inside.contains(['\n', '<']) {
        return None;
    }
    let (id, label) = inside.split_once('|')?;
    if id.is_empty() || label.is_empty() || id.chars().any(char::is_whitespace) {
        return None;
    }
    let kind = if kind_hint == "person" && id.starts_with("28:") { "bot" } else { kind_hint };
    let itemid = mentions.len();
    mentions.push(OutMention {
        itemid,
        mri: id.to_owned(),
        kind,
        display_name: label.to_owned(),
    });
    out.push_str("<span itemscope=\"\" itemtype=\"");
    out.push_str(MENTION);
    out.push_str("\" itemid=\"");
    out.push_str(&itemid.to_string());
    out.push_str("\">");
    escape_text(out, label, false);
    out.push_str("</span>");
    Some(close_at + 1)
}

fn take_underscore(out: &mut String, text: &str, after_word: bool, mentions: &mut Vec<OutMention>) -> Option<usize> {
    if after_word || !text.starts_with('_') || text.starts_with("__") {
        return None;
    }
    let close_at = closing_underscore(text, 1)?;
    let inner = &text[1..close_at];
    if inner.is_empty() || edged(inner) {
        return None;
    }
    if text[close_at + 1..].chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    out.push_str("<em>");
    inline_into(out, inner, mentions);
    out.push_str("</em>");
    Some(close_at + 1)
}

fn inline_into(out: &mut String, text: &str, mentions: &mut Vec<OutMention>) {
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let after_word = text[..at].chars().next_back().is_some_and(char::is_alphanumeric);
        if let Some(escaped) = rest.strip_prefix('\\').and_then(|rest| rest.chars().next()).filter(char::is_ascii_punctuation) {
            escape_text(out, &escaped.to_string(), true);
            at += 1 + escaped.len_utf8();
            continue;
        }
        let taken = take_code(out, rest)
            .or_else(|| take_mention(out, rest, mentions))
            .or_else(|| take_wrap(out, rest, "~~", "<s>", "</s>", mentions))
            .or_else(|| take_wrap(out, rest, "***", "<strong><em>", "</em></strong>", mentions))
            .or_else(|| take_wrap(out, rest, "**", "<strong>", "</strong>", mentions))
            .or_else(|| take_wrap(out, rest, "*", "<em>", "</em>", mentions))
            .or_else(|| take_underscore(out, rest, after_word, mentions))
            .or_else(|| take_link(out, rest, mentions))
            .or_else(|| take_autolink(out, rest, after_word));
        if let Some(taken) = taken {
            at += taken;
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        escape_text(out, &ch.to_string(), true);
        at += ch.len_utf8();
    }
}

fn fence_open(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("```")?;
    if rest.contains('`') {
        return None;
    }
    let lang = rest.trim();
    if lang.is_empty() {
        return Some(String::new());
    }
    lang.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '#' | '_' | '.'))
        .then(|| lang.to_ascii_lowercase())
}

fn is_fence_close(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3 && line.bytes().all(|byte| byte == b'`')
}

fn quote_body(line: &str) -> Option<&str> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if spaces > 3 {
        return None;
    }
    let rest = line[spaces..].strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn list_marker(line: &str) -> Option<(u8, bool, &str)> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if spaces > 16 {
        return None;
    }
    let rest = &line[spaces..];
    let depth = (spaces / 2).min(8) as u8;
    if let Some(body) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
        return Some((depth, false, body));
    }
    let digits = rest.bytes().take_while(|byte| byte.is_ascii_digit()).count();
    if (1..=6).contains(&digits) && rest[digits..].starts_with(". ") {
        return Some((depth, true, &rest[digits + 2..]));
    }
    None
}

fn closed_fence(lines: &[&str], open: usize) -> Option<usize> {
    fence_open(lines.get(open).copied()?)?;
    lines.iter().skip(open + 1).position(|line| is_fence_close(line)).map(|rel| open + 1 + rel)
}

fn structured(text: &str) -> bool {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        if closed_fence(&lines, i).is_some() {
            return true;
        }
        if quote_body(lines[i]).is_some() || list_marker(lines[i]).is_some() {
            return true;
        }
        i += 1;
    }
    false
}

fn push_item(items: &mut Vec<Item>, depth: u8, ordered: bool, body: &str) {
    let depth = match items.last() {
        Some(prev) if depth > prev.depth.saturating_add(1) => prev.depth + 1,
        _ => depth,
    };
    items.push(Item { depth, ordered, body: body.to_owned() });
}

fn flush_para(para: &mut Vec<&str>, blocks: &mut Vec<Block>, before_block: bool) {
    if para.iter().all(|line| line.trim().is_empty()) {
        para.clear();
        return;
    }
    let mut body = para.join("\n");
    para.clear();
    if before_block && body.ends_with('\n') {
        body.push('\n');
    }
    if !before_block {
        while body.ends_with('\n') {
            body.pop();
        }
    }
    if !body.is_empty() {
        blocks.push(Block::Para(body));
    }
}

fn split_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks = Vec::new();
    let mut para = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(close) = closed_fence(&lines, i) {
            flush_para(&mut para, &mut blocks, true);
            let language = fence_open(lines[i]).unwrap_or_default();
            blocks.push(Block::Code { language, body: lines[i + 1..close].join("\n") });
            i = close + 1;
            continue;
        }
        if quote_body(lines[i]).is_some() {
            flush_para(&mut para, &mut blocks, true);
            let mut parts = Vec::new();
            while i < lines.len() {
                match quote_body(lines[i]) {
                    Some(body) => {
                        parts.push(body);
                        i += 1;
                    }
                    None => break,
                }
            }
            blocks.push(Block::Quote(parts.join("\n")));
            continue;
        }
        if list_marker(lines[i]).is_some() {
            flush_para(&mut para, &mut blocks, true);
            let mut items = Vec::new();
            while i < lines.len() {
                if let Some((depth, ordered, body)) = list_marker(lines[i]) {
                    push_item(&mut items, depth, ordered, body);
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::List(items));
            continue;
        }
        para.push(lines[i]);
        i += 1;
    }
    flush_para(&mut para, &mut blocks, false);
    blocks
}

fn write_run(out: &mut String, items: &[Item], mut index: usize, mentions: &mut Vec<OutMention>) -> usize {
    let depth = items[index].depth;
    let ordered = items[index].ordered;
    out.push_str(if ordered { "<ol>" } else { "<ul>" });
    while index < items.len() && items[index].depth == depth && items[index].ordered == ordered {
        out.push_str("<li>");
        inline_into(out, &items[index].body, mentions);
        index += 1;
        while index < items.len() && items[index].depth > depth {
            index = write_run(out, items, index, mentions);
        }
        out.push_str("</li>");
    }
    out.push_str(if ordered { "</ol>" } else { "</ul>" });
    index
}

fn write_list(out: &mut String, items: &[Item], mentions: &mut Vec<OutMention>) {
    let mut index = 0;
    while index < items.len() {
        index = write_run(out, items, index, mentions);
    }
}

fn emit_paragraph(out: &mut String, body: &str, quote: bool, mentions: &mut Vec<OutMention>) {
    if quote {
        out.push_str("<blockquote>");
    }
    out.push_str("<p>");
    inline_into(out, body, mentions);
    out.push_str("</p>");
    if quote {
        out.push_str("</blockquote>");
    }
}

fn emit_code(out: &mut String, language: &str, body: &str) {
    if language.is_empty() {
        out.push_str("<codeblock>");
    } else {
        out.push_str("<codeblock class=\"language-");
        out.push_str(language);
        out.push_str("\">");
    }
    escape_text(out, body, false);
    out.push_str("</codeblock>");
}

fn emit_block(out: &mut String, block: Block, mentions: &mut Vec<OutMention>) {
    match block {
        Block::Para(body) => emit_paragraph(out, &body, false, mentions),
        Block::Quote(body) => emit_paragraph(out, &body, true, mentions),
        Block::Code { language, body } => emit_code(out, &language, &body),
        Block::List(items) => write_list(out, &items, mentions),
    }
}

/// The text the user typed, as HTML Teams will show, along with any outgoing mentions extracted
/// from wire markers (`<@id|Name>`, `<@&id|Name>`, `<#id|Name>`).
pub(super) fn compose_rich(text: &str) -> (String, Vec<OutMention>) {
    let text = normalized(text);
    let mut mentions = Vec::new();
    if !structured(&text) {
        let mut out = String::with_capacity(text.len() + 7);
        out.push_str("<p>");
        inline_into(&mut out, &text, &mut mentions);
        out.push_str("</p>");
        return (out, mentions);
    }
    let mut out = String::new();
    for block in split_blocks(&text) {
        emit_block(&mut out, block, &mut mentions);
    }
    if out.is_empty() {
        out.push_str("<p></p>");
    }
    (out, mentions)
}

/// The text the user typed, as HTML Teams will show. Bold, italic, strike, code, quotes,
/// lists and links become tags. Underline, headings, colour and tables have no markdown, so
/// they stay as typed. Only http, https and mailto become links; everything else is escaped.
pub(super) fn compose(text: &str) -> String {
    compose_rich(text).0
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;

    impl Lookup for Fixed {
        fn mention(&self, item: &str) -> Option<(MentionKind, String)> {
            match item {
                "0" => Some((MentionKind::User, "8:orgid:ana".to_owned())),
                "1" => Some((MentionKind::User, "8:orgid:me".to_owned())),
                "2" => Some((MentionKind::Channel, "19:general@thread.tacv2".to_owned())),
                _ => None,
            }
        }

        fn hosted(&self, url: &str) -> bool {
            url.starts_with("https://eu-api.asm.skype.com/")
        }

        fn emoji(&self, id: &str, _url: &str) -> Option<MediaFile> {
            Some(MediaFile::new(format!("emoji:{id}"), protocol::MediaKind::Emoji))
        }
    }

    fn read(html: &str) -> Parsed {
        parse(html, &Fixed, "8:orgid:me")
    }

    fn ranges(parsed: &Parsed) -> Vec<(&str, &Style)> {
        parsed.spans.iter().map(|span| (&parsed.text[span.start as usize..span.end as usize], &span.style)).collect()
    }

    fn has(parsed: &Parsed, text: &str, style: Style) -> bool {
        ranges(parsed).contains(&(text, &style))
    }

    #[test]
    fn inline_styles_become_ranges_and_no_markup_is_left() {
        let parsed = read(
            "<p>Ship <b>to<i>day</i></b>, <s>not</s> <u>now</u>: <code>cargo&nbsp;test</code> \
             &amp; <a href=\"https://ex.com/a?b=1&amp;c=2\" title=\"x\">the plan</a> &lt;3</p>",
        );
        assert_eq!(parsed.text, "Ship today, not now: cargo test & the plan <3");
        assert!(has(&parsed, "today", Style::Bold) && has(&parsed, "day", Style::Italic));
        assert!(has(&parsed, "not", Style::Strike) && has(&parsed, "now", Style::Underline));
        assert!(has(&parsed, "cargo test", Style::Code));
        assert!(has(&parsed, "the plan", Style::Link { url: "https://ex.com/a?b=1&c=2".into() }));
        assert!(!parsed.card && !parsed.mentioned && parsed.images.is_empty());
    }

    #[test]
    fn blocks_cover_whole_lines() {
        let parsed = read(
            "<p>Steps</p><ol><li>build</li><li>deploy<ul><li>eu</li></ul></li></ol>\
             <blockquote><p>looks good</p></blockquote>\
             <pre class=\"language-rust\"><code>let a = 1;\n  let b = 2;</code></pre><p>&nbsp;</p><div>done<br>really</div>",
        );
        assert_eq!(parsed.text, "Steps\nbuild\ndeploy\neu\nlooks good\nlet a = 1;\n  let b = 2;\n \ndone\nreally");
        assert!(has(&parsed, "build", Style::ListItem { ordered: true, depth: 0 }));
        assert!(has(&parsed, "eu", Style::ListItem { ordered: false, depth: 1 }));
        assert!(has(&parsed, "looks good", Style::Quote));
        assert!(has(&parsed, "let a = 1;\n  let b = 2;", Style::CodeBlock { language: "rust".into() }));
        // Inside a code block, `code` is not a second style.
        assert!(!parsed.spans.iter().any(|span| span.style == Style::Code));
    }

    #[test]
    fn mentions_read_as_names_and_say_who() {
        let parsed = read(
            "<p><span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Ana Ruiz</span> and \
             <span itemtype=\"http://schema.skype.com/Mention\" itemscope=\"\" itemid=\"1\"><b>Me</b></span> in \
             <span itemtype=\"http://schema.skype.com/Mention\" itemid=\"2\">General</span></p>",
        );
        assert_eq!(parsed.text, "@Ana Ruiz and @Me in @General");
        assert!(has(&parsed, "@Ana Ruiz", Style::Mention { kind: MentionKind::User, id: "8:orgid:ana".into() }));
        assert!(has(&parsed, "@Me", Style::Mention { kind: MentionKind::User, id: "8:orgid:me".into() }));
        assert!(has(&parsed, "Me", Style::Bold), "what was inside the mention moved over with it");
        assert!(has(&parsed, "@General", Style::Mention { kind: MentionKind::Channel, id: "19:general@thread.tacv2".into() }));
        assert!(parsed.mentioned);
        assert!(!read("<p><at id=\"0\">Ana</at> hi</p>").mentioned);
    }

    #[test]
    fn pictures_emoji_and_cards_leave_the_text() {
        let parsed = read(
            "<p>Look <span class=\"animated-emoticon-20-smile\" title=\"Smile\" type=\"(smile)\">\
             <img itemscope itemtype=\"http://schema.skype.com/Emoji\" itemid=\"smile\" alt=\"🙂\" \
             src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/smile/default/20_f.png\" \
             style=\"width:20px; height:20px\"></span></p>\
             <p><img alt=\"image\" src=\"https://eu-api.asm.skype.com/v1/objects/0-weu-d1-abc/views/imgo\" \
             itemid=\"0-weu-d1-abc\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"1024\" style=\"height:auto; width:50px\" height=\"576.5\"></p>\
             <p><img itemtype=\"http://schema.skype.com/Giphy\" alt=\"Cat GIF\" src=\"https://media4.giphy.com/media/x/giphy.gif\" width=\"200\" height=\"100\"></p>\
             <p><img itemtype=\"http://schema.skype.com/CustomEmoji\" itemid=\"0-weu-d2-partyparrot\" alt=\"partyparrot\" \
             src=\"https://eu-api.asm.skype.com/v1/objects/0-weu-d2-partyparrot/views/imgt1_anim\"></p>",
        );
        assert_eq!(parsed.text, "Look 🙂\n:partyparrot:");
        let image = Image {
            src: "https://eu-api.asm.skype.com/v1/objects/0-weu-d1-abc/views/imgo".into(),
            width: 1024,
            height: 576,
            name: "image".into(),
        };
        assert_eq!(parsed.images, [image]);
        // A GIF from Teams' Giphy button is a picture of its own; Giphy's host is the only one
        // outside Teams that is ever fetched, and only for that.
        let gif = Image { src: "https://media4.giphy.com/media/x/giphy.gif".into(), width: 200, height: 100, name: "Cat GIF".into() };
        assert_eq!(parsed.gifs, [gif]);
        assert!(giphy("https://i.giphy.com/x.gif") && !giphy("http://media.giphy.com/x.gif") && !giphy("https://giphy.com.evil.example/x.gif"));
        let elsewhere = read("<p><img itemtype=\"http://schema.skype.com/Giphy\" alt=\"Not Giphy\" src=\"https://example.org/x.gif\"></p>");
        assert!(elsewhere.gifs.is_empty() && has(&elsewhere, "Not Giphy", Style::Link { url: "https://example.org/x.gif".into() }));
        assert!(parsed.spans.iter().any(|span| matches!(&span.style, Style::Emoji { file } if file.id == "emoji:0-weu-d2-partyparrot")));

        let card = read("<URIObject type=\"SWIFT.1\" url_thumbnail=\"https://x\"><Swift b64=\"eyJ0eXBlIjoibWVzc2FnZSJ9\"></Swift>Card - access it on https://go.skype.com/cards.unsupported</URIObject>");
        assert!(card.card && card.text.is_empty());
    }

    #[test]
    fn a_quoted_reply_is_lifted_out_of_the_text() {
        let parsed = read(
            "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"1760000100100\">\
             <strong itemprop=\"mri\" itemid=\"8:orgid:ana\">Ana Ruiz</strong>\
             <span itemprop=\"time\" itemid=\"1760000100100\"></span>\
             <p itemprop=\"preview\">is the build <b>green</b>?</p></blockquote><p>yes</p>",
        );
        assert_eq!(parsed.text, "yes");
        assert!(parsed.spans.is_empty());
        let quote = Quote {
            id: "1760000100100".into(),
            mri: "8:orgid:ana".into(),
            name: "Ana Ruiz".into(),
            preview: "is the build green?".into(),
        };
        assert_eq!(parsed.quote, Some(quote));
    }

    #[test]
    fn broken_markup_is_read_as_far_as_it_goes() {
        assert_eq!(read("a < b and c > d").text, "a < b and c > d");
        assert_eq!(read("<p>unclosed <b>bold").text, "unclosed bold");
        assert!(has(&read("<p>unclosed <b>bold"), "bold", Style::Bold));
        assert_eq!(read("</div>stray &bogus; &#128512; &#x1F600;<!-- gone --><script>alert(1)</script>").text, "stray &bogus; 😀 😀");
        assert_eq!(read("<a href=\"javascript:alert(1)\">x</a>").spans, []);
        assert_eq!(read("plain   text\n  on lines").text, "plain text on lines");
        assert_eq!(read("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>").text, "a | b\nc");
        assert_eq!(read("").text, "");
    }

    #[test]
    fn typed_text_is_escaped_on_the_way_out() {
        assert_eq!(compose("a < b & \"c\"\nnext"), "<p>a &lt; b &amp; &quot;c&quot;<br>next</p>");
        assert_eq!(read(&compose("1 < 2 && <b>not bold</b>\nline")).text, "1 < 2 && <b>not bold</b>\nline");
    }

    #[test]
    fn typed_markdown_reads_back_as_the_same_text_and_spans() {
        let parsed = read(&compose("Ship **today**, not ~~yesterday~~: `cargo test` and *now*"));
        assert_eq!(parsed.text, "Ship today, not yesterday: cargo test and now");
        assert!(has(&parsed, "today", Style::Bold));
        assert!(has(&parsed, "yesterday", Style::Strike));
        assert!(has(&parsed, "cargo test", Style::Code));
        assert!(has(&parsed, "now", Style::Italic));

        let parsed = read(&compose("[the plan](https://ex.com/a?b=1&c=2) and [mail](mailto:a@b.c)"));
        assert_eq!(parsed.text, "the plan and mail");
        assert!(has(&parsed, "the plan", Style::Link { url: "https://ex.com/a?b=1&c=2".into() }));
        assert!(has(&parsed, "mail", Style::Link { url: "mailto:a@b.c".into() }));
        let unsafe_link = read(&compose("[x](javascript:alert(1))"));
        assert_eq!(unsafe_link.text, "[x](javascript:alert(1))");
        assert!(unsafe_link.spans.is_empty());

        let parsed = read(&compose("- build\n- deploy\n  - eu\n1. ship"));
        assert_eq!(parsed.text, "build\ndeploy\neu\nship");
        assert!(has(&parsed, "build", Style::ListItem { ordered: false, depth: 0 }));
        assert!(has(&parsed, "eu", Style::ListItem { ordered: false, depth: 1 }));
        assert!(has(&parsed, "ship", Style::ListItem { ordered: true, depth: 0 }));

        let parsed = read(&compose("> looks good\n> really"));
        assert_eq!(parsed.text, "looks good\nreally");
        assert!(has(&parsed, "looks good\nreally", Style::Quote));

        let parsed = read(&compose("```rust\nlet a = 1;\n  let b = 2;\n```"));
        assert_eq!(parsed.text, "let a = 1;\n  let b = 2;");
        assert!(has(&parsed, "let a = 1;\n  let b = 2;", Style::CodeBlock { language: "rust".into() }));

        // Underline, headings, colour and tables have no markdown in what we send.
        let plain = read(&compose("# Title\n__nope__\nsnake_case"));
        assert_eq!(plain.text, "# Title\n__nope__\nsnake_case");
        assert!(plain.spans.is_empty());
        prove_markdown().unwrap();
        let (shown, spans) = styled("**today**");
        assert_eq!(shown, "today");
        assert!(spans.iter().any(|span| span.style == Style::Bold));
    }
}
