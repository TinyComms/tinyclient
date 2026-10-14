//! Slack's two ways of writing a message, as the one text-and-ranges model: `blocks` (rich text
//! and Block Kit's text-bearing blocks) and mrkdwn, the older inline markup.

use super::emoji;
use protocol::{MediaFile, MentionKind, Span, Style};
use serde_json::Value;

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
    fn custom(&self, name: &str) -> Option<Custom>;
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
                    // The picture is at an address Slack does not serve, so it is named, not
                    // fetched.
                    let label = [&block["title"]["text"], &block["alt_text"]];
                    let label = label.into_iter().filter_map(Value::as_str).find(|label| !label.is_empty());
                    self.text.push_str(&format!("[image: {}]", label.unwrap_or("untitled")));
                }
                "divider" => {}
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
                mark @ (b'*' | b'_' | b'~') => closer(line, at, mark).map(|close| (close + 1, mark)),
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
            && !line[..*at].ends_with(char::is_whitespace)
            && !line[at + 1..].chars().next().is_some_and(word_like)
    })
}

/// The closing colon of an emoji name opened at `open`.
fn emoji_end(line: &str, open: usize) -> Option<usize> {
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
            (id == "S1").then(|| "oncall".to_owned())
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
}
